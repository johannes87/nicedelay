//! The settings sheet: how to connect OBS, where to stream, quality and recording.

use egui::{Align, Color32, CornerRadius, Frame, Id, Layout, Margin, RichText, Sense, Stroke, TextEdit, Ui, Vec2};

use crate::config::{Config, Destination};
use crate::gui::theme;
use crate::media::hw;

// Secure (RTMPS) ingest, so stream keys never cross the network in clear text.
const SERVICES: &[(&str, &str)] = &[
    ("Twitch", "rtmps://ingest.global-contribute.live-video.net/app"),
    ("YouTube", "rtmps://a.rtmps.youtube.com/live2"),
];

const RESOLUTIONS: &[(&str, u32, u32)] = &[("720p", 1280, 720), ("1080p", 1920, 1080), ("1440p", 2560, 1440)];
const BUFFER_MINUTES: &[f64] = &[2.0, 5.0, 10.0, 15.0, 30.0];

pub enum Outcome {
    Open,
    Cancel,
    Save(Box<Config>),
}

pub struct Settings {
    draft: Config,
    copied: Option<(&'static str, std::time::Instant)>,
}

impl Settings {
    pub fn new(config: Config) -> Settings {
        Settings { draft: config, copied: None }
    }

    pub fn show(&mut self, ctx: &egui::Context, server: &str) -> Outcome {
        let mut outcome = Outcome::Open;
        let frame = Frame::new()
            .fill(theme::SURFACE)
            .corner_radius(18)
            .inner_margin(Margin::ZERO)
            .stroke(Stroke::new(0.5, theme::HAIRLINE));
        let resp = egui::Modal::new(Id::unique("settings"))
            .frame(frame)
            .backdrop_color(Color32::from_black_alpha(140))
            .show(ctx, |ui| {
                ui.set_width(540.0_f32.min(ctx.content_rect().width() - 24.0));
                // header: Cancel · Settings · Done
                Frame::new().inner_margin(Margin::symmetric(18, 12)).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if text_button(ui, "Cancel", false).clicked() {
                            outcome = Outcome::Cancel;
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if text_button(ui, "Done", true).clicked() {
                                outcome = Outcome::Save(Box::new(self.draft.clone()));
                            }
                            ui.centered_and_justified(|ui| {
                                ui.label(RichText::new("Settings").font(theme::semibold(16.0)))
                            });
                        });
                    });
                });
                ui.painter().hline(ui.max_rect().x_range(), ui.cursor().top(), Stroke::new(0.5, theme::HAIRLINE));
                let max_h = (ctx.content_rect().height() - 120.0).clamp(200.0, 680.0);
                egui::ScrollArea::vertical().max_height(max_h).min_scrolled_height(max_h).show(ui, |ui| {
                    Frame::new()
                        .inner_margin(Margin { left: 18, right: 18, top: 4, bottom: 20 })
                        .show(ui, |ui| self.body(ui, server));
                });
            });
        if resp.should_close() && matches!(outcome, Outcome::Open) {
            outcome = Outcome::Cancel;
        }
        outcome
    }

    fn body(&mut self, ui: &mut Ui, server: &str) {
        let d = &mut self.draft;
        group(ui, "Connect OBS", Some("In OBS, open Settings → Stream, choose Custom, and paste these."), |ui| {
            let key = d.ingest.key.clone();
            for (i, (label, value)) in [("Server", server.to_string()), ("Stream key", key)].into_iter().enumerate() {
                if i > 0 {
                    separator(ui);
                }
                row(ui, label, |ui| {
                    let copied = self.copied.is_some_and(|(l, t)| l == label && t.elapsed().as_secs_f32() < 1.4);
                    if text_button(ui, if copied { "Copied" } else { "Copy" }, false).clicked() {
                        ui.ctx().copy_text(value.clone());
                        self.copied =
                            Some((if label == "Server" { "Server" } else { "Stream key" }, std::time::Instant::now()));
                    }
                    ui.label(RichText::new(value).monospace().color(theme::TEXT_2));
                });
            }
        });

        group(ui, "Stream to", Some("nicedelay sends the delayed stream here — instead of OBS."), |ui| {
            if d.destinations.is_empty() {
                Frame::new()
                    .inner_margin(14)
                    .show(ui, |ui| ui.label(RichText::new("No destinations yet.").color(theme::TEXT_3)));
            }
            let mut remove = None;
            for (i, dest) in d.destinations.iter_mut().enumerate() {
                if i > 0 {
                    separator(ui);
                }
                Frame::new().inner_margin(Margin { left: 14, right: 10, top: 10, bottom: 10 }).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        toggle(ui, &mut dest.enabled);
                        let known = SERVICES.iter().find(|(_, url)| *url == dest.url);
                        let w = (ui.available_width() - 40.0).max(120.0);
                        match known {
                            Some((name, _)) => {
                                ui.add_sized(
                                    Vec2::new(w * 0.4, 34.0),
                                    egui::Label::new(RichText::new(*name).font(theme::semibold(15.0))),
                                );
                            }
                            None => {
                                ui.add(
                                    TextEdit::singleline(&mut dest.url)
                                        .hint_text("rtmps://server/app")
                                        .desired_width(w * 0.4 - 8.0),
                                );
                            }
                        }
                        ui.add(
                            TextEdit::singleline(&mut dest.key)
                                .password(true)
                                .hint_text("Stream key")
                                .desired_width(w * 0.6 - 8.0),
                        );
                        if ui
                            .add(egui::Button::new(RichText::new("×").size(20.0).color(theme::TEXT_3)).frame(false))
                            .on_hover_text("Remove")
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                });
            }
            if let Some(i) = remove {
                d.destinations.remove(i);
            }
            separator(ui);
            Frame::new().inner_margin(Margin::symmetric(14, 12)).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (name, url) in SERVICES {
                        if pill_button(ui, &format!("+ {name}")).clicked() {
                            d.destinations.push(Destination {
                                name: (*name).into(),
                                url: (*url).into(),
                                key: String::new(),
                                enabled: true,
                            });
                        }
                    }
                    if pill_button(ui, "+ Other").clicked() {
                        d.destinations.push(Destination {
                            name: String::new(),
                            url: String::new(),
                            key: String::new(),
                            enabled: true,
                        });
                    }
                });
            });
        });

        group(ui, "Quality", None, |ui| {
            row(ui, "Resolution", |ui| {
                let current = RESOLUTIONS.iter().find(|r| r.2 == d.output.height).map_or("1080p", |r| r.0);
                if let Some(label) = choice(ui, &RESOLUTIONS.iter().map(|r| r.0).collect::<Vec<_>>(), current)
                    && let Some(&(_, w, h)) = RESOLUTIONS.iter().find(|r| r.0 == label)
                {
                    d.output.width = w;
                    d.output.height = h;
                }
            });
            separator(ui);
            row(ui, "Frame rate", |ui| {
                let current = if d.output.fps == 60 { "60" } else { "30" };
                if let Some(v) = choice(ui, &["30", "60"], current) {
                    d.output.fps = v.parse().unwrap_or(30);
                }
            });
            separator(ui);
            row(ui, "Bitrate", |ui| {
                ui.add(egui::DragValue::new(&mut d.output.video_bitrate).range(500..=50_000).speed(50).suffix(" kbps"));
            });
            separator(ui);
            row(ui, "Encoder", |ui| {
                let label = |e: &str| {
                    if e == "auto" { "Automatic".to_string() } else { e.trim_start_matches("h264_").to_uppercase() }
                };
                egui::ComboBox::from_id_salt("encoder").selected_text(label(&d.output.encoder)).show_ui(ui, |ui| {
                    ui.selectable_value(&mut d.output.encoder, "auto".to_string(), "Automatic");
                    for e in hw::encoder_candidates() {
                        ui.selectable_value(&mut d.output.encoder, (*e).to_string(), label(e));
                    }
                });
            });
        });

        group(ui, "Recording", None, |ui| {
            row(ui, "Keep the last", |ui| {
                egui::ComboBox::from_id_salt("buffer").selected_text(format!("{} minutes", d.buffer_minutes)).show_ui(
                    ui,
                    |ui| {
                        for m in BUFFER_MINUTES {
                            ui.selectable_value(&mut d.buffer_minutes, *m, format!("{m} minutes"));
                        }
                    },
                );
            });
            separator(ui);
            row(ui, "Rewind sound", |ui| {
                toggle(ui, &mut d.transition.sound);
            });
        });
    }
}

