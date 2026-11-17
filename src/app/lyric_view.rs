use super::*;

impl PlayerApp {
    /// Makes `current_lyrics` follow `track`, the file that is about to play.
    /// Same rules as `ensure_chapters`: nothing happens while they already are,
    /// or are being read from, this version of the file; the lyrics come from
    /// the cache or from a worker (one per load); the lyrics of another file are
    /// dropped at once, those of an earlier version of the same file stay until
    /// the new ones arrive. A live stream or station is never looked at.
    pub(super) fn ensure_lyrics(&mut self, track: &Track) {
        let key = LyricsKey::of(track);
        if self.lyrics_key.as_ref() == Some(&key) {
            return;
        }
        let same_file = self
            .lyrics_key
            .as_ref()
            .is_some_and(|stored| stored.id == key.id);
        self.lyrics_key = Some(key.clone());
        self.lyrics_reading = false;
        if !wants_lyrics(track) {
            self.show_lyrics(Arc::default());
            return;
        }
        if let Some(cached) = self.lyrics_cache.get(&key) {
            self.show_lyrics(cached);
            return;
        }
        self.lyrics_reading = true;
        if !same_file {
            self.show_lyrics(Arc::default());
        }
        let tx = self.job_sender();
        let path = track.path.clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                lyrics::read_lyrics(&path)
            }))
            .unwrap_or_else(|_| Err("The lyrics reader stopped unexpectedly".to_owned()));
            tx.send(Event::Lyrics { key, result });
        });
    }

    /// A worker's answer. It is dropped when the file (or its version) changed
    /// meanwhile; the answer is kept in the cache either way, and "no lyrics" and
    /// "could not be read" are kept alike, so the same file version is not read
    /// again and again.
    pub(super) fn lyrics_read(&mut self, key: LyricsKey, result: Result<Option<lyrics::Lyrics>, String>) {
        let found = Arc::new(result.ok().flatten());
        self.lyrics_cache.insert(key.clone(), Arc::clone(&found));
        if self.lyrics_reading && self.lyrics_key.as_ref() == Some(&key) {
            self.lyrics_reading = false;
            self.show_lyrics(found);
        }
    }

    /// Installs the lyrics of the current file and drops what was worked out
    /// from the old ones (the list starts at the top, or at the current line).
    pub(super) fn show_lyrics(&mut self, lyrics: Arc<Option<lyrics::Lyrics>>) {
        self.lyrics_serial = self.lyrics_serial.wrapping_add(1);
        self.current_lyrics = lyrics;
        self.lyrics_view = LyricsView {
            recenter: true,
            ..LyricsView::default()
        };
        self.leave_empty_lyrics_tab();
    }

    /// No lyrics, and none being looked for.
    pub(super) fn clear_lyrics(&mut self) {
        self.lyrics_key = None;
        self.lyrics_reading = false;
        self.show_lyrics(Arc::default());
    }

    /// The Lyrics tab only exists while the playing file has lyrics (or they are
    /// being read); without them the list shows the files again.
    pub(super) fn leave_empty_lyrics_tab(&mut self) {
        if self.state.tab == LYRICS_TAB && self.current_lyrics.is_none() && !self.lyrics_reading {
            self.state.tab = "Files".into();
            self.state.scroll = 0.;
            self.restore_scroll = true;
        }
    }

    /// The lyrics follow `state.current`, however it changed.
    pub(super) fn sync_lyrics(&mut self) {
        let stale = self
            .lyrics_key
            .as_ref()
            .is_some_and(|key| self.state.current.as_deref() != Some(key.id.as_str()));
        if stale {
            self.clear_lyrics();
        }
    }

    /// About once a second, looks at the text of the library, the stations and
    /// the playlists for scripts the bundled fonts cannot draw (Chinese,
    /// Japanese, Korean, Thai, Indic, Hebrew, Arabic). For each one that no font
    /// of the player draws yet, the system font is read once, off the UI thread,
    /// and added as a fallback (`Event::FallbackFonts`). A library of Latin,
    /// Greek and Cyrillic text loads nothing. The library is only looked through
    /// again when its revision changed; nothing is asked for twice, and a script
    /// without a usable font stays without one, silently.
    pub(super) fn fallback_fonts_tick(&mut self, ctx: &egui::Context) {
        if !self.state.settings.fallback_fonts || !self.fonts_ready {
            return;
        }
        let now = Instant::now();
        if !self.font_fallbacks.due(now) {
            return;
        }
        self.font_fallbacks.checked(now);
        self.font_fallbacks
            .scan_library(self.library_revision, &self.state.library);
        // The few names that have no revision of their own.
        for station in self.state.stations.iter() {
            self.font_fallbacks.note_text(&station.name);
            self.font_fallbacks.note_text(&station.last_title);
        }
        for playlist in &self.state.playlists {
            self.font_fallbacks.note_text(&playlist.name);
        }
        if let Some((_, title)) = &self.live_title {
            self.font_fallbacks.note_text(title);
        }
        let wanted = self.font_fallbacks.wanted();
        if wanted.is_empty() {
            return;
        }
        // A script that a font of the player draws already needs nothing.
        let missing = appearance::scripts_without_glyphs(ctx, wanted);
        self.font_fallbacks.settle(wanted.minus(missing));
        if missing.is_empty() {
            return;
        }
        self.font_fallbacks.start(missing);
        let loader = Arc::clone(&self.font_loader);
        let tx = self.job_sender();
        std::thread::spawn(move || {
            let fonts = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loader(missing)))
                .unwrap_or_default();
            tx.send(Event::FallbackFonts(fonts));
        });
    }

    /// The Lyrics tab: the source of the lyrics, then the lines, centred and
    /// wrapped. Synchronised lyrics highlight the line being sung (accent colour
    /// and larger), the list keeps it in the middle unless the user scrolled
    /// within the last few seconds, and a click on a line seeks to it. Plain lyrics
    /// are just text. Only the lines in view are laid out each frame; the wrapped
    /// heights are worked out once per width and text size.
    pub(super) fn lyrics_ui(&mut self, ui: &mut egui::Ui) {
        const SIDE_PADDING: f32 = 24.;
        const MAX_TEXT_WIDTH: f32 = 720.;
        const LINE_SPACING: f32 = 10.;
        const GLIDE: f32 = 0.35;
        let lyrics = Arc::clone(&self.current_lyrics);
        let (Some(data), Some(track_id)) = (lyrics.as_ref().as_ref(), self.state.current.clone())
        else {
            ui.add_space(12.);
            ui.label(if self.lyrics_reading {
                "Reading lyrics…"
            } else {
                "The playing file has no lyrics."
            });
            return;
        };
        let weak = ui.visuals().weak_text_color();
        ui.add_space(6.);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new(lyrics_caption(data)).small().color(weak));
        });
        ui.add_space(4.);
        let body = egui::TextStyle::Body.resolve(ui.style());
        let text_color = ui.visuals().text_color();
        let accent = ui.visuals().hyperlink_color;
        let area_width = ui.available_width();
        let viewport_height = ui.available_height();
        let width = (area_width - 2. * SIDE_PADDING)
            .clamp(40., MAX_TEXT_WIDTH)
            .floor();
        let pixels = ui.ctx().pixels_per_point();
        let layout_key = (
            self.lyrics_serial,
            width.to_bits(),
            body.size.to_bits(),
            pixels.to_bits(),
            self.font_epoch,
        );
        let position = self.position();
        let mut view = std::mem::take(&mut self.lyrics_view);
        if view.layout.as_ref().map(|layout| layout.key) != Some(layout_key) {
            let painter = ui.painter();
            view.layout = Some(LyricsLayout::build(
                layout_key,
                data,
                LINE_SPACING,
                body.size,
                |text| {
                    lyrics_galley(painter, text, &body, width, text_color)
                        .size()
                        .y
                },
            ));
            view.large = None;
            view.recenter = true;
        }
        let Some(layout) = view.layout.take() else {
            self.lyrics_view = view;
            return;
        };
        // The line being sung, and the larger galley it is drawn with.
        let current = if data.synced {
            view.cursor.locate(data, position)
        } else {
            None
        };
        let line_changed = view.shown != current;
        let wanted = current
            .and_then(|index| Some((index, data.lines.get(index)?)))
            .filter(|(_, line)| !line.text.is_empty());
        match wanted {
            Some((index, line)) => {
                if view.large.as_ref().map(|large| large.index) != Some(index) {
                    let font =
                        egui::FontId::new(body.size * LYRICS_CURRENT_SCALE, body.family.clone());
                    let galley = lyrics_galley(ui.painter(), &line.text, &font, width, accent);
                    let extra = (galley.size().y - layout.text_height(index)).max(0.);
                    view.large = Some(LyricsLargeLine {
                        index,
                        galley,
                        extra: if extra.is_finite() { extra } else { 0. },
                    });
                }
            }
            None => view.large = None,
        }
        let extra = view.large.as_ref().map(|large| (large.index, large.extra));
        let total = layout.total(extra);
        let target = match current {
            Some(index) => layout.scroll_target(extra, index, viewport_height),
            None => 0.,
        };
        // Following the line: only while the user has not scrolled lately.
        let now = ui.input(|input| input.time);
        let follow = data.synced && lyrics_follow_allowed(view.manual_at, now);
        let mut set_offset = None;
        if !data.synced {
            if view.recenter {
                set_offset = Some(0.);
            }
        } else if follow {
            if view.recenter {
                view.gliding = false;
                set_offset = Some(target);
            } else {
                if line_changed || !view.following {
                    view.gliding = true;
                }
                if view.gliding {
                    let step = view.offset + (target - view.offset) * GLIDE;
                    set_offset = Some(if (target - step).abs() < 1. {
                        view.gliding = false;
                        target
                    } else {
                        step
                    });
                    if view.gliding {
                        ui.ctx().request_repaint();
                    }
                }
            }
        } else {
            view.gliding = false;
        }
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("lyrics rows")
            .auto_shrink([false, false]);
        if let Some(offset) = set_offset.filter(|offset| offset.is_finite()) {
            scroll =
                scroll.vertical_scroll_offset(offset.clamp(0., (total - viewport_height).max(0.)));
        }
        let mut actions = Vec::new();
        let output = scroll.show_viewport(ui, |ui, viewport| {
            let origin = ui.max_rect().min;
            let content_width = ui.max_rect().width();
            ui.set_min_size(Vec2::new(content_width, total));
            for index in layout.visible(extra, viewport.min.y, viewport.max.y) {
                let Some(line) = data.lines.get(index) else {
                    break;
                };
                if line.text.is_empty() {
                    continue; // A gap in the singing: spacing only.
                }
                let top = origin.y + layout.top(index, extra);
                let bottom = origin.y + layout.top(index + 1, extra);
                let rect = egui::Rect::from_min_max(
                    egui::pos2(origin.x, top),
                    egui::pos2(origin.x + content_width, bottom),
                );
                if let Some(time) = line.time.filter(|_| data.synced) {
                    // A click does not take the keyboard (Space must still pause).
                    let response = ui.interact(rect, lyrics_row_id(index), egui::Sense::click());
                    if response.hovered() {
                        ui.painter()
                            .rect_filled(rect, 6., appearance::row_fill(ui, true, false));
                    }
                    if response.has_focus() {
                        ui.painter().rect_stroke(
                            rect.shrink(1.),
                            6.,
                            egui::Stroke::new(1.5, appearance::row_primary_outline(ui)),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if response.clicked() {
                        actions.push(Action::SeekLine(track_id.clone(), time));
                    }
                    response.on_hover_cursor(egui::CursorIcon::PointingHand);
                }
                let at = egui::pos2(rect.center().x, top);
                match view.large.as_ref().filter(|large| large.index == index) {
                    Some(large) => ui.painter().galley_with_override_text_color(
                        at,
                        Arc::clone(&large.galley),
                        accent,
                    ),
                    None => ui.painter().galley(
                        at,
                        lyrics_galley(ui.painter(), &line.text, &body, width, text_color),
                        text_color,
                    ),
                }
            }
        });
        // An offset that moved without our setting it is the user scrolling.
        let offset = output.state.offset.y;
        if set_offset.is_none() && (offset - view.offset).abs() > 0.5 {
            view.manual_at = Some(now);
        }
        view.offset = offset;
        view.shown = current;
        view.following = follow;
        view.recenter = false;
        view.layout = Some(layout);
        // Draw again exactly when the next line is due, not a tick later.
        if self.snapshot.playing && self.snapshot.loaded == self.state.current {
            if let Some(delay) = lyrics_repaint_delay(data, current, position, self.state.speed) {
                ui.ctx().request_repaint_after(delay);
            }
        }
        self.actions.extend(actions);
        self.lyrics_view = view;
    }
}

