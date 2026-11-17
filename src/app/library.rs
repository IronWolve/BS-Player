use super::*;

impl PlayerApp {
    pub(super) fn job_sender(&self) -> JobSender {
        JobSender {
            generation: self.session_generation,
            tx: self.tx.clone(),
            repaint: self.gui_context.clone(),
        }
    }

    /// Claims the single native file dialog slot; `None` (with a note) when a
    /// dialog is already open, e.g. because a shortcut key was held down.
    pub(super) fn begin_picker(&mut self) -> Option<PickerGuard> {
        if self.picker_open.swap(true, Ordering::SeqCst) {
            self.set_status("A file dialog is already open.");
            None
        } else {
            Some(PickerGuard(Arc::clone(&self.picker_open)))
        }
    }

    pub(super) fn invalidate_library(&mut self) {
        self.library_revision = self.library_revision.wrapping_add(1);
        self.view_revision = self.view_revision.wrapping_add(1);
    }

    pub(super) fn cancel_validation(&mut self) {
        self.cancel_playing_drop();
        if cancel_pending_validation(&mut self.loading_request, &mut self.pending_position) {
            self.expected_audio_request = None;
            self.snapshot = Snapshot::default();
            self.pending_play_intent = None;
        } else if self.pending_position.is_some() {
            self.pending_play_intent = Some(false);
        }
    }

    pub(super) fn invalidate_jobs(&mut self) {
        self.cancel_playing_drop();
        self.session_generation = self.session_generation.wrapping_add(1);
        self.busy = 0;
        // The save's last answer belongs to the old generation and never comes.
        self.tag_save_running = false;
        self.tag_resume = None;
        self.association_busy = false;
        self.reread = None;
        // Workers of the old profile are told to stop; their answers are not wanted.
        self.scan.reset();
        self.loading_request = None;
        self.pending_play_intent = None;
        self.pending_progress_restore = None;
        self.expected_audio_request = None;
        self.pending_position = None;
        self.snapshot = Snapshot::default();
        self.seen_audio_error = None;
        // The preload and the kept shuffle pick are for the old profile's queue.
        self.drop_preload();
        self.planned_next = None;
        // A restored or replaced profile makes every captured undo step stale.
        self.undo.clear();
        self.undo_note = false;
        // Queued thumbnails belong to the old generation; ask again for what is visible.
        self.thumbs.reset();
        // A restored or replaced profile has other rows: nothing stays selected
        // but the profile's own primary row.
        self.selection = SelectionSet::default();
        self.selected_cache = None;
        self.selection_summary = None;
        // The chapters and lyrics belong to the old profile's loaded file.
        self.clear_chapters();
        self.clear_lyrics();
        // A font worker of the old session is not answered: ask again.
        self.font_fallbacks.abandon();
    }

    pub(super) fn import(&mut self, paths: Vec<PathBuf>, audiobook: bool, append: bool) {
        self.import_jobs(vec![(paths, audiobook)], append, false, false);
    }

