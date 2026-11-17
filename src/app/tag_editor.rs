use super::*;

// ---------------------------------------------------------------------------
// Edit file tags: the window, its worker jobs and the library carry-over.
// ---------------------------------------------------------------------------

/// Most other files of an album the editor offers to change together.
pub(super) const MAX_ALBUM_MATES: usize = 1000;

/// Largest cover image read for a replacement (the tag writer's own limit).
pub(super) const MAX_PICKED_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// What the Edit file tags window needs from the file, read on a worker.
pub(super) struct TagEditorData {
    pub(super) tags: tagwrite::EditableTags,
    /// The cover embedded in the file, decoded for the artwork box.
    pub(super) art: Option<egui::ColorImage>,
    /// Where the cover came from, or why there is none to show.
    pub(super) art_note: String,
    /// The other editable files of the same album in the same folder.
    pub(super) album_ids: Vec<String>,
    /// Files of that album that cannot be edited (counted, not errors).
    pub(super) album_skipped: usize,
}

/// A cover image the user picked: the file's bytes (stored as they are) and a
/// preview for the artwork box.
pub(super) struct ChosenArtwork {
    pub(super) bytes: Vec<u8>,
    pub(super) preview: egui::ColorImage,
    pub(super) name: String,
}

/// What will happen to the embedded cover when the tags are saved.
pub(super) enum EditorArt {
    Keep,
    Remove,
    Replace {
        bytes: Vec<u8>,
        texture: egui::TextureHandle,
        name: String,
    },
}

pub(super) enum EditorLoad {
    Loading,
    Failed(String),
    Ready,
}

/// The Edit file tags window.
pub(super) struct TagEditor {
    /// Answers carry this number; those for an earlier editor are ignored.
    pub(super) session: u64,
    pub(super) id: String,
    pub(super) path: PathBuf,
    pub(super) art_seed: String,
    pub(super) audiobook: bool,
    pub(super) load: EditorLoad,
    /// What the file held when it was read, and what is being edited.
    pub(super) loaded: tagwrite::EditableTags,
    pub(super) fields: tagwrite::EditableTags,
    pub(super) current_art: Option<egui::TextureHandle>,
    pub(super) art_note: String,
    pub(super) art: EditorArt,
    pub(super) album_ids: Vec<String>,
    pub(super) album_skipped: usize,
    pub(super) apply_to_album: bool,
    /// The rows that were selected when the editor opened (several rows only,
    /// else empty): an album batch is pre-checked when they are the whole album.
    pub(super) selected_when_opened: Vec<String>,
    /// Files finished and files in all while a save runs.
    pub(super) saving: Option<(usize, usize)>,
    pub(super) error: Option<String>,
}

impl TagEditor {
    pub(super) fn new(session: u64, track: &Track) -> Self {
        Self {
            session,
            id: track.id.clone(),
            path: track.path.clone(),
            art_seed: art_seed(track).to_owned(),
            audiobook: track.audiobook,
            load: EditorLoad::Loading,
            loaded: tagwrite::EditableTags::default(),
            fields: tagwrite::EditableTags::default(),
            current_art: None,
            art_note: String::new(),
            art: EditorArt::Keep,
            album_ids: Vec::new(),
            album_skipped: 0,
            apply_to_album: false,
            selected_when_opened: Vec::new(),
            saving: None,
            error: None,
        }
    }
    /// The line under the artwork box.
    pub(super) fn art_line(&self) -> String {
        match (&self.art, &self.load) {
            (_, EditorLoad::Loading) => "Artwork: reading…".to_owned(),
            (EditorArt::Remove, _) => "Artwork: will be removed when you save".to_owned(),
            (EditorArt::Replace { name, .. }, _) => {
                format!("Artwork: new image {name} (saved when you press Save)")
            }
            (EditorArt::Keep, _) => {
                let mut line = format!("Artwork: {}", self.art_note);
                if self.loaded.artwork_count > 1 {
                    line.push_str(&format!(
                        " · {} pictures in the file, saving a new one keeps a single front cover",
                        self.loaded.artwork_count
                    ));
                }
                line
            }
        }
    }
}

/// How a tag save ended.
#[derive(Default)]
pub(super) struct TagSummary {
    /// Files the job tried.
    pub(super) total: usize,
    pub(super) saved: usize,
    /// Other files that already had the values.
    pub(super) unchanged: usize,
    pub(super) failed: usize,
    /// Other files left out because they cannot be edited or are playing.
    pub(super) skipped: usize,
    pub(super) first_error: Option<String>,
    /// The edited file itself failed (the other files were then not touched).
    pub(super) primary_error: Option<String>,
}

/// The files of one tag save.
pub(super) struct TagJob {
    pub(super) primary: Track,
    pub(super) edit: tagwrite::TagEdit,
    pub(super) others: Vec<Track>,
    /// The part of `edit` that is applied to `others`; `None`: no other files.
    pub(super) other_edit: Option<tagwrite::TagEdit>,
    pub(super) skipped: usize,
}