// ---------------------------------------------------------------------------
// Lyrics
// ---------------------------------------------------------------------------

/// Files whose lyrics stay in memory, newest last.
pub(super) const LYRICS_CACHE_ENTRIES: usize = 16;

/// The Lyrics tab as the tab strip and the list menu name it.
pub(super) const LYRICS_TAB: &str = "Lyrics";

/// The list stops following the highlighted line for this long (seconds of
/// input time) after the user scrolled it.
pub(super) const LYRICS_MANUAL_SCROLL_HOLD: f64 = 5.;

/// The shortest wait before the player draws again for the next line (seconds).
pub(super) const LYRICS_MIN_REPAINT: f64 = 0.05;

/// The highlighted line is drawn this many times larger than the others.
pub(super) const LYRICS_CURRENT_SCALE: f32 = 1.25;

/// Whether the lyrics reader is tried for `track`: a file of the library, never
/// a live stream.
pub(super) fn wants_lyrics(track: &Track) -> bool {
    !track.id.starts_with("stream:") && !track.path.as_os_str().is_empty()
}

/// Which version of which file lyrics were read from (the same identity the
/// chapters use): a changed file is read again.
pub(super) type LyricsKey = ChapterKey;

/// The lyrics of the last few files (`None`: the file has none or could not be
/// read), so playing a file again does not read it again. Memory only.
#[derive(Default)]
pub(super) struct LyricsCache {
    pub(super) entries: VecDeque<(LyricsKey, Arc<Option<lyrics::Lyrics>>)>,
}

