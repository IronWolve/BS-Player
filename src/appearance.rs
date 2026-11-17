//! Shared BS Player desktop appearance, native font loading, and visual tokens.

use crate::{media::Track, settings::Settings};
use eframe::egui;
use egui::{
    Color32, Context, FontDefinitions, FontFamily, FontId, Mesh, Rect, Response, Sense, Shape,
    Stroke, StrokeKind, Ui, Vec2,
};
use std::{fs::File, io::Read, path::PathBuf, sync::Arc};

const FONT_LIMIT: u64 = 20 * 1024 * 1024;
const NORMAL_FONT: &str = "bs-player-proportional";
const SEMIBOLD_FONT: &str = "bs-player-semibold";
const SEMIBOLD_FAMILY: &str = "semibold";

const CHROME_TOP: Color32 = Color32::from_rgb(0xf5, 0xf7, 0xfa);
const CHROME_BOTTOM: Color32 = Color32::from_rgb(0xed, 0xf1, 0xf6);
const SEPARATOR: Color32 = Color32::from_rgb(0xd3, 0xd9, 0xe2);
const INK: Color32 = Color32::from_rgb(0x17, 0x1b, 0x22);
const SECONDARY: Color32 = Color32::from_rgb(0x5b, 0x65, 0x78);
const CONTROL_BORDER: Color32 = Color32::from_rgb(0x7d, 0x8a, 0x9d);
const ERROR: Color32 = Color32::from_rgb(190, 55, 45);
const SELECTED_ROW: Color32 = Color32::from_rgb(0xde, 0xed, 0xff);
const SELECTED_ROW_TEXT: Color32 = Color32::from_rgb(0x06, 0x4b, 0x94);

const DARK_CHROME_TOP: Color32 = Color32::from_rgb(0x2b, 0x30, 0x38);
const DARK_CHROME_BOTTOM: Color32 = Color32::from_rgb(0x24, 0x29, 0x30);
const DARK_SEPARATOR: Color32 = Color32::from_rgb(0x3d, 0x45, 0x51);
const DARK_WORKSPACE: Color32 = Color32::from_rgb(0x1f, 0x24, 0x2b);
const DARK_INK: Color32 = Color32::from_rgb(0xed, 0xf1, 0xf6);
const DARK_SECONDARY: Color32 = Color32::from_rgb(0xa8, 0xb2, 0xc0);
const DARK_CONTROL_BORDER: Color32 = Color32::from_rgb(0x7d, 0x87, 0x96);
const DARK_ERROR: Color32 = Color32::from_rgb(0xff, 0x8a, 0x80);
const DARK_SELECTED_ROW: Color32 = Color32::from_rgb(0x17, 0x3c, 0x61);
const DARK_SELECTED_ROW_TEXT: Color32 = Color32::from_rgb(0xe2, 0xf0, 0xff);
const ACTIVE_THEME_ID: &str = "bs-player-active-theme";

