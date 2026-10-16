// Byte/frame queues used to move raw video and audio between ffmpeg processes.

export const AUDIO_RATE = 48_000;
export const AUDIO_BYTES_PER_SAMPLE = 4; // s16le stereo

export interface AVFrame {
  video: Buffer;
  audio: Buffer;
}

export class ByteQueue {
  chunks: Buffer[] = [];
  offset = 0; // read offset into chunks[0]
  length = 0;

  push(buf: Buffer): void {
    if (!buf.length) return;
    this.chunks.push(buf);
    this.length += buf.length;
  }

  /** Copy up to `n` bytes into `dst` at `dstOffset`; returns bytes copied. */
  readInto(dst: Buffer, dstOffset: number, n: number): number {
    return this.consume(n, (head, from, take, done) => head.copy(dst, dstOffset + done, from, from + take));
  }

  skip(n: number): number {
    return this.consume(n);
  }

  clear(): void {
    this.chunks = [];
    this.offset = 0;
    this.length = 0;
  }

  private consume(n: number, each?: (head: Buffer, from: number, take: number, done: number) => void): number {
    let done = 0;
    while (done < n && this.chunks.length) {
      const head = this.chunks[0];
      const take = Math.min(n - done, head.length - this.offset);
      each?.(head, this.offset, take, done);
      done += take;
      this.offset += take;
      if (this.offset === head.length) {
        this.chunks.shift();
        this.offset = 0;
      }
    }
    this.length -= done;
    return done;
  }
}

/** Cuts a raw byte stream into fixed-size frames. */
export class FrameAssembler {
  readonly frameBytes: number;
  private readonly onFrame: (frame: Buffer) => void;
  private cur: Buffer | null = null;
  private pos = 0;

  constructor(frameBytes: number, onFrame: (frame: Buffer) => void) {
    this.frameBytes = frameBytes;
    this.onFrame = onFrame;
  }

  push(chunk: Buffer): void {
    let off = 0;
    while (off < chunk.length) {
      if (!this.cur) {
        this.cur = Buffer.allocUnsafe(this.frameBytes);
        this.pos = 0;
      }
      const n = Math.min(this.frameBytes - this.pos, chunk.length - off);
      chunk.copy(this.cur, this.pos, off, off + n);
      this.pos += n;
      off += n;
      if (this.pos === this.frameBytes) {
        const frame = this.cur;
        this.cur = null;
        this.onFrame(frame);
      }
    }
  }
}

/**
 * Pairs video frame N with audio samples [N*spf, (N+1)*spf) of the same stream, so
 * dropping, repeating or skipping frames never pulls audio and video apart.
 */
export class AVQueue {
  readonly fps: number;
  readonly bytesPerFrame: number;
  frames: Buffer[] = [];
  /** absolute index of frames[0] */
  next = 0;
  audio = new ByteQueue();
  /** absolute byte position of the first queued audio byte */
  audioHead = 0;
  received = 0;

  constructor(fps: number) {
    this.fps = fps;
    this.bytesPerFrame = (AUDIO_RATE / fps) * AUDIO_BYTES_PER_SAMPLE;
  }

  pushVideo(frame: Buffer): void {
    this.frames.push(frame);
    this.received++;
  }

  pushAudio(chunk: Buffer): void {
    this.audio.push(chunk);
  }

  get length(): number {
    return this.frames.length;
  }

  /** Drop audio belonging to frames that were already consumed or dropped. */
  alignAudio(): void {
    const want = this.next * this.bytesPerFrame;
    if (this.audioHead < want) this.audioHead += this.audio.skip(want - this.audioHead);
    // Never let audio run more than 3 s ahead of video (e.g. video stalled).
    const maxAhead = this.bytesPerFrame * this.fps * 3;
    if (this.audio.length > maxAhead) this.audioHead += this.audio.skip(this.audio.length - maxAhead);
  }

  hasAudioForNext(): boolean {
    this.alignAudio();
    return this.audioHead === this.next * this.bytesPerFrame && this.audio.length >= this.bytesPerFrame;
  }

  /**
   * The next frame with its audio, or null when it isn't available yet.
   * With `force`, missing audio is replaced by silence instead of waiting for it.
   */
  pull(force = false): AVFrame | null {
    if (!this.frames.length) return null;
    if (!this.hasAudioForNext() && !force) return null;
    const video = this.frames.shift()!;
    const audio = Buffer.alloc(this.bytesPerFrame);
    if (this.audioHead === this.next * this.bytesPerFrame) {
      this.audioHead += this.audio.readInto(audio, 0, this.bytesPerFrame);
    }
    this.next++;
    return { video, audio };
  }

  /** Drop the oldest frames (and their audio) so at most `max` remain. */
  trimTo(max: number): number {
    let dropped = 0;
    while (this.frames.length > max) {
      this.frames.shift();
      this.next++;
      dropped++;
    }
    this.alignAudio();
    return dropped;
  }

  clear(): void {
    this.next += this.frames.length;
    this.frames = [];
    this.alignAudio();
  }
}
