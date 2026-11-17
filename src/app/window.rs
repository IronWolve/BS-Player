use super::*;

impl PlayerApp {
    pub(super) fn request_quit(&mut self, ctx: &egui::Context) {
        self.tray_quitting = true;
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
    }

    pub(super) fn recover_window_position(&mut self, frame: &eframe::Frame) {
        if !std::mem::take(&mut self.window_recovery_pending) {
            return;
        }
        let Some(window) = frame.winit_window() else {
            return;
        };
        let Ok(position) = window.outer_position() else {
            return;
        };
        let size = window.outer_size();
        let rect = egui::Rect::from_min_size(
            egui::pos2(position.x as f32, position.y as f32),
            egui::vec2(size.width as f32, size.height as f32),
        );
        let monitors: Vec<_> = window
            .available_monitors()
            .map(|monitor| {
                let origin = monitor.position();
                let extent = monitor.size();
                egui::Rect::from_min_size(
                    egui::pos2(origin.x as f32, origin.y as f32),
                    egui::vec2(extent.width as f32, extent.height as f32),
                )
            })
            .collect();
        if let Some(origin) = reachable_window_origin(rect, &monitors, window.scale_factor() as f32)
        {
            // Keep native physical coordinates: conversion through the player's
            // zoom or a removed monitor's DPI would introduce a second offset.
            let mut recovered = position;
            recovered.x = origin.x.round() as i32;
            recovered.y = origin.y.round() as i32;
            window.set_outer_position(recovered);
        }
    }

    /// The player's window level; the Options, Equalizer and About windows use it
    /// too so that they are not hidden behind an always-on-top player.
    pub(super) fn window_level(&self) -> egui::WindowLevel {
        if self.state.settings.always_on_top {
            egui::WindowLevel::AlwaysOnTop
        } else {
            egui::WindowLevel::Normal
        }
    }

    pub(super) fn set_secondary_visibility(&self, ctx: &egui::Context, visible: bool) {
        for (open, id) in [
            (self.options.is_some(), "bs-player-options"),
            (self.equalizer.is_some(), "bs-player-equalizer"),
            (self.about, "bs-player-about"),
        ] {
            if open {
                ctx.send_viewport_cmd_to(
                    egui::ViewportId::from_hash_of(id),
                    egui::ViewportCommand::Visible(visible),
                );
            }
        }
    }

    pub(super) fn hide_to_tray(&mut self, ctx: &egui::Context) {
        if !self.tray_available || !self.state.settings.tray_enabled {
            return;
        }
        self.save();
        self.tray_hidden = true;
        self.set_secondary_visibility(ctx, false);
        // Minimised as well as hidden: egui then sees the player as not visible and
        // runs only `logic` for it, not full UI passes, while it sits in the tray.
        ctx.send_viewport_cmd_to(
            egui::ViewportId::ROOT,
            egui::ViewportCommand::Minimized(true),
        );
        ctx.send_viewport_cmd_to(
            egui::ViewportId::ROOT,
            egui::ViewportCommand::Visible(false),
        );
    }

    pub(super) fn restore_from_tray(&mut self, ctx: &egui::Context, request_focus: bool) {
        self.window_recovery_pending |= request_focus;
        self.tray_restore_guard.requested();
        self.tray_hidden = false;
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd_to(
            egui::ViewportId::ROOT,
            egui::ViewportCommand::Minimized(false),
        );
        self.set_secondary_visibility(ctx, true);
        if request_focus {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
        }
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }

    pub(super) fn minimize_window(&mut self, ctx: &egui::Context) {
        if self.state.settings.minimize_to_tray && self.tray_available {
            self.hide_to_tray(ctx);
        } else {
            ctx.send_viewport_cmd_to(
                egui::ViewportId::ROOT,
                egui::ViewportCommand::Minimized(true),
            );
        }
    }