fn group(ui: &mut Ui, title: &str, note: Option<&str>, body: impl FnOnce(&mut Ui)) {
    ui.add_space(22.0);
    ui.label(
        RichText::new(title.to_uppercase()).font(theme::semibold(12.0)).color(theme::TEXT_3).extra_letter_spacing(1.0),
    );
    ui.add_space(2.0);
    Frame::new().fill(theme::SURFACE_2).corner_radius(12).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        body(ui);
    });
    if let Some(n) = note {
        ui.add_space(4.0);
        ui.label(RichText::new(n).size(12.5).color(theme::TEXT_3));
    }
}

fn separator(ui: &mut Ui) {
    let y = ui.cursor().top();
    ui.painter().hline(ui.max_rect().x_range(), y, Stroke::new(0.5, theme::HAIRLINE));
}

fn row(ui: &mut Ui, label: &str, right: impl FnOnce(&mut Ui)) {
    Frame::new().inner_margin(Margin::symmetric(14, 7)).show(ui, |ui| {
        ui.set_min_height(34.0);
        ui.horizontal(|ui| {
            ui.label(label);
            ui.with_layout(Layout::right_to_left(Align::Center), right);
        });
    });
}

fn text_button(ui: &mut Ui, text: &str, strong: bool) -> egui::Response {
    let font = if strong { theme::semibold(15.0) } else { theme::regular(15.0) };
    ui.add(egui::Button::new(RichText::new(text).font(font).color(theme::REPLAY)).frame(false))
}

fn pill_button(ui: &mut Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).size(14.0)).fill(theme::FILL).corner_radius(CornerRadius::same(255)))
}

/// A segmented choice; returns the newly picked option.
fn choice<'a>(ui: &mut Ui, options: &[&'a str], current: &str) -> Option<&'a str> {
    let mut picked = None;
    Frame::new().fill(theme::FILL).corner_radius(9).inner_margin(3).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        ui.horizontal(|ui| {
            for o in options.iter().rev() {
                let selected = *o == current;
                let b = egui::Button::new(RichText::new(*o).size(14.0).color(if selected {
                    theme::TEXT
                } else {
                    theme::TEXT_2
                }))
                .fill(if selected { theme::FILL_STRONG } else { Color32::TRANSPARENT })
                .corner_radius(7)
                .min_size(Vec2::new(54.0, 28.0));
                if ui.add(b).clicked() && !selected {
                    picked = Some(*o);
                }
            }
        });
    });
    picked
}

/// An iOS-style switch.
pub fn toggle(ui: &mut Ui, on: &mut bool) -> egui::Response {
    let size = Vec2::new(42.0, 26.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let bg = theme::FILL_STRONG.lerp_to_gamma(theme::GOOD, t);
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(255), bg);
    let r = size.y / 2.0 - 2.0;
    let x = egui::lerp(rect.left() + r + 2.0..=rect.right() - r - 2.0, t);
    p.circle_filled(egui::pos2(x, rect.center().y), r, Color32::WHITE);
    resp
}
