use super::*;

impl PlayerApp {
    pub(super) fn request_artwork(&mut self) {
        let catalog = self
            .gui_cache
            .catalog(&self.state.library, self.library_revision);
        // The pane follows the playing track; the selection only when nothing plays.
        let track = art_subject(
            self.state.current.as_deref(),
            self.state.selected.as_deref(),
            &catalog,
        );
        if let Some(track) = track {
            let embedded = self.state.settings.prefer_embedded_art;
            // A changed art preference reloads the cover of the same track.
            let stale = self.art_requested.as_deref() != Some(&track.id)
                || self.art_embedded.is_some_and(|used| used != embedded);
            if stale {
                // While the track is still changing fast (arrow keys held),
                // wait for it to settle instead of decoding every cover passed.
                if let Some(wait) = self.art_debounce.poll(&track.id, Instant::now()) {
                    self.gui_context.request_repaint_after(wait);
                    return;
                }
                let track = track.clone();
                self.art_requested = Some(track.id.clone());
                self.art_embedded = Some(embedded);
                self.artwork = None;
                self.art_note = None;
                let tx = self.job_sender();
                std::thread::spawn(move || {
                    let result =
                        decode_cover_with_origin(&track, embedded).map(|(image, origin)| {
                            let img = image.thumbnail(1600, 1600).to_rgba8();
                            (
                                egui::ColorImage::from_rgba_unmultiplied(
                                    [img.width() as usize, img.height() as usize],
                                    img.as_raw(),
                                ),
                                origin,
                            )
                        });
                    let _ = tx.send(Event::Artwork(track.id, result));
                });
            }
        }
    }

    /// Starts decoding the File details thumbnail of `track` unless it is
    /// already loaded or loading.
    pub(super) fn ensure_details_art(&mut self, track: &Track) {
        let embedded = self.state.settings.prefer_embedded_art;
        if self
            .details_art
            .as_ref()
            .is_some_and(|art| art.id == track.id && art.embedded == embedded)
        {
            return;
        }
        self.details_art = Some(DetailsArt {
            id: track.id.clone(),
            embedded,
            state: DetailsArtState::Loading,
        });
        let track = track.clone();
        let tx = self.job_sender();
        std::thread::spawn(move || {
            let result = decode_cover_with_origin(&track, embedded)
                .map(|(image, origin)| (square_thumbnail(&image, DETAILS_ART_PIXELS), origin));
            tx.send(Event::DetailsArt(track.id, result));
        });
    }

    pub(super) fn artwork_context_menu(&mut self, ui: &mut egui::Ui, has_art: bool) {
        for (value, label) in [
            ("fit", "Fit"),
            ("fill", "Fill"),
            ("original", "Original size"),
        ] {
            ui.selectable_value(&mut self.state.settings.artwork_fit, value.into(), label);
        }
        ui.separator();
        if widgets::text_icon_button(ui, Icon::Expand, "Expand").clicked() {
            self.expanded_art = true;
            ui.close();
        }
        if ui
            .add_enabled(has_art, egui::Button::new("Export artwork…"))
            .clicked()
        {
            // Export the cover the pane shows: the playing track, else the selection.
            let Some(track) = self
                .state
                .current_track()
                .or_else(|| {
                    self.state
                        .selected
                        .as_deref()
                        .and_then(|id| self.state.track(id))
                })
                .cloned()
            else {
                ui.close();
                return;
            };
            let embedded = self.state.settings.prefer_embedded_art;
            self.export_artwork(track, embedded);
            ui.close();
        }
    }

    /// The art pane while a station is current: the generic cover with a radio
    /// set and the caption "Live stream". Nothing is looked up.
    pub(super) fn station_artwork(&mut self, ui: &mut egui::Ui, label: &str) {
        ui.set_min_height(ui.available_height());
        let available = Vec2::new(
            ui.available_width().max(1.0),
            ui.available_height().max(1.0),
        );
        let (rect, response) = ui.allocate_exact_size(available, egui::Sense::hover());
        let caption = "Live stream";
        let font = egui::TextStyle::Small.resolve(ui.style());
        let weak = ui.visuals().weak_text_color();
        let galley =
            ui.painter()
                .layout(caption.to_owned(), font, weak, (rect.width() - 8.).max(1.));
        // The cover and its caption are centred together; with too little
        // height the caption is left to the tooltip.
        let gap = 6.;
        let caption_height = galley.size().y + gap;
        let galley = Some(galley).filter(|_| rect.height() >= caption_height + 48.);
        let reserved = galley.as_ref().map_or(0., |_| caption_height);
        let side = rect.width().min(rect.height() - reserved).max(1.);
        let top = rect.top() + (rect.height() - side - reserved) / 2.;
        let tile = egui::Rect::from_min_size(
            egui::pos2(rect.center().x - side / 2., top),
            Vec2::splat(side),
        );
        widgets::paint_station_art(ui.painter(), tile, label, ui.visuals().dark_mode);
        if let Some(galley) = galley {
            let origin = egui::pos2(rect.center().x - galley.size().x / 2., tile.bottom() + gap);
            ui.painter().galley(origin, galley, weak);
        }
        response.on_hover_text(caption);
    }

