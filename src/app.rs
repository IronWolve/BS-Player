use crate::{
    appearance,
    chapters::{self, Chapter},
    gui_cache::{
        BookRow, BrowseGroup, BrowseKind, BrowseSection, BrowseSort, GuiCache, LibraryCatalog,
        ThumbCache, ThumbKey, ThumbQueue, ThumbState, VisibleView, art_seed, book_rows,
        browse_section, browse_sort_of,
        columns::{self, COLUMNS, COUNT as COLUMN_COUNT},
        group_section, use_author_groups, with_browse_sort,
    },
    instance, lyrics,
    media::{self, Track},
    playback::{
        AudioEngine, DeviceEvent, PreloadRequest, PreloadState, PreloadStatus, Snapshot,
        TrackChange,
    },
    settings::{self, OptionsDialog, Settings, SettingsAction},
    state::{self, BookProgress, BookState, Bookmark, Playlist, SelectionSet, Session},
    stream::{self, StreamStatus},
    tagwrite,
    widgets::{self, Icon},
    winintegration::{self, AssociationState, MediaKey},
};

use eframe::egui::{self, RichText, Vec2};



use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsString,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, Instant},
};

mod actions;
mod artwork;
mod backup;
mod bookmarks;
mod books;
mod browse;
mod chapter_picker;
mod chapter_view;
mod content;
mod dialogs;
mod events;
mod layout;
mod library;
mod lyric_view;
mod menus;
mod now_playing;
mod playing;
mod preload;
mod selection;
mod shortcuts;
mod stations;
mod tag_editor;
mod tick;
mod track_play;
mod window;

use self::actions::*;
use self::artwork::*;
use self::bookmarks::*;
use self::books::*;
use self::browse::*;
use self::chapter_picker::ChapterPicker;
use self::chapter_view::*;
use self::content::*;
use self::events::*;
use self::layout::*;
use self::library::*;
use self::lyric_view::*;
use self::menus::*;
pub use self::now_playing::time_text;

use self::preload::*;
use self::shortcuts::*;
use self::stations::*;
use self::tag_editor::*;
use self::tick::*;
use self::track_play::*;


enum Event {
    PlayingImported {
        request: u64,
        found: media::PathImport,
        aliases: HashMap<PathBuf, PathBuf>,
    },
    Imported(
        Vec<Track>,
        Vec<String>,
        bool,
        HashMap<PathBuf, PathBuf>,
        bool,
        bool,
    ),
    Paths(Vec<PathBuf>, bool),
    PlayingPaths(Vec<PathBuf>),
    /// One part of "Re-read tags of all files": the records read again, the
    /// files that could not be read, how many files are done of the total, and
    /// whether this is the last part.
    Reread {
        tracks: Vec<Track>,
        errors: Vec<String>,
        aliases: Arc<HashMap<PathBuf, PathBuf>>,
        done: usize,
        total: usize,
        last: bool,
        /// Stopped on request: this is the last part and holds what was read.
        cancelled: bool,
    },
    /// How far a scan or a folder move has come (sent without a repaint; the
    /// 200 ms tick of a busy player shows it).
    ScanProgress {
        kind: ScanKind,
        progress: media::ScanProgress,
    },
    /// An import job has ended; sent just before its `Imported`. `added` files
    /// were read of the `found` files it came across.
    ScanDone {
        added: usize,
        found: usize,
        cancelled: bool,
    },
    /// The folder picked as the new place of a watched folder.
    MoveFolderChosen {
        old: PathBuf,
        new: PathBuf,
    },
    /// A folder move has ended (or could not start).
    FolderMoved(Box<FolderMove>),
    /// A listening progress file was read (the file picker's worker).
    ProgressFileRead(Result<state::ProgressFile, String>),
    /// The safety copy ahead of a progress import is written (or failed).
    ProgressImportReady {
        file: Box<state::ProgressFile>,
        safety: Result<(), String>,
    },
    /// Cover of the track the art pane follows, with where it came from.
    Artwork(String, Result<(egui::ColorImage, String), String>),
    /// Cover of the track shown in File details.
    DetailsArt(String, Result<(egui::ColorImage, String), String>),
    /// A row thumbnail (`None`: no readable artwork).
    Thumb {
        key: ThumbKey,
        embedded: bool,
        image: Option<egui::ColorImage>,
    },
    Backup(Result<Session, String>),
    /// The pre-restore safety copy was written off the UI thread.
    RestorePrepared {
        session: Box<Session>,
        result: Result<(), String>,
    },
    Message(String),
    ValidatedTrack {
        request: u64,
        result: Result<Track, String>,
        play: bool,
    },
    /// The file a gapless preload would open ahead of time was checked off the
    /// UI thread (see `PlayerApp::start_preload`).
    PreloadValidated {
        request: u64,
        result: Result<Track, String>,
    },
    Relocated {
        original_id: String,
        result: Result<(Track, bool), String>,
    },
    Devices {
        request: u64,
        result: Result<Vec<String>, String>,
    },
    /// The Edit file tags window's file, read off the UI thread.
    TagEditorLoaded {
        session: u64,
        result: Result<TagEditorData, String>,
    },
    /// A cover image the user picked, decoded for the preview.
    TagArtworkChosen {
        session: u64,
        result: Result<ChosenArtwork, String>,
    },
    /// One file of a tag save is done: its refreshed record, or why not.
    TagFileSaved {
        session: u64,
        result: Result<Track, String>,
        done: usize,
        total: usize,
    },
    /// The tag save has ended.
    TagSaveFinished {
        session: u64,
        summary: TagSummary,
    },
    /// A file-association request (Register or Unregister) has ended; `state` is
    /// what the registry says now.
    Associations {
        result: Result<(), String>,
        state: AssociationState,
    },
    /// The chapters of one file version, read off the UI thread.
    Chapters {
        key: ChapterKey,
        result: Result<Vec<Chapter>, String>,
    },
    /// The lyrics of one file version, read off the UI thread.
    Lyrics {
        key: LyricsKey,
        result: Result<Option<lyrics::Lyrics>, String>,
    },
    /// The system fonts read for the scripts the library needs (possibly none).
    FallbackFonts(Vec<appearance::LoadedFont>),
    /// The `http(s)` entries of the playlists an import read, to become stations.
    Stations(Vec<media::PlaylistStation>),
    /// The answer of the Test button of the station window.
    StationTest {
        serial: u64,
        result: Result<TestedStation, String>,
    },
}

/// A click on a row of a browse list that waits to see whether a second click
/// makes it a double-click (Play) before it opens the group.
struct BrowsePending {
    kind: BrowseKind,
    key: String,
    /// The input time of the click, in seconds.
    at: f64,
}