/// Load available system sans fonts once, before the app's first styled frame.
/// All fonts remain runtime OS files; the embedded egui fonts are always kept as
/// fallbacks, and an unavailable/bad system font is ignored safely.
pub fn install_fonts(ctx: &Context) {
    let mut definitions = FontDefinitions::default();
    let proportional_fallbacks = definitions
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();

    let normal_bytes = first_readable_font(normal_font_candidates());
    let semibold_bytes = first_readable_font(semibold_font_candidates());
    let mut semibold_fallbacks = Vec::new();

    if let Some(bytes) = normal_bytes {
        definitions.font_data.insert(
            NORMAL_FONT.to_owned(),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        if let Some(family) = definitions.families.get_mut(&FontFamily::Proportional) {
            family.insert(0, NORMAL_FONT.to_owned());
        }
        semibold_fallbacks.push(NORMAL_FONT.to_owned());
    }

    if let Some(bytes) = semibold_bytes {
        definitions.font_data.insert(
            SEMIBOLD_FONT.to_owned(),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        semibold_fallbacks.insert(0, SEMIBOLD_FONT.to_owned());
    }
    semibold_fallbacks.extend(proportional_fallbacks);
    definitions
        .families
        .insert(FontFamily::Name(SEMIBOLD_FAMILY.into()), semibold_fallbacks);

    ctx.set_fonts(definitions);
}

#[cfg(windows)]
fn normal_font_candidates() -> Vec<PathBuf> {
    std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .map(|root| vec![root.join("Fonts/segoeui.ttf")])
        .unwrap_or_default()
}

#[cfg(windows)]
fn semibold_font_candidates() -> Vec<PathBuf> {
    std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .map(|root| {
            vec![
                root.join("Fonts/seguisb.ttf"),
                root.join("Fonts/segoeuib.ttf"),
            ]
        })
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn normal_font_candidates() -> Vec<PathBuf> {
    [
        "/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/opentype/noto/NotoSans-Regular.ttf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(target_os = "linux")]
fn semibold_font_candidates() -> Vec<PathBuf> {
    [
        "/usr/share/fonts/truetype/liberation2/LiberationSans-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-SemiBold.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf",
        "/usr/share/fonts/opentype/noto/NotoSans-SemiBold.ttf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .collect()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn normal_font_candidates() -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn semibold_font_candidates() -> Vec<PathBuf> {
    Vec::new()
}

fn first_readable_font(paths: Vec<PathBuf>) -> Option<Vec<u8>> {
    paths.into_iter().find_map(|path| read_font(&path))
}

fn read_font(path: &std::path::Path) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let length = file.metadata().ok()?.len();
    if length == 0 || length > FONT_LIMIT {
        return None;
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(FONT_LIMIT + 1).read_to_end(&mut bytes).ok()?;
    // The toolkit panics on a font it cannot read, so the file must really be one
    // it takes (`typ1` is not).
    let recognized_font = font_container_ok(&bytes, 0);
    (bytes.len() as u64 <= FONT_LIMIT && recognized_font).then_some(bytes)
}

/// Install the shared light/dark styles and preserve native monitor DPI.
pub fn apply(ctx: &Context, settings: &Settings) {
    ctx.data_mut(|data| {
        let palette = crate::themes::find(&settings.theme)
            .filter(|palette| !matches!(palette.id, "light" | "dark"));
        let index = palette
            .and_then(|palette| crate::themes::catalogue().iter().position(|item| item.id == palette.id))
            .unwrap_or(usize::MAX);
        data.insert_temp(egui::Id::new(ACTIVE_THEME_ID), index);
    });
    let preference = match settings.theme.as_str() {
        "system" => egui::ThemePreference::System,
        theme if crate::themes::effective_theme(theme, false) == "dark" => {
            egui::ThemePreference::Dark
        }
        _ => egui::ThemePreference::Light,
    };
    if settings.theme == "system" {
        ctx.options_mut(|options| options.fallback_theme = egui::Theme::Light);
    }
    ctx.set_theme(preference);

    let body_size = settings.font_size.clamp(10.0, 32.0);
    let palette = crate::themes::find(&settings.theme)
        .filter(|palette| !matches!(palette.id, "light" | "dark"));
    // A named theme selection seeds Settings.accent with its source palette;
    // subsequent user accent changes always take precedence.
    let accent = rgb(settings.accent);
    for (theme, mut style) in [
        (egui::Theme::Light, egui::Style::default()),
        (egui::Theme::Dark, egui::Style::default()),
    ] {
        let dark = theme == egui::Theme::Dark;
        let base_colors = if dark {
            (
                DARK_INK,
                DARK_SECONDARY,
                DARK_SEPARATOR,
                DARK_WORKSPACE,
                DARK_SELECTED_ROW,
                DARK_SELECTED_ROW_TEXT,
            )
        } else {
            (
                INK,
                SECONDARY,
                SEPARATOR,
                Color32::WHITE,
                SELECTED_ROW,
                SELECTED_ROW_TEXT,
            )
        };
        let (ink, secondary, border, workspace, selected_row, selected_text) = palette
            .filter(|palette| palette.dark == dark)
            .map_or(base_colors, |palette| (
                rgb(crate::themes::normal_text(palette)),
                muted_palette_text(palette),
                rgb(palette.groove),
                rgb(palette.bg),
                rgb(palette.on),
                rgb(crate::themes::selected_text(palette)),
            ));

        style.visuals = if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        // Let each widget state use its own foreground so blue active controls
        // retain white labels while ordinary text keeps the ink color.
        style.visuals.override_text_color = None;
        style.visuals.weak_text_color = Some(secondary);
        if let Some(palette) = palette.filter(|p| p.dark == dark) {
            style.visuals.error_fg_color = readable_palette_color(palette,
                if dark { DARK_ERROR } else { ERROR });
        }
        style.visuals.panel_fill = workspace;
        style.visuals.window_fill = palette
            .filter(|palette| palette.dark == dark)
            .map_or(if dark { DARK_CHROME_BOTTOM } else { CHROME_TOP }, |palette| rgb(palette.panel));
        style.visuals.window_stroke = Stroke::new(1.0, border);
        style.visuals.window_corner_radius = egui::CornerRadius::same(5);
        style.visuals.window_shadow = egui::Shadow {
            offset: [0, 2],
            blur: 8,
            spread: 0,
            color: Color32::from_black_alpha(25),
        };
        style.visuals.text_edit_bg_color = Some(palette
            .filter(|palette| palette.dark == dark)
            .map_or(if dark {
            Color32::from_rgb(0x26, 0x2c, 0x34)
        } else {
            Color32::WHITE
        }, |palette| rgb(palette.panel2)));
        style.visuals.selection.bg_fill = selected_row;
        style.visuals.selection.stroke = Stroke::new(1.0, selected_text);
        style.visuals.hyperlink_color = accent;
        style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, ink);
        style.visuals.widgets.noninteractive.bg_fill = workspace;
        style.visuals.widgets.noninteractive.weak_bg_fill = workspace;
        style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
        style.visuals.widgets.inactive.bg_fill = palette
            .filter(|palette| palette.dark == dark)
            .map_or(if dark {
            Color32::from_rgb(0x2b, 0x31, 0x39)
        } else {
            Color32::WHITE
        }, |palette| rgb(palette.panel));
        style.visuals.widgets.inactive.weak_bg_fill = style.visuals.widgets.inactive.bg_fill;
        style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, border);
        style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, ink);
        style.visuals.widgets.hovered.bg_fill = palette
            .filter(|palette| palette.dark == dark)
            .map_or(if dark {
            Color32::from_rgb(0x34, 0x41, 0x50)
        } else {
            Color32::from_rgb(0xf0, 0xf6, 0xfd)
        }, |palette| rgb(palette.panel2));
        style.visuals.widgets.hovered.weak_bg_fill = style.visuals.widgets.hovered.bg_fill;
        style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, accent);
        style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, ink);
        style.visuals.widgets.active.bg_fill = selected_row;
        style.visuals.widgets.active.weak_bg_fill = selected_row;
        style.visuals.widgets.active.bg_stroke = Stroke::new(2.0, accent);
        style.visuals.widgets.active.fg_stroke = Stroke::new(
            1.0,
            if palette.is_some() && palette.is_some_and(|palette| palette.dark == dark) {
                selected_text
            } else if dark {
                Color32::WHITE
            } else {
                ink
            },
        );
        style.visuals.widgets.open.bg_fill = style.visuals.widgets.hovered.bg_fill;
        style.visuals.widgets.open.weak_bg_fill = style.visuals.widgets.open.bg_fill;
        style.visuals.widgets.open.bg_stroke = Stroke::new(1.0, accent);
        style.visuals.widgets.open.fg_stroke = Stroke::new(1.0, ink);
        for widgets in [
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            widgets.corner_radius = egui::CornerRadius::same(4);
        }

        style.text_styles.insert(
            egui::TextStyle::Body,
            FontId::new(body_size, FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            FontId::new(body_size, FontFamily::Proportional),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            FontId::new((body_size - 2.0).max(11.0), FontFamily::Proportional),
        );
        style
            .text_styles
            .insert(egui::TextStyle::Heading, semibold(body_size + 10.0));
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(9.0, 5.0);
        style.spacing.interact_size.y = body_size + 8.0;
        style.spacing.scroll = egui::style::ScrollStyle::thin();
        style.spacing.scroll.bar_width = 7.0;
        style.spacing.scroll.floating_width = 4.0;
        style.spacing.scroll.floating_allocated_width = 5.0;
        style.spacing.scroll.handle_min_length = 24.0;
        style.spacing.scroll.foreground_color = true;
        style.spacing.scroll.dormant_background_opacity = 0.0;
        style.spacing.scroll.active_background_opacity = 0.15;
        style.spacing.scroll.interact_background_opacity = 0.25;
        style.spacing.scroll.dormant_handle_opacity = 0.35;
        style.spacing.scroll.active_handle_opacity = 0.5;
        style.spacing.scroll.interact_handle_opacity = 0.65;
        ctx.set_style_of(theme, style);
    }

    ctx.set_zoom_factor(settings.ui_scale);
}

/// The selected non-default palette, when the active UI is using one.
/// Widgets that paint custom surfaces can use these same palette colors.
pub fn custom_palette(ui: &Ui) -> Option<&'static crate::themes::Palette> {
    ui.ctx().data(|data| {
        data.get_temp::<usize>(egui::Id::new(ACTIVE_THEME_ID))
            .and_then(|index| crate::themes::catalogue().get(index))
    })
}

fn rgb(value: [u8; 3]) -> Color32 {
    Color32::from_rgb(value[0], value[1], value[2])
}

fn readable_palette_color(palette: &crate::themes::Palette, preferred: Color32) -> Color32 {
    let foreground = rgb(crate::themes::normal_text(palette));
    for amount in 0..=100 {
        let candidate = mix(preferred, foreground, amount as f32 / 100.);
        if [palette.bg, palette.panel, palette.panel2].into_iter()
            .all(|surface| contrast_ratio(candidate, rgb(surface)) >= 4.5) {
            return candidate;
        }
    }
    foreground
}

fn muted_palette_text(palette: &crate::themes::Palette) -> Color32 {
    let bg = rgb(palette.bg);
    let text = rgb(crate::themes::normal_text(palette));
    // Preserve the palette's text color when possible. Otherwise blend toward
    // the foreground until muted labels still meet normal text contrast.
    for step in (60..=100).step_by(5) {
        let candidate = mix(bg, text, step as f32 / 100.0);
        if [bg, rgb(palette.panel), rgb(palette.panel2)]
            .into_iter().all(|surface| contrast_ratio(candidate, surface) >= 4.5) {
            return candidate;
        }
    }
    text
}

/// A cheap interpolated vertical mesh for chrome strips and selected controls.
pub fn gradient(rect: Rect, top: Color32, bottom: Color32) -> Shape {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    Shape::mesh(mesh)
}

fn rounded_gradient(rect: Rect, top: Color32, bottom: Color32) -> Shape {
    let radius = 3.0_f32.min(rect.width() * 0.5).min(rect.height() * 0.5);
    if radius <= 0.0 {
        return gradient(rect, top, bottom);
    }
    let mut ys = [0.; 9];
    for step in 0..=4 {
        ys[step] = rect.top() + radius * step as f32 / 4.0;
    }
    for step in 1..=4 {
        ys[step + 4] = rect.bottom() - radius + radius * step as f32 / 4.0;
    }
    let mut mesh = Mesh::default();
    for (row, y) in ys.iter().enumerate() {
        let in_top = *y - rect.top();
        let in_bottom = rect.bottom() - *y;
        let inset = if in_top < radius {
            radius
                - (radius * radius - (radius - in_top).powi(2))
                    .max(0.0)
                    .sqrt()
        } else if in_bottom < radius {
            radius
                - (radius * radius - (radius - in_bottom).powi(2))
                    .max(0.0)
                    .sqrt()
        } else {
            0.0
        };
        let t = ((*y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
        let color = mix(top, bottom, t);
        mesh.colored_vertex(egui::pos2(rect.left() + inset, *y), color);
        mesh.colored_vertex(egui::pos2(rect.right() - inset, *y), color);
        if row > 0 {
            let previous = (row as u32 - 1) * 2;
            let current = row as u32 * 2;
            mesh.add_triangle(previous, previous + 1, current);
            mesh.add_triangle(previous + 1, current + 1, current);
        }
    }
    Shape::mesh(mesh)
}

fn mix(top: Color32, bottom: Color32, amount: f32) -> Color32 {
    let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
    Color32::from_rgba_unmultiplied(
        channel(top.r(), bottom.r()),
        channel(top.g(), bottom.g()),
        channel(top.b(), bottom.b()),
        channel(top.a(), bottom.a()),
    )
}

fn relative_luminance(color: Color32) -> f32 {
    let linear = |channel: u8| {
        let value = channel as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
}

fn contrast_ratio(a: Color32, b: Color32) -> f32 {
    let (lighter, darker) = {
        let a = relative_luminance(a);
        let b = relative_luminance(b);
        if a >= b { (a, b) } else { (b, a) }
    };
    (lighter + 0.05) / (darker + 0.05)
}

fn contrast_foreground(background: Color32) -> Color32 {
    const TARGET: f32 = 4.5;
    let white = Color32::WHITE;
    let ink = INK;
    if contrast_ratio(white, background) >= TARGET {
        white
    } else if contrast_ratio(ink, background) >= TARGET {
        ink
    } else {
        // The luminance crossover between pure black and white always permits
        // one of them to exceed 4.5:1; this is only needed for mid-tone colors.
        Color32::BLACK
    }
}

/// Choose readable text or icon ink for a surface filled with an accent color.
pub fn accent_foreground(accent: Color32) -> Color32 {
    contrast_foreground(accent)
}

/// Return the foreground used by selected navigation labels and icons.
///
/// The selected navigation fill is derived from the current accent, so its
/// foreground must be chosen from that same color rather than fixed to white.
pub fn navigation_selected_foreground(ui: &Ui) -> Color32 {
    accent_foreground(ui.visuals().hyperlink_color)
}

fn navigation_selected_colors(accent: Color32) -> (Color32, Color32, Color32) {
    let foreground = accent_foreground(accent);
    let opposite = if foreground == Color32::WHITE {
        Color32::BLACK
    } else {
        Color32::WHITE
    };
    // Keep the accent intact at the top of the selected row, then tint a little
    // farther away from the text color so both gradient stops remain readable.
    (foreground, accent, mix(accent, opposite, 0.08))
}

pub fn chrome_colors(ui: &Ui) -> (Color32, Color32) {
    if let Some(palette) = custom_palette(ui) {
        (rgb(palette.panel), rgb(palette.panel2))
    } else if ui.visuals().dark_mode {
        (DARK_CHROME_TOP, DARK_CHROME_BOTTOM)
    } else {
        (CHROME_TOP, CHROME_BOTTOM)
    }
}

pub fn separator(ui: &Ui) -> Color32 {
    if let Some(palette) = custom_palette(ui) {
        rgb(palette.groove)
    } else if ui.visuals().dark_mode {
        DARK_SEPARATOR
    } else {
        SEPARATOR
    }
}

/// Outline of an unchecked checkbox: at least 3:1 against the surfaces it sits on.
fn control_border(ui: &Ui) -> Color32 {
    if let Some(palette) = custom_palette(ui) {
        rgb(palette.groove)
    } else if ui.visuals().dark_mode {
        DARK_CONTROL_BORDER
    } else {
        CONTROL_BORDER
    }
}

/// Error text readable (4.5:1 or better) on every surface of both themes.
pub fn error_color(ui: &Ui) -> Color32 {
    if custom_palette(ui).is_some() {
        ui.visuals().error_fg_color
    } else if ui.visuals().dark_mode {
        DARK_ERROR
    } else {
        ERROR
    }
}

pub fn workspace(ui: &Ui) -> Color32 {
    if let Some(palette) = custom_palette(ui) {
        rgb(palette.bg)
    } else if ui.visuals().dark_mode {
        DARK_WORKSPACE
    } else {
        Color32::WHITE
    }
}

pub fn row_fill(ui: &Ui, selected: bool, alternate: bool) -> Color32 {
    if let Some(palette) = custom_palette(ui) {
        if selected {
            rgb(palette.on)
        } else if alternate {
            rgb(palette.panel2)
        } else {
            rgb(palette.bg)
        }
    } else if ui.visuals().dark_mode {
        if selected {
            DARK_SELECTED_ROW
        } else if alternate {
            Color32::from_rgb(0x25, 0x2b, 0x33)
        } else {
            DARK_WORKSPACE
        }
    } else if selected {
        SELECTED_ROW
    } else if alternate {
        Color32::from_rgb(0xf5, 0xf7, 0xfa)
    } else {
        Color32::WHITE
    }
}

/// Outline of the primary row while several rows are selected. It is the text
/// colour of a selected row, which stands out from the selected fill and from
/// the plain rows next to it.
pub fn row_primary_outline(ui: &Ui) -> Color32 {
    row_text(ui, true)
}

pub fn row_text(ui: &Ui, selected: bool) -> Color32 {
    if custom_palette(ui).is_some() {
        return if selected {
            ui.visuals().selection.stroke.color
        } else {
            ui.visuals().text_color()
        };
    }
    match (ui.visuals().dark_mode, selected) {
        (true, true) => DARK_SELECTED_ROW_TEXT,
        (false, true) => SELECTED_ROW_TEXT,
        (true, false) => DARK_INK,
        (false, false) => INK,
    }
}

/// Keep egui's checkbox interaction and accessibility metadata while painting a
/// crisp, blue-filled checked square that matches the Windows-style controls.
pub fn checkbox(ui: &mut Ui, checked: &mut bool, label: &str) -> Response {
    let response = ui.checkbox(checked, label);
    let size = ui.spacing().icon_width;
    let icon = Rect::from_center_size(
        egui::pos2(response.rect.left() + size * 0.5, response.rect.center().y),
        Vec2::splat(size),
    );
    let frame = icon.expand(2.0);
    let accent = ui.visuals().hyperlink_color;
    let disabled = !ui.is_enabled();
    let fill = if *checked {
        if disabled {
            if let Some(palette) = custom_palette(ui) {
                rgb(palette.panel2)
            } else if ui.visuals().dark_mode {
                Color32::from_rgb(0x3d, 0x51, 0x65)
            } else {
                Color32::from_rgb(0x8e, 0xb9, 0xe8)
            }
        } else {
            accent
        }
    } else if response.hovered() && !disabled {
        if let Some(palette) = custom_palette(ui) {
            rgb(palette.panel2)
        } else if ui.visuals().dark_mode {
            Color32::from_rgb(0x32, 0x3b, 0x47)
        } else {
            Color32::from_rgb(0xf0, 0xf6, 0xfd)
        }
    } else if let Some(palette) = custom_palette(ui) {
        rgb(palette.panel)
    } else if ui.visuals().dark_mode {
        Color32::from_rgb(0x20, 0x25, 0x2c)
    } else {
        Color32::WHITE
    };
    let border = if response.has_focus() || response.hovered() && !disabled {
        accent
    } else {
        control_border(ui)
    };
    let painter = ui.painter();
    painter.rect_filled(frame, egui::CornerRadius::same(3), fill);
    painter.rect_stroke(
        frame,
        egui::CornerRadius::same(3),
        Stroke::new(if response.has_focus() { 2.0 } else { 1.0 }, border),
        StrokeKind::Inside,
    );
    if *checked {
        let check = icon.shrink(size * 0.23);
        painter.add(Shape::line(
            vec![
                egui::pos2(check.left(), check.center().y),
                egui::pos2(check.center().x, check.bottom()),
                egui::pos2(check.right(), check.top()),
            ],
            Stroke::new(2.0, contrast_foreground(fill)),
        ));
    }
    response
}

/// Use egui's native slider interaction with a visible neutral rail and a
/// circular, lightly filled handle. The scoped style leaves surrounding buttons
/// and inputs untouched.
pub fn slider(ui: &mut Ui, slider: egui::Slider<'_>) -> Response {
    ui.scope(|ui| {
        let dark = ui.visuals().dark_mode;
        let accent = ui.visuals().hyperlink_color;
        let ink = ui.visuals().text_color();
        let rail = if let Some(palette) = custom_palette(ui) {
            rgb(palette.groove)
        } else if dark {
            Color32::from_rgb(0x4b, 0x55, 0x63)
        } else {
            Color32::from_rgb(0xcf, 0xd7, 0xe2)
        };
        let handle = if let Some(palette) = custom_palette(ui) {
            rgb(palette.panel2)
        } else if dark {
            Color32::from_rgb(0xe1, 0xe6, 0xed)
        } else {
            Color32::from_rgb(0xf8, 0xfa, 0xfc)
        };
        let handle_border = if let Some(palette) = custom_palette(ui) {
            rgb(palette.groove)
        } else if dark {
            Color32::from_rgb(0x9d, 0xa8, 0xb7)
        } else {
            Color32::from_rgb(0x9a, 0xa6, 0xb5)
        };
        let style = ui.style_mut();
        style.visuals.override_text_color = Some(ink);
        style.visuals.slider_trailing_fill = true;
        style.visuals.selection.bg_fill = accent;
        style.visuals.handle_shape = egui::style::HandleShape::Circle;
        style.spacing.slider_rail_height = 4.0;
        style.visuals.widgets.inactive.bg_fill = rail;
        style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, rail);
        style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, handle_border);
        style.visuals.widgets.hovered.bg_fill = handle;
        style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, handle_border);
        style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, handle_border);
        style.visuals.widgets.active.bg_fill = handle;
        style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, handle_border);
        style.visuals.widgets.active.fg_stroke = Stroke::new(1.0, handle_border);
        ui.add(slider)
    })
    .inner
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD_FAMILY.into()))
}

