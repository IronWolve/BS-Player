//! Cached library indexes and visible-file projections for the GUI.
//!
//! The cache owns an immutable copy of the library for a given revision. View
//! changes keep row indices into that catalog instead of cloning `Track`s.

use crate::{
    media::{Track, fold_text, natural_cmp},
    state::{BookProgress, Session},
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Index,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock, PoisonError},
    time::{Duration, Instant},
};

/// The file-list column table: the one place that names the columns, their
/// widths, labels, sortability and cell text. Header drawing, the Columns menu,
/// the sort selector, row cells, width handling and the Options page all read
/// from [`COLUMNS`].
///
/// The first [`LEGACY_COUNT`] entries keep the positions profiles saved before
/// the table existed (visibility flags, sort column and widths are stored by
/// position), so new columns are only ever appended.
pub mod columns {
    use crate::{media::Track, state::Progress};
    use std::borrow::Cow;

    /// Columns every profile already knows, in their saved order.
    pub const LEGACY_COUNT: usize = 6;
    pub const COUNT: usize = 28;

    pub const COL_INDEX: usize = 0;
    pub const COL_TITLE: usize = 1;
    pub const COL_ARTIST: usize = 2;
    pub const COL_DURATION: usize = 3;
    pub const COL_PROGRESS: usize = 4;
    pub const COL_ALBUM: usize = 6;
    pub const COL_YEAR: usize = 7;
    pub const COL_GENRE: usize = 8;
    pub const COL_RATING: usize = 9;
    pub const COL_PLAYS: usize = 10;
    pub const COL_LAST_PLAYED: usize = 11;
    pub const COL_ADDED: usize = 12;
    pub const COL_FORMAT: usize = 13;
    pub const COL_ALBUM_ARTIST: usize = 14;
    pub const COL_COMPOSER: usize = 15;
    pub const COL_DISC: usize = 16;
    pub const COL_NARRATOR: usize = 17;
    pub const COL_FILE_SIZE: usize = 18;
    pub const COL_SAMPLE_RATE: usize = 19;
    pub const COL_CHANNELS: usize = 20;
    pub const COL_TRACK_NUMBER: usize = 21;
    pub const COL_FILENAME: usize = 22;
    pub const COL_FOLDER: usize = 23;
    pub const COL_FULL_PATH: usize = 24;
    pub const COL_DATE_MODIFIED: usize = 25;
    pub const COL_COMMENT: usize = 26;
    pub const COL_FAVORITE: usize = 27;

    /// Longest relative date ("N days ago"); older dates print as YYYY-MM-DD.
    const RELATIVE_DAYS: u64 = 30;
    /// Dates past the year 9999 are corrupt data and show as an empty cell.
    const MAX_DATE_SECONDS: u64 = 253_402_300_799;
    const SECONDS_PER_DAY: u64 = 86_400;
    const STARS: [&str; 6] = ["", "★", "★★", "★★★", "★★★★", "★★★★★"];

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Align {
        Left,
        Right,
    }

