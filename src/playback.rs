//! Audio lives on its own thread: file probing and seeking never block the UI.
use crate::join::{Held, Joined, Marks, NextSegment, SharedSlot, Slot, lock};
use crate::settings::Settings;
use crate::silence::SkipSilence;
use crate::stream::{
    LiveStream, NonSeekable, StreamError, StreamHandle, StreamStatus, open_stream_with_cancel,
};
use rodio::{
    Decoder, OutputStream, OutputStreamBuilder, Sink, Source,
    cpal::{
        self,
        traits::{DeviceTrait, HostTrait},
    },
    decoder::DecoderError,
};
use std::{
    collections::VecDeque,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, RwLock,
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant},
};

const EXIT_ACK_TIMEOUT: Duration = Duration::from_secs(1);
const DSP_TRANSITION_SECONDS: f32 = 0.05;
/// Linear gain ramps that remove clicks at pause, resume and track changes.
const FADE_OUT_SECONDS: f32 = 0.012;
const FADE_IN_SECONDS: f32 = 0.010;
/// The engine thread waits at most this long for a fade-out to finish, so a
/// stalled output can never hold up a pause.
const FADE_WAIT_MAX: Duration = Duration::from_millis(30);
/// How often the system default output is compared with the one in use.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Device events not yet collected by the app; the oldest are dropped first.
const DEVICE_EVENT_CAPACITY: usize = 8;
const DEVICE_LOST_NOTE: &str = "Audio device disconnected — playback paused";
/// After a release the engine thread waits this long for the audio thread to drop
/// the stopped source (and with it the file handle). If that does not happen the
/// output stream is closed as a last resort and the wait continues for the second
/// span. Both are also capped by the caller's deadline.
const RELEASE_SINK_WAIT: Duration = Duration::from_millis(150);
const RELEASE_STREAM_WAIT: Duration = Duration::from_millis(350);
/// How much of the next track the opener thread decodes ahead of time, so the
/// first slow read of a file on a network share never reaches the audio thread.
const PREFETCH_SECONDS: f32 = 0.5;
/// Upper bound of that prefetch in samples whatever the format claims (8 MB).
const PREFETCH_MAX_SAMPLES: usize = 2_000_000;
/// Opener threads that may run at once. A file on a dead share can keep one busy
/// for a long time (and an abandoned one runs on until it is done); past this many
/// a new preload is refused instead of piling up threads.
const OPENER_THREADS_MAX: usize = 3;
/// A track that ends sooner than this (seconds of source time) cannot be opened
/// in time any more: a preload for it is refused at once instead of started.
const PRELOAD_MIN_REMAINING_SECONDS: f64 = 0.25;

/// Output-device news for the app, collected with `AudioEngine::take_device_event`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceEvent {
    /// The output went away or could not be opened. Playback is paused with the
    /// exact position kept; the text is ready to show. Press Play to reconnect.
    Lost(String),
    /// Output is working again on the named device (after a reconnect on Play,
    /// or after following a changed system default device).
    Restored(String),
}
impl DeviceEvent {
    /// One line the app can show as it is.
    pub fn note(&self) -> String {
        match self {
            Self::Lost(note) => note.clone(),
            Self::Restored(name) => format!("Audio output switched to {name}"),
        }
    }
}
/// Queue an event, dropping the oldest ones beyond `capacity`.
fn push_bounded(queue: &mut VecDeque<DeviceEvent>, event: DeviceEvent, capacity: usize) {
    queue.push_back(event);
    while queue.len() > capacity {
        queue.pop_front();
    }
}
type DeviceEvents = Arc<Mutex<VecDeque<DeviceEvent>>>;

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub request_id: u64,
    pub loaded: Option<String>,
    pub position: f64,
    pub duration: f64,
    pub playing: bool,
    pub ended: bool,
    pub error: Option<String>,
    pub device: String,
    pub busy: bool,
}
/// The engine moved from one track to the next by itself (a gapless join). Each
/// change is handed out once by `AudioEngine::poll`.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackChange {
    pub from: String,
    /// Request id of the segment that ended at the join.
    pub from_request_id: u64,
    /// Where the finished track's position stood at the boundary, in seconds.
    pub from_reached: f64,
    pub from_duration: f64,
    /// The finished track ended clearly before its reported length (see `ended_early`).
    pub from_early: bool,
    pub to: String,
    /// The request id the preload carried; the snapshot reports it from now on.
    pub request_id: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PreloadState {
    #[default]
    None,
    Opening,
    Ready,
    Failed(String),
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreloadStatus {
    /// The request id of the latest preload the engine was asked for.
    pub last_request: u64,
    pub state: PreloadState,
}
/// The track the engine should open ahead of time and join after the playing one
/// (see `AudioEngine::preload`).
#[derive(Clone, Debug)]
pub struct PreloadRequest {
    /// The track that must still be the playing one; anything else is refused.
    pub after: String,
    /// What the snapshot reports as `loaded` once the join has happened.
    pub id: String,
    pub path: PathBuf,
    /// ReplayGain of the next track in dB; it applies from its first sample.
    pub gain_db: f32,
    /// The request id the snapshot reports from the join on. It also names this
    /// preload in `PreloadStatus::last_request`.
    pub request_id: u64,
}
/// Everything the app learns from the engine in one tick.
#[derive(Clone, Debug)]
pub struct EnginePoll {
    pub snapshot: Snapshot,
    pub changes: Vec<TrackChange>,
    pub preload: PreloadStatus,
    /// Total silence skipped this session, in seconds of source time.
    pub skipped_seconds: f64,
}
/// What the engine thread writes for `AudioEngine::poll`. The snapshot lock is
/// always taken before this one.
#[derive(Debug, Default)]
struct Side {
    changes: Vec<TrackChange>,
    preload: PreloadStatus,
    skipped_seconds: f64,
}
/// Track changes waiting for the app; the oldest are dropped first if the app
/// never polls.
const CHANGES_CAPACITY: usize = 256;

#[derive(Clone, Debug)]
pub struct Dsp {
    pub mono: bool,
    pub balance: f32,
    pub preamp_db: f32,
    pub limiter: bool,
    pub equalizer: bool,
    pub gains: [f32; 10],
}
impl From<&Settings> for Dsp {
    fn from(s: &Settings) -> Self {
        Self {
            mono: s.mono,
            balance: s.balance,
            preamp_db: s.preamp_db,
            limiter: s.limiter,
            equalizer: s.equalizer_enabled,
            gains: s.eq_gains,
        }
    }
}
enum Command {
    Load {
        id: String,
        path: PathBuf,
        position: f64,
        play: bool,
        speed: f32,
        request_id: u64,
    },
    /// Connect to a live stream (see `AudioEngine::load_stream`).
    LoadStream {
        id: String,
        url: String,
        request_id: u64,
    },
    Play,
    Pause,
    PauseIfRequest(u64),
    Seek {
        position: f64,
        request_id: u64,
        target: Option<SeekTarget>,
    },
    Stop,
    /// Open a track ahead of time and join it after the playing one.
    Preload(PreloadRequest),
    CancelPreload,
    Volume(f32),
    Speed(f32),
    Device(String),
    Pitch(bool),
    PrepareExit(Sender<Snapshot>),
    /// Close the audio file but keep the track and position (see
    /// `AudioEngine::release_file`). The answer is true once the handle is closed.
    /// A request whose `deadline` has passed is not carried out: the caller gave up.
    ReleaseFile {
        reply: Sender<bool>,
        deadline: Option<Instant>,
    },
    Shutdown,
}

/// A seek may apply to the currently loaded decoder or carry enough metadata to
/// reopen the file the controller intended if a gapless join already moved the
/// engine to another track.
enum SeekTarget {
    Current(String),
    Track {
        id: String,
        path: PathBuf,
        speed: f32,
        gain_db: f32,
    },
}
pub struct AudioEngine {
    tx: Sender<Command>,
    snapshot: Arc<Mutex<Snapshot>>,
    dsp: Arc<RwLock<Dsp>>,
    /// ReplayGain of the playing track in dB (f32 bits). The first pipeline stage
    /// applies it per track (see `Joined`); it is not part of the shared DSP.
    live_gain: Arc<AtomicU32>,
    /// Skip silence on or off; the pipeline's silence stage reads it once per 10 ms
    /// block, so switching needs no command and no reload.
    skip: Arc<AtomicBool>,
    /// Track changes and preload news for `poll`, written by the engine thread.
    side: Arc<Mutex<Side>>,
    events: DeviceEvents,
    /// The connected stream's UI view; set by the engine thread (see `stream_handle`).
    live_handle: LiveHandle,
}
impl AudioEngine {



    pub fn new(settings: &Settings, volume: f32, speed: f32, silent: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let shared = snapshot.clone();
        let dsp = Arc::new(RwLock::new(Dsp::from(settings)));
        let effects = dsp.clone();
        let events: DeviceEvents = Arc::new(Mutex::new(VecDeque::new()));
        let worker_events = events.clone();
        let live_handle: LiveHandle = Arc::new(Mutex::new(None));
        let worker_handle = live_handle.clone();
        let live_gain = Arc::new(AtomicU32::new(0f32.to_bits()));
        let worker_gain = live_gain.clone();
        let skip = Arc::new(AtomicBool::new(false));
        let worker_skip = skip.clone();
        let side = Arc::new(Mutex::new(Side::default()));
        let worker_side = side.clone();
        let device = settings.output_device.clone();
        let preserve_pitch = settings.preserve_pitch;
        thread::Builder::new()
            .name("bs-player-audio".into())
            .spawn(move || {
                Worker::new(
                    effects,
                    shared,
                    device,
                    volume,
                    speed,
                    preserve_pitch,
                    silent,
                )
                .with_events(worker_events)
                .with_live_handle(worker_handle)
                .with_live_gain(worker_gain)
                .with_skip(worker_skip)
                .with_side(worker_side)
                .run(rx);
            })
            .expect("audio worker thread");
        Self {
            tx,
            snapshot,
            dsp,
            live_gain,
            skip,
            side,
            events,
            live_handle,
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().unwrap().clone()
    }
    /// One consistent view for the app's tick: the snapshot, the track changes the
    /// engine made by itself since the last call (each is returned once), the
    /// preload state and the total silence skipped. Both locks are held together
    /// (snapshot first, then the side lock, the order the engine thread uses), so
    /// a change and the snapshot that already shows the new track arrive together.
    pub fn poll(&self) -> EnginePoll {
        let snapshot = self.snapshot.lock().unwrap();
        let mut side = lock(&self.side);
        EnginePoll {
            snapshot: snapshot.clone(),
            changes: std::mem::take(&mut side.changes),
            preload: side.preload.clone(),
            skipped_seconds: side.skipped_seconds,
        }
    }
    /// Oldest output-device event not yet collected, if any. Poll it once per
    /// frame until it returns `None`; at most 8 events are kept.
    pub fn take_device_event(&self) -> Option<DeviceEvent> {
        self.events.lock().ok()?.pop_front()
    }
    pub fn load(
        &self,
        id: String,
        path: PathBuf,
        position: f64,
        play: bool,
        speed: f32,
        request_id: u64,
    ) {
        let _ = self.tx.send(Command::Load {
            id,
            path,
            position,
            play,
            speed,
            request_id,
        });
    }
    pub fn play(&self) {
        let _ = self.tx.send(Command::Play);
    }
    pub fn pause(&self) {
        let _ = self.tx.send(Command::Pause);
    }
    /// Pauses only if the engine is still playing the request named by a stale
    /// gapless join; a newer load that already won is left alone.
    pub fn pause_if_request(&self, request_id: u64) {
        let _ = self.tx.send(Command::PauseIfRequest(request_id));
    }
    pub fn stop(&self) {
        let _ = self.tx.send(Command::Stop);
    }
    /// A generic seek that is not tied to a file. The player uses `seek_track`.
    #[allow(dead_code)]
    pub fn seek(&self, pos: f64, request_id: u64) {
        if pos.is_finite() {
            let _ = self.tx.send(Command::Seek {
                position: pos.max(0.),
                request_id,
                target: None,
            });
        }
    }
    /// A current-decoder-only seek retained for low-level callers. The engine may
    /// move to the next track itself, in which case this request is dropped.
    pub fn seek_in(&self, track: &str, pos: f64, request_id: u64) {
        if pos.is_finite() {
            let _ = self.tx.send(Command::Seek {
                position: pos.max(0.),
                request_id,
                target: Some(SeekTarget::Current(track.to_owned())),
            });
        }
    }
    /// Seeks the requested local track, reopening its path if a gapless join
    /// changed the engine's current file before this command was processed.
    pub fn seek_track(
        &self,
        id: String,
        path: PathBuf,
        pos: f64,
        speed: f32,
        gain_db: f32,
        request_id: u64,
    ) {
        if pos.is_finite() {
            let _ = self.tx.send(Command::Seek {
                position: pos.max(0.),
                request_id,
                target: Some(SeekTarget::Track {
                    id,
                    path,
                    speed,
                    gain_db,
                }),
            });
        }
    }
    /// Opens `request.id` on a helper thread and joins it to the playing track:
    /// when the playing track runs out, the next one continues inside the same
    /// pipeline with no gap, and the change shows up once in `poll().changes`.
    ///
    /// Only a track with the playing one's channel count and sample rate can join;
    /// anything else (a stream, a missing file, a speed or format change, a track
    /// that is no longer the playing one) ends in `PreloadState::Failed` and the
    /// playing track simply ends, so the normal track change runs. Progress is
    /// reported in `poll().preload`: `last_request` is this request's id, `state`
    /// goes `Opening` -> `Ready`, and falls back to `None` when anything rebuilds
    /// the pipeline (load, seek, speed, pitch, device change, release) or the
    /// join happened. A newer request replaces an older one; call
    /// `cancel_preload` first when the wish changed anyway.
    pub fn preload(&self, mut request: PreloadRequest) {
        if !request.gain_db.is_finite() {
            request.gain_db = 0.;
        }
        let _ = self.tx.send(Command::Preload(request));
    }
    /// Withdraws the preload. Before the audio thread took it, nothing is heard of
    /// the next track and playback is untouched. If the audio thread took it but
    /// the boundary is not reached yet, the playing track is reopened at its
    /// position (a short seek-like restart, nothing of the next track is heard).
    /// After the boundary it does nothing: the track change is already out.
    pub fn cancel_preload(&self) {
        let _ = self.tx.send(Command::CancelPreload);
    }
    pub fn volume(&self, v: f32) {
        let _ = self.tx.send(Command::Volume(v));
    }
    pub fn speed(&self, v: f32) {
        let _ = self.tx.send(Command::Speed(v));
    }
    pub fn settings(&self, old: &Settings, new: &Settings) {
        self.dsp_settings(new);
        if old.preserve_pitch != new.preserve_pitch {
            let _ = self.tx.send(Command::Pitch(new.preserve_pitch));
        }
        if old.output_device != new.output_device {
            let _ = self.tx.send(Command::Device(new.output_device.clone()));
        }
    }
    /// Refresh live DSP without copying settings or touching output/pitch state.
    pub fn dsp_settings(&self, new: &Settings) {
        *self.dsp.write().unwrap() = Dsp::from(new);
    }
    /// ReplayGain of the playing track, in dB. It is applied per track at the
    /// start of the pipeline and ramped in over 50 ms, so it takes effect on the
    /// exact sample where a track begins.
    pub fn replay_gain(&self, gain: f32) {
        let gain = if gain.is_finite() { gain } else { 0. };
        self.live_gain.store(gain.to_bits(), Ordering::Relaxed);
    }
    /// Shortens long silences (over 0.6 s) to a quarter of a second while on. It
    /// is a stored flag the pipeline's silence stage reads every 10 ms: no
    /// command, no reload, and it takes effect within one block. The engine does
    /// not know about audiobooks; the app turns it on for the tracks that want it
    /// (it also applies across a gapless join, to whatever plays). Positions stay
    /// in source time, so nothing the app stores moves; `poll().skipped_seconds`
    /// is the total removed since the last `load`. Live streams never use it.
    pub fn skip_silence(&self, on: bool) {
        self.skip.store(on, Ordering::Relaxed);
    }
    pub fn prepare_exit(&self) -> (Snapshot, Option<String>) {
        let (reply, receive) = mpsc::channel();
        if self.tx.send(Command::PrepareExit(reply)).is_err() {
            return fallback_exit_snapshot(
                self.snapshot(),
                &self.tx,
                "The audio worker stopped before confirming the final position. The last published audio snapshot was used.",
            );
        }
        wait_for_exit_snapshot(receive, || self.snapshot(), &self.tx, EXIT_ACK_TIMEOUT)
    }
    /// Pauses (with the existing fade), drops the sink and decoder so the audio FILE
    /// HANDLE IS CLOSED, keeps the loaded track identity, position and duration in
    /// the snapshot (not playing, no error), and waits for the engine thread's
    /// acknowledgement. Returns true when the handle is released, false on timeout.
    ///
    /// The released state behaves like a lost device without telling anyone: no
    /// `DeviceEvent`, the output stream stays open, and only `play` or `load`
    /// reopen the file (at the saved position). A seek only moves the saved
    /// position; a speed or pitch change takes effect at the next reopen. A request
    /// that times out is not carried out later.
    pub fn release_file(&self, timeout: Duration) -> bool {
        let (reply, receive) = mpsc::channel();
        let deadline = Instant::now().checked_add(timeout);
        if self.tx.send(Command::ReleaseFile { reply, deadline }).is_err() {
            return false;
        }
        wait_for_release(receive, timeout)
    }
    /// Connect to a live stream and play it through the normal chain (EQ, preamp,
    /// limiter, mono, balance, volume, fades, device handling). `id` is what the
    /// snapshot reports as `loaded` (for example `stream:<station id>`); the
    /// request id is answered like a file load's.
    ///
    /// Opening takes up to 15 s on a helper thread: the snapshot is `busy` while
    /// connecting and then `loaded = Some(id)` with `duration` 0, `position` the
    /// elapsed listening time, and `ended` never set. A failure to connect, or a
    /// stream that fails for good, ends playback with `error` set and `playing`
    /// false; `play` connects again. Speed and pitch are not applied to streams,
    /// seeks are acknowledged and ignored, pause drops the connection and play
    /// reconnects live. Loading a file (or another stream) replaces the stream.
    pub fn load_stream(&self, id: String, url: String, request_id: u64) {
        // ReplayGain belongs to files: drop the previous file's gain from the chain.
        self.replay_gain(0.);
        let _ = self.tx.send(Command::LoadStream {
            id,
            url,
            request_id,
        });
    }
    /// The connected stream's title, status and station info for the player bar
    /// and tray, or `None` while connecting, paused or when no stream is loaded.
    /// Cheap enough to call every frame; the handle stays valid while the stream
    /// reconnects.
    pub fn stream_handle(&self) -> Option<StreamHandle> {
        self.live_handle.lock().ok()?.clone()
    }
    pub fn devices() -> Result<Vec<String>, String> {
        cpal::default_host()
            .output_devices()
            .map(|ds| ds.filter_map(|d| d.name().ok()).collect())
            .map_err(|error| format!("Could not enumerate audio output devices: {error}"))
    }
}
impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
    }
}