/// As `navigation`, with reserved leading space for a separately painted icon.
pub fn navigation_indented(ui: &mut Ui, label: &str, selected: bool, leading: f32) -> Response {
    let width = ui.available_width().max(1.0);
    let body_size = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Button)
        .map_or(14.0, |font| font.size);
    // Only selected rows need accent contrast and gradient calculations.
    let selected_colors =
        selected.then(|| navigation_selected_colors(ui.visuals().hyperlink_color));
    let text_color = selected_colors
        .map(|(text, _, _)| text)
        .unwrap_or_else(|| row_text(ui, false));
    let font_id = if selected {
        semibold(body_size)
    } else {
        FontId::new(body_size, FontFamily::Proportional)
    };
    let padding_x = leading.max(8.0);
    let galley = ui.painter().layout(
        label.to_owned(),
        font_id,
        text_color,
        (width - padding_x - 12.0).max(24.0),
    );
    let row_height = (galley.size().y + 6.).max(body_size + 10.0).max(26.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, row_height), Sense::click());
    if let Some((_, selected_top, selected_bottom)) = selected_colors {
        ui.painter()
            .add(rounded_gradient(rect, selected_top, selected_bottom));
    } else if response.hovered() {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(3),
            if ui.visuals().dark_mode {
                Color32::from_rgb(0x2c, 0x35, 0x40)
            } else {
                Color32::from_rgb(0xf0, 0xf5, 0xfb)
            },
        );
    }
    if response.has_focus() {
        let focus_color = if selected {
            text_color
        } else {
            ui.visuals().hyperlink_color
        };
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(3),
            Stroke::new(2.0, focus_color),
            StrokeKind::Inside,
        );
    }
    ui.painter().galley(
        egui::pos2(
            rect.left() + padding_x,
            rect.center().y - galley.size().y * 0.5,
        ),
        galley,
        text_color,
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label)
    });
    response.on_hover_ui(|ui| {
        ui.set_max_width(360.0);
        ui.label(label);
    })
}

/// Height of a sidebar row with a second line (`navigation_detail`): both lines
/// of text and a little padding, the same at every width.
pub fn detail_row_height(ui: &Ui) -> f32 {
    let body = ui.text_style_height(&egui::TextStyle::Button);
    let small = ui.text_style_height(&egui::TextStyle::Small);
    (body + small + 10.0).max(36.0)
}

/// Height of a sidebar group heading row (`navigation_heading`).
pub fn heading_row_height(ui: &Ui) -> f32 {
    (ui.text_style_height(&egui::TextStyle::Button) + 8.0).max(26.0)
}

fn truncated(
    ui: &Ui,
    text: &str,
    font_id: FontId,
    color: Color32,
    width: f32,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font_id, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(1.0));
    ui.painter().layout_job(job)
}

