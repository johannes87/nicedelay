//! The on-stream graphics: the rewind / fast-forward icon with its timecode, the timeline,
//! and the corner badge. Sizes are designed at 1080p and scale with the output.

use std::f32::consts::PI;

use skia_safe::textlayout::TextAlign;
use skia_safe::{Canvas, Color, Paint, PaintStyle, Path, PathBuilder, Point, RRect, Rect, image_filters, paint};

use crate::config::BadgePosition;
use crate::render::text::{Fonts, Style};
use crate::state::TransitionKind;

pub const REPLAY: Color = Color::from_rgb(0xFF, 0x9F, 0x0A);
pub const LIVE: Color = Color::from_rgb(0xFF, 0x45, 0x3A);
pub const WHITE: Color = Color::WHITE;

fn rgba(r: u8, g: u8, b: u8, a: f32) -> Color {
    Color::from_argb((a * 255.0).round() as u8, r, g, b)
}

pub struct CenterParams {
    pub kind: TransitionKind,
    pub alpha: f32,
    /// scale of the icon group (pop animation)
    pub pop: f32,
    /// 0..1 progress of the scrub
    pub progress: f32,
    pub timecode: String,
    pub label: &'static str,
}

pub struct TimelineParams {
    pub alpha: f32,
    /// seconds represented by the left end of the track
    pub span: f64,
    /// delay (seconds) at the knob
    pub knob: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PillKind {
    Replay,
    Live,
}

pub struct PillParams {
    pub alpha: f32,
    pub kind: PillKind,
    pub text: String,
}

/// Where a badge lands on the frame: the rectangle it covers (even-aligned) and what to
/// draw inside it.
pub struct PillLayout {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    paragraph_x: f32,
    pill: Rect,
}

pub struct Overlay {
    pub w: f32,
    pub h: f32,
    /// scale relative to 1080p
    pub s: f32,
    fonts: Fonts,
}

/// A paint with a canvas-style shadow (`shadowBlur`, twice the sigma) around what it draws.
fn with_shadow(mut p: Paint, color: Color, blur: f32) -> Paint {
    p.set_image_filter(image_filters::drop_shadow((0.0, 0.0), (blur / 2.0, blur / 2.0), color, None, None, None));
    p
}

fn fill(color: Color) -> Paint {
    let mut p = Paint::default();
    p.set_anti_alias(true);
    p.set_color(color);
    p
}

fn stroke(color: Color, width: f32) -> Paint {
    let mut p = fill(color);
    p.set_style(PaintStyle::Stroke);
    p.set_stroke_width(width);
    p.set_stroke_cap(paint::Cap::Round);
    p
}

fn circle(c: Point, r: f32) -> Path {
    Path::circle(c, r, None)
}

impl Overlay {
    pub fn new(width: u32, height: u32) -> Overlay {
        Overlay { w: width as f32, h: height as f32, s: height as f32 / 1080.0, fonts: Fonts::new() }
    }

