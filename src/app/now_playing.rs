use super::*;

impl PlayerApp {
    /// The title and book, plus a chapter selector when chapters are available.
    /// The transport reserves room for all three rows. `position` is file time.
    pub(super) fn now_playing(&mut self, ui: &mut egui::Ui, position: f64) {
        ui.add_space(2.);
        if let Some(label) = self.current_station_label() {
            // A station: its name, then the live title or what it is doing. No
            // book, chapter or remaining time.
            ui.add(
                egui::Label::new(
                    RichText::new(&label)
                        .font(appearance::semibold(self.state.settings.font_size + 2.)),
                )
                .truncate(),
            )
            .on_hover_text(&label);
            let (line, failed) = self.station_line();
            let color = if failed {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().weak_text_color()
            };
            ui.add(egui::Label::new(RichText::new(&line).small().color(color)).truncate())
                .on_hover_text(&line);
            return;
        }
        let title = self
            .state
            .current_track()
            .map(|t| t.display_title())
            .unwrap_or("Nothing playing");
        ui.add(
            egui::Label::new(
                RichText::new(title).font(appearance::semibold(self.state.settings.font_size + 2.)),
            )
            .truncate(),
        )
        .on_hover_text(title);
        let subtitle = self
            .state
            .current_track()
            .map(|t| t.book_label())
            .unwrap_or_else(|| "Choose a file to begin".into());
        ui.add(
            egui::Label::new(
                RichText::new(&subtitle)
                    .small()
                    .color(ui.visuals().weak_text_color()),
            )
            .truncate(),
        )
        .on_hover_text(subtitle);
        self.chapter_picker(ui, position);
    }

    pub(super) fn playback_controls(
        &mut self,
        ui: &mut egui::Ui,
        position: f64,
        button: f32,
        play_size: f32,
        gap: f32,
    ) {
        ui.spacing_mut().item_spacing.x = gap;
        let live = self.state.current_station.is_some();
        if widgets::icon_button(
            ui,
            Icon::Shuffle,
            "Shuffle tracks",
            self.state.shuffle,
            false,
            button,
        )
        .clicked()
        {
            self.state.shuffle = !self.state.shuffle;
            self.state.shuffle_history.clear();
        }
        let repeat = widgets::icon_button(
            ui,
            Icon::Repeat,
            &format!("Repeat: {}", self.state.repeat),
            self.state.repeat != "off",
            false,
            button,
        );
        if self.state.repeat == "track" {
            ui.painter().text(
                repeat.rect.center(),
                egui::Align2::CENTER_CENTER,
                "1",
                appearance::semibold(10.),
                ui.visuals().selection.bg_fill,
            );
        }
        if repeat.clicked() {
            self.state.repeat = match self.state.repeat.as_str() {
                "off" => "queue",
                "queue" => "track",
                _ => "off",
            }
            .into();
        }
        if widgets::icon_button(
            ui,
            Icon::Previous,
            if live {
                "Previous station"
            } else {
                "Previous track"
            },
            false,
            false,
            button,
        )
        .clicked()
        {
            self.next(true, false);
        }
        if ui
            .add_enabled_ui(!live, |ui| {
                widgets::skip_button(ui, self.skip_back_seconds() as u32, false, button)
            })
            .inner
            .on_disabled_hover_text(LIVE_UNAVAILABLE)
            .clicked()
        {
            self.seek(position - self.skip_back_seconds());
        }
        if widgets::icon_button(
            ui,
            if self.playback_intent() {
                Icon::Pause
            } else {
                Icon::Play
            },
            &match bound_key(&self.state.settings.shortcuts, "Play / pause") {
                Some(key) => format!("Play / pause ({key})"),
                None => "Play / pause".to_owned(),
            },
            false,
            true,
            play_size,
        )
        .clicked()
        {
            self.toggle_play();
        }
        if ui
            .add_enabled_ui(!live, |ui| {
                widgets::skip_button(ui, self.skip_forward_seconds() as u32, true, button)
            })
            .inner
            .on_disabled_hover_text(LIVE_UNAVAILABLE)
            .clicked()
        {
            self.seek(position + self.skip_forward_seconds());
        }
        if widgets::icon_button(
            ui,
            Icon::Next,
            if live { "Next station" } else { "Next track" },
            false,
            false,
            button,
        )
        .clicked()
        {
            self.next(false, false);
        }
        if widgets::icon_button(
            ui,
            Icon::Stop,
            if live {
                "Stop and disconnect"
            } else {
                "Stop and keep position"
            },
            false,
            false,
            button,
        )
        .clicked()
        {
            self.stop_playback();
        }
    }

