//! Portable, recoverable session storage. Audio positions are sampled from the sink.
use crate::{
    gui_cache::columns,
    media::{self, Track},
    settings::Settings,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_SESSION_BYTES: usize = 64 * 1024 * 1024;
/// Far beyond any real file, yet small enough for `Duration::from_secs_f64`.
pub const MAX_POSITION_SECONDS: f64 = 1.0e9;
const SESSION_VERSION: u32 = 2;
/// Most removed-file paths the profile remembers; the oldest are dropped first.
pub const MAX_REMOVED_PATHS: usize = 50_000;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Progress {
    pub seconds: f64,
    pub completed: bool,
    pub last_played: u64,
    pub play_count: u64,
    pub file_size: u64,
    pub modified: u64,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Bookmark {
    pub track_id: String,
    pub seconds: f64,
    pub name: String,
    pub note: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Playlist {
    pub name: String,
    pub tracks: Vec<String>,
}
/// The `Session::section` that shows the station list instead of files.
pub const STREAMS_SECTION: &str = "Streams";
/// The current editable playback workspace, backed by the ordered queue.
pub const PLAYING_SECTION: &str = "Playing";
/// Most stations the profile keeps.
pub const MAX_STATIONS: usize = 500;
/// Longest station name, in characters.
pub const MAX_STATION_NAME_CHARS: usize = 100;
/// Longest station address, in bytes (the limit of the stream reader).
pub const MAX_STATION_URL_BYTES: usize = 2048;
/// Longest remembered live title, in characters.
const MAX_STATION_TITLE_CHARS: usize = 256;

/// An internet radio stream the user keeps. It is not a library file: it has no
/// size, digest or duration, and nothing about listening to it is saved except
/// the last title the station announced.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Station {
    /// Stable and unique in the list; renaming or changing the address keeps it.
    pub id: String,
    /// What the user called it; empty until the station tells its own name.
    pub name: String,
    pub url: String,
    /// The last title the station announced (never written in private listening).
    pub last_title: String,
    pub added_at: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowState {
    pub width: f32,
    pub height: f32,
    pub x: f32,
    pub y: f32,
    pub maximized: bool,
}
impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1240.,
            height: 820.,
            x: 80.,
            y: 60.,
            maximized: false,
        }
    }
}

/// Settings are independent preferences. A single malformed value should not
/// make the enclosing profile unreadable; recover each known field against its
/// default and keep the values that still deserialize.
fn deserialize_settings_fields<'de, D>(deserializer: D) -> Result<Settings, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let input = serde_json::Value::deserialize(deserializer)?;
    if let Ok(settings) = serde_json::from_value::<Settings>(input.clone()) {
        return Ok(settings);
    }
    let serde_json::Value::Object(input) = input else {
        return Ok(Settings::default());
    };
    let defaults = serde_json::to_value(Settings::default()).map_err(serde::de::Error::custom)?;
    let Some(defaults) = defaults.as_object() else {
        return Ok(Settings::default());
    };
    // Keep absent fields absent: their serde defaults can intentionally differ
    // from new-profile defaults (for example migrated skip/key preferences).
    let mut recovered = serde_json::Map::new();
    for (key, value) in input {
        if !defaults.contains_key(&key) {
            continue;
        }
        let mut candidate = serde_json::Map::new();
        candidate.insert(key.clone(), value.clone());
        if serde_json::from_value::<Settings>(serde_json::Value::Object(candidate)).is_ok() {
            recovered.insert(key, value);
        }
    }
    serde_json::from_value(serde_json::Value::Object(recovered))
        .map_err(serde::de::Error::custom)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub version: u32,
    pub compact: bool,
    pub fullscreen: bool,
    #[serde(deserialize_with = "deserialize_settings_fields")]
    pub settings: Settings,
    #[serde(with = "shared_library")]
    pub library: Arc<Vec<Track>>,
    pub queue: Vec<String>,
    /// Library imports/rescans must not add unrelated files to a Playing workspace.
    pub playing_workspace: bool,
    pub current: Option<String>,
    pub progress: HashMap<String, Progress>,
    pub bookmarks: Vec<Bookmark>,
    pub playlists: Vec<Playlist>,
    pub favorites: Vec<String>,
    pub ratings: HashMap<String, u8>,
    pub book_speeds: HashMap<String, f32>,
    pub roots: Vec<(PathBuf, bool)>,
    pub volume: f32,
    pub muted: bool,
    pub speed: f32,
    pub repeat: String,
    pub shuffle: bool,
    pub shuffle_history: Vec<String>,
    pub was_playing: bool,
    pub stop_after: bool,
    pub section: String,
    pub tab: String,
    pub search: String,
    pub selected: Option<String>,
    pub sidebar_width: f32,
    pub collapsed_sections: Vec<String>,
    pub artwork_height: f32,
    pub scroll: f32,
    pub window: WindowState,
    pub sleep_deadline: Option<u64>,
    pub sleep_mode: String,
    pub sort_column: usize,
    pub sort_descending: bool,
    /// How the Artists, Albums and Genres lists are ordered, one entry per list
    /// (`artists=tracks;albums=year`). Empty means every list in its own default
    /// order; see `gui_cache::browse_sort_of`. Left out of the profile until used.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub browse_sort: String,
    pub column_widths: Vec<f32>,
    pub ab_a: Option<f64>,
    pub ab_b: Option<f64>,
    /// Canonical paths (see `removal_key`) of files removed from the library,
    /// oldest first. Rescans skip them; an explicit import adds them back.
    pub removed_paths: Vec<String>,
    /// Author headings of the BOOKS list the user collapsed (folded author
    /// names; the empty text is the "Unknown author" heading).
    pub collapsed_authors: Vec<String>,
    /// The internet radio stations (the Streams list), in the order shown.
    pub stations: Vec<Station>,
    /// The station that is current instead of a file, for the next start. A
    /// station and a file are never current together.
    pub current_station: Option<String>,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            version: SESSION_VERSION,
            compact: false,
            fullscreen: false,
            settings: Settings::default(),
            library: Arc::new(Vec::new()),
            queue: vec![],
            playing_workspace: false,
            current: None,
            progress: HashMap::new(),
            bookmarks: vec![],
            playlists: vec![],
            favorites: vec![],
            ratings: HashMap::new(),
            book_speeds: HashMap::new(),
            roots: vec![],
            volume: 0.8,
            muted: false,
            speed: 1.,
            repeat: "off".into(),
            shuffle: false,
            shuffle_history: vec![],
            was_playing: false,
            stop_after: false,
            section: "Music".into(),
            tab: "Files".into(),
            search: String::new(),
            selected: None,
            sidebar_width: 260.,
            collapsed_sections: vec![],
            artwork_height: 320.,
            scroll: 0.,
            window: WindowState::default(),
            sleep_deadline: None,
            sleep_mode: "off".into(),
            sort_column: 0,
            sort_descending: false,
            browse_sort: String::new(),
            column_widths: columns::default_widths().to_vec(),
            ab_a: None,
            ab_b: None,
            removed_paths: Vec::new(),
            collapsed_authors: Vec::new(),
            stations: Vec::new(),
            current_station: None,
        }
    }
}
/// The form in which a track's path is stored in `Session::removed_paths`.
/// `Track.path` is already canonical; on Windows the extended-length prefix,
/// the separator and letter case are folded too, since that file system does
/// not tell `C:\Books\A.mp3` from `c:/books/a.mp3`.
pub fn removal_key(path: &Path) -> String {
    fold_removal_key(&path.to_string_lossy(), cfg!(windows))
}
pub fn fold_removal_key(text: &str, windows: bool) -> String {
    if !windows {
        return text.to_owned();
    }
    let text = text.strip_prefix(r"\\?\UNC\").map_or_else(
        || {
            text.strip_prefix(r"\\?\")
                .unwrap_or(text)
                .replace('/', "\\")
        },
        |rest| format!(r"\\{}", rest.replace('/', "\\")),
    );
    text.to_lowercase()
}
/// Applies "removed files stay removed" to the tracks an import found. A rescan
/// (`rescan`) drops every track whose path was removed from the library and
/// returns how many it skipped; an explicit import (files, folder, drop,
/// command line, playlist) keeps them all and takes their paths off the list.
pub fn apply_removed_paths(
    removed: &mut Vec<String>,
    tracks: Vec<Track>,
    rescan: bool,
) -> (Vec<Track>, usize) {
    if removed.is_empty() || tracks.is_empty() {
        return (tracks, 0);
    }
    if rescan {
        let listed: HashSet<&str> = removed.iter().map(String::as_str).collect();
        let before = tracks.len();
        let kept: Vec<Track> = tracks
            .into_iter()
            .filter(|track| !listed.contains(removal_key(&track.path).as_str()))
            .collect();
        let skipped = before - kept.len();
        (kept, skipped)
    } else {
        let imported: HashSet<String> = tracks.iter().map(|t| removal_key(&t.path)).collect();
        removed.retain(|key| !imported.contains(key));
        (tracks, 0)
    }
}
/// Records `key` as the newest removed path and drops the oldest ones beyond
/// `MAX_REMOVED_PATHS`. A path removed again moves to the newest end.
#[allow(dead_code)]
fn push_removed(removed: &mut Vec<String>, key: String) {
    if key.is_empty() {
        return;
    }
    removed.retain(|existing| *existing != key);
    removed.push(key);
    cap_removed(removed);
}
fn cap_removed(removed: &mut Vec<String>) {
    removed.retain(|key| !key.is_empty());
    if removed.len() > MAX_REMOVED_PATHS {
        let excess = removed.len() - MAX_REMOVED_PATHS;
        removed.drain(..excess);
    }
}
// Keep the on-disk library as the same JSON array while snapshots share the
// immutable metadata allocation. Mutations use Arc::make_mut at their boundary.
mod shared_library {
    use super::Track;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;

    pub fn serialize<S: Serializer>(
        tracks: &Arc<Vec<Track>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        tracks.as_ref().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Arc<Vec<Track>>, D::Error> {
        Vec::<Track>::deserialize(deserializer).map(Arc::new)
    }
}

impl Session {
    /// Remember that the user removed the file at `path` from the library. (Files
    /// leaving the library go through `remove_files`, which does this in bulk.)
    #[allow(dead_code)]
    pub fn remember_removal(&mut self, path: &Path) {
        push_removed(&mut self.removed_paths, removal_key(path));
    }
    /// The file at `path` is in the library again by the user's own doing
    /// (reconnected), so scans may keep it up to date.
    pub fn forget_removal(&mut self, path: &Path) {
        let key = removal_key(path);
        self.removed_paths.retain(|existing| *existing != key);
    }
    pub fn clear_session(&mut self) {
        let defaults = Self::default();
        self.queue.clear();
        self.playing_workspace = false;
        self.current = None;
        self.current_station = None;
        self.was_playing = false;
        self.selected = None;
        self.search.clear();
        self.scroll = 0.;
        self.ab_a = None;
        self.ab_b = None;
        self.shuffle = false;
        self.shuffle_history.clear();
        self.repeat = defaults.repeat;
        self.speed = 1.;
        self.volume = defaults.volume;
        self.muted = false;
        self.sleep_deadline = None;
        self.sleep_mode = "off".into();
        self.stop_after = false;
        self.window = defaults.window;
        self.sidebar_width = defaults.sidebar_width;
        self.collapsed_sections = defaults.collapsed_sections;
        self.collapsed_authors = defaults.collapsed_authors;
        self.artwork_height = defaults.artwork_height;
        self.section = defaults.section;
        self.tab = defaults.tab;
        self.sort_column = 0;
        self.sort_descending = false;
        self.browse_sort = defaults.browse_sort;
        self.column_widths = defaults.column_widths;
        self.compact = false;
        self.fullscreen = false;
    }

    /// Moves everything saved under the library record `old_id` to the record
    /// `new_id` of the same file and removes the `old_id` record: the listening
    /// position (the more recently played one when both have one), play counts,
    /// bookmarks, rating (the new record's own rating wins when it has one),
    /// favourite, queue and playlist entries, shuffle history, current and
    /// selected. Returns whether the current track moved, which means the
    /// player must reload it under its new id.
    pub fn merge_track_history(&mut self, old_id: &str, new_id: &str) -> bool {
        if old_id == new_id {
            return false;
        }
        if let Some(old) = self.progress.remove(old_id) {
            let merged = match self.progress.get(new_id) {
                Some(new) => merged_progress(&old, new),
                None => old,
            };
            self.progress.insert(new_id.to_owned(), merged);
        }
        for bookmark in &mut self.bookmarks {
            if bookmark.track_id == old_id {
                bookmark.track_id = new_id.to_owned();
            }
        }
        if let Some(rating) = self.ratings.remove(old_id) {
            if rating > 0 && self.ratings.get(new_id).copied().unwrap_or(0) == 0 {
                self.ratings.insert(new_id.to_owned(), rating);
            }
        }
        if self.favorites.iter().any(|id| id == old_id) {
            self.favorites.retain(|id| id != old_id);
            if !self.favorites.iter().any(|id| id == new_id) {
                self.favorites.push(new_id.to_owned());
            }
        }
        replace_id_once_listed(&mut self.queue, old_id, new_id);
        replace_id_once_listed(&mut self.shuffle_history, old_id, new_id);
        for playlist in &mut self.playlists {
            replace_id_once_listed(&mut playlist.tracks, old_id, new_id);
        }
        let current_moved = self.current.as_deref() == Some(old_id);
        for slot in [&mut self.current, &mut self.selected] {
            if slot.as_deref() == Some(old_id) {
                *slot = Some(new_id.to_owned());
            }
        }
        Arc::make_mut(&mut self.library).retain(|track| track.id != old_id);
        current_moved
    }

    pub fn track(&self, id: &str) -> Option<&Track> {
        self.library.iter().find(|t| t.id == id)
    }
    pub fn current_track(&self) -> Option<&Track> {
        self.current.as_deref().and_then(|id| self.track(id))
    }
    pub fn normalize(&mut self) {
        self.settings.normalize();
        cap_removed(&mut self.removed_paths);
        cap_collapsed_authors(&mut self.collapsed_authors);
        self.normalize_stations();
        self.volume = finite(self.volume, 0.8).clamp(0., 1.);
        self.speed = finite(self.speed, 1.).clamp(0.25, 3.);
        self.sidebar_width = finite(self.sidebar_width, 260.).clamp(140., 600.);
        self.artwork_height = finite(self.artwork_height, 320.).clamp(36., 700.);
        self.window.width = finite(self.window.width, 1240.).clamp(760., 7680.);
        self.window.height = finite(self.window.height, 820.).clamp(520., 4320.);
        self.window.x = finite(self.window.x, 80.);
        self.window.y = finite(self.window.y, 60.);
        self.scroll = finite(self.scroll, 0.).max(0.);
        // A sort column this build does not know falls back to the default.
        if self.sort_column >= columns::COUNT {
            self.sort_column = 0;
        }
        self.browse_sort = crate::gui_cache::normalized_browse_sort(&self.browse_sort);
        // A browse section whose key no file spells that long is damage, not a
        // group: the list opens instead.
        if let Some(crate::gui_cache::BrowseSection::Group(kind, key)) =
            crate::gui_cache::browse_section(&self.section)
        {
            if key.chars().count() > crate::gui_cache::MAX_BROWSE_KEY_CHARS {
                self.section = kind.list_section().to_owned();
            }
        }
        columns::normalize_widths(&mut self.column_widths);
        for rating in self.ratings.values_mut() {
            *rating = (*rating).min(5);
        }
        if !["off", "track", "queue"].contains(&self.repeat.as_str()) {
            self.repeat = "off".into();
        }
        for speed in self.book_speeds.values_mut() {
            *speed = finite(*speed, 1.).clamp(0.25, 3.);
        }
        for p in self.progress.values_mut() {
            if !p.seconds.is_finite() || p.seconds < 0. {
                p.seconds = 0.;
            }
            p.seconds = p.seconds.min(MAX_POSITION_SECONDS);
        }
        for b in &mut self.bookmarks {
            if !b.seconds.is_finite() || b.seconds < 0. {
                b.seconds = 0.;
            }
            b.seconds = b.seconds.min(MAX_POSITION_SECONDS);
        }
        self.ab_a = self
            .ab_a
            .filter(|v| v.is_finite() && *v >= 0.)
            .map(|v| v.min(MAX_POSITION_SECONDS));
        self.ab_b = self
            .ab_b
            .filter(|v| v.is_finite() && *v >= 0.)
            .map(|v| v.min(MAX_POSITION_SECONDS));
        if self.ab_b.zip(self.ab_a).is_some_and(|(b, a)| b <= a) {
            self.ab_b = None;
        }
    }
}
/// The stored form of a typed stream address, or the sentence that says what is
/// wrong with it. Only plain `http` and `https` addresses with a host and no
/// user name or password are accepted; the text is trimmed, may not hold
/// spaces or control characters and is at most 2048 bytes. The scheme is
/// written in lower case.
pub fn validate_stream_url(raw: &str) -> Result<String, &'static str> {
    let url = raw.trim();
    if url.is_empty() {
        return Err("Type or paste a stream address.");
    }
    if url.len() > MAX_STATION_URL_BYTES {
        return Err("The address is longer than 2048 characters.");
    }
    if url
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err("The address must not contain spaces or control characters.");
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err("The address must start with http:// or https://.");
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err("Only http:// and https:// addresses are supported.");
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err("Addresses with a user name or password are not supported.");
    }
    let (host, port) = split_host_port(authority);
    if host.is_empty() || host.contains('\\') {
        return Err("The address has no host name.");
    }
    if let Some(port) = port {
        match port.parse::<u16>() {
            Ok(number) if number > 0 => {}
            _ => return Err("The port number is not valid."),
        }
    }
    Ok(format!("{}://{rest}", scheme.to_ascii_lowercase()))
}

