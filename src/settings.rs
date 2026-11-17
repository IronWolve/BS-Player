//! Settings data and the Options dialog.
//!
//! The options viewport edits a private draft. `show_body` returns a snapshot
//! only for Apply or OK; closing or cancelling restores the last applied value.

use crate::gui_cache::columns::{self, COLUMNS, COUNT as COLUMN_COUNT, LEGACY_COUNT};
use crate::winintegration::AssociationState;
use eframe::egui;
use egui::{Color32, Context, RichText, Ui, Vec2};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashSet;
use std::ops::RangeInclusive;

// One set of ranges for the Options sliders and for loading saved settings, so
// a slider never shows a value other than the one that is active.
const FONT_SIZE_RANGE: RangeInclusive<f32> = 10.0..=26.0;
const UI_SCALE_RANGE: RangeInclusive<f32> = 0.75..=1.5;
const ROW_HEIGHT_RANGE: RangeInclusive<f32> = 20.0..=44.0;
const MAX_SHORTCUTS: usize = 64;
const MAX_EXCLUDED_EXTENSIONS_CHARS: usize = 4096;
const MAX_CUSTOM_EQ_PRESETS: usize = 200;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shortcut {
    pub action: String,
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EqualizerPreset {
    pub name: String,
    pub preamp_db: f32,
    pub gains: [f32; 10],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub accent: [u8; 3],
    pub font_size: f32,
    pub ui_scale: f32,
    pub row_height: f32,
    pub show_library: bool,
    pub show_artwork: bool,
    /// Small cover thumbnails inside the Title column of the file list.
    pub show_list_art: bool,
    pub show_status: bool,
    pub always_on_top: bool,
    pub tray_enabled: bool,
    pub minimize_to_tray: bool,
    pub close_to_tray: bool,
    /// Start hidden in the tray instead of showing the player window. Only
    /// applies when the tray is available; the tray menu restores the window.
    pub start_minimized: bool,
    pub artwork_fit: String,
    pub prefer_embedded_art: bool,
    pub remember_positions: bool,
    pub restore_session: bool,
    pub startup_behavior: String,
    pub resume_rewind: f64,
    /// Audiobooks rewind further the longer the player was away; the fixed
    /// `resume_rewind` stays the minimum. Music keeps the fixed value.
    pub smart_rewind: bool,
    pub checkpoint_seconds: u64,
    pub music_speed: f32,
    pub preserve_pitch: bool,
    pub book_speed: f32,
    pub remember_book_speed: bool,
    /// Shorten silences longer than 0.6 s to a quarter of a second while an
    /// audiobook or podcast plays (the engine's silence stage; positions stay in
    /// source time). Off by default; old profiles load it off.
    pub skip_silence: bool,
    pub skip_back: f64,
    pub skip_forward: f64,
    /// Missing in older profiles: inherit their shared skip intervals once.
    #[serde(default)]
    pub audiobook_skip_back: Option<f64>,
    #[serde(default)]
    pub audiobook_skip_forward: Option<f64>,
    pub auto_advance: bool,
    pub stop_at_book_end: bool,
    /// Join consecutive tracks without a gap when they share the sample rate and
    /// channel layout and play at the same speed. On by default; an old profile
    /// without the field loads it on.
    pub gapless: bool,
    pub output_device: String,
    pub mono: bool,
    pub balance: f32,
    pub preamp_db: f32,
    pub limiter: bool,
    pub replaygain: String,
    pub replaygain_fallback_db: f32,
    pub replaygain_prevent_clipping: bool,
    pub equalizer_enabled: bool,
    pub eq_gains: [f32; 10],
    /// Voice boost: the engine adds the Voice curve and a little preamp on top
    /// of the saved equalizer (see `engine_settings`); the saved values stay.
    pub voice_boost: bool,
    pub custom_eq_presets: Vec<EqualizerPreset>,
    pub recursive_scan: bool,
    pub excluded_extensions: String,
    pub scan_on_start: bool,
    pub scan_interval_minutes: u64,
    pub external_files_append: bool,
    pub playlist_relative_paths: bool,
    /// Linux AppImage only: add BS Player to the desktop menu and give the dock and
    /// the AppImage file the player icon (see `desktop_entry`).
    pub desktop_integration: bool,
    pub remove_played: bool,
    pub private_listening: bool,
    pub confirm_remove: bool,
    pub sleep_fade_seconds: u64,
    pub shortcuts: Vec<Shortcut>,
    #[serde(default = "legacy_shortcut_defaults_version")]
    shortcut_defaults_version: u8,
    /// Visibility of the six columns older profiles already stored, by position
    /// in the column table. Kept in this shape so older builds still read it.
    pub visible_columns: [bool; LEGACY_COUNT],
    /// Ids of the visible columns added since (Album, Year, ...). Unknown ids are
    /// ignored, so a profile from a newer build never breaks this one.
    pub extra_columns: Vec<String>,
    pub double_click_plays: bool,
    pub follow_current: bool,
    pub queue_skip_missing: bool,
    /// The right-hand time label on the player bar shows the listening time
    /// left at the current speed instead of the file time left.
    pub show_remaining_time: bool,
    /// Windows: the play/pause, next, previous and stop keys of the keyboard work
    /// while another program has the focus (needs the tray icon). Other systems
    /// ignore it.
    pub media_keys: bool,
    /// The BOOKS list in the sidebar files its books under author headings
    /// (only when the library has more than eight books).
    pub group_books_by_author: bool,
    /// Load a system font for Chinese, Japanese, Korean, Thai, Indic, Hebrew or
    /// Arabic text, once, when titles in that script exist in the library. Off:
    /// nothing is ever loaded and such titles may show as boxes.
    pub fallback_fonts: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "light".into(),
            accent: [0x26, 0x89, 0xef],
            font_size: 14.0,
            ui_scale: 1.0,
            row_height: 28.0,
            show_library: true,
            show_artwork: true,
            show_list_art: true,
            show_status: true,
            always_on_top: false,
            tray_enabled: true,
            minimize_to_tray: false,
            close_to_tray: false,
            start_minimized: false,
            artwork_fit: "fit".into(),
            prefer_embedded_art: true,
            remember_positions: true,
            restore_session: true,
            startup_behavior: "previous".into(),
            resume_rewind: 0.0,
            smart_rewind: true,
            checkpoint_seconds: 15,
            music_speed: 1.0,
            preserve_pitch: true,
            book_speed: 1.0,
            remember_book_speed: true,
            skip_silence: false,
            skip_back: 15.0,
            skip_forward: 30.0,
            audiobook_skip_back: Some(15.0),
            audiobook_skip_forward: Some(30.0),
            auto_advance: true,
            stop_at_book_end: true,
            gapless: true,
            output_device: String::new(),
            mono: false,
            balance: 0.0,
            preamp_db: 0.0,
            limiter: true,
            replaygain: "off".into(),
            replaygain_fallback_db: 0.0,
            replaygain_prevent_clipping: true,
            equalizer_enabled: false,
            eq_gains: [0.0; 10],
            voice_boost: false,
            custom_eq_presets: Vec::new(),
            recursive_scan: true,
            excluded_extensions: "part, tmp".into(),
            scan_on_start: false,
            scan_interval_minutes: 0,
            external_files_append: true,
            playlist_relative_paths: true,
            desktop_integration: true,
            remove_played: false,
            private_listening: false,
            confirm_remove: true,
            sleep_fade_seconds: 0,
            shortcuts: default_shortcuts(),
            shortcut_defaults_version: 1,
            visible_columns: [true; LEGACY_COUNT],
            extra_columns: Vec::new(),
            double_click_plays: true,
            follow_current: true,
            queue_skip_missing: false,
            show_remaining_time: false,
            media_keys: true,
            group_books_by_author: true,
            fallback_fonts: true,
        }
    }
}

impl Settings {
    /// Clamp persisted values and replace unknown enum strings with safe defaults.
    pub fn normalize(&mut self) {
        if self.shortcut_defaults_version == 0 {
            for shortcut in &mut self.shortcuts {
                match (shortcut.action.as_str(), shortcut.key.as_str()) {
                    ("Seek backward", "Left") => shortcut.key = "[".into(),
                    ("Seek forward", "Right") => shortcut.key = "]".into(),
                    _ => {}
                }
            }
            self.shortcut_defaults_version = 1;
        }
        if self.theme != "system" && crate::themes::find(&self.theme).is_none() {
            self.theme = "light".into();
        }
        if !["fit", "fill", "original"].contains(&self.artwork_fit.as_str()) {
            self.artwork_fit = "fit".into();
        }
        if !["paused", "previous", "play"].contains(&self.startup_behavior.as_str()) {
            self.startup_behavior = "paused".into();
        }
        if !["off", "track", "album"].contains(&self.replaygain.as_str()) {
            self.replaygain = "off".into();
        }
        self.font_size = finite_clamp_range(self.font_size, &FONT_SIZE_RANGE, 14.0);
        self.ui_scale = finite_clamp_range(self.ui_scale, &UI_SCALE_RANGE, 1.0);
        self.row_height = finite_clamp_range(self.row_height, &ROW_HEIGHT_RANGE, 28.0);
        self.resume_rewind = finite_clamp_f64(self.resume_rewind, 0.0, 120.0, 0.0);
        self.checkpoint_seconds = self.checkpoint_seconds.clamp(1, 300);
        self.music_speed = finite_clamp(self.music_speed, 0.5, 2.0, 1.0);
        self.book_speed = finite_clamp(self.book_speed, 0.5, 3.0, 1.0);
        self.skip_back = finite_clamp_f64(self.skip_back, 1.0, 600.0, 15.0);
        self.skip_forward = finite_clamp_f64(self.skip_forward, 1.0, 600.0, 30.0);
        self.audiobook_skip_back = Some(finite_clamp_f64(self.audiobook_skip_back.unwrap_or(self.skip_back), 1., 600., self.skip_back));
        self.audiobook_skip_forward = Some(finite_clamp_f64(self.audiobook_skip_forward.unwrap_or(self.skip_forward), 1., 600., self.skip_forward));
        if let Some((end, _)) = self.excluded_extensions.char_indices().nth(MAX_EXCLUDED_EXTENSIONS_CHARS) {
            self.excluded_extensions.truncate(end);
        }
        self.balance = finite_clamp(self.balance, -1.0, 1.0, 0.0);
        self.preamp_db = finite_clamp(self.preamp_db, -24.0, 12.0, 0.0);
        self.replaygain_fallback_db = finite_clamp(self.replaygain_fallback_db, -24.0, 12.0, 0.0);
        for gain in &mut self.eq_gains {
            *gain = finite_clamp(*gain, -12.0, 12.0, 0.0);
        }
        self.custom_eq_presets.truncate(MAX_CUSTOM_EQ_PRESETS);
        for (index, preset) in self.custom_eq_presets.iter_mut().enumerate() {
            preset.name = preset
                .name
                .chars()
                .filter(|character| !character.is_control())
                .take(64)
                .collect::<String>()
                .trim()
                .to_owned();
            if preset.name.is_empty() {
                preset.name = format!("Custom preset {}", index + 1);
            }
            preset.preamp_db = finite_clamp(preset.preamp_db, -24.0, 12.0, 0.0);
            for gain in &mut preset.gains {
                *gain = finite_clamp(*gain, -12.0, 12.0, 0.0);
            }
        }
        self.scan_interval_minutes = self.scan_interval_minutes.min(10080);
        self.sleep_fade_seconds = self.sleep_fade_seconds.min(600);
        self.shortcuts.truncate(MAX_SHORTCUTS);
        for key in &mut self.shortcuts {
            key.key = key.key.trim().to_owned();
        }
        self.add_missing_shortcut_actions();
        // Re-saving the mask drops unknown or repeated column ids and restores
        // the one-visible-column rule.
        let mut mask = self.column_mask();
        ensure_visible_column(&mut mask);
        self.set_column_mask(&mask);
    }

