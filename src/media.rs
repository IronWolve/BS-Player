//! Library scanning, metadata, artwork, and playlist helpers.

mod identity;
mod safety;
pub use safety::validate_lofty_input;

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, FileType};
use lofty::picture::PictureType;
use lofty::prelude::{Accessor, ItemKey, TaggedFileExt};
use lofty::probe::Probe;
use crate::container_audio::{self, AudioProperties, OpenedAudio};
use symphonia::core::meta::{StandardTagKey, Value};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, Write};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::time::{Duration, Instant, UNIX_EPOCH};

const ARTWORK_LIMIT: u64 = 16 * 1024 * 1024;
const FINGERPRINT_SAMPLE: usize = 64 * 1024;
const MAX_PLAYLIST_BYTES: usize = 16 * 1024 * 1024;
const MAX_PLAYLIST_LINE_BYTES: usize = 64 * 1024;
const MAX_PLAYLIST_ENTRIES: usize = 100_000;
/// Most stream entries one playlist may turn into stations.
const MAX_PLAYLIST_STATIONS: usize = 1000;
const MAX_SCAN_DEPTH: usize = 128;
/// Maximum filesystem entries inspected in one import, including folders.
const MAX_SCAN_ENTRIES: usize = 100_000;
const MAX_TAG_TEXT_CHARS: usize = 512;
const MAX_COMMENT_CHARS: usize = 4096;
const NOT_UNICODE: &str = "name is not valid Unicode; rename it to import";
const AUDIO_EXTENSIONS: &[&str] = &[
    "aac", "aif", "aiff", "flac", "m4a", "m4b", "mp3", "mp4", "oga", "ogg", "opus", "wav", "wave",
];
/// Opus is always decoded and played at 48 kHz, whatever rate the encoder was fed.
const OPUS_PLAYBACK_RATE: u32 = 48_000;
/// The ReplayGain (dB) a library record keeps is held to the window the player
/// applies (see `App::replaygain_for`), so a damaged tag cannot store a wild value.
const REPLAYGAIN_MIN_DB: f32 = -60.;
const REPLAYGAIN_MAX_DB: f32 = 24.;
/// Opus `R128_*_GAIN` tags aim at -23 LUFS, ReplayGain at -18 LUFS: the same
/// loudness correction is this many dB higher on the ReplayGain scale.
const R128_TO_REPLAYGAIN_DB: f32 = 5.;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Track {
    pub id: String,
    pub content_digest: Option<String>,
    pub audio_digest: Option<String>,
    pub legacy_fingerprint: Option<String>,
    pub superseded: bool,
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub genre: String,
    pub duration: f64,
    pub bitrate: u64,
    pub sample_rate: u32,
    pub channels: u32,
    pub track_number: u32,
    pub year: String,
    pub comment: String,
    pub library_edited: bool,
    pub replaygain_track_db: Option<f32>,
    pub replaygain_album_db: Option<f32>,
    pub replaygain_track_peak: Option<f32>,
    pub replaygain_album_peak: Option<f32>,
    pub audiobook: bool,
    pub added_at: u64,
    pub file_size: u64,
    pub modified: u64,
    pub modified_nanos: Option<u64>,
    /// Read from the file's tags when it is scanned (the library tag editor does
    /// not change them): album artist, composer, disc number and narrator. They
    /// are empty (zero) for libraries scanned before they were read; the
    /// "Re-read tags" action in Options fills them in.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub album_artist: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub composer: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub disc: u32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub narrator: String,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl Default for Track {
    fn default() -> Self {
        Self {
            id: String::new(),
            content_digest: None,
            audio_digest: None,
            legacy_fingerprint: None,
            superseded: false,
            path: PathBuf::new(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            genre: String::new(),
            duration: 0.0,
            bitrate: 0,
            sample_rate: 0,
            channels: 0,
            track_number: 0,
            year: String::new(),
            comment: String::new(),
            library_edited: false,
            replaygain_track_db: None,
            replaygain_album_db: None,
            replaygain_track_peak: None,
            replaygain_album_peak: None,
            audiobook: false,
            added_at: 0,
            file_size: 0,
            modified: 0,
            modified_nanos: None,
            album_artist: String::new(),
            composer: String::new(),
            disc: 0,
            narrator: String::new(),
        }
    }
}

impl Track {
    pub fn display_title(&self) -> &str {
        if self.title.trim().is_empty() {
            self.path
                .file_stem()
                .and_then(|name| name.to_str())
                .unwrap_or("")
        } else {
            &self.title
        }
    }

    /// A readable label for a book; grouping identity remains separate.
    pub fn book_label(&self) -> String {
        if !self.album.trim().is_empty() {
            return self.album.clone();
        }
        self.path
            .parent()
            .and_then(|p| p.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.display_title().to_owned())
    }

    /// Groups audiobook parts by album and artist, or by their parent directory.
    pub fn book_key(&self) -> String {
        let album = self.album.trim();
        if !album.is_empty() {
            let artist = self.artist.trim();
            return if artist.is_empty() {
                album.to_owned()
            } else {
                format!("{album} — {artist}")
            };
        }
        self.path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.to_string_lossy().into_owned())
    }

    /// Restores user-edited library tags after rescanning file metadata.
    pub fn preserve_library_tags(&mut self, old: &Track) {
        if !old.library_edited {
            return;
        }
        self.title = old.title.clone();
        self.artist = old.artist.clone();
        self.album = old.album.clone();
        self.genre = old.genre.clone();
        self.year = old.year.clone();
        self.comment = old.comment.clone();
        self.track_number = old.track_number;
        self.audiobook = old.audiobook;
        self.library_edited = true;
    }
}

/// Imports audio files, directories, and M3U/PLS playlists. Repeated canonical paths
/// are imported once; separate files with identical bytes remain separate library items.
#[allow(dead_code)]
pub fn import_paths(
    paths: Vec<PathBuf>,
    audiobook: bool,
    recursive: bool,
    excluded_extensions: &[String],
) -> (Vec<Track>, Vec<String>) {
    let found = import_paths_with(
        paths,
        audiobook,
        recursive,
        excluded_extensions,
        None,
        false,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    (found.tracks, found.errors)
}

/// A stream address found in a playlist, with the name the playlist gave it
/// (`#EXTINF` text, PLS `TitleN`, or the M3U `#PLAYLIST` title of a playlist that
/// holds one entry; empty when it gave none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistStation {
    pub name: String,
    pub url: String,
}

/// What an import found: library files, problems, and the `http(s)` stream
/// addresses of the playlists that were named directly.
#[derive(Debug, Default)]
pub struct PathImport {
    pub tracks: Vec<Track>,
    pub errors: Vec<String>,
    pub stations: Vec<PlaylistStation>,
    /// How many files the folder walk found (the files that were to be read).
    pub found: usize,
    /// The import was stopped on request; `tracks` holds what was read until then.
    pub cancelled: bool,
}

/// Which part of an import a `ScanProgress` report describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanPhase {
    /// Folders are being walked to find the files; only `found` is known.
    Collecting,
    /// The files found are being read; `done` of `total` are finished.
    Reading,
}

/// A progress report of a running import (see `import_paths_cancellable`).
/// `found` is the number of files found so far (all of them once reading);
/// `done` and `total` are zero while collecting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanProgress {
    pub phase: ScanPhase,
    pub found: usize,
    pub done: usize,
    pub total: usize,
    /// The name (not the path) of the file or folder being looked at, at most
    /// `PROGRESS_NAME_CHARS` characters.
    pub current: String,
}

/// Progress reports go out at most this often (and at the end of each phase).
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
/// The longest `ScanProgress::current`, in characters.
pub const PROGRESS_NAME_CHARS: usize = 80;



fn progress_interval() -> Duration {

    PROGRESS_INTERVAL
}

/// The name of `path` (its last component) for a progress report, shortened to
/// `PROGRESS_NAME_CHARS` characters.
fn progress_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.chars().count() <= PROGRESS_NAME_CHARS {
        return name;
    }
    let head: String = name.chars().take(PROGRESS_NAME_CHARS - 1).collect();
    format!("{head}…")
}

/// Hands progress reports to a callback no more than five times a second.
struct ProgressSink<'a> {
    report: &'a mut dyn FnMut(ScanProgress),
    last: Option<Instant>,
}

impl<'a> ProgressSink<'a> {
    fn new(report: &'a mut dyn FnMut(ScanProgress)) -> Self {
        Self { report, last: None }
    }
    /// Reports `make()` when `force` is set or the last report is old enough.
    fn emit(&mut self, force: bool, make: impl FnOnce() -> ScanProgress) {
        let now = Instant::now();
        if !force
            && self
                .last
                .is_some_and(|last| now.duration_since(last) < progress_interval())
        {
            return;
        }
        self.last = Some(now);
        (self.report)(make());
    }
}

/// What the folder walk of one import shares: the cancel flag, the progress
/// sink and the number of files found so far.
struct ScanContext<'a> {
    cancel: &'a AtomicBool,
    sink: ProgressSink<'a>,
    found: usize,
    entries_seen: usize,
    entry_limit_reported: bool,
}

impl ScanContext<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(AtomicOrdering::Relaxed)
    }
    fn collecting(&mut self, force: bool, current: &Path) {
        let found = self.found;
        self.sink.emit(force, || ScanProgress {
            phase: ScanPhase::Collecting,
            found,
            done: 0,
            total: 0,
            current: progress_name(current),
        });
    }

    fn count_entry(&mut self, path: &Path, errors: &mut Vec<String>) -> bool {
        if self.entries_seen >= MAX_SCAN_ENTRIES {
            if !self.entry_limit_reported {
                errors.push(format!(
                    "{}: scan exceeds the {MAX_SCAN_ENTRIES} entry safety limit; remaining entries were skipped",
                    path.display()
                ));
                self.entry_limit_reported = true;
            }
            return false;
        }
        self.entries_seen += 1;
        true
    }
}

/// As `import_paths`, but the `http://` and `https://` entries of a playlist
/// that is itself one of `paths` are returned as stations instead of being
/// skipped. (A playlist met while walking a folder, or listed by another
/// playlist, still skips its stream entries: a rescan must not bring back
/// stations the user removed.) Entries with other schemes (`mms://`, `rtsp://`)
/// are skipped with a note.
#[allow(dead_code)]
pub fn import_paths_and_stations(
    paths: Vec<PathBuf>,
    audiobook: bool,
    recursive: bool,
    excluded_extensions: &[String],
) -> PathImport {
    import_paths_with(
        paths,
        audiobook,
        recursive,
        excluded_extensions,
        None,
        true,
        &AtomicBool::new(false),
        &mut |_| {},
    )
}