/// Splits `host:port` (the part of an address before the first `/`): an IPv6
/// address stays in its brackets. The port is `None` when there is none or it
/// is empty.
fn split_host_port(authority: &str) -> (&str, Option<&str>) {
    if let Some(inner) = authority.strip_prefix('[') {
        return match inner.split_once(']') {
            Some((host, tail)) => (host, tail.strip_prefix(':')),
            None => ("", None),
        };
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    }
}

/// The host of a stored station address (for lists and as the name of a station
/// that has none); empty when the text has no host.
pub fn station_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, tail)| tail);
    split_host_port(authority).0.to_owned()
}

/// What makes two addresses the same station: the scheme and host in lower case
/// and a path of just `/` taken as no path.
fn station_url_key(url: &str) -> String {
    let Some((scheme, rest)) = url.trim().split_once("://") else {
        return url.trim().to_lowercase();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let tail = if tail == "/" { "" } else { tail };
    format!(
        "{}://{}{tail}",
        scheme.to_lowercase(),
        authority.to_lowercase()
    )
}

/// A station name as stored: control characters removed, space around it
/// trimmed, at most 100 characters. Empty is allowed (the station then goes by
/// the name it announces, else its host).
pub fn clean_station_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_STATION_NAME_CHARS * 4)
        .collect();
    let trimmed = cleaned.trim();
    let name: String = trimmed.chars().take(MAX_STATION_NAME_CHARS).collect();
    name.trim_end().to_owned()
}

fn base36(mut value: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".into();
    }
    let mut digits = Vec::new();
    while value > 0 {
        digits.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    digits.reverse();
    digits.into_iter().map(char::from).collect()
}

/// What an attempt to add a station did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StationAdd {
    /// A new station with this id.
    Added(String),
    /// The address is in the list already, as this station.
    Existing(String),
}

impl Session {
    pub fn station(&self, id: &str) -> Option<&Station> {
        self.stations.iter().find(|station| station.id == id)
    }

    pub fn station_position(&self, id: &str) -> Option<usize> {
        self.stations.iter().position(|station| station.id == id)
    }

    /// The station that is current, if one is.
    pub fn current_station_ref(&self) -> Option<&Station> {
        self.current_station
            .as_deref()
            .and_then(|id| self.station(id))
    }

    /// The station with this address (or the same address in other spelling).
    pub fn station_by_url(&self, url: &str) -> Option<&Station> {
        let key = station_url_key(url);
        self.stations
            .iter()
            .find(|station| station_url_key(&station.url) == key)
    }

    /// An id that no station has. It starts from a hash of the address, so a
    /// station added again after it was removed gets its old id back.
    fn new_station_id(&self, url: &str) -> String {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in url.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        loop {
            let id = format!("s{}", base36(hash));
            if self.station(&id).is_none() {
                return id;
            }
            hash = hash.wrapping_add(1);
        }
    }

    /// Adds a station. The address is checked with `validate_stream_url`; one that
    /// is in the list already adds nothing and names the station that has it.
    /// Fails when the list holds `MAX_STATIONS`.
    pub fn add_station(
        &mut self,
        name: &str,
        url: &str,
        now: u64,
    ) -> Result<StationAdd, &'static str> {
        let url = validate_stream_url(url)?;
        if let Some(existing) = self.station_by_url(&url) {
            return Ok(StationAdd::Existing(existing.id.clone()));
        }
        if self.stations.len() >= MAX_STATIONS {
            return Err("The station list is full (500 stations).");
        }
        let id = self.new_station_id(&url);
        self.stations.push(Station {
            id: id.clone(),
            name: clean_station_name(name),
            url,
            last_title: String::new(),
            added_at: now,
        });
        Ok(StationAdd::Added(id))
    }

    /// Gives the station a new name (empty is allowed) and returns the stored one.
    pub fn rename_station(&mut self, id: &str, raw: &str) -> Result<String, &'static str> {
        let station = self
            .stations
            .iter_mut()
            .find(|station| station.id == id)
            .ok_or("That station no longer exists.")?;
        station.name = clean_station_name(raw);
        Ok(station.name.clone())
    }

    /// Points the station at another address (checked like a new one, and not
    /// one another station has) and returns the stored address. The id stays.
    pub fn set_station_url(&mut self, id: &str, raw: &str) -> Result<String, &'static str> {
        let url = validate_stream_url(raw)?;
        let position = self
            .station_position(id)
            .ok_or("That station no longer exists.")?;
        let key = station_url_key(&url);
        if self
            .stations
            .iter()
            .enumerate()
            .any(|(index, other)| index != position && station_url_key(&other.url) == key)
        {
            return Err("Another station already uses this address.");
        }
        if let Some(station) = self.stations.get_mut(position) {
            if station.url != url {
                station.url = url.clone();
                // The title belongs to the old address.
                station.last_title.clear();
            }
        }
        Ok(url)
    }

    /// Takes the station out of the list, with the place it had. When it was
    /// current, nothing is current any more (the player stops it).
    pub fn remove_station(&mut self, id: &str) -> Option<(usize, Station)> {
        let index = self.station_position(id)?;
        let station = self.stations.remove(index);
        if self.current_station.as_deref() == Some(id) {
            self.current_station = None;
        }
        Some((index, station))
    }

    /// The station after (`forward`) or before the one with this id in the list;
    /// `None` at either end (the list does not wrap) or for an unknown id.
    pub fn adjacent_station(&self, id: &str, forward: bool) -> Option<&Station> {
        let position = self.station_position(id)?;
        let target = if forward {
            position.checked_add(1)?
        } else {
            position.checked_sub(1)?
        };
        self.stations.get(target)
    }

    /// Puts a removed station back at its place (or last when the list got
    /// shorter). Refused when the list is full or the address or id is in it.
    fn restore_station(&mut self, index: usize, station: Station) -> UndoOutcome {
        if self.station_by_url(&station.url).is_some() || self.station(&station.id).is_some() {
            return UndoOutcome::refused("Could not restore: that station is in the list again");
        }
        if self.stations.len() >= MAX_STATIONS {
            return UndoOutcome::refused("Could not restore: the station list is full");
        }
        let label = station_label(&station);
        let at = index.min(self.stations.len());
        self.stations.insert(at, station);
        UndoOutcome {
            view: true,
            ..UndoOutcome::done(format!("Restored the station “{label}”"))
        }
    }

    /// Keeps the station list sane after loading a profile: drops entries that
    /// cannot be played (no id or address, or an address over the limit) and
    /// repeated ids, cuts names and titles to their limits, keeps at most
    /// `MAX_STATIONS` and forgets a current station that is not in the list.
    fn normalize_stations(&mut self) {
        let mut seen = HashSet::new();
        self.stations.retain(|station| {
            !station.id.is_empty()
                && !station.url.trim().is_empty()
                && station.url.len() <= MAX_STATION_URL_BYTES
                && seen.insert(station.id.clone())
        });
        self.stations.truncate(MAX_STATIONS);
        for station in &mut self.stations {
            station.name = clean_station_name(&station.name);
            station.url = station.url.trim().to_owned();
            if station.last_title.chars().count() > MAX_STATION_TITLE_CHARS {
                station.last_title = station
                    .last_title
                    .chars()
                    .take(MAX_STATION_TITLE_CHARS)
                    .collect();
            }
        }
        if let Some(id) = self.current_station.as_deref() {
            if !self.stations.iter().any(|station| station.id == id) {
                self.current_station = None;
            }
        }
    }
}

/// What a station is called in lists and notes when no live name is known: its
/// own name, else its host.
pub fn station_label(station: &Station) -> String {
    if !station.name.trim().is_empty() {
        station.name.clone()
    } else {
        let host = station_host(&station.url);
        if host.is_empty() {
            "Station".into()
        } else {
            host
        }
    }
}

/// Most collapsed author headings the profile remembers.
const MAX_COLLAPSED_AUTHORS: usize = 10_000;
/// Longest author key kept (keys are folded author names).
const MAX_AUTHOR_KEY_CHARS: usize = 512;
/// Drops repeated, oversized and surplus entries of the collapsed-authors list.
fn cap_collapsed_authors(keys: &mut Vec<String>) {
    let mut seen = HashSet::new();
    keys.retain(|key| key.chars().count() <= MAX_AUTHOR_KEY_CHARS && seen.insert(key.clone()));
    keys.truncate(MAX_COLLAPSED_AUTHORS);
}

/// Where a book stands: no file started, some listened to, or every file finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookState {
    NotStarted,
    InProgress,
    Finished,
}

/// Listening progress of one book across all its files.
#[derive(Clone, Debug, PartialEq)]
pub struct BookProgress {
    /// Files counted (superseded records are not part of a book).
    pub files: usize,
    pub finished_files: usize,
    /// Files whose length is not known; they add nothing to the seconds below.
    pub unknown_files: usize,
    /// Content length of all files with a known length.
    pub total_seconds: f64,
    /// Finished files in full plus the saved position of the others.
    pub listened_seconds: f64,
    /// Content still to listen to; zero for a finished book.
    pub left_seconds: f64,
    /// 0 to 100; exactly 100 only for a finished book.
    pub percent: f64,
    pub state: BookState,
    /// `left_seconds` divided by the speed this summary was made for.
    pub left_at_speed: f64,
}

/// Content seconds that take `left` at `speed` times normal speed. A speed that
/// is not a sensible number counts as normal speed.
pub fn seconds_at_speed(left: f64, speed: f32) -> f64 {
    if speed.is_finite() && speed > 0.05 {
        left / f64::from(speed)
    } else {
        left
    }
}

/// A file length that can be summed: finite and not negative, else zero.
fn known_length(duration: f64) -> f64 {
    if duration.is_finite() && duration > 0. {
        duration.min(MAX_POSITION_SECONDS)
    } else {
        0.
    }
}

impl BookProgress {
    /// The progress as a whole percent for display: a book that is not finished
    /// never shows 100.
    pub fn whole_percent(&self) -> u32 {
        match self.state {
            BookState::Finished => 100,
            _ => (self.percent.floor() as u32).min(99),
        }
    }

    /// Seconds left at another speed than the one this summary was made for.
    pub fn left_at(&self, speed: f32) -> f64 {
        seconds_at_speed(self.left_seconds, speed)
    }

    fn from_sums(
        files: usize,
        finished_files: usize,
        unknown_files: usize,
        total: f64,
        listened: f64,
        touched: bool,
        speed: f32,
    ) -> Self {
        let state = if files > 0 && finished_files == files {
            BookState::Finished
        } else if touched {
            BookState::InProgress
        } else {
            BookState::NotStarted
        };
        let listened = listened.clamp(0., total);
        let left = if state == BookState::Finished {
            0.
        } else {
            (total - listened).max(0.)
        };
        let percent = if state == BookState::Finished {
            100.
        } else if total > 0. {
            (listened / total * 100.).clamp(0., 100.)
        } else {
            0.
        };
        Self {
            files,
            finished_files,
            unknown_files,
            total_seconds: total,
            listened_seconds: if state == BookState::Finished {
                total
            } else {
                listened
            },
            left_seconds: left,
            percent,
            state,
            left_at_speed: seconds_at_speed(left, speed),
        }
    }

    /// This summary with the position of one of its files replaced by the live
    /// one (`saved` is that file's saved progress, which the summary counted).
    /// A finished book or a finished file is left as it is.
    pub fn with_live_position(
        &self,
        track: &Track,
        saved: Option<&Progress>,
        live: f64,
        speed: f32,
    ) -> Self {
        if self.state == BookState::Finished || saved.is_some_and(|p| p.completed) {
            return self.clone();
        }
        let length = known_length(track.duration);
        let counted = |seconds: f64| {
            if seconds.is_finite() && seconds > 0. {
                seconds.min(length)
            } else {
                0.
            }
        };
        let before = counted(saved.map_or(0., |p| p.seconds));
        let after = counted(live);
        let listened = (self.listened_seconds - before + after).clamp(0., self.total_seconds);
        Self::from_sums(
            self.files,
            self.finished_files,
            self.unknown_files,
            self.total_seconds,
            listened,
            self.state == BookState::InProgress || after > 0.,
            speed,
        )
    }
}

/// Progress of the book made of `tracks` (in any order: every file counts for
/// itself). A finished file counts in full, any other with its saved position
/// (at most its length). `speed` sets `left_at_speed`.
pub fn book_progress<'a>(
    tracks: impl IntoIterator<Item = &'a Track>,
    progress: &HashMap<String, Progress>,
    speed: f32,
) -> BookProgress {
    let (mut files, mut finished, mut unknown) = (0usize, 0usize, 0usize);
    let (mut total, mut listened) = (0f64, 0f64);
    let mut touched = false;
    for track in tracks {
        files += 1;
        let length = known_length(track.duration);
        if length <= 0. {
            unknown += 1;
        }
        total += length;
        match progress.get(&track.id) {
            Some(p) if p.completed => {
                finished += 1;
                touched = true;
                listened += length;
            }
            Some(p) if p.seconds.is_finite() && p.seconds > 0. => {
                touched = true;
                listened += p.seconds.min(length);
            }
            _ => {}
        }
    }
    BookProgress::from_sums(files, finished, unknown, total, listened, touched, speed)
}