fn next_audio_command(
    receiver: &mpsc::Receiver<Command>,
    deferred: &mut Option<Command>,
) -> Result<Command, mpsc::RecvTimeoutError> {
    let mut command = match deferred.take() {
        Some(command) => command,
        None => receiver.recv_timeout(Duration::from_millis(35))?,
    };
    if matches!(command, Command::Seek { .. }) {
        // Only consecutive seeks supersede each other. Keep the first other
        // command as an ordering barrier: never cross Pause/Stop/Load/exit.
        // Bound the drain so a continuously producing sender cannot starve work.
        for _ in 0..256 {
            match receiver.try_recv() {
                Ok(latest @ Command::Seek { .. }) => command = latest,
                Ok(barrier) => {
                    *deferred = Some(barrier);
                    break;
                }
                Err(_) => break,
            }
        }
    }
    Ok(command)
}

fn fallback_exit_snapshot(
    mut snapshot: Snapshot,
    commands: &Sender<Command>,
    warning: &str,
) -> (Snapshot, Option<String>) {
    // A busy shared snapshot still contains the last completed position and
    // request ID. It is safe fallback data, but not a confirmation of the
    // pending request.
    snapshot.busy = false;
    let _ = commands.send(Command::Shutdown);
    (snapshot, Some(warning.to_owned()))
}

/// True when the sink ran dry clearly before the reported end. Decoders only
/// estimate the length of files without a length header, so the allowance grows
/// with the duration (0.5 %, at least one second) instead of flagging a healthy
/// long file as damaged and stopping the queue.
fn ended_early(position: f64, duration: f64) -> bool {
    duration > 0. && position + (duration * 0.005).max(1.) < duration
}

fn begin_audio_request(snapshot: &mut Snapshot) {
    // Retain the last completed request ID and position until the new operation
    // has finished. Busy state must never make old data look like a new ACK.
    snapshot.busy = true;
    snapshot.error = None;
}

fn complete_audio_request(snapshot: &mut Snapshot, loaded: String, request_id: u64) {
    snapshot.busy = false;
    snapshot.request_id = request_id;
    snapshot.loaded = Some(loaded);
    snapshot.ended = false;
}

fn wait_for_exit_snapshot(
    receive: mpsc::Receiver<Snapshot>,
    fallback: impl Fn() -> Snapshot,
    commands: &Sender<Command>,
    timeout: Duration,
) -> (Snapshot, Option<String>) {
    match receive.recv_timeout(timeout) {
        Ok(snapshot) => (snapshot, None),
        Err(mpsc::RecvTimeoutError::Timeout) => fallback_exit_snapshot(
            fallback(),
            commands,
            "Audio shutdown did not finish before the timeout. The last confirmed audio snapshot was used; any pending seek or load was not assumed complete.",
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => fallback_exit_snapshot(
            fallback(),
            commands,
            "The audio worker stopped before confirming the final position. The last published audio snapshot was used.",
        ),
    }
}

/// True only when the engine thread confirmed that the file handle is closed.
/// A timeout or a stopped worker is "not confirmed", never an assumption.
fn wait_for_release(receive: mpsc::Receiver<bool>, timeout: Duration) -> bool {
    receive.recv_timeout(timeout) == Ok(true)
}

/// Counts source pipelines that still own a decoder, and so an open file handle.
/// The audio thread drops a stopped sink's source some milliseconds after `stop`;
/// this is how the engine thread learns that it really happened.
pub(crate) struct SourceToken(Arc<AtomicUsize>);
impl SourceToken {
    pub(crate) fn new(open: &Arc<AtomicUsize>) -> Self {
        open.fetch_add(1, Ordering::AcqRel);
        Self(open.clone())
    }
}
impl Drop for SourceToken {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
/// Waits until no pipeline owns a decoder any more; false when `until` came first.
fn wait_for_sources_closed(open: &AtomicUsize, until: Instant) -> bool {
    while open.load(Ordering::Acquire) > 0 {
        if Instant::now() >= until {
            return false;
        }
        thread::sleep(Duration::from_micros(500));
    }
    true
}

/// Text for a failure reported by a running output stream. `DeviceNotAvailable`
/// is the unplugged / dropped-link case; any other stream failure is handled the
/// same way (pause, keep the position) with the backend's own wording.
fn stream_error_note(error: &cpal::StreamError) -> String {
    match error {
        cpal::StreamError::DeviceNotAvailable => DEVICE_LOST_NOTE.to_owned(),
        other => format!("Audio output failed: {other}. Playback paused; press Play to reconnect."),
    }
}
fn open_failure_note(reason: &str) -> String {
    format!("Audio output unavailable — playback paused. {reason}")
}

/// Opens the selected device (the system default when `device` is empty) and
/// returns the stream with the device's name when it can be read.
fn open_output(
    device: &str,
    error_tx: Sender<(u64, String)>,
    generation: u64,
) -> Result<(OutputStream, Option<String>), String> {
    let host = cpal::default_host();
    let selected=if device.is_empty(){host.default_output_device()}else{host.output_devices().map_err(|e|e.to_string())?.find(|d|d.name().ok().as_deref()==Some(device))}.ok_or_else(||"The selected audio output is unavailable. Select a connected output in Options → Audio output.".to_owned())?;
    let name = selected.name().ok();
    let mut stream = OutputStreamBuilder::from_device(selected)
        .map_err(|e| e.to_string())?
        .with_error_callback(move |e| {
            let _ = error_tx.send((generation, stream_error_note(&e)));
        })
        .open_stream()
        .map_err(|e| format!("Could not open audio output: {e}"))?;
    stream.log_on_drop(false);
    Ok((stream, name))
}

/// Output availability as the engine thread sees it, kept pure so the
/// loss -> paused -> Play -> recovered sequence can be tested without a device.
#[derive(Debug, Default)]
struct OutputLink {
    lost: bool,
}
impl OutputLink {
    /// A running stream failed. Only the first failure of a loss is reported.
    fn stream_failed(&mut self, note: String) -> Option<DeviceEvent> {
        if self.lost {
            None
        } else {
            self.lost = true;
            Some(DeviceEvent::Lost(note))
        }
    }
    /// The output was opened (`Ok(device name)`) or could not be opened.
    /// `wants_sound` is true when playback was asked for (Play): a failed reopen
    /// is then reported again. Seeks and speed changes while the device is
    /// still absent stay quiet.
    fn open_attempt(
        &mut self,
        outcome: Result<&str, &str>,
        wants_sound: bool,
    ) -> Option<DeviceEvent> {
        match outcome {
            Err(reason) => {
                let report = wants_sound || !self.lost;
                self.lost = true;
                report.then(|| DeviceEvent::Lost(open_failure_note(reason)))
            }
            Ok(name) => {
                let was_lost = std::mem::take(&mut self.lost);
                was_lost.then(|| DeviceEvent::Restored(name.to_owned()))
            }
        }
    }
}

/// What the system's default output device looks like right now.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DefaultProbe {
    Named(String),
    NoDevice,
    /// A device exists but its name could not be read: no conclusion.
    Unreadable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum DefaultVerdict {
    Same,
    Switch(String),
    Lost,
}
/// Pure comparison of the device name in use with the current default.
fn default_device_verdict(opened: &str, probe: &DefaultProbe) -> DefaultVerdict {
    match probe {
        DefaultProbe::Named(name) if name != opened => DefaultVerdict::Switch(name.clone()),
        DefaultProbe::NoDevice => DefaultVerdict::Lost,
        _ => DefaultVerdict::Same,
    }
}
fn probe_default_device() -> DefaultProbe {
    match cpal::default_host().default_output_device() {
        None => DefaultProbe::NoDevice,
        Some(device) => device
            .name()
            .map_or(DefaultProbe::Unreadable, DefaultProbe::Named),
    }
}
fn output_is_listed(name: &str) -> bool {
    AudioEngine::devices().is_ok_and(|names| names.iter().any(|n| n == name))
}
/// Rate-limits the default-device comparison (enumeration can be slow on
/// Windows) and keeps a flapping name from causing more than one switch per
/// interval. Two empty answers in a row count as a loss.
#[derive(Debug, Default)]
struct DefaultWatch {
    opened: Option<String>,
    /// The device in use before the last `Switch` verdict.
    previous: Option<String>,
    last_poll: Option<Instant>,
    misses: u8,
}
impl DefaultWatch {
    /// Record the device the stream was opened on (`None`: unknown, so unwatched).
    fn opened_on(&mut self, name: Option<String>, now: Instant) {
        self.opened = name;
        self.last_poll = Some(now);
        self.misses = 0;
    }
    /// `probe` runs only when the interval has passed and a device is being
    /// watched; `None` means nothing was compared.
    fn poll(
        &mut self,
        now: Instant,
        probe: impl FnOnce() -> DefaultProbe,
    ) -> Option<DefaultVerdict> {
        self.opened.as_ref()?;
        if self
            .last_poll
            .is_some_and(|last| now.saturating_duration_since(last) < DEFAULT_POLL_INTERVAL)
        {
            return None;
        }
        self.last_poll = Some(now);
        let opened = self.opened.clone()?;
        match default_device_verdict(&opened, &probe()) {
            DefaultVerdict::Lost => {
                self.misses = self.misses.saturating_add(1);
                if self.misses < 2 {
                    return Some(DefaultVerdict::Same);
                }
                self.misses = 0;
                self.opened = None;
                Some(DefaultVerdict::Lost)
            }
            DefaultVerdict::Switch(name) => {
                self.misses = 0;
                self.previous = self.opened.replace(name.clone());
                Some(DefaultVerdict::Switch(name))
            }
            DefaultVerdict::Same => {
                self.misses = 0;
                Some(DefaultVerdict::Same)
            }
        }
    }
}
enum FollowAction {
    Keep,
    Switch(String),
    Pause,
}
/// A changed default is followed only while the device that was in use is still
/// present (headphones plugged in, default changed in the OS). If that device
/// vanished, the new default is, say, the speakers: pause instead of surprising
/// the listener with sound from them.
fn follow_action(verdict: DefaultVerdict, old_listed: impl FnOnce() -> bool) -> FollowAction {
    match verdict {
        DefaultVerdict::Same => FollowAction::Keep,
        DefaultVerdict::Switch(name) if old_listed() => FollowAction::Switch(name),
        DefaultVerdict::Switch(_) | DefaultVerdict::Lost => FollowAction::Pause,
    }
}

/// Frames in a ramp of `seconds` at `sample_rate`. `speed` is the sink speed
/// when it resamples after this stage (pitch-shifting speed): the pipeline then
/// runs `speed` times faster than real time and the ramp needs that many more
/// frames to last the same wall-clock time. Always at least one frame.
fn fade_ramp_frames(sample_rate: u32, seconds: f32, speed: f32) -> u32 {
    let speed = if speed.is_finite() && speed > 0. {
        speed.clamp(0.25, 3.)
    } else {
        1.
    };
    let frames = (sample_rate as f32 * seconds * speed).round();
    if frames.is_finite() { frames.max(1.) as u32 } else { 1 }
}
fn fade_step(frames: u32) -> f32 {
    1. / frames.max(1) as f32
}
/// One frame of the ramp: move `gain` towards `target`, never outside 0..=1 and
/// landing exactly on the target. Steady state is exact (gain == target).
fn fade_advance(gain: f32, target: f32, up_step: f32, down_step: f32) -> f32 {
    let target = if target.is_nan() { 1. } else { target.clamp(0., 1.) };
    let gain = if gain.is_finite() { gain.clamp(0., 1.) } else { target };
    if gain < target {
        (gain + up_step).min(target)
    } else if gain > target {
        (gain - down_step).max(target)
    } else {
        gain
    }
}
/// Gain target shared between the engine thread and the audio pipeline.
struct FadeControl {
    target: AtomicU32,
    level: AtomicU32,
}
impl FadeControl {
    fn new(level: f32, target: f32) -> Self {
        Self {
            target: AtomicU32::new(target.to_bits()),
            level: AtomicU32::new(level.to_bits()),
        }
    }
    fn set_target(&self, target: f32) {
        let target = if target.is_nan() { 1. } else { target.clamp(0., 1.) };
        self.target.store(target.to_bits(), Ordering::Relaxed);
    }
    fn target(&self) -> f32 {
        f32::from_bits(self.target.load(Ordering::Relaxed))
    }
    fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
    fn publish(&self, level: f32) {
        self.level.store(level.to_bits(), Ordering::Relaxed);
    }
}
/// Waits until the fade reached silence; false when `limit` ran out first.
fn wait_for_silence(control: &FadeControl, limit: Duration) -> bool {
    let started = Instant::now();
    while control.level() > 0. {
        if started.elapsed() >= limit {
            return false;
        }
        thread::sleep(Duration::from_micros(500));
    }
    true
}
/// Applies the shared gain to every channel of a frame. It neither drops nor
/// adds samples, so the position clock is unaffected, and at full gain it
/// returns each sample untouched.
struct Fade {
    source: Box<dyn Source + Send>,
    control: Arc<FadeControl>,
    channels: usize,
    index: usize,
    gain: f32,
    applied: f32,
    up_step: f32,
    down_step: f32,
}
impl Fade {
    fn new(
        source: Box<dyn Source + Send>,
        control: Arc<FadeControl>,
        start_gain: f32,
        speed: f32,
    ) -> Self {
        let rate = source.sample_rate();
        let start_gain = start_gain.clamp(0., 1.);
        Self {
            channels: (source.channels() as usize).max(1),
            up_step: fade_step(fade_ramp_frames(rate, FADE_IN_SECONDS, speed)),
            down_step: fade_step(fade_ramp_frames(rate, FADE_OUT_SECONDS, speed)),
            source,
            control,
            index: 0,
            gain: start_gain,
            applied: start_gain,
        }
    }
}
impl Iterator for Fade {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        let sample = self.source.next()?;
        if self.index == 0 {
            self.applied = self.gain;
            let target = self.control.target();
            if self.gain != target {
                self.gain = fade_advance(self.gain, target, self.up_step, self.down_step);
                self.control.publish(self.gain);
            }
        }
        self.index += 1;
        if self.index >= self.channels {
            self.index = 0;
        }
        Some(if self.applied == 1. {
            sample
        } else {
            sample * self.applied
        })
    }
}
impl Source for Fade {
    fn current_span_len(&self) -> Option<usize> {
        self.source.current_span_len()
    }
    fn channels(&self) -> u16 {
        self.source.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.source.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.source.total_duration()
    }
}

#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    target_b0: f32,
    target_b1: f32,
    target_b2: f32,
    target_a1: f32,
    target_a2: f32,
    ramp_remaining: u32,
    z1: f32,
    z2: f32,
}
impl Biquad {
    fn peak(hz: f32, gain: f32, rate: f32) -> Self {
        // A band at or above the usable range of this file would be folded onto
        // 0.45 x rate and stack with its neighbours: leave it flat instead.
        let gain = if hz >= rate * 0.45 { 0. } else { gain };
        let a = 10f32.powf(gain / 40.);
        let w = std::f32::consts::TAU * hz.min(rate * 0.45) / rate;
        let alpha = w.sin() / 2.;
        let a0 = 1. + alpha / a;
        let b0 = (1. + alpha * a) / a0;
        let b1 = -2. * w.cos() / a0;
        let b2 = (1. - alpha * a) / a0;
        let a1 = -2. * w.cos() / a0;
        let a2 = (1. - alpha / a) / a0;
        Self {
            b0,
            b1,
            b2,
            a1,
            a2,
            target_b0: b0,
            target_b1: b1,
            target_b2: b2,
            target_a1: a1,
            target_a2: a2,
            ramp_remaining: 0,
            z1: 0.,
            z2: 0.,
        }
    }
    fn set_target(&mut self, target: Self, ramp_frames: u32) {
        let changed = self.target_b0 != target.b0
            || self.target_b1 != target.b1
            || self.target_b2 != target.b2
            || self.target_a1 != target.a1
            || self.target_a2 != target.a2;
        if !changed {
            return;
        }
        self.target_b0 = target.b0;
        self.target_b1 = target.b1;
        self.target_b2 = target.b2;
        self.target_a1 = target.a1;
        self.target_a2 = target.a2;
        self.ramp_remaining = ramp_frames.max(1);
    }
    fn reset_state(&mut self) {
        self.z1 = 0.;
        self.z2 = 0.;
    }
    fn run(&mut self, x: f32) -> f32 {
        if self.ramp_remaining > 0 {
            let remaining = self.ramp_remaining as f32;
            self.b0 += (self.target_b0 - self.b0) / remaining;
            self.b1 += (self.target_b1 - self.b1) / remaining;
            self.b2 += (self.target_b2 - self.b2) / remaining;
            self.a1 += (self.target_a1 - self.a1) / remaining;
            self.a2 += (self.target_a2 - self.a2) / remaining;
            self.ramp_remaining -= 1;
            if self.ramp_remaining == 0 {
                self.b0 = self.target_b0;
                self.b1 = self.target_b1;
                self.b2 = self.target_b2;
                self.a1 = self.target_a1;
                self.a2 = self.target_a2;
            }
        }
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}
struct Processed {
    source: Box<dyn Source + Send>,
    shared: Arc<RwLock<Dsp>>,
    config: Dsp,
    frame: Vec<f32>,
    index: usize,
    frames: u64,
    filters: Vec<[Biquad; 10]>,
    gain: f32,
    gain_target: f32,
    gain_ramp_remaining: u32,
    eq_mix: f32,
    eq_mix_target: f32,
    eq_ramp_remaining: u32,
    transition_frames: u32,
}
impl Processed {
    fn new(source: Box<dyn Source + Send>, shared: Arc<RwLock<Dsp>>) -> Self {
        let config = shared.read().unwrap().clone();
        let n = source.channels() as usize;
        let transition_frames =
            ((source.sample_rate() as f32 * DSP_TRANSITION_SECONDS).round() as u32).max(1);
        let mut s = Self {
            source,
            shared,
            config,
            frame: vec![0.; n],
            index: n,
            frames: 0,
            filters: vec![[Biquad::default(); 10]; n],
            gain: 1.,
            gain_target: 1.,
            gain_ramp_remaining: 0,
            eq_mix: 0.,
            eq_mix_target: 0.,
            eq_ramp_remaining: 0,
            transition_frames,
        };
        s.configure_initial();
        s
    }
    fn configure_initial(&mut self) {
        self.gain = 10f32.powf(self.config.preamp_db / 20.);
        self.gain_target = self.gain;
        self.eq_mix = if self.config.equalizer { 1. } else { 0. };
        self.eq_mix_target = self.eq_mix;
        let bands = [
            31., 62., 125., 250., 500., 1000., 2000., 4000., 8000., 16000.,
        ];
        for channel in &mut self.filters {
            for (i, f) in channel.iter_mut().enumerate() {
                *f = Biquad::peak(
                    bands[i],
                    self.config.gains[i],
                    self.source.sample_rate() as f32,
                );
            }
        }
    }