    /// One worker thread runs the jobs in order and sends one `Imported` event per
    /// job. `incremental` skips reading and hashing files whose size and modified
    /// time still match their library record (rescans); `quiet` is an automatic
    /// scan, whose unchanged error messages are not raised again.
    pub(super) fn import_jobs(
        &mut self,
        jobs: Vec<(Vec<PathBuf>, bool)>,
        append: bool,
        incremental: bool,
        quiet: bool,
    ) {
        let mut jobs: Vec<_> = jobs
            .into_iter()
            .filter(|(paths, _)| !paths.is_empty())
            .collect();
        if jobs.is_empty() {
            return;
        }
        let mut known_roots: HashMap<PathBuf, bool> = HashMap::new();
        for (root, audiobook) in &self.state.roots {
            let identity = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            known_roots.entry(identity).or_insert(*audiobook);
        }
        let selected_jobs = std::mem::take(&mut jobs);
        for (paths, requested_audiobook) in selected_jobs {
            let mut groups: Vec<(Vec<PathBuf>, bool)> = Vec::new();
            for p in paths {
                // A folder whose name is not valid Unicode cannot be saved in the
                // profile; the import itself reports it.
                let audiobook = if p.is_dir() && p.to_str().is_some() {
                    let identity = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
                    if let Some(existing_audiobook) = known_roots.get(&identity).copied() {
                        // A different spelling of a watched root must keep the
                        // classification that root already established.
                        existing_audiobook
                    } else {
                        known_roots.insert(identity, requested_audiobook);
                        self.state.roots.push((p.clone(), requested_audiobook));
                        requested_audiobook
                    }
                } else {
                    requested_audiobook
                };
                if let Some((group, kind)) = groups.last_mut() {
                    if *kind == audiobook {
                        group.push(p);
                        continue;
                    }
                }
                groups.push((vec![p], audiobook));
            }
            jobs.extend(groups);
        }
        let recursive = self.state.settings.recursive_scan;
        let excluded: Vec<_> = self
            .state
            .settings
            .excluded_extensions
            .split([',', ';'])
            .map(|x| x.trim().trim_start_matches('.').to_lowercase())
            .filter(|x| !x.is_empty())
            .collect();
        self.busy += jobs.len();
        let cancel = self.scan.begin(jobs.len());
        self.set_status("Reading audio files and tags…");
        self.spawn_import(
            jobs,
            ImportMode {
                append,
                incremental,
                quiet,
                recursive,
                excluded,
            },
            cancel,
        );
    }