impl Session {
    /// The files that make up the book `key`: its current records, in library
    /// order (earlier versions of a changed file are not part of it).
    fn book_files<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a Track> + 'a {
        self.library
            .iter()
            .filter(move |track| !track.superseded && track.book_key() == key)
    }

    /// Marks every file of the book `key` finished at its full length and
    /// returns how many files that was. A file that was not finished yet counts
    /// one play and is dated `now`; a finished one keeps its history, so playing
    /// counts are never added twice. With `record_history` false (private
    /// listening) no play count or date is written. Bookmarks stay.
    pub fn mark_book_finished(&mut self, key: &str, now: u64, record_history: bool) -> usize {
        let files: Vec<(String, f64)> = self
            .book_files(key)
            .map(|track| (track.id.clone(), known_length(track.duration)))
            .collect();
        self.finish_files(&files, now, record_history)
    }

    /// Puts every file of the book `key` back to the start and not finished, and
    /// returns how many files had progress to reset. Play counts, dates and
    /// bookmarks stay (like "Reset this file's progress").
    pub fn mark_book_not_started(&mut self, key: &str) -> usize {
        let ids: Vec<String> = self.book_files(key).map(|track| track.id.clone()).collect();
        self.mark_files_not_started(&ids)
    }

    /// Like `mark_book_finished` for the library files with these ids (ids that
    /// are not in the library are skipped). Returns how many files were marked.
    pub fn mark_files_finished(&mut self, ids: &[String], now: u64, record_history: bool) -> usize {
        let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let files: Vec<(String, f64)> = self
            .library
            .iter()
            .filter(|track| wanted.contains(track.id.as_str()))
            .map(|track| (track.id.clone(), known_length(track.duration)))
            .collect();
        self.finish_files(&files, now, record_history)
    }

    /// The saved progress of the files with these ids (`None`: no entry), to put
    /// back with `UndoRestore::Progress`. Each id once, in the order given.
    pub fn capture_progress(&self, ids: &[String]) -> Vec<(String, Option<Progress>)> {
        let mut seen = HashSet::new();
        ids.iter()
            .filter(|id| seen.insert(id.as_str()))
            .map(|id| (id.clone(), self.progress.get(id).cloned()))
            .collect()
    }

    fn finish_files(&mut self, files: &[(String, f64)], now: u64, record_history: bool) -> usize {
        for (id, length) in files {
            let entry = self.progress.entry(id.clone()).or_default();
            let newly = !entry.completed;
            entry.completed = true;
            entry.seconds = entry.seconds.max(*length);
            if newly && record_history {
                entry.play_count = entry.play_count.saturating_add(1);
                entry.last_played = now;
            }
        }
        files.len()
    }

    /// Like `mark_book_not_started` for the files with these ids: returns how
    /// many had progress to reset.
    pub fn mark_files_not_started(&mut self, ids: &[String]) -> usize {
        let mut reset = 0;
        for id in ids {
            if let Some(entry) = self.progress.get_mut(id) {
                if entry.completed || entry.seconds != 0. {
                    reset += 1;
                }
                entry.completed = false;
                entry.seconds = 0.;
            }
        }
        reset
    }
}
/// Longest playlist name, in characters.
pub const MAX_PLAYLIST_NAME_CHARS: usize = 100;

/// Why a playlist name or edit is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistProblem {
    /// Nothing is left of the name once control characters and spaces are gone.
    Empty,
    TooLong,
    /// Another playlist has this name (case does not matter).
    Taken,
    /// The playlist to change is not in the profile (any more).
    Missing,
}

impl PlaylistProblem {
    /// The sentence a dialog shows under the name field.
    pub fn text(self) -> &'static str {
        match self {
            Self::Empty => "Type a name for the playlist.",
            Self::TooLong => "Use at most 100 characters.",
            Self::Taken => "A playlist with this name already exists.",
            Self::Missing => "That playlist no longer exists.",
        }
    }
}

/// A playlist name as it is stored: control characters removed, space around it
/// trimmed. Length and uniqueness are checked by `check_playlist_name`.
pub fn clean_playlist_name(raw: &str) -> String {
    raw.chars()
        .filter(|character| !character.is_control())
        .collect::<String>()
        .trim()
        .to_owned()
}

fn playlist_name_key(name: &str) -> String {
    clean_playlist_name(name).to_lowercase()
}

/// Whether a playlist other than the one at position `except` already has this
/// name, ignoring case and the space around it.
pub fn playlist_name_taken(playlists: &[Playlist], name: &str, except: Option<usize>) -> bool {
    let key = playlist_name_key(name);
    playlists
        .iter()
        .enumerate()
        .any(|(index, list)| Some(index) != except && playlist_name_key(&list.name) == key)
}

/// The stored form of a typed playlist name, or why it cannot be used. `except`
/// is the position of the playlist being renamed (it may keep its own name).
pub fn check_playlist_name(
    playlists: &[Playlist],
    raw: &str,
    except: Option<usize>,
) -> Result<String, PlaylistProblem> {
    let name = clean_playlist_name(raw);
    if name.is_empty() {
        Err(PlaylistProblem::Empty)
    } else if name.chars().count() > MAX_PLAYLIST_NAME_CHARS {
        Err(PlaylistProblem::TooLong)
    } else if playlist_name_taken(playlists, &name, except) {
        Err(PlaylistProblem::Taken)
    } else {
        Ok(name)
    }
}

/// `base` when no playlist has that name yet, else `base 2`, `base 3` and so on.
/// The result always fits the name rules (a long base is cut short).
pub fn unique_playlist_name(playlists: &[Playlist], base: &str) -> String {
    let base = clean_playlist_name(base);
    let base = if base.is_empty() {
        "Playlist".to_owned()
    } else {
        base
    };
    let fit = |suffix: &str| -> String {
        let room = MAX_PLAYLIST_NAME_CHARS.saturating_sub(suffix.chars().count());
        let head: String = base.chars().take(room).collect();
        format!("{}{suffix}", head.trim_end())
    };
    let mut name = fit("");
    let mut number = 2u32;
    while playlist_name_taken(playlists, &name, None) {
        name = fit(&format!(" {number}"));
        number = number.saturating_add(1);
    }
    name
}

impl Session {
    /// Position of the playlist called exactly `name`, the way the sidebar
    /// finds it (a playlist view is identified by its name).
    pub fn playlist_position(&self, name: &str) -> Option<usize> {
        self.playlists.iter().position(|list| list.name == name)
    }

    /// Adds a playlist holding `tracks` (each at most once, in the given order)
    /// and returns the name it was stored under.
    pub fn create_playlist(
        &mut self,
        raw: &str,
        tracks: Vec<String>,
    ) -> Result<String, PlaylistProblem> {
        let name = check_playlist_name(&self.playlists, raw, None)?;
        let mut seen = HashSet::new();
        let tracks: Vec<String> = tracks
            .into_iter()
            .filter(|id| seen.insert(id.clone()))
            .collect();
        self.playlists.push(Playlist {
            name: name.clone(),
            tracks,
        });
        Ok(name)
    }

    /// Gives the playlist `from` a new name and returns the stored name. When
    /// the playlist is the one on show, the view follows it to the new name.
    pub fn rename_playlist(&mut self, from: &str, raw: &str) -> Result<String, PlaylistProblem> {
        let index = self
            .playlist_position(from)
            .ok_or(PlaylistProblem::Missing)?;
        let name = check_playlist_name(&self.playlists, raw, Some(index))?;
        let was_shown = self.section == format!("Playlist:{from}");
        if let Some(list) = self.playlists.get_mut(index) {
            if list.name == name {
                return Ok(name);
            }
            list.name = name.clone();
        }
        if was_shown {
            self.section = format!("Playlist:{name}");
        }
        Ok(name)
    }

    /// Appends `ids` to the playlist `name`, each at most once. Returns how many
    /// were added and how many were in it already; `None` when there is no such
    /// playlist.
    pub fn add_to_playlist(&mut self, name: &str, ids: &[String]) -> Option<(usize, usize)> {
        let list = self.playlists.iter_mut().find(|list| list.name == name)?;
        let mut present: HashSet<String> = list.tracks.iter().cloned().collect();
        let (mut added, mut already) = (0, 0);
        for id in ids {
            if present.insert(id.clone()) {
                list.tracks.push(id.clone());
                added += 1;
            } else {
                already += 1;
            }
        }
        Some((added, already))
    }

    /// Takes `ids` out of the playlist `name` and returns what was removed with
    /// the position each entry had, ready for `UndoRestore::PlaylistEntries`.
    pub fn remove_from_playlist(&mut self, name: &str, ids: &[String]) -> Vec<(usize, String)> {
        let Some(list) = self.playlists.iter_mut().find(|list| list.name == name) else {
            return Vec::new();
        };
        let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let removed: Vec<(usize, String)> = list
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, id)| wanted.contains(id.as_str()))
            .map(|(index, id)| (index, id.clone()))
            .collect();
        if !removed.is_empty() {
            list.tracks.retain(|id| !wanted.contains(id.as_str()));
        }
        removed
    }

    /// Drag and drop: moves `id` to the place `target` has in the playlist.
    /// Returns whether anything moved.
    pub fn move_in_playlist(&mut self, name: &str, id: &str, target: &str) -> bool {
        let Some(list) = self.playlists.iter_mut().find(|list| list.name == name) else {
            return false;
        };
        let from = list.tracks.iter().position(|entry| entry == id);
        let to = list.tracks.iter().position(|entry| entry == target);
        let (Some(from), Some(to)) = (from, to) else {
            return false;
        };
        if from == to {
            return false;
        }
        let entry = list.tracks.remove(from);
        let at = to.min(list.tracks.len());
        list.tracks.insert(at, entry);
        true
    }

    /// Drag and drop of several rows: moves `ids` (and `dragged`) together to the
    /// place `target` has in the playlist, keeping their order. Returns whether
    /// anything moved.
    pub fn move_block_in_playlist(
        &mut self,
        name: &str,
        ids: &[String],
        dragged: &str,
        target: &str,
    ) -> bool {
        match self.playlists.iter_mut().find(|list| list.name == name) {
            Some(list) => move_block(&mut list.tracks, ids, dragged, target),
            None => false,
        }
    }

    /// Moves `id` one place up or down in the playlist. Returns whether it moved.
    pub fn step_in_playlist(&mut self, name: &str, id: &str, up: bool) -> bool {
        let Some(list) = self.playlists.iter_mut().find(|list| list.name == name) else {
            return false;
        };
        let Some(from) = list.tracks.iter().position(|entry| entry == id) else {
            return false;
        };
        let to = if up {
            match from.checked_sub(1) {
                Some(to) => to,
                None => return false,
            }
        } else {
            from.saturating_add(1)
        };
        if to >= list.tracks.len() {
            return false;
        }
        list.tracks.swap(from, to);
        true
    }
}

/// Most undo entries kept.
pub const MAX_UNDO_ENTRIES: usize = 20;
/// Most memory (estimated) the undo entries may hold together.
pub const MAX_UNDO_BYTES: usize = 5 * 1024 * 1024;

/// What was taken out of the profile when files left the library, so that
/// `Session::apply_undo` can put it all back. The listening positions,
/// bookmarks, ratings, favourites and shuffle history of a removed file stay in
/// the session (removing keeps them), so they need no copy here.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RemovedFiles {
    /// The library records with the position each had in the library.
    pub tracks: Vec<(usize, Track)>,
    /// Queue entries with their positions.
    pub queue: Vec<(usize, String)>,
    /// Playlist entries by playlist name, with their positions.
    pub playlists: Vec<(String, Vec<(usize, String)>)>,
    /// The `removed_paths` keys this removal added (a key that was listed
    /// before stays listed).
    pub new_removals: Vec<String>,
    /// The removed file was the loaded one.
    pub was_current: Option<String>,
    /// The removed file was the selected row.
    pub was_selected: Option<String>,
}

/// The data one undoable action took away, in the form that puts it back.
#[derive(Clone, Debug, PartialEq)]
pub enum UndoRestore {
    /// Files removed from the library.
    Files(Box<RemovedFiles>),
    /// The whole queue (Clear queue).
    Queue(Vec<String>),
    /// Entries taken out of the queue, with their positions.
    QueueEntries(Vec<(usize, String)>),
    /// Deleted bookmarks, with their positions in the list.
    Bookmarks(Vec<(usize, Bookmark)>),
    /// A removed playlist and where it stood in the list.
    Playlist { index: usize, playlist: Playlist },
    /// Entries taken out of the playlist `name`, with their positions.
    PlaylistEntries {
        name: String,
        entries: Vec<(usize, String)>,
    },
    /// A playlist renamed from `from` to `to`.
    PlaylistRenamed { from: String, to: String },
    /// The saved progress of files before it was changed (`None`: the file had
    /// no entry). `reposition`: the action also moved the loaded file, so an
    /// undo moves it back to the restored position.
    Progress {
        before: Vec<(String, Option<Progress>)>,
        reposition: bool,
    },
    /// A station removed from the Streams list and the place it had.
    Station { index: usize, station: Station },
    /// What an import of a listening progress file changed.
    ProgressImport(Box<ProgressUndo>),
}

/// The entries a progress file import touched, as they were before it: what
/// `Session::apply_undo` needs to put them back (`None`: there was no entry).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProgressUndo {
    pub progress: Vec<(String, Option<Progress>)>,
    pub ratings: Vec<(String, Option<u8>)>,
    /// Files the import made favourites.
    pub favorites_added: Vec<String>,
    /// Bookmarks the import added.
    pub bookmarks_added: Vec<Bookmark>,
    /// Book speeds the import set (key and the speed before).
    pub speeds: Vec<(String, Option<f32>)>,
}

fn total_bytes(parts: impl Iterator<Item = usize>) -> usize {
    parts.fold(0usize, |sum, part| sum.saturating_add(part))
}

fn text_bytes(text: &str) -> usize {
    text.len().saturating_add(std::mem::size_of::<String>())
}

fn track_bytes(track: &Track) -> usize {
    total_bytes(
        [
            std::mem::size_of::<Track>(),
            track.id.len(),
            track.content_digest.as_deref().map_or(0, str::len),
            track.legacy_fingerprint.as_deref().map_or(0, str::len),
            track.path.as_os_str().len(),
            track.title.len(),
            track.artist.len(),
            track.album.len(),
            track.genre.len(),
            track.year.len(),
            track.comment.len(),
            track.album_artist.len(),
            track.composer.len(),
            track.narrator.len(),
        ]
        .into_iter(),
    )
}

fn entries_bytes(entries: &[(usize, String)]) -> usize {
    total_bytes(entries.iter().map(|(_, id)| text_bytes(id) + 8))
}

