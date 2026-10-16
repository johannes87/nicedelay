// In-place pixel work on yuv420p frames (BT.709, limited range). Every function writes
// into a caller-provided destination so source frames are never mutated.

export function frameSize(w: number, h: number): number {
  return (w * h * 3) / 2;
}

function planeOffsets(w: number, h: number) {
  const ys = w * h;
  return { ys, cs: ys / 4, u: ys, v: ys + ys / 4 };
}

interface AxisTable {
  i0: Int32Array;
  i1: Int32Array;
  wt: Int32Array;
}

const tables = new Map<string, AxisTable>();
function axisTable(src: number, dst: number): AxisTable {
  const key = `${src}:${dst}`;
  let t = tables.get(key);
  if (t) return t;
  const i0 = new Int32Array(dst);
  const i1 = new Int32Array(dst);
  const wt = new Int32Array(dst);
  const ratio = src / dst;
  for (let i = 0; i < dst; i++) {
    let f = (i + 0.5) * ratio - 0.5;
    if (f < 0) f = 0;
    const a = Math.min(src - 1, Math.floor(f));
    i0[i] = a;
    i1[i] = Math.min(src - 1, a + 1);
    wt[i] = Math.round((f - a) * 256);
  }
  t = { i0, i1, wt };
  tables.set(key, t);
  return t;
}

// Bilinear scale of one plane with a fused linear tone map: out = v * mul/256 + add.
function scalePlane(
  src: Uint8Array,
  so: number,
  sw: number,
  sh: number,
  dst: Uint8Array,
  dOff: number,
  dw: number,
  dh: number,
  mul: number,
  add: number,
): void {
  const tx = axisTable(sw, dw);
  const ty = axisTable(sh, dh);
  const { i0: x0, i1: x1, wt: wx } = tx;
  for (let y = 0; y < dh; y++) {
    const r0 = so + ty.i0[y] * sw;
    const r1 = so + ty.i1[y] * sw;
    const wy = ty.wt[y];
    let o = dOff + y * dw;
    for (let x = 0; x < dw; x++, o++) {
      const ax = x0[x];
      const bx = x1[x];
      const w = wx[x];
      const a = src[r0 + ax];
      const top = (a << 8) + (src[r0 + bx] - a) * w;
      const c = src[r1 + ax];
      const bot = (c << 8) + (src[r1 + bx] - c) * w;
      const v = ((top << 8) + (bot - top) * wy) >> 16;
      dst[o] = (v * mul + add) >> 8;
    }
  }
}

export interface Tone {
  /** 0..1 brightness factor */
  luma?: number;
  /** 0..1 saturation factor */
  sat?: number;
}

function toneCoeffs({ luma = 1, sat = 1 }: Tone) {
  const ly = Math.round(luma * 256);
  const cs = Math.round(sat * 256);
  return { ly, ay: Math.round(16 * (256 - ly)), cs, ac: Math.round(128 * (256 - cs)) };
}

export function scaleFrame(
  src: Buffer,
  sw: number,
  sh: number,
  dst: Buffer,
  dw: number,
  dh: number,
  tone: Tone = {},
): void {
  const s = planeOffsets(sw, sh);
  const d = planeOffsets(dw, dh);
  const { ly, ay, cs, ac } = toneCoeffs(tone);
  scalePlane(src, 0, sw, sh, dst, 0, dw, dh, ly, ay + 128);
  scalePlane(src, s.u, sw / 2, sh / 2, dst, d.u, dw / 2, dh / 2, cs, ac + 128);
  scalePlane(src, s.v, sw / 2, sh / 2, dst, d.v, dw / 2, dh / 2, cs, ac + 128);
}

export function toneFrame(src: Buffer, dst: Buffer, w: number, h: number, tone: Tone = {}): void {
  const { ys, cs: csize } = planeOffsets(w, h);
  const { ly, ay, cs, ac } = toneCoeffs(tone);
  if (ly === 256 && cs === 256) {
    src.copy(dst, 0, 0, frameSize(w, h));
    return;
  }
  for (let i = 0; i < ys; i++) dst[i] = (src[i] * ly + ay + 128) >> 8;
  const end = ys + 2 * csize;
  for (let i = ys; i < end; i++) dst[i] = (src[i] * cs + ac + 128) >> 8;
}

// dst = a*(1-t) + b*t
export function crossfade(a: Buffer, b: Buffer, dst: Buffer, t: number): void {
  const w = Math.round(Math.min(1, Math.max(0, t)) * 256);
  if (w === 0) {
    a.copy(dst);
    return;
  }
  if (w === 256) {
    b.copy(dst);
    return;
  }
  const n = dst.length;
  for (let i = 0; i < n; i++) {
    const x = a[i];
    dst[i] = x + (((b[i] - x) * w + 128) >> 8);
  }
}

// Small deterministic PRNG so effects are stable per frame.
export function rng(seed: number): () => number {
  let s = seed >>> 0 || 1;
  return () => {
    s ^= s << 13;
    s ^= s >>> 17;
    s ^= s << 5;
    return (s >>> 0) / 4294967296;
  };
}

