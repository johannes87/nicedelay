import { EventEmitter } from 'node:events';
import { errorMessage } from './config.ts';
import type { BufferStore } from './media/buffer.ts';
import { Encoder } from './media/encoder.ts';
import type { Ingest } from './media/ingest.ts';
import type { AVFrame } from './media/queues.ts';
import { BufferReader } from './media/reader.ts';
import { formatDelay, Overlay, type PillParams } from './render/overlay.ts';
import { mixInto, tapeWhoosh } from './render/sfx.ts';
import { crossfade, frameSize, scaleFrame, tapeEffect, toneFrame } from './render/yuv.ts';
import type { CommandResult, Config, Mode, PlayoutState, TransitionEvent, TransitionKind } from './types.ts';

export type { CommandResult, Mode, PlayoutState, TransitionEvent } from './types.ts';

const PILL_HOLD = 3; // seconds the corner pill stays after a transition
const PILL_FADE = 0.6;
const READY_TIMEOUT = 3; // max seconds to wait for a reader at landing

const easeInOut = (t: number) => (t < 0.5 ? 4 * t * t * t : 1 - (-2 * t + 2) ** 3 / 2);
const easeOut = (t: number) => 1 - (1 - t) ** 3;
const backOut = (t: number) => 1 + 2.9 * (t - 1) ** 3 + 1.9 * (t - 1) ** 2;
const clamp01 = (t: number) => Math.min(1, Math.max(0, t));

type LiveSource = { kind: 'live' };
type ReaderSource = { kind: 'reader'; reader: BufferReader };
type Source = LiveSource | ReaderSource;
type Correction = 'none' | 'steady' | 'fast';

interface PlayoutEvents {
  change: [];
  tick: [];
  preview: [Buffer];
  transition: [TransitionEvent];
}

/**
 * Drives the output: one frame per tick, from the live feed, a buffer reader, or a
 * transition that renders the rewind / fast-forward animation between the two.
 */
export class Playout extends EventEmitter<PlayoutEvents> {
  config: Config;
  readonly store: BufferStore;
  readonly ingest: Ingest;
  mode: Mode = 'idle';
  targetDelay = 0;
  source: Source | null = null;
  transition: Transition | null = null;
  encoder: Encoder | null = null;
  lastFrame: Buffer | null = null;
  readonly overlay: Overlay;
  readonly w: number;
  readonly h: number;
  readonly fps: number;
  readonly frameBytes: number;
  readonly audioBytes: number;
  private readonly liveTarget: number;
  private readonly liveMax: number;
  private pending: { delay: number } | null = null;
  private pill: { kind: 'replay' | 'live'; start: number } | null = null;
  private standby: { from: Buffer; frame: Buffer } | null = null;
  private ticks = 0;
  private underruns = 0;
  private starvedSince: number | null = null;
  private liveRebuffer = true;
  private lastCorrection = 0;
  private clockRunning = false;
  private timer: NodeJS.Timeout | undefined;

  constructor({ config, store, ingest }: { config: Config; store: BufferStore; ingest: Ingest }) {
    super();
    this.config = config;
    this.store = store;
    this.ingest = ingest;
    const { width, height, fps } = config.output;
    this.w = width;
    this.h = height;
    this.fps = fps;
    this.frameBytes = frameSize(width, height);
    this.audioBytes = (48_000 / fps) * 4;
    this.liveTarget = Math.max(2, Math.round(fps * 0.1));
    this.liveMax = Math.max(6, Math.round(fps * 0.3));
    this.overlay = new Overlay(width, height);
    ingest.on('connected', () => this.onConnected());
  }

  // ---------------------------------------------------------------- lifecycle

  private onConnected(): void {
    if (this.mode === 'idle') this.startOutput();
    this.starvedSince = null;
  }

  private startOutput(): void {
    this.mode = 'live';
    this.targetDelay = 0;
    this.source = { kind: 'live' };
    this.liveRebuffer = true;
    this.lastFrame = null;
    this.startEncoder();
    this.startClock();
    this.emit('change');
  }

  private startEncoder(): void {
    const enc = new Encoder({ config: this.config });
    this.encoder = enc;
    enc.on('preview', (jpeg) => this.emit('preview', jpeg));
    enc.on('exit', ({ unexpected }) => {
      if (unexpected && this.encoder === enc && this.mode !== 'idle') {
        setTimeout(() => {
          if (this.encoder === enc && this.mode !== 'idle') this.startEncoder();
        }, 2000);
      }
      this.emit('change');
    });
    enc.start();
  }