/// Whether the editor's fields or artwork differ from what the file held.
pub(super) fn tag_fields_changed(
    loaded: &tagwrite::EditableTags,
    fields: &tagwrite::EditableTags,
    art: &EditorArt,
) -> bool {
    !matches!(art, EditorArt::Keep)
        || [
            (&fields.title, &loaded.title),
            (&fields.artist, &loaded.artist),
            (&fields.album, &loaded.album),
            (&fields.album_artist, &loaded.album_artist),
            (&fields.composer, &loaded.composer),
            (&fields.genre, &loaded.genre),
            (&fields.year, &loaded.year),
            (&fields.comment, &loaded.comment),
            (&fields.track, &loaded.track),
            (&fields.track_total, &loaded.track_total),
            (&fields.disc, &loaded.disc),
            (&fields.disc_total, &loaded.disc_total),
        ]
        .iter()
        .any(|(new, old)| new != old)
}

/// The edit that turns what the file held into what the editor shows: only
/// the changed fields are set (an emptied field removes the tag).
pub(super) fn tag_edit_from(
    loaded: &tagwrite::EditableTags,
    fields: &tagwrite::EditableTags,
    art: &EditorArt,
) -> tagwrite::TagEdit {
    let changed = |new: &String, old: &String| (new != old).then(|| new.clone());
    tagwrite::TagEdit {
        title: changed(&fields.title, &loaded.title),
        artist: changed(&fields.artist, &loaded.artist),
        album: changed(&fields.album, &loaded.album),
        album_artist: changed(&fields.album_artist, &loaded.album_artist),
        composer: changed(&fields.composer, &loaded.composer),
        genre: changed(&fields.genre, &loaded.genre),
        year: changed(&fields.year, &loaded.year),
        comment: changed(&fields.comment, &loaded.comment),
        track: changed(&fields.track, &loaded.track),
        track_total: changed(&fields.track_total, &loaded.track_total),
        disc: changed(&fields.disc, &loaded.disc),
        disc_total: changed(&fields.disc_total, &loaded.disc_total),
        artwork: match art {
            EditorArt::Keep => None,
            EditorArt::Remove => Some(tagwrite::ArtworkEdit::Remove),
            EditorArt::Replace { bytes, .. } => Some(tagwrite::ArtworkEdit::Replace(bytes.clone())),
        },
    }
}

pub(super) fn tag_edit_is_empty(edit: &tagwrite::TagEdit) -> bool {
    [
        &edit.title,
        &edit.artist,
        &edit.album,
        &edit.album_artist,
        &edit.composer,
        &edit.genre,
        &edit.year,
        &edit.comment,
        &edit.track,
        &edit.track_total,
        &edit.disc,
        &edit.disc_total,
    ]
    .iter()
    .all(|field| field.is_none())
        && matches!(edit.artwork, None | Some(tagwrite::ArtworkEdit::Keep))
}

/// The part of `edit` that belongs to every file of an album: artist, album
/// artist, album, genre, year and the artwork. `None` when none of those changed.
pub(super) fn album_edit_from(edit: &tagwrite::TagEdit) -> Option<tagwrite::TagEdit> {
    let shared = tagwrite::TagEdit {
        artist: edit.artist.clone(),
        album_artist: edit.album_artist.clone(),
        album: edit.album.clone(),
        genre: edit.genre.clone(),
        year: edit.year.clone(),
        artwork: edit
            .artwork
            .clone()
            .filter(|art| *art != tagwrite::ArtworkEdit::Keep),
        ..tagwrite::TagEdit::default()
    };
    (!tag_edit_is_empty(&shared)).then_some(shared)
}

/// The other files of `track`'s album: same folder, same album text, not an
/// earlier version of a file. At most `MAX_ALBUM_MATES`.
/// Whether the files selected in the list are exactly the file `id` and its
/// album mates `album_ids`: then "Apply to the whole album" is the same as
/// "apply to the selected files" and is checked from the start.
pub(super) fn selection_is_whole_album(
    id: &str,
    album_ids: &[String],
    selected: &[String],
) -> bool {
    if selected.len() < 2 || album_ids.is_empty() {
        return false;
    }
    let album: HashSet<&str> = album_ids
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(id))
        .collect();
    let chosen: HashSet<&str> = selected.iter().map(String::as_str).collect();
    album == chosen
}

pub(super) fn album_mates(library: &[Track], track: &Track) -> Vec<Track> {
    let album = track.album.trim();
    let Some(folder) = track.path.parent().filter(|_| !album.is_empty()) else {
        return Vec::new();
    };
    library
        .iter()
        .filter(|other| {
            other.id != track.id
                && !other.superseded
                && other.album.trim() == album
                && other.path.parent() == Some(folder)
        })
        .take(MAX_ALBUM_MATES)
        .cloned()
        .collect()
}