/// As `import_paths_and_stations`, and as `import_paths_incremental` when `known`
/// is given, but it reports `progress` (at most five times a second, and when a
/// phase starts or ends) and stops soon after `cancel` is set: the flag is
/// looked at between directory entries and between files. What was read until
/// then is returned with `PathImport::cancelled` set; nothing is taken away.
pub fn import_paths_cancellable(
    paths: Vec<PathBuf>,
    audiobook: bool,
    recursive: bool,
    excluded_extensions: &[String],
    known: Option<&HashMap<PathBuf, Track>>,
    take_stations: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ScanProgress),
) -> PathImport {
    import_paths_with(
        paths,
        audiobook,
        recursive,
        excluded_extensions,
        known,
        take_stations,
        cancel,
        progress,
    )
}

/// As `import_paths`, but a file whose canonical path is in `known` and whose
/// size and modification time (seconds and nanoseconds) are unchanged returns
/// a clone of the known record instead of being read and hashed again. Legacy
/// (digest-less) and superseded records are always refreshed.
#[allow(dead_code)]
pub fn import_paths_incremental(
    paths: Vec<PathBuf>,
    audiobook: bool,
    recursive: bool,
    excluded_extensions: &[String],
    known: &HashMap<PathBuf, Track>,
) -> (Vec<Track>, Vec<String>) {
    let found = import_paths_with(
        paths,
        audiobook,
        recursive,
        excluded_extensions,
        Some(known),
        false,
        &AtomicBool::new(false),
        &mut |_| {},
    );
    (found.tracks, found.errors)
}

fn import_paths_with(
    paths: Vec<PathBuf>,
    audiobook: bool,
    recursive: bool,
    excluded_extensions: &[String],
    known: Option<&HashMap<PathBuf, Track>>,
    take_stations: bool,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ScanProgress),
) -> PathImport {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    let mut stations = Vec::new();
    let mut visited_paths = HashSet::new();
    let mut context = ScanContext {
        cancel,
        sink: ProgressSink::new(progress),
        found: 0,
        entries_seen: 0,
        entry_limit_reported: false,
    };
    for path in paths {
        if context.cancelled() {
            break;
        }
        if !context.count_entry(&path, &mut errors) {
            break;
        }
        // A link the user chose directly is followed; links met while walking a
        // tree are still skipped by `collect_path`.
        let path = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => match fs::canonicalize(&path) {
                Ok(target) => target,
                Err(error) => {
                    errors.push(format!("{}: {error}", path.display()));
                    continue;
                }
            },
            _ => path,
        };
        collect_path(
            &path,
            recursive,
            excluded_extensions,
            &mut visited_paths,
            &mut files,
            &mut errors,
            0,
            true,
            take_stations.then_some(&mut stations),
            &mut context,
        );
    }
    let total = files.len();
    if context.cancelled() {
        // Nothing was read yet: the files found so far are not imported.
        context.collecting(true, Path::new(""));
        return PathImport {
            tracks: Vec::new(),
            errors,
            stations,
            found: total,
            cancelled: true,
        };
    }
    context.sink.emit(true, || ScanProgress {
        phase: ScanPhase::Reading,
        found: total,
        done: 0,
        total,
        current: String::new(),
    });

    // Directory entries are naturally sorted during traversal; retain explicit
    // playlist and file-picker order here.
    let mut seen = HashSet::new();
    let mut tracks = Vec::new();
    let mut cancelled = false;
    for (index, path) in files.into_iter().enumerate() {
        if context.cancelled() {
            cancelled = true;
            break;
        }
        context.sink.emit(false, || ScanProgress {
            phase: ScanPhase::Reading,
            found: total,
            done: index,
            total,
            current: progress_name(&path),
        });
        let key = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !seen.insert(key.clone()) {
            continue;
        }
        if let Some(track) = known.and_then(|known| unchanged_known_track(known, &key, &path)) {
            tracks.push(track);
            continue;
        }
        // One malformed file must not take the whole import worker down.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            refresh_track_cancellable(&path, audiobook, cancel)
        })) {
            Ok(Ok(track)) => tracks.push(track),
            Ok(Err(_)) if context.cancelled() => {
                cancelled = true;
                break;
            }
            Ok(Err(error)) => errors.push(format!("{}: {error}", path.display())),
            Err(_) => errors.push(format!(
                "{}: internal error while reading this file; it was skipped",
                path.display()
            )),
        }
    }
    // Completion is decided before notifying the UI; cancellation triggered by
    // that final notification must not turn a completed import into a partial one.
    let cancelled = cancelled || context.entry_limit_reported || context.cancelled();
    if !cancelled {
        context.sink.emit(true, || ScanProgress {
            phase: ScanPhase::Reading,
            found: total,
            done: total,
            total,
            current: String::new(),
        });
    }
    PathImport {
        tracks,
        errors,
        stations,
        found: total,
        cancelled,
    }
}

/// Returns the known record for `key` when the file on disk still has the size
/// and modification time recorded in it, so it need not be read or hashed.
fn unchanged_known_track(
    known: &HashMap<PathBuf, Track>,
    key: &Path,
    path: &Path,
) -> Option<Track> {
    let old = known.get(key)?;
    if old.superseded || old.content_digest.is_none() {
        return None;
    }
    let metadata = fs::metadata(path).ok().filter(|metadata| metadata.is_file())?;
    let modified_time = file_modified_time(&metadata);
    let modified = modified_time.map(|elapsed| elapsed.as_secs()).unwrap_or(0);
    let modified_nanos =
        modified_time.map(|elapsed| elapsed.as_nanos().min(u64::MAX as u128) as u64);
    if metadata.len() == old.file_size
        && modified == old.modified
        && modified_nanos == old.modified_nanos
    {
        let mut unchanged = old.clone();
        let _ = fill_missing_container_audio_properties(&mut unchanged);
        Some(unchanged)
    } else {
        None
    }
}

/// OS metadata and trash folders that a whole-drive import must not walk into.
fn is_os_metadata_name(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with("._")
        || name.starts_with(".Trash")
        || name == "@eaDir"
        || name.eq_ignore_ascii_case("$RECYCLE.BIN")
        || name.eq_ignore_ascii_case("System Volume Information")
}

fn collect_path(
    path: &Path,
    recursive: bool,
    excluded_extensions: &[String],
    visited_paths: &mut HashSet<PathBuf>,
    files: &mut Vec<PathBuf>,
    errors: &mut Vec<String>,
    depth: usize,
    entry_precounted: bool,
    stations: Option<&mut Vec<PlaylistStation>>,
    context: &mut ScanContext<'_>,
) {
    if context.cancelled() {
        return;
    }
    if !entry_precounted && !context.count_entry(path, errors) {
        return;
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            errors.push(format!("{}: {error}", path.display()));
            return;
        }
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.is_dir() {
        if path.to_str().is_none() {
            errors.push(format!("{}: {NOT_UNICODE}", path.display()));
            return;
        }
        if depth >= MAX_SCAN_DEPTH {
            errors.push(format!(
                "{}: folders are nested more than {MAX_SCAN_DEPTH} levels deep; skipped",
                path.display()
            ));
            return;
        }
        let identity = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        if !visited_paths.insert(identity) {
            return;
        }
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) => {
                errors.push(format!("{}: {error}", path.display()));
                return;
            }
        };
        context.collecting(false, path);
        let mut children = Vec::new();
        for entry in entries {
            if context.cancelled() {
                return;
            }
            match entry {
                Ok(entry) if is_os_metadata_name(&entry.file_name()) => {}
                Ok(entry) => {
                    if !context.count_entry(path, errors) {
                        break;
                    }
                    children.push(entry.path());
                }
                Err(error) => errors.push(format!("{}: {error}", path.display())),
            }
        }
        children.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
        for child in children {
            if context.cancelled() {
                return;
            }
            if recursive || child.is_file() {
                collect_path(
                    &child,
                    recursive,
                    excluded_extensions,
                    visited_paths,
                    files,
                    errors,
                    depth + 1,
                    true,
                    None,
                    context,
                );
            }
        }
        return;
    }
    if !metadata.is_file() {
        return;
    }

    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let playlist = is_playlist_extension(&ext);
    if (playlist || is_audio_extension(&ext, excluded_extensions)) && path.to_str().is_none() {
        // serde cannot write such a path, which would make every profile save fail.
        errors.push(format!("{}: {NOT_UNICODE}", path.display()));
        return;
    }
    if playlist {
        if depth >= MAX_SCAN_DEPTH {
            errors.push(format!(
                "{}: playlists are nested more than {MAX_SCAN_DEPTH} levels deep; skipped",
                path.display()
            ));
            return;
        }
        let identity = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        if !visited_paths.insert(identity) {
            return;
        }
        match read_playlist_entries(path) {
            Ok(entries) => {
                let mut streams = 0usize;
                let mut unsupported = 0usize;
                let mut dropped = 0usize;
                let mut stations = stations;
                for entry in entries {
                    if context.cancelled() {
                        return;
                    }
                    if !context.count_entry(&entry.target, errors) {
                        break;
                    }
                    if playlist_target_is_filesystem_root(&entry.target) {
                        errors.push(format!(
                            "{}: playlist target resolves to a filesystem root; skipped",
                            entry.target.display()
                        ));
                        continue;
                    }
                    let text = entry.target.to_string_lossy().into_owned();
                    if is_stream_url(&text) {
                        match stations.as_mut() {
                            Some(found) if is_http_url(&text) => {
                                if found.len() < MAX_PLAYLIST_STATIONS {
                                    found.push(PlaylistStation {
                                        name: entry.title,
                                        url: text,
                                    });
                                } else {
                                    dropped += 1;
                                }
                            }
                            Some(_) => unsupported += 1,
                            None => streams += 1,
                        }
                        continue;
                    }
                    collect_path(
                        &entry.target,
                        recursive,
                        excluded_extensions,
                        visited_paths,
                        files,
                        errors,
                        depth + 1,
                        true,
                        None,
                        context,
                    );
                }
                if streams > 0 {
                    errors.push(format!(
                        "{}: skipped {streams} stream entries; only local files can be imported",
                        path.display()
                    ));
                }
                if unsupported > 0 {
                    errors.push(format!(
                        "{}: skipped {unsupported} entries whose address is not http:// or https://",
                        path.display()
                    ));
                }
                if dropped > 0 {
                    errors.push(format!(
                        "{}: only the first {MAX_PLAYLIST_STATIONS} stream entries were read; {dropped} more were skipped",
                        path.display()
                    ));
                }
            }
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    } else if is_audio_extension(&ext, excluded_extensions) {
        files.push(path.to_owned());
        context.found += 1;
        context.collecting(false, path);
    }
}

/// A playlist may name a file or folder, but it cannot expand a selected scan
/// into a filesystem root. Direct user-selected roots still go through the
/// normal explicit import path.
fn playlist_target_is_filesystem_root(path: &Path) -> bool {
    if is_filesystem_root_path(path) {
        return true;
    }
    if !path.components().any(|part| part == std::path::Component::ParentDir) {
        return false;
    }
    fs::canonicalize(path)
        .is_ok_and(|canonical| is_filesystem_root_path(&canonical))
}

