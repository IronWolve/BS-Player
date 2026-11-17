use super::*;

pub(super) fn chrome_panel<R>(
    ui: &mut egui::Ui,
    panel: egui::Panel,
    margin: egui::Margin,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    let painter = ui.painter().clone();
    let background = painter.add(egui::Shape::Noop);
    let (top, bottom) = appearance::chrome_colors(ui);
    let border = appearance::separator(ui);
    let response = panel
        .frame(egui::Frame::NONE.inner_margin(margin))
        .show(ui, contents);
    painter.set(
        background,
        appearance::gradient(response.response.rect, top, bottom),
    );
    painter.line_segment(
        [
            response.response.rect.left_bottom(),
            response.response.rect.right_bottom(),
        ],
        egui::Stroke::new(1., border),
    );
    response
}

pub(super) fn navigation(
    ui: &mut egui::Ui,
    label: &str,
    icon: Icon,
    selected: bool,
) -> egui::Response {
    let size = egui::TextStyle::Body.resolve(ui.style()).size + 4.;
    let response = appearance::navigation_indented(ui, label, selected, size + 20.);
    let rect = egui::Rect::from_center_size(
        egui::pos2(
            response.rect.left() + 10. + size / 2.,
            response.rect.center().y,
        ),
        Vec2::splat(size),
    );
    widgets::icon(
        ui.painter(),
        rect,
        icon,
        if selected {
            appearance::navigation_selected_foreground(ui)
        } else {
            ui.visuals().text_color()
        },
    );
    response
}

/// As `navigation`, with a weak second line `detail` under the label.
pub(super) fn navigation_detail(
    ui: &mut egui::Ui,
    label: &str,
    detail: &str,
    icon: Icon,
    selected: bool,
) -> egui::Response {
    let size = egui::TextStyle::Body.resolve(ui.style()).size + 4.;
    let response = appearance::navigation_detail(ui, label, detail, selected, size + 20.);
    let rect = egui::Rect::from_center_size(
        egui::pos2(
            response.rect.left() + 10. + size / 2.,
            response.rect.center().y,
        ),
        Vec2::splat(size),
    );
    widgets::icon(
        ui.painter(),
        rect,
        icon,
        if selected {
            appearance::navigation_selected_foreground(ui)
        } else {
            ui.visuals().text_color()
        },
    );
    response
}

// Native window coordinates use each window's monitor DPI and the application
// zoom. Convert through physical pixels so a restored dialog can move between
// monitors without being centered in the wrong coordinate system.
pub(super) fn player_center_pixels(ctx: &egui::Context) -> Option<egui::Pos2> {
    let zoom = ctx.zoom_factor();
    ctx.input(|input| {
        let player = input.raw.viewports.get(&egui::ViewportId::ROOT)?;
        let rect = player.outer_rect.or(player.inner_rect)?;
        Some(rect.center() * (player.native_pixels_per_point.unwrap_or(1.) * zoom))
    })
}

pub(super) fn dialog_placement(
    ctx: &egui::Context,
    player_center: Option<egui::Pos2>,
    monitor: Option<egui::Rect>,
    pending: bool,
    mut builder: egui::ViewportBuilder,
) -> egui::ViewportBuilder {
    if !pending {
        // None leaves native geometry unchanged. Re-sending the original size
        // would undo the initial monitor fit or a later user resize/move.
        builder.inner_size = None;
        builder.min_inner_size = None;
        builder.position = None;
        return builder;
    }
    builder = builder.with_clamp_size_to_monitor_size(true);
    let scale = ctx.pixels_per_point().max(0.1);
    let mut size = builder.inner_size.unwrap_or(Vec2::new(850., 560.));
    let area = monitor.map(|bounds| dialog_monitor_area(bounds, scale));
    if let Some(area) = area {
        // Reserve native frame space as well as a shell/edge margin. Monitor
        // bounds are physical; the builder takes application-scaled points.
        let room = (area.size() / scale - Vec2::new(16., 64.)).max(Vec2::splat(1.));
        size = size.min(room);
        builder.inner_size = Some(size);
        builder.min_inner_size = builder.min_inner_size.map(|minimum| minimum.min(size));
    }
    if let Some(center) = player_center {
        return builder.with_position(dialog_origin(center, size * scale, area) / scale);
    }
    builder
}

