use super::*;

impl PlayerApp {
    pub(super) fn tick(&mut self, ctx: &egui::Context) {
        self.events(ctx);
        if self.last_stop_poll.elapsed() >= Duration::from_secs(1) {
            self.last_stop_poll = Instant::now();
            let stop_request = self.profile.join("stop.request");
            if stop_request.exists() {
                let _ = std::fs::remove_file(stop_request);
                self.request_quit(ctx);
            }
        }
        if self
            .exit_after
            .is_some_and(|s| self.started.elapsed().as_secs_f64() >= s)
        {
            self.request_quit(ctx);
        }
        if self.restore_pending {
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        if self.startup_scan_pending && self.busy == 0 {
            self.startup_scan_pending = false;
            self.rescan_with(true);
        }
        if let Some(result) = self.persistence.result() {
            match result {
                Ok(()) => self.save_failed = false,
                Err(error) => {
                    // Raise the message when saving starts failing, not on every
                    // checkpoint while it keeps failing.
                    if !self.save_failed {
                        self.add_message(format!("Could not save progress: {error}"));
                    }
                    self.save_failed = true;
                }
            }
        }
        let poll = self.audio.poll();
        // Joins the engine made by itself come first, in order: the app follows
        // each one before it looks at the snapshot, which already shows the new
        // track (see `handle_track_change`).
        let joined = !poll.changes.is_empty();
        for change in poll.changes {
            self.handle_track_change(change);
        }
        let snapshot = poll.snapshot;
        let preload_status = poll.preload;
        self.skipped_seconds = poll.skipped_seconds;
        if snapshot.error.is_some() && self.snapshot.error.is_none() {
            self.checkpoint();
        }
        // Connecting is the engine being busy with the station asked for; the
        // snapshot of a busy engine is not taken (see `accepted_snapshot`).
        self.station_connecting = self.state.current_station.is_some()
            && self.expected_audio_request.is_some()
            && snapshot.busy;
        let engine_id = self.engine_current();
        self.snapshot =
            accepted_snapshot(snapshot, self.expected_audio_request, engine_id.as_deref());
        if joined
            && self.snapshot.loaded.is_some()
            && self.snapshot.loaded == self.state.current
            && !self.snapshot.playing
        {
            // Paused in the moment of the join: the new track is not playing.
            self.state.was_playing = false;
        }
        self.station_tick();
        while let Some(event) = self.audio.take_device_event() {
            self.handle_device_event(event);
        }
        self.sync_chapters();
        self.sync_lyrics();
        self.fallback_fonts_tick(ctx);
        // A pause is the moment the loaded track stops playing; how long it
        // stays paused sets the smart rewind when playback continues.
        self.paused_at = track_pause(
            self.paused_at,
            self.snapshot.playing,
            self.playing_last_tick,
            self.state.current.is_some() && self.snapshot.loaded == self.state.current,
            state::now(),
        );
        self.playing_last_tick = self.snapshot.playing;
        if let Some((id, _, _)) = &self.pending_position {
            if pending_request_acknowledged(&self.snapshot, id) {
                let restore = self.pending_progress_restore.take().filter(|(request, track, _)| {
                    self.expected_audio_request == Some(*request)
                        && track == id
                        && self.snapshot.error.is_none()
                });
                let restored_authority = restore.is_some();
                self.pending_position = None;
                self.pending_play_intent = None;
                if restored_authority {
                    self.mark_restored_end_handled();
                    // Persist the restored authority before a later ordinary
                    // checkpoint can sample a stale engine snapshot.
                    self.persistence.save(self.state.clone());
                    self.last_save = Instant::now();
                } else if self.snapshot.error.is_none()
                    && !self.state.settings.private_listening
                    && self.private_track != self.state.current
                {
                    if let Some(id) = self.state.current.clone() {
                        self.state.progress.entry(id).or_default().completed = false;
                        self.progress_changed();
                    }
                }
                // The request moved playback away from the end; the next end of
                // this track must be handled again.
                if !self.snapshot.ended {
                    self.ended_handled = false;
                }
                if !restored_authority {
                    self.last_save = state::instant_ago(Duration::from_secs(
                        self.state.settings.checkpoint_seconds,
                    ));
                }
            }
        }
        if self.snapshot.error != self.seen_audio_error {
            self.seen_audio_error = self.snapshot.error.clone();
            if let Some(error) = self.snapshot.error.clone() {
                if let Some(label) = self.current_station_label() {
                    // A station that cannot be reached is no reason for a dialog:
                    // the status bar and the station row say it.
                    self.set_status(format!("{label}: {}", short_error(&error)));
                } else {
                    self.error = Some(error);
                    self.state.was_playing = false;
                }
            }
        }
        if self.initial_restore {
            if !self.silent {
                if let Some(id) = self.state.current.clone() {
                    let completed = self.state.progress.get(&id).is_some_and(|p| p.completed);
                    let play = !self.suppress_autoplay
                        && !completed
                        && !self.state.stop_after
                        && (self.state.settings.startup_behavior == "play"
                            || (self.state.settings.startup_behavior == "previous"
                                && self.state.was_playing));
                    if !completed {
                        let selection = self.state.selected.clone();
                        let start = resume_start(
                            &self.state.settings,
                            &self.state.progress.get(&id).cloned().unwrap_or_default(),
                            self.state.track(&id).is_some_and(|track| track.audiobook),
                            state::now(),
                        );
                        self.load_track(&id, play, Some(start));
                        self.state.selected = selection;
                        self.scroll_to_current = false;
                    }
                } else if let Some(id) = self.state.current_station.clone() {
                    // A station comes back selected and paused; it connects at the
                    // start only when the startup setting says "playing".
                    if self.state.station(&id).is_some() {
                        let play = !self.suppress_autoplay
                            && self.state.settings.startup_behavior == "play";
                        self.begin_station(&id, play);
                    } else {
                        self.state.current_station = None;
                    }
                }
            }
            self.initial_restore = false;
        }
        self.handle_ended();
        if let Some(target) = loop_seek_target(
            &self.snapshot,
            self.position(),
            self.pending_position.is_some(),
            self.state.ab_a,
            self.state.ab_b,
        ) {
            self.seek(target);
        }
        self.update_preload(&preload_status);
        if let Some(deadline) = self.state.sleep_deadline {
            let remaining = deadline.saturating_sub(state::now());
            if remaining == 0 {
                self.cancel_validation();
                self.audio.pause();
                self.state.was_playing = false;
                self.state.sleep_deadline = None;
                self.state.sleep_mode = "off".into();
                self.audio.volume(if self.state.muted {
                    0.
                } else {
                    self.state.volume
                });
                self.save();
                self.set_status("Sleep timer finished — playback paused");
            } else if self.state.settings.sleep_fade_seconds > 0
                && remaining < self.state.settings.sleep_fade_seconds
            {
                self.audio.volume(if self.state.muted {
                    0.
                } else {
                    self.state.volume * remaining as f32
                        / self.state.settings.sleep_fade_seconds as f32
                });
            }
        }
        let chapter_left = self.chapter_sleep_tick();
        if self.last_save.elapsed().as_secs() >= self.state.settings.checkpoint_seconds {
            self.save();
        }
        if self.state.settings.scan_interval_minutes > 0
            && self.last_scan.elapsed().as_secs() >= self.state.settings.scan_interval_minutes * 60
        {
            self.rescan_with(true);
        }
        let drops = ctx.input(|i| {
            if self.tray_hidden || i.viewport().visible() == Some(false) {
                return Vec::new();
            }
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect::<Vec<_>>()
        });
        if !drops.is_empty() {
            self.start_playing_drop(drops);
        }
        // While the player sits in the tray with its popup closed, only `logic`
        // runs; tick slowly, but keep the fine cadence where timing matters.
        let hidden = self.tray_hidden && !self.tray_popup_open;
        let loading = self.loading_request.is_some() || self.pending_position.is_some();
        let sleep_close = self.state.sleep_deadline.is_some_and(|deadline| {
            deadline.saturating_sub(state::now()) <= self.state.settings.sleep_fade_seconds + 2
        });
        let urgent = self.busy > 0
            || loading
            || (hidden
                && (sleep_close || (self.state.ab_a.is_some() && self.state.ab_b.is_some())));
        let remaining = (self.snapshot.loaded == self.state.current && self.snapshot.duration > 0.)
            .then(|| self.snapshot.duration - self.snapshot.position);
        // A stream has no end to wake for: a hidden player looks about once a second.
        let remaining = if self.state.current_station.is_some() {
            Some(60.)
        } else {
            remaining
        };
        // An armed "end of chapter" sleep is the nearer end.
        let remaining = match (remaining, chapter_left) {
            (Some(file), Some(chapter)) => Some(file.min(chapter)),
            (file, chapter) => file.or(chapter),
        };
        self.update_window_title(ctx);
        // The request folder is read about twice a second, in the tray too.
        let mut wake =
            tick_interval(hidden, self.snapshot.playing, urgent, remaining).min(REQUEST_POLL);
        // Close to the chapter end the player looks again exactly then, so the
        // pause lands on the boundary and not up to a tick later.
        if let Some(delay) =
            chapter_left.and_then(|left| chapter_sleep_wake(left, self.state.speed))
        {
            wake = wake.min(delay);
        }
        ctx.request_repaint_after(wake);
    }

    /// One output-device event from the engine: a lost device pauses playback (the
    /// engine kept the exact position), a restored one is only news.
    pub(super) fn handle_device_event(&mut self, event: DeviceEvent) {
        let note = event.note();
        match event {
            DeviceEvent::Lost(_) => {
                self.state.was_playing = false;
                // Save the position and the paused state.
                self.save();
                self.add_message(note);
            }
            DeviceEvent::Restored(_) => self.set_status(note),
        }
    }

    /// A restored completed position at the end is already finished. Treating
    /// its seek acknowledgement as a fresh end would increment play count and
    /// apply Remove Played to the restored profile.
    pub(super) fn mark_restored_end_handled(&mut self) {
        if self.snapshot.ended
            && self
                .state
                .current
                .as_deref()
                .and_then(|id| self.state.progress.get(id))
                .is_some_and(|progress| progress.completed)
        {
            self.ended_handled = true;
        }
    }

    /// Second-launch hand-off and global media keys; runs every frame after the
    /// tray was serviced, in the tray too.
    pub(super) fn integration_tick(&mut self, ctx: &egui::Context) {
        self.service_requests(ctx);
        while let Some(key) = winintegration::poll_media_key() {
            self.run_media_key(key);
        }
        self.sync_media_keys();
    }

    /// Reads the requests other launches left in the profile and acts on them when
    /// the player can: never while the session is still being restored or a
    /// confirmation is open (they wait in `pending_requests`).
    pub(super) fn service_requests(&mut self, ctx: &egui::Context) {
        if request_poll_due(self.last_request_poll.elapsed()) {
            self.last_request_poll = Instant::now();
            queue_requests(
                &mut self.pending_requests,
                instance::take_requests(&self.profile),
            );
        }
        if requests_blocked(self.initial_restore || self.restore_pending, self.confirm.is_some()) {
            return;
        }
        let Some(paths) = drain_requests(&mut self.pending_requests) else {
            return;
        };
        // Someone asked for the player: a start hidden in the tray is over.
        self.start_hidden_pending = false;
        if !paths.is_empty() {
            let append = self.state.settings.external_files_append;
            self.import(paths, false, append);
        }
        self.restore_from_tray(ctx, true);
    }

    /// Does what the player's own button for this media key does.
    pub(super) fn run_media_key(&mut self, key: MediaKey) {
        match media_command(key) {
            MediaCommand::TogglePlay => self.toggle_play(),
            MediaCommand::Next => self.next(false, false),
            MediaCommand::Previous => self.next(true, false),
            MediaCommand::Stop => self.stop_playback(),
        }
    }

    /// Asks the tray thread for the media keys, or releases them, only when the
    /// wish changes (a key another program holds is not retried every frame).
    pub(super) fn sync_media_keys(&mut self) {
        let wanted = media_keys_wanted(
            self.state.settings.media_keys,
            self.tray_available,
            self.state.current.is_some() || self.state.current_station.is_some(),
        );
        if wanted != self.media_keys_on {
            self.media_keys_on = wanted;
            winintegration::set_media_keys_enabled(wanted);
        }
    }

    /// Register or unregister the file associations on a worker thread (the
    /// registry and the shell notification can take a moment).
    pub(super) fn start_association_job(&mut self, register: bool) {
        if self.association_busy {
            return;
        }
        self.association_busy = true;
        self.association_error = None;
        let tx = self.job_sender();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(|| association_job(register))
                .unwrap_or_else(|_| Err("The request failed unexpectedly.".into()));
            tx.send(Event::Associations {
                result,
                state: winintegration::association_state(),
            });
        });
    }
}

