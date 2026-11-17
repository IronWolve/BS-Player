use super::*;

impl PlayerApp {
    pub(super) fn load_track(&mut self, id: &str, play: bool, override_position: Option<f64>) {
        let Some(track) = self.state.track(id).cloned() else {
            return;
        };
        // Not while a tag save writes this very file; it plays when the save is over.
        if let Some(resume) = self.tag_resume.as_mut().filter(|resume| resume.id == id) {
            resume.play |= play;
            self.set_status("This file is being saved; it plays when the tags are saved");
            return;
        }
        self.pending_progress_restore = None;
        self.checkpoint();
        // Whatever was prepared as the next file belongs to the track that played.
        self.release_preload();
        let changed = self.state.current.as_deref() != Some(id);
        let progress = self.state.progress.get(id).cloned().unwrap_or_default();
        let position = override_position.unwrap_or_else(|| {
            resume_start(
                &self.state.settings,
                &progress,
                track.audiobook,
                state::now(),
            )
        });
        if changed {
            self.state.speed = self.speed_for_track(&track);
        }
        self.switch_to_track(id, &track);
        self.audio.pause();
        self.state.was_playing = false;
        self.load_request = self.load_request.wrapping_add(1);
        let request = self.load_request;
        self.loading_request = Some(request);
        self.pending_play_intent = Some(play);
        self.expected_audio_request = None;
        self.snapshot = Snapshot::default();
        self.pending_position = Some((id.into(), position, Instant::now()));
        let tx = self.job_sender();
        std::thread::spawn(move || {
            tx.send(Event::ValidatedTrack {
                request,
                result: media::validate_for_playback(&track),
                play,
            });
        });
        self.set_status(STATUS_CHECKING);
    }

    /// The speed a track starts at: the audiobook speed (the one remembered for
    /// its book when that is on) or the music speed.
    pub(super) fn speed_for_track(&self, track: &Track) -> f32 {
        if track.audiobook {
            if self.state.settings.remember_book_speed {
                self.state
                    .book_speeds
                    .get(&track.book_key())
                    .copied()
                    .unwrap_or(self.state.settings.book_speed)
            } else {
                self.state.settings.book_speed
            }
        } else {
            self.state.settings.music_speed
        }
    }

    /// The engine-free part of making `id` the current track, shared by
    /// `load_track` (the engine is told afterwards) and `enter_track` (the engine
    /// already plays it): the repeat points go when the track changes, then the
    /// private-listening mark, the current and selected track, the chapters and
    /// the list follow it, and the automatic-next plan made for the old track is
    /// dropped. The speed and the play intent are the callers' business.
    pub(super) fn switch_to_track(&mut self, id: &str, track: &Track) {
        let changed = self.state.current.as_deref() != Some(id);
        self.paused_at = None;
        self.sleep_chapter_end = None;
        if changed {
            self.state.ab_a = None;
            self.state.ab_b = None;
        }
        self.private_track = if self.state.settings.private_listening {
            Some(id.into())
        } else {
            None
        };
        self.state.current = Some(id.into());
        // A file replaces a station as what the player holds.
        self.leave_station();
        self.ensure_chapters(track);
        self.ensure_lyrics(track);
        // The selection follows the playing file, except that several selected
        // rows stay selected (a track change must not undo the user's picks).
        self.selection.coordinate(self.state.selected.as_deref());
        if self.selection.len() > 1 {
            if self.selection.contains(id) {
                self.selection.set_primary(id);
            }
            self.state.selected = self.selection.primary().map(str::to_owned);
        } else {
            self.state.selected = Some(id.into());
        }
        self.ended_handled = false;
        self.scroll_to_current = self.state.settings.follow_current;
        if !self.state.queue.iter().any(|q| q == id) {
            self.state.queue.push(id.into());
            self.view_revision = self.view_revision.wrapping_add(1);
        }
        self.planned_next = None;
    }