    fn update_config(&mut self, new: Dsp) {
        let bands = [
            31., 62., 125., 250., 500., 1000., 2000., 4000., 8000., 16000.,
        ];
        if new.gains != self.config.gains {
            for channel in &mut self.filters {
                for (index, filter) in channel.iter_mut().enumerate() {
                    filter.set_target(
                        Biquad::peak(
                            bands[index],
                            new.gains[index],
                            self.source.sample_rate() as f32,
                        ),
                        self.transition_frames,
                    );
                }
            }
        }
        if new.preamp_db != self.config.preamp_db {
            self.gain_target = 10f32.powf(new.preamp_db / 20.);
            self.gain_ramp_remaining = self.transition_frames;
        }
        if new.equalizer != self.config.equalizer {
            self.eq_mix_target = if new.equalizer { 1. } else { 0. };
            self.eq_ramp_remaining = self.transition_frames;
        }
        self.config = new;
    }
}

pub(crate) fn advance_ramp(current: &mut f32, target: f32, remaining: &mut u32) {
    if *remaining == 0 {
        return;
    }
    *current += (target - *current) / *remaining as f32;
    *remaining -= 1;
    if *remaining == 0 {
        *current = target;
    }
}
impl Iterator for Processed {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.index >= self.frame.len() {
            if self.frames % 2048 == 0 {
                let new = self.shared.read().unwrap().clone();
                self.update_config(new);
            }
            advance_ramp(
                &mut self.gain,
                self.gain_target,
                &mut self.gain_ramp_remaining,
            );
            advance_ramp(
                &mut self.eq_mix,
                self.eq_mix_target,
                &mut self.eq_ramp_remaining,
            );
            for sample in &mut self.frame {
                *sample = self.source.next()?;
            }
            if self.config.mono {
                let mean = self.frame.iter().sum::<f32>() / self.frame.len() as f32;
                self.frame.fill(mean);
            }
            let channels = self.frame.len();
            for (i, s) in self.frame.iter_mut().enumerate() {
                if self.eq_mix > 0. || self.eq_mix_target > 0. {
                    // One non-finite input sample would otherwise stay in the
                    // filter state and silence this channel until the next reload.
                    let dry = if s.is_finite() { *s } else { 0. };
                    let mut wet = dry;
                    for f in &mut self.filters[i] {
                        wet = f.run(wet);
                    }
                    if !wet.is_finite() {
                        for f in &mut self.filters[i] {
                            f.reset_state();
                        }
                        wet = dry;
                    }
                    *s = dry + (wet - dry) * self.eq_mix;
                }
                *s *= self.gain;
                // Balance needs two channels: on a one-channel file it would
                // only turn the sound down (silence at full right).
                if i == 0 && channels > 1 {
                    *s *= 1. - self.config.balance.max(0.);
                } else if i == 1 {
                    *s *= 1. + self.config.balance.min(0.);
                }
                if self.config.limiter {
                    *s = s.clamp(-1., 1.);
                }
                if !s.is_finite() {
                    *s = 0.;
                }
            }
            self.index = 0;
            self.frames += 1;
        }
        let value = self.frame[self.index];
        self.index += 1;
        Some(value)
    }
}
impl Source for Processed {
    fn current_span_len(&self) -> Option<usize> {
        self.source
            .current_span_len()
            .map(|n| n + self.frame.len().saturating_sub(self.index))
    }
    fn channels(&self) -> u16 {
        self.source.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.source.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.source.total_duration()
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.source.try_seek(pos)?;
        self.index = self.frame.len();
        for channel in &mut self.filters {
            for filter in channel {
                filter.reset_state();
            }
        }
        Ok(())
    }
}




/// Counts samples pulled by Rodio's output pipeline, after time stretching.
/// Decoder and stretch read-ahead buffers are deliberately outside this clock.
struct Clocked {
    source: Box<dyn Source + Send>,
    samples: Arc<AtomicU64>,
    count: u64,
}
impl Iterator for Clocked {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        let value = self.source.next()?;
        self.count += 1;
        if self.count % 64 == 0 {
            self.samples.store(self.count, Ordering::Relaxed);
        }
        Some(value)
    }
}
impl Source for Clocked {
    fn current_span_len(&self) -> Option<usize> {
        self.source.current_span_len()
    }
    fn channels(&self) -> u16 {
        self.source.channels()
    }
    fn sample_rate(&self) -> u32 {
        self.source.sample_rate()
    }
    fn total_duration(&self) -> Option<Duration> {
        self.source.total_duration()
    }
}
/// What the engine thread keeps of a pipeline: the output-sample counter, the
/// marks the audio thread pushes upstream, the hand-over slot for the next track
/// and the maths that turn output samples into source frames.
struct Timing {
    out_samples: Arc<AtomicU64>,
    marks: Arc<Marks>,
    slot: SharedSlot,
    /// Source frames consumed per output sample: the speed when the stretcher
    /// keeps the pitch, else 1, divided by the channel count.
    k_per_out_sample: f64,
    /// Source frames per second.
    rate: f64,
    /// Channels of the pipeline; a joined track must have the same count.
    channels: u16,
}
/// The chain between a decoder and the sink, with the handles the engine thread
/// keeps. The decoder (the only owner of the file handle: no clone, probe or
/// background reader exists) and every wrapper stage live inside `clocked`;
/// dropping it closes the file. The open-source token sits behind the decoder in
/// its `Held`, so it is released only after the file is closed.
struct Pipeline {
    clocked: Clocked,
    fade: Arc<FadeControl>,
    timing: Timing,
    /// Speed the sink itself must apply (resampling speed when not preserving pitch).
    sink_speed: f32,
}
/// What a pipeline's first stage shares with the engine thread.
struct Wiring {
    /// ReplayGain of the playing track in dB (f32 bits).
    live_gain: Arc<AtomicU32>,
    /// Finished tracks go here so their files close on the engine thread.
    retire: Sender<Held>,
    /// The skip-silence switch; `None` builds the pipeline without the stage
    /// (live streams, where cutting would only add delay).
    skip: Option<Arc<AtomicBool>>,
}

fn build_pipeline(
    decoder: Box<dyn Source + Send>,
    effects: &Arc<RwLock<Dsp>>,
    preserve_pitch: bool,
    speed: f32,
    open_sources: &Arc<AtomicUsize>,
    wiring: &Wiring,
) -> Pipeline {
    let channels = decoder.channels();
    let sample_rate = decoder.sample_rate();
    let marks = Arc::new(Marks::default());
    let slot = Slot::new_shared();
    let held = Held::new(decoder, VecDeque::new(), SourceToken::new(open_sources));
    let mut source: Box<dyn Source + Send> = Box::new(Joined::new(
        held,
        slot.clone(),
        marks.clone(),
        wiring.live_gain.clone(),
        wiring.retire.clone(),
    ));
    // Silence is judged and cut in source time, in front of the stretcher.
    if let Some(skip) = &wiring.skip {
        source = Box::new(SkipSilence::new(source, skip.clone(), marks.clone()));
    }
    if preserve_pitch && (speed - 1.).abs() > f32::EPSILON {
        source = Box::new(crate::pitch::PitchSource::new(source, speed));
    }
    source = Box::new(Processed::new(source, effects.clone()));
    // The sink resamples for a pitch-shifting speed after this point.
    let sink_speed = if preserve_pitch { 1. } else { speed };
    // Every new decoder starts from silence and ramps up.
    let fade = Arc::new(FadeControl::new(0., 1.));
    source = Box::new(Fade::new(source, fade.clone(), 0., sink_speed));
    let out_samples = Arc::new(AtomicU64::new(0));
    Pipeline {
        clocked: Clocked {
            source,
            samples: out_samples.clone(),
            count: 0,
        },
        fade,
        timing: Timing {
            out_samples,
            marks,
            slot,
            k_per_out_sample: if preserve_pitch { speed as f64 } else { 1. } / channels as f64,
            rate: sample_rate as f64,
            channels,
        },
        sink_speed,
    }
}
// ---------------------------------------------------------------------------
// Live streams
// ---------------------------------------------------------------------------
//
// A station is a byte reader that blocks (even for long stretches while it
// reconnects), so it is never read from the audio callback. One helper thread per
// connection opens the stream, builds the decoder, and keeps decoding into a small
// bounded queue of samples. The output side reads that queue through `LiveSource`,
// which turns an empty queue into silence instead of waiting.

