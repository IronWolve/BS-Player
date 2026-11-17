use super::*;

impl PlayerApp {
    /// Makes `current_chapters` follow `track`, the file that is about to play.
    /// Nothing happens while they already are, or are being read from, this
    /// version of the file. Otherwise the chapters come from the cache or from a
    /// worker (one per load). The chapters of another file are dropped at once;
    /// those of an earlier version of the same file (after a tag save) stay
    /// until the new ones arrive.
    pub(super) fn ensure_chapters(&mut self, track: &Track) {
        let key = ChapterKey::of(track);
        if self.chapters_key.as_ref() == Some(&key) {
            return;
        }
        let same_file = self
            .chapters_key
            .as_ref()
            .is_some_and(|stored| stored.id == key.id);
        self.chapters_key = Some(key.clone());
        self.chapters_reading = false;
        if !wants_chapters(track) {
            self.show_chapters(Arc::default());
            return;
        }
        if let Some(cached) = self.chapter_cache.get(&key) {
            self.show_chapters(cached);
            return;
        }
        self.chapters_reading = true;
        if !same_file {
            self.show_chapters(Arc::default());
        }
        let tx = self.job_sender();
        let path = track.path.clone();
        let duration = track.duration;
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                chapters::read_chapters(&path, duration)
            }))
            .unwrap_or_else(|_| Err("The chapter reader stopped unexpectedly".to_owned()));
            tx.send(Event::Chapters { key, result });
        });
    }

    /// A worker's answer. It is dropped when the file (or its version) changed
    /// meanwhile; a good answer is kept in the cache either way.
    pub(super) fn chapters_read(&mut self, key: ChapterKey, result: Result<Vec<Chapter>, String>) {
        let list = result.ok().map(Arc::new);
        if let Some(list) = &list {
            self.chapter_cache.insert(key.clone(), Arc::clone(list));
        }
        if self.chapters_reading && self.chapters_key.as_ref() == Some(&key) {
            self.chapters_reading = false;
            if list.is_none() {
                // The file could not be read: the next load of it tries again.
                self.chapters_key = None;
            }
            self.show_chapters(list.unwrap_or_default());
        }
    }

    /// Installs the chapter list of the current file and drops what was worked
    /// out from the old one.
    pub(super) fn show_chapters(&mut self, chapters: Arc<Vec<Chapter>>) {
        self.chapter_picker_state.close(&self.gui_context);
        self.chapter_marks = chapters
            .iter()
            .map(|chapter| chapter.start)
            .filter(|start| *start > 0.)
            .collect();
        self.current_chapters = chapters;
        self.chapter_cursor = ChapterCursor::default();
        self.chapter_line = None;
        self.chapters_scroll = true;
        self.sleep_chapter_end = None;
        self.leave_empty_chapters_tab();
    }

    /// No chapters, and none being looked for.
    pub(super) fn clear_chapters(&mut self) {
        self.chapters_key = None;
        self.chapters_reading = false;
        self.show_chapters(Arc::default());
    }

    /// The Chapters tab only exists while the playing file has chapters (or they
    /// are being read); without them the list shows the files again.
    pub(super) fn leave_empty_chapters_tab(&mut self) {
        if self.state.tab == CHAPTERS_TAB
            && self.current_chapters.is_empty()
            && !self.chapters_reading
        {
            self.state.tab = "Files".into();
            self.state.scroll = 0.;
            self.restore_scroll = true;
        }
    }

    /// The chapters follow `state.current`, however it changed.
    pub(super) fn sync_chapters(&mut self) {
        let stale = self
            .chapters_key
            .as_ref()
            .is_some_and(|key| self.state.current.as_deref() != Some(key.id.as_str()));
        if stale {
            self.clear_chapters();
        }
    }

    /// A tab of the file list was chosen.
    pub(super) fn tab_chosen(&mut self, tab: &str) {
        if tab == CHAPTERS_TAB {
            self.chapters_scroll = true;
        }
        if tab == LYRICS_TAB {
            self.lyrics_view.recenter = true;
        }
    }

    /// Next / Previous chapter (Alt+Down / Alt+Up): seeks to the chapter start
    /// and keeps playing or staying paused.
    pub(super) fn step_chapter(&mut self, forward: bool) {
        let chapters = Arc::clone(&self.current_chapters);
        if chapters.is_empty() {
            self.set_brief_status(if self.chapters_reading {
                "Reading chapters…"
            } else {
                "No chapters in this file"
            });
            return;
        }
        let position = self.position();
        let target = if forward {
            match next_chapter_start(&chapters, position) {
                Some(start) => start,
                None => {
                    self.set_brief_status("Last chapter");
                    return;
                }
            }
        } else {
            match previous_chapter_start(&chapters, position) {
                Some(start) => start,
                None => return,
            }
        };
        let Some(id) = self.state.current.clone() else { return; };
        if !self.jump_to_chapter(&id, target) { return; }
        if let Some((index, chapter)) = chapters::chapter_at(&chapters, target)
            .and_then(|index| Some((index, chapters.get(index)?)))
        {
            self.set_brief_status(chapter_text(index, chapters.len(), &chapter.title));
        }
    }

    /// Seek within the current track to a chapter while retaining the user's
    /// current play/pause intent. Persist the destination after `seek`, whose
    /// checkpoint intentionally skips while the new position is pending.
    pub(super) fn jump_to_chapter(&mut self, id: &str, seconds: f64) -> bool {
        if self.state.current.as_deref() != Some(id)
            || self.state.current_station.is_some()
            || !seconds.is_finite()
        {
            return false;
        }
        if self.tag_resume.as_ref().is_some_and(|resume| resume.id == id) {
            self.set_brief_status("Wait for file tags to finish before seeking");
            return false;
        }
        self.cancel_playing_drop();
        self.seek(seconds);
        let Some((pending_id, position, _)) = self.pending_position.as_ref() else {
            return false;
        };
        if pending_id != id {
            return false;
        }
        if self.state.settings.remember_positions
            && !self.state.settings.private_listening
            && self.private_track.as_deref() != Some(id)
        {
            if let Some(track) = self.state.track(id).cloned() {
                let progress = self.state.progress.entry(id.to_owned()).or_default();
                progress.seconds = *position;
                progress.completed = false;
                progress.file_size = track.file_size;
                progress.modified = track.modified;
                self.progress_changed();
                // `save` calls checkpoint, which skips the pending seek above.
                self.save();
            }
        }
        true
    }

    /// Seek to a selected chapter and stop at its end. The last chapter uses
    /// the existing end-of-file sleep behavior.
    pub(super) fn sleep_at_chapter_end(&mut self, id: &str, index: usize) {
        if self.state.current.as_deref() != Some(id)
            || self.state.current_station.is_some()
            || index >= self.current_chapters.len()
        {
            return;
        }
        let chapters = Arc::clone(&self.current_chapters);
        let Some(chapter) = chapters.get(index) else {
            return;
        };
        if !self.jump_to_chapter(id, chapter.start) {
            return;
        }
        self.state.sleep_mode = "chapter".into();
        self.state.sleep_deadline = None;
        self.sleep_chapter_end = chapters.get(index + 1).map(|next| {
            chapter
                .end
                .filter(|end| *end > chapter.start)
                .unwrap_or(next.start)
                .min(next.start)
        });
        self.apply_volume();
        self.save();
    }

    /// The "end of chapter" sleep timer: pauses when the playing position reaches
    /// the end of the current chapter, which is looked up again every tick, so a
    /// seek moves the target. Returns the seconds of the file left until that end
    /// while the timer is armed and playing. Without chapters (or in the last
    /// one) nothing is done here: the end of the file stops playback as it always
    /// did. The check runs once per tick (about every 200 ms; `tick` wakes the
    /// player for the exact moment when the end is close).
    pub(super) fn chapter_sleep_tick(&mut self) -> Option<f64> {
        if self.state.sleep_mode != "chapter" || self.current_chapters.is_empty() {
            self.sleep_chapter_end = None;
            return None;
        }
        if !self.snapshot.playing
            || self.snapshot.busy
            || self.snapshot.loaded != self.state.current
            || self.pending_position.is_some()
            || self.loading_request.is_some()
        {
            return None;
        }
        let position = self.snapshot.position;
        let target = match self.sleep_chapter_end {
            Some(target) => target,
            None => {
                let target = chapter_sleep_target(&self.current_chapters, position)?;
                self.sleep_chapter_end = Some(target);
                target
            }
        };
        if position < target {
            return Some(target - position);
        }
        self.sleep_chapter_end = None;
        self.cancel_validation();
        self.audio.pause();
        self.state.was_playing = false;
        self.state.sleep_mode = "off".into();
        self.state.sleep_deadline = None;
        self.apply_volume();
        self.save();
        self.set_status("Sleep timer: chapter ended");
        None
    }

    /// The Chapters tab: number, title, start and length of every chapter, the
    /// current one highlighted. A click (or Enter on a focused row) seeks to the
    /// chapter start. Only the rows in view are drawn, so thousands are cheap.
    pub(super) fn chapters_ui(&mut self, ui: &mut egui::Ui) {
        const PADDING: f32 = 14.;
        const NUMBER_WIDTH: f32 = 44.;
        const TIME_WIDTH: f32 = 84.;
        let chapters = Arc::clone(&self.current_chapters);
        let Some(track_id) = self.state.current.clone().filter(|_| !chapters.is_empty()) else {
            ui.add_space(12.);
            ui.label(if self.chapters_reading {
                "Reading chapters…"
            } else {
                "The playing file has no chapters."
            });
            return;
        };
        let position = self.position();
        let current = self.chapter_cursor.locate(&chapters, position);
        let font = egui::TextStyle::Body.resolve(ui.style());
        let weak = ui.visuals().weak_text_color();
        let row_height = (ui.text_style_height(&egui::TextStyle::Body) + 12.).max(28.);
        // The same columns for the header and every row.
        let columns = |rect: egui::Rect| {
            let length_right = rect.right() - PADDING;
            let start_right = length_right - TIME_WIDTH - 10.;
            let title_right = start_right - TIME_WIDTH - 12.;
            let number_right = rect.left() + PADDING + NUMBER_WIDTH;
            let title_left = number_right + 12.;
            (
                number_right,
                title_left,
                (title_right - title_left).max(0.),
                start_right,
                length_right,
            )
        };
        let (header, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), (row_height - 6.).max(20.)),
            egui::Sense::hover(),
        );
        let (number_right, title_left, _, start_right, length_right) = columns(header);
        let small = egui::TextStyle::Small.resolve(ui.style());
        for (label, x, align) in [
            ("#", number_right, egui::Align2::RIGHT_CENTER),
            ("Title", title_left, egui::Align2::LEFT_CENTER),
            ("Start", start_right, egui::Align2::RIGHT_CENTER),
            ("Length", length_right, egui::Align2::RIGHT_CENTER),
        ] {
            ui.painter().text(
                egui::pos2(x, header.center().y),
                align,
                label,
                small.clone(),
                weak,
            );
        }
        ui.painter().hline(
            header.x_range(),
            header.bottom(),
            egui::Stroke::new(1., appearance::separator(ui)),
        );
        ui.spacing_mut().item_spacing.y = 2.;
        let pitch = row_height + ui.spacing().item_spacing.y;
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("chapter rows")
            .auto_shrink([false, false]);
        if std::mem::take(&mut self.chapters_scroll) {
            if let Some(index) = current {
                scroll = scroll.vertical_scroll_offset(chapter_scroll_offset(
                    index,
                    pitch,
                    ui.available_height(),
                ));
            }
        }
        let mut actions = Vec::new();
        let mut sleep_actions = Vec::new();
        scroll.show_rows(ui, row_height, chapters.len(), |ui, range| {
            for index in range {
                let Some(chapter) = chapters.get(index) else {
                    break;
                };
                let (rect, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), row_height),
                    egui::Sense::hover(),
                );
                // A click does not take the keyboard (Space must still pause); Tab
                // reaches a row and Enter on it seeks.
                let response = ui.interact(rect, chapter_row_id(index), egui::Sense::click());
                let is_current = current == Some(index);
                ui.painter().rect_filled(
                    rect,
                    5.,
                    appearance::row_fill(ui, is_current, index % 2 == 1),
                );
                if response.has_focus() {
                    ui.painter().rect_stroke(
                        rect.shrink(1.),
                        5.,
                        egui::Stroke::new(1.5, appearance::row_primary_outline(ui)),
                        egui::StrokeKind::Inside,
                    );
                }
                let color = appearance::row_text(ui, is_current);
                let (number_right, title_left, title_width, start_right, length_right) =
                    columns(rect);
                let y = rect.center().y;
                ui.painter().text(
                    egui::pos2(number_right, y),
                    egui::Align2::RIGHT_CENTER,
                    index + 1,
                    font.clone(),
                    color,
                );
                if title_width >= 24. {
                    let mut job = egui::text::LayoutJob::simple_singleline(
                        chapter.title.clone(),
                        font.clone(),
                        color,
                    );
                    job.wrap = egui::text::TextWrapping::truncate_at_width(title_width);
                    let galley = ui.painter().layout_job(job);
                    ui.painter().galley(
                        egui::pos2(title_left, y - galley.size().y / 2.),
                        galley,
                        color,
                    );
                }
                ui.painter().text(
                    egui::pos2(start_right, y),
                    egui::Align2::RIGHT_CENTER,
                    time_text(chapter.start),
                    font.clone(),
                    color,
                );
                if let Some(end) = chapter.end.filter(|end| *end > chapter.start) {
                    ui.painter().text(
                        egui::pos2(length_right, y),
                        egui::Align2::RIGHT_CENTER,
                        time_text(end - chapter.start),
                        font.clone(),
                        if is_current { color } else { weak },
                    );
                }
                if response.clicked() {
                    actions.push(Action::SeekChapter(track_id.clone(), chapter.start));
                }
                response
                    .on_hover_ui(|ui| {
                        ui.label(chapter_tooltip(index, &chapters));
                    })
                    .context_menu(|ui| {
                        if ui.button("Go to chapter start").clicked() {
                            actions.push(Action::SeekChapter(track_id.clone(), chapter.start));
                            ui.close();
                        }
                        if ui.button("Add bookmark at chapter start").clicked() {
                            actions.push(Action::ChapterBookmark(
                                track_id.clone(),
                                chapter.start,
                                chapter.title.clone(),
                            ));
                            ui.close();
                        }
                        if ui.button("Sleep at end of this chapter").clicked() {
                            sleep_actions.push((track_id.clone(), index));
                            ui.close();
                        }
                    });
            }
        });
        self.actions.extend(actions);
        for (id, index) in sleep_actions {
            self.sleep_at_chapter_end(&id, index);
        }
    }
}

