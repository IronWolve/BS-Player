//! Chapter reader for audio files.
//!
//! Self-contained and panic-free: every read is bounded by the file length and by
//! fixed caps, every offset is computed with checked arithmetic, and no untrusted
//! number is used to size an allocation before it has been checked against data
//! that really exists. Nothing here touches the UI, the player or the profile.
//!
//! Sources, in order: embedded chapters of the container or tag (MP4/M4B/M4A,
//! ID3v2 `CHAP` frames, FLAC and Ogg Vorbis `CHAPTERnnn` comments), then a CUE
//! sheet with the same file stem beside the audio file.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use lofty::config::ParseOptions;
use lofty::file::AudioFile;
use lofty::ogg::VorbisFile;

/// Most chapters ever returned for one file.
const MAX_CHAPTERS: usize = 10_000;
/// Longest chapter title in characters.
const MAX_TITLE_CHARS: usize = 256;
/// Largest `moov` atom that is read into memory.
const MAX_MOOV_BYTES: u64 = 16 * 1024 * 1024;
/// Largest ID3v2 tag that is read into memory.
const MAX_ID3_BYTES: u64 = 16 * 1024 * 1024;
/// Largest FLAC Vorbis-comment block that is read into memory.
const MAX_VORBIS_BLOCK_BYTES: u64 = 16 * 1024 * 1024;
/// Largest CUE sheet that is read.
const MAX_CUE_BYTES: u64 = 256 * 1024;
/// Top-level MP4 atoms visited while looking for `moov`.
const MAX_TOP_LEVEL_ATOMS: usize = 10_000;
/// Atoms visited at one nesting level inside `moov`.
const MAX_ATOMS_PER_LEVEL: usize = 200_000;
/// Deepest atom path followed inside `moov`.
const MAX_ATOM_DEPTH: usize = 8;
/// Tracks examined when looking for a chapter text track.
const MAX_TRACKS: usize = 256;
/// Sample-to-chunk entries read from one `stsc` atom.
const MAX_STSC_ENTRIES: usize = 100_000;
/// FLAC metadata blocks visited.
const MAX_FLAC_BLOCKS: usize = 1024;
/// Directory entries scanned when looking for a CUE sheet.
const MAX_DIR_ENTRIES: usize = 50_000;
/// Largest chapter text sample read from an MP4 text track.
const MAX_SAMPLE_BYTES: u64 = 70_000;

/// One chapter of an audio file.
#[derive(Debug, Clone, PartialEq)]
pub struct Chapter {
    /// Start in seconds from the start of the file.
    pub start: f64,
    /// Start of the next chapter, or the file duration for the last one.
    pub end: Option<f64>,
    pub title: String,
}

/// All chapters of an audio file, sorted by start, deduplicated, never panics.
/// An empty list means the file has none. `Err` only means the file could not be
/// opened or measured. `duration` is the file length in seconds (0 when unknown).
///
/// Order of sources: embedded chapters of the container or tag, then a same-stem
/// `.cue` file beside the audio file (used only when it lists at least two tracks).
pub fn read_chapters(path: &Path, duration: f64) -> Result<Vec<Chapter>, String> {
    let mut file =
        File::open(path).map_err(|error| format!("Could not open {}: {error}", path.display()))?;
    let len = file
        .metadata()
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?
        .len();
    let hint_mp4 = has_extension(path, &["m4b", "m4a", "mp4", "m4p"]);
    if let Err(error) = crate::media::validate_lofty_input(path) {
        let from_cue = finalize(read_cue_beside(path), duration);
        return if from_cue.len() >= 2 { Ok(from_cue) } else { Err(error) };
    }
    let mut chapters = finalize(read_embedded(&mut file, len, hint_mp4), duration);
    if chapters.is_empty() {
        let from_cue = finalize(read_cue_beside(path), duration);
        if from_cue.len() >= 2 {
            chapters = from_cue;
        }
    }
    Ok(chapters)
}

/// Index of the chapter that contains `position` seconds, if any. Positions at or
/// past the start of the last chapter belong to the last chapter.
pub fn chapter_at(chapters: &[Chapter], position: f64) -> Option<usize> {
    if !position.is_finite() || chapters.is_empty() {
        return None;
    }
    let after = chapters.partition_point(|chapter| chapter.start <= position);
    let index = after.checked_sub(1)?;
    let chapter = chapters.get(index)?;
    if index + 1 < chapters.len()
        && let Some(end) = chapter.end
        && position >= end
    {
        return None;
    }
    Some(index)
}

/// Chapters of a CUE sheet: `INDEX 01` of every audio `TRACK`, with its `TITLE`.
/// Pure text parsing. It does not look at `FILE` lines, so a sheet that spans
/// several audio files gives meaningless times; `read_chapters` checks that.
#[allow(dead_code)] // the app reads CUE sheets through `read_chapters`; this is for tests and tools
pub fn parse_cue(text: &str) -> Vec<Chapter> {
    finalize(parse_cue_tracks(text, None), 0.0)
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn has_extension(path: &Path, wanted: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| wanted.iter().any(|w| ext.eq_ignore_ascii_case(w)))
}

fn clean_title(raw: &str) -> String {
    let no_control: String = raw.chars().filter(|c| !c.is_control()).collect();
    let limited: String = no_control.trim().chars().take(MAX_TITLE_CHARS).collect();
    limited.trim_end().to_owned()
}