    /// Everything a cell needs about one row, gathered once per visible row.
    pub struct CellInput<'a> {
        pub track: &'a Track,
        /// Zero-based position in the displayed list.
        pub row: usize,
        pub progress: Option<&'a Progress>,
        /// Star rating, 0 when unrated.
        pub rating: u8,
        /// Whether this file is in the current session's Favorites.
        pub favorite: bool,
        /// Current time as seconds since the Unix epoch, for relative dates.
        pub now: u64,
        /// Live playback position when this row is the loaded track.
        pub live_position: Option<f64>,
    }

    pub struct ColumnDef {
        /// Stable identifier. Profiles store the visibility of the columns after
        /// the first [`LEGACY_COUNT`] by this id; never rename one.
        pub id: &'static str,
        /// Header text and Columns menu entry.
        pub label: &'static str,
        /// Entry in the sort selector list.
        pub sort_label: &'static str,
        /// Short name in the sort selector's closed summary.
        pub short_label: &'static str,
        pub default_width: f32,
        pub min_width: f32,
        /// Share of the table width an untouched layout gives this column.
        pub fit_weight: f32,
        pub sortable: bool,
        pub align: Align,
        pub cell: for<'a> fn(&CellInput<'a>) -> Cow<'a, str>,
    }

    pub static COLUMNS: [ColumnDef; COUNT] = [
        ColumnDef {
            id: "index",
            label: "#",
            sort_label: "#",
            short_label: "#",
            default_width: 50.,
            min_width: 35.,
            fit_weight: 0.07,
            sortable: true,
            align: Align::Left,
            cell: cell_index,
        },
        ColumnDef {
            id: "title",
            label: "Title / filename",
            sort_label: "Title",
            short_label: "Title",
            default_width: 400.,
            min_width: 35.,
            fit_weight: 0.35,
            sortable: true,
            align: Align::Left,
            cell: cell_title,
        },
        ColumnDef {
            id: "artist",
            label: "Artist / author",
            sort_label: "Artist / author",
            short_label: "Artist",
            default_width: 180.,
            min_width: 35.,
            fit_weight: 0.18,
            sortable: true,
            align: Align::Left,
            cell: cell_artist,
        },
        ColumnDef {
            id: "duration",
            label: "Duration",
            sort_label: "Duration",
            short_label: "Duration",
            default_width: 100.,
            min_width: 35.,
            fit_weight: 0.13,
            sortable: true,
            align: Align::Left,
            cell: cell_duration,
        },
        ColumnDef {
            id: "progress",
            label: "Progress",
            sort_label: "Progress",
            short_label: "Progress",
            default_width: 150.,
            min_width: 35.,
            fit_weight: 0.15,
            sortable: true,
            align: Align::Left,
            cell: cell_progress,
        },
        ColumnDef {
            id: "bitrate",
            label: "Bitrate",
            sort_label: "Bitrate",
            short_label: "Bitrate",
            default_width: 100.,
            min_width: 35.,
            fit_weight: 0.12,
            sortable: true,
            align: Align::Left,
            cell: cell_bitrate,
        },
        ColumnDef {
            id: "album",
            label: "Album",
            sort_label: "Album",
            short_label: "Album",
            default_width: 180.,
            min_width: 60.,
            fit_weight: 0.16,
            sortable: true,
            align: Align::Left,
            cell: cell_album,
        },
        ColumnDef {
            id: "year",
            label: "Year",
            sort_label: "Year",
            short_label: "Year",
            default_width: 70.,
            min_width: 45.,
            fit_weight: 0.06,
            sortable: true,
            align: Align::Left,
            cell: cell_year,
        },
        ColumnDef {
            id: "genre",
            label: "Genre",
            sort_label: "Genre",
            short_label: "Genre",
            default_width: 120.,
            min_width: 60.,
            fit_weight: 0.10,
            sortable: true,
            align: Align::Left,
            cell: cell_genre,
        },
        ColumnDef {
            id: "rating",
            label: "Rating",
            sort_label: "Rating",
            short_label: "Rating",
            default_width: 100.,
            min_width: 60.,
            fit_weight: 0.09,
            sortable: true,
            align: Align::Left,
            cell: cell_rating,
        },
        ColumnDef {
            id: "plays",
            label: "Plays",
            sort_label: "Plays",
            short_label: "Plays",
            default_width: 70.,
            min_width: 45.,
            fit_weight: 0.06,
            sortable: true,
            align: Align::Right,
            cell: cell_plays,
        },
        ColumnDef {
            id: "last_played",
            label: "Last played",
            sort_label: "Last played",
            short_label: "Last played",
            default_width: 120.,
            min_width: 70.,
            fit_weight: 0.11,
            sortable: true,
            align: Align::Left,
            cell: cell_last_played,
        },
        ColumnDef {
            id: "date_added",
            label: "Date added",
            sort_label: "Date added",
            short_label: "Date added",
            default_width: 120.,
            min_width: 70.,
            fit_weight: 0.11,
            sortable: true,
            align: Align::Left,
            cell: cell_added,
        },
        ColumnDef {
            id: "format",
            label: "Format",
            sort_label: "Format",
            short_label: "Format",
            default_width: 80.,
            min_width: 50.,
            fit_weight: 0.07,
            sortable: true,
            align: Align::Left,
            cell: cell_format,
        },
        ColumnDef {
            id: "album_artist",
            label: "Album artist",
            sort_label: "Album artist",
            short_label: "Album artist",
            default_width: 180.,
            min_width: 60.,
            fit_weight: 0.16,
            sortable: true,
            align: Align::Left,
            cell: cell_album_artist,
        },
        ColumnDef {
            id: "composer",
            label: "Composer",
            sort_label: "Composer",
            short_label: "Composer",
            default_width: 160.,
            min_width: 60.,
            fit_weight: 0.14,
            sortable: true,
            align: Align::Left,
            cell: cell_composer,
        },
        ColumnDef {
            id: "disc",
            label: "Disc",
            sort_label: "Disc",
            short_label: "Disc",
            default_width: 60.,
            min_width: 45.,
            fit_weight: 0.05,
            sortable: true,
            align: Align::Right,
            cell: cell_disc,
        },
        ColumnDef {
            id: "narrator",
            label: "Narrator",
            sort_label: "Narrator",
            short_label: "Narrator",
            default_width: 160.,
            min_width: 60.,
            fit_weight: 0.14,
            sortable: true,
            align: Align::Left,
            cell: cell_narrator,
        },
        ColumnDef {
            id: "file_size",
            label: "File size",
            sort_label: "File size",
            short_label: "Size",
            default_width: 100., min_width: 55., fit_weight: 0.09,
            sortable: true, align: Align::Right, cell: cell_file_size,
        },
        ColumnDef {
            id: "sample_rate",
            label: "Sample rate",
            sort_label: "Sample rate",
            short_label: "Rate",
            default_width: 110., min_width: 60., fit_weight: 0.10,
            sortable: true, align: Align::Right, cell: cell_sample_rate,
        },
        ColumnDef {
            id: "channels",
            label: "Channels",
            sort_label: "Channels",
            short_label: "Channels",
            default_width: 100., min_width: 60., fit_weight: 0.09,
            sortable: true, align: Align::Right, cell: cell_channels,
        },
        ColumnDef {
            id: "track_number",
            label: "Track number",
            sort_label: "Track number",
            short_label: "Track #",
            default_width: 100., min_width: 60., fit_weight: 0.09,
            sortable: true, align: Align::Right, cell: cell_track_number,
        },
        ColumnDef {
            id: "filename",
            label: "Filename",
            sort_label: "Filename",
            short_label: "Filename",
            default_width: 180., min_width: 70., fit_weight: 0.15,
            sortable: true, align: Align::Left, cell: cell_filename,
        },
        ColumnDef {
            id: "folder",
            label: "Folder",
            sort_label: "Folder",
            short_label: "Folder",
            default_width: 180., min_width: 70., fit_weight: 0.15,
            sortable: true, align: Align::Left, cell: cell_folder,
        },
        ColumnDef {
            id: "full_path",
            label: "Full path",
            sort_label: "Full path",
            short_label: "Path",
            default_width: 260., min_width: 90., fit_weight: 0.22,
            sortable: true, align: Align::Left, cell: cell_full_path,
        },
        ColumnDef {
            id: "date_modified",
            label: "Date modified",
            sort_label: "Date modified",
            short_label: "Modified",
            default_width: 120., min_width: 70., fit_weight: 0.11,
            sortable: true, align: Align::Left, cell: cell_modified,
        },
        ColumnDef {
            id: "comment",
            label: "Comment",
            sort_label: "Comment",
            short_label: "Comment",
            default_width: 180., min_width: 70., fit_weight: 0.15,
            sortable: true, align: Align::Left, cell: cell_comment,
        },
        ColumnDef {
            id: "favorite",
            label: "Favorite",
            sort_label: "Favorite",
            short_label: "Favorite",
            default_width: 80., min_width: 50., fit_weight: 0.07,
            sortable: true, align: Align::Left, cell: cell_favorite,
        },
    ];

    /// Table position of the column with this id, if any.
    pub fn index_of(id: &str) -> Option<usize> {
        COLUMNS.iter().position(|column| column.id == id)
    }

    /// Saved widths of an untouched layout.
    pub fn default_widths() -> [f32; COUNT] {
        std::array::from_fn(|index| COLUMNS[index].default_width)
    }

    /// Repair saved widths: missing and non-finite entries take the column's
    /// default (profiles saved before newer columns existed are shorter), the
    /// rest are clamped to the column's range.
    pub fn normalize_widths(widths: &mut Vec<f32>) {
        widths.truncate(COUNT);
        for (index, column) in COLUMNS.iter().enumerate() {
            match widths.get_mut(index) {
                Some(width) => {
                    *width = if width.is_finite() {
                        width.clamp(column.min_width, 900.)
                    } else {
                        column.default_width
                    };
                }
                None => widths.push(column.default_width),
            }
        }
    }

    /// Leading digits of a year tag as a number; 0 when there is none.
    pub fn year_value(year: &str) -> u64 {
        year.trim()
            .bytes()
            .take_while(u8::is_ascii_digit)
            .take(9)
            .fold(0u64, |value, digit| value * 10 + u64::from(digit - b'0'))
    }

    /// File extension, the format shown in the Format column.
    pub fn format_name(track: &Track) -> &str {
        track
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
    }

    /// Calendar date of a day count since 1970-01-01 as (year, month, day).
    fn civil_date(days: u64) -> (i64, u32, u32) {
        // Days are at most u64::MAX / 86_400, far inside i64; every term below
        // stays small. Proleptic Gregorian calendar (Hinnant's algorithm).
        let z = days as i64 + 719_468;
        let era = z.div_euclid(146_097);
        let day_of_era = z.rem_euclid(146_097);
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        } as u32;
        let year = year_of_era + era * 400 + i64::from(month <= 2);
        (year, month, day)
    }

    /// A stored epoch-seconds date as cell text, in UTC: "Today", "Yesterday"
    /// and "N days ago" up to 30 days back, otherwise YYYY-MM-DD. Zero (never)
    /// and impossible values give an empty cell.
    pub fn date_text(epoch: u64, now: u64) -> Cow<'static, str> {
        if epoch == 0 || epoch > MAX_DATE_SECONDS {
            return Cow::Borrowed("");
        }
        let day = epoch / SECONDS_PER_DAY;
        let today = now / SECONDS_PER_DAY;
        if day <= today {
            match today - day {
                0 => return Cow::Borrowed("Today"),
                1 => return Cow::Borrowed("Yesterday"),
                age if age <= RELATIVE_DAYS => return Cow::Owned(format!("{age} days ago")),
                _ => {}
            }
        }
        let (year, month, day) = civil_date(day);
        Cow::Owned(format!("{year:04}-{month:02}-{day:02}"))
    }

    fn cell_index<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        Cow::Owned((input.row + 1).to_string())
    }

    fn cell_title<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        Cow::Borrowed(input.track.display_title())
    }

    fn cell_artist<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        Cow::Borrowed(input.track.artist.as_str())
    }

    fn cell_duration<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        Cow::Owned(crate::app::time_text(input.track.duration))
    }

    fn cell_progress<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        let position = |seconds: f64| -> Cow<'a, str> {
            Cow::Owned(format!(
                "{} / {}",
                crate::app::time_text(seconds),
                crate::app::time_text(input.track.duration)
            ))
        };
        if let Some(seconds) = input.live_position {
            return position(seconds);
        }
        match input.progress {
            Some(progress) if progress.completed => Cow::Borrowed("Finished"),
            Some(progress) if progress.seconds > 0. => position(progress.seconds),
            _ => Cow::Borrowed("Not started"),
        }
    }

    fn cell_bitrate<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        if input.track.bitrate > 0 {
            Cow::Owned(format!("{} kbps", input.track.bitrate))
        } else {
            Cow::Borrowed("—")
        }
    }

    fn cell_album<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        Cow::Borrowed(input.track.album.as_str())
    }

    fn cell_year<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        // An unset year is stored empty or as zero.
        let year = input.track.year.trim();
        if year.bytes().all(|byte| byte == b'0') {
            Cow::Borrowed("")
        } else {
            Cow::Borrowed(year)
        }
    }

    fn cell_genre<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        Cow::Borrowed(input.track.genre.as_str())
    }

    fn cell_rating<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        Cow::Borrowed(STARS[usize::from(input.rating).min(STARS.len() - 1)])
    }

    fn cell_plays<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        match input.progress.map(|progress| progress.play_count) {
            Some(count) if count > 0 => Cow::Owned(count.to_string()),
            _ => Cow::Borrowed(""),
        }
    }

    fn cell_last_played<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        date_text(
            input.progress.map_or(0, |progress| progress.last_played),
            input.now,
        )
    }

    fn cell_added<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        date_text(input.track.added_at, input.now)
    }

    fn cell_format<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        Cow::Owned(format_name(input.track).to_ascii_uppercase())
    }

    fn cell_album_artist<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        Cow::Borrowed(input.track.album_artist.as_str())
    }

    fn cell_composer<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        Cow::Borrowed(input.track.composer.as_str())
    }

    fn cell_disc<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        match input.track.disc {
            0 => Cow::Borrowed(""),
            disc => Cow::Owned(disc.to_string()),
        }
    }

    fn cell_narrator<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        Cow::Borrowed(input.track.narrator.as_str())
    }

    fn cell_file_size<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        let bytes = input.track.file_size;
        if bytes == 0 {
            return Cow::Borrowed("");
        }
        const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
        if bytes < 1024 {
            return Cow::Owned(format!("{bytes} B"));
        }
        let mut size = bytes as f64;
        let mut unit = 0;
        while size >= 1024. && unit < UNITS.len() - 1 {
            size /= 1024.;
            unit += 1;
        }
        Cow::Owned(format!("{size:.1} {}", UNITS[unit]))
    }

    fn cell_sample_rate<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        let rate = input.track.sample_rate;
        if rate == 0 {
            Cow::Borrowed("")
        } else if rate % 1000 == 0 {
            Cow::Owned(format!("{} kHz", rate / 1000))
        } else {
            Cow::Owned(format!("{:.1} kHz", rate as f64 / 1000.))
        }
    }

    fn cell_channels<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        match input.track.channels {
            0 => Cow::Borrowed(""),
            1 => Cow::Borrowed("Mono"),
            2 => Cow::Borrowed("Stereo"),
            count => Cow::Owned(format!("{count} channels")),
        }
    }

    fn cell_track_number<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        match input.track.track_number {
            0 => Cow::Borrowed(""),
            number => Cow::Owned(number.to_string()),
        }
    }

    fn cell_filename<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        input.track.path.file_name()
            .map(|name| Cow::Owned(name.to_string_lossy().into_owned()))
            .unwrap_or(Cow::Borrowed(""))
    }

    fn cell_folder<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        input.track.path.parent()
            .map(|folder| Cow::Owned(folder.to_string_lossy().into_owned()))
            .unwrap_or(Cow::Borrowed(""))
    }

    fn cell_full_path<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        Cow::Owned(input.track.path.to_string_lossy().into_owned())
    }

    fn cell_modified<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        date_text(input.track.modified, input.now)
    }

    fn cell_comment<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        Cow::Borrowed(input.track.comment.as_str())
    }

    fn cell_favorite<'a>(input: &CellInput<'a>) -> Cow<'a, str> {
        if input.favorite {
            Cow::Borrowed("★")
        } else {
            Cow::Borrowed("")
        }
    }
}

