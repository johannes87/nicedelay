import { EventEmitter } from 'node:events';
import type { Writable } from 'node:stream';
import type { Config, Destination, EncoderChoice } from '../config.ts';
import { type FfmpegProcess, hasEncoder, killGently, pipe, spawnFfmpeg, tailStderr, writablePipe } from './ffmpeg.ts';

const PREVIEW_WIDTH = 640;
const PREVIEW_FPS = 15;

export function pickEncoder(pref: EncoderChoice): Exclude<EncoderChoice, 'auto'> {
  if (pref !== 'auto') return pref;
  if (process.platform === 'darwin' && hasEncoder('h264_videotoolbox')) return 'h264_videotoolbox';
  return 'libx264';
}

export function destinationUrl(d: Destination): string {
  if (!/^rtmps?:\/\//i.test(d.url) || !d.key) return d.url;
  let base = d.url;
  while (base.endsWith('/')) base = base.slice(0, -1);
  return `${base}/${d.key}`;
}

/** tee muxer slave spec: escape the characters tee treats specially. */
function teeEscape(s: string): string {
  return s.replaceAll(/[\\|[\]]/g, (c) => `\\${c}`);
}

interface EncoderEvents {
  preview: [Buffer];
  exit: [{ code: number | null; unexpected: boolean }];
}

/**
 * The single, long-lived output: raw frames + PCM from Playout in, H.264/AAC out to every
 * destination at once, plus a low-res MJPEG preview for the dashboard.
 */
export class Encoder extends EventEmitter<EncoderEvents> {
  readonly config: Config;
  readonly frameSize: number;
  readonly destinations: Destination[];
  readonly codec: Exclude<EncoderChoice, 'auto'>;
  dropped = 0;
  written = 0;
  running = false;
  startedAt = 0;
  lastError: string | null = null;
  private proc: FfmpegProcess | null = null;
  private video: Writable | null = null;
  private audio: Writable | null = null;
  private stopping = false;

  constructor({ config }: { config: Config }) {
    super();
    this.config = config;
    const { width, height } = config.output;
    this.frameSize = (width * height * 3) / 2;
    this.destinations = config.destinations.filter((d) => d.enabled);
    this.codec = pickEncoder(config.output.encoder);
  }

  args(): string[] {
    const { width, height, fps, videoBitrate: vb, audioBitrate: ab } = this.config.output;
    const inputs = [
      ['-loglevel', 'warning'],
      ['-f', 'rawvideo', '-pix_fmt', 'yuv420p', '-s', `${width}x${height}`, '-framerate', String(fps)],
      ['-thread_queue_size', '64', '-i', 'pipe:0'],
      ['-f', 's16le', '-ar', '48000', '-ac', '2', '-thread_queue_size', '512', '-i', 'pipe:3'],
    ].flat();
    const preview = `fps=${PREVIEW_FPS},scale=${PREVIEW_WIDTH}:-2:flags=bilinear`;
    const previewOut = ['-map', '[prev]', '-c:v', 'mjpeg', '-q:v', '6', '-f', 'image2pipe', 'pipe:4'];
    if (!this.destinations.length) {
      return [...inputs, '-filter_complex', `[0:v]${preview}[prev]`, ...previewOut, '-map', '1:a', '-f', 'null', '-'];
    }
    const codec =
      this.codec === 'h264_videotoolbox'
        ? ['-c:v', 'h264_videotoolbox', '-realtime', '1', '-profile:v', 'high', '-allow_sw', '1']
        : [
            '-c:v',
            'libx264',
            '-preset',
            'veryfast',
            '-tune',
            'zerolatency',
            '-profile:v',
            'high',
            '-sc_threshold',
            '0',
          ];
    const tee = this.destinations.map((d) => `[f=flv:onfail=ignore]${teeEscape(destinationUrl(d))}`).join('|');
    return [
      inputs,
      ['-filter_complex', `[0:v]split=2[vout][vp];[vp]${preview}[prev]`],
      ['-map', '[vout]', '-map', '1:a'],
      codec,
      ['-b:v', `${vb}k`, '-maxrate', `${vb}k`, '-bufsize', `${vb * 2}k`],
      ['-g', String(fps * 2), '-force_key_frames', 'expr:gte(t,n_forced*2)', '-pix_fmt', 'yuv420p'],
      ['-colorspace', 'bt709', '-color_primaries', 'bt709', '-color_trc', 'bt709', '-color_range', 'tv'],
      ['-c:a', 'aac', '-b:a', `${ab}k`, '-ar', '48000', '-ac', '2'],
      ['-flags', '+global_header', '-f', 'tee', tee],
      previewOut,
    ].flat();
  }

  start(): void {
    const proc = spawnFfmpeg(this.args(), { fds: 5, stdin: 'pipe' });
    this.proc = proc;
    this.running = true;
    this.stopping = false;
    this.startedAt = Date.now();
    this.lastError = null;
    this.video = writablePipe(proc, 0);
    this.audio = writablePipe(proc, 3);

    let jpeg: Buffer[] = [];
    pipe(proc, 4).on('data', (d: Buffer) => {
      // ffmpeg's mjpeg encoder ends every image with FFD9 and never emits it elsewhere.
      let start = 0;
      for (let i = 0; i < d.length - 1; i++) {
        if (d[i] === 0xff && d[i + 1] === 0xd9) {
          jpeg.push(d.subarray(start, i + 2));
          this.emit('preview', Buffer.concat(jpeg));
          jpeg = [];
          start = i + 2;
          i++;
        }
      }
      if (start < d.length) jpeg.push(d.subarray(start));
    });
    proc.stdout.resume();

    const stderr = tailStderr(proc, (line) => {
      if (/error|failed|refused|invalid|broken pipe/i.test(line)) {
        this.lastError = line;
        console.warn(`[encoder] ${line}`);
      }
    });
    proc.on('close', (code, signal) => {
      this.running = false;
      if (!this.stopping) {
        this.lastError = stderr.slice(-2).join(' | ') || `exited (${code ?? signal})`;
        console.warn(`[encoder] exited unexpectedly: ${this.lastError}`);
      }
      this.emit('exit', { code, unexpected: !this.stopping });
    });
  }

  write(frame: Buffer, audio: Buffer): void {
    if (!this.running || !this.video || !this.audio) return;
    // If the encoder can't keep up, drop whole frames (with their audio) rather than
    // buffering without bound.
    if (this.video.writableLength > this.frameSize * 4) {
      this.dropped++;
      return;
    }
    this.video.write(frame);
    this.audio.write(audio);
    this.written++;
  }

  stop(): void {
    const proc = this.proc;
    if (!proc) return;
    this.stopping = true;
    // Closing the inputs lets ffmpeg flush and close the streams cleanly.
    this.video?.end();
    this.audio?.end();
    setTimeout(() => killGently(proc, 2000), 4000).unref();
  }
}
