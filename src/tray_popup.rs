//! Compact, dark tray controls rendered inside the app-owned native viewport.
//!
//! This module returns one semantic action per frame. The application owns the
//! tray lifecycle, playback commands, sleep timer, and child viewport.

use eframe::egui::{self, Color32, RichText, Ui, Vec2};

const CANVAS: Color32 = Color32::from_rgb(0x0b, 0x0f, 0x18);
const SURFACE: Color32 = Color32::from_rgb(0x18, 0x20, 0x34);
const RAISED: Color32 = Color32::from_rgb(0x22, 0x2c, 0x42);
const SURFACE_SOFT: Color32 = Color32::from_rgb(0x2e, 0x3a, 0x54);
const BORDER: Color32 = Color32::from_rgb(0x3a, 0x47, 0x63);
const HAIRLINE: Color32 = Color32::from_rgba_premultiplied(18, 18, 18, 18);
const TEXT: Color32 = Color32::from_rgb(0xe4, 0xe8, 0xf0);
const STRONG: Color32 = Color32::WHITE;
const MUTED: Color32 = Color32::from_rgb(0x9a, 0xa7, 0xbd);
const ACCENT: Color32 = Color32::from_rgb(0xff, 0xb4, 0x5e);
const HEADER_TEXT_WIDTH: f32 = 260.0;

pub const DEFAULT_INNER_SIZE: [f32; 2] = [320.0, 520.0];
pub const MIN_INNER_SIZE: [f32; 2] = [300.0, 300.0];

#[derive(Clone, Debug, PartialEq)]
pub struct TrayPopupState {
    pub title: String,
    pub artist: String,
    pub position: f64,
    pub duration: f64,
    pub playing: bool,
    pub can_control: bool,
    pub speed: f32,
    pub skip_back: f64,
    pub skip_forward: f64,
    pub sleep_mode: String,
    pub sleep_deadline: Option<u64>,
    pub font_size: f32,
    /// A live stream is current: no skipping, speed, chapter or book timers, and
    /// the position is the listening time.
    pub live: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SleepChoice {
    Off,
    Minutes(u64),
    EndOfChapter,
    EndOfBook,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrayAction {
    Play,
    Pause,
    SkipBack,
    SkipForward,
    Next,
    SetSpeed(f32),
    SetSleep(SleepChoice),
    /// Add 15 minutes to the running minutes-based sleep timer.
    ExtendSleep,
    ShowPlayer,
    OpenOptions,
    OpenEqualizer,
    Quit,
}

/// Render the tray popup body in a bounded native viewport and return the
/// selected action. The caller applies actions to live application state.
pub fn show_body(ui: &mut Ui, state: &TrayPopupState) -> (Option<TrayAction>, f32) {
    let font_size = if state.font_size.is_finite() {
        state.font_size.clamp(9.0, 32.0)
    } else {
        14.0
    };
    let mut action = None;
    // The player may use a light theme; cover the native viewport clear color
    // as well as the rounded menu surface so no white corners remain.
    ui.painter().rect_filled(ui.max_rect(), 0., CANVAS);

    let measured = ui.scope(|ui| {
        apply_dark_style(ui, font_size);
        egui::Frame::new()
            .fill(RAISED)
            .stroke(egui::Stroke::new(1.0, BORDER))
            .corner_radius(egui::CornerRadius::same(12))
            .inner_margin(6)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("bs-player-tray-popup")
                    .auto_shrink([false, true])
                    .max_height(ui.available_height())
                    .show(ui, |ui| {
                        let width = ui.available_width();
                        ui.set_min_width(width);
                        ui.set_max_width(width);
                        show_header(ui, state);
                        ui.add_space(5.0);
                        ui.separator();

                        let toggle = if state.playing {
                            TrayAction::Pause
                        } else {
                            TrayAction::Play
                        };
                        let toggle_label = if state.playing { "Pause" } else { "Play" };
                        if menu_button(ui, toggle_label, state.can_control).clicked() {
                            action = Some(toggle);
                        }
                        if menu_button(
                            ui,
                            &skip_label("Skip back", state.skip_back),
                            state.can_control && !state.live,
                        )
                        .clicked()
                        {
                            action = Some(TrayAction::SkipBack);
                        }
                        if menu_button(
                            ui,
                            &skip_label("Skip forward", state.skip_forward),
                            state.can_control && !state.live,
                        )
                        .clicked()
                        {
                            action = Some(TrayAction::SkipForward);
                        }
                        if menu_button(
                            ui,
                            if state.live {
                                "Next station"
                            } else {
                                "Next track"
                            },
                            state.can_control,
                        )
                        .clicked()
                        {
                            action = Some(TrayAction::Next);
                        }

                        ui.add_space(3.0);
                        ui.separator();
                        show_speed(ui, state, &mut action, font_size);
                        show_sleep(ui, state, &mut action, font_size);
                        if sleep_extendable(state)
                            && menu_button(ui, "Add 15 minutes to sleep timer", state.can_control)
                                .clicked()
                        {
                            action = Some(TrayAction::ExtendSleep);
                        }

                        ui.add_space(3.0);
                        ui.separator();
                        if menu_button(ui, "Show BS Player", true).clicked() {
                            action = Some(TrayAction::ShowPlayer);
                        }
                        if menu_button(ui, "Options…", true).clicked() {
                            action = Some(TrayAction::OpenOptions);
                        }
                        if menu_button(ui, "Equalizer…", true).clicked() {
                            action = Some(TrayAction::OpenEqualizer);
                        }
                        ui.add_space(2.0);
                        if menu_button(ui, "Quit", true).clicked() {
                            action = Some(TrayAction::Quit);
                        }
                    })
                    .content_size
                    .y
            })
            .inner
    });