    pub(super) fn artwork_ui(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        if let Some(label) = self.current_station_label() {
            self.station_artwork(ui, &label);
            return;
        }
        self.request_artwork();
        ui.set_min_height(ui.available_height());
        let available = Vec2::new(
            ui.available_width().max(1.0),
            ui.available_height().max(1.0),
        );

        // Match the texture to the playing (else selected) track so a stale cover
        // does not remain after the selection has been cleared.
        let catalog = self
            .gui_cache
            .catalog(&self.state.library, self.library_revision);
        let subject = art_subject(
            self.state.current.as_deref(),
            self.state.selected.as_deref(),
            &catalog,
        );
        let target_id = subject.map(|track| track.id.as_str());
        let (seed, audiobook) = subject
            .map(|track| (art_seed(track).to_owned(), track.audiobook))
            .unwrap_or_default();
        let texture = self
            .artwork
            .as_ref()
            .filter(|(id, _)| Some(id.as_str()) == target_id)
            .map(|(_, texture)| texture.clone());
        let note = self
            .art_note
            .as_ref()
            .filter(|(id, _)| Some(id.as_str()) == target_id)
            .map(|(_, note)| note.clone());

        let (response, has_art) = if let Some(texture) = texture {
            let response = match self.state.settings.artwork_fit.as_str() {
                "fill" => {
                    let source_aspect = texture.size_vec2().x / texture.size_vec2().y;
                    let target_aspect = available.x / available.y;
                    let (u, v) = if source_aspect > target_aspect {
                        (target_aspect / source_aspect, 1.0)
                    } else {
                        (1.0, source_aspect / target_aspect)
                    };
                    let uv = egui::Rect::from_min_max(
                        egui::pos2((1.0 - u) * 0.5, (1.0 - v) * 0.5),
                        egui::pos2((1.0 + u) * 0.5, (1.0 + v) * 0.5),
                    );
                    ui.add(
                        egui::Image::new(&texture)
                            .corner_radius(4)
                            .uv(uv)
                            .fit_to_exact_size(available)
                            .maintain_aspect_ratio(false)
                            .sense(egui::Sense::click()),
                    )
                }
                "original" => {
                    egui::ScrollArea::both()
                        .max_height(available.y)
                        .show(ui, |ui| {
                            ui.add(egui::Image::new(&texture).sense(egui::Sense::click()))
                        })
                        .inner
                }
                _ => {
                    ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Image::new(&texture)
                                .corner_radius(4)
                                .fit_to_exact_size(available)
                                .sense(egui::Sense::click()),
                        )
                    })
                    .inner
                }
            };
            let response = match &note {
                Some(note) => response.on_hover_text(note.tooltip()),
                None => response,
            };
            (response, true)
        } else {
            let (rect, response) = ui.allocate_exact_size(available, egui::Sense::click());
            let caption = note.as_ref().and_then(ArtNote::caption);
            let font = egui::TextStyle::Small.resolve(ui.style());
            let weak = ui.visuals().weak_text_color();
            let galley = caption.as_ref().map(|text| {
                ui.painter()
                    .layout(text.clone(), font, weak, (rect.width() - 8.).max(1.))
            });
            // The cover and its caption are centred together; with too little
            // height the caption is left to the tooltip.
            let gap = 6.;
            let caption_height = galley.as_ref().map_or(0., |galley| galley.size().y + gap);
            let galley = galley.filter(|_| rect.height() >= caption_height + 48.);
            let reserved = galley.as_ref().map_or(0., |_| caption_height);
            let side = rect.width().min(rect.height() - reserved).max(1.);
            let top = rect.top() + (rect.height() - side - reserved) / 2.;
            let tile = egui::Rect::from_min_size(
                egui::pos2(rect.center().x - side / 2., top),
                Vec2::splat(side),
            );
            widgets::paint_default_art(
                ui.painter(),
                tile,
                audiobook,
                &seed,
                ui.visuals().dark_mode,
            );
            if let Some(galley) = galley {
                let origin =
                    egui::pos2(rect.center().x - galley.size().x / 2., tile.bottom() + gap);
                ui.painter().galley(origin, galley, weak);
            }
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    ui.is_enabled(),
                    "Artwork placeholder; right-click for artwork options",
                )
            });
            let response = match caption {
                Some(text) => response.on_hover_text(text),
                None => response,
            };
            (response, false)
        };

        response.context_menu(|ui| self.artwork_context_menu(ui, has_art));
        if response.double_clicked() {
            self.expanded_art = true;
        }
    }

    /// Paints the cover of `track` in `rect` (a row thumbnail). A picture that is
    /// not decoded yet, or none at all, shows the generic cover at once, so the
    /// row never changes size; a missing picture is queued in `wanted`.
    pub(super) fn row_thumbnail(
        &mut self,
        ui: &egui::Ui,
        rect: egui::Rect,
        track: &Track,
        now: Instant,
        wanted: &mut Vec<(ThumbKey, ThumbJob)>,
    ) {
        let key = ThumbKey::of(track);
        match self.thumbs.cache.lookup(&key, now) {
            ThumbState::Ready(texture) => {
                egui::Image::new(texture)
                    .corner_radius(widgets::art_corner_radius(rect.width()))
                    .paint_at(ui, rect);
                return;
            }
            ThumbState::Missing => {}
            ThumbState::Unknown => wanted.push((
                key,
                ThumbJob {
                    path: track.path.clone(),
                    embedded: self.state.settings.prefer_embedded_art,
                    generation: self.session_generation,
                },
            )),
        }
        widgets::paint_default_art(
            ui.painter(),
            rect,
            track.audiobook,
            art_seed(track),
            ui.visuals().dark_mode,
        );
    }
}