pub(super) fn native_monitor_rect(frame: &eframe::Frame) -> Option<egui::Rect> {
    let monitor = frame.winit_window()?.current_monitor()?;
    let origin = monitor.position();
    let size = monitor.size();
    if size.width < 2 || size.height < 2 {
        return None;
    }
    Some(egui::Rect::from_min_size(
        egui::pos2(origin.x as f32, origin.y as f32),
        Vec2::new(size.width as f32, size.height as f32),
    ))
}

pub(super) fn dialog_monitor_area(bounds: egui::Rect, scale: f32) -> egui::Rect {
    // Winit exposes full monitor bounds, not platform work-area reservations.
    // Keep a conservative edge margin; native taskbar/Dock acceptance is separate.
    bounds.shrink((48. * scale).min(bounds.width().min(bounds.height()) * 0.1))
}

pub(super) fn dialog_origin(
    center: egui::Pos2,
    size: Vec2,
    area: Option<egui::Rect>,
) -> egui::Pos2 {
    let origin = center - size * 0.5;
    if let Some(area) = area {
        egui::pos2(
            origin
                .x
                .clamp(area.left(), (area.right() - size.x).max(area.left())),
            origin
                .y
                .clamp(area.top(), (area.bottom() - size.y).max(area.top())),
        )
    } else {
        origin
    }
}

pub(super) fn center_dialog_once(
    ctx: &egui::Context,
    player_center: Option<egui::Pos2>,
    monitor: Option<egui::Rect>,
    pending: &mut bool,
) {
    if !*pending {
        return;
    }
    let Some(center) = player_center else {
        // Wayland deliberately does not expose global window positions.
        *pending = false;
        return;
    };
    let (outer, minimized, maximized) = ctx.input(|input| {
        let window = input.viewport();
        (window.outer_rect, window.minimized, window.maximized)
    });
    if maximized == Some(true) {
        *pending = false;
    } else if let Some(outer) = outer.filter(|_| minimized != Some(true)) {
        let scale = ctx.pixels_per_point().max(0.1);
        let area = monitor.map(|bounds| dialog_monitor_area(bounds, scale));
        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
            dialog_origin(center, outer.size() * scale, area) / scale,
        ));
        *pending = false;
    } else {
        ctx.request_repaint();
    }
}

pub(super) fn sidebar_section(ui: &mut egui::Ui, label: &str, collapsed: &mut Vec<String>) -> bool {
    sidebar_section_with(ui, label, collapsed, None).0
}

/// As `sidebar_section`; with `add_tip` the header also has a small plus button
/// at its right end. Returns whether the section is open, whether the plus was
/// clicked and the header's response (for a context menu).
pub(super) fn sidebar_section_with(
    ui: &mut egui::Ui,
    label: &str,
    collapsed: &mut Vec<String>,
    add_tip: Option<&str>,
) -> (bool, bool, egui::Response) {
    let mut open = !collapsed.iter().any(|section| section == label);
    let height = ui.text_style_height(&egui::TextStyle::Small) + 10.;
    let response = ui.add_sized(
        [ui.available_width(), height],
        egui::Button::new("").frame(false),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            format!("{} {label}", if open { "Collapse" } else { "Expand" }),
        )
    });
    if response.clicked() {
        open = !open;
        if open {
            collapsed.retain(|section| section != label);
        } else {
            collapsed.push(label.to_owned());
        }
    }
    let color = ui.visuals().weak_text_color();
    let center = egui::pos2(response.rect.left() + 6., response.rect.center().y);
    let points = if open {
        vec![
            center + Vec2::new(-4., -2.),
            center + Vec2::new(4., -2.),
            center + Vec2::new(0., 3.),
        ]
    } else {
        vec![
            center + Vec2::new(-2., -4.),
            center + Vec2::new(-2., 4.),
            center + Vec2::new(3., 0.),
        ]
    };
    ui.painter().add(egui::Shape::convex_polygon(
        points,
        color,
        egui::Stroke::NONE,
    ));
    ui.painter().text(
        egui::pos2(response.rect.left() + 18., response.rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        appearance::semibold(egui::TextStyle::Small.resolve(ui.style()).size),
        color,
    );
    let mut added = false;
    if let Some(tip) = add_tip {
        // Registered after the header button, so a click on the plus is its own.
        let side = response.rect.height().min(22.);
        let plus_rect = egui::Rect::from_center_size(
            egui::pos2(
                response.rect.right() - side / 2. - 2.,
                response.rect.center().y,
            ),
            Vec2::splat(side),
        );
        let plus = ui.interact(plus_rect, response.id.with("add"), egui::Sense::click());
        plus.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tip));
        let ink = if plus.hovered() {
            ui.visuals().text_color()
        } else {
            color
        };
        let arm = side * 0.22;
        let middle = plus_rect.center();
        let stroke = egui::Stroke::new(1.4, ink);
        ui.painter().line_segment(
            [middle - Vec2::new(arm, 0.), middle + Vec2::new(arm, 0.)],
            stroke,
        );
        ui.painter().line_segment(
            [middle - Vec2::new(0., arm), middle + Vec2::new(0., arm)],
            stroke,
        );
        added = plus.on_hover_text(tip).clicked();
    }
    (open, added, response)
}