    // Include the frame's six-point padding and one-point border on each side.
    (action, measured.inner + 14.)
}

fn show_header(ui: &mut Ui, state: &TrayPopupState) {
    if state.title.trim().is_empty() {
        ui.scope(|ui| {
            ui.set_max_width(HEADER_TEXT_WIDTH.min(ui.available_width()));
            ui.label(RichText::new("Nothing playing").color(STRONG).strong());
        });
    } else {
        ui.horizontal_top(|ui| {
            let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(16.0), egui::Sense::hover());
            crate::widgets::icon(
                ui.painter(),
                icon_rect,
                if state.playing {
                    crate::widgets::Icon::Pause
                } else {
                    crate::widgets::Icon::Play
                },
                ACCENT,
            );
            ui.vertical(|ui| {
                let text_width = HEADER_TEXT_WIDTH.min(ui.available_width());
                ui.set_min_width(text_width);
                ui.set_max_width(text_width);
                ui.add(egui::Label::new(RichText::new(&state.title).color(STRONG).strong()).wrap());
                if !state.artist.trim().is_empty() {
                    ui.add(egui::Label::new(RichText::new(&state.artist).color(MUTED)).wrap());
                }
                ui.add(
                    egui::Label::new(RichText::new(header_progress(state)).small().color(MUTED))
                        .wrap(),
                );
                if state.duration.is_finite() && state.duration > 0.0 {
                    let fraction = (state.position / state.duration).clamp(0.0, 1.0) as f32;
                    let (rect, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 3.0),
                        egui::Sense::hover(),
                    );
                    ui.painter().rect_filled(rect, 1.5, RAISED);
                    if fraction > 0.0 {
                        let fill = egui::Rect::from_min_size(
                            rect.min,
                            Vec2::new(rect.width() * fraction, rect.height()),
                        );
                        ui.painter().rect_filled(fill, 1.5, ACCENT);
                    }
                }
            });
        });
    }
}

/// The small line under the title: `1:02 of 45:00 · 1.50×` for a file, `Live ·
/// 1:02` (the listening time) for a stream.
fn header_progress(state: &TrayPopupState) -> String {
    if state.live {
        return format!("Live · {}", format_time(state.position));
    }
    let mut progress = format!(
        "{} of {}",
        format_time(state.position),
        format_time(state.duration)
    );
    if (state.speed - 1.0).abs() > 0.01 {
        progress.push_str(&format!(" · {}×", format_speed(state.speed)));
    }
    progress
}

fn show_speed(
    ui: &mut Ui,
    state: &TrayPopupState,
    action: &mut Option<TrayAction>,
    font_size: f32,
) {
    let selected = if state.live {
        "Speed   1×".to_owned()
    } else {
        format!("Speed   {}×", format_speed(state.speed))
    };
    let button = submenu_button(ui, &selected, state.can_control && !state.live);
    egui::Popup::menu(&button)
        .frame(egui::Frame::popup(ui.style()))
        .show(|choices| {
            apply_dark_style(choices, font_size);
            egui::ScrollArea::vertical()
                .max_height(360.)
                .show(choices, |choices| {
                    for speed in [0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0] {
                        if choices
                            .selectable_label(
                                (speed - state.speed).abs() < 0.01,
                                format!("{}×", format_speed(speed)),
                            )
                            .clicked()
                        {
                            *action = Some(TrayAction::SetSpeed(speed));
                            choices.close();
                        }
                    }
                });
        });
}