impl UndoRestore {
    /// A rough size of the captured data, for the undo memory cap.
    pub fn estimated_bytes(&self) -> usize {
        match self {
            Self::Files(files) => total_bytes(
                files
                    .tracks
                    .iter()
                    .map(|(_, track)| track_bytes(track) + 8)
                    .chain(std::iter::once(entries_bytes(&files.queue)))
                    .chain(
                        files
                            .playlists
                            .iter()
                            .map(|(name, entries)| text_bytes(name) + entries_bytes(entries)),
                    )
                    .chain(files.new_removals.iter().map(|key| text_bytes(key))),
            ),
            Self::Queue(ids) => total_bytes(ids.iter().map(|id| text_bytes(id))),
            Self::QueueEntries(entries) => entries_bytes(entries),
            Self::Bookmarks(entries) => total_bytes(entries.iter().map(|(_, bookmark)| {
                8 + text_bytes(&bookmark.track_id)
                    + text_bytes(&bookmark.name)
                    + text_bytes(&bookmark.note)
                    + std::mem::size_of::<Bookmark>()
            })),
            Self::Playlist { playlist, .. } => {
                text_bytes(&playlist.name)
                    + total_bytes(playlist.tracks.iter().map(|id| text_bytes(id)))
            }
            Self::PlaylistEntries { name, entries } => text_bytes(name) + entries_bytes(entries),
            Self::PlaylistRenamed { from, to } => text_bytes(from) + text_bytes(to),
            Self::Progress { before, .. } => total_bytes(
                before
                    .iter()
                    .map(|(id, _)| text_bytes(id) + std::mem::size_of::<Option<Progress>>()),
            ),
            Self::Station { station, .. } => total_bytes(
                [
                    std::mem::size_of::<Station>(),
                    station.id.len(),
                    station.name.len(),
                    station.url.len(),
                    station.last_title.len(),
                ]
                .into_iter(),
            ),
            Self::ProgressImport(undo) => total_bytes(
                undo.progress
                    .iter()
                    .map(|(id, _)| text_bytes(id) + std::mem::size_of::<Option<Progress>>())
                    .chain(undo.ratings.iter().map(|(id, _)| text_bytes(id) + 8))
                    .chain(undo.favorites_added.iter().map(|id| text_bytes(id)))
                    .chain(undo.bookmarks_added.iter().map(|bookmark| {
                        text_bytes(&bookmark.track_id)
                            + text_bytes(&bookmark.name)
                            + text_bytes(&bookmark.note)
                            + std::mem::size_of::<Bookmark>()
                    }))
                    .chain(undo.speeds.iter().map(|(key, _)| text_bytes(key) + 8)),
            ),
        }
    }
}

/// One step the user can take back: what to call it and the data to restore.
#[derive(Clone, Debug, PartialEq)]
pub struct UndoEntry {
    pub label: String,
    pub restore: UndoRestore,
}

/// The last few undoable actions, newest last. Kept in memory only. Old entries
/// drop out when there are more than the entry limit or the estimated captured
/// data passes the byte limit.
#[derive(Debug)]
pub struct UndoStack {
    entries: std::collections::VecDeque<(UndoEntry, usize)>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl Default for UndoStack {
    fn default() -> Self {
        Self::with_limits(MAX_UNDO_ENTRIES, MAX_UNDO_BYTES)
    }
}

impl UndoStack {
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: std::collections::VecDeque::new(),
            bytes: 0,
            max_entries,
            max_bytes,
        }
    }

    /// Records `entry`, dropping the oldest ones that no longer fit. Returns
    /// false (and keeps nothing) when the entry alone is over the byte limit.
    pub fn push(&mut self, entry: UndoEntry) -> bool {
        let size = entry
            .restore
            .estimated_bytes()
            .saturating_add(entry.label.len());
        if self.max_entries == 0 || size > self.max_bytes {
            return false;
        }
        self.entries.push_back((entry, size));
        self.bytes = self.bytes.saturating_add(size);
        while self.entries.len() > self.max_entries || self.bytes > self.max_bytes {
            match self.entries.pop_front() {
                Some((_, dropped)) => self.bytes = self.bytes.saturating_sub(dropped),
                None => break,
            }
        }
        true
    }

    /// Takes the newest entry.
    pub fn pop(&mut self) -> Option<UndoEntry> {
        let (entry, size) = self.entries.pop_back()?;
        self.bytes = self.bytes.saturating_sub(size);
        Some(entry)
    }

    /// What the newest entry is called.
    pub fn last_label(&self) -> Option<&str> {
        self.entries.back().map(|(entry, _)| entry.label.as_str())
    }



    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }


    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

/// What `Session::apply_undo` did, for the app to refresh around it.
#[derive(Debug, Default, PartialEq)]
pub struct UndoOutcome {
    /// A sentence for the status bar: what came back, or why nothing did.
    pub note: String,
    /// False when the entry was refused and nothing changed.
    pub restored: bool,
    /// Library records came back: library lists and caches must be rebuilt.
    pub library: bool,
    /// Queue, playlists, bookmarks or the selection changed: lists must refresh.
    pub view: bool,
    /// Saved progress changed.
    pub progress: bool,
    /// The loaded file is back as `current`; the player must load it again.
    pub current: Option<String>,
    /// The loaded file's position was restored; seek there.
    pub seek: Option<(String, f64)>,
}

impl UndoOutcome {
    fn refused(note: impl Into<String>) -> Self {
        Self {
            note: note.into(),
            ..Self::default()
        }
    }
    fn done(note: impl Into<String>) -> Self {
        Self {
            note: note.into(),
            restored: true,
            ..Self::default()
        }
    }
}

/// Puts `removed` items (with the position each had) back into `current`. The
/// positions are those of the original list, so when nothing else changed the
/// original order comes back exactly; when it did change, an item whose position
/// is past the end goes last.
fn reinsert_sorted<T>(current: Vec<T>, mut removed: Vec<(usize, T)>) -> Vec<T> {
    removed.sort_by_key(|(index, _)| *index);
    let mut out = Vec::with_capacity(current.len().saturating_add(removed.len()));
    let mut current = current.into_iter();
    let mut removed = removed.into_iter().peekable();
    loop {
        if removed.peek().is_some_and(|(index, _)| *index <= out.len()) {
            if let Some((_, item)) = removed.next() {
                out.push(item);
            }
            continue;
        }
        match current.next() {
            Some(item) => out.push(item),
            None => {
                out.extend(removed.map(|(_, item)| item));
                break;
            }
        }
    }
    out
}

/// Adds every key of `keys` as a newest removed path, in order, scanning the
/// list once however many keys there are.
fn push_removed_many(removed: &mut Vec<String>, keys: &[String]) {
    let incoming: HashSet<&str> = keys
        .iter()
        .map(String::as_str)
        .filter(|key| !key.is_empty())
        .collect();
    if incoming.is_empty() {
        return;
    }
    removed.retain(|existing| !incoming.contains(existing.as_str()));
    let mut seen = HashSet::new();
    for key in keys {
        if !key.is_empty() && seen.insert(key.as_str()) {
            removed.push(key.clone());
        }
    }
    cap_removed(removed);
}

/// Puts `ids` (each once, in the given order) into `queue`: right after the
/// loaded file (`next`; at the front when it is not queued) or at the end. A file
/// that is queued already moves to the new place; the loaded file never moves,
/// as that would change what "Next" plays. Returns how many files were placed.
pub fn queue_ids(
    queue: &mut Vec<String>,
    current: Option<&str>,
    ids: &[String],
    next: bool,
) -> usize {
    let mut seen = HashSet::new();
    let ids: Vec<&String> = ids
        .iter()
        .filter(|id| Some(id.as_str()) != current && seen.insert(id.as_str()))
        .collect();
    if ids.is_empty() {
        return 0;
    }
    let wanted: HashSet<&str> = ids.iter().map(|id| id.as_str()).collect();
    queue.retain(|queued| !wanted.contains(queued.as_str()));
    let at = if next {
        current
            .and_then(|current| queue.iter().position(|queued| queued == current))
            .map_or(0, |index| index + 1)
    } else {
        queue.len()
    };
    let count = ids.len();
    queue.splice(at..at, ids.into_iter().cloned());
    count
}

/// Drag and drop of a block of rows: moves every entry of `list` named in `ids`
/// (and the `dragged` one) to where `target` stands, keeping their order. Moving
/// down puts the block after the target, moving up before it, which is what a
/// single dragged row has always done. Dropping onto a row of the block, or a
/// file that is not in `list`, changes nothing. Returns whether anything moved.
pub fn move_block(list: &mut Vec<String>, ids: &[String], dragged: &str, target: &str) -> bool {
    let Some(from) = list.iter().position(|entry| entry == dragged) else {
        return false;
    };
    let Some(to) = list.iter().position(|entry| entry == target) else {
        return false;
    };
    let mut wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
    wanted.insert(dragged);
    if wanted.contains(target) {
        return false;
    }
    let moved: Vec<String> = list
        .iter()
        .filter(|entry| wanted.contains(entry.as_str()))
        .cloned()
        .collect();
    list.retain(|entry| !wanted.contains(entry.as_str()));
    let Some(target_at) = list.iter().position(|entry| entry == target) else {
        return false;
    };
    let at = if to > from { target_at + 1 } else { target_at };
    list.splice(at..at, moved);
    true
}

/// The rows selected in the file list, in memory only. `Session::selected` stays
/// the primary row (the one the art pane and File details show, and where Enter
/// starts); the set holds every selected row and the anchor a Shift+click or
/// Shift+arrow range starts from. The set follows `Session::selected`: when other
/// code changes that field, `coordinate` collapses the set to the new row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectionSet {
    ids: HashSet<String>,
    anchor: Option<String>,
    /// The primary row the set was last brought in step with.
    primary: Option<String>,
    /// Counts every change, so cached figures about the selection can be checked.
    revision: u64,
}

impl SelectionSet {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn contains(&self, id: &str) -> bool {
        !self.ids.is_empty() && self.ids.contains(id)
    }

    pub fn anchor(&self) -> Option<&str> {
        self.anchor.as_deref()
    }

    pub fn primary(&self) -> Option<&str> {
        self.primary.as_deref()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn touch(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Brings the set in step with `Session::selected` after code that does not
    /// know about the set changed it: the set collapses to that row (or empties).
    /// Returns whether it changed.
    pub fn coordinate(&mut self, selected: Option<&str>) -> bool {
        if self.primary.as_deref() == selected {
            return false;
        }
        match selected {
            Some(id) => self.click(id),
            None => self.clear(),
        }
        true
    }

    /// Nothing is selected.
    pub fn clear(&mut self) {
        self.ids.clear();
        self.anchor = None;
        self.primary = None;
        self.touch();
    }

    /// A plain click: only this row is selected, and it is the anchor.
    pub fn click(&mut self, id: &str) {
        self.ids.clear();
        self.ids.insert(id.to_owned());
        self.anchor = Some(id.to_owned());
        self.primary = Some(id.to_owned());
        self.touch();
    }

    /// Esc: collapse to `id` (the primary row), or to nothing without one.
    pub fn clear_to(&mut self, id: Option<&str>) {
        match id {
            Some(id) => self.click(id),
            None => self.clear(),
        }
    }

    /// Ctrl/Cmd+click: the row joins or leaves the selection. Returns whether it
    /// is selected now. A row that joins becomes the primary row and the anchor;
    /// when the primary row leaves, the set has no primary row until `focus`.
    pub fn toggle(&mut self, id: &str) -> bool {
        self.touch();
        if self.ids.remove(id) {
            if self.primary.as_deref() == Some(id) {
                self.primary = None;
            }
            if self.anchor.as_deref() == Some(id) {
                self.anchor = self.primary.clone();
            }
            false
        } else {
            self.ids.insert(id.to_owned());
            self.anchor = Some(id.to_owned());
            self.primary = Some(id.to_owned());
            true
        }
    }

    /// Makes a selected row the primary row and the anchor.
    pub fn focus(&mut self, id: &str) {
        if self.ids.contains(id) {
            self.primary = Some(id.to_owned());
            self.anchor = Some(id.to_owned());
            self.touch();
        }
    }

    /// Makes a selected row the primary row; the anchor stays where it is.
    pub fn set_primary(&mut self, id: &str) {
        if self.ids.contains(id) && self.primary.as_deref() != Some(id) {
            self.primary = Some(id.to_owned());
            self.touch();
        }
    }

    /// Shift+click (and Shift+arrows): selects the rows from `anchor` to `id` in
    /// the displayed `order`, both included. Without `additive` the range
    /// replaces the selection; with it (Ctrl+Shift+click) the range joins it.
    /// `id` becomes the primary row; the anchor stays. When the anchor is not
    /// displayed this is a plain click on `id`; when `id` is not displayed
    /// nothing changes.
    pub fn range<'a>(
        &mut self,
        anchor: &str,
        id: &str,
        order: impl IntoIterator<Item = &'a str>,
        additive: bool,
    ) {
        let mut run: Vec<&str> = Vec::new();
        let mut other_end: Option<&str> = None;
        let mut closed = false;
        let mut saw_id = false;
        for item in order {
            saw_id |= item == id;
            match other_end {
                None => {
                    let end = if item == anchor {
                        Some(id)
                    } else if item == id {
                        Some(anchor)
                    } else {
                        None
                    };
                    if let Some(end) = end {
                        run.push(item);
                        if end == item {
                            closed = true;
                            break;
                        }
                        other_end = Some(end);
                    }
                }
                Some(end) => {
                    run.push(item);
                    if item == end {
                        closed = true;
                        break;
                    }
                }
            }
        }
        if !closed {
            if saw_id {
                self.click(id);
            }
            return;
        }
        if !additive {
            self.ids.clear();
        }
        self.ids.extend(run.into_iter().map(str::to_owned));
        self.anchor = Some(anchor.to_owned());
        self.primary = Some(id.to_owned());
        self.touch();
    }

    /// Ctrl+A: every displayed row is selected. The primary row and the anchor
    /// stay when they are displayed, else the first row takes both over.
    pub fn select_all<'a>(&mut self, order: impl IntoIterator<Item = &'a str>) {
        let mut first: Option<String> = None;
        let ids: HashSet<String> = order
            .into_iter()
            .map(|id| {
                if first.is_none() {
                    first = Some(id.to_owned());
                }
                id.to_owned()
            })
            .collect();
        if ids.is_empty() {
            return;
        }
        let keep = |wanted: &Option<String>| wanted.clone().filter(|id| ids.contains(id));
        let primary = keep(&self.primary).or_else(|| first.clone());
        let anchor = keep(&self.anchor).or(first);
        self.primary = primary;
        self.anchor = anchor;
        self.ids = ids;
        self.touch();
    }

    /// Drops every selected id that `exists` does not know (files removed from
    /// the library).
    pub fn retain_existing(&mut self, exists: impl Fn(&str) -> bool) {
        let before = self.ids.len();
        self.ids.retain(|id| exists(id));
        if self.ids.len() != before {
            self.forget_anchor();
            self.touch();
        }
    }

