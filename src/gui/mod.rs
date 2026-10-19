//! The app window: preview, the big delay readout and button, the delay picker, a status
//! line and the settings sheet.

mod preview;
mod settings;
mod theme;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Align2, Color32, CornerRadius, Frame, Key, Margin, Pos2, Rect, RichText, Sense, Stroke, TextureHandle,
    TextureOptions, Ui, Vec2,
};

use crate::app::App;
use crate::format;
use crate::playout::Command;
use crate::state::{AppState, CommandResult, Mode, TransitionEvent, TransitionKind};

/// The preview runs this far behind the stream (encoder thread and repaint).
const PREVIEW_LATENCY: f64 = 0.1;
const DELAY_OPTIONS: &[f64] = &[5.0, 10.0, 30.0, 60.0, 120.0, 300.0];
/// The logo as the app's icon (scripts/icons.sh); on macOS on the rounded square its icons have.
#[cfg(target_os = "macos")]
const ICON: &[u8] = include_bytes!("../../assets/icon/icon-macos.png");
#[cfg(not(target_os = "macos"))]
const ICON: &[u8] = include_bytes!("../../assets/icon/icon.png");

pub fn run(app: Arc<App>) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("nicedelay")
            .with_inner_size([1080.0, 720.0])
            .with_min_inner_size([480.0, 600.0])
            .with_icon(Arc::new(eframe::icon_data::from_png_bytes(ICON).expect("icon is a valid PNG"))),
        ..Default::default()
    };
    eframe::run_native(
        "nicedelay",
        options,
        Box::new(move |cc| {
            theme::install(&cc.egui_ctx);
            let ctx = cc.egui_ctx.clone();
            app.on_repaint(move || ctx.request_repaint());
            // Ctrl+C or SIGTERM in a terminal closes the window like the close button.
            let ctx = cc.egui_ctx.clone();
            let _ = std::thread::Builder::new().name("signals".into()).spawn(move || {
                crate::wait_for_ctrl_c();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                ctx.request_repaint();
            });
            Ok(Box::new(Window::new(app)))
        }),
    )
    .map_err(|e| e.to_string())
}

struct Window {
    app: Arc<App>,
    texture: Option<TextureHandle>,
    preview_seq: u64,
    settings: Option<settings::Settings>,
    toast: Option<(String, Instant)>,
    /// the transition being mirrored by the readout, and when the GUI saw it start
    transition: Option<(u64, TransitionEvent, Instant)>,
    badge_drag: Option<preview::BadgeDrag>,
    /// `NICEDELAY_SCREENSHOT=<file.png>`: save a screenshot of the window after a few seconds
    /// (for the README and for checking the layout).
    screenshot: Option<(std::path::PathBuf, Instant, bool)>,
}

impl Window {
    fn new(app: Arc<App>) -> Window {
        let screenshot = std::env::var_os("NICEDELAY_SCREENSHOT").map(|p| (p.into(), Instant::now(), false));
        Window {
            app,
            texture: None,
            preview_seq: 0,
            settings: None,
            toast: None,
            transition: None,
            badge_drag: None,
            screenshot,
        }
    }

    fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn report(&mut self, r: &CommandResult) {
        if !r.ok {
            self.toast(r.error.clone().unwrap_or_else(|| "That didn’t work.".into()));
        } else if r.clamped
            && let Some(d) = r.delay
        {
            self.toast(format!("Only {} recorded so far — using that.", format::clock(d, false)));
        }
    }

    fn command(&mut self, cmd: Command) {
        let r = self.app.command(cmd);
        self.report(&r);
    }

    fn pick_delay(&mut self, seconds: f64, mode: Mode) {
        self.app.update_config(&serde_json::json!({ "delaySeconds": seconds }));
        if mode == Mode::Delayed {
            self.command(Command::SetDelay(Some(seconds)));
        }
    }

    fn update_preview(&mut self, ctx: &egui::Context) {
        let (seq, img) = self.app.preview();
        if seq == self.preview_seq {
            return;
        }
        self.preview_seq = seq;
        if let Some(img) = img {
            let image = egui::ColorImage::from_rgba_unmultiplied([img.w as usize, img.h as usize], &img.rgba);
            match &mut self.texture {
                Some(t) => t.set(image, TextureOptions::LINEAR),
                None => self.texture = Some(ctx.load_texture("preview", image, TextureOptions::LINEAR)),
            }
        }
    }