    pub(super) fn dismiss_tray_popup(&mut self, ctx: &egui::Context) {
        self.tray_popup_open = false;
        {
            let mut shared = self.tray_popup_shared.lock().unwrap();
            shared.seen_focus = false;
            shared.placed = None;
        }
        ctx.send_viewport_cmd_to(
            egui::ViewportId::from_hash_of("bs-player-tray-popup"),
            egui::ViewportCommand::Visible(false),
        );
    }

    pub(super) fn tray_action(&mut self, ctx: &egui::Context, action: crate::tray_popup::TrayAction) {
        use crate::tray_popup::{SleepChoice, TrayAction};
        if self.restore_pending && !matches!(&action, TrayAction::ShowPlayer | TrayAction::Quit) {
            return;
        }
        self.dismiss_tray_popup(ctx);
        match action {
            TrayAction::Play => {
                if !self.playback_intent() {
                    self.toggle_play();
                }
            }
            TrayAction::Pause => {
                if self.state.current_station.is_some() {
                    // Pause drops the connection; Play connects again, live.
                    if self.station_intent() {
                        self.toggle_station();
                    }
                    return;
                }
                self.cancel_validation();
                self.audio.pause();
                self.state.was_playing = false;
                self.save();
            }
            TrayAction::SkipBack => self.seek(self.position() - self.skip_back_seconds()),
            TrayAction::SkipForward => {
                self.seek(self.position() + self.skip_forward_seconds())
            }
            TrayAction::Next => self.next(false, false),
            TrayAction::SetSpeed(_) if self.state.current_station.is_some() => {
                self.set_brief_status(LIVE_UNAVAILABLE);
            }
            TrayAction::SetSpeed(speed) => {
                self.state.speed = speed.clamp(0.5, 3.);
                self.audio.speed(self.state.speed);
                if self.state.settings.remember_book_speed {
                    if let Some(track) = self.state.current_track().filter(|track| track.audiobook)
                    {
                        self.state
                            .book_speeds
                            .insert(track.book_key(), self.state.speed);
                    }
                }
                self.save();
            }
            TrayAction::SetSleep(choice) => {
                self.state.sleep_deadline = None;
                self.state.sleep_mode = match choice {
                    SleepChoice::Off => "off",
                    SleepChoice::Minutes(minutes) => {
                        self.state.sleep_deadline = Some(state::now() + minutes * 60);
                        "duration"
                    }
                    SleepChoice::EndOfChapter => "chapter",
                    SleepChoice::EndOfBook => "book",
                }
                .into();
                self.apply_volume();
                self.save();
            }
            TrayAction::ExtendSleep => self.extend_sleep(),
            TrayAction::ShowPlayer => self.restore_from_tray(ctx, true),
            TrayAction::OpenOptions => {
                self.restore_from_tray(ctx, true);
                self.open_options(ctx);
            }
            TrayAction::OpenEqualizer => {
                self.restore_from_tray(ctx, true);
                self.open_equalizer(ctx);
            }
            TrayAction::Quit => self.request_quit(ctx),
        }
    }