/// As `navigation_indented`, with a weak second line `detail` under the label.
/// The row has the fixed height `detail_row_height`; a long label or detail is
/// cut with an ellipsis instead of wrapping.
pub fn navigation_detail(
    ui: &mut Ui,
    label: &str,
    detail: &str,
    selected: bool,
    leading: f32,
) -> Response {
    let width = ui.available_width().max(1.0);
    let body_size = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Button)
        .map_or(14.0, |font| font.size);
    let small_size = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Small)
        .map_or(11.0, |font| font.size);
    let selected_colors =
        selected.then(|| navigation_selected_colors(ui.visuals().hyperlink_color));
    let text_color = selected_colors
        .map(|(text, _, _)| text)
        .unwrap_or_else(|| row_text(ui, false));
    let detail_color = if selected_colors.is_some() {
        text_color.gamma_multiply(0.85)
    } else {
        ui.visuals().weak_text_color()
    };
    let font_id = if selected {
        semibold(body_size)
    } else {
        FontId::new(body_size, FontFamily::Proportional)
    };
    let padding_x = leading.max(8.0);
    let text_width = (width - padding_x - 12.0).max(24.0);
    let label_galley = truncated(ui, label, font_id, text_color, text_width);
    let detail_galley = truncated(
        ui,
        detail,
        FontId::new(small_size, FontFamily::Proportional),
        detail_color,
        text_width,
    );
    let height = detail_row_height(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    if let Some((_, selected_top, selected_bottom)) = selected_colors {
        ui.painter()
            .add(rounded_gradient(rect, selected_top, selected_bottom));
    } else if response.hovered() {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(3),
            if ui.visuals().dark_mode {
                Color32::from_rgb(0x2c, 0x35, 0x40)
            } else {
                Color32::from_rgb(0xf0, 0xf5, 0xfb)
            },
        );
    }
    if response.has_focus() {
        let focus_color = if selected {
            text_color
        } else {
            ui.visuals().hyperlink_color
        };
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(3),
            Stroke::new(2.0, focus_color),
            StrokeKind::Inside,
        );
    }
    let block = label_galley.size().y + detail_galley.size().y;
    let top = rect.center().y - block * 0.5;
    let label_height = label_galley.size().y;
    ui.painter().galley(
        egui::pos2(rect.left() + padding_x, top),
        label_galley,
        text_color,
    );
    ui.painter().galley(
        egui::pos2(rect.left() + padding_x, top + label_height),
        detail_galley,
        detail_color,
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            true,
            selected,
            format!("{label}, {detail}"),
        )
    });
    response
}

/// A collapsible group heading row of a sidebar list: a triangle that points
/// down when `open`, the name and a weak count. Click toggles it.
pub fn navigation_heading(ui: &mut Ui, label: &str, count: usize, open: bool) -> Response {
    let width = ui.available_width().max(1.0);
    let height = heading_row_height(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(3),
            if ui.visuals().dark_mode {
                Color32::from_rgb(0x2c, 0x35, 0x40)
            } else {
                Color32::from_rgb(0xf0, 0xf5, 0xfb)
            },
        );
    }
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            egui::CornerRadius::same(3),
            Stroke::new(2.0, ui.visuals().hyperlink_color),
            StrokeKind::Inside,
        );
    }
    let weak = ui.visuals().weak_text_color();
    let center = egui::pos2(rect.left() + 12.0, rect.center().y);
    let points = if open {
        vec![
            center + Vec2::new(-4.0, -2.0),
            center + Vec2::new(4.0, -2.0),
            center + Vec2::new(0.0, 3.0),
        ]
    } else {
        vec![
            center + Vec2::new(-2.0, -4.0),
            center + Vec2::new(-2.0, 4.0),
            center + Vec2::new(3.0, 0.0),
        ]
    };
    ui.painter()
        .add(Shape::convex_polygon(points, weak, Stroke::NONE));
    let count_text = count.to_string();
    let small_size = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Small)
        .map_or(11.0, |font| font.size);
    let count_galley = ui.painter().layout_no_wrap(
        count_text,
        FontId::new(small_size, FontFamily::Proportional),
        weak,
    );
    let count_width = count_galley.size().x;
    let body_size = ui
        .style()
        .text_styles
        .get(&egui::TextStyle::Button)
        .map_or(14.0, |font| font.size);
    let label_galley = truncated(
        ui,
        label,
        semibold(body_size),
        row_text(ui, false),
        rect.width() - 26.0 - count_width - 20.0,
    );
    ui.painter().galley(
        egui::pos2(
            rect.left() + 24.0,
            rect.center().y - label_galley.size().y * 0.5,
        ),
        label_galley,
        row_text(ui, false),
    );
    ui.painter().galley(
        egui::pos2(
            rect.right() - 10.0 - count_width,
            rect.center().y - count_galley.size().y * 0.5,
        ),
        count_galley,
        weak,
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!(
                "{} {label}, {count} books",
                if open { "Collapse" } else { "Expand" }
            ),
        )
    });
    response
}
/// A small clickable schematic of the real player layout for the theme picker.
/// The System card shows both palettes; it contains no sample library content.
pub fn theme_preview(ui: &mut Ui, theme: &str, selected: bool) -> Response {
    let size = Vec2::new(105.0, 70.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let border = if selected {
        ui.visuals().hyperlink_color
    } else {
        separator(ui)
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(4), workspace(ui));
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(4),
        Stroke::new(if selected { 2.0 } else { 1.0 }, border),
        StrokeKind::Inside,
    );
    let inner = rect.shrink(5.0);
    match theme {
        "dark" => miniature(ui, inner, true),
        "system" => {
            let mid = inner.center().x;
            miniature(
                ui,
                Rect::from_min_max(inner.min, egui::pos2(mid, inner.max.y)),
                false,
            );
            miniature(
                ui,
                Rect::from_min_max(egui::pos2(mid, inner.min.y), inner.max),
                true,
            );
        }
        _ => miniature(ui, inner, false),
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, theme)
    });
    response.on_hover_text(match theme {
        "dark" => "Dark theme",
        "system" => "Follow the system theme",
        _ => "Light theme",
    })
}

fn miniature(ui: &Ui, rect: Rect, dark: bool) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let (top, bottom, border, page, alternate, selected) = if dark {
        (
            DARK_CHROME_TOP,
            DARK_CHROME_BOTTOM,
            DARK_SEPARATOR,
            DARK_WORKSPACE,
            Color32::from_rgb(0x25, 0x2b, 0x33),
            DARK_SELECTED_ROW,
        )
    } else {
        (
            CHROME_TOP,
            CHROME_BOTTOM,
            SEPARATOR,
            Color32::WHITE,
            Color32::from_rgb(0xf5, 0xf7, 0xfa),
            SELECTED_ROW,
        )
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(2), page);
    let title_height = (rect.height() * 0.16).max(4.0);
    let footer_height = (rect.height() * 0.13).max(3.0);
    let title = Rect::from_min_max(
        rect.min,
        egui::pos2(rect.right(), rect.top() + title_height),
    );
    ui.painter().add(gradient(title, top, bottom));
    let content = Rect::from_min_max(
        egui::pos2(rect.left(), title.bottom()),
        egui::pos2(rect.right(), rect.bottom() - footer_height),
    );
    let nav_width = content.width() * 0.26;
    let nav = Rect::from_min_size(content.min, Vec2::new(nav_width, content.height()));
    ui.painter().rect_filled(nav, 0.0, bottom);
    ui.painter().line_segment(
        [nav.right_top(), nav.right_bottom()],
        Stroke::new(0.7, border),
    );
    let selected_height = (content.height() * 0.18).max(3.0);
    let selected_nav = Rect::from_min_size(
        egui::pos2(nav.left() + 1.0, nav.top() + 2.0),
        Vec2::new((nav.width() - 2.0).max(1.0), selected_height),
    );
    ui.painter().rect_filled(
        selected_nav,
        egui::CornerRadius::same(1),
        Color32::from_rgb(0x26, 0x89, 0xef),
    );
    let list_left = nav.right() + content.width() * 0.05;
    let list_right = rect.right() - 3.0;
    let first_y = content.top() + 3.0;
    let row_height = ((content.bottom() - first_y) / 4.0).max(2.0);
    for row in 0..3 {
        let y = first_y + row as f32 * row_height;
        let row_rect = Rect::from_min_max(
            egui::pos2(list_left, y),
            egui::pos2(list_right, (y + row_height).min(content.bottom())),
        );
        if row == 1 {
            ui.painter().rect_filled(row_rect, 1.0, selected);
        } else if row == 2 {
            ui.painter().rect_filled(row_rect, 1.0, alternate);
        }
        let line_y = row_rect.center().y;
        ui.painter().line_segment(
            [
                egui::pos2(list_left + 2.0, line_y),
                egui::pos2((list_right - 3.0).max(list_left + 3.0), line_y),
            ],
            Stroke::new(
                1.0,
                if row == 1 && !dark {
                    SELECTED_ROW_TEXT
                } else {
                    border
                },
            ),
        );
    }
    let footer = Rect::from_min_max(
        egui::pos2(rect.left(), rect.bottom() - footer_height),
        rect.max,
    );
    ui.painter().add(gradient(footer, top, bottom));
}

// ---------------------------------------------------------------------------
// Fonts for other scripts
//
// The fonts the player draws with cover Latin, Greek and Cyrillic. Titles in
// Chinese, Japanese, Korean, Thai, the Indic scripts, Hebrew or Arabic would
// show as boxes. When such text exists in the library, the matching font of the
// operating system is read once and appended to the font families as a
// fallback, so nothing is loaded for a library without it.
// ---------------------------------------------------------------------------

