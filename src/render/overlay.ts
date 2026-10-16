import { type Canvas, createCanvas, type SKRSContext2D } from '@napi-rs/canvas';
import type { TransitionKind } from '../types.ts';
import { blendRGBA } from './yuv.ts';

export const COLORS = {
  replay: '#FF9F0A',
  live: '#FF453A',
  white: '#FFFFFF',
};

const FONT = '"Helvetica Neue", Helvetica, Arial, sans-serif';

export function formatDelay(seconds: number, { sign = true } = {}): string {
  const s = Math.max(0, Math.round(seconds));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, '0');
  const body = h ? `${h}:${String(m).padStart(2, '0')}:${sec}` : `${m}:${sec}`;
  return sign ? `−${body}` : body;
}

export interface CenterParams {
  kind: TransitionKind;
  alpha: number;
  /** scale of the icon group (pop animation) */
  pop: number;
  /** 0..1 progress of the scrub */
  progress: number;
  timecode: string;
  label: string;
}

export interface TimelineParams {
  alpha: number;
  /** seconds represented by the left end of the track */
  span: number;
  /** delay (seconds) at the knob */
  knob: number;
}

export interface PillParams {
  alpha: number;
  kind: 'replay' | 'live';
  text: string;
}

interface Layer {
  canvas: Canvas;
  ctx: SKRSContext2D;
  w: number;
  h: number;
}

/** Renders the on-stream graphics into small canvases and blends them into yuv frames. */
export class Overlay {
  readonly w: number;
  readonly h: number;
  /** scale relative to 1080p */
  readonly s: number;
  private readonly center: Layer;
  private readonly timeline: Layer;
  private readonly pill: Layer;

  constructor(width: number, height: number) {
    this.w = width;
    this.h = height;
    this.s = height / 1080;
    const s = this.s;
    this.center = layer(Math.round(640 * s), Math.round(470 * s));
    this.timeline = layer(Math.round(width * 0.72), Math.round(130 * s));
    this.pill = layer(Math.round(760 * s), Math.round(104 * s));
  }

  drawCenter(frame: Buffer, p: CenterParams): void {
    if (p.alpha <= 0.01) return;
    const { ctx, w, h } = this.center;
    const s = this.s;
    const accent = p.kind === 'rewind' ? COLORS.replay : COLORS.live;
    ctx.clearRect(0, 0, w, h);
    ctx.save();
    const cx = w / 2;
    const cy = 150 * s;
    ctx.translate(cx, cy);
    ctx.scale(p.pop, p.pop);

    // backdrop disc
    ctx.beginPath();
    ctx.arc(0, 0, 118 * s, 0, Math.PI * 2);
    ctx.fillStyle = 'rgba(10,10,14,0.55)';
    ctx.fill();

    // progress ring: sweeps counter-clockwise for rewind, clockwise for forward
    ctx.lineWidth = 7 * s;
    ctx.lineCap = 'round';
    ctx.strokeStyle = 'rgba(255,255,255,0.18)';
    ctx.beginPath();
    ctx.arc(0, 0, 104 * s, 0, Math.PI * 2);
    ctx.stroke();
    if (p.progress > 0.002) {
      const start = -Math.PI / 2;
      const sweep = Math.PI * 2 * Math.min(1, p.progress);
      ctx.strokeStyle = accent;
      ctx.shadowColor = accent;
      ctx.shadowBlur = 16 * s;
      ctx.beginPath();
      if (p.kind === 'rewind') ctx.arc(0, 0, 104 * s, start, start - sweep, true);
      else ctx.arc(0, 0, 104 * s, start, start + sweep, false);
      ctx.stroke();
      ctx.shadowBlur = 0;
    }

    // double-triangle icon, nudged in the direction of travel
    const dir = p.kind === 'rewind' ? -1 : 1;
    const nudge = Math.sin(p.progress * Math.PI * 6) * 5 * s * dir;
    ctx.fillStyle = COLORS.white;
    ctx.shadowColor = 'rgba(0,0,0,0.5)';
    ctx.shadowBlur = 10 * s;
    const th = 44 * s; // half height
    const tw = 42 * s;
    for (const off of [-tw / 2 - 1 * s, tw / 2 + 1 * s]) {
      const x = off + nudge;
      ctx.beginPath();
      ctx.moveTo(x + dir * (tw / 2), 0);
      ctx.lineTo(x - dir * (tw / 2), -th);
      ctx.lineTo(x - dir * (tw / 2), th);
      ctx.closePath();
      ctx.fill();
    }
    ctx.restore();

    // timecode + label (no pop scaling, just fade)
    ctx.save();
    ctx.textAlign = 'center';
    ctx.textBaseline = 'alphabetic';
    ctx.shadowColor = 'rgba(0,0,0,0.65)';
    ctx.shadowBlur = 14 * s;
    ctx.font = `700 ${Math.round(86 * s)}px ${FONT}`;
    ctx.fillStyle = accent;
    ctx.fillText(p.timecode, cx, 375 * s);
    ctx.font = `600 ${Math.round(25 * s)}px ${FONT}`;
    ctx.fillStyle = 'rgba(255,255,255,0.88)';
    ctx.letterSpacing = `${Math.round(5 * s)}px`;
    ctx.fillText(p.label, cx, 425 * s);
    ctx.restore();

    blendLayer(frame, this, this.center, Math.round((this.w - w) / 2), Math.round(this.h * 0.5 - 220 * s), p.alpha);
  }