    pub(super) fn tray_tick(&mut self, ctx: &egui::Context) {
        let (close_requested, minimization) = ctx.input_for(egui::ViewportId::ROOT, |input| {
            (
                input.viewport().close_requested(),
                input.viewport().minimized,
            )
        });
        // Observe before handling this tick's restore requests. Their queued
        // Minimized(false) command is acknowledged by a subsequent snapshot.
        self.tray_restore_guard.observe(minimization);
        let minimized = minimization.unwrap_or(false);
        if self.tray_retiring && self.tray.as_mut().is_none_or(|tray| tray.poll_shutdown()) {
            self.tray = None;
            self.tray_retiring = false;
        }
        if self.state.settings.tray_enabled && self.tray.is_none() {
            self.tray_reason = "Starting system tray…".into();
            self.tray = Some(crate::tray::TrayHandle::new(ctx.clone()));
            self.tray_tooltip_key = None;
        } else if !self.state.settings.tray_enabled {
            self.start_hidden_pending = false;
            if self.tray_hidden {
                self.restore_from_tray(ctx, false);
            }
            if self.tray_popup_open {
                self.dismiss_tray_popup(ctx);
            }
            if let Some(tray) = self.tray.as_mut() {
                tray.request_shutdown();
                self.tray_retiring = true;
            }
            self.tray_available = false;
            self.tray_reason = "System tray is disabled.".into();
        }
        if self.tray_retiring {
            self.tray_available = false;
            if self.state.settings.tray_enabled {
                self.tray_reason =
                    "Waiting for the previous tray backend to finish shutting down…".into();
            }
        }
        let events = if self.tray_retiring {
            Vec::new()
        } else {
            self.tray
                .as_mut()
                .map(|tray| tray.poll_events())
                .unwrap_or_default()
        };
        for event in events {
            match event {
                crate::tray::TrayEvent::Restore => {
                    self.dismiss_tray_popup(ctx);
                    self.restore_from_tray(ctx, true);
                }
                crate::tray::TrayEvent::OpenMenu {
                    position,
                    work_area,
                } => {
                    let id = egui::ViewportId::from_hash_of("bs-player-tray-popup");
                    let zoom = ctx.zoom_factor();
                    let native_scale = ctx.input(|input| {
                        input
                            .raw
                            .viewports
                            .get(&id)
                            .and_then(|view| view.native_pixels_per_point)
                    });
                    let scale = if cfg!(target_os = "windows") {
                        native_scale
                            .map(|value| value * zoom)
                            .unwrap_or_else(|| ctx.pixels_per_point())
                    } else {
                        1.
                    };
                    let anchor = TrayMenuAnchor {
                        position: egui::pos2(position[0], position[1]),
                        bounds: work_area.map(|[left, top, right, bottom]| {
                            egui::Rect::from_min_max(
                                egui::pos2(left, top),
                                egui::pos2(right, bottom),
                            )
                        }),
                        physical_pixels: cfg!(target_os = "windows"),
                    };
                    self.tray_popup_position = tray_menu_position(
                        anchor,
                        Vec2::from(crate::tray_popup::DEFAULT_INNER_SIZE),
                        scale,
                    );
                    self.tray_popup_open = true;
                    {
                        let mut shared = self.tray_popup_shared.lock().unwrap();
                        shared.anchor = Some(anchor);
                        shared.seen_focus = false;
                        shared.placed = None;
                    }
                    let id = egui::ViewportId::from_hash_of("bs-player-tray-popup");
                    ctx.send_viewport_cmd_to(
                        id,
                        egui::ViewportCommand::OuterPosition(self.tray_popup_position),
                    );
                    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd_to(id, egui::ViewportCommand::Focus);
                    ctx.request_repaint_of(id);
                }
                crate::tray::TrayEvent::AvailabilityChanged { available, reason } => {
                    self.tray_available = available;
                    self.tray_reason = reason;
                    // The first report decides: a working tray lets the window
                    // start hidden, a failed one leaves it on screen.
                    let start_hidden = std::mem::take(&mut self.start_hidden_pending)
                        && self.started.elapsed() < START_HIDDEN_WINDOW;
                    if !available {
                        self.dismiss_tray_popup(ctx);
                        if self.tray_hidden {
                            self.restore_from_tray(ctx, false);
                        }
                    } else if start_hidden && !self.tray_hidden {
                        self.hide_to_tray(ctx);
                    }
                }
            }
        }
        while let Ok(event) = self.tray_popup_rx.try_recv() {
            match event {
                TrayPopupEvent::Action(action) => self.tray_action(ctx, action),
                TrayPopupEvent::Dismiss => self.dismiss_tray_popup(ctx),
            }
        }
        if close_requested
            && !self.tray_quitting
            && self.state.settings.close_to_tray
            && self.tray_available
        {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::CancelClose);
            self.hide_to_tray(ctx);
        } else if self
            .tray_restore_guard
            .should_hide(minimized, self.tray_quitting)
            && !self.tray_hidden
            && self.state.settings.minimize_to_tray
            && self.tray_available
        {
            self.hide_to_tray(ctx);
        }
        // Without a tray icon and with the popup closed there is nothing to show.
        if self.tray.is_none() && !self.tray_popup_open {
            return;
        }
        let position = self.position();
        let duration = self.duration();
        let playing = self.snapshot.playing;
        // The tooltip only changes with the track, the play state or the minute.
        let key = (position as u64 / 60, duration as u64 / 60, playing);
        let identity = self.tray_identity();
        let tooltip_stale = self
            .tray_tooltip_key
            .as_ref()
            .is_none_or(|(current, last)| *current != identity || *last != key);
        if tooltip_stale && !self.tray_retiring && self.tray.is_some() {
            let (title, artist) = self.tray_labels();
            let tooltip = if self.state.current_station.is_some() {
                // `Station — Title`, then the state of the stream.
                format!(
                    "BS Player\n{}\n{}",
                    self.station_window_title().unwrap_or(title),
                    if playing { "Live" } else { "Paused" },
                )
            } else if self.state.current.is_some() {
                format!(
                    "BS Player\n{title}\n{artist}\n{} · {} of {} min",
                    if playing { "Playing" } else { "Paused" },
                    key.0,
                    key.1
                )
            } else {
                "BS Player".into()
            };
            if let Some(tray) = &mut self.tray {
                tray.update_tooltip(&tooltip);
            }
            self.tray_tooltip_key = Some((identity, key));
        }
        // The popup is only drawn while it is open.
        if self.tray_popup_open {
            let (title, artist) = self.tray_labels();
            self.tray_popup_shared.lock().unwrap().snapshot = crate::tray_popup::TrayPopupState {
                title,
                artist,
                position,
                duration,
                playing: self.playback_intent(),
                can_control: self.state.current.is_some() || self.state.current_station.is_some(),
                speed: self.state.speed,
                skip_back: self.skip_back_seconds(),
                skip_forward: self.skip_forward_seconds(),
                sleep_mode: self.state.sleep_mode.clone(),
                sleep_deadline: self.state.sleep_deadline,
                font_size: self.state.settings.font_size,
                live: self.state.current_station.is_some(),
            };
            ctx.request_repaint_of(egui::ViewportId::from_hash_of("bs-player-tray-popup"));
        }
    }

    /// What the tray tooltip is about: the file's id, or the station and its
    /// live title (a new title makes a new tooltip).
    pub(super) fn tray_identity(&self) -> Option<String> {
        match &self.state.current_station {
            Some(id) => Some(format!(
                "{}\n{}",
                stream_engine_id(id),
                self.live_title_now().unwrap_or_default()
            )),
            None => self.state.current.clone(),
        }
    }

    /// Title and artist shown by the tray tooltip and popup.
    pub(super) fn tray_labels(&self) -> (String, String) {
        if let Some(label) = self.current_station_label() {
            return (label, self.station_line().0);
        }
        match self.state.current_track() {
            Some(track) => (track.display_title().to_owned(), track.artist.clone()),
            None => ("Nothing playing".into(), String::new()),
        }
    }

    pub(super) fn register_tray_popup(&self, ctx: &egui::Context) {
        // Only with a working tray: without one (disabled, no host, or a platform
        // that cannot place the popup) the window would never be opened, and on
        // some platforms a hidden viewport is still mapped.
        if !self.state.settings.tray_enabled || !self.tray_available {
            return;
        }
        // Keep this deferred native viewport alive while hidden. Logic-only
        // ticks can then show it without rendering the hidden player first.
        let shared = self.tray_popup_shared.clone();
        let tx = self.tray_popup_tx.clone();
        let root_ctx = ctx.clone();
        ctx.show_viewport_deferred(
            egui::ViewportId::from_hash_of("bs-player-tray-popup"),
            egui::ViewportBuilder::default()
                .with_title("BS Player — Tray")
                .with_decorations(false)
                .with_resizable(false)
                .with_taskbar(false)
                .with_window_level(egui::WindowLevel::AlwaysOnTop)
                .with_inner_size(crate::tray_popup::DEFAULT_INNER_SIZE)
                .with_min_inner_size(crate::tray_popup::MIN_INNER_SIZE)
                .with_position(self.tray_popup_position)
                .with_visible(self.tray_popup_open),
            move |ui, _class| {
                let (close, focused, escape) = ui.input(|input| {
                    (
                        input.viewport().close_requested(),
                        input.viewport().focused.unwrap_or(false),
                        input.key_pressed(egui::Key::Escape),
                    )
                });
                let (snapshot, anchor, lost_focus) = {
                    let mut shared = shared.lock().unwrap();
                    let lost = shared.seen_focus && !focused;
                    if focused {
                        shared.seen_focus = true;
                    }
                    if lost {
                        shared.seen_focus = false;
                    }
                    (shared.snapshot.clone(), shared.anchor, lost)
                };
                if close || lost_focus || escape {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    let _ = tx.send(TrayPopupEvent::Dismiss);
                    root_ctx.request_repaint_of(egui::ViewportId::ROOT);
                    return;
                }
                let (action, content_height) = crate::tray_popup::show_body(ui, &snapshot);
                let mut desired_size = Vec2::new(
                    crate::tray_popup::DEFAULT_INNER_SIZE[0],
                    content_height
                        .ceil()
                        .max(crate::tray_popup::MIN_INNER_SIZE[1]),
                );
                if let Some(anchor) = anchor {
                    // Convert the physical anchor using THIS popup's DPI, not
                    // the player's monitor. Re-evaluate after a monitor change.
                    let scale = if anchor.physical_pixels {
                        ui.ctx().pixels_per_point()
                    } else {
                        1.
                    };
                    let (size, origin) = ui.input(|input| {
                        (
                            input
                                .viewport()
                                .inner_rect
                                .map(|rect| rect.size())
                                .unwrap_or(Vec2::from(crate::tray_popup::DEFAULT_INNER_SIZE)),
                            input.viewport().outer_rect.map(|rect| rect.min),
                        )
                    });

                    {
                        let max_size = anchor
                            .bounds
                            .map(|bounds| bounds.size() / scale)
                            .unwrap_or_else(|| {
                                ui.input(|input| {
                                    input
                                        .viewport()
                                        .monitor_size
                                        .map(|size| size - Vec2::splat(16.))
                                        .unwrap_or(Vec2::from(
                                            crate::tray_popup::DEFAULT_INNER_SIZE,
                                        ))
                                })
                            })
                            .max(Vec2::new(1., 1.));
                        let clamped = desired_size.min(max_size);
                        if clamped != desired_size {
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::MinInnerSize(
                                    Vec2::from(crate::tray_popup::MIN_INNER_SIZE).min(max_size),
                                ));
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::InnerSize(clamped));
                            desired_size = clamped;
                        }
                    }
                    if (size - desired_size).abs().max_elem() > 0.5 {
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::InnerSize(desired_size));
                    }
                    let target = tray_menu_position(anchor, desired_size, scale);
                    if origin.is_none_or(|origin| origin.distance(target) > 0.5) {
                        // Send each target once; a window that does not report its
                        // position (or is held elsewhere) must not be told every frame.
                        let mut shared = shared.lock().unwrap();
                        if shared.placed != Some(target) {
                            shared.placed = Some(target);
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::OuterPosition(target));
                        }
                    }
                }
                if let Some(action) = action {
                    let _ = tx.send(TrayPopupEvent::Action(action));
                    root_ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            },
        );
    }

    pub(super) fn resize_frame(&self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let maximized = ctx.input(|input| {
            input
                .viewport()
                .maximized
                .unwrap_or(self.state.window.maximized)
        });
        if maximized || self.state.fullscreen {
            return;
        }
        let rect = ctx.content_rect();
        let edge = 4.;
        let corner = 10.;
        use egui::{CursorIcon as C, ResizeDirection as D};
        let zones = [
            (
                egui::Rect::from_min_max(rect.left_top(), rect.left_top() + Vec2::splat(corner)),
                D::NorthWest,
                C::ResizeNwSe,
            ),
            (
                egui::Rect::from_min_max(
                    rect.right_top() - Vec2::new(corner, 0.),
                    rect.right_top() + Vec2::new(0., corner),
                ),
                D::NorthEast,
                C::ResizeNeSw,
            ),
            (
                egui::Rect::from_min_max(
                    rect.left_bottom() - Vec2::new(0., corner),
                    rect.left_bottom() + Vec2::new(corner, 0.),
                ),
                D::SouthWest,
                C::ResizeNeSw,
            ),
            (
                egui::Rect::from_min_max(
                    rect.right_bottom() - Vec2::splat(corner),
                    rect.right_bottom(),
                ),
                D::SouthEast,
                C::ResizeNwSe,
            ),
            (
                egui::Rect::from_min_max(
                    rect.left_top() + Vec2::new(corner, 0.),
                    rect.right_top() + Vec2::new(-corner, edge),
                ),
                D::North,
                C::ResizeVertical,
            ),
            (
                egui::Rect::from_min_max(
                    rect.left_bottom() + Vec2::new(corner, -edge),
                    rect.right_bottom() - Vec2::new(corner, 0.),
                ),
                D::South,
                C::ResizeVertical,
            ),
            (
                egui::Rect::from_min_max(
                    rect.left_top() + Vec2::new(0., corner),
                    rect.left_bottom() + Vec2::new(edge, -corner),
                ),
                D::West,
                C::ResizeHorizontal,
            ),
            (
                egui::Rect::from_min_max(
                    rect.right_top() + Vec2::new(-edge, corner),
                    rect.right_bottom() - Vec2::new(0., corner),
                ),
                D::East,
                C::ResizeHorizontal,
            ),
        ];
        for (index, (zone, direction, cursor)) in zones.into_iter().enumerate() {
            let response = ui.interact(
                zone,
                ui.id().with(("window resize", index)),
                egui::Sense::drag(),
            );
            if response.hovered() || response.dragged() {
                ctx.set_cursor_icon(cursor);
            }
            if response.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
            }
        }
    }
}

