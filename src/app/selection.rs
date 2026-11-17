use super::*;

impl PlayerApp {
    /// Delete: remove the selected rows from the queue or from the playlist on
    /// show (neither asks; Undo takes it back), or ask once to remove them from
    /// the library like the row menu does.
    pub(super) fn remove_selected(&mut self) {
        if self.state.section == state::STREAMS_SECTION {
            if let Some(id) = self.selected_station() {
                self.request_remove_station(&id);
            }
            return;
        }
        if !matches!(self.state.tab.as_str(), "Files" | "Queue") || self.browse_kind().is_some() {
            return;
        }
        // Rows hidden by the search or view are not removed.
        let ids = self.selected_rows();
        if ids.is_empty() {
            return;
        }
        let playlist = playlist_section(&self.state).map(str::to_owned);
        if self.state.tab == "Queue" || playlist.is_some() {
            let tracks = self.visible_tracks();
            let removed = if let Some(name) = &playlist {
                self.remove_from_playlist(name, &ids)
            } else {
                self.remove_from_queue(&ids)
            };
            if removed {
                // Keep going down the list: select the row that takes its place.
                let neighbour = row_after_removal(&tracks, &ids);
                self.select_only(neighbour.map(|index| tracks[index].id.clone()));
            }
        } else if let [id] = ids.as_slice() {
            self.actions.push(Action::Remove(id.clone()));
        } else {
            self.actions
                .push(Action::Bulk(BulkAction::RemoveFromLibrary, (*ids).clone()));
        }
    }

    /// The rows a drag of `dragged` moves: all selected rows when it is one of
    /// several, else none (the row moves alone).
    pub(super) fn drag_block(&mut self, dragged: &str) -> Arc<Vec<String>> {
        if self.selection.len() > 1 && self.selection.contains(dragged) {
            self.selected_rows()
        } else {
            Arc::new(Vec::new())
        }
    }

    /// Makes `id` (or nothing) the whole selection.
    pub(super) fn select_only(&mut self, id: Option<String>) {
        match &id {
            Some(id) => self.selection.click(id),
            None => self.selection.clear(),
        }
        self.state.selected = id;
    }

    /// Brings the selection in step with the list: other code that changed
    /// `Session::selected` collapses it to that row; after the list was rebuilt
    /// it loses the rows that are not displayed any more, and a change of
    /// section or tab keeps only the primary row.
    pub(super) fn sync_selection(&mut self) {
        self.selection.coordinate(self.state.selected.as_deref());
        let tracks = self.visible_tracks();
        let generation = self.gui_cache.view_generation();
        if generation == self.selection_view {
            return;
        }
        self.selection_view = generation;
        let moved = self.selection_scope.0 != self.state.section
            || self.selection_scope.1 != self.state.tab;
        if moved {
            self.selection_scope = (self.state.section.clone(), self.state.tab.clone());
            if self.selection.len() > 1 {
                let primary = self.selection.primary().map(str::to_owned);
                self.selection.clear_to(primary.as_deref());
            }
        } else {
            self.selection
                .retain_displayed(tracks.iter().map(|track| track.id.as_str()));
        }
    }

    /// The selected rows, in the order the list shows them. Kept until the
    /// selection or the list changes.
    pub(super) fn selected_rows(&mut self) -> Arc<Vec<String>> {
        self.sync_selection();
        let key = (self.selection.revision(), self.gui_cache.view_generation());
        if let Some((made_for, rows)) = &self.selected_cache {
            if *made_for == key {
                return Arc::clone(rows);
            }
        }
        let tracks = self.visible_tracks();
        let rows = Arc::new(
            self.selection
                .ordered(tracks.iter().map(|track| track.id.as_str())),
        );
        self.selected_cache = Some((key, Arc::clone(&rows)));
        rows
    }