pub(super) const ART_SETTLE: Duration = Duration::from_millis(150);

/// What `media::load_artwork_with_origin` reports when the file and its folder
/// hold no picture at all (as opposed to a picture that could not be read).
pub(super) const NO_ART_FOUND: &str = "no embedded or folder artwork found";

/// Largest decoded side of that thumbnail (room for high-DPI screens).
pub(super) const DETAILS_ART_PIXELS: u32 = 320;

/// Largest decoded side of a row thumbnail.
pub(super) const ROW_THUMB_PIXELS: u32 = 96;

/// Space between a row thumbnail and the title text.
pub(super) const THUMB_GAP: f32 = 6.;

/// A row thumbnail is only drawn while the title keeps at least this much width.
pub(super) const THUMB_MIN_TEXT: f32 = 30.;

/// Why the art pane has no picture, or where its picture came from.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ArtNote {
    Origin(String),
    Missing,
    Unreadable(String),
}

impl ArtNote {
    pub(super) fn from_error(error: &str) -> Self {
        if error == NO_ART_FOUND {
            Self::Missing
        } else {
            Self::Unreadable(short_error(error))
        }
    }
    /// Weak caption under the generic cover; `None` while a picture shows.
    pub(super) fn caption(&self) -> Option<String> {
        match self {
            Self::Origin(_) => None,
            Self::Missing => Some("No artwork found in the file or its folder".to_owned()),
            Self::Unreadable(error) => Some(format!("Artwork could not be read: {error}")),
        }
    }
    pub(super) fn tooltip(&self) -> String {
        match self {
            Self::Origin(origin) => format!("Artwork: {origin}"),
            _ => self.caption().unwrap_or_default(),
        }
    }
    pub(super) fn details_line(&self) -> String {
        match self {
            Self::Origin(origin) => format!("Artwork: {origin}"),
            Self::Missing => "Artwork: none found".to_owned(),
            Self::Unreadable(error) => format!("Artwork: could not be read: {error}"),
        }
    }
}

/// One line of at most 80 characters.
pub(super) fn short_error(error: &str) -> String {
    let line = error.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= 80 {
        line
    } else {
        line.chars().take(79).chain(std::iter::once('…')).collect()
    }
}

pub(super) struct DetailsArt {
    pub(super) id: String,
    pub(super) embedded: bool,
    pub(super) state: DetailsArtState,
}

pub(super) enum DetailsArtState {
    Loading,
    Ready(egui::TextureHandle, String),
    Note(ArtNote),
}

/// The track whose cover the art pane shows: the playing one, and the selected
/// one only when nothing (known) is playing.
pub(super) fn art_subject<'a>(
    current: Option<&str>,
    selected: Option<&str>,
    catalog: &'a LibraryCatalog,
) -> Option<&'a Track> {
    current
        .and_then(|id| catalog.track(id))
        .or_else(|| selected.and_then(|id| catalog.track(id)))
}

/// A row thumbnail to decode. Only the path matters to the artwork lookup.
pub(super) struct ThumbJob {
    pub(super) path: PathBuf,
    pub(super) embedded: bool,
    pub(super) generation: u64,
}

/// Row thumbnails: textures already decoded, and the one background thread that
/// decodes more. The thread starts with the first request and ends with the app.
pub(super) struct ThumbLoader {
    pub(super) cache: ThumbCache<egui::TextureHandle>,
    pub(super) queue: Arc<ThumbQueue<ThumbJob>>,
    pub(super) worker_started: bool,
}