fn is_filesystem_root_path(path: &Path) -> bool {
    let mut has_root = false;
    for component in path.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::CurDir => {}
            std::path::Component::RootDir => has_root = true,
            std::path::Component::ParentDir | std::path::Component::Normal(_) => return false,
        }
    }
    has_root
}

fn is_audio_extension(ext: &str, excluded_extensions: &[String]) -> bool {
    if excluded_extensions.iter().any(|excluded| {
        excluded
            .trim()
            .trim_start_matches('.')
            .eq_ignore_ascii_case(ext)
    }) {
        return false;
    }
    AUDIO_EXTENSIONS.contains(&ext)
}

fn is_playlist_extension(ext: &str) -> bool {
    matches!(ext, "m3u" | "m3u8" | "pls")
}

pub fn refresh_track(path: &Path, audiobook: bool) -> Result<Track, String> {
    refresh_track_cancellable(path, audiobook, &AtomicBool::new(false))
}

fn refresh_track_cancellable(
    path: &Path,
    audiobook: bool,
    cancel: &AtomicBool,
) -> Result<Track, String> {
    if path.to_str().is_none() {
        return Err(NOT_UNICODE.to_owned());
    }
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("not a regular file".to_owned());
    }
    let modified_time = file_modified_time(&metadata);
    let modified = modified_time.map(|elapsed| elapsed.as_secs()).unwrap_or(0);
    let modified_nanos =
        modified_time.map(|elapsed| elapsed.as_nanos().min(u64::MAX as u128) as u64);
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let canonical_path = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    if canonical_path.to_str().is_none() {
        return Err(NOT_UNICODE.to_owned());
    }
    let mut track = Track {
        path: canonical_path.clone(),
        audiobook,
        file_size: metadata.len(),
        modified,
        modified_nanos,
        added_at: now,
        ..Track::default()
    };
    let legacy_fingerprint = sampled_fingerprint(path, metadata.len())?;
    let content_digest = full_content_digest_cancellable(path, metadata.len(), cancel)?
        .ok_or_else(|| "the digest was cancelled".to_owned())?;
    let audio_identity = identity::audio_content_identity_cancellable(path, cancel);
    if cancel.load(AtomicOrdering::Relaxed) {
        return Err("the digest was cancelled".to_owned());
    }
    let after_digest = fs::metadata(path).map_err(|error| error.to_string())?;
    if after_digest.len() != metadata.len()
        || file_modified_time(&after_digest) != file_modified_time(&metadata)
    {
        return Err("the file changed while its content identity was being checked".to_owned());
    }
    track.legacy_fingerprint = Some(legacy_fingerprint);
    track.content_digest = Some(content_digest.clone());
    track.audio_digest = audio_identity.as_ref().map(|identity| identity.digest.clone());
    track.id = item_id(&canonical_path, &content_digest);
    track.title = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_owned();

    // A parser that panics on a damaged header (Lofty divides by the duration of
    // an Opus file that holds only a few samples) must end this read, not the caller.
    safety::validate_lofty_input_cancellable(path, Some(cancel))?;
    // MP4 and Matroska can carry video and subtitle streams alongside audio.
    // Read the selected audio stream from the container before asking Lofty for
    // tags so an unsupported MKV tag reader cannot prevent audio import.
    let container_audio = probe_container_audio(path, cancel)?;
    if cancel.load(AtomicOrdering::Relaxed) {
        return Err("metadata reading was cancelled".to_owned());
    }
    let tagged_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Probe::open(path)
            .map_err(|error| format!("could not read media metadata: {error}"))?
            .guess_file_type()
            .map_err(|error| format!("could not identify media file: {error}"))?
            .options(ParseOptions::new().read_cover_art(false))
            .read()
            .map_err(|error| format!("could not read media metadata: {error}"))
    }))
    .unwrap_or_else(|_| {
        Err("could not read media metadata: the file's headers are damaged".to_owned())
    });
    let tagged = match tagged_result {
        Ok(tagged) => Some(tagged),
        Err(_) if container_audio.is_some() => None,
        Err(error) => return Err(error),
    };
    let mut opus = false;
    if let Some(tagged) = tagged.as_ref() {
        let properties = tagged.properties();
        opus = tagged.file_type() == FileType::Opus;
        track.duration = properties.duration().as_secs_f64();
        if audio_identity
            .as_ref()
            .is_some_and(|identity| identity.mp3)
        {
            if let Some(duration) = audio_identity
                .as_ref()
                .and_then(|identity| identity.duration)
            {
                track.duration = duration;
            }
        }
        track.bitrate = properties.overall_bitrate().unwrap_or(0) as u64;
        // Lofty reports the rate the Opus encoder was fed (for example 44100); the
        // file is decoded and played at 48 kHz, which is what the library shows.
        track.sample_rate = if opus {
            OPUS_PLAYBACK_RATE
        } else {
            properties.sample_rate().unwrap_or(0)
        };
        track.channels = properties.channels().unwrap_or(0) as u32;
    }

    if let Some(tagged) = tagged.as_ref() {
        if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        track.title = tag
            .title()
            .map(|value| cap_text(value.into_owned(), MAX_TAG_TEXT_CHARS))
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(track.title);
        track.artist = tag
            .artist()
            .map(|value| cap_text(value.into_owned(), MAX_TAG_TEXT_CHARS))
            .unwrap_or_default();
        track.album = tag
            .album()
            .map(|value| cap_text(value.into_owned(), MAX_TAG_TEXT_CHARS))
            .unwrap_or_default();
        track.genre = tag
            .genre()
            .map(|value| cap_text(value.into_owned(), MAX_TAG_TEXT_CHARS))
            .unwrap_or_default();
        track.track_number = tag.track().unwrap_or(0);
        track.year = tag.year().map(|year| year.to_string()).unwrap_or_default();
        track.comment = tag
            .comment()
            .map(|value| cap_text(value.into_owned(), MAX_COMMENT_CHARS))
            .unwrap_or_default();
        track.album_artist = tag_text(tag, &ItemKey::AlbumArtist);
        track.composer = tag_text(tag, &ItemKey::Composer);
        track.disc = tag.disk().unwrap_or(0);
        track.narrator = narrator_text(tag);
        track.replaygain_track_db = replaygain_value(tag.get_string(&ItemKey::ReplayGainTrackGain));
        track.replaygain_album_db = replaygain_value(tag.get_string(&ItemKey::ReplayGainAlbumGain));
        track.replaygain_track_peak =
            replaygain_value(tag.get_string(&ItemKey::ReplayGainTrackPeak));
        track.replaygain_album_peak =
            replaygain_value(tag.get_string(&ItemKey::ReplayGainAlbumPeak));
        if opus {
            // Opus files carry R128_*_GAIN instead of REPLAYGAIN_*; real
            // ReplayGain tags, when a file has both, win.
            track.replaygain_track_db = track
                .replaygain_track_db
                .or_else(|| r128_gain_db(unknown_item_text(tag, "R128_TRACK_GAIN")));
            track.replaygain_album_db = track
                .replaygain_album_db
                .or_else(|| r128_gain_db(unknown_item_text(tag, "R128_ALBUM_GAIN")));
        }
        }
    }
    if let Some(opened) = container_audio {
        if let Some(properties) = container_audio::audio_properties(&opened) {
            // Use the chosen audio stream's facts even when the container also
            // has a video track or a subtitle stream with its own duration.
            apply_container_audio_properties(&mut track, properties);
        }
        fill_missing_track_tags_from_container(&mut track, path, opened);
    }
    let after = fs::metadata(path).map_err(|error| error.to_string())?;
    if after.len() != metadata.len() || file_modified_time(&after) != modified_time {
        return Err("media file changed while its metadata was being read".to_owned());
    }
    Ok(track)
}

fn is_container_audio_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "m4a" | "m4b" | "mp4" | "mkv" | "mka" | "webm"
            )
        })
}

/// Opens a bounded Symphonia container probe for formats where other tracks can
/// supply misleading file-wide properties. The caller checks its borrowed
/// cancellation token before and after this bounded probe.
fn probe_container_audio(
    path: &Path,
    cancel: &AtomicBool,
) -> Result<Option<OpenedAudio>, String> {
    if !is_container_audio_path(path) {
        return Ok(None);
    }
    if cancel.load(AtomicOrdering::Relaxed) {
        return Err("metadata reading was cancelled".to_owned());
    }
    let mut input = File::open(path).map_err(|error| error.to_string())?;
    let Some(kind) = container_audio::detect_container(&mut input)? else {
        return Ok(None);
    };
    if cancel.load(AtomicOrdering::Relaxed) {
        return Err("metadata reading was cancelled".to_owned());
    }
    let opened = container_audio::open_container(input, kind, None)?;
    if cancel.load(AtomicOrdering::Relaxed) {
        return Err("metadata reading was cancelled".to_owned());
    }
    Ok(Some(opened))
}

fn apply_container_audio_properties(track: &mut Track, properties: AudioProperties) {
    track.sample_rate = properties.sample_rate;
    track.channels = u32::from(properties.channels);
    track.duration = properties
        .duration
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0);
}

/// Fills only legacy container facts that older library rows left empty. This
/// does not hash, retag, or change the row's identity or listening state.
pub fn fill_missing_container_audio_properties(track: &mut Track) -> Result<bool, String> {
    let missing = track.duration <= 0.0 || track.sample_rate == 0 || track.channels == 0;
    if !missing || !is_container_audio_path(&track.path) {
        return Ok(false);
    }
    let opened = probe_container_audio(&track.path, &AtomicBool::new(false))?;
    let Some(opened) = opened else {
        return Ok(false);
    };
    let Some(properties) = container_audio::audio_properties(&opened) else {
        return Ok(false);
    };
    if track.duration <= 0.0 {
        if let Some(duration) = properties.duration {
            track.duration = duration.as_secs_f64();
        }
    }
    if track.sample_rate == 0 {
        track.sample_rate = properties.sample_rate;
    }
    if track.channels == 0 {
        track.channels = u32::from(properties.channels);
    }
    Ok(true)
}