/// Largest font file the fallback loader reads (Noto Sans CJK is about 20 MiB).
const FALLBACK_FONT_LIMIT: u64 = 64 * 1024 * 1024;
/// Most entries the search of a font folder looks at, and how deep it goes.
const FONT_SCAN_ENTRIES: usize = 20_000;
const FONT_SCAN_DEPTH: usize = 5;
/// The library and the name lists are looked at no more often than this.
const FONT_CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// A writing system the bundled fonts do not draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Script {
    /// Chinese characters (also the ones Japanese and Korean text uses), CJK
    /// punctuation and full-width forms.
    Han,
    /// Hiragana and Katakana.
    Kana,
    Hangul,
    Thai,
    Devanagari,
    Bengali,
    Tamil,
    Gurmukhi,
    Gujarati,
    Oriya,
    Telugu,
    Kannada,
    Malayalam,
    Sinhala,
    Hebrew,
    Arabic,
}

impl Script {
    pub const ALL: [Script; 16] = [
        Script::Han,
        Script::Kana,
        Script::Hangul,
        Script::Thai,
        Script::Devanagari,
        Script::Bengali,
        Script::Tamil,
        Script::Gurmukhi,
        Script::Gujarati,
        Script::Oriya,
        Script::Telugu,
        Script::Kannada,
        Script::Malayalam,
        Script::Sinhala,
        Script::Hebrew,
        Script::Arabic,
    ];

    const fn bit(self) -> u32 {
        1 << self as u32
    }

    /// A letter that every font made for the script draws.
    pub const fn probe(self) -> char {
        match self {
            Script::Han => '\u{7684}',
            Script::Kana => '\u{3042}',
            Script::Hangul => '\u{D55C}',
            Script::Thai => '\u{0E01}',
            Script::Devanagari => '\u{0905}',
            Script::Bengali => '\u{0985}',
            Script::Tamil => '\u{0B85}',
            Script::Gurmukhi => '\u{0A05}',
            Script::Gujarati => '\u{0A85}',
            Script::Oriya => '\u{0B05}',
            Script::Telugu => '\u{0C05}',
            Script::Kannada => '\u{0C85}',
            Script::Malayalam => '\u{0D05}',
            Script::Sinhala => '\u{0D85}',
            Script::Hebrew => '\u{05D0}',
            Script::Arabic => '\u{0627}',
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Script::Han => "han",
            Script::Kana => "kana",
            Script::Hangul => "hangul",
            Script::Thai => "thai",
            Script::Devanagari => "devanagari",
            Script::Bengali => "bengali",
            Script::Tamil => "tamil",
            Script::Gurmukhi => "gurmukhi",
            Script::Gujarati => "gujarati",
            Script::Oriya => "oriya",
            Script::Telugu => "telugu",
            Script::Kannada => "kannada",
            Script::Malayalam => "malayalam",
            Script::Sinhala => "sinhala",
            Script::Hebrew => "hebrew",
            Script::Arabic => "arabic",
        }
    }

    /// The script a character belongs to, if it is one that needs a font of its
    /// own. Latin, Greek, Cyrillic, symbols and emoji are `None`.
    pub fn of(c: char) -> Option<Script> {
        let code = c as u32;
        Some(match code {
            0x0590..=0x05FF | 0xFB1D..=0xFB4F => Script::Hebrew,
            0x0600..=0x06FF
            | 0x0750..=0x077F
            | 0x08A0..=0x08FF
            | 0xFB50..=0xFDFF
            | 0xFE70..=0xFEFF => Script::Arabic,
            0x0900..=0x097F | 0xA8E0..=0xA8FF => Script::Devanagari,
            0x0980..=0x09FF => Script::Bengali,
            0x0A00..=0x0A7F => Script::Gurmukhi,
            0x0A80..=0x0AFF => Script::Gujarati,
            0x0B00..=0x0B7F => Script::Oriya,
            0x0B80..=0x0BFF => Script::Tamil,
            0x0C00..=0x0C7F => Script::Telugu,
            0x0C80..=0x0CFF => Script::Kannada,
            0x0D00..=0x0D7F => Script::Malayalam,
            0x0D80..=0x0DFF => Script::Sinhala,
            0x0E00..=0x0E7F => Script::Thai,
            0x1100..=0x11FF
            | 0x3130..=0x318F
            | 0xA960..=0xA97F
            | 0xAC00..=0xD7FF
            | 0xFFA0..=0xFFDC => Script::Hangul,
            0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F | 0x1B000..=0x1B16F => Script::Kana,
            0x2E80..=0x2FDF
            | 0x3001..=0x303F
            | 0x31C0..=0x31EF
            | 0x3200..=0x33FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF01..=0xFF65
            | 0xFFE0..=0xFFE6
            | 0x20000..=0x2FA1F
            | 0x30000..=0x3134F => Script::Han,
            _ => return None,
        })
    }
}

/// A set of scripts.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct ScriptSet(u32);

impl ScriptSet {
    pub const EMPTY: Self = Self(0);

    pub fn all() -> Self {
        Script::ALL
            .into_iter()
            .fold(Self::EMPTY, |set, script| set.with(script))
    }
    pub fn of(script: Script) -> Self {
        Self(script.bit())
    }
    pub fn with(self, script: Script) -> Self {
        Self(self.0 | script.bit())
    }
    pub fn contains(self, script: Script) -> bool {
        self.0 & script.bit() != 0
    }
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    pub fn minus(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    /// The scripts of the set, in the order of `Script::ALL`.
    pub fn iter(self) -> impl Iterator<Item = Script> {
        Script::ALL
            .into_iter()
            .filter(move |script| self.contains(*script))
    }
}

impl std::fmt::Debug for ScriptSet {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_set()
            .entries(self.iter().map(Script::slug))
            .finish()
    }
}

/// The scripts in `text` that the bundled fonts cannot draw. Text in Latin,
/// Greek and Cyrillic (and symbols and emoji) needs nothing; pure ASCII is
/// answered without looking at a single character.
pub fn scripts_needed(text: &str) -> ScriptSet {
    let mut set = ScriptSet::EMPTY;
    if text.is_ascii() {
        return set;
    }
    for c in text.chars() {
        // Everything below the Hebrew block is Latin, Greek, Cyrillic or Armenian.
        if (c as u32) < 0x0590 {
            continue;
        }
        if let Some(script) = Script::of(c) {
            set = set.with(script);
        }
    }
    set
}

/// What one library entry needs: its tags and its file name.
pub fn scripts_in_track(track: &Track) -> ScriptSet {
    let mut set = ScriptSet::EMPTY;
    for text in [
        &track.title,
        &track.artist,
        &track.album,
        &track.genre,
        &track.album_artist,
        &track.composer,
        &track.narrator,
    ] {
        set = set.union(scripts_needed(text));
    }
    if let Some(name) = track.path.file_name().and_then(|name| name.to_str()) {
        set = set.union(scripts_needed(name));
    }
    set
}

/// The operating systems whose font folders are known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontPlatform {
    Windows,
    Linux,
    MacOs,
    Other,
}

impl FontPlatform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

/// A font file to try, and which face of a collection (`.ttc`) to use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontCandidate {
    pub path: PathBuf,
    pub index: u32,
}

/// Where a font for one script may be: files to try in order, then (Linux) folders
/// to search for files of these names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontPlan {
    pub files: Vec<FontCandidate>,
    pub roots: Vec<PathBuf>,
    pub names: Vec<(String, u32)>,
}

/// How the planner reads the environment (a closure, so tests need no real one).
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<std::ffi::OsString>;

/// The Windows font folder: `%WINDIR%`, else `%SystemRoot%`, else `C:\Windows`.
fn windows_fonts_dir(env: EnvLookup) -> PathBuf {
    ["WINDIR", "SystemRoot"]
        .into_iter()
        .find_map(|name| env(name).filter(|value| !value.is_empty()))
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join("Fonts")
}

const WIN_CHINESE: &[&str] = &[
    "msyh.ttc",
    "msyh.ttf",
    "msjh.ttc",
    "simsun.ttc",
    "simhei.ttf",
    "mingliu.ttc",
];
const WIN_JAPANESE: &[&str] = &[
    "YuGothR.ttc",
    "meiryo.ttc",
    "YuGothM.ttc",
    "msgothic.ttc",
    "YuGothL.ttc",
];
const WIN_KOREAN: &[&str] = &["malgun.ttf", "gulim.ttc", "batang.ttc"];
const WIN_ARIAL_UNICODE: &str = "arialuni.ttf";

/// Font files of the Windows font folder for `script`, best first.
fn windows_files(script: Script, wanted: ScriptSet) -> Vec<&'static str> {
    let own: &[&str] = match script {
        Script::Han if wanted.contains(Script::Kana) => {
            return [WIN_JAPANESE, WIN_CHINESE, WIN_KOREAN, &[WIN_ARIAL_UNICODE]].concat();
        }
        Script::Han => {
            return [WIN_CHINESE, WIN_JAPANESE, WIN_KOREAN, &[WIN_ARIAL_UNICODE]].concat();
        }
        Script::Kana => return [WIN_JAPANESE, WIN_CHINESE, &[WIN_ARIAL_UNICODE]].concat(),
        Script::Hangul => return [WIN_KOREAN, &[WIN_ARIAL_UNICODE]].concat(),
        Script::Thai => &["leelawui.ttf", "leelawad.ttf", "tahoma.ttf"],
        Script::Hebrew => &["segoeui.ttf", "tahoma.ttf", "arial.ttf", "david.ttf"],
        Script::Arabic => &["segoeui.ttf", "tahoma.ttf", "arial.ttf"],
        // Nirmala UI draws every Indic script; the older single-script fonts follow.
        Script::Devanagari => &["nirmala.ttf", "mangal.ttf"],
        Script::Bengali => &["nirmala.ttf", "vrinda.ttf"],
        Script::Tamil => &["nirmala.ttf", "latha.ttf"],
        Script::Gurmukhi => &["nirmala.ttf", "raavi.ttf"],
        Script::Gujarati => &["nirmala.ttf", "shruti.ttf"],
        Script::Oriya => &["nirmala.ttf", "kalinga.ttf"],
        Script::Telugu => &["nirmala.ttf", "gautami.ttf"],
        Script::Kannada => &["nirmala.ttf", "tunga.ttf"],
        Script::Malayalam => &["nirmala.ttf", "kartika.ttf"],
        Script::Sinhala => &["nirmala.ttf", "iskpota.ttf"],
    };
    [own, &[WIN_ARIAL_UNICODE]].concat()
}