/// How soon `tick` must run again. `urgent` work (loads, imports, a sleep fade,
/// an A-B loop) and playback in a visible player use the fine 200 ms cadence.
/// A player hidden in the tray ticks about once a second, but wakes in time for
/// the end of the track so the next one starts as promptly as before.
pub(super) fn tick_interval(
    hidden: bool,
    playing: bool,
    urgent: bool,
    remaining: Option<f64>,
) -> Duration {
    const FINE: Duration = Duration::from_millis(200);
    const SLOW: Duration = Duration::from_secs(1);
    if urgent {
        FINE
    } else if !playing {
        SLOW
    } else if !hidden {
        FINE
    } else {
        match remaining {
            Some(left) if left > 1.2 => Duration::from_secs_f64((left - 1.0).min(1.)),
            _ => FINE,
        }
    }
}

/// How often the profile's request folder is read for a second launch's hand-off.
pub(super) const REQUEST_POLL: Duration = Duration::from_millis(500);

/// Most hand-off requests kept while the player cannot act on them.
pub(super) const MAX_QUEUED_REQUESTS: usize = 64;

/// Whether the request folder should be read now. A frame that comes a little
/// early still counts, so a repaint timer that fires just short of the
/// interval does not push the next read a whole tick away.
pub(super) fn request_poll_due(since_last: Duration) -> bool {
    since_last + Duration::from_millis(50) >= REQUEST_POLL
}

