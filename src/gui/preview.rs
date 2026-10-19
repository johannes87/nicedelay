//! The preview: what viewers see (including the on-stream animation), with a handle to
//! drag the on-stream badge around.

use egui::{Align2, Color32, CornerRadius, Id, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, TextureHandle, Ui, Vec2};

use crate::config::BadgePosition;
use crate::gui::theme;

/// Same margin as the on-stream badge, in 1080p pixels.
const BADGE_MARGIN: f32 = 36.0;
/// Positions this close to an edge or the centre snap to it.
const SNAP: f32 = 0.02;

fn snap(v: f32) -> f32 {
    let c = v.clamp(0.0, 1.0);
    [0.0, 0.5, 1.0].into_iter().find(|t| (c - t).abs() < SNAP).unwrap_or(c)
}

pub struct BadgeDrag {
    /// where the pointer grabbed the handle, relative to its top left
    grab: Vec2,
    pub pos: BadgePosition,
}

pub struct PreviewProps<'a> {
    pub texture: Option<&'a TextureHandle>,
    pub running: bool,
    pub idle: bool,
    /// output picture size
    pub size: (u32, u32),
    pub badge: BadgePosition,
    pub badge_text: String,
    pub badge_dot: bool,
    pub delayed: bool,
    /// 0..1 scale dip while a transition plays
    pub dip: f32,
}

/// Draws the preview into `rect`. Returns a new badge position when a drag ends.
pub fn show(ui: &mut Ui, rect: Rect, p: &PreviewProps, drag: &mut Option<BadgeDrag>) -> Option<BadgePosition> {
    let rect = Rect::from_center_size(rect.center(), rect.size() * (1.0 - 0.025 * p.dip));
    let painter = ui.painter_at(rect.expand(40.0));
    let radius = CornerRadius::same(18);
    // shadow / glow in the mode's colour
    let glow = if p.delayed { theme::REPLAY.gamma_multiply(0.35) } else { Color32::from_black_alpha(160) };
    for i in 1..=6 {
        let r = rect.expand(i as f32 * 3.0).translate(Vec2::new(0.0, 8.0));
        painter.rect_filled(r, 18 + i * 3, glow.gamma_multiply(0.12));
    }
    painter.rect_filled(rect, radius, Color32::BLACK);
    let border =
        if p.delayed { Stroke::new(1.5, theme::REPLAY.gamma_multiply(0.7)) } else { Stroke::new(1.0, theme::HAIRLINE) };

    // The output picture, letterboxed in the preview.
    let ar = p.size.0 as f32 / p.size.1.max(1) as f32;
    let pic = {
        let (w, h) = (rect.width().min(rect.height() * ar), rect.height().min(rect.width() / ar));
        Rect::from_center_size(rect.center(), Vec2::new(w, h))
    };
    match (p.running, p.texture) {
        (true, Some(tex)) => {
            // Rounded where the picture reaches the preview's corners.
            let r = if pic.width() >= rect.width() - 1.0 && pic.height() >= rect.height() - 1.0 {
                radius
            } else {
                CornerRadius::ZERO
            };
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            painter.add(egui::epaint::RectShape::filled(pic, r, Color32::WHITE).with_texture(tex.id(), uv));
        }
        _ => {
            painter.add(theme::glow(rect.center(), rect.size() * 0.6, Color32::from_rgb(0x17, 0x17, 0x1a)));
            let icon = Rect::from_center_size(rect.center() - Vec2::new(0.0, 16.0), Vec2::new(30.0, 22.0));
            painter.rect_stroke(
                Rect::from_min_size(icon.min, Vec2::new(19.0, 22.0)),
                4,
                Stroke::new(1.4, theme::TEXT_3),
                StrokeKind::Middle,
            );
            painter.add(Shape::closed_line(
                vec![
                    Pos2::new(icon.left() + 21.0, icon.top() + 8.0),
                    Pos2::new(icon.right(), icon.top() + 3.0),
                    Pos2::new(icon.right(), icon.bottom() - 3.0),
                    Pos2::new(icon.left() + 21.0, icon.bottom() - 8.0),
                ],
                Stroke::new(1.4, theme::TEXT_3),
            ));
            let text = if p.idle { "Your stream will appear here" } else { "Starting…" };
            painter.text(
                rect.center() + Vec2::new(0.0, 22.0),
                Align2::CENTER_CENTER,
                text,
                theme::semibold(15.0),
                theme::TEXT_3,
            );
        }
    }
    painter.rect_stroke(rect, radius, border, StrokeKind::Inside);

    badge_handle(ui, rect, pic, p, drag)
}