// Geometry-only recovery for startup and an explicit tray restore. Coordinates
// are native physical pixels; valid placement on negative-origin monitors stays.
pub(super) fn reachable_window_origin(
    window: egui::Rect,
    monitors: &[egui::Rect],
    scale: f32,
) -> Option<egui::Pos2> {
    if window.width() <= 0.
        || window.height() <= 0.
        || !window.min.x.is_finite()
        || !window.min.y.is_finite()
    {
        return None;
    }
    let scale = if scale.is_finite() {
        scale.max(0.1)
    } else {
        1.
    };
    let title_height = (32. * scale).min(window.height());
    let title = egui::Rect::from_min_size(window.min, egui::vec2(window.width(), title_height));
    let valid = |monitor: &&egui::Rect| monitor.width() > 0. && monitor.height() > 0.;
    if monitors.iter().filter(valid).any(|monitor| {
        let visible = title.intersect(*monitor);
        visible.width() >= (120. * scale).min(window.width()).min(monitor.width())
            && visible.height() >= (16. * scale).min(title_height).min(monitor.height())
    }) {
        return None;
    }
    let center = window.center();
    let distance = |monitor: &&egui::Rect| {
        let x = center.x.clamp(monitor.left(), monitor.right());
        let y = center.y.clamp(monitor.top(), monitor.bottom());
        center.distance_sq(egui::pos2(x, y))
    };
    let monitor = monitors
        .iter()
        .filter(valid)
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))?;
    let margin = (24. * scale)
        .min(monitor.width() / 4.)
        .min(monitor.height() / 4.);
    let left = monitor.left() + margin;
    let top = monitor.top() + margin;
    let right = (monitor.right() - margin - window.width()).max(left);
    let bottom = (monitor.bottom() - margin - window.height()).max(top);
    Some(egui::pos2(
        window.left().clamp(left, right),
        window.top().clamp(top, bottom),
    ))
}
