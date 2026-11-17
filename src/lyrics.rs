//! Lyrics reader for audio files.
//!
//! Self-contained and panic-free: every read is bounded by fixed caps, every loop
//! is bounded by data that really exists, and nothing from a file or a tag is
//! trusted. Nothing here touches the UI, the player or the profile.
//!
//! Sources, in order:
//! 1. a `.lrc` file with the same stem beside the audio file (UTF-8 with or
//!    without a BOM, UTF-16 with a BOM, otherwise Windows-1252);
//! 2. lyrics embedded in the file's tags, read through Lofty inside
//!    `catch_unwind`: ID3v2 `SYLT` (synchronised, MPEG files only, millisecond
//!    timestamps), then the text of ID3v2 `USLT`, Vorbis/Opus `LYRICS` or
//!    `UNSYNCEDLYRICS`, MP4 `©lyr` and APE `Lyrics` (all `ItemKey::Lyrics`). When the
//!    embedded text itself holds `[mm:ss.xx]` stamps it is parsed as LRC.

use std::borrow::Cow;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};

use lofty::config::ParseOptions;
use lofty::file::{AudioFile, FileType, TaggedFile, TaggedFileExt};
use lofty::id3::v2::{
    FrameFlags, Id3v2Tag, SyncTextContentType, SynchronizedTextFrame, TimestampFormat,
};
use lofty::mpeg::MpegFile;
use lofty::probe::Probe;
use lofty::tag::{ItemKey, Tag};

/// Most lines ever returned for one file.
const MAX_LINES: usize = 5_000;
/// Longest line in characters.
const MAX_LINE_CHARS: usize = 1_000;
/// Largest `.lrc` file that is read, and the most embedded lyrics text kept.
const MAX_TEXT_BYTES: usize = 512 * 1024;
/// Latest time that is accepted, in seconds (100 hours).
const MAX_SECONDS: f64 = 100.0 * 3600.0;
/// Largest `[offset:]` magnitude that is accepted, in milliseconds (100 hours).
const MAX_OFFSET_MS: u64 = 360_000_000;
/// Lines of a text that are looked at.
const MAX_SCANNED_LINES: usize = 100_000;
/// Lines (or time stamps) kept before sorting and truncating.
const MAX_RAW_ENTRIES: usize = 4 * MAX_LINES;
/// Time stamps taken from one line.
const MAX_STAMPS_PER_LINE: usize = 64;
/// Characters searched for the `]` that closes a bracket tag.
const MAX_BRACKET_CHARS: usize = 200;
/// Characters searched for the `>` that closes an enhanced word tag.
const MAX_ENHANCED_CHARS: usize = 14;
/// Directory entries scanned when looking for a `.lrc` file.
const MAX_DIR_ENTRIES: usize = 50_000;
/// Tag items examined per tag.
const MAX_TAG_ITEMS: usize = 20_000;
/// Embedded lyrics texts kept when a file carries several.
const MAX_EMBEDDED_TEXTS: usize = 8;

const SOURCE_FILE: &str = "lyrics file beside the track";
const SOURCE_EMBEDDED: &str = "embedded lyrics";

/// Tag names (before the `:`) of LRC metadata lines, compared case-insensitively.
const META_KEYS: [&str; 13] = [
    "ar", "ti", "al", "au", "by", "length", "re", "ve", "offset", "la", "lr", "id", "tool",
];

/// One line of lyrics.
#[derive(Debug, Clone, PartialEq)]
pub struct LyricLine {
    /// Seconds from the start of the file; `None` for unsynchronised lyrics.
    pub time: Option<f64>,
    /// The text, trimmed and without control characters; empty for a gap.
    pub text: String,
}

/// The lyrics of one audio file.
#[derive(Debug, Clone, PartialEq)]
pub struct Lyrics {
    /// Synchronised lyrics: at least two lines, every line has a time and the
    /// lines are sorted by time. Otherwise every line has `time: None`.
    pub lines: Vec<LyricLine>,
    pub synced: bool,
    /// Where the lyrics came from: "lyrics file beside the track" or "embedded
    /// lyrics". Empty when the lyrics come straight from `parse_lrc`.
    pub source: String,
}