    /// The engine went on to `id` by itself (a gapless join) and now plays it at
    /// the start, answering `request_id`: the app follows without telling the
    /// engine anything. Same bookkeeping as `load_track`, except that playing is
    /// certain, nothing is being loaded or waited for (the snapshot that shows
    /// `id` carries `request_id`), and a finished mark the track had from an
    /// earlier listen goes, as it does when a load is acknowledged. The speed is
    /// not touched: the engine plays on at the one speed it has.
    pub(super) fn enter_track(&mut self, id: &str, request_id: u64) {
        let Some(track) = self.state.track(id).cloned() else {
            return;
        };
        self.switch_to_track(id, &track);
        self.pending_progress_restore = None;
        self.state.was_playing = true;
        self.loading_request = None;
        self.pending_position = None;
        self.pending_play_intent = None;
        self.expected_audio_request = Some(request_id);
        if !self.state.settings.private_listening
            && self.private_track.as_deref() != Some(id)
            && self
                .state
                .progress
                .get_mut(id)
                .is_some_and(|p| std::mem::replace(&mut p.completed, false))
        {
            self.progress_changed();
        }
    }

    pub(super) fn playback_intent(&self) -> bool {
        if self.state.current_station.is_some() {
            return self.station_intent();
        }
        requested_play_state(
            self.pending_play_intent,
            self.state.current.as_deref(),
            &self.snapshot,
            self.state.was_playing,
        )
    }

    pub(super) fn toggle_play(&mut self) {
        self.cancel_playing_drop();
        if self.state.current_station.is_some() {
            self.toggle_station();
            return;
        }
        // Nothing is loaded and the Streams list shows a station: Play connects it.
        if self.state.current.is_none() && self.state.section == state::STREAMS_SECTION {
            if let Some(id) = self.selected_station() {
                self.begin_station(&id, true);
                return;
            }
        }
        // The engine let go of this track's file for a tag save: it is not
        // reopened while the save writes it; the press decides what happens after.
        if let Some(resume) = self
            .tag_resume
            .as_mut()
            .filter(|resume| self.state.current.as_deref() == Some(resume.id.as_str()))
        {
            resume.play = !resume.play;
            let note = if resume.play {
                "Playback continues when the tags are saved"
            } else {
                "Playback stays paused after the tags are saved"
            };
            self.set_status(note);
            return;
        }
        if self.loading_request.is_some() {
            self.pending_play_intent = Some(!self.playback_intent());
            return;
        }
        if self.playback_intent() {
            self.cancel_validation();
            self.audio.pause();
            self.state.was_playing = false;
            self.save();
        } else if let Some(id) = self.state.current.clone() {
            if self.expected_audio_request.is_some() && !self.snapshot.ended {
                self.rewind_after_pause();
                self.audio.play();
                if self.pending_position.is_some() {
                    self.pending_play_intent = Some(true);
                }
                self.state.was_playing = true;
            } else {
                self.load_track(&id, true, None);
            }
        } else if let Some(id) = self
            .state
            .selected
            .clone()
            .or_else(|| self.state.queue.first().cloned())
            .or_else(|| self.state.library.first().map(|t| t.id.clone()))
        {
            self.load_track(&id, true, None);
        }
    }

    /// Smart rewind inside a session: continuing an audiobook after a long
    /// pause starts a little earlier (see `settings::pause_rewind_seconds`).
    pub(super) fn rewind_after_pause(&mut self) {
        let Some(paused_at) = self.paused_at.take() else {
            return;
        };
        let audiobook = self.state.current_track().is_some_and(|t| t.audiobook);
        let rewind = settings::pause_rewind_seconds(
            self.state.settings.resume_rewind,
            self.state.settings.smart_rewind,
            audiobook,
            state::now().saturating_sub(paused_at),
        );
        let position = self.position();
        if rewind > 0. && position > 0. {
            self.seek((position - rewind).max(0.));
        }
    }