// ---------------------------------------------------------------------------
// Chapters of the loaded file (single-file audiobooks, CUE sheets)
// ---------------------------------------------------------------------------

/// Files whose chapters stay in memory, newest last.
pub(super) const CHAPTER_CACHE_ENTRIES: usize = 16;

/// "Previous chapter" restarts the chapter when it is more than this many
/// seconds in; sooner than that it goes to the chapter before.
pub(super) const CHAPTER_RESTART_AFTER: f64 = 3.;

/// The "end of chapter" sleep wakes the player for the exact moment once the
/// chapter end is this close (seconds of real time).
pub(super) const CHAPTER_SLEEP_WAKE_WITHIN: f64 = 0.3;

/// The Chapters tab as the tab strip and the list menu name it.
pub(super) const CHAPTERS_TAB: &str = "Chapters";

/// Whether the chapter reader is tried for `track`: a supported format that can
/// carry chapters, never a live stream. Duration and audiobook metadata do not
/// determine whether a chapter-bearing file is inspected.
pub(super) fn wants_chapters(track: &Track) -> bool {
    if track.id.starts_with("stream:") {
        return false;
    }
    let extension = track
        .path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let Some(extension) = extension else {
        return false;
    };
    if !matches!(
        extension.as_str(),
        "m4b" | "m4a" | "mp4" | "mp3" | "flac" | "ogg" | "oga"
    ) {
        return false;
    }
    true
}