    /// How many rows are selected and how long they play together (files of
    /// unknown length count nothing), for the status bar.
    pub(super) fn selection_totals(&mut self) -> (usize, f64) {
        let rows = self.selected_rows();
        let key = (self.selection.revision(), self.gui_cache.view_generation());
        if let Some((made_for, count, seconds)) = self.selection_summary {
            if made_for == key {
                return (count, seconds);
            }
        }
        let catalog = self
            .gui_cache
            .catalog(&self.state.library, self.library_revision);
        let seconds = rows
            .iter()
            .filter_map(|id| catalog.track(id))
            .map(|track| known_seconds(track.duration))
            .sum();
        self.selection_summary = Some((key, rows.len(), seconds));
        (rows.len(), seconds)
    }

    /// A click on a row with Ctrl/Cmd or Shift held, or one of the keys that
    /// extend the selection.
    pub(super) fn select_with(&mut self, id: String, mode: SelectMode) {
        self.sync_selection();
        let tracks = self.visible_tracks();
        match mode {
            SelectMode::Plain => self.selection.click(&id),
            SelectMode::Toggle => {
                if !self.selection.toggle(&id)
                    && self.selection.primary().is_none()
                    && !self.selection.is_empty()
                {
                    // The primary row left: the nearest selected row takes over.
                    let at = tracks.iter().position(|track| track.id == id);
                    let near = at.and_then(|at| {
                        self.selection
                            .nearest_selected(tracks.len(), at, |row| tracks[row].id.as_str())
                    });
                    if let Some(near) = near {
                        self.selection.focus(&tracks[near].id);
                    }
                }
            }
            SelectMode::Range | SelectMode::RangeAdd => {
                let anchor = self
                    .selection
                    .anchor()
                    .map(str::to_owned)
                    .or_else(|| self.state.selected.clone());
                match anchor {
                    Some(anchor) => self.selection.range(
                        &anchor,
                        &id,
                        tracks.iter().map(|track| track.id.as_str()),
                        mode == SelectMode::RangeAdd,
                    ),
                    None => self.selection.click(&id),
                }
            }
        }
        self.state.selected = self.selection.primary().map(str::to_owned);
    }

    /// The files of `ids` that are still in the library, each once.
    pub(super) fn existing_ids(&mut self, ids: Vec<String>) -> Vec<String> {
        let catalog = self
            .gui_cache
            .catalog(&self.state.library, self.library_revision);
        let mut seen = HashSet::new();
        ids.into_iter()
            .filter(|id| catalog.track(id).is_some() && seen.insert(id.clone()))
            .collect()
    }

