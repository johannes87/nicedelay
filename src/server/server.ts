import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { styleText } from 'node:util';
import { type Config, errorMessage, loadConfig, saveConfig, validate } from './config.ts';
import { BufferStore } from './media/buffer.ts';
import { pickEncoder } from './media/encoder.ts';
import { FFMPEG, hasEncoder } from './media/ffmpeg.ts';
import { Ingest } from './media/ingest.ts';
import { Playout } from './playout.ts';
import type { AppState, CommandResult, ConfigResponse } from '../shared/types.ts';

const WEB_ROOT = path.resolve(import.meta.dirname, '../..', 'dist', 'web');
const MIME: Record<string, string> = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.woff2': 'font/woff2',
};

/** Settings that need the pipeline rebuilt (frame size, fps, ingest socket). */
const STRUCTURAL = ['output', 'ingest'] as const;

type Handler = (body: Record<string, unknown>) => unknown;

class App {
  config: Config;
  private pending: Config | null = null;
  private store!: BufferStore;
  private ingest!: Ingest;
  private playout!: Playout;
  private readonly previewClients = new Set<http.ServerResponse>();
  private readonly sseClients = new Set<http.ServerResponse>();
  private lastState = '';

  constructor() {
    this.config = loadConfig();
    this.build();
  }

  private build(): void {
    const c = this.config;
    this.store = new BufferStore({ fps: c.output.fps, bufferSeconds: c.bufferMinutes * 60 });
    this.ingest = new Ingest({ config: c, store: this.store });
    this.playout = new Playout({ config: c, store: this.store, ingest: this.ingest });
    this.ingest.on('connected', () => {
      const info = this.ingest.info;
      const details = info ? ` (${info.codec} ${info.width}x${info.height} ${info.fps}fps)` : '';
      log('green', `OBS connected${details}`);
      this.broadcastState(true);
    });
    this.ingest.on('disconnected', () => {
      log(
        'yellow',
        this.ingest.endedByObs ? 'OBS stopped streaming' : 'OBS disconnected (waiting for it to reconnect)',
      );
      this.broadcastState(true);
    });
    this.playout.on('preview', (jpeg) => this.sendPreview(jpeg));
    this.playout.on('transition', (t) => {
      log('cyan', t.to > 0 ? `delay → ${t.to}s` : 'back to live');
      this.sendEvent('transition', t);
    });
    this.playout.on('change', () => {
      this.broadcastState(true);
      if (this.playout.mode === 'idle') this.applyPending();
    });
    this.ingest.start();
  }

  /** Apply structural settings once nothing is streaming. */
  private applyPending(): void {
    if (!this.pending || this.playout.mode !== 'idle' || this.ingest.connected) return;
    log('cyan', 'applying new output/ingest settings');
    this.ingest.stop();
    this.ingest.removeAllListeners();
    this.playout.removeAllListeners();
    this.store.dispose();
    this.config = this.pending;
    this.pending = null;
    this.build();
  }

  private updateConfig(body: Record<string, unknown>): ConfigResponse {
    const next = validate({ ...this.config, ...body });
    saveConfig(next);
    const structural = STRUCTURAL.some((k) => JSON.stringify(next[k]) !== JSON.stringify(this.config[k]));
    // Everything else is read live from the shared config object.
    for (const k of Object.keys(next) as (keyof Config)[]) {
      if (!(STRUCTURAL as readonly string[]).includes(k)) Object.assign(this.config, { [k]: next[k] });
    }
    this.store.bufferSeconds = next.bufferMinutes * 60;
    this.pending = structural ? next : null;
    this.applyPending();
    this.broadcastState(true);
    return { config: this.pending ?? this.config, restartPending: !!this.pending };
  }

  private state(): AppState {
    const c = this.config;
    return {
      ingest: {
        connected: this.ingest.connected,
        server: this.ingest.url,
        key: c.ingest.key,
        info: this.ingest.connected ? this.ingest.info : null,
      },
      buffer: { recorded: Math.floor(this.store.recorded), capacity: c.bufferMinutes * 60 },
      playout: this.playout.state(),
      delaySeconds: c.delaySeconds,
      restartPending: !!this.pending,
    };
  }

  // ------------------------------------------------------------ push channels