    /// Sends the chosen volume to the engine (silent while muted). A sleep-timer
    /// fade lowers the engine volume, so every change of the timer restores it.
    /// Voice boost was switched: tell the engine (the saved equalizer and
    /// preamp stay as they are) and note it.
    pub(super) fn apply_voice_boost(&mut self) {
        self.audio
            .dsp_settings(&settings::engine_settings(&self.state.settings));
        let note = if self.state.settings.voice_boost {
            "Voice boost on"
        } else {
            "Voice boost off"
        };
        self.set_brief_status(note);
        self.save();
    }

    pub(super) fn apply_volume(&self) {
        self.audio.volume(if self.state.muted {
            0.
        } else {
            self.state.volume
        });
    }

    /// Adds 15 minutes to the running minutes-based sleep timer, up to 12 hours
    /// from now. Without such a timer nothing happens.
    pub(super) fn extend_sleep(&mut self) {
        let Some(deadline) = self
            .state
            .sleep_deadline
            .filter(|_| self.minutes_timer_runs())
        else {
            return;
        };
        let now = state::now();
        match extended_sleep_deadline(deadline, now) {
            Some(next) => {
                self.state.sleep_deadline = Some(next);
                // A fade that had started raised the engine volume down; undo it.
                self.apply_volume();
                let left = next.saturating_sub(now).div_ceil(60);
                self.set_brief_status(format!("Sleep timer +15 min ({left} min left)"));
                self.save();
            }
            None => self.set_brief_status("The sleep timer is already at its 12 hour limit"),
        }
    }

    /// A sleep timer set in minutes is running (not "end of chapter" or book).
    pub(super) fn minutes_timer_runs(&self) -> bool {
        self.state.sleep_deadline.is_some() && self.state.sleep_mode == "duration"
    }

    pub(super) fn sleep_menu(&mut self, ui: &mut egui::Ui) {
        if self.minutes_timer_runs() {
            if ui.button("Add 15 minutes").clicked() {
                self.extend_sleep();
                ui.close();
            }
            ui.separator();
        }
        for minutes in [15, 30, 45, 60, 90] {
            if ui.button(format!("{minutes} minutes")).clicked() {
                self.state.sleep_deadline = Some(state::now() + minutes * 60);
                self.state.sleep_mode = "duration".into();
                self.apply_volume();
                ui.close();
            }
        }
        // A live stream has no chapter or book to end: only the minutes timer.
        if self.state.current_station.is_none() {
            if ui
                .button("End of current chapter")
                .on_hover_text(
                    "Stops at the end of the current chapter (or the end of the file when it has none)",
                )
                .clicked()
            {
                self.state.sleep_mode = "chapter".into();
                self.sleep_chapter_end = None;
                self.state.sleep_deadline = None;
                self.apply_volume();
                ui.close();
            }
            if ui.button("End of current book").clicked() {
                self.state.sleep_mode = "book".into();
                self.state.sleep_deadline = None;
                self.apply_volume();
                ui.close();
            }
        }
        if ui.button("Off").clicked() {
            self.state.sleep_deadline = None;
            self.state.sleep_mode = "off".into();
            self.apply_volume();
            ui.close();
        }
    }