// VHS-style tape artifacts: horizontally displaced bands, a rolling noise bar and
// scanlines. `strength` 0..1, `seed` changes the pattern per frame.
export function tapeEffect(buf: Buffer, w: number, h: number, strength: number, seed: number, rollPos: number): void {
  if (strength <= 0) return;
  const rand = rng(seed);
  const { u, v } = planeOffsets(w, h);
  const cw = w / 2;
  // displaced bands
  const bands = 2 + Math.floor(rand() * 3);
  for (let b = 0; b < bands; b++) {
    const bh = Math.max(2, Math.floor(h * (0.01 + rand() * 0.05)));
    const y0 = Math.floor(rand() * (h - bh)) & ~1;
    const dx = (Math.round((rand() - 0.5) * w * 0.06 * strength) >> 1) << 1;
    if (!dx) continue;
    for (let y = y0; y < y0 + bh; y++) shiftRow(buf, y * w, w, dx);
    for (let y = y0 >> 1; y < (y0 + bh) >> 1; y++) {
      shiftRow(buf, u + y * cw, cw, dx >> 1);
      shiftRow(buf, v + y * cw, cw, dx >> 1);
    }
  }
  // rolling noise bar (tracking noise)
  const barH = Math.floor(h * 0.07);
  const barY = Math.floor(rollPos * (h + barH)) - barH;
  const amp = 90 * strength;
  for (let y = Math.max(0, barY); y < Math.min(h, barY + barH); y++) {
    const edge = 1 - Math.abs((y - barY) / barH - 0.5) * 2; // 0..1..0 across the bar
    const k = edge * amp;
    let o = y * w;
    const shift = Math.round((rand() - 0.5) * 16 * strength * edge);
    if (shift) shiftRow(buf, o, w, shift);
    for (let x = 0; x < w; x++, o++) {
      const n = buf[o] + (rand() - 0.5) * k * 2 + k * 0.3;
      buf[o] = n < 16 ? 16 : n > 235 ? 235 : n;
    }
  }
  // scanlines: darken every other line pair a little
  const sl = Math.round(256 - 40 * strength);
  for (let y = 0; y < h; y += 4) {
    let o = y * w;
    for (let r = 0; r < 2; r++) {
      const end = o + w;
      for (; o < end; o++) buf[o] = (((buf[o] - 16) * sl) >> 8) + 16;
    }
  }
}

function shiftRow(buf: Buffer, off: number, len: number, dx: number): void {
  if (dx > 0) buf.copyWithin(off + dx, off, off + len - dx);
  else buf.copyWithin(off, off - dx, off + len);
}

// Alpha-blend an RGBA image (non-premultiplied, e.g. canvas getImageData) onto a frame.
export function blendRGBA(
  buf: Buffer,
  w: number,
  h: number,
  rgba: Uint8ClampedArray,
  rw: number,
  rh: number,
  x0: number,
  y0: number,
  opacity = 1,
): void {
  x0 &= ~1;
  y0 &= ~1;
  const { u, v } = planeOffsets(w, h);
  const cw = w / 2;
  const op = Math.round(opacity * 256);
  if (op <= 0) return;
  const xa = Math.max(0, -x0);
  const ya = Math.max(0, -y0);
  const xb = Math.min(rw, w - x0);
  const yb = Math.min(rh, h - y0);
  // luma
  for (let y = ya; y < yb; y++) {
    let si = (y * rw + xa) * 4;
    let di = (y0 + y) * w + x0 + xa;
    for (let x = xa; x < xb; x++, si += 4, di++) {
      const a = (rgba[si + 3] * op) >> 8;
      if (!a) continue;
      const yy = 16 + ((rgba[si] * 47 + rgba[si + 1] * 157 + rgba[si + 2] * 16) >> 8);
      const d = buf[di];
      buf[di] = d + ((((yy - d) * a + 127) / 255) | 0);
    }
  }
  // chroma (one sample per 2x2 block)
  for (let y = ya & ~1; y < yb - 1; y += 2) {
    for (let x = xa & ~1; x < xb - 1; x += 2) {
      let sa = 0;
      let sr = 0;
      let sg = 0;
      let sb = 0;
      for (let k = 0; k < 4; k++) {
        const si = ((y + (k >> 1)) * rw + x + (k & 1)) * 4;
        const a = rgba[si + 3];
        sa += a;
        sr += rgba[si] * a;
        sg += rgba[si + 1] * a;
        sb += rgba[si + 2] * a;
      }
      if (!sa) continue;
      const r = sr / sa;
      const g = sg / sa;
      const b = sb / sa;
      const a = (((sa >> 2) * op) >> 8) / 255;
      const cb = 128 + (-0.1006 * r - 0.3386 * g + 0.4392 * b);
      const cr = 128 + (0.4392 * r - 0.3989 * g - 0.0403 * b);
      const ci = ((y0 + y) >> 1) * cw + ((x0 + x) >> 1);
      buf[u + ci] += (cb - buf[u + ci]) * a;
      buf[v + ci] += (cr - buf[v + ci]) * a;
    }
  }
}
