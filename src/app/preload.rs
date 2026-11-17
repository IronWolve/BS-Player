use super::*;

impl PlayerApp {
    // ---- Gapless playback: the preload wish, the join and what follows it -----
    //
    // The engine can continue from one file into the next without a gap when it
    // has opened the next file ahead of time. The player decides what that file is
    // (`preload_wish`), asks for it, keeps asking for exactly that (`update_preload`)
    // and, when the engine reports that it went on by itself, follows
    // (`handle_track_change`). No hook is needed where the wish can change (queue
    // edits, shuffle and repeat, stop after, sleep): the next look sees another
    // wish and withdraws the old one.

    /// Whether what the engine plays and how far it is make a preload worth
    /// asking for: the loaded track is the current one and is in order, it is
    /// playing (or paused for under `PRELOAD_PAUSED_SECONDS`), and its end is at
    /// most `PRELOAD_LEAD_SECONDS` of listening time away.
    pub(super) fn preload_engine_ready(&self) -> bool {
        let snapshot = &self.snapshot;
        let Some(current) = self.state.current.as_deref() else {
            return false;
        };
        if self.initial_restore
            || self.loading_request.is_some()
            || self.pending_position.is_some()
            || self.expected_audio_request.is_none()
            || snapshot.loaded.as_deref() != Some(current)
            || current.starts_with("stream:")
            || snapshot.busy
            || snapshot.error.is_some()
            || snapshot.ended
            || !snapshot.duration.is_finite()
            || snapshot.duration <= 0.
        {
            return false;
        }
        if !snapshot.playing {
            // A track paused for a while would hold the next file open for nothing;
            // Play asks again.
            let fresh = self
                .paused_at
                .is_some_and(|at| state::now().saturating_sub(at) < PRELOAD_PAUSED_SECONDS);
            if !fresh {
                return false;
            }
        }
        let speed = f64::from(self.state.speed);
        // A position that is not a number makes this false, like any comparison.
        speed > 0. && (snapshot.duration - snapshot.position) / speed <= PRELOAD_LEAD_SECONDS
    }

    /// Everything about the automatic next step that decides whether the next file
    /// may be joined without a gap, except what the engine is doing now: gapless
    /// and automatic advance are on, no station, no stop after the current track,
    /// no "end of chapter" sleep, no A-B loop, no tag save writing files, and the
    /// plan is to repeat the track or to play another file of the same book or
    /// list that is not a superseded record and starts at the speed that plays
    /// now (the engine has one speed). The plan is returned with the wish, for
    /// `handle_track_change`; asking for it may draw the shuffle pick, which is
    /// then kept.
    pub(super) fn preload_plan(&mut self) -> Option<(AutoNext, PreloadWish)> {
        if !(self.state.settings.gapless && self.state.settings.auto_advance)
            || self.state.current_station.is_some()
            || self.state.stop_after
            || self.state.sleep_mode == "chapter"
            || (self.state.ab_a.is_some() && self.state.ab_b.is_some())
            || self.tag_save_running
            || self.tag_resume.is_some()
            || self.state.queue.is_empty()
        {
            return None;
        }
        let from = self.state.current.clone()?;
        let plan = self.plan_auto_next();
        let to = match &plan {
            AutoNext::RepeatSame(id) | AutoNext::Track { id, .. } => id.clone(),
            // A book end and a queue end are stops: the track simply ends.
            AutoNext::BookEnd { .. } | AutoNext::QueueEnd { .. } => return None,
        };
        let next = self.state.track(&to)?;
        if next.superseded || to.starts_with("stream:") {
            return None;
        }
        // The same file again keeps the speed; another one starts at its own.
        let next_speed = if to == from {
            self.state.speed
        } else {
            self.speed_for_track(next)
        };
        if next_speed != self.state.speed {
            return None;
        }
        let wish = PreloadWish {
            from,
            to,
            path: next.path.clone(),
            gain_db: self.replaygain_for(next),
        };
        Some((plan, wish))
    }