    pub(super) fn playback_extras(&mut self, ui: &mut egui::Ui, row_height: f32) {
        // A live stream always plays at 1×.
        let live = self.state.current_station.is_some();
        let speed_label = format!("{:.2}×", if live { 1. } else { self.state.speed });
        let sleep_label = if let Some(deadline) = self.state.sleep_deadline {
            format!("{} min", deadline.saturating_sub(state::now()).div_ceil(60))
        } else if self.state.sleep_mode == "chapter" {
            "Chapter".into()
        } else if self.state.sleep_mode == "book" {
            "Book".into()
        } else {
            "Sleep".into()
        };
        let (minimum_width, full_width, volume_width) =
            playback_extra_widths(ui, &speed_label, &sleep_label);
        let width = ui.available_width().max(minimum_width);
        let (row, _) = ui.allocate_exact_size(Vec2::new(width, row_height), egui::Sense::hover());
        let control_height = (ui.text_style_height(&egui::TextStyle::Body) + 12.).max(34.);

        let left_rect =
            egui::Rect::from_min_size(row.min, Vec2::new(width - volume_width - 8., row_height));
        let volume_rect = egui::Rect::from_min_size(
            egui::pos2(row.right() - volume_width, row.top()),
            Vec2::new(volume_width, row_height),
        );
        let mut controls = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(left_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        controls.spacing_mut().item_spacing.x = 6.;
        controls.spacing_mut().interact_size.y = control_height;
        controls.set_clip_rect(left_rect.intersect(ui.clip_rect()));
        let full = width >= full_width;
        {
            let ui = &mut controls;
            let old_speed = self.state.speed;
            let speed_button = ui
                .add_enabled_ui(!live, |ui| widgets::text_menu_button(ui, &speed_label))
                .inner
                .on_hover_text(if self.state.settings.preserve_pitch {
                    "Playback speed with pitch preservation"
                } else {
                    "Playback speed also changes pitch"
                })
                .on_disabled_hover_text(LIVE_UNAVAILABLE);
            egui::Popup::menu(&speed_button).show(|ui| {
                for speed in SPEED_STEPS {
                    if ui
                        .selectable_value(&mut self.state.speed, speed, format!("{speed:.2}×"))
                        .clicked()
                    {
                        ui.close();
                    }
                }
            });
            if old_speed != self.state.speed {
                self.apply_speed();
            }
            if widgets::text_icon_button(ui, Icon::Equalizer, "EQ")
                .on_hover_text(if self.equalizer.is_some() {
                    "Close equalizer"
                } else {
                    "Open equalizer"
                })
                .clicked()
            {
                self.toggle_equalizer(ui.ctx());
            }
            if full {
                if ui
                    .add_enabled_ui(!live, |ui| {
                        widgets::icon_button(ui, Icon::Bookmark, "Add bookmark", false, false, 30.)
                    })
                    .inner
                    .on_disabled_hover_text(LIVE_UNAVAILABLE)
                    .clicked()
                {
                    self.add_bookmark();
                }
                let sleep = widgets::menu_icon_button(ui, Icon::Moon, &sleep_label);
                egui::Popup::menu(&sleep).show(|ui| self.sleep_menu(ui));
            } else {
                let more = widgets::text_menu_button(ui, "More");
                egui::Popup::menu(&more).show(|ui| {
                    if ui
                        .add_enabled(!live, egui::Button::new("Add bookmark"))
                        .on_disabled_hover_text(LIVE_UNAVAILABLE)
                        .clicked()
                    {
                        self.add_bookmark();
                        ui.close();
                    }
                    ui.menu_button("Sleep timer", |ui| self.sleep_menu(ui));
                });
            }
        }
        let mut volume_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(volume_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        volume_ui.set_clip_rect(volume_rect.intersect(ui.clip_rect()));
        volume_ui.spacing_mut().item_spacing.x = 4.;
        volume_ui.spacing_mut().interact_size.y = control_height;
        let ui = &mut volume_ui;
        if widgets::icon_button(
            ui,
            if self.state.muted {
                Icon::Muted
            } else {
                Icon::Volume
            },
            "Mute / unmute",
            self.state.muted,
            false,
            control_height,
        )
        .clicked()
        {
            self.state.muted = !self.state.muted;
            self.audio.volume(if self.state.muted {
                0.
            } else {
                self.state.volume
            });
        }
        let mut volume = self.state.volume as f64;
        if widgets::slider(ui, &mut volume, 0., 1., 150., "Volume").changed() {
            self.state.volume = volume as f32;
            self.audio.volume(if self.state.muted {
                0.
            } else {
                self.state.volume
            });
        }
    }

    pub(super) fn transport(&mut self, root: &mut egui::Ui) {
        chrome_panel(
            root,
            egui::Panel::bottom("transport"),
            egui::Margin::symmetric(16, 4),
            |ui| {
                ui.spacing_mut().item_spacing.y = 2.;
                let position = self.position();
                let duration = self.duration();
                // A drag that began on another track must not seek this one.
                if self
                    .seek_drag
                    .as_ref()
                    .is_some_and(|(id, _)| self.state.current.as_ref() != Some(id))
                {
                    self.seek_drag = None;
                }
                let shown = self.seek_drag.as_ref().map_or(position, |(_, drag)| *drag);
                // A live stream shows the time since it connected, a bar that is
                // switched off and no time left.
                let live = self.state.current_station.is_some();
                let elapsed = if live {
                    live_time_text(shown)
                } else {
                    time_text(shown)
                };
                let listening = self.state.settings.show_remaining_time;
                let speed = self.state.speed;
                let remaining = if live {
                    String::new()
                } else {
                    remaining_text(duration, shown, speed, listening)
                };
                let elapsed_width = label_width(ui, &elapsed, egui::TextStyle::Body);
                // Room for both readings, so clicking the label never moves the bar.
                let remaining_width = if live {
                    0.
                } else {
                    label_width(ui, &remaining, egui::TextStyle::Body).max(label_width(
                        ui,
                        &remaining_text(duration, shown, speed, !listening),
                        egui::TextStyle::Body,
                    ))
                };
                let remaining_tip = if live {
                    LIVE_TIME_TIP.to_owned()
                } else {
                    let mut tip = remaining_tooltip(duration, shown, speed, listening);
                    // Only while the option is in force; the file time left stays exact.
                    if let Some(skipped) = self
                        .skip_silence_wanted()
                        .then(|| skipped_note(self.skipped_seconds))
                        .flatten()
                    {
                        tip.push('\n');
                        tip.push_str(&skipped);
                    }
                    tip
                };
                let mut toggle_remaining = false;
                let seek_width = ui.available_width()
                    - elapsed_width
                    - remaining_width
                    - 2. * ui.spacing().item_spacing.x;
                // An audiobook's progress through the whole book, beside the time
                // labels, when the seek bar keeps a usable length without it.
                let chip = self
                    .book_chip(shown, speed, listening)
                    .map(|(text, tip)| {
                        // Reserved in steps, so the bar does not creep as digits change.
                        let width =
                            (label_width(ui, &text, egui::TextStyle::Small) / 24.).ceil() * 24.;
                        (text, tip, width)
                    })
                    .filter(|(_, _, width)| {
                        seek_width - width - ui.spacing().item_spacing.x >= BOOK_CHIP_MIN_SEEK
                    });
                let seek_width = match &chip {
                    Some((_, _, width)) => seek_width - width - ui.spacing().item_spacing.x,
                    None => seek_width,
                };
                let mut seek = shown;
                // A position past the known duration (an under-estimated length)
                // widens the range instead of being clamped into a seek.
                let seek_max = duration.max(position).max(1.);
                let response = if seek_width >= 40. {
                    ui.horizontal(|ui| {
                        toggle_remaining |=
                            time_label(ui, &elapsed, None, false, &remaining_tip).clicked();
                        let response = ui
                            .add_enabled_ui(!live, |ui| {
                                widgets::slider_with_marks(
                                    ui,
                                    &mut seek,
                                    0.,
                                    seek_max,
                                    seek_width,
                                    "Seek playback position",
                                    &self.chapter_marks,
                                )
                            })
                            .inner
                            .on_disabled_hover_text(LIVE_UNAVAILABLE);
                        if let Some((text, tip, width)) = &chip {
                            toggle_remaining |= book_chip_label(ui, text, *width, tip).clicked();
                        }
                        if !live {
                            toggle_remaining |= time_label(
                                ui,
                                &remaining,
                                Some(remaining_width),
                                listening,
                                &remaining_tip,
                            )
                            .clicked();
                        }
                        response
                    })
                    .inner
                } else {
                    ui.horizontal_wrapped(|ui| {
                        toggle_remaining |=
                            time_label(ui, &elapsed, None, false, &remaining_tip).clicked();
                        if !live {
                            toggle_remaining |=
                                time_label(ui, &remaining, None, listening, &remaining_tip)
                                    .clicked();
                        }
                    });
                    let width = ui.available_width();
                    ui.add_enabled_ui(!live, |ui| {
                        widgets::slider_with_marks(
                            ui,
                            &mut seek,
                            0.,
                            seek_max,
                            width,
                            "Seek playback position",
                            &self.chapter_marks,
                        )
                    })
                    .inner
                    .on_disabled_hover_text(LIVE_UNAVAILABLE)
                };
                if toggle_remaining && !live {
                    self.toggle_remaining_time();
                }
                if response.dragged() && !live {
                    self.seek_drag = self.state.current.clone().map(|id| (id, seek));
                }
                if !live && (response.drag_stopped() || response.changed() && !response.dragged()) {
                    self.seek_drag = None;
                    self.seek(seek);
                }
                ui.add_space(2.);
                let row_width = ui.available_width();
                let button = (self.state.settings.font_size + 18.).clamp(32., 38.);
                let play_size = button + 16.;
                let controls_height = (play_size + 4.).max(
                    ui.text_style_height(&egui::TextStyle::Body)
                        + ui.text_style_height(&egui::TextStyle::Small)
                        + 10.,
                );
                let title_font = appearance::semibold(self.state.settings.font_size + 2.);
                let title_height = ui.fonts_mut(|fonts| fonts.row_height(&title_font));
                let chapter_height = if !live && !self.current_chapters.is_empty() {
                    (ui.text_style_height(&egui::TextStyle::Body) + 12.).max(34.)
                        + ui.spacing().item_spacing.y
                } else {
                    0.
                };
                let information_height = 2. + title_height
                    + ui.text_style_height(&egui::TextStyle::Small)
                    + ui.spacing().item_spacing.y + chapter_height + 4.;
                let row_height = controls_height.max(information_height);
                let gap = 4.;
                // Four control slots on each side place the play button exactly
                // at the viewport centre, independent of title or volume width.
                let center_width = play_size + 8. * (button + gap);
                let side_width = ((row_width - center_width) / 2. - 10.).max(0.);
                let (extras_minimum, _, _) =
                    playback_extra_widths(ui, &format!("{:.2}×", self.state.speed), "Sleep");
                if side_width >= extras_minimum {
                    let (row, _) = ui.allocate_exact_size(
                        Vec2::new(row_width, row_height),
                        egui::Sense::hover(),
                    );
                    let title_rect =
                        egui::Rect::from_min_size(row.min, Vec2::new(side_width, row_height));
                    let center_rect = egui::Rect::from_center_size(
                        row.center(),
                        Vec2::new(center_width, row_height),
                    );
                    let extras_rect = egui::Rect::from_min_size(
                        egui::pos2(row.right() - side_width, row.top()),
                        Vec2::new(side_width, row_height),
                    );
                    let mut title_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(title_rect)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    title_ui.set_clip_rect(title_rect.intersect(ui.clip_rect()));
                    self.now_playing(&mut title_ui, position);
                    let mut controls_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(center_rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    self.playback_controls(&mut controls_ui, position, button, play_size, gap);
                    let mut extras_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(extras_rect)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    extras_ui.set_clip_rect(extras_rect.intersect(ui.clip_rect()));
                    self.playback_extras(&mut extras_ui, row_height);
                } else {
                    self.now_playing(ui, position);
                    egui::ScrollArea::horizontal()
                        .id_salt("transport controls")
                        .max_height(controls_height + 4.)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            let (row, _) = ui.allocate_exact_size(
                                Vec2::new(row_width.max(center_width), controls_height),
                                egui::Sense::hover(),
                            );
                            let rect = egui::Rect::from_center_size(
                                row.center(),
                                Vec2::new(center_width, controls_height),
                            );
                            let mut controls = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(rect)
                                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                            );
                            self.playback_controls(&mut controls, position, button, play_size, gap);
                        });
                    egui::ScrollArea::horizontal()
                        .id_salt("transport extras")
                        .max_height(controls_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| self.playback_extras(ui, controls_height));
                }
                if self.state.settings.show_status {
                    ui.add_space(3.);
                    ui.separator();
                    // The station line (its state, or why it failed) is red when it failed.
                    let mut left_failed = false;
                    let left = if let Some(note) = self.status_for_bar() {
                        if self.busy == 0 {
                            // Draw again when the note has been shown long enough.
                            ui.ctx().request_repaint_after(
                                STATUS_SHOWN.saturating_sub(self.status_age()),
                            );
                        }
                        note.to_owned()
                    } else if let Some((text, failed)) = self.station_status_text() {
                        left_failed = failed;
                        text
                    } else if self.state.section == state::STREAMS_SECTION {
                        let shown = self.shown_station_count();
                        stations_summary(shown, self.state.stations.len())
                    } else if let Some(view) = self
                        .gui_cache
                        .browse_view(&self.state, self.library_revision)
                    {
                        browse_summary(view.kind, view.len(), view.total, view.tracks)
                    } else if self.state.library.is_empty() {
                        "Ready — add files or a folder to begin".into()
                    } else {
                        let (selected, selected_seconds) = self.selection_totals();
                        if selected > 1 {
                            selection_status(selected, selected_seconds)
                        } else {
                            let tracks = self.visible_tracks();
                            let (count, duration) = (tracks.len(), tracks.total_duration);
                            format!("{count} files · {}", duration_summary(duration))
                        }
                    };
                    let right = if self.save_failed {
                        "Save failed"
                    } else if self.state.settings.private_listening || self.private_track.is_some()
                    {
                        "Private listening"
                    } else if self.state.current_station.is_some() {
                        "Live stream"
                    } else if self.state.current.is_none() {
                        "Ready"
                    } else {
                        "Position saved"
                    };
                    // A running job's note offers "Cancel", an undoable action's "Undo".
                    let offer_cancel = self.cancel_offered();
                    let offer_undo = !offer_cancel && self.undo_offered();
                    let offer_link = offer_cancel || offer_undo;
                    let mut undo_clicked = false;
                    let mut cancel_clicked = false;
                    ui.horizontal(|ui| {
                        let right_width = ui
                            .painter()
                            .layout_no_wrap(
                                right.into(),
                                egui::TextStyle::Small.resolve(ui.style()),
                                ui.visuals().weak_text_color(),
                            )
                            .size()
                            .x
                            + 30.;
                        let left_width = (ui.available_width() - right_width - 8.).max(1.);
                        ui.allocate_ui_with_layout(
                            Vec2::new(left_width, 20.),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_min_width(left_width);
                                ui.set_max_width(left_width);
                                // The clickable "Undo" or "Cancel" after the note keeps its room.
                                let link_width = if offer_link {
                                    ui.painter()
                                        .layout_no_wrap(
                                            if offer_cancel {
                                                "— Cancel".into()
                                            } else {
                                                "— Undo".into()
                                            },
                                            egui::TextStyle::Small.resolve(ui.style()),
                                            ui.visuals().weak_text_color(),
                                        )
                                        .size()
                                        .x
                                        + 3. * ui.spacing().item_spacing.x
                                } else {
                                    0.
                                };
                                ui.scope(|ui| {
                                    ui.set_max_width((left_width - link_width).max(1.));
                                    ui.add(
                                        egui::Label::new(RichText::new(&left).small().color(
                                            if left_failed {
                                                ui.visuals().error_fg_color
                                            } else {
                                                ui.visuals().weak_text_color()
                                            },
                                        ))
                                        .truncate(),
                                    )
                                    .on_hover_text(&left);
                                });
                                if offer_link {
                                    ui.label(
                                        RichText::new("—")
                                            .small()
                                            .color(ui.visuals().weak_text_color()),
                                    );
                                    if offer_cancel {
                                        let link = ui.link(RichText::new("Cancel").small()).on_hover_text(
                                            "Stop now; the files read so far are kept (Library → Cancel scan)",
                                        );
                                        cancel_clicked = link.clicked();
                                    } else {
                                        let link = ui
                                            .link(RichText::new("Undo").small())
                                            .on_hover_text("Take this back (Edit → Undo)");
                                        undo_clicked = link.clicked();
                                    }
                                }
                            },
                        );
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::new(14., 18.), egui::Sense::hover());
                        if !self.save_failed {
                            let p = ui.painter();
                            let c = ui.visuals().text_color();
                            p.line_segment(
                                [
                                    rect.left_center() + Vec2::new(1., 0.),
                                    rect.left_center() + Vec2::new(5., 4.),
                                ],
                                egui::Stroke::new(1.4, c),
                            );
                            p.line_segment(
                                [
                                    rect.left_center() + Vec2::new(5., 4.),
                                    rect.left_center() + Vec2::new(12., -4.),
                                ],
                                egui::Stroke::new(1.4, c),
                            );
                        }
                        ui.label(
                            RichText::new(right)
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        )
                        .on_hover_text(
                            if self.state.current_station.is_some() {
                                "Nothing is saved about a live stream except its last title"
                                    .to_owned()
                            } else {
                                format!(
                                    "Progress checkpoint interval: {} seconds",
                                    self.state.settings.checkpoint_seconds
                                )
                            },
                        );
                    });
                    if undo_clicked {
                        self.undo_last();
                    }
                    if cancel_clicked {
                        self.cancel_scans();
                    }
                }
            },
        );
    }

    /// Click on the time labels (or the View menu item): the right-hand label
    /// switches between the file time left and the listening time left.
    pub(super) fn toggle_remaining_time(&mut self) {
        let settings = &mut self.state.settings;
        settings.show_remaining_time = !settings.show_remaining_time;
        let note = if settings.show_remaining_time {
            format!(
                "Showing listening time left at {:.2}× speed",
                self.state.speed
            )
        } else {
            "Showing file time left".to_owned()
        };
        self.set_brief_status(note);
        self.save();
    }
}

