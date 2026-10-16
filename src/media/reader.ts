import { EventEmitter } from 'node:events';
import fs from 'node:fs/promises';
import { setTimeout as sleep } from 'node:timers/promises';
import { type Config, errorMessage } from '../config.ts';
import type { BufferStore, Session } from './buffer.ts';
import {
  AUDIO_CHAIN,
  drained,
  type FfmpegProcess,
  killGently,
  pipe,
  scaleChain,
  spawnFfmpeg,
  tailStderr,
  writablePipe,
} from './ffmpeg.ts';
import { type AVFrame, AVQueue, FrameAssembler } from './queues.ts';

const CHUNK = 512 * 1024;

/**
 * Plays the recording from global time `t`: feeds segment files (tailing the one still
 * being written) into a decoder and exposes the decoded frames as an AVQueue.
 * One reader covers one ingest session; Playout chains readers across sessions.
 */
export class BufferReader extends EventEmitter<{ closed: [] }> {
  readonly store: BufferStore;
  readonly config: Config;
  readonly session: Session;
  /** global time of the first frame this reader outputs */
  readonly startTime: number;
  readonly fps: number;
  readonly queue: AVQueue;
  readonly maxQueue: number;
  private readonly index: number;
  private readonly offset: number;
  private readonly proc: FfmpegProcess;
  private stderr: string[] = [];
  private paused = false;
  private closed = false;
  private destroyed = false;

  constructor({ store, config, t }: { store: BufferStore; config: Config; t: number }) {
    super();
    this.store = store;
    this.config = config;
    const loc = store.locate(t);
    if (!loc) throw new Error('nothing recorded at that time');
    this.session = loc.session;
    this.index = loc.index;
    this.offset = loc.offset;
    this.startTime = loc.session.base + loc.start + loc.offset;
    this.fps = config.output.fps;
    this.queue = new AVQueue(this.fps);
    this.maxQueue = Math.max(8, Math.round(this.fps * 0.6));
    this.proc = this.start();
  }

  /** Global time of the next frame to be pulled. */
  get position(): number {
    return this.startTime + this.queue.next / this.fps;
  }

  get ready(): boolean {
    return this.queue.length > 0 && (this.queue.hasAudioForNext() || this.queue.length >= this.maxQueue - 1);
  }

  /** True once everything was decoded and consumed. */
  get exhausted(): boolean {
    return this.closed && this.queue.length === 0;
  }

  private start(): FfmpegProcess {
    const { width: w, height: h, fps } = this.config.output;
    const off = this.offset.toFixed(4);
    const args = [
      ['-loglevel', 'error'],
      ['-f', 'mpegts', '-probesize', '2M', '-analyzeduration', '1M', '-i', 'pipe:0'],
      ['-map', '0:v:0', '-f', 'rawvideo'],
      ['-vf', `trim=start=${off},setpts=PTS-STARTPTS,fps=${fps}:start_time=0,${scaleChain(w, h)}`],
      ['pipe:1'],
      ['-map', '0:a:0', '-f', 's16le'],
      ['-af', `atrim=start=${off},asetpts=PTS-STARTPTS,${AUDIO_CHAIN}`],
      ['pipe:3'],
    ].flat();
    const proc = spawnFfmpeg(args, { fds: 4, stdin: 'pipe' });
    const asm = new FrameAssembler((w * h * 3) / 2, (f) => {
      this.queue.pushVideo(f);
      if (this.queue.length >= this.maxQueue && !this.paused) {
        this.paused = true;
        proc.stdout.pause();
      }
    });
    proc.stdout.on('data', (d: Buffer) => asm.push(d));
    pipe(proc, 3).on('data', (d: Buffer) => this.queue.pushAudio(d));
    this.stderr = tailStderr(proc);
    proc.on('close', (code) => {
      this.closed = true;
      if (code && !this.destroyed)
        console.warn(`[reader] decoder exited (${code}): ${this.stderr.slice(-3).join(' | ')}`);
      this.emit('closed');
    });
    this.feed(proc).catch((err: unknown) => {
      if (!this.destroyed) console.warn(`[reader] feed error: ${errorMessage(err)}`);
      writablePipe(proc, 0).end();
    });
    return proc;
  }

  private async feed(proc: FfmpegProcess): Promise<void> {
    const stdin = writablePipe(proc, 0);
    const s = this.session;
    const buf = Buffer.allocUnsafe(CHUNK);
    let index = this.index;
    while (!this.destroyed) {
      await using fh = await fs.open(this.store.segmentPath(s, index), 'r').catch(() => null);
      if (!fh) {
        // Finished-but-missing means trimmed; otherwise the muxer hasn't opened it yet.
        if (index < s.completed || s.ended) break;
        await sleep(30);
        continue;
      }
      let pos = 0;
      while (!this.destroyed) {
        // Snapshot before reading: if the segment was already finished, an empty read
        // really is its end; otherwise keep tailing the growing file.
        const finished = index < s.completed || s.ended;
        const { bytesRead } = await fh.read(buf, 0, CHUNK, pos);
        if (bytesRead > 0) {
          pos += bytesRead;
          if (!stdin.write(Buffer.from(buf.subarray(0, bytesRead)))) await drained(stdin);
          continue;
        }
        if (finished) break;
        await sleep(25);
      }
      index++;
      if (s.ended && index >= s.completed) break;
    }
    if (!stdin.destroyed) stdin.end();
  }

  pull(force = false): AVFrame | null {
    const out = this.queue.pull(force);
    this.maybeResume();
    return out;
  }

  /** Drop up to `n` frames (keeping at least one queued) to catch up. */
  skip(n: number): number {
    let skipped = 0;
    while (skipped < n && this.queue.length > 1) {
      this.queue.pull(true);
      skipped++;
    }
    this.maybeResume();
    return skipped;
  }

  private maybeResume(): void {
    if (this.paused && this.queue.length < this.maxQueue) {
      this.paused = false;
      this.proc.stdout.resume();
    }
  }

  destroy(): void {
    if (this.destroyed) return;
    this.destroyed = true;
    killGently(this.proc, 500);
    this.queue.clear();
  }
}
