use super::*;

impl PlayerApp {
    pub(super) fn settings_action(&mut self, ctx: &egui::Context, action: SettingsAction) {
        match action {
            SettingsAction::OpenEqualizer => self.open_equalizer(ctx),
            SettingsAction::ExportBackup => self.export_backup(),
            SettingsAction::ImportBackup => self.import_backup(),
            SettingsAction::ExportProgress => self.export_progress(),
            SettingsAction::ImportProgress => self.import_progress(),
            SettingsAction::MoveFolder(root) => self.move_folder(root),
            SettingsAction::Rescan => self.rescan(),
            SettingsAction::OpenProfile => {
                let _ = std::fs::create_dir_all(&self.profile);
                if let Err(e) = media::reveal(&self.profile) {
                    self.error = Some(e);
                }
            }
            SettingsAction::ClearHistory => self.confirm = Some(Confirm::ClearHistory),
            SettingsAction::ResetProgress => self.confirm = Some(Confirm::ResetProgress),
            SettingsAction::ForgetRemovals => self.confirm = Some(Confirm::ForgetRemovals),
            SettingsAction::RereadTags => self.confirm = Some(Confirm::RereadTags),
            SettingsAction::RegisterAssociations => self.start_association_job(true),
            SettingsAction::UnregisterAssociations => self.start_association_job(false),
            SettingsAction::OpenDefaultApps => {
                let tx = self.job_sender();
                std::thread::spawn(move || {
                    if !winintegration::open_default_apps_settings() {
                        tx.send(Event::Message(
                            "Windows did not open the Default apps settings.".into(),
                        ));
                    }
                });
            }
        }
    }