    /// Seconds into the current transition as the viewer sees it, if one is playing.
    fn transition_time(&self) -> Option<(f64, &TransitionEvent)> {
        let (_, t, start) = self.transition.as_ref()?;
        let total = PREVIEW_LATENCY + t.phases.intro + t.phases.scrub + t.phases.land;
        let elapsed = start.elapsed().as_secs_f64();
        (elapsed < total).then_some((elapsed, t))
    }
}

impl eframe::App for Window {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.update_preview(&ctx);
        let state = self.app.state();
        if let Some((seq, ev)) = self.app.transition_event()
            && self.transition.as_ref().is_none_or(|(s, ..)| *s != seq)
        {
            self.transition = Some((seq, ev, Instant::now()));
        }
        let mode = state.playout.mode;
        let accent = theme::accent(mode);

        // Space toggles, L goes live — unless typing or the settings are open.
        if self.settings.is_none() && !ctx.egui_wants_keyboard_input() {
            let (space, l) = ctx.input(|i| (i.key_pressed(Key::Space), i.key_pressed(Key::L)));
            if space {
                self.command(Command::Toggle);
            } else if l {
                self.command(Command::GoLive);
            }
        }

        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0, theme::BG);
        // Ambient light in the current accent colour.
        let glow_center = Pos2::new(full.center().x, full.top() + full.height() * 0.05);
        ui.painter().add(theme::glow(
            glow_center,
            Vec2::new(full.width() * 0.6, full.height() * 0.45),
            accent.gamma_multiply(0.13),
        ));

        egui::Panel::top("bar")
            .show_separator_line(false)
            .frame(Frame::new().inner_margin(Margin::symmetric(20, 12)))
            .show(ui, |ui| self.bar(ui, accent));
        egui::Panel::bottom("status")
            .show_separator_line(false)
            .frame(Frame::new().inner_margin(Margin { left: 20, right: 20, top: 10, bottom: 18 }))
            .show(ui, |ui| {
                self.status_line(ui, &state);
            });
        egui::CentralPanel::no_frame().show(ui, |ui| self.main(ui, &state));

        if let Some(s) = self.settings.as_mut() {
            match s.show(&ctx, &state.ingest.server) {
                settings::Outcome::Open => {}
                settings::Outcome::Cancel => self.settings = None,
                settings::Outcome::Save(cfg) => {
                    let r = self.app.update_config(&serde_json::to_value(&cfg).unwrap_or_default());
                    self.settings = None;
                    if r.restart_pending {
                        self.toast("Saved. Quality and connection changes apply when your stream ends.");
                    }
                }
            }
        }
        self.draw_toast(ui);
        self.take_screenshot(&ctx);

        // Keep the status fresh; animate smoothly while something moves.
        if self.transition_time().is_some() || self.toast.is_some() || mode == Mode::Live {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
}