    /// Profiles saved before an action existed have no entry for it. Add one
    /// with the default key so the editor lists it and it works at once; when
    /// that key already belongs to another action the new one starts unbound
    /// (an empty key), so an upgrade never creates a conflict.
    fn add_missing_shortcut_actions(&mut self) {
        for (action, key) in DEFAULT_SHORTCUTS {
            if self.shortcuts.len() >= MAX_SHORTCUTS {
                break;
            }
            if self
                .shortcuts
                .iter()
                .any(|shortcut| shortcut.action == action)
            {
                continue;
            }
            let wanted = canonical_shortcut(key);
            let taken = self
                .shortcuts
                .iter()
                .any(|shortcut| canonical_shortcut(&shortcut.key) == wanted);
            self.shortcuts.push(Shortcut {
                action: action.into(),
                key: if taken { String::new() } else { key.into() },
            });
        }
    }

    /// Visibility of every file-table column, indexed like `COLUMNS`. Unknown
    /// ids in `extra_columns` are ignored.
    pub fn column_mask(&self) -> [bool; COLUMN_COUNT] {
        let mut mask = [false; COLUMN_COUNT];
        mask[..LEGACY_COUNT].copy_from_slice(&self.visible_columns);
        for id in &self.extra_columns {
            if let Some(index) = columns::index_of(id).filter(|index| *index >= LEGACY_COUNT) {
                mask[index] = true;
            }
        }
        mask
    }

    /// Store a visibility mask: the first columns in `visible_columns`, the
    /// rest by id in `extra_columns` (table order, no duplicates).
    pub fn set_column_mask(&mut self, mask: &[bool; COLUMN_COUNT]) {
        self.visible_columns.copy_from_slice(&mask[..LEGACY_COUNT]);
        let extras = (LEGACY_COUNT..COLUMN_COUNT).filter(|index| mask[*index]);
        let extras = extras.map(|index| COLUMNS[index].id);
        if !self
            .extra_columns
            .iter()
            .map(String::as_str)
            .eq(extras.clone())
        {
            self.extra_columns = extras.map(str::to_owned).collect();
        }
    }

    /// Show or hide one column by table position, keeping at least one visible.
    /// Returns false for an unknown column or when it would hide the last one.
    pub fn set_column_visible(&mut self, index: usize, visible: bool) -> bool {
        let mut mask = self.column_mask();
        if !set_visible_column(&mut mask, index, visible) {
            return false;
        }
        self.set_column_mask(&mask);
        true
    }

    pub fn skip_steps(&self, audiobook: bool) -> (f64, f64) {
        if audiobook {
            (self.audiobook_skip_back.unwrap_or(self.skip_back), self.audiobook_skip_forward.unwrap_or(self.skip_forward))
        } else { (self.skip_back, self.skip_forward) }
    }
}

/// Keep at least one file-table column visible. Title / filename is the
/// fallback for profiles saved before this invariant was enforced.
pub fn ensure_visible_column(mask: &mut [bool; COLUMN_COUNT]) {
    if !mask.iter().any(|visible| *visible) {
        mask[columns::COL_TITLE] = true;
    }
}

/// Apply a file-column visibility request while keeping at least one column.
/// Returns false when the index is invalid or the request would hide the last
/// visible column.
pub fn set_visible_column(mask: &mut [bool; COLUMN_COUNT], index: usize, visible: bool) -> bool {
    let Some(current) = mask.get(index).copied() else {
        return false;
    };
    if current == visible {
        return true;
    }
    if !visible && mask.iter().filter(|is_visible| **is_visible).count() == 1 {
        return false;
    }
    mask[index] = visible;
    true
}

fn legacy_shortcut_defaults_version() -> u8 {
    0
}

fn finite_clamp(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

fn finite_clamp_range(value: f32, range: &RangeInclusive<f32>, fallback: f32) -> f32 {
    finite_clamp(value, *range.start(), *range.end(), fallback)
}

fn finite_clamp_f64(value: f64, min: f64, max: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

/// Every bindable action with its default key, in the order the editor lists
/// them. Plain Up, Down and Enter are not used: they move through the file
/// list and open the selected file. `,` `.` `/` sit side by side on the bottom
/// key row of the common layouts.
pub const DEFAULT_SHORTCUTS: [(&str, &str); 22] = [
    ("Play / pause", "Space"),
    ("Stop", "S"),
    ("Previous track", "Ctrl+Left"),
    ("Next track", "Ctrl+Right"),
    ("Seek backward", "["),
    ("Seek forward", "]"),
    ("Open files", "Ctrl+O"),
    ("Open folder", "Ctrl+Shift+O"),
    ("Volume up", "Ctrl+Up"),
    ("Volume down", "Ctrl+Down"),
    ("Mute", "M"),
    ("Speed faster", "."),
    ("Speed slower", ","),
    ("Reset speed", "/"),
    ("Add bookmark", "Ctrl+B"),
    ("Focus search", "Ctrl+F"),
    ("Remove selected", "Delete"),
    ("Next bookmark", "Alt+Right"),
    ("Previous bookmark", "Alt+Left"),
    ("Next chapter", "Alt+Down"),
    ("Previous chapter", "Alt+Up"),
    ("Undo", "Ctrl+Z"),
];

/// Seconds to rewind when listening resumes after `away` seconds. A short gap
/// needs none; the longer the gap, the more of the previous passage is replayed.
pub fn smart_rewind_seconds(away: u64) -> f64 {
    match away {
        0..=19 => 0.,
        20..=59 => 2.,
        60..=599 => 4.,
        600..=3599 => 8.,
        3600..=43_199 => 15.,
        43_200..=259_199 => 30.,
        _ => 45.,
    }
}

/// How far before the stored position a resume starts. `fixed` is the Resume
/// rewind setting and stays the minimum. With smart rewind on, an audiobook
/// also rewinds by the table value for the time `away` (`None` when the last
/// listening time is unknown); music always uses the fixed value.
pub fn resume_rewind_seconds(fixed: f64, smart: bool, audiobook: bool, away: Option<u64>) -> f64 {
    let fixed = finite_clamp_f64(fixed, 0.0, 120.0, 0.0);
    match away {
        Some(away) if smart && audiobook => fixed.max(smart_rewind_seconds(away)),
        _ => fixed,
    }
}

/// Rewind when playback continues after a pause inside one session. Only a
/// pause long enough to lose the thread of an audiobook rewinds (the larger of
/// the fixed value and the table value); anything shorter continues in place.
pub fn pause_rewind_seconds(fixed: f64, smart: bool, audiobook: bool, paused: u64) -> f64 {
    if smart && audiobook && smart_rewind_seconds(paused) > 0. {
        resume_rewind_seconds(fixed, true, true, Some(paused))
    } else {
        0.
    }
}

/// The Voice equalizer curve (same bands as the built-in "Voice" preset) that
/// voice boost adds, and the preamp it adds with it.
pub const VOICE_BOOST_GAINS: [f32; 10] = crate::eq::VOICE_GAINS;
pub const VOICE_BOOST_PREAMP_DB: f32 = 4.0;

/// Equalizer switch, preamp and band gains the engine uses when voice boost is
/// `boost`: the saved values with the Voice curve and the extra preamp mixed in
/// (the curve alone when the user's equalizer is off). The preamp stays inside
/// the range the Options slider offers and every value stays finite; the output
/// limiter is switched on because the boost can reach past full scale.
pub fn voice_boost_mix(
    boost: bool,
    enabled: bool,
    preamp_db: f32,
    gains: [f32; 10],
) -> (bool, f32, [f32; 10]) {
    if !boost {
        return (enabled, preamp_db, gains);
    }
    let mut mixed = VOICE_BOOST_GAINS;
    for (band, user) in mixed.iter_mut().zip(gains) {
        let user = if enabled && user.is_finite() {
            user
        } else {
            0.0
        };
        *band = (*band + user).clamp(-12.0, 12.0);
    }
    let preamp = finite_clamp(preamp_db, -24.0, 12.0, 0.0) + VOICE_BOOST_PREAMP_DB;
    (true, preamp.clamp(-24.0, 12.0), mixed)
}

/// The settings as the audio engine should see them. Voice boost is an overlay
/// applied here only: the user's equalizer preset, bands and preamp in
/// `settings` are never changed. Borrowed (no copy) while the boost is off.
pub fn engine_settings(settings: &Settings) -> Cow<'_, Settings> {
    if !settings.voice_boost {
        return Cow::Borrowed(settings);
    }
    let (enabled, preamp_db, gains) = voice_boost_mix(
        true,
        settings.equalizer_enabled,
        settings.preamp_db,
        settings.eq_gains,
    );
    let mut effective = settings.clone();
    effective.equalizer_enabled = enabled;
    effective.preamp_db = preamp_db;
    effective.eq_gains = gains;
    effective.limiter = true;
    Cow::Owned(effective)
}

fn default_shortcuts() -> Vec<Shortcut> {
    DEFAULT_SHORTCUTS
        .into_iter()
        .map(|(action, key)| Shortcut {
            action: action.into(),
            key: key.into(),
        })
        .collect()
}

/// Set the immediate-mode UI appearance from settings. In System mode the
/// native theme supplied by eframe is used, with light as the unknown fallback.
pub fn apply_appearance(ctx: &Context, settings: &Settings) {
    crate::appearance::apply(ctx, settings);
}

/// Parse the focused-window shortcut syntax used by validation and the app
/// handler: one key, optionally preceded by physical Ctrl, Shift, and/or Alt.
pub fn parse_shortcut(input: &str) -> Result<(egui::Modifiers, egui::Key), String> {
    let input = input.trim();
    let plus;
    let input = if input == "+" {
        "Plus"
    } else if let Some(prefix) = input.strip_suffix("++") {
        plus = format!("{prefix}+Plus");
        &plus
    } else { input };
    let parts: Vec<_> = input.split('+').map(str::trim).collect();
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return Err("Use a key name such as Space or Ctrl+O.".into());
    }

    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut key = None;
    for part in parts {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => {
                if key.is_some() {
                    return Err("Modifiers must come before the key.".into());
                }
                if ctrl {
                    return Err("Each modifier may appear only once.".into());
                }
                ctrl = true;
            }
            "shift" => {
                if key.is_some() {
                    return Err("Modifiers must come before the key.".into());
                }
                if shift {
                    return Err("Each modifier may appear only once.".into());
                }
                shift = true;
            }
            "alt" => {
                if key.is_some() {
                    return Err("Modifiers must come before the key.".into());
                }
                if alt {
                    return Err("Each modifier may appear only once.".into());
                }
                alt = true;
            }
            "meta" | "cmd" | "command" | "super" | "win" | "windows" => {
                return Err("Supported modifiers are Ctrl, Shift, and Alt.".into());
            }
            _ => {
                if key.is_some() {
                    return Err("Use exactly one key after the modifiers.".into());
                }
                key = Some(
                    parse_key_name(part)
                        .ok_or_else(|| format!("{} is not a recognized key name.", part))?,
                );
            }
        }
    }

    let key = key.ok_or_else(|| "Add a key after the modifiers.".to_owned())?;
    if matches!(
        key,
        egui::Key::ShiftLeft
            | egui::Key::ShiftRight
            | egui::Key::ControlLeft
            | egui::Key::ControlRight
            | egui::Key::AltLeft
            | egui::Key::AltRight
            | egui::Key::SuperLeft
            | egui::Key::SuperRight
    ) {
        return Err("Choose a non-modifier key.".into());
    }
    Ok((
        egui::Modifiers {
            alt,
            ctrl,
            shift,
            mac_cmd: false,
            command: false,
        },
        key,
    ))
}

fn parse_key_name(name: &str) -> Option<egui::Key> {
    match name.to_ascii_lowercase().as_str() {
        "arrowdown" => return Some(egui::Key::ArrowDown),
        "arrowleft" => return Some(egui::Key::ArrowLeft),
        "arrowright" => return Some(egui::Key::ArrowRight),
        "arrowup" => return Some(egui::Key::ArrowUp),
        _ => {}
    }
    egui::Key::ALL
        .iter()
        .copied()
        .find(|key| {
            key.name().eq_ignore_ascii_case(name) || key.symbol_or_name().eq_ignore_ascii_case(name)
        })
        .or_else(|| egui::Key::from_name(name))
}

/// Return shortcuts whose normalized key is assigned more than once.
pub fn shortcut_conflicts(shortcuts: &[Shortcut]) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    shortcuts
        .iter()
        .filter_map(|shortcut| {
            let key = canonical_shortcut(&shortcut.key)?;
            if seen.insert(key.clone()) {
                None
            } else {
                Some((shortcut.action.clone(), key))
            }
        })
        .collect()
}

