import { EventEmitter } from 'node:events';
import fs from 'node:fs/promises';
import type { Writable } from 'node:stream';
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
import { frameBytes } from '../render/yuv.ts';
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
    const asm = new FrameAssembler(frameBytes(w, h), (f) => {
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
    const buf = Buffer.allocUnsafe(CHUNK);
    for (let index = this.index; !this.destroyed && !this.isPastEnd(index); index++) {
      await using fh = await this.openSegment(index);
      if (!fh) break;
      await this.tail(fh, index, stdin, buf);
    }
    if (!stdin.destroyed) stdin.end();
  }

  /** Whether segment `index` is complete on disk (or will never grow again). */
  private isFinished(index: number): boolean {
    return index < this.session.completed || this.session.ended;
  }

  private isPastEnd(index: number): boolean {
    return this.session.ended && index >= this.session.completed;
  }

  /** Opens a segment, waiting for the muxer to create it. Null if it was trimmed or will never exist. */
  private async openSegment(index: number): Promise<fs.FileHandle | null> {
    const file = this.store.segmentPath(this.session, index);
    while (!this.destroyed) {
      const fh = await fs.open(file, 'r').catch(() => null);
      if (fh) return fh;
      if (this.isFinished(index)) return null;
      await sleep(30);
    }
    return null;
  }

  /** Streams one segment into the decoder, following it while it's still being written. */
  private async tail(fh: fs.FileHandle, index: number, stdin: Writable, buf: Buffer): Promise<void> {
    let pos = 0;
    while (!this.destroyed) {
      // Snapshot before reading: if the segment was already finished, an empty read
      // really is its end; otherwise more data may still arrive.
      const finished = this.isFinished(index);
      const { bytesRead } = await fh.read(buf, 0, CHUNK, pos);
      if (bytesRead > 0) {
        pos += bytesRead;
        if (!stdin.write(Buffer.from(buf.subarray(0, bytesRead)))) await drained(stdin);
      } else if (finished) {
        return;
      } else {
        await sleep(25);
      }
    }
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