/// Sort, validate, deduplicate, name and close the chapters of one source.
fn finalize(mut raw: Vec<Chapter>, duration: f64) -> Vec<Chapter> {
    let duration = if duration.is_finite() && duration > 0.0 {
        duration
    } else {
        0.0
    };
    raw.retain(|chapter| {
        chapter.start.is_finite()
            && chapter.start >= 0.0
            && (duration <= 0.0 || chapter.start <= duration + 1.0)
    });
    // Stable, so equal starts keep source order and the first one survives.
    raw.sort_by(|a, b| a.start.total_cmp(&b.start));
    raw.dedup_by(|current, previous| current.start - previous.start < 1e-6);
    raw.truncate(MAX_CHAPTERS);
    let starts: Vec<f64> = raw.iter().map(|chapter| chapter.start).collect();
    for (index, chapter) in raw.iter_mut().enumerate() {
        chapter.title = clean_title(&chapter.title);
        if chapter.title.is_empty() {
            chapter.title = format!("Chapter {}", index + 1);
        }
        chapter.end = match starts.get(index + 1) {
            Some(next) => Some(*next),
            None if duration > chapter.start => Some(duration),
            None => None,
        };
    }
    raw
}

fn raw_chapter(start: f64, title: String) -> Chapter {
    Chapter {
        start,
        end: None,
        title,
    }
}

/// Big-endian reader over a slice. Every method returns `None` instead of
/// reading past the end.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(count)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        self.take(count).map(|_| ())
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }

    fn u32_le(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn rest(&self) -> &'a [u8] {
        self.data.get(self.pos..).unwrap_or(&[])
    }
}

/// Read exactly `buf.len()` bytes at `pos`, never seeking or reading past `len`.
fn read_exact_at<R: Read + Seek>(r: &mut R, len: u64, pos: u64, buf: &mut [u8]) -> bool {
    let Some(end) = pos.checked_add(buf.len() as u64) else {
        return false;
    };
    if end > len {
        return false;
    }
    r.seek(SeekFrom::Start(pos)).is_ok() && r.read_exact(buf).is_ok()
}

/// Read `count` bytes at `pos` into a new vector, refusing more than `max` bytes
/// or anything that would pass `len`.
fn read_vec_at<R: Read + Seek>(
    r: &mut R,
    len: u64,
    pos: u64,
    count: u64,
    max: u64,
) -> Option<Vec<u8>> {
    if count > max || pos.checked_add(count)? > len {
        return None;
    }
    let wanted = usize::try_from(count).ok()?;
    r.seek(SeekFrom::Start(pos)).ok()?;
    let mut out = Vec::new();
    out.try_reserve_exact(wanted).ok()?;
    (&mut *r).take(count).read_to_end(&mut out).ok()?;
    (out.len() == wanted).then_some(out)
}

/// UTF-16 to a string without ever failing; bad units become U+FFFD.
fn utf16_lossy(bytes: &[u8], big_endian: bool) -> String {
    let units = bytes.chunks_exact(2).map(|pair| {
        let pair = [
            pair.first().copied().unwrap_or(0),
            pair.get(1).copied().unwrap_or(0),
        ];
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

// ---------------------------------------------------------------------------
// Format detection
// ---------------------------------------------------------------------------

fn read_embedded<R: Read + Seek>(r: &mut R, len: u64, hint_mp4: bool) -> Vec<Chapter> {
    let mut head = [0u8; 16];
    let head_len = head.len().min(usize::try_from(len).unwrap_or(usize::MAX));
    let Some(head) = head.get_mut(..head_len) else {
        return Vec::new();
    };
    if !read_exact_at(r, len, 0, head) {
        return Vec::new();
    }
    let head: &[u8] = head;
    if head.starts_with(b"ID3") {
        let (chapters, tag_end) = read_id3(r, len);
        if !chapters.is_empty() {
            return chapters;
        }
        // A FLAC file may carry an ID3v2 tag in front of its stream.
        if let Some(end) = tag_end {
            let mut magic = [0u8; 4];
            if read_exact_at(r, len, end, &mut magic) && &magic == b"fLaC" {
                return read_flac(r, len, end);
            }
        }
        return Vec::new();
    }
    if head.starts_with(b"fLaC") {
        return read_flac(r, len, 0);
    }
    if head.starts_with(b"OggS") {
        return read_ogg(r);
    }
    const MP4_FIRST_ATOMS: [&[u8; 4]; 7] = [
        b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"pnot",
    ];
    let mp4_magic = head
        .get(4..8)
        .is_some_and(|kind| MP4_FIRST_ATOMS.iter().any(|known| known.as_slice() == kind));
    if mp4_magic || hint_mp4 {
        return read_mp4(r, len);
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// MP4 / M4B / M4A
// ---------------------------------------------------------------------------

struct Atom<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

/// Iterates the atoms of one nesting level of an in-memory atom tree. Stops at
/// the first atom whose header is short, whose size is smaller than its header or
/// whose size runs past the parent.
struct AtomIter<'a> {
    data: &'a [u8],
    pos: usize,
    count: usize,
}

impl<'a> AtomIter<'a> {
    fn step(&mut self) -> Option<Atom<'a>> {
        if self.count >= MAX_ATOMS_PER_LEVEL {
            return None;
        }
        self.count += 1;
        let mut reader = Reader {
            data: self.data,
            pos: self.pos,
        };
        let size32 = reader.u32()?;
        let kind: [u8; 4] = reader.take(4)?.try_into().ok()?;
        let mut header = 8usize;
        let size = match size32 {
            0 => self.data.len().saturating_sub(self.pos),
            1 => {
                header = 16;
                usize::try_from(reader.u64()?).ok()?
            }
            other => usize::try_from(other).ok()?,
        };
        if size < header {
            return None;
        }
        let end = self.pos.checked_add(size)?;
        let body = self.data.get(self.pos.checked_add(header)?..end)?;
        self.pos = end;
        Some(Atom { kind, body })
    }
}

impl<'a> Iterator for AtomIter<'a> {
    type Item = Atom<'a>;

    fn next(&mut self) -> Option<Atom<'a>> {
        let atom = self.step();
        if atom.is_none() {
            self.pos = self.data.len();
        }
        atom
    }
}

fn atoms(data: &[u8]) -> AtomIter<'_> {
    AtomIter {
        data,
        pos: 0,
        count: 0,
    }
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    atoms(data)
        .find(|atom| &atom.kind == kind)
        .map(|atom| atom.body)
}

fn children<'a>(data: &'a [u8], kind: &'a [u8; 4]) -> impl Iterator<Item = &'a [u8]> + 'a {
    atoms(data)
        .filter(move |atom| &atom.kind == kind)
        .map(|atom| atom.body)
}