#[derive(Clone, Debug)]
pub struct LibraryCatalog {
    tracks: Arc<Vec<Track>>,
    by_id: HashMap<String, usize>,
    book_keys: Vec<String>,
    search_text: Vec<String>,
    path_sort_keys: Vec<String>,
    book_labels: HashMap<String, String>,
    /// Natural-sorted unique audiobook keys and their original display labels.
    pub books: Vec<(String, String)>,
    /// Position of each key of `books`.
    book_lookup: HashMap<String, usize>,
    /// For each entry of `books`: the indices of its current files (earlier
    /// versions of a changed file are left out), in library order.
    book_members: Vec<Vec<usize>>,
    /// For each entry of `books`: the author the BOOKS list files it under (the
    /// album artist, else the track artist), empty when the files name none.
    book_authors: Vec<String>,
    author_groups: Vec<AuthorGroup>,
    /// The Artists / Albums / Genres groupings of the music, made on first use
    /// (see `LibraryCatalog::browse`) and kept as long as this catalog lives.
    browse: OnceLock<BrowseIndex>,
}

impl LibraryCatalog {
    fn new(source: &Arc<Vec<Track>>) -> Self {
        let tracks = Arc::clone(source);
        let mut by_id = HashMap::with_capacity(tracks.len());
        let mut book_keys = Vec::with_capacity(tracks.len());
        let mut search_text = Vec::with_capacity(tracks.len());
        let mut path_sort_keys = Vec::with_capacity(tracks.len());
        let mut book_labels = HashMap::new();
        let mut audiobook_keys = HashSet::new();

        for (index, track) in tracks.iter().enumerate() {
            // Session::track returns the first matching row; preserve that when
            // malformed or legacy profiles contain duplicate IDs.
            by_id.entry(track.id.clone()).or_insert(index);
            let book_key = track.book_key();
            book_labels
                .entry(book_key.clone())
                .or_insert_with(|| track.book_label());
            if track.audiobook {
                audiobook_keys.insert(book_key.clone());
            }
            book_keys.push(book_key);
            search_text.push(fold_text(&format!(
                "{} {} {} {} {} {} {}",
                track.title,
                track.artist,
                track.album,
                track.genre,
                track.path.display(),
                track.album_artist,
                track.narrator
            )));
            path_sort_keys.push(track.path.to_string_lossy().into_owned());
        }

        // One entry per unique book, ordered by the label the sidebar shows
        // (the key breaks ties, e.g. two folders with the same name).
        let mut books: Vec<(String, String)> = audiobook_keys
            .into_iter()
            .map(|key| {
                let label = book_labels
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| key.clone());
                (key, label)
            })
            .collect();
        books.sort_by(|a, b| natural_cmp(&a.1, &b.1).then_with(|| natural_cmp(&a.0, &b.0)));

        // The files and the author of every book, found in one pass.
        let book_lookup: HashMap<String, usize> = books
            .iter()
            .enumerate()
            .map(|(position, (key, _))| (key.clone(), position))
            .collect();
        let mut book_members = vec![Vec::new(); books.len()];
        let mut book_authors = vec![String::new(); books.len()];
        for (index, track) in tracks.iter().enumerate() {
            if track.superseded {
                continue;
            }
            if let Some(&position) = book_lookup.get(&book_keys[index]) {
                book_members[position].push(index);
                if book_authors[position].is_empty() {
                    book_authors[position] = author_name(track).to_owned();
                }
            }
        }
        let author_groups = group_by_author(book_authors.iter().map(String::as_str));

        Self {
            tracks,
            by_id,
            book_keys,
            search_text,
            path_sort_keys,
            book_labels,
            books,
            book_lookup,
            book_members,
            book_authors,
            author_groups,
            browse: OnceLock::new(),
        }
    }

    pub fn track(&self, id: &str) -> Option<&Track> {
        self.by_id.get(id).map(|&index| &self.tracks[index])
    }

    /// Position in `books` of the book with this key.
    pub fn book_position(&self, key: &str) -> Option<usize> {
        self.book_lookup.get(key).copied()
    }

    /// Position in `books` of the book that the file with this id belongs to
    /// (none for a file outside every audiobook).
    pub fn book_position_of(&self, id: &str) -> Option<usize> {
        let &index = self.by_id.get(id)?;
        self.book_lookup.get(&self.book_keys[index]).copied()
    }

    /// The current files of the book at `position` in `books`, in library order.
    pub fn book_files(&self, position: usize) -> impl Iterator<Item = &Track> + '_ {
        self.book_members
            .get(position)
            .into_iter()
            .flatten()
            .map(|&index| &self.tracks[index])
    }

    /// The author of the book at `position` in `books` ("" when unknown).
    pub fn book_author(&self, position: usize) -> &str {
        self.book_authors.get(position).map_or("", String::as_str)
    }

    /// The books grouped under their authors (see `use_author_groups`).
    pub fn author_groups(&self) -> &[AuthorGroup] {
        &self.author_groups
    }

    /// The music grouped by artist, album and genre. Built in one pass the
    /// first time a browse list or drill-down asks, then shared by every view
    /// of this library revision.
    pub fn browse(&self) -> &BrowseIndex {
        self.browse.get_or_init(|| BrowseIndex::build(&self.tracks))
    }

    /// The files of a browse group in the order its drill-down lists them
    /// (the default `#` sort), as positions in the library.
    fn group_files(&self, kind: BrowseKind, group: &BrowseGroup) -> Vec<usize> {
        let mut files = group.members.clone();
        if kind == BrowseKind::Genres {
            files.sort_by(|&a, &b| self.book_order(a, b));
        } else {
            files.sort_by(|&a, &b| self.album_order(a, b));
        }
        files
    }

    /// The ids of the files of a browse group, in drill-down order: what a
    /// Play on its row queues. Empty when the group is gone.
    pub fn group_file_ids(&self, kind: BrowseKind, key: &str) -> Vec<String> {
        let Some(group) = self.browse().group(kind, key) else {
            return Vec::new();
        };
        self.group_files(kind, group)
            .into_iter()
            .map(|index| self.tracks[index].id.clone())
            .collect()
    }

    /// The default `#` order of two files: by book, then track number, then path.
    fn book_order(&self, a: usize, b: usize) -> std::cmp::Ordering {
        natural_cmp(&self.book_keys[a], &self.book_keys[b])
            .then_with(|| {
                self.tracks[a]
                    .track_number
                    .cmp(&self.tracks[b].track_number)
            })
            .then_with(|| natural_cmp(&self.path_sort_keys[a], &self.path_sort_keys[b]))
    }

    /// The order of an artist's or an album's files: album (files without one
    /// last), then disc (none counts as the first), then track number, then path.
    fn album_order(&self, a: usize, b: usize) -> std::cmp::Ordering {
        let (x, y) = (&self.tracks[a], &self.tracks[b]);
        let (x_album, y_album) = (x.album.trim(), y.album.trim());
        empty_last(x_album.is_empty(), y_album.is_empty())
            .unwrap_or_else(|| natural_cmp(x_album, y_album))
            .then_with(|| x.disc.max(1).cmp(&y.disc.max(1)))
            .then_with(|| x.track_number.cmp(&y.track_number))
            .then_with(|| natural_cmp(&self.path_sort_keys[a], &self.path_sort_keys[b]))
    }

    fn indices(&self) -> std::ops::Range<usize> {
        0..self.tracks.len()
    }
}

/// The author a book is filed under for a file: the album artist when the
/// file names one, else the track artist (the author in audiobook tags).
fn author_name(track: &Track) -> &str {
    let album_artist = track.album_artist.trim();
    if album_artist.is_empty() {
        track.artist.trim()
    } else {
        album_artist
    }
}

/// The BOOKS list gets author headings only above this many books.
pub const MIN_BOOKS_FOR_AUTHORS: usize = 8;

/// Whether the BOOKS list shows author headings: switched on and more than
/// `MIN_BOOKS_FOR_AUTHORS` books.
pub fn use_author_groups(book_count: usize, enabled: bool) -> bool {
    enabled && book_count > MIN_BOOKS_FOR_AUTHORS
}

/// The books of one author heading.
#[derive(Clone, Debug, PartialEq)]
pub struct AuthorGroup {
    /// The folded author name that the heading is remembered by when collapsed;
    /// empty for books that name no author.
    pub key: String,
    /// The author as the first of the books spells it ("Unknown author" is
    /// drawn for an empty one).
    pub label: String,
    /// Positions in `LibraryCatalog::books`, in that list's order.
    pub books: Vec<usize>,
}

/// Groups books under their authors. `authors` holds one author text per book,
/// in the order of the book list. Authors that differ only in case or accents
/// are one; the headings are in natural order with books without an author last.
pub fn group_by_author<'a>(authors: impl Iterator<Item = &'a str>) -> Vec<AuthorGroup> {
    let mut groups: Vec<AuthorGroup> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    for (position, author) in authors.enumerate() {
        let author = author.trim();
        let key = fold_text(author);
        let group = *by_key.entry(key.clone()).or_insert_with(|| {
            groups.push(AuthorGroup {
                key,
                label: author.to_owned(),
                books: Vec::new(),
            });
            groups.len() - 1
        });
        groups[group].books.push(position);
    }
    groups.sort_by(|a, b| {
        a.key
            .is_empty()
            .cmp(&b.key.is_empty())
            .then_with(|| natural_cmp(&a.label, &b.label))
            .then_with(|| a.key.cmp(&b.key))
    });
    groups
}

/// One line of the BOOKS list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookRow {
    /// The heading of the author group at this index.
    Author(usize),
    /// The book at this position of `LibraryCatalog::books`.
    Book(usize),
}

/// The lines of the BOOKS list. Without `groups` (few books, or grouping off)
/// they are the books alone; with them, each author heading is followed by its
/// books unless its key is in `collapsed`.
pub fn book_rows(
    book_count: usize,
    groups: Option<&[AuthorGroup]>,
    collapsed: &[String],
) -> Vec<BookRow> {
    let Some(groups) = groups else {
        return (0..book_count).map(BookRow::Book).collect();
    };
    let collapsed: HashSet<&str> = collapsed.iter().map(String::as_str).collect();
    let mut rows = Vec::with_capacity(groups.len() + book_count);
    for (index, group) in groups.iter().enumerate() {
        rows.push(BookRow::Author(index));
        if !collapsed.contains(group.key.as_str()) {
            rows.extend(group.books.iter().map(|&book| BookRow::Book(book)));
        }
    }
    rows
}
/// What the browse lists group the music by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BrowseKind {
    Artists = 0,
    Albums = 1,
    Genres = 2,
}