/// Return shortcut indices with the most explicit modifiers first.
///
/// egui's logical key matching allows extra Shift/Alt modifiers for keyboard
/// layouts. A more-specific binding must therefore consume an event before a
/// broader binding with the same key. Sorting is stable, so equal-specificity
/// bindings retain their configured order.
pub fn shortcut_dispatch_order(shortcuts: &[Shortcut]) -> Vec<usize> {
    let mut order: Vec<_> = (0..shortcuts.len()).collect();
    let specificity = |index: usize| {
        parse_shortcut(&shortcuts[index].key)
            .map(|(modifiers, _)| {
                modifiers.ctrl as usize + modifiers.shift as usize + modifiers.alt as usize
            })
            .unwrap_or(0)
    };
    order.sort_by(|left, right| specificity(*right).cmp(&specificity(*left)));
    order
}

/// Find distinct shortcuts that can match the same logical key event.
///
/// egui treats Shift and Alt in a binding as required modifiers, while allowing
/// extra Shift/Alt modifiers on the event. Apply remains enabled for these
/// pairs because `shortcut_dispatch_order` gives the more-specific binding
/// priority; this list is an informational warning for the user.
pub fn shortcut_overlaps(shortcuts: &[Shortcut]) -> Vec<(String, String)> {
    let mut overlaps = Vec::new();
    for left in 0..shortcuts.len() {
        let Ok((left_modifiers, left_key)) = parse_shortcut(&shortcuts[left].key) else {
            continue;
        };
        for right in (left + 1)..shortcuts.len() {
            let Ok((right_modifiers, right_key)) = parse_shortcut(&shortcuts[right].key) else {
                continue;
            };
            if left_key == right_key
                && left_modifiers.ctrl == right_modifiers.ctrl
                && canonical_shortcut(&shortcuts[left].key)
                    != canonical_shortcut(&shortcuts[right].key)
            {
                overlaps.push((
                    shortcuts[left].action.clone(),
                    shortcuts[right].action.clone(),
                ));
            }
        }
    }
    overlaps
}

/// Whether `input` is plain Up, Down or Enter, the keys that move through the
/// file list and open the selected file. The defaults never use them; a user
/// binding to one takes priority over the list.
pub fn is_reserved_navigation_key(input: &str) -> bool {
    matches!(
        parse_shortcut(input),
        Ok((modifiers, egui::Key::ArrowUp | egui::Key::ArrowDown | egui::Key::Enter))
            if !modifiers.ctrl && !modifiers.shift && !modifiers.alt
    )
}

/// Find shortcut strings that the focused-window handler cannot parse. An
/// empty key means the action is not bound and is not an error.
pub fn shortcut_validation_errors(shortcuts: &[Shortcut]) -> Vec<(String, String)> {
    shortcuts
        .iter()
        .filter(|shortcut| !shortcut.key.trim().is_empty())
        .filter_map(|shortcut| {
            parse_shortcut(&shortcut.key)
                .err()
                .map(|message| (shortcut.action.clone(), message))
        })
        .collect()
}

