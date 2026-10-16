import { EventEmitter } from 'node:events';
import net from 'node:net';
import { errorMessage } from '../config.ts';
import type { Config, IngestInfo } from '../../shared/types.ts';
import type { BufferStore, Session } from './buffer.ts';
import {
  AUDIO_CHAIN,
  type FfmpegProcess,
  killGently,
  pipe,
  scaleChain,
  spawnFfmpeg,
  tailStderr,
} from './ffmpeg.ts';
import { frameBytes } from '../render/yuv.ts';
import { AVQueue, FrameAssembler } from './queues.ts';
import { freePort, StopDetector } from './relay.ts';

interface IngestEvents {
  connected: [Session];
  disconnected: [Session];
}

/**
 * Listens for OBS over RTMP. OBS connects to a small relay in Node, which forwards the
 * stream to ffmpeg on an internal port and notices when OBS stops streaming on purpose
 * (as opposed to crashing or losing the connection). One ffmpeg process per connection produces:
 *   fd 1: decoded live video (output size, CFR)   -> low-latency live path
 *   fd 3: decoded live audio (s16le 48k stereo)
 *   fd 4: segment list (one CSV line per finished segment)
 *   fd 5: small thumbnails at a low rate           -> scrub animation
 *   disk: MPEG-TS segments (stream copy)           -> the rolling buffer
 */
/**
 * Picks the input's video/audio details out of ffmpeg's stream listing, e.g.
 * `Stream #0:0: Video: h264 (High), yuv420p(tv, bt709), 1920x1080, 6000 kb/s, 30 fps, ...`
 */
export function parseStreamInfo(
  line: string,
  info: IngestInfo | null,
): IngestInfo | null {
  if (!line.includes('Stream #0:')) return info;
  const video = /Video: (\w+)/.exec(line);
  const size = /\b(\d{2,5})x(\d{2,5})\b/.exec(line);
  const fps = /\b(\d{1,3}(?:\.\d{1,3})?) fps\b/.exec(line);
  if (video && size) {
    return {
      codec: video[1],
      width: Number(size[1]),
      height: Number(size[2]),
      fps: fps ? Number(fps[1]) : 0,
    };
  }
  const audio = /Audio: (\w+)/.exec(line);
  return audio && info ? { ...info, audio: audio[1] } : info;
}

export class Ingest extends EventEmitter<IngestEvents> {
  readonly config: Config;
  readonly store: BufferStore;
  /** decoded live feed of the current session */
  live: AVQueue | null = null;
  session: Session | null = null;
  connected = false;
  connectedAt = 0;
  info: IngestInfo | null = null;
  lastError: string | null = null;
  /** OBS sent its "stop streaming" commands before the current/last connection closed. */
  endedByObs = false;
  private proc: FfmpegProcess | null = null;
  private stopped = false;
  private relay: net.Server | null = null;
  private obs: net.Socket | null = null;
  /** where the current ffmpeg process listens (127.0.0.1 only) */
  private internalPort = 0;

  constructor({ config, store }: { config: Config; store: BufferStore }) {
    super();
    this.config = config;
    this.store = store;
  }

  get url(): string {
    const { host, port, app } = this.config.ingest;
    return `rtmp://${host === '0.0.0.0' ? '127.0.0.1' : host}:${port}/${app}`;
  }

  thumbGeometry(): { tw: number; th: number; rate: number } {
    const { width, height } = this.config.output;
    // Bound thumbnail memory to ~900 thumbs across the whole buffer.
    const rate = Math.min(
      5,
      Math.max(0.5, 900 / (this.config.bufferMinutes * 60)),
    );
    return {
      tw: Math.round(width / 10) * 2,
      th: Math.round(height / 10) * 2,
      rate,
    };
  }

  start(): void {
    this.stopped = false;
    const { host, port } = this.config.ingest;
    const relay = net.createServer((socket) => this.accept(socket));
    relay.on('error', (err) => {
      this.lastError = `can't listen on ${host}:${port}: ${errorMessage(err)}`;
      console.warn(`[ingest] ${this.lastError}`);
    });
    relay.listen(port, host, () => void this.listen());
    this.relay = relay;
  }

  stop(): void {
    this.stopped = true;
    this.relay?.close();
    this.obs?.destroy();
    killGently(this.proc);
  }

