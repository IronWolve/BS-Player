//! Tag and artwork editing inside audio files (the "Media information" editor).
//!
//! Data-safety contract: the original file is never modified in place. An edit
//! is applied to a copy created beside the original, the copy is verified
//! (it must re-open, keep the same audio properties and audio payload, read back
//! the edited fields and keep every untouched field and frame), and only then
//! does an atomic rename replace the original. Every error path removes the
//! copy and leaves the original untouched.
//!
//! Self-contained and panic-free: every read is bounded, every untrusted number
//! is checked before it sizes an allocation or a loop, and the public entry
//! points convert a stray panic inside a parser into an error. Nothing here
//! touches the UI, the player or the profile; the caller refreshes the library
//! identity after a write.
//!
//! Supported for writing: MP3 (ID3v2), M4A/M4B/MP4 (see `mp4_preflight`), FLAC,
//! Ogg Vorbis and Ogg Opus. Everything else, WAV and AIFF included, is shown
//! read-only.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::hash::Hasher;
use std::io::{self, BufReader, Cursor, Read, Seek, SeekFrom};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::{AudioFile, FileType, TaggedFile, TaggedFileExt};
use lofty::id3::v2::Id3v2Tag;
use lofty::mp4::{AtomIdent, Mp4File};
use lofty::mpeg::MpegFile;
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::probe::Probe;
use lofty::tag::{ItemKey, ItemValue, MergeTag, SplitTag, Tag, TagExt, TagType};

/// Longest text accepted for one single-line field, in characters.
const MAX_TEXT_CHARS: usize = 512;
/// Longest comment accepted, in characters.
const MAX_COMMENT_CHARS: usize = 4096;
/// Largest number accepted for a track or disc number or total (an MP4 `trkn`
/// atom stores 16 bits).
const MAX_COUNT: u32 = 65_535;
/// Largest year accepted.
const MAX_YEAR: u32 = 9999;
/// Largest file the editor will rewrite. The format writers hold the whole
/// file in memory (peak use is roughly two to three times its size).
const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;
/// Largest artwork file accepted.
const MAX_ARTWORK_BYTES: usize = 32 * 1024 * 1024;
/// Largest artwork width or height accepted, in pixels.
const MAX_ARTWORK_SIDE: u32 = 16_000;
/// Artwork bigger than this, in bytes, is downscaled.
const DOWNSCALE_BYTES: usize = 5 * 1024 * 1024;
/// Artwork wider or taller than this, in pixels, is downscaled to fit.
const DOWNSCALE_SIDE: u32 = 1600;
/// Decoder memory budget while validating or downscaling artwork.
const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;
/// Most top-level MP4 atoms examined.
const MAX_TOP_ATOMS: usize = 100_000;
/// Largest `moov` atom read into memory.
const MAX_MOOV_BYTES: u64 = 32 * 1024 * 1024;
/// Deepest MP4 atom nesting followed inside `moov`.
const MAX_ATOM_DEPTH: u32 = 8;
/// Most MP4 chunk offsets collected.
const MAX_CHUNK_OFFSETS: usize = 8_000_000;
/// Most FLAC metadata blocks visited.
const MAX_FLAC_BLOCKS: usize = 4096;
/// Prefix of the working copy; anything starting with it is ours to remove.
const TEMP_PREFIX: &str = ".bs-player-tag-";
/// Rename attempts on Windows (a playing file is briefly locked).
const RENAME_ATTEMPTS_WINDOWS: u32 = 5;
/// Pause between rename attempts.
const RENAME_RETRY_DELAY: Duration = Duration::from_millis(100);
/// Wording shown when the original cannot be replaced because it is open.
const IN_USE: &str = "The file is in use (is it playing? stop it first)";

/// What the editor shows for one file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EditableTags {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub composer: String,
    pub genre: String,
    pub year: String,
    pub comment: String,
    pub track: String,
    pub track_total: String,
    pub disc: String,
    pub disc_total: String,
    pub has_artwork: bool,
    pub artwork_count: usize,
    /// For example "MP3 (ID3v2.3)", "M4A", "FLAC", "Ogg Vorbis", "Ogg Opus".
    pub format: String,
    pub writable: bool,
    /// Why the file cannot be edited (unsupported format, read-only file, ...).
    pub read_only_reason: Option<String>,
}

/// What to do with the artwork.
#[derive(Debug, Clone, PartialEq)]
pub enum ArtworkEdit {
    Keep,
    Remove,
    /// The bytes of a JPEG, PNG, GIF, WebP or BMP image.
    Replace(Vec<u8>),
}

/// One edit. `None` leaves the field alone, `Some("")` removes it and
/// `Some(text)` sets it.
#[derive(Debug, Clone, Default)]
pub struct TagEdit {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub genre: Option<String>,
    pub year: Option<String>,
    pub comment: Option<String>,
    pub track: Option<String>,
    pub track_total: Option<String>,
    pub disc: Option<String>,
    pub disc_total: Option<String>,
    /// `None` is the same as `Keep`.
    pub artwork: Option<ArtworkEdit>,
}

/// Reads what the editor shows. Never panics; a file that cannot be edited is
/// reported with `writable == false` and a reason instead of an error. An error
/// means the file could not be read at all.
pub fn read_editable_tags(path: &Path) -> Result<EditableTags, String> {
    guarded(|| read_editable_inner(path))
}

/// Applies `edit` to the file at `path` safely (see the module notes). On any
/// error the original is left exactly as it was and no working copy remains.
/// The file's content digest changes: the caller must refresh the library
/// record afterwards.
pub fn write_tags(path: &Path, edit: &TagEdit) -> Result<(), String> {
    guarded(|| write_inner(path, edit))
}

fn guarded<T>(work: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(work)) {
        Ok(result) => result,
        Err(_) => Err("The tag editor hit an unexpected problem; nothing was changed".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Fields
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    Title,
    Artist,
    Album,
    AlbumArtist,
    Composer,
    Genre,
    Year,
    Comment,
    Track,
    TrackTotal,
    Disc,
    DiscTotal,
}

impl Field {
    const ALL: [Field; 12] = [
        Field::Title,
        Field::Artist,
        Field::Album,
        Field::AlbumArtist,
        Field::Composer,
        Field::Genre,
        Field::Year,
        Field::Comment,
        Field::Track,
        Field::TrackTotal,
        Field::Disc,
        Field::DiscTotal,
    ];

    fn label(self) -> &'static str {
        match self {
            Field::Title => "title",
            Field::Artist => "artist",
            Field::Album => "album",
            Field::AlbumArtist => "album artist",
            Field::Composer => "composer",
            Field::Genre => "genre",
            Field::Year => "year",
            Field::Comment => "comment",
            Field::Track => "track number",
            Field::TrackTotal => "track total",
            Field::Disc => "disc number",
            Field::DiscTotal => "disc total",
        }
    }

    /// Largest value of a numeric field; `None` for text fields.
    fn numeric_max(self) -> Option<u32> {
        match self {
            Field::Year => Some(MAX_YEAR),
            Field::Track | Field::TrackTotal | Field::Disc | Field::DiscTotal => Some(MAX_COUNT),
            _ => None,
        }
    }

    /// The generic tag key of a plain text field.
    fn text_key(self) -> Option<ItemKey> {
        match self {
            Field::Title => Some(ItemKey::TrackTitle),
            Field::Artist => Some(ItemKey::TrackArtist),
            Field::Album => Some(ItemKey::AlbumTitle),
            Field::AlbumArtist => Some(ItemKey::AlbumArtist),
            Field::Composer => Some(ItemKey::Composer),
            Field::Genre => Some(ItemKey::Genre),
            Field::Comment => Some(ItemKey::Comment),
            _ => None,
        }
    }

    /// Generic keys this field may change, for the untouched-items comparison.
    fn item_keys(self) -> Vec<ItemKey> {
        match self {
            Field::Year => vec![ItemKey::Year, ItemKey::RecordingDate],
            Field::Track | Field::TrackTotal => vec![ItemKey::TrackNumber, ItemKey::TrackTotal],
            Field::Disc | Field::DiscTotal => vec![ItemKey::DiscNumber, ItemKey::DiscTotal],
            other => other.text_key().into_iter().collect(),
        }
    }

    /// Format-specific frame or atom names this field may change.
    fn raw_names(self, kind: Kind) -> &'static [&'static str] {
        match kind {
            Kind::Mp3 => match self {
                Field::Title => &["TIT2"],
                Field::Artist => &["TPE1"],
                Field::Album => &["TALB"],
                Field::AlbumArtist => &["TPE2"],
                Field::Composer => &["TCOM"],
                Field::Genre => &["TCON"],
                Field::Year => &["TYER", "TDAT", "TIME", "TDRC", "TRDA"],
                Field::Comment => &["COMM"],
                Field::Track | Field::TrackTotal => &["TRCK"],
                Field::Disc | Field::DiscTotal => &["TPOS"],
            },
            Kind::Mp4 => match self {
                Field::Title => &["\u{a9}nam"],
                Field::Artist => &["\u{a9}ART"],
                Field::Album => &["\u{a9}alb"],
                Field::AlbumArtist => &["aART"],
                Field::Composer => &["\u{a9}wrt"],
                Field::Genre => &["\u{a9}gen", "gnre"],
                Field::Year => &["\u{a9}day"],
                Field::Comment => &["\u{a9}cmt"],
                Field::Track | Field::TrackTotal => &["trkn"],
                Field::Disc | Field::DiscTotal => &["disk"],
            },
            Kind::Flac | Kind::Vorbis | Kind::Opus => &[],
        }
    }
}