fn canonical_shortcut(input: &str) -> Option<String> {
    let (modifiers, key) = parse_shortcut(input).ok()?;
    let mut parts = Vec::with_capacity(4);
    if modifiers.ctrl {
        parts.push("ctrl".to_owned());
    }
    if modifiers.shift {
        parts.push("shift".to_owned());
    }
    if modifiers.alt {
        parts.push("alt".to_owned());
    }
    let key_name = match key {
        egui::Key::ArrowDown => "arrowdown".to_owned(),
        egui::Key::ArrowLeft => "arrowleft".to_owned(),
        egui::Key::ArrowRight => "arrowright".to_owned(),
        egui::Key::ArrowUp => "arrowup".to_owned(),
        _ => key.name().to_ascii_lowercase(),
    };
    parts.push(key_name);
    Some(parts.join("+"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    General,
    Appearance,
    Playback,
    Resume,
    Audio,
    Equalizer,
    Library,
    Artwork,
    Playlists,
    Files,
    Keyboard,
    Notifications,
    Backup,
    Advanced,
}

impl Category {
    const ALL: [Category; 14] = [
        Self::General,
        Self::Appearance,
        Self::Playback,
        Self::Resume,
        Self::Audio,
        Self::Equalizer,
        Self::Library,
        Self::Artwork,
        Self::Playlists,
        Self::Files,
        Self::Keyboard,
        Self::Notifications,
        Self::Backup,
        Self::Advanced,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Playback => "Playback",
            Self::Resume => "Resume & Audiobooks",
            Self::Audio => "Audio output",
            Self::Equalizer => "Equalizer & DSP",
            Self::Library => "Library & folders",
            Self::Artwork => "Tags & artwork",
            Self::Playlists => "Playlists & queue",
            Self::Files => "Files & associations",
            Self::Keyboard => "Keyboard & mouse",
            Self::Notifications => "Notifications & tray",
            Self::Backup => "Backup & privacy",
            Self::Advanced => "Advanced",
        }
    }

    fn icon(self) -> crate::widgets::Icon {
        use crate::widgets::Icon;
        match self {
            Self::General => Icon::Settings,
            Self::Appearance => Icon::Appearance,
            Self::Playback => Icon::Play,
            Self::Resume => Icon::Book,
            Self::Audio => Icon::Volume,
            Self::Equalizer => Icon::Equalizer,
            Self::Library => Icon::Folder,
            Self::Artwork => Icon::Tag,
            Self::Playlists => Icon::Playlist,
            Self::Files => Icon::File,
            Self::Keyboard => Icon::Keyboard,
            Self::Notifications => Icon::Bell,
            Self::Backup => Icon::Backup,
            Self::Advanced => Icon::Settings,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsAction {
    ExportBackup,
    ImportBackup,
    /// Write the portable listening progress file (no paths).
    ExportProgress,
    /// Merge a listening progress file into this profile.
    ImportProgress,
    /// Reconnect the library files of this watched folder after it was moved
    /// (the app asks where it went).
    MoveFolder(std::path::PathBuf),
    OpenEqualizer,
    Rescan,
    OpenProfile,
    ClearHistory,
    ResetProgress,
    /// Forget the files removed from the library so a rescan may add them again.
    ForgetRemovals,
    /// Windows: add BS Player to the "Open with" lists (runs on a worker thread).
    RegisterAssociations,
    /// Windows: remove what `RegisterAssociations` added (worker thread).
    UnregisterAssociations,
    /// Windows: open Settings at "Default apps".
    OpenDefaultApps,
    /// Read the tags of every file in the library again (the app asks first).
    RereadTags,
}

#[derive(Clone)]
struct SearchEntry {
    title: &'static str,
    description: &'static str,
    category: Category,
}

const SEARCH_ENTRIES: &[SearchEntry] = &[
    SearchEntry {
        title: "Theme",
        description: "Light, dark, or system appearance",
        category: Category::Appearance,
    },
    SearchEntry {
        title: "Accent color",
        description: "Selection and highlight color",
        category: Category::Appearance,
    },
    SearchEntry {
        title: "Text size",
        description: "Font size, interface scale, and row density",
        category: Category::Appearance,
    },
    SearchEntry {
        title: "Fonts for other scripts",
        description: "Load a system font for Chinese, Japanese, Korean, Thai, Indic, Hebrew or Arabic titles when the library has them",
        category: Category::Appearance,
    },
    SearchEntry {
        title: "Artwork fit",
        description: "Fit, fill, or original artwork size",
        category: Category::Appearance,
    },
    SearchEntry {
        title: "Show library pane",
        description: "Visible panes, show or hide the library",
        category: Category::Appearance,
    },
    SearchEntry {
        title: "Show artwork pane",
        description: "Visible panes, show or hide artwork",
        category: Category::Appearance,
    },
    SearchEntry {
        title: "Artwork in file list",
        description: "Small cover thumbnails beside each title",
        category: Category::Appearance,
    },
    SearchEntry {
        title: "Show status bar",
        description: "Visible panes, show or hide the status bar",
        category: Category::Appearance,
    },
    SearchEntry {
        title: "Always on top",
        description: "Keep the player window above other windows",
        category: Category::Appearance,
    },
    SearchEntry {
        title: "Visible file columns",
        description: "Choose which columns appear in the file list: album, year, genre, rating, plays, last played, date added, format",
        category: Category::Appearance,
    },
    SearchEntry {
        title: "Interface scale",
        description: "Scale the application interface",
        category: Category::Appearance,
    },
    SearchEntry {
        title: "Row density",
        description: "Change file list row height and spacing",
        category: Category::Appearance,
    },
    SearchEntry {
        title: "Restore session",
        description: "Restore the previous session, remember position, queue, and playback state",
        category: Category::Resume,
    },
    SearchEntry {
        title: "Startup behavior",
        description: "Paused, resume previous state, or play",
        category: Category::Resume,
    },
    SearchEntry {
        title: "Resume rewind",
        description: "Seconds to rewind when resuming a file; the minimum when smart rewind is on",
        category: Category::Resume,
    },
    SearchEntry {
        title: "Smart rewind for audiobooks",
        description: "Rewind further after a longer pause or time away, from 2 seconds up to 45 seconds",
        category: Category::Resume,
    },
    SearchEntry {
        title: "Audiobook speed",
        description: "Playback speed and per-book speed memory",
        category: Category::Resume,
    },
    SearchEntry {
        title: "Remember file positions",
        description: "Save listening position for every file",
        category: Category::Resume,
    },
    SearchEntry {
        title: "Save checkpoint",
        description: "How often playback position is checkpointed",
        category: Category::Resume,
    },
    SearchEntry {
        title: "Sleep timer fade",
        description: "Fade audio at the end of a sleep timer",
        category: Category::Resume,
    },
    SearchEntry {
        title: "Skip intervals",
        description: "Backward and forward seek durations",
        category: Category::Playback,
    },
    SearchEntry {
        title: "Audiobook skip intervals",
        description: "Set backward and forward seek durations for audiobooks",
        category: Category::Resume,
    },
    SearchEntry {
        title: "Music speed",
        description: "Default music playback speed",
        category: Category::Playback,
    },
    SearchEntry {
        title: "Stop at book end",
        description: "Stop after the final audiobook file",
        category: Category::Playback,
    },
    SearchEntry {
        title: "Continue to next queue item",
        description: "Automatically advance playback to the next item",
        category: Category::Playback,
    },
    SearchEntry {
        title: "Gapless playback",
        description: "Join consecutive tracks with no gap or pause between songs or audiobook files",
        category: Category::Playback,
    },
    SearchEntry {
        title: "Skip long silences",
        description: "Shorten silences longer than 0.6 seconds in audiobooks and podcasts, skip silence",
        category: Category::Resume,
    },
    SearchEntry {
        title: "Output device",
        description: "Choose an available audio output",
        category: Category::Audio,
    },
    SearchEntry {
        title: "Mono and balance",
        description: "Channel mode and left-right balance",
        category: Category::Audio,
    },
    SearchEntry {
        title: "Mix output to mono",
        description: "Combine left and right output channels",
        category: Category::Audio,
    },
    SearchEntry {
        title: "Balance",
        description: "Adjust left and right output balance",
        category: Category::Audio,
    },
    SearchEntry {
        title: "Equalizer",
        description: "Ten-band equalizer, preamp, ReplayGain, fallback gain, and clipping prevention",
        category: Category::Equalizer,
    },
    SearchEntry {
        title: "Enable equalizer",
        description: "Turn the live ten-band equalizer on or off",
        category: Category::Equalizer,
    },
    SearchEntry {
        title: "Equalizer preamp",
        description: "Adjust equalizer input gain",
        category: Category::Equalizer,
    },
    SearchEntry {
        title: "Equalizer bands",
        description: "Adjust ten frequency band gains in dB",
        category: Category::Equalizer,
    },
    SearchEntry {
        title: "Equalizer presets",
        description: "Choose built-in presets or save, update, and delete custom presets",
        category: Category::Equalizer,
    },
    SearchEntry {
        title: "Output limiter",
        description: "Enable the output limiter and peak protection",
        category: Category::Equalizer,
    },
    SearchEntry {
        title: "ReplayGain mode",
        description: "Off, track gain, or album gain",
        category: Category::Equalizer,
    },
    SearchEntry {
        title: "ReplayGain fallback gain",
        description: "Fallback gain when audio has no ReplayGain tag",
        category: Category::Equalizer,
    },
    SearchEntry {
        title: "ReplayGain clipping prevention",
        description: "Prevent clipping from ReplayGain adjustment",
        category: Category::Equalizer,
    },
    SearchEntry {
        title: "Preserve pitch",
        description: "Change playback speed while keeping the original pitch",
        category: Category::Playback,
    },
    SearchEntry {
        title: "Watched folders",
        description: "Recursive scanning and excluded file extensions",
        category: Category::Library,
    },
    SearchEntry {
        title: "Include subfolders",
        description: "Recursively scan watched folders",
        category: Category::Library,
    },
    SearchEntry {
        title: "Excluded extensions",
        description: "Skip files with selected extensions while scanning",
        category: Category::Library,
    },
    SearchEntry {
        title: "Scan schedule",
        description: "Scan at startup and repeat scan every selected interval",
        category: Category::Library,
    },
    SearchEntry {
        title: "Rescan library now",
        description: "Start a manual library scan",
        category: Category::Library,
    },
    SearchEntry {
        title: "Forgotten removals",
        description: "Files removed from the library stay removed on rescans; forget them to let a rescan add them back",
        category: Category::Library,
    },
    SearchEntry {
        title: "Skip unavailable items",
        description: "Continue the queue past missing files",
        category: Category::Library,
    },
    SearchEntry {
        title: "Group books by author",
        description: "File the books of the sidebar list under author headings",
        category: Category::Library,
    },
    SearchEntry {
        title: "Move folder",
        description: "Reconnect the library files of a watched folder that was moved to another drive or place",
        category: Category::Library,
    },
    SearchEntry {
        title: "Re-read tags of all files",
        description: "Read album artist, composer, disc and narrator again for the whole library",
        category: Category::Library,
    },
    SearchEntry {
        title: "Embedded artwork",
        description: "Prefer cover art stored in audio files",
        category: Category::Artwork,
    },
    SearchEntry {
        title: "Remove played",
        description: "Remove completed items from the queue",
        category: Category::Playlists,
    },
    SearchEntry {
        title: "Playlist paths",
        description: "Use paths relative to playlist files",
        category: Category::Playlists,
    },
    SearchEntry {
        title: "Double-click",
        description: "Choose whether double-click starts playback",
        category: Category::Keyboard,
    },
    SearchEntry {
        title: "Shortcuts",
        description: "Edit keys for playback, volume, speed, bookmarks, search and remove; detect duplicate assignments",
        category: Category::Keyboard,
    },
    SearchEntry {
        title: "Follow current track",
        description: "Keep the playing item visible in the file list",
        category: Category::Keyboard,
    },
    SearchEntry {
        title: "Private listening (current file stays private)",
        description: "Do not add playback to listening history",
        category: Category::Backup,
    },
    SearchEntry {
        title: "Confirm removal",
        description: "Confirm removal of files from the library",
        category: Category::General,
    },
    SearchEntry {
        title: "Append externally opened files",
        description: "Append files opened outside the player to the queue",
        category: Category::General,
    },
    SearchEntry {
        title: "Backup",
        description: "Import and export a local settings backup",
        category: Category::Backup,
    },
    SearchEntry {
        title: "Import backup",
        description: "Import settings and playback data from a local backup file",
        category: Category::Backup,
    },
    SearchEntry {
        title: "Export backup",
        description: "Export settings and playback data to a local backup file",
        category: Category::Backup,
    },
    SearchEntry {
        title: "Export listening progress",
        description: "Write positions, ratings, favourites and bookmarks to a small file without paths, for a synced folder",
        category: Category::Backup,
    },
    SearchEntry {
        title: "Import listening progress",
        description: "Merge a listening progress file from another computer into this profile",
        category: Category::Backup,
    },
    SearchEntry {
        title: "Clear listening history",
        description: "Clear listening history after confirmation",
        category: Category::Backup,
    },
    SearchEntry {
        title: "Reset playback progress",
        description: "Reset saved playback positions after confirmation",
        category: Category::Backup,
    },
    SearchEntry {
        title: "Open data folder",
        description: "Open the folder containing settings and listening progress",
        category: Category::Advanced,
    },
    SearchEntry {
        title: "Unavailable features",
        description: "Track notifications, taskbar progress, and sleep/wake integration",
        category: Category::Notifications,
    },
    SearchEntry {
        title: "Global media keys",
        description: "Play/pause, next, previous and stop keys work while another app has focus",
        category: Category::Notifications,
    },
    SearchEntry {
        title: "Desktop menu entry",
        description: "Linux AppImage: add BS Player to the application menu with its icon",
        category: Category::Files,
    },
    SearchEntry {
        title: "File associations",
        description: "Add or remove BS Player in the Windows Open with list and open Default apps",
        category: Category::Files,
    },
    SearchEntry {
        title: "System tray",
        description: "Enable tray popup and choose minimize or close behavior",
        category: Category::Notifications,
    },
    SearchEntry {
        title: "Close to tray",
        description: "Keep the player running when the window closes",
        category: Category::Notifications,
    },
    SearchEntry {
        title: "Minimize to tray",
        description: "Hide the player window in the system tray",
        category: Category::Notifications,
    },
    SearchEntry {
        title: "Start minimised to tray",
        description: "Open hidden in the system tray instead of showing the window",
        category: Category::Notifications,
    },
];

/// Settings the main window changes directly (View menu, artwork fit, column
/// menu) while Options is open. The last values seen let `sync_equalizer` tell a
/// live change from an unchanged value, so Apply cannot revert it.
struct LiveLayout {
    show_library: bool,
    show_artwork: bool,
    show_list_art: bool,
    show_status: bool,
    artwork_fit: String,
    visible_columns: [bool; LEGACY_COUNT],
    extra_columns: Vec<String>,
}

impl LiveLayout {
    fn of(settings: &Settings) -> Self {
        Self {
            show_library: settings.show_library,
            show_artwork: settings.show_artwork,
            show_list_art: settings.show_list_art,
            show_status: settings.show_status,
            artwork_fit: settings.artwork_fit.clone(),
            visible_columns: settings.visible_columns,
            extra_columns: settings.extra_columns.clone(),
        }
    }

    fn differs_from(&self, live: &Settings) -> bool {
        self.show_library != live.show_library
            || self.show_artwork != live.show_artwork
            || self.show_list_art != live.show_list_art
            || self.show_status != live.show_status
            || self.visible_columns != live.visible_columns
            || self.extra_columns != live.extra_columns
            || self.artwork_fit != live.artwork_fit
    }
}

/// State for the Options window. Parent code should call `preview()` while open
/// to preview theme changes, and restore its current settings after cancellation.
pub struct OptionsDialog {
    pub open: bool,
    draft: Settings,
    applied: Settings,
    category: Category,
    search: String,
    pub actions: Vec<SettingsAction>,
    last_apply: bool,
    tray_available: bool,
    tray_unavailable_reason: String,
    /// Windows file-association state shown on Files & associations.
    association: AssociationState,
    association_busy: bool,
    association_error: Option<String>,
    /// Global media keys: whether the player holds the wish for them now, and
    /// which of the four it actually got (play/pause, next, previous, stop).
    media_keys_active: bool,
    media_keys_held: [bool; 4],
    device_pending: bool,
    device_error: Option<String>,
    live_layout: LiveLayout,
    live_skip_silence: bool,
    /// How many removed files a rescan skips (kept in the session, not the settings).
    removed_count: usize,
    /// The watched folders, for the list in Library & folders (session data).
    roots: Vec<std::path::PathBuf>,
    /// A scan or save is running: a folder cannot be moved now.
    folder_move_blocked: bool,
}

impl OptionsDialog {
    pub fn new(settings: &Settings) -> Self {
        let mut applied = settings.clone();
        applied.normalize();
        Self {
            open: true,
            draft: applied.clone(),
            applied,
            category: Category::General,
            search: String::new(),
            actions: Vec::new(),
            last_apply: false,
            tray_available: false,
            tray_unavailable_reason: "No system tray is available in this session.".into(),
            association: AssociationState::NotRegistered,
            association_busy: false,
            association_error: None,
            media_keys_active: false,
            media_keys_held: [false; 4],
            device_pending: false,
            device_error: None,
            live_layout: LiveLayout::of(settings),
            live_skip_silence: settings.skip_silence,
            removed_count: 0,
            roots: Vec::new(),
            folder_move_blocked: false,
        }
    }

    pub fn preview(&self) -> &Settings {
        &self.draft
    }

    /// Merge live equalizer edits, and pane/column/artwork-fit changes made in
    /// the main window since the last call, into both the draft and its cancel
    /// baseline. The Options draft owns every other setting and remains untouched.
    pub fn sync_equalizer(&mut self, live: &Settings) {
        let layout_changed = self.live_layout.differs_from(live);
        let silence_changed = self.live_skip_silence != live.skip_silence;
        for target in [&mut self.draft, &mut self.applied] {
            target.equalizer_enabled = live.equalizer_enabled;
            target.preamp_db = live.preamp_db;
            target.eq_gains = live.eq_gains;
            // Switched from the Playback and View menus while Options is open.
            target.voice_boost = live.voice_boost;
            target.show_remaining_time = live.show_remaining_time;
            if silence_changed { target.skip_silence = live.skip_silence; }
            if target.custom_eq_presets != live.custom_eq_presets {
                target.custom_eq_presets.clone_from(&live.custom_eq_presets);
            }
            if layout_changed {
                target.show_library = live.show_library;
                target.show_artwork = live.show_artwork;
                target.show_list_art = live.show_list_art;
                target.show_status = live.show_status;
                target.visible_columns = live.visible_columns;
                target.extra_columns.clone_from(&live.extra_columns);
                target.artwork_fit.clone_from(&live.artwork_fit);
            }
        }
        if layout_changed {
            self.live_layout = LiveLayout::of(live);
        }
        self.live_skip_silence = live.skip_silence;
    }

    /// Supply the runtime tray status so unavailable OS behavior stays disabled
    /// with an honest explanation in Notifications & tray.
    pub fn set_tray_status(&mut self, available: bool, reason: &str) {
        self.tray_available = available;
        self.tray_unavailable_reason = if available {
            String::new()
        } else if reason.trim().is_empty() {
            "No system tray is available in this session.".into()
        } else {
            reason.trim().to_owned()
        };
    }

    /// Supply the Windows file-association state (read by the app when Options
    /// opens and after each Register/Unregister), whether such an action is still
    /// running, and the error the last one returned.
    pub fn set_association_status(
        &mut self,
        state: AssociationState,
        busy: bool,
        error: Option<&str>,
    ) {
        self.association = state;
        self.association_busy = busy;
        let error = error.map(str::trim).filter(|message| !message.is_empty());
        if self.association_error.as_deref() != error {
            self.association_error = error.map(str::to_owned);
        }
    }

    /// Supply the global media key status: `active` is true while the player has
    /// asked for the keys (setting on, tray running, a track loaded); `held` says
    /// which keys it got, in the order play/pause, next, previous, stop.
    pub fn set_media_keys_status(&mut self, active: bool, held: [bool; 4]) {
        self.media_keys_active = active;
        self.media_keys_held = held;
    }

    /// Supply how many removed files are remembered (shown in Library & folders).
    pub fn set_removed_count(&mut self, count: usize) {
        self.removed_count = count;
    }

    /// Supply the watched folders (listed in Library & folders) and whether a
    /// job that forbids moving a folder runs now.
    pub fn set_watched_folders(&mut self, roots: &[(std::path::PathBuf, bool)], blocked: bool) {
        if self.roots.len() != roots.len()
            || self
                .roots
                .iter()
                .zip(roots)
                .any(|(shown, (path, _))| shown != path)
        {
            self.roots = roots.iter().map(|(path, _)| path.clone()).collect();
        }
        self.folder_move_blocked = blocked;
    }

    /// Supply the asynchronous output-device discovery status.
    pub fn set_device_status(&mut self, pending: bool, error: Option<&str>) {
        self.device_pending = pending;
        let error = error.map(str::trim).filter(|message| !message.is_empty());
        if self.device_error.as_deref() != error {
            self.device_error = error.map(str::to_owned);
        }
    }

    /// Restore the last applied settings and close the options viewport.
    pub fn cancel(&mut self) {
        self.open = false;
        self.draft = self.applied.clone();
    }

    /// Draw the bounded options content in the caller's viewport. Apply returns a
    /// snapshot and keeps the dialog open; OK returns a snapshot and closes it.
    /// Cancel and native viewport close discard edits since the last Apply.
    pub fn show_body(
        &mut self,
        ui: &mut Ui,
        _current: &Settings,
        devices: &[String],
    ) -> Option<Settings> {
        if !self.open {
            return None;
        }
        if ui.input(|input| input.viewport().close_requested()) {
            self.cancel();
            return None;
        }
        self.last_apply = false;
        let mut result = None;
        let mut cancel = false;
        let conflicts = shortcut_conflicts(&self.draft.shortcuts);
        let shortcut_errors = shortcut_validation_errors(&self.draft.shortcuts);
        let shortcuts_valid = conflicts.is_empty() && shortcut_errors.is_empty();
        let mut close_after = false;
        let system_theme = ui.ctx().system_theme();
        // Keep the footer anchored to the original viewport allocation;
        // a wide control in the scrollable page must never push it away.
        let content_rect = ui.available_rect_before_wrap();
        let available = content_rect.size();
        let button_font_height = egui::TextStyle::Button.resolve(ui.style()).size;
        let button_height = (button_font_height + ui.spacing().button_padding.y * 2.0 + 2.0)
            .max(ui.spacing().interact_size.y)
            .ceil();
        let footer_height = button_height + 16.0;
        let body_height = (available.y - footer_height - 8.0).max(120.0);
        let body_width = available.x;
        ui.allocate_ui_with_layout(
            Vec2::new(body_width, body_height),
            egui::Layout::left_to_right(egui::Align::Min),
            |body| {
                let nav_width = 205.0_f32
                    .clamp(170.0, 230.0)
                    .min(body.available_width() * 0.36);
                body.allocate_ui_with_layout(
                    Vec2::new(nav_width, body_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |nav| {
                        let (chrome_top, chrome_bottom) = crate::appearance::chrome_colors(nav);
                        nav.painter().add(crate::appearance::gradient(
                            nav.max_rect(),
                            chrome_top,
                            chrome_bottom,
                        ));
                        nav.add(
                            egui::TextEdit::singleline(&mut self.search)
                                .hint_text("Search settings…")
                                .desired_width(nav.available_width()),
                        );
                        nav.add_space(6.0);
                        let reset_height = nav.spacing().interact_size.y + 8.0;
                        let list_height = (nav.available_height() - reset_height).max(0.0);
                        egui::ScrollArea::vertical()
                            .id_salt("options_categories")
                            .max_height(list_height)
                            .auto_shrink([false, false])
                            .show(nav, |nav| {
                                nav.spacing_mut().item_spacing.y = 2.0;
                                if self.search.trim().is_empty() {
                                    for category in Category::ALL {
                                        let selected = self.category == category;
                                        let response = crate::appearance::navigation_indented(
                                            nav,
                                            category.title(),
                                            selected,
                                            34.0,
                                        );
                                        let icon_color = if selected {
                                            crate::appearance::navigation_selected_foreground(nav)
                                        } else {
                                            crate::appearance::row_text(nav, false)
                                        };
                                        let icon_rect = egui::Rect::from_min_size(
                                            egui::pos2(
                                                response.rect.left() + 8.0,
                                                response.rect.center().y - 9.0,
                                            ),
                                            Vec2::splat(18.0),
                                        );
                                        crate::widgets::icon(
                                            nav.painter(),
                                            icon_rect,
                                            category.icon(),
                                            icon_color,
                                        );
                                        if response.clicked() {
                                            self.category = category;
                                        }
                                    }
                                } else {
                                    self.search_results(nav);
                                }
                            });
                        nav.add_space(4.0);
                        let reset = nav.add_enabled(
                            self.category != Category::Advanced,
                            egui::Button::new("Restore defaults"),
                        );
                        if self.category == Category::Advanced {
                            reset.on_hover_text("This page has no resettable settings.");
                        } else if reset.clicked() {
                            self.reset_category();
                        }
                    },
                );
                body.separator();
                let page_width = body.available_width();
                body.allocate_ui_with_layout(
                    Vec2::new(page_width, body_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |page| {
                        page.painter().rect_filled(
                            page.max_rect(),
                            0.0,
                            crate::appearance::workspace(page),
                        );
                        page.heading(self.category.title());
                        page.add_space(6.0);
                        let scroll_height = page.available_height();
                        egui::ScrollArea::both()
                            .id_salt("options_page")
                            .max_width(page_width)
                            .max_height(scroll_height)
                            .auto_shrink([false, false])
                            .show(page, |page| {
                                page.set_min_width(page_width);
                                page.set_max_width(page_width);
                                self.show_page(page, devices, system_theme);
                            });
                    },
                );
            },
        );
        let footer_rect = egui::Rect::from_min_size(
            egui::pos2(content_rect.left(), content_rect.bottom() - footer_height),
            Vec2::new(body_width, footer_height),
        );
        let (chrome_top, chrome_bottom) = crate::appearance::chrome_colors(ui);
        ui.painter().add(crate::appearance::gradient(
            footer_rect,
            chrome_top,
            chrome_bottom,
        ));
        ui.painter().line_segment(
            [
                egui::pos2(footer_rect.left(), footer_rect.top() + 1.0),
                egui::pos2(footer_rect.right(), footer_rect.top() + 1.0),
            ],
            egui::Stroke::new(1.0, crate::appearance::separator(ui)),
        );
        let button_row = egui::Rect::from_min_max(
            egui::pos2(footer_rect.left(), footer_rect.top() + 6.0),
            egui::pos2(footer_rect.right(), footer_rect.bottom() - 5.0),
        );
        let accent_fill = Color32::from_rgb(
            self.draft.accent[0],
            self.draft.accent[1],
            self.draft.accent[2],
        );
        let accent_text = crate::appearance::accent_foreground(accent_fill);
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(button_row)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
            |footer| {
                footer.set_clip_rect(button_row.intersect(footer.clip_rect()));
                if footer
                    .add_enabled(
                        shortcuts_valid,
                        egui::Button::new(
                            RichText::new("OK")
                                .font(crate::appearance::semibold(button_font_height))
                                .color(accent_text),
                        )
                        .fill(accent_fill)
                        .stroke(egui::Stroke::new(1.0, accent_fill))
                        .corner_radius(egui::CornerRadius::same(4))
                        .min_size(Vec2::new(70.0, button_height)),
                    )
                    .clicked()
                {
                    result = Some(self.draft.clone());
                    close_after = true;
                }
                if footer
                    .add_enabled(
                        shortcuts_valid,
                        egui::Button::new("Apply").min_size(Vec2::new(76.0, button_height)),
                    )
                    .clicked()
                {
                    result = Some(self.draft.clone());
                }
                if footer
                    .add(egui::Button::new("Cancel").min_size(Vec2::new(76.0, button_height)))
                    .clicked()
                {
                    cancel = true;
                    close_after = true;
                }
                let status = if !shortcuts_valid {
                    "Fix shortcut errors"
                } else if self.draft != self.applied {
                    "Changes are pending"
                } else {
                    ""
                };
                footer
                    .add(egui::Label::new(status).truncate())
                    .on_hover_text(status);
            },
        );
        if let Some(candidate) = result.as_ref() {
            let has_conflicts = !shortcut_conflicts(&candidate.shortcuts).is_empty();
            let has_invalid_keys = !shortcut_validation_errors(&candidate.shortcuts).is_empty();
            if has_conflicts || has_invalid_keys {
                result = None;
                close_after = false;
            } else {
                self.applied = candidate.clone();
                self.last_apply = true;
            }
        }
        if cancel {
            result = None;
        }
        self.open = !close_after;
        if cancel {
            self.draft = self.applied.clone();
        }
        if !self.open && !self.last_apply {
            self.draft = self.applied.clone();
        }
        result
    }

    fn search_results(&mut self, ui: &mut Ui) {
        let hits = matching_search_entries(&self.search);
        if hits.is_empty() {
            ui.weak("No matching settings");
            return;
        }
        for entry in hits {
            if ui
                .selectable_label(false, RichText::new(entry.title).strong())
                .clicked()
            {
                self.category = entry.category;
            }
            ui.label(RichText::new(entry.description).small().weak());
            ui.add_space(5.0);
        }
    }

    fn reset_category(&mut self) {
        let defaults = Settings::default();
        match self.category {
            Category::General => {
                self.draft.restore_session = defaults.restore_session;
                self.draft.startup_behavior = defaults.startup_behavior;
                self.draft.external_files_append = defaults.external_files_append;
                self.draft.double_click_plays = defaults.double_click_plays;
                self.draft.confirm_remove = defaults.confirm_remove;
            }
            Category::Appearance => {
                self.draft.theme = defaults.theme;
                self.draft.accent = defaults.accent;
                self.draft.font_size = defaults.font_size;
                self.draft.ui_scale = defaults.ui_scale;
                self.draft.row_height = defaults.row_height;
                self.draft.show_library = defaults.show_library;
                self.draft.show_artwork = defaults.show_artwork;
                self.draft.fallback_fonts = defaults.fallback_fonts;
                self.draft.show_list_art = defaults.show_list_art;
                self.draft.show_status = defaults.show_status;
                self.draft.always_on_top = defaults.always_on_top;
                self.draft.artwork_fit = defaults.artwork_fit;
                self.draft.visible_columns = defaults.visible_columns;
                self.draft.extra_columns = defaults.extra_columns;
            }
            Category::Playback => {
                self.draft.music_speed = defaults.music_speed;
                self.draft.preserve_pitch = defaults.preserve_pitch;
                self.draft.skip_back = defaults.skip_back;
                self.draft.skip_forward = defaults.skip_forward;
                self.draft.auto_advance = defaults.auto_advance;
                self.draft.stop_at_book_end = defaults.stop_at_book_end;
                self.draft.gapless = defaults.gapless;
            }
            Category::Resume => {
                self.draft.remember_positions = defaults.remember_positions;
                self.draft.restore_session = defaults.restore_session;
                self.draft.startup_behavior = defaults.startup_behavior;
                self.draft.resume_rewind = defaults.resume_rewind;
                self.draft.smart_rewind = defaults.smart_rewind;
                self.draft.checkpoint_seconds = defaults.checkpoint_seconds;
                self.draft.audiobook_skip_back = defaults.audiobook_skip_back;
                self.draft.audiobook_skip_forward = defaults.audiobook_skip_forward;
                self.draft.book_speed = defaults.book_speed;
                self.draft.remember_book_speed = defaults.remember_book_speed;
                self.draft.skip_silence = defaults.skip_silence;
                self.draft.stop_at_book_end = defaults.stop_at_book_end;
                self.draft.sleep_fade_seconds = defaults.sleep_fade_seconds;
            }
            Category::Audio => {
                self.draft.output_device = defaults.output_device;
                self.draft.mono = defaults.mono;
                self.draft.balance = defaults.balance;
            }
            Category::Equalizer => {
                self.draft.limiter = defaults.limiter;
                self.draft.replaygain = defaults.replaygain;
                self.draft.replaygain_fallback_db = defaults.replaygain_fallback_db;
                self.draft.replaygain_prevent_clipping = defaults.replaygain_prevent_clipping;
            }
            Category::Library => {
                self.draft.recursive_scan = defaults.recursive_scan;
                self.draft.excluded_extensions = defaults.excluded_extensions;
                self.draft.scan_on_start = defaults.scan_on_start;
                self.draft.scan_interval_minutes = defaults.scan_interval_minutes;
                self.draft.queue_skip_missing = defaults.queue_skip_missing;
                self.draft.group_books_by_author = defaults.group_books_by_author;
            }
            Category::Artwork => self.draft.prefer_embedded_art = defaults.prefer_embedded_art,
            Category::Playlists => {
                self.draft.playlist_relative_paths = defaults.playlist_relative_paths;
                self.draft.external_files_append = defaults.external_files_append;
                self.draft.remove_played = defaults.remove_played;
                self.draft.private_listening = defaults.private_listening;
                self.draft.queue_skip_missing = defaults.queue_skip_missing;
            }
            Category::Keyboard => {
                self.draft.double_click_plays = defaults.double_click_plays;
                self.draft.follow_current = defaults.follow_current;
                self.draft.shortcuts = defaults.shortcuts;
            }
            Category::Files => {
                self.draft.external_files_append = defaults.external_files_append;
                self.draft.playlist_relative_paths = defaults.playlist_relative_paths;
                self.draft.desktop_integration = defaults.desktop_integration;
            }
            Category::Backup => {
                self.draft.private_listening = defaults.private_listening;
            }
            Category::Notifications => {
                self.draft.tray_enabled = defaults.tray_enabled;
                self.draft.minimize_to_tray = defaults.minimize_to_tray;
                self.draft.close_to_tray = defaults.close_to_tray;
                self.draft.start_minimized = defaults.start_minimized;
                self.draft.media_keys = defaults.media_keys;
            }
            Category::Advanced => {}
        }
    }

    fn show_page(&mut self, ui: &mut Ui, devices: &[String], system_theme: Option<egui::Theme>) {
        match self.category {
            Category::General => self.page_general(ui),
            Category::Appearance => self.page_appearance(ui, system_theme),
            Category::Playback => self.page_playback(ui),
            Category::Resume => self.page_resume(ui),
            Category::Audio => self.page_audio(ui, devices),
            Category::Equalizer => self.page_equalizer(ui),
            Category::Library => self.page_library(ui),
            Category::Artwork => self.page_artwork(ui),
            Category::Playlists => self.page_playlists(ui),
            Category::Files => self.page_files(ui),
            Category::Keyboard => self.page_keyboard(ui),
            Category::Notifications => self.page_notifications(ui),
            Category::Backup => self.page_backup(ui),
            Category::Advanced => self.page_advanced(ui),
        }
    }

    fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
        let fill = if ui.visuals().dark_mode {
            Color32::from_rgb(0x25, 0x2b, 0x33)
        } else {
            Color32::WHITE
        };
        let title_size = ui
            .style()
            .text_styles
            .get(&egui::TextStyle::Body)
            .map_or(14.0, |font| font.size)
            + 1.0;
        egui::Frame::group(ui.style())
            .fill(fill)
            .stroke(egui::Stroke::new(1.0, crate::appearance::separator(ui)))
            .corner_radius(egui::CornerRadius::same(4))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.label(RichText::new(title).font(crate::appearance::semibold(title_size)));
                ui.add_space(5.0);
                add(ui);
            });
        ui.add_space(7.0);
    }

    fn page_general(&mut self, ui: &mut Ui) {
        Self::section(ui, "Startup", |ui| {
            crate::appearance::checkbox(
                ui,
                &mut self.draft.restore_session,
                "Restore the previous session",
            );
            labeled(ui, "When playback starts", |ui| {
                choice(
                    ui,
                    "startup_behavior",
                    &mut self.draft.startup_behavior,
                    &[
                        ("paused", "Start paused"),
                        ("previous", "Resume if previously playing"),
                        ("play", "Always start playing"),
                    ],
                );
            });
        });
        Self::section(ui, "Opening files", |ui| {
            crate::appearance::checkbox(
                ui,
                &mut self.draft.external_files_append,
                "Append externally opened files to the queue",
            );
            crate::appearance::checkbox(
                ui,
                &mut self.draft.double_click_plays,
                "Double-clicking a file starts playback",
            );
        });
        Self::section(ui, "Confirmations", |ui| {
            crate::appearance::checkbox(
                ui,
                &mut self.draft.confirm_remove,
                "Confirm before removing files from the library",
            );
        });
        unavailable_section(
            ui,
            "Unavailable on this build",
            "Launch at sign-in is not available. Each profile is protected against concurrent writes; a second launch with the same profile hands its files to the running player.",
        );
    }

    fn page_appearance(&mut self, ui: &mut Ui, system_theme: Option<egui::Theme>) {
        let previous_theme = self.draft.theme.clone();
        Self::section(ui, "Theme", |ui| {
            ui.horizontal_wrapped(|ui| {
                for (theme, title) in [("light", "Light"), ("dark", "Dark"), ("system", "System")] {
                    ui.vertical(|ui| {
                        if crate::appearance::theme_preview(ui, theme, self.draft.theme == theme)
                            .clicked()
                        {
                            self.draft.theme = theme.to_owned();
                        }
                        ui.small(title);
                    });
                }
            });
            ui.horizontal(|ui| {
                let selected = crate::themes::find(&self.draft.theme)
                    .map_or("System", |palette| palette.label);
                egui::ComboBox::from_id_salt("appearance-theme-choice")
                    .selected_text(selected)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for (id, label) in [("light", "Light"), ("dark", "Dark"), ("system", "System")] {
                            ui.selectable_value(&mut self.draft.theme, id.to_owned(), label);
                        }
                        ui.separator();
                        for group in ["Classic", "Bold", "Soft"] {
                            ui.menu_button(group, |ui| {
                                let prefix = format!("{group} · ");
                                let height = (ui.ctx().content_rect().height() - 80.).max(80.);
                                egui::ScrollArea::vertical().id_salt(("theme group", group))
                                    .max_height(height).show(ui, |ui| {
                                for palette in crate::themes::catalogue().iter().filter(|palette| {
                                    palette.id != "light" && palette.id != "dark"
                                        && palette.group.unwrap_or("Classic") == group
                                }) {
                                    let label = palette.label.strip_prefix(&prefix).unwrap_or(palette.label);
                                    ui.selectable_value(
                                        &mut self.draft.theme,
                                        palette.id.to_owned(),
                                        label,
                                    );
                                }
                                });
                            });
                        }
                    });
                if let Some(palette) = crate::themes::find(&self.draft.theme) {
                    ui.small(if palette.dark { "Dark palette" } else { "Light palette" });
                }
            });
            if self.draft.theme == "system" {
                let theme = match system_theme {
                    Some(egui::Theme::Dark) => "Dark",
                    Some(egui::Theme::Light) => "Light",
                    None => "Unknown (light is used until the system reports a theme)",
                };
                ui.small(format!("Following the system theme: {theme}"));
            }
            ui.horizontal(|ui| {
                ui.label("Accent color");
                for color in [
                    [0x26, 0x89, 0xef],
                    [128, 87, 205],
                    [220, 55, 55],
                    [230, 120, 30],
                    [34, 150, 78],
                ] {
                    let selected = self.draft.accent == color;
                    let fill = Color32::from_rgb(color[0], color[1], color[2]);
                    let response = ui.add(egui::Button::new("●").fill(fill).stroke(if selected {
                        egui::Stroke::new(2.0, ui.visuals().text_color())
                    } else {
                        egui::Stroke::NONE
                    }));
                    if response.clicked() {
                        self.draft.accent = color;
                    }
                }
            });
        });
        if self.draft.theme != previous_theme {
            if let Some(palette) = crate::themes::find(&self.draft.theme) {
                if !matches!(palette.id, "light" | "dark") {
                    self.draft.accent = palette.accent;
                }
            }
        }
        Self::section(ui, "Text & spacing", |ui| {
            labeled(ui, "Text size", |ui| {
                crate::appearance::slider(
                    ui,
                    egui::Slider::new(&mut self.draft.font_size, FONT_SIZE_RANGE).suffix(" pt"),
                );
            });
            labeled(ui, "Interface scale", |ui| {
                crate::appearance::slider(
                    ui,
                    egui::Slider::new(&mut self.draft.ui_scale, UI_SCALE_RANGE)
                        .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                );
            });
            labeled(ui, "Row density", |ui| {
                crate::appearance::slider(
                    ui,
                    egui::Slider::new(&mut self.draft.row_height, ROW_HEIGHT_RANGE).suffix(" px"),
                );
            });
            crate::appearance::checkbox(
                ui,
                &mut self.draft.fallback_fonts,
                "Load system fonts for other scripts when needed",
            )
            .on_hover_text(
                "Titles in Chinese, Japanese, Korean, Thai, Hindi, Bengali, Tamil, Hebrew or Arabic need a font the player does not carry. When such a title is in the library, the matching font of this computer is loaded once. Fonts already loaded stay until the player is restarted.",
            );
            ui.small("Uses extra memory only when such titles exist.");
        });
        Self::section(ui, "Layout", |ui| {
            crate::appearance::checkbox(ui, &mut self.draft.show_library, "Show library pane");
            crate::appearance::checkbox(ui, &mut self.draft.show_artwork, "Show artwork pane");
            crate::appearance::checkbox(ui, &mut self.draft.show_list_art, "Artwork in file list");
            crate::appearance::checkbox(ui, &mut self.draft.show_status, "Show status bar");
            crate::appearance::checkbox(
                ui,
                &mut self.draft.always_on_top,
                "Keep the player window above other windows",
            );
            labeled(ui, "Artwork fit", |ui| {
                choice(
                    ui,
                    "artwork_fit",
                    &mut self.draft.artwork_fit,
                    &[
                        ("fit", "Fit, preserve aspect ratio"),
                        ("fill", "Fill, crop to frame"),
                        ("original", "Original size"),
                    ],
                )
            });
            ui.label("Visible file columns");
            ui.horizontal_wrapped(|ui| {
                let mask = self.draft.column_mask();
                for (idx, column) in COLUMNS.iter().enumerate() {
                    let mut visible = mask[idx];
                    let allowed = !visible || mask.iter().filter(|shown| **shown).count() > 1;
                    if ui.add_enabled_ui(allowed, |ui| crate::appearance::checkbox(ui, &mut visible, column.label)).inner
                        .on_disabled_hover_text("Keep at least one file column visible").changed() {
                        self.draft.set_column_visible(idx, visible);
                    }
                }
            });
        });
    }

    fn page_playback(&mut self, ui: &mut Ui) {
        Self::section(ui, "Music defaults", |ui| {
            labeled(ui, "Playback speed", |ui| {
                crate::appearance::slider(
                    ui,
                    egui::Slider::new(&mut self.draft.music_speed, 0.5..=2.0)
                        .custom_formatter(|v, _| format!("{v:.2}×")),
                );
            });
            crate::appearance::checkbox(
                ui,
                &mut self.draft.preserve_pitch,
                "Preserve pitch while changing playback speed",
            );
            ui.small("Pitch preservation can alter transient sound; audio is not bit-perfect.");
            pair_seconds(
                ui,
                "Skip backward / forward",
                &mut self.draft.skip_back,
                &mut self.draft.skip_forward,
            );
            crate::appearance::checkbox(
                ui,
                &mut self.draft.auto_advance,
                "Continue to the next queue item",
            );
            crate::appearance::checkbox(
                ui,
                &mut self.draft.stop_at_book_end,
                "Stop after the final file in a book",
            );
            crate::appearance::checkbox(ui, &mut self.draft.gapless, "Gapless playback");
            ui.small(
                "Joins one file to the next without a gap when both have the same sample rate and channels and play at the same speed; otherwise the next file simply starts.",
            );
        });
        Self::section(ui, "Unavailable on this build", |ui| {
            unavailable(
                ui,
                "Crossfade is not provided by the current playback path.",
            );
            ui.small("Shuffle, repeat modes, and A–B repeat are available from the Playback menu.");
        });
    }

    fn page_resume(&mut self, ui: &mut Ui) {
        Self::section(ui, "Restore your session", |ui| {
            crate::appearance::checkbox(
                ui,
                &mut self.draft.remember_positions,
                "Remember position for every file",
            );
            crate::appearance::checkbox(
                ui,
                &mut self.draft.restore_session,
                "Restore queue, current track, shuffle, repeat, and window layout",
            );
            labeled(ui, "On startup", |ui| {
                choice(
                    ui,
                    "startup_behavior",
                    &mut self.draft.startup_behavior,
                    &[
                        ("paused", "Start paused"),
                        ("previous", "Resume if previously playing"),
                        ("play", "Always play"),
                    ],
                )
            });
        });
        Self::section(ui, "Position saving", |ui| {
            labeled(ui, "Resume rewind", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.draft.resume_rewind)
                        .range(0.0..=120.0)
                        .speed(1.0)
                        .suffix(" sec"),
                );
            });
            crate::appearance::checkbox(
                ui,
                &mut self.draft.smart_rewind,
                "Smart rewind for audiobooks",
            );
            ui.small(
                "Rewinds 2 to 45 seconds after longer pauses or time away; the Resume rewind above is the minimum. Music always uses the fixed value.",
            );
            labeled(ui, "Save checkpoint", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.draft.checkpoint_seconds)
                        .range(1..=300)
                        .suffix(" sec"),
                );
            });
        });
        Self::section(ui, "Audiobook defaults", |ui| {
            labeled(ui, "Playback speed", |ui| {
                crate::appearance::slider(
                    ui,
                    egui::Slider::new(&mut self.draft.book_speed, 0.5..=3.0)
                        .custom_formatter(|v, _| format!("{v:.2}×")),
                );
            });
            crate::appearance::checkbox(
                ui,
                &mut self.draft.remember_book_speed,
                "Remember speed for each book",
            );
            let back = self.draft.audiobook_skip_back.get_or_insert(self.draft.skip_back);
            let forward = self.draft.audiobook_skip_forward.get_or_insert(self.draft.skip_forward);
            pair_seconds(
                ui,
                "Skip backward / forward",
                back,
                forward,
            );
            crate::appearance::checkbox(
                ui,
                &mut self.draft.stop_at_book_end,
                "Stop at the end of a book",
            );
            crate::appearance::checkbox(
                ui,
                &mut self.draft.skip_silence,
                "Skip long silences (audiobooks and podcasts)",
            );
            ui.small(
                "Silences longer than 0.6 s are shortened to a quarter second. Positions and bookmarks are unaffected.",
            );
            labeled(ui, "Sleep timer fade", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.draft.sleep_fade_seconds)
                        .range(0..=600)
                        .suffix(" sec (0 disables)"),
                );
            });
        });
        ui.small("Position checkpoints are saved while playing and on pause or track changes.");
    }

    fn page_audio(&mut self, ui: &mut Ui, devices: &[String]) {
        Self::section(ui, "Output device", |ui| {
            labeled(ui, "Device", |ui| {
                let selected_device = self.draft.output_device.clone();
                let selected_is_listed = devices.iter().any(|device| device == &selected_device);
                let selected_text = if selected_device.is_empty() {
                    "System default".to_owned()
                } else if selected_is_listed {
                    selected_device.clone()
                } else {
                    format!("{selected_device} (currently unavailable)")
                };
                ui.add_enabled_ui(!self.device_pending, |ui| {
                    egui::ComboBox::from_id_salt("output_device")
                        .selected_text(selected_text)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.draft.output_device,
                                String::new(),
                                "System default",
                            );
                            if !selected_device.is_empty() && !selected_is_listed {
                                ui.selectable_value(
                                    &mut self.draft.output_device,
                                    selected_device.clone(),
                                    format!("{selected_device} (currently unavailable)"),
                                );
                                ui.separator();
                            }
                            for device in devices {
                                ui.selectable_value(
                                    &mut self.draft.output_device,
                                    device.clone(),
                                    device,
                                );
                            }
                        });
                });
                if self.device_pending {
                    ui.label(RichText::new("Loading output devices…").weak());
                } else if let Some(error) = &self.device_error {
                    ui.colored_label(
                        crate::appearance::error_color(ui),
                        format!("Output device discovery failed: {error}"),
                    );
                }
            });
        });
        unavailable_section(
            ui,
            "Unavailable on this build",
            "Exclusive output mode, sample-rate policy, buffer tuning, and automatic system-device following are unsupported by the current audio backend.",
        );
        Self::section(ui, "Playback processing", |ui| {
            crate::appearance::checkbox(ui, &mut self.draft.mono, "Mix output to mono");
            labeled(ui, "Balance", |ui| {
                crate::appearance::slider(
                    ui,
                    egui::Slider::new(&mut self.draft.balance, -1.0..=1.0).custom_formatter(
                        |v, _| {
                            if v < 0.0 {
                                format!("L {:.0}%", -v * 100.0)
                            } else if v > 0.0 {
                                format!("R {:.0}%", v * 100.0)
                            } else {
                                "Center".into()
                            }
                        },
                    ),
                );
            });
            ui.small("Volume and mute are controlled by the player transport.");
        });
    }

    fn page_equalizer(&mut self, ui: &mut Ui) {
        Self::section(ui, "Equalizer", |ui| {
            ui.label("Adjust ten frequency bands, preamp, and named sound presets.");
            if crate::widgets::text_icon_button(
                ui,
                crate::widgets::Icon::Equalizer,
                "Open Equalizer",
            )
            .clicked()
            {
                self.actions.push(SettingsAction::OpenEqualizer);
            }
        });
        Self::section(ui, "Gain & protection", |ui| {
            ui.add_enabled_ui(!self.draft.voice_boost, |ui| {
                crate::appearance::checkbox(ui, &mut self.draft.limiter, "Enable output limiter");
            }).response.on_disabled_hover_text("Voice boost always enables the limiter for peak protection");
            if self.draft.voice_boost { ui.small("Voice boost is using the limiter. Turn off voice boost to use this preference."); }
        });
        Self::section(ui, "ReplayGain", |ui| {
            labeled(ui, "Mode", |ui| {
                choice(
                    ui,
                    "replaygain_mode",
                    &mut self.draft.replaygain,
                    &[
                        ("off", "Off"),
                        ("track", "Track gain"),
                        ("album", "Album gain"),
                    ],
                );
            });
            ui.add_enabled_ui(self.draft.replaygain != "off", |ui| {
                labeled(ui, "Fallback gain", |ui| {
                    crate::appearance::slider(
                        ui,
                        egui::Slider::new(&mut self.draft.replaygain_fallback_db, -24.0..=12.0)
                            .suffix(" dB"),
                    );
                });
                crate::appearance::checkbox(
                    ui,
                    &mut self.draft.replaygain_prevent_clipping,
                    "Prevent clipping from ReplayGain",
                );
            });
            ui.small(
                "The output limiter is a separate peak cap. Voice boost always enables it; otherwise this preference controls it.",
            );
        });
        ui.small("Equalizer gains are adjusted live in the dedicated window; changes are saved with this profile.");
    }

    fn page_library(&mut self, ui: &mut Ui) {
        Self::section(ui, "Scanning", |ui| {
            crate::appearance::checkbox(ui, &mut self.draft.recursive_scan, "Include subfolders");
            crate::appearance::checkbox(
                ui,
                &mut self.draft.scan_on_start,
                "Scan library at startup",
            );
            labeled(ui, "Repeat scan every", |ui| {
                ui.add(
                    egui::DragValue::new(&mut self.draft.scan_interval_minutes)
                        .range(0..=10080)
                        .suffix(" min (0 disables)"),
                );
            });
            labeled(ui, "Excluded extensions", |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.draft.excluded_extensions)
                        .char_limit(MAX_EXCLUDED_EXTENSIONS_CHARS)
                        .hint_text("part, tmp, log")
                        .desired_width(260.0),
                );
            });
            ui.small("Separate extensions with commas. Watched-folder selection is managed from the Library view.");
            if ui.button("Rescan library now").clicked() {
                self.actions.push(SettingsAction::Rescan);
            }
        });
        Self::section(ui, "Audiobooks", |ui| {
            crate::appearance::checkbox(
                ui,
                &mut self.draft.group_books_by_author,
                "Group books by author in the sidebar",
            );
            ui.small(
                "Applies when the library has more than eight books; author headings can be collapsed.",
            );
        });
        Self::section(ui, "Watched folders", |ui| {
            if self.roots.is_empty() {
                ui.label("No folder is watched. Add one with File → Add folder….");
            }
            let mut moved = None;
            for root in &self.roots {
                ui.horizontal_wrapped(|ui| {
                    ui.label(crate::media::display_path(root));
                    if ui
                        .add_enabled(!self.folder_move_blocked, egui::Button::new("Move folder…"))
                        .on_hover_text(
                            "Pick the place the folder was moved to: its library files are reconnected there, keeping their saved positions, bookmarks and ratings",
                        )
                        .on_disabled_hover_text("Wait until the running scan ends, or cancel it")
                        .clicked()
                    {
                        moved = Some(root.clone());
                    }
                });
            }
            if let Some(root) = moved {
                self.actions.push(SettingsAction::MoveFolder(root));
            }
            ui.small(
                "After you moved a folder to another drive or place, \"Move folder…\" finds its files there by size and content and points the library at them. Nothing is merged on name or size alone, and no audio file is changed. The profile is copied to before-move-folder.json first.",
            );
        });
        Self::section(ui, "Tags", |ui| {
            if ui.button("Re-read tags of all files…").clicked() {
                self.actions.push(SettingsAction::RereadTags);
            }
            ui.small(
                "Scans skip files that did not change, so a library scanned by an older version has no album artist, composer, disc or narrator. This reads every file again (it can take a while on a large library); listening progress, bookmarks and library tag edits stay.",
            );
        });
        Self::section(ui, "Missing files", |ui| {
            crate::appearance::checkbox(
                ui,
                &mut self.draft.queue_skip_missing,
                "Skip unavailable items when advancing the queue",
            );
            ui.label("Missing files remain in the library so saved progress is retained.");
        });
        Self::section(ui, "Removed files", |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("Forgotten removals: {}", self.removed_count));
                if ui
                    .add_enabled(self.removed_count > 0, egui::Button::new("Forget all"))
                    .clicked()
                {
                    self.actions.push(SettingsAction::ForgetRemovals);
                }
            });
            ui.small(
                "Files you remove from the library stay removed when a scan runs. Adding them again yourself (files, folder, drag and drop) always works.",
            );
        });
        unavailable_section(
            ui,
            "Unavailable on this build",
            "Live folder watching and a duplicate-management view are not available. Manual, startup and interval scans are supported.",
        );
    }

    fn page_artwork(&mut self, ui: &mut Ui) {
        Self::section(ui, "Artwork source", |ui| {
            crate::appearance::checkbox(
                ui,
                &mut self.draft.prefer_embedded_art,
                "Prefer artwork embedded in audio files",
            );
            ui.label("Folder artwork is used when embedded artwork is unavailable.");
        });
        unavailable_section(
            ui,
            "Unavailable on this build",
            "Online artwork downloads are not available.",
        );
        ui.small(
            "File details edits the library's tags only; Edit file tags… there writes tags and artwork into MP3, M4A/M4B, FLAC and Ogg files.",
        );
    }

    fn page_playlists(&mut self, ui: &mut Ui) {
        Self::section(ui, "Queue", |ui| {
            crate::appearance::checkbox(
                ui,
                &mut self.draft.remove_played,
                "Remove completed items from the queue",
            );
            crate::appearance::checkbox(
                ui,
                &mut self.draft.queue_skip_missing,
                "Skip unavailable items while advancing",
            );
            crate::appearance::checkbox(
                ui,
                &mut self.draft.playlist_relative_paths,
                "Use paths relative to playlist files",
            );
            crate::appearance::checkbox(
                ui,
                &mut self.draft.external_files_append,
                "Append externally opened files to the queue",
            );
        });
        Self::section(ui, "Listening history", |ui| {
            crate::appearance::checkbox(
                ui,
                &mut self.draft.private_listening,
                "Private listening (do not record history)",
            );
        });
        unavailable_section(
            ui,
            "Unavailable on this build",
            "Smart playlists are not available. Set track ratings in File details.",
        );
        ui.small("Playlist import/export and restoring the saved queue are supported.");
    }

    fn page_files(&mut self, ui: &mut Ui) {
        Self::section(ui, "File handling", |ui| {
            crate::appearance::checkbox(
                ui,
                &mut self.draft.external_files_append,
                "Append externally opened files to the queue",
            );
            crate::appearance::checkbox(
                ui,
                &mut self.draft.playlist_relative_paths,
                "Use relative paths in saved playlists",
            );
        });
        if desktop_menu_entry_available() {
            Self::section(ui, "Desktop menu (AppImage)", |ui| {
                crate::appearance::checkbox(
                    ui,
                    &mut self.draft.desktop_integration,
                    "Add BS Player to the application menu and use its icon in the dock and file manager",
                );
                ui.weak("Written once per start under ~/.local/share. A launcher entry of your own is never replaced.");
            });
        }
        if windows_integration_available() {
            self.association_section(ui);
            unavailable_section(
                ui,
                "Unavailable on this build",
                "Direct file rename and disk deletion are not available in this build.",
            );
        } else {
            unavailable_section(
                ui,
                "Unavailable on this build",
                "OS file associations, direct file rename, and disk deletion are not available in this build.",
            );
        }
        ui.small("Drag-and-drop, reveal in the file manager, and locating a missing file are available from the player.");
    }

    /// Windows only: register BS Player in the "Open with" lists. Registration
    /// runs on a worker thread; the state shown is read when Options opens and
    /// after each action, never per frame.
    fn association_section(&mut self, ui: &mut Ui) {
        Self::section(ui, "Windows", |ui| {
            ui.label(format!(
                "File associations: {}",
                association_state_text(self.association)
            ));
            if self.association_busy {
                ui.weak("Working…");
            }
            if let Some(error) = &self.association_error {
                ui.colored_label(crate::appearance::error_color(ui), error);
            }
            let (can_register, can_unregister) =
                association_buttons(self.association, self.association_busy);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        can_register,
                        egui::Button::new("Register 'Open with BS Player'"),
                    )
                    .clicked()
                {
                    self.actions.push(SettingsAction::RegisterAssociations);
                }
                if ui
                    .add_enabled(can_unregister, egui::Button::new("Unregister"))
                    .clicked()
                {
                    self.actions.push(SettingsAction::UnregisterAssociations);
                }
                if ui.button("Default apps…").clicked() {
                    self.actions.push(SettingsAction::OpenDefaultApps);
                }
            });
            ui.small("To make BS Player the default for a file type, choose it in Windows Settings → Default apps.");
        });
    }

    fn page_keyboard(&mut self, ui: &mut Ui) {
        Self::section(ui, "Keyboard shortcuts", |ui| {
            ui.label(
                "Enter a key such as Space, Left, [, or Ctrl+O, with optional Ctrl, Shift, or Alt. Leave a field empty to leave that action unbound.",
            );
            for (idx, shortcut) in self.draft.shortcuts.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(&shortcut.action);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut shortcut.key)
                                .desired_width(150.0)
                                .id(egui::Id::new(("shortcut", idx))),
                        );
                    });
                });
            }
            let conflicts = shortcut_conflicts(&self.draft.shortcuts);
            let overlaps = shortcut_overlaps(&self.draft.shortcuts);
            if conflicts.is_empty() {
                if overlaps.is_empty() {
                    ui.small("No overlapping shortcut assignments.");
                } else {
                    ui.label(format!(
                        "Overlapping shortcuts: {} and {} can match the same key event. More-specific bindings run first; ties follow list order.",
                        overlaps[0].0, overlaps[0].1
                    ));
                }
            } else {
                ui.colored_label(
                    crate::appearance::error_color(ui),
                    format!(
                        "Shortcut conflict: {} uses {} more than once.",
                        conflicts[0].0, conflicts[0].1
                    ),
                );
            }
            for (action, message) in shortcut_validation_errors(&self.draft.shortcuts) {
                ui.colored_label(
                    crate::appearance::error_color(ui),
                    format!("{action}: {message}"),
                );
            }
            if let Some(shortcut) = self
                .draft
                .shortcuts
                .iter()
                .find(|shortcut| is_reserved_navigation_key(&shortcut.key))
            {
                ui.small(format!(
                    "{} uses {}, which also moves through the file list. The binding takes priority.",
                    shortcut.action,
                    shortcut.key.trim()
                ));
            }
        });
        Self::section(ui, "Mouse", |ui| {
            crate::appearance::checkbox(
                ui,
                &mut self.draft.double_click_plays,
                "Double-click a file to play it",
            );
            crate::appearance::checkbox(
                ui,
                &mut self.draft.follow_current,
                "Keep the current track visible in the file list",
            );
        });
        unavailable_section(
            ui,
            "Unavailable on this build",
            "Global shortcuts that work in any program are not available; the media keys are under Notifications & tray (Windows).",
        );
    }

    fn page_notifications(&mut self, ui: &mut Ui) {
        Self::section(ui, "System tray", |ui| {
            crate::appearance::checkbox(
                ui,
                &mut self.draft.tray_enabled,
                "Show a player control popup in the system tray",
            );
            ui.add_enabled_ui(self.tray_available && self.draft.tray_enabled, |ui| {
                crate::appearance::checkbox(
                    ui,
                    &mut self.draft.minimize_to_tray,
                    "Hide in tray when the player is minimized",
                );
                crate::appearance::checkbox(
                    ui,
                    &mut self.draft.close_to_tray,
                    "Keep running in tray when the player window closes",
                );
                crate::appearance::checkbox(
                    ui,
                    &mut self.draft.start_minimized,
                    "Start minimised to tray",
                );
            });
            if !self.tray_available {
                ui.weak(&self.tray_unavailable_reason);
            } else if !self.draft.tray_enabled {
                ui.weak("Tray controls are disabled by this setting. Turn it on to use the tray.");
            } else {
                ui.small("Minimize and close behavior are off by default. Use Quit from the tray popup to exit.");
            }
        });
        Self::section(ui, "Media keys", |ui| {
            ui.add_enabled_ui(
                windows_integration_available() && self.tray_available && self.draft.tray_enabled,
                |ui| {
                    crate::appearance::checkbox(
                        ui,
                        &mut self.draft.media_keys,
                        "Global media keys (play/pause, next, previous, stop)",
                    );
                },
            );
            ui.small(
                "Needs the tray icon; other apps cannot use these keys while BS Player holds them.",
            );
            if !windows_integration_available() {
                ui.weak("Global media keys are available on Windows only.");
            } else if !self.tray_available {
                ui.weak(&self.tray_unavailable_reason);
            } else if !self.draft.tray_enabled {
                ui.weak("Turn on the system tray to use the media keys.");
            } else if let Some(note) =
                media_keys_conflict_note(self.media_keys_active, self.media_keys_held)
            {
                ui.label(RichText::new(note).color(ui.visuals().warn_fg_color));
            }
        });
        Self::section(ui, "Unavailable on this build", |ui| {
            unavailable(
                ui,
                "Track notifications, taskbar progress, and OS sleep/wake integration are not supported by the current platform layer.",
            );
            ui.label("System tray controls are available only when a tray service is detected.");
        });
    }

    fn page_backup(&mut self, ui: &mut Ui) {
        Self::section(ui, "Backup", |ui| {
            ui.label("Settings and playback data can be exported to a local backup file.");
            ui.horizontal(|ui| {
                if ui.button("Export backup…").clicked() {
                    self.actions.push(SettingsAction::ExportBackup);
                }
                if ui.button("Import backup…").clicked() {
                    self.actions.push(SettingsAction::ImportBackup);
                }
            });
            ui.small("Import should show a preview before replacing current data.");
        });
        Self::section(ui, "Listening progress", |ui| {
            ui.label(
                "A small file with your positions, play counts, ratings, favourites, bookmarks and book speeds. It holds no file paths, so you can keep it in a synced folder and import it in BS Player on another computer.",
            );
            ui.horizontal(|ui| {
                if ui.button("Export listening progress…").clicked() {
                    self.actions.push(SettingsAction::ExportProgress);
                }
                if ui.button("Import listening progress…").clicked() {
                    self.actions.push(SettingsAction::ImportProgress);
                }
            });
            ui.small(
                "Files are matched by their content. A saved position is replaced only when the file's was played more recently; ratings and favourites only where you have none; bookmarks are added. Edit → Undo takes an import back.",
            );
        });
        Self::section(ui, "Privacy & history", |ui| {
            crate::appearance::checkbox(ui, &mut self.draft.private_listening, "Private listening");
            ui.small("The current file stays private until you open a file for playback, even if this option is switched off.");
            ui.small("History and progress clearing always require confirmation.");
            ui.horizontal(|ui| {
                if ui.button("Clear listening history…").clicked() {
                    self.actions.push(SettingsAction::ClearHistory);
                }
                if ui.button("Reset playback progress…").clicked() {
                    self.actions.push(SettingsAction::ResetProgress);
                }
            });
        });
        unavailable_section(
            ui,
            "Unavailable on this build",
            "Scheduled backups, automatic cleanup, and portable/per-user profile selection are not available yet.",
        );
    }

    fn page_advanced(&mut self, ui: &mut Ui) {
        Self::section(ui, "Application data", |ui| {
            ui.label("Settings and playback progress are stored in the application data folder.");
            if ui.button("Open data folder").clicked() {
                self.actions.push(SettingsAction::OpenProfile);
            }
            ui.small(
                "Open the folder containing this profile's saved settings and listening progress.",
            );
        });
        unavailable_section(
            ui,
            "Unavailable on this build",
            "Cache limits and index rebuilding are not available. Version and CPU information are under Help → About BS Player.",
        );
    }
}