pub(super) fn tray_menu_position(anchor: TrayMenuAnchor, size: Vec2, scale: f32) -> egui::Pos2 {
    let scale = scale.max(0.1);
    let scaled_size = size * scale;
    // Open to the right of the tray icon where space permits. At the desktop
    // edge, clamp against the tray monitor instead of always opening left.
    let mut point = egui::pos2(anchor.position.x, anchor.position.y - scaled_size.y);
    if let Some(bounds) = anchor.bounds {
        point.x = point.x.clamp(
            bounds.left(),
            (bounds.right() - scaled_size.x).max(bounds.left()),
        );
        point.y = point.y.clamp(
            bounds.top(),
            (bounds.bottom() - scaled_size.y).max(bounds.top()),
        );
    }
    egui::pos2(point.x / scale, point.y / scale)
}

/// The status bar's text while several rows are selected: how many and how
/// long they play together (nothing is said when no length is known).
pub(super) fn selection_status(count: usize, seconds: f64) -> String {
    if seconds > 0. {
        format!("{count} selected · {}", book_time_text(seconds))
    } else {
        format!("{count} selected")
    }
}

/// "1 file" or "N files".
pub(super) fn files_text(count: usize) -> String {
    if count == 1 {
        "1 file".to_owned()
    } else {
        format!("{count} files")
    }
}

/// A file length that can be added up: finite and not negative, else zero.
pub(super) fn known_seconds(duration: f64) -> f64 {
    if duration.is_finite() && duration > 0. {
        duration
    } else {
        0.
    }
}

/// The "Rating" submenu of a row menu: the rating picked now, 0 for "Unrated".
pub(super) fn rating_menu(ui: &mut egui::Ui, label: &str) -> Option<u8> {
    let mut chosen = None;
    ui.menu_button(label, |ui| {
        for rating in 0..=5u8 {
            let text = if rating == 0 {
                "Unrated".to_owned()
            } else {
                "★".repeat(usize::from(rating))
            };
            if ui.button(text).clicked() {
                chosen = Some(rating);
                ui.close();
            }
        }
    });
    chosen
}

/// The row to select after the rows of `removed` left the list: the first one
/// below the last of them that stays, else the last one above the first.
pub(super) fn row_after_removal(rows: &VisibleView, removed: &[String]) -> Option<usize> {
    let gone: HashSet<&str> = removed.iter().map(String::as_str).collect();
    let first = (0..rows.len()).find(|row| gone.contains(rows[*row].id.as_str()))?;
    let last = (0..rows.len())
        .rev()
        .find(|row| gone.contains(rows[*row].id.as_str()))?;
    (last + 1..rows.len())
        .find(|row| !gone.contains(rows[*row].id.as_str()))
        .or_else(|| {
            (0..first)
                .rev()
                .find(|row| !gone.contains(rows[*row].id.as_str()))
        })
}