    /// Starts the worker thread of `import_jobs`; `cancel` is the flag Cancel sets.
    pub(super) fn spawn_import(
        &self,
        jobs: Vec<(Vec<PathBuf>, bool)>,
        mode: ImportMode,
        cancel: Arc<AtomicBool>,
    ) {
        let ImportMode {
            append,
            incremental,
            quiet,
            recursive,
            excluded,
        } = mode;
        let tx = self.job_sender();
        let library = Arc::clone(&self.state.library);
        std::thread::spawn(move || {
            let (aliases, known) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut aliases: HashMap<PathBuf, PathBuf> = HashMap::new();
                for (index, track) in library.iter().enumerate() {
                    // A cancelled scan reads nothing, so the paths need not be finished.
                    if index % 256 == 0 && cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Ok(canonical) = std::fs::canonicalize(&track.path) {
                        aliases.insert(track.path.clone(), canonical);
                    }
                }
                let known = if incremental && !cancel.load(Ordering::Relaxed) {
                    known_tracks(&library, &aliases)
                } else {
                    HashMap::new()
                };
                (aliases, known)
            }))
            .unwrap_or_default();
            drop(library);
            for (paths, audiobook) in jobs {
                // Every job must send its event, or `busy` never returns to zero.
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut report = |progress| {
                        tx.send_quiet(Event::ScanProgress {
                            kind: ScanKind::Import,
                            progress,
                        })
                    };
                    // Stream entries of playlists the user chose become stations
                    // (not on a rescan, which must not bring back removed stations).
                    let mut found = media::import_paths_cancellable(
                        paths,
                        audiobook,
                        recursive,
                        &excluded,
                        incremental.then_some(&known),
                        !incremental,
                        &cancel,
                        &mut report,
                    );
                    if incremental {
                        for track in &mut found.tracks {
                            track.audiobook |= audiobook;
                        }
                    } else if !found.stations.is_empty() {
                        tx.send(Event::Stations(std::mem::take(&mut found.stations)));
                    }
                    found
                }));
                let found = run.unwrap_or_else(|_| media::PathImport {
                    errors: vec!["The import stopped unexpectedly and was skipped.".to_owned()],
                    ..media::PathImport::default()
                });
                // A stopped import must not replace the queue with the part it read.
                let append = append || found.cancelled;
                tx.send(Event::ScanDone {
                    added: found.tracks.len(),
                    found: found.found,
                    cancelled: found.cancelled,
                });
                tx.send(Event::Imported(
                    found.tracks,
                    found.errors,
                    append,
                    aliases.clone(),
                    quiet,
                    incremental,
                ));
            }
        });
    }

    pub(super) fn choose_files(&mut self, folder: bool) {
        let Some(guard) = self.begin_picker() else {
            return;
        };
        let tx = self.job_sender();
        let book = self.audiobook_context();
        let playing = self.state.section == state::PLAYING_SECTION;
        std::thread::spawn(move || {
            let _guard = guard;
            let files = if folder {
                rfd::FileDialog::new()
                    .set_title("Add an audio folder")
                    .pick_folder()
                    .map(|p| vec![p])
            } else {
                rfd::FileDialog::new()
                    .set_title("Add audio files or playlists")
                    .add_filter("Audio and playlists", ADD_FILE_EXTENSIONS)
                    .pick_files()
            };
            if let Some(paths) = files {
                tx.send(if playing {
                    Event::PlayingPaths(paths)
                } else {
                    Event::Paths(paths, book)
                });
            }
        });
    }

    /// Files added while a single book or the Audiobooks list is shown are audiobooks.
    pub(super) fn audiobook_context(&self) -> bool {
        self.state.section == "Audiobooks" || self.state.section.starts_with("Book:")
    }

    pub(super) fn rescan(&mut self) {
        self.rescan_with(false);
    }

    pub(super) fn rescan_with(&mut self, quiet: bool) {
        if self.busy > 0 {
            return;
        }
        let jobs: Vec<_> = self
            .state
            .roots
            .iter()
            .map(|(path, book)| (vec![path.clone()], *book))
            .collect();
        self.import_jobs(jobs, true, true, quiet);
        self.last_scan = Instant::now();
    }

    /// Library → Cancel scan and the status bar's Cancel: stops the scans,
    /// re-reads and folder moves that run. What was read until then is kept.
    pub(super) fn cancel_scans(&mut self) {
        if self.scan.request_cancel() {
            self.set_status("Cancelling…");
        }
    }

    /// Whether the status bar offers "Cancel" now: a job runs, Cancel was not
    /// pressed and the bar shows the job's note.
    pub(super) fn cancel_offered(&self) -> bool {
        self.scan.running() && !self.scan.cancel_requested && self.status_for_bar().is_some()
    }

    /// Shows a progress report of a running job in the status bar. The text
    /// changes at most every `SCAN_TEXT_INTERVAL` (a new phase at once), so it
    /// asks for no more than five repaints a second.
    pub(super) fn scan_progress(&mut self, kind: ScanKind, progress: &media::ScanProgress) {
        if !self.scan.running() || self.scan.cancel_requested {
            return;
        }
        let text = scan_progress_text(kind, progress);
        let new_phase = self.scan.phase != Some(progress.phase);
        let fresh = self
            .scan
            .changed_at
            .is_none_or(|at| at.elapsed() >= SCAN_TEXT_INTERVAL);
        if text == self.scan.text || !(fresh || new_phase) {
            return;
        }
        self.scan.phase = Some(progress.phase);
        self.scan.changed_at = Some(Instant::now());
        self.scan.text.clone_from(&text);
        self.set_status(text);
    }

    /// The last import job of a run has ended: a run that was cancelled says how
    /// much of it was added.
    pub(super) fn finish_scan_run(&mut self) {
        if self.scan.cancelled {
            let note = if self.scan.added == 0 {
                "Scan cancelled — no files were added".to_owned()
            } else {
                format!(
                    "Scan cancelled — {} of {} files added",
                    count_text(self.scan.added),
                    count_text(self.scan.found.max(self.scan.added))
                )
            };
            self.set_status(note);
        }
        self.scan.finish_run();
    }

    pub(super) fn merge_import(
        &mut self,
        tracks: Vec<Track>,
        errors: Vec<String>,
        append: bool,
        aliases: HashMap<PathBuf, PathBuf>,
        quiet: bool,
        rescan: bool,
    ) {
        self.merge_import_target(tracks, errors, append, aliases, quiet, rescan, false);
    }

    pub(super) fn merge_import_target(
        &mut self,
        tracks: Vec<Track>,
        errors: Vec<String>,
        append: bool,
        aliases: HashMap<PathBuf, PathBuf>,
        quiet: bool,
        rescan: bool,
        playing: bool,
    ) {
        self.checkpoint();
        // Files the user removed from the library stay removed on a rescan; an
        // explicit import (files, folder, drop, playlist) adds them back.
        let (tracks, skipped_removed) =
            state::apply_removed_paths(&mut self.state.removed_paths, tracks, rescan);
        // A rescan that finds nothing new must not copy the library or rebuild
        // the list caches.
        let protected = self.state.playing_workspace && !playing;
        let unchanged = import_changes_nothing(
            &self.state,
            &tracks,
            append || protected,
            rescan || protected,
        );
        let library_changed = !tracks.is_empty() && !unchanged;
        let outcome = if unchanged {
            ImportOutcome::default()
        } else if playing {
            reconcile_import_target(&mut self.state, tracks, append, rescan, &aliases, true)
        } else {
            reconcile_import(&mut self.state, tracks, append, rescan, &aliases)
        };
        if library_changed {
            self.invalidate_library();
        }
        if outcome.pause {
            self.cancel_validation();
            self.audio.pause();
            self.state.was_playing = false;
        }
        if outcome.invalidate_playback {
            self.loading_request = None;
            self.pending_play_intent = None;
            self.expected_audio_request = None;
            self.pending_position = None;
            self.snapshot = Snapshot::default();
        }
        if quiet && outcome.added == 0 && errors.is_empty() {
            // An automatic scan that found nothing is not worth a note.
            if self.busy == 0 {
                self.status.clear();
            }
        } else {
            self.set_status(format!(
                "Imported {} new files · {} files in library",
                outcome.added,
                self.state.library.len()
            ));
            if skipped_removed > 0 && !quiet {
                self.status
                    .push_str(&format!(" · {skipped_removed} removed files skipped"));
            }
        }
        if let Some(note) = self.station_import_note.take() {
            if outcome.added == 0 && !library_changed {
                // The playlists held only stations: that is the news, and they
                // are in the Streams list.
                self.set_status(note);
                self.show_streams();
            } else {
                self.status.push_str(&format!(" · {note}"));
            }
        }
        if outcome.retained_originals > 0 {
            self.add_message(format!(
                "{} files have different or unverified content. New files were imported without old progress; the original library records, positions and bookmarks were kept. Use File details → Locate file on an original record to reconnect its audio.",
                outcome.retained_originals
            ));
        }
        if !errors.is_empty() {
            let count = errors.len();
            // An automatic scan raises each distinct problem once, not on every pass.
            let shown: Vec<String> = if quiet {
                if self.scan_errors_reported.len() > 1000 {
                    self.scan_errors_reported.clear();
                }
                errors
                    .into_iter()
                    .filter(|error| self.scan_errors_reported.insert(error.clone()))
                    .collect()
            } else {
                errors
            };
            if shown.is_empty() {
                self.status
                    .push_str(&format!(" · {count} could not be read"));
            } else {
                self.add_message(format!(
                    "Some files could not be imported:\n{}",
                    shown.into_iter().take(12).collect::<Vec<_>>().join("\n")
                ));
            }
        }
        self.save();
    }

    /// How many files "Re-read tags of all files" covers.
    pub(super) fn reread_file_count(&self) -> usize {
        self.state
            .library
            .iter()
            .filter(|track| !track.superseded)
            .count()
    }

    /// Reads the tags of every library file again on a worker thread, a part at
    /// a time through the normal scan path, so each file keeps its record (id,
    /// progress, bookmarks) and only the file's own facts are refreshed.
    pub(super) fn start_reread(&mut self) {
        if self.busy > 0 {
            self.set_status("A scan or save is running; re-read the tags when it ends");
            return;
        }
        let paths = reread_paths(&self.state.library);
        if paths.is_empty() {
            self.set_status("The library has no files to re-read");
            return;
        }
        let total = paths.len();
        let part_size = reread_part_size(total);
        let tx = self.job_sender();
        let library = Arc::clone(&self.state.library);
        self.busy += 1;
        let cancel = self.scan.begin(1);
        self.reread = Some(RereadRun {
            total,
            ..RereadRun::default()
        });
        self.set_status(format!("Re-read 0 of {total} files"));
        std::thread::spawn(move || {
            let aliases = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                library
                    .iter()
                    .filter_map(|track| {
                        std::fs::canonicalize(&track.path)
                            .ok()
                            .map(|canonical| (track.path.clone(), canonical))
                    })
                    .collect::<HashMap<PathBuf, PathBuf>>()
            }))
            .unwrap_or_default();
            drop(library);
            let aliases = Arc::new(aliases);
            let mut done = 0;
            let mut tracks = Vec::new();
            let mut errors = Vec::new();
            let mut cancelled = false;
            for part in paths.chunks(part_size) {
                let before = done;
                // Every part must send its event, or `busy` never returns to zero.
                let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut report = |progress: media::ScanProgress| {
                        // The files are named, so there is nothing to collect: the
                        // numbers count the whole job, not the part.
                        if progress.phase == media::ScanPhase::Reading {
                            tx.send_quiet(Event::ScanProgress {
                                kind: ScanKind::Import,
                                progress: media::ScanProgress {
                                    found: total,
                                    done: before + progress.done,
                                    total,
                                    ..progress
                                },
                            });
                        }
                    };
                    media::import_paths_cancellable(
                        part.to_vec(),
                        false,
                        false,
                        &[],
                        None,
                        false,
                        &cancel,
                        &mut report,
                    )
                }));
                let found = run.unwrap_or_else(|_| media::PathImport {
                    errors: vec!["The re-read stopped unexpectedly for some files".to_owned()],
                    ..media::PathImport::default()
                });
                cancelled = found.cancelled;
                done += if cancelled {
                    found.tracks.len()
                } else {
                    part.len()
                };
                tracks.extend(found.tracks);
                errors.extend(found.errors);
                if cancelled {
                    break;
                }
            }
            // Reconcile once, after the worker has gathered every part. This
            // avoids repeated full-library indexes and Arc copy-on-write clones
            // on the UI thread for large libraries.
            tx.send(Event::Reread {
                tracks,
                errors,
                aliases,
                done,
                total,
                last: true,
                cancelled,
            });
        });
    }

    /// Merges one part of a re-read into the library and shows how far it is.
    pub(super) fn reread_part(
        &mut self,
        tracks: Vec<Track>,
        errors: Vec<String>,
        aliases: &HashMap<PathBuf, PathBuf>,
        done: usize,
        total: usize,
        last: bool,
        cancelled: bool,
    ) {
        if let Some(run) = self.reread.as_mut() {
            run.done = done;
            run.total = total;
            run.failed += errors.len();
            let room = 5usize.saturating_sub(run.first_errors.len());
            run.first_errors.extend(errors.into_iter().take(room));
        }
        if last {
            self.busy = self.busy.saturating_sub(1);
            if self.scan.job_ended() {
                self.scan.finish_run();
            }
        }
        self.merge_import(tracks, Vec::new(), true, aliases.clone(), true, true);
        if !last {
            self.set_status(format!("Re-read {done} of {total} files"));
            return;
        }
        let run = self.reread.take().unwrap_or_default();
        let mut note = if cancelled {
            format!(
                "Re-read cancelled — {} of {} files read",
                count_text(done),
                count_text(total)
            )
        } else {
            format!("Re-read {total} files")
        };
        if run.failed > 0 {
            note.push_str(&format!(" · {} could not be read", run.failed));
            self.add_message(format!(
                "{} could not be read again (missing or unreadable):\n{}",
                if run.failed == 1 {
                    "1 file".to_owned()
                } else {
                    format!("{} files", run.failed)
                },
                run.first_errors.join("\n")
            ));
        }
        self.set_status(note);
    }
}