fn atom_path<'a>(data: &'a [u8], kinds: &[&[u8; 4]]) -> Option<&'a [u8]> {
    if kinds.len() > MAX_ATOM_DEPTH {
        return None;
    }
    let mut current = data;
    for kind in kinds {
        current = child(current, kind)?;
    }
    Some(current)
}

/// Offset and size of the body of the first top-level `moov` atom.
fn find_moov<R: Read + Seek>(r: &mut R, len: u64) -> Option<(u64, u64)> {
    let mut pos = 0u64;
    for _ in 0..MAX_TOP_LEVEL_ATOMS {
        let mut header = [0u8; 8];
        if !read_exact_at(r, len, pos, &mut header) {
            return None;
        }
        let [s0, s1, s2, s3, k0, k1, k2, k3] = header;
        let size32 = u32::from_be_bytes([s0, s1, s2, s3]);
        let kind = [k0, k1, k2, k3];
        let mut header_len = 8u64;
        let size = match size32 {
            0 => len.saturating_sub(pos),
            1 => {
                let mut wide = [0u8; 8];
                if !read_exact_at(r, len, pos.saturating_add(8), &mut wide) {
                    return None;
                }
                header_len = 16;
                u64::from_be_bytes(wide)
            }
            other => u64::from(other),
        };
        if size < header_len {
            return None;
        }
        let end = pos.checked_add(size)?;
        if end > len {
            return None;
        }
        if &kind == b"moov" {
            return Some((pos + header_len, size - header_len));
        }
        pos = end;
    }
    None
}

fn read_mp4<R: Read + Seek>(r: &mut R, len: u64) -> Vec<Chapter> {
    let Some((start, size)) = find_moov(r, len) else {
        return Vec::new();
    };
    let Some(moov) = read_vec_at(r, len, start, size, MAX_MOOV_BYTES) else {
        return Vec::new();
    };
    let nero = atom_path(&moov, &[b"udta", b"chpl"])
        .map(parse_chpl)
        .unwrap_or_default();
    let quicktime = quicktime_chapters(&moov, r, len);
    match (nero.is_empty(), quicktime.is_empty()) {
        (false, false) if nero.len() != quicktime.len() => nero,
        (_, false) => quicktime,
        _ => nero,
    }
}

/// Nero `chpl` atom: version, 3 flag bytes, (4 reserved bytes), u8 count, then per
/// entry a u64 start in 100 ns units and a u8-length UTF-8 title. Writers differ on
/// whether the reserved bytes exist for version 0, so both layouts are tried.
fn parse_chpl(body: &[u8]) -> Vec<Chapter> {
    let mut reader = Reader::new(body);
    let Some(version) = reader.u8() else {
        return Vec::new();
    };
    if reader.skip(3).is_none() {
        return Vec::new();
    }
    let rest = reader.rest();
    let layouts: [usize; 2] = if version == 0 { [0, 4] } else { [4, 0] };
    for reserved in layouts {
        if let Some(chapters) = parse_chpl_entries(rest, reserved)
            && !chapters.is_empty()
        {
            return chapters;
        }
    }
    Vec::new()
}

fn parse_chpl_entries(rest: &[u8], reserved: usize) -> Option<Vec<Chapter>> {
    let mut reader = Reader::new(rest);
    reader.skip(reserved)?;
    let count = reader.u8()?;
    let mut out = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let start = reader.u64()?;
        let title_len = usize::from(reader.u8()?);
        let title = reader.take(title_len)?;
        out.push(raw_chapter(
            start as f64 / 10_000_000.0,
            String::from_utf8_lossy(title).into_owned(),
        ));
    }
    Some(out)
}

fn track_id(trak: &[u8]) -> Option<u32> {
    let mut reader = Reader::new(child(trak, b"tkhd")?);
    let version = reader.u8()?;
    reader.skip(3)?;
    reader.skip(if version == 1 { 16 } else { 8 })?;
    reader.u32()
}

