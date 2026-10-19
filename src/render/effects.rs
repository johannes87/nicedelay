//! Picture effects for the transitions, all drawn with Skia: dimming, and the VHS-style
//! tape artifacts.

use skia_safe::gradient::shaders::linear_gradient;
use skia_safe::gradient::{Colors, Gradient, Interpolation};
use skia_safe::{
    BlendMode, Canvas, Color4f, ColorFilter, Image, Paint, Rect, SamplingOptions, Shader, TileMode, canvas,
    color_filters, shaders,
};

/// Small deterministic PRNG so effects are stable per frame.
pub fn rng(seed: u32) -> impl FnMut() -> f32 {
    let mut s = if seed == 0 { 1 } else { seed };
    move || {
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        s as f32 / 4_294_967_296.0
    }
}

/// Darkens (`luma`, 0..1) and desaturates (`sat`, 0..1) a picture, with BT.709 weights.
pub fn tone(luma: f32, sat: f32) -> ColorFilter {
    let (r, g, b) = (0.2126 * (1.0 - sat), 0.7152 * (1.0 - sat), 0.0722 * (1.0 - sat));
    let m = [
        (r + sat) * luma,
        g * luma,
        b * luma,
        0.0,
        0.0, //
        r * luma,
        (g + sat) * luma,
        b * luma,
        0.0,
        0.0, //
        r * luma,
        g * luma,
        (b + sat) * luma,
        0.0,
        0.0, //
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
    ];
    color_filters::matrix_row_major(&m, None)
}

/// Draws `image` over the whole canvas, toned.
pub fn draw_toned(canvas: &Canvas, image: &Image, w: f32, h: f32, luma: f32, sat: f32, alpha: f32) {
    let mut paint = Paint::default();
    if luma < 0.999 || sat < 0.999 {
        paint.set_color_filter(tone(luma, sat));
    }
    paint.set_alpha_f(alpha);
    let src = Rect::from_wh(image.width() as f32, image.height() as f32);
    canvas.draw_image_rect_with_sampling_options(
        image,
        Some((&src, canvas::SrcRectConstraint::Fast)),
        Rect::from_wh(w, h),
        SamplingOptions::from(skia_safe::FilterMode::Linear),
        &paint,
    );
}

/// VHS-style tape artifacts over what's on the canvas (`picture` is a snapshot of it):
/// horizontally displaced bands, a rolling noise bar and scanlines. `strength` 0..1,
/// `seed` changes the pattern per frame, `roll` 0..1 is where the bar is.
pub fn tape(canvas: &Canvas, picture: &Image, w: f32, h: f32, strength: f32, seed: u32, roll: f32) {
    if strength <= 0.0 {
        return;
    }
    let mut rand = rng(seed);
    // A few horizontal bands of the picture shifted sideways.
    let bands = 2 + (rand() * 3.0) as u32;
    let plain = Paint::default();
    for _ in 0..bands {
        let bh = (h * (0.01 + rand() * 0.05)).floor().max(2.0);
        let y0 = (rand() * (h - bh)).floor();
        let dx = ((rand() - 0.5) * w * 0.06 * strength).round();
        if dx == 0.0 {
            continue;
        }
        let band = Rect::from_xywh(0.0, y0, w, bh);
        canvas.draw_image_rect(
            picture,
            Some((&band, canvas::SrcRectConstraint::Fast)),
            band.with_offset((dx, 0.0)),
            &plain,
        );
    }

    // A noisy, jittering bar rolling down the picture.
    let bar_h = (h * 0.07).floor();
    let bar_y = (roll * (h + bar_h)).floor() - bar_h;
    let bar = Rect::from_xywh(0.0, bar_y, w, bar_h);
    let shift = ((rand() - 0.5) * 16.0 * strength).round();
    if shift != 0.0 {
        canvas.draw_image_rect(
            picture,
            Some((&bar, canvas::SrcRectConstraint::Fast)),
            bar.with_offset((shift, 0.0)),
            &plain,
        );
    }
    // 0..1..0 across the bar
    let edge = |a: f32| {
        let c = [Color4f::new(1.0, 1.0, 1.0, 0.0), Color4f::new(1.0, 1.0, 1.0, a), Color4f::new(1.0, 1.0, 1.0, 0.0)];
        let g = Gradient::new(Colors::new(&c, None, TileMode::Clamp, None), Interpolation::default());
        linear_gradient(((0.0, bar_y), (0.0, bar_y + bar_h)), &g, None)
    };
    if let (Some(noise), Some(mask)) =
        (Shader::fractal_perlin_noise((0.9, 0.6), 1, (seed % 9973) as f32, None), edge(1.0))
    {
        let mut paint = Paint::default();
        paint.set_shader(shaders::blend(BlendMode::DstIn, noise, mask));
        paint.set_color_filter(tone(1.0, 0.0));
        paint.set_alpha_f((0.7 * strength).min(1.0));
        canvas.draw_rect(bar, &paint);
    }
    if let Some(glow) = edge(0.12 * strength) {
        let mut paint = Paint::default();
        paint.set_shader(glow);
        canvas.draw_rect(bar, &paint);
    }

    // Darken every other line pair a little.
    let dark = Color4f::new(0.0, 0.0, 0.0, 40.0 / 256.0 * strength);
    let clear = Color4f::new(0.0, 0.0, 0.0, 0.0);
    let c = [dark, dark, clear, clear];
    let pos = [0.0, 0.5, 0.5, 1.0];
    let g = Gradient::new(Colors::new(&c, Some(&pos), TileMode::Repeat, None), Interpolation::default());
    if let Some(lines) = linear_gradient(((0.0, 0.0), (0.0, 4.0)), &g, None) {
        let mut paint = Paint::default();
        paint.set_shader(lines);
        canvas.draw_rect(Rect::from_wh(w, h), &paint);
    }
}