pub(super) fn duration_summary(seconds: f64) -> String {
    let n = seconds.max(0.) as u64;
    if n >= 3600 {
        format!("{} hr {} min", n / 3600, n / 60 % 60)
    } else if n >= 60 {
        format!("{} min", n / 60)
    } else {
        time_text(seconds)
    }
}

pub(super) fn file_column_widths(
    stored: &[f32],
    shown: &[bool; COLUMN_COUNT],
    available: f32,
) -> Vec<f32> {
    let defaults = columns::default_widths();
    let mut widths = stored.to_vec();
    for index in widths.len()..COLUMN_COUNT {
        widths.push(defaults[index]);
    }
    widths.truncate(COLUMN_COUNT);
    // Auto-fit only the untouched default layout. Once a divider is dragged,
    // honor every saved width, including a title narrower than the empty space.
    if stored == defaults {
        if available >= 700. {
            // The shown columns share the width by weight. The six original
            // columns add up to exactly one, so a layout without the newer
            // columns divides the width as before; extra columns shrink the
            // shares instead of pushing columns out of view.
            let total_weight: f32 = (0..COLUMN_COUNT)
                .filter(|i| shown[*i])
                .map(|i| COLUMNS[i].fit_weight)
                .sum();
            let scale = (available - 12.) / total_weight.max(1.);
            for (index, column) in COLUMNS.iter().enumerate() {
                if index < columns::LEGACY_COUNT || shown[index] {
                    widths[index] = (column.fit_weight * scale).max(column.min_width);
                }
            }
        }
        let fixed: f32 = (0..COLUMN_COUNT)
            .filter(|i| shown[*i] && *i != columns::COL_TITLE)
            .map(|i| widths[i])
            .sum();
        widths[columns::COL_TITLE] =
            widths[columns::COL_TITLE].max((available - fixed - 25.).max(150.));
    }
    widths
}

pub(super) fn column_header(
    ui: &mut egui::Ui,
    label: &str,
    width: f32,
    height: f32,
    sorted: bool,
    descending: bool,
    sortable: bool,
    align: columns::Align,
) -> egui::Response {
    let response = ui
        .add_enabled_ui(sortable, |ui| {
            ui.add_sized([width, height], egui::Button::new("").frame(false))
        })
        .inner;
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled() && sortable,
            if sortable {
                format!("Sort by {label}")
            } else {
                format!("{label}; queue playback order")
            },
        )
    });
    let painter = ui.painter().with_clip_rect(response.rect.shrink(1.));
    let font = egui::TextStyle::Body.resolve(ui.style());
    let text = painter.layout_no_wrap(label.into(), font, ui.visuals().text_color());
    // Right-aligned headers stop short of the sort arrow, which sits at the edge.
    let left = match align {
        columns::Align::Left => response.rect.left() + 14.,
        columns::Align::Right => {
            (response.rect.right() - if sorted { 24. } else { 14. } - text.size().x)
                .max(response.rect.left() + 14.)
        }
    };
    painter.galley(
        egui::pos2(left, response.rect.center().y - text.size().y / 2.),
        text,
        ui.visuals().text_color(),
    );
    if sorted {
        let x = response.rect.right() - 10.;
        let y = response.rect.center().y;
        let direction = if descending { 1. } else { -1. };
        painter.line_segment(
            [
                egui::pos2(x - 3., y - direction * 2.),
                egui::pos2(x, y + direction * 2.),
            ],
            egui::Stroke::new(1., ui.visuals().weak_text_color()),
        );
        painter.line_segment(
            [
                egui::pos2(x, y + direction * 2.),
                egui::pos2(x + 3., y - direction * 2.),
            ],
            egui::Stroke::new(1., ui.visuals().weak_text_color()),
        );
    }
    if sortable {
        response.on_hover_text(format!("Sort by {label}"))
    } else {
        response.on_disabled_hover_text("Queue keeps playback order; drag rows to reorder")
    }
}