/// QuickTime chapter track: a track holding a `tref/chap` reference to a text
/// track whose samples are the chapter titles.
fn quicktime_chapters<R: Read + Seek>(moov: &[u8], r: &mut R, len: u64) -> Vec<Chapter> {
    for trak in children(moov, b"trak").take(MAX_TRACKS) {
        let Some(tref) = child(trak, b"tref") else {
            continue;
        };
        for chap in children(tref, b"chap").take(MAX_TRACKS) {
            let mut ids = Reader::new(chap);
            for _ in 0..8 {
                let Some(id) = ids.u32() else {
                    break;
                };
                let target = children(moov, b"trak")
                    .take(MAX_TRACKS)
                    .find(|candidate| track_id(candidate) == Some(id));
                if let Some(target) = target
                    && let Some(chapters) = text_track_chapters(target, r, len)
                    && !chapters.is_empty()
                {
                    return chapters;
                }
            }
        }
    }
    Vec::new()
}

fn text_track_chapters<R: Read + Seek>(trak: &[u8], r: &mut R, len: u64) -> Option<Vec<Chapter>> {
    let mdia = child(trak, b"mdia")?;
    let mut mdhd = Reader::new(child(mdia, b"mdhd")?);
    let version = mdhd.u8()?;
    mdhd.skip(3)?;
    mdhd.skip(if version == 1 { 16 } else { 8 })?;
    let timescale = mdhd.u32()?;
    if timescale == 0 {
        return None;
    }
    if let Some(hdlr) = child(mdia, b"hdlr") {
        let mut handler = Reader::new(hdlr);
        handler.skip(8)?;
        let kind = handler.take(4)?;
        let textual: [&[u8; 4]; 4] = [b"text", b"sbtl", b"subt", b"tx3g"];
        if !textual.iter().any(|known| known.as_slice() == kind) {
            return None;
        }
    }
    let stbl = atom_path(mdia, &[b"minf", b"stbl"])?;
    let times = parse_stts(child(stbl, b"stts")?, MAX_CHAPTERS);
    let sizes = parse_stsz(child(stbl, b"stsz")?, times.len());
    let stsc = parse_stsc(child(stbl, b"stsc")?)?;
    let located = if let Some(stco) = child(stbl, b"stco") {
        locate_samples(&stsc, stco, false, &sizes)
    } else {
        locate_samples(&stsc, child(stbl, b"co64")?, true, &sizes)
    };
    let mut out = Vec::with_capacity(located.len());
    for (index, (offset, size)) in located.into_iter().enumerate() {
        let start_units = *times.get(index)?;
        let title = read_text_sample(r, len, offset, size).unwrap_or_default();
        out.push(raw_chapter(
            start_units as f64 / f64::from(timescale),
            title,
        ));
    }
    Some(out)
}

/// Start time of each sample, in media timescale units, for at most `max` samples.
fn parse_stts(body: &[u8], max: usize) -> Vec<u64> {
    let mut reader = Reader::new(body);
    let mut times = Vec::new();
    if reader.skip(4).is_none() {
        return times;
    }
    let Some(entries) = reader.u32() else {
        return times;
    };
    let mut now = 0u64;
    'entries: for _ in 0..entries {
        let (Some(count), Some(delta)) = (reader.u32(), reader.u32()) else {
            break;
        };
        for _ in 0..count {
            if times.len() >= max {
                break 'entries;
            }
            times.push(now);
            now = now.saturating_add(u64::from(delta));
        }
    }
    times
}

/// Size of each sample, for at most `max` samples (fewer when the table is short).
fn parse_stsz(body: &[u8], max: usize) -> Vec<u32> {
    let mut reader = Reader::new(body);
    let mut sizes = Vec::new();
    if reader.skip(4).is_none() {
        return sizes;
    }
    let (Some(uniform), Some(count)) = (reader.u32(), reader.u32()) else {
        return sizes;
    };
    let wanted = usize::try_from(count).unwrap_or(usize::MAX).min(max);
    if uniform != 0 {
        sizes.resize(wanted, uniform);
        return sizes;
    }
    for _ in 0..wanted {
        match reader.u32() {
            Some(size) => sizes.push(size),
            None => break,
        }
    }
    sizes
}

/// `(first_chunk, samples_per_chunk)` entries; `None` when the table is not in
/// ascending order of 1-based chunk numbers.
fn parse_stsc(body: &[u8]) -> Option<Vec<(u32, u32)>> {
    let mut reader = Reader::new(body);
    reader.skip(4)?;
    let entries = reader.u32()?;
    let mut out = Vec::new();
    for _ in 0..entries {
        if out.len() >= MAX_STSC_ENTRIES {
            break;
        }
        let (Some(first), Some(per_chunk), Some(_description)) =
            (reader.u32(), reader.u32(), reader.u32())
        else {
            break;
        };
        if first == 0 || out.last().is_some_and(|(previous, _)| *previous > first) {
            return None;
        }
        out.push((first, per_chunk));
    }
    Some(out)
}