impl ThumbLoader {
    pub(super) fn new() -> Self {
        Self {
            cache: ThumbCache::new(),
            queue: Arc::new(ThumbQueue::new()),
            worker_started: false,
        }
    }
    /// Forgets every thumbnail and queued request.
    pub(super) fn reset(&mut self) {
        self.cache.clear();
        self.queue.clear();
    }
    /// Hands over the visible, still missing thumbnails (see `ThumbQueue::submit`).
    pub(super) fn submit(
        &mut self,
        wanted: Vec<(ThumbKey, ThumbJob)>,
        tx: &Sender<(u64, Event)>,
        repaint: &egui::Context,
    ) {
        if wanted.is_empty() && self.queue.is_empty() {
            return;
        }
        if !wanted.is_empty() && !self.worker_started {
            self.worker_started = true;
            let queue = Arc::clone(&self.queue);
            let tx = tx.clone();
            let repaint = repaint.clone();
            // Without a thread every row simply keeps its generic cover.
            let _ = std::thread::Builder::new()
                .name("row-thumbnails".into())
                .spawn(move || {
                    while let Some((key, job)) = queue.next() {
                        let image = decode_row_thumbnail(&job.path, job.embedded);
                        JobSender {
                            generation: job.generation,
                            tx: tx.clone(),
                            repaint: repaint.clone(),
                        }
                        .send(Event::Thumb {
                            key,
                            embedded: job.embedded,
                            image,
                        });
                        queue.finish();
                    }
                });
        }
        self.queue.submit(wanted);
    }
}

impl Drop for ThumbLoader {
    fn drop(&mut self) {
        self.queue.close();
    }
}

pub(super) fn decode_row_thumbnail(path: &std::path::Path, prefer_embedded: bool) -> Option<egui::ColorImage> {
    let track = Track {
        path: path.to_path_buf(),
        ..Track::default()
    };
    let (image, _) = decode_cover_with_origin(&track, prefer_embedded).ok()?;
    Some(square_thumbnail(&image, ROW_THUMB_PIXELS))
}

/// The centred square of `image`, scaled down to at most `max_side` pixels.
pub(super) fn square_thumbnail(image: &image::DynamicImage, max_side: u32) -> egui::ColorImage {
    let side = image.width().min(image.height()).max(1);
    let (x, y) = ((image.width() - side) / 2, (image.height() - side) / 2);
    let thumb = image
        .crop_imm(x, y, side, side)
        .thumbnail(max_side, max_side)
        .to_rgba8();
    egui::ColorImage::from_rgba_unmultiplied(
        [thumb.width() as usize, thumb.height() as usize],
        thumb.as_raw(),
    )
}

/// Starts the first cover decode after a quiet period at once and holds back the
/// following ones until the wanted track has stopped changing.
#[derive(Default)]
pub(super) struct ArtDebounce {
    pub(super) id: Option<String>,
    pub(super) changed: Option<Instant>,
}

impl ArtDebounce {
    /// `None`: decode now. `Some(wait)`: ask again after `wait`.
    pub(super) fn poll(&mut self, id: &str, now: Instant) -> Option<Duration> {
        if self.id.as_deref() != Some(id) {
            let quiet = self
                .changed
                .is_none_or(|at| now.saturating_duration_since(at) >= ART_SETTLE);
            self.id = Some(id.to_owned());
            self.changed = Some(now);
            if !quiet {
                return Some(ART_SETTLE);
            }
        } else if let Some(at) = self.changed {
            let waited = now.saturating_duration_since(at);
            if waited < ART_SETTLE {
                return Some(ART_SETTLE - waited);
            }
        }
        None
    }
}

/// Decodes the cover of `track` within fixed size limits. When the preferred
/// source (embedded picture or folder image) cannot be decoded, the other one is
/// tried, so one bad embedded picture does not hide a good `cover.jpg`.
pub(super) fn decode_cover(track: &Track, prefer_embedded: bool) -> Result<image::DynamicImage, String> {
    decode_cover_with_origin(track, prefer_embedded).map(|(image, _)| image)
}

/// `decode_cover`, plus where the picture came from ("embedded picture" or
/// "folder file cover.jpg").
pub(super) fn decode_cover_with_origin(
    track: &Track,
    prefer_embedded: bool,
) -> Result<(image::DynamicImage, String), String> {
    let attempt = |embedded: bool| {
        let source = media::load_artwork_with_origin(track, embedded)?;
        decode_image_bytes(&source.bytes).map(|image| (image, source.origin))
    };
    attempt(prefer_embedded).or_else(|first| attempt(!prefer_embedded).map_err(|_| first))
}

/// Decodes a picture within fixed size limits (a damaged or huge image is an
/// error, never a large allocation).
pub(super) fn decode_image_bytes(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    reader.limits(limits);
    reader.decode().map_err(|error| error.to_string())
}