impl LyricsCache {
    pub(super) fn get(&self, key: &LyricsKey) -> Option<Arc<Option<lyrics::Lyrics>>> {
        self.entries
            .iter()
            .find(|(stored, _)| stored == key)
            .map(|(_, lyrics)| Arc::clone(lyrics))
    }
    pub(super) fn insert(&mut self, key: LyricsKey, lyrics: Arc<Option<lyrics::Lyrics>>) {
        self.entries.retain(|(stored, _)| *stored != key);
        self.entries.push_back((key, lyrics));
        while self.entries.len() > LYRICS_CACHE_ENTRIES {
            self.entries.pop_front();
        }
    }
}

/// The line the position is in, remembered between calls: the search only runs
/// again when the position leaves the range of the line it found. File time, so
/// it does not depend on the playback speed.
#[derive(Clone, Copy, Debug)]
pub(super) struct LyricsCursor {
    pub(super) index: Option<usize>,
    /// The positions `from..to` have `index` as their line.
    pub(super) from: f64,
    pub(super) to: f64,
}

impl Default for LyricsCursor {
    fn default() -> Self {
        // An empty range: nothing is remembered.
        Self {
            index: None,
            from: 0.,
            to: 0.,
        }
    }
}

impl LyricsCursor {
    pub(super) fn locate(&mut self, lyrics: &lyrics::Lyrics, position: f64) -> Option<usize> {
        if !lyrics.synced || !position.is_finite() {
            return None;
        }
        if position >= self.from
            && position < self.to
            && self.index.is_none_or(|index| index < lyrics.lines.len())
        {
            return self.index;
        }
        let found = lyrics::current_line(lyrics, position);
        let time_of = |index: usize| lyrics.lines.get(index).and_then(|line| line.time);
        *self = Self {
            index: found,
            from: found.and_then(time_of).unwrap_or(f64::NEG_INFINITY),
            to: time_of(found.map_or(0, |index| index + 1)).unwrap_or(f64::INFINITY),
        };
        found
    }
}