/// File offset and size of each sample, in sample order.
fn locate_samples(
    stsc: &[(u32, u32)],
    chunk_table: &[u8],
    wide: bool,
    sizes: &[u32],
) -> Vec<(u64, u32)> {
    let mut out = Vec::with_capacity(sizes.len());
    let mut reader = Reader::new(chunk_table);
    if reader.skip(4).is_none() {
        return out;
    }
    let Some(chunks) = reader.u32() else {
        return out;
    };
    let mut entry = 0usize;
    let mut chunk = 0u32;
    while out.len() < sizes.len() && chunk < chunks {
        chunk += 1;
        let offset = if wide {
            reader.u64()
        } else {
            reader.u32().map(u64::from)
        };
        let Some(mut offset) = offset else {
            break;
        };
        while let Some((next_first, _)) = stsc.get(entry + 1) {
            if *next_first <= chunk {
                entry += 1;
            } else {
                break;
            }
        }
        let Some(&(first, per_chunk)) = stsc.get(entry) else {
            break;
        };
        if first > chunk {
            continue;
        }
        for _ in 0..per_chunk {
            let Some(&size) = sizes.get(out.len()) else {
                break;
            };
            out.push((offset, size));
            offset = offset.saturating_add(u64::from(size));
        }
    }
    out
}

/// A text-track sample: u16 big-endian length, then the text (UTF-8, or UTF-16
/// with a byte-order mark).
fn read_text_sample<R: Read + Seek>(r: &mut R, len: u64, offset: u64, size: u32) -> Option<String> {
    let count = u64::from(size).min(MAX_SAMPLE_BYTES);
    let buffer = read_vec_at(r, len, offset, count, MAX_SAMPLE_BYTES)?;
    let mut reader = Reader::new(&buffer);
    let declared = usize::from(reader.u16()?);
    let text = reader.take(declared.min(reader.remaining()))?;
    Some(match text {
        [0xFE, 0xFF, rest @ ..] => utf16_lossy(rest, true),
        [0xFF, 0xFE, rest @ ..] => utf16_lossy(rest, false),
        _ => String::from_utf8_lossy(text).into_owned(),
    })
}

// ---------------------------------------------------------------------------
// ID3v2 (MP3 and others)
// ---------------------------------------------------------------------------

fn synchsafe(bytes: [u8; 4]) -> u32 {
    bytes
        .iter()
        .fold(0u32, |acc, byte| (acc << 7) | u32::from(byte & 0x7F))
}

/// Undo unsynchronisation: every `FF 00` becomes `FF`.
fn unsync_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut index = 0usize;
    while let Some(&byte) = data.get(index) {
        out.push(byte);
        index += if byte == 0xFF && data.get(index + 1) == Some(&0) {
            2
        } else {
            1
        };
    }
    out
}

struct Id3Frame<'a> {
    id: [u8; 4],
    body: Cow<'a, [u8]>,
}

/// Iterates ID3v2.3 / v2.4 frames. Skips compressed and encrypted frames, applies
/// the per-frame header bytes (group id, data length indicator, unsynchronisation)
/// and stops at padding, an invalid frame id or a frame that runs past the data.
struct Id3Frames<'a> {
    data: &'a [u8],
    pos: usize,
    major: u8,
    all_unsync: bool,
}

impl<'a> Iterator for Id3Frames<'a> {
    type Item = Id3Frame<'a>;

    fn next(&mut self) -> Option<Id3Frame<'a>> {
        loop {
            let header_end = self.pos.checked_add(10)?;
            let header = self.data.get(self.pos..header_end)?;
            let [i0, i1, i2, i3, z0, z1, z2, z3, _flags1, flags2] =
                <[u8; 10]>::try_from(header).ok()?;
            let id = [i0, i1, i2, i3];
            if !id
                .iter()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            {
                return None;
            }
            let size_bytes = [z0, z1, z2, z3];
            let size = if self.major == 4 && size_bytes.iter().all(|byte| byte & 0x80 == 0) {
                synchsafe(size_bytes)
            } else {
                u32::from_be_bytes(size_bytes)
            };
            let end = header_end.checked_add(usize::try_from(size).ok()?)?;
            let mut body = self.data.get(header_end..end)?;
            self.pos = end;
            let (skip, grouped, length_indicator, unsync) = if self.major == 4 {
                (
                    flags2 & 0x0C != 0,
                    flags2 & 0x40 != 0,
                    flags2 & 0x01 != 0,
                    flags2 & 0x02 != 0 || self.all_unsync,
                )
            } else {
                (flags2 & 0xC0 != 0, flags2 & 0x20 != 0, false, false)
            };
            if skip {
                continue;
            }
            if grouped {
                let Some(rest) = body.get(1..) else {
                    continue;
                };
                body = rest;
            }
            if length_indicator {
                let Some(rest) = body.get(4..) else {
                    continue;
                };
                body = rest;
            }
            let body = if unsync {
                Cow::Owned(unsync_decode(body))
            } else {
                Cow::Borrowed(body)
            };
            return Some(Id3Frame { id, body });
        }
    }
}

/// Text frame body: encoding byte, then the first string of the frame.
fn decode_id3_text(body: &[u8]) -> String {
    let Some((&encoding, text)) = body.split_first() else {
        return String::new();
    };
    match encoding {
        1 | 2 => {
            let (big_endian, text) = match (encoding, text) {
                (_, [0xFE, 0xFF, rest @ ..]) => (true, rest),
                (_, [0xFF, 0xFE, rest @ ..]) => (false, rest),
                (2, rest) => (true, rest),
                (_, rest) => (false, rest),
            };
            let units = text
                .chunks_exact(2)
                .position(|pair| pair == [0, 0])
                .map_or(text.len(), |at| at * 2);
            utf16_lossy(text.get(..units).unwrap_or(&[]), big_endian)
        }
        3 => {
            let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
            String::from_utf8_lossy(text.get(..end).unwrap_or(&[])).into_owned()
        }
        _ => {
            // ISO-8859-1: every byte is its own code point.
            let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
            text.get(..end)
                .unwrap_or(&[])
                .iter()
                .map(|&b| char::from(b))
                .collect()
        }
    }
}