/// Face numbers inside `NotoSansCJK-Regular.ttc`: Japanese, Korean, Simplified
/// Chinese, Traditional Chinese.
const NOTO_CJK_JAPANESE: u32 = 0;
const NOTO_CJK_KOREAN: u32 = 1;
const NOTO_CJK_SIMPLIFIED: u32 = 2;

/// Folders under which a Linux font is searched for when it is not at a known place.
fn linux_roots(env: EnvLookup) -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    let home = env("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    match env("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        Some(data) => roots.push(PathBuf::from(data).join("fonts")),
        None => {
            if let Some(home) = &home {
                roots.push(home.join(".local/share/fonts"));
            }
        }
    }
    if let Some(home) = &home {
        roots.push(home.join(".fonts"));
    }
    roots
}

/// The plan for a Linux font: the usual places of the Noto, Droid, WenQuanYi,
/// DejaVu, FreeFont and Lohit families, then a search by file name.
fn linux_plan(script: Script, wanted: ScriptSet, env: EnvLookup) -> FontPlan {
    let kana = wanted.contains(Script::Kana);
    // (file name, face) pairs, best first.
    let mut names: Vec<(String, u32)> = Vec::new();
    let mut add = |name: &str, index: u32| names.push((name.to_owned(), index));
    let cjk_ttc = "NotoSansCJK-Regular.ttc";
    let wenquanyi = ["wqy-microhei.ttc", "wqy-zenhei.ttc"];
    match script {
        Script::Han | Script::Kana | Script::Hangul => {
            let (face, split) = match script {
                Script::Kana => (NOTO_CJK_JAPANESE, "NotoSansCJKjp-Regular.otf"),
                Script::Hangul => (NOTO_CJK_KOREAN, "NotoSansCJKkr-Regular.otf"),
                _ if kana => (NOTO_CJK_JAPANESE, "NotoSansCJKjp-Regular.otf"),
                _ => (NOTO_CJK_SIMPLIFIED, "NotoSansCJKsc-Regular.otf"),
            };
            add(cjk_ttc, face);
            add(split, 0);
            if script == Script::Hangul {
                add("DroidSansFallbackFull.ttf", 0);
            } else {
                for name in wenquanyi {
                    add(name, 0);
                }
                add("DroidSansFallbackFull.ttf", 0);
                add("uming.ttc", 0);
            }
        }
        Script::Thai => {
            for name in [
                "NotoSansThai-Regular.ttf",
                "NotoSansThaiUI-Regular.ttf",
                "Loma.otf",
                "Loma.ttf",
                "Garuda.otf",
                "Garuda.ttf",
                "Norasi.otf",
                "Norasi.ttf",
                "Kinnari.otf",
                "Waree.otf",
                "FreeSerif.ttf",
            ] {
                add(name, 0);
            }
        }
        Script::Hebrew => {
            for name in [
                "NotoSansHebrew-Regular.ttf",
                "DejaVuSans.ttf",
                "FreeSans.ttf",
            ] {
                add(name, 0);
            }
        }
        Script::Arabic => {
            for name in [
                "NotoSansArabic-Regular.ttf",
                "NotoNaskhArabic-Regular.ttf",
                "DejaVuSans.ttf",
            ] {
                add(name, 0);
            }
        }
        indic => {
            let noto = match indic {
                Script::Devanagari => "Devanagari",
                Script::Bengali => "Bengali",
                Script::Tamil => "Tamil",
                Script::Gurmukhi => "Gurmukhi",
                Script::Gujarati => "Gujarati",
                Script::Oriya => "Oriya",
                Script::Telugu => "Telugu",
                Script::Kannada => "Kannada",
                Script::Malayalam => "Malayalam",
                _ => "Sinhala",
            };
            add(&format!("NotoSans{noto}-Regular.ttf"), 0);
            add(&format!("Lohit-{noto}.ttf"), 0);
            if indic == Script::Oriya {
                add("Lohit-Odia.ttf", 0);
            }
            add("FreeSans.ttf", 0);
        }
    }
    // The usual folders of the families above, tried before any search.
    let folders = [
        "/usr/share/fonts/opentype/noto",
        "/usr/share/fonts/truetype/noto",
        "/usr/share/fonts/noto",
        "/usr/share/fonts/noto-cjk",
        "/usr/share/fonts/google-noto",
        "/usr/share/fonts/google-noto-cjk",
        "/usr/share/fonts/google-noto-sans-cjk-fonts",
        "/usr/share/fonts/truetype/droid",
        "/usr/share/fonts/droid",
        "/usr/share/fonts/truetype/wqy",
        "/usr/share/fonts/wenquanyi/wqy-microhei",
        "/usr/share/fonts/wenquanyi/wqy-zenhei",
        "/usr/share/fonts/truetype/arphic",
        "/usr/share/fonts/truetype/dejavu",
        "/usr/share/fonts/dejavu",
        "/usr/share/fonts/TTF",
        "/usr/share/fonts/truetype/freefont",
        "/usr/share/fonts/truetype/tlwg",
        "/usr/share/fonts/opentype/tlwg",
    ];
    let mut files = Vec::new();
    for (name, index) in &names {
        for folder in folders {
            files.push(FontCandidate {
                path: PathBuf::from(folder).join(name),
                index: *index,
            });
        }
    }
    FontPlan {
        files,
        roots: linux_roots(env),
        names,
    }
}

/// Font files of macOS (`/System/Library/Fonts`, its `Supplemental` folder and
/// `/Library/Fonts`) for `script`, best first.
fn macos_files(script: Script, wanted: ScriptSet) -> Vec<(&'static str, u32)> {
    let kana = wanted.contains(Script::Kana);
    let arial_unicode = [
        ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0),
        ("/Library/Fonts/Arial Unicode.ttf", 0),
    ];
    let own: Vec<(&'static str, u32)> = match script {
        Script::Han | Script::Kana => {
            let japanese = [
                ("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", 0),
                ("/System/Library/Fonts/Hiragino Sans W3.ttc", 0),
            ];
            let chinese = [
                ("/System/Library/Fonts/PingFang.ttc", 0),
                ("/System/Library/Fonts/STHeiti Light.ttc", 0),
                ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
                ("/System/Library/Fonts/Supplemental/Songti.ttc", 0),
            ];
            if script == Script::Kana || kana {
                [&japanese[..], &chinese[..]].concat()
            } else {
                [&chinese[..], &japanese[..]].concat()
            }
        }
        Script::Hangul => vec![
            ("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0),
            ("/System/Library/Fonts/Supplemental/AppleGothic.ttf", 0),
            ("/System/Library/Fonts/Supplemental/AppleMyungjo.ttf", 0),
        ],
        Script::Thai => vec![
            ("/System/Library/Fonts/Supplemental/Ayuthaya.ttf", 0),
            ("/System/Library/Fonts/Thonburi.ttc", 0),
            ("/System/Library/Fonts/Supplemental/Thonburi.ttc", 0),
            ("/System/Library/Fonts/Supplemental/Sathu.ttf", 0),
        ],
        Script::Hebrew => vec![
            ("/System/Library/Fonts/ArialHB.ttc", 0),
            ("/System/Library/Fonts/Supplemental/ArialHB.ttc", 0),
            ("/System/Library/Fonts/Supplemental/Arial Hebrew.ttc", 0),
        ],
        Script::Arabic => vec![
            ("/System/Library/Fonts/GeezaPro.ttc", 0),
            ("/System/Library/Fonts/Supplemental/GeezaPro.ttc", 0),
            ("/System/Library/Fonts/SFArabic.ttf", 0),
            ("/System/Library/Fonts/Supplemental/Al Nile.ttc", 0),
            ("/System/Library/Fonts/Supplemental/DecoTypeNaskh.ttc", 0),
        ],
        Script::Devanagari => vec![
            ("/System/Library/Fonts/Supplemental/DevanagariMT.ttc", 0),
            (
                "/System/Library/Fonts/Supplemental/Devanagari Sangam MN.ttc",
                0,
            ),
            ("/System/Library/Fonts/Kohinoor.ttc", 0),
        ],
        Script::Bengali => vec![
            ("/System/Library/Fonts/Supplemental/Bangla MN.ttc", 0),
            ("/System/Library/Fonts/Supplemental/Bangla Sangam MN.ttc", 0),
        ],
        Script::Tamil => vec![
            ("/System/Library/Fonts/Supplemental/Tamil MN.ttc", 0),
            ("/System/Library/Fonts/Supplemental/Tamil Sangam MN.ttc", 0),
        ],
        Script::Gurmukhi => vec![
            ("/System/Library/Fonts/Supplemental/Gurmukhi MN.ttc", 0),
            (
                "/System/Library/Fonts/Supplemental/Gurmukhi Sangam MN.ttc",
                0,
            ),
        ],
        Script::Gujarati => vec![
            ("/System/Library/Fonts/Supplemental/Gujarati MT.ttc", 0),
            (
                "/System/Library/Fonts/Supplemental/Gujarati Sangam MN.ttc",
                0,
            ),
        ],
        Script::Oriya => vec![
            ("/System/Library/Fonts/Supplemental/Oriya MN.ttc", 0),
            ("/System/Library/Fonts/Supplemental/Oriya Sangam MN.ttc", 0),
        ],
        Script::Telugu => vec![
            ("/System/Library/Fonts/Supplemental/Telugu MN.ttc", 0),
            ("/System/Library/Fonts/Supplemental/Telugu Sangam MN.ttc", 0),
        ],
        Script::Kannada => vec![
            ("/System/Library/Fonts/Supplemental/Kannada MN.ttc", 0),
            (
                "/System/Library/Fonts/Supplemental/Kannada Sangam MN.ttc",
                0,
            ),
        ],
        Script::Malayalam => vec![
            ("/System/Library/Fonts/Supplemental/Malayalam MN.ttc", 0),
            (
                "/System/Library/Fonts/Supplemental/Malayalam Sangam MN.ttc",
                0,
            ),
        ],
        Script::Sinhala => vec![
            ("/System/Library/Fonts/Supplemental/Sinhala MN.ttc", 0),
            (
                "/System/Library/Fonts/Supplemental/Sinhala Sangam MN.ttc",
                0,
            ),
        ],
    };
    // Arial Unicode (installed with Office) draws all of them, as a last resort.
    [own, arial_unicode.to_vec()].concat()
}