    /// What a bulk command of the row menu does. The ids are checked against
    /// the library first: files removed meanwhile are skipped.
    pub(super) fn run_bulk(&mut self, action: BulkAction, ids: Vec<String>) {
        let ids = self.existing_ids(ids);
        if ids.is_empty() {
            return;
        }
        let files = files_text(ids.len());
        match action {
            BulkAction::Play => {
                self.cancel_playing_drop();
                // As a double-click does: the files become the queue, except in
                // the Queue view, where they are queued already.
                if self.state.tab != "Queue" {
                    self.state.playing_workspace = false;
                    self.state.queue = ids.clone();
                    self.view_revision = self.view_revision.wrapping_add(1);
                }
                self.load_track(&ids[0], true, None);
            }
            BulkAction::PlayNext | BulkAction::AddToQueue => {
                let next = action == BulkAction::PlayNext;
                let placed = state::queue_ids(
                    &mut self.state.queue,
                    self.state.current.as_deref(),
                    &ids,
                    next,
                );
                self.view_revision = self.view_revision.wrapping_add(1);
                self.set_brief_status(match (placed, next) {
                    (0, _) => "The playing file stays where it is in the queue".to_owned(),
                    (placed, true) => format!("{} play next", files_text(placed)),
                    (placed, false) => format!("Added {} to the queue", files_text(placed)),
                });
            }
            BulkAction::AddToPlaylist(name) => self.add_to_playlist(&name, &ids),
            BulkAction::ToggleFavorite => {
                let all_marked = {
                    let marked: HashSet<&str> =
                        self.state.favorites.iter().map(String::as_str).collect();
                    ids.iter().all(|id| marked.contains(id.as_str()))
                };
                if all_marked {
                    let gone: HashSet<&str> = ids.iter().map(String::as_str).collect();
                    self.state
                        .favorites
                        .retain(|favorite| !gone.contains(favorite.as_str()));
                    self.set_brief_status(format!("Removed {files} from the favorites"));
                } else {
                    let marked: HashSet<String> = self.state.favorites.iter().cloned().collect();
                    self.state
                        .favorites
                        .extend(ids.iter().filter(|id| !marked.contains(*id)).cloned());
                    self.set_brief_status(format!("Added {files} to the favorites"));
                }
                self.view_revision = self.view_revision.wrapping_add(1);
                self.save();
            }
            BulkAction::Rate(rating) => {
                for id in &ids {
                    self.state.ratings.insert(id.clone(), rating);
                }
                if self.state.sort_column == columns::COL_RATING {
                    self.view_revision = self.view_revision.wrapping_add(1);
                }
                self.set_brief_status(if rating == 0 {
                    format!("Cleared the rating of {files}")
                } else {
                    format!("Rated {files} {}", "★".repeat(usize::from(rating)))
                });
                self.save();
            }
            BulkAction::MarkFinished => {
                self.checkpoint();
                let before = self.state.capture_progress(&ids);
                let record_history = !self.state.settings.private_listening;
                let marked = self
                    .state
                    .mark_files_finished(&ids, state::now(), record_history);
                self.progress_changed();
                self.save();
                self.record_undo(
                    format!("Mark {files} finished"),
                    state::UndoRestore::Progress {
                        before,
                        reposition: false,
                    },
                    format!("Marked {} finished", files_text(marked)),
                );
            }
            BulkAction::MarkNotStarted => {
                let playing_here = self
                    .state
                    .current
                    .as_deref()
                    .is_some_and(|current| ids.iter().any(|id| id == current));
                self.checkpoint();
                let before = self.state.capture_progress(&ids);
                if playing_here {
                    self.seek(0.);
                }
                let reset = self.state.mark_files_not_started(&ids);
                self.progress_changed();
                self.save();
                if reset == 0 {
                    self.set_brief_status("Those files had no progress");
                } else {
                    self.record_undo(
                        format!("Mark {files} not started"),
                        state::UndoRestore::Progress {
                            before,
                            reposition: playing_here,
                        },
                        format!("Reset the progress of {}", files_text(reset)),
                    );
                }
            }
            BulkAction::RemoveFromQueue => {
                self.remove_from_queue(&ids);
            }
            BulkAction::RemoveFromPlaylist(name) => {
                self.remove_from_playlist(&name, &ids);
            }
            BulkAction::RemoveFromLibrary => {
                if self.state.settings.confirm_remove {
                    self.confirm = Some(Confirm::RemoveMany(ids));
                } else {
                    self.remove_from_library_many(&ids);
                }
            }
        }
    }