/// One value per `Field`, indexed by `Field as usize`.
type Values = [String; 12];

fn slot(field: Field) -> usize {
    field as usize
}

fn edit_value(edit: &TagEdit, field: Field) -> Option<&String> {
    match field {
        Field::Title => edit.title.as_ref(),
        Field::Artist => edit.artist.as_ref(),
        Field::Album => edit.album.as_ref(),
        Field::AlbumArtist => edit.album_artist.as_ref(),
        Field::Composer => edit.composer.as_ref(),
        Field::Genre => edit.genre.as_ref(),
        Field::Year => edit.year.as_ref(),
        Field::Comment => edit.comment.as_ref(),
        Field::Track => edit.track.as_ref(),
        Field::TrackTotal => edit.track_total.as_ref(),
        Field::Disc => edit.disc.as_ref(),
        Field::DiscTotal => edit.disc_total.as_ref(),
    }
}

/// The values the editor shows for a tag.
fn read_values(tag: &Tag) -> Values {
    let mut values: Values = Default::default();
    for field in Field::ALL {
        values[slot(field)] = match field {
            Field::Year => year_text(tag),
            Field::Comment => join_values(tag, &ItemKey::Comment, "\n"),
            Field::Track => number_pair(tag, &ItemKey::TrackNumber, &ItemKey::TrackTotal).0,
            Field::TrackTotal => number_pair(tag, &ItemKey::TrackNumber, &ItemKey::TrackTotal).1,
            Field::Disc => number_pair(tag, &ItemKey::DiscNumber, &ItemKey::DiscTotal).0,
            Field::DiscTotal => number_pair(tag, &ItemKey::DiscNumber, &ItemKey::DiscTotal).1,
            text => match text.text_key() {
                Some(key) => join_values(tag, &key, "; "),
                None => String::new(),
            },
        };
    }
    values
}

fn join_values(tag: &Tag, key: &ItemKey, separator: &str) -> String {
    let mut out = String::new();
    for text in tag.get_strings(key) {
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str(separator);
        }
        out.push_str(text);
    }
    out
}

/// The year as the library shows it: the first four digits of the stored date.
fn year_text(tag: &Tag) -> String {
    let stored = tag
        .get_string(&ItemKey::Year)
        .or_else(|| tag.get_string(&ItemKey::RecordingDate))
        .unwrap_or("");
    let digits: String = stored
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .take(4)
        .collect();
    match digits.len() {
        4 => digits
            .parse::<u32>()
            .map(|y| y.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Splits "3/12"-style values: the number, and the total from its own key or
/// from the part after the slash.
fn number_pair(tag: &Tag, number_key: &ItemKey, total_key: &ItemKey) -> (String, String) {
    let raw = tag.get_string(number_key).unwrap_or("").trim();
    let (number, inline_total) = match raw.split_once('/') {
        Some((number, total)) => (number.trim(), total.trim()),
        None => (raw, ""),
    };
    let total = tag
        .get_string(total_key)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or(inline_total);
    (number.to_string(), total.to_string())
}

/// True when `new` says the same as `current` for this field.
fn same_value(field: Field, new: &str, current: &str) -> bool {
    let (new, current) = (new.trim(), current.trim());
    if field.numeric_max().is_some() {
        if let (Ok(a), Ok(b)) = (new.parse::<u32>(), current.parse::<u32>()) {
            return a == b;
        }
    }
    new == current
}

// ---------------------------------------------------------------------------
// Validation and planning
// ---------------------------------------------------------------------------

/// An edit after validation: normalised text and decoded, bounded artwork.
struct Validated {
    texts: [Option<String>; 12],
    artwork: PlannedArtwork,
}

/// The edit actually to apply to one file.
struct Plan {
    texts: [Option<String>; 12],
    artwork: PlannedArtwork,
}

enum PlannedArtwork {
    Keep,
    Remove,
    Replace(Prepared),
}

struct Prepared {
    data: Vec<u8>,
    mime: MimeType,
}

fn validate_edit(edit: &TagEdit) -> Result<Validated, String> {
    let mut texts: [Option<String>; 12] = Default::default();
    for field in Field::ALL {
        if let Some(raw) = edit_value(edit, field) {
            texts[slot(field)] = Some(normalise_value(field, raw)?);
        }
    }
    let artwork = match &edit.artwork {
        None | Some(ArtworkEdit::Keep) => PlannedArtwork::Keep,
        Some(ArtworkEdit::Remove) => PlannedArtwork::Remove,
        Some(ArtworkEdit::Replace(bytes)) => PlannedArtwork::Replace(prepare_artwork(bytes)?),
    };
    Ok(Validated { texts, artwork })
}

fn normalise_value(field: Field, raw: &str) -> Result<String, String> {
    if let Some(max) = field.numeric_max() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(String::new());
        }
        if !trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(format!(
                "The {} must be a whole number such as 7, not \"{}\"",
                field.label(),
                shorten(trimmed)
            ));
        }
        return match trimmed.parse::<u32>() {
            Ok(number) if number <= max => Ok(number.to_string()),
            _ => Err(format!("The {} must be between 0 and {max}", field.label())),
        };
    }
    let (multiline, limit) = if field == Field::Comment {
        (true, MAX_COMMENT_CHARS)
    } else {
        (false, MAX_TEXT_CHARS)
    };
    let text = clean_text(raw, multiline);
    if text.chars().count() > limit {
        return Err(format!(
            "The {} is too long (at most {limit} characters)",
            field.label()
        ));
    }
    Ok(text)
}

/// Removes control characters (NUL included), folds line breaks, and trims.
fn clean_text(raw: &str, multiline: bool) -> String {
    let unified = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(unified.len().min(MAX_COMMENT_CHARS * 4));
    for ch in unified.chars() {
        if ch == '\n' {
            out.push(if multiline { '\n' } else { ' ' });
        } else if ch == '\t' {
            out.push(' ');
        } else if !ch.is_control() {
            out.push(ch);
        }
    }
    out.trim().to_string()
}

fn shorten(text: &str) -> String {
    text.chars().take(24).collect()
}