/// How long a click on a browse row waits for a second click before it opens
/// the group: a little over the toolkit's double-click interval (0.3 s). Only
/// used while a double-click plays; without that, a click opens at once.
const BROWSE_OPEN_DELAY: f64 = 0.32;

/// Reads the system fonts for some scripts (see `appearance::load_system_fonts`).
type FontLoader = dyn Fn(appearance::ScriptSet) -> Vec<appearance::LoadedFont> + Send + Sync;

struct JobSender {
    generation: u64,
    tx: Sender<(u64, Event)>,
    repaint: egui::Context,
}

impl JobSender {
    fn send(&self, event: Event) {
        if self.tx.send((self.generation, event)).is_ok() {
            self.repaint.request_repaint_of(egui::ViewportId::ROOT);
        }
    }
    /// Sends without asking for a repaint: for progress reports, which the tick
    /// of a busy player (every 200 ms) shows, so they cost no extra frames.
    fn send_quiet(&self, event: Event) {
        let _ = self.tx.send((self.generation, event));
    }
}

/// How the jobs of one `import_jobs` call are run.
struct ImportMode {
    append: bool,
    incremental: bool,
    quiet: bool,
    recursive: bool,
    excluded: Vec<String>,
}

/// What a `ScanProgress` report is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanKind {
    /// Importing, scanning or re-reading files: `Scanning… N files found`,
    /// `Reading tags X of Y`.
    Import,
    /// Looking for the files of a moved folder and checking them.
    Move,
}

/// What the worker of a "Move folder" job starts from.
struct MoveJob {
    wants: Vec<media::RelocateWant>,
    /// The form of the old folder the library's paths are below.
    effective_old: PathBuf,
    /// The profile, for the safety copy.
    session: Session,
    copy: PathBuf,
    stored_root: PathBuf,
    new_root: PathBuf,
}

/// The outcome of a "Move folder" job.
struct FolderMove {
    /// The watched folder as the profile lists it.
    stored_root: PathBuf,
    new_root: PathBuf,
    result: Result<media::RelocateReport, String>,
}

/// Changes to the status text no more often than this while a scan runs.
const SCAN_TEXT_INTERVAL: Duration = Duration::from_millis(200);

/// What the status bar and Cancel know about the scans, re-reads and folder
/// moves that run now.
#[derive(Default)]
struct ScanState {
    /// One flag per worker that was started; Cancel sets all of them.
    flags: Vec<Arc<AtomicBool>>,
    /// Jobs whose end has not been seen yet.
    jobs: usize,
    cancel_requested: bool,
    /// Files read and files found by the import jobs that ended meanwhile.
    added: usize,
    found: usize,
    /// One of those jobs was stopped by Cancel.
    cancelled: bool,
    /// The progress text last shown and when it last changed.
    text: String,
    changed_at: Option<Instant>,
    phase: Option<media::ScanPhase>,
}

impl ScanState {
    /// Starts `jobs` jobs on one worker; the flag is what Cancel sets.
    fn begin(&mut self, jobs: usize) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        if self.jobs == 0 {
            self.flags.clear();
        }
        // The new worker has not been told to stop.
        self.cancel_requested = false;
        self.flags.push(Arc::clone(&flag));
        self.jobs += jobs;
        flag
    }
    /// One job has ended; true when it was the last one.
    fn job_ended(&mut self) -> bool {
        self.jobs = self.jobs.saturating_sub(1);
        self.jobs == 0
    }
    fn running(&self) -> bool {
        self.jobs > 0
    }
    /// Asks every worker to stop; false when nothing runs or Cancel was already pressed.
    fn request_cancel(&mut self) -> bool {
        if self.jobs == 0 || self.cancel_requested {
            return false;
        }
        self.cancel_requested = true;
        for flag in &self.flags {
            flag.store(true, Ordering::SeqCst);
        }
        true
    }
    /// Forgets everything after the last job (or a restored profile, whose
    /// running workers are told to stop).
    fn reset(&mut self) {
        for flag in &self.flags {
            flag.store(true, Ordering::SeqCst);
        }
        *self = Self::default();
    }
    /// Clears the counters of a finished run (the flags stay until the next run).
    fn finish_run(&mut self) {
        self.added = 0;
        self.found = 0;
        self.cancelled = false;
        self.text.clear();
        self.changed_at = None;
        self.phase = None;
    }
}