/// How much audio `open_stream` buffers before it returns.
const STREAM_PRELOAD_MS: u32 = 600;
/// Decoded audio kept ahead of the output (the stream's own ring holds more).
const LIVE_QUEUE_SECONDS: f32 = 1.5;
/// After an underrun the output stays silent until this much audio is queued, so a
/// thin connection becomes one short gap instead of a stutter.
const LIVE_RESUME_SECONDS: f32 = 0.4;
/// Upper bound of the queue whatever the stream claims (samples, 8 MB of f32).
const LIVE_QUEUE_MAX_SAMPLES: usize = 2_000_000;
/// Decoded samples travel in chunks of this many frames (one MP3 frame).
const LIVE_CHUNK_FRAMES: usize = 1152;
/// Silence is produced in blocks of this length while the queue is empty.
const LIVE_SILENCE_MS: usize = 10;
/// Length of the fade-in when audio resumes after silence.
const LIVE_RESUME_RAMP_MS: usize = 5;
/// How often a producer waiting for room looks at the stop flags.
const LIVE_PUSH_SLICE: Duration = Duration::from_millis(20);
/// Decoder rebuilds after the first failure wait 100 ms, 200 ms, ... up to 2 s,
/// and give up after this many failures in a row.
const LIVE_REBUILD_BASE: Duration = Duration::from_millis(100);
const LIVE_REBUILD_MAX_DELAY: Duration = Duration::from_secs(2);
const LIVE_REBUILD_MAX_FAILURES: u32 = 12;
/// A decoder that produced at least this much audio before it ended was healthy.
const LIVE_GOOD_RUN_SECONDS: f64 = 0.5;
/// Spent sample buffers kept for reuse so the audio thread does not free memory.
const LIVE_POOL_MAX: usize = 4;
/// Stopped helper threads still winding down (they always do; see `reap_retired`).
const LIVE_RETIRED_MAX: usize = 16;
/// How long dropping a session may wait for its helper thread.
const LIVE_DROP_JOIN: Duration = Duration::from_millis(250);
const MSG_FORMAT_CHANGED: &str = "The station changed its audio format";
const MSG_UNPLAYABLE: &str = "The station is not sending audio that can be played";
const MSG_STREAM_ENDED: &str = "The stream ended";
const MSG_STREAM_FORMAT: &str = "This stream's audio format is not supported";
const MSG_STREAM_THREAD: &str = "Could not start the stream reader";

/// The connected stream's UI view, shared between the engine thread and the app.
type LiveHandle = Arc<Mutex<Option<StreamHandle>>>;

fn lock_live<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// What the helper thread tells the engine thread.
enum LiveEvent {
    /// The stream is open and a decoder was built; samples will flow into `queue`.
    Opened {
        serial: u64,
        handle: StreamHandle,
        queue: Arc<LiveQueue>,
        channels: u16,
        sample_rate: u32,
    },
    /// Connecting or decoding failed before any audio could be played.
    Failed { serial: u64, message: String },
}

#[derive(Debug, Default)]
struct QueueInner {
    chunks: VecDeque<Vec<f32>>,
    buffered: usize,
    capacity: usize,
    resume: usize,
    pool: Vec<Vec<f32>>,
    /// The engine asked to stop: the producer quits and the output ends.
    shutdown: bool,
    /// The producer is done (stream closed, failed, or given up).
    finished: bool,
    error: Option<String>,
    /// Samples ever queued / ever handed to the output.
    pushed: u64,
    taken: u64,
}
/// What the engine thread can see of the queue.
#[derive(Clone, Debug, PartialEq, Eq)]
struct QueueState {
    buffered: usize,
    finished: bool,
    error: Option<String>,
    pushed: u64,
    taken: u64,
}
/// Bounded hand-over of decoded samples from the decode thread to the output.
/// The producer waits while the queue is full; the consumer never waits (see
/// `LiveSource`). Samples travel in chunks, so the lock is taken once per chunk.
#[derive(Debug)]
struct LiveQueue {
    inner: Mutex<QueueInner>,
    space: Condvar,
}
impl LiveQueue {
    fn new(capacity: usize, resume: usize) -> Self {
        let capacity = capacity.clamp(1, LIVE_QUEUE_MAX_SAMPLES);
        Self {
            inner: Mutex::new(QueueInner {
                capacity,
                resume: resume.min(capacity / 2),
                ..Default::default()
            }),
            space: Condvar::new(),
        }
    }
    /// Sized for about 1.5 s of audio in this format.
    fn for_format(channels: u16, sample_rate: u32) -> Self {
        let per_second = (sample_rate as usize).saturating_mul(usize::from(channels));
        let seconds = |s: f32| (per_second as f32 * s) as usize;
        Self::new(seconds(LIVE_QUEUE_SECONDS), seconds(LIVE_RESUME_SECONDS))
    }
    /// A buffer for the next chunk, reusing a spent one when there is one.
    fn take_buffer(&self, len: usize) -> Vec<f32> {
        let mut inner = lock_live(&self.inner);
        let mut buffer = inner.pool.pop().unwrap_or_default();
        drop(inner);
        buffer.clear();
        buffer.reserve(len);
        buffer
    }
    /// Queue a chunk (whole frames). Waits while the queue is full and returns
    /// false, dropping the chunk, once the queue was shut down or `closed` is set.
    fn push(&self, chunk: Vec<f32>, closed: &AtomicBool) -> bool {
        if chunk.is_empty() {
            return true;
        }
        let mut inner = lock_live(&self.inner);
        loop {
            if inner.shutdown || closed.load(Ordering::SeqCst) {
                return false;
            }
            if inner.buffered < inner.capacity {
                break;
            }
            inner = match self.space.wait_timeout(inner, LIVE_PUSH_SLICE) {
                Ok((guard, _)) => guard,
                Err(poison) => poison.into_inner().0,
            };
        }
        inner.buffered += chunk.len();
        inner.pushed = inner.pushed.saturating_add(chunk.len() as u64);
        inner.chunks.push_back(chunk);
        true
    }
    /// The producer is done; `error` says why when it was not a plain stop.
    fn finish(&self, error: Option<String>) {
        let mut inner = lock_live(&self.inner);
        inner.finished = true;
        if inner.error.is_none() {
            inner.error = error;
        }
    }
    /// Stop both sides at once: the producer quits, the output ends.
    fn shutdown(&self) {
        let mut inner = lock_live(&self.inner);
        inner.shutdown = true;
        inner.chunks.clear();
        inner.buffered = 0;
        drop(inner);
        self.space.notify_all();
    }
    fn state(&self) -> QueueState {
        let inner = lock_live(&self.inner);
        QueueState {
            buffered: inner.buffered,
            finished: inner.finished,
            error: inner.error.clone(),
            pushed: inner.pushed,
            taken: inner.taken,
        }
    }
}

/// The output side of a live stream. It never blocks and, until the stream is
/// finished, never ends: an empty queue is silence (in blocks of whole frames, so
/// channels stay aligned) and after such an underrun the output waits until a
/// little audio has built up again, then fades in over a few milliseconds so the
/// restart does not click.
struct LiveSource {
    queue: Arc<LiveQueue>,
    channels: u16,
    sample_rate: u32,
    chunk: Vec<f32>,
    index: usize,
    silence: usize,
    silence_block: usize,
    rebuffering: bool,
    /// Samples of the fade-in still to come, and its full length.
    ramp_left: usize,
    ramp_total: usize,
}
impl LiveSource {
    fn new(queue: Arc<LiveQueue>, channels: u16, sample_rate: u32) -> Self {
        let frames = ((sample_rate as usize / 1000).max(1) * LIVE_SILENCE_MS).max(1);
        let ramp_frames = (sample_rate as usize / 1000).max(1) * LIVE_RESUME_RAMP_MS;
        Self {
            queue,
            channels,
            sample_rate,
            chunk: Vec::new(),
            index: 0,
            silence: 0,
            silence_block: frames * usize::from(channels.max(1)),
            // The first audio waits for the same cushion an underrun does.
            rebuffering: true,
            ramp_left: 0,
            ramp_total: ramp_frames * usize::from(channels.max(1)),
        }
    }
    /// The current chunk is used up: take the next one, or produce silence, or end.
    fn refill(&mut self) -> Option<f32> {
        let mut inner = lock_live(&self.queue.inner);
        let spent = std::mem::take(&mut self.chunk);
        if spent.capacity() > 0 && inner.pool.len() < LIVE_POOL_MAX {
            inner.pool.push(spent);
        }
        self.index = 0;
        if inner.buffered == 0 {
            if inner.finished || inner.shutdown {
                return None;
            }
            self.rebuffering = true;
            self.silence = self.silence_block.saturating_sub(1);
            return Some(0.);
        }
        if self.rebuffering && inner.buffered < inner.resume && !inner.finished {
            self.silence = self.silence_block.saturating_sub(1);
            return Some(0.);
        }
        if self.rebuffering {
            self.rebuffering = false;
            self.ramp_left = self.ramp_total;
        }
        while let Some(chunk) = inner.chunks.pop_front() {
            inner.buffered = inner.buffered.saturating_sub(chunk.len());
            inner.taken = inner.taken.saturating_add(chunk.len() as u64);
            if !chunk.is_empty() {
                drop(inner);
                self.queue.space.notify_one();
                self.chunk = chunk;
                self.index = 0;
                // Takes the first sample (and applies a pending fade-in to it).
                return self.next();
            }
        }
        // Only empty chunks (never queued): treat it as an underrun.
        inner.buffered = 0;
        self.silence = self.silence_block.saturating_sub(1);
        Some(0.)
    }
}
impl Iterator for LiveSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if let Some(sample) = self.chunk.get(self.index).copied() {
            self.index += 1;
            if self.ramp_left == 0 {
                return Some(sample);
            }
            let channels = usize::from(self.channels.max(1));
            let done = self.ramp_total - self.ramp_left;
            self.ramp_left -= 1;
            let gain = (done / channels) as f32 / (self.ramp_total / channels).max(1) as f32;
            return Some(sample * gain);
        }
        if self.silence > 0 {
            self.silence -= 1;
            return Some(0.);
        }
        self.refill()
    }
}
impl Source for LiveSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// Extension hint for the decoder from the response's media type.
fn decoder_hint(content_type: &str) -> Option<&'static str> {
    let kind = content_type.to_ascii_lowercase();
    if kind.contains("aac") {
        Some("aac")
    } else if kind.contains("mpeg") || kind.contains("mp3") {
        Some("mp3")
    } else if kind.contains("ogg") || kind.contains("vorbis") {
        Some("ogg")
    } else if kind.contains("flac") {
        Some("flac")
    } else {
        None
    }
}

/// The station's byte stream, shared so a decoder can be rebuilt over it. Only
/// one decoder reads at a time (the old one is dropped first), so the lock is
/// never contended; closing goes through the `StreamHandle`, not this lock.
#[derive(Clone)]
struct SharedLive(Arc<Mutex<LiveStream>>);
impl Read for SharedLive {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        lock_live(&self.0).read(buf)
    }
}
type LiveDecoder = Decoder<NonSeekable<SharedLive>>;

/// A decoder over the live byte stream: not seekable, no gapless trimming, and
/// no byte length (which would make it claim to be seekable).
fn build_live_decoder(reader: &SharedLive, content_type: &str) -> Result<LiveDecoder, DecoderError> {
    let mut builder = Decoder::builder()
        .with_data(NonSeekable(reader.clone()))
        .with_seekable(false)
        .with_gapless(false);
    if let Some(hint) = decoder_hint(content_type) {
        builder = builder.with_hint(hint);
    }
    if !content_type.is_empty() {
        builder = builder.with_mime_type(content_type);
    }
    builder.build()
}
fn decoder_failure(error: &DecoderError) -> String {
    match error {
        DecoderError::UnrecognizedFormat => MSG_STREAM_FORMAT.to_owned(),
        _ => "The station's audio could not be decoded".to_owned(),
    }
}
fn live_format_supported(channels: u16, sample_rate: u32) -> bool {
    (1..=32).contains(&channels) && (8000..=384_000).contains(&sample_rate)
}

/// What to do when a decoder ended while the stream is still alive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rebuild {
    After(Duration),
    GiveUp,
}
/// Back-off for rebuilding the decoder: a healthy run (or a reconnect splice)
/// rebuilds at once; repeated quick failures wait longer each time, and a stream
/// that never yields audio again is abandoned, so garbage cannot spin the CPU.
#[derive(Debug)]
struct RebuildPolicy {
    failures: u32,
    base: Duration,
}
impl RebuildPolicy {
    fn new(base: Duration) -> Self {
        Self { failures: 0, base }
    }
    /// The decoder ended after `audio_seconds` of audio; `spliced` is true when
    /// the stream reconnected meanwhile (an expected reason to start a new decoder).
    fn decoder_ended(&mut self, audio_seconds: f64, spliced: bool) -> Rebuild {
        if spliced || audio_seconds >= LIVE_GOOD_RUN_SECONDS {
            self.failures = 0;
        } else {
            self.failures = self.failures.saturating_add(1);
        }
        self.next()
    }
    fn build_failed(&mut self) -> Rebuild {
        self.failures = self.failures.saturating_add(1);
        self.next()
    }
    fn next(&self) -> Rebuild {
        if self.failures > LIVE_REBUILD_MAX_FAILURES {
            return Rebuild::GiveUp;
        }
        if self.failures == 0 {
            return Rebuild::After(Duration::ZERO);
        }
        let shift = (self.failures - 1).min(10);
        Rebuild::After(
            self.base
                .saturating_mul(1u32 << shift)
                .min(LIVE_REBUILD_MAX_DELAY),
        )
    }
}