  stopOutput(): void {
    this.clockRunning = false;
    clearTimeout(this.timer);
    this.encoder?.stop();
    this.encoder = null;
    if (this.source?.kind === 'reader') this.source.reader.destroy();
    this.transition?.dispose();
    this.transition = null;
    this.pending = null;
    this.source = null;
    this.mode = 'idle';
    this.targetDelay = 0;
    this.pill = null;
    this.emit('change');
  }

  private startClock(): void {
    this.clockRunning = true;
    const period = 1000 / this.fps;
    let t0 = performance.now();
    let n = 0;
    const loop = () => {
      if (!this.clockRunning) return;
      const now = performance.now();
      let due = Math.floor((now - t0) / period) + 1 - n;
      if (due > this.fps) {
        // Fell more than a second behind (machine stalled): resync instead of bursting.
        t0 = now - n * period;
        due = 1;
      }
      for (let i = 0; i < due && this.clockRunning; i++) {
        this.tick();
        n++;
      }
      this.timer = setTimeout(loop, Math.max(0, t0 + n * period - performance.now()));
    };
    loop();
  }

  // ---------------------------------------------------------------- commands

  /** Max delay that can be applied right now (limited by what's recorded). */
  get maxDelay(): number {
    return Math.max(0, Math.min(this.config.bufferMinutes * 60 - 2, this.store.recorded - 0.5));
  }

  setDelay(seconds: unknown): CommandResult {
    if (this.mode === 'idle') return { ok: false, error: 'Not streaming — connect OBS first.' };
    let d = Number(seconds);
    if (!Number.isFinite(d) || d <= 0) return this.goLive();
    let clamped = false;
    if (d > this.maxDelay) {
      d = Math.floor(this.maxDelay);
      clamped = true;
    }
    if (d < 1) return { ok: false, error: 'Not enough recorded yet.' };
    if (this.transition) {
      this.pending = { delay: d };
      return { ok: true, queued: true, delay: d, clamped };
    }
    if (this.mode === 'delayed' && Math.abs(d - this.targetDelay) < 0.5) return { ok: true, delay: d, clamped };
    this.beginTransition(d);
    return { ok: true, delay: d, clamped };
  }

  goLive(): CommandResult {
    if (this.mode === 'idle') return { ok: false, error: 'Not streaming.' };
    if (this.transition) {
      this.pending = { delay: 0 };
      return { ok: true, queued: true, delay: 0 };
    }
    if (this.mode === 'delayed') this.beginTransition(0);
    return { ok: true, delay: 0 };
  }

  toggle(): CommandResult {
    const target = this.pending ? this.pending.delay : this.targetDelay;
    return target > 0 ? this.goLive() : this.setDelay(this.config.delaySeconds);
  }

  private beginTransition(toDelay: number): void {
    const fromDelay = this.mode === 'delayed' ? this.targetDelay : 0;
    let t: Transition;
    try {
      t = new Transition(this, fromDelay, toDelay);
    } catch (err) {
      console.warn(`[playout] cannot start transition: ${errorMessage(err)}`);
      return;
    }
    this.transition = t;
    this.mode = toDelay > 0 ? 'delayed' : 'live';
    this.targetDelay = toDelay;
    this.pill = null;
    this.emit('transition', {
      kind: t.kind,
      from: fromDelay,
      to: toDelay,
      phases: { intro: t.A, scrub: t.B, land: t.C },
    });
    this.emit('change');
  }

  // ---------------------------------------------------------------- per frame

  private tick(): void {
    this.ticks++;
    // The live queue must keep flowing even while not shown, so it stays at the live edge.
    if (this.ingest.live && !this.wantsLive()) this.ingest.live.trimTo(this.liveTarget);

    let out: AVFrame | null;
    let fresh = false; // whether out.video is a buffer we own and may draw on
    if (this.transition) {
      out = this.transition.render();
      fresh = true;
      if (this.transition.done) this.finishTransition();
    } else {
      out = this.pullSource();
    }

    if (out) {
      this.starvedSince = null;
      this.lastFrame = out.video;
    } else {
      this.underruns++;
      this.checkStarved();
      out = { video: this.standbyFrame(), audio: Buffer.alloc(this.audioBytes) };
      fresh = false;
    }

    const pill = this.currentPill();
    if (pill) {
      if (!fresh) {
        const copy = Buffer.allocUnsafe(this.frameBytes);
        out.video.copy(copy);
        out.video = copy;
      }
      this.overlay.drawPill(out.video, pill);
    }

    this.encoder?.write(out.video, out.audio);
    if (this.ticks % this.fps === 0) this.housekeeping();
  }