/// The lyrics of an audio file, or `Ok(None)` when it has none. `Err` only means
/// the file itself could not be opened. Reads from disk: call it off the UI thread.
///
/// Order of sources: a same-stem `.lrc` file beside the audio file, then lyrics
/// embedded in the file's tags. A source that yields no text at all is skipped.
pub fn read_lyrics(path: &Path) -> Result<Option<Lyrics>, String> {
    let file =
        File::open(path).map_err(|error| format!("Could not open {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("Could not open {}: not a file", path.display()));
    }
    drop(file);
    if let Some(lyrics) = read_sidecar(path) {
        return Ok(Some(lyrics));
    }
    Ok(embedded_lyrics(read_embedded_raw(path)))
}

/// Parses LRC text, or plain lyrics text. Pure; never panics.
///
/// Synchronised (`synced`) when at least two time-stamped lines remain after
/// validation: only the stamped lines are kept, sorted by time (stable), with
/// `[offset:]` applied. Otherwise the text is plain: every line in its original
/// order with `time: None`, stamps and metadata removed, runs of blank lines
/// collapsed and leading and trailing blank lines dropped. `source` is left empty.
pub fn parse_lrc(text: &str) -> Lyrics {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let text: Cow<str> = if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    };
    let mut offset_ms: Option<f64> = None;
    let mut entries: Vec<(Vec<f64>, String)> = Vec::new();
    for line in text.split('\n').take(MAX_SCANNED_LINES) {
        match parse_line(line) {
            Parsed::Skip => {}
            Parsed::Offset(ms) => {
                offset_ms.get_or_insert(ms);
            }
            Parsed::Line { times, text } => {
                if entries.len() < MAX_RAW_ENTRIES {
                    entries.push((times, text));
                }
            }
        }
    }

    let shift = offset_ms.unwrap_or(0.0) / 1000.0;
    let mut timed: Vec<LyricLine> = Vec::new();
    'collect: for (times, text) in &entries {
        for &time in times {
            if timed.len() >= MAX_RAW_ENTRIES {
                break 'collect;
            }
            timed.push(LyricLine {
                time: Some((time - shift).max(0.0)),
                text: text.clone(),
            });
        }
    }
    let timed = finish_timed(timed);
    if timed.len() >= 2 {
        return Lyrics {
            lines: timed,
            synced: true,
            source: String::new(),
        };
    }
    Lyrics {
        lines: finish_plain(entries.into_iter().map(|(_, text)| text)),
        synced: false,
        source: String::new(),
    }
}

/// Index of the line to highlight at `position` seconds: the last line whose time
/// is at or before `position`. `None` before the first line, for a position that is
/// not finite, and for lyrics that are not synchronised. A blank line (a gap in the
/// singing) can be returned; the caller decides how to show it.
pub fn current_line(lyrics: &Lyrics, position: f64) -> Option<usize> {
    if !lyrics.synced || !position.is_finite() {
        return None;
    }
    let after = lyrics
        .lines
        .partition_point(|line| line.time.is_some_and(|time| time <= position));
    after.checked_sub(1)
}

// ---------------------------------------------------------------------------
// LRC parsing
// ---------------------------------------------------------------------------

enum Parsed {
    /// Metadata or another line that carries nothing to show.
    Skip,
    /// `[offset:+/-ms]`
    Offset(f64),
    /// Zero or more time stamps and the text after them.
    Line { times: Vec<f64>, text: String },
}

fn parse_line(line: &str) -> Parsed {
    let mut rest = line.trim_start();
    let mut times: Vec<f64> = Vec::new();
    let mut stamped = false;
    while let Some(after_open) = rest.strip_prefix('[') {
        let Some((inner, tail)) = split_bracket(after_open) else {
            break;
        };
        if let Some(seconds) = parse_time(inner) {
            stamped = true;
            if seconds <= MAX_SECONDS && times.len() < MAX_STAMPS_PER_LINE {
                times.push(seconds);
            }
            rest = tail.trim_start();
            continue;
        }
        if !stamped && let Some(parsed) = parse_meta(inner) {
            return parsed;
        }
        break;
    }
    let limited = take_chars(rest, 8 * MAX_LINE_CHARS);
    Parsed::Line {
        times,
        text: clean_text(&strip_enhanced(limited)),
    }
}

/// `inner` and the text after the `]`, for text that follows a `[`.
fn split_bracket(after_open: &str) -> Option<(&str, &str)> {
    let (index, _) = after_open
        .char_indices()
        .take(MAX_BRACKET_CHARS)
        .find(|&(_, c)| c == ']')?;
    Some((after_open.get(..index)?, after_open.get(index + 1..)?))
}