    pub(super) fn actions(&mut self) {
        if self.actions.iter().any(|action| {
            matches!(
                action,
                Action::Queue(..) | Action::Favorite(..) | Action::MoveQueue(..)
            )
        }) {
            self.view_revision = self.view_revision.wrapping_add(1);
        }
        for action in std::mem::take(&mut self.actions) {
            match action {
                Action::PlayWorkspace(ids) => self.play_workspace(ids),
                Action::Play(id) => {
                    self.cancel_playing_drop();
                    self.load_track(&id, true, None);
                }
                Action::Select(id) => self.select_only(Some(id)),
                Action::SelectWith(id, mode) => self.select_with(id, mode),
                Action::FocusInSelection(id) => {
                    self.selection.set_primary(&id);
                    self.state.selected = self.selection.primary().map(str::to_owned);
                }
                Action::Bulk(action, ids) => self.run_bulk(action, ids),
                Action::PlayGroup(kind, key) => self.play_group(kind, &key),
                Action::OpenGroup(kind, key) => self.open_group(kind, &key),
                Action::Queue(id, next) => {
                    // Moving the playing track would change what "Next" plays.
                    state::queue_ids(
                        &mut self.state.queue,
                        self.state.current.as_deref(),
                        &[id],
                        next,
                    );
                }
                Action::Favorite(id) => {
                    if self.state.favorites.contains(&id) {
                        self.state.favorites.retain(|q| q != &id);
                    } else {
                        self.state.favorites.push(id);
                    }
                }
                Action::Details(id) => {
                    // Opening another file would silently drop unsaved tag edits.
                    let editing = self.details.as_ref().is_some_and(|draft| {
                        draft.id != id
                            && self
                                .state
                                .track(&draft.id)
                                .is_some_and(|saved| tags_differ(saved, draft))
                    });
                    if editing {
                        self.set_status("Save or cancel the open file details first.");
                    } else {
                        self.details = self.state.track(&id).cloned();
                    }
                }
                Action::Remove(id) => {
                    if self.state.settings.confirm_remove {
                        self.confirm = Some(Confirm::Remove(id));
                    } else {
                        self.remove_from_library(&id);
                    }
                }
                Action::RemoveFromQueue(id) => {
                    self.remove_from_queue(&[id]);
                }
                Action::AddToPlaylist(name, id) => self.add_to_playlist(&name, &[id]),
                Action::RemoveFromPlaylist(name, id) => {
                    self.remove_from_playlist(&name, &[id]);
                }
                Action::MovePlaylist(name, from, to) => {
                    let block = self.drag_block(&from);
                    let moved = if block.len() > 1 {
                        self.state.move_block_in_playlist(&name, &block, &from, &to)
                    } else {
                        self.state.move_in_playlist(&name, &from, &to)
                    };
                    if moved {
                        self.view_revision = self.view_revision.wrapping_add(1);
                    }
                }
                Action::StepPlaylist(name, id, up) => {
                    if self.state.step_in_playlist(&name, &id, up) {
                        self.view_revision = self.view_revision.wrapping_add(1);
                    }
                }
                Action::Relocate(id) => {
                    if let Some(track) = self.state.track(&id).cloned() {
                        let Some(guard) = self.begin_picker() else {
                            continue;
                        };
                        let tx = self.job_sender();
                        std::thread::spawn(move || {
                            let _guard = guard;
                            if let Some(path) = rfd::FileDialog::new()
                                .set_title("Locate the original audio file")
                                .pick_file()
                            {
                                tx.send(Event::Relocated {
                                    original_id: id,
                                    result: media::relink_candidate(&track, &path),
                                });
                            }
                        });
                    }
                }
                Action::MoveQueue(from, to) => {
                    let block = self.drag_block(&from);
                    if block.len() > 1 {
                        state::move_block(&mut self.state.queue, &block, &from, &to);
                        continue;
                    }
                    if let (Some(from), Some(to)) = (
                        self.state.queue.iter().position(|id| id == &from),
                        self.state.queue.iter().position(|id| id == &to),
                    ) {
                        let id = self.state.queue.remove(from);
                        self.state.queue.insert(to, id);
                    }
                }
                Action::Jump(id, seconds) => {
                    if self.state.track(&id).is_some() {
                        self.cancel_playing_drop();
                        self.load_track(&id, true, Some(seconds));
                    } else {
                        self.add_message(
                            "This bookmark's file is no longer in the library. Add the file again or remove the bookmark."
                                .into(),
                        );
                    }
                }
                Action::SeekChapter(id, seconds) => {
                    if self.state.current.as_deref() == Some(id.as_str()) {
                        self.jump_to_chapter(&id, seconds);
                    }
                }
                Action::SeekLine(id, seconds) => {
                    if self.state.current.as_deref() == Some(id.as_str()) {
                        self.cancel_playing_drop();
                        self.seek(seconds);
                    }
                }
                Action::ChapterBookmark(id, seconds, name) => {
                    self.add_bookmark_at(&id, seconds, name);
                }
                Action::EditBookmark(i) => {
                    if let Some(bookmark) = self.state.bookmarks.get(i).cloned() {
                        let title = self
                            .state
                            .track(&bookmark.track_id)
                            .map_or("Unavailable file", |track| track.display_title())
                            .to_owned();
                        self.bookmark_edit = Some(BookmarkEdit {
                            title,
                            name: bookmark.name.clone(),
                            note: bookmark.note.clone(),
                            original: bookmark,
                        });
                    }
                }
                Action::RemoveBookmark(i) => {
                    if let Some(bookmark) = self.state.bookmarks.get(i).cloned() {
                        if self.state.settings.confirm_remove {
                            self.confirm = Some(Confirm::RemoveBookmark(bookmark));
                        } else {
                            self.delete_bookmark(i);
                        }
                    }
                }
            }
        }
    }

    pub(super) fn accept_relink(&mut self, original_id: &str, mut track: Track) {
        let Some(existing) = Arc::make_mut(&mut self.state.library)
            .iter_mut()
            .find(|t| t.id == original_id)
        else {
            return;
        };
        track.id = original_id.to_owned();
        track.added_at = existing.added_at;
        track.preserve_library_tags(existing);
        preserve_book_speed(&mut self.state.book_speeds, existing, &track);
        let reconnected = track.path.clone();
        *existing = track;
        // A file the user reconnects is wanted again: scans may refresh it.
        self.state.forget_removal(&reconnected);
        self.invalidate_library();
        self.save();
        self.error = Some("File reconnected. Its saved position and bookmarks were kept.".into());
    }