    /// Keeps only the selected ids that are in the displayed `order` (the list
    /// was sorted, searched or rebuilt). The primary row stays selected even when
    /// it is not displayed, so it is still selected when the search is cleared.
    pub fn retain_displayed<'a>(&mut self, order: impl IntoIterator<Item = &'a str>) {
        if self.ids.is_empty() {
            return;
        }
        let mut kept: HashSet<String> = order
            .into_iter()
            .filter(|id| self.ids.contains(*id))
            .map(str::to_owned)
            .collect();
        if let Some(primary) = self.primary.as_ref().filter(|id| self.ids.contains(*id)) {
            kept.insert(primary.clone());
        }
        if kept.len() != self.ids.len() {
            self.ids = kept;
            self.forget_anchor();
            self.touch();
        }
    }

    fn forget_anchor(&mut self) {
        if self
            .anchor
            .as_ref()
            .is_some_and(|id| !self.ids.contains(id))
        {
            self.anchor = None;
        }
    }

    /// The selected ids in the displayed `order`, each once. Ids that are not
    /// displayed are left out.
    pub fn ordered<'a>(&self, order: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        if self.ids.is_empty() {
            return Vec::new();
        }
        let mut seen: HashSet<&str> = HashSet::with_capacity(self.ids.len());
        let mut out = Vec::with_capacity(self.ids.len());
        for id in order {
            if self.ids.contains(id) && seen.insert(id) {
                out.push(id.to_owned());
                if out.len() == self.ids.len() {
                    break;
                }
            }
        }
        out
    }

    /// The selected row closest to row `at` of `count` displayed rows (`id_at`
    /// names row `i`), looking below before above at the same distance.
    pub fn nearest_selected<'a>(
        &self,
        count: usize,
        at: usize,
        id_at: impl Fn(usize) -> &'a str,
    ) -> Option<usize> {
        if self.ids.is_empty() {
            return None;
        }
        for distance in 1..count {
            let below = at.checked_add(distance).filter(|row| *row < count);
            let above = at.checked_sub(distance).filter(|row| *row < count);
            if below.is_none() && above.is_none() {
                break;
            }
            for row in [below, above].into_iter().flatten() {
                if self.ids.contains(id_at(row)) {
                    return Some(row);
                }
            }
        }
        None
    }
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

impl Session {
    /// Removes the library records with these ids and everything that points at
    /// them from the queue and the playlists, remembers their paths as removed
    /// (rescans leave them out) and clears `current` and `selected` when they
    /// pointed at one. Listening positions, bookmarks, ratings and favourites
    /// stay. Returns what is needed to put it all back; `None` when none of the
    /// ids is in the library (the queue and playlists are cleaned of them anyway).
    pub fn remove_files(&mut self, ids: &[String]) -> Option<RemovedFiles> {
        let wanted: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let queue: Vec<(usize, String)> = self
            .queue
            .iter()
            .enumerate()
            .filter(|(_, id)| wanted.contains(id.as_str()))
            .map(|(index, id)| (index, id.clone()))
            .collect();
        self.queue.retain(|id| !wanted.contains(id.as_str()));
        let mut playlists = Vec::new();
        for list in &mut self.playlists {
            let entries: Vec<(usize, String)> = list
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, id)| wanted.contains(id.as_str()))
                .map(|(index, id)| (index, id.clone()))
                .collect();
            if !entries.is_empty() {
                list.tracks.retain(|id| !wanted.contains(id.as_str()));
                playlists.push((list.name.clone(), entries));
            }
        }
        let was_current = self
            .current
            .clone()
            .filter(|id| wanted.contains(id.as_str()));
        if was_current.is_some() {
            self.current = None;
        }
        let was_selected = self
            .selected
            .clone()
            .filter(|id| wanted.contains(id.as_str()));
        if was_selected.is_some() {
            self.selected = None;
        }
        let mut tracks = Vec::new();
        if self
            .library
            .iter()
            .any(|track| wanted.contains(track.id.as_str()))
        {
            let library = Arc::make_mut(&mut self.library);
            let mut kept = Vec::with_capacity(library.len());
            for (index, track) in std::mem::take(library).into_iter().enumerate() {
                if wanted.contains(track.id.as_str()) {
                    tracks.push((index, track));
                } else {
                    kept.push(track);
                }
            }
            *library = kept;
        }
        let retained_paths: HashSet<String> = self
            .library
            .iter()
            .map(|track| removal_key(&track.path))
            .collect();
        let keys: Vec<String> = tracks
            .iter()
            .map(|(_, track)| removal_key(&track.path))
            .filter(|key| !retained_paths.contains(key))
            .collect();
        let new_removals: Vec<String> = {
            let listed: HashSet<&str> = self.removed_paths.iter().map(String::as_str).collect();
            let mut seen = HashSet::new();
            keys.iter()
                .filter(|key| !key.is_empty() && !listed.contains(key.as_str()))
                .filter(|key| seen.insert(key.as_str()))
                .cloned()
                .collect()
        };
        push_removed_many(&mut self.removed_paths, &keys);
        if tracks.is_empty() {
            return None;
        }
        Some(RemovedFiles {
            tracks,
            queue,
            playlists,
            new_removals,
            was_current,
            was_selected,
        })
    }

    /// The saved progress of every current file of the book `key`, to put back
    /// with `UndoRestore::Progress`.
    pub fn capture_book_progress(&self, key: &str) -> Vec<(String, Option<Progress>)> {
        self.book_files(key)
            .map(|track| (track.id.clone(), self.progress.get(&track.id).cloned()))
            .collect()
    }

    /// Takes back one undoable action.
    pub fn apply_undo(&mut self, restore: UndoRestore) -> UndoOutcome {
        match restore {
            UndoRestore::Files(files) => self.restore_files(*files),
            UndoRestore::Queue(ids) => self.restore_queue(ids),
            UndoRestore::QueueEntries(entries) => self.restore_queue_entries(entries),
            UndoRestore::Bookmarks(entries) => {
                let count = entries.len();
                self.bookmarks = reinsert_sorted(std::mem::take(&mut self.bookmarks), entries);
                UndoOutcome {
                    view: true,
                    ..UndoOutcome::done(format!(
                        "Restored {}",
                        plural(count, "bookmark", "bookmarks")
                    ))
                }
            }
            UndoRestore::Playlist { index, playlist } => self.restore_playlist(index, playlist),
            UndoRestore::PlaylistEntries { name, entries } => {
                self.restore_playlist_entries(&name, entries)
            }
            UndoRestore::PlaylistRenamed { from, to } => self.restore_playlist_name(&from, &to),
            UndoRestore::Progress { before, reposition } => {
                self.restore_progress(before, reposition)
            }
            UndoRestore::Station { index, station } => self.restore_station(index, station),
            UndoRestore::ProgressImport(undo) => self.restore_progress_import(*undo),
        }
    }

    fn restore_files(&mut self, files: RemovedFiles) -> UndoOutcome {
        let RemovedFiles {
            tracks,
            queue,
            playlists,
            new_removals,
            was_current,
            was_selected,
        } = files;
        let ids_present: HashSet<String> =
            self.library.iter().map(|track| track.id.clone()).collect();
        let paths_present: HashSet<String> = self
            .library
            .iter()
            .filter(|track| !track.superseded)
            .map(|track| removal_key(&track.path))
            .collect();
        let (mut back, mut already, mut conflicts) = (0usize, 0usize, 0usize);
        let mut insert = Vec::new();
        // Ids that are in the library once this is done.
        let mut usable: HashSet<String> = HashSet::new();
        for (index, track) in tracks {
            if ids_present.contains(&track.id) {
                already += 1;
                usable.insert(track.id);
            } else if !track.superseded && paths_present.contains(&removal_key(&track.path)) {
                // The same file was added again with other content: restoring
                // this record would show it twice.
                conflicts += 1;
            } else {
                back += 1;
                usable.insert(track.id.clone());
                insert.push((index, track));
            }
        }
        if back + already == 0 {
            return UndoOutcome::refused(
                "Could not restore: the file was added again with other content",
            );
        }
        let mut outcome = UndoOutcome::default();
        if !insert.is_empty() {
            let library = Arc::make_mut(&mut self.library);
            *library = reinsert_sorted(std::mem::take(library), insert);
            outcome.library = true;
        }
        let queued: HashSet<String> = self.queue.iter().cloned().collect();
        let mut seen = HashSet::new();
        let entries: Vec<(usize, String)> = queue
            .into_iter()
            .filter(|(_, id)| {
                usable.contains(id) && !queued.contains(id) && seen.insert(id.clone())
            })
            .collect();
        if !entries.is_empty() {
            self.queue = reinsert_sorted(std::mem::take(&mut self.queue), entries);
        }
        for (name, entries) in playlists {
            let Some(list) = self.playlists.iter_mut().find(|list| list.name == name) else {
                continue;
            };
            let present: HashSet<String> = list.tracks.iter().cloned().collect();
            let mut seen = HashSet::new();
            let entries: Vec<(usize, String)> = entries
                .into_iter()
                .filter(|(_, id)| {
                    usable.contains(id) && !present.contains(id) && seen.insert(id.clone())
                })
                .collect();
            if !entries.is_empty() {
                list.tracks = reinsert_sorted(std::mem::take(&mut list.tracks), entries);
            }
        }
        if !new_removals.is_empty() {
            let gone: HashSet<&str> = new_removals.iter().map(String::as_str).collect();
            self.removed_paths
                .retain(|key| !gone.contains(key.as_str()));
        }
        if let Some(id) = was_current.filter(|id| usable.contains(id)) {
            if self.current.is_none() {
                self.current = Some(id.clone());
                outcome.current = Some(id);
            }
        }
        if let Some(id) = was_selected.filter(|id| usable.contains(id)) {
            if self.selected.is_none() {
                self.selected = Some(id);
            }
        }
        outcome.view = true;
        let mut parts = Vec::new();
        if back > 0 || already > 0 {
            parts.push(format!(
                "Restored {}",
                plural(back + already, "file", "files")
            ));
        }
        if already > 0 {
            parts.push(format!("{already} already in the library"));
        }
        if conflicts > 0 {
            parts.push(format!(
                "{} added again with other content and left as it is",
                plural(conflicts, "file was", "files were")
            ));
        }
        outcome.note = parts.join(" · ");
        outcome.restored = true;
        outcome
    }

    fn restore_queue(&mut self, ids: Vec<String>) -> UndoOutcome {
        let in_library: HashSet<&str> = self.library.iter().map(|t| t.id.as_str()).collect();
        let mut seen = HashSet::new();
        let mut restored: Vec<String> = ids
            .into_iter()
            .filter(|id| in_library.contains(id.as_str()) && seen.insert(id.clone()))
            .collect();
        let count = restored.len();
        restored.extend(
            std::mem::take(&mut self.queue)
                .into_iter()
                .filter(|id| !seen.contains(id)),
        );
        self.queue = restored;
        UndoOutcome {
            view: true,
            ..UndoOutcome::done(format!(
                "Restored the queue ({})",
                plural(count, "file", "files")
            ))
        }
    }

    fn restore_queue_entries(&mut self, entries: Vec<(usize, String)>) -> UndoOutcome {
        let in_library: HashSet<&str> = self.library.iter().map(|t| t.id.as_str()).collect();
        let queued: HashSet<&str> = self.queue.iter().map(String::as_str).collect();
        let mut seen = HashSet::new();
        let entries: Vec<(usize, String)> = entries
            .into_iter()
            .filter(|(_, id)| {
                in_library.contains(id.as_str())
                    && !queued.contains(id.as_str())
                    && seen.insert(id.clone())
            })
            .collect();
        let count = entries.len();
        if count == 0 {
            return UndoOutcome::refused("Nothing to put back in the queue");
        }
        self.queue = reinsert_sorted(std::mem::take(&mut self.queue), entries);
        UndoOutcome {
            view: true,
            ..UndoOutcome::done(format!(
                "Put {} back in the queue",
                plural(count, "file", "files")
            ))
        }
    }

    fn restore_playlist(&mut self, index: usize, mut playlist: Playlist) -> UndoOutcome {
        let mut renamed = false;
        if playlist_name_taken(&self.playlists, &playlist.name, None) {
            playlist.name = unique_playlist_name(&self.playlists, &playlist.name);
            renamed = true;
        }
        let name = playlist.name.clone();
        let at = index.min(self.playlists.len());
        self.playlists.insert(at, playlist);
        UndoOutcome {
            view: true,
            ..UndoOutcome::done(if renamed {
                format!("Restored the playlist as “{name}” (the old name is in use)")
            } else {
                format!("Restored the playlist “{name}”")
            })
        }
    }

    fn restore_playlist_entries(
        &mut self,
        name: &str,
        entries: Vec<(usize, String)>,
    ) -> UndoOutcome {
        let in_library: HashSet<&str> = self.library.iter().map(|t| t.id.as_str()).collect();
        let Some(list) = self.playlists.iter_mut().find(|list| list.name == name) else {
            return UndoOutcome::refused("Could not restore: that playlist no longer exists");
        };
        let present: HashSet<String> = list.tracks.iter().cloned().collect();
        let mut seen = HashSet::new();
        let entries: Vec<(usize, String)> = entries
            .into_iter()
            .filter(|(_, id)| {
                in_library.contains(id.as_str()) && !present.contains(id) && seen.insert(id.clone())
            })
            .collect();
        let count = entries.len();
        if count == 0 {
            return UndoOutcome::refused("Nothing to put back in the playlist");
        }
        list.tracks = reinsert_sorted(std::mem::take(&mut list.tracks), entries);
        UndoOutcome {
            view: true,
            ..UndoOutcome::done(format!(
                "Put {} back in “{name}”",
                plural(count, "file", "files")
            ))
        }
    }

    fn restore_playlist_name(&mut self, from: &str, to: &str) -> UndoOutcome {
        match self.rename_playlist(to, from) {
            Ok(name) => UndoOutcome {
                view: true,
                ..UndoOutcome::done(format!("Renamed the playlist back to “{name}”"))
            },
            Err(PlaylistProblem::Missing) => {
                UndoOutcome::refused("Could not undo: that playlist no longer exists")
            }
            Err(_) => UndoOutcome::refused(format!(
                "Could not undo: another playlist is called “{from}” now"
            )),
        }
    }

    fn restore_progress(
        &mut self,
        before: Vec<(String, Option<Progress>)>,
        reposition: bool,
    ) -> UndoOutcome {
        let count = before.len();
        let mut seek = None;
        for (id, previous) in before {
            if reposition && self.current.as_deref() == Some(id.as_str()) {
                seek = Some((id.clone(), previous.as_ref().map_or(0., |p| p.seconds)));
            }
            match previous {
                Some(progress) => {
                    self.progress.insert(id, progress);
                }
                None => {
                    self.progress.remove(&id);
                }
            }
        }
        UndoOutcome {
            progress: true,
            seek,
            ..UndoOutcome::done(format!(
                "Restored the progress of {}",
                plural(count, "file", "files")
            ))
        }
    }
}
/// Two saved positions of one file joined (see `Session::merge_track_history`):
/// the position and finished flag of the more recently played record (the old
/// one when only it has a position or on a tie), summed play counts and the
/// newest play time.
fn merged_progress(old: &Progress, new: &Progress) -> Progress {
    let has_position = |progress: &Progress| progress.seconds > 0. || progress.completed;
    let take_new = match (has_position(old), has_position(new)) {
        (_, false) => false,
        (false, true) => true,
        (true, true) => new.last_played > old.last_played,
    };
    let source = if take_new { new } else { old };
    Progress {
        seconds: source.seconds,
        completed: source.completed,
        last_played: old.last_played.max(new.last_played),
        play_count: old.play_count.saturating_add(new.play_count),
        file_size: if new.file_size != 0 {
            new.file_size
        } else {
            old.file_size
        },
        modified: if new.modified != 0 {
            new.modified
        } else {
            old.modified
        },
    }
}
/// Replaces `old` by `new` in an id list and keeps only the first `new`.
fn replace_id_once_listed(ids: &mut Vec<String>, old: &str, new: &str) {
    let mut seen = false;
    let replaced = std::mem::take(ids).into_iter().filter_map(|id| {
        let id = if id == old { new.to_owned() } else { id };
        if id == new {
            if seen {
                return None;
            }
            seen = true;
        }
        Some(id)
    });
    *ids = replaced.collect();
}
fn finite(x: f32, default: f32) -> f32 {
    if x.is_finite() { x } else { default }
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
/// An `Instant` `d` ago. Plain `Instant::now() - d` panics on Windows when `d`
/// exceeds the time since boot; this falls back to "now" instead.
pub fn instant_ago(d: Duration) -> Instant {
    Instant::now().checked_sub(d).unwrap_or_else(Instant::now)
}
pub fn profile_directory() -> PathBuf {
    let args: Vec<_> = std::env::args_os().collect();
    if let Some(i) = args.iter().position(|x| x == "--profile") {
        if let Some(p) = args.get(i + 1) {
            return PathBuf::from(p);
        }
    }
    #[cfg(target_os = "macos")]
    let macos_home = {
        // std's Unix implementation also consults the account database when
        // HOME is absent, as can happen outside an interactive shell.
        #[allow(deprecated)]
        let home = std::env::home_dir().filter(|path| path.is_absolute());
        Some(home.unwrap_or_else(|| {
            crate::startup_error(
                "Could not locate the macOS user home folder. Launch with --profile <folder> to choose a writable data location.",
            )
        }))
    };
    #[cfg(not(target_os = "macos"))]
    let macos_home: Option<PathBuf> = None;
    default_profile_directory(
        std::env::current_exe().ok().as_deref(),
        macos_home.as_deref(),
    )
}

fn default_profile_directory(executable: Option<&Path>, macos_home: Option<&Path>) -> PathBuf {
    if let Some(home) = macos_home {
        // Data stays outside the application bundle so moving/updating or
        // signing the .app does not alter its seal or replace the user's state.
        return home
            .join("Library")
            .join("Application Support")
            .join("BS Player");
    }
    executable
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("profile")
}
/// Reads a profile file in one go. The size is capped first, so the parse below
/// works on a slice instead of one system call per byte.
fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
    let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let length = f.metadata().map_err(|e| e.to_string())?.len();
    if length > MAX_SESSION_BYTES as u64 {
        return Err("Profile exceeds 64 MiB safety limit".into());
    }
    let mut data = Vec::with_capacity(length as usize);
    f.take(MAX_SESSION_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if data.len() > MAX_SESSION_BYTES {
        return Err("Profile exceeds 64 MiB safety limit".into());
    }
    Ok(data)
}
fn parse_session(data: &[u8], path: &Path) -> Result<Session, String> {
    let mut state: Session =
        serde_json::from_slice(data).map_err(|e| format!("{}: {e}", path.display()))?;
    if state.version > SESSION_VERSION {
        return Err(
            "This profile was written by a newer BS Player; use that version to preserve its data."
                .into(),
        );
    }
    if state.version < SESSION_VERSION {
        // v1 Track records gain optional identity metadata as they are refreshed.
        // Keep their IDs and all referencing session data opaque and unchanged.
        state.version = SESSION_VERSION;
    }
    state.normalize();
    Ok(state)
}
fn read_session(path: &Path) -> Result<Session, String> {
    parse_session(&read_limited(path)?, path)
}
/// Refuses a profile written by a newer version. Returns the bytes of
/// `session.json` (when it could be read) so a save does not read it again.
fn compatible_bytes(profile: &Path) -> Result<Option<Vec<u8>>, String> {
    #[derive(Deserialize)]
    struct Version {
        version: Option<u64>,
    }
    let path = profile.join("session.json");
    let Ok(file) = File::open(&path) else {
        return Ok(None);
    };
    let Ok(length) = file.metadata().map(|m| m.len()) else {
        return Err("Profile is unreadable or exceeds 64 MiB; it was not changed.".into());
    };
    if length > MAX_SESSION_BYTES as u64 {
        return Err("Profile is unreadable or exceeds 64 MiB; it was not changed.".into());
    }
    let mut data = Vec::with_capacity(length as usize);
    if file
        .take(MAX_SESSION_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .is_err()
    {
        return Ok(None);
    }
    if data.len() > MAX_SESSION_BYTES {
        return Err("Profile is unreadable or exceeds 64 MiB; it was not changed.".into());
    }
    if let Ok(found) = serde_json::from_slice::<Version>(&data) {
        if found.version.unwrap_or(1) > u64::from(SESSION_VERSION) {
            return Err("This profile belongs to a newer BS Player version. It has not been changed. Open it with that version, or select a different --profile directory.".into());
        }
    }
    Ok(Some(data))
}
pub fn check_compatible(profile: &Path) -> Result<(), String> {
    compatible_bytes(profile).map(|_| ())
}
pub fn load(profile: &Path) -> (Session, Option<String>) {
    let main = profile.join("session.json");
    if !main.exists() && !profile.join("session.backup.json").exists() {
        return (Session::default(), None);
    }
    match read_session(&main) {
        Ok(s) => (s, None),
        Err(e) => match read_session(&profile.join("session.backup.json")) {
            Ok(s) => (
                s,
                Some(format!("Recovered the last good profile backup. {e}")),
            ),
            Err(_) => {
                // The first save sets the unreadable main file aside, and the
                // second would replace the unreadable backup: copy it now.
                let backup = profile.join("session.backup.json");
                if backup.exists() {
                    let _ = fs::copy(
                        &backup,
                        profile.join(format!(
                            "session.backup.unreadable-{}-{}.json",
                            now(),
                            std::process::id()
                        )),
                    );
                }
                (
                    Session::default(),
                    Some(format!(
                        "Could not read the saved session. Original files are preserved; export or inspect them before saving a new session. {e}"
                    )),
                )
            }
        },
    }
}
pub fn save(profile: &Path, state: &Session) -> Result<(), String> {
    save_with_limit(profile, state, MAX_SESSION_BYTES)
}
fn save_with_limit(profile: &Path, state: &Session, max_bytes: usize) -> Result<(), String> {
    let previous = compatible_bytes(profile)?;
    let bytes = serialize_session_with_limit(state, max_bytes)
        .map_err(|error| format!("{error}; the existing profile was not changed."))?;
    fs::create_dir_all(profile).map_err(|e| e.to_string())?;
    let target = profile.join("session.json");
    // Only a readable previous generation is promoted to the backup.
    match previous {
        Some(data) if parse_session(&data, &target).is_ok() => {
            atomic_write(&profile.join("session.backup.json"), &data)?;
        }
        _ if target.exists() => {
            let recovered = profile.join(format!(
                "session.unreadable-{}-{}-{}.json",
                now(),
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos()
            ));
            fs::copy(&target, &recovered)
                .map_err(|e| format!("Could not preserve unreadable profile: {e}"))?;
        }
        _ => {}
    }
    atomic_write(&target, &bytes)
}
pub fn export(path: &Path, state: &Session) -> Result<(), String> {
    // Non-finite numbers would be written as null and make the file unreadable.
    let mut sanitized = state.clone();
    sanitized.normalize();
    let bytes = serialize_session(&sanitized)
        .map_err(|error| format!("{error}; the backup was not written."))?;
    atomic_write(path, &bytes)
}
fn serialize_session(state: &Session) -> Result<Vec<u8>, String> {
    serialize_session_with_limit(state, MAX_SESSION_BYTES)
}
/// JSON cannot hold a path that is not valid Unicode, and one such path would
/// stop every save. Returns a copy with the offending paths made lossy, or
/// `None` when every path is already valid.
fn with_lossy_paths(state: &Session) -> Option<Session> {
    let bad = |path: &Path| path.to_str().is_none();
    if !state.library.iter().any(|t| bad(&t.path)) && !state.roots.iter().any(|(p, _)| bad(p)) {
        return None;
    }
    let lossy = |path: &Path| PathBuf::from(path.to_string_lossy().into_owned());
    let mut copy = state.clone();
    for track in Arc::make_mut(&mut copy.library) {
        if bad(&track.path) {
            track.path = lossy(&track.path);
        }
    }
    for (root, _) in &mut copy.roots {
        if bad(root) {
            *root = lossy(root);
        }
    }
    Some(copy)
}
fn serialize_session_with_limit(state: &Session, max_bytes: usize) -> Result<Vec<u8>, String> {
    let lossy = with_lossy_paths(state);
    let state = lossy.as_ref().unwrap_or(state);
    let mut writer = LimitedSessionWriter::new(max_bytes);
    match serde_json::to_writer_pretty(&mut writer, state) {
        Ok(()) => Ok(writer.bytes),
        Err(_) if writer.limit_exceeded => Err(format!(
            "Serialized session exceeds the {max_bytes} byte safety limit"
        )),
        Err(error) => Err(error.to_string()),
    }
}

struct LimitedSessionWriter {
    bytes: Vec<u8>,
    max_bytes: usize,
    limit_exceeded: bool,
}

impl LimitedSessionWriter {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(max_bytes.min(64 * 1024)),
            max_bytes,
            limit_exceeded: false,
        }
    }
}