/// How long to wait before the player must draw again so the highlight moves to
/// the next line on time: the file time to the next line over the playback
/// speed, at least `LYRICS_MIN_REPAINT`. `None` for plain lyrics, a position that
/// is not finite, and after the last line.
pub(super) fn lyrics_repaint_delay(
    lyrics: &lyrics::Lyrics,
    current: Option<usize>,
    position: f64,
    speed: f32,
) -> Option<Duration> {
    if !lyrics.synced || !position.is_finite() {
        return None;
    }
    let next = lyrics.lines.get(current.map_or(0, |index| index + 1))?;
    let speed = if speed.is_finite() && speed > 0. {
        f64::from(speed)
    } else {
        1.
    };
    let wait = (next.time? - position) / speed;
    wait.is_finite()
        .then(|| Duration::from_secs_f64(wait.clamp(LYRICS_MIN_REPAINT, 24. * 3600.)))
}

/// Whether the list may scroll itself: not within `LYRICS_MANUAL_SCROLL_HOLD`
/// seconds after the user scrolled it (`last_manual` and `now` are input times).
pub(super) fn lyrics_follow_allowed(last_manual: Option<f64>, now: f64) -> bool {
    match last_manual {
        Some(at) => {
            let since = now - at;
            !(since >= 0. && since < LYRICS_MANUAL_SCROLL_HOLD)
        }
        None => true,
    }
}