/// The tags File details shows read-only: those the scan read from the file
/// (album artist, composer, disc, narrator) that the file actually has.
pub(super) fn scanned_tag_rows(track: &Track) -> Vec<(&'static str, String)> {
    [
        ("Album artist", track.album_artist.clone()),
        ("Composer", track.composer.clone()),
        (
            "Disc",
            if track.disc > 0 {
                track.disc.to_string()
            } else {
                String::new()
            },
        ),
        ("Narrator", track.narrator.clone()),
    ]
    .into_iter()
    .filter(|(_, value)| !value.trim().is_empty())
    .collect()
}

/// Files per part of a re-read: about forty parts, so the status line moves and
/// the library is merged often, without merging a large library hundreds of times.
pub(super) fn reread_part_size(total: usize) -> usize {
    (total / 40).clamp(100, 2_500)
}

/// The files a re-read covers: every current record's file once, in library order.
pub(super) fn reread_paths(library: &[Track]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    library
        .iter()
        .filter(|track| !track.superseded && seen.insert(track.path.as_path()))
        .map(|track| track.path.clone())
        .collect()
}

#[derive(Default, Debug)]
pub(super) struct ImportOutcome {
    pub(super) added: usize,
    pub(super) retained_originals: usize,
    pub(super) pause: bool,
    pub(super) invalidate_playback: bool,
}