/// Which version of which file a chapter list was read from: a changed file (a
/// tag save, an edit by another program) is read again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ChapterKey {
    pub(super) id: String,
    pub(super) modified: u64,
    pub(super) nanos: u64,
}

impl ChapterKey {
    pub(super) fn of(track: &Track) -> Self {
        Self {
            id: track.id.clone(),
            modified: track.modified,
            nanos: track.modified_nanos.unwrap_or(0),
        }
    }
}

/// The chapter lists of the last few files, so playing a file again (or going
/// back to it) does not read it again. Memory only.
#[derive(Default)]
pub(super) struct ChapterCache {
    pub(super) entries: VecDeque<(ChapterKey, Arc<Vec<Chapter>>)>,
}

impl ChapterCache {
    pub(super) fn get(&self, key: &ChapterKey) -> Option<Arc<Vec<Chapter>>> {
        self.entries
            .iter()
            .find(|(stored, _)| stored == key)
            .map(|(_, chapters)| Arc::clone(chapters))
    }
    pub(super) fn insert(&mut self, key: ChapterKey, chapters: Arc<Vec<Chapter>>) {
        self.entries.retain(|(stored, _)| *stored != key);
        self.entries.push_back((key, chapters));
        while self.entries.len() > CHAPTER_CACHE_ENTRIES {
            self.entries.pop_front();
        }
    }
}