/// Decides what really changes. Fields that already say the requested thing
/// are dropped so that an untouched value is never rewritten.
fn make_plan(valid: Validated, current: &Values, artwork_count: usize) -> Result<Plan, String> {
    let mut texts: [Option<String>; 12] = Default::default();
    let mut result = current.clone();
    for field in Field::ALL {
        if let Some(new) = &valid.texts[slot(field)] {
            if !same_value(field, new, &current[slot(field)]) {
                result[slot(field)] = new.clone();
                texts[slot(field)] = Some(new.clone());
            }
        }
    }
    for (number, total, name) in [
        (Field::Track, Field::TrackTotal, "track"),
        (Field::Disc, Field::DiscTotal, "disc"),
    ] {
        if result[slot(number)].trim().is_empty() && !result[slot(total)].trim().is_empty() {
            return Err(format!(
                "Set the {name} number as well, or clear the {name} total"
            ));
        }
    }
    let artwork = match valid.artwork {
        PlannedArtwork::Remove if artwork_count == 0 => PlannedArtwork::Keep,
        other => other,
    };
    let unchanged = texts.iter().all(Option::is_none) && matches!(artwork, PlannedArtwork::Keep);
    if unchanged {
        return Err("Nothing to change: the file already has these values".to_string());
    }
    Ok(Plan { texts, artwork })
}

/// The values a correct write must produce.
fn expected_values(before: &Values, plan: &Plan) -> Values {
    let mut values = before.clone();
    for field in Field::ALL {
        if let Some(new) = &plan.texts[slot(field)] {
            values[slot(field)] = new.clone();
        }
    }
    values
}

// ---------------------------------------------------------------------------
// Artwork
// ---------------------------------------------------------------------------

fn prepare_artwork(bytes: &[u8]) -> Result<Prepared, String> {
    if bytes.is_empty() {
        return Err("The artwork file is empty".to_string());
    }
    if bytes.len() > MAX_ARTWORK_BYTES {
        return Err(format!(
            "The artwork is too large ({} MiB; the limit is {} MiB)",
            bytes.len() / (1024 * 1024),
            MAX_ARTWORK_BYTES / (1024 * 1024)
        ));
    }
    let not_image = || "The artwork is not a valid JPEG, PNG, GIF, WebP or BMP image".to_string();
    let format = image::guess_format(bytes).map_err(|_| not_image())?;
    let mime = mime_for(format).ok_or_else(not_image)?;
    let (width, height) = image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| not_image())?;
    if width == 0 || height == 0 {
        return Err(not_image());
    }
    if width > MAX_ARTWORK_SIDE || height > MAX_ARTWORK_SIDE {
        return Err(format!(
            "The artwork is too large ({width} x {height} pixels; the limit is {MAX_ARTWORK_SIDE} on a side)"
        ));
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_ARTWORK_SIDE);
    limits.max_image_height = Some(MAX_ARTWORK_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| not_image())?;
    if bytes.len() <= DOWNSCALE_BYTES && width <= DOWNSCALE_SIDE && height <= DOWNSCALE_SIDE {
        return Ok(Prepared {
            data: bytes.to_vec(),
            mime,
        });
    }
    let scaled = if width > DOWNSCALE_SIDE || height > DOWNSCALE_SIDE {
        decoded.resize(
            DOWNSCALE_SIDE,
            DOWNSCALE_SIDE,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        decoded
    };
    let rgb = scaled.to_rgb8();
    let mut out = Vec::new();
    {
        use image::ImageEncoder;
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
            .write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|error| format!("The artwork could not be resized: {error}"))?;
    }
    Ok(Prepared {
        data: out,
        mime: MimeType::Jpeg,
    })
}

fn reencode_as_png(bytes: &[u8]) -> Result<Prepared, String> {
    let fail = |error: image::ImageError| format!("The artwork could not be converted: {error}");
    let decoded = image::load_from_memory(bytes).map_err(fail)?;
    let mut out = Vec::new();
    decoded
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(fail)?;
    Ok(Prepared {
        data: out,
        mime: MimeType::Png,
    })
}

fn mime_for(format: image::ImageFormat) -> Option<MimeType> {
    match format {
        image::ImageFormat::Jpeg => Some(MimeType::Jpeg),
        image::ImageFormat::Png => Some(MimeType::Png),
        image::ImageFormat::Gif => Some(MimeType::Gif),
        image::ImageFormat::Bmp => Some(MimeType::Bmp),
        image::ImageFormat::WebP => Some(MimeType::Unknown("image/webp".to_string())),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Reading a file
// ---------------------------------------------------------------------------

/// The supported write targets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Mp3,
    Mp4,
    Flac,
    Vorbis,
    Opus,
}

fn kind_for(file_type: FileType) -> Option<Kind> {
    match file_type {
        FileType::Mpeg => Some(Kind::Mp3),
        FileType::Mp4 => Some(Kind::Mp4),
        FileType::Flac => Some(Kind::Flac),
        FileType::Vorbis => Some(Kind::Vorbis),
        FileType::Opus => Some(Kind::Opus),
        _ => None,
    }
}

/// Audio properties that an edit must never change.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct AudioSig {
    duration_ms: u128,
    sample_rate: Option<u32>,
    channels: Option<u8>,
}

/// Hash and length of a blob, for comparisons that must not keep big data.
type BlobSig = (usize, u64);

/// Everything compared before and after a write.
struct Snapshot {
    file_type: FileType,
    /// Values of the displayed tag (the primary tag, else the first one).
    values: Values,
    /// Pictures of the displayed tag, sorted.
    pictures: Vec<(String, BlobSig)>,
    /// Every item of the displayed tag, sorted.
    items: Vec<(String, BlobSig)>,
    sig: AudioSig,
    tag_types: Vec<TagType>,
    /// Type of the displayed tag, when the file has one.
    display_type: Option<TagType>,
}

fn blob_sig(bytes: &[u8]) -> BlobSig {
    let mut hasher = std::hash::DefaultHasher::new();
    hasher.write(bytes);
    (bytes.len(), hasher.finish())
}

fn parse_options() -> ParseOptions {
    ParseOptions::new()
}

/// Detects the format from the content only, never from the file name: the
/// working copy has no useful extension, so the original is judged the same way.
fn detect_type(path: &Path) -> Result<Option<FileType>, String> {
    let file = File::open(path).map_err(|error| format!("Cannot open the file: {error}"))?;
    let probe = Probe::new(BufReader::new(file))
        .guess_file_type()
        .map_err(|error| format!("Cannot read the file: {error}"))?;
    Ok(probe.file_type())
}

fn open_tagged(path: &Path, file_type: FileType) -> Result<TaggedFile, String> {
    crate::media::validate_lofty_input(path)?;
    let file = File::open(path).map_err(|error| format!("Cannot open the file: {error}"))?;
    Probe::with_file_type(BufReader::new(file), file_type)
        .options(parse_options())
        .read()
        .map_err(|error| error.to_string())
}

fn snapshot(path: &Path, file_type: FileType) -> Result<Snapshot, String> {
    let tagged = open_tagged(path, file_type)?;
    let properties = tagged.properties();
    let sig = AudioSig {
        duration_ms: properties.duration().as_millis(),
        sample_rate: properties.sample_rate(),
        channels: properties.channels(),
    };
    let tag_types: Vec<TagType> = tagged.tags().iter().map(Tag::tag_type).collect();
    let mut snap = Snapshot {
        file_type: tagged.file_type(),
        values: Default::default(),
        pictures: Vec::new(),
        items: Vec::new(),
        sig,
        tag_types,
        display_type: None,
    };
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        snap.display_type = Some(tag.tag_type());
        snap.values = read_values(tag);
        snap.pictures = tag
            .pictures()
            .iter()
            .map(|picture| {
                (
                    format!("{:?}", picture.pic_type()),
                    blob_sig(picture.data()),
                )
            })
            .collect();
        snap.pictures.sort();
        snap.items = tag
            .items()
            .map(|item| {
                let bytes: &[u8] = match item.value() {
                    ItemValue::Text(text) | ItemValue::Locator(text) => text.as_bytes(),
                    ItemValue::Binary(binary) => binary,
                };
                (format!("{:?}", item.key()), blob_sig(bytes))
            })
            .collect();
        snap.items.sort();
    }
    Ok(snap)
}