  drawTimeline(frame: Buffer, p: TimelineParams): void {
    if (p.alpha <= 0.01) return;
    const { ctx, w, h } = this.timeline;
    const s = this.s;
    ctx.clearRect(0, 0, w, h);
    const left = 30 * s;
    const right = w - 30 * s;
    const y = 82 * s;
    const xOf = (d: number) => right - (Math.min(d, p.span) / p.span) * (right - left);
    const accent = p.knob > 0.05 ? COLORS.replay : COLORS.live;

    ctx.lineCap = 'round';
    ctx.lineWidth = 8 * s;
    ctx.strokeStyle = 'rgba(255,255,255,0.28)';
    ctx.shadowColor = 'rgba(0,0,0,0.5)';
    ctx.shadowBlur = 8 * s;
    ctx.beginPath();
    ctx.moveTo(left, y);
    ctx.lineTo(right, y);
    ctx.stroke();
    ctx.shadowBlur = 0;

    // the stretch viewers are behind live
    const kx = xOf(p.knob);
    if (right - kx > 1) {
      ctx.strokeStyle = accent;
      ctx.beginPath();
      ctx.moveTo(kx, y);
      ctx.lineTo(right, y);
      ctx.stroke();
    }

    // live marker
    ctx.fillStyle = COLORS.live;
    ctx.beginPath();
    ctx.arc(right, y, 10 * s, 0, Math.PI * 2);
    ctx.fill();
    ctx.font = `700 ${Math.round(24 * s)}px ${FONT}`;
    ctx.textAlign = 'right';
    ctx.letterSpacing = `${Math.round(3 * s)}px`;
    ctx.fillStyle = COLORS.white;
    ctx.shadowColor = 'rgba(0,0,0,0.6)';
    ctx.shadowBlur = 8 * s;
    ctx.fillText('LIVE', right + 12 * s, y - 26 * s);

    // knob
    ctx.shadowBlur = 14 * s;
    ctx.fillStyle = COLORS.white;
    ctx.beginPath();
    ctx.arc(kx, y, 17 * s, 0, Math.PI * 2);
    ctx.fill();
    ctx.shadowBlur = 0;
    ctx.fillStyle = accent;
    ctx.beginPath();
    ctx.arc(kx, y, 7 * s, 0, Math.PI * 2);
    ctx.fill();

    blendLayer(frame, this, this.timeline, Math.round((this.w - w) / 2), Math.round(this.h * 0.86 - y), p.alpha);
  }

  /** Corner status pill. */
  drawPill(frame: Buffer, p: PillParams): void {
    if (p.alpha <= 0.01) return;
    const { ctx, h } = this.pill;
    const s = this.s;
    ctx.clearRect(0, 0, this.pill.w, h);
    ctx.font = `700 ${Math.round(34 * s)}px ${FONT}`;
    ctx.letterSpacing = `${Math.round(3 * s)}px`;
    const textW = ctx.measureText(p.text).width;
    const pad = 26 * s;
    const iconW = 38 * s;
    const pw = Math.min(this.pill.w - 4 * s, pad * 2 + iconW + textW);
    const ph = 72 * s;
    const py = (h - ph) / 2;
    const accent = p.kind === 'replay' ? COLORS.replay : COLORS.live;

    ctx.fillStyle = 'rgba(12,12,16,0.68)';
    roundRect(ctx, 2 * s, py, pw, ph, ph / 2);
    ctx.fill();

    const ix = 2 * s + pad;
    const iy = h / 2;
    ctx.fillStyle = accent;
    if (p.kind === 'replay') {
      for (const off of [0, 14 * s]) {
        ctx.beginPath();
        ctx.moveTo(ix + off, iy);
        ctx.lineTo(ix + off + 15 * s, iy - 13 * s);
        ctx.lineTo(ix + off + 15 * s, iy + 13 * s);
        ctx.closePath();
        ctx.fill();
      }
    } else {
      ctx.shadowColor = accent;
      ctx.shadowBlur = 12 * s;
      ctx.beginPath();
      ctx.arc(ix + 12 * s, iy, 11 * s, 0, Math.PI * 2);
      ctx.fill();
      ctx.shadowBlur = 0;
    }
    ctx.fillStyle = COLORS.white;
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'left';
    ctx.fillText(p.text, ix + iconW, iy + 1 * s);

    blendLayer(frame, this, this.pill, Math.round(44 * s), Math.round(36 * s), p.alpha, Math.ceil(pw + 6 * s));
  }
}

function layer(w: number, h: number): Layer {
  w += w & 1;
  h += h & 1;
  const canvas = createCanvas(w, h);
  return { canvas, ctx: canvas.getContext('2d'), w, h };
}

function blendLayer(frame: Buffer, ov: Overlay, l: Layer, x: number, y: number, alpha: number, usedW = l.w): void {
  const width = Math.min(l.w, usedW + (usedW & 1));
  const img = l.ctx.getImageData(0, 0, width, l.h);
  blendRGBA(frame, ov.w, ov.h, img.data, width, l.h, x, y, alpha);
}

function roundRect(ctx: SKRSContext2D, x: number, y: number, w: number, h: number, r: number): void {
  ctx.beginPath();
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}