/// The extra height of the highlighted line (the larger text wraps to more
/// space): its index and how much it adds to the lines below it.
pub(super) type LyricsExtra = Option<(usize, f32)>;

/// What a `LyricsLayout` was made for.
pub(super) type LyricsLayoutKey = (u64, u32, u32, u32, u32);

/// The wrapped heights of the lines of one text for one width and text size.
pub(super) struct LyricsLayout {
    /// The lyrics serial, wrap width, text size, pixels per point and number of
    /// fallback fonts it was made for.
    pub(super) key: LyricsLayoutKey,
    /// `tops[i]` is where line `i` starts; the last entry is the total height.
    pub(super) tops: Vec<f32>,
    /// The height of the text of line `i` (0 for a gap).
    pub(super) text_heights: Vec<f32>,
}

impl LyricsLayout {
    /// `measure` gives the wrapped height of a line's text. A gap line takes the
    /// height of one text line and nothing else; every other line adds `spacing`.
    pub(super) fn build(
        key: LyricsLayoutKey,
        lyrics: &lyrics::Lyrics,
        spacing: f32,
        gap: f32,
        mut measure: impl FnMut(&str) -> f32,
    ) -> Self {
        let mut tops = Vec::with_capacity(lyrics.lines.len() + 1);
        let mut text_heights = Vec::with_capacity(lyrics.lines.len());
        let mut y = 0.;
        for line in &lyrics.lines {
            tops.push(y);
            if line.text.is_empty() {
                text_heights.push(0.);
                y += gap;
            } else {
                let height = measure(&line.text);
                let height = if height.is_finite() {
                    height.max(0.)
                } else {
                    0.
                };
                text_heights.push(height);
                y += height + spacing;
            }
        }
        tops.push(y);
        Self {
            key,
            tops,
            text_heights,
        }
    }
    pub(super) fn len(&self) -> usize {
        self.text_heights.len()
    }
    pub(super) fn text_height(&self, index: usize) -> f32 {
        self.text_heights.get(index).copied().unwrap_or(0.)
    }
    /// Where line `index` starts, with the highlighted line's extra height in
    /// place; the line after the last is the total.
    pub(super) fn top(&self, index: usize, extra: LyricsExtra) -> f32 {
        let base = self
            .tops
            .get(index)
            .or(self.tops.last())
            .copied()
            .unwrap_or(0.);
        match extra {
            Some((current, more)) if index > current => base + more,
            _ => base,
        }
    }
    pub(super) fn total(&self, extra: LyricsExtra) -> f32 {
        self.top(self.len(), extra)
    }
    /// The lines that overlap the vertical range `from..to` of the content.
    pub(super) fn visible(&self, extra: LyricsExtra, from: f32, to: f32) -> std::ops::Range<usize> {
        let count = self.len();
        let (mut low, mut high) = (0, count);
        while low < high {
            let middle = low + (high - low) / 2;
            if self.top(middle + 1, extra) <= from {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        let first = low;
        let mut high = count;
        while low < high {
            let middle = low + (high - low) / 2;
            if self.top(middle, extra) < to {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        first..low
    }
    /// The scroll offset that puts line `index` in the middle of a viewport
    /// `viewport` high, never beyond either end of the content.
    pub(super) fn scroll_target(&self, extra: LyricsExtra, index: usize, viewport: f32) -> f32 {
        if !viewport.is_finite() {
            return 0.;
        }
        let middle = (self.top(index, extra) + self.top(index + 1, extra)) / 2.;
        (middle - viewport / 2.).clamp(0., (self.total(extra) - viewport).max(0.))
    }
}

/// The highlighted line, laid out larger.
pub(super) struct LyricsLargeLine {
    pub(super) index: usize,
    pub(super) galley: Arc<egui::Galley>,
    /// How much taller it is than the same line at the normal size.
    pub(super) extra: f32,
}

/// What the Lyrics list remembers from frame to frame (memory only).
#[derive(Default)]
pub(super) struct LyricsView {
    pub(super) cursor: LyricsCursor,
    pub(super) layout: Option<LyricsLayout>,
    pub(super) large: Option<LyricsLargeLine>,
    /// The line that was highlighted in the last frame.
    pub(super) shown: Option<usize>,
    /// Input time of the last scroll the user made.
    pub(super) manual_at: Option<f64>,
    /// The scroll offset at the end of the last frame.
    pub(super) offset: f32,
    /// The list is moving towards the highlighted line.
    pub(super) gliding: bool,
    /// The list could follow the highlighted line in the last frame.
    pub(super) following: bool,
    /// The next frame puts the highlighted line (or the top) in place at once.
    pub(super) recenter: bool,
}

/// A line of lyrics laid out centred and wrapped at `width`.
pub(super) fn lyrics_galley(
    painter: &egui::Painter,
    text: &str,
    font: &egui::FontId,
    width: f32,
    color: egui::Color32,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), font.clone(), color, width);
    job.halign = egui::Align::Center;
    painter.layout_job(job)
}

/// The widget id of line `index` of the Lyrics list.
pub(super) fn lyrics_row_id(index: usize) -> egui::Id {
    egui::Id::new(("lyrics row", index))
}

/// Where the lyrics came from, for the top of the Lyrics tab.
pub(super) fn lyrics_caption(lyrics: &lyrics::Lyrics) -> &'static str {
    match lyrics.source.as_str() {
        "lyrics file beside the track" => "Lyrics file beside the track",
        "embedded lyrics" => "Embedded lyrics",
        _ => "Lyrics",
    }
}

/// The `Lyrics:` line of File details for a file whose lyrics were looked for:
/// `None` when it has none.
pub(super) fn lyrics_details_line(lyrics: &Option<lyrics::Lyrics>) -> Option<&'static str> {
    Some(match lyrics.as_ref()?.source.as_str() {
        "lyrics file beside the track" => "Lyrics: lyrics file",
        "embedded lyrics" => "Lyrics: embedded",
        _ => "Lyrics: available",
    })
}
