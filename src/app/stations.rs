use super::*;

// ---------------------------------------------------------------------------
// Internet radio stations (the Streams list)
// ---------------------------------------------------------------------------

/// What the engine calls a station: its snapshot reports `stream:<station id>`.
pub(super) const STREAM_PREFIX: &str = "stream:";

/// The tooltip of every control a live stream switches off.
pub(super) const LIVE_UNAVAILABLE: &str = "Not available for live streams";

pub(super) const LIVE_TIME_TIP: &str =
    "Time since you connected. A live stream has no length and no saved position.";

/// Longest live title the player shows or remembers, in characters.
pub(super) const LIVE_TITLE_CHARS: usize = 256;

/// The engine's name for the station with this id.
pub(super) fn stream_engine_id(id: &str) -> String {
    format!("{STREAM_PREFIX}{id}")
}

/// The left time label of a live stream: `LIVE 12:34`, the listening time.
pub(super) fn live_time_text(seconds: f64) -> String {
    format!("LIVE {}", time_text(seconds))
}

/// A menu item that a live stream switches off (the reason is its tooltip).
pub(super) fn live_off_item(
    ui: &mut egui::Ui,
    live: bool,
    label: impl Into<String>,
) -> egui::Response {
    ui.add_enabled(!live, egui::Button::new(label.into()))
        .on_disabled_hover_text(LIVE_UNAVAILABLE)
}

/// What a station is doing, for the player bar, the status bar and the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum StationPhase {
    /// Not connected: stopped, paused or not started yet.
    Idle,
    Connecting,
    Live,
    /// The connection dropped and is being made again (the attempt number).
    Reconnecting(u32),
    /// The station could not be reached or stopped for good (a short sentence).
    Failed(String),
}

impl StationPhase {
    /// The status text (`None` while idle).
    pub(super) fn text(&self) -> Option<String> {
        match self {
            Self::Idle => None,
            Self::Connecting => Some("Connecting…".to_owned()),
            Self::Live => Some("Live".to_owned()),
            Self::Reconnecting(attempt) => Some(format!("Reconnecting… ({attempt})")),
            Self::Failed(message) => Some(message.clone()),
        }
    }
}

/// What the current station is doing. `connecting`: the engine is busy opening
/// it. `snapshot`: the engine snapshot when it is the station's (the default
/// one when none is accepted). `status`: the state of the open connection.
/// `wants_play`: Play was pressed and the engine has not answered yet.
pub(super) fn station_phase(
    connecting: bool,
    snapshot: &Snapshot,
    status: Option<&StreamStatus>,
    wants_play: bool,
) -> StationPhase {
    if connecting {
        return StationPhase::Connecting;
    }
    if snapshot.loaded.is_none() {
        return if wants_play {
            StationPhase::Connecting
        } else {
            StationPhase::Idle
        };
    }
    if !snapshot.playing {
        return match &snapshot.error {
            Some(error) => StationPhase::Failed(short_error(error)),
            None => StationPhase::Idle,
        };
    }
    match status {
        Some(StreamStatus::Reconnecting { attempt }) => StationPhase::Reconnecting(*attempt),
        Some(StreamStatus::Failed(message)) => StationPhase::Failed(short_error(message)),
        Some(StreamStatus::Connecting) => StationPhase::Connecting,
        Some(StreamStatus::Playing) | None => StationPhase::Live,
    }
}

/// The second line of the player bar and whether it is a failure: a failure
/// sentence, the connection state while there is no stream, else the live title
/// (`title`, or the `last` one while paused), else `Live` or `Paused`.
pub(super) fn station_line_text(
    phase: &StationPhase,
    title: Option<&str>,
    last: &str,
) -> (String, bool) {
    match phase {
        StationPhase::Failed(message) => (message.clone(), true),
        StationPhase::Connecting | StationPhase::Reconnecting(_) => {
            (phase.text().unwrap_or_default(), false)
        }
        StationPhase::Live => (title.unwrap_or("Live").to_owned(), false),
        StationPhase::Idle => {
            let shown = title.or_else(|| (!last.trim().is_empty()).then_some(last));
            (shown.unwrap_or("Paused").to_owned(), false)
        }
    }
}

/// The name a station goes by: the one the user gave it, else the one it
/// announced, else its host.
pub(super) fn station_display_name(station: &state::Station, live: Option<&StationLive>) -> String {
    if !station.name.trim().is_empty() {
        return station.name.clone();
    }
    match live {
        Some(live) if !live.name.trim().is_empty() => live.name.clone(),
        _ => state::station_label(station),
    }
}

/// A short name of the audio format a station sends (`audio/mpeg` is `MP3`).
pub(super) fn stream_kind(content_type: &str) -> String {
    match content_type.trim().to_ascii_lowercase().as_str() {
        "" => String::new(),
        "audio/mpeg" | "audio/mp3" | "audio/mpeg3" => "MP3".to_owned(),
        "audio/aac" | "audio/aacp" | "audio/x-aac" => "AAC".to_owned(),
        "audio/ogg" | "application/ogg" | "audio/vorbis" => "Ogg".to_owned(),
        "audio/flac" | "audio/x-flac" => "FLAC".to_owned(),
        other => other.to_owned(),
    }
}