/// Counts of format-specific frames (ID3v2) or atoms (MP4 `ilst`): the part of
/// the tag the generic view cannot show, such as chapter frames.
fn raw_counts(path: &Path, kind: Kind) -> Result<BTreeMap<String, usize>, String> {
    crate::media::validate_lofty_input(path)?;
    let mut counts = BTreeMap::new();
    let open = || -> Result<BufReader<File>, String> {
        File::open(path)
            .map(BufReader::new)
            .map_err(|error| format!("Cannot open the file: {error}"))
    };
    match kind {
        Kind::Mp3 => {
            let mut reader = open()?;
            let file = MpegFile::read_from(&mut reader, parse_options())
                .map_err(|error| error.to_string())?;
            if let Some(tag) = file.id3v2() {
                for frame in tag {
                    *counts.entry(frame.id().as_str().to_string()).or_insert(0) += 1;
                }
            }
        }
        Kind::Mp4 => {
            let mut reader = open()?;
            let file = Mp4File::read_from(&mut reader, parse_options())
                .map_err(|error| error.to_string())?;
            if let Some(ilst) = file.ilst() {
                for atom in ilst {
                    let name = match atom.ident() {
                        AtomIdent::Fourcc(bytes) => bytes.iter().map(|&b| b as char).collect(),
                        AtomIdent::Freeform { mean, name } => format!("----:{mean}:{name}"),
                    };
                    *counts.entry(name).or_insert(0) += 1;
                }
            }
        }
        Kind::Flac | Kind::Vorbis | Kind::Opus => {}
    }
    Ok(counts)
}

fn format_label(file_type: FileType, path: &Path, snap: Option<&Snapshot>) -> String {
    match file_type {
        FileType::Mpeg => {
            if let Some((version, _)) = id3v2_info(path) {
                format!("MP3 (ID3v2.{version})")
            } else if snap.is_some_and(|s| s.tag_types.contains(&TagType::Id3v1)) {
                "MP3 (ID3v1)".to_string()
            } else {
                "MP3".to_string()
            }
        }
        FileType::Mp4 => {
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase);
            match extension.as_deref() {
                Some("m4b") => "M4B",
                Some("m4a") => "M4A",
                _ => "MP4",
            }
            .to_string()
        }
        FileType::Flac => "FLAC".to_string(),
        FileType::Vorbis => "Ogg Vorbis".to_string(),
        FileType::Opus => "Ogg Opus".to_string(),
        FileType::Speex => "Ogg Speex".to_string(),
        FileType::Wav => "WAV".to_string(),
        FileType::Aiff => "AIFF".to_string(),
        FileType::Aac => "AAC".to_string(),
        FileType::Ape => "Monkey's Audio".to_string(),
        FileType::Mpc => "Musepack".to_string(),
        FileType::WavPack => "WavPack".to_string(),
        _ => "Audio".to_string(),
    }
}

fn read_editable_inner(path: &Path) -> Result<EditableTags, String> {
    let meta = fs::metadata(path).map_err(|error| format!("Cannot read the file: {error}"))?;
    if !meta.is_file() {
        return Err("This is not a regular file".to_string());
    }
    let mut out = EditableTags::default();
    let Some(file_type) = detect_type(path)? else {
        out.format = "Unknown".to_string();
        out.read_only_reason = Some("This file's format is not recognised".to_string());
        return Ok(out);
    };
    out.format = format_label(file_type, path, None);
    let snap = match snapshot(path, file_type) {
        Ok(snap) => snap,
        Err(error) => {
            out.read_only_reason = Some(format!("The tags could not be read ({error})"));
            return Ok(out);
        }
    };
    out.format = format_label(file_type, path, Some(&snap));
    let v = &snap.values;
    out.title = v[slot(Field::Title)].clone();
    out.artist = v[slot(Field::Artist)].clone();
    out.album = v[slot(Field::Album)].clone();
    out.album_artist = v[slot(Field::AlbumArtist)].clone();
    out.composer = v[slot(Field::Composer)].clone();
    out.genre = v[slot(Field::Genre)].clone();
    out.year = v[slot(Field::Year)].clone();
    out.comment = v[slot(Field::Comment)].clone();
    out.track = v[slot(Field::Track)].clone();
    out.track_total = v[slot(Field::TrackTotal)].clone();
    out.disc = v[slot(Field::Disc)].clone();
    out.disc_total = v[slot(Field::DiscTotal)].clone();
    out.artwork_count = snap.pictures.len();
    out.has_artwork = out.artwork_count > 0;
    match write_blocker(path, &meta, file_type) {
        Ok(_) => out.writable = true,
        Err(reason) => out.read_only_reason = Some(reason),
    }
    Ok(out)
}

/// Why a file cannot be written, or the kind of writer to use.
fn write_blocker(path: &Path, meta: &fs::Metadata, file_type: FileType) -> Result<Kind, String> {
    let Some(kind) = kind_for(file_type) else {
        return Err(format!(
            "Editing tags in {} files is not supported",
            format_label(file_type, path, None)
        ));
    };
    if meta.permissions().readonly() {
        return Err("The file is read-only; clear its read-only flag to edit it".to_string());
    }
    if !within_size_limit(meta.len()) {
        return Err("The file is larger than 1 GiB, too large to edit safely".to_string());
    }
    let file = File::open(path).map_err(|error| format!("Cannot open the file: {error}"))?;
    if link_count(&file, meta).is_some_and(|links| links > 1) {
        return Err(
            "The file has several hard links; editing it would split them apart".to_string(),
        );
    }
    drop(file);
    if kind == Kind::Mp4 {
        mp4_preflight(path)?;
    }
    Ok(kind)
}

fn within_size_limit(len: u64) -> bool {
    len <= MAX_FILE_BYTES
}

#[cfg(unix)]
fn link_count(_file: &File, meta: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.nlink())
}

#[cfg(windows)]
fn link_count(file: &File, _meta: &fs::Metadata) -> Option<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    // SAFETY: the handle is valid for the lifetime of `file`, and the all-zero
    // structure is a valid output buffer for the call.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) == 0 {
            return None;
        }
        Some(u64::from(info.nNumberOfLinks))
    }
}

#[cfg(not(any(unix, windows)))]
fn link_count(_file: &File, _meta: &fs::Metadata) -> Option<u64> {
    None
}

// ---------------------------------------------------------------------------
// ID3v2 and FLAC layout helpers
// ---------------------------------------------------------------------------

/// Version and total length (header, body and footer) of a leading ID3v2 tag.
fn id3v2_info(path: &Path) -> Option<(u8, u64)> {
    let mut file = File::open(path).ok()?;
    id3v2_info_of(&mut file)
}

fn id3v2_info_of(file: &mut File) -> Option<(u8, u64)> {
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut header = [0u8; 10];
    file.read_exact(&mut header).ok()?;
    if &header[0..3] != b"ID3" || header[3] == 0xFF || header[4] == 0xFF {
        return None;
    }
    if header[6..10].iter().any(|&byte| byte & 0x80 != 0) {
        return None;
    }
    let body = (u64::from(header[6]) << 21)
        | (u64::from(header[7]) << 14)
        | (u64::from(header[8]) << 7)
        | u64::from(header[9]);
    let footer = if header[3] == 4 && header[5] & 0x10 != 0 {
        10
    } else {
        0
    };
    Some((header[3], 10 + body + footer))
}