/// Sleeps `duration` in short slices; false when the stream was closed meanwhile.
fn sleep_unless_closed(closed: &AtomicBool, duration: Duration) -> bool {
    let end = Instant::now() + duration;
    loop {
        if closed.load(Ordering::SeqCst) {
            return false;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        thread::sleep(left.min(LIVE_PUSH_SLICE));
    }
}

/// Everything the decode loop needs from the stream, so it can be tested
/// without a network.
struct LoopEnv<'a> {
    queue: &'a LiveQueue,
    closed: &'a AtomicBool,
    status: &'a dyn Fn() -> StreamStatus,
    connections: &'a dyn Fn() -> u32,
}
enum Pumped {
    /// The decoder ended after producing this many samples.
    Ended(u64),
    /// The queue was shut down (or the stream closed) while pushing.
    Stopped,
    /// The decoder now produces another channel count or sample rate.
    FormatChanged,
}
/// Moves decoded samples into the queue, a chunk of whole frames at a time.
fn pump_decoder<S: Source>(decoder: &mut S, env: &LoopEnv, format: (u16, u32)) -> Pumped {
    let channels = usize::from(format.0).max(1);
    let chunk_len = LIVE_CHUNK_FRAMES * channels;
    let mut chunk = env.queue.take_buffer(chunk_len);
    let mut total = 0u64;
    loop {
        match decoder.next() {
            Some(sample) => {
                chunk.push(if sample.is_finite() { sample } else { 0. });
                if chunk.len() >= chunk_len {
                    if (decoder.channels(), decoder.sample_rate()) != format {
                        return Pumped::FormatChanged;
                    }
                    total = total.saturating_add(chunk.len() as u64);
                    if !env.queue.push(chunk, env.closed) {
                        return Pumped::Stopped;
                    }
                    chunk = env.queue.take_buffer(chunk_len);
                }
            }
            None => {
                let whole = chunk.len() - chunk.len() % channels;
                chunk.truncate(whole);
                total = total.saturating_add(chunk.len() as u64);
                if !env.queue.push(chunk, env.closed) {
                    return Pumped::Stopped;
                }
                return Pumped::Ended(total);
            }
        }
    }
}
/// Decodes until the stream is closed or fails for good. A decoder that ends while
/// the stream is alive (the demuxer gave up on a splice or on garbage) is rebuilt
/// over the same stream with `rebuild`, with back-off. Ends the queue with the
/// reason: `None` for a plain stop.
fn decode_loop<S: Source>(
    env: &LoopEnv,
    first: S,
    rebuild: &mut dyn FnMut() -> Result<S, String>,
    policy: &mut RebuildPolicy,
) {
    let format = (first.channels(), first.sample_rate());
    let per_second = (f64::from(format.0.max(1)) * f64::from(format.1.max(1))).max(1.);
    let mut seen = (env.connections)();
    let mut next = Some(first);
    while let Some(mut decoder) = next.take() {
        let produced = match pump_decoder(&mut decoder, env, format) {
            Pumped::Ended(samples) => samples,
            Pumped::Stopped => return env.queue.finish(None),
            Pumped::FormatChanged => return env.queue.finish(Some(MSG_FORMAT_CHANGED.into())),
        };
        drop(decoder);
        let spliced = (env.connections)() != seen;
        let mut decision = policy.decoder_ended(produced as f64 / per_second, spliced);
        loop {
            if env.closed.load(Ordering::SeqCst) {
                return env.queue.finish(None);
            }
            if let StreamStatus::Failed(message) = (env.status)() {
                return env.queue.finish(Some(message));
            }
            match decision {
                Rebuild::GiveUp => return env.queue.finish(Some(MSG_UNPLAYABLE.into())),
                Rebuild::After(delay) => {
                    if !delay.is_zero() && !sleep_unless_closed(env.closed, delay) {
                        return env.queue.finish(None);
                    }
                }
            }
            match rebuild() {
                Ok(decoder) => {
                    if (decoder.channels(), decoder.sample_rate()) != format {
                        return env.queue.finish(Some(MSG_FORMAT_CHANGED.into()));
                    }
                    seen = (env.connections)();
                    next = Some(decoder);
                    break;
                }
                Err(_) => decision = policy.build_failed(),
            }
        }
    }
}

/// Reports a helper thread that died: before the stream opened as a failed
/// connection, afterwards as the end of the sample queue. On a normal exit it only
/// makes sure the queue is marked finished.
struct Sentinel {
    events: Sender<LiveEvent>,
    serial: u64,
    queue: Option<Arc<LiveQueue>>,
}
impl Drop for Sentinel {
    fn drop(&mut self) {
        let died = thread::panicking().then(|| "The stream reader stopped unexpectedly".to_owned());
        match &self.queue {
            Some(queue) => queue.finish(died),
            None => {
                if let Some(message) = died {
                    let _ = self.events.send(LiveEvent::Failed {
                        serial: self.serial,
                        message,
                    });
                }
            }
        }
    }
}
/// What the engine hands the helper thread.
struct StreamJob {
    url: String,
    serial: u64,
    /// Shared with the stream itself: setting it closes the stream.
    cancel: Arc<AtomicBool>,
    events: Sender<LiveEvent>,
    /// Counts running helper threads (see `Worker::live_threads`).
    _token: SourceToken,
}
/// Open, decode and keep decoding one connection (runs on its own thread).
fn run_stream_job(job: StreamJob) {
    let StreamJob {
        url,
        serial,
        cancel,
        events,
        _token,
    } = job;
    // A panic anywhere below must not leave the engine waiting for ever.
    let mut sentinel = Sentinel {
        events: events.clone(),
        serial,
        queue: None,
    };
    let fail = |message: String| {
        let _ = events.send(LiveEvent::Failed { serial, message });
    };
    let live = match open_stream_with_cancel(&url, STREAM_PRELOAD_MS, cancel.clone()) {
        Ok(live) => live,
        Err(StreamError::Cancelled) => return,
        Err(error) => return fail(error.to_string()),
    };
    if cancel.load(Ordering::SeqCst) {
        return;
    }
    let handle = live.handle();
    let content_type = handle.info().content_type.clone();
    let reader = SharedLive(Arc::new(Mutex::new(live)));
    let decoder = match build_live_decoder(&reader, &content_type) {
        Ok(decoder) => decoder,
        Err(error) => {
            handle.close();
            if !cancel.load(Ordering::SeqCst) {
                fail(decoder_failure(&error));
            }
            return;
        }
    };
    let (channels, sample_rate) = (decoder.channels(), decoder.sample_rate());
    if !live_format_supported(channels, sample_rate) {
        handle.close();
        return fail(MSG_STREAM_FORMAT.to_owned());
    }
    let queue = Arc::new(LiveQueue::for_format(channels, sample_rate));
    sentinel.queue = Some(queue.clone());
    let opened = LiveEvent::Opened {
        serial,
        handle: handle.clone(),
        queue: queue.clone(),
        channels,
        sample_rate,
    };
    if events.send(opened).is_err() {
        handle.close();
        return;
    }
    let status = || handle.status();
    let connections = || handle.connections();
    let env = LoopEnv {
        queue: &queue,
        closed: &cancel,
        status: &status,
        connections: &connections,
    };
    let mut rebuild = || build_live_decoder(&reader, &content_type).map_err(|e| e.to_string());
    let mut policy = RebuildPolicy::new(LIVE_REBUILD_BASE);
    decode_loop(&env, decoder, &mut rebuild, &mut policy);
    // Whatever ended the loop, the connection goes with it.
    handle.close();
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LiveState {
    /// The helper thread is connecting.
    Opening,
    /// Connected and playing (or reconnecting inside the stream reader).
    Running,
    /// Nothing is connected: paused, failed or lost its output. Play reconnects.
    Parked,
}
/// The station the engine currently has loaded instead of a file.
struct LiveSession {
    id: String,
    url: String,
    state: LiveState,
    /// Identifies the connection attempt; results of older attempts are dropped.
    serial: u64,
    cancel: Arc<AtomicBool>,
    queue: Option<Arc<LiveQueue>>,
    handle: Option<StreamHandle>,
    thread: Option<thread::JoinHandle<()>>,
}
impl LiveSession {
    fn new(id: String, url: String) -> Self {
        Self {
            id,
            url,
            state: LiveState::Parked,
            serial: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            queue: None,
            handle: None,
            thread: None,
        }
    }
    /// Tell every part to stop and hand back the helper thread to reap. Never
    /// blocks: closing wakes a reader at once, and an open in progress watches
    /// the cancel flag.
    fn stop(&mut self) -> Option<thread::JoinHandle<()>> {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle.close();
        }
        if let Some(queue) = self.queue.take() {
            queue.shutdown();
        }
        self.state = LiveState::Parked;
        self.thread.take()
    }
}
impl Drop for LiveSession {
    fn drop(&mut self) {
        if let Some(thread) = self.stop() {
            wait_for_thread(thread, LIVE_DROP_JOIN);
        }
    }
}
/// Joins a thread that was told to stop, waiting at most `limit`; a thread that is
/// still running after that is left to finish on its own.
fn wait_for_thread(thread: thread::JoinHandle<()>, limit: Duration) {
    let end = Instant::now() + limit;
    while !thread.is_finished() {
        if Instant::now() >= end {
            return;
        }
        thread::sleep(Duration::from_micros(500));
    }
    let _ = thread.join();
}
/// Elapsed listening time of a stream: what was heard before the last reconnect
/// (`parked`) plus what the current pipeline has played. Paused time never counts
/// because a parked stream has no pipeline and the pipeline's clock only runs
/// while the output pulls samples.
fn listening_seconds(parked: f64, samples: u64, seconds_per_sample: f64) -> f64 {
    let parked = if parked.is_finite() { parked.max(0.) } else { 0. };
    let played = samples as f64 * seconds_per_sample;
    if played.is_finite() && played >= 0. {
        parked + played
    } else {
        parked
    }
}

/// One track inside the pipeline's continuous run of source frames.
#[allow(dead_code)] // `path` and `request_id` are read by the preload join
struct Segment {
    id: String,
    path: PathBuf,
    request_id: u64,
    /// Position in the track, in seconds, where the segment's first frame is.
    start: f64,
    /// Source-frame index (counted across segments) of the segment's first frame.
    base_j: u64,
    duration: f64,
}
/// The preloaded track the engine has handed to the pipeline's slot and will
/// become the current track when the join mark is reached.
#[allow(dead_code)] // `request_id` is read from the join mark instead
struct Armed {
    id: String,
    path: PathBuf,
    duration: f64,
    request_id: u64,
}
/// Where the playing pipeline is, in source time, however many tracks it has
/// joined and however much silence it has cut. The output-sample counter is the
/// clock; the marks the audio thread pushes upstream say where the source frames
/// stop being one track and where some were removed. The read-ahead buffers of the
/// decoder and the stretcher are outside this clock, as they always were.
struct Timeline {
    out_samples: Arc<AtomicU64>,
    marks: Arc<Marks>,
    slot: SharedSlot,
    k_per_out_sample: f64,
    seconds_per_sample: f64,
    rate: f64,
    channels: u16,
    /// Source frames removed by silence cuts the clock has already passed.
    skipped: u64,
    /// The part of `skipped` already added to the engine's session total.
    counted: u64,
    /// The track playing now.
    segment: Segment,
}
impl Timeline {
    fn new(timing: Timing, segment: Segment) -> Self {
        Self {
            out_samples: timing.out_samples,
            marks: timing.marks,
            slot: timing.slot,
            k_per_out_sample: timing.k_per_out_sample,
            seconds_per_sample: timing.k_per_out_sample / timing.rate,
            rate: timing.rate,
            channels: timing.channels,
            skipped: 0,
            counted: 0,
            segment,
        }
    }
    /// Source frames the output has used up (cuts passed included): the unit the
    /// join marks are counted in. Applies the cuts the clock has reached.
    fn j_now(&mut self) -> f64 {
        let out = self.out_samples.load(Ordering::Relaxed);
        self.j_at(out)
    }
    /// `j_now` for one reading of the output counter, so the cuts passed and the
    /// position derived from them always come from the same count.
    fn j_at(&mut self, out: u64) -> f64 {
        let k = out as f64 * self.k_per_out_sample;
        self.skipped = self.skipped.saturating_add(self.marks.take_cuts_due(k));
        k + self.skipped as f64
    }
    /// Position in the current track. With no join or cut this is the segment's
    /// start plus the output count times the seconds per sample, exactly as the
    /// single-file clock was.
    fn position(&mut self) -> f64 {
        let out = self.out_samples.load(Ordering::Relaxed);
        self.j_at(out);
        let played = listening_seconds(self.segment.start, out, self.seconds_per_sample);
        let shift = if self.rate > 0. {
            (self.skipped as f64 - self.segment.base_j as f64) / self.rate
        } else {
            0.
        };
        let position = played + shift;
        if position.is_finite() {
            position.max(0.)
        } else {
            self.segment.start.max(0.)
        }
    }
    /// Engine side: refuse any later offer to the slot and drop what waits in it
    /// here, so a preloaded file is closed on this thread.
    fn close_slot(&self) {
        let waiting = lock(&self.slot).close();
        drop(waiting);
    }
}

/// A preloaded track on its way from the opener thread: decoded ahead, with its
/// own length.
struct Opened {
    held: Held,
    duration: f64,
}
/// What the opener thread sends: the preload's request id and its outcome.
type OpenResult = (u64, Result<Opened, String>);
/// The preload whose opener thread is running (or whose result is not collected
/// yet). The track is not in the slot until `arm_preload` puts it there.
struct Pending {
    request_id: u64,
    id: String,
    path: PathBuf,
    gain_db: f32,
    /// Tells the opener to stop early; its result is dropped on arrival anyway.
    cancel: Arc<AtomicBool>,
}

/// Open the local file using the existing Ogg/raw decoders or the selected-track
/// container path for MP4/Matroska.
fn open_file_decoder(
    mut file: File,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<Box<dyn Source + Send>, String> {
    if crate::opus::is_ogg_opus(&mut file) {
        return match cancel {
            Some(cancel) => crate::opus::open_opus_cancelable(file, &cancel),
            None => crate::opus::open_opus(file),
        };
    }
    if let Some(kind) = crate::container_audio::detect_container(&mut file)? {
        let opened = crate::container_audio::open_container(file, kind, cancel)?;
        return if opened.codec_params.codec == symphonia::core::codecs::CODEC_TYPE_OPUS {
            crate::opus::open_container_opus(opened)
        } else {
            crate::container_audio::open_symphonia_audio(opened)
        };
    }
    match Decoder::try_from(file) {
        Ok(decoder) => Ok(Box::new(decoder)),
        Err(error) => Err(error.to_string()),
    }
}

/// Opens the file of the next track and decodes the first half second of it.
/// Runs on the opener thread. The token counts the open file from the first
/// instant, so the engine's accounting of open files stays truthful while this
/// runs. The prefetch is only made for the format the pipeline runs, which keeps
/// it bounded by what the playing track already costs.
fn open_next_track(
    path: &Path,
    token: SourceToken,
    expect: (u16, u32),
    cancel: Arc<AtomicBool>,
) -> Result<Opened, String> {
    let cancelled = || cancel.load(Ordering::Relaxed);
    let file = File::open(path).map_err(|e| format!("Cannot open the next track: {e}"))?;
    if cancelled() {
        return Err("cancelled".into());
    }
    let decoder = open_file_decoder(file, Some(cancel.clone()))
        .map_err(|e| format!("Cannot decode the next track: {e}"))?;
    if cancelled() {
        return Err("cancelled".into());
    }
    let duration = decoder
        .total_duration()
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.);
    let mut held = Held::new(decoder, VecDeque::new(), token);
    let format = (held.channels(), held.sample_rate());
    if format == expect && live_format_supported(format.0, format.1) {
        let frames = (f64::from(format.1) * f64::from(PREFETCH_SECONDS)) as usize;
        let samples = frames
            .saturating_mul(usize::from(format.0))
            .min(PREFETCH_MAX_SAMPLES);
        held.fill_prefetch(samples);
    }
    if cancelled() {
        return Err("cancelled".into());
    }
    Ok(Opened { held, duration })
}