/// Why the file of `id` cannot be rewritten right now, if it cannot: the player
/// is still opening it. (A loaded file is not a blocker: on Windows the engine
/// is asked to let go of it first, see `tag_release_target`.)
pub(super) fn tag_save_blocker(
    id: &str,
    current: Option<&str>,
    loading: bool,
) -> Option<&'static str> {
    (loading && current == Some(id))
        .then_some("The player is opening this file. Try again in a moment.")
}

/// What the player says when the audio engine could not close the loaded file.
pub(super) const TAG_FILE_HELD_NOTE: &str = "Windows keeps the file open while the player has it loaded. Play a different file first, then save.";

/// How long a tag save waits for the engine to close the loaded file.
pub(super) const TAG_RELEASE_TIMEOUT: Duration = Duration::from_secs(2);

/// The id of the file the engine holds open that the tag save must replace, if
/// the save writes one: the loaded file, or the preloaded one that follows it
/// (the engine closes both when it is asked to let go). Only Windows keeps a
/// file open against replacement, and a track that already ended holds nothing
/// (nor is a next file waiting then).
pub(super) fn tag_release_target<'a>(
    windows: bool,
    loaded: Option<&'a str>,
    ended: bool,
    preloaded: Option<&'a str>,
    job_ids: &[&str],
) -> Option<&'a str> {
    let loaded = loaded.filter(|_| windows && !ended)?;
    [Some(loaded), preloaded]
        .into_iter()
        .flatten()
        .find(|held| job_ids.contains(held))
}

/// A track the tag save made the engine let go of; it is loaded again, at the
/// position it had, once the save is over.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct TagResume {
    pub(super) id: String,
    pub(super) position: f64,
    /// Playback was on (or was asked for during the save).
    pub(super) play: bool,
}

/// The note that ends a tag save.
pub(super) fn tag_save_note(summary: &TagSummary) -> String {
    if summary.total <= 1 && summary.skipped == 0 && summary.failed == 0 {
        return "Saved the file's tags".to_owned();
    }
    let mut note = format!("Saved {} of {} files", summary.saved, summary.total);
    if summary.unchanged > 0 {
        note.push_str(&format!("; {} already had these values", summary.unchanged));
    }
    if summary.skipped > 0 {
        note.push_str(&format!(
            "; {} skipped (cannot be edited or playing now)",
            summary.skipped
        ));
    }
    if summary.failed > 0 {
        note.push_str(&format!("; {} failed", summary.failed));
        if let Some(error) = &summary.first_error {
            note.push_str(&format!(" (first error: {error})"));
        }
    }
    note.push('.');
    note
}

/// Writes the job's files one by one, each safely on its own, and reports each
/// finished file (`done` of `total`) to `report`. The edited file goes first;
/// if it fails the other files are not touched. A failure among the others is
/// counted and the rest continue.
pub(super) fn run_tag_job(
    job: TagJob,
    mut report: impl FnMut(Result<Track, String>, usize, usize),
) -> TagSummary {
    let others: &[Track] = if job.other_edit.is_some() {
        &job.others
    } else {
        &[]
    };
    let total = 1 + others.len();
    let mut summary = TagSummary {
        total,
        skipped: job.skipped,
        ..TagSummary::default()
    };
    let write = |track: &Track, edit: &tagwrite::TagEdit| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            media::write_tags_and_refresh(track, edit)
        }))
        .unwrap_or_else(|_| {
            Err("An unexpected problem stopped the tag writer for this file. Scan the folder to refresh it."
                .to_owned())
        })
    };
    let result = write(&job.primary, &job.edit);
    match &result {
        Ok(_) => summary.saved += 1,
        Err(error) => summary.primary_error = Some(error.clone()),
    }
    report(result, 1, total);
    if summary.primary_error.is_some() {
        return summary;
    }
    if let Some(edit) = &job.other_edit {
        for (index, other) in others.iter().enumerate() {
            let result = write(other, edit);
            match &result {
                Ok(_) => summary.saved += 1,
                Err(error) if error.starts_with(media::NOTHING_TO_CHANGE) => {
                    summary.unchanged += 1;
                }
                Err(error) => {
                    summary.failed += 1;
                    if summary.first_error.is_none() {
                        let name = other
                            .path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        summary.first_error = Some(format!("{name}: {error}"));
                    }
                }
            }
            report(result, index + 2, total);
        }
    }
    summary
}

/// Replaces the library record of a re-tagged file by its refreshed record
/// (which carries the old id, see `media::carry_identity`) and returns the
/// record it replaced. `None`, and nothing changed, when the library no longer
/// has that id: the file is never added a second time. Listening position,
/// bookmarks, ratings, favourites, queue and playlists point at the id, so they
/// stay where they were.
pub(super) fn apply_carried_record(state: &mut Session, new: Track) -> Option<Track> {
    let index = state.library.iter().position(|track| track.id == new.id)?;
    let old = std::mem::replace(&mut Arc::make_mut(&mut state.library)[index], new.clone());
    if let Some(progress) = state.progress.get_mut(&new.id) {
        progress.file_size = new.file_size;
        progress.modified = new.modified;
    }
    preserve_book_speed(&mut state.book_speeds, &old, &new);
    Some(old)
}