fn fill_missing_track_tags_from_container(track: &mut Track, path: &Path, mut opened: OpenedAudio) {
    let file_title = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let metadata = opened.reader.metadata();
    let Some(revision) = metadata.current() else {
        return;
    };
    for tag in revision.tags().iter().take(MAX_NARRATOR_SCAN_ITEMS) {
        let value = match &tag.value {
            Value::String(value) => value.trim().to_owned(),
            Value::SignedInt(value) => value.to_string(),
            Value::UnsignedInt(value) => value.to_string(),
            Value::Float(value) => value.to_string(),
            Value::Boolean(value) => value.to_string(),
            Value::Flag | Value::Binary(_) => continue,
        };
        if value.is_empty() {
            continue;
        }
        match tag.std_key {
            Some(StandardTagKey::TrackTitle)
                if track.title.is_empty() || track.title == file_title =>
            {
                track.title = cap_text(value, MAX_TAG_TEXT_CHARS);
            }
            Some(StandardTagKey::Artist) if track.artist.is_empty() => {
                track.artist = cap_text(value, MAX_TAG_TEXT_CHARS);
            }
            Some(StandardTagKey::Album) if track.album.is_empty() => {
                track.album = cap_text(value, MAX_TAG_TEXT_CHARS);
            }
            Some(StandardTagKey::Genre) if track.genre.is_empty() => {
                track.genre = cap_text(value, MAX_TAG_TEXT_CHARS);
            }
            Some(StandardTagKey::Comment | StandardTagKey::Description)
                if track.comment.is_empty() =>
            {
                track.comment = cap_text(value, MAX_COMMENT_CHARS);
            }
            Some(StandardTagKey::AlbumArtist) if track.album_artist.is_empty() => {
                track.album_artist = cap_text(value, MAX_TAG_TEXT_CHARS);
            }
            Some(StandardTagKey::Composer) if track.composer.is_empty() => {
                track.composer = cap_text(value, MAX_TAG_TEXT_CHARS);
            }
            Some(StandardTagKey::TrackNumber) if track.track_number == 0 => {
                track.track_number = value
                    .split('/')
                    .next()
                    .and_then(|number| number.trim().parse().ok())
                    .unwrap_or(0);
            }
            Some(StandardTagKey::DiscNumber) if track.disc == 0 => {
                track.disc = value
                    .split('/')
                    .next()
                    .and_then(|number| number.trim().parse().ok())
                    .unwrap_or(0);
            }
            Some(StandardTagKey::Date | StandardTagKey::ReleaseDate) if track.year.is_empty() => {
                track.year = cap_text(value, MAX_TAG_TEXT_CHARS);
            }
            _ => {}
        }
    }
}

/// A trimmed, bounded text tag; empty when the file has none.
fn tag_text(tag: &lofty::tag::Tag, key: &ItemKey) -> String {
    tag.get_string(key)
        .map(|value| cap_text(value.trim().to_owned(), MAX_TAG_TEXT_CHARS))
        .unwrap_or_default()
}

/// Most tag items looked at when searching for the narrator or an Opus gain
/// item; a file cannot make the scan walk an unbounded list.
const MAX_NARRATOR_SCAN_ITEMS: usize = 4096;

/// Whether a tag key that Lofty has no named variant for ("unknown" key)
/// names the narrator: an ID3 user text frame or Vorbis comment called
/// NARRATOR (or NARRATED_BY), an iTunes free-form atom of that name, or the
/// `©nrt` atom some audiobook tools write into MP4 files.
fn is_narrator_key(name: &str) -> bool {
    let base = name.rsplit(':').next().unwrap_or(name).trim();
    base.eq_ignore_ascii_case("narrator")
        || base.eq_ignore_ascii_case("narrated_by")
        || base.eq_ignore_ascii_case("narratedby")
        || base == "\u{a9}nrt"
}

/// The narrator named by the file's tags, empty when it names none. Lofty has no
/// narrator item key, so the items it keeps under their raw names are searched.
fn narrator_text(tag: &lofty::tag::Tag) -> String {
    tag.items()
        .take(MAX_NARRATOR_SCAN_ITEMS)
        .filter(|item| matches!(item.key(), ItemKey::Unknown(name) if is_narrator_key(name)))
        .filter_map(|item| item.value().text())
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(|value| cap_text(value.to_owned(), MAX_TAG_TEXT_CHARS))
        .unwrap_or_default()
}

/// The text of the first item that Lofty keeps under its raw name `name`
/// (compared without regard to case), for keys it has no named variant for.
fn unknown_item_text<'a>(tag: &'a lofty::tag::Tag, name: &str) -> Option<&'a str> {
    tag.items()
        .take(MAX_NARRATOR_SCAN_ITEMS)
        .filter(
            |item| matches!(item.key(), ItemKey::Unknown(key) if key.eq_ignore_ascii_case(name)),
        )
        .find_map(|item| item.value().text())
}

/// The ReplayGain (dB) equal to an Opus `R128_TRACK_GAIN` or `R128_ALBUM_GAIN`
/// comment (RFC 7845 section 5.2.1): a signed 16-bit integer in Q7.8 dB written
/// as decimal text, aimed at -23 LUFS and relative to the audio after the header
/// output gain, so `dB = q78 / 256 + 5` on the -18 LUFS ReplayGain scale. The
/// result is held to the window the player applies. `None` for anything that is
/// not such an integer (a fraction, an exponent, "dB", out of the 16-bit range).
fn r128_gain_db(value: Option<&str>) -> Option<f32> {
    let text = value?.trim();
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    if digits.is_empty() || digits.len() > 6 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let q78: i16 = text.parse().ok()?;
    Some(
        (f32::from(q78) / 256. + R128_TO_REPLAYGAIN_DB).clamp(REPLAYGAIN_MIN_DB, REPLAYGAIN_MAX_DB),
    )
}

/// Bounds tag text copied into the library so one oversized tag cannot push
/// the saved profile past its size limit. Truncates on a character boundary.
fn cap_text(value: String, max_chars: usize) -> String {
    // Bytes are never fewer than characters, so a short byte length is always in bounds.
    if value.len() <= max_chars {
        return value;
    }
    value.chars().take(max_chars).collect()
}

fn file_modified_time(metadata: &fs::Metadata) -> Option<std::time::Duration> {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
}

/// Returns whether a refreshed file is still the same record content.
/// Legacy IDs are accepted only under their saved size/mtime and sampled-fingerprint
/// assumptions; all refreshed records compare their full-input digest.
pub fn same_content(existing: &Track, fresh: &Track) -> bool {
    match (&existing.content_digest, &fresh.content_digest) {
        (Some(old), Some(new)) => existing.file_size == fresh.file_size && old == new,
        (None, Some(_)) => fresh
            .legacy_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| {
                existing.file_size == fresh.file_size
                    && existing.modified == fresh.modified
                    && existing.id == fingerprint
            }),
        _ => false,
    }
}

/// Validates a library record before playback. Call from a worker: changed or
/// legacy files may require a full content read to refresh their digest.
pub fn validate_for_playback(track: &Track) -> Result<Track, String> {
    if track.superseded {
        let Some(expected_digest) = &track.content_digest else {
            return Err(
                "This superseded legacy record cannot be automatically verified. Use Locate file to confirm a candidate; saved progress has been retained."
                    .into(),
            );
        };
        let mut refreshed = refresh_track(&track.path, track.audiobook)?;
        if refreshed.file_size != track.file_size
            || refreshed.content_digest.as_deref() != Some(expected_digest)
        {
            return Err(
                "This superseded record no longer matches the file at its saved path; its progress was retained. Use Locate file to reconnect it."
                    .into(),
            );
        }
        refreshed.id = track.id.clone();
        refreshed.superseded = false;
        refreshed.preserve_library_tags(track);
        return Ok(refreshed);
    }

    let metadata = fs::metadata(&track.path)
        .map_err(|error| format!("Cannot read {}: {error}", track.path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", track.path.display()));
    }
    let actual_modified = file_modified_time(&metadata);
    let actual_seconds = actual_modified.map(|time| time.as_secs()).unwrap_or(0);
    let metadata_matches = metadata.len() == track.file_size
        && actual_seconds == track.modified
        && track.modified_nanos.is_none_or(|expected| {
            actual_modified.map(|time| time.as_nanos().min(u64::MAX as u128) as u64)
                == Some(expected)
        });

    if let Some(expected_digest) = &track.content_digest {
        if metadata_matches {
            let mut unchanged = track.clone();
            fill_missing_container_audio_properties(&mut unchanged)?;
            return Ok(unchanged);
        }
        let mut refreshed = refresh_track(&track.path, track.audiobook)?;
        if refreshed.file_size == track.file_size
            && refreshed.content_digest.as_deref() == Some(expected_digest)
        {
            refreshed.id = track.id.clone();
            refreshed.preserve_library_tags(track);
            return Ok(refreshed);
        }
        return Err(
            "This file has changed since it was imported. Rescan its folder before playing; the old progress will not be applied to different content."
                .into(),
        );
    }

    // A v1 profile only stores a sparse sample as its identity. Preserve its
    // resume state only when the recorded metadata and that legacy sample agree;
    // the new full digest is then stored for subsequent checks.
    if !metadata_matches {
        return Err(
            "This legacy file cannot be verified against its saved identity. Rescan it before playing; its saved progress has been retained."
                .into(),
        );
    }
    let expected_fingerprint = track.legacy_fingerprint.as_deref().unwrap_or(&track.id);
    if expected_fingerprint != track.id.as_str() {
        return Err(
            "This file has changed since it was imported. Rescan its folder before playing; the old progress will not be applied to different content."
                .into(),
        );
    }
    let mut refreshed = refresh_track(&track.path, track.audiobook)?;
    if refreshed.legacy_fingerprint.as_deref() != Some(expected_fingerprint) {
        return Err(
            "This file has changed since it was imported. Rescan its folder before playing; the old progress will not be applied to different content."
                .into(),
        );
    }
    refreshed.id = track.id.clone();
    refreshed.preserve_library_tags(track);
    Ok(refreshed)
}

/// Checks a selected relink target. Known full digests must match exactly.
/// Legacy records may match only their old sample and size, so the caller must
/// ask the user before attaching the original progress to that candidate.
pub fn relink_candidate(track: &Track, path: &Path) -> Result<(Track, bool), String> {
    let mut candidate = refresh_track(path, track.audiobook)?;
    if let Some(expected_digest) = &track.content_digest {
        if candidate.file_size != track.file_size
            || candidate.content_digest.as_deref() != Some(expected_digest)
        {
            return Err(
                "The selected file has different content; saved progress was not reassigned."
                    .into(),
            );
        }
        candidate.id = track.id.clone();
        return Ok((candidate, false));
    }

    if track.file_size != candidate.file_size
        || candidate.legacy_fingerprint.as_deref() != Some(track.id.as_str())
    {
        return Err(
            "The selected file does not match the legacy saved identity; saved progress was not reassigned."
                .into(),
        );
    }
    candidate.id = track.id.clone();
    Ok((candidate, true))
}

// -- Reconnecting a moved folder --------------------------------------------

/// Most directory entries (files and folders) the search of the new place looks at.
const MAX_RELOCATE_WALK_ENTRIES: usize = 200_000;
/// How far the length of a record without a digest may differ from the length
/// of the file that is offered for it, in seconds.
const RELOCATE_DURATION_SLACK: f64 = 0.05;
/// The read errors a report keeps.
const MAX_RELOCATE_ERRORS: usize = 5;

/// What the library knows about a file a folder move looks for.
#[derive(Clone, Debug, PartialEq)]
pub struct RelocateWant {
    pub id: String,
    pub path: PathBuf,
    pub file_size: u64,
    /// The full content digest; `None` for a record from an old profile.
    pub digest: Option<String>,
    /// The sampled fingerprint a record without a digest is known by.
    pub fingerprint: Option<String>,
    pub duration: f64,
}

impl RelocateWant {
    pub fn of(track: &Track) -> Self {
        Self {
            id: track.id.clone(),
            path: track.path.clone(),
            file_size: track.file_size,
            digest: track.content_digest.clone(),
            fingerprint: track.content_digest.is_none().then(|| {
                track
                    .legacy_fingerprint
                    .clone()
                    .unwrap_or_else(|| track.id.clone())
            }),
            duration: track.duration,
        }
    }
}

/// A file found below the new folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundFile {
    pub path: PathBuf,
    pub size: u64,
}