    pub(super) fn seek(&mut self, pos: f64) {
        // A live stream cannot be seeked.
        if self.state.current_station.is_some() {
            return;
        }
        self.pending_progress_restore = None;
        self.checkpoint();
        self.paused_at = None;
        // A seek moves the "end of chapter" target to the chapter it lands in.
        self.sleep_chapter_end = None;
        let play = self.playback_intent();
        let max = self.duration();
        let pos = if max > 0. {
            pos.clamp(0., max)
        } else {
            pos.max(0.)
        };
        if self.loading_request.is_some() {
            // The existing worker will use the newest target after validation;
            // rapid seeks must not start another full-file read.
        } else if let Some(current) = self
            .state
            .current
            .clone()
            .filter(|_| self.expected_audio_request.is_some())
        {
            self.load_request = self.load_request.wrapping_add(1);
            self.expected_audio_request = Some(self.load_request);
            self.seek_engine(&current, pos, self.load_request);
        } else if let Some(id) = self.state.current.clone() {
            self.load_track(&id, play, Some(pos));
        }
        self.pending_play_intent = self.state.current.as_ref().map(|_| play);
        self.pending_position = self
            .state
            .current
            .clone()
            .map(|id| (id, pos, Instant::now()));
        self.last_save =
            state::instant_ago(Duration::from_secs(self.state.settings.checkpoint_seconds));
    }

    /// Restores the progress authority for the loaded file while seeking to its
    /// saved position. The explicit progress remains authoritative until that
    /// seek is acknowledged, so an older playback snapshot cannot overwrite it.
    pub(super) fn seek_progress(&mut self, id: &str, progress: state::Progress) -> bool {
        if self.state.current.as_deref() != Some(id) {
            return false;
        }
        if self.tag_resume.as_ref().is_some_and(|resume| resume.id == id) {
            self.set_status("Wait for file tags to finish before restoring this position");
            return false;
        }
        self.seek(progress.seconds);
        let Some(request) = self.loading_request.or(self.expected_audio_request) else {
            return false;
        };
        self.state.progress.insert(id.to_owned(), progress.clone());
        self.pending_progress_restore = Some((request, id.to_owned(), progress));
        self.progress_changed();
        true
    }

    /// Seeks the intended track. If the engine joined another file first, its
    /// id, path, speed and gain travel with the request so the worker can reopen
    /// this track instead of applying the seek to the joined decoder.
    pub(super) fn seek_engine(&mut self, id: &str, pos: f64, request: u64) {
        if let Some(track) = self.state.track(id) {
            self.audio.seek_track(
                id.to_owned(),
                track.path.clone(),
                pos,
                self.state.speed,
                self.replaygain_for(track),
                request,
            );
        } else {
            self.audio.seek_in(id, pos, request);
        }
    }

    pub(super) fn next(&mut self, back: bool, automatic: bool) {
        if !automatic {
            self.cancel_playing_drop();
        }
        // Next and Previous move along the station list while a station plays;
        // a stream never ends by itself, so there is no automatic step.
        if self.state.current_station.is_some() {
            if !automatic {
                self.step_station(back);
            }
            return;
        }
        if self.state.queue.is_empty() {
            return;
        }
        // The step after a track that played to its end is planned and then taken.
        if automatic && !back {
            self.advance_automatically();
            return;
        }
        let current = self.state.current.clone();
        let current_index = current
            .as_ref()
            .and_then(|id| self.state.queue.iter().position(|q| q == id));
        let i = current_index.unwrap_or(0);
        if back && self.position() > 3. && !self.state.shuffle {
            self.seek(0.);
            return;
        }
        let next = if self.state.shuffle {
            if back {
                // Tracks removed since they were played cannot be returned to.
                let mut previous = self.state.shuffle_history.pop();
                while previous
                    .as_ref()
                    .is_some_and(|id| self.state.track(id).is_none())
                {
                    previous = self.state.shuffle_history.pop();
                }
                previous
            } else {
                let candidates: Vec<_> = self
                    .state
                    .queue
                    .iter()
                    .filter(|id| {
                        Some(*id) != current.as_ref() && !self.state.shuffle_history.contains(id)
                    })
                    .cloned()
                    .collect();
                let candidates = if candidates.is_empty() {
                    self.state.shuffle_history.clear();
                    self.state
                        .queue
                        .iter()
                        .filter(|id| Some(*id) != current.as_ref())
                        .cloned()
                        .collect()
                } else {
                    candidates
                };
                if candidates.is_empty() {
                    None
                } else {
                    if let Some(id) = current.clone() {
                        self.state.shuffle_history.push(id);
                    }
                    Some(candidates[pick_index(&mut self.rng, candidates.len())].clone())
                }
            }
        } else if back {
            Some(self.state.queue[i.saturating_sub(1)].clone())
        } else if current_index.is_none() {
            self.state.queue.first().cloned()
        } else if i + 1 < self.state.queue.len() {
            Some(self.state.queue[i + 1].clone())
        } else if self.state.repeat == "queue" {
            self.state.queue.first().cloned()
        } else {
            None
        };
        if let Some(id) = next {
            if current.as_deref() == Some(id.as_str()) && self.state.track(&id).is_some() {
                self.seek(0.);
            } else {
                self.load_track(&id, true, None);
            }
        } else if back {
            // Previous without history restarts the track.
            self.seek(0.);
        }
        // A manual Next on the last track leaves the current track playing and
        // the saved state untouched.
    }