/// An outline of the on-stream badge that appears on hover; dragging it moves the badge on
/// the stream.
fn badge_handle(
    ui: &mut Ui,
    preview: Rect,
    pic: Rect,
    p: &PreviewProps,
    drag: &mut Option<BadgeDrag>,
) -> Option<BadgePosition> {
    let u = pic.height() / 1080.0;
    let m = BADGE_MARGIN * u;
    let font = theme::semibold((22.0 * u).max(6.0));
    let galley = ui.painter().layout_no_wrap(p.badge_text.clone(), font, Color32::WHITE);
    let pad = 16.0 * u;
    let dot = if p.badge_dot { 29.0 * u } else { 0.0 };
    let size = Vec2::new(pad * 2.0 + dot + galley.size().x, 44.0 * u);
    let pos = drag.as_ref().map_or(p.badge, |d| d.pos);
    let at = |pos: BadgePosition| {
        Pos2::new(
            pic.left() + m + pos.x as f32 * (pic.width() - 2.0 * m - size.x),
            pic.top() + m + pos.y as f32 * (pic.height() - 2.0 * m - size.y),
        )
    };
    let handle = Rect::from_min_size(at(pos), size);
    let id = Id::unique("badge-handle");
    let resp = ui.interact(handle.expand(10.0), id, Sense::drag());
    if resp.drag_started()
        && let Some(ptr) = resp.interact_pointer_pos()
    {
        *drag = Some(BadgeDrag { grab: ptr - handle.min, pos: p.badge });
    }
    if let (Some(d), Some(ptr)) = (drag.as_mut(), resp.interact_pointer_pos())
        && resp.dragged()
    {
        let tl = ptr - d.grab;
        d.pos = BadgePosition {
            x: f64::from(snap((tl.x - pic.left() - m) / (pic.width() - 2.0 * m - size.x))),
            y: f64::from(snap((tl.y - pic.top() - m) / (pic.height() - 2.0 * m - size.y))),
        };
    }
    let mut done = None;
    if resp.drag_stopped() {
        done = drag.take().map(|d| d.pos);
    }
    if resp.hovered() || drag.is_some() {
        ui.ctx().set_cursor_icon(if drag.is_some() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
    }

    let hovering = ui.rect_contains_pointer(preview);
    if !(hovering || drag.is_some()) {
        return done;
    }
    let painter = ui.painter();
    let handle = Rect::from_min_size(at(drag.as_ref().map_or(p.badge, |d| d.pos)), size);
    let r = CornerRadius::same(255);
    // While streaming the real badge is in the picture: only outline it.
    let full = !p.running || drag.is_some();
    if full {
        painter.rect_filled(handle, r, Color32::from_rgba_unmultiplied(12, 12, 16, 115));
        let mut x = handle.left() + pad;
        if p.badge_dot {
            painter.circle_filled(Pos2::new(x + 8.0 * u, handle.center().y), 7.0 * u, theme::LIVE);
            x += dot;
        }
        painter.galley(Pos2::new(x, handle.center().y - galley.size().y / 2.0), galley, Color32::WHITE);
    }
    painter.rect_stroke(
        handle,
        r,
        Stroke::new((2.0 * u).max(1.0), Color32::from_white_alpha(180)),
        StrokeKind::Outside,
    );
    if resp.hovered() && drag.is_none() {
        resp.on_hover_text("Drag to move the delay badge");
    }
    done
}
