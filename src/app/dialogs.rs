use super::*;

impl PlayerApp {
    /// Opens the playlist name window.
    pub(super) fn open_playlist_dialog(&mut self, kind: PlaylistDialogKind) {
        let name = match &kind {
            PlaylistDialogKind::Rename { from } => from.clone(),
            _ => String::new(),
        };
        self.playlist_dialog = Some(PlaylistDialog {
            kind,
            name,
            fresh: true,
        });
    }

    /// The playlist name window: a new playlist, the queue as a playlist, or a
    /// rename. A name that is empty, too long or already used is explained under
    /// the field and cannot be accepted; Enter accepts, Escape cancels.
    pub(super) fn show_playlist_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.playlist_dialog.take() else {
            return;
        };
        let (title, button, note) = match &dialog.kind {
            PlaylistDialogKind::FromQueue => (
                "Save queue as playlist",
                "Save playlist",
                Some(format!(
                    "{} files in the current queue",
                    self.state.queue.len()
                )),
            ),
            PlaylistDialogKind::New { tracks } => (
                "New playlist",
                "Create",
                match tracks.len() {
                    0 => None,
                    1 => Some("The file is added to the new playlist.".to_owned()),
                    count => Some(format!("{count} files are added to the new playlist.")),
                },
            ),
            PlaylistDialogKind::Rename { .. } => ("Rename playlist", "Rename", None),
        };
        let renamed = match &dialog.kind {
            PlaylistDialogKind::Rename { from } => Some(self.state.playlist_position(from)),
            _ => None,
        };
        let verdict = match renamed {
            Some(None) => Err(state::PlaylistProblem::Missing),
            Some(index) => state::check_playlist_name(&self.state.playlists, &dialog.name, index),
            None => state::check_playlist_name(&self.state.playlists, &dialog.name, None),
        };
        let mut open = true;
        let mut accept = false;
        let mut cancel = false;
        in_player_window(ctx, title, 400.)
            .open(&mut open)
            .collapsible(false)
            .show(ctx, |ui| {
                if let Some(note) = &note {
                    ui.label(note);
                }
                ui.label("Name");
                let edit = ui
                    .add(egui::TextEdit::singleline(&mut dialog.name).desired_width(f32::INFINITY));
                if dialog.fresh {
                    dialog.fresh = false;
                    edit.request_focus();
                    let mut state =
                        egui::text_edit::TextEditState::load(ui.ctx(), edit.id).unwrap_or_default();
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(dialog.name.chars().count()),
                        )));
                    state.store(ui.ctx(), edit.id);
                }
                match &verdict {
                    Ok(_) => {}
                    Err(state::PlaylistProblem::Empty) => {
                        ui.small(state::PlaylistProblem::Empty.text());
                    }
                    Err(problem) => {
                        ui.colored_label(ui.visuals().error_fg_color, problem.text());
                    }
                }
                let enter =
                    edit.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui
                        .add_enabled(verdict.is_ok(), egui::Button::new(button))
                        .clicked()
                    {
                        accept = true;
                    }
                });
                if enter && verdict.is_ok() {
                    accept = true;
                } else if enter {
                    // A refused Enter keeps the cursor in the field.
                    edit.request_focus();
                }
            });
        if accept {
            let queue = self.state.queue.clone();
            let done = match &dialog.kind {
                PlaylistDialogKind::FromQueue => self
                    .state
                    .create_playlist(&dialog.name, queue)
                    .map(|name| (format!("Saved the queue as the playlist “{name}”"), None)),
                PlaylistDialogKind::New { tracks } => {
                    // A file removed from the library meanwhile is not added.
                    let tracks = self.existing_ids(tracks.clone());
                    self.state
                        .create_playlist(&dialog.name, tracks)
                        .map(|name| (format!("Created the playlist “{name}”"), None))
                }
                PlaylistDialogKind::Rename { from } => {
                    self.state.rename_playlist(from, &dialog.name).map(|name| {
                        let undo = (name != *from).then(|| {
                            (
                                format!(
                                    "Rename playlist “{}”",
                                    short_label(from, UNDO_LABEL_CHARS)
                                ),
                                state::UndoRestore::PlaylistRenamed {
                                    from: from.clone(),
                                    to: name.clone(),
                                },
                            )
                        });
                        (format!("Renamed the playlist to “{name}”"), undo)
                    })
                }
            };
            if let Ok((note, undo)) = done {
                self.view_revision = self.view_revision.wrapping_add(1);
                match undo {
                    Some((label, restore)) => self.record_undo(label, restore, note),
                    None => self.set_status(note),
                }
                self.save();
                return;
            }
        }
        if open && !cancel {
            self.playlist_dialog = Some(dialog);
        }
    }

    /// Escape closes the topmost in-window dialog (like its Cancel or Dismiss
    /// button); Enter dismisses the message dialog.
    pub(super) fn dismiss_dialogs_on_keys(&mut self, ctx: &egui::Context) {
        let any_open = self.confirm.is_some()
            || self.error.is_some()
            || self.tag_editor.is_some()
            || self.details.is_some()
            || self.bookmark_dialog
            || self.bookmark_edit.is_some()
            || self.playlist_dialog.is_some()
            || self.station_dialog.is_some();
        if !any_open || egui::Popup::is_any_open(ctx) {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            if self.confirm.is_some() {
                self.confirm = None;
            } else if self.station_dialog.is_some() {
                self.station_dialog = None;
            } else if self.playlist_dialog.is_some() {
                self.playlist_dialog = None;
            } else if self.bookmark_edit.is_some() {
                self.bookmark_edit = None;
            } else if self.bookmark_dialog {
                self.bookmark_dialog = false;
                self.bookmark_target = None;
            } else if self.tag_editor.is_some() {
                // A save in progress cannot be cancelled; the window stays.
                if self.tag_editor.as_ref().is_some_and(|e| e.saving.is_none()) {
                    self.tag_editor = None;
                }
            } else if self.details.is_some() {
                self.details = None;
            } else if self.error.is_some() {
                self.error = None;
            }
        } else if self.error.is_some()
            && !ctx.text_edit_focused()
            && ctx.input_mut(|i| consume_plain_key(i, egui::Key::Enter))
        {
            self.error = None;
        }
    }

    pub(super) fn dialogs(&mut self, ctx: &egui::Context, monitor: Option<egui::Rect>) {
        self.dismiss_dialogs_on_keys(ctx);
        let player_center = player_center_pixels(ctx);
        let level = self.window_level();
        // Playback keys pressed in the Options, Equalizer and About windows.
        let mut window_actions: Vec<String> = Vec::new();
        if let Some(mut options) = self.options.take() {
            // The separate EQ window owns these live settings. Refresh only
            // those fields, preserving all other pending Options edits.
            options.sync_equalizer(&self.state.settings);
            options.set_tray_status(self.tray_available, &self.tray_reason);
            options.set_media_keys_status(self.media_keys_on, winintegration::media_keys_held());
            options.set_association_status(
                self.association,
                self.association_busy,
                self.association_error.as_deref(),
            );
            options.set_removed_count(self.state.removed_paths.len());
            options.set_watched_folders(&self.state.roots, self.busy > 0 || self.scan.running());
            let applied = ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("bs-player-options"),
                dialog_placement(
                    ctx,
                    player_center,
                    monitor,
                    self.options_center_pending,
                    egui::ViewportBuilder::default()
                        .with_title("BS Player — Options")
                        .with_window_level(level)
                        .with_decorations(true)
                        .with_resizable(true)
                        .with_inner_size([860., 820.])
                        .with_min_inner_size([480., 360.]),
                ),
                |ui, _class| {
                    center_dialog_once(
                        ui.ctx(),
                        player_center,
                        monitor,
                        &mut self.options_center_pending,
                    );
                    window_actions.extend(pressed_bindings(
                        ui.ctx(),
                        &self.state.settings.shortcuts,
                        None,
                        true,
                    ));
                    egui::CentralPanel::default()
                        .frame(
                            egui::Frame::NONE
                                .fill(appearance::workspace(ui))
                                .inner_margin(10),
                        )
                        .show(ui, |ui| {
                            options.show_body(ui, &self.state.settings, &self.devices)
                        })
                        .inner
                },
            );
            if let Some(settings) = applied {
                self.audio.settings(
                    &settings::engine_settings(&self.state.settings),
                    &settings::engine_settings(&settings),
                );
                if self.state.settings.prefer_embedded_art != settings.prefer_embedded_art {
                    self.art_requested = None;
                    self.thumbs.reset();
                }
                if settings.private_listening && !self.state.settings.private_listening {
                    self.private_track = self.engine_current();
                }
                self.state.settings = settings;
                self.apply_replaygain();
                self.apply_skip_silence();
                self.save();
            }
            let actions = std::mem::take(&mut options.actions);
            for action in actions {
                self.settings_action(ctx, action);
            }
            if options.open {
                self.options = Some(options);
            }
        }
        if let Some(mut equalizer) = self.equalizer.take() {
            let previous = (
                self.state.settings.equalizer_enabled,
                self.state.settings.preamp_db,
                self.state.settings.eq_gains,
            );
            let voice_boost = self.state.settings.voice_boost;
            let (changed, pointer_down) = ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("bs-player-equalizer"),
                dialog_placement(
                    ctx,
                    player_center,
                    monitor,
                    self.equalizer_center_pending,
                    egui::ViewportBuilder::default()
                        .with_title("BS Player — Equalizer")
                        .with_window_level(level)
                        .with_decorations(true)
                        .with_resizable(true)
                        .with_inner_size([850., 560.])
                        .with_min_inner_size([480., 380.]),
                ),
                |ui, _class| {
                    center_dialog_once(
                        ui.ctx(),
                        player_center,
                        monitor,
                        &mut self.equalizer_center_pending,
                    );
                    window_actions.extend(pressed_bindings(
                        ui.ctx(),
                        &self.state.settings.shortcuts,
                        None,
                        true,
                    ));
                    let changed = egui::CentralPanel::default()
                        .frame(
                            egui::Frame::NONE
                                .fill(appearance::workspace(ui))
                                .inner_margin(12),
                        )
                        .show(ui, |ui| {
                            if voice_boost {
                                // The overlay lives in the engine settings only; the
                                // bands below are the user's own and stay untouched.
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(VOICE_BOOST_NOTE)
                                            .small()
                                            .color(ui.visuals().hyperlink_color),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(VOICE_BOOST_NOTE);
                            }
                            equalizer.show_body(ui, &mut self.state.settings)
                        })
                        .inner;
                    (changed, ui.input(|input| input.pointer.any_down()))
                },
            );
            if changed {
                self.equalizer_dirty = true;
                if previous
                    != (
                        self.state.settings.equalizer_enabled,
                        self.state.settings.preamp_db,
                        self.state.settings.eq_gains,
                    )
                {
                    self.audio
                        .dsp_settings(&settings::engine_settings(&self.state.settings));
                }
                if let Some(options) = &mut self.options {
                    options.sync_equalizer(&self.state.settings);
                }
            }
            // Live DSP follows the drag; persistence waits for the completed
            // edit or window close instead of cloning/saving the session each frame.
            if self.equalizer_dirty && (!pointer_down || !equalizer.open) {
                self.save();
                self.equalizer_dirty = false;
            }
            if equalizer.open {
                self.equalizer = Some(equalizer);
            }
        }
        if let Some(error) = self.error.clone() {
            let mut open = true;
            in_player_window(ctx, "BS Player — message", 550.)
                .id(egui::Id::new("error dialog"))
                .open(&mut open)
                .resizable(true)
                .show(ctx, |ui| {
                    ui.add(egui::Label::new(&error).wrap());
                    if ui.button("Dismiss").clicked() {
                        self.error = None;
                    }
                });
            if !open {
                self.error = None;
            }
        }
        if self.details.is_none() {
            // The dialog is closed: its thumbnail goes with it.
            self.details_art = None;
        }
        // The file editor temporarily takes over this inspector; keep the library
        // draft alive underneath so returning never discards unsaved edits.
        let file_editor_was_open = self.tag_editor.is_some();
        if let Some(mut track) = if file_editor_was_open {
            None
        } else {
            self.details.take()
        } {
            let mut open = true;
            let mut close = false;
            let mut open_editor = false;
            let mut carry_to: Option<String> = None;
            self.ensure_details_art(&track);
            inspector_window(ctx, "File details")
                .open(&mut open)
                .show(ctx, |ui| {
                    let tab_id = egui::Id::new(("details-tab", &track.id));
                    let mut tab = ui.data_mut(|data| data.get_temp::<usize>(tab_id).unwrap_or(0));
                    let body_height = inspector_body_height(ui, 2.);
                    egui::ScrollArea::vertical()
                        .id_salt(("details-body", &track.id))
                        .auto_shrink([false, false])
                        .max_height(body_height)
                        .show(ui, |ui| {
                            ui.horizontal_top(|ui| {
                                let side = if ui.available_width() < 430. { 76. } else { 112. };
                                let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), egui::Sense::hover());
                                match self.details_art.as_ref().map(|art| &art.state) {
                                    Some(DetailsArtState::Ready(texture, _)) => {
                                        egui::Image::new(texture)
                                            .corner_radius(widgets::art_corner_radius(rect.width()))
                                            .paint_at(ui, rect);
                                    }
                                    _ => widgets::paint_default_art(ui.painter(), rect, track.audiobook, art_seed(&track), ui.visuals().dark_mode),
                                }
                                ui.vertical(|ui| {
                                    ui.add(egui::Label::new(RichText::new(track.display_title()).heading().strong()).wrap());
                                    if !track.artist.is_empty() { ui.add(egui::Label::new(&track.artist).wrap()); }
                                    if !track.album.is_empty() { ui.add(egui::Label::new(RichText::new(&track.album).weak()).wrap()); }
                                    ui.label(RichText::new(format!("{} · {} Hz · {} channels", time_text(track.duration), track.sample_rate, track.channels)).small().weak());
                                    if ui.add_enabled(!track.superseded, egui::Button::new("Edit file tags & artwork…").wrap())
                                        .on_hover_text("Edit information and the cover stored in the audio file; library edits below remain separate")
                                        .clicked() { open_editor = true; }
                                });
                            });
                            if track.superseded {
                                ui.add(egui::Label::new(RichText::new("Previous file content. Listening progress is kept; use Locate file to reconnect the original audio.").color(ui.visuals().warn_fg_color)).wrap());
                            }
                            ui.add_space(8.);
                            ui.horizontal_wrapped(|ui| {
                                for (index, label) in ["Library tags", "File information", "Listening"].iter().enumerate() {
                                    ui.selectable_value(&mut tab, index, *label);
                                }
                            });
                            ui.separator();
                            match tab {
                                0 => {
                                    ui.label(RichText::new("These edits stay in BS Player. The audio file is unchanged.").small().weak());
                                    inspector_text_fields(ui, &mut [
                                        ("Title", &mut track.title), ("Artist / author", &mut track.artist),
                                        ("Album / book", &mut track.album), ("Genre", &mut track.genre),
                                        ("Year", &mut track.year), ("Comment / narrator", &mut track.comment),
                                    ]);
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label("Track / chapter");
                                        ui.add(egui::DragValue::new(&mut track.track_number).range(0..=9999));
                                        ui.separator();
                                        ui.checkbox(&mut track.audiobook, "Audiobook");
                                    });
                                }
                                1 => {
                                    ui.label(RichText::new("File location").strong());
                                    ui.add(egui::Label::new(media::display_path(&track.path)).wrap());
                                    ui.add_space(6.);
                                    ui.label(format!("{} bytes · {} Hz · {} channels", track.file_size, track.sample_rate, track.channels));
                                    for (label, value) in scanned_tag_rows(&track) {
                                        ui.horizontal_wrapped(|ui| { ui.label(RichText::new(label).weak()); ui.label(value); });
                                    }
                                    if let Some(count) = self.chapter_cache.get(&ChapterKey::of(&track)).map(|chapters| chapters.len()).filter(|count| *count > 0) {
                                        ui.label(format!("Chapters: {count}"));
                                    }
                                    if let Some(line) = self.lyrics_cache.get(&LyricsKey::of(&track)).and_then(|lyrics| lyrics_details_line(&lyrics)) { ui.label(line); }
                                    let artwork_note = match self.details_art.as_ref().map(|art| &art.state) {
                                        Some(DetailsArtState::Ready(_, origin)) => format!("Artwork: {origin}"),
                                        Some(DetailsArtState::Note(note)) => note.details_line(),
                                        _ => "Artwork: loading…".to_owned(),
                                    };
                                    ui.add(egui::Label::new(RichText::new(artwork_note).small().weak()).wrap());
                                    ui.add_space(8.);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Reveal file").clicked() {
                            if let Err(e) = media::reveal(&track.path) {
                                self.error = Some(e);
                            }
                        }
                        if ui.button("Copy path").clicked() {
                            ctx.copy_text(media::display_path(&track.path));
                        }
                        // Locating closes this dialog, which would drop unsaved edits.
                        let dirty = self
                            .state
                            .track(&track.id)
                            .is_some_and(|saved| tags_differ(saved, &track));
                        if ui
                            .add_enabled(!dirty, egui::Button::new("Locate file…"))
                            .on_disabled_hover_text("Save or cancel your tag edits first.")
                            .clicked()
                        {
                            self.actions.push(Action::Relocate(track.id.clone()));
                            close = true;
                        }
                        if track.superseded {
                            // The current version of the same file, if the library has one.
                            let current = self
                                .state
                                .library
                                .iter()
                                .find(|other| {
                                    !other.superseded
                                        && other.id != track.id
                                        && other.path == track.path
                                })
                                .map(|other| other.id.clone());
                            if let Some(current) = current {
                                let dirty = self
                                    .state
                                    .track(&track.id)
                                    .is_some_and(|saved| tags_differ(saved, &track));
                                if ui
                                    .add_enabled(
                                        !dirty,
                                        egui::Button::new("Carry progress to the current version"),
                                    )
                                    .on_hover_text(if dirty {
                                        "Save or cancel your library tag edits first."
                                    } else {
                                        "Move the saved position, bookmarks, rating and queue entries to the current version of this file"
                                    })
                                    .clicked()
                                {
                                    carry_to = Some(current);
                                }
                            }
                        }
                    });

                                }
                                _ => {
                    let progress = self
                        .state
                        .progress
                        .get(&track.id)
                        .cloned()
                        .unwrap_or_default();
                    ui.label(format!(
                        "Play count: {} · Saved position: {}{}",
                        progress.play_count,
                        time_text(progress.seconds),
                        if progress.completed {
                            " · Finished"
                        } else {
                            ""
                        }
                    ));
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Rating");
                        // Read without inserting: merely viewing a file must not
                        // create a rating entry; only an edit writes one.
                        let mut rating = self.state.ratings.get(&track.id).copied().unwrap_or(0);
                        if ui
                            .add(egui::Slider::new(&mut rating, 0..=5).suffix(" stars"))
                            .changed()
                        {
                            self.state.ratings.insert(track.id.clone(), rating);
                            self.save();
                            // A list sorted by rating must be rebuilt.
                            if self.state.sort_column == columns::COL_RATING {
                                self.view_revision = self.view_revision.wrapping_add(1);
                            }
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Reset this file’s progress").clicked() {
                            if self.state.current.as_deref() == Some(&track.id) {
                                self.seek(0.);
                            }
                            if let Some(p) = self.state.progress.get_mut(&track.id) {
                                p.seconds = 0.;
                                p.completed = false;
                            }
                            self.progress_changed();
                            self.save();
                        }
                        if ui.button("Mark finished").clicked() {
                            let p = self.state.progress.entry(track.id.clone()).or_default();
                            p.completed = true;
                            p.seconds = track.duration;
                            self.progress_changed();
                            self.save();
                        }
                    });

                                    ui.label(RichText::new("Listening changes are saved immediately.").small().weak());
                                }
                            }
                        });
                    ui.data_mut(|data| data.insert_temp(tab_id, tab));
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        let record_exists = self.state.track(&track.id).is_some();
                        if ui
                            .add_enabled(record_exists, egui::Button::new("Save library tags"))
                            .on_disabled_hover_text(
                                "This file was removed from the library. The draft is still open; cancel or restore the file before saving.",
                            )
                            .clicked()
                        {
                            if let Some(t) =
                                Arc::make_mut(&mut self.state.library).iter_mut().find(|t| t.id == track.id)
                            {
                                // Saving without a change must not freeze the tags
                                // against later refreshes from the file.
                                track.library_edited |= tags_differ(t, &track);
                                let before = t.clone();
                                // A scan/validation may have refreshed identity while
                                // this editor was open. Commit only edited tag fields.
                                t.preserve_library_tags(&track);
                                // A new album or artist changes the book key.
                                preserve_book_speed(&mut self.state.book_speeds, &before, t);
                            }
                            self.invalidate_library();
                            self.save();
                            close = true;
                        }
                    });

                });
            if open_editor {
                self.open_tag_editor(&track.id);
            }
            if let Some(new_id) = carry_to {
                self.confirm = Some(Confirm::CarryProgress {
                    old_id: track.id.clone(),
                    new_id,
                });
            }
            if open && !close {
                self.details = Some(track);
            } else {
                self.details_art = None;
            }
        }
        if file_editor_was_open {
            self.show_tag_editor(ctx);
        }
        if self.bookmark_dialog && self.bookmark_target.is_none() {
            self.bookmark_dialog = false;
        }
        if let Some(draft) = self
            .bookmark_target
            .clone()
            .filter(|_| self.bookmark_dialog)
        {
            let mut open = true;
            let mut close = false;
            let can_save = self
                .gui_cache
                .catalog(&self.state.library, self.library_revision)
                .track(&draft.track_id)
                .is_some();
            in_player_window(ctx, "Add bookmark", 430.)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.add(egui::Label::new(format!(
                        "{} at {}",
                        draft.title,
                        time_text(draft.seconds)
                    )).wrap());
                    if !can_save {
                        ui.label("This file was removed from the library. Your draft is kept; cancel or reconnect the original item before saving.");
                    }
                    ui.label("Name");
                    ui.text_edit_singleline(&mut self.bookmark_name);
                    ui.label("Notes");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.bookmark_note)
                            .desired_rows(3)
                            .desired_width(ui.available_width().clamp(80., 420.)),
                    );
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.add_enabled(can_save, egui::Button::new("Save bookmark")).clicked() {
                            close = self.save_bookmark_draft();
                        }
                    });
                });
            self.bookmark_dialog = open && !close;
            if !self.bookmark_dialog {
                self.bookmark_target = None;
            }
        }
        if let Some(mut edit) = self.bookmark_edit.take() {
            let mut open = true;
            let mut close = false;
            in_player_window(ctx, "Edit bookmark", 430.)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.add(
                        egui::Label::new(format!(
                            "{} at {}",
                            edit.title,
                            time_text(edit.original.seconds)
                        ))
                        .wrap(),
                    );
                    ui.label("Name");
                    ui.text_edit_singleline(&mut edit.name);
                    ui.label("Notes");
                    ui.add(
                        egui::TextEdit::multiline(&mut edit.note)
                            .desired_rows(3)
                            .desired_width(ui.available_width().clamp(80., 420.)),
                    );
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui.button("Save changes").clicked() {
                            if apply_bookmark_edit(&mut self.state.bookmarks, &edit) {
                                self.save();
                            } else {
                                self.add_message(
                                    "This bookmark was removed while it was open, so the changes were not saved."
                                        .into(),
                                );
                            }
                            close = true;
                        }
                    });
                });
            if open && !close {
                self.bookmark_edit = Some(edit);
            }
        }
        self.show_playlist_dialog(ctx);
        self.show_station_dialog(ctx);
        // Taken out while it is shown: a pending Restore holds a whole session,
        // which must not be copied every frame.
        if let Some(confirm) = self.confirm.take() {
            let mut open = true;
            let mut accept = false;
            let mut cancelled = false;
            in_player_window(ctx, "Confirm change", 480.).open(&mut open).collapsible(false).show(ctx,|ui|{ui.label(match &confirm{Confirm::RemoveMany(ids)=>format!("Remove {} from the library? They also leave the queue and playlists. The original files and saved listening positions are kept.",files_text(ids.len())),Confirm::Remove(id)=>format!("Remove {} from the library and playlists? The original file and saved listening position are kept.",self.state.track(id).map(|t|t.display_title()).unwrap_or("this file")),Confirm::ClearHistory=>"Clear play counts and last-played dates? Listening positions and bookmarks are kept.".into(),Confirm::ResetProgress=>"Reset every listening position and completion flag? Bookmarks and audio files are kept. Export a backup first if you may need these positions.".into(),Confirm::Restore(s)=>format!("Replace this profile with the imported backup?\n{} files, {} playlists, {} bookmarks, {} saved positions.\nThe current profile is saved as before-restore.json before replacement; the two previous copies are kept. Audio files are not modified.",s.library.len(),s.playlists.len(),s.bookmarks.len(),s.progress.len()),Confirm::Relink{track,..}=>format!("This older profile has only a sampled file fingerprint, so the selected file cannot be fully verified against the original. Its sampled bytes and size match.\n\nReconnect {} and keep the original saved position and bookmarks? Cancel leaves the profile unchanged.",media::display_path(&track.path)),Confirm::ClearQueue=>"Clear the queue? Playback will pause and all per-file positions remain saved.".into(),Confirm::RemoveBookmark(bookmark)=>format!("Remove the bookmark “{}” at {}? Its note is deleted with it.",bookmark.name,time_text(bookmark.seconds)),Confirm::ForgetRemovals=>format!("Forget the {} files you removed from the library? A rescan of a watched folder will then add them back. Your audio files are not touched.",self.state.removed_paths.len()),Confirm::CarryProgress{new_id,..}=>format!("Move the saved position, bookmarks, rating, favourite and queue and playlist entries of this earlier version of {} to the current version of the file, and remove the earlier entry from the library? The audio file is not touched.",self.state.track(new_id).map(|t|t.display_title()).unwrap_or("this file")),Confirm::BookNotStarted{label,..}=>format!("Mark “{label}” as not started? Every file of the book goes back to the start and loses its finished flag. Bookmarks, play counts and the audio files are kept."),Confirm::RemoveStation(id)=>format!("Remove the station “{}” from the Streams list?",self.state.station(id).map(|station|station_display_name(station,self.station_live.get(id))).unwrap_or_else(||"this station".into())),Confirm::RereadTags=>format!("Read the tags of all {} files in the library again? Listening progress, bookmarks, ratings and your library tag edits stay. This reads every file completely and can take a while on a large library; the status line shows how far it is.",self.reread_file_count())});ui.separator();ui.horizontal_wrapped(|ui|{if ui.button("Cancel").clicked(){cancelled=true;}if ui.button("Confirm").clicked(){accept=true;}});});
            if !accept && open && !cancelled {
                self.confirm = Some(confirm);
            } else if accept {
                match confirm {
                    Confirm::Remove(id) => self.remove_from_library(&id),
                    Confirm::RemoveMany(ids) => {
                        let ids = self.existing_ids(ids);
                        self.remove_from_library_many(&ids);
                    }
                    Confirm::ClearHistory => {
                        for p in self.state.progress.values_mut() {
                            p.last_played = 0;
                            p.play_count = 0;
                        }
                        self.progress_changed();
                        self.save();
                    }
                    Confirm::RemoveBookmark(bookmark) => {
                        if let Some(index) =
                            self.state.bookmarks.iter().position(|b| b == &bookmark)
                        {
                            self.delete_bookmark(index);
                        }
                    }
                    Confirm::ForgetRemovals => self.forget_removed_files(),
                    Confirm::CarryProgress { old_id, new_id } => {
                        self.carry_progress_to_current(&old_id, &new_id)
                    }
                    Confirm::ResetProgress => {
                        self.cancel_validation();
                        self.audio.pause();
                        self.state.was_playing = false;
                        if self.state.current.is_some() {
                            self.seek(0.);
                        }
                        reset_positions(&mut self.state.progress);
                        self.progress_changed();
                        self.save();
                    }
                    Confirm::Restore(s) => {
                        self.begin_restore(s);
                    }
                    Confirm::Relink { original_id, track } => {
                        self.accept_relink(&original_id, track)
                    }
                    Confirm::BookNotStarted { key, .. } => self.mark_book_not_started(&key),
                    Confirm::RereadTags => self.start_reread(),
                    Confirm::RemoveStation(id) => self.remove_station_now(&id),
                    Confirm::ClearQueue => {
                        self.checkpoint();
                        self.cancel_validation();
                        self.audio.pause();
                        let cleared = std::mem::take(&mut self.state.queue);
                        let count = cleared.len();
                        self.view_revision = self.view_revision.wrapping_add(1);
                        self.state.was_playing = false;
                        if count > 0 {
                            self.record_undo(
                                "Clear queue",
                                state::UndoRestore::Queue(cleared),
                                format!(
                                    "Cleared the queue ({count} {})",
                                    if count == 1 { "file" } else { "files" }
                                ),
                            );
                        }
                        self.save();
                    }
                }
            }
        }
        if self.expanded_art {
            let mut open = true;
            let catalog = self
                .gui_cache
                .catalog(&self.state.library, self.library_revision);
            let (seed, audiobook) = art_subject(
                self.state.current.as_deref(),
                self.state.selected.as_deref(),
                &catalog,
            )
            .map(|track| (art_seed(track).to_owned(), track.audiobook))
            .unwrap_or_default();
            let caption = self.art_note.as_ref().and_then(|(_, note)| note.caption());
            egui::Window::new("Artwork")
                .open(&mut open)
                .default_size([640., 680.])
                .resizable(true)
                .show(ctx, |ui| {
                    if let Some((_, texture)) = &self.artwork {
                        ui.add(egui::Image::new(texture).fit_to_exact_size(ui.available_size()));
                    } else {
                        let text = caption
                            .unwrap_or_else(|| "No artwork is available for this file.".into());
                        let side = ui
                            .available_width()
                            .min(ui.available_height() - 28.)
                            .max(48.);
                        ui.vertical_centered(|ui| {
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::splat(side), egui::Sense::hover());
                            widgets::paint_default_art(
                                ui.painter(),
                                rect,
                                audiobook,
                                &seed,
                                ui.visuals().dark_mode,
                            );
                            ui.label(
                                RichText::new(text)
                                    .small()
                                    .color(ui.visuals().weak_text_color()),
                            );
                        });
                    }
                });
            self.expanded_art = open;
        }
        if self.about {
            if self.about_logo.is_none() {
                self.about_logo = Some(load_about_logo(ctx));
            }
            let mut close_requested = false;
            let mut reveal_profile = false;
            let logo = self.about_logo.as_ref().expect("logo load was cached");
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("bs-player-about"),
                dialog_placement(
                    ctx,
                    player_center,
                    monitor,
                    self.about_center_pending,
                    egui::ViewportBuilder::default()
                        .with_title("BS Player — About")
                        .with_window_level(level)
                        .with_decorations(true)
                        .with_resizable(true)
                        .with_inner_size([720., 740.])
                        .with_min_inner_size([480., 360.]),
                ),
                |ui, _class| {
                    center_dialog_once(ui.ctx(), player_center, monitor, &mut self.about_center_pending);
                    if ui.input(|input| input.viewport().close_requested()) {
                        close_requested = true;
                        return;
                    }
                    window_actions.extend(pressed_bindings(
                        ui.ctx(),
                        &self.state.settings.shortcuts,
                        None,
                        true,
                    ));
                    egui::CentralPanel::default()
                        .frame(egui::Frame::NONE.fill(appearance::workspace(ui)).inner_margin(12))
                        .show(ui, |ui| {
                    let content_rect = ui.available_rect_before_wrap();
                    let available = content_rect.size();
                    let footer_height =
                        (ui.text_style_height(&egui::TextStyle::Body) + 12.).max(34.) + 16.;
                    let body_height = (available.y - footer_height - 4.).max(1.);
                    egui::ScrollArea::both()
                        .id_salt("about-content")
                        .auto_shrink([false, false])
                        .max_height(body_height)
                        .max_width(available.x)
                        .show(ui, |ui| {
                            let body_width = (available.x - 16.).max(1.);
                            ui.set_min_width(body_width);
                            ui.set_max_width(body_width);
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                            ui.add_space(8.);
                            ui.vertical_centered(|ui| {
                                match logo {
                                    Ok(texture) => {
                                        let source = texture.size_vec2();
                                        let aspect = source.y / source.x.max(1.);
                                        let width = ui.available_width().min(580.).max(1.);
                                        let height = (width * aspect)
                                            .min((body_height * 0.5).max(32.));
                                        let width = height / aspect.max(0.001);
                                        ui.add(
                                            egui::Image::new(texture)
                                                .fit_to_exact_size(Vec2::new(width, height)),
                                        );
                                    }
                                    Err(_) => {
                                        ui.heading("BS Player");
                                        ui.label("Brand artwork could not be loaded.");
                                    }
                                }
                                ui.add_space(6.);
                                ui.label(
                                    RichText::new(format!(
                                        "BS Player {}",
                                        env!("CARGO_PKG_VERSION")
                                    ))
                                    .size(16.),
                                );
                                ui.label("Music and audiobook player.");
                                ui.hyperlink_to(
                                    "github.com/IronWolve/BS-Player",
                                    "https://github.com/IronWolve/BS-Player",
                                );
                            });
                            ui.add_space(10.);
                            ui.separator();
                            ui.label(format!("Build: {}", crate::cpu::build_profile()));
                            ui.label(format!("Build ID: {}", option_env!("BS_PLAYER_BUILD_ID").unwrap_or("unrecorded development build")));
                            ui.label(format!(
                                "This computer: {}",
                                ISA_SUPPORT.get_or_init(crate::cpu::isa_support)
                            ));
                            ui.label("Personal, non-commercial use only. See LICENSE for terms.");
                            ui.add_space(6.);
                            ui.label(RichText::new("Your profile").strong());
                            ui.add(egui::Label::new(self.profile.display().to_string()).wrap());
                            if ui.button("Open data folder").clicked() {
                                reveal_profile = true;
                            }
                            ui.add_space(6.);
                            egui::CollapsingHeader::new("Technical details")
                                .id_salt("about-technical-details")
                                .show(ui, |ui| {
                                    ui.label(format!(
                                        "Detected CPU features: {}",
                                        RUNTIME_FEATURES.get_or_init(crate::cpu::runtime_features)
                                    ));
                                    ui.label(crate::cpu::compiled_features());
                                    ui.label("x86-64-v1 / x86-64 is the baseline. x86-64-v3 uses AVX2 and x86-64-v4 uses AVX512; these instruction baselines support Intel and AMD CPUs with the required features. Detection is informational and does not provide an automatic compatibility fallback.");
                                    ui.separator();
                                    ui.label(ABOUT_AUDIO_NOTE);
                                    ui.label("Progress is sampled from played audio frames, so device buffering can add latency. Long or variable-bitrate files may seek differently depending on the codec and target platform.");
                                    ui.label("Audio: Rodio / CPAL / Symphonia. Interface: egui / eframe. Tags: Lofty.");
                                });
                        });
                    let footer_rect = egui::Rect::from_min_size(
                        egui::pos2(content_rect.left(), content_rect.bottom() - footer_height),
                        Vec2::new(available.x, footer_height),
                    );
                    let (top, bottom) = appearance::chrome_colors(ui);
                    ui.painter().add(appearance::gradient(footer_rect, top, bottom));
                    ui.painter().line_segment(
                        [footer_rect.left_top(), footer_rect.right_top()],
                        egui::Stroke::new(1., appearance::separator(ui)),
                    );
                    let button_rect = egui::Rect::from_min_max(
                        footer_rect.left_top() + Vec2::new(0., 6.),
                        footer_rect.right_bottom() - Vec2::new(0., 5.),
                    );
                    ui.scope_builder(
                        egui::UiBuilder::new()
                            .max_rect(button_rect)
                            .layout(egui::Layout::right_to_left(egui::Align::Center)),
                        |footer| {
                            footer.set_clip_rect(button_rect.intersect(footer.clip_rect()));
                            if widgets::text_button(footer, "Close").clicked() {
                                close_requested = true;
                            }
                        },
                    );
                        });
                },
            );
            if reveal_profile {
                if let Err(error) = media::reveal(&self.profile) {
                    self.error = Some(error);
                }
            }
            if close_requested {
                self.about = false;
            }
        }
        self.run_window_actions(ctx, window_actions);
    }

    /// Completes the asynchronous safety-copy step before a profile restore.
    pub(super) fn finish_restore(
        &mut self,
        session: Session,
        result: Result<(), String>,
        ctx: &egui::Context,
    ) {
        self.restore_pending = false;
        self.busy = self.busy.saturating_sub(1);
        if let Err(error) = result {
            self.add_message(format!(
                "Could not back up the current profile; restore cancelled: {error}"
            ));
            return;
        }
        self.install_restored_profile(session, ctx);
    }

    fn install_restored_profile(&mut self, session: Session, ctx: &egui::Context) {
        self.audio.pause();
        self.audio.settings(
            &settings::engine_settings(&self.state.settings),
            &settings::engine_settings(&session.settings),
        );
        self.invalidate_jobs();
        self.private_track = None;
        self.reset_station_runtime();
        self.ended_handled = false;
        self.artwork = None;
        self.seek_drag = None;
        self.details = None;
        self.tag_editor = None;
        self.bookmark_dialog = false;
        self.bookmark_target = None;
        self.playlist_dialog = None;
        self.actions.clear();
        install_restored_session(
            &mut self.state,
            session,
            &mut self.snapshot,
            &mut self.pending_position,
            &mut self.expected_audio_request,
            &mut self.loading_request,
        );
        self.invalidate_library();
        // A restored profile replaces editor snapshots too; no stale draft may
        // overwrite its EQ or other settings.
        self.options = None;
        self.equalizer = None;
        self.equalizer_dirty = false;
        self.suppress_autoplay = self
            .state
            .sleep_deadline
            .is_some_and(|deadline| deadline <= state::now());
        if self.suppress_autoplay {
            self.state.sleep_deadline = None;
            self.state.sleep_mode = "off".into();
        }
        self.state.was_playing = false;
        self.initial_restore = true;
        self.art_requested = None;
        self.restore_scroll = true;
        self.audio.volume(if self.state.muted {
            0.
        } else {
            self.state.volume
        });
        self.audio.speed(self.state.speed);
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(
            self.state.fullscreen,
        ));
        self.save();
    }
}