    /// The automatic step after the current track played to its end: the plan
    /// (a still-valid shuffle pick or a new one), its bookkeeping, then the next
    /// file loaded. Missing files are skipped here, by looking at the disk, only
    /// when the option asks for it: the gapless preload never does, and a next
    /// file that cannot be opened simply ends up in this path.
    pub(super) fn advance_automatically(&mut self) {
        // Repeating the track needs a track to repeat.
        if self.state.repeat == "track" && self.state.current.is_none() {
            return;
        }
        let plan = self.plan_auto_next();
        self.commit_auto_next(&plan);
        match plan {
            AutoNext::RepeatSame(id) => self.load_track(&id, true, Some(0.)),
            AutoNext::Track { id, .. } => {
                let mut id = id;
                if self.state.settings.queue_skip_missing {
                    let start = self.state.queue.iter().position(|q| q == &id).unwrap_or(0);
                    let found = first_existing(
                        &self.state.queue,
                        start,
                        self.state.repeat == "queue",
                        |q| self.state.track(q).is_some_and(|t| t.path.is_file()),
                    );
                    match found {
                        Some(found) => id = found,
                        None => {
                            self.finish_queue();
                            return;
                        }
                    }
                }
                self.load_track(&id, true, Some(0.));
            }
            // Everything these two needed was done by `commit_auto_next`.
            AutoNext::BookEnd { .. } | AutoNext::QueueEnd { .. } => {}
        }
    }

    /// What the automatic step after the current track will be, worked out
    /// without doing anything: the repeat modes, the shuffle pick, the end of the
    /// queue and the end of a book. A shuffle pick uses the random generator
    /// once and is then kept in `planned_next`, so asking again gives the same
    /// file until something it depends on changes (see `planned_is_valid`); the
    /// book end is judged anew every time. `commit_auto_next` does the
    /// bookkeeping when the step is really taken. The caller has dealt with a
    /// playing station and an empty queue, as `next` does.
    pub(super) fn plan_auto_next(&mut self) -> AutoNext {
        // A pick is only kept for as long as shuffle picks the next file.
        if !self.state.shuffle || self.state.repeat == "track" {
            self.planned_next = None;
        }
        if self.state.repeat == "track" {
            return match self.state.current.clone() {
                Some(id) => AutoNext::RepeatSame(id),
                None => AutoNext::QueueEnd {
                    clear_shuffle_history: false,
                },
            };
        }
        let (next, clear_shuffle_history) = if self.state.shuffle {
            self.shuffle_pick()
        } else {
            (self.linear_next(), false)
        };
        let Some(id) = next else {
            return AutoNext::QueueEnd {
                clear_shuffle_history,
            };
        };
        if self.ends_book(&id) {
            AutoNext::BookEnd {
                id,
                clear_shuffle_history,
            }
        } else {
            AutoNext::Track {
                id,
                clear_shuffle_history,
            }
        }
    }

    /// The queue entry after the current one; the first when the current track
    /// is not in the queue, or is last and the queue repeats.
    pub(super) fn linear_next(&self) -> Option<String> {
        let queue = &self.state.queue;
        let index = self
            .state
            .current
            .as_ref()
            .and_then(|id| queue.iter().position(|q| q == id));
        match index {
            None => queue.first().cloned(),
            Some(i) if i + 1 < queue.len() => Some(queue[i + 1].clone()),
            Some(_) if self.state.repeat == "queue" => queue.first().cloned(),
            Some(_) => None,
        }
    }