// Measure the same fonts and padding used by the controls before deciding which
// arrangement fits. Font size and application zoom must not rely on fixed breakpoints.
pub(super) fn label_width(ui: &egui::Ui, text: &str, style: egui::TextStyle) -> f32 {
    ui.painter()
        .layout_no_wrap(
            text.to_owned(),
            style.resolve(ui.style()),
            ui.visuals().text_color(),
        )
        .size()
        .x
}

pub(super) fn combo_width(ui: &egui::Ui, text: &str, minimum: f32) -> f32 {
    minimum.max(
        label_width(ui, text, egui::TextStyle::Button)
            + ui.spacing().icon_spacing
            + ui.spacing().icon_width
            + 2. * ui.spacing().button_padding.x,
    )
}

pub(super) fn file_header_widths(
    ui: &egui::Ui,
    tab_names: &[&str],
    selected_tab: &str,
    sort_column: usize,
    descending: bool,
) -> (f32, f32) {
    let tabs = tab_names
        .iter()
        .map(|label| {
            let body = egui::TextStyle::Body.resolve(ui.style());
            let font = if *label == selected_tab {
                appearance::semibold(body.size)
            } else {
                body
            };
            ui.painter()
                .layout_no_wrap((*label).into(), font, ui.visuals().text_color())
                .size()
                .x
                + 24.
        })
        .sum::<f32>()
        + tab_names.len().saturating_sub(1) as f32 * ui.spacing().item_spacing.x;
    let sort_label = format!(
        "{} ({})",
        COLUMNS[sort_column.min(COLUMN_COUNT - 1)].short_label,
        if descending {
            "Descending"
        } else {
            "Ascending"
        }
    );
    let tools = combo_width(ui, &sort_label, 150.) + 32. + ui.spacing().item_spacing.x;
    (tabs, tools)
}

pub(super) fn visible_column_checkbox(
    ui: &mut egui::Ui,
    settings: &mut Settings,
    index: usize,
    label: &str,
) {
    let mut mask = settings.column_mask();
    let can_toggle = !mask[index] || mask.iter().filter(|visible| **visible).count() > 1;
    let response = ui
        .add_enabled(can_toggle, egui::Checkbox::new(&mut mask[index], label))
        .on_disabled_hover_text("Keep at least one file column visible");
    if response.changed() {
        settings.set_column_visible(index, mask[index]);
    }
}

// Returns minimum (More menu), full-controls, and volume group widths.
pub(super) fn playback_extra_widths(ui: &egui::Ui, speed: &str, sleep: &str) -> (f32, f32, f32) {
    let speed_width = label_width(ui, speed, egui::TextStyle::Body) + 28.;
    let eq_width = label_width(ui, "EQ", egui::TextStyle::Body) + 46.;
    let more_width = label_width(ui, "More", egui::TextStyle::Body) + 28.;
    let sleep_width = label_width(ui, sleep, egui::TextStyle::Body) + 60.;
    let volume_width = (ui.text_style_height(&egui::TextStyle::Body) + 12.).max(34.) + 4. + 150.;
    let minimum = speed_width + eq_width + more_width + 2. * 6. + 8. + volume_width;
    let full = speed_width + eq_width + 30. + sleep_width + 3. * 6. + 8. + volume_width;
    (minimum, full, volume_width)
}

pub(super) fn prepare_player_pane(
    ctx: &egui::Context,
    name: &str,
    axis: usize,
    effective_size: f32,
) {
    let id = egui::Id::new(name);
    let mut state = egui::containers::panel::PanelState::load(ctx, id).unwrap_or(
        egui::containers::panel::PanelState {
            outer_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, Vec2::ZERO),
        },
    );
    state.outer_rect.max[axis] = state.outer_rect.min[axis] + effective_size;
    ctx.data_mut(|data| data.insert_persisted(id, state));
}

pub(super) fn player_pane_resize_active(ctx: &egui::Context, name: &str) -> bool {
    ctx.read_response(egui::Id::new(name).with("__resize"))
        .is_some_and(|response| response.dragged() || response.drag_stopped())
}

pub(super) fn reset_player_pane_layout(ctx: &egui::Context) {
    ctx.data_mut(|data| {
        for name in ["library", "artwork panel"] {
            data.remove::<egui::containers::panel::PanelState>(egui::Id::new(name));
        }
    });
}
