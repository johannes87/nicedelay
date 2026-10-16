import fs from 'node:fs';
import path from 'node:path';
import type { Config, EncoderChoice } from './types.ts';

export type { Config, Destination, EncoderChoice } from './types.ts';

const ROOT = path.resolve(import.meta.dirname, '..');

export const CONFIG_PATH = process.env.NICEDELAY_CONFIG || path.join(ROOT, 'config.json');

export const DEFAULTS: Config = {
  httpHost: '127.0.0.1',
  httpPort: 8080,
  ingest: { host: '127.0.0.1', port: 1935, app: 'live', key: 'nicedelay' },
  output: {
    width: 1920,
    height: 1080,
    fps: 30,
    videoBitrate: 6000,
    audioBitrate: 160,
    encoder: 'auto',
  },
  destinations: [],
  bufferMinutes: 10,
  delaySeconds: 10,
  transition: { sound: true, soundVolume: 0.6, badge: false },
  reconnectGraceSeconds: 15,
};

type Json = Record<string, unknown>;

function isObject(v: unknown): v is Json {
  return !!v && typeof v === 'object' && !Array.isArray(v);
}

const obj = (v: unknown): Json => (isObject(v) ? v : {});

function toNumber(v: unknown): number {
  if (typeof v === 'number') return v;
  if (typeof v === 'string' && v.trim()) return Number(v);
  return Number.NaN;
}

function num(v: unknown, min: number, max: number, dflt: number): number {
  const n = toNumber(v);
  return Number.isFinite(n) ? Math.min(max, Math.max(min, n)) : dflt;
}

const even = (n: number) => Math.round(n / 2) * 2;
const str = (v: unknown, dflt = '') => (typeof v === 'string' ? v.trim() : dflt);
const bool = (v: unknown, dflt: boolean) => (typeof v === 'boolean' ? v : dflt);

const ALLOWED_FPS = new Set([24, 25, 30, 48, 50, 60]);
const ENCODERS = new Set<string>(['auto', 'h264_videotoolbox', 'libx264']);
const isEncoder = (v: unknown): v is EncoderChoice => typeof v === 'string' && ENCODERS.has(v);

/** Builds a complete, sane Config from untrusted JSON (file or API), falling back to defaults. */
export function validate(input: unknown): Config {
  const c = obj(input);
  const d = DEFAULTS;
  const ingest = obj(c.ingest);
  const output = obj(c.output);
  const transition = obj(c.transition);
  const fps = num(output.fps, 1, 240, d.output.fps);
  const bufferMinutes = num(c.bufferMinutes, 1, 60, d.bufferMinutes);
  const key = str(ingest.key).replaceAll(/[^\w.-]/g, '');
  return {
    httpHost: str(c.httpHost) || d.httpHost,
    httpPort: num(c.httpPort, 1, 65_535, d.httpPort),
    ingest: {
      host: str(ingest.host) || d.ingest.host,
      port: num(ingest.port, 1, 65_535, d.ingest.port),
      app: str(ingest.app).replaceAll(/[^\w.-]/g, '') || d.ingest.app,
      key: key || d.ingest.key,
    },
    output: {
      width: even(num(output.width, 320, 7680, d.output.width)),
      height: even(num(output.height, 180, 4320, d.output.height)),
      fps: ALLOWED_FPS.has(fps) ? fps : d.output.fps,
      videoBitrate: num(output.videoBitrate, 500, 50_000, d.output.videoBitrate),
      audioBitrate: num(output.audioBitrate, 64, 320, d.output.audioBitrate),
      encoder: isEncoder(output.encoder) ? output.encoder : d.output.encoder,
    },
    destinations: (Array.isArray(c.destinations) ? c.destinations : []).filter(isObject).flatMap((x) => {
      const url = str(x.url);
      return url ? [{ name: str(x.name).slice(0, 60), url, key: str(x.key), enabled: x.enabled !== false }] : [];
    }),
    bufferMinutes,
    delaySeconds: num(c.delaySeconds, 1, bufferMinutes * 60, d.delaySeconds),
    transition: {
      sound: bool(transition.sound, d.transition.sound),
      soundVolume: num(transition.soundVolume, 0, 1, d.transition.soundVolume),
      badge: bool(transition.badge, d.transition.badge),
    },
    reconnectGraceSeconds: num(c.reconnectGraceSeconds, 0, 300, d.reconnectGraceSeconds),
  };
}

export function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export function loadConfig(): Config {
  try {
    return validate(JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf8')));
  } catch (err) {
    if (!(err instanceof Error && 'code' in err && err.code === 'ENOENT')) {
      console.warn(`[config] could not read ${CONFIG_PATH}: ${errorMessage(err)}`);
    }
    return validate({});
  }
}

export function saveConfig(cfg: Config): void {
  const tmp = `${CONFIG_PATH}.tmp`;
  fs.writeFileSync(tmp, `${JSON.stringify(cfg, null, 2)}\n`);
  fs.renameSync(tmp, CONFIG_PATH);
}