impl Write for LimitedSessionWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let Some(required) = self.bytes.len().checked_add(bytes.len()) else {
            self.limit_exceeded = true;
            return Err(std::io::Error::other("session size overflow"));
        };
        if required > self.max_bytes {
            self.limit_exceeded = true;
            return Err(std::io::Error::other("session size limit exceeded"));
        }
        if required > self.bytes.capacity() {
            let doubled_capacity = self.bytes.capacity().saturating_mul(2);
            let target_capacity = required.max(doubled_capacity).min(self.max_bytes);
            self.bytes
                .try_reserve_exact(target_capacity.saturating_sub(self.bytes.len()))
                .map_err(std::io::Error::other)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn import(path: &Path) -> Result<Session, String> {
    read_session(path)
}
/// Removes `.session*.pending` temporaries a crashed save left behind. Call it
/// only while holding the profile lock, so no other instance is mid-write.
pub fn remove_stale_pending(profile: &Path) {
    let Ok(entries) = fs::read_dir(profile) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".session.") && name.ends_with(".pending") {
            let _ = fs::remove_file(entry.path());
        }
    }
}
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temporary = parent.join(format!(".{name}.{}.pending", std::process::id()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&temporary).map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        drop(f);
        replace(&temporary, path)?;
        #[cfg(unix)]
        {
            // The rename has already happened: a file system that refuses to
            // sync a directory must not turn a completed save into a failure.
            let _ = File::open(parent).and_then(|f| f.sync_all());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> Result<(), String> {
    fs::rename(from, to).map_err(|e| e.to_string())
}
#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut attempt = 1;
    loop {
        if unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } != 0
        {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        // A scanner, indexer or sync client may briefly hold the target open
        // (access denied, sharing or lock violation): retry for about half a second.
        if attempt >= 5 || !matches!(error.raw_os_error(), Some(5 | 32 | 33)) {
            return Err(error.to_string());
        }
        attempt += 1;
        std::thread::sleep(Duration::from_millis(100));
    }
}

// -- Reconnecting a moved folder ------------------------------------------------

/// What `Session::apply_relocation` did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RelocationApplied {
    /// Records that now point at the file in its new place (same id).
    pub reconnected: usize,
    /// Records whose history was moved to the record the new place already had.
    pub merged: usize,
    /// The new place already has a record with other content: left alone.
    pub conflicts: usize,
    /// The record of the file that is playing is not merged into another one.
    pub kept_current: usize,
    /// The record was removed or changed while the job ran: left alone.
    pub changed_meanwhile: usize,
    /// The watched folder list now names the new folder.
    pub root_replaced: bool,
}

impl Session {
    /// The library files below the watched folder `root` that a folder move looks
    /// for, and the form of `root` they are below (the folder as typed, or its
    /// canonical form when the library knows its files by that). Paths are
    /// compared folded (see `removal_key`): on Windows case and `\\?\` do not matter.
    pub fn relocation_wants(&self, root: &Path) -> (Vec<media::RelocateWant>, PathBuf) {
        let windows = cfg!(windows);
        let mut forms = vec![root.to_path_buf()];
        if let Ok(canonical) = fs::canonicalize(root) {
            if canonical != root {
                forms.push(canonical);
            }
        }
        for form in forms {
            let key = fold_removal_key(&form.to_string_lossy(), windows);
            let wants: Vec<media::RelocateWant> = self
                .library
                .iter()
                .filter(|track| !track.superseded)
                .filter(|track| {
                    let path = removal_key(&track.path);
                    media::relative_key(&key, &path, windows).is_some()
                })
                .map(media::RelocateWant::of)
                .collect();
            if !wants.is_empty() {
                return (wants, form);
            }
        }
        (Vec::new(), root.to_path_buf())
    }