    pub fn draw_center(&self, canvas: &Canvas, p: &CenterParams) {
        if p.alpha <= 0.01 {
            return;
        }
        let s = self.s;
        let (lw, lh) = (640.0 * s, 470.0 * s);
        let (ox, oy) = (((self.w - lw) / 2.0).round(), (self.h * 0.5 - 220.0 * s).round());
        let accent = if p.kind == TransitionKind::Rewind { REPLAY } else { LIVE };
        canvas.save_layer_alpha_f(Rect::from_xywh(ox, oy, lw, lh), p.alpha);
        canvas.translate((ox, oy));
        let (cx, cy) = (lw / 2.0, 150.0 * s);

        canvas.save();
        canvas.translate((cx, cy));
        canvas.scale((p.pop, p.pop));
        // backdrop disc
        canvas.draw_path(&circle(Point::new(0.0, 0.0), 118.0 * s), &fill(rgba(10, 10, 14, 0.55)));
        // progress ring: sweeps counter-clockwise for rewind, clockwise for forward
        let ring = Rect::from_xywh(-104.0 * s, -104.0 * s, 208.0 * s, 208.0 * s);
        canvas.draw_oval(ring, &stroke(rgba(255, 255, 255, 0.18), 7.0 * s));
        if p.progress > 0.002 {
            let sweep = 360.0 * p.progress.min(1.0);
            let sweep = if p.kind == TransitionKind::Rewind { -sweep } else { sweep };
            let paint = with_shadow(stroke(accent, 7.0 * s), accent, 16.0 * s);
            canvas.draw_arc(ring, -90.0, sweep, false, &paint);
        }
        // double-triangle icon, nudged in the direction of travel
        let dir = if p.kind == TransitionKind::Rewind { -1.0 } else { 1.0 };
        let nudge = (p.progress * PI * 6.0).sin() * 5.0 * s * dir;
        let (th, tw) = (44.0 * s, 42.0 * s);
        let mut b = PathBuilder::new();
        for off in [-tw / 2.0 - s, tw / 2.0 + s] {
            let x = off + nudge;
            b.move_to((x + dir * tw / 2.0, 0.0));
            b.line_to((x - dir * tw / 2.0, -th));
            b.line_to((x - dir * tw / 2.0, th));
            b.close();
        }
        canvas.draw_path(&b.detach(), &with_shadow(fill(WHITE), rgba(0, 0, 0, 0.5), 10.0 * s));
        canvas.restore();

        // timecode + label (no pop scaling, just fade)
        let shadow = Some((rgba(0, 0, 0, 0.65), 14.0 * s));
        let tc = self.fonts.line(
            &p.timecode,
            &Style { size: (86.0 * s).round(), weight: 700, color: accent, letter_spacing: 0.0, shadow },
            TextAlign::Center,
            lw,
        );
        tc.paint(canvas, (0.0, 375.0 * s - tc.alphabetic_baseline()));
        let label = self.fonts.line(
            p.label,
            &Style {
                size: (25.0 * s).round(),
                weight: 600,
                color: rgba(255, 255, 255, 0.88),
                letter_spacing: (5.0 * s).round(),
                shadow,
            },
            TextAlign::Center,
            lw,
        );
        label.paint(canvas, (0.0, 425.0 * s - label.alphabetic_baseline()));
        canvas.restore();
    }

    pub fn draw_timeline(&self, canvas: &Canvas, p: &TimelineParams) {
        if p.alpha <= 0.01 {
            return;
        }
        let s = self.s;
        let (lw, lh) = ((self.w * 0.72).round(), (130.0 * s).round());
        let y = 82.0 * s;
        let (ox, oy) = (((self.w - lw) / 2.0).round(), (self.h * 0.86 - y).round());
        canvas.save_layer_alpha_f(Rect::from_xywh(ox, oy, lw, lh), p.alpha);
        canvas.translate((ox, oy));
        let (left, right) = (30.0 * s, lw - 30.0 * s);
        let span = p.span.max(1e-6);
        let x_of = |d: f64| right - (d.min(span) / span) as f32 * (right - left);
        let accent = if p.knob > 0.05 { REPLAY } else { LIVE };

        let track = with_shadow(stroke(rgba(255, 255, 255, 0.28), 8.0 * s), rgba(0, 0, 0, 0.5), 8.0 * s);
        canvas.draw_line((left, y), (right, y), &track);
        // the stretch viewers are behind live
        let kx = x_of(p.knob);
        if right - kx > 1.0 {
            canvas.draw_line((kx, y), (right, y), &stroke(accent, 8.0 * s));
        }
        // live marker
        canvas.draw_path(&circle(Point::new(right, y), 10.0 * s), &fill(LIVE));
        let live = self.fonts.line(
            "LIVE",
            &Style {
                size: (24.0 * s).round(),
                weight: 700,
                color: WHITE,
                letter_spacing: (3.0 * s).round(),
                shadow: Some((rgba(0, 0, 0, 0.6), 8.0 * s)),
            },
            TextAlign::Right,
            lw,
        );
        // right-aligned to the marker, like the canvas version (letter spacing trails the text)
        live.paint(canvas, (right + 12.0 * s - lw, y - 26.0 * s - live.alphabetic_baseline()));
        // knob
        canvas.draw_path(&circle(Point::new(kx, y), 17.0 * s), &with_shadow(fill(WHITE), rgba(0, 0, 0, 0.5), 14.0 * s));
        canvas.draw_path(&circle(Point::new(kx, y), 7.0 * s), &fill(accent));
        canvas.restore();
    }