/// How a file below the new folder came to be offered for a record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchHow {
    /// The same path below the new folder as below the old one.
    SamePlace,
    /// The only file of that name and size below the new folder.
    NameAndSize,
}

/// The candidates for a folder move, before any file content was read.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RelocationPlan {
    /// `(index into the wants, index into the found files, how)`.
    pub matches: Vec<(usize, usize, MatchHow)>,
    /// Wants for which nothing below the new folder fits.
    pub missing: Vec<usize>,
    /// Wants with several (or contested) candidates: refused.
    pub ambiguous: Vec<usize>,
    /// Wants whose own place holds a file of another size and nothing else fits.
    pub different: Vec<usize>,
}

/// The separator `fold_key` leaves between path parts.
fn key_separator(windows: bool) -> char {
    if windows { '\\' } else { '/' }
}

/// The folded form of `path` used to compare paths (see `state::removal_key`).
fn fold_key(path: &Path, windows: bool) -> String {
    crate::state::fold_removal_key(&path.to_string_lossy(), windows)
}

/// The part of `path_key` below `root_key` (both folded by `fold_key`), without
/// a leading separator; `None` when the path is not below the root.
pub fn relative_key<'a>(root_key: &str, path_key: &'a str, windows: bool) -> Option<&'a str> {
    let separator = key_separator(windows);
    let root = root_key.trim_end_matches(separator);
    let rest = path_key.strip_prefix(root)?;
    let rest = rest.strip_prefix(separator)?;
    (!rest.is_empty()).then_some(rest)
}

/// The path of `want`'s file below the new folder, as a native path made from
/// the relative key (`new_root` followed by its parts).
fn path_below(new_root: &Path, relative: &str, windows: bool) -> PathBuf {
    let mut path = new_root.to_path_buf();
    for part in relative.split(key_separator(windows)) {
        if !part.is_empty() {
            path.push(part);
        }
    }
    path
}

/// Chooses, for each wanted file, the file below the new folder that may be it.
/// Pass 1 takes the file at the same relative path when its size is the
/// record's; pass 2, for the wants left, takes the only file anywhere below
/// the new folder with the record's file name and size, provided no other
/// want asks for that name and size too. Nothing is read from disk; the
/// candidates still have to be verified (see `relocate_files`). Paths are
/// compared folded (`windows`: separators, `\\?\` prefix and letter case).
pub fn plan_relocation(
    wants: &[RelocateWant],
    old_root: &Path,
    new_root: &Path,
    found: &[FoundFile],
    windows: bool,
) -> RelocationPlan {
    let separator = key_separator(windows);
    let old_key = fold_key(old_root, windows);
    let new_key = fold_key(new_root, windows);
    let mut by_place: HashMap<&str, usize> = HashMap::new();
    let found_keys: Vec<String> = found
        .iter()
        .map(|file| fold_key(&file.path, windows))
        .collect();
    let mut by_name: HashMap<(&str, u64), Vec<usize>> = HashMap::new();
    for (index, key) in found_keys.iter().enumerate() {
        let Some(relative) = relative_key(&new_key, key, windows) else {
            continue;
        };
        by_place.insert(relative, index);
        let name = relative.rsplit(separator).next().unwrap_or(relative);
        by_name
            .entry((name, found[index].size))
            .or_default()
            .push(index);
    }
    let mut plan = RelocationPlan::default();
    let mut taken = vec![false; found.len()];
    let mut size_differs = vec![false; wants.len()];
    let mut second_pass = Vec::new();
    let want_keys: Vec<String> = wants
        .iter()
        .map(|want| fold_key(&want.path, windows))
        .collect();
    for (index, want) in wants.iter().enumerate() {
        let Some(relative) = relative_key(&old_key, &want_keys[index], windows) else {
            plan.missing.push(index);
            continue;
        };
        match by_place.get(relative) {
            Some(&hit) if found[hit].size == want.file_size && !taken[hit] => {
                taken[hit] = true;
                plan.matches.push((index, hit, MatchHow::SamePlace));
            }
            Some(_) => {
                size_differs[index] = true;
                second_pass.push(index);
            }
            None => second_pass.push(index),
        }
    }
    // How many of the wants left ask for each name and size.
    let mut asking: HashMap<(&str, u64), usize> = HashMap::new();
    for &index in &second_pass {
        let relative = relative_key(&old_key, &want_keys[index], windows).unwrap_or("");
        let name = relative.rsplit(separator).next().unwrap_or(relative);
        *asking.entry((name, wants[index].file_size)).or_default() += 1;
    }
    for &index in &second_pass {
        let relative = relative_key(&old_key, &want_keys[index], windows).unwrap_or("");
        let name = relative.rsplit(separator).next().unwrap_or(relative);
        let key = (name, wants[index].file_size);
        let free: Vec<usize> = by_name
            .get(&key)
            .map(|list| list.iter().copied().filter(|&hit| !taken[hit]).collect())
            .unwrap_or_default();
        match free.as_slice() {
            [] if size_differs[index] => plan.different.push(index),
            [] => plan.missing.push(index),
            [hit] if asking.get(&key).copied() == Some(1) => {
                plan.matches.push((index, *hit, MatchHow::NameAndSize));
            }
            _ => plan.ambiguous.push(index),
        }
    }
    plan.matches.sort_unstable();
    plan.missing.sort_unstable();
    plan.ambiguous.sort_unstable();
    plan.different.sort_unstable();
    plan
}

/// A file whose record can point at its new place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedMove {
    pub id: String,
    /// The path the record has now.
    pub old_path: PathBuf,
    /// The canonical path of the verified file.
    pub new_path: PathBuf,
    pub file_size: u64,
    pub modified: u64,
    pub modified_nanos: Option<u64>,
    /// The verified content digest (`None`: accepted on the sampled evidence).
    pub digest: Option<String>,
}

/// What `relocate_files` found out.
#[derive(Debug, Default)]
pub struct RelocateReport {
    pub accepted: Vec<AcceptedMove>,
    /// How many library files were looked for.
    pub total: usize,
    pub missing: usize,
    pub ambiguous: usize,
    /// A candidate exists but its size or content is not the record's.
    pub different: usize,
    /// A candidate could not be read.
    pub unreadable: usize,
    /// Accepted without a digest: size, sampled fingerprint and length agree.
    pub by_sample: usize,
    /// The job was stopped on request; `accepted` holds the verified files so far.
    pub cancelled: bool,
    /// The new folder holds more than `MAX_RELOCATE_WALK_ENTRIES` entries.
    pub truncated: bool,
    /// The first few read problems.
    pub errors: Vec<String>,
}

/// Walks `root` once and collects its audio files (links and OS metadata
/// folders are skipped), looking at no more than `limit` directory entries.
/// Returns whether the limit stopped the walk, or `None` when `cancel` did.
fn walk_audio_files(
    root: &Path,
    limit: usize,
    cancel: &AtomicBool,
    sink: &mut ProgressSink<'_>,
    files: &mut Vec<FoundFile>,
) -> Option<bool> {
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if cancel.load(AtomicOrdering::Relaxed) {
            return None;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let found = files.len();
        sink.emit(false, || ScanProgress {
            phase: ScanPhase::Collecting,
            found,
            done: 0,
            total: 0,
            current: progress_name(&dir),
        });
        for entry in entries.flatten() {
            if cancel.load(AtomicOrdering::Relaxed) {
                return None;
            }
            seen += 1;
            if seen > limit {
                return Some(true);
            }
            if is_os_metadata_name(&entry.file_name()) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                if depth < MAX_SCAN_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if kind.is_file() {
                let ext = path
                    .extension()
                    .and_then(|value| value.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if is_audio_extension(&ext, &[]) {
                    if let Ok(metadata) = entry.metadata() {
                        files.push(FoundFile {
                            path,
                            size: metadata.len(),
                        });
                    }
                }
            }
        }
    }
    Some(false)
}

/// The modification time of `metadata` as the library records it.
fn modified_fields(metadata: &fs::Metadata) -> (u64, Option<u64>) {
    let time = file_modified_time(metadata);
    (
        time.map(|elapsed| elapsed.as_secs()).unwrap_or(0),
        time.map(|elapsed| elapsed.as_nanos().min(u64::MAX as u128) as u64),
    )
}

/// Looks for the files of `wants` (library records below `old_root`) below
/// `new_root` and verifies every candidate by its content: a record with a full
/// digest takes a file only when the digest is identical; one without takes a
/// file whose size, sampled fingerprint and length (within 50 ms) agree. Never
/// writes to disk. Blocking: call from a worker. `cancel` is looked at between
/// directory entries, between files and between blocks of a file being
/// hashed; the report then holds the files verified until then.
pub fn relocate_files(
    wants: &[RelocateWant],
    old_root: &Path,
    new_root: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(ScanProgress),
) -> RelocateReport {
    let windows = cfg!(windows);
    let mut report = RelocateReport {
        total: wants.len(),
        ..RelocateReport::default()
    };
    if wants.is_empty() {
        return report;
    }
    let mut sink = ProgressSink::new(progress);
    let mut found = Vec::new();
    match walk_audio_files(
        new_root,
        MAX_RELOCATE_WALK_ENTRIES,
        cancel,
        &mut sink,
        &mut found,
    ) {
        Some(truncated) => report.truncated = truncated,
        None => {
            report.cancelled = true;
            return report;
        }
    }
    if report.truncated {
        // Files the walk did not reach are still looked for at their own place.
        let old_key = fold_key(old_root, windows);
        let known: HashSet<PathBuf> = found.iter().map(|file| file.path.clone()).collect();
        for want in wants {
            let key = fold_key(&want.path, windows);
            let Some(relative) = relative_key(&old_key, &key, windows) else {
                continue;
            };
            let candidate = path_below(new_root, relative, windows);
            if known.contains(&candidate) {
                continue;
            }
            if let Ok(metadata) = fs::metadata(&candidate) {
                if metadata.is_file() {
                    found.push(FoundFile {
                        path: candidate,
                        size: metadata.len(),
                    });
                }
            }
        }
    }
    let plan = plan_relocation(wants, old_root, new_root, &found, windows);
    report.missing = plan.missing.len();
    report.ambiguous = plan.ambiguous.len();
    report.different = plan.different.len();
    let total = plan.matches.len();
    sink.emit(true, || ScanProgress {
        phase: ScanPhase::Reading,
        found: wants.len(),
        done: 0,
        total,
        current: String::new(),
    });
    for (done, &(want_index, found_index, _)) in plan.matches.iter().enumerate() {
        if cancel.load(AtomicOrdering::Relaxed) {
            report.cancelled = true;
            break;
        }
        let want = &wants[want_index];
        let file = &found[found_index];
        sink.emit(false, || ScanProgress {
            phase: ScanPhase::Reading,
            found: wants.len(),
            done,
            total,
            current: progress_name(&file.path),
        });
        // One malformed file must not take the whole job down.
        let verdict = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            verify_candidate(want, &file.path, cancel)
        }))
        .unwrap_or_else(|_| Err("internal error while reading this file".to_owned()));
        match verdict {
            Ok(Verdict::Accepted { moved, by_sample }) => {
                if by_sample {
                    report.by_sample += 1;
                }
                report.accepted.push(moved);
            }
            Ok(Verdict::Different) => report.different += 1,
            Ok(Verdict::Cancelled) => {
                report.cancelled = true;
                break;
            }
            Err(error) => {
                report.unreadable += 1;
                if report.errors.len() < MAX_RELOCATE_ERRORS {
                    report
                        .errors
                        .push(format!("{}: {error}", display_path(&file.path)));
                }
            }
        }
    }
    if !report.cancelled {
        sink.emit(true, || ScanProgress {
            phase: ScanPhase::Reading,
            found: wants.len(),
            done: total,
            total,
            current: String::new(),
        });
    }
    report
}