/// Minutes the sleep timer's "Add 15 minutes" item adds, and the most it may
/// ever have left (12 hours).
pub(super) const SLEEP_STEP_SECONDS: u64 = 15 * 60;

pub(super) const SLEEP_LIMIT_SECONDS: u64 = 12 * 3600;

/// The deadline of a running sleep timer after adding 15 minutes, capped at 12
/// hours from `now`; `None` when it already is at the cap.
pub(super) fn extended_sleep_deadline(deadline: u64, now: u64) -> Option<u64> {
    let cap = now.saturating_add(SLEEP_LIMIT_SECONDS);
    let next = deadline
        .max(now)
        .saturating_add(SLEEP_STEP_SECONDS)
        .min(cap);
    (next > deadline).then_some(next)
}

/// The right-hand time label of the player bar. It shows the file time left,
/// or with `listening` the real listening time left: the file time divided by
/// the playback speed. Always a leading minus, like the existing label.
pub(super) fn remaining_text(duration: f64, shown: f64, speed: f32, listening: bool) -> String {
    format!(
        "−{}",
        time_text(time_left(duration, shown, speed, listening))
    )
}

/// Seconds left to show: finite and never negative.
pub(super) fn time_left(duration: f64, shown: f64, speed: f32, listening: bool) -> f64 {
    let left = duration - shown;
    let left = if left.is_finite() { left.max(0.) } else { 0. };
    if listening && speed.is_finite() && speed > 0.05 {
        left / f64::from(speed)
    } else {
        left
    }
}