/// `Some` when `inner` (the text between the brackets) is an LRC metadata tag.
fn parse_meta(inner: &str) -> Option<Parsed> {
    let (key, value) = inner.split_once(':')?;
    let key = key.trim();
    if key.len() > 8 || !META_KEYS.iter().any(|meta| key.eq_ignore_ascii_case(meta)) {
        return None;
    }
    if key.eq_ignore_ascii_case("offset") {
        let offset = value
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|ms| ms.unsigned_abs() <= MAX_OFFSET_MS);
        return Some(offset.map_or(Parsed::Skip, |ms| Parsed::Offset(ms as f64)));
    }
    Some(Parsed::Skip)
}

fn digits(part: &str, length: RangeInclusive<usize>) -> Option<u32> {
    if !length.contains(&part.len()) || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

/// `mm:ss`, `m:ss`, `mm:ss.x` to `mm:ss.xxx` or `h:mm:ss[.xxx]` (up to three digits
/// of hours) as seconds. The value is not range checked.
fn parse_time(inner: &str) -> Option<f64> {
    let (clock, fraction) = match inner.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (inner, None),
    };
    let mut parts = clock.split(':');
    let first = parts.next()?;
    let second = parts.next()?;
    let third = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let (hours, minutes, seconds) = match third {
        Some(third) => (
            digits(first, 1..=3)?,
            digits(second, 2..=2)?,
            digits(third, 2..=2)?,
        ),
        None => (0, digits(first, 1..=4)?, digits(second, 2..=2)?),
    };
    let fraction = match fraction {
        Some(fraction) => {
            let value = digits(fraction, 1..=3)?;
            f64::from(value) / 10f64.powi(i32::try_from(fraction.len()).ok()?)
        }
        None => 0.0,
    };
    Some(f64::from(hours) * 3600.0 + f64::from(minutes) * 60.0 + f64::from(seconds) + fraction)
}

fn take_chars(text: &str, count: usize) -> &str {
    match text.char_indices().nth(count) {
        Some((index, _)) => text.get(..index).unwrap_or(text),
        None => text,
    }
}

/// Removes enhanced-LRC word tags such as `<00:12.50>`.
fn strip_enhanced(text: &str) -> Cow<'_, str> {
    if !text.contains('<') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((before, after)) = rest.split_once('<') {
        out.push_str(before);
        let close = after
            .char_indices()
            .take(MAX_ENHANCED_CHARS)
            .find(|&(_, c)| c == '>')
            .map(|(index, _)| index);
        let tag = close.and_then(|index| Some((after.get(..index)?, after.get(index + 1..)?)));
        match tag {
            Some((inner, tail)) if is_enhanced_tag(inner) => rest = tail,
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn is_enhanced_tag(inner: &str) -> bool {
    inner.contains(':')
        && inner
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b':' || b == b'.')
}

/// Whitespace trimmed, control characters removed (tabs and line ends become
/// spaces), at most `MAX_LINE_CHARS` characters.
fn clean_text(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .chars()
        .map(|c| {
            if matches!(c, '\t' | '\n' | '\r') {
                ' '
            } else {
                c
            }
        })
        .filter(|c| !c.is_control())
        .take(MAX_LINE_CHARS)
        .collect();
    cleaned.trim().to_owned()
}

/// Valid times only, sorted by time (stable, so equal times keep source order).
fn finish_timed(mut lines: Vec<LyricLine>) -> Vec<LyricLine> {
    lines.retain(|line| {
        line.time
            .is_some_and(|time| time.is_finite() && (0.0..=MAX_SECONDS).contains(&time))
    });
    lines.sort_by(|a, b| a.time.unwrap_or(0.0).total_cmp(&b.time.unwrap_or(0.0)));
    lines.truncate(MAX_LINES);
    lines
}

/// Plain lines: no leading blank lines, runs of blank lines collapsed to one, no
/// trailing blank line.
fn finish_plain(texts: impl Iterator<Item = String>) -> Vec<LyricLine> {
    let mut lines: Vec<LyricLine> = Vec::new();
    for text in texts {
        if text.is_empty() && lines.last().is_none_or(|last| last.text.is_empty()) {
            continue;
        }
        lines.push(LyricLine { time: None, text });
        if lines.len() > MAX_LINES {
            break;
        }
    }
    while lines.last().is_some_and(|last| last.text.is_empty()) {
        lines.pop();
    }
    lines.truncate(MAX_LINES);
    lines
}

fn has_content(lyrics: &Lyrics) -> bool {
    lyrics.lines.iter().any(|line| !line.text.is_empty())
}

// ---------------------------------------------------------------------------
// Text decoding
// ---------------------------------------------------------------------------

fn cp1252_char(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '\u{20AC}', '\u{81}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{8D}',
        '\u{017D}', '\u{8F}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}',
        '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}',
        '\u{9D}', '\u{017E}', '\u{0178}',
    ];
    match byte {
        0x80..=0x9F => HIGH
            .get(usize::from(byte - 0x80))
            .copied()
            .unwrap_or(char::REPLACEMENT_CHARACTER),
        other => char::from(other),
    }
}