    /// A random queue entry other than the current track that is not in the
    /// shuffle history (when every one is, a repeating queue starts a new round,
    /// which the second value tells), or the pick kept from an earlier look.
    pub(super) fn shuffle_pick(&mut self) -> (Option<String>, bool) {
        if let Some(planned) = &self.planned_next {
            if let AutoNext::Track {
                id,
                clear_shuffle_history,
            } = &planned.plan
            {
                if self.planned_is_valid(planned) {
                    return (Some(id.clone()), *clear_shuffle_history);
                }
            }
            self.planned_next = None;
        }
        let current = self.state.current.clone();
        let candidates: Vec<String> = self
            .state
            .queue
            .iter()
            .filter(|id| Some(*id) != current.as_ref() && !self.state.shuffle_history.contains(id))
            .cloned()
            .collect();
        let wrapped = candidates.is_empty() && self.state.repeat == "queue";
        let candidates = if wrapped {
            self.state
                .queue
                .iter()
                .filter(|id| Some(*id) != current.as_ref())
                .cloned()
                .collect()
        } else {
            candidates
        };
        if candidates.is_empty() {
            return (None, wrapped);
        }
        let id = candidates[pick_index(&mut self.rng, candidates.len())].clone();
        if let Some(from) = current {
            self.planned_next = Some(Planned {
                from,
                plan: AutoNext::Track {
                    id: id.clone(),
                    clear_shuffle_history: wrapped,
                },
                shuffle: true,
                repeat: self.state.repeat.clone(),
            });
        }
        (Some(id), wrapped)
    }

    /// Whether the kept shuffle pick still stands: it was made for the track that
    /// plays now, with the same shuffle and repeat settings, and its file is still
    /// in the queue and the library.
    pub(super) fn planned_is_valid(&self, planned: &Planned) -> bool {
        self.state.current.as_deref() == Some(planned.from.as_str())
            && self.state.shuffle == planned.shuffle
            && self.state.repeat == planned.repeat
            && match &planned.plan {
                AutoNext::Track { id, .. } => {
                    self.state.queue.iter().any(|q| q == id) && self.state.track(id).is_some()
                }
                _ => false,
            }
    }

    /// Whether going on to `next` leaves the book the current audiobook track
    /// belongs to, and the settings say to stop there.
    pub(super) fn ends_book(&self, next: &str) -> bool {
        if !(self.state.settings.stop_at_book_end || self.state.sleep_mode == "book") {
            return false;
        }
        match (self.state.current_track(), self.state.track(next)) {
            (Some(a), Some(b)) => a.audiobook && a.book_key() != b.book_key(),
            _ => false,
        }
    }

    /// The bookkeeping of an automatic step that is taken: the shuffle history, a
    /// stop at the end of a book (the "end of book" sleep timer is used up), the
    /// finished queue. Loading the next file is up to the caller. Any plan kept
    /// for this step is spent.
    pub(super) fn commit_auto_next(&mut self, plan: &AutoNext) {
        self.planned_next = None;
        match plan {
            AutoNext::RepeatSame(_) => {}
            AutoNext::Track {
                clear_shuffle_history,
                ..
            } => self.record_shuffle_step(*clear_shuffle_history),
            AutoNext::BookEnd {
                clear_shuffle_history,
                ..
            } => {
                self.record_shuffle_step(*clear_shuffle_history);
                self.state.was_playing = false;
                if self.state.sleep_mode == "book" {
                    self.state.sleep_mode = "off".into();
                }
            }
            AutoNext::QueueEnd {
                clear_shuffle_history,
            } => {
                if *clear_shuffle_history {
                    self.state.shuffle_history.clear();
                }
                self.finish_queue();
            }
        }
    }

    /// A shuffle pick was taken: a new round starts first when the pick wrapped
    /// the queue, and the track that ends now joins the history.
    pub(super) fn record_shuffle_step(&mut self, clear_shuffle_history: bool) {
        if !self.state.shuffle {
            return;
        }
        if clear_shuffle_history {
            self.state.shuffle_history.clear();
        }
        if let Some(id) = self.state.current.clone() {
            self.state.shuffle_history.push(id);
        }
    }