  private sendEvent(event: string, data: unknown): void {
    const msg = `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
    for (const res of this.sseClients) res.write(msg);
  }

  private broadcastState(force = false): void {
    const json = JSON.stringify(this.state());
    if (!force && json === this.lastState) return;
    this.lastState = json;
    for (const res of this.sseClients) res.write(`event: state\ndata: ${json}\n\n`);
  }

  private sendPreview(jpeg: Buffer): void {
    for (const res of this.previewClients) {
      if (res.writableLength > 1_000_000) continue; // slow client: skip frames
      res.write(`--frame\r\nContent-Type: image/jpeg\r\nContent-Length: ${jpeg.length}\r\n\r\n`);
      res.write(jpeg);
      res.write('\r\n');
    }
  }

  // ------------------------------------------------------------ http

  private readonly routes: { method: string; pattern: URLPattern; fn: Handler }[] = (
    [
      ['GET', '/api/state', () => this.state()],
      ['POST', '/api/delay', (b) => this.command(this.playout.setDelay(b.seconds ?? this.config.delaySeconds))],
      ['POST', '/api/live', () => this.command(this.playout.goLive())],
      ['POST', '/api/toggle', () => this.command(this.playout.toggle())],
      [
        'GET',
        '/api/config',
        (): ConfigResponse => ({ config: this.pending ?? this.config, restartPending: !!this.pending }),
      ],
      ['PUT', '/api/config', (b) => this.updateConfig(b)],
    ] satisfies [string, string, Handler][]
  ).map(([method, pathname, fn]) => ({ method, pattern: new URLPattern({ pathname }), fn }));

  private command(result: CommandResult): CommandResult {
    this.broadcastState(true);
    return result;
  }

  async handle(req: http.IncomingMessage, res: http.ServerResponse): Promise<void> {
    const url = new URL(req.url ?? '/', 'http://localhost');
    try {
      if (url.pathname === '/api/events') return this.openEvents(req, res);
      if (url.pathname === '/preview.mjpg') return this.openPreview(req, res);
      const matches = this.routes.filter((r) => r.pattern.test(url));
      if (matches.length) {
        const route = matches.find((r) => r.method === req.method);
        if (!route) return send(res, 405, { error: 'method not allowed' });
        const body = req.method === 'GET' ? {} : await readJson(req);
        const result = route.fn(body);
        return send(res, (result as CommandResult | undefined)?.ok === false ? 409 : 200, result);
      }
      if (req.method === 'GET' || req.method === 'HEAD') return serveStatic(url.pathname, res);
      send(res, 404, { error: 'not found' });
    } catch (err) {
      console.error(err);
      send(res, 500, { error: errorMessage(err) });
    }
  }

  private openEvents(req: http.IncomingMessage, res: http.ServerResponse): void {
    res.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', Connection: 'keep-alive' });
    res.write(`retry: 1000\nevent: state\ndata: ${JSON.stringify(this.state())}\n\n`);
    this.sseClients.add(res);
    req.on('close', () => this.sseClients.delete(res));
  }

  private openPreview(req: http.IncomingMessage, res: http.ServerResponse): void {
    res.writeHead(200, {
      'Content-Type': 'multipart/x-mixed-replace; boundary=frame',
      'Cache-Control': 'no-cache, no-store',
      Connection: 'close',
    });
    this.previewClients.add(res);
    req.on('close', () => this.previewClients.delete(res));
  }

  listen(): void {
    const { httpHost, httpPort } = this.config;
    const server = http.createServer((req, res) => void this.handle(req, res));
    server.listen(httpPort, httpHost, () => {
      const c = this.config;
      const dests = c.destinations.filter((d) => d.enabled);
      const host = httpHost === '0.0.0.0' ? 'localhost' : httpHost;
      log('bold', '\n  nicedelay');
      const dashboard = `http://${host}:${httpPort}`;
      console.log(`  Dashboard   ${styleText('cyan', dashboard)}`);
      console.log(
        `  OBS server  ${styleText('cyan', this.ingest.url)}   stream key ${styleText('cyan', c.ingest.key)}`,
      );
      console.log(
        `  Output      ${c.output.width}x${c.output.height}@${c.output.fps}, ` +
          `${pickEncoder(c.output.encoder)}, buffer ${c.bufferMinutes} min`,
      );
      const to = dests.length
        ? dests.map((d) => d.name || d.url).join(', ')
        : styleText('yellow', 'nowhere yet (preview only)');
      console.log(`  Sending to  ${to}\n`);
    });
    setInterval(() => this.broadcastState(), 250).unref();
  }

  shutdown(): void {
    this.ingest.stop();
    this.playout.stopOutput();
    setTimeout(() => {
      this.store.dispose();
      process.exit(0);
    }, 500);
  }
}

function log(color: Parameters<typeof styleText>[0], msg: string): void {
  console.log(styleText(color, msg));
}

function send(res: http.ServerResponse, status: number, body: unknown): void {
  res.writeHead(status, { 'Content-Type': 'application/json', 'Cache-Control': 'no-store' });
  res.end(JSON.stringify(body ?? {}));
}

async function readJson(req: http.IncomingMessage): Promise<Record<string, unknown>> {
  let raw = '';
  for await (const chunk of req) {
    raw += String(chunk);
    if (raw.length > 100_000) throw new Error('body too large');
  }
  try {
    const parsed: unknown = raw.trim() ? JSON.parse(raw) : {};
    return parsed && typeof parsed === 'object' ? (parsed as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

function serveStatic(pathname: string, res: http.ServerResponse): void {
  const rel = pathname === '/' ? 'index.html' : decodeURIComponent(pathname).replace(/^\/+/, '');
  const file = path.resolve(WEB_ROOT, rel);
  if (!file.startsWith(WEB_ROOT + path.sep)) return send(res, 403, { error: 'forbidden' });
  fs.readFile(file, (err, data) => {
    if (err) {
      if (rel === 'index.html') {
        res.writeHead(503, { 'Content-Type': 'text/plain' });
        res.end('Dashboard not built yet. Run `npm run build`.');
        return;
      }
      return send(res, 404, { error: 'not found' });
    }
    const immutable = rel.startsWith('assets/');
    res.writeHead(200, {
      'Content-Type': MIME[path.extname(file)] ?? 'application/octet-stream',
      'Cache-Control': immutable ? 'public, max-age=31536000, immutable' : 'no-cache',
    });
    res.end(data);
  });
}

if (!hasEncoder('aac')) {
  console.error(`ffmpeg not found or unusable (${FFMPEG}). Install it, e.g. \`brew install ffmpeg\`.`);
  process.exit(1);
}

const app = new App();
app.listen();
process.on('SIGINT', () => app.shutdown());
process.on('SIGTERM', () => app.shutdown());
