use super::*;

impl PlayerApp {
    pub(super) fn events(&mut self, ctx: &egui::Context) {
        let mut incoming = std::mem::take(&mut self.deferred_events);
        incoming.extend(self.rx.try_iter());
        let mut incoming = incoming.into_iter();
        while let Some((epoch, e)) = incoming.next() {
            // A restore can change generation inside this very batch.
            if epoch != self.session_generation { continue; }
            if self.restore_pending && !matches!(&e, Event::RestorePrepared { .. }) {
                self.deferred_events.push((epoch, e));
                continue;
            }
            match e {
                Event::RestorePrepared { session, result } => {
                    if self.restore_pending {
                        self.finish_restore(*session, result, ctx);
                        // On failure, earlier deferred answers still precede
                        // later answers from this batch. On success all answers
                        // from the replaced generation are discarded together.
                        self.deferred_events.extend(incoming);
                        self.deferred_events.retain(|(generation, _)| *generation == self.session_generation);
                        break;
                    }
                }
                Event::PlayingImported {
                    request,
                    found,
                    aliases,
                } => {
                    self.finish_playing_drop(request, found, aliases);
                }
                Event::Imported(tracks, errors, append, aliases, quiet, rescan) => {
                    self.busy = self.busy.saturating_sub(1);
                    let last = self.scan.job_ended();
                    self.merge_import(tracks, errors, append, aliases, quiet, rescan);
                    if last {
                        self.finish_scan_run();
                    }
                }
                Event::ScanDone {
                    added,
                    found,
                    cancelled,
                } => {
                    self.scan.added = self.scan.added.saturating_add(added);
                    self.scan.found = self.scan.found.saturating_add(found);
                    self.scan.cancelled |= cancelled;
                }
                Event::ScanProgress { kind, progress } => self.scan_progress(kind, &progress),
                Event::MoveFolderChosen { old, new } => self.start_move_folder(old, new),
                Event::FolderMoved(finished) => {
                    self.busy = self.busy.saturating_sub(1);
                    if self.scan.job_ended() {
                        self.scan.finish_run();
                    }
                    self.finish_move_folder(*finished);
                }
                Event::ProgressFileRead(result) => match result {
                    Ok(file) => self.begin_progress_import(file),
                    Err(error) => self.error = Some(error),
                },
                Event::ProgressImportReady { file, safety } => {
                    self.busy = self.busy.saturating_sub(1);
                    match safety {
                        Ok(()) => self.apply_progress_file(&file),
                        Err(error) => self.add_message(format!(
                            "The listening progress was not imported: a copy of your profile could not be saved first ({error})."
                        )),
                    }
                }
                Event::Paths(paths, book) => {
                    self.import(paths, book, self.state.settings.external_files_append)
                }
                Event::PlayingPaths(paths) => self.start_playing_drop(paths),
                Event::Reread {
                    tracks,
                    errors,
                    aliases,
                    done,
                    total,
                    last,
                    cancelled,
                } => self.reread_part(tracks, errors, &aliases, done, total, last, cancelled),
                Event::Artwork(id, result) => {
                    if self.art_requested.as_deref() == Some(&id) {
                        match result {
                            Ok((pixels, origin)) => {
                                let texture = ctx.load_texture(
                                    "album artwork",
                                    pixels,
                                    egui::TextureOptions::LINEAR,
                                );
                                self.artwork = Some((id.clone(), texture));
                                self.art_note = Some((id, ArtNote::Origin(origin)));
                            }
                            Err(error) => {
                                self.artwork = None;
                                self.art_note = Some((id, ArtNote::from_error(&error)));
                            }
                        }
                    }
                }
                Event::DetailsArt(id, result) => {
                    if let Some(art) = self.details_art.as_mut().filter(|art| art.id == id) {
                        art.state = match result {
                            Ok((pixels, origin)) => DetailsArtState::Ready(
                                ctx.load_texture(
                                    "file details artwork",
                                    pixels,
                                    egui::TextureOptions::LINEAR,
                                ),
                                origin,
                            ),
                            Err(error) => DetailsArtState::Note(ArtNote::from_error(&error)),
                        };
                    }
                }
                Event::Thumb {
                    key,
                    embedded,
                    image,
                } => {
                    // An answer for an old art preference, or after the thumbnails
                    // were switched off, is not wanted any more.
                    if embedded == self.state.settings.prefer_embedded_art
                        && self.state.settings.show_list_art
                    {
                        match image {
                            Some(pixels) => {
                                let texture = ctx.load_texture(
                                    "row thumbnail",
                                    pixels,
                                    egui::TextureOptions::LINEAR,
                                );
                                self.thumbs.cache.insert(key, texture);
                            }
                            None => self.thumbs.cache.insert_missing(key, Instant::now()),
                        }
                    }
                }
                Event::Backup(result) => match result {
                    Ok(s) => self.confirm = Some(Confirm::Restore(s)),
                    Err(e) => self.error = Some(e),
                },
                Event::Associations { result, state } => {
                    self.association_busy = false;
                    self.association = state;
                    self.association_error = result.err();
                }
                Event::Chapters { key, result } => self.chapters_read(key, result),
                Event::Lyrics { key, result } => self.lyrics_read(key, result),
                Event::FallbackFonts(fonts) => {
                    self.font_fallbacks.finished();
                    if !fonts.is_empty() {
                        self.font_epoch = self.font_epoch.wrapping_add(1);
                    }
                    appearance::add_fallback_fonts(ctx, fonts);
                }
                Event::Stations(found) => self.add_playlist_stations(found),
                Event::StationTest { serial, result } => {
                    if let Some(test) = self
                        .station_dialog
                        .as_mut()
                        .and_then(|dialog| dialog.test.as_mut())
                        .filter(|test| test.serial == serial)
                    {
                        test.outcome = Some(result);
                    }
                }
                Event::Message(s) => {
                    // File-operation results must remain visible even with an idle
                    // library or a hidden status bar. The existing message dialog
                    // is non-modal and provides an explicit Dismiss action.
                    self.set_status(s.clone());
                    self.error = Some(s);
                }
                Event::ValidatedTrack {
                    request,
                    result,
                    play,
                } => {
                    if !validation_is_current(self.loading_request, request) {
                        continue;
                    }
                    self.loading_request = None;
                    match result {
                        Ok(mut track) if self.state.current.as_deref() == Some(&track.id) => {
                            if let Some(index) =
                                self.state.library.iter().position(|t| t.id == track.id)
                            {
                                let existing = &self.state.library[index];
                                preserve_current_library_edits(existing, &mut track);
                                preserve_book_speed(&mut self.state.book_speeds, existing, &track);
                                if *existing != track {
                                    Arc::make_mut(&mut self.state.library)[index] = track.clone();
                                    self.invalidate_library();
                                }
                            }
                            // The file may have changed since it was scanned.
                            self.ensure_chapters(&track);
                            self.ensure_lyrics(&track);
                            let position = self.position();
                            let play = self.pending_play_intent.unwrap_or(play);
                            self.expected_audio_request = Some(request);
                            self.state.was_playing = play;
                            self.apply_replaygain();
                            self.apply_skip_silence();
                            self.audio.load(
                                track.id,
                                track.path,
                                position,
                                play,
                                self.state.speed,
                                request,
                            );
                            self.set_status(if play {
                                STATUS_OPENING
                            } else {
                                "Position restored — paused"
                            });
                        }
                        Ok(_) => {
                            self.pending_progress_restore = None;
                            self.pending_position = None;
                            self.pending_play_intent = None;
                        }
                        Err(error) => {
                            self.pending_progress_restore = None;
                            self.pending_position = None;
                            self.pending_play_intent = None;
                            self.state.was_playing = false;
                            self.error = Some(error);
                        }
                    }
                }
                Event::PreloadValidated { request, result } => {
                    self.preload_validated(request, result);
                }
                Event::Relocated {
                    original_id,
                    result,
                } => match result {
                    Ok((track, true)) => {
                        self.confirm = Some(Confirm::Relink { original_id, track })
                    }
                    Ok((track, false)) => self.accept_relink(&original_id, track),
                    Err(error) => self.error = Some(error),
                },
                Event::Devices { request, result } => {
                    if request != self.device_request {
                        continue;
                    }
                    if let Some(options) = self.options.as_mut() {
                        match result {
                            Ok(devices) => {
                                self.devices = devices;
                                options.set_device_status(false, None);
                            }
                            Err(error) => options.set_device_status(false, Some(&error)),
                        }
                    }
                }
                Event::TagEditorLoaded { session, result } => {
                    if let Some(editor) = self
                        .tag_editor
                        .as_mut()
                        .filter(|editor| editor.session == session)
                    {
                        match result {
                            Ok(data) => {
                                editor.current_art = data.art.map(|pixels| {
                                    ctx.load_texture(
                                        "tag editor artwork",
                                        pixels,
                                        egui::TextureOptions::LINEAR,
                                    )
                                });
                                editor.art_note = data.art_note;
                                editor.album_ids = data.album_ids;
                                editor.album_skipped = data.album_skipped;
                                if selection_is_whole_album(
                                    &editor.id,
                                    &editor.album_ids,
                                    &editor.selected_when_opened,
                                ) {
                                    editor.apply_to_album = true;
                                }
                                editor.fields = data.tags.clone();
                                editor.loaded = data.tags;
                                editor.load = EditorLoad::Ready;
                            }
                            Err(error) => editor.load = EditorLoad::Failed(error),
                        }
                    }
                }
                Event::TagArtworkChosen { session, result } => {
                    if let Some(editor) = self
                        .tag_editor
                        .as_mut()
                        .filter(|editor| editor.session == session)
                    {
                        match result {
                            Ok(chosen) => {
                                editor.error = None;
                                editor.art = EditorArt::Replace {
                                    texture: ctx.load_texture(
                                        "tag editor new artwork",
                                        chosen.preview,
                                        egui::TextureOptions::LINEAR,
                                    ),
                                    bytes: chosen.bytes,
                                    name: chosen.name,
                                };
                            }
                            Err(error) => editor.error = Some(error),
                        }
                    }
                }
                Event::TagFileSaved {
                    session,
                    result,
                    done,
                    total,
                } => {
                    if let Ok(track) = result {
                        self.apply_saved_tags(track);
                    }
                    if total > 1 {
                        self.set_status(format!("Saved {done} of {total} files"));
                    }
                    if let Some(editor) = self
                        .tag_editor
                        .as_mut()
                        .filter(|editor| editor.session == session)
                    {
                        editor.saving = Some((done, total));
                    }
                }
                Event::TagSaveFinished { session, summary } => {
                    self.busy = self.busy.saturating_sub(1);
                    self.tag_save_running = false;
                    self.finish_tag_save(session, summary);
                }
            }
        }
    }
}

pub(super) fn cancel_pending_validation(
    active: &mut Option<u64>,
    pending: &mut Option<(String, f64, Instant)>,
) -> bool {
    if active.take().is_some() {
        *pending = None;
        true
    } else {
        false
    }
}

pub(super) fn pending_request_acknowledged(snapshot: &Snapshot, track_id: &str) -> bool {
    // accepted_snapshot already checked the explicit request ID. Position can be
    // clamped by a decoder or advance before the next UI tick, so it is not an ACK.
    !snapshot.busy && snapshot.loaded.as_deref() == Some(track_id)
}

pub(super) fn validation_is_current(active_request: Option<u64>, result_request: u64) -> bool {
    active_request == Some(result_request)
}


pub(super) fn install_restored_session(
    state: &mut Session,
    imported: Session,
    snapshot: &mut Snapshot,
    pending: &mut Option<(String, f64, Instant)>,
    expected: &mut Option<u64>,
    validating: &mut Option<u64>,
) {
    // Old audio state must lose authority before any checkpoint of the new session.
    *snapshot = Snapshot::default();
    *pending = None;
    *expected = None;
    *validating = None;
    *state = imported;
    state.was_playing = false;
}