/// "Skipped silences: 4 m 12 s" for a total of `seconds`, or nothing while it is
/// under a second (or not a number).
pub(super) fn skipped_note(seconds: f64) -> Option<String> {
    if !seconds.is_finite() || seconds < 1. {
        return None;
    }
    let total = seconds.min(1.0e9) as u64;
    let (hours, minutes, secs) = (total / 3600, total / 60 % 60, total % 60);
    let text = if hours > 0 {
        format!("{hours} h {minutes} m")
    } else if minutes > 0 {
        format!("{minutes} m {secs} s")
    } else {
        format!("{secs} s")
    };
    Some(format!("Skipped silences: {text}"))
}

pub(super) fn remaining_tooltip(duration: f64, shown: f64, speed: f32, listening: bool) -> String {
    if listening {
        format!(
            "Listening time left at {speed:.2}× speed (file time left {}). Click to show file time left.",
            time_text(time_left(duration, shown, speed, false))
        )
    } else {
        format!(
            "File time left. Click to show the listening time left at the current speed ({}).",
            time_text(time_left(duration, shown, speed, true))
        )
    }
}

/// One of the two time labels beside the seek bar; a click toggles the reading.
/// `width` reserves a fixed, right-aligned slot.
pub(super) fn time_label(
    ui: &mut egui::Ui,
    text: &str,
    width: Option<f32>,
    strong: bool,
    tip: &str,
) -> egui::Response {
    let color = if strong {
        ui.visuals().text_color()
    } else {
        ui.visuals().weak_text_color()
    };
    let label = egui::Label::new(RichText::new(text).color(color)).sense(egui::Sense::click());
    let response = match width {
        Some(width) => {
            let height = ui
                .spacing()
                .interact_size
                .y
                .max(ui.text_style_height(&egui::TextStyle::Body));
            ui.allocate_ui_with_layout(
                Vec2::new(width, height),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| ui.add(label),
            )
            .inner
        }
        None => ui.add(label),
    };
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(tip)
}