enum Verdict {
    Accepted {
        moved: AcceptedMove,
        by_sample: bool,
    },
    Different,
    Cancelled,
}

/// Reads `candidate` and decides whether it is the file `want` describes.
fn verify_candidate(
    want: &RelocateWant,
    candidate: &Path,
    cancel: &AtomicBool,
) -> Result<Verdict, String> {
    if candidate.to_str().is_none() {
        return Err(NOT_UNICODE.to_owned());
    }
    let before = fs::metadata(candidate).map_err(|error| error.to_string())?;
    if !before.is_file() {
        return Err("not a regular file".to_owned());
    }
    if before.len() != want.file_size {
        return Ok(Verdict::Different);
    }
    let by_sample = match &want.digest {
        Some(expected) => {
            let Some(digest) = full_content_digest_cancellable(candidate, before.len(), cancel)?
            else {
                return Ok(Verdict::Cancelled);
            };
            if &digest != expected {
                return Ok(Verdict::Different);
            }
            false
        }
        None => {
            let Some(expected) = want.fingerprint.as_deref() else {
                return Ok(Verdict::Different);
            };
            if want.duration.is_nan() || want.duration <= 0. {
                return Ok(Verdict::Different);
            }
            if cancel.load(AtomicOrdering::Relaxed) {
                return Ok(Verdict::Cancelled);
            }
            let fresh = refresh_track(candidate, false)?;
            if fresh.file_size != want.file_size
                || fresh.legacy_fingerprint.as_deref() != Some(expected)
                || !fresh.duration.is_finite()
                || (fresh.duration - want.duration).abs() > RELOCATE_DURATION_SLACK + 1e-9
            {
                return Ok(Verdict::Different);
            }
            true
        }
    };
    let after = fs::metadata(candidate).map_err(|error| error.to_string())?;
    if after.len() != before.len() || file_modified_time(&after) != file_modified_time(&before) {
        return Err("the file changed while it was being checked".to_owned());
    }
    let new_path = fs::canonicalize(candidate).map_err(|error| error.to_string())?;
    if new_path.to_str().is_none() {
        return Err(NOT_UNICODE.to_owned());
    }
    let (modified, modified_nanos) = modified_fields(&after);
    Ok(Verdict::Accepted {
        moved: AcceptedMove {
            id: want.id.clone(),
            old_path: want.path.clone(),
            new_path,
            file_size: after.len(),
            modified,
            modified_nanos,
            digest: want.digest.clone(),
        },
        by_sample,
    })
}

/// The prefix of the tag writer's answer when an edit would change nothing.
pub const NOTHING_TO_CHANGE: &str = "Nothing to change";

/// The library record for a file whose bytes changed only because its tags were
/// rewritten: everything read from the file is `refreshed`; everything the
/// library owns (the record id that progress, bookmarks, ratings, favourites,
/// queue and playlists point at, when it was added, the audiobook flag and any
/// library tag edits) stays that of `old`.
pub fn carry_identity(old: &Track, mut refreshed: Track) -> Track {
    refreshed.id = old.id.clone();
    refreshed.added_at = old.added_at;
    refreshed.audiobook |= old.audiobook;
    refreshed.superseded = false;
    refreshed.preserve_library_tags(old);
    refreshed
}

/// Whether `fresh`, read from the path of `old` with different file bytes,
/// contains the same demuxed audio and playback-affecting codec configuration.
/// Missing historical or unsupported-format digests cannot prove identity.
pub fn same_track_after_external_edit(old: &Track, fresh: &Track) -> bool {
    if old.superseded || fresh.superseded {
        return false;
    }
    old.audio_digest.is_some() && old.audio_digest == fresh.audio_digest
}

/// Writes `edit` into the audio file of `track`, then returns the library
/// record for the changed file with the identity of `track` (see
/// `carry_identity`). Blocking: call from a worker. The file is checked first:
/// a record that no longer describes the file on disk, or an earlier version of
/// a file, is not edited, so saved progress is never attached to other audio.
pub fn write_tags_and_refresh(
    track: &Track,
    edit: &crate::tagwrite::TagEdit,
) -> Result<Track, String> {
    if track.superseded {
        return Err(
            "This entry is for an earlier version of the file. Edit the current version instead."
                .to_owned(),
        );
    }
    validate_for_playback(track).map_err(|error| format!("Not changed: {error}"))?;
    crate::tagwrite::write_tags(&track.path, edit)?;
    let refreshed = refresh_track(&track.path, track.audiobook).map_err(|error| {
        format!(
            "The tags were saved, but the library entry could not be refreshed ({error}). Scan the folder to update it."
        )
    })?;
    let mut record = carry_identity(track, refreshed.clone());
    if track.library_edited {
        // The user edited these in the file just now, so the list shows them
        // even where library tags otherwise take precedence.
        adopt_edited_fields(&mut record, &refreshed, edit);
    }
    Ok(record)
}

/// Copies the tag fields `edit` set from the freshly read `file` record.
fn adopt_edited_fields(record: &mut Track, file: &Track, edit: &crate::tagwrite::TagEdit) {
    if edit.title.is_some() {
        record.title = file.title.clone();
    }
    if edit.artist.is_some() {
        record.artist = file.artist.clone();
    }
    if edit.album.is_some() {
        record.album = file.album.clone();
    }
    if edit.genre.is_some() {
        record.genre = file.genre.clone();
    }
    if edit.year.is_some() {
        record.year = file.year.clone();
    }
    if edit.comment.is_some() {
        record.comment = file.comment.clone();
    }
    if edit.track.is_some() {
        record.track_number = file.track_number;
    }
}

fn replaygain_value(value: Option<&str>) -> Option<f32> {
    let value = value?.trim();
    let numeric = value
        .strip_suffix("dB")
        .or_else(|| value.strip_suffix("db"))
        .unwrap_or(value)
        .trim();
    let parsed = numeric.parse::<f32>().ok()?;
    parsed.is_finite().then_some(parsed)
}

fn sampled_fingerprint(path: &Path, size: u64) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Fnv64::new();
    hasher
        .write_all(&size.to_le_bytes())
        .map_err(|error| error.to_string())?;

    let sample_len = size.min(FINGERPRINT_SAMPLE as u64) as usize;
    let mut sample = vec![0u8; sample_len];
    file.read_exact(&mut sample)
        .map_err(|error| error.to_string())?;
    hasher
        .write_all(&sample)
        .map_err(|error| error.to_string())?;
    if size > FINGERPRINT_SAMPLE as u64 {
        let middle = size / 2 - (FINGERPRINT_SAMPLE as u64 / 2);
        file.seek(std::io::SeekFrom::Start(middle))
            .map_err(|error| error.to_string())?;
        file.read_exact(&mut sample)
            .map_err(|error| error.to_string())?;
        hasher
            .write_all(&sample)
            .map_err(|error| error.to_string())?;
        file.seek(std::io::SeekFrom::End(-(FINGERPRINT_SAMPLE as i64)))
            .map_err(|error| error.to_string())?;
        file.read_exact(&mut sample)
            .map_err(|error| error.to_string())?;
        hasher
            .write_all(&sample)
            .map_err(|error| error.to_string())?;
    }
    Ok(format!("{:016x}", hasher.finish()))
}

#[allow(dead_code)]
fn full_content_digest(path: &Path, size: u64) -> Result<String, String> {
    full_content_digest_cancellable(path, size, &AtomicBool::new(false))?
        .ok_or_else(|| "the digest was cancelled".to_owned())
}

/// The full content digest of `path` (see `Track::content_digest`), read in
/// blocks; `Ok(None)` when `cancel` was set before the whole file was read.
fn full_content_digest_cancellable(
    path: &Path,
    size: u64,
    cancel: &AtomicBool,
) -> Result<Option<String>, String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Fnv64::new();
    hasher
        .write_all(&size.to_le_bytes())
        .map_err(|error| error.to_string())?;

    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        if cancel.load(AtomicOrdering::Relaxed) {
            return Ok(None);
        }
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| "media file size overflow while hashing".to_owned())?;
        if total > size {
            return Err("media file changed while its content was being hashed".to_owned());
        }
        hasher
            .write_all(&buffer[..count])
            .map_err(|error| error.to_string())?;
    }
    if total != size {
        return Err("media file changed while its content was being hashed".to_owned());
    }
    Ok(Some(format!("fnv64-full:{:016x}", hasher.finish())))
}

fn item_id(path: &Path, content_digest: &str) -> String {
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let mut path_hasher = Fnv64::new();
    path_hasher
        .write_all(b"bs-player-path-v2\0")
        .expect("in-memory FNV writer cannot fail");
    #[cfg(unix)]
    path_hasher
        .write_all(canonical.as_os_str().as_bytes())
        .expect("in-memory FNV writer cannot fail");
    #[cfg(windows)]
    for unit in canonical.as_os_str().encode_wide() {
        path_hasher
            .write_all(&unit.to_le_bytes())
            .expect("in-memory FNV writer cannot fail");
    }
    #[cfg(not(any(unix, windows)))]
    path_hasher
        .write_all(canonical.to_string_lossy().as_bytes())
        .expect("in-memory FNV writer cannot fail");

    format!("item-v2:{:016x}:{content_digest}", path_hasher.finish())
}

