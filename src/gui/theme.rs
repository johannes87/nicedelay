//! Colours, fonts and widget styling: a dark theme matching the on-stream graphics.

use std::sync::Arc;

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Color32, Context, FontData, FontDefinitions, FontFamily, FontId, Pos2, Rect, Shape, Stroke, Vec2};

use crate::render::text::{INTER_BOLD, INTER_REGULAR, INTER_SEMIBOLD};
use crate::state::Mode;

pub const BG: Color32 = Color32::from_rgb(0x0b, 0x0b, 0x0d);
pub const SURFACE: Color32 = Color32::from_rgb(0x1c, 0x1c, 0x1e);
pub const SURFACE_2: Color32 = Color32::from_rgb(0x2c, 0x2c, 0x2e);
pub const FILL: Color32 = Color32::from_rgba_premultiplied(24, 24, 26, 51); // rgba(120,120,128,0.2)
pub const FILL_STRONG: Color32 = Color32::from_rgba_premultiplied(43, 43, 46, 92); // 0.36
pub const HAIRLINE: Color32 = Color32::from_rgba_premultiplied(20, 20, 20, 20); // white 0.08
pub const TEXT: Color32 = Color32::from_rgb(0xf5, 0xf5, 0xf7);
pub const TEXT_2: Color32 = Color32::from_rgb(0xa1, 0xa1, 0xa6);
pub const TEXT_3: Color32 = Color32::from_rgb(0x6e, 0x6e, 0x73);
pub const LIVE: Color32 = Color32::from_rgb(0xff, 0x45, 0x3a);
pub const REPLAY: Color32 = Color32::from_rgb(0xff, 0x9f, 0x0a);
pub const GOOD: Color32 = Color32::from_rgb(0x30, 0xd1, 0x58);

pub fn accent(mode: Mode) -> Color32 {
    match mode {
        Mode::Live => LIVE,
        Mode::Delayed => REPLAY,
        Mode::Idle => TEXT_3,
    }
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn install(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    let fallbacks = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    for (name, data) in [("Inter", INTER_REGULAR), ("Inter-SemiBold", INTER_SEMIBOLD), ("Inter-Bold", INTER_BOLD)] {
        fonts.font_data.insert(name.into(), Arc::new(FontData::from_static(data)));
    }
    let family = |first: &str| [vec![first.to_string()], fallbacks.clone()].concat();
    fonts.families.insert(FontFamily::Proportional, family("Inter"));
    fonts.families.insert(FontFamily::Name("semibold".into()), family("Inter-SemiBold"));
    fonts.families.insert(FontFamily::Name("bold".into()), family("Inter-Bold"));
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = SURFACE;
    v.extreme_bg_color = FILL;
    v.faint_bg_color = SURFACE_2;
    v.override_text_color = Some(TEXT);
    v.window_stroke = Stroke::new(0.5, HAIRLINE);
    v.selection.bg_fill = REPLAY.gamma_multiply(0.5);
    v.selection.stroke = Stroke::new(1.0, REPLAY);
    v.text_cursor.stroke = Stroke::new(2.0, REPLAY);
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = 8.into();
        w.bg_stroke = Stroke::NONE;
    }
    v.widgets.inactive.weak_bg_fill = FILL;
    v.widgets.inactive.bg_fill = FILL;
    v.widgets.hovered.weak_bg_fill = FILL_STRONG;
    v.widgets.hovered.bg_fill = FILL_STRONG;
    v.widgets.active.weak_bg_fill = FILL_STRONG;
    // Always dark, whatever the system's appearance.
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.interact_size.y = 34.0;
        s.text_styles.insert(egui::TextStyle::Body, regular(15.0));
        s.text_styles.insert(egui::TextStyle::Button, regular(15.0));
        s.text_styles.insert(egui::TextStyle::Small, regular(12.5));
        s.text_styles.insert(egui::TextStyle::Monospace, FontId::monospace(13.0));
    });
}

/// A soft elliptical glow (radial gradient from `color` to transparent).
pub fn glow(center: Pos2, radius: Vec2, color: Color32) -> Shape {
    let mut mesh = Mesh::default();
    mesh.vertices.push(Vertex { pos: center, uv: WHITE_UV, color });
    let n = 64;
    let rings: [(f32, f32); 3] = [(0.45, 0.6), (0.75, 0.22), (1.0, 0.0)];
    for (ring, (r, a)) in rings.iter().enumerate() {
        for i in 0..n {
            let t = i as f32 / n as f32 * std::f32::consts::TAU;
            let pos = center + Vec2::new(t.cos() * radius.x * r, t.sin() * radius.y * r);
            mesh.vertices.push(Vertex { pos, uv: WHITE_UV, color: color.gamma_multiply(*a) });
        }
        let base = 1 + (ring * n) as u32;
        for i in 0..n as u32 {
            let (a0, a1) = (base + i, base + (i + 1) % n as u32);
            if ring == 0 {
                mesh.add_triangle(0, a0, a1);
            } else {
                let (b0, b1) = (a0 - n as u32, a1 - n as u32);
                mesh.add_triangle(b0, a0, a1);
                mesh.add_triangle(b0, a1, b1);
            }
        }
    }
    Shape::mesh(mesh)
}

/// The nicedelay logo: a disc in the accent colour with a rewind symbol.
pub fn logo(painter: &egui::Painter, rect: Rect, accent: Color32) {
    let c = rect.center();
    let s = rect.width() / 32.0;
    painter.circle_filled(c, 15.0 * s, accent);
    let p = |x: f32, y: f32| Pos2::new(rect.left() + x * s, rect.top() + y * s);
    painter.add(Shape::convex_polygon(vec![p(15.0, 9.0), p(15.0, 23.0), p(6.0, 16.0)], BG, Stroke::NONE));
    painter.add(Shape::convex_polygon(vec![p(25.0, 9.0), p(25.0, 23.0), p(16.0, 16.0)], BG, Stroke::NONE));
}

/// Two triangles pointing back (rewind) or forward.
pub fn transport(painter: &egui::Painter, rect: Rect, forward: bool, color: Color32) {
    let (w, h) = (rect.width() / 2.0, rect.height());
    for i in 0..2 {
        let x0 = rect.left() + i as f32 * w;
        let (tip, base) = if forward { (x0 + w, x0) } else { (x0, x0 + w) };
        painter.add(Shape::convex_polygon(
            vec![Pos2::new(base, rect.top()), Pos2::new(tip, rect.top() + h / 2.0), Pos2::new(base, rect.bottom())],
            color,
            Stroke::NONE,
        ));
    }
}

/// A settings gear: a ring with eight teeth around a hole.
pub fn gear(painter: &egui::Painter, c: Pos2, color: Color32) {
    let stroke = Stroke::new(1.6, color);
    for i in 0..8 {
        let a = i as f32 / 8.0 * std::f32::consts::TAU;
        let d = Vec2::new(a.cos(), a.sin());
        painter.line_segment([c + d * 6.5, c + d * 9.5], Stroke::new(3.0, color));
    }
    painter.circle_stroke(c, 6.5, stroke);
    painter.circle_stroke(c, 2.6, stroke);
}