fn id3_sub_text(frames: &[u8], major: u8, wanted: &[u8; 4]) -> Option<String> {
    Id3Frames {
        data: frames,
        pos: 0,
        major,
        all_unsync: false,
    }
    .find(|frame| &frame.id == wanted)
    .map(|frame| decode_id3_text(&frame.body))
}

/// `CHAP`: element id, start ms, end ms, start offset, end offset, sub-frames.
fn parse_chap(body: &[u8], major: u8) -> Option<(Vec<u8>, Chapter)> {
    let nul = body.iter().position(|&b| b == 0)?;
    let element = body.get(..nul)?.to_vec();
    let mut reader = Reader::new(body.get(nul + 1..)?);
    let start_ms = reader.u32()?;
    reader.skip(12)?; // end ms, start offset, end offset
    let subframes = reader.rest();
    let title = id3_sub_text(subframes, major, b"TIT2")
        .or_else(|| id3_sub_text(subframes, major, b"TIT3"))
        .unwrap_or_default();
    Some((element, raw_chapter(f64::from(start_ms) / 1000.0, title)))
}

/// `CTOC`: element id, flags, entry count, then the child element ids.
fn parse_ctoc(body: &[u8]) -> Vec<Vec<u8>> {
    fn inner(body: &[u8]) -> Option<Vec<Vec<u8>>> {
        let nul = body.iter().position(|&b| b == 0)?;
        let mut reader = Reader::new(body.get(nul + 1..)?);
        reader.skip(1)?; // flags
        let count = reader.u8()?;
        let mut children = Vec::new();
        for _ in 0..count {
            let rest = reader.rest();
            let end = rest.iter().position(|&b| b == 0)?;
            children.push(rest.get(..end)?.to_vec());
            reader.skip(end + 1)?;
        }
        Some(children)
    }
    inner(body).unwrap_or_default()
}

fn id3_chapters(major: u8, flags: u8, body: Vec<u8>) -> Vec<Chapter> {
    let body = if major == 3 && flags & 0x80 != 0 {
        unsync_decode(&body)
    } else {
        body
    };
    let mut data: &[u8] = &body;
    if flags & 0x40 != 0 {
        let Some(ext_size) = data.get(..4).and_then(|b| <[u8; 4]>::try_from(b).ok()) else {
            return Vec::new();
        };
        let skip = if major == 4 {
            u64::from(synchsafe(ext_size))
        } else {
            u64::from(u32::from_be_bytes(ext_size)) + 4
        };
        let Some(rest) = usize::try_from(skip).ok().and_then(|n| data.get(n..)) else {
            return Vec::new();
        };
        data = rest;
    }
    let frames = Id3Frames {
        data,
        pos: 0,
        major,
        all_unsync: major == 4 && flags & 0x80 != 0,
    };
    let mut found: Vec<(Vec<u8>, Chapter)> = Vec::new();
    let mut table_order: HashMap<Vec<u8>, usize> = HashMap::new();
    for frame in frames {
        match &frame.id {
            b"CHAP" if found.len() < MAX_CHAPTERS => {
                if let Some(parsed) = parse_chap(&frame.body, major) {
                    found.push(parsed);
                }
            }
            b"CTOC" => {
                for child in parse_ctoc(&frame.body) {
                    let next = table_order.len();
                    table_order.entry(child).or_insert(next);
                }
            }
            _ => {}
        }
    }
    // Start time decides the order; the table of contents only breaks ties.
    found.sort_by(|(a_id, a), (b_id, b)| {
        let a_rank = table_order.get(a_id).copied().unwrap_or(usize::MAX);
        let b_rank = table_order.get(b_id).copied().unwrap_or(usize::MAX);
        a.start.total_cmp(&b.start).then(a_rank.cmp(&b_rank))
    });
    found.into_iter().map(|(_, chapter)| chapter).collect()
}

/// Chapters of the ID3v2 tag at the start of the file, and the offset just past
/// that tag (when it lies inside the file).
fn read_id3<R: Read + Seek>(r: &mut R, len: u64) -> (Vec<Chapter>, Option<u64>) {
    let mut header = [0u8; 10];
    if !read_exact_at(r, len, 0, &mut header) {
        return (Vec::new(), None);
    }
    let [i, d, three, major, _revision, flags, s0, s1, s2, s3] = header;
    if [i, d, three] != *b"ID3" || [s0, s1, s2, s3].iter().any(|byte| byte & 0x80 != 0) {
        return (Vec::new(), None);
    }
    let size = u64::from(synchsafe([s0, s1, s2, s3]));
    let footer = major == 4 && flags & 0x10 != 0;
    let total = 10 + size + if footer { 10 } else { 0 };
    let tag_end = (total <= len).then_some(total);
    if !(major == 3 || major == 4) || size > MAX_ID3_BYTES {
        return (Vec::new(), tag_end);
    }
    let available = size.min(len.saturating_sub(10));
    let Some(body) = read_vec_at(r, len, 10, available, MAX_ID3_BYTES) else {
        return (Vec::new(), tag_end);
    };
    (id3_chapters(major, flags, body), tag_end)
}

// ---------------------------------------------------------------------------
// FLAC and Ogg Vorbis comments
// ---------------------------------------------------------------------------

