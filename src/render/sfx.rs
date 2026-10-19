//! Synthesized tape-transport whoosh: band-passed noise plus a buzzy chirp whose pitch
//! falls for rewind and rises for fast-forward, panned in the direction of travel.

use std::f64::consts::PI;

use crate::media::queue::AUDIO_RATE;
use crate::render::effects::rng;
use crate::state::TransitionKind;

fn to_i16(v: f64) -> i16 {
    v.round().clamp(-32768.0, 32767.0) as i16
}

/// Interleaved s16 stereo at 48 kHz.
pub fn tape_whoosh(kind: TransitionKind, seconds: f64, volume: f64) -> Vec<i16> {
    let rate = f64::from(AUDIO_RATE);
    let n = (seconds * rate).round() as usize;
    let mut out = vec![0i16; n * 2];
    let rewind = kind == TransitionKind::Rewind;
    let mut rand = rng(if rewind { 7 } else { 11 });
    let (f0, f1): (f64, f64) = if rewind { (2600.0, 420.0) } else { (420.0, 2600.0) };
    let gain = 0.32 * volume;
    // band-pass biquad coefficients (b1 is always 0 for this filter)
    let (mut b0, mut b2, mut a1, mut a2) = (0.0, 0.0, 0.0, 0.0);
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    let mut phase = 0.0;
    let attack = 0.04 * rate;
    let release = (0.25 * rate).min(n as f64 / 3.0);
    for i in 0..n {
        let u = i as f64 / n as f64;
        let ease = u * u * (3.0 - 2.0 * u);
        let f = f0 * (f1 / f0).powf(ease);
        if i & 31 == 0 {
            // RBJ band-pass, constant 0 dB peak gain
            let w = 2.0 * PI * f / rate;
            let alpha = w.sin() / (2.0 * 2.2);
            let a0 = 1.0 + alpha;
            b0 = alpha / a0;
            b2 = -alpha / a0;
            a1 = -2.0 * w.cos() / a0;
            a2 = (1.0 - alpha) / a0;
        }
        let x = f64::from(rand()) * 2.0 - 1.0;
        let y = b0 * x + b2 * x2 - a1 * y1 - a2 * y2;
        x2 = x1;
        x1 = x;
        y2 = y1;
        y1 = y;
        phase += f * 0.5 / rate;
        let saw = (phase % 1.0) * 2.0 - 1.0;
        let flutter = 0.65 + 0.35 * (2.0 * PI * 23.0 * i as f64 / rate).sin();
        let mut env = 1.0;
        let fi = i as f64;
        if fi < attack {
            env = fi / attack;
        }
        if fi > n as f64 - release {
            env = f64::min(env, (n as f64 - fi) / release);
        }
        let s = (y * 1.6 + saw * 0.12 * flutter) * env * gain;
        let pan = if rewind { 1.0 - ease } else { ease }; // 0 = left, 1 = right
        out[i * 2] = to_i16(s * (pan * PI / 2.0).cos() * 1.3 * 32767.0);
        out[i * 2 + 1] = to_i16(s * (pan * PI / 2.0).sin() * 1.3 * 32767.0);
    }
    out
}

/// Mixes `src` (from sample `src_off`) into `dst`, with `dst_gain` applied to `dst` first.
pub fn mix_into(dst: &mut [i16], dst_gain: f64, src: Option<&[i16]>, src_off: usize) {
    for (i, d) in dst.iter_mut().enumerate() {
        let mut v = f64::from(*d) * dst_gain;
        if let Some(s) = src.and_then(|s| s.get(src_off + i)) {
            v += f64::from(*s);
        }
        *d = to_i16(v);
    }
}