struct Worker {
    effects: Arc<RwLock<Dsp>>,
    shared: Arc<Mutex<Snapshot>>,
    device: String,
    volume: f32,
    speed: f32,
    preserve_pitch: bool,
    silent: bool,
    stream: Option<OutputStream>,
    sink: Option<Sink>,
    current: Option<(String, PathBuf)>,
    /// The pipeline's position clock; `None` while nothing is open.
    timeline: Option<Timeline>,
    /// The position kept while there is no pipeline (paused with the file
    /// closed, a lost device, a parked station). For a station it is the
    /// listening time before the current connection.
    parked: f64,
    /// The preloaded track waiting in the slot (see `cross_boundaries`).
    armed: Option<Armed>,
    /// The preload being opened on the helper thread, if any.
    pending: Option<Pending>,
    ready_tx: Sender<OpenResult>,
    ready_rx: Receiver<OpenResult>,
    /// Opener threads running or about to start.
    opener_threads: Arc<AtomicUsize>,
    live_gain: Arc<AtomicU32>,
    /// The skip-silence switch the pipeline's silence stage reads.
    skip: Arc<AtomicBool>,
    /// Silence removed since the last `load`, in seconds of source time: the
    /// cuts the clock has passed in pipelines that are gone, plus the current one's
    /// (see `collect_skipped`).
    skipped_total: f64,
    /// What `side` shows at the moment, so the lock is taken only on a change.
    skipped_published: f64,
    side: Arc<Mutex<Side>>,
    retire_tx: Sender<Held>,
    retire_rx: Receiver<Held>,
    error_tx: Sender<(u64, String)>,
    error_rx: mpsc::Receiver<(u64, String)>,
    generation: u64,
    current_request_id: u64,
    fade: Option<Arc<FadeControl>>,
    events: DeviceEvents,
    link: OutputLink,
    default_watch: DefaultWatch,
    /// Pipelines that still own a decoder, i.e. an open audio file.
    open_sources: Arc<AtomicUsize>,
    /// The file was closed on purpose (`release_file`). Seeks only move the saved
    /// position and speed or pitch changes wait; only `play` and `load` reopen it.
    released: bool,
    /// The loaded station when a stream (not a file) is the current item; then
    /// `current` is `None` and `parked` holds the listening time before the
    /// current connection.
    live: Option<LiveSession>,
    /// Source of `LiveSession::serial`.
    live_serial: u64,
    live_tx: Sender<LiveEvent>,
    live_rx: Receiver<LiveEvent>,
    live_handle: LiveHandle,
    /// "Audio output switched to ..." for the reconnect that follows a changed
    /// default device; raised once the station plays on the new device.
    live_restored: Option<String>,
    /// Stream helper threads running or about to start.
    live_threads: Arc<AtomicUsize>,
    /// Stopped helper threads that have not been joined yet.
    retired: Vec<thread::JoinHandle<()>>,



}
/// Why `Worker::load` gave up. A device problem keeps the track loaded and the
/// position; it is reported as a device event, not as a file error.
enum LoadFailure {
    Device,
    Other(String),
}
impl From<String> for LoadFailure {
    fn from(error: String) -> Self {
        Self::Other(error)
    }
}
impl From<&str> for LoadFailure {
    fn from(error: &str) -> Self {
        Self::Other(error.to_owned())
    }
}
impl Worker {
    fn new(
        effects: Arc<RwLock<Dsp>>,
        shared: Arc<Mutex<Snapshot>>,
        device: String,
        volume: f32,
        speed: f32,
        preserve_pitch: bool,
        silent: bool,
    ) -> Self {
        let (error_tx, error_rx) = mpsc::channel();
        let (live_tx, live_rx) = mpsc::channel();
        let (retire_tx, retire_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        Self {
            effects,
            shared,
            device,
            volume,
            speed,
            preserve_pitch,
            silent,
            stream: None,
            sink: None,
            current: None,
            timeline: None,
            parked: 0.,
            armed: None,
            pending: None,
            ready_tx,
            ready_rx,
            opener_threads: Arc::new(AtomicUsize::new(0)),
            live_gain: Arc::new(AtomicU32::new(0f32.to_bits())),
            skip: Arc::new(AtomicBool::new(false)),
            skipped_total: 0.,
            skipped_published: 0.,
            side: Arc::new(Mutex::new(Side::default())),
            retire_tx,
            retire_rx,
            error_tx,
            error_rx,
            generation: 0,
            current_request_id: 0,
            fade: None,
            events: Arc::new(Mutex::new(VecDeque::new())),
            link: OutputLink::default(),
            default_watch: DefaultWatch::default(),
            open_sources: Arc::new(AtomicUsize::new(0)),
            released: false,
            live: None,
            live_serial: 0,
            live_tx,
            live_rx,
            live_handle: Arc::new(Mutex::new(None)),
            live_restored: None,
            live_threads: Arc::new(AtomicUsize::new(0)),
            retired: Vec::new(),



        }
    }
    fn with_events(mut self, events: DeviceEvents) -> Self {
        self.events = events;
        self
    }
    fn with_live_handle(mut self, handle: LiveHandle) -> Self {
        self.live_handle = handle;
        self
    }
    fn with_live_gain(mut self, gain: Arc<AtomicU32>) -> Self {
        self.live_gain = gain;
        self
    }
    fn with_skip(mut self, skip: Arc<AtomicBool>) -> Self {
        self.skip = skip;
        self
    }
    fn with_side(mut self, side: Arc<Mutex<Side>>) -> Self {
        self.side = side;
        self
    }
    fn wiring(&self) -> Wiring {
        Wiring {
            live_gain: self.live_gain.clone(),
            retire: self.retire_tx.clone(),
            skip: Some(self.skip.clone()),
        }
    }
    fn push_event(&self, event: Option<DeviceEvent>) {
        if let (Some(event), Ok(mut queue)) = (event, self.events.lock()) {
            push_bounded(&mut queue, event, DEVICE_EVENT_CAPACITY);
        }
    }
    /// Position of the loaded track in seconds of source time: the pipeline's
    /// clock, or the parked position when no pipeline is open.
    fn position(&mut self) -> f64 {
        match self.timeline.as_mut() {
            Some(timeline) => timeline.position(),
            None => listening_seconds(self.parked, 0, 0.),
        }
    }
    /// Pause (with the fade-out wait), cross any boundary the clock has reached,
    /// and read the position: the place where playback really stopped.
    fn settle(&mut self) -> f64 {
        self.pause();
        self.cross_boundaries();
        self.position()
    }
    /// No pipeline any more (or a new one is about to replace it): keep
    /// `position`, close the slot (a preloaded file closes here), forget the armed
    /// preload and abandon an opener still at work (its result is dropped when it
    /// arrives). Every path that drops or rebuilds the pipeline comes here instead
    /// of resetting a clock by hand.
    fn park(&mut self, position: f64) {
        // What the clock passed is counted before the pipeline is let go.
        self.collect_skipped();
        if let Some(timeline) = self.timeline.take() {
            timeline.close_slot();
        }
        self.parked = position;
        self.armed = None;
        self.abandon_opener();
        self.set_preload_state(PreloadState::None);
    }
    /// Adds the silence cuts the clock has passed in the current pipeline, since
    /// the last call, to the session total.
    fn collect_skipped(&mut self) {
        let Some(timeline) = self.timeline.as_mut() else {
            return;
        };
        timeline.j_now();
        let fresh = timeline.skipped.saturating_sub(timeline.counted);
        timeline.counted = timeline.skipped;
        if fresh > 0 && timeline.rate > 0. {
            self.skipped_total += fresh as f64 / timeline.rate;
        }
    }
    /// Shows the session total of removed silence to `poll`, when it changed.
    fn publish_skipped(&mut self) {
        self.collect_skipped();
        if self.skipped_total != self.skipped_published {
            self.skipped_published = self.skipped_total;
            lock(&self.side).skipped_seconds = self.skipped_total;
        }
    }
    /// The state of the preload, as `poll` reports it for `last_request`.
    fn set_preload_state(&self, state: PreloadState) {
        lock(&self.side).preload.state = state;
    }
    /// A request arrived: it is the latest one from now on.
    fn start_preload_status(&self, request_id: u64, state: PreloadState) {
        lock(&self.side).preload = PreloadStatus {
            last_request: request_id,
            state,
        };
    }
    /// The opener's result, if it is still coming, is no longer wanted.
    fn abandon_opener(&mut self) {
        if let Some(pending) = self.pending.take() {
            pending.cancel.store(true, Ordering::Relaxed);
        }
    }
    /// A pipeline starts: its clock begins at the segment's start.
    fn start_timeline(&mut self, timing: Timing, segment: Segment) {
        self.park(segment.start);
        self.timeline = Some(Timeline::new(timing, segment));
    }
    /// Files the audio thread finished are closed here, not in its callback.
    fn drain_retired(&mut self) {
        while let Ok(finished) = self.retire_rx.try_recv() {
            drop(finished);
        }
    }
    /// Why a preload after `after` cannot work now, or `None` when it can.
    fn preload_refusal(&mut self, after: &str) -> Option<&'static str> {
        if self.silent {
            return Some("silent session");
        }
        if self.released {
            return Some("file released");
        }
        if self.live.is_some() {
            return Some("stream loaded");
        }
        if !self.sink.as_ref().is_some_and(|sink| !sink.empty()) {
            return Some("nothing playing");
        }
        if self.current.as_ref().map(|(id, _)| id.as_str()) != Some(after) {
            return Some("track changed");
        }
        let Some(timeline) = self.timeline.as_mut() else {
            return Some("nothing playing");
        };
        if matches!(&*lock(&timeline.slot), Slot::Closed) {
            return Some("too late");
        }
        let duration = timeline.segment.duration;
        if duration > 0. && duration - timeline.position() < PRELOAD_MIN_REMAINING_SECONDS {
            return Some("too late");
        }
        if self.opener_threads.load(Ordering::Relaxed) >= OPENER_THREADS_MAX {
            return Some("busy");
        }
        None
    }
    /// `Command::Preload`: refuse at once when it cannot work, otherwise open the
    /// track on a helper thread; `service_preload` arms it when it is ready.
    fn preload(&mut self, request: PreloadRequest) {
        // The newer request wins: whatever the older one holds is let go first.
        self.cancel_preload();
        let PreloadRequest {
            after,
            id,
            path,
            gain_db,
            request_id,
        } = request;
        self.start_preload_status(request_id, PreloadState::None);
        if let Some(reason) = self.preload_refusal(&after) {
            self.set_preload_state(PreloadState::Failed(reason.into()));
            return;
        }
        let Some(timeline) = self.timeline.as_ref() else {
            return;
        };
        let expect = (timeline.channels, timeline.rate as u32);
        let cancel = Arc::new(AtomicBool::new(false));
        // Both counters rise now, on this thread, so a release that comes while
        // the opener is still at work waits for it and the count is truthful.
        let token = SourceToken::new(&self.open_sources);
        let running = SourceToken::new(&self.opener_threads);
        let results = self.ready_tx.clone();
        let (open_path, open_cancel) = (path.clone(), cancel.clone());

        let spawned = thread::Builder::new()
            .name("bs-player-preload".into())
            .spawn(move || {
                // A panic while opening a file must end as a failed preload.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    open_next_track(&open_path, token, expect, open_cancel)
                }))
                .unwrap_or_else(|_| {
                    Err("The audio decoder failed unexpectedly on the next track.".into())
                });