    /// Options → Library → Forget all: a rescan may add the removed files back.
    pub(super) fn forget_removed_files(&mut self) {
        let count = self.state.removed_paths.len();
        self.state.removed_paths.clear();
        self.set_status(format!("Forgot {count} removed files"));
        self.save();
    }

    pub(super) fn remove_from_library(&mut self, id: &str) {
        self.remove_from_library_many(&[id.to_owned()]);
    }

    /// Takes files out of the library, the queue and the playlists and
    /// remembers their paths so a rescan does not bring them back. Their saved
    /// positions and bookmarks stay. The removal is recorded for Undo.
    pub(super) fn remove_from_library_many(&mut self, ids: &[String]) {
        let mut seen = HashSet::new();
        let ids: Vec<String> = ids
            .iter()
            .filter(|id| seen.insert(id.as_str()))
            .cloned()
            .collect();
        if ids.is_empty() {
            return;
        }
        if self
            .state
            .current
            .as_deref()
            .is_some_and(|current| ids.iter().any(|id| id == current))
        {
            self.audio.pause();
            self.checkpoint();
        }
        let label = match ids.as_slice() {
            [id] => format!(
                "Remove “{}”",
                short_label(
                    self.state
                        .track(id)
                        .map_or("this file", |track| track.display_title()),
                    UNDO_LABEL_CHARS
                )
            ),
            many => format!("Remove {} files", many.len()),
        };
        let removed = self.state.remove_files(&ids);
        {
            // The removed files are not selected any more.
            let gone: HashSet<&str> = ids.iter().map(String::as_str).collect();
            self.selection.retain_existing(|id| !gone.contains(id));
        }
        self.invalidate_library();
        let count = removed
            .as_ref()
            .map_or(ids.len(), |files| files.tracks.len());
        let note = if count == 1 {
            "Removed from library. The audio file and its saved progress were kept.".to_owned()
        } else {
            format!(
                "Removed {count} files from the library. The audio files and their saved progress were kept."
            )
        };
        match removed {
            Some(files) => {
                self.record_undo(label, state::UndoRestore::Files(Box::new(files)), note)
            }
            None => self.set_status(note),
        }
        self.save();
    }

    /// Records an undoable action and shows `note` for it, with the "Undo" link
    /// beside it. An entry that alone is over the undo memory limit is not kept;
    /// its note shows without the link.
    pub(super) fn record_undo(
        &mut self,
        label: impl Into<String>,
        restore: state::UndoRestore,
        note: impl Into<String>,
    ) {
        let kept = self.undo.push(state::UndoEntry {
            label: label.into(),
            restore,
        });
        if kept {
            self.set_undo_status(note);
        } else {
            self.set_status(note);
        }
    }

    /// Edit → Undo and the Undo key: takes back the newest undoable action.
    pub(super) fn undo_last(&mut self) {
        if self.tag_resume.is_some() {
            self.set_brief_status("Wait for file tags to finish before using Undo");
            return;
        }
        let Some(entry) = self.undo.pop() else {
            self.set_brief_status("Nothing to undo");
            return;
        };
        self.checkpoint();
        let outcome = self.state.apply_undo(entry.restore);
        if outcome.library {
            self.invalidate_library();
        }
        if outcome.view {
            self.view_revision = self.view_revision.wrapping_add(1);
        }
        if outcome.progress {
            self.progress_changed();
        }
        if let Some(id) = &outcome.current {
            // The loaded file is back: load it again, paused, where it stood. The
            // selection is what it was before the removal, not the loaded file.
            let selection = self.state.selected.clone();
            let position = self.state.progress.get(id).map_or(0., |p| p.seconds);
            self.load_track(id, false, Some(position));
            self.state.selected = selection;
        }
        if let Some((id, seconds)) = &outcome.seek {
            if self.state.current.as_deref() == Some(id.as_str()) {
                let mut progress = self.state.progress.get(id).cloned().unwrap_or_default();
                progress.seconds = *seconds;
                self.seek_progress(id, progress);
            }
        }
        if outcome.restored {
            self.set_status(outcome.note);
        } else {
            self.add_message(outcome.note);
        }
        self.save();
    }