/// "1,240": a count with thousands separators.
fn count_text(count: usize) -> String {
    let digits = count.to_string();
    let mut text = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

/// The status bar text for a progress report.
fn scan_progress_text(kind: ScanKind, progress: &media::ScanProgress) -> String {
    let name = if progress.current.is_empty() {
        String::new()
    } else {
        format!(" · {}", progress.current)
    };
    match (kind, progress.phase) {
        (ScanKind::Import, media::ScanPhase::Collecting) => {
            format!("Scanning… {} files found", count_text(progress.found))
        }
        (ScanKind::Import, media::ScanPhase::Reading) => format!(
            "Reading tags {} of {}{name}",
            count_text(progress.done),
            count_text(progress.total)
        ),
        (ScanKind::Move, media::ScanPhase::Collecting) => format!(
            "Looking through the new folder… {} files found",
            count_text(progress.found)
        ),
        (ScanKind::Move, media::ScanPhase::Reading) => format!(
            "Checking files {} of {}{name}",
            count_text(progress.done),
            count_text(progress.total)
        ),
    }
}

/// Held by the thread of a native file dialog; at most one dialog is open.
struct PickerGuard(Arc<AtomicBool>);

impl Drop for PickerGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[derive(Clone)]
enum Confirm {
    Remove(String),
    /// Several selected files leave the library (the ids, in displayed order).
    RemoveMany(Vec<String>),
    ClearHistory,
    ResetProgress,
    Restore(Session),
    Relink {
        original_id: String,
        track: Track,
    },
    ClearQueue,
    RemoveBookmark(Bookmark),
    ForgetRemovals,
    /// Move the history of an earlier version of a file to its current version.
    CarryProgress {
        old_id: String,
        new_id: String,
    },
    /// Put every file of a book back to the start (`key` is the book key).
    BookNotStarted {
        key: String,
        label: String,
    },
    /// Read the tags of every library file again.
    RereadTags,
    /// Take a station out of the Streams list (the station's id).
    RemoveStation(String),
}

/// The running "Re-read tags of all files" job, for its status line and result.
#[derive(Debug, Default)]
struct RereadRun {
    total: usize,
    done: usize,
    failed: usize,
    /// The first few files that could not be read, for the result message.
    first_errors: Vec<String>,
}

#[derive(Clone, Debug)]
struct BookmarkDraft {
    track_id: String,
    title: String,
    seconds: f64,
}

/// A bookmark being renamed. `original` finds it again by value when saved, as
/// the list may have changed while the dialog was open.
#[derive(Clone, Debug)]
struct BookmarkEdit {
    original: Bookmark,
    title: String,
    name: String,
    note: String,
}

/// What the playlist name window is for.
#[derive(Clone, Debug, PartialEq)]
enum PlaylistDialogKind {
    /// "Save queue as playlist…": the queue becomes a new playlist.
    FromQueue,
    /// A new playlist that starts with these files (maybe none).
    New { tracks: Vec<String> },
    /// The playlist called `from` gets a new name.
    Rename { from: String },
}

/// The open playlist name window.
#[derive(Clone, Debug)]
struct PlaylistDialog {
    kind: PlaylistDialogKind,
    name: String,
    /// The name field takes the keyboard focus (and selects its text) on the
    /// first frame.
    fresh: bool,
}

/// What the station window is for.
#[derive(Clone, Debug, PartialEq)]
enum StationDialogKind {
    /// A new station.
    Add,
    /// Change the name and the address of this station (its id).
    Edit(String),
    /// Change only the name of this station (its id).
    Rename(String),
}

/// What a successful Test of a stream address learned.
#[derive(Clone, Debug, PartialEq)]
struct TestedStation {
    name: String,
    bitrate: Option<u32>,
    content_type: String,
}

impl TestedStation {
    /// The result line of the station window.
    fn line(&self, host: &str) -> String {
        let mut parts = vec![format!(
            "Station: {}",
            if self.name.is_empty() {
                host
            } else {
                &self.name
            }
        )];
        if let Some(kbps) = self.bitrate {
            parts.push(format!("{kbps} kbps"));
        }
        if !self.content_type.is_empty() {
            parts.push(self.content_type.clone());
        }
        parts.join(" · ")
    }
}

/// A Test of the address in the station window. Dropping it (the window closes,
/// or another test starts) cancels the connection attempt.
struct StationTest {
    serial: u64,
    /// The address that was tested.
    url: String,
    cancel: Arc<AtomicBool>,
    /// `None` while the test runs.
    outcome: Option<Result<TestedStation, String>>,
}

impl Drop for StationTest {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

/// The open station window.
struct StationDialog {
    kind: StationDialogKind,
    name: String,
    url: String,
    /// The first field takes the keyboard focus on the first frame.
    fresh: bool,
    test: Option<StationTest>,
    /// Why the last Add or Save was refused.
    problem: Option<&'static str>,
}

/// What the player learned from the connections of one station (memory only;
/// the profile keeps just the list and the last title).
#[derive(Clone, Debug, Default, PartialEq)]
struct StationLive {
    /// The name the station announced.
    name: String,
    bitrate: Option<u32>,
    content_type: String,
    /// Why the last attempt to connect failed (a short sentence).
    error: Option<String>,
}

#[derive(Clone)]
enum Action {
    PlayWorkspace(Vec<String>),
    Play(String),
    Queue(String, bool),
    Favorite(String),
    Details(String),
    Remove(String),
    /// Take a file out of the queue.
    RemoveFromQueue(String),
    /// Add a file to the playlist with this name.
    AddToPlaylist(String, String),
    /// Take a file out of the playlist with this name.
    RemoveFromPlaylist(String, String),
    /// Drag and drop in a playlist: playlist, dragged file, file it was dropped on.
    MovePlaylist(String, String, String),
    /// "Move up" / "Move down" in a playlist: playlist, file, whether it goes up.
    StepPlaylist(String, String, bool),
    Relocate(String),
    MoveQueue(String, String),
    Jump(String, f64),
    RemoveBookmark(usize),
    EditBookmark(usize),
    /// A click on a chapter of the Chapters list: the file and the chapter start.
    SeekChapter(String, f64),
    /// A click on a line of the Lyrics list: the file and the line's time.
    SeekLine(String, f64),
    /// "Add bookmark at chapter start": the file, the start and the chapter's name.
    ChapterBookmark(String, f64, String),
    Select(String),
    /// A click on a row with Ctrl/Cmd or Shift held.
    SelectWith(String, SelectMode),
    /// A right-click on a row inside the selection: it becomes the primary row
    /// and the selection stays.
    FocusInSelection(String),
    /// A bulk command of the row menu on these files (displayed order).
    Bulk(BulkAction, Vec<String>),
    /// Play on a row of the Artists, Albums or Genres list: the group's files
    /// become the queue, in drill-down order, and the first plays.
    PlayGroup(BrowseKind, String),
    /// Open the drill-down of a group (by its key).
    OpenGroup(BrowseKind, String),
}

/// How a click on a row changes the selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectMode {
    /// Only this row.
    Plain,
    /// Ctrl/Cmd: the row joins or leaves the selection.
    Toggle,
    /// Shift: the rows from the anchor to this row.
    Range,
    /// Ctrl+Shift: the same rows join the selection.
    RangeAdd,
}

/// What a click with these modifiers does to the selection.
fn select_mode(modifiers: egui::Modifiers) -> SelectMode {
    match (modifiers.command, modifiers.shift) {
        (true, true) => SelectMode::RangeAdd,
        (false, true) => SelectMode::Range,
        (true, false) => SelectMode::Toggle,
        (false, false) => SelectMode::Plain,
    }
}

/// A command of the row menu for several selected rows.
#[derive(Clone, Debug, PartialEq)]
enum BulkAction {
    /// The files become the queue (outside the Queue view) and the first plays.
    Play,
    PlayNext,
    AddToQueue,
    AddToPlaylist(String),
    /// Add them all to the favorites, or remove them when all are favorites.
    ToggleFavorite,
    Rate(u8),
    MarkFinished,
    MarkNotStarted,
    RemoveFromQueue,
    RemoveFromPlaylist(String),
    RemoveFromLibrary,
}

fn load_about_logo(ctx: &egui::Context) -> Result<egui::TextureHandle, String> {
    let decoded = image::load_from_memory(include_bytes!("../assets/bs-player-logo.png"))
        .map_err(|error| error.to_string())?;
    let rgba = decoded.to_rgba8();
    let [width, height] = [rgba.width() as usize, rgba.height() as usize];
    let color_image = egui::ColorImage::from_rgba_unmultiplied([width, height], rgba.as_raw());
    Ok(ctx.load_texture(
        "bs-player-about-logo",
        color_image,
        egui::TextureOptions::LINEAR,
    ))
}

enum TrayPopupEvent {
    Action(crate::tray_popup::TrayAction),
    Dismiss,
}

#[derive(Clone, Copy)]
struct TrayMenuAnchor {
    position: egui::Pos2,
    bounds: Option<egui::Rect>,
    physical_pixels: bool,
}

struct TrayPopupShared {
    anchor: Option<TrayMenuAnchor>,
    snapshot: crate::tray_popup::TrayPopupState,
    seen_focus: bool,
    /// The last `OuterPosition` sent for this opening, so it is sent once.
    placed: Option<egui::Pos2>,
}

#[derive(Default)]
struct TrayRestoreGuard {
    awaiting_window_ack: bool,
}

impl TrayRestoreGuard {
    fn requested(&mut self) {
        self.awaiting_window_ack = true;
    }
    fn observe(&mut self, minimized: Option<bool>) {
        if minimized == Some(false) {
            self.awaiting_window_ack = false;
        }
    }
    fn should_hide(&self, minimized: bool, quitting: bool) -> bool {
        minimized && !self.awaiting_window_ack && !quitting
    }
}

pub struct PlayerApp {
    profile: PathBuf,
    persistence: crate::persistence::Persistence,
    pub state: Session,
    audio: AudioEngine,
    snapshot: Snapshot,
    options: Option<OptionsDialog>,
    options_center_pending: bool,
    equalizer: Option<crate::eq::EqualizerWindow>,
    equalizer_center_pending: bool,
    equalizer_dirty: bool,
    devices: Vec<String>,
    device_request: u64,
    tx: Sender<(u64, Event)>,
    rx: Receiver<(u64, Event)>,
    deferred_events: Vec<(u64, Event)>,
    restore_windows_hidden: bool,
    gui_context: egui::Context,
    session_generation: u64,
    load_request: u64,
    expected_audio_request: Option<u64>,
    loading_request: Option<u64>,
    pending_play_intent: Option<bool>,
    list_focus: Option<egui::Id>,
    /// Esc in the search box asks the next list draw to focus the selected row.
    refocus_list: bool,
    /// The rows selected in the file list; `state.selected` is its primary row.
    /// In memory only.
    selection: SelectionSet,
    /// The visible-list build the selection was last checked against.
    selection_view: u64,
    /// The section and tab that build showed.
    selection_scope: (String, String),
    /// The selected rows in displayed order, made for (selection revision,
    /// list build).
    selected_cache: Option<((u64, u64), Arc<Vec<String>>)>,
    /// How many rows are selected and how long they play, for the same key.
    selection_summary: Option<((u64, u64), usize, f64)>,
    picker_open: Arc<AtomicBool>,
    status: String,
    status_at: Instant,
    error: Option<String>,
    busy: usize,
    restore_pending: bool,
    playing_request: u64,
    playing_cancel: Option<Arc<AtomicBool>>,
    last_save: Instant,
    last_scan: Instant,
    started: Instant,
    exit_after: Option<f64>,
    silent: bool,
    initial_restore: bool,
    suppress_autoplay: bool,
    ended_handled: bool,
    /// The shuffle pick `plan_auto_next` made for the automatic next step, kept
    /// until something it depends on changes (see `Planned`).
    planned_next: Option<Planned>,
    /// The gapless preload asked of the engine (or whose file is being checked),
    /// kept while it is still what `preload_wish` says.
    preload: Option<Preload>,
    /// When `preload_wish` was last looked at; it is looked at most every
    /// `PRELOAD_CHECK_EVERY`.
    preload_checked: Instant,
    /// Silence the engine has cut since the last load, in seconds (a tooltip line).
    skipped_seconds: f64,

    artwork: Option<(String, egui::TextureHandle)>,
    /// Where the art pane's cover came from, or why there is none (by track id).
    art_note: Option<(String, ArtNote)>,
    details_art: Option<DetailsArt>,
    thumbs: ThumbLoader,
    art_requested: Option<String>,
    art_embedded: Option<bool>,
    art_debounce: ArtDebounce,
    expanded_art: bool,
    details: Option<Track>,
    /// The Edit file tags window.
    tag_editor: Option<TagEditor>,
    /// Numbers each editor, so answers for an earlier one are ignored.
    tag_session: u64,
    /// A tag save is writing files; only one runs at a time.
    tag_save_running: bool,
    /// The loaded track the running tag save made the engine let go of.
    tag_resume: Option<TagResume>,
    bookmark_dialog: bool,
    bookmark_target: Option<BookmarkDraft>,
    bookmark_name: String,
    bookmark_note: String,
    bookmark_edit: Option<BookmarkEdit>,
    /// Seconds since the epoch at which the loaded track went from playing to
    /// paused; smart rewind reads it when playback continues. A load or a seek
    /// clears it.
    paused_at: Option<u64>,
    playing_last_tick: bool,
    /// Chapters of the current file (empty: it has none, or they are not read yet).
    current_chapters: Arc<Vec<Chapter>>,
    /// The file version `current_chapters` belongs to, or is being read from.
    chapters_key: Option<ChapterKey>,
    /// A worker is reading the chapters of `chapters_key`.
    chapters_reading: bool,
    chapter_cache: ChapterCache,
    chapter_cursor: ChapterCursor,
    chapter_picker_state: ChapterPicker,
    /// Chapter starts for the tick marks of the seek bar.
    chapter_marks: Vec<f64>,
    /// The player bar's chapter line, made for the chapter with this index.
    chapter_line: Option<(usize, String)>,
    /// The Chapters list scrolls to the current chapter when it is next drawn.
    chapters_scroll: bool,
    /// Lyrics of the current file (`None`: it has none, or they are not read
    /// yet). Memory only.
    current_lyrics: Arc<Option<lyrics::Lyrics>>,
    /// The file version `current_lyrics` belongs to, or is being read from.
    lyrics_key: Option<LyricsKey>,
    /// A worker is reading the lyrics of `lyrics_key`.
    lyrics_reading: bool,
    lyrics_cache: LyricsCache,
    /// Numbers the lyrics shown, so a layout made for other lyrics is never reused.
    lyrics_serial: u64,
    lyrics_view: LyricsView,
    /// What the library, stations and playlists need in fonts, and what is done
    /// about it (memory only).
    font_fallbacks: appearance::FontFallbacks,
    /// Reads the system fonts for the scripts asked for (tests replace it).
    font_loader: Arc<FontLoader>,
    /// The first frame has run, so the fonts of the context can be asked about.
    fonts_ready: bool,
    /// Counts the fallback fonts added; the lyrics layout is made again after one.
    font_epoch: u32,
    /// Where the armed "end of chapter" sleep stops playback. It is kept once
    /// worked out, so playing across the boundary pauses (the position is then
    /// already in the next chapter); every seek and load drops it.
    sleep_chapter_end: Option<f64>,
    /// The window that asks for a playlist name (new, from the queue, rename).
    playlist_dialog: Option<PlaylistDialog>,
    /// The window that adds, edits or renames a station.
    station_dialog: Option<StationDialog>,
    /// Numbers the Test runs of the station window.
    station_test_serial: u64,
    /// What the connections of each station told, by station id.
    station_live: HashMap<String, StationLive>,
    /// The row selected in the Streams list (in memory only).
    station_selected: Option<String>,
    /// The row selected in the Artists, Albums or Genres list: the key of its
    /// group (in memory only; one row, not a set).
    browse_selected: Option<String>,
    /// A click on a browse row that has not opened its group yet (see
    /// `BROWSE_OPEN_DELAY`).
    browse_pending: Option<BrowsePending>,
    /// The next draw of a browse list scrolls its selected row into view.
    browse_reveal: bool,
    /// The files of the browse group a menu or Play last asked for, for the
    /// library revision they were made for.
    browse_ids: Option<(BrowseKind, String, u64, Arc<Vec<String>>)>,
    /// The latest live title and the station it is from; kept while paused.
    live_title: Option<(String, String)>,
    /// Listening time of the current station, kept while it reconnects.
    station_position: f64,
    /// Listening time before the current connection (a reconnect goes on from it).
    station_base: f64,
    /// The engine is connecting to the current station.
    station_connecting: bool,
    /// What the last playlist import did to the station list, for the status note.
    station_import_note: Option<String>,
    /// The window title last sent (`None`: the default one).
    window_title: Option<String>,
    /// What the user can take back with Undo: in memory, never saved.
    undo: state::UndoStack,
    /// The status note shown now belongs to the newest undo entry, so the
    /// status bar offers "Undo" beside it.
    undo_note: bool,
    /// How much longer than `STATUS_SHOWN` the current status note stays.
    status_extra: Duration,
    confirm: Option<Confirm>,
    /// The "Re-read tags of all files" job in progress.
    reread: Option<RereadRun>,
    /// The scans, re-reads and folder moves that run now (status text, Cancel).
    scan: ScanState,
    about: bool,
    about_center_pending: bool,
    about_logo: Option<Result<egui::TextureHandle, String>>,
    last_appearance: Settings,
    window_level_applied: Option<bool>,
    window_recovery_pending: bool,
    seek_drag: Option<(String, f64)>,
    actions: Vec<Action>,
    save_failed: bool,
    scan_errors_reported: HashSet<String>,
    startup_scan_pending: bool,
    rng: u64,
    list_viewport: f32,
    scroll_to_current: bool,
    restore_scroll: bool,
    pending_position: Option<(String, f64, Instant)>,
    pending_progress_restore: Option<(u64, String, state::Progress)>,
    seen_audio_error: Option<String>,
    private_track: Option<String>,
    view_revision: u64,
    library_revision: u64,
    progress_revision: u64,
    gui_cache: GuiCache,
    last_stop_poll: Instant,
    /// When the profile's request folder was last read (second-launch hand-off).
    last_request_poll: Instant,
    /// Requests read from disk that wait for a moment the player can act on them.
    pending_requests: VecDeque<instance::Request>,
    /// Whether the global media keys are asked for now (see `media_keys_wanted`).
    media_keys_on: bool,
    /// File-association state last read (when Options opened, after each action).
    association: AssociationState,
    association_busy: bool,
    association_error: Option<String>,
    tray: Option<crate::tray::TrayHandle>,
    tray_retiring: bool,
    tray_available: bool,
    tray_reason: String,
    tray_hidden: bool,
    /// "Start minimised to tray" waits for the tray to report it is working.
    start_hidden_pending: bool,
    tray_restore_guard: TrayRestoreGuard,
    tray_quitting: bool,
    tray_popup_open: bool,
    tray_tooltip_key: Option<(Option<String>, (u64, u64, bool))>,
    tray_popup_position: egui::Pos2,
    tray_popup_shared: Arc<Mutex<TrayPopupShared>>,
    tray_popup_tx: Sender<TrayPopupEvent>,
    tray_popup_rx: Receiver<TrayPopupEvent>,
}

impl PlayerApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        profile: PathBuf,
        state: Session,
        warning: Option<String>,
    ) -> Self {
        Self::new_with_args(cc, profile, state, warning, std::env::args_os().collect())
    }

    fn new_with_args(
        cc: &eframe::CreationContext<'_>,
        profile: PathBuf,
        state: Session,
        warning: Option<String>,
        args: Vec<OsString>,
    ) -> Self {
        Self::new_with_args_and_audio(cc, profile, state, warning, args, None)
    }



    fn new_with_args_and_audio(
        cc: &eframe::CreationContext<'_>,
        profile: PathBuf,
        mut state: Session,
        warning: Option<String>,
        args: Vec<OsString>,
        audio: Option<AudioEngine>,
    ) -> Self {
        let _ = std::fs::remove_file(profile.join("stop.request"));
        let silent = args.iter().any(|s| s == "--silent");
        let exit_after = args
            .iter()
            .position(|s| s == "--exit-after")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse().ok());
        if !state.settings.restore_session {
            state.queue.clear();
            state.playing_workspace = false;
            state.current = None;
            state.current_station = None;
            state.was_playing = false;
            state.selected = None;
            state.search.clear();
            state.scroll = 0.;
            state.ab_a = None;
            state.ab_b = None;
        }
        let station_selected = state.current_station.clone();
        let expired_sleep = state
            .sleep_deadline
            .is_some_and(|deadline| deadline <= state::now());
        if expired_sleep {
            state.sleep_deadline = None;
            state.sleep_mode = "off".into();
            state.was_playing = false;
        }
        appearance::install_fonts(&cc.egui_ctx);
        settings::apply_appearance(&cc.egui_ctx, &state.settings);
        let audio = audio.unwrap_or_else(|| {
            AudioEngine::new(
                &settings::engine_settings(&state.settings),
                if state.muted { 0. } else { state.volume },
                state.speed,
                silent,
            )
        });
        let (tx, rx) = mpsc::channel();
        let (tray_popup_tx, tray_popup_rx) = mpsc::channel();
        let persistence = crate::persistence::Persistence::new(profile.clone());
        let mut app = Self {
            gui_context: cc.egui_ctx.clone(),
            profile,
            persistence,
            state,
            audio,
            snapshot: Snapshot::default(),
            options: None,
            options_center_pending: false,
            equalizer: None,
            equalizer_center_pending: false,
            equalizer_dirty: false,
            devices: vec![],
            device_request: 0,
            tx,
            rx,
            deferred_events: Vec::new(),
            restore_windows_hidden: false,
            session_generation: 0,
            load_request: 0,
            expected_audio_request: None,
            loading_request: None,
            pending_play_intent: None,
            list_focus: None,
            refocus_list: false,
            selection: SelectionSet::default(),
            selection_view: 0,
            selection_scope: (String::new(), String::new()),
            selected_cache: None,
            selection_summary: None,
            picker_open: Arc::new(AtomicBool::new(false)),
            status: "Ready — add files or a folder to begin".into(),
            status_at: state::instant_ago(STATUS_SHOWN),
            error: warning,
            busy: 0,
            restore_pending: false,
            playing_request: 0,
            playing_cancel: None,
            last_save: Instant::now(),
            last_scan: Instant::now(),
            started: Instant::now(),
            exit_after,
            silent,
            initial_restore: true,
            suppress_autoplay: expired_sleep,
            ended_handled: false,
            planned_next: None,
            preload: None,
            preload_checked: state::instant_ago(PRELOAD_CHECK_EVERY),
            skipped_seconds: 0.,

            artwork: None,
            art_note: None,
            details_art: None,
            thumbs: ThumbLoader::new(),
            art_requested: None,
            art_embedded: None,
            art_debounce: ArtDebounce::default(),
            expanded_art: false,
            details: None,
            tag_editor: None,
            tag_session: 0,
            tag_save_running: false,
            tag_resume: None,
            bookmark_dialog: false,
            bookmark_target: None,
            bookmark_name: String::new(),
            bookmark_note: String::new(),
            bookmark_edit: None,
            paused_at: None,
            playing_last_tick: false,
            current_chapters: Arc::default(),
            chapters_key: None,
            chapters_reading: false,
            chapter_cache: ChapterCache::default(),
            chapter_cursor: ChapterCursor::default(),
            chapter_picker_state: ChapterPicker::default(),
            chapter_marks: Vec::new(),
            chapter_line: None,
            chapters_scroll: false,
            current_lyrics: Arc::default(),
            lyrics_key: None,
            lyrics_reading: false,
            lyrics_cache: LyricsCache::default(),
            lyrics_serial: 0,
            lyrics_view: LyricsView::default(),
            font_fallbacks: appearance::FontFallbacks::default(),
            font_loader: Arc::new(appearance::load_system_fonts),
            fonts_ready: false,
            font_epoch: 0,
            sleep_chapter_end: None,
            playlist_dialog: None,
            station_dialog: None,
            station_test_serial: 0,
            station_live: HashMap::new(),
            station_selected,
            browse_selected: None,
            browse_pending: None,
            browse_reveal: false,
            browse_ids: None,
            live_title: None,
            station_position: 0.,
            station_base: 0.,
            station_connecting: false,
            station_import_note: None,
            window_title: None,
            undo: state::UndoStack::default(),
            undo_note: false,
            status_extra: Duration::ZERO,
            confirm: None,
            reread: None,
            scan: ScanState::default(),
            about: false,
            about_center_pending: false,
            about_logo: None,
            last_appearance: settings::Settings::default(),
            window_level_applied: None,
            window_recovery_pending: true,
            seek_drag: None,
            actions: vec![],
            save_failed: false,
            scan_errors_reported: HashSet::new(),
            startup_scan_pending: false,
            rng: random_seed(),
            list_viewport: 0.,
            scroll_to_current: false,
            restore_scroll: true,
            pending_position: None,
            pending_progress_restore: None,
            seen_audio_error: None,
            private_track: None,
            view_revision: 0,
            library_revision: 0,
            progress_revision: 0,
            gui_cache: GuiCache::default(),
            last_stop_poll: state::instant_ago(Duration::from_secs(1)),
            last_request_poll: state::instant_ago(REQUEST_POLL),
            pending_requests: VecDeque::new(),
            media_keys_on: false,
            association: AssociationState::NotRegistered,
            association_busy: false,
            association_error: None,
            tray: None,
            tray_retiring: false,
            tray_available: false,
            tray_reason: "Starting system tray…".into(),
            tray_hidden: false,
            start_hidden_pending: false,
            tray_restore_guard: TrayRestoreGuard::default(),
            tray_quitting: false,
            tray_popup_open: false,
            tray_tooltip_key: None,
            tray_popup_position: egui::Pos2::ZERO,
            tray_popup_shared: Arc::new(Mutex::new(TrayPopupShared {
                anchor: None,
                snapshot: crate::tray_popup::TrayPopupState {
                    title: "Nothing playing".into(),
                    artist: String::new(),
                    position: 0.,
                    duration: 0.,
                    playing: false,
                    can_control: false,
                    speed: 1.,
                    skip_back: 15.,
                    skip_forward: 30.,
                    sleep_mode: "off".into(),
                    sleep_deadline: None,
                    font_size: 14.,
                    live: false,
                },
                seen_focus: false,
                placed: None,
            })),
            tray_popup_tx,
            tray_popup_rx,
        };
        let mut paths = vec![];
        let mut skip = false;
        for arg in args.into_iter().skip(1) {
            if skip {
                skip = false;
                continue;
            }
            if arg == "--profile" || arg == "--exit-after" {
                skip = true;
                continue;
            }
            if !arg.as_encoded_bytes().starts_with(b"--") {
                paths.push(PathBuf::from(arg));
            }
        }
        // The silence switch and the ReplayGain follow the restored track from the start.
        app.apply_skip_silence();
        // Files named on the command line mean the user wants to see the player.
        app.start_hidden_pending = app.state.settings.start_minimized && paths.is_empty();
        let scan_on_start = app.state.settings.scan_on_start;
        if !paths.is_empty() {
            let append = app.state.settings.external_files_append;
            app.import(paths, false, append);
            // A scan is refused while an import runs; tick() starts it once the
            // command-line import has finished.
            app.startup_scan_pending = scan_on_start;
        } else if scan_on_start {
            app.rescan_with(true);
        }
        app
    }

    /// Sets the status note; the status bar shows it for a few seconds.
    fn set_status(&mut self, text: impl Into<String>) {
        self.status = text.into();
        self.status_at = Instant::now();
        self.status_extra = Duration::ZERO;
        self.undo_note = false;
    }

    /// A status note for an action that was just recorded for Undo: it stays a
    /// little longer and the status bar offers "Undo" beside it.
    fn set_undo_status(&mut self, text: impl Into<String>) {
        self.set_status(text);
        self.undo_note = !self.undo.is_empty();
        self.status_extra = UNDO_NOTE_SHOWN.saturating_sub(STATUS_SHOWN);
    }

    /// How long the status note has been showing, not counting the extra time an
    /// undo note stays.
    fn status_age(&self) -> Duration {
        self.status_at.elapsed().saturating_sub(self.status_extra)
    }

    /// The note the status bar shows instead of the file count, if any.
    fn status_for_bar(&self) -> Option<&str> {
        let loading = self.loading_request.is_some() || self.pending_position.is_some();
        status_visible(&self.status, self.busy > 0, loading, self.status_age())
            .then_some(self.status.as_str())
    }

    /// Whether the status bar shows the clickable "Undo" now.
    fn undo_offered(&self) -> bool {
        self.undo_note && !self.undo.is_empty() && self.status_for_bar().is_some()
    }

    /// Adds `text` to the message dialog instead of replacing what is showing.
    fn add_message(&mut self, text: String) {
        append_message(&mut self.error, text);
    }

    fn position(&self) -> f64 {
        if self.state.current_station.is_some() {
            // Listening time of the station, never a saved position.
            return self.station_position;
        }
        playback_position(&self.state, &self.snapshot, self.pending_position.as_ref())
    }

    fn duration(&self) -> f64 {
        if self.snapshot.loaded == self.state.current && self.snapshot.duration > 0. {
            self.snapshot.duration
        } else {
            self.state.current_track().map(|t| t.duration).unwrap_or(0.)
        }
    }

    fn checkpoint(&mut self) {
        // A live stream has no position to save, whatever else is going on.
        if self.state.current_station.is_some() {
            return;
        }
        if self.initial_restore
            || self.loading_request.is_some()
            || self.pending_position.is_some()
            || self.snapshot.error.is_some()
            || self.state.settings.private_listening
            || self.private_track == self.state.current
        {
            return;
        }
        if let Some(track) = self.state.current_track().cloned() {
            let position = self.position();
            let p = self.state.progress.entry(track.id).or_default();
            let previous_seconds = p.seconds;
            if self.state.settings.remember_positions && !p.completed {
                p.seconds = position.max(0.);
            }
            p.file_size = track.file_size;
            p.modified = track.modified;
            if self.snapshot.playing {
                p.last_played = state::now();
            }
            if p.seconds != previous_seconds {
                self.progress_changed();
            }
        }
    }

    /// Progress (position, finished flag, counts) changed: the lists that depend
    /// on it (Continue listening, sorting by progress) must be rebuilt.
    fn progress_changed(&mut self) {
        self.progress_revision = self.progress_revision.wrapping_add(1);
    }

    fn save(&mut self) {
        self.checkpoint();
        // Saving a volume, window or selection change must not rebuild or re-sort
        // the library lists; code that changes progress calls `progress_changed`.
        self.persistence.save(self.state.clone());
        self.last_save = Instant::now();
    }
}