fn decode_utf16(bytes: &[u8], big_endian: bool) -> String {
    let units = bytes
        .chunks_exact(2)
        .filter_map(|pair| <[u8; 2]>::try_from(pair).ok())
        .map(|pair| {
            if big_endian {
                u16::from_be_bytes(pair)
            } else {
                u16::from_le_bytes(pair)
            }
        });
    char::decode_utf16(units)
        .map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect()
}

/// UTF-16 with a BOM, UTF-8 (BOM optional) when valid, otherwise Windows-1252
/// (every byte maps to one character, so nothing is lost).
fn decode_text_bytes(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, false);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, true);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|&b| cp1252_char(b)).collect(),
    }
}

// ---------------------------------------------------------------------------
// Sidecar `.lrc` file
// ---------------------------------------------------------------------------

fn has_lrc_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lrc"))
}

/// A `.lrc` file with the same stem beside the audio file (stem compared
/// case-insensitively).
fn find_lrc(path: &Path) -> Option<PathBuf> {
    for extension in ["lrc", "LRC", "Lrc"] {
        let candidate = path.with_extension(extension);
        if candidate != path && candidate.is_file() {
            return Some(candidate);
        }
    }
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = path.file_stem()?.to_string_lossy().to_lowercase();
    std::fs::read_dir(directory)
        .ok()?
        .take(MAX_DIR_ENTRIES)
        .flatten()
        .map(|entry| entry.path())
        .find(|candidate| {
            has_lrc_extension(candidate)
                && candidate
                    .file_stem()
                    .is_some_and(|name| name.to_string_lossy().to_lowercase() == stem)
                && candidate.is_file()
        })
}

/// Lyrics from the `.lrc` file beside `path`, when there is one that is small
/// enough and holds text.
fn read_sidecar(path: &Path) -> Option<Lyrics> {
    let lrc = find_lrc(path)?;
    let mut file = File::open(lrc).ok()?;
    let metadata = file.metadata().ok()?;
    let limit = MAX_TEXT_BYTES as u64;
    if !metadata.is_file() || metadata.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    (&mut file).take(limit + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > limit {
        return None;
    }
    let mut lyrics = parse_lrc(&decode_text_bytes(&bytes));
    if !has_content(&lyrics) {
        return None;
    }
    lyrics.source = SOURCE_FILE.to_owned();
    Some(lyrics)
}

// ---------------------------------------------------------------------------
// Embedded lyrics
// ---------------------------------------------------------------------------

/// What was found in the tags of one file, before it is parsed.
struct EmbeddedRaw {
    /// ID3v2 `SYLT` lines (already valid, sorted and cleaned); empty when none.
    synced: Vec<LyricLine>,
    /// Lyrics texts of the tags, primary tag first, each at most `MAX_TEXT_BYTES`.
    texts: Vec<String>,
}

/// Reads the tags of `path`. A parser that panics on a hostile file (Lofty can)
/// ends this read, not the caller.
fn read_embedded_raw(path: &Path) -> Option<EmbeddedRaw> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| collect_embedded(path)))
        .ok()
        .flatten()
}