  /** Relays one OBS connection to the waiting ffmpeg process. */
  private accept(obs: net.Socket): void {
    if (this.obs) {
      obs.destroy(); // one publisher at a time
      return;
    }
    this.obs = obs;
    this.endedByObs = false;
    obs.setNoDelay(true);
    obs.pause();
    obs.on('error', () => {});
    const detector = new StopDetector();
    let upstream: net.Socket | null = null;
    obs.on('close', () => {
      if (this.obs === obs) this.obs = null;
      upstream?.end();
    });
    // ffmpeg may still be starting up (it restarts after each connection): retry briefly.
    const connect = (attempt: number) => {
      const up = net.connect(this.internalPort, '127.0.0.1');
      up.on('error', () => {
        if (!upstream && attempt < 50 && !obs.destroyed)
          setTimeout(() => connect(attempt + 1), 50);
        else obs.destroy();
      });
      up.once('connect', () => {
        upstream = up;
        up.setNoDelay(true);
        up.on('close', () => obs.destroy());
        obs.on('data', (chunk: Buffer) => {
          if (detector.push(chunk)) this.endedByObs = true;
        });
        obs.pipe(up);
        up.pipe(obs);
        obs.resume();
      });
    };
    connect(0);
  }

  private async listen(): Promise<void> {
    if (this.stopped) return;
    this.internalPort = await freePort();
    const { width: w, height: h, fps } = this.config.output;
    const { app, key } = this.config.ingest;
    const { tw, th, rate } = this.thumbGeometry();
    const session = this.store.createSession();
    const filter =
      `[0:v:0]fps=${fps}:start_time=0,${scaleChain(w, h)},split=2[full][t];` +
      `[t]fps=${rate}:start_time=0,scale=${tw}:${th}:flags=area,format=yuv420p[th];` +
      `[0:a:0]${AUDIO_CHAIN}[a]`;
    const args = [
      ['-loglevel', 'info'],
      [
        '-listen',
        '1',
        '-i',
        `rtmp://127.0.0.1:${this.internalPort}/${app}/${key}`,
      ],
      ['-filter_complex', filter],
      ['-map', '[full]', '-f', 'rawvideo', 'pipe:1'],
      ['-map', '[a]', '-f', 's16le', 'pipe:3'],
      ['-map', '[th]', '-f', 'rawvideo', 'pipe:5'],
      ['-map', '0:v:0', '-map', '0:a:0?', '-c', 'copy'],
      [
        '-f',
        'segment',
        '-segment_time',
        '1',
        '-segment_format',
        'mpegts',
        '-reset_timestamps',
        '0',
      ],
      ['-segment_list', 'pipe:4', '-segment_list_type', 'csv'],
      [this.store.segmentPath(session, 0).replace('s00000000.ts', 's%08d.ts')],
    ].flat();
    const proc = spawnFfmpeg(args, { fds: 6 });
    this.proc = proc;
    const live = new AVQueue(fps);
    let thumbs = 0;

    const onFirstData = () => {
      if (this.connected || proc !== this.proc) return;
      this.connected = true;
      this.store.register(session);
      this.session = session;
      this.live = live;
      this.connectedAt = Date.now();
      this.emit('connected', session);
    };

    const video = new FrameAssembler(frameBytes(w, h), (frame) => {
      onFirstData();
      session.liveFrames++;
      live.pushVideo(frame);
    });
    proc.stdout.on('data', (d: Buffer) => video.push(d));
    pipe(proc, 3).on('data', (d: Buffer) => {
      onFirstData();
      live.pushAudio(d);
    });

    let listBuf = '';
    const list = pipe(proc, 4);
    list.setEncoding('utf8');
    list.on('data', (d: string) => {
      listBuf += d;
      const lines = listBuf.split('\n');
      listBuf = lines.pop() ?? '';
      for (const line of lines) {
        const fields = line.trim().split(',');
        if (fields.length >= 3)
          this.store.addSegment(session, Number(fields[1]), Number(fields[2]));
      }
    });

    const thumbAsm = new FrameAssembler(frameBytes(tw, th), (frame) => {
      this.store.addThumb(session.base + thumbs / rate, frame);
      thumbs++;
    });
    pipe(proc, 5).on('data', (d: Buffer) => thumbAsm.push(d));

    const stderr = tailStderr(proc, (line) => {
      this.info = parseStreamInfo(line, this.info);
    });

    proc.on('close', (code) => {
      session.ended = true;
      const wasConnected = this.connected && this.session === session;
      if (wasConnected) {
        this.connected = false;
        this.emit('disconnected', session);
      } else {
        this.store.discard(session);
        if (code && !this.stopped) {
          this.lastError = stderr.slice(-3).join(' | ');
          console.warn(`[ingest] ffmpeg exited (${code}): ${this.lastError}`);
        }
      }
      if (proc === this.proc) this.proc = null;
      if (!this.stopped)
        setTimeout(() => void this.listen(), wasConnected ? 200 : 1000);
    });
  }
}