impl Window {
    fn bar(&mut self, ui: &mut Ui, accent: Color32) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
            theme::logo(ui.painter(), rect, accent);
            ui.label(RichText::new("nicedelay").font(theme::semibold(15.0)).color(theme::TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::click());
                let color = if resp.hovered() { theme::TEXT } else { theme::TEXT_2 };
                if resp.hovered() {
                    ui.painter().circle_filled(rect.center(), 17.0, theme::FILL);
                }
                theme::gear(ui.painter(), rect.center(), color);
                if resp.on_hover_text("Settings").clicked() {
                    self.open_settings();
                }
            });
        });
    }

    fn open_settings(&mut self) {
        self.settings = Some(settings::Settings::new(self.app.config().config));
    }

    fn main(&mut self, ui: &mut Ui, state: &AppState) {
        let area = ui.max_rect().shrink2(Vec2::new(24.0, 8.0));
        let wide = area.width() >= 820.0;
        let max_w = area.width().min(1180.0);
        let area = Rect::from_center_size(area.center(), Vec2::new(max_w, area.height()));
        let (preview_rect, controls_rect) = if wide {
            let cw = (max_w - 40.0) / 2.55;
            let pw = max_w - 40.0 - cw.max(300.0);
            let ph = (pw * 9.0 / 16.0).min(area.height());
            let p = Rect::from_min_size(Pos2::new(area.left(), area.center().y - ph / 2.0), Vec2::new(pw, ph));
            let c = Rect::from_min_max(Pos2::new(p.right() + 40.0, area.top()), area.max);
            (p, c)
        } else {
            let ph = (area.width() * 9.0 / 16.0).min(area.height() * 0.45);
            let p = Rect::from_center_size(
                Pos2::new(area.center().x, area.top() + ph / 2.0),
                Vec2::new(ph * 16.0 / 9.0, ph),
            );
            let c = Rect::from_min_max(Pos2::new(area.left(), p.bottom() + 24.0), area.max);
            (p, c)
        };

        let mode = state.playout.mode;
        let delay = state.playout.target_delay;
        let dip = self.transition_time().map_or(0.0, |(t, ev)| {
            let t = t - PREVIEW_LATENCY;
            let p = &ev.phases;
            let x = if t < 0.0 {
                0.0
            } else if t < p.intro {
                t / p.intro
            } else if t < p.intro + p.scrub {
                1.0
            } else {
                1.0 - ((t - p.intro - p.scrub) / p.land).min(1.0)
            };
            (x * x * (3.0 - 2.0 * x)) as f32
        });
        let props = preview::PreviewProps {
            texture: self.texture.as_ref(),
            running: state.playout.output.running,
            idle: mode == Mode::Idle,
            size: (state.playout.output.width, state.playout.output.height),
            badge: state.badge,
            badge_text: if mode == Mode::Delayed {
                format!("{} delay", format::duration(delay))
            } else {
                "LIVE".into()
            },
            badge_dot: mode != Mode::Delayed,
            delayed: mode == Mode::Delayed,
            dip,
        };
        if let Some(pos) = preview::show(ui, preview_rect, &props, &mut self.badge_drag) {
            self.app.update_config(&serde_json::json!({ "badge": pos }));
        }

        let mut controls = ui.new_child(
            egui::UiBuilder::new().max_rect(controls_rect).layout(egui::Layout::top_down(egui::Align::Center)),
        );
        if wide {
            let h = 112.0 + 30.0 + 18.0 + 58.0 + 30.0 + 36.0 + 70.0;
            controls.add_space(((controls_rect.height() - h) / 2.0).max(0.0));
        }
        self.hero(&mut controls, state);
        controls.add_space(30.0);
        self.picker(&mut controls, state);
    }

    fn hero(&mut self, ui: &mut Ui, state: &AppState) {
        let mode = state.playout.mode;
        let transition = self.transition_time().map(|(t, ev)| (t, *ev));
        let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 112.0), Sense::hover());
        let painter = ui.painter();
        if let Some((t, ev)) = transition {
            // ripple
            let k = (t / 1.1).min(1.0) as f32;
            let r = 56.0 * (0.6 + 1.6 * k);
            let color = if ev.kind == TransitionKind::Rewind { theme::REPLAY } else { theme::LIVE };
            painter.circle_stroke(rect.center(), r, Stroke::new(2.0, color.gamma_multiply(0.55 * (1.0 - k))));
        }
        match (mode, transition) {
            (Mode::Idle, _) => {
                painter.text(rect.center(), Align2::CENTER_CENTER, "Off air", theme::bold(84.0), theme::TEXT_3);
            }
            (Mode::Live, None) => {
                let font = theme::bold(68.0);
                let g = painter.layout_no_wrap("LIVE".into(), font, theme::LIVE);
                let dot = 18.0;
                let w = dot + 18.0 + g.size().x;
                let left = rect.center().x - w / 2.0;
                let pulse = 0.55 + 0.45 * ((ui.input(|i| i.time) * 3.0).sin() as f32 * 0.5 + 0.5);
                painter.circle_filled(
                    Pos2::new(left + dot / 2.0, rect.center().y),
                    dot / 2.0,
                    theme::LIVE.gamma_multiply(pulse),
                );
                painter.galley(Pos2::new(left + dot + 18.0, rect.center().y - g.size().y / 2.0), g, theme::LIVE);
            }
            _ => {
                let value = match transition {
                    Some((t, ev)) => {
                        let u = ((t - PREVIEW_LATENCY - ev.phases.intro) / ev.phases.scrub).clamp(0.0, 1.0);
                        let e = if u < 0.5 { 4.0 * u * u * u } else { 1.0 - (-2.0 * u + 2.0).powi(3) / 2.0 };
                        ev.from + (ev.to - ev.from) * e
                    }
                    None => state.playout.target_delay,
                };
                let color = if mode == Mode::Delayed || transition.is_some_and(|(_, ev)| ev.to > 0.0) {
                    theme::REPLAY
                } else {
                    theme::LIVE
                };
                tabular(painter, rect.center(), &format::clock(value, true), theme::bold(84.0), color);
            }
        }

        let caption = caption(state, transition.map(|(_, ev)| ev));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            // centred caption, with a link while waiting for OBS
            let text = RichText::new(&caption).size(15.0).color(theme::TEXT_2);
            let link = mode == Mode::Idle;
            let w = ui.painter().layout_no_wrap(caption.clone(), theme::regular(15.0), Color32::WHITE).size().x
                + if link { 120.0 } else { 0.0 };
            ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
            ui.label(text);
            if link && ui.link(RichText::new("How to connect").size(15.0).color(theme::TEXT)).clicked() {
                self.open_settings();
            }
        });

        ui.add_space(18.0);
        let action_live = mode == Mode::Delayed;
        let label = match mode {
            Mode::Idle => "Waiting for OBS".to_string(),
            Mode::Delayed => "Go live".to_string(),
            Mode::Live => format!("Delay {}", format::short(state.delay_seconds)),
        };
        let w = ui.available_width().min(380.0);
        let (rect, resp) = ui
            .allocate_exact_size(Vec2::new(w, 58.0), if mode == Mode::Idle { Sense::hover() } else { Sense::click() });
        let (fill, text) = match mode {
            Mode::Idle => (theme::FILL, theme::TEXT_3),
            Mode::Delayed => (theme::LIVE, Color32::WHITE),
            Mode::Live => (theme::REPLAY, Color32::from_rgb(0x1d, 0x13, 0x00)),
        };
        let scale = if resp.is_pointer_button_down_on() {
            0.97
        } else if resp.hovered() {
            1.015
        } else {
            1.0
        };
        let r = Rect::from_center_size(rect.center(), rect.size() * scale);
        let painter = ui.painter();
        if mode != Mode::Idle {
            painter.add(theme::glow(
                r.center() + Vec2::new(0.0, 14.0),
                Vec2::new(r.width() * 0.5, 30.0),
                fill.gamma_multiply(0.35),
            ));
        }
        painter.rect_filled(r, CornerRadius::same(255), fill);
        let g = painter.layout_no_wrap(label, theme::semibold(17.0), text);
        let icon = if mode == Mode::Idle { 0.0 } else { 32.0 };
        let left = r.center().x - (g.size().x + icon) / 2.0;
        if mode != Mode::Idle {
            theme::transport(
                painter,
                Rect::from_min_size(Pos2::new(left, r.center().y - 7.5), Vec2::new(22.0, 15.0)),
                action_live,
                text,
            );
        }
        painter.galley(Pos2::new(left + icon, r.center().y - g.size().y / 2.0), g, text);
        if mode != Mode::Idle && r.width() > 300.0 {
            let k = painter.layout_no_wrap("space".into(), theme::semibold(11.0), text.gamma_multiply(0.55));
            let kr = Rect::from_min_size(
                Pos2::new(r.right() - 22.0 - k.size().x - 12.0, r.center().y - 10.0),
                Vec2::new(k.size().x + 12.0, 20.0),
            );
            painter.rect_stroke(kr, 6, Stroke::new(1.0, text.gamma_multiply(0.55)), egui::StrokeKind::Inside);
            painter.galley(kr.center() - k.size() / 2.0, k, text);
        }
        if resp.clicked() {
            self.command(Command::Toggle);
        }

        if let Some(hint) = end_hint(state) {
            ui.add_space(8.0);
            ui.label(RichText::new(hint).size(13.0).color(theme::TEXT_3));
        }
    }

    fn picker(&mut self, ui: &mut Ui, state: &AppState) {
        ui.label(RichText::new("DELAY").font(theme::semibold(12.0)).color(theme::TEXT_3).extra_letter_spacing(1.0));
        ui.add_space(2.0);
        let options: Vec<f64> = DELAY_OPTIONS.iter().copied().filter(|s| *s <= state.buffer.capacity - 2.0).collect();
        let mode = state.playout.mode;
        let selected = if mode == Mode::Delayed { state.playout.target_delay } else { state.delay_seconds };
        let w = ui.available_width().min(380.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 40.0), Sense::hover());
        ui.painter().rect_filled(rect, 12, theme::FILL);
        let inner = rect.shrink(3.0);
        let seg_w = inner.width() / options.len().max(1) as f32;
        let mut picked = None;
        for (i, s) in options.iter().enumerate() {
            let r = Rect::from_min_size(
                Pos2::new(inner.left() + i as f32 * seg_w, inner.top()),
                Vec2::new(seg_w, inner.height()),
            );
            let resp = ui.interact(r, egui::Id::unique(("delay", i)), Sense::click());
            let is_sel = (*s - selected).abs() < 0.5;
            if is_sel {
                ui.painter().rect_filled(r, 9, theme::FILL_STRONG);
            }
            let color = if is_sel || resp.hovered() { theme::TEXT } else { theme::TEXT_2 };
            let font = if is_sel { theme::semibold(14.0) } else { theme::regular(14.0) };
            ui.painter().text(r.center(), Align2::CENTER_CENTER, format::short(*s), font, color);
            if resp.clicked() {
                picked = Some(*s);
            }
        }
        if let Some(s) = picked {
            self.pick_delay(s, mode);
        }
    }

    fn status_line(&mut self, ui: &mut Ui, state: &AppState) {
        let mut items: Vec<(Option<Color32>, String, bool)> = vec![];
        let ingest = &state.ingest;
        let obs = if let Some(e) = &ingest.error {
            (Some(theme::LIVE), e.clone(), false)
        } else if !ingest.connected {
            (Some(theme::TEXT_3), "Waiting for OBS".into(), false)
        } else if let Some(info) = &ingest.info {
            (Some(theme::GOOD), format!("OBS connected · {}p{}", info.height, info.fps.round()), false)
        } else {
            (Some(theme::GOOD), "OBS connected".into(), false)
        };
        items.push(obs);
        let out = &state.playout.output;
        if out.running {
            if let Some(e) = &out.error {
                items.push((Some(theme::LIVE), e.clone(), false));
            } else if out.preview_only {
                items.push((Some(theme::REPLAY), "Not streaming anywhere yet — add a destination".into(), true));
            } else {
                items.push((Some(theme::GOOD), format!("Streaming to {}", out.destinations.join(", ")), false));
            }
        }
        if state.buffer.recorded > 0.0 {
            items.push((
                None,
                format!(
                    "{} of {} recorded",
                    format::clock(state.buffer.recorded, false),
                    format::clock(state.buffer.capacity, false)
                ),
                false,
            ));
        }
        ui.horizontal_wrapped(|ui| {
            let total: f32 = items
                .iter()
                .map(|(_, t, _)| {
                    ui.painter().layout_no_wrap(t.clone(), theme::regular(13.0), Color32::WHITE).size().x + 36.0
                })
                .sum();
            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
            ui.spacing_mut().item_spacing.x = 7.0;
            for (dot, text, link) in items {
                if let Some(c) = dot {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(7.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 3.5, c);
                }
                let rt = RichText::new(text).size(13.0).color(theme::TEXT_2);
                if link {
                    if ui.link(rt).clicked() {
                        self.open_settings();
                    }
                } else {
                    ui.label(rt);
                }
                ui.add_space(15.0);
            }
        });
    }

    fn take_screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, start, requested)) = &mut self.screenshot else { return };
        let after = std::env::var("NICEDELAY_SCREENSHOT_AFTER").ok().and_then(|s| s.parse().ok()).unwrap_or(4.0);
        if *requested || start.elapsed() < Duration::from_secs_f64(after) {
            return;
        }
        *requested = true;
        let path = path.clone();
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
            let [w, h] = img.size;
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            let info = skia_safe::ImageInfo::new(
                (w as i32, h as i32),
                skia_safe::ColorType::RGBA8888,
                skia_safe::AlphaType::Premul,
                None,
            );
            let png = skia_safe::images::raster_from_data(&info, skia_safe::Data::new_copy(&bytes), w * 4)
                .and_then(|i| i.encode(None, skia_safe::EncodedImageFormat::PNG, 100));
            match png.map(|d| std::fs::write(&path, d.as_bytes())) {
                Some(Ok(())) => tracing::info!("screenshot saved to {}", path.display()),
                _ => tracing::warn!("couldn't save the screenshot"),
            }
        })));
    }

    fn draw_toast(&mut self, ui: &mut Ui) {
        let Some((msg, at)) = &self.toast else { return };
        let age = at.elapsed().as_secs_f32();
        if age > 3.8 {
            self.toast = None;
            return;
        }
        let alpha = (age / 0.2).min(1.0).min((3.8 - age) / 0.3);
        let full = ui.ctx().content_rect();
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::unique("toast")));
        let g = painter.layout(
            msg.clone(),
            theme::regular(14.0),
            theme::TEXT.gamma_multiply(alpha),
            (full.width() - 64.0).min(560.0),
        );
        let size = g.size() + Vec2::new(36.0, 22.0);
        let rect = Rect::from_center_size(
            Pos2::new(full.center().x, full.bottom() - 64.0 - size.y / 2.0 + 16.0 * (1.0 - alpha)),
            size,
        );
        painter.rect_filled(rect, 14, Color32::from_rgba_unmultiplied(44, 44, 46, (235.0 * alpha) as u8));
        painter.galley(rect.min + Vec2::new(18.0, 11.0), g, theme::TEXT);
    }
}