                // If the engine is gone the result is dropped here, closing the file.
                let _ = results.send((request_id, result));
                drop(running);
            });
        match spawned {
            Ok(_detached) => {
                self.pending = Some(Pending {
                    request_id,
                    id,
                    path,
                    gain_db,
                    cancel,
                });
                self.set_preload_state(PreloadState::Opening);
            }
            Err(_) => self.set_preload_state(PreloadState::Failed(
                "could not start the preload reader".into(),
            )),
        }
    }
    /// Collects what opener threads finished. A result that is not the pending
    /// preload's (abandoned, replaced, or the pipeline was rebuilt since) is
    /// dropped here, which closes its file on this thread.
    fn service_preload(&mut self) {
        while let Ok((request_id, result)) = self.ready_rx.try_recv() {
            let wanted = self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.request_id == request_id);
            if !wanted {
                drop(result);
                continue;
            }
            let Some(pending) = self.pending.take() else {
                continue;
            };
            match result {
                Ok(opened) => self.arm_preload(pending, opened),
                Err(reason) => self.set_preload_state(PreloadState::Failed(reason)),
            }
        }
    }
    /// Offers the opened track to the pipeline's slot. The format must be the
    /// pipeline's own (there is no resampler), and the slot must not have closed
    /// because the playing track already ran dry.
    fn arm_preload(&mut self, pending: Pending, opened: Opened) {
        let Pending {
            request_id,
            id,
            path,
            gain_db,
            ..
        } = pending;
        let Some(timeline) = self.timeline.as_ref() else {
            self.set_preload_state(PreloadState::Failed("nothing playing".into()));
            return;
        };
        let (channels, rate) = (opened.held.channels(), opened.held.sample_rate());
        if channels != timeline.channels
            || f64::from(rate) != timeline.rate
            || !live_format_supported(channels, rate)
        {
            self.set_preload_state(PreloadState::Failed("format change".into()));
            return;
        }
        let duration = opened.duration;
        let refused = lock(&timeline.slot)
            .put(NextSegment {
                held: opened.held,
                gain_db,
                request_id,
            })
            .err();
        if let Some(late) = refused {
            // Dropped outside the lock: the file closes here.
            drop(late);
            self.set_preload_state(PreloadState::Failed("too late".into()));
            return;
        }
        self.armed = Some(Armed {
            id,
            path,
            duration,
            request_id,
        });
        self.set_preload_state(PreloadState::Ready);
    }
    /// `Command::CancelPreload`. Where the preload is decides what it costs:
    /// still opening or waiting in the slot, it is simply dropped; taken by the
    /// audio thread but not yet reached by the clock, the next track already sits
    /// in the pipeline (behind the stretcher), so the playing track is reopened at
    /// its position, which throws that pipeline away; already crossed, there is
    /// nothing to cancel.
    fn cancel_preload(&mut self) {
        self.abandon_opener();
        if self.armed.is_some() {
            let waiting = match self.timeline.as_ref() {
                Some(timeline) => lock(&timeline.slot).take_ready(),
                None => None,
            };
            match waiting {
                // The audio thread did not take it: nothing of it was heard.
                // It is closed here, outside the lock.
                Some(next) => drop(next),
                None => self.reload(),
            }
            self.armed = None;
        }
        self.set_preload_state(PreloadState::None);
    }
    /// Waits until no file is open any more, closing what only this thread can:
    /// decoders the audio thread retired and results the openers delivered.
    fn wait_for_files_closed(&mut self, until: Instant) -> bool {
        loop {
            self.drain_retired();
            self.service_preload();
            let step = (Instant::now() + Duration::from_micros(500)).min(until);
            if wait_for_sources_closed(&self.open_sources, step) {
                return true;
            }
            if Instant::now() >= until {
                return false;
            }
        }
    }
    /// The audio thread joined the preloaded track: the clock reached a join mark.
    /// The armed record becomes the current track, the snapshot is switched under
    /// both locks (snapshot first, then the side lock) and the change is queued
    /// for the app. Called first for every command, after every fade wait and
    /// before each snapshot is published.
    fn cross_boundaries(&mut self) {
        loop {
            let Some(timeline) = self.timeline.as_mut() else {
                return;
            };
            let j = timeline.j_now();
            let Some(mark) = timeline.marks.pop_join_due(j) else {
                return;
            };
            let Some(armed) = self.armed.take() else {
                // A boundary nobody announced: nothing to switch to.
                continue;
            };
            let reached = (timeline.segment.start
                + (mark.j as f64 - timeline.segment.base_j as f64) / timeline.rate)
                .max(0.);
            let finished = std::mem::replace(
                &mut timeline.segment,
                Segment {
                    id: armed.id.clone(),
                    path: armed.path.clone(),
                    request_id: mark.request_id,
                    start: 0.,
                    base_j: mark.j,
                    duration: armed.duration,
                },
            );
            self.current = Some((armed.id.clone(), armed.path));
            self.current_request_id = mark.request_id;
            let mut snap = self.shared.lock().unwrap();
            let mut side = lock(&self.side);
            snap.loaded = Some(armed.id.clone());
            snap.request_id = mark.request_id;
            snap.duration = armed.duration;
            snap.position = 0.;
            snap.ended = false;
            snap.error = None;
            side.changes.push(TrackChange {
                from: finished.id,
                from_request_id: finished.request_id,
                from_reached: reached,
                from_duration: finished.duration,
                from_early: ended_early(reached, finished.duration),
                to: armed.id,
                request_id: mark.request_id,
            });
            while side.changes.len() > CHANGES_CAPACITY {
                side.changes.remove(0);
            }
            side.preload.state = PreloadState::None;
        }
    }
    /// The id of what is loaded: the file, or the station.
    fn loaded_id(&self) -> Option<&str> {
        match &self.current {
            Some((id, _)) => Some(id.as_str()),
            None => self.live.as_ref().map(|live| live.id.as_str()),
        }
    }
    fn playing(&self) -> bool {
        self.sink
            .as_ref()
            .is_some_and(|s| !s.is_paused() && !s.empty())
    }
    /// Pause with a short fade-out so the cut does not click. The sink keeps
    /// pulling samples while the gain ramps down, so the position clock stays
    /// exact; the wait is capped when the output is stalled.
    fn pause(&self) {
        if let Some(s) = &self.sink {
            if !s.is_paused() && !s.empty() {
                if let Some(fade) = &self.fade {
                    fade.set_target(0.);
                    wait_for_silence(fade, FADE_WAIT_MAX);
                }
            }
            s.pause();
        }
    }
    /// Pause at once, for an output that is already gone (nothing to fade).
    fn pause_now(&self) {
        if let Some(s) = &self.sink {
            s.pause();
        }
    }
    /// The output stream failed or vanished: pause, keep the loaded track and
    /// the exact position, and tell the app. Playback never resumes by itself;
    /// the next Play reopens the output (see `load`).
    fn lose_output(&mut self, note: String) {
        // A join the clock already passed counts before the position is kept.
        self.cross_boundaries();
        let position = self.position();
        self.pause_now();
        if let Some(s) = self.sink.take() {
            s.stop();
        }
        // A live stream is dropped like a pause does it; Play connects again.
        self.park_live_now();
        self.stream = None;
        self.fade = None;
        // Late errors from the dead stream carry the old generation: ignore them.
        self.generation = self.generation.wrapping_add(1);
        self.park(position);
        {
            let mut snap = self.shared.lock().unwrap();
            snap.position = if snap.duration > 0. {
                position.min(snap.duration)
            } else {
                position
            };
            snap.playing = false;
        }
        let event = self.link.stream_failed(note);
        self.push_event(event);
    }
    /// Reopen the output on the selected device at the same position and play
    /// state. A device that cannot be opened is reported by the load itself.
    fn switch_output(&mut self) {
        if self.live.is_some() {
            return self.switch_output_live();
        }
        let active = self.sink.as_ref().is_some_and(|s| !s.empty());
        let play = self.playing();
        // Pause first, then read the position: a join inside the fade-out window
        // is counted, so the new track is the one that is carried over.
        let position = self.settle();
        if let Some(s) = self.sink.take() {
            s.stop();
        }
        self.stream = None;
        self.park(position);
        match self.current.clone() {
            // Carry on from the same position on the new device, paused or
            // playing as before.
            Some((id, path)) if active => {
                self.load(id, path, position, play, self.current_request_id);
            }
            // Nothing is open or the track had finished: the next Play opens
            // the new device at the kept position.
            _ => {
                self.shared.lock().unwrap().position = position;
            }
        }
    }
    /// With the system default selected, follow it when the OS changes it.
    fn follow_default_device(&mut self) {
        if !self.device.is_empty() || self.stream.is_none() || self.sink.is_none() {
            return;
        }
        let Some(verdict) = self.default_watch.poll(Instant::now(), probe_default_device) else {
            return;
        };
        let old = self.default_watch.previous.take();
        let old_listed = || old.as_deref().is_some_and(output_is_listed);
        match follow_action(verdict, old_listed) {
            FollowAction::Keep => {}
            FollowAction::Switch(name) => {
                self.switch_output();
                if self.stream.is_some() {
                    self.push_event(Some(DeviceEvent::Restored(name)));
                } else if self.live_state() == Some(LiveState::Opening) {
                    // The station reconnects on the new device; tell the app then.
                    self.live_restored = Some(name);
                }
            }
            FollowAction::Pause => self.lose_output(DEVICE_LOST_NOTE.to_owned()),
        }
    }
    /// Opens the output stream when none is open. A device that cannot be opened is
    /// reported as a device event and as `LoadFailure::Device`.
    fn ensure_output(&mut self, play: bool) -> Result<(), LoadFailure> {

        if self.stream.is_none() {
            self.generation = self.generation.wrapping_add(1);
            match open_output(&self.device, self.error_tx.clone(), self.generation) {
                Ok((stream, name)) => {
                    self.stream = Some(stream);
                    let watched = if self.device.is_empty() { name.clone() } else { None };
                    self.default_watch.opened_on(watched, Instant::now());
                    let shown = name.unwrap_or_else(|| "the selected device".to_owned());
                    let event = self.link.open_attempt(Ok(&shown), play);
                    self.push_event(event);
                }
                Err(reason) => {
                    let event = self.link.open_attempt(Err(&reason), play);
                    self.push_event(event);
                    return Err(LoadFailure::Device);
                }
            }
        }
        Ok(())
    }
    /// A sink on the open output (tests: on a pull-based output instead).
    fn new_sink(&mut self) -> Result<Sink, LoadFailure> {

        let stream = self.stream.as_ref().ok_or(LoadFailure::Device)?;
        Ok(Sink::connect_new(stream.mixer()))
    }
    fn load(&mut self, id: String, path: PathBuf, position: f64, play: bool, request_id: u64) {
        self.load_with_gain(id, path, position, play, request_id, None);
    }
    fn load_with_gain(
        &mut self,
        id: String,
        path: PathBuf,
        position: f64,
        play: bool,
        request_id: u64,
        gain_override: Option<f32>,
    ) {
        self.current_request_id = request_id;
        self.released = false;
        // A damaged profile can hold an absurd position; `Duration` would panic on it.
        let position = if position.is_finite() {
            position.clamp(0., crate::state::MAX_POSITION_SECONDS)
        } else {
            0.
        };
        self.pause();
        if let Some(old) = self.sink.take() {
            old.stop();
        }
        // A file replaces a loaded station: close its connection and helper.
        self.drop_live();
        if let Some(gain) = gain_override {
            let gain = if gain.is_finite() { gain } else { 0. };
            self.live_gain.store(gain.to_bits(), Ordering::Relaxed);
        }
        {
            let mut snapshot = self.shared.lock().unwrap();
            begin_audio_request(&mut snapshot);
        }
        let wiring = self.wiring();
        let open = || -> Result<(Sink, f64, f64, Timing, Arc<FadeControl>), LoadFailure> {
            if self.silent {
                return Err("Audio output is disabled for this silent session.".into());
            }
            let file =
                File::open(&path).map_err(|e| format!("Cannot open {}: {e}", path.display()))?;
            let mut decoder = open_file_decoder(file, None)
                .map_err(|e| format!("Cannot decode {}: {e}", path.display()))?;
            let duration = decoder
                .total_duration()
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.);
            let position = if duration > 0. {
                position.clamp(0., (duration - 0.001).max(0.))
            } else {
                position.max(0.)
            };
            if position > 0. {
                decoder
                    .try_seek(Duration::from_secs_f64(position))
                    .map_err(|e| format!("Cannot seek to this listening position: {e}"))?;
            }
            self.ensure_output(play)?;
            let channels = decoder.channels();
            let sample_rate = decoder.sample_rate();
            if channels == 0 || channels > 32 || !(8000..=384000).contains(&sample_rate) {
                return Err(format!(
                    "Unsupported audio format: {channels} channels at {sample_rate} Hz. Supported output range is 1–32 channels, 8–384 kHz."
                )
                .into());
            }
            let pipeline = build_pipeline(
                decoder,
                &self.effects,
                self.preserve_pitch,
                self.speed,
                &self.open_sources,
                &wiring,
            );
            let sink = self.new_sink()?;
            sink.pause();
            sink.set_volume(self.volume);
            sink.set_speed(pipeline.sink_speed);
            sink.append(pipeline.clocked);
            Ok((sink, duration, position, pipeline.timing, pipeline.fade))
        };
        // A panic while opening a file must end as an error message, not leave
        // the player "busy" for good with a dead audio thread.
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(open)).unwrap_or_else(|_| {
                Err(LoadFailure::Other(format!(
                    "The audio decoder failed unexpectedly on {}.",
                    path.display()
                )))
            });
        if let Some(old) = self.sink.take() {
            old.stop();
        }
        let same_track = self
            .current
            .as_ref()
            .is_some_and(|(old_id, old_path)| *old_id == id && *old_path == path);
        self.current = Some((id.clone(), path.clone()));
        // The clock is this thread's own state: set it before the snapshot lock
        // is taken. A failed load parks the requested position.
        let result = match result {
            Ok((sink, duration, start, timing, fade)) => {
                let segment = Segment {
                    id: id.clone(),
                    path,
                    request_id,
                    start,
                    base_j: 0,
                    duration,
                };
                self.start_timeline(timing, segment);
                Ok((sink, duration, start, fade))
            }
            Err(failure) => {
                self.park(position);
                Err(failure)
            }
        };
        let mut snap = self.shared.lock().unwrap();
        complete_audio_request(&mut snap, id, request_id);
        match result {
            Ok((sink, duration, position, fade)) => {
                self.fade = Some(fade);
                if play {
                    sink.play();
                }
                self.sink = Some(sink);
                snap.position = position;
                snap.duration = duration;
                snap.playing = play;
                snap.error = None;
                snap.device = if self.device.is_empty() {
                    "System default".into()
                } else {
                    self.device.clone()
                };
            }
            Err(failure) => {
                self.fade = None;
                snap.position = position;
                snap.playing = false;
                match failure {
                    LoadFailure::Other(error) => {
                        snap.duration = 0.;
                        snap.error = Some(error);
                    }
                    // The track stays loaded and the position is kept; the app
                    // hears about it through `take_device_event`.
                    LoadFailure::Device => {
                        if !same_track {
                            snap.duration = 0.;
                        } else if snap.duration > 0. {
                            snap.position = snap.position.min(snap.duration);
                        }
                    }
                }
            }
        }
    }
    /// Close the audio file on request (the app is about to rewrite it). Like
    /// `lose_output` it pauses, saves the exact position, drops the sink and keeps
    /// the loaded track, but it keeps the output stream open, bumps no generation
    /// and raises no `DeviceEvent`: the app asked for this. Returns true once the
    /// audio thread has dropped the stopped source, which closes the file handle.
    fn release_file(&mut self, deadline: Option<Instant>) -> bool {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            // The caller already gave up and reported a failure; closing the
            // file now would surprise it.
            return false;
        }
        if self.live.is_some() {
            // A stream has no file to close.
            return true;
        }
        let position = self.settle();
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
        self.fade = None;
        self.park(position);
        if self.current.is_some() {
            self.released = true;
        }
        {
            let mut snap = self.shared.lock().unwrap();
            snap.position = if snap.duration > 0. {
                position.min(snap.duration)
            } else {
                position
            };
            snap.playing = false;
        }
        let until = |span: Duration| {
            let end = Instant::now() + span;
            deadline.map_or(end, |d| end.min(d))
        };
        if self.wait_for_files_closed(until(RELEASE_SINK_WAIT)) {
            return true;
        }
        // The audio thread is not pulling (stalled output): closing the stream
        // drops everything it still owns. Late errors from it are ignored.
        if self.stream.take().is_some() {
            self.generation = self.generation.wrapping_add(1);
        }
        self.wait_for_files_closed(until(RELEASE_STREAM_WAIT))
    }
    fn play(&mut self) {
        if self.live.is_some() {
            return self.play_live();
        }
        if let Some(s) = &self.sink {
            if !s.empty() {
                // Resume from silence; the ramp-up runs on the first samples
                // pulled after `play`.
                if let Some(fade) = &self.fade {
                    fade.set_target(1.);
                }
                s.play();
            }
        } else if let Some((id, path)) = self.current.clone() {
            let position = self.position();
            self.load(id, path, position, true, self.current_request_id);
        }
    }
    fn seek(&mut self, position: f64, request_id: u64) {
        if self.live.is_some() {
            // A live stream cannot be seeked: the request is answered and
            // nothing moves, so the app's bookkeeping still sees its id.
            self.current_request_id = request_id;
            lock_live(&self.shared).request_id = request_id;
            return;
        }
        if let Some((id, path)) = self.current.clone() {
            if self.released {
                self.seek_released(position, request_id);
            } else {
                let play = self.playing();
                self.load(id, path, position, play, request_id);
            }
        }
    }
    /// A seek while the file is closed on purpose: move the saved position only.
    /// The request is answered like a finished load so the app sees its seek land.
    fn seek_released(&mut self, position: f64, request_id: u64) {
        self.current_request_id = request_id;
        let position = if position.is_finite() {
            position.clamp(0., crate::state::MAX_POSITION_SECONDS)
        } else {
            0.
        };
        let duration = self.shared.lock().unwrap().duration;
        let position = if duration > 0. {
            position.clamp(0., (duration - 0.001).max(0.))
        } else {
            position
        };
        self.park(position);
        let mut snap = self.shared.lock().unwrap();
        snap.position = position;
        snap.request_id = request_id;
        snap.busy = false;
        snap.ended = false;
        snap.playing = false;
    }
    fn reload(&mut self) {
        if self.live.is_some() {
            // Speed and pitch do not apply to a stream; they are kept for the
            // next file.
            return;
        }
        if self.released {
            // Speed and pitch are stored; the next reopen uses them.
            return;
        }
        if self.current.is_some() {
            let play = self.playing();
            // Pause, count a join inside the fade-out, then read the position; the
            // track to reopen is the one playing after that.
            let position = self.settle();
            if let Some((id, path)) = self.current.clone() {
                self.load(id, path, position, play, self.current_request_id);
            }
        }
    }
    fn run(mut self, rx: mpsc::Receiver<Command>) {
        let mut deferred = None;
        loop {
            let message = match next_audio_command(&rx, &mut deferred) {
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => break,
            };
            let idle = message.is_none();
            if let Some(command) = message {
                if !self.apply(command) {
                    break;
                }
            }
            self.tick(idle);
        }
    }
    /// Carries out one command; false when the engine thread is to end.
    fn apply(&mut self, command: Command) -> bool {
        // A boundary the audio thread passed since the last turn counts before
        // any command looks at what is loaded.
        self.cross_boundaries();
        match command {
            Command::Shutdown => return false,
            Command::PrepareExit(reply) => {
                let was_playing = self.playing();
                let position = self.settle();
                if let Some(s) = self.sink.take() {
                    s.stop();
                }
                self.park(position);
                self.stop_live();
                self.stream = None;
                let mut snapshot = self.shared.lock().unwrap();
                snapshot.position = position;
                snapshot.playing = was_playing;
                snapshot.busy = false;
                let _ = reply.send(snapshot.clone());
                return false;
            }
            Command::Load {
                id,
                path,
                position,
                play,
                speed,
                request_id,
            } => {
                self.speed = speed.clamp(0.25, 3.);
                // A new load starts the removed-silence total again; seeks and
                // rebuilds of the same load keep adding to it.
                self.collect_skipped();
                self.skipped_total = 0.;
                self.load(id, path, position, play, request_id);
            }
            Command::LoadStream {
                id,
                url,
                request_id,
            } => self.load_stream(id, url, request_id),
            Command::Seek {
                position,
                request_id,
                target,
            } => {
                match target {
                    Some(SeekTarget::Current(track)) => {
                        if self.loaded_id() == Some(track.as_str()) {
                            self.seek(position, request_id);
                        }
                    }
                    Some(SeekTarget::Track {
                        id,
                        path,
                        speed,
                        gain_db,
                    }) => {
                        let current = self.current.as_ref();
                        if self.released
                            && current.is_some_and(|(loaded, _)| loaded == &id)
                        {
                            // Tag writing deliberately closed this file. Move
                            // only its parked position; never reopen it here.
                            self.seek_released(position, request_id);
                        } else {
                            let play = self.playing();
                            self.speed = speed.clamp(0.25, 3.);
                            self.load_with_gain(
                                id,
                                path,
                                position,
                                play,
                                request_id,
                                Some(gain_db),
                            );
                        }
                    }
                    None => self.seek(position, request_id),
                }
            }
            Command::Play => self.play(),
            Command::Preload(request) => self.preload(request),
            Command::CancelPreload => self.cancel_preload(),
            Command::ReleaseFile { reply, deadline } => {
                let released = self.release_file(deadline);
                let _ = reply.send(released);
            }
            Command::Pause | Command::Stop => self.pause_command(),
            Command::PauseIfRequest(request_id) => {
                if self.current_request_id == request_id {
                    self.pause_command();
                }
            }
            Command::Volume(v) => {
                self.volume = v.clamp(0., 1.);
                if let Some(s) = &self.sink {
                    s.set_volume(self.volume);
                }
            }
            Command::Speed(v) => {
                self.speed = v.clamp(0.25, 3.);
                self.reload();
            }
            Command::Pitch(value) => {
                self.preserve_pitch = value;
                self.reload();
            }
            Command::Device(name) => {
                self.device = name;
                self.switch_output();
            }
        }
        true
    }
    /// What the engine thread does after every command (or idle wait): react to
    /// output failures, follow the default device, service a live stream, and
    /// publish the snapshot.
    fn tick(&mut self, idle: bool) {
        self.drain_retired();
        while let Ok((generation, note)) = self.error_rx.try_recv() {
            if generation != self.generation {
                continue;
            }
            self.lose_output(note);
        }
        if idle {
            self.follow_default_device();
        }
        self.service_live();
        self.service_preload();
        self.cross_boundaries();
        self.publish_skipped();
        let position = self.position();
        if let Some(s) = &self.sink {
            let mut snap = self.shared.lock().unwrap();
            snap.position = position;
            if self.live.is_some() {
                // A stream has no duration and no end: when it stops for good
                // `service_live` reports an error instead.
                snap.playing = !s.is_paused() && !s.empty();
                snap.ended = false;
            } else {
                if snap.duration > 0. {
                    snap.position = snap.position.min(snap.duration);
                }
                snap.playing = !s.is_paused() && !s.empty();
                snap.ended = s.empty();
                if snap.ended && ended_early(snap.position, snap.duration) && snap.error.is_none() {
                    snap.error=Some("Audio decoding ended before the end of this file. Progress is retained; check whether the file is damaged.".into());
                }
            }
        }
    }
}