    /// Lays out the status badge at `pos` within a margin around the frame.
    pub fn pill_layout(&self, p: &PillParams, pos: BadgePosition) -> (PillLayout, skia_safe::textlayout::Paragraph) {
        let s = self.s;
        let (layer_w, layer_h) = ((480.0 * s).round(), (64.0 * s).round());
        let text = self.pill_text(&p.text);
        let text_w = text.max_intrinsic_width();
        let pad = 16.0 * s;
        // The delay badge is text only; LIVE / RECONNECTING get a red dot.
        let icon_w = if p.kind == PillKind::Live { 29.0 * s } else { 0.0 };
        let pw = (layer_w - 4.0 * s).min(pad * 2.0 + icon_w + text_w);
        let ph = 44.0 * s;
        let py = (layer_h - ph) / 2.0;
        let margin = 36.0 * s;
        let x = margin + pos.x as f32 * (self.w - 2.0 * margin - pw) - 2.0 * s;
        let y = margin + pos.y as f32 * (self.h - 2.0 * margin - ph) - py;
        let used_w = (pw + 6.0 * s).ceil();
        let even = |v: f32| (v.max(0.0) as u32) & !1;
        let (rx, ry) = (even(x), even(y));
        let rw = ((used_w as u32 + 1) & !1).min(self.w as u32 - rx);
        let rh = ((layer_h as u32 + 1) & !1).min(self.h as u32 - ry);
        let pill = Rect::from_xywh(x - rx as f32 + 2.0 * s, y - ry as f32 + py, pw, ph);
        let layout = PillLayout { x: rx, y: ry, w: rw, h: rh, paragraph_x: pill.left + pad + icon_w, pill };
        (layout, text)
    }

    fn pill_text(&self, text: &str) -> skia_safe::textlayout::Paragraph {
        let s = self.s;
        self.fonts.line(
            text,
            &Style { size: (22.0 * s).round(), weight: 600, color: WHITE, letter_spacing: s.round(), shadow: None },
            TextAlign::Left,
            1000.0 * s,
        )
    }

    /// Draws the badge into a canvas covering exactly `layout`'s rectangle.
    pub fn draw_pill(
        &self,
        canvas: &Canvas,
        p: &PillParams,
        layout: &PillLayout,
        text: &skia_safe::textlayout::Paragraph,
    ) {
        if p.alpha <= 0.01 {
            return;
        }
        let s = self.s;
        let pill = layout.pill;
        canvas.save_layer_alpha_f(None, p.alpha);
        let rr = RRect::new_rect_xy(pill, pill.height() / 2.0, pill.height() / 2.0);
        canvas.draw_rrect(rr, &fill(rgba(12, 12, 16, 0.55)));
        canvas.draw_rrect(rr, &stroke(rgba(255, 255, 255, 0.3), 2.0 * s));
        let iy = pill.center_y();
        if p.kind == PillKind::Live {
            let dot = Point::new(pill.left + 16.0 * s + 8.0 * s, iy);
            canvas.draw_path(&circle(dot, 7.0 * s), &with_shadow(fill(LIVE), LIVE, 8.0 * s));
        }
        // vertically centred, like textBaseline = middle
        text.paint(canvas, (layout.paragraph_x, iy + s - text.height() / 2.0));
        canvas.restore();
    }
}