/// The index of the chapter the position is in, remembered between calls: the
/// search only runs again when the position leaves the chapter it found. File
/// time, so it does not depend on the playback speed.
#[derive(Clone, Copy, Debug)]
pub(super) struct ChapterCursor {
    pub(super) index: usize,
    /// The positions `from..to` are in chapter `index`.
    pub(super) from: f64,
    pub(super) to: f64,
}

impl Default for ChapterCursor {
    fn default() -> Self {
        // An empty range: nothing is remembered.
        Self {
            index: 0,
            from: 0.,
            to: 0.,
        }
    }
}

impl ChapterCursor {
    pub(super) fn locate(&mut self, chapters: &[Chapter], position: f64) -> Option<usize> {
        if !position.is_finite() {
            return None;
        }
        if position >= self.from && position < self.to && self.index < chapters.len() {
            return Some(self.index);
        }
        let found = chapters::chapter_at(chapters, position);
        *self = match found.and_then(|index| Some((index, chapters.get(index)?))) {
            Some((index, chapter)) => Self {
                index,
                from: chapter.start,
                // The last chapter owns every later position.
                to: match chapters.get(index + 1) {
                    Some(next) => chapter.end.map_or(next.start, |end| end.min(next.start)),
                    None => f64::INFINITY,
                },
            },
            None => Self::default(),
        };
        found
    }
}