struct Fnv64(u64);

impl Fnv64 {
    fn new() -> Self {
        Self(0xcbf29ce484222325)
    }
    fn finish(&self) -> u64 {
        self.0
    }
}

impl Write for Fnv64 {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Artwork bytes together with a short human description of where they came from.
#[derive(Debug, Clone)]
pub struct ArtworkSource {
    pub bytes: Vec<u8>,
    pub origin: String,
}

pub fn load_artwork_with_origin(
    track: &Track,
    prefer_embedded: bool,
) -> Result<ArtworkSource, String> {
    let embedded = |path: &Path| {
        embedded_artwork(path).map(|found| {
            found.map(|bytes| ArtworkSource {
                bytes,
                origin: "embedded picture".to_owned(),
            })
        })
    };
    if prefer_embedded {
        if let Some(source) = embedded(&track.path)? {
            return Ok(source);
        }
    }
    if let Some(source) = folder_artwork(&track.path)? {
        return Ok(source);
    }
    if !prefer_embedded {
        // The setting is a preference order: embedded art is still the fallback.
        if let Some(source) = embedded(&track.path)? {
            return Ok(source);
        }
    }
    Err("no embedded or folder artwork found".to_owned())
}

/// The front cover (else the first picture) embedded in the file itself, never
/// a folder image. `Ok(None)`: the file has no picture.
pub fn embedded_artwork(path: &Path) -> Result<Option<Vec<u8>>, String> {
    safety::validate_lofty_input(path)?;
    let tagged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Probe::open(path)
            .map_err(|error| error.to_string())?
            .guess_file_type()
            .map_err(|error| error.to_string())?
            .options(ParseOptions::new().read_properties(false))
            .read()
            .map_err(|error| error.to_string())
    }))
    .map_err(|_| "media headers are damaged; embedded artwork could not be read".to_owned())??;
    // Look through every tag: the primary tag may carry no picture while another does.
    // Prefer the front cover over a back cover or icon that happens to come first.
    let Some(picture) = tagged
        .tags()
        .iter()
        .flat_map(|tag| tag.pictures())
        .find(|picture| picture.pic_type() == PictureType::CoverFront)
        .or_else(|| tagged.tags().iter().flat_map(|tag| tag.pictures()).next())
    else {
        return Ok(None);
    };
    if picture.data().len() as u64 <= ARTWORK_LIMIT {
        return Ok(Some(picture.data().to_vec()));
    }
    Err("embedded artwork exceeds the 16 MiB limit".to_owned())
}

const ARTWORK_STEMS: [&str; 6] = ["cover", "folder", "front", "albumart", "album", "artwork"];
const ARTWORK_EXTENSIONS: [&str; 6] = ["jpg", "jpeg", "png", "webp", "bmp", "gif"];

/// Priority of a recognised folder-art name (lower wins), or `None` when the name is not
/// one of the well-known cover names. `stem` and `ext` are already lower-case.
fn folder_artwork_rank(stem: &str, ext: usize) -> Option<(u8, usize, usize)> {
    if let Some(index) = ARTWORK_STEMS.iter().position(|known| *known == stem) {
        return Some((0, index, ext));
    }
    // Windows Media Player style names; the large picture beats the small one.
    if stem.starts_with("albumart_") && stem.contains("large") {
        return Some((1, 0, ext));
    }
    if stem == "albumartsmall" {
        return Some((2, 0, ext));
    }
    None
}

/// True for a regular (or symlinked-to-regular) non-empty file within the artwork limit.
fn artwork_file_usable(path: &Path) -> bool {
    matches!(
        fs::metadata(path),
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 && metadata.len() <= ARTWORK_LIMIT
    )
}

fn read_artwork_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => metadata,
        _ => return Ok(None),
    };
    if metadata.len() > ARTWORK_LIMIT {
        return Ok(None);
    }
    let file = File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(metadata.len().min(ARTWORK_LIMIT) as usize);
    file.take(ARTWORK_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.is_empty() || bytes.len() as u64 > ARTWORK_LIMIT {
        return Ok(None);
    }
    Ok(Some(bytes))
}

fn folder_artwork(path: &Path) -> Result<Option<ArtworkSource>, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "media path has no parent directory".to_owned())?;
    // One directory read; an unreadable or missing folder simply has no folder art.
    let Ok(entries) = fs::read_dir(parent) else {
        return Ok(None);
    };
    // (rank, exact file name, path); unrecognised images sort after every ranked one.
    let mut images: Vec<((u8, usize, usize), String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Hidden files and macOS `._` resource forks are never artwork.
        if name.starts_with('.') {
            continue;
        }
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(true) {
            continue;
        }
        let entry_path = entry.path();
        let Some(ext) = entry_path
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
        else {
            continue;
        };
        let Some(ext_index) = ARTWORK_EXTENSIONS.iter().position(|known| *known == ext) else {
            continue;
        };
        let stem = entry_path
            .file_stem()
            .map(|value| value.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let rank = folder_artwork_rank(&stem, ext_index).unwrap_or((u8::MAX, 0, 0));
        images.push((rank, name, entry_path));
    }
    images.sort();
    let mut first_error = None;
    for (rank, name, image) in &images {
        if rank.0 == u8::MAX {
            break;
        }
        match read_artwork_file(image) {
            Ok(Some(bytes)) => {
                return Ok(Some(ArtworkSource {
                    bytes,
                    origin: format!("folder file {name}"),
                }))
            }
            Ok(None) => {}
            Err(error) => first_error = first_error.or(Some(error)),
        }
    }
    // No recognised name: a lone usable image is the folder art; several are ambiguous.
    let mut usable = images.iter().filter(|(_, _, image)| artwork_file_usable(image));
    if let (Some((_, name, image)), None) = (usable.next(), usable.next()) {
        match read_artwork_file(image) {
            Ok(Some(bytes)) => {
                return Ok(Some(ArtworkSource {
                    bytes,
                    origin: format!("folder file {name}"),
                }))
            }
            Ok(None) => {}
            Err(error) => first_error = first_error.or(Some(error)),
        }
    }
    first_error.map_or(Ok(None), Err)
}

pub fn write_playlist(path: &Path, tracks: &[Track], relative: bool) -> Result<(), String> {
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    for track in tracks {
        if track
            .path
            .to_string_lossy()
            .chars()
            .any(|character| character == '\n' || character == '\r')
        {
            return Err(format!(
                "cannot export a path containing a line break: {}",
                track.path.display()
            ));
        }
    }
    let mut output = String::new();
    if ext == "pls" {
        output.push_str("[playlist]\n");
        for (index, track) in tracks.iter().enumerate() {
            output.push_str(&format!(
                "File{}={}\n",
                index + 1,
                playlist_path(path, &track.path, relative)
            ));
            output.push_str(&format!(
                "Title{}={}\n",
                index + 1,
                playlist_title(track.display_title())
            ));
            output.push_str(&format!(
                "Length{}={}\n",
                index + 1,
                track.duration.max(0.0) as u64
            ));
        }
        output.push_str(&format!("NumberOfEntries={}\nVersion=2\n", tracks.len()));
    } else {
        output.push_str("#EXTM3U\n");
        for track in tracks {
            output.push_str(&format!(
                "#EXTINF:{},{}\n",
                track.duration.max(0.0) as u64,
                playlist_title(track.display_title())
            ));
            output.push_str(&playlist_path(path, &track.path, relative));
            output.push('\n');
        }
    }
    fs::write(path, output).map_err(|error| error.to_string())
}

fn playlist_title(title: &str) -> String {
    title.replace(['\r', '\n'], " ")
}

fn playlist_path(playlist: &Path, track: &Path, relative: bool) -> String {
    if relative {
        if let Some(parent) = playlist.parent() {
            if let Ok(value) = pathdiff(parent, track) {
                return value.to_string_lossy().into_owned();
            }
        }
    }
    display_path(track)
}

/// A path as users and other players expect to see it: the Windows
/// extended-length prefix (`\\?\C:\a` and `\\?\UNC\server\share`) is removed.
/// Works string-wise on every platform and leaves ordinary paths untouched.
pub fn display_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        let bytes = rest.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return rest.to_owned();
        }
    }
    text.into_owned()
}

fn pathdiff(base: &Path, target: &Path) -> Result<PathBuf, ()> {
    let base = if base.as_os_str().is_empty() {
        Path::new(".")
    } else {
        base
    };
    let base = fs::canonicalize(base).unwrap_or_else(|_| base.to_owned());
    let target = fs::canonicalize(target).unwrap_or_else(|_| target.to_owned());
    let base_parts: Vec<_> = base.components().collect();
    let target_parts: Vec<_> = target.components().collect();
    let common = base_parts
        .iter()
        .zip(&target_parts)
        .take_while(|(a, b)| a == b)
        .count();
    if common == 0 {
        return Err(());
    }
    let mut result = PathBuf::new();
    for _ in common..base_parts.len() {
        result.push("..");
    }
    for component in &target_parts[common..] {
        result.push(component.as_os_str());
    }
    Ok(result)
}

/// One entry of a playlist: where it points and the name the playlist gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistEntry {
    pub target: PathBuf,
    /// The `#EXTINF` text or PLS `TitleN` of the entry (the `#PLAYLIST` title for
    /// the only entry of an M3U playlist that has one); empty when there is none.
    pub title: String,
}

#[allow(dead_code)]
pub fn read_playlist(path: &Path) -> Result<Vec<PathBuf>, String> {
    Ok(read_playlist_entries(path)?
        .into_iter()
        .map(|entry| entry.target)
        .collect())
}

/// As `read_playlist`, with the name each entry carries.
pub fn read_playlist_entries(path: &Path) -> Result<Vec<PlaylistEntry>, String> {
    read_playlist_entries_with_limits(
        path,
        MAX_PLAYLIST_BYTES,
        MAX_PLAYLIST_LINE_BYTES,
        MAX_PLAYLIST_ENTRIES,
    )
}



/// `text` without `prefix`, when it starts with it in any letter case.
fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .and_then(|_| text.get(prefix.len()..))
}

/// The display text of an `#EXTINF:` line (what follows the length, after the
/// first comma that is not inside quotes, as attributes may hold commas).
fn extinf_title(rest: &str) -> String {
    let mut quoted = false;
    for (index, character) in rest.char_indices() {
        match character {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                return cap_text(rest[index + 1..].trim().to_owned(), MAX_TAG_TEXT_CHARS);
            }
            _ => {}
        }
    }
    String::new()
}