/// Text with every digit in a cell as wide as "0", so a changing number doesn't jitter.
fn tabular(painter: &egui::Painter, center: Pos2, text: &str, font: egui::FontId, color: Color32) {
    let cell = painter.layout_no_wrap("0".into(), font.clone(), color).size().x;
    let galleys: Vec<_> = text.chars().map(|c| painter.layout_no_wrap(c.to_string(), font.clone(), color)).collect();
    let widths: Vec<f32> =
        text.chars().zip(&galleys).map(|(c, g)| if c.is_ascii_digit() { cell } else { g.size().x }).collect();
    let mut x = center.x - widths.iter().sum::<f32>() / 2.0;
    for (g, w) in galleys.into_iter().zip(widths) {
        let h = g.size().y;
        painter.galley(Pos2::new(x + (w - g.size().x) / 2.0, center.y - h / 2.0), g, color);
        x += w;
    }
}

fn caption(state: &AppState, transition: Option<TransitionEvent>) -> String {
    let mode = state.playout.mode;
    if let Some(t) = transition {
        return match t.kind {
            TransitionKind::Rewind => "Rewinding…".into(),
            TransitionKind::Forward if t.to == 0.0 => "Catching up to live…".into(),
            TransitionKind::Forward => "Skipping ahead…".into(),
        };
    }
    match mode {
        Mode::Idle => "Start streaming in OBS to go on air.".into(),
        Mode::Delayed => format!("Viewers see everything {} later.", format::spoken(state.playout.target_delay)),
        Mode::Live if state.buffer.recorded < state.delay_seconds => {
            format!(
                "Recording… {} of {} ready.",
                format::short(state.buffer.recorded),
                format::short(state.delay_seconds)
            )
        }
        Mode::Live => "Viewers see you in real time.".into(),
    }
}

/// How the stream ends, shown while on air.
fn end_hint(state: &AppState) -> Option<&'static str> {
    if state.playout.mode == Mode::Idle {
        return None;
    }
    Some(if state.ingest.connected {
        "To end the stream, stop streaming in OBS."
    } else {
        "OBS stopped. The stream ends automatically."
    })
}