/// `HH:MM:SS.mmm`, also `MM:SS.mmm` and `SS.mmm`, in seconds.
fn parse_vorbis_time(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    let mut total = 0.0f64;
    for (index, part) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        let part = if last {
            part.replace(',', ".")
        } else {
            (*part).to_owned()
        };
        let digits = part.bytes().filter(u8::is_ascii_digit).count();
        let dots = part.bytes().filter(|&b| b == b'.').count();
        let clean = part.bytes().all(|b| b.is_ascii_digit() || b == b'.');
        let fraction_ok = last || dots == 0;
        if !clean || digits == 0 || digits > 12 || dots > 1 || !fraction_ok {
            return None;
        }
        total = total * 60.0 + part.parse::<f64>().ok()?;
    }
    (total.is_finite() && total >= 0.0).then_some(total)
}

/// `CHAPTERnnn=time` and `CHAPTERnnnNAME=title` comment pairs to chapters.
fn chapters_from_vorbis<'a>(items: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<Chapter> {
    let mut times: BTreeMap<u32, f64> = BTreeMap::new();
    let mut names: BTreeMap<u32, String> = BTreeMap::new();
    for (key, value) in items {
        let upper = key.to_ascii_uppercase();
        let Some(rest) = upper.strip_prefix("CHAPTER") else {
            continue;
        };
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || digits > 9 {
            continue;
        }
        let Some((number, suffix)) = rest.split_at_checked(digits) else {
            continue;
        };
        let Ok(number) = number.parse::<u32>() else {
            continue;
        };
        match suffix {
            "" => {
                if times.len() < MAX_CHAPTERS
                    && let Some(time) = parse_vorbis_time(value)
                {
                    times.entry(number).or_insert(time);
                }
            }
            "NAME" => {
                if names.len() < MAX_CHAPTERS {
                    names.entry(number).or_insert_with(|| value.to_owned());
                }
            }
            _ => {}
        }
    }
    times
        .into_iter()
        .map(|(number, start)| raw_chapter(start, names.remove(&number).unwrap_or_default()))
        .collect()
}

/// `CHAPTER*` comments of a FLAC Vorbis-comment block (little-endian lengths).
fn flac_chapter_comments(block: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut reader = Reader::new(block);
    let Some(vendor_len) = reader.u32_le() else {
        return out;
    };
    if reader
        .skip(usize::try_from(vendor_len).unwrap_or(usize::MAX))
        .is_none()
    {
        return out;
    }
    let Some(count) = reader.u32_le() else {
        return out;
    };
    for _ in 0..count {
        let Some(item_len) = reader.u32_le() else {
            break;
        };
        let Some(item) = reader.take(usize::try_from(item_len).unwrap_or(usize::MAX)) else {
            break;
        };
        let Some(equals) = item.iter().position(|&b| b == b'=') else {
            continue;
        };
        let (Some(key), Some(value)) = (item.get(..equals), item.get(equals + 1..)) else {
            continue;
        };
        if key
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"CHAPTER"))
        {
            out.push((
                String::from_utf8_lossy(key).into_owned(),
                String::from_utf8_lossy(value).into_owned(),
            ));
            if out.len() >= 4 * MAX_CHAPTERS {
                break;
            }
        }
    }
    out
}

fn read_flac<R: Read + Seek>(r: &mut R, len: u64, start: u64) -> Vec<Chapter> {
    let mut magic = [0u8; 4];
    if !read_exact_at(r, len, start, &mut magic) || &magic != b"fLaC" {
        return Vec::new();
    }
    let mut pos = start.saturating_add(4);
    for _ in 0..MAX_FLAC_BLOCKS {
        let mut header = [0u8; 4];
        if !read_exact_at(r, len, pos, &mut header) {
            break;
        }
        let [flags, l0, l1, l2] = header;
        let block_len = u64::from(u32::from_be_bytes([0, l0, l1, l2]));
        let body_pos = pos.saturating_add(4);
        let Some(next) = body_pos.checked_add(block_len) else {
            break;
        };
        if next > len {
            break;
        }
        if flags & 0x7F == 4 {
            let Some(block) = read_vec_at(r, len, body_pos, block_len, MAX_VORBIS_BLOCK_BYTES)
            else {
                break;
            };
            let comments = flac_chapter_comments(&block);
            return chapters_from_vorbis(
                comments
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str())),
            );
        }
        if flags & 0x80 != 0 {
            break;
        }
        pos = next;
    }
    Vec::new()
}

/// Ogg Vorbis comment header through Lofty (cover art skipped). Lofty is wrapped
/// in `catch_unwind` so a parser bug on a hostile file cannot take the player down.
fn read_ogg<R: Read + Seek>(r: &mut R) -> Vec<Chapter> {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        r.seek(SeekFrom::Start(0)).ok()?;
        let options = ParseOptions::new()
            .read_properties(false)
            .read_cover_art(false);
        let file = VorbisFile::read_from(r, options).ok()?;
        let items: Vec<(String, String)> = file
            .vorbis_comments()
            .items()
            .filter(|(key, _)| {
                key.get(..7)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("CHAPTER"))
            })
            .take(4 * MAX_CHAPTERS)
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        Some(items)
    }));
    match outcome {
        Ok(Some(items)) => chapters_from_vorbis(
            items
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        ),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// CUE sheets
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

/// UTF-8 when valid, otherwise Windows-1252 (every byte maps to one character).
fn decode_cue_bytes(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|&b| cp1252_char(b)).collect(),
    }
}

