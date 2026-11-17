use super::*;

impl PlayerApp {
    pub(super) fn open_options(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd_to(
            egui::ViewportId::from_hash_of("bs-player-options"),
            egui::ViewportCommand::Minimized(false),
        );
        ctx.send_viewport_cmd_to(
            egui::ViewportId::from_hash_of("bs-player-options"),
            egui::ViewportCommand::Focus,
        );
        if self.options.is_some() {
            return;
        }
        self.options_center_pending = true;
        self.devices.clear();
        self.device_request = self.device_request.wrapping_add(1);
        // The registry is read when Options opens and after each action, not per frame.
        if !self.association_busy {
            self.association = winintegration::association_state();
            self.association_error = None;
        }
        let mut options = OptionsDialog::new(&self.state.settings);
        options.set_device_status(!self.silent, None);
        self.options = Some(options);
        if !self.silent {
            let request = self.device_request;
            let tx = self.job_sender();
            std::thread::spawn(move || {
                tx.send(Event::Devices {
                    request,
                    result: AudioEngine::devices(),
                });
            });
        }
    }

    pub(super) fn open_equalizer(&mut self, ctx: &egui::Context) {
        let viewport = egui::ViewportId::from_hash_of("bs-player-equalizer");
        ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd_to(viewport, egui::ViewportCommand::Focus);
        if self.equalizer.is_none() {
            self.equalizer_center_pending = true;
            self.equalizer = Some(crate::eq::EqualizerWindow::new(&self.state.settings));
        }
    }

    pub(super) fn toggle_equalizer(&mut self, ctx: &egui::Context) {
        if self.equalizer.take().is_some() {
            self.equalizer_center_pending = false;
            if self.equalizer_dirty {
                self.save();
                self.equalizer_dirty = false;
            }
        } else {
            self.open_equalizer(ctx);
        }
    }