/// The Type cell of a station row: bitrate and format of its last connection.
pub(super) fn station_kind_text(live: Option<&StationLive>) -> String {
    let Some(live) = live else {
        return String::new();
    };
    let kind = stream_kind(&live.content_type);
    match (live.bitrate, kind.is_empty()) {
        (Some(kbps), false) => format!("{kbps} kbps · {kind}"),
        (Some(kbps), true) => format!("{kbps} kbps"),
        (None, false) => kind,
        (None, true) => String::new(),
    }
}

/// "3 stations", or "2 of 3 stations" while the search hides some.
pub(super) fn stations_summary(shown: usize, total: usize) -> String {
    let word = if total == 1 { "station" } else { "stations" };
    if shown == total {
        format!("{total} {word}")
    } else {
        format!("{shown} of {total} {word}")
    }
}

/// The status note after a playlist import added stations.
pub(super) fn station_import_text(added: usize, existing: usize, skipped: usize) -> String {
    let mut parts = Vec::new();
    if added > 0 {
        parts.push(format!(
            "Added {added} {}",
            if added == 1 { "station" } else { "stations" }
        ));
    } else {
        parts.push("No new stations".to_owned());
    }
    if existing > 0 {
        parts.push(format!("{existing} already in the list"));
    }
    if skipped > 0 {
        parts.push(format!(
            "{skipped} not added (invalid address or the list is full)"
        ));
    }
    parts.join(" · ")
}

/// A live title as shown and remembered: control characters removed, at most
/// `LIVE_TITLE_CHARS` characters; `None` when nothing is left.
pub(super) fn clean_live_title(title: &str) -> Option<String> {
    let text: String = title
        .chars()
        .filter(|character| !character.is_control())
        .take(LIVE_TITLE_CHARS)
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// One row of the Streams list, made for the frame.
pub(super) struct StationRow {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) host: String,
    pub(super) kind: String,
    pub(super) title: String,
    pub(super) url: String,
    /// Why the last connection failed (shown red under the name).
    pub(super) error: Option<String>,
    pub(super) current: bool,
    /// What the current station is doing (second line of its name cell).
    pub(super) phase: Option<String>,
}

impl PlayerApp {
    /// What the engine calls what the player holds now: `stream:<id>` for a
    /// station, else the id of the file.
    pub(super) fn engine_current(&self) -> Option<String> {
        match &self.state.current_station {
            Some(id) => Some(stream_engine_id(id)),
            None => self.state.current.clone(),
        }
    }

    /// The name of the current station, if one is current.
    pub(super) fn current_station_label(&self) -> Option<String> {
        let station = self.state.current_station_ref()?;
        Some(station_display_name(
            station,
            self.station_live.get(&station.id),
        ))
    }

    /// The row selected in the Streams list, when it is still in the list.
    pub(super) fn selected_station(&self) -> Option<String> {
        self.station_selected
            .clone()
            .filter(|id| self.state.station(id).is_some())
    }

    /// Play is wanted (or on): the intent while the engine has not answered,
    /// else what the engine does.
    pub(super) fn station_intent(&self) -> bool {
        self.pending_play_intent.unwrap_or(self.snapshot.playing)
    }

    pub(super) fn station_phase(&self) -> StationPhase {
        let status = self
            .audio
            .stream_handle()
            .filter(|_| self.snapshot.loaded.is_some())
            .map(|handle| handle.status());
        station_phase(
            self.station_connecting,
            &self.snapshot,
            status.as_ref(),
            self.pending_play_intent == Some(true),
        )
    }

    /// The live title of the current station (kept while paused).
    pub(super) fn live_title_now(&self) -> Option<&str> {
        let id = self.state.current_station.as_deref()?;
        self.live_title
            .as_ref()
            .filter(|(owner, _)| owner == id)
            .map(|(_, title)| title.as_str())
    }

    /// The second line of the player bar for the current station.
    pub(super) fn station_line(&self) -> (String, bool) {
        let phase = self.station_phase();
        let last = self
            .state
            .current_station_ref()
            .map_or("", |station| station.last_title.as_str());
        station_line_text(&phase, self.live_title_now(), last)
    }

    /// The status bar text of the current station (name and state) and whether
    /// it is a failure.
    pub(super) fn station_status_text(&self) -> Option<(String, bool)> {
        let label = self.current_station_label()?;
        let phase = self.station_phase();
        let text = match &phase {
            StationPhase::Idle => "Paused".to_owned(),
            other => other.text().unwrap_or_default(),
        };
        Some((
            format!("{label} · {text}"),
            matches!(phase, StationPhase::Failed(_)),
        ))
    }

    /// The window title: `Station — Title` while a station is current.
    pub(super) fn station_window_title(&self) -> Option<String> {
        let label = self.current_station_label()?;
        Some(match self.live_title_now() {
            Some(title) => format!("{label} — {title}"),
            None => label,
        })
    }