/// First word of a CUE line (uppercased) and the rest of the line.
fn cue_command(line: &str) -> (String, &str) {
    match line.split_once(char::is_whitespace) {
        Some((command, rest)) => (command.to_ascii_uppercase(), rest.trim()),
        None => (line.to_ascii_uppercase(), ""),
    }
}

/// First token, which may be double-quoted, and the remainder of the line.
fn cue_token(text: &str) -> (String, &str) {
    let text = text.trim_start();
    if let Some(quoted) = text.strip_prefix('"') {
        return match quoted.split_once('"') {
            Some((inside, rest)) => (inside.to_owned(), rest),
            None => (quoted.to_owned(), ""),
        };
    }
    match text.split_once(char::is_whitespace) {
        Some((token, rest)) => (token.to_owned(), rest),
        None => (text.to_owned(), ""),
    }
}

/// `mm:ss:ff` with 75 frames per second.
fn parse_cue_time(text: &str) -> Option<f64> {
    let mut parts = text.trim().split(':');
    let mut next = || -> Option<f64> {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 6 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse::<u32>().ok().map(f64::from)
    };
    let (minutes, seconds, frames) = (next()?, next()?, next()?);
    if parts.next().is_some() {
        return None;
    }
    Some(minutes * 60.0 + seconds + frames / 75.0)
}

fn last_path_segment(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

fn same_file_name(a: &str, b: &str) -> bool {
    last_path_segment(a).to_lowercase() == last_path_segment(b).to_lowercase()
}

/// The names on the `FILE` lines of a CUE sheet.
fn cue_file_names(text: &str) -> Vec<String> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .filter_map(|line| {
            let (command, rest) = cue_command(line.trim());
            (command == "FILE").then(|| cue_token(rest).0)
        })
        .collect()
}

struct CueTrack {
    title: String,
    start: Option<f64>,
    audio: bool,
}

fn finish_cue_track(current: &mut Option<CueTrack>, out: &mut Vec<Chapter>) {
    if let Some(track) = current.take()
        && track.audio
        && let Some(start) = track.start
        && out.len() < MAX_CHAPTERS
    {
        out.push(raw_chapter(start, track.title));
    }
}

/// Tracks of a CUE sheet in file order. With `only_file`, only the tracks that
/// belong to the `FILE` line with that name are used.
fn parse_cue_tracks(text: &str, only_file: Option<&str>) -> Vec<Chapter> {
    let mut out = Vec::new();
    let mut current: Option<CueTrack> = None;
    let mut in_scope = only_file.is_none();
    for line in text.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (command, rest) = cue_command(line);
        match command.as_str() {
            "FILE" => {
                finish_cue_track(&mut current, &mut out);
                in_scope = match only_file {
                    None => true,
                    Some(wanted) => same_file_name(&cue_token(rest).0, wanted),
                };
            }
            "TRACK" => {
                finish_cue_track(&mut current, &mut out);
                if in_scope {
                    let (_number, kind) = cue_token(rest);
                    current = Some(CueTrack {
                        title: String::new(),
                        start: None,
                        audio: cue_token(kind).0.eq_ignore_ascii_case("AUDIO"),
                    });
                }
            }
            "TITLE" => {
                if let Some(track) = current.as_mut()
                    && track.title.is_empty()
                {
                    track.title = if rest.starts_with('"') {
                        cue_token(rest).0
                    } else {
                        rest.to_owned()
                    };
                }
            }
            "INDEX" => {
                if let Some(track) = current.as_mut() {
                    let (number, time) = cue_token(rest);
                    if number.parse::<u32>().ok() == Some(1) {
                        track.start = parse_cue_time(&cue_token(time).0);
                    }
                }
            }
            _ => {}
        }
    }
    finish_cue_track(&mut current, &mut out);
    out
}

/// A `.cue` file with the same stem beside the audio file (stem compared
/// case-insensitively).
fn find_cue(path: &Path) -> Option<PathBuf> {
    for extension in ["cue", "CUE", "Cue"] {
        let candidate = path.with_extension(extension);
        if candidate.is_file() {
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
            has_extension(candidate, &["cue"])
                && candidate
                    .file_stem()
                    .is_some_and(|name| name.to_string_lossy().to_lowercase() == stem)
                && candidate.is_file()
        })
}

/// Raw chapters from the CUE sheet beside `path`, only when the sheet is for this
/// file: it names exactly one audio file, or exactly one of its `FILE` lines
/// matches this file's name.
fn read_cue_beside(path: &Path) -> Vec<Chapter> {
    fn load(path: &Path) -> Option<Vec<Chapter>> {
        let cue_path = find_cue(path)?;
        let mut file = File::open(cue_path).ok()?;
        if file.metadata().ok()?.len() > MAX_CUE_BYTES {
            return None;
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(MAX_CUE_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_CUE_BYTES {
            return None;
        }
        let text = decode_cue_bytes(&bytes);
        let files = cue_file_names(&text);
        let this_name = path.file_name()?.to_string_lossy().into_owned();
        match files.as_slice() {
            [] => None,
            [_single] => Some(parse_cue_tracks(&text, None)),
            many => {
                let matching = many
                    .iter()
                    .filter(|name| same_file_name(name, &this_name))
                    .count();
                (matching == 1).then(|| parse_cue_tracks(&text, Some(&this_name)))
            }
        }
    }
    load(path).unwrap_or_default()
}
