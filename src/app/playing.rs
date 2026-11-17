//! The current playback workspace. Saved playlists are independent copies.
use super::*;

impl PlayerApp {
    pub(super) fn workspace_tabs(&self) -> &'static [&'static str] {
        let tabs = list_tabs(
            !self.current_chapters.is_empty(),
            self.current_lyrics.is_some(),
        );
        if self.state.section == state::PLAYING_SECTION {
            &tabs[1..]
        } else {
            tabs
        }
    }
    pub(super) fn show_playing(&mut self) {
        self.state.playing_workspace = true;
        self.state.section = state::PLAYING_SECTION.into();
        self.state.tab = "Queue".into();
        self.state.search.clear();
        self.state.scroll = 0.;
        self.restore_scroll = true;
        self.scroll_to_current = true;
    }

    pub(super) fn cancel_playing_drop(&mut self) {
        if let Some(cancel) = self.playing_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
            self.playing_request = self.playing_request.wrapping_add(1);
        }
    }

    pub(super) fn play_workspace(&mut self, ids: Vec<String>) {
        let ids = self.existing_ids(ids);
        if ids.is_empty() {
            return;
        }
        self.cancel_playing_drop();
        self.checkpoint();
        self.state.queue = ids;
        self.view_revision = self.view_revision.wrapping_add(1);
        self.activate_playing_workspace();
    }

    fn activate_playing_workspace(&mut self) {
        self.drop_preload();
        self.planned_next = None;
        self.state.shuffle_history.clear();
        self.state.ab_a = None;
        self.state.ab_b = None;
        self.show_playing();
        if let Some(first) = self.state.queue.first().cloned() {
            self.select_only(Some(first.clone()));
            self.load_track(&first, true, None);
        }
        self.save();
    }

    /// A new drop supersedes older drops, but the old queue stays usable until
    /// this worker returns a nonempty, uncancelled set of audio files.
    pub(super) fn start_playing_drop(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        if self.tag_save_running {
            self.set_status("Wait for the file-tag save to finish before opening dropped files.");
            return;
        }
        let audiobook = self.audiobook_context();
        self.initial_restore = false;
        self.cancel_playing_drop();
        self.playing_request = self.playing_request.wrapping_add(1);
        let request = self.playing_request;
        self.busy += 1;
        let cancel = self.scan.begin(1);
        self.playing_cancel = Some(Arc::clone(&cancel));
        let recursive = self.state.settings.recursive_scan;
        let excluded: Vec<String> = self
            .state
            .settings
            .excluded_extensions
            .split([',', ';'])
            .map(|part| part.trim().trim_start_matches('.').to_lowercase())
            .filter(|part| !part.is_empty())
            .collect();
        let tx = self.job_sender();
        self.set_status("Opening dropped files in Playing…");
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut progress = |progress| {
                    tx.send_quiet(Event::ScanProgress {
                        kind: ScanKind::Import,
                        progress,
                    })
                };
                let found = media::import_paths_cancellable(
                    paths,
                    audiobook,
                    recursive,
                    &excluded,
                    None,
                    true,
                    &cancel,
                    &mut progress,
                );
                found
            }));
            let mut found = result.unwrap_or_else(|_| media::PathImport {
                errors: vec!["The dropped files could not be read. Your existing workspace was kept.".into()],
                ..Default::default()
            });
            found.cancelled |= cancel.load(Ordering::Relaxed);
            tx.send(Event::PlayingImported {
                request,
                found,
                aliases: HashMap::new(),
            });
        });
    }

    pub(super) fn finish_playing_drop(
        &mut self,
        request: u64,
        mut found: media::PathImport,
        aliases: HashMap<PathBuf, PathBuf>,
    ) {
        self.busy = self.busy.saturating_sub(1);
        let last = self.scan.job_ended();
        if request != self.playing_request {
            if last {
                self.scan.finish_run();
            }
            return;
        }
        self.playing_cancel = None;
        self.scan.found = self.scan.found.saturating_add(found.found);
        self.scan.cancelled |= found.cancelled;
        if found.cancelled {
            self.set_status("Opening files cancelled. Your current workspace was kept.");
        } else if found.tracks.is_empty() {
            if !found.stations.is_empty() {
                self.add_playlist_stations(std::mem::take(&mut found.stations));
                if let Some(note) = self.station_import_note.take() {
                    self.set_status(note);
                }
                self.show_streams();
            } else {
                self.set_status("No playable files were found. Your current workspace was kept.");
            }
            if !found.errors.is_empty() {
                self.add_message(
                    found
                        .errors
                        .into_iter()
                        .take(12)
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
        } else {
            self.scan.added = self.scan.added.saturating_add(found.tracks.len());
            // Merge retains known IDs, file progress and user-edited metadata.
            // Only this latest successful drop is allowed to replace the queue.
            if !found.stations.is_empty() {
                self.add_playlist_stations(std::mem::take(&mut found.stations));
            }
            self.state.playing_workspace = true;
            self.merge_import_target(
                found.tracks,
                found.errors,
                false,
                aliases,
                false,
                false,
                true,
            );
            self.activate_playing_workspace();
        }
        if last {
            self.scan.finish_run();
        }
    }

    pub(super) fn playing_navigation(&mut self, ui: &mut egui::Ui) {
        let response = navigation(
            ui,
            state::PLAYING_SECTION,
            Icon::Play,
            self.state.section == state::PLAYING_SECTION,
        )
        .on_hover_text("Current playlist workspace. Drop audio files here to start playing.");
        if response.clicked() {
            self.show_playing();
            self.save();
        }
        if response.dnd_hover_payload::<String>().is_some() {
            ui.painter().rect_stroke(
                response.rect,
                4.,
                egui::Stroke::new(2., ui.visuals().hyperlink_color),
                egui::StrokeKind::Inside,
            );
        }
        if let Some(id) = response.dnd_release_payload::<String>() {
            let ids = if self.selection.contains(id.as_str()) {
                self.selected_rows().as_ref().clone()
            } else {
                vec![id.as_ref().clone()]
            };
            self.actions.push(Action::PlayWorkspace(ids));
        }
        response.context_menu(|ui| {
            if ui
                .add_enabled(
                    !self.state.queue.is_empty(),
                    egui::Button::new("Save playlist…"),
                )
                .clicked()
            {
                self.open_playlist_dialog(PlaylistDialogKind::FromQueue);
                ui.close();
            }
            if ui
                .add_enabled(
                    !self.state.queue.is_empty(),
                    egui::Button::new("Clear Playing"),
                )
                .clicked()
            {
                self.confirm = Some(Confirm::ClearQueue);
                ui.close();
            }
        });
        ui.separator();
    }

    pub(super) fn playing_tools(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.state.queue.is_empty(),
                    egui::Button::new("Save playlist…"),
                )
                .clicked()
            {
                self.open_playlist_dialog(PlaylistDialogKind::FromQueue);
            }
            if ui
                .add_enabled(
                    self.state.tab == "Queue" && self.state.selected.is_some(),
                    egui::Button::new("Remove selected"),
                )
                .clicked()
            {
                self.remove_selected();
            }
            if ui
                .add_enabled(!self.state.queue.is_empty(), egui::Button::new("Clear"))
                .clicked()
            {
                self.confirm = Some(Confirm::ClearQueue);
            }
        });
    }
}