    /// The file the engine should have ready to follow the playing one, if any:
    /// the plan is joinable (see `preload_plan`) and the engine is in a state
    /// where it can be asked (see `preload_engine_ready`).
    pub(super) fn preload_wish(&mut self) -> Option<PreloadWish> {
        if !self.preload_engine_ready() {
            return None;
        }
        self.preload_plan().map(|(_, wish)| wish)
    }

    /// The end of the playing track is so near that the engine can no longer take
    /// a preload back without restarting the track: the audio thread has probably
    /// taken it already. Turning silence skipping on widens this, because a long
    /// silence at the end of a file is read ahead (and dropped) long before it
    /// would be heard.
    pub(super) fn in_last_moments(&self) -> bool {
        let snapshot = &self.snapshot;
        let speed = f64::from(self.state.speed);
        if snapshot.loaded.is_none() || !(snapshot.duration > 0.) || !(speed > 0.) {
            return false;
        }
        let window = PRELOAD_LAST_SECONDS
            + if self.skip_silence_wanted() {
                SKIP_SILENCE_LOOKAHEAD_SECONDS
            } else {
                0.
            };
        (snapshot.duration - snapshot.position) / speed <= window
    }

    /// Once per tick: reads what the engine says about the preload, and at most
    /// every `PRELOAD_CHECK_EVERY` looks at the wish and makes the preload match.
    pub(super) fn update_preload(&mut self, status: &PreloadStatus) {
        self.absorb_preload_status(status);
        if self.preload_checked.elapsed() < PRELOAD_CHECK_EVERY {
            return;
        }
        self.preload_checked = Instant::now();
        let wish = self.preload_wish();
        self.reconcile_preload(wish);
    }

    /// The engine's answer to the request this player made last (answers about
    /// anything else are not ours): a refusal ends the attempt, and it is not
    /// repeated until the wish changes; a preload the engine dropped by itself (a
    /// seek, a speed or device change) is forgotten, so it is asked for again if
    /// the wish still holds.
    pub(super) fn absorb_preload_status(&mut self, status: &PreloadStatus) {
        let Some(preload) = self.preload.as_mut() else {
            return;
        };
        if status.last_request != preload.request_id {
            return;
        }
        match (&status.state, preload.stage) {
            (PreloadState::Failed(_), PreloadStage::Sent) => preload.stage = PreloadStage::Failed,
            (PreloadState::None, PreloadStage::Sent) => self.preload = None,
            _ => {}
        }
    }

    /// Makes the preload match the wish: the same wish keeps what is there (a
    /// refused one is not asked again); another wish, or none, withdraws it and a
    /// wish starts a new one. In the last moments of the track a sent preload is
    /// left alone: taking it back would restart the playing track, and the join
    /// that follows is reconciled by `handle_track_change`.
    pub(super) fn reconcile_preload(&mut self, wish: Option<PreloadWish>) {
        if let (Some(preload), Some(wish)) = (&self.preload, &wish) {
            if preload.wish == *wish {
                return;
            }
        }
        if let Some(preload) = &self.preload {
            let sent = preload.stage == PreloadStage::Sent;
            if sent && self.in_last_moments() {
                return;
            }
            if sent {
                self.audio.cancel_preload();
            }
            self.preload = None;
        }
        if let Some(wish) = wish {
            self.start_preload(wish);
        }
    }

    /// Checks the next file off the UI thread, exactly as `load_track` does; the
    /// answer (`preload_validated`) sends the request to the engine. The request
    /// number comes from the same counter as every load, so it is unique and is
    /// what the snapshot reports once the engine has joined the file.
    pub(super) fn start_preload(&mut self, wish: PreloadWish) {
        let Some(track) = self.state.track(&wish.to).cloned() else {
            return;
        };
        self.load_request = self.load_request.wrapping_add(1);
        let request = self.load_request;
        self.preload = Some(Preload {
            wish,
            request_id: request,
            stage: PreloadStage::Validating,
        });
        let tx = self.job_sender();
        std::thread::spawn(move || {
            tx.send(Event::PreloadValidated {
                request,
                result: media::validate_for_playback(&track),
            });
        });
    }