  private wantsLive(): boolean {
    if (this.transition) return this.transition.wantsLive();
    return this.source?.kind === 'live';
  }

  pullLive(): AVFrame | null {
    const q = this.ingest.live;
    if (!q) return null;
    if (q.length === 0) this.liveRebuffer = true;
    if (this.liveRebuffer) {
      if (q.length < this.liveTarget) return null;
      this.liveRebuffer = false;
    }
    // Drift: if the feed runs ahead of our clock, drop single frames to stay near the edge.
    if (q.length > this.liveMax) q.trimTo(q.length - 1);
    return q.pull(q.length > this.liveMax);
  }

  pullReader(src: ReaderSource, correct: Correction = 'steady'): AVFrame | null {
    let r = src.reader;
    if (r.exhausted) {
      const next = this.store.nextSession(r.session);
      if (!next) return null;
      try {
        const nr = new BufferReader({ store: this.store, config: this.config, t: next.base });
        r.destroy();
        src.reader = r = nr;
      } catch {
        return null;
      }
    }
    // Keep the real delay honest: drift, underruns and slow starts accumulate.
    if (correct !== 'none') {
      const err = this.store.liveEdge - r.position - this.targetDelay;
      const now = this.ticks / this.fps;
      if (correct === 'fast' && err > 2 / this.fps) {
        r.skip(1);
      } else if (Math.abs(err) > 0.2 && now - this.lastCorrection > 0.5) {
        this.lastCorrection = now;
        if (err < 0) return null; // repeat a frame
        r.skip(1);
      }
    }
    return r.pull(r.queue.length >= r.maxQueue - 1);
  }

  private pullSource(): AVFrame | null {
    if (!this.source) return null;
    if (this.source.kind === 'live') return this.pullLive();
    return this.pullReader(this.source);
  }

  private finishTransition(): void {
    const t = this.transition;
    if (!t) return;
    this.source = t.target;
    this.transition = null;
    this.pill = { kind: t.toDelay > 0 ? 'replay' : 'live', start: this.ticks };
    this.emit('change');
    const pending = this.pending;
    this.pending = null;
    if (pending) {
      if (pending.delay > 0) this.setDelay(pending.delay);
      else this.goLive();
    }
  }

  private currentPill(): PillParams | null {
    if (this.starvedSince !== null && this.ticks / this.fps - this.starvedSince > 1) {
      return { kind: 'live', text: 'RECONNECTING', alpha: 0.95 };
    }
    if (this.transition) return null;
    let alpha = this.config.transition.badge && this.mode === 'delayed' ? 0.9 : 0;
    if (this.pill) {
      const age = (this.ticks - this.pill.start) / this.fps;
      const a = age < 0.3 ? easeOut(age / 0.3) : age < PILL_HOLD ? 1 : 1 - (age - PILL_HOLD) / PILL_FADE;
      if (age > PILL_HOLD + PILL_FADE) this.pill = null;
      alpha = Math.max(alpha, clamp01(a));
    }
    if (alpha <= 0) return null;
    if (this.mode === 'delayed') return { kind: 'replay', text: `REPLAY  ${formatDelay(this.targetDelay)}`, alpha };
    return { kind: 'live', text: 'LIVE', alpha };
  }

  standbyFrame(): Buffer {
    if (!this.lastFrame) {
      const black = Buffer.alloc(this.frameBytes, 128);
      black.fill(16, 0, this.w * this.h);
      this.lastFrame = black;
    }
    if (this.starvedSince === null) return this.lastFrame;
    if (this.standby?.from !== this.lastFrame) {
      const dim = Buffer.allocUnsafe(this.frameBytes);
      toneFrame(this.lastFrame, dim, this.w, this.h, { luma: 0.45, sat: 0.3 });
      this.standby = { from: this.lastFrame, frame: dim };
    }
    return this.standby.frame;
  }