    pub(super) fn finish_queue(&mut self) {
        self.state.was_playing = false;
        self.set_status("Queue finished");
        if self.state.sleep_mode == "book" {
            self.state.sleep_mode = "off".into();
        }
    }

    /// The engine went from `c.from` to `c.to` by itself (a gapless join). Each
    /// change comes once, before the snapshot that already shows the new track.
    ///
    /// The source request must still be the one the app expects before any end
    /// progress changes. A stale join is ignored (and its joined audio is paused
    /// only while that same request remains current). For a valid request, the
    /// finished track is marked and either the planned join is committed or the
    /// normal end-of-track path takes over.
    pub(super) fn handle_track_change(&mut self, c: TrackChange) {
        if self.expected_audio_request != Some(c.from_request_id) {
            // This join belongs to a stale source request. Stop its joined file
            // only if a newer load has not already replaced it.
            self.audio.pause_if_request(c.request_id);
            return;
        }
        // The preload that made this change is spent, however the rest goes.
        if self
            .preload
            .as_ref()
            .is_some_and(|preload| preload.request_id == c.request_id)
        {
            self.preload = None;
        }
        let seconds = c.from_duration.max(c.from_reached);
        if self.state.track(&c.from).is_some() {
            self.mark_finished(&c.from, seconds);
        }
        if c.from_early {
            self.add_message(EARLY_END_NOTE.to_owned());
        }
        if self.loading_request.is_some() {
            // The engine may have joined `c.to` while the replacement chosen by
            // the user is still being validated. Stop that transient file; the
            // pending validation will load the selected track when it answers.
            self.audio.pause_if_request(c.request_id);
            return;
        }
        if self.state.current.as_deref() != Some(c.from.as_str()) {
            return;
        }
        let plan = self
            .preload_plan()
            .filter(|(_, wish)| wish.from == c.from && wish.to == c.to);
        let Some((plan, _)) = plan else {
            self.undo_unwanted_join(&c, seconds);
            return;
        };
        self.commit_auto_next(&plan);
        self.enter_track(&c.to, c.request_id);
        if self.state.settings.remove_played && c.from != c.to {
            self.state.queue.retain(|q| q != &c.from);
            self.view_revision = self.view_revision.wrapping_add(1);
        }
        // The chapters were set up by `enter_track`. The engine switched the
        // ReplayGain on the boundary sample; this makes the live value match.
        self.apply_replaygain();
        self.apply_skip_silence();
        self.save();
    }

    /// The engine joined the next file, but the player no longer wants that (see
    /// `handle_track_change`): the second half of the end of a track runs as it
    /// would have without the join. A load of the right file replaces the
    /// engine's; when nothing is to follow, the engine is paused and the track
    /// is left as one that ended, so that Play loads it again.
    pub(super) fn undo_unwanted_join(&mut self, c: &TrackChange, seconds: f64) {
        // The snapshot is a tick old: it must not take the end of the finished
        // track back to where it was then, when the load below saves it.
        if self.snapshot.loaded.as_deref() == Some(c.from.as_str()) && seconds.is_finite() {
            self.snapshot.position = seconds;
        }
        self.ended_handled = true;
        self.after_track_end(&c.from);
        if self.loading_request.is_none() {
            self.audio.pause();
            self.expected_audio_request = None;
            self.snapshot = Snapshot::default();
            self.state.was_playing = false;
        }
        self.save();
    }

    /// The loaded track played to its end (the snapshot says so, once per play):
    /// it is marked finished, a stop request is honoured, the queue goes on and
    /// a played file may leave the queue.
    pub(super) fn handle_ended(&mut self) {
        if !(self.snapshot.loaded == self.state.current
            && !self.snapshot.busy
            && self.snapshot.ended
            && self.snapshot.error.as_deref().is_none_or(is_early_end_note)
            && !self.ended_handled)
        {
            return;
        }
        self.ended_handled = true;
        self.checkpoint();
        if let Some(id) = self.state.current.clone() {
            self.mark_finished(&id, self.snapshot.duration);
            self.after_track_end(&id);
        }
        self.save();
    }