/// Reads what the editor shows, on a worker: the file's tags, its embedded
/// cover and how many other files of the album can be edited too.
pub(super) fn load_tag_editor(
    path: &std::path::Path,
    mates: &[Track],
) -> Result<TagEditorData, String> {
    let tags = tagwrite::read_editable_tags(path)?;
    let (art, art_note) = if tags.has_artwork {
        let shown = media::embedded_artwork(path)
            .and_then(|bytes| bytes.ok_or_else(|| "no picture".to_owned()))
            .and_then(|bytes| decode_image_bytes(&bytes))
            .map(|image| square_thumbnail(&image, DETAILS_ART_PIXELS));
        match shown {
            Ok(pixels) => (Some(pixels), "embedded picture".to_owned()),
            Err(error) => (
                None,
                format!(
                    "embedded picture (cannot be shown: {})",
                    short_error(&error)
                ),
            ),
        }
    } else {
        (None, "none in the file".to_owned())
    };
    let (mut album_ids, mut album_skipped) = (Vec::new(), 0);
    if tags.writable {
        for mate in mates {
            match tagwrite::read_editable_tags(&mate.path) {
                Ok(other) if other.writable => album_ids.push(mate.id.clone()),
                _ => album_skipped += 1,
            }
        }
    }
    Ok(TagEditorData {
        tags,
        art,
        art_note,
        album_ids,
        album_skipped,
    })
}