/// Live streams on the engine thread. A station replaces `current` (a file) as the
/// loaded item; see `AudioEngine::load_stream` for the behaviour the app sees.
impl Worker {
    fn live_state(&self) -> Option<LiveState> {
        self.live.as_ref().map(|live| live.state)
    }
    /// Stops the stream session, if any: the connection closes, the sample queue
    /// is shut and the helper thread is handed to `reap_retired`. Never blocks.
    fn stop_live(&mut self) {
        if let Some(live) = self.live.as_mut()
            && let Some(thread) = live.stop()
        {
            self.retired.push(thread);
            while self.retired.len() > LIVE_RETIRED_MAX {
                self.retired.remove(0);
            }
        }
        *lock_live(&self.live_handle) = None;
        self.live_restored = None;
    }
    /// A file takes over: forget the station altogether.
    fn drop_live(&mut self) {
        self.stop_live();
        self.live = None;
    }
    /// Joins the helper threads that have finished since the last call.
    fn reap_retired(&mut self) {
        for thread in std::mem::take(&mut self.retired) {
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                self.retired.push(thread);
            }
        }
    }
    /// Publishes a station that is not connected: not playing, position kept, the
    /// request answered, with `error` when the connection failed.
    fn settle_parked(&mut self, error: Option<String>) {
        let Some(id) = self.live.as_ref().map(|live| live.id.clone()) else {
            return;
        };
        let position = self.position();
        let mut snap = lock_live(&self.shared);
        complete_audio_request(&mut snap, id, self.current_request_id);
        snap.position = position;
        snap.duration = 0.;
        snap.playing = false;
        snap.error = error;
    }
    /// Closes the stream at once without touching sink, fade or position (the
    /// caller does that); a connect in progress is cancelled and answered.
    fn park_live_now(&mut self) {
        let opening = self.live_state() == Some(LiveState::Opening);
        self.stop_live();
        if opening {
            self.settle_parked(None);
        }
    }
    fn load_stream(&mut self, id: String, url: String, request_id: u64) {
        self.current_request_id = request_id;
        self.released = false;
        // Fade out what plays (a file or the previous station), then drop it.
        self.pause();
        if let Some(old) = self.sink.take() {
            old.stop();
        }
        self.stop_live();
        self.fade = None;
        self.current = None;
        self.park(0.);
        self.live = Some(LiveSession::new(id, url));
        self.begin_stream_open();
    }
    /// Starts connecting the loaded station on a helper thread.
    fn begin_stream_open(&mut self) {
        {
            let mut snap = lock_live(&self.shared);
            begin_audio_request(&mut snap);
        }
        if self.silent {
            self.settle_parked(Some(
                "Audio output is disabled for this silent session.".into(),
            ));
            return;
        }
        self.live_serial = self.live_serial.wrapping_add(1);
        let serial = self.live_serial;
        let cancel = Arc::new(AtomicBool::new(false));
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let job = StreamJob {
            url: live.url.clone(),
            serial,
            cancel: cancel.clone(),
            events: self.live_tx.clone(),
            _token: SourceToken::new(&self.live_threads),
        };
        match thread::Builder::new()
            .name("bs-stream-decode".into())
            .spawn(move || run_stream_job(job))
        {
            Ok(thread) => {
                live.state = LiveState::Opening;
                live.serial = serial;
                live.cancel = cancel;
                live.queue = None;
                live.handle = None;
                live.thread = Some(thread);
            }
            Err(_) => self.settle_parked(Some(MSG_STREAM_THREAD.into())),
        }
    }
    /// Pause and Stop: a station fades out and drops its connection; Play connects
    /// again from live.
    fn pause_command(&mut self) {
        if self.live.is_some() {
            self.park_stream();
        } else {
            self.pause();
        }
    }
    fn park_stream(&mut self) {
        if self.live_state() == Some(LiveState::Parked) {
            return;
        }
        self.pause();
        let position = self.position();
        if let Some(s) = self.sink.take() {
            s.stop();
        }
        self.fade = None;
        self.stop_live();
        self.park(position);
        self.settle_parked(None);
    }
    fn play_live(&mut self) {
        match self.live_state() {
            Some(LiveState::Running) => {
                if let Some(s) = &self.sink
                    && !s.empty()
                {
                    if let Some(fade) = &self.fade {
                        fade.set_target(1.);
                    }
                    s.play();
                }
            }
            Some(LiveState::Parked) => self.begin_stream_open(),
            // Already connecting.
            Some(LiveState::Opening) | None => {}
        }
    }
    /// The output changed: a playing station reconnects on the new device (live,
    /// there is nothing to resume); a connecting one just uses it when it opens.
    fn switch_output_live(&mut self) {
        match self.live_state() {
            Some(LiveState::Running) => {
                let active = self.sink.as_ref().is_some_and(|s| !s.empty());
                let position = self.position();
                self.pause();
                if let Some(s) = self.sink.take() {
                    s.stop();
                }
                self.fade = None;
                self.stop_live();
                self.stream = None;
                self.park(position);
                if active {
                    self.begin_stream_open();
                } else {
                    self.settle_parked(None);
                }
            }
            _ => self.stream = None,
        }
    }
    fn on_live_event(&mut self, event: LiveEvent) {
        match event {
            LiveEvent::Opened {
                serial,
                handle,
                queue,
                channels,
                sample_rate,
            } => self.on_stream_opened(serial, handle, queue, channels, sample_rate),
            LiveEvent::Failed { serial, message } => {
                let current = self
                    .live
                    .as_ref()
                    .is_some_and(|l| l.state == LiveState::Opening && l.serial == serial);
                if current {
                    self.stop_live();
                    self.settle_parked(Some(message));
                }
            }
        }
    }
    fn on_stream_opened(
        &mut self,
        serial: u64,
        handle: StreamHandle,
        queue: Arc<LiveQueue>,
        channels: u16,
        sample_rate: u32,
    ) {
        let current = self
            .live
            .as_ref()
            .is_some_and(|l| l.state == LiveState::Opening && l.serial == serial);
        if !current {
            // An attempt the user already moved on from.
            handle.close();
            queue.shutdown();
            return;
        }
        if let Err(failure) = self.attach_stream(&handle, &queue, channels, sample_rate) {
            handle.close();
            queue.shutdown();
            self.stop_live();
            // A device problem was reported as a device event already.
            let error = match failure {
                LoadFailure::Device => None,
                LoadFailure::Other(message) => Some(message),
            };
            self.settle_parked(error);
        }
    }
    /// Builds the normal chain on the sample queue and starts playing it. Speed is
    /// 1 and there is no pitch stage, whatever the file settings are.
    fn attach_stream(
        &mut self,
        handle: &StreamHandle,
        queue: &Arc<LiveQueue>,
        channels: u16,
        sample_rate: u32,
    ) -> Result<(), LoadFailure> {
        if !live_format_supported(channels, sample_rate) {
            return Err(MSG_STREAM_FORMAT.into());
        }
        self.ensure_output(true)?;
        let source = LiveSource::new(queue.clone(), channels, sample_rate);
        // A live stream is never shortened: cutting would only add delay.
        let wiring = Wiring {
            skip: None,
            ..self.wiring()
        };
        let pipeline = build_pipeline(
            Box::new(source),
            &self.effects,
            false,
            1.,
            &self.open_sources,
            &wiring,
        );
        let sink = self.new_sink()?;
        sink.pause();
        sink.set_volume(self.volume);
        sink.set_speed(pipeline.sink_speed);
        sink.append(pipeline.clocked);
        sink.play();
        self.fade = Some(pipeline.fade);
        self.sink = Some(sink);
        let Some(live) = self.live.as_mut() else {
            return Ok(());
        };
        live.state = LiveState::Running;
        live.queue = Some(queue.clone());
        live.handle = Some(handle.clone());
        let id = live.id.clone();
        // A station is one endless segment; its clock starts at the listening
        // time before this connection.
        let segment = Segment {
            id: id.clone(),
            path: PathBuf::new(),
            request_id: self.current_request_id,
            start: self.parked,
            base_j: 0,
            duration: 0.,
        };
        self.start_timeline(pipeline.timing, segment);
        *lock_live(&self.live_handle) = Some(handle.clone());
        if let Some(name) = self.live_restored.take() {
            self.push_event(Some(DeviceEvent::Restored(name)));
        }
        let mut snap = lock_live(&self.shared);
        complete_audio_request(&mut snap, id, self.current_request_id);
        snap.position = self.parked;
        snap.duration = 0.;
        snap.playing = true;
        snap.error = None;
        snap.device = if self.device.is_empty() {
            "System default".into()
        } else {
            self.device.clone()
        };
        Ok(())
    }
    /// Collects helper-thread news and notices a stream that ended for good: once
    /// its queue is finished and played out, the reason becomes the snapshot error
    /// (`ended` is never used for a stream) and Play connects again.
    fn service_live(&mut self) {
        self.reap_retired();
        while let Ok(event) = self.live_rx.try_recv() {
            self.on_live_event(event);
        }
        let over = match (&self.live, &self.sink) {
            (Some(live), Some(sink)) if live.state == LiveState::Running => {
                live.queue.as_ref().is_some_and(|q| q.state().finished) && sink.empty()
            }
            _ => false,
        };
        if over {
            self.end_live_stream();
        }
    }
    fn end_live_stream(&mut self) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let from_queue = live.queue.as_ref().and_then(|q| q.state().error);
        let from_stream = live.handle.as_ref().and_then(|h| match h.status() {
            StreamStatus::Failed(message) => Some(message),
            _ => None,
        });
        let message = from_queue
            .or(from_stream)
            .unwrap_or_else(|| MSG_STREAM_ENDED.to_owned());
        let position = self.position();
        if let Some(s) = self.sink.take() {
            s.stop();
        }
        self.fade = None;
        self.stop_live();
        self.park(position);
        self.settle_parked(Some(message));
    }
}