fn read_playlist_entries_with_limits(
    path: &Path,
    max_bytes: usize,
    max_line_bytes: usize,
    max_entries: usize,
) -> Result<Vec<PlaylistEntry>, String> {
    let file = File::open(path).map_err(|error| error.to_string())?;
    let ext = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut reader = BufReader::new(file).take((max_bytes as u64).saturating_add(1));
    let mut total_bytes = 0usize;
    let mut line = Vec::new();
    let mut entries = Vec::<(String, String)>::new();
    let mut indexed = Vec::<(u64, String)>::new();
    let mut titles = HashMap::<u64, String>::new();
    let mut pending_title = String::new();
    let mut playlist_title = String::new();
    let mut parse_pls = false;
    let mut format_decided = false;

    loop {
        line.clear();
        let bytes_read = reader
            .read_until(b'\n', &mut line)
            .map_err(|error| error.to_string())?;
        if bytes_read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(bytes_read);
        if total_bytes > max_bytes {
            return Err("playlist exceeds the 16 MiB byte safety limit".into());
        }
        if line.len() > max_line_bytes {
            return Err("playlist contains a line longer than the 64 KiB safety limit".into());
        }
        // Plain .m3u files are traditionally written in the system ANSI code
        // page; a line that is not UTF-8 is read as Windows-1252.
        let decoded;
        let line = match std::str::from_utf8(&line) {
            Ok(text) => text,
            Err(_) => {
                decoded = decode_windows_1252(&line);
                decoded.as_str()
            }
        };

        if !format_decided {
            let first_non_whitespace = line.trim_start().trim_start_matches('\u{feff}');
            if !first_non_whitespace.is_empty() {
                let starts = |prefix: &str| {
                    first_non_whitespace
                        .get(..prefix.len())
                        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
                };
                // The content decides; a .pls name only breaks the tie unless the
                // text is plainly M3U.
                parse_pls = starts("[playlist]") || (ext == "pls" && !starts("#extm3u"));
                format_decided = true;
            }
        }

        if parse_pls {
            if let Some((key, value)) = line.trim().split_once('=') {
                if let Some(index) = key
                    .trim()
                    .get(..4)
                    .filter(|prefix| prefix.eq_ignore_ascii_case("File"))
                    .and_then(|_| key.trim().get(4..))
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    if indexed.len() >= max_entries {
                        return Err(format!(
                            "playlist exceeds the {max_entries} entry safety limit"
                        ));
                    }
                    indexed.push((index, value.trim().to_owned()));
                } else if let Some(index) = strip_prefix_ci(key.trim(), "Title")
                    .and_then(|number| number.parse::<u64>().ok())
                {
                    if titles.len() < max_entries {
                        titles.insert(index, cap_text(value.trim().to_owned(), MAX_TAG_TEXT_CHARS));
                    }
                }
            }
        } else {
            let line = line.trim().trim_start_matches('\u{feff}');
            if let Some(rest) = strip_prefix_ci(line, "#EXTINF:") {
                pending_title = extinf_title(rest);
            } else if let Some(rest) = strip_prefix_ci(line, "#PLAYLIST:") {
                playlist_title = cap_text(rest.trim().to_owned(), MAX_TAG_TEXT_CHARS);
            } else if !line.is_empty() && !line.starts_with('#') {
                if entries.len() >= max_entries {
                    return Err(format!(
                        "playlist exceeds the {max_entries} entry safety limit"
                    ));
                }
                entries.push((line.to_owned(), std::mem::take(&mut pending_title)));
            }
        }
    }

    if parse_pls {
        indexed.sort_by_key(|(index, _)| *index);
        entries.extend(
            indexed
                .into_iter()
                .map(|(index, value)| (value, titles.remove(&index).unwrap_or_default())),
        );
    }
    if let [(_, title)] = entries.as_mut_slice() {
        if title.is_empty() {
            *title = playlist_title;
        }
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    Ok(entries
        .into_iter()
        .map(|(entry, title)| {
            let entry = file_uri_to_path(&entry).unwrap_or(entry);
            if is_stream_url(&entry) {
                // Kept as text; the importer reports skipped streams once.
                return PlaylistEntry {
                    target: PathBuf::from(entry),
                    title,
                };
            }
            let candidate = PathBuf::from(entry);
            PlaylistEntry {
                target: if candidate.is_absolute() {
                    candidate
                } else {
                    parent.join(candidate)
                },
                title,
            }
        })
        .collect())
}

fn decode_windows_1252(bytes: &[u8]) -> String {
    const HIGH: [char; 32] = [
        '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}',
        '\u{17d}', '\u{8f}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}',
        '\u{2022}', '\u{2013}', '\u{2014}', '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}',
        '\u{153}', '\u{9d}', '\u{17e}', '\u{178}',
    ];
    bytes
        .iter()
        .map(|&byte| match byte {
            0x80..=0x9f => HIGH[usize::from(byte - 0x80)],
            _ => char::from(byte),
        })
        .collect()
}

/// An `http://` or `https://` address (a stream the player can try).
pub fn is_http_url(entry: &str) -> bool {
    ["http://", "https://"].iter().any(|scheme| {
        entry
            .get(..scheme.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
    })
}

/// `scheme://...` entries (http, mms, ...) other than `file://`.
fn is_stream_url(entry: &str) -> bool {
    entry.split_once("://").is_some_and(|(scheme, _)| {
        scheme.len() > 1
            && scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    })
}

/// Turns a `file://` URI into a plain path string, decoding `%XX` escapes.
fn file_uri_to_path(entry: &str) -> Option<String> {
    let rest = entry
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("file://"))
        .map(|_| &entry[7..])?;
    let rest = if let Some(localhost_tail) = rest
        .get(..9)
        .filter(|host| host.eq_ignore_ascii_case("localhost"))
        .and_then(|_| rest.get(9..))
        .filter(|tail| tail.is_empty() || tail.starts_with('/'))
    {
        localhost_tail
    } else if !rest.starts_with('/') {
        // A nonempty authority is a UNC host (file://server/share/file), not a
        // path relative to the playlist's directory.
        return decode_file_uri_path(&format!("//{rest}"));
    } else {
        rest
    };
    decode_file_uri_path(rest)
}

fn decode_file_uri_path(rest: &str) -> Option<String> {
    let bytes = rest.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| rest.get(index + 1..index + 3))
            .flatten()
            .filter(|hex| hex.bytes().all(|digit| digit.is_ascii_hexdigit()))
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escaped {
            Some(value) => {
                decoded.push(value);
                index += 3;
            }
            None => {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
    }
    let text = String::from_utf8(decoded).ok()?;
    // file:///C:/dir/a.mp3 carries the drive after the URI's leading slash.
    let drive = text.as_bytes();
    if drive.len() >= 3 && drive[0] == b'/' && drive[1].is_ascii_alphabetic() && drive[2] == b':' {
        return Some(text[1..].to_owned());
    }
    Some(text)
}

/// Lowercases one character and maps common accented Latin letters (Latin-1
/// and Latin Extended-A) to their base letter, so collation and search treat
/// "Édith" like "Edith". Other characters only get simple Unicode lowercasing;
/// Greek final sigma folds to sigma.
pub fn fold_char(character: char) -> char {
    if character.is_ascii() {
        return character.to_ascii_lowercase();
    }
    match character.to_lowercase().next().unwrap_or(character) {
        'à'..='å' | '\u{101}' | '\u{103}' | '\u{105}' => 'a',
        'ç' | '\u{107}' | '\u{109}' | '\u{10b}' | '\u{10d}' => 'c',
        'ð' | '\u{10f}' | '\u{111}' => 'd',
        'è'..='ë' | '\u{113}' | '\u{115}' | '\u{117}' | '\u{119}' | '\u{11b}' => 'e',
        '\u{11d}' | '\u{11f}' | '\u{121}' | '\u{123}' => 'g',
        '\u{125}' | '\u{127}' => 'h',
        'ì'..='ï' | '\u{129}' | '\u{12b}' | '\u{12d}' | '\u{12f}' | '\u{131}' => 'i',
        '\u{135}' => 'j',
        '\u{137}' => 'k',
        '\u{13a}' | '\u{13c}' | '\u{13e}' | '\u{140}' | '\u{142}' => 'l',
        'ñ' | '\u{144}' | '\u{146}' | '\u{148}' => 'n',
        'ò'..='ö' | 'ø' | '\u{14d}' | '\u{14f}' | '\u{151}' => 'o',
        '\u{155}' | '\u{157}' | '\u{159}' => 'r',
        '\u{15b}' | '\u{15d}' | '\u{15f}' | '\u{161}' => 's',
        '\u{163}' | '\u{165}' | '\u{167}' => 't',
        'ù'..='ü' | '\u{169}' | '\u{16b}' | '\u{16d}' | '\u{16f}' | '\u{171}' | '\u{173}' => 'u',
        '\u{175}' => 'w',
        'ý' | 'ÿ' | '\u{177}' => 'y',
        '\u{17a}' | '\u{17c}' | '\u{17e}' => 'z',
        'ς' => 'σ',
        other => other,
    }
}

/// Search key: `fold_char` over the text, `ß` as "ss", and combining accent
/// marks dropped, so decomposed and precomposed spellings match each other.
pub fn fold_text(text: &str) -> String {
    if text.is_ascii() {
        return text.to_ascii_lowercase();
    }
    let mut folded = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\u{300}'..='\u{36f}' => {}
            'ß' | 'ẞ' => folded.push_str("ss"),
            _ => folded.push(fold_char(character)),
        }
    }
    folded
}

pub fn natural_cmp(mut left: &str, mut right: &str) -> Ordering {
    loop {
        match (left.chars().next(), right.chars().next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                // ASCII digit runs end on UTF-8 boundaries. Compare borrowed
                // slices instead of allocating two Strings per sort comparison.
                let llen = left
                    .as_bytes()
                    .iter()
                    .take_while(|c| c.is_ascii_digit())
                    .count();
                let rlen = right
                    .as_bytes()
                    .iter()
                    .take_while(|c| c.is_ascii_digit())
                    .count();
                let (ldigits, lrest) = left.split_at(llen);
                let (rdigits, rrest) = right.split_at(rlen);
                let ltrim = ldigits.trim_start_matches('0');
                let rtrim = rdigits.trim_start_matches('0');
                let order = ltrim.len().cmp(&rtrim.len()).then_with(|| ltrim.cmp(rtrim));
                if order != Ordering::Equal {
                    return order;
                }
                left = lrest;
                right = rrest;
                let order = ldigits.len().cmp(&rdigits.len());
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = fold_char(x).cmp(&fold_char(y));
                if order != Ordering::Equal {
                    return order;
                }
                left = &left[x.len_utf8()..];
                right = &right[y.len_utf8()..];
            }
        }
    }
}

pub fn reveal(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer.exe");
        // Explorer does not select a file given as an extended-length path.
        command.arg("/select,").arg(display_path(path));
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg("-R").arg(path);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        });
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}