impl eframe::App for PlayerApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.recover_window_position(frame);
        self.tick(ctx);
        self.tray_tick(ctx);
        if !self.restore_pending { self.integration_tick(ctx); }
    }
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.restore_pending {
            if !self.restore_windows_hidden {
                self.set_secondary_visibility(&ctx, false);
                self.restore_windows_hidden = true;
            }
            egui::CentralPanel::default().show(ui, |ui| {
                ui.heading("Preparing profile restore");
                ui.spinner();
                ui.label("Saving a safety copy of the current profile before replacing it…");
                if ui.button("Close player").clicked() { self.request_quit(&ctx); }
            });
            ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        if self.restore_windows_hidden {
            self.set_secondary_visibility(&ctx, true);
            self.restore_windows_hidden = false;
        }
        self.fonts_ready = true;
        self.register_tray_popup(&ctx);
        self.settle_browse_section();
        self.resolve_browse_pending(&ctx);
        self.shortcuts(&ctx);
        if self.window_level_applied != Some(self.state.settings.always_on_top) {
            let level = self.window_level();
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
            // Options, Equalizer and About are separate top-level windows; they
            // follow the player, whether the setting is on or off.
            for (open, id) in [
                (self.options.is_some(), "bs-player-options"),
                (self.equalizer.is_some(), "bs-player-equalizer"),
                (self.about, "bs-player-about"),
            ] {
                if open {
                    ctx.send_viewport_cmd_to(
                        egui::ViewportId::from_hash_of(id),
                        egui::ViewportCommand::WindowLevel(level),
                    );
                }
            }
            self.window_level_applied = Some(self.state.settings.always_on_top);
        }
        self.menus(ui);
        self.transport(ui);
        self.sidebar(ui);
        self.content(ui);
        // The list had its chance to take the focus back from the search box.
        self.refocus_list = false;
        // The follow-current request applies to the frame that just drew the list;
        // when the track was not in the shown list it must not fire later.
        self.scroll_to_current = false;
        self.actions();
        let monitor = if self.options_center_pending
            || self.equalizer_center_pending
            || self.about_center_pending
        {
            native_monitor_rect(frame)
        } else {
            None
        };
        self.dialogs(&ctx, monitor);
        self.resize_frame(ui);
        let appearance = self
            .options
            .as_ref()
            .map(|o| o.preview())
            .unwrap_or(&self.state.settings);
        if appearance.theme != self.last_appearance.theme
            || appearance.accent != self.last_appearance.accent
            || appearance.font_size != self.last_appearance.font_size
            || appearance.ui_scale != self.last_appearance.ui_scale
        {
            settings::apply_appearance(&ctx, appearance);
            self.last_appearance = appearance.clone();
        }
        // Context input transactions hold its write lock; read zoom beforehand.
        let zoom = ctx.zoom_factor();
        ctx.input(|input| {
            let Some(view) = input.raw.viewports.get(&egui::ViewportId::ROOT) else {
                return;
            };
            if let Some(rect) = view
                .inner_rect
                .filter(|_| !view.maximized.unwrap_or(false) && !self.state.fullscreen)
            {
                // Viewport rectangles use egui points including application zoom.
                // NativeOptions expects native logical pixels, excluding that zoom.
                self.state.window.width = rect.width() * zoom;
                self.state.window.height = rect.height() * zoom;
                let origin = view.outer_rect.map(|outer| outer.min).unwrap_or(rect.min);
                self.state.window.x = origin.x * zoom;
                self.state.window.y = origin.y * zoom;
            }
            self.state.window.maximized = view.maximized.unwrap_or(false);
        });
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let was_playing = self.state.was_playing;
        self.checkpoint();
        let (snapshot, audio_warning) = self.audio.prepare_exit();
        // The engine settles a join the clock already passed before it answers:
        // the app follows it first, then accepts the exit snapshot that shows the
        // track after the join.
        for change in self.audio.poll().changes {
            self.handle_track_change(change);
        }
        let engine_id = self.engine_current();
        self.snapshot =
            accepted_snapshot(snapshot, self.expected_audio_request, engine_id.as_deref());
        self.pending_position = None;
        self.checkpoint();
        self.state.was_playing = was_playing;
        if let Err(error) = self.persistence.finish(self.state.clone()) {
            eprintln!("Final profile save failed: {error}");
            if !self.silent {
                rfd::MessageDialog::new()
                    .set_title("BS Player — progress could not be saved")
                    .set_description(error)
                    .set_level(rfd::MessageLevel::Error)
                    .show();
            }
        }
        if let Some(warning) = audio_warning {
            eprintln!("{warning}");
            if !self.silent {
                rfd::MessageDialog::new()
                    .set_title("BS Player — final playback position unavailable")
                    .set_description(warning)
                    .set_level(rfd::MessageLevel::Warning)
                    .show();
            }
        }
        // Give the media keys back before the tray window that holds them goes.
        self.media_keys_on = false;
        winintegration::set_media_keys_enabled(false);
        // Final playback state is durable before any platform tray teardown.
        if let Some(mut tray) = self.tray.take() {
            tray.shutdown();
        }
    }
}