impl BrowseKind {
    pub const ALL: [BrowseKind; 3] = [Self::Artists, Self::Albums, Self::Genres];

    /// The sidebar entry, the section and the title of the browse list.
    pub const fn list_section(self) -> &'static str {
        match self {
            Self::Artists => "Artists",
            Self::Albums => "Albums",
            Self::Genres => "Genres",
        }
    }

    /// The start of the sections that drill into one group of this kind.
    pub const fn group_prefix(self) -> &'static str {
        match self {
            Self::Artists => "Artist:",
            Self::Albums => "Album:",
            Self::Genres => "Genre:",
        }
    }

    /// What stands for a group whose tag is empty.
    pub const fn unknown(self) -> &'static str {
        match self {
            Self::Artists => "Unknown artist",
            Self::Albums => "Unknown album",
            Self::Genres => "Unknown genre",
        }
    }

    /// One group of this kind, as a word.
    pub const fn noun(self) -> &'static str {
        match self {
            Self::Artists => "artist",
            Self::Albums => "album",
            Self::Genres => "genre",
        }
    }

    const fn slot(self) -> usize {
        self as usize
    }
}

/// Where a section stands in the browse lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowseSection<'a> {
    /// The Artists, Albums or Genres list itself.
    List(BrowseKind),
    /// The files of one group, by the folded key the group is remembered by.
    Group(BrowseKind, &'a str),
}

/// Reads a `Session::section` as a browse section (`None` for every other).
pub fn browse_section(section: &str) -> Option<BrowseSection<'_>> {
    BrowseKind::ALL.into_iter().find_map(|kind| {
        if section == kind.list_section() {
            Some(BrowseSection::List(kind))
        } else {
            section
                .strip_prefix(kind.group_prefix())
                .map(|key| BrowseSection::Group(kind, key))
        }
    })
}

/// The section that shows the files of the group with this key.
pub fn group_section(kind: BrowseKind, key: &str) -> String {
    format!("{}{key}", kind.group_prefix())
}

/// A group key longer than this is not one any file spells (a tag is read to at
/// most 512 characters and folding doubles a few letters); the profile loader
/// treats such a section as damage.
pub const MAX_BROWSE_KEY_CHARS: usize = 4200;

/// Between the folded artist and the folded album in the key of an album. The
/// unit separator cannot occur in a real tag.
const KEY_SEPARATOR: char = '\u{1f}';

/// Most genres one file is listed under.
pub const MAX_GENRES_PER_TRACK: usize = 4;

/// The genres a genre tag names, each with its folded key, in tag order and
/// without repeats (case and accents do not make a new genre): at most
/// `MAX_GENRES_PER_TRACK`. The tag is split on `;` and `/` only when it clearly
/// lists several genres, that is when it has at least two parts of two
/// characters or more ("Rock; Pop", "Hip-Hop/Rap"); "N/A" stays whole, and a
/// stray separator ("Rock;") is dropped. An empty tag names no genre.
fn genre_parts(tag: &str) -> Vec<(String, &str)> {
    let tag = tag.trim();
    if tag.is_empty() {
        return Vec::new();
    }
    if !tag.contains([';', '/']) {
        return vec![(fold_text(tag), tag)];
    }
    let parts: Vec<&str> = tag
        .split([';', '/'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    match parts.as_slice() {
        [] => return Vec::new(),
        [only] => return vec![(fold_text(only), only)],
        _ => {}
    }
    if parts.iter().any(|part| part.chars().count() < 2) {
        return vec![(fold_text(tag), tag)];
    }
    let mut found: Vec<(String, &str)> = Vec::with_capacity(MAX_GENRES_PER_TRACK);
    for part in parts {
        let key = fold_text(part);
        if !found.iter().any(|(known, _)| *known == key) {
            found.push((key, part));
            if found.len() == MAX_GENRES_PER_TRACK {
                break;
            }
        }
    }
    found
}

/// The year of a year tag for the lists: its leading digits when they make a
/// year (1 to 9999), else 0 for "none".
fn browse_year(tag: &str) -> u32 {
    let year = columns::year_value(tag);
    if (1..=9999).contains(&year) {
        year as u32
    } else {
        0
    }
}

/// A file length that can be added up: finite and not negative, else zero.
fn summable_seconds(duration: f64) -> f64 {
    if duration.is_finite() && duration > 0. {
        duration
    } else {
        0.
    }
}

/// One row of a browse list: an artist, an album (of one artist) or a genre.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BrowseGroup {
    /// What the group is remembered by (the folded name; for an album the
    /// folded artist, a separator and the folded album). Empty for the group of
    /// files without that tag.
    pub key: String,
    /// The name as the first of its files spells it; empty for the group of
    /// files without the tag (the list draws `BrowseKind::unknown`).
    pub label: String,
    /// Albums only: the artist the album is filed under (empty when unknown).
    pub artist: String,
    /// Albums only: the earliest year among the files, 0 when none has one.
    pub year: u32,
    /// Artists and genres: how many albums they hold.
    pub albums: usize,
    /// Total length of the files.
    pub duration: f64,
    /// When the newest file was added.
    pub added: u64,
    /// The files, as positions in the library, in library order.
    pub members: Vec<usize>,
}

impl BrowseGroup {
    pub fn tracks(&self) -> usize {
        self.members.len()
    }

    fn add(&mut self, position: usize, seconds: f64, year: u32, added: u64) {
        self.members.push(position);
        self.duration += seconds;
        self.added = self.added.max(added);
        if year != 0 && (self.year == 0 || year < self.year) {
            self.year = year;
        }
    }
}

/// The music of a library grouped by artist, album and genre. Only music takes
/// part: no audiobook and no superseded record.
#[derive(Clone, Debug, Default)]
pub struct BrowseIndex {
    groups: [Vec<BrowseGroup>; 3],
    lookup: [HashMap<String, usize>; 3],
    /// How many files take part.
    pub tracks: usize,
}

/// The group with this key in `groups`, made when it is new. Returns its
/// position and whether it was made now.
fn intern(
    groups: &mut Vec<BrowseGroup>,
    by_key: &mut HashMap<String, usize>,
    key: &str,
    label: &str,
) -> (usize, bool) {
    if let Some(&position) = by_key.get(key) {
        return (position, false);
    }
    groups.push(BrowseGroup {
        key: key.to_owned(),
        label: label.to_owned(),
        ..BrowseGroup::default()
    });
    by_key.insert(key.to_owned(), groups.len() - 1);
    (groups.len() - 1, true)
}

impl BrowseIndex {
    /// Groups the music in one pass over the library. Artists are filed under
    /// the album artist, else the track artist; an album is one artist's
    /// album; a genre tag with several genres files the track under each.
    fn build(tracks: &[Track]) -> Self {
        let mut index = Self::default();
        let mut by_key: [HashMap<String, usize>; 3] = Default::default();
        let mut genre_albums: HashSet<(usize, usize)> = HashSet::new();
        for (position, track) in tracks.iter().enumerate() {
            if track.audiobook || track.superseded {
                continue;
            }
            index.tracks += 1;
            let seconds = summable_seconds(track.duration);
            let year = browse_year(&track.year);
            let artist_name = author_name(track);
            let artist_key = fold_text(artist_name);
            let album_key = format!(
                "{artist_key}{KEY_SEPARATOR}{}",
                fold_text(track.album.trim())
            );
            let (artist, _) = intern(
                &mut index.groups[BrowseKind::Artists.slot()],
                &mut by_key[BrowseKind::Artists.slot()],
                &artist_key,
                artist_name,
            );
            index.groups[BrowseKind::Artists.slot()][artist].add(
                position,
                seconds,
                year,
                track.added_at,
            );
            let (album, made) = intern(
                &mut index.groups[BrowseKind::Albums.slot()],
                &mut by_key[BrowseKind::Albums.slot()],
                &album_key,
                track.album.trim(),
            );
            if made {
                index.groups[BrowseKind::Albums.slot()][album].artist = artist_name.to_owned();
                index.groups[BrowseKind::Artists.slot()][artist].albums += 1;
            }
            index.groups[BrowseKind::Albums.slot()][album].add(
                position,
                seconds,
                year,
                track.added_at,
            );
            let genres = genre_parts(&track.genre);
            let unnamed = [(String::new(), "")];
            for (key, label) in if genres.is_empty() {
                &unnamed[..]
            } else {
                &genres[..]
            } {
                let (genre, _) = intern(
                    &mut index.groups[BrowseKind::Genres.slot()],
                    &mut by_key[BrowseKind::Genres.slot()],
                    key,
                    label,
                );
                let group = &mut index.groups[BrowseKind::Genres.slot()][genre];
                group.add(position, seconds, year, track.added_at);
                if genre_albums.insert((genre, album)) {
                    group.albums += 1;
                }
            }
        }
        // Natural order, case and accents ignored, the group without a name last.
        index.groups[BrowseKind::Artists.slot()].sort_by(|a, b| {
            a.key
                .is_empty()
                .cmp(&b.key.is_empty())
                .then_with(|| natural_cmp(&a.label, &b.label))
                .then_with(|| a.key.cmp(&b.key))
        });
        index.groups[BrowseKind::Genres.slot()].sort_by(|a, b| {
            a.key
                .is_empty()
                .cmp(&b.key.is_empty())
                .then_with(|| natural_cmp(&a.label, &b.label))
                .then_with(|| a.key.cmp(&b.key))
        });
        // Albums: artist, then year (none last), then title (none last).
        index.groups[BrowseKind::Albums.slot()].sort_by(|a, b| {
            a.artist
                .is_empty()
                .cmp(&b.artist.is_empty())
                .then_with(|| natural_cmp(&a.artist, &b.artist))
                .then_with(|| {
                    empty_last(a.year == 0, b.year == 0).unwrap_or_else(|| a.year.cmp(&b.year))
                })
                .then_with(|| a.label.is_empty().cmp(&b.label.is_empty()))
                .then_with(|| natural_cmp(&a.label, &b.label))
                .then_with(|| a.key.cmp(&b.key))
        });
        for kind in BrowseKind::ALL {
            index.lookup[kind.slot()] = index.groups[kind.slot()]
                .iter()
                .enumerate()
                .map(|(position, group)| (group.key.clone(), position))
                .collect();
        }
        index
    }

    /// The groups of a list in its default order: artists and genres by name,
    /// albums by artist, year and title.
    pub fn groups(&self, kind: BrowseKind) -> &[BrowseGroup] {
        &self.groups[kind.slot()]
    }

    /// Position in `groups` of the group with this key.
    pub fn position(&self, kind: BrowseKind, key: &str) -> Option<usize> {
        self.lookup[kind.slot()].get(key).copied()
    }

    pub fn group(&self, kind: BrowseKind, key: &str) -> Option<&BrowseGroup> {
        self.position(kind, key)
            .and_then(|position| self.groups[kind.slot()].get(position))
    }
}

impl LibraryCatalog {
    /// What the heading of a group's drill-down says: the artist or the genre,
    /// or the album with its artist ("Album · Artist"). `None` when the group
    /// does not exist.
    pub fn group_title(&self, kind: BrowseKind, key: &str) -> Option<String> {
        let group = self.browse().group(kind, key)?;
        let name = if group.label.is_empty() {
            kind.unknown()
        } else {
            group.label.as_str()
        };
        Some(if kind == BrowseKind::Albums && !group.artist.is_empty() {
            format!("{name} · {}", group.artist)
        } else {
            name.to_owned()
        })
    }
}

/// How a browse list is ordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BrowseSort {
    /// By name, natural order (artists and genres).
    Name,
    /// Most files first.
    Tracks,
    /// Longest first.
    Duration,
    /// By artist, then year, then title (albums).
    Artist,
    /// By title, then artist (albums).
    Title,
    /// Oldest year first, albums without a year last.
    Year,
    /// Most recently added first.
    Added,
}