/// `Chapter 7 of 31 · The Long Road`; a chapter with the default name
/// ("Chapter 7") is not said twice.
pub(super) fn chapter_text(index: usize, total: usize, title: &str) -> String {
    let number = index + 1;
    if title.is_empty() || title == format!("Chapter {number}") {
        format!("Chapter {number} of {total}")
    } else {
        format!("Chapter {number} of {total} · {title}")
    }
}

/// The tooltip of the chapter line: the full title with where it runs.
pub(super) fn chapter_tooltip(index: usize, chapters: &[Chapter]) -> String {
    let Some(chapter) = chapters.get(index) else {
        return String::new();
    };
    let span = match chapter.end {
        Some(end) if end > chapter.start => format!(
            "{} – {} ({})",
            time_text(chapter.start),
            time_text(end),
            time_text(end - chapter.start)
        ),
        _ => format!("from {}", time_text(chapter.start)),
    };
    format!(
        "{}\n{span}",
        chapter_text(index, chapters.len(), &chapter.title)
    )
}

/// Where "Next chapter" goes from `position`: the start of the chapter after
/// the current one (the first chapter's start while still before it). `None` at
/// the last chapter, or when there are no chapters.
pub(super) fn next_chapter_start(chapters: &[Chapter], position: f64) -> Option<f64> {
    if !position.is_finite() {
        return None;
    }
    let next = match chapters::chapter_at(chapters, position) {
        Some(index) => chapters.get(index + 1)?,
        None => chapters.first().filter(|first| first.start > position)?,
    };
    Some(next.start)
}