    /// The check of the next file came back. Only the preload that asked for it
    /// counts. A file that is as the library knows it goes to the engine; any
    /// other outcome (it cannot be read, or the check found it changed) ends the
    /// attempt quietly: the normal track change handles and reports it when the
    /// file is really loaded.
    pub(super) fn preload_validated(&mut self, request: u64, result: Result<Track, String>) {
        let Some(preload) = self.preload.as_mut() else {
            return;
        };
        if preload.request_id != request || preload.stage != PreloadStage::Validating {
            return;
        }
        let unchanged = self.state.track(&preload.wish.to) == result.as_ref().ok();
        if !unchanged {
            preload.stage = PreloadStage::Failed;
            return;
        }
        preload.stage = PreloadStage::Sent;
        let wish = &preload.wish;
        self.audio.preload(PreloadRequest {
            after: wish.from.clone(),
            id: wish.to.clone(),
            path: wish.path.clone(),
            gain_db: wish.gain_db,
            request_id: request,
        });
    }

    /// The preload is not wanted any more: the engine is told to let go of it and
    /// it is forgotten.
    pub(super) fn drop_preload(&mut self) {
        if self
            .preload
            .take()
            .is_some_and(|preload| preload.stage == PreloadStage::Sent)
        {
            self.audio.cancel_preload();
        }
    }

    /// A load or a station is about to replace what plays: the engine drops the
    /// preload by itself then, so it is only asked to let go when that costs
    /// nothing (not in the last moments). The request is forgotten either way.
    pub(super) fn release_preload(&mut self) {
        let sent = self
            .preload
            .as_ref()
            .is_some_and(|preload| preload.stage == PreloadStage::Sent);
        if sent && !self.in_last_moments() {
            self.audio.cancel_preload();
        }
        self.preload = None;
    }

    /// The file a preload has the engine hold open (one that was sent), if any.
    pub(super) fn preloaded_file(&self) -> Option<&str> {
        self.preload
            .as_ref()
            .filter(|preload| preload.stage == PreloadStage::Sent)
            .map(|preload| preload.wish.to.as_str())
    }

    /// Whether skip long silences is in force for what is current: the setting is
    /// on and an audiobook file plays (a station and music never skip).
    pub(super) fn skip_silence_wanted(&self) -> bool {
        self.state.settings.skip_silence
            && self.state.current_station.is_none()
            && self
                .state
                .current_track()
                .is_some_and(|track| track.audiobook)
    }
}

/// How long before the end of the playing file (seconds of listening time) the
/// next one is opened ahead of time for a gapless join. A shorter file asks as
/// soon as it has started.
pub(super) const PRELOAD_LEAD_SECONDS: f64 = 20.;

/// The wish for a preload is looked at no more often than this.
pub(super) const PRELOAD_CHECK_EVERY: Duration = Duration::from_millis(250);

/// A track paused for longer than this (seconds) lets go of the next file; Play
/// asks for it again.
pub(super) const PRELOAD_PAUSED_SECONDS: u64 = 30;

/// Within this many seconds of listening time before the end, a preload that was
/// sent is not taken back (that would restart the playing track): the join
/// happens and `handle_track_change` sorts out what changed.
pub(super) const PRELOAD_LAST_SECONDS: f64 = 1.5;

/// With skip long silences on, a silence at the end of a file is read ahead and
/// dropped up to this long (seconds of source time) before it would be heard, so
/// the engine takes the next file that much earlier than the clock says.
pub(super) const SKIP_SILENCE_LOOKAHEAD_SECONDS: f64 = 10.5;

/// The file the engine should open ahead of time to follow `from` with, and how
/// loud it plays (see `PlayerApp::preload_wish`).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PreloadWish {
    pub(super) from: String,
    pub(super) to: String,
    pub(super) path: PathBuf,
    pub(super) gain_db: f32,
}

/// How far a preload has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PreloadStage {
    /// The file is being checked off the UI thread.
    Validating,
    /// The engine was asked (it may still be opening the file).
    Sent,
    /// Refused or failed: not tried again until the wish changes.
    Failed,
}

/// The preload this player asked for: the wish it answers, its request number
/// (the one the snapshot reports after a join) and its stage.
#[derive(Clone, Debug)]
pub(super) struct Preload {
    pub(super) wish: PreloadWish,
    pub(super) request_id: u64,
    pub(super) stage: PreloadStage,
}