/// Reads and decodes a picked cover image within fixed limits.
pub(super) fn read_picked_image(path: &std::path::Path) -> Result<ChosenArtwork, String> {
    use std::io::Read;
    let file =
        std::fs::File::open(path).map_err(|error| format!("Cannot open the image: {error}"))?;
    let mut bytes = Vec::new();
    file.take(MAX_PICKED_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Cannot read the image: {error}"))?;
    if bytes.len() as u64 > MAX_PICKED_IMAGE_BYTES {
        return Err("The image file is larger than 32 MiB.".to_owned());
    }
    if bytes.is_empty() {
        return Err("The image file is empty.".to_owned());
    }
    let image = decode_image_bytes(&bytes).map_err(|error| {
        format!(
            "This is not an image BS Player can read ({}).",
            short_error(&error)
        )
    })?;
    Ok(ChosenArtwork {
        preview: square_thumbnail(&image, DETAILS_ART_PIXELS),
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        bytes,
    })
}

/// Puts the library tags typed in File details (those that differ from `before`,
/// the record they were typed over) onto `fresh`, the record just read from the
/// file; every other field keeps what the file says.
pub(super) fn keep_typed_tags(fresh: &mut Track, typed: &Track, before: &Track) {
    if typed.title != before.title {
        fresh.title = typed.title.clone();
    }
    if typed.artist != before.artist {
        fresh.artist = typed.artist.clone();
    }
    if typed.album != before.album {
        fresh.album = typed.album.clone();
    }
    if typed.genre != before.genre {
        fresh.genre = typed.genre.clone();
    }
    if typed.year != before.year {
        fresh.year = typed.year.clone();
    }
    if typed.comment != before.comment {
        fresh.comment = typed.comment.clone();
    }
    if typed.track_number != before.track_number {
        fresh.track_number = typed.track_number;
    }
    if typed.audiobook != before.audiobook {
        fresh.audiobook = typed.audiobook;
    }
}

impl PlayerApp {
    /// Opens the Edit file tags window for the library record `id`.
    pub(super) fn open_tag_editor(&mut self, id: &str) {
        let Some(track) = self.state.track(id).cloned() else {
            return;
        };
        if let Some(open) = &self.tag_editor {
            if open.id == id {
                return;
            }
            if open.saving.is_some() || tag_fields_changed(&open.loaded, &open.fields, &open.art) {
                self.set_status("Save or cancel the open tag editor first.");
                return;
            }
        }
        self.tag_session = self.tag_session.wrapping_add(1);
        let session = self.tag_session;
        let mates = album_mates(&self.state.library, &track);
        let path = track.path.clone();
        let tx = self.job_sender();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                load_tag_editor(&path, &mates)
            }))
            .unwrap_or_else(|_| {
                Err("The tags of this file could not be read (unexpected problem).".to_owned())
            });
            tx.send(Event::TagEditorLoaded { session, result });
        });
        let mut editor = TagEditor::new(session, &track);
        if self.selection.len() > 1 && self.selection.contains(id) {
            editor.selected_when_opened = (*self.selected_rows()).clone();
        }
        self.tag_editor = Some(editor);
    }

    pub(super) fn tag_save_blocker(&self, id: &str) -> Option<&'static str> {
        tag_save_blocker(
            id,
            self.state.current.as_deref(),
            self.loading_request.is_some() || self.snapshot.busy,
        )
    }

    /// Windows keeps the loaded file open, and the engine may hold the next file
    /// open too (a gapless preload), so when the save rewrites either the engine
    /// is asked to close them (pause with a short fade, keep the track and position;
    /// the preload is dropped and asked for again later). `Ok(Some(..))`: released,
    /// the loaded track is loaded again after the save; `Ok(None)`: nothing to
    /// release; `Err(id)`: the engine did not let go of that file within
    /// `TAG_RELEASE_TIMEOUT`. Blocks the UI thread for at most that long; usually
    /// a few milliseconds.
    pub(super) fn release_for_tag_save(
        &mut self,
        windows: bool,
        job_ids: &[&str],
    ) -> Result<Option<TagResume>, String> {
        let Some(held) = tag_release_target(
            windows,
            self.snapshot.loaded.as_deref(),
            self.snapshot.ended,
            self.preloaded_file(),
            job_ids,
        )
        .map(str::to_owned) else {
            return Ok(None);
        };
        // What resumes is the loaded track, even when the file the save needs is
        // the one that was to follow it.
        let resume = TagResume {
            position: self.position(),
            play: self.playback_intent(),
            id: self.snapshot.loaded.clone().unwrap_or_else(|| held.clone()),
        };
        // The engine drops its preload when it lets go; the request is forgotten
        // either way, and the wish makes a new one after the save.
        self.preload = None;
        if self.audio.release_file(TAG_RELEASE_TIMEOUT) {
            Ok(Some(resume))
        } else {
            Err(held)
        }
    }

    /// The tag save is over: the track it released plays on from where it was,
    /// unless another file was chosen meanwhile.
    pub(super) fn resume_after_tag_save(&mut self) {
        let Some(resume) = self.tag_resume.take() else {
            return;
        };
        if self.state.current.as_deref() != Some(resume.id.as_str())
            || self.state.track(&resume.id).is_none()
        {
            return;
        }
        // A seek during the save moved the engine's saved position.
        let position = if self.snapshot.loaded.as_deref() == Some(resume.id.as_str()) {
            self.position()
        } else {
            resume.position
        };
        let selection = self.state.selected.clone();
        self.load_track(&resume.id, resume.play, Some(position));
        self.state.selected = selection;
        self.scroll_to_current = false;
    }

    /// Starts writing the editor's changes (and, when asked, the shared album
    /// fields of the other files) on a worker. Each finished file comes back as
    /// an event that updates its library record in place.
    pub(super) fn start_tag_save(&mut self, editor: &mut TagEditor) {
        editor.error = None;
        if self.tag_save_running {
            editor.error = Some("Another tag save is still running.".to_owned());
            return;
        }
        let Some(track) = self.state.track(&editor.id).cloned() else {
            editor.error = Some("This file is no longer in the library.".to_owned());
            return;
        };
        let edit = tag_edit_from(&editor.loaded, &editor.fields, &editor.art);
        if tag_edit_is_empty(&edit) {
            return;
        }
        if let Some(reason) = self.tag_save_blocker(&track.id) {
            editor.error = Some(reason.to_owned());
            return;
        }
        let other_edit = if editor.apply_to_album {
            album_edit_from(&edit)
        } else {
            None
        };
        let (mut others, mut skipped) = (Vec::new(), 0);
        if other_edit.is_some() {
            skipped = editor.album_skipped;
            let wanted: HashSet<&str> = editor.album_ids.iter().map(String::as_str).collect();
            for other in self
                .state
                .library
                .iter()
                .filter(|other| wanted.contains(other.id.as_str()) && !other.superseded)
            {
                if self.tag_save_blocker(&other.id).is_some() {
                    skipped += 1;
                } else {
                    others.push(other.clone());
                }
            }
        }
        // The loaded file is written too: have the engine let go of it first.
        let mut job_ids: Vec<&str> = vec![track.id.as_str()];
        job_ids.extend(others.iter().map(|other| other.id.as_str()));
        match self.release_for_tag_save(cfg!(windows), &job_ids) {
            Ok(resume) => self.tag_resume = resume,
            Err(held) if held == track.id => {
                editor.error = Some(TAG_FILE_HELD_NOTE.to_owned());
                return;
            }
            Err(held) => {
                others.retain(|other| other.id != held);
                skipped += 1;
            }
        }
        let total = 1 + others.len();
        let job = TagJob {
            primary: track,
            edit,
            others,
            other_edit,
            skipped,
        };
        self.tag_save_running = true;
        self.busy += 1;
        editor.saving = Some((0, total));
        self.set_status(if total > 1 {
            format!("Saving {total} files…")
        } else {
            "Saving the file's tags…".to_owned()
        });
        let tx = self.job_sender();
        let session = editor.session;
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_tag_job(job, |result, done, total| {
                    tx.send(Event::TagFileSaved {
                        session,
                        result,
                        done,
                        total,
                    })
                })
            }));
            let summary = outcome.unwrap_or_else(|_| TagSummary {
                total,
                failed: 1,
                primary_error: Some(
                    "The tag editor stopped unexpectedly. Scan the folder to refresh the library."
                        .to_owned(),
                ),
                ..TagSummary::default()
            });
            tx.send(Event::TagSaveFinished { session, summary });
        });
    }

    /// A file of a tag save was written and re-read: its record replaces the
    /// library's in place, and everything drawn from the old bytes is dropped.
    pub(super) fn apply_saved_tags(&mut self, new: Track) {
        let Some(old) = apply_carried_record(&mut self.state, new.clone()) else {
            return;
        };
        self.invalidate_library();
        // The artwork pane, the File details cover and the row thumbnails of
        // the old and the new album key were decoded from the old bytes.
        self.art_requested = None;
        self.details_art = None;
        for key in [ThumbKey::of(&old), ThumbKey::of(&new)] {
            self.thumbs.cache.remove(&key);
        }
        // File details may be open on this file: show what it holds now, and
        // keep library tags typed there but not yet saved.
        if let Some(details) = self.details.as_mut().filter(|d| d.id == new.id) {
            let typed = tags_differ(&old, details).then(|| details.clone());
            *details = new;
            if let Some(typed) = typed {
                keep_typed_tags(details, &typed, &old);
            }
        }
    }

    /// The tag save has ended: close the editor and report, or show the error
    /// of the edited file in the editor.
    pub(super) fn finish_tag_save(&mut self, session: u64, summary: TagSummary) {
        // Whether the save worked or not, the released track plays on.
        self.resume_after_tag_save();
        let mine = self
            .tag_editor
            .as_ref()
            .is_some_and(|editor| editor.session == session);
        if let Some(error) = summary.primary_error {
            self.set_status(short_error(&error));
            if let Some(editor) = self.tag_editor.as_mut().filter(|_| mine) {
                editor.saving = None;
                editor.error = Some(error);
            } else {
                self.add_message(error);
            }
            return;
        }
        if mine {
            self.tag_editor = None;
        }
        let note = tag_save_note(&summary);
        self.set_status(note.clone());
        if summary.failed > 0 || summary.skipped > 0 {
            self.add_message(note);
        }
        self.save();
    }

    /// Asks for an image file, then decodes it for the editor's preview.
    pub(super) fn choose_tag_artwork(&mut self, session: u64) {
        let Some(guard) = self.begin_picker() else {
            return;
        };
        let tx = self.job_sender();
        std::thread::spawn(move || {
            let _guard = guard;
            let picked = rfd::FileDialog::new()
                .set_title("Choose a cover image")
                .add_filter("Images", &["jpg", "jpeg", "png", "webp", "bmp", "gif"])
                .pick_file();
            if let Some(path) = picked {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    read_picked_image(&path)
                }))
                .unwrap_or_else(|_| {
                    Err("The image could not be read (unexpected problem).".into())
                });
                tx.send(Event::TagArtworkChosen { session, result });
            }
        });
    }

    /// Saves the cover of `track` as a PNG the user names.
    pub(super) fn export_artwork(&mut self, track: Track, prefer_embedded: bool) {
        if let Some(guard) = self.begin_picker() {
            let tx = self.job_sender();
            std::thread::spawn(move || {
                let _guard = guard;
                // Decoded within the same limits as the panel, with the same
                // fallback to the other artwork source.
                match decode_cover(&track, prefer_embedded) {
                    Ok(image) => {
                        if let Some(path) = rfd::FileDialog::new()
                            .set_file_name("cover.png")
                            .save_file()
                        {
                            let result = image.save(&path).map_err(|error| error.to_string());
                            let _ =
                                tx.send(Event::Message(
                                    result.map(|_| "Artwork exported".into()).unwrap_or_else(
                                        |error| format!("Artwork export failed: {error}"),
                                    ),
                                ));
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Event::Message(format!("Artwork export failed: {error}")));
                    }
                }
            });
        }
    }

    /// Moves the saved history of an earlier version of a file (`old_id`) to
    /// the current version at the same path (`new_id`) and removes the earlier
    /// entry.
    pub(super) fn carry_progress_to_current(&mut self, old_id: &str, new_id: &str) {
        let (Some(old), Some(new)) = (
            self.state.track(old_id).cloned(),
            self.state.track(new_id).cloned(),
        ) else {
            self.set_status("One of the two entries is no longer in the library.");
            return;
        };
        if !old.superseded || new.superseded || old.path != new.path {
            return;
        }
        self.checkpoint();
        preserve_book_speed(&mut self.state.book_speeds, &old, &new);
        let current_moved = self.state.merge_track_history(old_id, new_id);
        if current_moved {
            // The player holds the file under the old id; load it again under the new one.
            self.cancel_validation();
            self.audio.pause();
            self.state.was_playing = false;
            self.loading_request = None;
            self.pending_play_intent = None;
            self.expected_audio_request = None;
            self.pending_position = None;
            self.snapshot = Snapshot::default();
        }
        if self
            .details
            .as_ref()
            .is_some_and(|details| details.id == old_id)
        {
            self.details = None;
            self.details_art = None;
        }
        self.invalidate_library();
        self.progress_changed();
        self.set_status("Progress moved to the current version of the file");
        self.save();
    }

    /// The Edit file tags window.
    pub(super) fn show_tag_editor(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.tag_editor.take() else {
            return;
        };
        let (mut open, mut close) = (true, false);
        let (mut save, mut choose, mut export) = (false, false, false);
        let ready = matches!(editor.load, EditorLoad::Ready);
        let saving = editor.saving.is_some() || self.tag_save_running;
        let editable = ready && editor.loaded.writable && !saving;
        inspector_window(ctx, "File details — edit file")
            .open(&mut open)
            .show(ctx, |ui| {
                let tab_id = ui.id().with(("file-editor-tab", editor.session));
                let mut tab = ui.data_mut(|data| data.get_temp::<usize>(tab_id).unwrap_or(0));
                let body_height = inspector_body_height(ui, if saving { 4. } else { 3. });
                egui::ScrollArea::vertical()
                    .id_salt(("file-editor-body", editor.session))
                    .auto_shrink([false, false])
                    .max_height(body_height)
                    .show(ui, |ui| {
                        let title = if editor.fields.title.is_empty() {
                            editor.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| "Audio file".into())
                        } else { editor.fields.title.clone() };
                        ui.add(egui::Label::new(RichText::new(title).heading().strong()).wrap());
                        ui.add(egui::Label::new(RichText::new(media::display_path(&editor.path)).small().weak()).wrap());
                        if self.details.as_ref().is_some_and(|details| details.id == editor.id) {
                            ui.label(RichText::new("Your library-tag draft is kept when you return.").small().weak());
                        }
                        ui.add_space(6.);
                        ui.horizontal_wrapped(|ui| {
                            ui.selectable_value(&mut tab, 0, "Information");
                            ui.selectable_value(&mut tab, 1, "Artwork");
                        });
                        ui.separator();
                match &editor.load {
                    EditorLoad::Loading => {
                        ui.label("Reading the file…");
                    }
                    EditorLoad::Failed(error) => {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("The tags could not be read: {error}"))
                                    .color(ui.visuals().error_fg_color),
                            )
                            .wrap(),
                        );
                    }
                    EditorLoad::Ready => {
                        if let Some(reason) =
                            editor.loaded.read_only_reason.as_ref().filter(|_| !editor.loaded.writable)
                        {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(format!("Read-only: {reason}"))
                                        .color(ui.visuals().warn_fg_color),
                                )
                                .wrap(),
                            );
                        }
                    }
                }

                if let Some(error) = &editor.error {
                    ui.add(
                        egui::Label::new(
                            RichText::new(error).color(ui.visuals().error_fg_color),
                        )
                        .wrap(),
                    );
                }
                        if tab == 1 {
                let artwork_layout = if ui.available_width() < 540. {
                    egui::Layout::top_down(egui::Align::Min)
                } else {
                    egui::Layout::left_to_right(egui::Align::Min)
                };
                ui.with_layout(artwork_layout, |ui| {
                    let (rect, _) = ui
                        .allocate_exact_size(Vec2::splat(ui.available_width().min(200.)), egui::Sense::hover());
                    let shown = match &editor.art {
                        EditorArt::Replace { texture, .. } => Some(texture),
                        EditorArt::Remove => None,
                        EditorArt::Keep => editor.current_art.as_ref(),
                    };
                    match shown {
                        Some(texture) => {
                            egui::Image::new(texture)
                                .corner_radius(widgets::art_corner_radius(rect.width()))
                                .paint_at(ui, rect);
                        }
                        None => widgets::paint_default_art(
                            ui.painter(),
                            rect,
                            editor.audiobook,
                            &editor.art_seed,
                            ui.visuals().dark_mode,
                        ),
                    }
                    ui.vertical(|ui| {
                        if ready {
                            ui.label(format!("Format: {}", editor.loaded.format));
                        }
                        ui.add(
                            egui::Label::new(
                                RichText::new(editor.art_line())
                                    .small()
                                    .color(ui.visuals().weak_text_color()),
                            )
                            .wrap(),
                        );
                        ui.horizontal_wrapped(|ui| {
                            if ui
                                .add_enabled(editable, egui::Button::new("Choose image…"))
                                .clicked()
                            {
                                choose = true;
                            }
                            let can_remove = editable
                                && match &editor.art {
                                    EditorArt::Replace { .. } => true,
                                    EditorArt::Keep => editor.loaded.has_artwork,
                                    EditorArt::Remove => false,
                                };
                            if ui
                                .add_enabled(can_remove, egui::Button::new("Remove artwork"))
                                .clicked()
                            {
                                editor.art = if editor.loaded.has_artwork {
                                    EditorArt::Remove
                                } else {
                                    EditorArt::Keep
                                };
                            }
                            if ui
                                .add_enabled(
                                    ready && editor.loaded.has_artwork && !saving,
                                    egui::Button::new("Export artwork…"),
                                )
                                .clicked()
                            {
                                export = true;
                            }
                            if editable
                                && !matches!(editor.art, EditorArt::Keep)
                                && ui.button("Undo artwork change").clicked()
                            {
                                editor.art = EditorArt::Keep;
                            }
                        });
                    });
                });

                        } else {
                            ui.add_enabled_ui(editable, |ui| {
                                let fields = &mut editor.fields;
                                inspector_text_fields(ui, &mut [
                                    ("Title", &mut fields.title), ("Artist", &mut fields.artist),
                                    ("Album", &mut fields.album), ("Album artist", &mut fields.album_artist),
                                    ("Composer", &mut fields.composer), ("Genre", &mut fields.genre),
                                    ("Year", &mut fields.year),
                                ]);
                                ui.horizontal_wrapped(|ui| {
                                    for (label, number, total) in [
                                        ("Track", &mut fields.track, &mut fields.track_total),
                                        ("Disc", &mut fields.disc, &mut fields.disc_total),
                                    ] {
                                        ui.label(label);
                                        ui.add(egui::TextEdit::singleline(number).desired_width(42.));
                                        ui.label("of");
                                        ui.add(egui::TextEdit::singleline(total).desired_width(42.));
                                    }
                                });
                                ui.label("Comment");
                                ui.add(egui::TextEdit::multiline(&mut fields.comment).desired_rows(2).desired_width(f32::INFINITY));
                            });
                        }
                if !editor.album_ids.is_empty() {
                    ui.separator();
                    let count = editor.album_ids.len();
                    ui.add_enabled(
                        editable,
                        egui::Checkbox::new(
                            &mut editor.apply_to_album,
                            format!(
                                "Also apply Artist, Album artist, Album, Genre, Year and artwork to the other {count} file{} of this album",
                                if count == 1 { "" } else { "s" }
                            ),
                        ),
                    );
                    if editor.album_skipped > 0 {
                        ui.label(
                            RichText::new(format!(
                                "{} other file(s) of this album cannot be edited and are skipped.",
                                editor.album_skipped
                            ))
                            .small()
                            .color(ui.visuals().weak_text_color()),
                        );
                    }
                }

                    });
                ui.data_mut(|data| data.insert_temp(tab_id, tab));
                ui.separator();
                ui.label(RichText::new("Save to file changes the audio file, including its embedded cover.").small().weak());
                if let Some((done, total)) = editor.saving {
                    ui.label(if total > 1 {
                        format!("Saving… {done} of {total} files done")
                    } else {
                        "Saving…".to_owned()
                    });
                }
                ui.horizontal_wrapped(|ui| {
                    let changed = tag_fields_changed(&editor.loaded, &editor.fields, &editor.art);
                    if ui
                        .add_enabled(editable && changed, egui::Button::new("Save to file"))
                        .clicked()
                    {
                        save = true;
                    }
                    if ui
                        .add_enabled(!saving, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        close = true;
                    }
                });

            });
        if save {
            self.start_tag_save(&mut editor);
        }
        if choose {
            self.choose_tag_artwork(editor.session);
        }
        if export {
            let track = Track {
                path: editor.path.clone(),
                ..Track::default()
            };
            self.export_artwork(track, true);
        }
        // A save in progress keeps its window; closing it would hide the result.
        if !close && (open || editor.saving.is_some()) {
            self.tag_editor = Some(editor);
        }
    }
}