/// Merges scanned or chosen tracks into the session. A `rescan` of watched
/// folders only queues files that were not in the library before; explicit
/// imports queue everything they bring.
pub(super) fn reconcile_import(
    state: &mut Session,
    tracks: Vec<Track>,
    append: bool,
    rescan: bool,
    aliases: &HashMap<PathBuf, PathBuf>,
) -> ImportOutcome {
    let queue_files = !state.playing_workspace;
    reconcile_import_target(state, tracks, append, rescan, aliases, queue_files)
}

pub(super) fn reconcile_import_target(
    state: &mut Session,
    tracks: Vec<Track>,
    append: bool,
    rescan: bool,
    aliases: &HashMap<PathBuf, PathBuf>,
    queue_files: bool,
) -> ImportOutcome {
    let mut outcome = ImportOutcome::default();
    if tracks.is_empty() {
        return outcome;
    }
    let library = Arc::make_mut(&mut state.library);
    let mut by_id: HashMap<String, usize> = library
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id.clone(), i))
        .collect();
    let mut by_path: HashMap<PathBuf, usize> = library
        .iter()
        .enumerate()
        .map(|(i, t)| (aliases.get(&t.path).unwrap_or(&t.path).clone(), i))
        .collect();
    let mut by_content: HashMap<(PathBuf, String), usize> = library
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            t.content_digest.as_ref().map(|digest| {
                (
                    (
                        aliases.get(&t.path).unwrap_or(&t.path).clone(),
                        digest.clone(),
                    ),
                    i,
                )
            })
        })
        .collect();
    let mut versions: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    for (index, track) in library.iter().enumerate() {
        versions
            .entry(aliases.get(&track.path).unwrap_or(&track.path).clone())
            .or_default()
            .push(index);
    }
    let mut replacements = HashMap::new();
    let mut ids = Vec::with_capacity(tracks.len());
    let mut imported = HashSet::with_capacity(tracks.len());
    for mut track in tracks {
        track.superseded = false;
        let content_match = track
            .content_digest
            .as_ref()
            .and_then(|digest| by_content.get(&(track.path.clone(), digest.clone())))
            .copied();
        let matched = content_match.or_else(|| {
            by_path
                .get(&track.path)
                .or_else(|| by_id.get(&track.id))
                .copied()
        });
        let mut is_new = false;
        let index = if let Some(index) = matched {
            let existing = &library[index];
            let old_id = existing.id.clone();
            // The same file with other bytes is also the same record when another
            // program only re-tagged it: its length is unchanged, so the saved
            // position still means the same place (see `media::carry_identity`).
            if media::same_content(existing, &track)
                || media::same_track_after_external_edit(existing, &track)
            {
                track = media::carry_identity(existing, track);
                preserve_book_speed(&mut state.book_speeds, existing, &track);
                by_path.remove(aliases.get(&existing.path).unwrap_or(&existing.path));
                library[index] = track.clone();
                index
            } else {
                // Keep the original row so its progress/bookmarks still have a
                // visible recovery path. Only the new content gets a fresh ID.
                replacements.insert(old_id, track.id.clone());
                outcome.added += 1;
                outcome.retained_originals += 1;
                let index = library.len();
                library.push(track.clone());
                index
            }
        } else {
            let index = library.len();
            library.push(track.clone());
            outcome.added += 1;
            is_new = true;
            index
        };
        let siblings = versions.entry(track.path.clone()).or_default();
        if !siblings.contains(&index) {
            siblings.push(index);
        }
        for &sibling in siblings.iter() {
            library[sibling].superseded = sibling != index;
            if sibling != index && state.current.as_deref() == Some(&library[sibling].id) {
                replacements.insert(library[sibling].id.clone(), track.id.clone());
            }
        }
        by_id.insert(track.id.clone(), index);
        by_path.insert(track.path.clone(), index);
        if let Some(digest) = &track.content_digest {
            by_content.insert((track.path.clone(), digest.clone()), index);
        }
        if imported.insert(track.id.clone()) && (!rescan || is_new) {
            ids.push(track.id);
        }
    }
    for id in state
        .queue
        .iter_mut()
        .chain(state.playlists.iter_mut().flat_map(|p| p.tracks.iter_mut()))
    {
        if let Some(replacement) = replacements.get(id) {
            *id = replacement.clone();
        }
    }
    if let Some(id) = state.current.as_mut() {
        if let Some(replacement) = replacements.get(id) {
            *id = replacement.clone();
            outcome.pause = true;
            outcome.invalidate_playback = true;
        }
    }
    if let Some(id) = state.selected.as_mut() {
        if let Some(replacement) = replacements.get(id) {
            *id = replacement.clone();
        }
    }
    if !append && queue_files {
        state.queue.clear();
        state.was_playing = false;
        outcome.pause = true;
        if state
            .current
            .as_ref()
            .is_some_and(|id| !imported.contains(id))
        {
            state.current = None;
            state.selected = ids.first().cloned();
            outcome.invalidate_playback = true;
        }
    }
    let mut queued: HashSet<_> = state.queue.iter().cloned().collect();
    for id in ids {
        if queue_files && queued.insert(id.clone()) {
            state.queue.push(id);
        }
    }
    if outcome.pause {
        state.was_playing = false;
    }
    outcome
}