/// The places to look for a font that draws `script`, best first. `wanted` is
/// everything being loaded together: Chinese characters next to kana are
/// Japanese, so the Japanese font comes first then. Pure: nothing is read.
pub fn font_plan(
    platform: FontPlatform,
    script: Script,
    wanted: ScriptSet,
    env: EnvLookup,
) -> FontPlan {
    match platform {
        FontPlatform::Windows => {
            let dir = windows_fonts_dir(env);
            FontPlan {
                files: windows_files(script, wanted)
                    .into_iter()
                    .map(|name| FontCandidate {
                        path: dir.join(name),
                        index: 0,
                    })
                    .collect(),
                ..FontPlan::default()
            }
        }
        FontPlatform::Linux => linux_plan(script, wanted, env),
        FontPlatform::MacOs => FontPlan {
            files: macos_files(script, wanted)
                .into_iter()
                .map(|(path, index)| FontCandidate {
                    path: PathBuf::from(path),
                    index,
                })
                .collect(),
            ..FontPlan::default()
        },
        FontPlatform::Other => FontPlan::default(),
    }
}

/// Whether `bytes` is a font file the font reader will take, with face `index`:
/// a TrueType or OpenType file (`\0\1\0\0`, `OTTO`, `true`) with a table
/// directory that fits in the file, or a collection (`ttcf`) that has that face.
/// Anything else is refused, including the old `typ1` type and web fonts: the
/// reader of the toolkit would reject them and panics on a font it cannot read.
pub fn font_container_ok(bytes: &[u8], index: u32) -> bool {
    fn be16(bytes: &[u8], at: usize) -> Option<usize> {
        let part = bytes.get(at..at.checked_add(2)?)?;
        Some(usize::from(u16::from_be_bytes([part[0], part[1]])))
    }
    fn be32(bytes: &[u8], at: usize) -> Option<usize> {
        let part = bytes.get(at..at.checked_add(4)?)?;
        usize::try_from(u32::from_be_bytes([part[0], part[1], part[2], part[3]])).ok()
    }
    /// A table directory at `start`: a known tag, at least one table, and every
    /// table record inside the file.
    fn directory_ok(bytes: &[u8], start: usize) -> bool {
        let Some(tag) = start.checked_add(4).and_then(|end| bytes.get(start..end)) else {
            return false;
        };
        if !(tag == [0, 1, 0, 0] || tag == *b"OTTO" || tag == *b"true") {
            return false;
        }
        let Some(tables) = start.checked_add(4).and_then(|at| be16(bytes, at)) else {
            return false;
        };
        let end = tables
            .checked_mul(16)
            .and_then(|size| size.checked_add(12))
            .and_then(|size| size.checked_add(start));
        tables >= 1 && end.is_some_and(|end| end <= bytes.len())
    }
    match bytes.get(..4) {
        Some(b"ttcf") => {
            let Some(faces) = be32(bytes, 8) else {
                return false;
            };
            // The list of face offsets must fit in the file, as the reader needs.
            let list_end = faces.checked_mul(4).and_then(|size| size.checked_add(12));
            if faces == 0 || list_end.is_none_or(|end| end > bytes.len()) {
                return false;
            }
            let Some(wanted) = usize::try_from(index).ok().filter(|wanted| *wanted < faces) else {
                return false;
            };
            wanted
                .checked_mul(4)
                .and_then(|offset| offset.checked_add(12))
                .and_then(|at| be32(bytes, at))
                .is_some_and(|start| directory_ok(bytes, start))
        }
        Some(_) => index == 0 && directory_ok(bytes, 0),
        None => false,
    }
}

/// Whether the font (face `index` of `bytes`, which `font_container_ok` accepted)
/// maps `c` to a glyph other than the missing-glyph one, read from its `cmap`
/// table (subtable formats 4 and 12, the ones fonts use for Unicode). Every read
/// is bounds-checked; a font that cannot be read answers `false`.
pub fn font_has_char(bytes: &[u8], index: u32, c: char) -> bool {
    fn be16(bytes: &[u8], at: usize) -> Option<u32> {
        let part = bytes.get(at..at.checked_add(2)?)?;
        Some(u32::from(u16::from_be_bytes([part[0], part[1]])))
    }
    fn be32(bytes: &[u8], at: usize) -> Option<u32> {
        let part = bytes.get(at..at.checked_add(4)?)?;
        Some(u32::from_be_bytes([part[0], part[1], part[2], part[3]]))
    }
    fn at(base: usize, offset: u32) -> Option<usize> {
        base.checked_add(usize::try_from(offset).ok()?)
    }
    /// Format 4: segments of the Basic Multilingual Plane.
    fn format4(bytes: &[u8], table: usize, code: u32) -> Option<bool> {
        let code = u16::try_from(code).ok()?;
        let segments = usize::try_from(be16(bytes, table.checked_add(6)?)? / 2).ok()?;
        let ends = table.checked_add(14)?;
        let starts = ends.checked_add(segments.checked_mul(2)?)?.checked_add(2)?;
        let deltas = starts.checked_add(segments.checked_mul(2)?)?;
        let ranges = deltas.checked_add(segments.checked_mul(2)?)?;
        // The first segment whose end is not before the code.
        let (mut low, mut high) = (0usize, segments);
        while low < high {
            let middle = low + (high - low) / 2;
            if be16(bytes, ends.checked_add(middle * 2)?)? < u32::from(code) {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        if low >= segments || be16(bytes, starts.checked_add(low * 2)?)? > u32::from(code) {
            return Some(false);
        }
        let delta = be16(bytes, deltas.checked_add(low * 2)?)?;
        let range_at = ranges.checked_add(low * 2)?;
        let range = be16(bytes, range_at)?;
        let glyph = if range == 0 {
            u32::from(code)
        } else {
            let start = be16(bytes, starts.checked_add(low * 2)?)?;
            let slot = range_at
                .checked_add(usize::try_from(range).ok()?)?
                .checked_add(
                    usize::try_from(u32::from(code) - start)
                        .ok()?
                        .checked_mul(2)?,
                )?;
            be16(bytes, slot)?
        };
        Some(glyph != 0 && (glyph + delta) & 0xFFFF != 0)
    }
    /// Format 12: groups of the whole of Unicode.
    fn format12(bytes: &[u8], table: usize, code: u32) -> Option<bool> {
        let groups = usize::try_from(be32(bytes, table.checked_add(12)?)?).ok()?;
        let first = table.checked_add(16)?;
        let (mut low, mut high) = (0usize, groups);
        while low < high {
            let middle = low + (high - low) / 2;
            let group = first.checked_add(middle.checked_mul(12)?)?;
            if be32(bytes, group.checked_add(4)?)? < code {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        if low >= groups {
            return Some(false);
        }
        let group = first.checked_add(low.checked_mul(12)?)?;
        let (start, glyph) = (be32(bytes, group)?, be32(bytes, group.checked_add(8)?)?);
        Some(start <= code && glyph.checked_add(code - start)? != 0)
    }
    if !font_container_ok(bytes, index) {
        return false;
    }
    let search = || -> Option<bool> {
        let directory = if bytes.starts_with(b"ttcf") {
            let list = usize::try_from(index)
                .ok()?
                .checked_mul(4)?
                .checked_add(12)?;
            usize::try_from(be32(bytes, list)?).ok()?
        } else {
            0
        };
        let tables = usize::try_from(be16(bytes, directory.checked_add(4)?)?).ok()?;
        let mut cmap = None;
        for table in 0..tables {
            let record = directory
                .checked_add(12)?
                .checked_add(table.checked_mul(16)?)?;
            if bytes.get(record..record.checked_add(4)?)? == b"cmap" {
                cmap = Some(at(0, be32(bytes, record.checked_add(8)?)?)?);
                break;
            }
        }
        let cmap = cmap?;
        let code = u32::from(c);
        let subtables = usize::try_from(be16(bytes, cmap.checked_add(2)?)?).ok()?;
        for subtable in 0..subtables.min(64) {
            let record = cmap.checked_add(4)?.checked_add(subtable.checked_mul(8)?)?;
            let (platform, encoding) = (be16(bytes, record)?, be16(bytes, record.checked_add(2)?)?);
            // Unicode tables only: the Unicode platform, or Windows Unicode BMP / full.
            if !(platform == 0 || (platform == 3 && (encoding == 1 || encoding == 10))) {
                continue;
            }
            let table = at(cmap, be32(bytes, record.checked_add(4)?)?)?;
            let found = match be16(bytes, table)? {
                4 => format4(bytes, table, code),
                12 => format12(bytes, table, code),
                _ => None,
            };
            if found == Some(true) {
                return Some(true);
            }
        }
        Some(false)
    };
    search().unwrap_or(false)
}

/// A file read for the fallback loader: not empty, at most `FALLBACK_FONT_LIMIT`.
fn read_capped(path: &std::path::Path) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    let length = metadata.len();
    if !metadata.is_file() || length < 12 || length > FALLBACK_FONT_LIMIT {
        return None;
    }
    let mut bytes = Vec::with_capacity(usize::try_from(length).ok()?);
    file.take(FALLBACK_FONT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= FALLBACK_FONT_LIMIT).then_some(bytes)
}

/// Looks below `roots` for files called `names` (compared without regard to case;
/// symbolic links are followed), going at most `FONT_SCAN_DEPTH` folders down and
/// looking at no more than `FONT_SCAN_ENTRIES` entries. The result follows the
/// order of `names`.
pub fn scan_font_folders(roots: &[PathBuf], names: &[(String, u32)]) -> Vec<FontCandidate> {
    let mut found: Vec<Option<PathBuf>> = vec![None; names.len()];
    let mut stack: Vec<(PathBuf, usize)> = roots.iter().map(|root| (root.clone(), 0)).collect();
    let mut looked_at = 0usize;
    'folders: while let Some((folder, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            looked_at += 1;
            if looked_at > FONT_SCAN_ENTRIES {
                break 'folders;
            }
            let path = entry.path();
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                if depth < FONT_SCAN_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if let Some(name) = entry.file_name().to_str() {
                for (slot, (wanted, _)) in found.iter_mut().zip(names) {
                    if slot.is_none() && wanted.eq_ignore_ascii_case(name) {
                        *slot = Some(path.clone());
                    }
                }
            }
        }
    }
    names
        .iter()
        .zip(found)
        .filter_map(|((_, index), path)| {
            Some(FontCandidate {
                path: path?,
                index: *index,
            })
        })
        .collect()
}

/// A system font read for some scripts, ready to be added to the fonts.
pub struct LoadedFont {
    /// The name it is added under (unique per file).
    pub name: String,
    pub path: PathBuf,
    /// The face of a collection.
    pub index: u32,
    pub bytes: Vec<u8>,
    pub scripts: ScriptSet,
}

impl std::fmt::Debug for LoadedFont {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoadedFont")
            .field("name", &self.name)
            .field("path", &self.path)
            .field("index", &self.index)
            .field("bytes", &self.bytes.len())
            .field("scripts", &self.scripts)
            .finish()
    }
}