/// Shared inspector shell: the content scrolls only when it does not fit; the
/// save boundary stays outside that scroll region and remains reachable.
pub(super) fn inspector_window<'a>(ctx: &egui::Context, title: &'a str) -> egui::Window<'a> {
    let bounds = ctx.content_rect().shrink(8.);
    let size = bounds.size().max(Vec2::splat(32.));
    egui::Window::new(title)
        .id(egui::Id::new("file-details-inspector"))
        .default_size([760_f32.min(size.x), 620_f32.min(size.y)])
        .min_size([360_f32.min(size.x), 180_f32.min(size.y)])
        .max_size(size)
        .constrain_to(bounds)
        .collapsible(false)
        .resizable(true)
        .scroll([false, false])
}

pub(super) fn inspector_body_height(ui: &egui::Ui, footer_rows: f32) -> f32 {
    let row = ui
        .text_style_height(&egui::TextStyle::Body)
        .max(ui.spacing().interact_size.y);
    (ui.available_height() - footer_rows * (row + ui.spacing().item_spacing.y) - 16.).max(1.)
}

/// Labels above fluid-width fields avoid a fixed label column squeezing values
/// at large text sizes. Narrow inspectors fall back to one column.
pub(super) fn inspector_text_fields(ui: &mut egui::Ui, fields: &mut [(&str, &mut String)]) {
    let columns = if ui.available_width() >= 520. { 2 } else { 1 };
    for row in fields.chunks_mut(columns) {
        ui.columns(columns, |uis| {
            for (ui, (label, value)) in uis.iter_mut().zip(row.iter_mut()) {
                ui.label(*label);
                ui.add(egui::TextEdit::singleline(*value).desired_width(f32::INFINITY));
            }
        });
        ui.add_space(3.);
    }
}