// The About window is redrawn on input; CPUID need not run each time.
static ISA_SUPPORT: OnceLock<String> = OnceLock::new();

static RUNTIME_FEATURES: OnceLock<String> = OnceLock::new();

/// How long a status note replaces the file count in the status bar.
const STATUS_SHOWN: Duration = Duration::from_secs(6);

/// How long the note of an undoable action (and its "Undo" link) stays.
const UNDO_NOTE_SHOWN: Duration = Duration::from_secs(8);

/// How long a volume or speed note stays; it is shown through the same status
/// bar, backdated so the normal timer ends it sooner.
const BRIEF_STATUS_SHOWN: Duration = Duration::from_secs(2);

/// "Start minimised to tray" only hides the window when the tray reports it is
/// working within this long after launch; later it would hide a window in use.
const START_HIDDEN_WINDOW: Duration = Duration::from_secs(10);

const VOICE_BOOST_NOTE: &str = "Voice boost is on: the Voice curve and +4 dB are added to what you hear. These bands and the preamp are not changed.";

const STATUS_CHECKING: &str = "Checking audio file…";

/// Where the profile is copied to before a folder move (one slot, overwritten).
const MOVE_FOLDER_COPY: &str = "before-move-folder.json";

/// Where the profile is copied to before a listening progress import.
const PROGRESS_IMPORT_COPY: &str = "before-import-progress.json";