    /// What follows the end of the current track `id`: a stop request is honoured
    /// (and used up), the queue goes on and a played file may leave the queue.
    pub(super) fn after_track_end(&mut self, id: &str) {
        let stop = self.state.stop_after || self.state.sleep_mode == "chapter";
        self.state.stop_after = false;
        if self.state.sleep_mode == "chapter" {
            // The last chapter (or a file without chapters) ends with the file.
            self.state.sleep_mode = "off".into();
            self.set_status("Sleep timer: chapter ended");
        }
        if self.state.settings.auto_advance && !stop {
            self.next(false, true);
        }
        let repeating_current = self.state.repeat == "track"
            && self.state.current.as_deref() == Some(id);
        if self.state.settings.remove_played && !repeating_current {
            self.state.queue.retain(|q| q != id);
            self.view_revision = self.view_revision.wrapping_add(1);
        }
    }

    /// `id` played to its end: it counts as finished (its position is the end, or
    /// `seconds` when that is later), one more play, played now. Private listening
    /// records nothing.
    pub(super) fn mark_finished(&mut self, id: &str, seconds: f64) {
        if self.state.settings.private_listening || self.private_track.as_deref() == Some(id) {
            return;
        }
        let seconds = if seconds.is_finite() { seconds } else { 0. };
        let p = self.state.progress.entry(id.to_owned()).or_default();
        p.completed = true;
        p.seconds = seconds.max(p.seconds);
        p.play_count = p.play_count.saturating_add(1);
        p.last_played = state::now();
        self.progress_changed();
    }

    /// The Stop button: stop, keep the position.
    pub(super) fn stop_playback(&mut self) {
        if self.state.current_station.is_some() {
            // Stopping a station drops the connection; Play connects it again.
            self.audio.stop();
            self.pending_play_intent = Some(false);
            self.state.was_playing = false;
            self.save();
            return;
        }
        self.cancel_validation();
        self.audio.stop();
        self.state.was_playing = false;
        self.save();
    }

    pub(super) fn apply_replaygain(&self) {
        let gain = self
            .state
            .current_track()
            .map_or(0., |track| self.replaygain_for(track));
        self.audio.replay_gain(gain);
    }

    /// The ReplayGain of `track` in dB under the current settings: 0 when it is
    /// off; the track or album tag (the other kind when an album tag is missing,
    /// the fallback when there is none), held under the peak when clipping
    /// prevention is on, within -60 to +24 dB.
    pub(super) fn replaygain_for(&self, track: &Track) -> f32 {
        let settings = &self.state.settings;
        if settings.replaygain == "off" {
            return 0.;
        }
        let (tag, peak) = if settings.replaygain == "album" {
            (
                track.replaygain_album_db.or(track.replaygain_track_db),
                track.replaygain_album_peak.or(track.replaygain_track_peak),
            )
        } else {
            (track.replaygain_track_db, track.replaygain_track_peak)
        };
        let mut gain = tag.unwrap_or(settings.replaygain_fallback_db);
        if settings.replaygain_prevent_clipping {
            if let Some(peak) = peak.filter(|p| *p > 0.) {
                gain = gain.min(-20. * peak.log10());
            }
        }
        gain.clamp(-60., 24.)
    }

    /// Skip long silences was switched (the menu, Options) or a track started that
    /// may want it: tell the engine.
    pub(super) fn apply_skip_silence(&self) {
        self.audio.skip_silence(self.skip_silence_wanted());
    }
}

pub(super) fn requested_play_state(
    pending: Option<bool>,
    current: Option<&str>,
    snapshot: &Snapshot,
    remembered: bool,
) -> bool {
    if current.is_none() {
        return false;
    }
    pending.unwrap_or_else(|| {
        if snapshot.loaded.as_deref() == current && !snapshot.busy {
            snapshot.playing
        } else {
            remembered
        }
    })
}