    pub(super) fn menu_contents(&mut self, ui: &mut egui::Ui, name: &str) {
        let ctx = ui.ctx().clone();
        match name {
            "File" => {
                let shortcuts = &self.state.settings.shortcuts;
                let add_files = with_hint("Add files…", bound_key(shortcuts, "Open files"));
                let add_folder = with_hint("Add folder…", bound_key(shortcuts, "Open folder"));
                if ui.button(add_files).clicked() {
                    self.choose_files(false);
                    ui.close();
                }
                if ui.button(add_folder).clicked() {
                    self.choose_files(true);
                    ui.close();
                }
                if ui.button("Add stream URL…").clicked() {
                    self.open_station_dialog(StationDialogKind::Add);
                    ui.close();
                }
                ui.separator();
                if ui.button("Import playlist…").clicked() {
                    self.choose_files(false);
                    ui.close();
                }
                if ui.button("Export queue…").clicked() {
                    self.export_playlist();
                    ui.close();
                }
                ui.separator();
                if ui.button("Exit").clicked() {
                    self.request_quit(&ctx);
                }
            }
            "Edit" => {
                let shortcuts = &self.state.settings.shortcuts;
                let undo = with_hint(
                    &match self.undo.last_label() {
                        Some(label) => format!("Undo {label}"),
                        None => "Undo".to_owned(),
                    },
                    bound_key(shortcuts, "Undo"),
                );
                let bookmark = with_hint("Add bookmark…", bound_key(shortcuts, "Add bookmark"));
                let find = with_hint("Search files", bound_key(shortcuts, "Focus search"));
                let remove = with_hint(
                    if self.state.section == state::STREAMS_SECTION {
                        "Remove selected station…"
                    } else if self.browse_kind().is_some() {
                        "Remove selected files…"
                    } else if self.state.tab == "Queue" {
                        "Remove selected from queue"
                    } else if playlist_section(&self.state).is_some() {
                        "Remove selected from playlist"
                    } else {
                        "Remove selected from library…"
                    },
                    bound_key(shortcuts, "Remove selected"),
                );
                let can_remove = if self.state.section == state::STREAMS_SECTION {
                    self.selected_station().is_some()
                } else if self.browse_kind().is_some() {
                    // The Artists, Albums and Genres lists have no file to remove.
                    false
                } else {
                    self.state.selected.is_some()
                        && matches!(self.state.tab.as_str(), "Files" | "Queue")
                };
                if ui
                    .add_enabled(!self.undo.is_empty(), egui::Button::new(undo))
                    .clicked()
                {
                    self.undo_last();
                    ui.close();
                }
                ui.separator();
                if ui.button("File details / edit library tags…").clicked() {
                    if let Some(id) = self.state.selected.clone() {
                        self.actions.push(Action::Details(id));
                    }
                    ui.close();
                }
                if ui.add_enabled(self.state.current_track().is_some(), egui::Button::new(bookmark))
                    .on_disabled_hover_text("Choose a file to bookmark first").clicked() {
                    self.add_bookmark();
                    ui.close();
                }
                if ui.button(find).clicked() {
                    self.focus_search(&ctx);
                    ui.close();
                }
                if ui
                    .add_enabled(can_remove, egui::Button::new(remove))
                    .clicked()
                {
                    self.remove_selected();
                    ui.close();
                }
                if ui.button("Clear queue…").clicked() {
                    self.confirm = Some(Confirm::ClearQueue);
                    ui.close();
                }
            }
            "View" => {
                ui.checkbox(&mut self.state.settings.show_library, "Library pane");
                ui.checkbox(&mut self.state.settings.show_artwork, "Artwork pane");
                ui.checkbox(
                    &mut self.state.settings.show_list_art,
                    "Artwork in file list",
                );
                ui.checkbox(&mut self.state.settings.show_status, "Status bar");
                ui.checkbox(
                    &mut self.state.settings.show_remaining_time,
                    "Show remaining time",
                )
                .on_hover_text(
                    "The right-hand time shows the listening time left at the current speed",
                );
                ui.checkbox(&mut self.state.compact, "Compact player");
                if ui
                    .checkbox(&mut self.state.fullscreen, "Full screen")
                    .changed()
                {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.state.fullscreen));
                }
                if ui.button("Reset layout").clicked() {
                    self.state.sidebar_width = 260.;
                    self.state.artwork_height = 320.;
                    reset_player_pane_layout(&ctx);
                    self.state.settings.show_library = true;
                    self.state.settings.show_artwork = true;
                    self.state.settings.show_list_art = true;
                    self.state.settings.show_status = true;
                    self.state.compact = false;
                    ctx.memory_mut(|m| m.reset_areas());
                    ui.close();
                }
            }
            "Playback" => {
                // A live stream has no seeking, speed, bookmarks or A–B repeat.
                let live = self.state.current_station.is_some();
                let shortcuts = &self.state.settings.shortcuts;
                // Every hint comes from the user's current binding.
                let hint =
                    |label: &str, action: &str| with_hint(label, bound_key(shortcuts, action));
                let play_pause = hint(
                    if self.playback_intent() {
                        "Pause"
                    } else {
                        "Play"
                    },
                    "Play / pause",
                );
                let stop = hint("Stop (keep position)", "Stop");
                let previous = hint(
                    if live {
                        "Previous station"
                    } else {
                        "Previous track"
                    },
                    "Previous track",
                );
                let next = hint(
                    if live { "Next station" } else { "Next track" },
                    "Next track",
                );
                let volume_up = hint("Volume up", "Volume up");
                let volume_down = hint("Volume down", "Volume down");
                let mute = hint(if self.state.muted { "Unmute" } else { "Mute" }, "Mute");
                let faster = hint("Speed faster", "Speed faster");
                let slower = hint("Speed slower", "Speed slower");
                let reset = hint("Reset speed to 1×", "Reset speed");
                let bookmark = hint("Add bookmark…", "Add bookmark");
                let next_bookmark = hint("Next bookmark", "Next bookmark");
                let previous_bookmark = hint("Previous bookmark", "Previous bookmark");
                let next_chapter = hint("Next chapter", "Next chapter");
                let previous_chapter = hint("Previous chapter", "Previous chapter");
                let has_chapters = !self.current_chapters.is_empty();
                if ui.button(play_pause).clicked() {
                    self.toggle_play();
                    ui.close();
                }
                if ui.button(stop).clicked() {
                    self.stop_playback();
                    ui.close();
                }
                if ui.button(previous).clicked() {
                    self.next(true, false);
                    ui.close();
                }
                if ui.button(next).clicked() {
                    self.next(false, false);
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(has_chapters, egui::Button::new(next_chapter))
                    .on_disabled_hover_text("No chapters in this file")
                    .clicked()
                {
                    self.step_chapter(true);
                    ui.close();
                }
                if ui
                    .add_enabled(has_chapters, egui::Button::new(previous_chapter))
                    .on_disabled_hover_text("No chapters in this file")
                    .clicked()
                {
                    self.step_chapter(false);
                    ui.close();
                }
                ui.separator();
                if ui.button(volume_up).clicked() {
                    self.nudge_volume(true);
                    ui.close();
                }
                if ui.button(volume_down).clicked() {
                    self.nudge_volume(false);
                    ui.close();
                }
                if ui.button(mute).clicked() {
                    self.toggle_mute();
                    ui.close();
                }
                ui.separator();
                if live_off_item(ui, live, faster).clicked() {
                    self.nudge_speed(true);
                    ui.close();
                }
                if live_off_item(ui, live, slower).clicked() {
                    self.nudge_speed(false);
                    ui.close();
                }
                if live_off_item(ui, live, reset).clicked() {
                    self.set_speed(1.);
                    ui.close();
                }
                if live_off_item(ui, live, bookmark).clicked() {
                    self.add_bookmark_from_key();
                    ui.close();
                }
                if live_off_item(ui, live, next_bookmark).clicked() {
                    self.step_bookmark(true);
                    ui.close();
                }
                if live_off_item(ui, live, previous_bookmark).clicked() {
                    self.step_bookmark(false);
                    ui.close();
                }
                ui.separator();
                if ui
                    .checkbox(&mut self.state.shuffle, "Shuffle tracks")
                    .changed()
                {
                    self.state.shuffle_history.clear();
                }
                ui.checkbox(&mut self.state.stop_after, "Stop after current track");
                if ui
                    .checkbox(&mut self.state.settings.skip_silence, "Skip long silences")
                    .on_hover_text("Shortens silences longer than 0.6 s in audiobooks and podcasts; positions and bookmarks are unaffected")
                    .changed()
                {
                    self.apply_skip_silence();
                    self.save();
                }
                ui.menu_button("Repeat", |ui| {
                    for (v, label) in [("off", "Off"), ("track", "Track"), ("queue", "Queue")] {
                        ui.selectable_value(&mut self.state.repeat, v.into(), label);
                    }
                });
                ui.separator();
                if live_off_item(ui, live, "Set A repeat point").clicked() {
                    self.state.ab_a = Some(self.position());
                    self.state.ab_b = None;
                    ui.close();
                }
                let can_set_b = !live && self.state.current.is_some()
                    && self.state.ab_a.is_some_and(|a| self.position() > a);
                if ui.add_enabled(can_set_b, egui::Button::new("Set B repeat point"))
                    .on_disabled_hover_text("Set A first, then move to a position after A").clicked() {
                    let pos = self.position();
                    if self.state.ab_a.is_some_and(|a| pos > a) {
                        self.state.ab_b = Some(pos);
                    }
                    ui.close();
                }
                if live_off_item(ui, live, "Clear A–B repeat").clicked() {
                    self.state.ab_a = None;
                    self.state.ab_b = None;
                    ui.close();
                }
                ui.separator();
                if ui
                    .checkbox(&mut self.state.settings.voice_boost, "Voice boost")
                    .on_hover_text("Adds the Voice equalizer curve and 4 dB for quiet narration; your equalizer settings stay as they are")
                    .changed()
                {
                    self.apply_voice_boost();
                }
            }
            "Library" => {
                if ui.button("Rescan folders").clicked() {
                    self.rescan();
                    ui.close();
                }
                let can_cancel = self.scan.running() && !self.scan.cancel_requested;
                if ui
                    .add_enabled(can_cancel, egui::Button::new("Cancel scan"))
                    .on_hover_text("Stop the scan that is running; the files read so far are kept")
                    .clicked()
                {
                    self.cancel_scans();
                    ui.close();
                }
                if ui.button("New playlist…").clicked() {
                    self.open_playlist_dialog(PlaylistDialogKind::New { tracks: Vec::new() });
                    ui.close();
                }
                if ui.button("Save queue as playlist…").clicked() {
                    self.open_playlist_dialog(PlaylistDialogKind::FromQueue);
                    ui.close();
                }
                if ui.button("Export library backup…").clicked() {
                    self.export_backup();
                    ui.close();
                }
                if ui.button("Import backup…").clicked() {
                    self.import_backup();
                    ui.close();
                }
            }
            "Options" => {
                if ui.button("Preferences…").clicked() {
                    self.open_options(ui.ctx());
                    ui.close();
                }
            }
            "Help" => {
                if ui.button("About BS Player").clicked() {
                    if self.about {
                        // Already open (maybe behind the player): bring it forward.
                        let about = egui::ViewportId::from_hash_of("bs-player-about");
                        ctx.send_viewport_cmd_to(about, egui::ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd_to(about, egui::ViewportCommand::Focus);
                    } else {
                        self.about = true;
                        self.about_center_pending = true;
                    }
                    ui.close();
                }
                if ui.button("Open profile folder").clicked() {
                    let _ = std::fs::create_dir_all(&self.profile);
                    if let Err(e) = media::reveal(&self.profile) {
                        self.error = Some(e);
                    }
                    ui.close();
                }
            }
            _ => {}
        }
    }