    pub(super) fn update_window_title(&mut self, ctx: &egui::Context) {
        let wanted = self.station_window_title();
        if wanted != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(
                wanted.clone().unwrap_or_else(|| "BS Player".to_owned()),
            ));
            self.window_title = wanted;
        }
    }

    /// Whether what plays now is not saved: the setting, or a station started
    /// while it was on.
    pub(super) fn station_is_private(&self, id: &str) -> bool {
        self.state.settings.private_listening
            || self.private_track.as_deref() == Some(stream_engine_id(id).as_str())
    }

    /// The player lets go of its station because a file takes over.
    pub(super) fn leave_station(&mut self) {
        if self.state.current_station.take().is_some() {
            self.live_title = None;
            self.station_position = 0.;
            self.station_base = 0.;
            self.station_connecting = false;
        }
    }

    /// Shows the station list.
    pub(super) fn show_streams(&mut self) {
        self.state.section = state::STREAMS_SECTION.to_owned();
        self.state.scroll = 0.;
        self.restore_scroll = true;
    }

    /// Makes the station the current one instead of any file. With `connect`
    /// it connects now; without, it stays selected and paused (Play connects).
    /// Nothing about listening is saved for it: no position, no play count.
    pub(super) fn begin_station(&mut self, id: &str, connect: bool) {
        self.cancel_playing_drop();
        let Some(station) = self.state.station(id).cloned() else {
            return;
        };
        // A file's position is saved before the station takes over, and nothing
        // is prepared to follow it.
        self.checkpoint();
        self.release_preload();
        self.planned_next = None;
        let continuing = self.state.current_station.as_deref() == Some(id);
        // A file being checked or opened is dropped, and so is the file itself.
        self.loading_request = None;
        self.pending_position = None;
        self.state.current = None;
        self.state.current_station = Some(id.to_owned());
        self.state.was_playing = false;
        self.state.ab_a = None;
        self.state.ab_b = None;
        self.paused_at = None;
        self.sleep_chapter_end = None;
        if matches!(self.state.sleep_mode.as_str(), "chapter" | "book") {
            // Nothing ends at a chapter or a book in a live stream.
            self.state.sleep_mode = "off".into();
        }
        self.clear_chapters();
        self.clear_lyrics();
        self.seek_drag = None;
        self.ended_handled = false;
        self.private_track = self
            .state
            .settings
            .private_listening
            .then(|| stream_engine_id(id));
        self.station_selected = Some(id.to_owned());
        if self
            .live_title
            .as_ref()
            .is_some_and(|(owner, _)| owner != id)
        {
            self.live_title = None;
        }
        // The listening time of the same station goes on across a reconnect.
        self.station_base = if continuing {
            self.station_position
        } else {
            0.
        };
        self.station_position = self.station_base;
        self.station_connecting = false;
        self.snapshot = Snapshot::default();
        self.expected_audio_request = None;
        self.pending_play_intent = None;
        self.station_live.entry(id.to_owned()).or_default().error = None;
        if !connect {
            return;
        }
        // Same request discipline as a file: only the answer to this request counts.
        self.load_request = self.load_request.wrapping_add(1);
        let request = self.load_request;
        self.expected_audio_request = Some(request);
        self.pending_play_intent = Some(true);
        self.audio
            .load_stream(stream_engine_id(id), station.url, request);
        self.set_status("Connecting…");
    }

    /// Play / pause of the current station: Pause drops the connection, Play
    /// connects again, live.
    pub(super) fn toggle_station(&mut self) {
        let Some(id) = self.state.current_station.clone() else {
            return;
        };
        if self.station_intent() {
            self.audio.pause();
            self.pending_play_intent = Some(false);
        } else {
            self.begin_station(&id, true);
        }
        self.state.was_playing = false;
        self.save();
    }

    /// Next / Previous while a station is current: the neighbour in the list
    /// (the list does not wrap).
    pub(super) fn step_station(&mut self, back: bool) {
        let Some(id) = self.state.current_station.clone() else {
            return;
        };
        match self
            .state
            .adjacent_station(&id, !back)
            .map(|station| station.id.clone())
        {
            Some(next) => self.begin_station(&next, true),
            None => self.set_brief_status(if back {
                "This is the first station"
            } else {
                "This is the last station"
            }),
        }
    }

    /// Lets go of the connection of the current station (its address changed or
    /// the station was removed); the next Play connects again from scratch.
    pub(super) fn drop_station_connection(&mut self) {
        self.audio.stop();
        self.expected_audio_request = None;
        self.pending_play_intent = None;
        self.snapshot = Snapshot::default();
        self.station_connecting = false;
        self.live_title = None;
    }

    /// Reads what the engine says about the current station: the time, the live
    /// title and name, and why a connection failed.
    pub(super) fn station_tick(&mut self) {
        let Some(id) = self.state.current_station.clone() else {
            return;
        };
        let accepted = self.snapshot.loaded.is_some();
        if accepted {
            let elapsed = self.snapshot.position;
            let elapsed = if elapsed.is_finite() {
                elapsed.max(0.)
            } else {
                0.
            };
            self.station_position = self.station_base + elapsed;
            if let Some(wanted) = self.pending_play_intent {
                if wanted == self.snapshot.playing || self.snapshot.error.is_some() {
                    self.pending_play_intent = None;
                }
            }
        }
        let handle = self
            .audio
            .stream_handle()
            .filter(|_| accepted && self.snapshot.playing);
        if let Some(handle) = handle {
            self.station_connected(&id, &handle);
        } else if accepted && !self.snapshot.playing {
            if let Some(error) = self.snapshot.error.as_deref() {
                let text = short_error(error);
                let live = self.station_live.entry(id).or_default();
                if live.error.as_deref() != Some(text.as_str()) {
                    live.error = Some(text);
                }
            }
        }
    }

    /// The station is connected: remember what it announced.
    pub(super) fn station_connected(&mut self, id: &str, handle: &stream::StreamHandle) {
        let info = handle.info();
        let announced = state::clean_station_name(&info.name);
        let live = self.station_live.entry(id.to_owned()).or_default();
        live.error = None;
        if live.name != announced {
            live.name = announced.clone();
        }
        if live.bitrate != info.bitrate_kbps {
            live.bitrate = info.bitrate_kbps;
        }
        if live.content_type != info.content_type {
            live.content_type = info.content_type.clone();
        }
        let title = handle.title().and_then(|title| clean_live_title(&title));
        self.live_title = title.clone().map(|title| (id.to_owned(), title));
        let private = self.station_is_private(id);
        if let Some(station) = self.state.stations.iter_mut().find(|s| s.id == id) {
            // A station without a name goes by the one it announced.
            if station.name.is_empty() && !announced.is_empty() {
                station.name = announced;
            }
            // The last title is history: never kept in private listening.
            if let (Some(title), false) = (title, private) {
                if station.last_title != title {
                    station.last_title = title;
                }
            }
        }
    }

    /// Forgets what the player knew about stations (a restored profile has
    /// other ones).
    pub(super) fn reset_station_runtime(&mut self) {
        self.station_live.clear();
        self.live_title = None;
        self.station_position = 0.;
        self.station_base = 0.;
        self.station_connecting = false;
        self.station_selected = None;
        self.station_dialog = None;
        self.station_import_note = None;
    }

    /// Retry after a failure: connect again.
    pub(super) fn retry_station(&mut self, id: &str) {
        self.begin_station(id, true);
    }

    // -- the station list ----------------------------------------------------

    /// Whether the station matches the search text (already folded): its name,
    /// the name it announced, its address or its last title.
    pub(super) fn station_matches(&self, station: &state::Station, folded: &str) -> bool {
        if folded.is_empty() {
            return true;
        }
        let announced = self
            .station_live
            .get(&station.id)
            .map_or("", |live| live.name.as_str());
        [
            station.name.as_str(),
            announced,
            station.url.as_str(),
            station.last_title.as_str(),
        ]
        .iter()
        .any(|text| media::fold_text(text).contains(folded))
    }

    /// The stations the Streams list shows (the search applies), in list order.
    pub(super) fn station_rows(&self) -> Vec<StationRow> {
        let folded = media::fold_text(&self.state.search);
        let current = self.state.current_station.as_deref();
        let phase = current.map(|_| self.station_phase());
        self.state
            .stations
            .iter()
            .filter(|station| self.station_matches(station, &folded))
            .map(|station| {
                let live = self.station_live.get(&station.id);
                let is_current = current == Some(station.id.as_str());
                let title = if is_current {
                    self.live_title_now()
                        .map_or_else(|| station.last_title.clone(), str::to_owned)
                } else {
                    station.last_title.clone()
                };
                let failed_now = match (&phase, is_current) {
                    (Some(StationPhase::Failed(message)), true) => Some(message.clone()),
                    _ => None,
                };
                StationRow {
                    id: station.id.clone(),
                    name: station_display_name(station, live),
                    host: state::station_host(&station.url),
                    kind: station_kind_text(live),
                    title,
                    url: station.url.clone(),
                    error: failed_now.or_else(|| live.and_then(|live| live.error.clone())),
                    current: is_current,
                    phase: if is_current {
                        phase
                            .as_ref()
                            .filter(|phase| !matches!(phase, StationPhase::Failed(_)))
                            .and_then(StationPhase::text)
                    } else {
                        None
                    },
                }
            })
            .collect()
    }

    pub(super) fn shown_station_count(&self) -> usize {
        let folded = media::fold_text(&self.state.search);
        self.state
            .stations
            .iter()
            .filter(|station| self.station_matches(station, &folded))
            .count()
    }

    /// Up / Down / Home / End and Enter in the Streams list.
    pub(super) fn station_keys(&mut self, ctx: &egui::Context) {
        let rows: Vec<String> = self.station_rows().into_iter().map(|row| row.id).collect();
        if let Some((step, _)) = ctx.input_mut(list_step) {
            if !rows.is_empty() {
                let current = self
                    .station_selected
                    .as_ref()
                    .and_then(|id| rows.iter().position(|row| row == id));
                let last = rows.len() - 1;
                let next = match (step, current) {
                    (ListStep::Down, Some(current)) => (current + 1).min(last),
                    (ListStep::Up, Some(current)) => current.saturating_sub(1),
                    (ListStep::Home, _) => 0,
                    (ListStep::End, _) => last,
                    (_, None) => 0,
                };
                self.station_selected = rows.get(next).cloned();
            }
        }
        if self.error.is_none() && ctx.input_mut(|i| consume_plain_key(i, egui::Key::Enter)) {
            if let Some(id) = self.selected_station() {
                self.begin_station(&id, true);
            }
        }
    }

    pub(super) fn open_station_dialog(&mut self, kind: StationDialogKind) {
        let (name, url) = match &kind {
            StationDialogKind::Add => (String::new(), String::new()),
            StationDialogKind::Edit(id) => match self.state.station(id) {
                Some(station) => (station.name.clone(), station.url.clone()),
                None => return,
            },
            StationDialogKind::Rename(id) => match self.state.station(id) {
                Some(station) => (
                    station_display_name(station, self.station_live.get(id)),
                    station.url.clone(),
                ),
                None => return,
            },
        };
        self.station_dialog = Some(StationDialog {
            kind,
            name,
            url,
            fresh: true,
            test: None,
            problem: None,
        });
    }

    /// Starts the Test of the station window: connects to the address on a
    /// worker (never the UI thread) and sends back what the station says.
    pub(super) fn start_station_test(&mut self, dialog: &mut StationDialog, url: String) {
        self.station_test_serial = self.station_test_serial.wrapping_add(1);
        let serial = self.station_test_serial;
        let cancel = Arc::new(AtomicBool::new(false));
        let tx = self.job_sender();
        let worker_cancel = Arc::clone(&cancel);
        let worker_url = url.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                stream::open_stream_with_cancel(&worker_url, 600, worker_cancel)
            }))
            .unwrap_or_else(|_| {
                Err(stream::StreamError::Network(
                    "The test stopped unexpectedly.".into(),
                ))
            })
            .map(|live| {
                let info = live.info().clone();
                live.close();
                TestedStation {
                    name: state::clean_station_name(&info.name),
                    bitrate: info.bitrate_kbps,
                    content_type: info.content_type,
                }
            })
            .map_err(|error| error.to_string());
            tx.send(Event::StationTest { serial, result });
        });
        // The test of the address before is dropped, which cancels it.
        dialog.test = Some(StationTest {
            serial,
            url,
            cancel,
            outcome: None,
        });
    }

    /// What Add, Save or Rename of the station window does. `Err` keeps the
    /// window open with the reason under the fields.
    pub(super) fn apply_station_dialog(
        &mut self,
        dialog: &StationDialog,
    ) -> Result<(), &'static str> {
        match &dialog.kind {
            StationDialogKind::Add => {
                let url = state::validate_stream_url(&dialog.url)?;
                let typed = state::clean_station_name(&dialog.name);
                // An empty name takes the one a successful test heard.
                let name = if typed.is_empty() {
                    dialog
                        .test
                        .as_ref()
                        .filter(|test| test.url == url)
                        .and_then(|test| test.outcome.as_ref())
                        .and_then(|outcome| outcome.as_ref().ok())
                        .map(|tested| tested.name.clone())
                        .unwrap_or_default()
                } else {
                    typed
                };
                match self.state.add_station(&name, &url, state::now())? {
                    state::StationAdd::Added(id) => {
                        let label = self.state.station(&id).map(state::station_label);
                        self.station_selected = Some(id);
                        self.show_streams();
                        self.set_status(format!(
                            "Added the station “{}”",
                            label.unwrap_or_default()
                        ));
                    }
                    state::StationAdd::Existing(id) => {
                        let label = self.state.station(&id).map(state::station_label);
                        self.station_selected = Some(id);
                        self.show_streams();
                        self.set_status(format!(
                            "“{}” is already in your stations",
                            label.unwrap_or_default()
                        ));
                    }
                }
            }
            StationDialogKind::Edit(id) => {
                let old_url = self
                    .state
                    .station(id)
                    .map(|station| station.url.clone())
                    .ok_or("That station no longer exists.")?;
                let url = self.state.set_station_url(id, &dialog.url)?;
                self.state.rename_station(id, &dialog.name)?;
                if url != old_url {
                    // What plays came from the old address.
                    self.station_live.remove(id);
                    if self.state.current_station.as_deref() == Some(id.as_str()) {
                        self.drop_station_connection();
                    }
                }
                self.set_status("Saved the station");
            }
            StationDialogKind::Rename(id) => {
                self.state.rename_station(id, &dialog.name)?;
                self.set_status("Renamed the station");
            }
        }
        self.save();
        Ok(())
    }

    /// The station window: name and address, Test, and Add (or Save, Rename).
    /// Add stays off until the address is valid; Enter accepts, Escape cancels.
    pub(super) fn show_station_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.station_dialog.take() else {
            return;
        };
        let (title, button) = match &dialog.kind {
            StationDialogKind::Add => ("Add stream URL", "Add"),
            StationDialogKind::Edit(_) => ("Edit station", "Save"),
            StationDialogKind::Rename(_) => ("Rename station", "Rename"),
        };
        let has_url = !matches!(dialog.kind, StationDialogKind::Rename(_));
        let verdict = if has_url {
            state::validate_stream_url(&dialog.url)
        } else {
            Ok(String::new())
        };
        let cleaned = verdict.as_ref().ok().cloned().filter(|_| has_url);
        let mut open = true;
        let mut accept = false;
        let mut cancel = false;
        let mut test_url: Option<String> = None;
        in_player_window(ctx, title, 460.)
            .id(egui::Id::new("station dialog"))
            .open(&mut open)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.label(if has_url { "Name (optional)" } else { "Name" });
                let name_edit = ui.add(
                    egui::TextEdit::singleline(&mut dialog.name)
                        .desired_width(f32::INFINITY)
                        .hint_text(if has_url {
                            "The station's own name is used when this is empty"
                        } else {
                            ""
                        }),
                );
                let mut url_edit = None;
                if has_url {
                    ui.add_space(4.);
                    ui.label("Stream URL");
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut dialog.url)
                            .desired_width(f32::INFINITY)
                            .hint_text("http:// or https:// address of the stream"),
                    );
                    match (&verdict, dialog.url.trim().is_empty()) {
                        (Err(problem), false) => {
                            ui.colored_label(ui.visuals().error_fg_color, *problem);
                        }
                        (Err(_), true) => {
                            ui.small("Paste the address of an internet radio stream. Playlist links (.pls, .m3u) work too.");
                        }
                        _ => {}
                    }
                    url_edit = Some(edit);
                }
                if dialog.fresh {
                    dialog.fresh = false;
                    // The address is what gets pasted; a rename starts in the name.
                    let first = url_edit.as_ref().unwrap_or(&name_edit);
                    first.request_focus();
                    let mut edit_state =
                        egui::text_edit::TextEditState::load(ui.ctx(), first.id).unwrap_or_default();
                    let length = if url_edit.is_some() {
                        dialog.url.chars().count()
                    } else {
                        dialog.name.chars().count()
                    };
                    edit_state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(length),
                        )));
                    edit_state.store(ui.ctx(), first.id);
                }
                if has_url {
                    ui.add_space(2.);
                    ui.horizontal_wrapped(|ui| {
                        let running = dialog
                            .test
                            .as_ref()
                            .is_some_and(|test| test.outcome.is_none());
                        if ui
                            .add_enabled(cleaned.is_some() && !running, egui::Button::new("Test"))
                            .on_hover_text("Connect to the address and show what the station says")
                            .clicked()
                        {
                            test_url = cleaned.clone();
                        }
                        // A result belongs to the address it was run with.
                        let shown = dialog
                            .test
                            .as_ref()
                            .filter(|test| Some(&test.url) == cleaned.as_ref());
                        match shown.map(|test| (&test.outcome, &test.url)) {
                            Some((None, _)) => {
                                ui.spinner();
                                ui.label("Testing…");
                            }
                            Some((Some(Ok(tested)), url)) => {
                                ui.label(tested.line(&state::station_host(url)));
                            }
                            Some((Some(Err(error)), _)) => {
                                ui.colored_label(ui.visuals().error_fg_color, error);
                            }
                            None => {}
                        }
                    });
                }
                if let Some(problem) = dialog.problem {
                    ui.colored_label(ui.visuals().error_fg_color, problem);
                }
                ui.add_space(4.);
                let enter = url_edit
                    .iter()
                    .chain(std::iter::once(&name_edit))
                    .any(|edit| edit.lost_focus())
                    && ui.input(|input| input.key_pressed(egui::Key::Enter));
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
                    url_edit.as_ref().unwrap_or(&name_edit).request_focus();
                }
            });
        if let Some(url) = test_url {
            self.start_station_test(&mut dialog, url);
        }
        if accept {
            match self.apply_station_dialog(&dialog) {
                Ok(()) => return,
                Err(problem) => dialog.problem = Some(problem),
            }
        }
        if open && !cancel {
            self.station_dialog = Some(dialog);
        }
    }

    /// Adds the stream entries of the playlists an import read as stations and
    /// keeps the note for the status bar.
    pub(super) fn add_playlist_stations(&mut self, found: Vec<media::PlaylistStation>) {
        let now = state::now();
        let (mut added, mut existing, mut skipped) = (0usize, 0usize, 0usize);
        let mut first = None;
        for item in found {
            match self.state.add_station(&item.name, &item.url, now) {
                Ok(state::StationAdd::Added(id)) => {
                    added += 1;
                    first.get_or_insert(id);
                }
                Ok(state::StationAdd::Existing(_)) => existing += 1,
                Err(_) => skipped += 1,
            }
        }
        if first.is_some() {
            self.station_selected = first;
        }
        self.station_import_note = Some(station_import_text(added, existing, skipped));
    }

    /// Remove a station: asks first when "confirm remove" is on.
    pub(super) fn request_remove_station(&mut self, id: &str) {
        if self.state.settings.confirm_remove {
            self.confirm = Some(Confirm::RemoveStation(id.to_owned()));
        } else {
            self.remove_station_now(id);
        }
    }

    /// Takes the station out of the list (stopping it when it plays) and
    /// records the removal for Undo.
    pub(super) fn remove_station_now(&mut self, id: &str) {
        let was_current = self.state.current_station.as_deref() == Some(id);
        let label = self
            .state
            .station(id)
            .map(|station| station_display_name(station, self.station_live.get(id)));
        let Some((index, station)) = self.state.remove_station(id) else {
            return;
        };
        let label = label.unwrap_or_else(|| "station".to_owned());
        if was_current {
            self.drop_station_connection();
            self.private_track = None;
            self.station_position = 0.;
            self.station_base = 0.;
        }
        self.station_live.remove(id);
        if self.station_selected.as_deref() == Some(id) {
            // The row that takes its place, else the one above.
            self.station_selected = self
                .state
                .stations
                .get(index)
                .or_else(|| {
                    index
                        .checked_sub(1)
                        .and_then(|i| self.state.stations.get(i))
                })
                .map(|station| station.id.clone());
        }
        self.record_undo(
            format!("Remove “{}”", short_label(&label, UNDO_LABEL_CHARS)),
            state::UndoRestore::Station { index, station },
            format!("Removed the station “{label}”"),
        );
        self.save();
    }

    /// The Streams list: name, URL, type and last title of every station,
    /// with a menu per row. Only the rows in view are drawn.
    pub(super) fn streams_ui(&mut self, ui: &mut egui::Ui) {
        let font = self.state.settings.font_size;
        let rows = self.station_rows();
        let total = self.state.stations.len();
        ui.add_space(5.);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new("Streams").font(appearance::semibold(font + 14.)),
                    )
                    .truncate(),
                );
                ui.label(
                    RichText::new(stations_summary(rows.len(), total))
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::text_icon_button(ui, Icon::Plus, "Add stream").clicked() {
                    self.open_station_dialog(StationDialogKind::Add);
                }
            });
        });
        ui.add_space(8.);
        if total == 0 {
            ui.add_space(27.);
            ui.vertical_centered(|ui| {
                ui.heading("No stations yet");
                ui.label("Paste the address of an internet radio stream and it plays. Playlists (.m3u, .pls) with stream addresses are added as stations too.");
                ui.add_space(12.);
                if widgets::text_icon_button(ui, Icon::Plus, "Add stream URL").clicked() {
                    self.open_station_dialog(StationDialogKind::Add);
                }
                if widgets::text_icon_button(ui, Icon::Playlist, "Import playlist").clicked() {
                    self.choose_files(false);
                }
            });
            return;
        }
        if rows.is_empty() {
            ui.add_space(27.);
            ui.vertical_centered(|ui| {
                ui.heading("No matching stations");
                ui.label("Clear the search field to see every station.");
            });
            return;
        }
        // Column widths: name, URL (host), type, then the last title.
        let width = ui.available_width().max(200.);
        let name_width = (width * 0.30).max(140.).min(width);
        let host_width = (width * 0.20).max(90.);
        let kind_width = (width * 0.14).clamp(70., 150.);
        let title_width = (width - name_width - host_width - kind_width).max(80.);
        let widths = [name_width, host_width, kind_width, title_width];
        let total_width: f32 = widths.iter().sum();
        let header_height = (ui.text_style_height(&egui::TextStyle::Body) + 12.).max(28.);
        let row_height = self.state.settings.row_height.max(
            ui.text_style_height(&egui::TextStyle::Body)
                + ui.text_style_height(&egui::TextStyle::Small)
                + 8.,
        );
        egui::ScrollArea::horizontal()
            .id_salt("stations horizontal")
            .show(ui, |ui| {
                ui.set_min_width(total_width);
                let (header, _) = ui.allocate_exact_size(
                    Vec2::new(total_width, header_height),
                    egui::Sense::hover(),
                );
                let mut x = header.left();
                for (label, column) in ["Name", "URL", "Type", "Last title"]
                    .into_iter()
                    .zip(widths)
                {
                    ui.painter().text(
                        egui::pos2(x + 14., header.center().y),
                        egui::Align2::LEFT_CENTER,
                        label,
                        egui::TextStyle::Body.resolve(ui.style()),
                        ui.visuals().text_color(),
                    );
                    x += column;
                }
                ui.painter().line_segment(
                    [header.left_bottom(), header.right_bottom()],
                    egui::Stroke::new(1., appearance::separator(ui)),
                );
                ui.add_space(8.);
                let previous_spacing = ui.spacing().item_spacing.y;
                ui.spacing_mut().item_spacing.y = 2.;
                let thumb_side = if self.state.settings.show_list_art {
                    (row_height - 6.).max(0.)
                } else {
                    0.
                };
                let dark = ui.visuals().dark_mode;
                let mut scroll = egui::ScrollArea::vertical()
                    .id_salt("station rows")
                    .auto_shrink([false, false])
                    .max_height(ui.available_height().max(1.));
                if self.restore_scroll {
                    scroll = scroll.vertical_scroll_offset(self.state.scroll);
                    self.restore_scroll = false;
                }
                let output = scroll.show_rows(ui, row_height, rows.len(), |ui, range| {
                    for index in range {
                        let Some(row) = rows.get(index) else {
                            continue;
                        };
                        let selected = self.station_selected.as_deref() == Some(row.id.as_str());
                        let (rect, response) = ui.allocate_exact_size(
                            Vec2::new(total_width, row_height),
                            egui::Sense::click(),
                        );
                        ui.painter().rect_filled(
                            rect,
                            5.,
                            appearance::row_fill(ui, selected, index % 2 == 1),
                        );
                        let text = appearance::row_text(ui, selected);
                        let weak = if selected {
                            text
                        } else {
                            ui.visuals().weak_text_color()
                        };
                        let mut x = rect.left();
                        for (column, column_width) in widths.into_iter().enumerate() {
                            let mut cell = egui::Rect::from_min_size(
                                egui::pos2(x + 14., rect.top()),
                                Vec2::new((column_width - 18.).max(1.), row_height),
                            );
                            x += column_width;
                            if column == 0
                                && thumb_side > 0.
                                && cell.width() >= thumb_side + THUMB_GAP + THUMB_MIN_TEXT
                            {
                                widgets::paint_station_art(
                                    ui.painter(),
                                    egui::Rect::from_min_size(
                                        egui::pos2(cell.left(), rect.top() + 3.),
                                        Vec2::splat(thumb_side),
                                    ),
                                    &row.name,
                                    dark,
                                );
                                cell.min.x += thumb_side + THUMB_GAP;
                            }
                            let mut cell_ui = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(cell)
                                    .layout(egui::Layout::top_down(egui::Align::Min)),
                            );
                            cell_ui.style_mut().interaction.selectable_labels = false;
                            let ui = &mut cell_ui;
                            ui.set_clip_rect(cell.intersect(ui.clip_rect()));
                            ui.spacing_mut().item_spacing.y = 2.;
                            ui.add_space(3.);
                            match column {
                                0 => {
                                    let name = RichText::new(&row.name).color(text);
                                    let name = if row.current {
                                        name.font(appearance::semibold(font))
                                    } else {
                                        name
                                    };
                                    ui.add(egui::Label::new(name).truncate());
                                    if let Some(error) = &row.error {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(error)
                                                    .small()
                                                    .color(ui.visuals().error_fg_color),
                                            )
                                            .truncate(),
                                        );
                                    } else if let Some(phase) = &row.phase {
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(phase).small().color(weak),
                                            )
                                            .truncate(),
                                        );
                                    }
                                }
                                1 => {
                                    ui.add_space((row_height - 28.) / 2.);
                                    ui.add(
                                        egui::Label::new(RichText::new(&row.host).color(text))
                                            .truncate(),
                                    );
                                }
                                2 => {
                                    ui.add_space((row_height - 28.) / 2.);
                                    ui.add(
                                        egui::Label::new(RichText::new(&row.kind).color(text))
                                            .truncate(),
                                    );
                                }
                                _ => {
                                    ui.add_space((row_height - 28.) / 2.);
                                    ui.add(
                                        egui::Label::new(RichText::new(&row.title).color(text))
                                            .truncate(),
                                    );
                                }
                            }
                        }
                        if self.refocus_list && selected {
                            response.request_focus();
                            self.refocus_list = false;
                        }
                        if response.has_focus() {
                            self.list_focus = Some(response.id);
                        }
                        if response.clicked() || response.secondary_clicked() {
                            response.request_focus();
                            self.list_focus = Some(response.id);
                            self.station_selected = Some(row.id.clone());
                        }
                        if response.double_clicked() && self.state.settings.double_click_plays {
                            self.begin_station(&row.id, true);
                        }
                        response
                            .on_hover_text(format!("{}\n{}", row.name, row.url))
                            .context_menu(|ui| self.station_menu(ui, row));
                    }
                });
                self.state.scroll = output.state.offset.y;
                self.list_viewport = output.inner_rect.height();
                ui.spacing_mut().item_spacing.y = previous_spacing;
            });
    }

    /// The menu of a station row.
    pub(super) fn station_menu(&mut self, ui: &mut egui::Ui, row: &StationRow) {
        if ui.button("Play").clicked() {
            self.begin_station(&row.id, true);
            ui.close();
        }
        if row.error.is_some() && ui.button("Retry").clicked() {
            self.retry_station(&row.id);
            ui.close();
        }
        ui.separator();
        if ui.button("Rename…").clicked() {
            self.open_station_dialog(StationDialogKind::Rename(row.id.clone()));
            ui.close();
        }
        if ui.button("Edit URL…").clicked() {
            self.open_station_dialog(StationDialogKind::Edit(row.id.clone()));
            ui.close();
        }
        if ui.button("Copy URL").clicked() {
            ui.ctx().copy_text(row.url.clone());
            ui.close();
        }
        ui.separator();
        if ui.button("Remove").clicked() {
            self.request_remove_station(&row.id);
            ui.close();
        }
    }
}