    /// Takes files out of the queue and records it for Undo. Returns whether
    /// anything was in the queue to remove.
    pub(super) fn remove_from_queue(&mut self, ids: &[String]) -> bool {
        let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let entries: Vec<(usize, String)> = self
            .state
            .queue
            .iter()
            .enumerate()
            .filter(|(_, id)| wanted.contains(id.as_str()))
            .map(|(index, id)| (index, id.clone()))
            .collect();
        if entries.is_empty() {
            return false;
        }
        self.state.queue.retain(|id| !wanted.contains(id.as_str()));
        self.view_revision = self.view_revision.wrapping_add(1);
        let (label, note) = match ids {
            [id] => (
                format!(
                    "Remove “{}” from the queue",
                    short_label(
                        self.state
                            .track(id)
                            .map_or("this file", |track| track.display_title()),
                        UNDO_LABEL_CHARS
                    )
                ),
                "Removed from the queue".to_owned(),
            ),
            many => (
                format!("Remove {} files from the queue", many.len()),
                format!("Removed {} files from the queue", many.len()),
            ),
        };
        self.record_undo(label, state::UndoRestore::QueueEntries(entries), note);
        true
    }

    /// Takes a playlist out of the profile and records it for Undo.
    pub(super) fn remove_playlist(&mut self, index: usize) {
        if index >= self.state.playlists.len() {
            return;
        }
        let playlist = self.state.playlists.remove(index);
        let name = playlist.name.clone();
        self.view_revision = self.view_revision.wrapping_add(1);
        self.record_undo(
            format!("Remove playlist “{}”", short_label(&name, UNDO_LABEL_CHARS)),
            state::UndoRestore::Playlist { index, playlist },
            format!("Removed the playlist “{name}”"),
        );
        self.save();
    }

    /// Adds files to the playlist `name`, each at most once, and says what happened.
    pub(super) fn add_to_playlist(&mut self, name: &str, ids: &[String]) {
        match self.state.add_to_playlist(name, ids) {
            None => self.set_status("That playlist no longer exists"),
            Some((added, already)) => {
                let note = match (added, already) {
                    (0, _) if ids.len() == 1 => format!("Already in {name}"),
                    (0, _) => format!("All {} files are already in {name}", ids.len()),
                    (1, 0) => format!("Added to {name}"),
                    (added, 0) => format!("Added {added} files to {name}"),
                    (added, already) => {
                        format!("Added {added} to {name} ({already} already in it)")
                    }
                };
                self.set_status(note);
                if added > 0 {
                    self.view_revision = self.view_revision.wrapping_add(1);
                    self.save();
                }
            }
        }
    }

    /// Takes files out of the playlist `name` (not out of the library) and
    /// records it for Undo. Returns whether anything was in it to remove.
    pub(super) fn remove_from_playlist(&mut self, name: &str, ids: &[String]) -> bool {
        let removed = self.state.remove_from_playlist(name, ids);
        if removed.is_empty() {
            return false;
        }
        let count = removed.len();
        self.view_revision = self.view_revision.wrapping_add(1);
        self.record_undo(
            format!(
                "Remove from playlist “{}”",
                short_label(name, UNDO_LABEL_CHARS)
            ),
            state::UndoRestore::PlaylistEntries {
                name: name.to_owned(),
                entries: removed,
            },
            if count == 1 {
                format!("Removed from {name}")
            } else {
                format!("Removed {count} files from {name}")
            },
        );
        self.save();
        true
    }
}

/// A name for a copy of `base` that no playlist uses yet.
pub(super) fn unique_copy_name(playlists: &[Playlist], base: &str) -> String {
    state::unique_playlist_name(playlists, &format!("{base} copy"))
}

/// Longest text taken from a file or playlist name into an undo label.
pub(super) const UNDO_LABEL_CHARS: usize = 32;

/// The name of the playlist the Files list shows, if it shows one.
pub(super) fn playlist_section(state: &Session) -> Option<&str> {
    if state.tab == "Files" {
        state.section.strip_prefix("Playlist:")
    } else {
        None
    }
}

/// `text` cut to `max` characters, with an ellipsis when it was longer.
pub(super) fn short_label(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let head: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", head.trim_end())
    }
}
