use super::*;

impl PlayerApp {
    /// Writes the pre-restore safety copy off the UI thread, then lets the
    /// matching session generation either keep or replace the current profile.
    pub(super) fn begin_restore(&mut self, session: Session) {
        if self.restore_pending {
            self.set_brief_status("A profile restore is already being prepared");
            return;
        }
        if self.busy > 0 || self.scan.running() || self.tag_save_running || self.tag_resume.is_some() {
            self.confirm = Some(Confirm::Restore(session));
            self.set_status("Wait for the current scan or save to finish before restoring a profile");
            return;
        }
        self.checkpoint();
        let profile = self.profile.clone();
        let current = self.state.clone();
        let tx = self.job_sender();
        self.restore_pending = true;
        self.busy += 1;
        self.set_status("Saving the current profile before restore…");
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                write_restore_copy(&profile, &current)
            }))
            .unwrap_or_else(|_| Err("the safety copy stopped unexpectedly".to_owned()));
            tx.send(Event::RestorePrepared {
                session: Box::new(session),
                result,
            });
        });
    }

    pub(super) fn export_playlist(&mut self) {
        let tracks: Vec<_> = self
            .state
            .queue
            .iter()
            .filter_map(|id| self.state.track(id).cloned())
            .collect();
        let relative = self.state.settings.playlist_relative_paths;
        let Some(guard) = self.begin_picker() else {
            return;
        };
        let tx = self.job_sender();
        std::thread::spawn(move || {
            let _guard = guard;
            if let Some(path) = rfd::FileDialog::new()
                .set_file_name("playlist.m3u8")
                .add_filter("M3U8 playlist", &["m3u8"])
                .add_filter("PLS playlist", &["pls"])
                .save_file()
            {
                let result = media::write_playlist(&path, &tracks, relative);
                let _ = tx.send(Event::Message(
                    result
                        .map(|_| "Playlist exported".into())
                        .unwrap_or_else(|e| format!("Playlist export failed: {e}")),
                ));
            }
        });
    }

    pub(super) fn export_backup(&mut self) {
        let Some(guard) = self.begin_picker() else {
            return;
        };
        self.checkpoint();
        let session = self.state.clone();
        let tx = self.job_sender();
        std::thread::spawn(move || {
            let _guard = guard;
            if let Some(path) = rfd::FileDialog::new()
                .set_file_name("bs-player-backup.json")
                .add_filter("BS Player backup", &["json"])
                .save_file()
            {
                let _ = tx.send(Event::Message(
                    state::export(&path, &session)
                        .map(|_| "Backup exported".into())
                        .unwrap_or_else(|e| format!("Backup export failed: {e}")),
                ));
            }
        });
    }

    pub(super) fn import_backup(&mut self) {
        let Some(guard) = self.begin_picker() else {
            return;
        };
        let tx = self.job_sender();
        std::thread::spawn(move || {
            let _guard = guard;
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("BS Player backup", &["json"])
                .pick_file()
            {
                let _ = tx.send(Event::Backup(state::import(&path)));
            }
        });
    }

    /// Options → Backup & privacy → Export listening progress…: writes the
    /// portable progress file (no paths) where the user chooses.
    pub(super) fn export_progress(&mut self) {
        let Some(guard) = self.begin_picker() else {
            return;
        };
        self.checkpoint();
        let session = self.state.clone();
        let skip = self.private_track.clone();
        let tx = self.job_sender();
        std::thread::spawn(move || {
            let _guard = guard;
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Export listening progress")
                .set_file_name("bs-player-progress.json")
                .add_filter("BS Player progress", &["json"])
                .save_file()
            {
                let note = match session.try_export_progress(skip.as_deref(), state::now()) {
                    Ok(file) => match state::write_progress_file(&path, &file) {
                        Ok(()) => {
                            let marks: usize = file.entries.iter().map(|e| e.bookmarks.len()).sum();
                            format!(
                                "Exported listening progress for {} · {} · {}",
                                files_text(file.entries.len()),
                                if marks == 1 {
                                    "1 bookmark".to_owned()
                                } else {
                                    format!("{marks} bookmarks")
                                },
                                path.file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                                    .unwrap_or_default()
                            )
                        }
                        Err(error) => format!("The progress export failed: {error}"),
                    },
                    Err(error) => error,
                };
                tx.send(Event::Message(note));
            }
        });
    }

    /// Options → Backup & privacy → Import listening progress…: the file is read
    /// on a worker; `begin_progress_import` takes over when it is read.
    pub(super) fn import_progress(&mut self) {
        let Some(guard) = self.begin_picker() else {
            return;
        };
        let tx = self.job_sender();
        std::thread::spawn(move || {
            let _guard = guard;
            if let Some(path) = rfd::FileDialog::new()
                .set_title("Import listening progress")
                .add_filter("BS Player progress", &["json"])
                .pick_file()
            {
                let result = std::panic::catch_unwind(|| state::read_progress_file(&path))
                    .unwrap_or_else(|_| Err("The progress file could not be read.".to_owned()));
                tx.send(Event::ProgressFileRead(result));
            }
        });
    }

    /// A progress file was read. A copy of the whole profile goes to
    /// `before-import-progress.json` first, off the UI thread; the file is not
    /// applied when that copy cannot be written.
    pub(super) fn begin_progress_import(&mut self, file: state::ProgressFile) {
        self.checkpoint();
        let session = self.state.clone();
        let copy = self.profile.join(PROGRESS_IMPORT_COPY);
        let tx = self.job_sender();
        self.busy += 1;
        self.set_status("Importing listening progress…");
        std::thread::spawn(move || {
            // The answer must always arrive, or `busy` never returns to zero.
            let safety = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                state::export(&copy, &session)
            }))
            .unwrap_or_else(|_| Err("the copy stopped unexpectedly".to_owned()));
            drop(session);
            tx.send(Event::ProgressImportReady {
                file: Box::new(file),
                safety,
            });
        });
    }

    /// Merges a progress file into the profile (see `Session::apply_progress_import`)
    /// as one step for Undo when the touched entries are small enough to keep.
    pub(super) fn apply_progress_file(&mut self, file: &state::ProgressFile) {
        if self.tag_resume.is_some() {
            self.set_status("Wait for file tags to finish before importing listening progress");
            return;
        }
        self.checkpoint();
        let skip = self.private_track.clone();
        let outcome = self.state.apply_progress_import(file, skip.as_deref());
        let summary = outcome.summary();
        let changed = outcome.undo.is_some();
        if let Some((id, seconds)) = &outcome.seek {
            if self.state.current.as_deref() == Some(id.as_str()) {
                let progress = self.state.progress.get(id).cloned().unwrap_or_default();
                debug_assert_eq!(progress.seconds, *seconds);
                self.seek_progress(id, progress);
            }
        }
        let mut message = summary.clone();
        match outcome.undo {
            Some(undo) => {
                self.progress_changed();
                self.view_revision = self.view_revision.wrapping_add(1);
                let kept = self.undo.push(state::UndoEntry {
                    label: "Import listening progress".to_owned(),
                    restore: state::UndoRestore::ProgressImport(Box::new(undo)),
                });
                if kept {
                    self.set_undo_status(summary);
                    message.push_str("\nEdit → Undo takes this back.");
                } else {
                    self.set_status(summary);
                    message.push_str(
                        "\nThis changed too much to undo here; the copy of the profile saved just before is the way back.",
                    );
                }
            }
            None => self.set_status(summary),
        }
        message.push_str(&format!(
            "\nA copy of your profile from just before is in {PROGRESS_IMPORT_COPY} in the profile folder."
        ));
        self.save();
        if changed || file.entries.len() + file.book_speeds.len() > 0 {
            self.add_message(message);
        }
    }

    /// "Move folder…": picks the folder the files of the watched folder `old`
    /// were moved to; `start_move_folder` takes over.
    pub(super) fn move_folder(&mut self, old: PathBuf) {
        if self.busy > 0 || self.scan.running() {
            self.set_status("A scan or save is running; move the folder when it ends");
            return;
        }
        let Some(guard) = self.begin_picker() else {
            return;
        };
        let tx = self.job_sender();
        std::thread::spawn(move || {
            let _guard = guard;
            if let Some(new) = rfd::FileDialog::new()
                .set_title("Choose the new location of the folder")
                .pick_folder()
            {
                tx.send(Event::MoveFolderChosen { old, new });
            }
        });
    }

    /// Looks in `new` for the files of every library record below the watched
    /// folder `old`, checks each candidate by its content on a worker and then
    /// points the records at the new place (`finish_move_folder`). The profile
    /// is copied to `before-move-folder.json` first; without that copy nothing
    /// starts. No file on disk is touched.
    pub(super) fn start_move_folder(&mut self, old: PathBuf, new: PathBuf) {
        let Some(job) = self.prepare_move_folder(old, new) else {
            return;
        };
        self.busy += 1;
        let cancel = self.scan.begin(1);
        self.set_status("Looking through the new folder…");
        self.spawn_move_folder(job, cancel);
    }

    /// Checks the two folders and collects what the worker of a folder move needs
    /// (`None`, with a message, when it cannot start).
    pub(super) fn prepare_move_folder(&mut self, old: PathBuf, new: PathBuf) -> Option<MoveJob> {
        if self.busy > 0 || self.scan.running() {
            self.add_message(
                "A scan or save is running. Move the folder when it has ended.".to_owned(),
            );
            return None;
        }
        if new.to_str().is_none() {
            self.add_message(format!("{NOT_UNICODE_FOLDER}: {}", new.display()));
            return None;
        }
        let windows = cfg!(windows);
        let (old_key, new_key) = (state::removal_key(&old), state::removal_key(&new));
        let inside =
            |outer: &str, inner: &str| media::relative_key(outer, inner, windows).is_some();
        if old_key == new_key || inside(&old_key, &new_key) || inside(&new_key, &old_key) {
            self.add_message(
                "Choose a folder other than the old one that is not inside it and does not contain it."
                    .to_owned(),
            );
            return None;
        }
        self.checkpoint();
        let (wants, effective_old) = self.state.relocation_wants(&old);
        Some(MoveJob {
            wants,
            effective_old,
            session: self.state.clone(),
            copy: self.profile.join(MOVE_FOLDER_COPY),
            stored_root: old,
            new_root: new,
        })
    }

    /// The worker of a folder move: the safety copy first, then the search and
    /// the content checks. It always ends with a `FolderMoved` event.
    pub(super) fn spawn_move_folder(&self, job: MoveJob, cancel: Arc<AtomicBool>) {
        let tx = self.job_sender();
        std::thread::spawn(move || {
            let MoveJob {
                wants,
                effective_old,
                session,
                copy,
                stored_root,
                new_root,
            } = job;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                state::export(&copy, &session).map_err(|error| {
                    format!(
                        "Nothing was moved: a copy of your profile could not be saved first ({error})."
                    )
                })?;
                drop(session);
                let mut report = |progress| {
                    tx.send_quiet(Event::ScanProgress {
                        kind: ScanKind::Move,
                        progress,
                    })
                };
                Ok(media::relocate_files(
                    &wants,
                    &effective_old,
                    &new_root,
                    &cancel,
                    &mut report,
                ))
            }))
            .unwrap_or_else(|_| Err("The move stopped unexpectedly; nothing was changed.".to_owned()));
            // The answer must always arrive, or `busy` never returns to zero.
            tx.send(Event::FolderMoved(Box::new(FolderMove {
                stored_root,
                new_root,
                result,
            })));
        });
    }

    /// Applies a finished folder move to the profile and says what happened.
    pub(super) fn finish_move_folder(&mut self, finished: FolderMove) {
        let FolderMove {
            stored_root,
            new_root,
            result,
        } = finished;
        let report = match result {
            Ok(report) => report,
            Err(error) => {
                self.add_message(error);
                return;
            }
        };
        self.checkpoint();
        // The folder is replaced only when the whole job ran; a stopped job
        // keeps the old folder so the rest can be moved later.
        let playing = self.playback_intent();
        let position = self.position();
        let loaded = self.state.current_track().map(|track| (track.id.clone(), track.path.clone()));
        let replace = !report.cancelled && (report.total == 0 || !report.accepted.is_empty());
        let applied =
            self.state
                .apply_relocation(&report.accepted, &stored_root, &new_root, replace);
        if let Some((id, old_path)) = loaded {
            if self.state.current.as_deref() == Some(&id)
                && self.state.track(&id).is_some_and(|track| track.path != old_path)
            {
                // The decoder owns its own path. Updating just the library
                // would make the next seek reopen the now-missing old location.
                let progress = self.state.progress.get(&id).cloned();
                self.load_track(&id, playing, Some(position));
                if let (Some(progress), Some(request)) = (progress, self.loading_request) {
                    self.pending_progress_restore = Some((request, id, progress));
                }
            }
        }
        if applied.reconnected + applied.merged > 0 {
            self.invalidate_library();
            self.progress_changed();
            self.thumbs.reset();
            self.art_requested = None;
            if let Some(draft) = self.details.as_mut() {
                if let Some(record) = self.state.track(&draft.id) {
                    draft.path = record.path.clone();
                    draft.file_size = record.file_size;
                    draft.modified = record.modified;
                    draft.modified_nanos = record.modified_nanos;
                }
            }
        }
        let reconnected = applied.reconnected + applied.merged;
        let mut note = format!(
            "Reconnected {} of {} files · {} not found · {} different",
            count_text(reconnected),
            count_text(report.total),
            count_text(report.missing),
            count_text(report.different)
        );
        let mut message = note.clone();
        if report.ambiguous > 0 {
            note.push_str(&format!(" · {} ambiguous", count_text(report.ambiguous)));
            message.push_str(&format!(
                "\n{} files have several possible matches in the new folder and were left alone.",
                count_text(report.ambiguous)
            ));
        }
        if report.unreadable > 0 {
            note.push_str(&format!(" · {} unreadable", count_text(report.unreadable)));
            message.push_str(&format!(
                "\n{} files in the new folder could not be read:\n{}",
                count_text(report.unreadable),
                report.errors.join("\n")
            ));
        }
        if applied.merged > 0 {
            message.push_str(&format!(
                "\n{} of them were already in the library at the new place; their position, bookmarks and other history moved to that entry.",
                count_text(applied.merged)
            ));
        }
        if applied.conflicts + applied.kept_current + applied.changed_meanwhile > 0 {
            message.push_str(&format!(
                "\n{} files were left as they were: {} already in the library with other content, {} the file that is loaded, {} changed while the move ran.",
                count_text(applied.conflicts + applied.kept_current + applied.changed_meanwhile),
                applied.conflicts,
                applied.kept_current,
                applied.changed_meanwhile
            ));
        }
        if report.total == 0 && !report.cancelled {
            message.push_str(
                "\nThe library has no files below the old folder, so only the watched folder was changed.",
            );
        }
        if report.by_sample > 0 {
            message.push_str(&format!(
                "\n{} files came from an old profile with no full digest; they were accepted because size, sampled fingerprint and length agree.",
                count_text(report.by_sample)
            ));
        }
        if report.truncated {
            message.push_str(
                "\nThe new folder holds more than 200,000 entries; only the first part was searched for files that moved to another place.",
            );
        }
        if report.cancelled {
            note = format!(
                "Move cancelled — reconnected {} of {} files",
                count_text(reconnected),
                count_text(report.total)
            );
            message = format!(
                "{note}. The files that were checked are reconnected; the rest keep their old location, and the watched folder was not changed."
            );
        } else if applied.root_replaced {
            message.push_str("\nThe watched folder now points to the new place.");
        } else if replace {
            message.push_str(
                "\nThe old folder is no longer watched; the new place is covered by another watched folder.",
            );
        }
        message.push_str(&format!(
            "\nNo audio file was touched. A copy of your profile from just before is in {MOVE_FOLDER_COPY} in the profile folder."
        ));
        self.set_status(note);
        self.add_message(message);
        self.save();
    }
}

fn write_restore_copy(profile: &std::path::Path, state: &Session) -> Result<(), String> {
    let staged = profile.join("before-restore.new.json");
    state::export(&staged, state)?;
    super::rotate_restore_copies(profile, &staged).map_err(|error| {
        let _ = std::fs::remove_file(&staged);
        error.to_string()
    })
}
