import { AUDIO_RATE } from '../media/queues.ts';
import { rng } from './yuv.ts';

// Synthesized tape-transport whoosh: band-passed noise plus a buzzy chirp whose pitch
// falls for rewind and rises for fast-forward, panned in the direction of travel.
// Returns s16le stereo PCM.
export function tapeWhoosh(
  kind: 'rewind' | 'forward',
  seconds: number,
  volume = 0.6,
): Buffer {
  const n = Math.round(seconds * AUDIO_RATE);
  const out = Buffer.alloc(n * 4);
  const rand = rng(kind === 'rewind' ? 7 : 11);
  const [f0, f1] = kind === 'rewind' ? [2600, 420] : [420, 2600];
  const gain = 0.32 * volume;
  // band-pass biquad coefficients (b1 is always 0 for this filter)
  let b0 = 0;
  let b2 = 0;
  let a1 = 0;
  let a2 = 0;
  let x1 = 0;
  let x2 = 0;
  let y1 = 0;
  let y2 = 0;
  let phase = 0;
  const attack = 0.04 * AUDIO_RATE;
  const release = Math.min(0.25 * AUDIO_RATE, n / 3);
  for (let i = 0; i < n; i++) {
    const u = i / n;
    const ease = u * u * (3 - 2 * u);
    const f = f0 * (f1 / f0) ** ease;
    if ((i & 31) === 0) {
      // RBJ band-pass, constant 0 dB peak gain
      const w = (2 * Math.PI * f) / AUDIO_RATE;
      const q = 2.2;
      const alpha = Math.sin(w) / (2 * q);
      const a0 = 1 + alpha;
      b0 = alpha / a0;
      b2 = -alpha / a0;
      a1 = (-2 * Math.cos(w)) / a0;
      a2 = (1 - alpha) / a0;
    }
    const x = rand() * 2 - 1;
    const y = b0 * x + b2 * x2 - a1 * y1 - a2 * y2;
    x2 = x1;
    x1 = x;
    y2 = y1;
    y1 = y;
    phase += (f * 0.5) / AUDIO_RATE;
    const saw = (phase % 1) * 2 - 1;
    const flutter = 0.65 + 0.35 * Math.sin((2 * Math.PI * 23 * i) / AUDIO_RATE);
    let env = 1;
    if (i < attack) env = i / attack;
    if (i > n - release) env = Math.min(env, (n - i) / release);
    const s = (y * 1.6 + saw * 0.12 * flutter) * env * gain;
    const pan = kind === 'rewind' ? 1 - ease : ease; // 0 = left, 1 = right
    const l = s * Math.cos((pan * Math.PI) / 2) * 1.3;
    const r = s * Math.sin((pan * Math.PI) / 2) * 1.3;
    out.writeInt16LE(clamp16(l), i * 4);
    out.writeInt16LE(clamp16(r), i * 4 + 2);
  }
  return out;
}

const toInt16 = (v: number) => Math.min(32767, Math.max(-32768, Math.round(v)));

function clamp16(v: number): number {
  return toInt16(v * 32767);
}

/** Mix s16le stereo `src` (from byte offset `srcOff`) into `dst`, with gain applied to dst first. */
export function mixInto(
  dst: Buffer,
  dstGain: number,
  src: Buffer | null,
  srcOff: number,
): void {
  for (let i = 0; i < dst.length; i += 2) {
    let v = dst.readInt16LE(i) * dstGain;
    const j = srcOff + i;
    if (src && j < src.length) v += src.readInt16LE(j);
    dst.writeInt16LE(toInt16(v), i);
  }
}