fn show_sleep(
    ui: &mut Ui,
    state: &TrayPopupState,
    action: &mut Option<TrayAction>,
    font_size: f32,
) {
    let selected = sleep_status(state);
    let button = submenu_button(ui, &format!("Sleep timer   {selected}"), state.can_control);
    egui::Popup::menu(&button)
        .frame(egui::Frame::popup(ui.style()))
        .show(|choices| {
            apply_dark_style(choices, font_size);
            egui::ScrollArea::vertical()
                .max_height(360.)
                .show(choices, |choices| {
                    for (label, choice) in [
                        ("Off", SleepChoice::Off),
                        ("15 min", SleepChoice::Minutes(15)),
                        ("30 min", SleepChoice::Minutes(30)),
                        ("45 min", SleepChoice::Minutes(45)),
                        ("1 hour", SleepChoice::Minutes(60)),
                        ("90 min", SleepChoice::Minutes(90)),
                        ("End of chapter", SleepChoice::EndOfChapter),
                        ("End of book", SleepChoice::EndOfBook),
                    ] {
                        // A live stream has no chapter or book to end.
                        if state.live
                            && matches!(choice, SleepChoice::EndOfChapter | SleepChoice::EndOfBook)
                        {
                            continue;
                        }
                        if choices
                            .selectable_label(sleep_choice_active(state, choice), label)
                            .clicked()
                        {
                            *action = Some(TrayAction::SetSleep(choice));
                            choices.close();
                        }
                    }
                });
        });
}

fn submenu_button(ui: &mut Ui, label: &str, enabled: bool) -> egui::Response {
    let response = menu_button(ui, label, enabled);
    let x = response.rect.right() - 14.;
    let y = response.rect.center().y;
    let color = if enabled { TEXT } else { MUTED };
    ui.painter().line_segment(
        [egui::pos2(x - 2., y - 4.), egui::pos2(x + 2., y)],
        egui::Stroke::new(1.3, color),
    );
    ui.painter().line_segment(
        [egui::pos2(x + 2., y), egui::pos2(x - 2., y + 4.)],
        egui::Stroke::new(1.3, color),
    );
    response
}

fn apply_dark_style(ui: &mut Ui, font_size: f32) {
    let mut style = ui.style().as_ref().clone();
    style.visuals = egui::Visuals::dark();
    style.visuals.panel_fill = CANVAS;
    style.visuals.window_fill = RAISED;
    style.visuals.extreme_bg_color = SURFACE;
    style.visuals.faint_bg_color = SURFACE_SOFT;
    style.visuals.override_text_color = Some(TEXT);
    style.visuals.weak_text_color = Some(MUTED);
    style.visuals.hyperlink_color = ACCENT;
    style.visuals.selection.bg_fill = SURFACE_SOFT;
    style.visuals.selection.stroke.color = ACCENT;
    style.visuals.widgets.noninteractive.bg_fill = SURFACE;
    style.visuals.widgets.noninteractive.fg_stroke.color = TEXT;
    style.visuals.widgets.noninteractive.bg_stroke.color = HAIRLINE;
    style.visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
    style.visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    style.visuals.widgets.inactive.fg_stroke.color = TEXT;
    style.visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
    style.visuals.widgets.hovered.bg_fill = SURFACE_SOFT;
    style.visuals.widgets.hovered.weak_bg_fill = SURFACE_SOFT;
    style.visuals.widgets.hovered.fg_stroke.color = STRONG;
    style.visuals.widgets.hovered.bg_stroke.color = BORDER;
    style.visuals.widgets.active.bg_fill = SURFACE_SOFT;
    style.visuals.widgets.active.weak_bg_fill = SURFACE_SOFT;
    style.visuals.widgets.active.fg_stroke.color = STRONG;
    style.visuals.widgets.active.bg_stroke.color = ACCENT;
    style.visuals.widgets.open.bg_fill = SURFACE;
    style.visuals.widgets.open.weak_bg_fill = SURFACE;
    style.visuals.widgets.open.fg_stroke.color = STRONG;
    style.visuals.widgets.open.bg_stroke.color = BORDER;
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(font_size));
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::proportional(font_size),
    );
    style.text_styles.insert(
        egui::TextStyle::Small,
        egui::FontId::proportional((font_size - 2.0).max(10.0)),
    );
    style.spacing.item_spacing = Vec2::new(6.0, 3.0);
    style.spacing.button_padding = Vec2::new(10.0, 4.0);
    style.spacing.interact_size.y = font_size + 10.0;
    ui.set_style(style);
}