fn collect_embedded(path: &Path) -> Option<EmbeddedRaw> {
    crate::media::validate_lofty_input(path).ok()?;
    let options = ParseOptions::new()
        .read_properties(false)
        .read_cover_art(false);
    let probe = Probe::open(path).ok()?.guess_file_type().ok()?;
    if probe.file_type() == Some(FileType::Mpeg) {
        // MPEG files are read as such so the ID3v2 tag stays available for SYLT
        // before it is converted to the generic tag.
        let mut reader = probe.into_inner();
        reader.seek(SeekFrom::Start(0)).ok()?;
        let file = MpegFile::read_from(&mut reader, options).ok()?;
        let synced = file.id3v2().map(sylt_lines).unwrap_or_default();
        let tagged: TaggedFile = file.into();
        Some(EmbeddedRaw {
            synced,
            texts: lyrics_texts(&tagged),
        })
    } else {
        let tagged = probe.options(options).read().ok()?;
        Some(EmbeddedRaw {
            synced: Vec::new(),
            texts: lyrics_texts(&tagged),
        })
    }
}

/// Lines of the first usable ID3v2 `SYLT` lyrics frame. Only millisecond
/// timestamps are supported (MPEG-frame timestamps would need the frame rate).
fn sylt_lines(tag: &Id3v2Tag) -> Vec<LyricLine> {
    for frame in tag {
        if frame.id_str() != "SYLT" {
            continue;
        }
        let lofty::id3::v2::Frame::Binary(binary) = frame else {
            continue;
        };
        if binary.data.len() > MAX_TEXT_BYTES {
            continue;
        }
        let Ok(sylt) = SynchronizedTextFrame::parse(&binary.data, FrameFlags::default()) else {
            continue;
        };
        if sylt.timestamp_format != TimestampFormat::MS
            || sylt.content_type != SyncTextContentType::Lyrics
        {
            continue;
        }
        let lines = finish_timed(
            sylt.content
                .iter()
                .take(MAX_RAW_ENTRIES)
                .map(|(milliseconds, text)| LyricLine {
                    time: Some(f64::from(*milliseconds) / 1000.0),
                    text: clean_text(text),
                })
                .collect(),
        );
        if lines.len() >= 2 {
            return lines;
        }
    }
    Vec::new()
}

fn is_lyrics_key(key: &ItemKey) -> bool {
    match key {
        ItemKey::Lyrics => true,
        ItemKey::Unknown(name) => {
            name.eq_ignore_ascii_case("UNSYNCEDLYRICS") || name.eq_ignore_ascii_case("LYRICS")
        }
        _ => false,
    }
}

/// `text` cut to at most `MAX_TEXT_BYTES` at a character boundary.
fn cap_bytes(text: &str) -> &str {
    let mut end = text.len().min(MAX_TEXT_BYTES);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or("")
}

/// Non-blank lyrics texts of all tags of a file, the primary tag first.
fn lyrics_texts(tagged: &TaggedFile) -> Vec<String> {
    let primary = tagged.primary_tag();
    let mut tags: Vec<&Tag> = primary.into_iter().collect();
    tags.extend(
        tagged
            .tags()
            .iter()
            .filter(|tag| primary.is_none_or(|first| !std::ptr::eq(first, *tag))),
    );
    let mut texts = Vec::new();
    for tag in tags {
        for item in tag.items().take(MAX_TAG_ITEMS) {
            if texts.len() >= MAX_EMBEDDED_TEXTS {
                return texts;
            }
            if !is_lyrics_key(item.key()) {
                continue;
            }
            if let Some(text) = item.value().text()
                && !text.trim().is_empty()
            {
                texts.push(cap_bytes(text).to_owned());
            }
        }
    }
    texts
}

/// Picks the best lyrics out of what the tags held: `SYLT`, then the first text
/// that parses as synchronised LRC, then the first text that has any content.
fn embedded_lyrics(raw: Option<EmbeddedRaw>) -> Option<Lyrics> {
    let raw = raw?;
    if raw.synced.len() >= 2 && raw.synced.iter().any(|line| !line.text.is_empty()) {
        return Some(Lyrics {
            lines: raw.synced,
            synced: true,
            source: SOURCE_EMBEDDED.to_owned(),
        });
    }
    let mut plain: Option<Lyrics> = None;
    for text in &raw.texts {
        let lyrics = parse_lrc(text);
        if !has_content(&lyrics) {
            continue;
        }
        if lyrics.synced {
            return Some(Lyrics {
                source: SOURCE_EMBEDDED.to_owned(),
                ..lyrics
            });
        }
        plain.get_or_insert(lyrics);
    }
    plain.map(|lyrics| Lyrics {
        source: SOURCE_EMBEDDED.to_owned(),
        ..lyrics
    })
}