/// Requests wait while the session is being restored and while a confirmation
/// is open: an import then would change what the dialog is about.
pub(super) fn requests_blocked(restoring: bool, confirm_open: bool) -> bool {
    restoring || confirm_open
}

/// Appends `incoming` to `queue`. A `Show` adds nothing when something is already
/// queued (every request brings the window forward anyway) and the queue never
/// grows past `MAX_QUEUED_REQUESTS`; the newest requests are dropped then.
pub(super) fn queue_requests(
    queue: &mut VecDeque<instance::Request>,
    incoming: Vec<instance::Request>,
) {
    for request in incoming {
        if queue.len() >= MAX_QUEUED_REQUESTS {
            break;
        }
        if request == instance::Request::Show && !queue.is_empty() {
            continue;
        }
        queue.push_back(request);
    }
}

/// Empties the queue: `None` when it was empty, otherwise the paths of every
/// `Open` in arrival order (empty for a lone `Show`). Opening files that came
/// from several launches at once (Explorer starts one launch per selected file)
/// is one import, so a replacing import keeps all of them.
pub(super) fn drain_requests(queue: &mut VecDeque<instance::Request>) -> Option<Vec<PathBuf>> {
    if queue.is_empty() {
        return None;
    }
    let mut paths = Vec::new();
    for request in queue.drain(..) {
        if let instance::Request::Open(opened) = request {
            paths.extend(opened);
        }
    }
    Some(paths)
}

/// What a global media key does in the player.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MediaCommand {
    TogglePlay,
    Next,
    Previous,
    Stop,
}

pub(super) fn media_command(key: MediaKey) -> MediaCommand {
    match key {
        MediaKey::PlayPause => MediaCommand::TogglePlay,
        MediaKey::Next => MediaCommand::Next,
        MediaKey::Previous => MediaCommand::Previous,
        MediaKey::Stop => MediaCommand::Stop,
    }
}

/// Whether the player should hold the global media keys: the setting is on, the
/// tray (whose window owns the keys) works and a track is loaded, so the keys
/// stay free for other programs while BS Player has nothing to play.
pub(super) fn media_keys_wanted(setting: bool, tray_available: bool, track_loaded: bool) -> bool {
    setting && tray_available && track_loaded
}

/// The Register / Unregister request of the Options window, run on a worker thread.
pub(super) fn association_job(register: bool) -> Result<(), String> {
    if register {
        let exe = std::env::current_exe()
            .map_err(|error| format!("Could not find the program file: {error}"))?;
        winintegration::register_associations(&exe)
    } else {
        winintegration::unregister_associations()
    }
}