/// Offset of the first audio frame of a FLAC file (after any ID3v2 prefix and
/// all metadata blocks).
fn flac_audio_start(path: &Path) -> Result<u64, String> {
    let bad = || "The FLAC layout could not be checked".to_string();
    let mut file = File::open(path).map_err(|_| bad())?;
    let length = file.metadata().map_err(|_| bad())?.len();
    let mut position = id3v2_info_of(&mut file).map_or(0, |(_, total)| total);
    file.seek(SeekFrom::Start(position)).map_err(|_| bad())?;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).map_err(|_| bad())?;
    if &magic != b"fLaC" {
        return Err(bad());
    }
    position += 4;
    for _ in 0..MAX_FLAC_BLOCKS {
        let mut header = [0u8; 4];
        file.seek(SeekFrom::Start(position)).map_err(|_| bad())?;
        file.read_exact(&mut header).map_err(|_| bad())?;
        let size =
            (u64::from(header[1]) << 16) | (u64::from(header[2]) << 8) | u64::from(header[3]);
        position = position
            .checked_add(4 + size)
            .filter(|end| *end <= length)
            .ok_or_else(bad)?;
        if header[0] & 0x80 != 0 {
            return Ok(position);
        }
    }
    Err(bad())
}

/// Compares `len` bytes at `a_start` in one file with `b_start` in another.
fn ranges_equal(a: &Path, a_start: u64, b: &Path, b_start: u64, len: u64) -> io::Result<bool> {
    let mut fa = File::open(a)?;
    let mut fb = File::open(b)?;
    fa.seek(SeekFrom::Start(a_start))?;
    fb.seek(SeekFrom::Start(b_start))?;
    let mut buf_a = vec![0u8; 64 * 1024];
    let mut buf_b = vec![0u8; 64 * 1024];
    let mut left = len;
    while left > 0 {
        let chunk = usize::try_from(left.min(buf_a.len() as u64)).unwrap_or(buf_a.len());
        fa.read_exact(&mut buf_a[..chunk])?;
        fb.read_exact(&mut buf_b[..chunk])?;
        if buf_a[..chunk] != buf_b[..chunk] {
            return Ok(false);
        }
        left -= chunk as u64;
    }
    Ok(true)
}

fn file_len(path: &Path) -> Result<u64, String> {
    fs::metadata(path)
        .map(|meta| meta.len())
        .map_err(|error| format!("Cannot read the file: {error}"))
}

// ---------------------------------------------------------------------------
// MP4 layout
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Atom {
    kind: [u8; 4],
    start: u64,
    len: u64,
    header: u64,
}

#[derive(Default)]
struct MoovInfo {
    traks: usize,
    has_chpl: bool,
    has_mvex: bool,
    /// Every chunk offset of every track, in file order of the tables.
    offsets: Vec<u64>,
}

struct Mp4Layout {
    atoms: Vec<Atom>,
    moov: usize,
    info: MoovInfo,
}

fn mp4_unchecked() -> String {
    "The structure of this MP4 file could not be checked, so it was not edited".to_string()
}

fn read_top_level(path: &Path) -> Result<Vec<Atom>, String> {
    let length = file_len(path)?;
    let file = File::open(path).map_err(|error| format!("Cannot open the file: {error}"))?;
    let mut reader = BufReader::new(file);
    let mut atoms = Vec::new();
    let mut position = 0u64;
    while position < length {
        if atoms.len() >= MAX_TOP_ATOMS {
            return Err(mp4_unchecked());
        }
        reader
            .seek(SeekFrom::Start(position))
            .map_err(|_| mp4_unchecked())?;
        let mut head = [0u8; 8];
        reader.read_exact(&mut head).map_err(|_| mp4_unchecked())?;
        let size32 = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
        let kind = [head[4], head[5], head[6], head[7]];
        let (len, header) = match size32 {
            0 => (length - position, 8),
            1 => {
                let mut wide = [0u8; 8];
                reader.read_exact(&mut wide).map_err(|_| mp4_unchecked())?;
                (u64::from_be_bytes(wide), 16)
            }
            other => (other, 8),
        };
        if len < header || len > length - position {
            return Err(mp4_unchecked());
        }
        atoms.push(Atom {
            kind,
            start: position,
            len,
            header,
        });
        position += len;
    }
    Ok(atoms)
}

fn scan_boxes(buf: &[u8], depth: u32, info: &mut MoovInfo) -> Result<(), String> {
    if depth > MAX_ATOM_DEPTH {
        return Err(mp4_unchecked());
    }
    let mut position = 0usize;
    while position < buf.len() {
        let rest = &buf[position..];
        if rest.len() < 8 {
            // QuickTime pads some containers with a zero terminator.
            if rest.iter().all(|&byte| byte == 0) {
                break;
            }
            return Err(mp4_unchecked());
        }
        let size32 = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        let kind = [rest[4], rest[5], rest[6], rest[7]];
        let (len, header) = match size32 {
            0 => (rest.len(), 8),
            1 => {
                if rest.len() < 16 {
                    return Err(mp4_unchecked());
                }
                let mut wide = [0u8; 8];
                wide.copy_from_slice(&rest[8..16]);
                let len = usize::try_from(u64::from_be_bytes(wide)).map_err(|_| mp4_unchecked())?;
                (len, 16)
            }
            other => (other, 8),
        };
        if len < header || len > rest.len() {
            return Err(mp4_unchecked());
        }
        let body = &rest[header..len];
        match &kind {
            b"trak" => {
                info.traks += 1;
                scan_boxes(body, depth + 1, info)?;
            }
            b"mdia" | b"minf" | b"stbl" | b"udta" => scan_boxes(body, depth + 1, info)?,
            b"mvex" => info.has_mvex = true,
            b"chpl" => info.has_chpl = true,
            b"stco" => collect_offsets(body, 4, info)?,
            b"co64" => collect_offsets(body, 8, info)?,
            _ => {}
        }
        position += len;
    }
    Ok(())
}

fn collect_offsets(body: &[u8], width: usize, info: &mut MoovInfo) -> Result<(), String> {
    if body.len() < 8 {
        return Err(mp4_unchecked());
    }
    let count = u32::from_be_bytes([body[4], body[5], body[6], body[7]]) as usize;
    let needed = count.checked_mul(width).ok_or_else(mp4_unchecked)?;
    if needed > body.len() - 8 || info.offsets.len() + count > MAX_CHUNK_OFFSETS {
        return Err(mp4_unchecked());
    }
    for entry in body[8..8 + needed].chunks_exact(width) {
        let mut wide = [0u8; 8];
        wide[8 - width..].copy_from_slice(entry);
        info.offsets.push(u64::from_be_bytes(wide));
    }
    Ok(())
}

fn read_mp4_layout(path: &Path) -> Result<Mp4Layout, String> {
    let atoms = read_top_level(path)?;
    let mut moovs = atoms
        .iter()
        .enumerate()
        .filter(|(_, atom)| &atom.kind == b"moov");
    let (moov, atom) = moovs.next().ok_or_else(mp4_unchecked)?;
    if moovs.next().is_some() {
        return Err(mp4_unchecked());
    }
    let body_len = atom.len - atom.header;
    if body_len > MAX_MOOV_BYTES {
        return Err("The MP4 index is too large to edit safely".to_string());
    }
    let mut file = File::open(path).map_err(|error| format!("Cannot open the file: {error}"))?;
    file.seek(SeekFrom::Start(atom.start + atom.header))
        .map_err(|_| mp4_unchecked())?;
    let mut body = vec![0u8; usize::try_from(body_len).map_err(|_| mp4_unchecked())?];
    file.read_exact(&mut body).map_err(|_| mp4_unchecked())?;
    let mut info = MoovInfo::default();
    scan_boxes(&body, 0, &mut info)?;
    Ok(Mp4Layout { atoms, moov, info })
}