/// Where "Previous chapter" goes from `position`: the start of the current
/// chapter when more than `CHAPTER_RESTART_AFTER` seconds into it, else the
/// start of the chapter before; from the first chapter (or before it) the start
/// of the file. `None` only when there are no chapters.
pub(super) fn previous_chapter_start(chapters: &[Chapter], position: f64) -> Option<f64> {
    chapters.first()?;
    let position = if position.is_finite() { position } else { 0. };
    let Some(index) = chapters::chapter_at(chapters, position) else {
        return Some(0.);
    };
    let current = chapters.get(index)?;
    if position - current.start > CHAPTER_RESTART_AFTER {
        Some(current.start)
    } else if let Some(previous) = index.checked_sub(1).and_then(|i| chapters.get(i)) {
        Some(previous.start)
    } else {
        Some(0.)
    }
}

/// Where the "end of chapter" sleep timer stops playback: the end of the chapter
/// at `position` (the start of the first chapter while still before it). The last
/// chapter has no target of its own: it ends with the file, which the end-of-file
/// handling already stops at. `None` also when there are no chapters.
pub(super) fn chapter_sleep_target(chapters: &[Chapter], position: f64) -> Option<f64> {
    if !position.is_finite() {
        return None;
    }
    match chapters::chapter_at(chapters, position) {
        Some(index) => {
            let next = chapters.get(index + 1)?;
            let end = chapters.get(index)?.end.unwrap_or(next.start);
            Some(end.min(next.start))
        }
        None => chapters
            .first()
            .map(|first| first.start)
            .filter(|start| *start > position),
    }
}

/// How long to wait before the player must look again so the "end of chapter"
/// pause lands on the boundary: `None` while the end is further away than
/// `CHAPTER_SLEEP_WAKE_WITHIN` of real time at `speed`.
pub(super) fn chapter_sleep_wake(left_file_seconds: f64, speed: f32) -> Option<Duration> {
    let speed = if speed.is_finite() && speed > 0. {
        f64::from(speed)
    } else {
        1.
    };
    let left = left_file_seconds / speed;
    (left.is_finite() && left <= CHAPTER_SLEEP_WAKE_WITHIN)
        .then(|| Duration::from_secs_f64(left.max(0.)).max(Duration::from_millis(5)))
}

/// Where the Chapters list scrolls to so that row `index` sits in the middle of
/// a viewport `viewport` points high; rows are `pitch` apart.
pub(super) fn chapter_scroll_offset(index: usize, pitch: f32, viewport: f32) -> f32 {
    if !(pitch.is_finite() && pitch > 0. && viewport.is_finite()) {
        return 0.;
    }
    (index as f32 * pitch - (viewport - pitch) / 2.).max(0.)
}

/// The widget id of row `index` of the Chapters list.
pub(super) fn chapter_row_id(index: usize) -> egui::Id {
    egui::Id::new(("chapter row", index))
}