const NOT_UNICODE_FOLDER: &str =
    "The folder name is not valid Unicode, so it cannot be kept in the profile";

const STATUS_OPENING: &str = "Opening audio…";

/// Whether the status bar shows `status`: always while an import runs, for
/// `STATUS_SHOWN` after it was set otherwise. The two load notes only make
/// sense while a load is under way.
fn status_visible(status: &str, busy: bool, loading: bool, age: Duration) -> bool {
    if status.is_empty() {
        return false;
    }
    if busy {
        return true;
    }
    if (status == STATUS_CHECKING || status == STATUS_OPENING) && !loading {
        return false;
    }
    age < STATUS_SHOWN
}

/// What the "Add audio files or playlists" dialog offers: the audio formats the
/// player reads and the playlists it imports.
const ADD_FILE_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "ogg", "oga", "opus", "m4a", "m4b", "aac", "aiff", "aif", "mp4", "m3u",
    "m3u8", "pls", "mkv", "mka", "webm",
];

/// What the About window says about the audio path and what it does not do.
const ABOUT_AUDIO_NOTE: &str = "Audio uses a shared output stream. Pitch-preserving speed uses streaming WSOLA. Gapless playback joins files that have the same sample rate and channels, long silences can be skipped in audiobooks, the global media keys work on Windows, and the tags and artwork of MP3, M4A/M4B, FLAC and Ogg files can be written. Crossfade is not implemented.";