/// The seek bar keeps at least this much length when the book readout is shown.
pub(super) const BOOK_CHIP_MIN_SEEK: f32 = 200.;

/// The book readout beside the time labels: small, weak, right-aligned in a
/// slot of `width`. A click toggles the time reading like the labels do.
pub(super) fn book_chip_label(ui: &mut egui::Ui, text: &str, width: f32, tip: &str) -> egui::Response {
    let height = ui
        .spacing()
        .interact_size
        .y
        .max(ui.text_style_height(&egui::TextStyle::Body));
    let label = egui::Label::new(
        RichText::new(text)
            .small()
            .color(ui.visuals().weak_text_color()),
    )
    .sense(egui::Sense::click());
    ui.allocate_ui_with_layout(
        Vec2::new(width, height),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| ui.add(label),
    )
    .inner
    .on_hover_cursor(egui::CursorIcon::PointingHand)
    .on_hover_text(tip)
}

pub fn time_text(seconds: f64) -> String {
    let n = if seconds.is_finite() {
        seconds.max(0.) as u64
    } else {
        0
    };
    if n >= 3600 {
        format!("{}:{:02}:{:02}", n / 3600, n / 60 % 60, n % 60)
    } else {
        format!("{}:{:02}", n / 60, n % 60)
    }
}