fn matching_search_entries(query: &str) -> Vec<&'static SearchEntry> {
    let query = query.trim().to_lowercase();
    SEARCH_ENTRIES
        .iter()
        .filter(|entry| {
            entry.title.to_lowercase().contains(&query)
                || entry.description.to_lowercase().contains(&query)
        })
        .collect()
}

fn labeled(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        add(ui);
    });
}

fn choice(ui: &mut Ui, id: &'static str, value: &mut String, choices: &[(&str, &str)]) {
    let selected = choices
        .iter()
        .find(|(key, _)| *key == value.as_str())
        .map(|(_, title)| *title)
        .unwrap_or("Choose…");
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for (key, title) in choices {
                ui.selectable_value(value, (*key).to_owned(), *title);
            }
        });
}

fn pair_seconds(ui: &mut Ui, label: &str, back: &mut f64, forward: &mut f64) {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        skip_preset(ui, "skip-back-preset", "Backward [", back);
        ui.label("/");
        skip_preset(ui, "skip-forward-preset", "Forward ]", forward);
    });
}

fn skip_preset(ui: &mut Ui, id: &'static str, label: &str, seconds: &mut f64) {
    const PRESETS: [f64; 4] = [5.0, 10.0, 15.0, 30.0];
    let selected = PRESETS
        .iter()
        .find(|preset| (**preset - *seconds).abs() < f64::EPSILON)
        .map(|preset| format!("{preset:.0} sec"))
        .unwrap_or_else(|| format!("Custom ({seconds:.0} sec)"));
    ui.label(label);
    egui::ComboBox::from_id_salt(id)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for preset in PRESETS {
                ui.selectable_value(seconds, preset, format!("{preset:.0} sec"));
            }
        });
}