    /// Points the records of the verified `moves` at their files in the new place.
    /// A record keeps its id, so its position, bookmarks, rating, favourite,
    /// queue and playlist entries and the loaded and selected rows stay attached;
    /// size and modification time come from the new file. When the library
    /// already has a record at the new path (the new folder was added before),
    /// the verified old record's history moves to it (see `merge_track_histories`),
    /// unless the old record is loaded, in which case the destination row's
    /// history is merged into it and its path is updated so the player keeps its
    /// current id. Content conflicts stay untouched. With
    /// `replace_root` the watched folder `old_root` is replaced by `new_root`
    /// (keeping its audiobook flag), or just dropped when another watched folder
    /// already covers `new_root`.
    pub fn apply_relocation(
        &mut self,
        moves: &[media::AcceptedMove],
        old_root: &Path,
        new_root: &Path,
        replace_root: bool,
    ) -> RelocationApplied {
        let mut applied = RelocationApplied::default();
        let current = self.current.clone();
        let mut merges: Vec<(String, String)> = Vec::new();
        let mut speed_copies: Vec<(String, String)> = Vec::new();
        let mut current_progress_overrides: Vec<(String, Option<Progress>)> = Vec::new();
        let mut forget: HashSet<String> = HashSet::new();
        // Nothing verified: the library is not even copied (it may be shared).
        if !moves.is_empty() {
            let library = Arc::make_mut(&mut self.library);
            let by_id: HashMap<String, usize> = library
                .iter()
                .enumerate()
                .map(|(index, track)| (track.id.clone(), index))
                .collect();
            let mut by_path: HashMap<String, usize> = library
                .iter()
                .enumerate()
                .filter(|(_, track)| !track.superseded)
                .map(|(index, track)| (removal_key(&track.path), index))
                .collect();
            for moved in moves {
                let Some(&index) = by_id.get(moved.id.as_str()) else {
                    applied.changed_meanwhile += 1;
                    continue;
                };
                if library[index].superseded || library[index].path != moved.old_path {
                    applied.changed_meanwhile += 1;
                    continue;
                }
                let new_key = removal_key(&moved.new_path);
                let old_key = removal_key(&moved.old_path);
                if let Some(&other) = by_path.get(&new_key).filter(|&&other| other != index) {
                    let existing_id = library[other].id.clone();
                    let existing_key = library[other].book_key();
                    let same = moved.digest.is_some()
                        && library[other].content_digest == moved.digest
                        && library[other].file_size == moved.file_size;
                    if !same {
                        applied.conflicts += 1;
                    } else if current.as_deref() == Some(moved.id.as_str()) {
                        // Keep the engine's current id, while merging the row
                        // already at the destination into it. Compute the
                        // current row's post-move book identity first so both
                        // records' remembered speeds can follow that identity.
                        let current_key = library[index].book_key();
                        let destination_key = existing_key;
                        let mut moved_current = library[index].clone();
                        moved_current.path = moved.new_path.clone();
                        let after_key = moved_current.book_key();
                        if destination_key != after_key {
                            speed_copies.push((destination_key, after_key.clone()));
                        }
                        if current_key != after_key {
                            speed_copies.push((current_key, after_key));
                        }
                        let current_progress = self.progress.get(&moved.id).cloned();
                        let destination_progress = self.progress.get(&existing_id).cloned();
                        let merged = match (current_progress, destination_progress) {
                            (Some(mut current), Some(destination)) => {
                                current.play_count = current
                                    .play_count
                                    .saturating_add(destination.play_count);
                                current.last_played = current.last_played.max(destination.last_played);
                                current.file_size = moved.file_size;
                                current.modified = moved.modified;
                                Some(current)
                            }
                            (Some(mut current), None) => {
                                current.file_size = moved.file_size;
                                current.modified = moved.modified;
                                Some(current)
                            }
                            (None, Some(mut destination)) => {
                                destination.file_size = moved.file_size;
                                destination.modified = moved.modified;
                                Some(destination)
                            }
                            (None, None) => None,
                        };
                        let current_record = &mut library[index];
                        current_record.path = moved.new_path.clone();
                        current_record.file_size = moved.file_size;
                        current_record.modified = moved.modified;
                        current_record.modified_nanos = moved.modified_nanos;
                        by_path.remove(&old_key);
                        by_path.insert(new_key.clone(), index);
                        current_progress_overrides.push((moved.id.clone(), merged));
                        merges.push((existing_id, moved.id.clone()));
                        applied.merged += 1;
                        forget.insert(new_key);
                        forget.insert(old_key);
                    } else {
                        let before = library[index].book_key();
                        let after = existing_key;
                        if before != after {
                            speed_copies.push((before, after));
                        }
                        merges.push((moved.id.clone(), existing_id));
                        applied.merged += 1;
                        forget.insert(new_key);
                    }
                    continue;
                }
                let record = &mut library[index];
                let before = record.book_key();
                record.path = moved.new_path.clone();
                record.file_size = moved.file_size;
                record.modified = moved.modified;
                record.modified_nanos = moved.modified_nanos;
                let after = record.book_key();
                if before != after {
                    speed_copies.push((before, after));
                }
                if let Some(progress) = self.progress.get_mut(&moved.id) {
                    progress.file_size = moved.file_size;
                    progress.modified = moved.modified;
                }
                by_path.remove(&old_key);
                by_path.insert(new_key.clone(), index);
                forget.insert(new_key);
                forget.insert(old_key);
                applied.reconnected += 1;
            }
        }
        for (before, after) in speed_copies {
            if let Some(speed) = self.book_speeds.get(&before).copied() {
                self.book_speeds.entry(after).or_insert(speed);
            }
        }
        self.merge_track_histories(&merges);
        for (id, progress) in current_progress_overrides {
            match progress {
                Some(progress) => {
                    self.progress.insert(id, progress);
                }
                None => {
                    self.progress.remove(&id);
                }
            }
        }
        if !forget.is_empty() {
            self.removed_paths.retain(|key| !forget.contains(key));
        }
        if replace_root {
            applied.root_replaced = self.replace_root(old_root, new_root);
        }
        applied
    }

    /// Replaces the watched folder `old_root` by `new_root`, or drops it when
    /// another watched folder already holds `new_root`. False when `old_root`
    /// is not a watched folder.
    fn replace_root(&mut self, old_root: &Path, new_root: &Path) -> bool {
        let windows = cfg!(windows);
        let old_key = removal_key(old_root);
        let new_key = removal_key(new_root);
        let Some(index) = self
            .roots
            .iter()
            .position(|(path, _)| removal_key(path) == old_key)
        else {
            return false;
        };
        let covered = self.roots.iter().enumerate().any(|(other, (path, _))| {
            let key = removal_key(path);
            other != index
                && (key == new_key || media::relative_key(&key, &new_key, windows).is_some())
        });
        if covered {
            self.roots.remove(index);
        } else {
            self.roots[index].0 = new_root.to_path_buf();
        }
        true
    }

    /// `merge_track_history` for many records at once: each `(old id, new id)`
    /// pair moves the history of the old record to the new one and removes the
    /// old record, in one pass over the lists instead of one per pair. The pairs'
    /// old ids must be distinct from every new id. The loaded row follows too,
    /// which means the player would have to reload it: callers keep the loaded
    /// record out of the pairs.
    pub fn merge_track_histories(&mut self, pairs: &[(String, String)]) {
        let pairs: Vec<&(String, String)> = pairs.iter().filter(|(old, new)| old != new).collect();
        if pairs.is_empty() {
            return;
        }
        let map: HashMap<&str, &str> = pairs
            .iter()
            .map(|(old, new)| (old.as_str(), new.as_str()))
            .collect();
        let news: HashSet<&str> = pairs.iter().map(|(_, new)| new.as_str()).collect();
        for (old, new) in &pairs {
            if let Some(old_progress) = self.progress.remove(old) {
                let merged = match self.progress.get(new) {
                    Some(new_progress) => merged_progress(&old_progress, new_progress),
                    None => old_progress,
                };
                self.progress.insert(new.clone(), merged);
            }
            if let Some(rating) = self.ratings.remove(old) {
                if rating > 0 && self.ratings.get(new).copied().unwrap_or(0) == 0 {
                    self.ratings.insert(new.clone(), rating);
                }
            }
        }
        for bookmark in &mut self.bookmarks {
            if let Some(new) = map.get(bookmark.track_id.as_str()) {
                bookmark.track_id = (*new).to_owned();
            }
        }
        let favorite_olds: Vec<&(String, String)> = pairs
            .iter()
            .copied()
            .filter(|(old, _)| self.favorites.iter().any(|id| id == old))
            .collect();
        if !favorite_olds.is_empty() {
            self.favorites.retain(|id| !map.contains_key(id.as_str()));
            let mut listed: HashSet<String> = self.favorites.iter().cloned().collect();
            for (_, new) in favorite_olds {
                if listed.insert(new.clone()) {
                    self.favorites.push(new.clone());
                }
            }
        }
        let replace_once = |ids: &mut Vec<String>| {
            let mut seen: HashSet<String> = HashSet::new();
            let replaced = std::mem::take(ids).into_iter().filter_map(|id| {
                let id = map.get(id.as_str()).map_or(id, |new| (*new).to_owned());
                if news.contains(id.as_str()) && !seen.insert(id.clone()) {
                    return None;
                }
                Some(id)
            });
            *ids = replaced.collect();
        };
        replace_once(&mut self.queue);
        replace_once(&mut self.shuffle_history);
        for playlist in &mut self.playlists {
            replace_once(&mut playlist.tracks);
        }
        for slot in [&mut self.current, &mut self.selected] {
            if let Some(new) = slot.as_deref().and_then(|id| map.get(id)) {
                *slot = Some((*new).to_owned());
            }
        }
        Arc::make_mut(&mut self.library).retain(|track| !map.contains_key(track.id.as_str()));
    }
}

// -- The portable listening progress file ---------------------------------------

pub const PROGRESS_FILE_FORMAT: &str = "bs-player-progress";
pub const PROGRESS_FILE_VERSION: u32 = 1;
/// The largest progress file that is written or read.
pub const MAX_PROGRESS_FILE_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_PROGRESS_ENTRIES: usize = 200_000;
/// Longest text (digest, key, bookmark name or note) of a progress file entry.
const MAX_PROGRESS_TEXT_CHARS: usize = 1_000;
/// Most bookmarks one entry of a progress file may hold.
const MAX_PROGRESS_BOOKMARKS: usize = 1_000;
/// Most book speeds a progress file may hold.
const MAX_PROGRESS_SPEEDS: usize = 100_000;
/// Each of the three tag parts of a fallback key is cut to this many characters.
const MAX_PROGRESS_KEY_PART_CHARS: usize = 300;

/// A bookmark of a progress file: where it is, its name and its note.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressBookmark {
    pub seconds: f64,
    pub name: String,
    pub note: String,
}

/// What one file of the library has to say in a progress file. It holds no
/// path: the file is found again by its content `digest`, or by the `key`
/// (folded title, author and album tags and the length in whole seconds).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressEntry {
    /// `Track::content_digest`, or empty when the record has none.
    pub digest: String,
    /// `progress_key`, or empty when the file has no title, author and album.
    pub key: String,
    pub seconds: f64,
    pub completed: bool,
    pub play_count: u32,
    pub last_played: u64,
    pub rating: u8,
    pub favorite: bool,
    pub bookmarks: Vec<ProgressBookmark>,
}

/// The speed remembered for a book (by the book key the profile stores).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressSpeed {
    pub key: String,
    pub speed: f32,
}

/// The portable listening progress file (`bs-player-progress.json`): meant to
/// be put in a synced folder and imported by BS Player on another computer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressFile {
    pub format: String,
    pub version: u32,
    pub exported_at: u64,
    pub entries: Vec<ProgressEntry>,
    pub book_speeds: Vec<ProgressSpeed>,
}

/// The fallback key of a library file: the folded title, author and album tags
/// and the length rounded to the second, or empty when the file has no tags to
/// go by. It is made from tags only, so it never holds a file name.
pub fn progress_key(track: &Track) -> String {
    let part = |text: &str| -> String {
        media::fold_text(text.trim())
            .chars()
            .take(MAX_PROGRESS_KEY_PART_CHARS)
            .collect()
    };
    let (title, author, album) = (part(&track.title), part(&track.artist), part(&track.album));
    if title.is_empty() && author.is_empty() && album.is_empty() {
        return String::new();
    }
    let seconds = if track.duration.is_finite() && track.duration > 0. {
        track.duration.round().min(MAX_POSITION_SECONDS) as u64
    } else {
        0
    };
    format!("{title}|{author}|{album}|{seconds}")
}

fn has_progress(progress: &Progress) -> bool {
    progress.seconds > 0.
        || progress.completed
        || progress.play_count > 0
        || progress.last_played > 0
}

fn cap_progress_text(text: &str) -> String {
    text.chars().take(MAX_PROGRESS_TEXT_CHARS).collect()
}

fn clean_seconds(seconds: f64) -> f64 {
    if seconds.is_finite() && seconds > 0. {
        seconds.min(MAX_POSITION_SECONDS)
    } else {
        0.
    }
}

/// The book keys of the library that name an album (`album — author`): the only
/// ones a progress file may carry, as the others are folder paths.
fn album_book_keys(library: &[Track]) -> HashSet<String> {
    library
        .iter()
        .filter(|track| !track.superseded && !track.album.trim().is_empty())
        .map(Track::book_key)
        .collect()
}

impl Session {
    /// Everything this profile knows about the listening of its library files
    /// (position, finished flag, play count, rating, favourite, bookmarks), for
    /// every file that has any of it, and the remembered speed of each book that
    /// has an album tag. `skip` is a file that is left out (the private one).
    pub fn export_progress(&self, skip: Option<&str>, exported_at: u64) -> ProgressFile {
        let mut marks: HashMap<&str, Vec<ProgressBookmark>> = HashMap::new();
        for bookmark in &self.bookmarks {
            marks
                .entry(bookmark.track_id.as_str())
                .or_default()
                .push(ProgressBookmark {
                    seconds: clean_seconds(bookmark.seconds),
                    name: cap_progress_text(&bookmark.name),
                    note: cap_progress_text(&bookmark.note),
                });
        }
        let favorites: HashSet<&str> = self.favorites.iter().map(String::as_str).collect();
        let mut entries = Vec::new();
        for track in self.library.iter() {
            if track.superseded || skip == Some(track.id.as_str()) {
                continue;
            }
            let progress = self
                .progress
                .get(&track.id)
                .filter(|progress| has_progress(progress));
            let rating = self.ratings.get(&track.id).copied().unwrap_or(0).min(5);
            let favorite = favorites.contains(track.id.as_str());
            let mut bookmarks = marks.get(track.id.as_str()).cloned().unwrap_or_default();
            bookmarks.truncate(MAX_PROGRESS_BOOKMARKS);
            if progress.is_none() && rating == 0 && !favorite && bookmarks.is_empty() {
                continue;
            }
            if entries.len() >= MAX_PROGRESS_ENTRIES {
                break;
            }
            let mut entry = ProgressEntry {
                digest: cap_progress_text(track.content_digest.as_deref().unwrap_or("")),
                key: progress_key(track),
                rating,
                favorite,
                bookmarks,
                ..ProgressEntry::default()
            };
            if let Some(progress) = progress {
                entry.seconds = clean_seconds(progress.seconds);
                entry.completed = progress.completed;
                entry.play_count = u32::try_from(progress.play_count).unwrap_or(u32::MAX);
                entry.last_played = progress.last_played;
            }
            entries.push(entry);
        }
        let keys = album_book_keys(&self.library);
        let mut speeds: Vec<ProgressSpeed> = self
            .book_speeds
            .iter()
            .filter(|(key, speed)| keys.contains(*key) && speed.is_finite() && **speed > 0.)
            .filter(|(key, _)| key.chars().count() <= MAX_PROGRESS_TEXT_CHARS)
            .map(|(key, speed)| ProgressSpeed {
                key: key.clone(),
                speed: *speed,
            })
            .collect();
        speeds.sort_by(|a, b| a.key.cmp(&b.key));
        speeds.truncate(MAX_PROGRESS_SPEEDS);
        ProgressFile {
            format: PROGRESS_FILE_FORMAT.to_owned(),
            version: PROGRESS_FILE_VERSION,
            exported_at,
            entries,
            book_speeds: speeds,
        }
    }

    /// Builds a portable progress file only when its safety limits will not
    /// silently omit eligible history.
    pub fn try_export_progress(
        &self,
        skip: Option<&str>,
        exported_at: u64,
    ) -> Result<ProgressFile, String> {
        let mut marks: HashMap<&str, usize> = HashMap::new();
        for bookmark in &self.bookmarks {
            *marks.entry(bookmark.track_id.as_str()).or_default() += 1;
        }
        let favorites: HashSet<&str> = self.favorites.iter().map(String::as_str).collect();
        let mut entries = 0usize;
        for track in self.library.iter() {
            if track.superseded || skip == Some(track.id.as_str()) {
                continue;
            }
            let has_progress = self
                .progress
                .get(&track.id)
                .is_some_and(has_progress);
            let rating = self.ratings.get(&track.id).copied().unwrap_or(0).min(5);
            let favorite = favorites.contains(track.id.as_str());
            let bookmark_count = marks.get(track.id.as_str()).copied().unwrap_or(0);
            if bookmark_count > MAX_PROGRESS_BOOKMARKS {
                return Err(format!(
                    "The progress export was not written: {} has more than {MAX_PROGRESS_BOOKMARKS} bookmarks.",
                    track.display_title()
                ));
            }
            if has_progress || rating > 0 || favorite || bookmark_count > 0 {
                entries += 1;
                if entries > MAX_PROGRESS_ENTRIES {
                    return Err(format!(
                        "The progress export was not written: it contains more than {MAX_PROGRESS_ENTRIES} eligible files. Remove some library entries or export a full backup instead."
                    ));
                }
            }
        }
        let book_keys = album_book_keys(&self.library);
        let speed_count = self
            .book_speeds
            .iter()
            .filter(|(key, speed)| {
                book_keys.contains(*key)
                    && speed.is_finite()
                    && **speed > 0.
                    && key.chars().count() <= MAX_PROGRESS_TEXT_CHARS
            })
            .count();
        if speed_count > MAX_PROGRESS_SPEEDS {
            return Err(format!(
                "The progress export was not written: it contains more than {MAX_PROGRESS_SPEEDS} saved book speeds."
            ));
        }
        Ok(self.export_progress(skip, exported_at))
    }
}