/// Reads a font for every script of `scripts`: the first file of its plan that
/// exists, is a valid font file (see `font_container_ok`), is not too big and
/// really draws the script (`font_has_char` on its probe letter). A
/// file that serves several scripts is read once. A script without a usable font
/// is left out, silently. Reads files: run it off the UI thread.
pub fn load_fallback_fonts(
    scripts: ScriptSet,
    platform: FontPlatform,
    env: EnvLookup,
    read: &dyn Fn(&std::path::Path) -> Option<Vec<u8>>,
) -> Vec<LoadedFont> {
    let mut fonts: Vec<LoadedFont> = Vec::new();
    for script in scripts.iter() {
        // A font read for another script serves this one if it draws it (one CJK
        // font often draws Chinese, kana and Hangul).
        if let Some(font) = fonts
            .iter_mut()
            .find(|font| font_has_char(&font.bytes, font.index, script.probe()))
        {
            font.scripts = font.scripts.with(script);
            continue;
        }
        let plan = font_plan(platform, script, scripts, env);
        let mut tried: Vec<FontCandidate> = Vec::new();
        let mut candidates = plan.files.into_iter();
        let mut searched = plan.roots.is_empty();
        loop {
            let Some(candidate) = candidates.next() else {
                if searched {
                    break;
                }
                // Not at a usual place: look through the font folders.
                searched = true;
                let found = scan_font_folders(&plan.roots, &plan.names);
                candidates = found.into_iter();
                continue;
            };
            if tried.contains(&candidate) {
                continue;
            }
            // A font that is loaded already was looked at above: it lacks this script.
            if fonts
                .iter()
                .any(|font| font.path == candidate.path && font.index == candidate.index)
            {
                continue;
            }
            let usable = read(&candidate.path).filter(|bytes| {
                font_container_ok(bytes, candidate.index)
                    && font_has_char(bytes, candidate.index, script.probe())
            });
            if let Some(bytes) = usable {
                fonts.push(LoadedFont {
                    name: format!("bs-player-fallback-{}", script.slug()),
                    path: candidate.path,
                    index: candidate.index,
                    bytes,
                    scripts: ScriptSet::of(script),
                });
                break;
            }
            tried.push(candidate);
        }
    }
    fonts
}

/// The real loader: this computer's fonts.
pub fn load_system_fonts(scripts: ScriptSet) -> Vec<LoadedFont> {
    load_fallback_fonts(
        scripts,
        FontPlatform::current(),
        &|name| std::env::var_os(name),
        &read_capped,
    )
}

/// Adds the fonts as the last fallback of the proportional, monospace and
/// semibold families, keeping every font the context has (the user's text size
/// and the semibold family are untouched). Takes effect at the start of the
/// next pass of the context, which every window of the player shares.
///
/// The font bytes are leaked on purpose: they are held for the life of the
/// process anyway, and a borrowed `'static` font is not copied again each time
/// the toolkit rebuilds its glyph cache (an owned one is, every time).
pub fn add_fallback_fonts(ctx: &Context, fonts: Vec<LoadedFont>) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    for font in fonts {
        let bytes: &'static [u8] = Box::leak(font.bytes.into_boxed_slice());
        let data = egui::FontData {
            font: std::borrow::Cow::Borrowed(bytes),
            index: font.index,
            tweak: egui::FontTweak::default(),
        };
        let families = [
            FontFamily::Proportional,
            FontFamily::Monospace,
            FontFamily::Name(SEMIBOLD_FAMILY.into()),
        ]
        .into_iter()
        .map(|family| InsertFontFamily {
            family,
            priority: FontPriority::Lowest,
        })
        .collect();
        ctx.add_font(FontInsert::new(&font.name, data, families));
    }
    ctx.request_repaint();
}

/// The scripts of `scripts` that no font of the context draws (checked with one
/// letter each). Needs the first frame of the context to have run.
pub fn scripts_without_glyphs(ctx: &Context, scripts: ScriptSet) -> ScriptSet {
    let font = FontId::proportional(14.0);
    ctx.fonts_mut(|fonts| {
        scripts
            .iter()
            .filter(|script| !fonts.has_glyph(&font, script.probe()))
            .fold(ScriptSet::EMPTY, |set, script| set.with(script))
    })
}

/// What the player knows about the scripts its text needs (memory only).
#[derive(Debug, Default)]
pub struct FontFallbacks {
    /// Every script the library, the stations and the playlists have used so
    /// far. It only grows.
    needed: ScriptSet,
    /// Scripts that need nothing more: a font was asked for, or one is there.
    settled: ScriptSet,
    /// Scripts a worker is reading fonts for.
    loading: ScriptSet,
    /// The library revision that was last looked through.
    library_seen: Option<u64>,
    last_check: Option<std::time::Instant>,
    /// How many tracks the scans have looked at in all.
    pub tracks_scanned: u64,
}

impl FontFallbacks {
    /// Whether it is time to look at the text again (about once a second).
    pub fn due(&self, now: std::time::Instant) -> bool {
        self.last_check
            .is_none_or(|last| now.saturating_duration_since(last) >= FONT_CHECK_EVERY)
    }
    pub fn checked(&mut self, now: std::time::Instant) {
        self.last_check = Some(now);
    }
    /// Looks through the library when it has a revision not looked through yet.
    /// Returns whether it did.
    pub fn scan_library(&mut self, revision: u64, tracks: &[Track]) -> bool {
        if self.library_seen == Some(revision) {
            return false;
        }
        self.library_seen = Some(revision);
        let everything = ScriptSet::all();
        for track in tracks {
            if self.needed == everything {
                break;
            }
            self.tracks_scanned += 1;
            self.needed = self.needed.union(scripts_in_track(track));
        }
        true
    }
    pub fn note_text(&mut self, text: &str) {
        self.needed = self.needed.union(scripts_needed(text));
    }
    /// What still needs a font; nothing while a worker is loading.
    pub fn wanted(&self) -> ScriptSet {
        if self.loading.is_empty() {
            self.needed.minus(self.settled)
        } else {
            ScriptSet::EMPTY
        }
    }
    /// These scripts are drawn already: they are never looked at again.
    pub fn settle(&mut self, scripts: ScriptSet) {
        self.settled = self.settled.union(scripts);
    }
    /// A worker starts reading fonts for these: they are never asked for again.
    pub fn start(&mut self, scripts: ScriptSet) {
        self.settled = self.settled.union(scripts);
        self.loading = scripts;
    }
    pub fn finished(&mut self) {
        self.loading = ScriptSet::EMPTY;
    }


    /// The worker's answer will not come (the session it belonged to is gone):
    /// its scripts may be asked for again.
    pub fn abandon(&mut self) {
        self.settled = self.settled.minus(self.loading);
        self.loading = ScriptSet::EMPTY;
    }
}