    /// The paths of `ids`, one per line, as the row menu copies them.
    pub(super) fn paths_text(&mut self, ids: &[String]) -> String {
        let catalog = self
            .gui_cache
            .catalog(&self.state.library, self.library_revision);
        ids.iter()
            .filter_map(|id| catalog.track(id))
            .map(|track| media::display_path(&track.path))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The right-click menu of a file row. With several rows selected and this
    /// one among them it works on all of them; otherwise on this row only.
    pub(super) fn row_menu(
        &mut self,
        ui: &mut egui::Ui,
        track: &Track,
        in_playlist: Option<&str>,
        reorderable: bool,
    ) {
        if self.selection.len() > 1 && self.selection.contains(&track.id) {
            let ids = self.selected_rows();
            if ids.len() > 1 {
                self.bulk_row_menu(ui, track, &ids, in_playlist);
                return;
            }
        }
        self.single_row_menu(ui, track, in_playlist, reorderable);
    }

    pub(super) fn single_row_menu(
        &mut self,
        ui: &mut egui::Ui,
        track: &Track,
        in_playlist: Option<&str>,
        reorderable: bool,
    ) {
        if ui.button("Play / resume").clicked() {
            self.actions.push(Action::Play(track.id.clone()));
            ui.close();
        }
        if ui.button("Play next").clicked() {
            self.actions.push(Action::Queue(track.id.clone(), true));
            ui.close();
        }
        if ui.button("Add to queue").clicked() {
            self.actions.push(Action::Queue(track.id.clone(), false));
            ui.close();
        }
        self.add_to_playlist_menu(ui, "Add to playlist", std::slice::from_ref(&track.id));
        let favorite = self.state.favorites.contains(&track.id);
        if ui
            .button(if favorite {
                "Remove favorite"
            } else {
                "Add favorite"
            })
            .clicked()
        {
            self.actions.push(Action::Favorite(track.id.clone()));
            ui.close();
        }
        if let Some(rating) = rating_menu(ui, "Rating") {
            self.state.ratings.insert(track.id.clone(), rating);
            if self.state.sort_column == columns::COL_RATING {
                self.view_revision = self.view_revision.wrapping_add(1);
            }
            self.save();
        }
        ui.separator();
        if ui.button("File details…").clicked() {
            self.actions.push(Action::Details(track.id.clone()));
            ui.close();
        }
        if ui.button("Copy path").clicked() {
            ui.ctx().copy_text(media::display_path(&track.path));
            ui.close();
        }
        if ui.button("Reveal in file manager").clicked() {
            if let Err(error) = media::reveal(&track.path) {
                self.error = Some(error);
            }
            ui.close();
        }
        if self.state.tab == "Queue" {
            if ui.button("Remove from queue").clicked() {
                self.actions.push(Action::RemoveFromQueue(track.id.clone()));
                ui.close();
            }
        } else {
            if let Some(name) = in_playlist {
                if reorderable {
                    if ui.button("Move up in playlist").clicked() {
                        self.actions.push(Action::StepPlaylist(
                            name.to_owned(),
                            track.id.clone(),
                            true,
                        ));
                        ui.close();
                    }
                    if ui.button("Move down in playlist").clicked() {
                        self.actions.push(Action::StepPlaylist(
                            name.to_owned(),
                            track.id.clone(),
                            false,
                        ));
                        ui.close();
                    }
                }
                if ui.button("Remove from this playlist").clicked() {
                    self.actions.push(Action::RemoveFromPlaylist(
                        name.to_owned(),
                        track.id.clone(),
                    ));
                    ui.close();
                }
            }
            if ui.button("Remove from library…").clicked() {
                self.actions.push(Action::Remove(track.id.clone()));
                ui.close();
            }
        }
    }

    /// The row menu for several selected rows: commands on all of them carry the
    /// count; the ones that only make sense for one file work on the clicked row.
    pub(super) fn bulk_row_menu(
        &mut self,
        ui: &mut egui::Ui,
        track: &Track,
        ids: &Arc<Vec<String>>,
        in_playlist: Option<&str>,
    ) {
        let files = format!("{} files", ids.len());
        let bulk = |this: &mut Self, ui: &mut egui::Ui, label: String, action: BulkAction| {
            if ui.button(label).clicked() {
                this.actions.push(Action::Bulk(action, (**ids).clone()));
                ui.close();
            }
        };
        bulk(
            self,
            ui,
            format!("Play selected ({files})"),
            BulkAction::Play,
        );
        bulk(
            self,
            ui,
            format!("Play next ({files})"),
            BulkAction::PlayNext,
        );
        bulk(
            self,
            ui,
            format!("Add to queue ({files})"),
            BulkAction::AddToQueue,
        );
        self.add_to_playlist_menu(ui, &format!("Add to playlist ({files})"), ids);
        let all_marked = {
            let marked: HashSet<&str> = self.state.favorites.iter().map(String::as_str).collect();
            ids.iter().all(|id| marked.contains(id.as_str()))
        };
        bulk(
            self,
            ui,
            if all_marked {
                format!("Remove favorite ({files})")
            } else {
                format!("Add favorite ({files})")
            },
            BulkAction::ToggleFavorite,
        );
        if let Some(rating) = rating_menu(ui, &format!("Rating ({files})")) {
            self.actions
                .push(Action::Bulk(BulkAction::Rate(rating), (**ids).clone()));
        }
        ui.separator();
        bulk(
            self,
            ui,
            format!("Mark finished ({files})"),
            BulkAction::MarkFinished,
        );
        bulk(
            self,
            ui,
            format!("Mark not started ({files})"),
            BulkAction::MarkNotStarted,
        );
        ui.separator();
        if ui
            .button("File details…")
            .on_hover_text("Shows the file you clicked")
            .clicked()
        {
            self.actions.push(Action::Details(track.id.clone()));
            ui.close();
        }
        if ui.button(format!("Copy paths ({files})")).clicked() {
            let text = self.paths_text(ids);
            ui.ctx().copy_text(text);
            ui.close();
        }
        if ui
            .button("Reveal in file manager")
            .on_hover_text("Shows the file you clicked")
            .clicked()
        {
            if let Err(error) = media::reveal(&track.path) {
                self.error = Some(error);
            }
            ui.close();
        }
        if self.state.tab == "Queue" {
            bulk(
                self,
                ui,
                format!("Remove from queue ({files})"),
                BulkAction::RemoveFromQueue,
            );
        } else {
            if let Some(name) = in_playlist {
                bulk(
                    self,
                    ui,
                    format!("Remove from this playlist ({files})"),
                    BulkAction::RemoveFromPlaylist(name.to_owned()),
                );
            }
            bulk(
                self,
                ui,
                format!("Remove from library… ({files})"),
                BulkAction::RemoveFromLibrary,
            );
        }
    }

    /// The "Add to playlist" submenu for `ids`: one entry per playlist, and
    /// "New playlist…" that starts one with these files.
    pub(super) fn add_to_playlist_menu(&mut self, ui: &mut egui::Ui, label: &str, ids: &[String]) {
        ui.menu_button(label, |ui| {
            egui::ScrollArea::vertical()
                .max_height(260.)
                .show(ui, |ui| {
                    let names: Vec<String> = self
                        .state
                        .playlists
                        .iter()
                        .map(|list| list.name.clone())
                        .collect();
                    for name in &names {
                        if ui.button(name).clicked() {
                            self.actions.push(match ids {
                                [id] => Action::AddToPlaylist(name.clone(), id.clone()),
                                _ => Action::Bulk(
                                    BulkAction::AddToPlaylist(name.clone()),
                                    ids.to_vec(),
                                ),
                            });
                            ui.close();
                        }
                    }
                    if !names.is_empty() {
                        ui.separator();
                    }
                    if ui.button("New playlist…").clicked() {
                        self.open_playlist_dialog(PlaylistDialogKind::New {
                            tracks: ids.to_vec(),
                        });
                        ui.close();
                    }
                });
        });
    }

    /// The queue becomes the displayed list, as a double-click does outside the
    /// Queue view.
    pub(super) fn queue_displayed(&mut self, tracks: &VisibleView) {
        self.cancel_playing_drop();
        if self.state.tab != "Queue" {
            self.state.playing_workspace = false;
            self.state.queue = tracks.iter().map(|t| t.id.clone()).collect();
            self.view_revision = self.view_revision.wrapping_add(1);
        }
    }
}
