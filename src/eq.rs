//! Dedicated live equalizer controls and preset management.

use crate::{
    appearance,
    settings::{EqualizerPreset, Settings},
    widgets::{self, Icon},
};
use eframe::egui::{self, Align, Layout, RichText, Ui, Vec2};

/// The ten band gains of the built-in Voice preset. Voice boost (see
/// `settings::voice_boost_mix`) adds the same curve, so the two cannot drift.
pub const VOICE_GAINS: [f32; 10] = [-2.0, -2.0, -1.0, 1.0, 3.0, 3.0, 1.0, -1.0, -2.0, -2.0];

#[derive(Clone, Copy)]
struct BuiltinPreset {
    name: &'static str,
    preamp_db: f32,
    gains: [f32; 10],
}

const BUILTINS: &[BuiltinPreset] = &[
    BuiltinPreset {
        name: "Flat",
        preamp_db: 0.0,
        gains: [0.0; 10],
    },
    BuiltinPreset {
        name: "Podcast",
        preamp_db: -3.0,
        gains: [-3.0, -2.0, -1.0, 1.0, 3.0, 3.0, 2.0, 1.0, -1.0, -3.0],
    },
    BuiltinPreset {
        name: "Voice",
        preamp_db: -3.0,
        gains: VOICE_GAINS,
    },
    BuiltinPreset {
        name: "Rock",
        preamp_db: -3.0,
        gains: [3.0, 2.0, 1.0, -1.0, -2.0, 1.0, 2.0, 3.0, 2.0, 0.0],
    },
    BuiltinPreset {
        name: "Country",
        preamp_db: -3.0,
        gains: [2.0, 1.0, 0.0, -1.0, 1.0, 2.0, 3.0, 2.0, 1.0, 0.0],
    },
    BuiltinPreset {
        name: "Acoustic",
        preamp_db: -3.0,
        gains: [1.0, 0.0, -1.0, 0.0, 2.0, 2.0, 1.0, 2.0, 3.0, 2.0],
    },
    BuiltinPreset {
        name: "Pop",
        preamp_db: -3.0,
        gains: [-1.0, 0.0, 1.0, 2.0, 1.0, 0.0, 1.0, 2.0, 3.0, 2.0],
    },
    BuiltinPreset {
        name: "Jazz",
        preamp_db: -2.0,
        gains: [2.0, 1.0, 0.0, 1.0, -1.0, -1.0, 0.0, 1.0, 2.0, 2.0],
    },
    BuiltinPreset {
        name: "Classical",
        preamp_db: -3.0,
        gains: [2.0, 1.0, 0.0, -1.0, -2.0, -2.0, 0.0, 1.0, 2.0, 3.0],
    },
    BuiltinPreset {
        name: "Bass Boost",
        preamp_db: -4.0,
        gains: [4.0, 3.0, 2.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    },
];

const BAND_LABELS: [&str; 10] = [
    "31 Hz", "62 Hz", "125 Hz", "250 Hz", "500 Hz", "1 kHz", "2 kHz", "4 kHz", "8 kHz", "16 kHz",
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum PresetKey {
    Builtin(&'static str),
    Custom(usize),
}

type EqControls = (bool, f32, [f32; 10]);

fn eq_controls(settings: &Settings) -> EqControls {
    (
        settings.equalizer_enabled,
        settings.preamp_db,
        settings.eq_gains,
    )
}

fn settings_changed(before: EqControls, settings: &Settings, presets_changed: bool) -> bool {
    before != eq_controls(settings) || presets_changed
}

pub struct EqualizerWindow {
    pub open: bool,
    selected: Option<PresetKey>,
    save_as_open: bool,
    preset_name: String,
    delete_confirm: bool,
    message: Option<String>,
}

impl EqualizerWindow {
    pub fn new(settings: &Settings) -> Self {
        Self {
            open: true,
            selected: matching_preset(settings),
            save_as_open: false,
            preset_name: String::new(),
            delete_confirm: false,
            message: None,
        }
    }

    /// Draws live equalizer settings and a pinned Close footer. Returns true
    /// when a persisted setting or custom preset changed.
    pub fn show_body(&mut self, ui: &mut Ui, settings: &mut Settings) -> bool {
        if ui.input(|input| input.viewport().close_requested()) {
            self.open = false;
            return false;
        }

        let content_rect = ui.available_rect_before_wrap();
        let available = content_rect.size();
        // Match the custom action button's measured text height exactly.
        let button_height = (ui.text_style_height(&egui::TextStyle::Body) + 12.0).max(34.0);
        let footer_height = button_height + 16.0;
        let body_height = (available.y - footer_height - 8.0).max(80.0);
        let body_width = available.x;
        let before_controls = eq_controls(settings);
        let mut presets_changed = false;
        let mut preset_chosen = false;

        egui::ScrollArea::both()
            .id_salt("equalizer body")
            .auto_shrink([false, false])
            .max_width(body_width)
            .max_height(body_height)
            .show(ui, |ui| {
                ui.set_max_width(body_width);
                ui.heading("10-band equalizer");
                appearance::checkbox(ui, &mut settings.equalizer_enabled, "Enable equalizer");

                ui.horizontal(|ui| {
                    ui.label(RichText::new("Preamp").strong());
                    ui.add_space(8.0);
                    appearance::slider(
                        ui,
                        egui::Slider::new(&mut settings.preamp_db, -24.0..=12.0).suffix(" dB"),
                    );
                });

                let mut choice = self.selected.clone();
                let mut choose_requested = false;
                let previous_interact_size = ui.spacing().interact_size;
                let toolbar_height =
                    (ui.text_style_height(&egui::TextStyle::Body) + 12.0).max(34.0);
                ui.spacing_mut().interact_size.y = toolbar_height;
                ui.horizontal_wrapped(|ui| {
                    let preset_button =
                        widgets::text_menu_button(ui, &preset_label(&choice, settings));
                    egui::Popup::menu(&preset_button).show(|ui| {
                        let max_height = (ui.ctx().content_rect().height() - 32.).clamp(40., 320.);
                        egui::ScrollArea::vertical()
                            .id_salt("equalizer preset choices")
                            .max_height(max_height)
                            .show(ui, |ui| {
                                if ui.selectable_value(&mut choice, None, "Custom").clicked() {
                                    choose_requested = true;
                                    ui.close();
                                }
                                ui.separator();
                                for preset in BUILTINS {
                                    if ui
                                        .selectable_value(
                                            &mut choice,
                                            Some(PresetKey::Builtin(preset.name)),
                                            preset.name,
                                        )
                                        .clicked()
                                    {
                                        choose_requested = true;
                                        ui.close();
                                    }
                                }
                                if !settings.custom_eq_presets.is_empty() {
                                    ui.separator();
                                }
                                for (index, preset) in settings.custom_eq_presets.iter().enumerate()
                                {
                                    let duplicates = settings
                                        .custom_eq_presets
                                        .iter()
                                        .filter(|other| {
                                            other.name.eq_ignore_ascii_case(&preset.name)
                                        })
                                        .count()
                                        > 1;
                                    let label = if duplicates {
                                        format!("{} (custom {})", preset.name, index + 1)
                                    } else {
                                        preset.name.clone()
                                    };
                                    if ui
                                        .selectable_value(
                                            &mut choice,
                                            Some(PresetKey::Custom(index)),
                                            label,
                                        )
                                        .clicked()
                                    {
                                        choose_requested = true;
                                        ui.close();
                                    }
                                }
                            });
                    });

                    if self.save_as_open {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.preset_name)
                                .desired_width(170.0)
                                .char_limit(64)
                                .hint_text("Preset name"),
                        );
                        if widgets::text_button(ui, "Save").clicked() {
                            match self.save_as(settings) {
                                Ok(()) => {
                                    self.delete_confirm = false;
                                    self.message = Some("Preset saved".into());
                                    presets_changed = true;
                                }
                                Err(message) => self.message = Some(message),
                            }
                        }
                        if widgets::text_button(ui, "Cancel").clicked() {
                            self.save_as_open = false;
                            self.message = None;
                        }
                    } else {
                        if widgets::text_icon_button(ui, Icon::Plus, "Save as…").clicked() {
                            self.preset_name.clear();
                            self.save_as_open = true;
                            self.message = None;
                        }
                        let custom_selected = self.selected_custom_index(settings).is_some();
                        if ui
                            .add_enabled_ui(custom_selected, |ui| {
                                widgets::text_button(ui, "Update")
                            })
                            .inner
                            .clicked()
                        {
                            presets_changed |= self.update_selected(settings);
                            self.message = Some("Preset updated".into());
                        }
                        if ui
                            .add_enabled_ui(custom_selected, |ui| {
                                widgets::text_button(ui, "Delete")
                            })
                            .inner
                            .clicked()
                        {
                            self.delete_confirm = true;
                            self.message = None;
                        }
                    }
                });
                ui.spacing_mut().interact_size = previous_interact_size;

                if choose_requested {
                    self.select_preset(settings, choice.clone());
                    self.message = None;
                    preset_chosen = choice.is_some();
                }

                if self.delete_confirm {
                    ui.horizontal_wrapped(|ui| {
                        let name = self
                            .selected_custom_index(settings)
                            .and_then(|index| settings.custom_eq_presets.get(index))
                            .map(|preset| preset.name.as_str())
                            .unwrap_or("selected preset");
                        ui.label(format!("Delete custom preset ‘{name}’?"));
                        if widgets::text_button(ui, "Delete preset").clicked() {
                            let deleted = self.delete_selected(settings);
                            presets_changed |= deleted;
                            self.delete_confirm = false;
                            self.message = Some(if deleted {
                                "Preset deleted".into()
                            } else {
                                "Select a custom preset first.".into()
                            });
                        }
                        if widgets::text_button(ui, "Keep preset").clicked() {
                            self.delete_confirm = false;
                        }
                    });
                }
                if let Some(message) = &self.message {
                    ui.label(
                        RichText::new(message)
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                }

                ui.add_space(8.0);
                ui.add_enabled_ui(settings.equalizer_enabled, |ui| {
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    let label_height = ui.text_style_height(&egui::TextStyle::Body);
                    let unit_height = ui.text_style_height(&egui::TextStyle::Small);
                    let value_height = (label_height + ui.spacing().button_padding.y * 2.)
                        .max(ui.spacing().interact_size.y);
                    let rail_height = 164.;
                    let rail_width = label_height.max(ui.spacing().interact_size.y);
                    let column_width = ui
                        .painter()
                        .layout_no_wrap("16 kHz".into(), font, ui.visuals().text_color())
                        .size()
                        .x;
                    let column_width = (column_width + 16.).max(70.);
                    let gap = 10.;
                    let row_height =
                        label_height + 6. + rail_height + 8. + value_height + 4. + unit_height + 4.;
                    let row_width = column_width * BAND_LABELS.len() as f32
                        + gap * (BAND_LABELS.len() - 1) as f32;
                    // Reserve the full row before any child is drawn. Every
                    // column then has the same top, rail and value baseline.
                    let (row, _) = ui.allocate_exact_size(
                        Vec2::new(row_width, row_height),
                        egui::Sense::hover(),
                    );
                    for (index, label) in BAND_LABELS.iter().enumerate() {
                        let left = row.left() + index as f32 * (column_width + gap);
                        let center_x = left + column_width * 0.5;
                        let label_rect = egui::Rect::from_min_size(
                            egui::pos2(left, row.top()),
                            Vec2::new(column_width, label_height),
                        );
                        let mut label_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(label_rect)
                                .layout(Layout::top_down(Align::Center)),
                        );
                        label_ui.label(*label);

                        let slider_top = row.top() + label_height + 6.;
                        let slider_rect = egui::Rect::from_min_size(
                            egui::pos2(center_x - rail_width * 0.5, slider_top),
                            Vec2::new(rail_width, rail_height),
                        );
                        let mut slider_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(slider_rect)
                                .layout(Layout::top_down(Align::Min)),
                        );
                        slider_ui.spacing_mut().slider_width = rail_height;
                        let response = appearance::slider(
                            &mut slider_ui,
                            egui::Slider::new(&mut settings.eq_gains[index], -12.0..=12.0)
                                .vertical()
                                .show_value(false),
                        );
                        response.widget_info(|| {
                            egui::WidgetInfo::slider(
                                ui.is_enabled(),
                                settings.eq_gains[index] as f64,
                                *label,
                            )
                        });

                        let value_top = slider_top + rail_height + 8.;
                        let value_rect = egui::Rect::from_min_size(
                            egui::pos2(left, value_top),
                            Vec2::new(column_width, value_height),
                        );
                        let mut value_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(value_rect)
                                .layout(Layout::top_down(Align::Center)),
                        );
                        value_ui
                            .add(
                                egui::DragValue::new(&mut settings.eq_gains[index])
                                    .range(-12.0..=12.0)
                                    .speed(0.1)
                                    .custom_formatter(|value, _| format!("{value:+.1}")),
                            )
                            .on_hover_text(format!("{label} gain in dB"));

                        let unit_rect = egui::Rect::from_min_size(
                            egui::pos2(left, value_top + value_height + 4.),
                            Vec2::new(column_width, unit_height),
                        );
                        let mut unit_ui = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(unit_rect)
                                .layout(Layout::top_down(Align::Center)),
                        );
                        unit_ui.label(
                            RichText::new("dB")
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                });

                if !preset_chosen
                    && !matches!(self.selected.as_ref(), Some(PresetKey::Custom(_)))
                    && (before_controls.1, before_controls.2)
                        != (settings.preamp_db, settings.eq_gains)
                {
                    self.selected = None;
                }
            });

        let footer_rect = egui::Rect::from_min_size(
            egui::pos2(content_rect.left(), content_rect.bottom() - footer_height),
            Vec2::new(body_width, footer_height),
        );
        let (chrome_top, chrome_bottom) = appearance::chrome_colors(ui);
        ui.painter()
            .add(appearance::gradient(footer_rect, chrome_top, chrome_bottom));
        ui.painter().line_segment(
            [
                egui::pos2(footer_rect.left(), footer_rect.top() + 1.0),
                egui::pos2(footer_rect.right(), footer_rect.top() + 1.0),
            ],
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
        let button_row = egui::Rect::from_min_max(
            egui::pos2(footer_rect.left(), footer_rect.top() + 6.0),
            egui::pos2(footer_rect.right(), footer_rect.bottom() - 5.0),
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(button_row)
                .layout(Layout::right_to_left(Align::Center)),
            |footer| {
                footer.set_clip_rect(button_row.intersect(footer.clip_rect()));
                if widgets::text_button(footer, "Close").clicked() {
                    self.open = false;
                }
            },
        );
        settings_changed(before_controls, settings, presets_changed)
    }

    fn select_preset(&mut self, settings: &mut Settings, choice: Option<PresetKey>) {
        self.selected = choice.clone();
        self.delete_confirm = false;
        if let Some(key) = choice {
            apply_preset(settings, &key);
        }
    }

    fn selected_custom_index(&self, settings: &Settings) -> Option<usize> {
        match self.selected.as_ref() {
            Some(PresetKey::Custom(index)) if *index < settings.custom_eq_presets.len() => {
                Some(*index)
            }
            _ => None,
        }
    }

    fn save_as(&mut self, settings: &mut Settings) -> Result<(), String> {
        let name = sanitize_name(&self.preset_name);
        if name.is_empty() {
            return Err("Enter a preset name.".into());
        }
        if BUILTINS
            .iter()
            .any(|preset| preset.name.eq_ignore_ascii_case(&name))
        {
            return Err("Built-in preset names are protected.".into());
        }
        if settings
            .custom_eq_presets
            .iter()
            .any(|preset| preset.name.eq_ignore_ascii_case(&name))
        {
            return Err("That name already exists. Select it and choose Update.".into());
        }
        if settings.custom_eq_presets.len() >= MAX_CUSTOM_PRESETS {
            return Err(format!(
                "At most {MAX_CUSTOM_PRESETS} custom presets can be kept. Delete one first."
            ));
        }
        settings.custom_eq_presets.push(EqualizerPreset {
            name,
            preamp_db: settings.preamp_db,
            gains: settings.eq_gains,
        });
        self.selected = Some(PresetKey::Custom(settings.custom_eq_presets.len() - 1));
        self.preset_name.clear();
        self.save_as_open = false;
        Ok(())
    }

    fn update_selected(&mut self, settings: &mut Settings) -> bool {
        let Some(index) = self.selected_custom_index(settings) else {
            return false;
        };
        let preamp_db = settings.preamp_db;
        let gains = settings.eq_gains;
        let preset = &mut settings.custom_eq_presets[index];
        let changed = preset.preamp_db != preamp_db || preset.gains != gains;
        preset.preamp_db = preamp_db;
        preset.gains = gains;
        changed
    }

    fn delete_selected(&mut self, settings: &mut Settings) -> bool {
        let Some(index) = self.selected_custom_index(settings) else {
            return false;
        };
        settings.custom_eq_presets.remove(index);
        self.selected = None;
        true
    }
}

/// The same limit `Settings::normalize` applies when a profile is loaded; more
/// presets would be cut off at the next start.
const MAX_CUSTOM_PRESETS: usize = 200;

fn sanitize_name(name: &str) -> String {
    name.chars()
        .filter(|character| !character.is_control())
        .take(64)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn preset_label<'a>(selection: &Option<PresetKey>, settings: &'a Settings) -> &'a str {
    match selection {
        Some(PresetKey::Builtin(name)) => name,
        Some(PresetKey::Custom(index)) => settings
            .custom_eq_presets
            .get(*index)
            .map(|preset| preset.name.as_str())
            .unwrap_or("Custom"),
        None => "Custom",
    }
}