    pub(super) fn toggle_maximize(&mut self, ctx: &egui::Context) {
        if self.state.fullscreen {
            self.state.fullscreen = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        } else {
            let maximized = ctx.input(|input| {
                input
                    .viewport()
                    .maximized
                    .unwrap_or(self.state.window.maximized)
            });
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
        }
    }

    pub(super) fn menus(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        let maximized = ctx.input(|input| {
            input
                .viewport()
                .maximized
                .unwrap_or(self.state.window.maximized)
        });
        chrome_panel(
            root,
            egui::Panel::top("menus"),
            egui::Margin {
                left: 12,
                right: 2,
                top: 2,
                bottom: 0,
            },
            |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    let bar_height =
                        (ui.text_style_height(&egui::TextStyle::Button) + 12.).max(30.);
                    let control_width = 44.;
                    let controls_width = control_width * 3.;
                    let names = [
                        "File", "Edit", "View", "Playback", "Library", "Options", "Help",
                    ];
                    let font = egui::TextStyle::Button.resolve(ui.style());
                    let spacing = ui.spacing().item_spacing.x;
                    let widths: Vec<f32> = names
                        .iter()
                        .map(|name| {
                            ui.painter()
                                .layout_no_wrap(
                                    (*name).into(),
                                    font.clone(),
                                    ui.visuals().text_color(),
                                )
                                .size()
                                .x
                                + 2. * ui.spacing().button_padding.x
                                + spacing
                        })
                        .collect();
                    let available = (ui.available_width() - controls_width - 24.).max(1.);
                    let more_width = ui
                        .painter()
                        .layout_no_wrap("More".into(), font, ui.visuals().text_color())
                        .size()
                        .x
                        + 2. * ui.spacing().button_padding.x
                        + spacing;
                    let mut shown = names.len();
                    while shown > 0
                        && widths[..shown].iter().sum::<f32>()
                            + if shown < names.len() { more_width } else { 0. }
                            > available
                    {
                        shown -= 1;
                    }
                    for &name in &names[..shown] {
                        ui.menu_button(name, |ui| self.menu_contents(ui, name));
                    }
                    if shown < names.len() {
                        ui.menu_button("More", |ui| {
                            for &name in &names[shown..] {
                                ui.menu_button(name, |ui| self.menu_contents(ui, name));
                            }
                        });
                    }
                    let drag_width = (ui.available_width() - controls_width - spacing).max(1.);
                    let (_, drag) = ui.allocate_exact_size(
                        Vec2::new(drag_width, bar_height),
                        egui::Sense::click_and_drag(),
                    );
                    if drag.double_clicked() {
                        self.toggle_maximize(&ctx);
                    } else if drag.drag_started() && !self.state.fullscreen {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.;
                        if widgets::window_control(
                            ui,
                            widgets::WindowControl::Minimize,
                            control_width,
                            bar_height,
                        )
                        .clicked()
                        {
                            self.minimize_window(&ctx);
                        }
                        if widgets::window_control(
                            ui,
                            if maximized || self.state.fullscreen {
                                widgets::WindowControl::Restore
                            } else {
                                widgets::WindowControl::Maximize
                            },
                            control_width,
                            bar_height,
                        )
                        .clicked()
                        {
                            self.toggle_maximize(&ctx);
                        }
                        if widgets::window_control(
                            ui,
                            widgets::WindowControl::Close,
                            control_width,
                            bar_height,
                        )
                        .clicked()
                        {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
            },
        );
        if !self.state.compact {
            chrome_panel(
                root,
                egui::Panel::top("toolbar"),
                egui::Margin::symmetric(12, 5),
                |ui| {
                    ui.add_space(4.);
                    let font = egui::TextStyle::Body.resolve(ui.style());
                    let labeled_width: f32 =
                        [("Add files", 46.), ("Add folder", 46.), ("Playlist", 60.)]
                            .iter()
                            .map(|(label, chrome)| {
                                ui.painter()
                                    .layout_no_wrap(
                                        (*label).into(),
                                        font.clone(),
                                        ui.visuals().text_color(),
                                    )
                                    .size()
                                    .x
                                    + *chrome
                            })
                            .sum();
                    let compact_toolbar = ui.available_width()
                        < labeled_width + 140. + 6. * ui.spacing().item_spacing.x;
                    let icon_size = (ui.text_style_height(&egui::TextStyle::Body) + 12.).max(34.);
                    ui.horizontal(|ui| {
                        let add_files = if compact_toolbar {
                            widgets::icon_button(
                                ui,
                                Icon::Plus,
                                "Add files",
                                false,
                                false,
                                icon_size,
                            )
                        } else {
                            widgets::text_icon_button(ui, Icon::Plus, "Add files")
                        };
                        if add_files.clicked() {
                            self.choose_files(false);
                        }
                        ui.separator();
                        let add_folder = if compact_toolbar {
                            widgets::icon_button(
                                ui,
                                Icon::Folder,
                                "Add folder",
                                false,
                                false,
                                icon_size,
                            )
                        } else {
                            widgets::text_icon_button(ui, Icon::Folder, "Add folder")
                        };
                        if add_folder.clicked() {
                            self.choose_files(true);
                        }
                        ui.separator();
                        let playlist = if compact_toolbar {
                            widgets::icon_button(
                                ui,
                                Icon::Playlist,
                                "Playlist actions",
                                false,
                                false,
                                icon_size,
                            )
                        } else {
                            widgets::menu_icon_button(ui, Icon::Playlist, "Playlist")
                        };
                        egui::Popup::menu(&playlist).show(|ui| {
                            if ui.button("New playlist…").clicked() {
                                self.open_playlist_dialog(PlaylistDialogKind::New {
                                    tracks: Vec::new(),
                                });
                                ui.close();
                            }
                            if ui.button("Save queue as playlist…").clicked() {
                                self.open_playlist_dialog(PlaylistDialogKind::FromQueue);
                                ui.close();
                            }
                            if ui.button("Import playlist…").clicked() {
                                self.choose_files(false);
                                ui.close();
                            }
                            if ui.button("Export queue…").clicked() {
                                self.export_playlist();
                                ui.close();
                            }
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let search = ui.add(
                                egui::TextEdit::singleline(&mut self.state.search)
                                    .id(search_box_id())
                                    .desired_width(ui.available_width().clamp(60., 340.))
                                    .margin(egui::Margin {
                                        left: 30,
                                        right: 8,
                                        top: 6,
                                        bottom: 6,
                                    })
                                    .hint_text(if self.state.section == state::STREAMS_SECTION {
                                        "Search stations…"
                                    } else if let Some(BrowseSection::List(kind)) =
                                        browse_section(&self.state.section)
                                    {
                                        match kind {
                                            BrowseKind::Artists => "Search artists…",
                                            BrowseKind::Albums => "Search albums, artists…",
                                            BrowseKind::Genres => "Search genres…",
                                        }
                                    } else {
                                        "Search files, titles, authors…"
                                    }),
                            );
                            widgets::icon(
                                ui.painter(),
                                egui::Rect::from_center_size(
                                    egui::pos2(search.rect.left() + 15., search.rect.center().y),
                                    Vec2::splat(18.),
                                ),
                                Icon::Search,
                                ui.visuals().weak_text_color(),
                            );
                        });
                    });
                    ui.add_space(4.);
                },
            );
        }
    }

    pub(super) fn sidebar(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        if !self.state.settings.show_library || self.state.compact {
            return;
        }
        let catalog = self
            .gui_cache
            .catalog(&self.state.library, self.library_revision);
        let max_sidebar = (root.available_width() * 0.38).clamp(150., 520.);
        prepare_player_pane(
            &ctx,
            "library",
            0,
            self.state.sidebar_width.clamp(140., max_sidebar),
        );
        let panel = chrome_panel(
            root,
            egui::Panel::left("library")
                .resizable(true)
                .default_size(self.state.sidebar_width.min(max_sidebar))
                .size_range(140.0..=max_sidebar),
            egui::Margin::symmetric(10, 6),
            |ui| {
                if self.state.settings.show_artwork && ui.available_height() >= 170. {
                    let nav_floor = (self.state.settings.font_size + 10.) * 5. + 60.;
                    let max_art = (ui.available_height()
                        - nav_floor.min(ui.available_height() * 0.55))
                    .max(36.);
                    prepare_player_pane(
                        &ctx,
                        "artwork panel",
                        1,
                        self.state.artwork_height.clamp(36., max_art),
                    );
                    let result = egui::Panel::bottom("artwork panel")
                        .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(0, 8)))
                        .resizable(true)
                        .default_size(self.state.artwork_height.min(max_art))
                        .size_range(36.0..=max_art)
                        .show(ui, |ui| {
                            self.artwork_ui(ui, &ctx);
                        });
                    if player_pane_resize_active(&ctx, "artwork panel") {
                        self.state.artwork_height = result.response.rect.height();
                    }
                }
                egui::ScrollArea::vertical()
                    .id_salt("library navigation")
                    .max_height(ui.available_height().max(1.))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.;
                        self.playing_navigation(ui);
                        if sidebar_section(ui, "LIBRARY", &mut self.state.collapsed_sections) {
                            for (name, icon) in [
                                ("Music", Icon::Music),
                                ("Audiobooks", Icon::Book),
                                ("Recently added", Icon::Clock),
                                ("Continue listening", Icon::Continue),
                                ("Finished", Icon::Check),
                                (BrowseKind::Artists.list_section(), Icon::Artist),
                                (BrowseKind::Albums.list_section(), Icon::Album),
                                (BrowseKind::Genres.list_section(), Icon::Tag),
                            ] {
                                if navigation(
                                    ui,
                                    name,
                                    icon,
                                    library_entry_selected(&self.state.section, name),
                                )
                                .clicked()
                                {
                                    self.state.section = name.into();
                                    self.state.scroll = 0.;
                                    self.restore_scroll = true;
                                }
                            }
                        }
                        ui.add_space(3.);
                        ui.separator();
                        let (playlists_open, new_playlist, header) = sidebar_section_with(
                            ui,
                            "PLAYLISTS",
                            &mut self.state.collapsed_sections,
                            Some("New playlist…"),
                        );
                        if new_playlist {
                            self.open_playlist_dialog(PlaylistDialogKind::New {
                                tracks: Vec::new(),
                            });
                        }
                        header.context_menu(|ui| {
                            if ui.button("New playlist…").clicked() {
                                self.open_playlist_dialog(PlaylistDialogKind::New {
                                    tracks: Vec::new(),
                                });
                                ui.close();
                            }
                        });
                        if playlists_open {
                            if navigation(
                                ui,
                                "Favorites",
                                Icon::Playlist,
                                self.state.section == "Favorites",
                            )
                            .clicked()
                            {
                                self.state.section = "Favorites".into();
                                self.state.scroll = 0.;
                                self.restore_scroll = true;
                            }
                            // Copy names only; track-ID vectors can contain an entire
                            // library and are needed only when a menu action is used.
                            let playlists: Vec<_> = self
                                .state
                                .playlists
                                .iter()
                                .enumerate()
                                .map(|(index, list)| (index, list.name.clone()))
                                .collect();
                            for (index, name) in playlists {
                                let key = format!("Playlist:{name}");
                                let response = navigation(
                                    ui,
                                    &name,
                                    Icon::Playlist,
                                    self.state.section == key,
                                );
                                if response.clicked() {
                                    self.state.section = key;
                                    self.state.scroll = 0.;
                                    self.restore_scroll = true;
                                }
                                response.context_menu(|ui| {
                                    if ui.button("Load into queue").clicked() {
                                        if let Some(list) = self.state.playlists.get(index) {
                                            self.state.queue = list.tracks.clone();
                                        }
                                        self.view_revision = self.view_revision.wrapping_add(1);
                                        self.state.tab = "Queue".into();
                                        ui.close();
                                    }
                                    if ui.button("Rename playlist…").clicked() {
                                        self.open_playlist_dialog(PlaylistDialogKind::Rename {
                                            from: name.clone(),
                                        });
                                        ui.close();
                                    }
                                    if ui.button("Duplicate playlist").clicked() {
                                        if let Some(list) = self.state.playlists.get(index) {
                                            let copy = Playlist {
                                                name: unique_copy_name(
                                                    &self.state.playlists,
                                                    &name,
                                                ),
                                                tracks: list.tracks.clone(),
                                            };
                                            self.state.playlists.push(copy);
                                            self.view_revision = self.view_revision.wrapping_add(1);
                                        }
                                        ui.close();
                                    }
                                    if ui.button("Remove playlist").clicked() {
                                        self.remove_playlist(index);
                                        ui.close();
                                    }
                                });
                            }
                        }
                        ui.add_space(3.);
                        ui.separator();
                        let (streams_open, add_stream, streams_header) = sidebar_section_with(
                            ui,
                            "STREAMS",
                            &mut self.state.collapsed_sections,
                            Some("Add stream URL…"),
                        );
                        if add_stream {
                            self.open_station_dialog(StationDialogKind::Add);
                        }
                        streams_header.context_menu(|ui| {
                            if ui.button("Add stream URL…").clicked() {
                                self.open_station_dialog(StationDialogKind::Add);
                                ui.close();
                            }
                        });
                        if streams_open {
                            let selected = self.state.section == state::STREAMS_SECTION;
                            let response = match self.current_station_label() {
                                // The station that is tuned in shows under the heading.
                                Some(name) => {
                                    navigation_detail(ui, "Stations", &name, Icon::Radio, selected)
                                }
                                None => navigation(ui, "Stations", Icon::Radio, selected),
                            };
                            if response.clicked() {
                                self.show_streams();
                            }
                        }
                        if catalog.books.len() > 1 {
                            ui.add_space(3.);
                            ui.separator();
                            if sidebar_section(ui, "BOOKS", &mut self.state.collapsed_sections) {
                                self.books_list(ui, &catalog);
                            }
                        }
                        ui.add_space(3.);
                        ui.separator();
                        if sidebar_section(ui, "FOLDERS", &mut self.state.collapsed_sections) {
                            if self.state.roots.is_empty() {
                                if navigation(ui, "Folders", Icon::Folder, false)
                                    .on_hover_text("Add a music or audiobook folder")
                                    .clicked()
                                {
                                    self.choose_files(true);
                                }
                            }
                            for (path, book) in self.state.roots.clone() {
                                let key = format!("Folder:{}", path.display());
                                let label = path.file_name().unwrap_or_default().to_string_lossy();
                                let response =
                                    navigation(ui, &label, Icon::Folder, self.state.section == key);
                                if response.clicked() {
                                    self.state.section = key;
                                }
                                response
                                    .on_hover_text(path.display().to_string())
                                    .context_menu(|ui| {
                                        if ui.button("Rescan folder").clicked() {
                                            self.import_jobs(
                                                vec![(vec![path.clone()], book)],
                                                true,
                                                true,
                                                false,
                                            );
                                            ui.close();
                                        }
                                        let blocked = self.busy > 0 || self.scan.running();
                                        if ui
                                            .add_enabled(!blocked, egui::Button::new("Move folder…"))
                                            .on_hover_text(
                                                "Reconnect the library files of this folder after it was moved to another place",
                                            )
                                            .clicked()
                                        {
                                            self.move_folder(path.clone());
                                            ui.close();
                                        }
                                        if ui.button("Stop watching this folder")
                                            .on_hover_text("Exclude this folder from future scans. Use Cancel scan to stop a scan already running.").clicked() {
                                            self.state.roots.retain(|(p, _)| p != &path);
                                            ui.close();
                                        }
                                        if ui.button("Reveal folder").clicked() {
                                            if let Err(e) = media::reveal(&path) {
                                                self.error = Some(e);
                                            }
                                            ui.close();
                                        }
                                    });
                            }
                        }
                    });
            },
        );
        if player_pane_resize_active(&ctx, "library") {
            self.state.sidebar_width = panel.response.rect.width();
        }
    }
}

/// The key currently bound to `action`, if any.
pub(super) fn bound_key<'a>(bindings: &'a [settings::Shortcut], action: &str) -> Option<&'a str> {
    bindings
        .iter()
        .find(|binding| binding.action == action && !binding.key.trim().is_empty())
        .map(|binding| binding.key.as_str())
}

/// Menu text with the bound key right-aligned by spacing; plain when unbound.
pub(super) fn with_hint(label: &str, key: Option<&str>) -> String {
    match key {
        Some(key) => format!("{label}    {key}"),
        None => label.to_owned(),
    }
}