  /**
   * A momentary underrun just repeats the last frame. Only once OBS is gone and the
   * buffer has played out do we show standby and, after the grace period, end the stream.
   */
  private checkStarved(): void {
    if (this.ingest.connected) return;
    const now = this.ticks / this.fps;
    this.starvedSince ??= now;
    if (now - this.starvedSince > this.config.reconnectGraceSeconds) {
      console.log('[playout] source gone, ending output');
      this.stopOutput();
    }
  }

  private housekeeping(): void {
    let protect = Infinity;
    const readers = [this.source, this.transition?.target, this.transition?.old].flatMap((s) =>
      s?.kind === 'reader' ? [s.reader] : [],
    );
    for (const r of readers) protect = Math.min(protect, r.startTime, r.position);
    this.store.trim(protect);
    this.emit('tick');
  }

  // ---------------------------------------------------------------- state

  get position(): number {
    return this.source?.kind === 'reader' ? this.source.reader.position : this.store.liveEdge;
  }

  get effectiveDelay(): number {
    if (this.mode !== 'delayed' || this.transition) return this.targetDelay;
    return Math.max(0, this.store.liveEdge - this.position);
  }

  state(): PlayoutState {
    const t = this.transition;
    const enc = this.encoder;
    return {
      mode: this.mode,
      targetDelay: this.targetDelay,
      effectiveDelay: Math.round(this.effectiveDelay * 10) / 10,
      maxDelay: Math.floor(this.maxDelay),
      transition: t ? { kind: t.kind, from: t.fromDelay, to: t.toDelay, progress: t.progress } : null,
      output: {
        running: !!enc?.running,
        codec: enc?.codec ?? null,
        destinations: enc ? enc.destinations.map((d) => d.name || d.url) : [],
        previewOnly: enc ? !enc.destinations.length : false,
        since: enc?.startedAt ?? null,
        dropped: enc?.dropped ?? 0,
        error: enc?.lastError ?? null,
      },
      underruns: this.underruns,
    };
  }
}

/**
 * One rewind / fast-forward animation. Timeline (seconds):
 *   A  intro — the current picture keeps playing, dims, the icon pops in
 *   B  scrub — thumbnails race from the old position to the new one
 *   C  land  — crossfade into the new source while the graphics fade out
 */
class Transition {
  readonly kind: TransitionKind;
  readonly fromDelay: number;
  readonly toDelay: number;
  readonly A = 0.22;
  readonly B: number;
  readonly C = 0.5;
  readonly target: Source;
  old: Source | null;
  done = false;
  private readonly p: Playout;
  private readonly fps: number;
  private readonly scrubFrom: number;
  private readonly scrubTo: number;
  private readonly span: number;
  private readonly sfx: Buffer | null;
  private i = 0;
  private waited = 0;
  private held: Buffer | null = null;
  private lastScrub: Buffer | null = null;
  private incoming: Buffer | null = null;

  constructor(playout: Playout, fromDelay: number, toDelay: number) {
    this.p = playout;
    this.fromDelay = fromDelay;
    this.toDelay = toDelay;
    this.kind = toDelay > fromDelay ? 'rewind' : 'forward';
    this.B = Math.min(1.35, 0.7 + 0.25 * Math.log10(1 + Math.abs(toDelay - fromDelay) / 5));
    this.fps = playout.fps;

    const store = playout.store;
    const L0 = store.liveEdge;
    const land = this.A + this.B;
    this.old = playout.source;
    if (toDelay === 0) {
      this.target = { kind: 'live' };
      this.scrubTo = L0 + land;
    } else {
      // Start decoding now so the picture is ready exactly when the scrub lands.
      const reader = new BufferReader({ store, config: playout.config, t: L0 + land - toDelay });
      this.target = { kind: 'reader', reader };
      this.scrubTo = reader.startTime;
    }
    this.scrubFrom = L0 - fromDelay + this.A;
    this.span = Math.max(fromDelay, toDelay) * 1.18 || 10;
    const { sound, soundVolume } = playout.config.transition;
    this.sfx = sound ? tapeWhoosh(this.kind, land + 0.25, soundVolume) : null;
  }

  get progress(): number {
    return clamp01(this.i / this.fps / (this.A + this.B + this.C));
  }

  /** Whether the current phase consumes the live feed (otherwise Playout keeps it trimmed). */
  wantsLive(): boolean {
    const t = this.i / this.fps;
    return (t < this.A && this.old?.kind === 'live') || (t >= this.A + this.B && this.target.kind === 'live');
  }

  private get targetReady(): boolean {
    return this.target.kind === 'live' || this.target.reader.ready;
  }