/// What the engine says when a file ends clearly before its reported length.
const EARLY_END_NOTE: &str = "Audio decoding ended before the end of this file. Progress is retained; check whether the file is damaged.";

/// The first entry of `queue` from `start` on that exists, wrapping to the top
/// when `wrap` is set. `None` when there is no such entry.
fn first_existing(
    queue: &[String],
    start: usize,
    wrap: bool,
    exists: impl Fn(&str) -> bool,
) -> Option<String> {
    let len = queue.len();
    let span = if wrap { len } else { len.saturating_sub(start) };
    (0..span)
        .map(|offset| &queue[(start + offset) % len])
        .find(|id| exists(id))
        .cloned()
}

/// Adds `text` below a message that is already showing, once; sets it otherwise.
fn append_message(slot: &mut Option<String>, text: String) {
    match slot {
        None => *slot = Some(text),
        Some(existing) if existing.contains(&text) || existing.len() > 4000 => {}
        Some(existing) => {
            existing.push_str("\n\n");
            existing.push_str(&text);
        }
    }
}

/// The engine reports a track that ran out before its estimated length with this
/// note. It is not a failure: the queue still moves on.
fn is_early_end_note(error: &str) -> bool {
    error.starts_with("Audio decoding ended before the end of this file")
}

const RESTORE_COPIES: usize = 3;

fn restore_copy_name(number: usize) -> String {
    if number <= 1 {
        "before-restore.json".to_owned()
    } else {
        format!("before-restore-{number}.json")
    }
}

/// Moves `staged` to `before-restore.json` and each older safety copy one place
/// down (`-2`, `-3`); the oldest one is replaced.
fn rotate_restore_copies(dir: &std::path::Path, staged: &std::path::Path) -> std::io::Result<()> {
    for number in (1..RESTORE_COPIES).rev() {
        let from = dir.join(restore_copy_name(number));
        if from.is_file() {
            std::fs::rename(&from, dir.join(restore_copy_name(number + 1)))?;
        }
    }
    std::fs::rename(staged, dir.join(restore_copy_name(1)))
}

/// A book label that several books share, with the folder (or the tag text)
/// that tells them apart added.
fn disambiguated_label(label: &str, key: &str) -> String {
    match std::path::Path::new(key)
        .parent()
        .and_then(|parent| parent.file_name())
    {
        Some(parent) => format!("{label} · {}", parent.to_string_lossy()),
        None => key.to_owned(),
    }
}