impl BrowseSort {
    /// The word the profile stores.
    pub fn id(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Tracks => "tracks",
            Self::Duration => "duration",
            Self::Artist => "artist",
            Self::Title => "title",
            Self::Year => "year",
            Self::Added => "added",
        }
    }

    /// The word the selector shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Tracks => "Tracks",
            Self::Duration => "Duration",
            Self::Artist => "Artist",
            Self::Title => "Title",
            Self::Year => "Year",
            Self::Added => "Date added",
        }
    }

    /// The selector's line for this order.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Name => "Name (A–Z)",
            Self::Tracks => "Tracks (most first)",
            Self::Duration => "Duration (longest first)",
            Self::Artist => "Artist (then year, title)",
            Self::Title => "Title (A–Z)",
            Self::Year => "Year (oldest first)",
            Self::Added => "Date added (newest first)",
        }
    }

    /// The orders a list offers, the default first.
    pub fn options(kind: BrowseKind) -> &'static [BrowseSort] {
        match kind {
            BrowseKind::Albums => &[Self::Artist, Self::Title, Self::Year, Self::Added],
            _ => &[Self::Name, Self::Tracks, Self::Duration],
        }
    }

    pub fn default_for(kind: BrowseKind) -> Self {
        Self::options(kind)[0]
    }

    fn parse(kind: BrowseKind, id: &str) -> Option<Self> {
        Self::options(kind)
            .iter()
            .copied()
            .find(|sort| sort.id() == id)
    }
}

/// The longest browse-sort setting that is read; anything longer is damage.
const MAX_BROWSE_SORT_BYTES: usize = 256;

/// The order of each list found in a `Session::browse_sort` setting
/// (`artists=tracks;albums=year`); entries that are unknown, repeated badly or
/// not offered by their list are ignored.
fn browse_sort_entries(setting: &str) -> [Option<BrowseSort>; 3] {
    let mut found = [None; 3];
    if setting.len() > MAX_BROWSE_SORT_BYTES {
        return found;
    }
    for entry in setting.split(';') {
        let Some((list, id)) = entry.split_once('=') else {
            continue;
        };
        let Some(kind) = BrowseKind::ALL
            .into_iter()
            .find(|kind| kind.list_section().eq_ignore_ascii_case(list.trim()))
        else {
            continue;
        };
        if let Some(sort) = BrowseSort::parse(kind, id.trim()) {
            found[kind.slot()] = Some(sort);
        }
    }
    found
}