/// The text of `file` as it is written: indented, or compact when that alone
/// keeps it within `MAX_PROGRESS_FILE_BYTES`.
pub fn progress_file_bytes(file: &ProgressFile) -> Result<Vec<u8>, String> {
    let limit = MAX_PROGRESS_FILE_BYTES as usize;
    let pretty = serde_json::to_vec_pretty(file).map_err(|error| error.to_string())?;
    if pretty.len() <= limit {
        return Ok(pretty);
    }
    let compact = serde_json::to_vec(file).map_err(|error| error.to_string())?;
    if compact.len() <= limit {
        return Ok(compact);
    }
    Err("The listening progress is larger than 50 MiB and was not exported.".to_owned())
}

/// Writes `file` to `path` (a temporary file in the same folder, then a rename,
/// so a synced copy is never half written).
pub fn write_progress_file(path: &Path, file: &ProgressFile) -> Result<(), String> {
    atomic_write(path, &progress_file_bytes(file)?)
}

/// Reads a progress file within the size limit (see `parse_progress_file`).
pub fn read_progress_file(path: &Path) -> Result<ProgressFile, String> {
    let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let length = file.metadata().map_err(|error| error.to_string())?.len();
    if length > MAX_PROGRESS_FILE_BYTES {
        return Err(
            "This file is larger than 50 MiB, so it is not a BS Player progress file.".into(),
        );
    }
    let mut data = Vec::with_capacity(length as usize);
    file.take(MAX_PROGRESS_FILE_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    parse_progress_file(&data)
}

/// Parses a progress file. Refused, with a sentence saying why: text that is not
/// JSON, JSON that is not a progress file, a newer version, more than
/// `MAX_PROGRESS_ENTRIES` entries. Fields this build does not know are ignored.
/// The values of the entries are checked when they are applied.
pub fn parse_progress_file(bytes: &[u8]) -> Result<ProgressFile, String> {
    #[derive(Deserialize)]
    struct Header {
        format: Option<String>,
        version: Option<u64>,
    }
    if bytes.len() as u64 > MAX_PROGRESS_FILE_BYTES {
        return Err(
            "This file is larger than 50 MiB, so it is not a BS Player progress file.".into(),
        );
    }
    let header: Header = serde_json::from_slice(bytes)
        .map_err(|_| "This is not a BS Player progress file (it cannot be read as JSON).")?;
    if header.format.as_deref() != Some(PROGRESS_FILE_FORMAT) {
        return Err("This is not a BS Player progress file.".into());
    }
    match header.version {
        Some(version) if version > u64::from(PROGRESS_FILE_VERSION) => {
            return Err(format!(
                "This progress file is version {version}, newer than this BS Player understands (version {PROGRESS_FILE_VERSION}). Update BS Player to import it."
            ));
        }
        Some(version) if version >= 1 => {}
        _ => return Err("This progress file has no valid version number.".into()),
    }
    let file: ProgressFile = serde_json::from_slice(bytes)
        .map_err(|error| format!("This progress file is damaged ({error})."))?;
    if file.entries.len() > MAX_PROGRESS_ENTRIES {
        return Err(format!(
            "This progress file has more than {MAX_PROGRESS_ENTRIES} entries and was not read."
        ));
    }
    if file.book_speeds.len() > MAX_PROGRESS_SPEEDS {
        return Err(format!(
            "This progress file has more than {MAX_PROGRESS_SPEEDS} book speeds and was not read."
        ));
    }
    Ok(file)
}

/// Whether `entry` is within the limits (see `ProgressFile`); an entry that is
/// not is skipped when a file is applied.
fn progress_entry_valid(entry: &ProgressEntry) -> bool {
    let text_ok = |text: &str| text.chars().count() <= MAX_PROGRESS_TEXT_CHARS;
    let number_ok = |value: f64| value.is_finite() && value >= 0.;
    text_ok(&entry.digest)
        && text_ok(&entry.key)
        && number_ok(entry.seconds)
        && entry.rating <= 5
        && entry.bookmarks.len() <= MAX_PROGRESS_BOOKMARKS
        && entry.bookmarks.iter().all(|bookmark| {
            number_ok(bookmark.seconds) && text_ok(&bookmark.name) && text_ok(&bookmark.note)
        })
}

/// What `Session::apply_progress_import` did.
#[derive(Debug, Default, PartialEq)]
pub struct ProgressImport {
    /// Files of the library that something was applied to.
    pub files: usize,
    /// Files whose saved position here is newer than the file's: left as it is.
    pub newer_here: usize,
    /// Entries for files this library does not have.
    pub unmatched: usize,
    /// Entries that fit several library files (duplicates): not applied.
    pub ambiguous: usize,
    /// Entries outside the limits.
    pub invalid: usize,
    pub bookmarks_added: usize,
    pub speeds_applied: usize,
    /// The loaded file's position changed: seek there.
    pub seek: Option<(String, f64)>,
    /// What to put back for Undo; `None` when nothing changed.
    pub undo: Option<ProgressUndo>,
}

impl ProgressImport {
    /// The sentence for the status bar and the message window.
    pub fn summary(&self) -> String {
        let mut text = format!(
            "Imported progress for {} · {} already newer here · {} not in this library · {} added",
            plural(self.files, "file", "files"),
            self.newer_here,
            self.unmatched,
            plural(self.bookmarks_added, "bookmark", "bookmarks"),
        );
        if self.ambiguous > 0 {
            text.push_str(&format!(
                " · {} matched several files and were skipped",
                self.ambiguous
            ));
        }
        if self.invalid > 0 {
            text.push_str(&format!(" · {} invalid entries skipped", self.invalid));
        }
        if self.speeds_applied > 0 {
            text.push_str(&format!(
                " · {} set",
                plural(self.speeds_applied, "book speed", "book speeds")
            ));
        }
        text
    }
}

/// Where an entry of a progress file belongs.
enum EntryMatch {
    Track(usize),
    Ambiguous,
    Missing,
}

impl Session {
    /// Merges a progress file into this profile; per entry the library file is
    /// found by digest, else by the fallback key when exactly one file has it.
    /// Position, finished flag, play count and last-played time are taken only
    /// when the entry was last played after the saved one (or this file has
    /// none; the play count never goes down); a rating or favourite only where
    /// there is none here; bookmarks are added unless one with the same name at
    /// the same second exists; a book speed only where the book has none.
    /// `skip` is a file that is never touched (the private one).
    pub fn apply_progress_import(
        &mut self,
        file: &ProgressFile,
        skip: Option<&str>,
    ) -> ProgressImport {
        let mut outcome = ProgressImport::default();
        let mut by_digest: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, track) in self.library.iter().enumerate() {
            if track.superseded || skip == Some(track.id.as_str()) {
                continue;
            }
            if let Some(digest) = track.content_digest.as_deref().filter(|d| !d.is_empty()) {
                by_digest.entry(digest).or_default().push(index);
            }
            let key = progress_key(track);
            if !key.is_empty() {
                by_key.entry(key).or_default().push(index);
            }
        }
        let find = |entry: &ProgressEntry| -> EntryMatch {
            if !entry.digest.is_empty() {
                match by_digest.get(entry.digest.as_str()).map(Vec::as_slice) {
                    Some([only]) => return EntryMatch::Track(*only),
                    Some([_, ..]) => return EntryMatch::Ambiguous,
                    _ => {}
                }
            }
            if entry.key.is_empty() {
                return EntryMatch::Missing;
            }
            match by_key.get(entry.key.as_str()).map(Vec::as_slice) {
                Some([only]) => EntryMatch::Track(*only),
                Some([_, ..]) => EntryMatch::Ambiguous,
                _ => EntryMatch::Missing,
            }
        };
        // Resolve every entry first: the library does not change meanwhile.
        let targets: Vec<(usize, EntryMatch)> = file
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (index, find(entry)))
            .collect();
        let mut undo = ProgressUndo::default();
        let mut captured_progress: HashSet<String> = HashSet::new();
        let mut captured_ratings: HashSet<String> = HashSet::new();
        let mut marks: HashMap<String, HashSet<(u64, String)>> = HashMap::new();
        for bookmark in &self.bookmarks {
            marks.entry(bookmark.track_id.clone()).or_default().insert((
                bookmark.seconds.round().max(0.) as u64,
                bookmark.name.clone(),
            ));
        }
        let mut favorites: HashSet<String> = self.favorites.iter().cloned().collect();
        for (index, target) in targets {
            let entry = &file.entries[index];
            if !progress_entry_valid(entry) {
                outcome.invalid += 1;
                continue;
            }
            let track_index = match target {
                EntryMatch::Track(track_index) => track_index,
                EntryMatch::Ambiguous => {
                    outcome.ambiguous += 1;
                    continue;
                }
                EntryMatch::Missing => {
                    outcome.unmatched += 1;
                    continue;
                }
            };
            let track = &self.library[track_index];
            let id = track.id.clone();
            let mut changed = false;
            let incoming = Progress {
                seconds: entry.seconds,
                completed: entry.completed,
                last_played: entry.last_played,
                play_count: u64::from(entry.play_count),
                ..Progress::default()
            };
            if has_progress(&incoming) {
                let local = self.progress.get(&id);
                let local_has = local.is_some_and(has_progress);
                let newer =
                    !local_has || local.is_some_and(|p| incoming.last_played > p.last_played);
                if newer {
                    let before = local.cloned();
                    let mut seconds = incoming.seconds.min(MAX_POSITION_SECONDS);
                    if track.duration.is_finite() && track.duration > 0. {
                        seconds = seconds.min(track.duration);
                    }
                    let applied = Progress {
                        seconds,
                        completed: incoming.completed,
                        last_played: incoming.last_played,
                        play_count: incoming
                            .play_count
                            .max(before.as_ref().map_or(0, |p| p.play_count)),
                        file_size: track.file_size,
                        modified: track.modified,
                    };
                    if before.as_ref() != Some(&applied) {
                        if captured_progress.insert(id.clone()) {
                            undo.progress.push((id.clone(), before));
                        }
                        if self.current.as_deref() == Some(id.as_str()) {
                            outcome.seek = Some((id.clone(), applied.seconds));
                        }
                        self.progress.insert(id.clone(), applied);
                        changed = true;
                    }
                } else {
                    outcome.newer_here += 1;
                }
            }
            if (1..=5).contains(&entry.rating) && self.ratings.get(&id).copied().unwrap_or(0) == 0 {
                if captured_ratings.insert(id.clone()) {
                    undo.ratings
                        .push((id.clone(), self.ratings.get(&id).copied()));
                }
                self.ratings.insert(id.clone(), entry.rating);
                changed = true;
            }
            if entry.favorite && favorites.insert(id.clone()) {
                self.favorites.push(id.clone());
                undo.favorites_added.push(id.clone());
                changed = true;
            }
            for bookmark in &entry.bookmarks {
                let key = (bookmark.seconds.round() as u64, bookmark.name.clone());
                if marks.entry(id.clone()).or_default().insert(key) {
                    let added = Bookmark {
                        track_id: id.clone(),
                        seconds: bookmark.seconds.min(MAX_POSITION_SECONDS),
                        name: bookmark.name.clone(),
                        note: bookmark.note.clone(),
                    };
                    self.bookmarks.push(added.clone());
                    undo.bookmarks_added.push(added);
                    outcome.bookmarks_added += 1;
                    changed = true;
                }
            }
            if changed {
                outcome.files += 1;
            }
        }
        if !file.book_speeds.is_empty() {
            let keys = album_book_keys(&self.library);
            for speed in &file.book_speeds {
                let valid = speed.speed.is_finite()
                    && (0.25..=3.).contains(&speed.speed)
                    && speed.key.chars().count() <= MAX_PROGRESS_TEXT_CHARS
                    && keys.contains(speed.key.as_str());
                if valid && !self.book_speeds.contains_key(&speed.key) {
                    undo.speeds.push((speed.key.clone(), None));
                    self.book_speeds.insert(speed.key.clone(), speed.speed);
                    outcome.speeds_applied += 1;
                }
            }
        }
        let touched = !undo.progress.is_empty()
            || !undo.ratings.is_empty()
            || !undo.favorites_added.is_empty()
            || !undo.bookmarks_added.is_empty()
            || !undo.speeds.is_empty();
        outcome.undo = touched.then_some(undo);
        outcome
    }

    fn restore_progress_import(&mut self, undo: ProgressUndo) -> UndoOutcome {
        let ProgressUndo {
            progress,
            ratings,
            favorites_added,
            bookmarks_added,
            speeds,
        } = undo;
        let files = progress.len();
        let mut seek = None;
        for (id, previous) in progress {
            if self.current.as_deref() == Some(id.as_str()) {
                let seconds = previous.as_ref().map_or(0., |value| value.seconds);
                seek = Some((id.clone(), seconds));
            }
            match previous {
                Some(value) => {
                    self.progress.insert(id, value);
                }
                None => {
                    self.progress.remove(&id);
                }
            }
        }
        for (id, previous) in ratings {
            match previous {
                Some(value) => {
                    self.ratings.insert(id, value);
                }
                None => {
                    self.ratings.remove(&id);
                }
            }
        }
        if !favorites_added.is_empty() {
            let added: HashSet<&str> = favorites_added.iter().map(String::as_str).collect();
            self.favorites.retain(|id| !added.contains(id.as_str()));
        }
        if !bookmarks_added.is_empty() {
            // Each imported bookmark is taken out once, wherever it is now.
            let mut owed: HashMap<(String, u64, String, String), usize> = HashMap::new();
            for bookmark in bookmarks_added {
                *owed
                    .entry((
                        bookmark.track_id,
                        bookmark.seconds.to_bits(),
                        bookmark.name,
                        bookmark.note,
                    ))
                    .or_default() += 1;
            }
            self.bookmarks.retain(|bookmark| {
                let key = (
                    bookmark.track_id.clone(),
                    bookmark.seconds.to_bits(),
                    bookmark.name.clone(),
                    bookmark.note.clone(),
                );
                match owed.get_mut(&key) {
                    Some(count) if *count > 0 => {
                        *count -= 1;
                        false
                    }
                    _ => true,
                }
            });
        }
        for (key, previous) in speeds {
            match previous {
                Some(value) => {
                    self.book_speeds.insert(key, value);
                }
                None => {
                    self.book_speeds.remove(&key);
                }
            }
        }
        UndoOutcome {
            progress: true,
            view: true,
            seek,
            ..UndoOutcome::done(format!(
                "Took back the imported listening progress ({})",
                plural(files, "file", "files")
            ))
        }
    }
}