fn apply_preset(settings: &mut Settings, key: &PresetKey) {
    match key {
        PresetKey::Builtin(name) => {
            if let Some(preset) = BUILTINS.iter().find(|preset| preset.name == *name) {
                settings.equalizer_enabled = true;
                settings.preamp_db = preset.preamp_db;
                settings.eq_gains = preset.gains;
            }
        }
        PresetKey::Custom(index) => {
            if let Some(preset) = settings.custom_eq_presets.get(*index) {
                settings.equalizer_enabled = true;
                settings.preamp_db = preset.preamp_db;
                settings.eq_gains = preset.gains;
            }
        }
    }
}

fn matching_preset(settings: &Settings) -> Option<PresetKey> {
    if let Some(preset) = BUILTINS.iter().find(|preset| {
        (preset.preamp_db - settings.preamp_db).abs() < 0.01
            && preset
                .gains
                .iter()
                .copied()
                .zip(settings.eq_gains)
                .all(|(a, b)| (a - b).abs() < 0.01)
    }) {
        return Some(PresetKey::Builtin(preset.name));
    }
    settings
        .custom_eq_presets
        .iter()
        .enumerate()
        .find(|(_, preset)| {
            (preset.preamp_db - settings.preamp_db).abs() < 0.01
                && preset
                    .gains
                    .iter()
                    .copied()
                    .zip(settings.eq_gains)
                    .all(|(a, b)| (a - b).abs() < 0.01)
        })
        .map(|(index, _)| PresetKey::Custom(index))
}