/// The setting that holds these orders; a list in its default order has no entry.
fn browse_sort_setting(entries: &[Option<BrowseSort>; 3]) -> String {
    BrowseKind::ALL
        .into_iter()
        .filter_map(|kind| {
            entries[kind.slot()]
                .filter(|sort| *sort != BrowseSort::default_for(kind))
                .map(|sort| format!("{}={}", kind.list_section().to_ascii_lowercase(), sort.id()))
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// How the setting orders this list.
pub fn browse_sort_of(setting: &str, kind: BrowseKind) -> BrowseSort {
    browse_sort_entries(setting)[kind.slot()].unwrap_or_else(|| BrowseSort::default_for(kind))
}

/// The setting with this list's order replaced; the other lists keep theirs.
pub fn with_browse_sort(setting: &str, kind: BrowseKind, sort: BrowseSort) -> String {
    let mut entries = browse_sort_entries(setting);
    entries[kind.slot()] = Some(sort);
    browse_sort_setting(&entries)
}

/// The setting reduced to what this build reads (for a profile that was
/// edited by hand or written by another version).
pub fn normalized_browse_sort(setting: &str) -> String {
    browse_sort_setting(&browse_sort_entries(setting))
}

/// The rows of a browse list for one library revision, order and search: groups
/// of the catalog's `BrowseIndex`, by position.
#[derive(Clone, Debug)]
pub struct BrowseView {
    catalog: Arc<LibraryCatalog>,
    pub kind: BrowseKind,
    #[allow(dead_code)]
    pub sort: BrowseSort,
    /// Positions in `BrowseIndex::groups`, in display order.
    rows: Vec<usize>,
    /// How many groups the list has before the search narrows it.
    pub total: usize,
    /// How many different files the rows hold (a file of two genres counts once).
    pub tracks: usize,
    /// Their total length, each file once.
    pub duration: f64,
}

impl BrowseView {
    fn build(
        catalog: &Arc<LibraryCatalog>,
        kind: BrowseKind,
        sort: BrowseSort,
        search: &str,
    ) -> Self {
        let groups = catalog.browse().groups(kind);
        let folded = fold_text(search);
        let mut rows: Vec<usize> = (0..groups.len())
            .filter(|&row| group_matches(kind, &groups[row], &folded))
            .collect();
        // The index is in the default order and every sort below is stable, so
        // ties keep it. The group without a name stays last in all of them.
        let unnamed = |group: &BrowseGroup| group.label.is_empty();
        match sort {
            BrowseSort::Name | BrowseSort::Artist => {}
            BrowseSort::Tracks => rows.sort_by(|&a, &b| {
                unnamed(&groups[a])
                    .cmp(&unnamed(&groups[b]))
                    .then_with(|| groups[b].members.len().cmp(&groups[a].members.len()))
            }),
            BrowseSort::Duration => rows.sort_by(|&a, &b| {
                unnamed(&groups[a])
                    .cmp(&unnamed(&groups[b]))
                    .then_with(|| groups[b].duration.total_cmp(&groups[a].duration))
            }),
            BrowseSort::Title => rows.sort_by(|&a, &b| {
                unnamed(&groups[a])
                    .cmp(&unnamed(&groups[b]))
                    .then_with(|| natural_cmp(&groups[a].label, &groups[b].label))
                    .then_with(|| natural_cmp(&groups[a].artist, &groups[b].artist))
            }),
            BrowseSort::Year => rows.sort_by(|&a, &b| {
                empty_last(groups[a].year == 0, groups[b].year == 0)
                    .unwrap_or_else(|| groups[a].year.cmp(&groups[b].year))
            }),
            BrowseSort::Added => rows.sort_by(|&a, &b| groups[b].added.cmp(&groups[a].added)),
        }
        let mut seen = vec![false; catalog.tracks.len()];
        let (mut tracks, mut duration) = (0, 0.);
        for &row in &rows {
            for &member in &groups[row].members {
                if let Some(flag) = seen.get_mut(member) {
                    if !*flag {
                        *flag = true;
                        tracks += 1;
                        duration += summable_seconds(catalog.tracks[member].duration);
                    }
                }
            }
        }
        Self {
            catalog: Arc::clone(catalog),
            kind,
            sort,
            total: groups.len(),
            rows,
            tracks,
            duration,
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The group shown in row `row`.
    pub fn group(&self, row: usize) -> Option<&BrowseGroup> {
        let position = *self.rows.get(row)?;
        self.catalog.browse().groups(self.kind).get(position)
    }

    /// The row that shows the group with this key.
    pub fn row_of(&self, key: &str) -> Option<usize> {
        let groups = self.catalog.browse().groups(self.kind);
        self.rows
            .iter()
            .position(|&position| groups.get(position).is_some_and(|group| group.key == key))
    }

    /// A file of the group in row `row`, the one a row thumbnail is read from.
    pub fn cover(&self, row: usize) -> Option<&Track> {
        let first = *self.group(row)?.members.first()?;
        self.catalog.tracks.get(first)
    }
}

/// Whether a search (already folded) finds a group: in its name, and for an
/// album in its artist too. The words that stand for a missing tag find it.
fn group_matches(kind: BrowseKind, group: &BrowseGroup, folded: &str) -> bool {
    if folded.is_empty() {
        return true;
    }
    let found = if kind == BrowseKind::Albums {
        let (artist, album) = group
            .key
            .split_once(KEY_SEPARATOR)
            .unwrap_or(("", &group.key));
        artist.contains(folded)
            || album.contains(folded)
            || format!("{album} {artist}").contains(folded)
            || group.artist.is_empty() && fold_text("Unknown artist").contains(folded)
    } else {
        group.key.contains(folded)
    };
    found || group.label.is_empty() && fold_text(kind.unknown()).contains(folded)
}

#[derive(Clone, Debug)]
pub struct VisibleView {
    catalog: Arc<LibraryCatalog>,
    indices: Vec<usize>,
    favorite_ids: HashSet<String>,
    pub title: String,
    pub total_duration: f64,
}

impl VisibleView {
    pub fn len(&self) -> usize {
        self.indices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Whether a file is currently marked as a favorite.
    pub fn is_favorite(&self, id: &str) -> bool {
        self.favorite_ids.contains(id)
    }

    pub fn first(&self) -> Option<&Track> {
        self.indices
            .first()
            .map(|&index| &self.catalog.tracks[index])
    }

    pub fn iter(&self) -> impl Iterator<Item = &Track> + '_ {
        self.indices
            .iter()
            .map(|&index| &self.catalog.tracks[index])
    }
}

impl Index<usize> for VisibleView {
    type Output = Track;

    fn index(&self, index: usize) -> &Self::Output {
        &self.catalog.tracks[self.indices[index]]
    }
}

#[derive(Clone, Debug)]
struct ViewKey {
    library_revision: u64,
    view_revision: u64,
    progress_revision: Option<u64>,
    section: String,
    tab: String,
    search: String,
    sort_column: usize,
    sort_descending: bool,
}

impl ViewKey {
    fn matches(
        &self,
        state: &Session,
        library_revision: u64,
        view_revision: u64,
        progress_revision: u64,
    ) -> bool {
        let progress_revision = uses_progress_revision(state).then_some(progress_revision);
        self.library_revision == library_revision
            && self.view_revision == view_revision
            && self.progress_revision == progress_revision
            && self.section == state.section
            && self.tab == state.tab
            && self.search == state.search
            && self.sort_column == state.sort_column
            && self.sort_descending == state.sort_descending
    }

    fn new(
        state: &Session,
        library_revision: u64,
        view_revision: u64,
        progress_revision: u64,
    ) -> Self {
        Self {
            library_revision,
            view_revision,
            progress_revision: uses_progress_revision(state).then_some(progress_revision),
            section: state.section.clone(),
            tab: state.tab.clone(),
            search: state.search.clone(),
            sort_column: state.sort_column,
            sort_descending: state.sort_descending,
        }
    }
}

fn uses_progress_revision(state: &Session) -> bool {
    use columns::{COL_LAST_PLAYED, COL_PLAYS, COL_PROGRESS};
    state.tab != "Queue"
        && (state.section == "Continue listening"
            || state.section == "Finished"
            || matches!(
                state.sort_column,
                COL_PROGRESS | COL_PLAYS | COL_LAST_PLAYED
            ))
}

#[derive(Default)]
pub struct GuiCache {
    catalog: Option<(u64, Arc<LibraryCatalog>)>,
    visible: Option<(ViewKey, Arc<VisibleView>)>,
    /// Progress of every book, for the library revision and progress revision
    /// it was made for.
    books: Option<(u64, u64, Arc<Vec<BookProgress>>)>,
    /// How many times the visible list was rebuilt (see `view_generation`).
    view_builds: u64,
    /// The rows of the browse list that is shown.
    browse: Option<(BrowseKey, Arc<BrowseView>)>,
    /// How many times a browse list was rebuilt (see `browse_generation`).
    browse_builds: u64,
}

/// What a built browse list was made for.
#[derive(Clone, Debug, PartialEq)]
struct BrowseKey {
    library_revision: u64,
    kind: BrowseKind,
    sort: BrowseSort,
    search: String,
}

impl GuiCache {
    /// Read-only metadata lookup for frequently painted playback controls.
    pub fn track_at_revision(&self, id: &str, revision: u64) -> Option<&Track> {
        self.catalog.as_ref().filter(|(stored, _)| *stored == revision)
            .and_then(|(_, catalog)| catalog.track(id))
    }
    /// Changes whenever the visible list is rebuilt, which is when its rows,
    /// their order or the library behind them may differ. A cheap way for
    /// the selection to notice that it must be checked against the new rows.
    pub fn view_generation(&self) -> u64 {
        self.view_builds
    }

    pub fn catalog(
        &mut self,
        tracks: &Arc<Vec<Track>>,
        library_revision: u64,
    ) -> Arc<LibraryCatalog> {
        if let Some((revision, catalog)) = &self.catalog {
            if *revision == library_revision {
                return Arc::clone(catalog);
            }
        }
        let catalog = Arc::new(LibraryCatalog::new(tracks));
        self.catalog = Some((library_revision, Arc::clone(&catalog)));
        self.visible = None;
        self.browse = None;
        catalog
    }

    /// Changes whenever a browse list is rebuilt: a library, order or search
    /// change, never a repaint.
    #[allow(dead_code)]
    pub fn browse_generation(&self) -> u64 {
        self.browse_builds
    }

    /// The rows of the browse list the section shows (`None` when the section
    /// is not Artists, Albums or Genres). Kept until the library, the list's
    /// order or the search changes.
    pub fn browse_view(
        &mut self,
        state: &Session,
        library_revision: u64,
    ) -> Option<Arc<BrowseView>> {
        let BrowseSection::List(kind) = browse_section(&state.section)? else {
            return None;
        };
        let sort = browse_sort_of(&state.browse_sort, kind);
        if let Some((key, view)) = &self.browse {
            if key.library_revision == library_revision
                && key.kind == kind
                && key.sort == sort
                && key.search == state.search
            {
                return Some(Arc::clone(view));
            }
        }
        let catalog = self.catalog(&state.library, library_revision);
        let view = Arc::new(BrowseView::build(&catalog, kind, sort, &state.search));
        let key = BrowseKey {
            library_revision,
            kind,
            sort,
            search: state.search.clone(),
        };
        self.browse_builds = self.browse_builds.wrapping_add(1);
        self.browse = Some((key, Arc::clone(&view)));
        Some(view)
    }

    /// The listening progress of every book, parallel to `LibraryCatalog::books`
    /// and made at normal speed (`BookProgress::left_at` gives other speeds).
    /// Kept until the library or the listening progress changes, so the lists
    /// that show it never walk the library per frame.
    pub fn book_progress(
        &mut self,
        state: &Session,
        library_revision: u64,
        progress_revision: u64,
    ) -> Arc<Vec<BookProgress>> {
        if let Some((library, progress, books)) = &self.books {
            if *library == library_revision && *progress == progress_revision {
                return Arc::clone(books);
            }
        }
        let catalog = self.catalog(&state.library, library_revision);
        let books: Vec<BookProgress> = (0..catalog.books.len())
            .map(|position| {
                crate::state::book_progress(catalog.book_files(position), &state.progress, 1.)
            })
            .collect();
        let books = Arc::new(books);
        self.books = Some((library_revision, progress_revision, Arc::clone(&books)));
        books
    }

    pub fn view(
        &mut self,
        state: &Session,
        library_revision: u64,
        view_revision: u64,
        progress_revision: u64,
    ) -> Arc<VisibleView> {
        // Compare strings by reference on the hot path. Allocate key strings
        // only when a view actually needs rebuilding.
        if let Some((key, view)) = &self.visible {
            if key.matches(state, library_revision, view_revision, progress_revision) {
                return Arc::clone(view);
            }
        }

        let catalog = self.catalog(&state.library, library_revision);
        let key = ViewKey::new(state, library_revision, view_revision, progress_revision);
        let view = Arc::new(build_view(&catalog, state));
        self.view_builds = self.view_builds.wrapping_add(1);
        self.visible = Some((key, Arc::clone(&view)));
        view
    }
}

fn build_view(catalog: &Arc<LibraryCatalog>, state: &Session) -> VisibleView {
    let search = fold_text(&state.search);
    let favorite_ids: HashSet<String> = state.favorites.iter().cloned().collect();
    let mut indices = Vec::new();
    // The Streams section lists stations, not files: no file is visible in it,
    // so no file command (Enter, Delete, play displayed files) can reach one.
    if state.section == crate::state::STREAMS_SECTION {
        return VisibleView {
            catalog: Arc::clone(catalog),
            indices,
            favorite_ids,
            title: crate::state::STREAMS_SECTION.to_owned(),
            total_duration: 0.,
        };
    }
    // The same for the Artists, Albums and Genres lists, which list groups
    // (see `browse_view`); their drill-downs below are ordinary file lists.
    if let Some(BrowseSection::List(kind)) = browse_section(&state.section) {
        return VisibleView {
            catalog: Arc::clone(catalog),
            indices,
            favorite_ids,
            title: kind.list_section().to_owned(),
            total_duration: 0.,
        };
    }
    // Watched folders are stored as picked while tracks hold canonical paths
    // (symlinks resolved, `\\?\` prefix on Windows): accept either spelling.
    let folder_roots: Option<(PathBuf, Option<PathBuf>)> = state
        .section
        .strip_prefix("Folder:")
        .map(|folder| (PathBuf::from(folder), std::fs::canonicalize(folder).ok()));

    let queue_view = state.tab == "Queue" || state.section == crate::state::PLAYING_SECTION;
    if queue_view {
        // Queue order and duplicate IDs are user-visible; unknown IDs are
        // ignored just like Session::track in the previous implementation.
        for id in &state.queue {
            if let Some(&index) = catalog.by_id.get(id) {
                if search.is_empty() || catalog.search_text[index].contains(&search) {
                    indices.push(index);
                }
            }
        }
    } else if let Some(BrowseSection::Group(kind, key)) = browse_section(&state.section) {
        // A drill-down: the files of one artist, album or genre (none when the
        // group is gone; the app then reopens the list).
        if let Some(group) = catalog.browse().group(kind, key) {
            indices.extend(group.members.iter().copied().filter(|&index| {
                search.is_empty() || catalog.search_text[index].contains(&search)
            }));
        }
    } else {
        let favorite_ids: HashSet<&str> = if state.section == "Favorites" {
            state.favorites.iter().map(String::as_str).collect()
        } else {
            HashSet::new()
        };
        let playlist_ids: HashSet<&str> = state
            .section
            .strip_prefix("Playlist:")
            .and_then(|name| state.playlists.iter().find(|list| list.name == name))
            .map(|list| list.tracks.iter().map(String::as_str).collect())
            .unwrap_or_default();

        for index in catalog.indices() {
            let track = &catalog.tracks[index];
            let visible = if state.section == "Music" {
                !track.audiobook
            } else if state.section == "Audiobooks" {
                track.audiobook
            } else if state.section == "Favorites" {
                favorite_ids.contains(track.id.as_str())
            } else if state.section == "Continue listening" {
                state
                    .progress
                    .get(&track.id)
                    .is_some_and(|progress| progress.seconds > 0. && !progress.completed)
            } else if state.section == "Finished" {
                track.audiobook
                    && state
                        .progress
                        .get(&track.id)
                        .is_some_and(|progress| progress.completed)
            } else if let Some(book) = state.section.strip_prefix("Book:") {
                catalog.book_keys[index] == book
            } else if let Some((raw, canonical)) = &folder_roots {
                track.path.starts_with(raw)
                    || canonical
                        .as_ref()
                        .is_some_and(|canonical| track.path.starts_with(canonical))
            } else if state.section.starts_with("Playlist:") {
                playlist_ids.contains(track.id.as_str())
            } else {
                true
            };
            if visible && (search.is_empty() || catalog.search_text[index].contains(&search)) {
                indices.push(index);
            }
        }
    }

    if !queue_view {
        sort_indices(&mut indices, catalog, state);
    }

    let total_duration = indices
        .iter()
        .map(|&index| catalog.tracks[index].duration)
        .sum();
    let title = view_title(catalog, state, &indices);
    VisibleView {
        catalog: Arc::clone(catalog),
        indices,
        favorite_ids,
        title,
        total_duration,
    }
}

/// Orders the rows by the sort column. Every sort is stable, so ties keep the
/// library order, in both directions.
fn sort_indices(indices: &mut [usize], catalog: &LibraryCatalog, state: &Session) {
    use columns::COL_YEAR;
    use columns::{COL_ADDED, COL_ALBUM, COL_ARTIST, COL_DURATION, COL_FORMAT, COL_GENRE};
    use columns::{COL_ALBUM_ARTIST, COL_COMPOSER, COL_DISC, COL_NARRATOR};
    use columns::{COL_CHANNELS, COL_COMMENT, COL_DATE_MODIFIED, COL_FAVORITE, COL_FILE_SIZE};
    use columns::{COL_FILENAME, COL_FOLDER, COL_FULL_PATH, COL_SAMPLE_RATE, COL_TRACK_NUMBER};
    use columns::{COL_INDEX, COL_LAST_PLAYED, COL_PLAYS, COL_PROGRESS, COL_RATING, COL_TITLE};
    if state.section.starts_with("Playlist:") && state.sort_column == COL_INDEX {
        return sort_by_playlist_order(indices, catalog, state);
    }
    // An artist's or an album's files stand album by album, disc by disc, in
    // track order (a genre keeps the book order below).
    if state.sort_column == COL_INDEX
        && matches!(
            browse_section(&state.section),
            Some(BrowseSection::Group(
                BrowseKind::Artists | BrowseKind::Albums,
                _
            ))
        )
    {
        indices.sort_by(|&a, &b| catalog.album_order(a, b));
        if state.sort_descending {
            indices.reverse();
        }
        return;
    }
    if state.section != "Recently added" {
        match state.sort_column {
            COL_YEAR | COL_RATING | COL_PLAYS | COL_LAST_PLAYED | COL_ADDED | COL_DISC
            | COL_FILE_SIZE | COL_SAMPLE_RATE | COL_CHANNELS | COL_TRACK_NUMBER | COL_FAVORITE
            | COL_DATE_MODIFIED => {
                return sort_by_number(indices, catalog, state);
            }
            COL_ALBUM | COL_GENRE | COL_FORMAT | COL_ALBUM_ARTIST | COL_COMPOSER | COL_NARRATOR
            | COL_FILENAME | COL_FOLDER | COL_FULL_PATH | COL_COMMENT => {
                return sort_by_text(indices, catalog, state);
            }
            _ => {}
        }
    }
    indices.sort_by(|&a_index, &b_index| {
        let a = &catalog.tracks[a_index];
        let b = &catalog.tracks[b_index];
        let ordering = if state.section == "Recently added" {
            b.added_at.cmp(&a.added_at)
        } else {
            match state.sort_column {
                COL_INDEX => {
                    let in_book_order = catalog.book_order(a_index, b_index);
                    if state.section == "Finished" {
                        // Most recently finished first; files finished together
                        // (a whole book marked at once) stay in book order.
                        last_played(state, b)
                            .cmp(&last_played(state, a))
                            .then(in_book_order)
                    } else {
                        in_book_order
                    }
                }
                COL_TITLE => natural_cmp(a.display_title(), b.display_title()),
                COL_ARTIST => natural_cmp(&a.artist, &b.artist),
                COL_DURATION => a.duration.total_cmp(&b.duration),
                COL_PROGRESS => progress_seconds(state, a).total_cmp(&progress_seconds(state, b)),
                // Bitrate, the last original column.
                _ => a.bitrate.cmp(&b.bitrate),
            }
        };
        if state.sort_descending {
            ordering.reverse()
        } else {
            ordering
        }
    });
}

/// Whether the Files view of a playlist lists the files in the playlist's own
/// order (the `#` column, ascending, which is the default sort): only then do
/// the rows stand for the playlist's order, so dragging one changes it.
pub fn playlist_order_active(state: &Session) -> bool {
    state.tab == "Files"
        && state.section.starts_with("Playlist:")
        && state.sort_column == columns::COL_INDEX
        && !state.sort_descending
}

/// The playlist's own order; a file listed twice stands at its first place and
/// a descending sort reverses it.
fn sort_by_playlist_order(indices: &mut [usize], catalog: &LibraryCatalog, state: &Session) {
    let Some(list) = state
        .section
        .strip_prefix("Playlist:")
        .and_then(|name| state.playlists.iter().find(|list| list.name == name))
    else {
        return;
    };
    let mut position: HashMap<&str, usize> = HashMap::with_capacity(list.tracks.len());
    for (place, id) in list.tracks.iter().enumerate() {
        position.entry(id.as_str()).or_insert(place);
    }
    indices.sort_by_key(|&index| {
        position
            .get(catalog.tracks[index].id.as_str())
            .copied()
            .unwrap_or(usize::MAX)
    });
    if state.sort_descending {
        indices.reverse();
    }
}

/// Order for a column whose empty cells always come last, whichever way the
/// list is sorted. Two empty cells tie; `None` means neither is empty and the
/// values decide.
fn empty_last(a_empty: bool, b_empty: bool) -> Option<std::cmp::Ordering> {
    match (a_empty, b_empty) {
        (true, true) => Some(std::cmp::Ordering::Equal),
        (true, false) => Some(std::cmp::Ordering::Greater),
        (false, true) => Some(std::cmp::Ordering::Less),
        (false, false) => None,
    }
}

fn directed(ordering: std::cmp::Ordering, descending: bool) -> std::cmp::Ordering {
    if descending {
        ordering.reverse()
    } else {
        ordering
    }
}

/// Numeric sort value of a row for the number columns; 0 means "no value".
fn number_key(
    column: usize,
    track: &Track,
    state: &Session,
    favorites: &HashSet<&str>,
) -> u64 {
    let progress = || state.progress.get(&track.id);
    match column {
        columns::COL_YEAR => columns::year_value(&track.year),
        columns::COL_RATING => state.ratings.get(&track.id).map_or(0, |&r| u64::from(r)),
        columns::COL_PLAYS => progress().map_or(0, |p| p.play_count),
        columns::COL_LAST_PLAYED => progress().map_or(0, |p| p.last_played),
        columns::COL_ADDED => track.added_at,
        columns::COL_DISC => u64::from(track.disc),
        columns::COL_FILE_SIZE => track.file_size,
        columns::COL_SAMPLE_RATE => u64::from(track.sample_rate),
        columns::COL_CHANNELS => u64::from(track.channels),
        columns::COL_TRACK_NUMBER => u64::from(track.track_number),
        columns::COL_FAVORITE => u64::from(favorites.contains(track.id.as_str())),
        columns::COL_DATE_MODIFIED => track.modified,
        _ => 0,
    }
}

/// Sorts by a number column. Keys are read once per row, not once per
/// comparison, so 50k-row libraries stay quick.
fn sort_by_number(indices: &mut [usize], catalog: &LibraryCatalog, state: &Session) {
    let favorites: HashSet<&str> = state.favorites.iter().map(String::as_str).collect();
    let mut keyed: Vec<(u64, usize)> = indices
        .iter()
        .map(|&index| {
            let key = number_key(
                state.sort_column,
                &catalog.tracks[index],
                state,
                &favorites,
            );
            (key, index)
        })
        .collect();
    keyed.sort_by(|a, b| {
        empty_last(a.0 == 0, b.0 == 0)
            .unwrap_or_else(|| directed(a.0.cmp(&b.0), state.sort_descending))
    });
    for (slot, (_, index)) in indices.iter_mut().zip(keyed) {
        *slot = index;
    }
}

/// Text of a row for the text columns.
fn text_key(column: usize, track: &Track) -> &str {
    match column {
        columns::COL_ALBUM => &track.album,
        columns::COL_GENRE => &track.genre,
        columns::COL_FORMAT => columns::format_name(track),
        columns::COL_ALBUM_ARTIST => &track.album_artist,
        columns::COL_COMPOSER => &track.composer,
        columns::COL_NARRATOR => &track.narrator,
        columns::COL_FILENAME => track.path.file_name().and_then(|name| name.to_str()).unwrap_or(""),
        columns::COL_FOLDER => track.path.parent().and_then(|folder| folder.to_str()).unwrap_or(""),
        columns::COL_FULL_PATH => track.path.to_str().unwrap_or(""),
        columns::COL_COMMENT => &track.comment,
        _ => "",
    }
}

/// Sorts by a text column with the same folded natural comparison as Title;
/// blank values come last in both directions.
fn sort_by_text(indices: &mut [usize], catalog: &LibraryCatalog, state: &Session) {
    indices.sort_by(|&a_index, &b_index| {
        let a = text_key(state.sort_column, &catalog.tracks[a_index]);
        let b = text_key(state.sort_column, &catalog.tracks[b_index]);
        empty_last(a.trim().is_empty(), b.trim().is_empty())
            .unwrap_or_else(|| directed(natural_cmp(a, b), state.sort_descending))
    });
}

/// When the file was last played (0 when never).
fn last_played(state: &Session, track: &Track) -> u64 {
    state.progress.get(&track.id).map_or(0, |p| p.last_played)
}

fn progress_seconds(state: &Session, track: &Track) -> f64 {
    state
        .progress
        .get(&track.id)
        .map(|progress| progress.seconds)
        .unwrap_or(0.)
}

fn view_title(catalog: &LibraryCatalog, state: &Session, indices: &[usize]) -> String {
    if state.section == "Audiobooks"
        && !indices.is_empty()
        && indices
            .iter()
            .all(|&index| catalog.book_keys[index] == catalog.book_keys[indices[0]])
    {
        return catalog.tracks[indices[0]].book_label();
    }
    if let Some(book) = state.section.strip_prefix("Book:") {
        return catalog
            .book_labels
            .get(book)
            .cloned()
            .unwrap_or_else(|| "Audiobook".into());
    }
    if let Some(BrowseSection::Group(kind, key)) = browse_section(&state.section) {
        return catalog
            .group_title(kind, key)
            .unwrap_or_else(|| kind.list_section().to_owned());
    }
    if let Some(folder) = state.section.strip_prefix("Folder:") {
        return Path::new(folder)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
    }
    state
        .section
        .strip_prefix("Playlist:")
        .unwrap_or(&state.section)
        .to_owned()
}

/// What tints a track's generic cover: the album, else the folder name, else
/// the title.
pub fn art_seed(track: &Track) -> &str {
    if !track.album.trim().is_empty() {
        return &track.album;
    }
    track
        .path
        .parent()
        .and_then(|folder| folder.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&track.title)
}

/// Most row thumbnails kept as textures (at most 96 x 96 pixels each).
pub const THUMB_CACHE_ENTRIES: usize = 400;
/// Most "this album has no art" answers remembered.
const THUMB_MISSING_ENTRIES: usize = 1024;
/// A "no art" answer is trusted for this long, so a cover added later shows up.
const THUMB_MISSING_TTL: Duration = Duration::from_secs(60);
/// Most thumbnail requests waiting for the worker.
pub const THUMB_QUEUE_ENTRIES: usize = 64;

/// Identity of a row thumbnail: the tracks of one album in one folder share a
/// cover, so the album is decoded once.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ThumbKey {
    folder: PathBuf,
    album: String,
}

impl ThumbKey {
    pub fn of(track: &Track) -> Self {
        let folder = track
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let album = if track.album.trim().is_empty() {
            // Files without an album tag have nothing in common but the folder
            // (each may carry its own embedded picture), so each is its own key.
            // The NUL cannot occur in a real tag.
            format!(
                "\0{}",
                track.path.file_name().unwrap_or_default().to_string_lossy()
            )
        } else {
            track.album.clone()
        };
        Self { folder, album }
    }
}

/// Least-recently-used map from thumbnail key to a value (a texture).
pub struct Lru<V> {
    capacity: usize,
    clock: u64,
    entries: HashMap<ThumbKey, (V, u64)>,
}

impl<V> Lru<V> {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            clock: 0,
            entries: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn contains(&self, key: &ThumbKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Looks the key up and marks it as just used.
    pub fn get(&mut self, key: &ThumbKey) -> Option<&V> {
        self.clock += 1;
        let stamp = self.clock;
        self.entries.get_mut(key).map(|entry| {
            entry.1 = stamp;
            &entry.0
        })
    }

    pub fn insert(&mut self, key: ThumbKey, value: V) {
        self.clock += 1;
        self.entries.insert(key, (value, self.clock));
        while self.entries.len() > self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, stamp))| *stamp)
                .map(|(key, _)| key.clone());
            match oldest {
                Some(key) => self.entries.remove(&key),
                None => break,
            };
        }
    }

    pub fn remove(&mut self, key: &ThumbKey) {
        self.entries.remove(key);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

pub enum ThumbState<'a, V> {
    Ready(&'a V),
    /// Looked for; the album has no readable artwork (for now).
    Missing,
    Unknown,
}

/// Decoded thumbnails (an LRU) plus a bounded, expiring list of albums that
/// turned out to have no art, both under the same key.
pub struct ThumbCache<V> {
    ready: Lru<V>,
    missing: HashMap<ThumbKey, Instant>,
    missing_capacity: usize,
}

impl<V> ThumbCache<V> {
    pub fn new() -> Self {
        Self::with_limits(THUMB_CACHE_ENTRIES, THUMB_MISSING_ENTRIES)
    }

    fn with_limits(ready: usize, missing: usize) -> Self {
        Self {
            ready: Lru::new(ready),
            missing: HashMap::new(),
            missing_capacity: missing.max(1),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ready.len() == 0 && self.missing.is_empty()
    }

    pub fn lookup(&mut self, key: &ThumbKey, now: Instant) -> ThumbState<'_, V> {
        if self.ready.contains(key) {
            if let Some(value) = self.ready.get(key) {
                return ThumbState::Ready(value);
            }
        }
        if let Some(&at) = self.missing.get(key) {
            if now.saturating_duration_since(at) < THUMB_MISSING_TTL {
                return ThumbState::Missing;
            }
            self.missing.remove(key);
        }
        ThumbState::Unknown
    }

    pub fn insert(&mut self, key: ThumbKey, value: V) {
        self.missing.remove(&key);
        self.ready.insert(key, value);
    }

    pub fn insert_missing(&mut self, key: ThumbKey, now: Instant) {
        self.ready.remove(&key);
        if !self.missing.contains_key(&key) && self.missing.len() >= self.missing_capacity {
            self.missing
                .retain(|_, at| now.saturating_duration_since(*at) < THUMB_MISSING_TTL);
            if self.missing.len() >= self.missing_capacity {
                let oldest = self
                    .missing
                    .iter()
                    .min_by_key(|(_, at)| **at)
                    .map(|(key, _)| key.clone());
                if let Some(oldest) = oldest {
                    self.missing.remove(&oldest);
                }
            }
        }
        self.missing.insert(key, now);
    }

    /// Forgets the thumbnail and the "no art" answer of `key`: its picture changed.
    pub fn remove(&mut self, key: &ThumbKey) {
        self.ready.remove(key);
        self.missing.remove(key);
    }

    pub fn clear(&mut self) {
        self.ready.clear();
        self.missing.clear();
    }
}

struct QueueInner<J> {
    pending: VecDeque<(ThumbKey, J)>,
    in_flight: Option<ThumbKey>,
    closed: bool,
}

/// Work waiting for the thumbnail thread. Bounded; the UI hands over what is
/// visible right now, so entries that scrolled away are dropped and the newly
/// visible ones are served first.
pub struct ThumbQueue<J> {
    inner: Mutex<QueueInner<J>>,
    wake: Condvar,
    capacity: usize,
}

impl<J> ThumbQueue<J> {
    pub fn new() -> Self {
        Self::with_capacity(THUMB_QUEUE_ENTRIES)
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(QueueInner {
                pending: VecDeque::new(),
                in_flight: None,
                closed: false,
            }),
            wake: Condvar::new(),
            capacity: capacity.max(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QueueInner<J>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn is_empty(&self) -> bool {
        self.lock().pending.is_empty()
    }



    /// `wanted` is everything visible and still missing, in reading order. Queued
    /// entries not in it are dropped; keys not yet queued (and not being decoded)
    /// go to the front; the oldest are cut beyond the capacity.
    pub fn submit(&self, wanted: Vec<(ThumbKey, J)>) {
        let mut inner = self.lock();
        if inner.closed {
            return;
        }
        let still_wanted: HashSet<&ThumbKey> = wanted.iter().map(|(key, _)| key).collect();
        let mut kept: VecDeque<(ThumbKey, J)> = std::mem::take(&mut inner.pending)
            .into_iter()
            .filter(|(key, _)| still_wanted.contains(key))
            .collect();
        let mut seen: HashSet<ThumbKey> = kept.iter().map(|(key, _)| key.clone()).collect();
        let mut fresh = Vec::new();
        for (key, job) in wanted {
            if inner.in_flight.as_ref() == Some(&key) || !seen.insert(key.clone()) {
                continue;
            }
            fresh.push((key, job));
        }
        for entry in fresh.into_iter().rev() {
            kept.push_front(entry);
        }
        kept.truncate(self.capacity);
        let has_work = !kept.is_empty();
        inner.pending = kept;
        drop(inner);
        if has_work {
            self.wake.notify_one();
        }
    }

    fn take(&self, block: bool) -> Option<(ThumbKey, J)> {
        let mut inner = self.lock();
        loop {
            if inner.closed {
                return None;
            }
            if let Some(entry) = inner.pending.pop_front() {
                inner.in_flight = Some(entry.0.clone());
                return Some(entry);
            }
            if !block {
                return None;
            }
            inner = self
                .wake
                .wait(inner)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Next entry for the worker; blocks while idle, `None` once closed.
    pub fn next(&self) -> Option<(ThumbKey, J)> {
        self.take(true)
    }

    /// The worker is done with the entry `next` gave it.
    pub fn finish(&self) {
        self.lock().in_flight = None;
    }

    /// Drops everything waiting (an entry being decoded still completes).
    pub fn clear(&self) {
        self.lock().pending.clear();
    }

    /// Ends the worker.
    pub fn close(&self) {
        let mut inner = self.lock();
        inner.closed = true;
        inner.pending.clear();
        drop(inner);
        self.wake.notify_all();
    }
}