/// Library records a rescan may reuse without reading the file again: not
/// superseded and stored under their canonical path, the key the scan looks up.
pub(super) fn known_tracks(
    library: &[Track],
    aliases: &HashMap<PathBuf, PathBuf>,
) -> HashMap<PathBuf, Track> {
    library
        .iter()
        .filter(|track| !track.superseded && aliases.get(&track.path) == Some(&track.path))
        .map(|track| (track.path.clone(), track.clone()))
        .collect()
}

/// True when merging `tracks` would change neither the library nor the queue:
/// every one is identical to its current record, and queued unless this is a
/// rescan (which never queues files already in the library).
pub(super) fn import_changes_nothing(
    state: &Session,
    tracks: &[Track],
    append: bool,
    rescan: bool,
) -> bool {
    if !append || tracks.is_empty() {
        return false;
    }
    let by_id: HashMap<&str, &Track> = state
        .library
        .iter()
        .map(|track| (track.id.as_str(), track))
        .collect();
    let queued: HashSet<&str> = state.queue.iter().map(String::as_str).collect();
    tracks.iter().all(|track| {
        !track.superseded
            && (rescan || queued.contains(track.id.as_str()))
            && by_id
                .get(track.id.as_str())
                .is_some_and(|old| !old.superseded && **old == *track)
    })
}

/// Clears every saved listening position and finished flag. Play counts and
/// last-played dates are history, not positions, and stay.
pub(super) fn reset_positions(progress: &mut HashMap<String, state::Progress>) {
    for entry in progress.values_mut() {
        entry.seconds = 0.;
        entry.completed = false;
    }
}

/// Whether the editable library tags of the two records differ.
pub(super) fn tags_differ(a: &Track, b: &Track) -> bool {
    a.title != b.title
        || a.artist != b.artist
        || a.album != b.album
        || a.genre != b.genre
        || a.year != b.year
        || a.comment != b.comment
        || a.track_number != b.track_number
        || a.audiobook != b.audiobook
}

pub(super) fn preserve_current_library_edits(existing: &Track, refreshed: &mut Track) {
    refreshed.id = existing.id.clone();
    refreshed.added_at = existing.added_at;
    refreshed.audiobook |= existing.audiobook;
    refreshed.preserve_library_tags(existing);
}

pub(super) fn preserve_book_speed(speeds: &mut HashMap<String, f32>, old: &Track, new: &Track) {
    if let Some(speed) = speeds.get(&old.book_key()).copied() {
        speeds.entry(new.book_key()).or_insert(speed);
    }
}