/// What the automatic step after a track that played to its end will be.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum AutoNext {
    /// Repeat "track": the same file again from the start.
    RepeatSame(String),
    /// Play `id` next. `clear_shuffle_history`: the shuffle pick wrapped around
    /// the queue (every other file was already played), so a new round starts.
    Track {
        id: String,
        clear_shuffle_history: bool,
    },
    /// The next file `id` is in another book: playback stops here.
    BookEnd {
        id: String,
        clear_shuffle_history: bool,
    },
    /// Nothing follows: the queue is finished.
    QueueEnd { clear_shuffle_history: bool },
}

/// A shuffle pick kept for the automatic next step so that it is chosen once,
/// however often the plan is looked at: the track it was made for, the plan (always
/// a `Track`) and the settings it depends on.
#[derive(Clone, Debug)]
pub(super) struct Planned {
    pub(super) from: String,
    pub(super) plan: AutoNext,
    pub(super) shuffle: bool,
    pub(super) repeat: String,
}

/// splitmix64: advances `state` and returns 64 well-mixed bits.
pub(super) fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Seed for the shuffle generator, taken once at startup. The wall clock alone
/// is too coarse on Windows and macOS, so addresses and the process id join it.
pub(super) fn random_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let on_stack = 0u8;
    let on_heap = Box::new(0u8);
    nanos
        ^ (&on_stack as *const u8 as usize as u64).rotate_left(21)
        ^ (&*on_heap as *const u8 as usize as u64).rotate_left(43)
        ^ (std::process::id() as u64).rotate_left(32)
}

/// A uniformly distributed index in `0..len` (`len > 0`).
pub(super) fn pick_index(rng: &mut u64, len: usize) -> usize {
    ((splitmix64(rng) as u128 * len as u128) >> 64) as usize
}

/// The time at which the loaded track went from playing to paused, kept while
/// it stays paused (`None` while playing, after a load, or when the pause was
/// not seen). `loaded_current` says the engine holds the track the player shows.
pub(super) fn track_pause(
    paused_at: Option<u64>,
    playing: bool,
    playing_before: bool,
    loaded_current: bool,
    now: u64,
) -> Option<u64> {
    if playing {
        None
    } else if playing_before && loaded_current {
        Some(now)
    } else {
        paused_at
    }
}

/// Where a resume from the stored position starts: the stored position less the
/// rewind (the fixed setting, or the smart value for an audiobook after time
/// away since `last_played`), never before 0:00. Nothing is rewound to when
/// positions are not remembered or the file was finished.
pub(super) fn resume_start(
    settings: &Settings,
    progress: &state::Progress,
    audiobook: bool,
    now: u64,
) -> f64 {
    if !settings.remember_positions || progress.completed {
        return 0.;
    }
    let away = (progress.last_played > 0).then(|| now.saturating_sub(progress.last_played));
    let rewind = settings::resume_rewind_seconds(
        settings.resume_rewind,
        settings.smart_rewind,
        audiobook,
        away,
    );
    (progress.seconds - rewind).max(0.)
}

pub(super) fn playback_position(
    state: &Session,
    snapshot: &Snapshot,
    pending: Option<&(String, f64, Instant)>,
) -> f64 {
    if let Some((id, pos, _)) = pending {
        if state.current.as_deref() == Some(id) {
            return *pos;
        }
    }
    if snapshot.loaded.is_some() && snapshot.loaded == state.current {
        snapshot.position
    } else {
        state
            .current
            .as_ref()
            .and_then(|id| state.progress.get(id))
            .map(|p| p.seconds)
            .unwrap_or(0.)
    }
}

pub(super) fn accepted_snapshot(
    snapshot: Snapshot,
    expected_request: Option<u64>,
    current: Option<&str>,
) -> Snapshot {
    if !snapshot.busy
        && expected_request == Some(snapshot.request_id)
        && snapshot.loaded.as_deref() == current
    {
        snapshot
    } else {
        Snapshot::default()
    }
}

pub(super) fn loop_seek_target(
    snapshot: &Snapshot,
    position: f64,
    pending: bool,
    a: Option<f64>,
    b: Option<f64>,
) -> Option<f64> {
    let (a, b) = a.zip(b)?;
    (snapshot.playing && !snapshot.busy && !pending && position >= b).then_some(a)
}