  render(): AVFrame {
    const p = this.p;
    const t = this.i / this.fps;
    const audioOff = this.i * p.audioBytes;
    this.i++;
    const { w, h } = p;
    const { A, B, C } = this;
    const video: Buffer = Buffer.allocUnsafe(p.frameBytes);
    let audio: Buffer = Buffer.alloc(p.audioBytes);
    let center: { alpha: number; pop: number; progress: number; d: number };
    let knob: number;
    let lineAlpha: number;

    if (t < A) {
      // A: keep the old picture moving while it dims
      const e = easeOut(t / A);
      const old = this.old;
      const src = old ? (old.kind === 'live' ? p.pullLive() : p.pullReader(old, 'none')) : null;
      const base = src?.video ?? p.lastFrame ?? p.standbyFrame();
      if (src) {
        p.lastFrame = src.video;
        audio = src.audio;
        mixInto(audio, 1 - e, null, 0);
      }
      toneFrame(base, video, w, h, { luma: 1 - 0.4 * e, sat: 1 - 0.55 * e });
      tapeEffect(video, w, h, 0.45 * e, this.i * 7919, (t * 1.7) % 1);
      center = { alpha: e, pop: 0.55 + 0.45 * backOut(t / A), progress: 0, d: this.fromDelay };
      knob = this.fromDelay;
      lineAlpha = e;
      this.held = video;
    } else if (t < A + B || (!this.targetReady && this.waited < READY_TIMEOUT * this.fps)) {
      // B: scrub (or hold at the end of it while the target decoder warms up)
      if (t >= A + B) this.waited++;
      if (this.old?.kind === 'reader' && this.old !== this.target) {
        this.old.reader.destroy();
        this.old = null;
      }
      const u = easeInOut(clamp01((t - A) / B));
      const thumb = p.store.thumbAt(this.scrubFrom + (this.scrubTo - this.scrubFrom) * u);
      const tw = Math.round(w / 10) * 2;
      const th = Math.round(h / 10) * 2;
      if (thumb && thumb.frame.length === frameSize(tw, th)) {
        scaleFrame(thumb.frame, tw, th, video, w, h, { luma: 0.62, sat: 0.42 });
      } else if (this.held) {
        this.held.copy(video);
      }
      tapeEffect(video, w, h, 0.95, this.i * 7919, (t * 1.9) % 1);
      const d = this.fromDelay + (this.toDelay - this.fromDelay) * u;
      center = { alpha: 1, pop: 1 + 0.03 * Math.sin(t * 18), progress: u, d };
      knob = d;
      lineAlpha = 1;
      this.lastScrub = video;
    } else {
      // C: land
      const c = clamp01((t - A - B - this.waited / this.fps) / C);
      const target = this.target;
      const src = target.kind === 'live' ? p.pullLive() : p.pullReader(target, 'fast');
      const e = easeOut(c);
      if (src) {
        this.incoming = src.video;
        audio = src.audio;
        mixInto(audio, e, null, 0);
      }
      const from = this.lastScrub ?? this.held ?? p.standbyFrame();
      if (this.incoming) {
        crossfade(from, this.incoming, video, e);
        tapeEffect(video, w, h, 0.6 * (1 - e), this.i * 7919, (t * 1.9) % 1);
      } else {
        from.copy(video);
      }
      center = { alpha: 1 - e, pop: 1 + 0.18 * e, progress: 1, d: this.toDelay };
      knob = this.toDelay;
      lineAlpha = 1 - e;
      if (c >= 1) this.done = true;
    }

    const toLive = this.toDelay === 0;
    p.overlay.drawCenter(video, {
      kind: this.kind,
      alpha: center.alpha,
      pop: center.pop,
      progress: center.progress,
      timecode: toLive && center.progress > 0.97 ? 'LIVE' : formatDelay(center.d),
      label: this.kind === 'rewind' ? 'REWINDING' : toLive ? 'BACK TO LIVE' : 'SKIPPING AHEAD',
    });
    p.overlay.drawTimeline(video, { alpha: lineAlpha, span: this.span, knob });
    if (this.sfx) mixInto(audio, 1, this.sfx, audioOff);
    return { video, audio };
  }

  dispose(): void {
    if (this.target.kind === 'reader') this.target.reader.destroy();
    if (this.old?.kind === 'reader') this.old.reader.destroy();
  }
}