/// The Windows-only integration (file associations, media keys) is offered on
/// Windows; other systems hide it.
const fn windows_integration_available() -> bool {
    cfg!(windows)
}

/// The desktop menu entry is offered only to a Linux player running from an AppImage.
fn desktop_menu_entry_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        crate::desktop_entry::running_from_appimage()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// The status line for the file-association state.
pub fn association_state_text(state: AssociationState) -> &'static str {
    match state {
        AssociationState::Registered => "Registered",
        AssociationState::OtherLocation => "Registered for another location",
        AssociationState::NotRegistered => "Not registered",
    }
}

/// Which of "Register" and "Unregister" can be pressed: neither while a request
/// runs; registering again is offered when nothing is registered or when the
/// registration points at another copy of the program.
fn association_buttons(state: AssociationState, busy: bool) -> (bool, bool) {
    if busy {
        return (false, false);
    }
    (
        state != AssociationState::Registered,
        state != AssociationState::NotRegistered,
    )
}

/// Names of the four media keys, in the order `winintegration::media_keys_held`
/// reports them.
const MEDIA_KEY_LABELS: [&str; 4] = ["Play/pause", "Next", "Previous", "Stop"];

/// "Another app is using: Next, Previous" when the player asked for the media
/// keys and some could not be registered; `None` when it holds all of them or
/// does not want them (a key is only held while a track is loaded).
pub fn media_keys_conflict_note(active: bool, held: [bool; 4]) -> Option<String> {
    if !active {
        return None;
    }
    let missing: Vec<&str> = MEDIA_KEY_LABELS
        .iter()
        .zip(held)
        .filter(|(_, held)| !held)
        .map(|(label, _)| *label)
        .collect();
    (!missing.is_empty()).then(|| format!("Another app is using: {}", missing.join(", ")))
}

fn unavailable(ui: &mut Ui, message: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(message).weak());
}

fn unavailable_section(ui: &mut Ui, title: &str, message: &str) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(RichText::new(title).strong());
        ui.add_space(5.0);
        unavailable(ui, message);
    });
    ui.add_space(7.0);
}