/// Rule for MP4-family files. Lofty 0.22.4 shifts every `stco`/`co64` entry of
/// every `trak` when the tag grows inside an existing `udta`/`meta`/`ilst`, but
/// it does not shift any of them when it has to create `udta` or `meta`, and
/// that cannot be told from outside. A file with several tracks (chapter text
/// tracks, video) or a `chpl` chapter list is therefore refused, and every
/// write to a single-track file is checked against its chunk offsets afterwards
/// (`verify_mp4_layout`).
fn mp4_preflight(path: &Path) -> Result<(), String> {
    let layout = read_mp4_layout(path)?;
    if layout.info.has_mvex {
        return Err("Editing tags in this MP4 is not supported (it is fragmented)".to_string());
    }
    if layout.info.traks > 1 || layout.info.has_chpl {
        return Err(
            "Editing tags in this M4B/M4A is not supported yet (it has chapters)".to_string(),
        );
    }
    Ok(())
}

/// Checks that the edited MP4 still describes the same audio: every top-level
/// atom except `moov` is byte-identical, and every chunk offset still points at
/// the same position inside the same atom.
fn verify_mp4_layout(original: &Path, copy: &Path) -> Result<(), String> {
    let wrong = |what: &str| what.to_string();
    let a = read_mp4_layout(original)?;
    let b = read_mp4_layout(copy)?;
    if a.atoms.len() != b.atoms.len() || a.moov != b.moov {
        return Err(wrong("the MP4 atoms changed"));
    }
    for (x, y) in a.atoms.iter().zip(&b.atoms) {
        if x.kind != y.kind || (&x.kind != b"moov" && x.len != y.len) {
            return Err(wrong("the MP4 atoms changed"));
        }
    }
    if a.info.traks != b.info.traks || a.info.offsets.len() != b.info.offsets.len() {
        return Err(wrong("the MP4 track index changed"));
    }
    for (old, new) in a.info.offsets.iter().zip(&b.info.offsets) {
        let index = a.atoms.partition_point(|atom| atom.start <= *old);
        let inside = index
            .checked_sub(1)
            .and_then(|i| a.atoms.get(i).map(|atom| (i, atom)))
            .filter(|(i, atom)| *old < atom.start + atom.len && *i != a.moov);
        let Some((i, atom)) = inside else {
            return Err(wrong("an MP4 chunk offset is outside the media data"));
        };
        let moved = &b.atoms[i];
        if *new < moved.start || *new - moved.start != *old - atom.start {
            return Err(wrong(
                "the MP4 index would no longer match the media data (files without an existing tag area are not supported yet)",
            ));
        }
    }
    for (x, y) in a.atoms.iter().zip(&b.atoms) {
        if &x.kind == b"moov" {
            continue;
        }
        match ranges_equal(original, x.start, copy, y.start, x.len) {
            Ok(true) => {}
            _ => return Err(wrong("the media data changed")),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The working copy; removed on drop unless it was moved over the original.
struct TempCopy {
    path: PathBuf,
    armed: bool,
}

impl TempCopy {
    /// Creates a new, empty working copy in `dir`; never overwrites a file.
    fn create(dir: &Path) -> Result<(Self, File), String> {
        for _ in 0..64 {
            let number = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = dir.join(format!("{TEMP_PREFIX}{}-{number}.tmp", std::process::id()));
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok((Self { path, armed: true }, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "Cannot create a temporary file next to the original: {error}"
                    ));
                }
            }
        }
        Err("Cannot create a temporary file next to the original".to_string())
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempCopy {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Identity of the original while the edit runs, to notice a concurrent change.
#[derive(PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
}

fn stamp(meta: &fs::Metadata) -> Stamp {
    Stamp {
        len: meta.len(),
        modified: meta.modified().ok(),
    }
}

fn write_inner(path: &Path, edit: &TagEdit) -> Result<(), String> {
    // 1. Validate the request before touching the disk.
    let valid = validate_edit(edit)?;

    // 2. Resolve a symlink to its real target; the target is what gets replaced.
    let link =
        fs::symlink_metadata(path).map_err(|error| format!("Cannot read the file: {error}"))?;
    let target: PathBuf = if link.file_type().is_symlink() {
        fs::canonicalize(path).map_err(|error| format!("Cannot resolve the link: {error}"))?
    } else {
        path.to_path_buf()
    };
    let meta = fs::metadata(&target).map_err(|error| format!("Cannot read the file: {error}"))?;
    if !meta.is_file() {
        return Err("This is not a regular file".to_string());
    }
    let dir = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let started = stamp(&meta);

    // 3. Judge the original: format, permissions, links, MP4 layout.
    let file_type =
        detect_type(&target)?.ok_or_else(|| "This file's format is not recognised".to_string())?;
    let kind = write_blocker(&target, &meta, file_type)?;
    let before = snapshot(&target, file_type)
        .map_err(|error| format!("The tags could not be read, so nothing was changed ({error})"))?;
    let before_raw = raw_counts(&target, kind)?;
    let id3_version = if kind == Kind::Mp3 {
        id3v2_info(&target).map(|(version, _)| version)
    } else {
        None
    };
    let mut plan = make_plan(valid, &before.values, before.pictures.len())?;
    if kind == Kind::Mp4 {
        // MP4 cover atoms only hold JPEG, PNG, BMP and GIF.
        if let PlannedArtwork::Replace(prepared) = &mut plan.artwork {
            if matches!(prepared.mime, MimeType::Unknown(_)) {
                *prepared = reencode_as_png(&prepared.data)?;
            }
        }
    }
    if kind == Kind::Mp3 {
        let early = plan.texts[slot(Field::Year)]
            .as_deref()
            .and_then(|text| text.parse::<u32>().ok())
            .is_some_and(|year| year < 1000);
        if early {
            return Err("ID3 tags cannot store a year before 1000".to_string());
        }
    }

    // 4. Work on a copy beside the original.
    let (mut temp, mut temp_file) = TempCopy::create(&dir)?;
    {
        let mut source =
            File::open(&target).map_err(|error| format!("Cannot open the file: {error}"))?;
        let copied = io::copy(&mut source, &mut temp_file)
            .map_err(|error| format!("Cannot copy the file: {error}"))?;
        if copied != meta.len() {
            return Err("The file changed while it was being copied; nothing was written".into());
        }
    }
    drop(temp_file);

    apply_edit(&temp.path, file_type, &plan, id3_version)?;
    sync_file(&temp.path)?;

    // 5. Verify the copy before the original is touched.
    verify_copy(&target, &temp.path, &before, &before_raw, &plan, kind)?;

    // 6. Replace the original; permissions kept, modified time is the new one.
    let now = fs::metadata(&target).map_err(|error| format!("Cannot read the file: {error}"))?;
    if stamp(&now) != started {
        return Err("The file changed while it was being edited; nothing was written".into());
    }
    fs::set_permissions(&temp.path, meta.permissions())
        .map_err(|error| format!("Cannot copy the file permissions: {error}"))?;
    replace_original(&temp.path, &target)?;
    temp.disarm();
    sync_directory(&dir);
    Ok(())
}

fn sync_file(path: &Path) -> Result<(), String> {
    OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("Cannot flush the edited copy to disk: {error}"))
}

#[cfg(unix)]
fn sync_directory(dir: &Path) {
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_directory(_dir: &Path) {}

fn replace_original(temp: &Path, target: &Path) -> Result<(), String> {
    let attempts = if cfg!(windows) {
        RENAME_ATTEMPTS_WINDOWS
    } else {
        1
    };
    let mut last = None;
    for attempt in 0..attempts {
        match fs::rename(temp, target) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let locked = is_sharing_error(&error);
                last = Some(error);
                if !locked {
                    break;
                }
                if attempt + 1 < attempts {
                    std::thread::sleep(RENAME_RETRY_DELAY);
                }
            }
        }
    }
    match last {
        Some(error) if is_sharing_error(&error) && cfg!(windows) => Err(IN_USE.to_string()),
        Some(error) => Err(format!("Cannot replace the original file: {error}")),
        None => Err("Cannot replace the original file".to_string()),
    }
}

/// A file another process has open: access denied or a sharing/lock violation.
fn is_sharing_error(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied
        || matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

/// Applies the plan to the working copy with Lofty. The existing primary tag is
/// read, modified and saved back; nothing is rebuilt from scratch.
fn apply_edit(
    copy: &Path,
    file_type: FileType,
    plan: &Plan,
    id3_version: Option<u8>,
) -> Result<(), String> {
    // ID3v2.3 is the most compatible default; an existing 2.4 tag stays 2.4.
    let mut options = WriteOptions::new();
    options.use_id3v23(id3_version != Some(4));
    if file_type == FileType::Mpeg {
        return apply_edit_id3v2(copy, plan, options);
    }
    let mut tagged = open_tagged(copy, file_type)?;
    let primary = tagged.primary_tag_type();
    let mut tag = match tagged.remove(primary) {
        Some(tag) => tag,
        None => match tagged.tags().first() {
            Some(first) => {
                let mut mapped = first.clone();
                mapped.re_map(primary);
                mapped
            }
            None => Tag::new(primary),
        },
    };
    apply_plan(&mut tag, plan)?;
    tag.save_to_path(copy, options)
        .map_err(|error| format!("The tag could not be written: {error}"))
}

/// MP3: the ID3v2 tag is split into the generic view plus a remainder that
/// holds every frame the generic view cannot show (chapters, private data,
/// ...), edited, merged back and saved as ID3v2. Going through the generic tag
/// alone would drop date frames when writing ID3v2.3.
fn apply_edit_id3v2(copy: &Path, plan: &Plan, options: WriteOptions) -> Result<(), String> {
    crate::media::validate_lofty_input(copy)?;
    let file = File::open(copy).map_err(|error| format!("Cannot open the file: {error}"))?;
    let mut reader = BufReader::new(file);
    let mut mpeg =
        MpegFile::read_from(&mut reader, parse_options()).map_err(|error| error.to_string())?;
    let (remainder, mut tag) = match mpeg.remove_id3v2() {
        Some(existing) => existing.split_tag(),
        None => {
            // No ID3v2 tag yet: start from an older tag when the file has one.
            let legacy = match (mpeg.id3v1(), mpeg.ape()) {
                (Some(v1), _) => Some(Tag::from(v1.clone())),
                (None, Some(ape)) => Some(Tag::from(ape.clone())),
                (None, None) => None,
            };
            let (remainder, mut tag) = Id3v2Tag::new().split_tag();
            if let Some(old) = legacy {
                for item in old.items() {
                    tag.insert(item.clone());
                }
                for picture in old.pictures() {
                    tag.push_picture(picture.clone());
                }
                let year = year_text(&old);
                if !year.is_empty() {
                    let _ = set_year(&mut tag, &year);
                }
            }
            (remainder, tag)
        }
    };
    apply_plan(&mut tag, plan)?;
    let mut merged = remainder.merge_tag(tag);
    // The frames were decoded when the tag was read and are written as they
    // are, so the tag must not claim to be unsynchronised: a reader would then
    // strip bytes from the cover image.
    let mut flags = *merged.flags();
    flags.unsynchronisation = false;
    merged.set_flags(flags);
    merged
        .save_to_path(copy, options)
        .map_err(|error| format!("The tag could not be written: {error}"))
}

fn apply_plan(tag: &mut Tag, plan: &Plan) -> Result<(), String> {
    let refuse = |field: Field| format!("This file format cannot store the {}", field.label());
    for field in Field::ALL {
        let Some(new) = &plan.texts[slot(field)] else {
            continue;
        };
        if let Some(key) = field.text_key() {
            if new.is_empty() {
                tag.remove_key(&key);
            } else if !tag.insert_text(key, new.clone()) {
                return Err(refuse(field));
            }
        }
    }
    if let Some(new) = &plan.texts[slot(Field::Year)] {
        set_year(tag, new).ok_or_else(|| refuse(Field::Year))?;
    }
    for (number, total, number_key, total_key) in [
        (
            Field::Track,
            Field::TrackTotal,
            ItemKey::TrackNumber,
            ItemKey::TrackTotal,
        ),
        (
            Field::Disc,
            Field::DiscTotal,
            ItemKey::DiscNumber,
            ItemKey::DiscTotal,
        ),
    ] {
        let (new_number, new_total) = (&plan.texts[slot(number)], &plan.texts[slot(total)]);
        if new_number.is_none() && new_total.is_none() {
            continue;
        }
        let (mut n, mut t) = number_pair(tag, &number_key, &total_key);
        if let Some(text) = new_number {
            n = text.clone();
        }
        if let Some(text) = new_total {
            t = text.clone();
        }
        tag.remove_key(&number_key);
        tag.remove_key(&total_key);
        if !n.is_empty() && !tag.insert_text(number_key, n) {
            return Err(refuse(number));
        }
        if !t.is_empty() && !tag.insert_text(total_key, t) {
            return Err(refuse(total));
        }
    }
    match &plan.artwork {
        PlannedArtwork::Keep => {}
        PlannedArtwork::Remove => remove_pictures(tag),
        PlannedArtwork::Replace(prepared) => {
            remove_pictures(tag);
            tag.push_picture(Picture::new_unchecked(
                PictureType::CoverFront,
                Some(prepared.mime.clone()),
                None,
                prepared.data.clone(),
            ));
        }
    }
    if tag.tag_type() == TagType::Id3v2 {
        give_pictures_a_description(tag);
    }
    Ok(())
}

/// Lofty writes a picture without a description as a one-byte terminator even
/// when ID3v2.3 forces UTF-16 text, which it then cannot read back. An empty
/// description is written correctly.
fn give_pictures_a_description(tag: &mut Tag) {
    let pictures: Vec<Picture> = tag.pictures().to_vec();
    for (index, mut picture) in pictures.into_iter().enumerate() {
        if picture.description().is_none() {
            picture.set_description(Some(String::new()));
            tag.set_picture(index, picture);
        }
    }
}

fn remove_pictures(tag: &mut Tag) {
    for _ in 0..tag.pictures().len() {
        tag.remove_picture(0);
    }
}

/// Sets the year, keeping the month and day of a stored full date.
fn set_year(tag: &mut Tag, year: &str) -> Option<()> {
    let previous_date = tag.get_string(&ItemKey::RecordingDate).map(str::to_owned);
    tag.remove_key(&ItemKey::Year);
    tag.remove_key(&ItemKey::RecordingDate);
    if year.is_empty() {
        return Some(());
    }
    let number = year.parse::<u32>().ok()?;
    let text = format!("{number:04}");
    if let Some(date) = previous_date {
        let rest = date.get(4..).unwrap_or("");
        return tag
            .insert_text(ItemKey::RecordingDate, format!("{text}{rest}"))
            .then_some(());
    }
    if tag.insert_text(ItemKey::Year, text.clone()) {
        return Some(());
    }
    tag.insert_text(ItemKey::RecordingDate, text).then_some(())
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

fn verify_copy(
    original: &Path,
    copy: &Path,
    before: &Snapshot,
    before_raw: &BTreeMap<String, usize>,
    plan: &Plan,
    kind: Kind,
) -> Result<(), String> {
    let wrong = |what: String| format!("Verification failed, nothing was changed: {what}");
    let after = snapshot(copy, before.file_type)
        .map_err(|error| wrong(format!("the edited copy cannot be read ({error})")))?;
    if after.file_type != before.file_type {
        return Err(wrong("the file type changed".to_string()));
    }

    // Audio properties.
    let (a, b) = (before.sig, after.sig);
    if a.duration_ms.abs_diff(b.duration_ms) > 1
        || a.sample_rate != b.sample_rate
        || a.channels != b.channels
    {
        return Err(wrong("the audio properties changed".to_string()));
    }

    // Edited and untouched fields.
    let expected = expected_values(&before.values, plan);
    for field in Field::ALL {
        if !same_value(field, &after.values[slot(field)], &expected[slot(field)]) {
            return Err(wrong(format!("the {} did not read back", field.label())));
        }
    }

    // Untouched items of the tag. The track and disc pairs are compared above
    // as numbers, because a writer may drop leading zeros ("07" becomes "7").
    let mut exempt_keys: Vec<String> = [
        ItemKey::TrackNumber,
        ItemKey::TrackTotal,
        ItemKey::DiscNumber,
        ItemKey::DiscTotal,
    ]
    .iter()
    .map(|key| format!("{key:?}"))
    .collect();
    let mut exempt_raw: Vec<&str> = Vec::new();
    for field in Field::ALL {
        if plan.texts[slot(field)].is_some() {
            exempt_keys.extend(field.item_keys().iter().map(|key| format!("{key:?}")));
            exempt_raw.extend(field.raw_names(kind));
        }
    }
    let untouched = |items: &[(String, BlobSig)]| -> Vec<(String, BlobSig)> {
        items
            .iter()
            .filter(|(key, _)| !exempt_keys.contains(key))
            .cloned()
            .collect()
    };
    // A tag converted from another type (ID3v1 to ID3v2) names some items
    // differently; the values above are the check for that case.
    if before.display_type == after.display_type
        && untouched(&before.items) != untouched(&after.items)
    {
        return Err(wrong("a tag item that was not edited changed".to_string()));
    }

    // Artwork.
    match &plan.artwork {
        PlannedArtwork::Keep => {
            if before.pictures != after.pictures {
                return Err(wrong("the artwork changed".to_string()));
            }
        }
        PlannedArtwork::Remove => {
            if !after.pictures.is_empty() {
                return Err(wrong("the artwork was not removed".to_string()));
            }
        }
        PlannedArtwork::Replace(prepared) => {
            // MP4 `covr` atoms store no picture type, so only the data is compared.
            let want = blob_sig(&prepared.data);
            if after.pictures.len() != 1 || after.pictures[0].1 != want {
                return Err(wrong("the new artwork did not read back".to_string()));
            }
        }
    }
    if !matches!(plan.artwork, PlannedArtwork::Keep) {
        exempt_raw.extend(artwork_raw_names(kind));
    }

    // Format-specific frames and atoms, such as ID3v2 chapter frames.
    // Nothing to preserve when the file had no tag of this type before.
    let after_raw = raw_counts(copy, kind).map_err(wrong)?;
    let names: Vec<&String> = before_raw.keys().chain(after_raw.keys()).collect();
    for name in names {
        if exempt_raw.contains(&name.as_str()) || before.display_type != after.display_type {
            continue;
        }
        if before_raw.get(name) != after_raw.get(name) {
            return Err(wrong(format!("the {name} data changed")));
        }
    }

    // The audio itself.
    verify_payload(original, copy, kind).map_err(wrong)
}

fn artwork_raw_names(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Mp3 => &["APIC"],
        Kind::Mp4 => &["covr"],
        Kind::Flac | Kind::Vorbis | Kind::Opus => &[],
    }
}

fn verify_payload(original: &Path, copy: &Path, kind: Kind) -> Result<(), String> {
    match kind {
        Kind::Mp3 => {
            let start_a = id3v2_info(original).map_or(0, |(_, total)| total);
            let start_b = id3v2_info(copy).map_or(0, |(_, total)| total);
            same_tail(original, start_a, copy, start_b)
        }
        Kind::Flac => {
            let start_a = flac_audio_start(original)?;
            let start_b = flac_audio_start(copy)?;
            same_tail(original, start_a, copy, start_b)
        }
        Kind::Mp4 => verify_mp4_layout(original, copy),
        // Ogg pages are rewritten around the changed header; the audio
        // properties and the tag read-back above are the checks for it.
        Kind::Vorbis => Ok(()),
        // The same Ogg rewrite, but the audio packets can be compared without
        // Lofty: see `ogg_audio`.
        Kind::Opus => {
            let before = ogg_audio(original, OPUS_HEADER_PACKETS)?;
            let after = ogg_audio(copy, OPUS_HEADER_PACKETS)?;
            if before == after {
                Ok(())
            } else {
                Err("the audio data changed".to_string())
            }
        }
    }
}

/// An Ogg Opus stream starts with two header packets (OpusHead, OpusTags).
const OPUS_HEADER_PACKETS: u64 = 2;

/// What an Ogg tag edit must leave alone, read with the page layer only (no tag
/// library): the audio packets and the position where the stream ends.
#[derive(Debug, PartialEq, Eq)]
struct OggAudio {
    /// Packets after the header packets.
    packets: u64,
    /// Hash over the length and bytes of each of them, in order. Their page
    /// boundaries, sequence numbers and checksums may change; they may not.
    hash: u64,
    /// Granule position of the last page that has one: where the audio ends.
    end_granule: u64,
    /// Whatever follows the last whole page (normally nothing).
    tail: BlobSig,
}

/// Walks the Ogg pages of `path`, skipping `header_packets` packets, and returns
/// the facts that identify the audio. Every page consumes at least 27 bytes of
/// the file, so the walk is bounded by its length; a page that is cut short or
/// not an Ogg page ends the walk and the rest counts as the tail.
fn ogg_audio(path: &Path, header_packets: u64) -> Result<OggAudio, String> {
    let bad = || "The Ogg layout could not be checked".to_string();
    let file = File::open(path).map_err(|_| bad())?;
    let length = file.metadata().map_err(|_| bad())?.len();
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut hasher = std::hash::DefaultHasher::new();
    let mut completed = 0u64;
    let mut open_len = 0u64;
    let mut end_granule = 0u64;
    let mut position = 0u64;
    let mut segment = [0u8; 255];
    while position < length {
        let mut head = [0u8; 27];
        if reader.read_exact(&mut head).is_err() || &head[..4] != b"OggS" || head[4] != 0 {
            break;
        }
        let count = usize::from(head[26]);
        let mut lacing = [0u8; 255];
        if reader.read_exact(&mut lacing[..count]).is_err() {
            break;
        }
        let body: u64 = lacing[..count].iter().map(|&len| u64::from(len)).sum();
        let end = position + 27 + count as u64 + body;
        if end > length {
            break;
        }
        for &len in &lacing[..count] {
            let len = usize::from(len);
            reader.read_exact(&mut segment[..len]).map_err(|_| bad())?;
            let audio = completed >= header_packets;
            if audio {
                hasher.write(&segment[..len]);
            }
            open_len += len as u64;
            if len < 255 {
                if audio {
                    hasher.write_u64(open_len);
                }
                completed += 1;
                open_len = 0;
            }
        }
        let granule = u64::from_le_bytes(head[6..14].try_into().map_err(|_| bad())?);
        if granule != u64::MAX {
            end_granule = granule;
        }
        position = end;
    }
    if open_len > 0 && completed >= header_packets {
        hasher.write_u64(open_len);
    }
    let mut tail_hasher = std::hash::DefaultHasher::new();
    let mut tail_len = 0usize;
    reader.seek(SeekFrom::Start(position)).map_err(|_| bad())?;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = reader.read(&mut chunk).map_err(|_| bad())?;
        if read == 0 {
            break;
        }
        tail_hasher.write(&chunk[..read]);
        tail_len += read;
    }
    if completed < header_packets {
        return Err(bad());
    }
    Ok(OggAudio {
        packets: completed - header_packets,
        hash: hasher.finish(),
        end_granule,
        tail: (tail_len, tail_hasher.finish()),
    })
}

fn same_tail(a: &Path, a_start: u64, b: &Path, b_start: u64) -> Result<(), String> {
    let len_a = file_len(a)?.checked_sub(a_start);
    let len_b = file_len(b)?.checked_sub(b_start);
    match (len_a, len_b) {
        (Some(x), Some(y)) if x == y => match ranges_equal(a, a_start, b, b_start, x) {
            Ok(true) => Ok(()),
            _ => Err("the audio data changed".to_string()),
        },
        _ => Err("the audio data changed size".to_string()),
    }
}