fn menu_button(ui: &mut Ui, label: &str, enabled: bool) -> egui::Response {
    use crate::widgets::Icon;
    let width = ui.available_width();
    let color = if enabled {
        TEXT
    } else {
        Color32::from_rgb(0x6b, 0x7a, 0x96)
    };
    // Keep icon and submenu-arrow lanes outside the wrapping label. The popup
    // already scrolls vertically and resizes from its measured content height.
    let galley = ui.painter().layout(
        label.to_owned(),
        egui::TextStyle::Button.resolve(ui.style()),
        color,
        (width - 36. - 28.).max(1.),
    );
    let height = ui.spacing().interact_size.y.max(galley.size().y + 8.);
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(" ")
            .frame(false)
            .min_size(Vec2::new(width, height)),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
    if response.hovered() || response.has_focus() {
        ui.painter().rect_filled(response.rect, 6., SURFACE_SOFT);
    }
    let icon = match label {
        "Play" => Some(Icon::Play),
        "Pause" => Some(Icon::Pause),
        "Next track" => Some(Icon::Next),
        "Show BS Player" => Some(Icon::Music),
        "Options…" => Some(Icon::Settings),
        "Equalizer…" => Some(Icon::Equalizer),
        text if text.starts_with("Speed") => Some(Icon::Clock),
        text if text.starts_with("Sleep timer") => Some(Icon::Moon),
        text if text.starts_with("Add 15 minutes") => Some(Icon::Moon),
        text if text.starts_with("Skip back") => Some(Icon::Previous),
        text if text.starts_with("Skip forward") => Some(Icon::Next),
        _ => None,
    };
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(response.rect.left() + 18., response.rect.center().y),
        Vec2::splat(16.),
    );
    if let Some(icon) = icon {
        crate::widgets::icon(ui.painter(), icon_rect, icon, color);
    } else if label == "Quit" {
        ui.painter().line_segment(
            [
                icon_rect.left_top() + Vec2::splat(3.),
                icon_rect.right_bottom() - Vec2::splat(3.),
            ],
            egui::Stroke::new(1.5, color),
        );
        ui.painter().line_segment(
            [
                icon_rect.right_top() + Vec2::new(-3., 3.),
                icon_rect.left_bottom() + Vec2::new(3., -3.),
            ],
            egui::Stroke::new(1.5, color),
        );
    }
    ui.painter().galley(
        egui::pos2(
            response.rect.left() + 36.,
            response.rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        color,
    );
    response
}

fn skip_label(prefix: &str, seconds: f64) -> String {
    if seconds.is_finite() && seconds > 0.0 {
        format!("{prefix} {:.0} s", seconds.round())
    } else {
        prefix.to_owned()
    }
}

fn format_time(seconds: f64) -> String {
    let seconds = if seconds.is_finite() {
        seconds.max(0.0) as u64
    } else {
        0
    };
    let hours = seconds / 3600;
    let minutes = seconds / 60 % 60;
    let remainder = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{remainder:02}")
    } else {
        format!("{minutes}:{remainder:02}")
    }
}

fn format_speed(speed: f32) -> String {
    let speed = if speed.is_finite() { speed } else { 1.0 };
    if speed.fract().abs() < 0.001 {
        format!("{speed:.0}")
    } else {
        format!("{speed:.2}")
    }
}

fn sleep_status(state: &TrayPopupState) -> String {
    if let Some(deadline) = state.sleep_deadline {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let minutes = deadline.saturating_sub(now).div_ceil(60);
        return format!("{minutes} min left");
    }
    match state.sleep_mode.as_str() {
        "chapter" => "End of chapter".into(),
        "book" => "End of book".into(),
        _ => "Off".into(),
    }
}

/// The "add 15 minutes" row is offered only while a minutes-based timer runs:
/// not with no timer, and not with "end of chapter" or "end of book".
pub fn sleep_extendable(state: &TrayPopupState) -> bool {
    state.sleep_deadline.is_some() && state.sleep_mode == "duration"
}

fn sleep_choice_active(state: &TrayPopupState, choice: SleepChoice) -> bool {
    match choice {
        SleepChoice::Off => state.sleep_deadline.is_none() && state.sleep_mode == "off",
        SleepChoice::Minutes(_) => false,
        SleepChoice::EndOfChapter => {
            state.sleep_deadline.is_none() && state.sleep_mode == "chapter"
        }
        SleepChoice::EndOfBook => state.sleep_deadline.is_none() && state.sleep_mode == "book",
    }
}
