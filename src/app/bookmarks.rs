use super::*;

impl PlayerApp {
    pub(super) fn bookmarks_ui(&mut self, ui: &mut egui::Ui) {
        if self.state.bookmarks.is_empty() {
            ui.label("No bookmarks yet. Use the bookmark button while listening to save a position and note.");
        }
        let catalog = self
            .gui_cache
            .catalog(&self.state.library, self.library_revision);
        let mut actions = Vec::new();
        if !self.state.bookmarks.is_empty() {
            self.bookmark_export_buttons(ui, &catalog);
        }
        egui::ScrollArea::vertical()
            .id_salt("bookmarks")
            .show(ui, |ui| {
                for (index, bookmark) in self.state.bookmarks.iter().enumerate() {
                    let title = catalog
                        .track(&bookmark.track_id)
                        .map(|track| track.display_title())
                        .unwrap_or("Unavailable file");
                    ui.push_id(index, |ui| {
                        ui.group(|ui| {
                            let remove_width = label_width(ui, "Remove", egui::TextStyle::Button)
                                + label_width(ui, "Edit", egui::TextStyle::Button)
                                + 4. * ui.spacing().button_padding.x
                                + ui.spacing().item_spacing.x;
                            let text_width = (ui.available_width()
                                - 30.
                                - remove_width
                                - 2. * ui.spacing().item_spacing.x)
                                .max(1.);
                            if text_width < ui.text_style_height(&egui::TextStyle::Body) * 6. {
                                ui.horizontal_wrapped(|ui| {
                                    if widgets::icon_button(
                                        ui,
                                        Icon::Play,
                                        "Play bookmark",
                                        false,
                                        false,
                                        30.,
                                    )
                                    .clicked()
                                    {
                                        actions.push(Action::Jump(
                                            bookmark.track_id.clone(),
                                            bookmark.seconds,
                                        ));
                                    }
                                    if ui.button("Edit").clicked() {
                                        actions.push(Action::EditBookmark(index));
                                    }
                                    if ui.button("Remove").clicked() {
                                        actions.push(Action::RemoveBookmark(index));
                                    }
                                });
                                ui.add(
                                    egui::Label::new(RichText::new(&bookmark.name).strong()).wrap(),
                                );
                                ui.add(
                                    egui::Label::new(format!(
                                        "{} · {}",
                                        time_text(bookmark.seconds),
                                        title
                                    ))
                                    .wrap(),
                                );
                            } else {
                                ui.horizontal_top(|ui| {
                                    if widgets::icon_button(
                                        ui,
                                        Icon::Play,
                                        "Play bookmark",
                                        false,
                                        false,
                                        30.,
                                    )
                                    .clicked()
                                    {
                                        actions.push(Action::Jump(
                                            bookmark.track_id.clone(),
                                            bookmark.seconds,
                                        ));
                                    }
                                    ui.allocate_ui_with_layout(
                                        Vec2::new(text_width, 0.),
                                        egui::Layout::top_down(egui::Align::Min),
                                        |ui| {
                                            ui.set_width(text_width);
                                            ui.add(
                                                egui::Label::new(
                                                    RichText::new(&bookmark.name).strong(),
                                                )
                                                .wrap(),
                                            );
                                            ui.add(
                                                egui::Label::new(format!(
                                                    "{} · {}",
                                                    time_text(bookmark.seconds),
                                                    title
                                                ))
                                                .wrap(),
                                            );
                                        },
                                    );
                                    if ui.button("Edit").clicked() {
                                        actions.push(Action::EditBookmark(index));
                                    }
                                    if ui.button("Remove").clicked() {
                                        actions.push(Action::RemoveBookmark(index));
                                    }
                                });
                            }
                            if !bookmark.note.is_empty() {
                                ui.add(egui::Label::new(&bookmark.note).wrap());
                            }
                        });
                    });
                }
            });
        self.actions.extend(actions);
    }

    pub(super) fn add_bookmark(&mut self) {
        if self.bookmark_dialog {
            return; // Keep an unfinished draft when the button is pressed again.
        }
        if let Some(track) = self.state.current_track() {
            let draft = BookmarkDraft {
                track_id: track.id.clone(),
                title: track.display_title().to_owned(),
                seconds: self.position(),
            };
            self.bookmark_name = format!("Bookmark at {}", time_text(draft.seconds));
            self.bookmark_note.clear();
            self.bookmark_target = Some(draft);
            self.bookmark_dialog = true;
        }
    }

    pub(super) fn save_bookmark_draft(&mut self) -> bool {
        let Some(draft) = self.bookmark_target.as_ref() else {
            return false;
        };
        if self.state.track(&draft.track_id).is_none() {
            return false;
        }
        self.state.bookmarks.push(Bookmark {
            track_id: draft.track_id.clone(),
            seconds: draft.seconds,
            name: self.bookmark_name.clone(),
            note: self.bookmark_note.clone(),
        });
        self.bookmark_target = None;
        self.bookmark_dialog = false;
        self.save();
        true
    }

    /// Deletes the bookmark at `index` and records it for Undo.
    pub(super) fn delete_bookmark(&mut self, index: usize) {
        if index >= self.state.bookmarks.len() {
            return;
        }
        let bookmark = self.state.bookmarks.remove(index);
        let label = if bookmark.name.trim().is_empty() {
            "Delete bookmark".to_owned()
        } else {
            format!(
                "Delete bookmark “{}”",
                short_label(&bookmark.name, UNDO_LABEL_CHARS)
            )
        };
        self.record_undo(
            label,
            state::UndoRestore::Bookmarks(vec![(index, bookmark)]),
            "Deleted the bookmark",
        );
        self.save();
    }

    pub(super) fn add_bookmark_from_key(&mut self) {
        if self.state.current_station.is_some() {
            self.set_brief_status(LIVE_UNAVAILABLE);
        } else if self.state.current_track().is_none() {
            self.set_brief_status("Nothing is playing to bookmark");
        } else {
            self.add_bookmark();
        }
    }

    /// Bookmarks with the book and file they belong to, in listening order.
    pub(super) fn bookmark_rows(&mut self) -> Vec<BookmarkRow> {
        let catalog = self
            .gui_cache
            .catalog(&self.state.library, self.library_revision);
        let mut rows = bookmark_rows(&self.state.bookmarks, |id| catalog.track(id));
        sort_bookmark_rows(&mut rows);
        rows
    }

    /// Alt+Right / Alt+Left: jump to the next or previous bookmark of the
    /// playing book, across its files. It keeps playing or staying paused.
    pub(super) fn step_bookmark(&mut self, forward: bool) {
        if self.state.current_station.is_some() {
            self.set_brief_status(LIVE_UNAVAILABLE);
            return;
        }
        let Some(current) = self.state.current_track().cloned() else {
            self.set_brief_status("Nothing is playing");
            return;
        };
        let book = current.book_key();
        let rows: Vec<BookmarkRow> = self
            .bookmark_rows()
            .into_iter()
            .filter(|row| row.book_key.as_deref() == Some(book.as_str()))
            .collect();
        if rows.is_empty() {
            self.set_brief_status("No bookmarks in this book");
            return;
        }
        let here = (
            current.track_number,
            current.path.to_string_lossy().into_owned(),
            self.position(),
        );
        let Some(index) = adjacent_bookmark(&rows, (here.0, &here.1, here.2), forward) else {
            self.set_brief_status(if forward {
                "No later bookmark in this book"
            } else {
                "No earlier bookmark in this book"
            });
            return;
        };
        let row = &rows[index];
        let (id, seconds) = match self.state.bookmarks.get(row.index) {
            Some(bookmark) => (bookmark.track_id.clone(), bookmark.seconds),
            None => return,
        };
        let note = format!(
            "Bookmark {} of {}: {} ({})",
            index + 1,
            rows.len(),
            row.name,
            time_text(seconds)
        );
        if self.state.current.as_deref() == Some(id.as_str()) {
            self.seek(seconds);
        } else {
            let play = self.playback_intent();
            self.load_track(&id, play, Some(seconds));
        }
        self.set_brief_status(note);
    }

    /// "Add bookmark at chapter start": opens the bookmark window for that time.
    pub(super) fn add_bookmark_at(&mut self, track_id: &str, seconds: f64, name: String) {
        if self.bookmark_dialog {
            return; // Keep an unfinished draft.
        }
        let Some(track) = self.state.track(track_id) else {
            return;
        };
        let draft = BookmarkDraft {
            track_id: track.id.clone(),
            title: track.display_title().to_owned(),
            seconds,
        };
        self.bookmark_name = name;
        self.bookmark_note.clear();
        self.bookmark_target = Some(draft);
        self.bookmark_dialog = true;
    }

    /// The Export buttons of the Bookmarks tab: this book (when one is selected
    /// or playing) and all of them.
    pub(super) fn bookmark_export_buttons(&mut self, ui: &mut egui::Ui, catalog: &LibraryCatalog) {
        let book = [
            self.state.selected.as_deref(),
            self.state.current.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find_map(|id| catalog.track(id))
        .map(|track| (track.book_key(), track.book_label()));
        let mut export: Option<Option<(String, String)>> = None;
        ui.horizontal(|ui| {
            if let Some((key, label)) = &book {
                if ui
                    .button("Export this book…")
                    .on_hover_text(format!(
                        "Save the bookmarks of {label} as a text or CSV file"
                    ))
                    .clicked()
                {
                    export = Some(Some((key.clone(), label.clone())));
                }
            }
            if ui
                .button(if book.is_some() {
                    "Export all…"
                } else {
                    "Export…"
                })
                .on_hover_text("Save every bookmark as a text or CSV file")
                .clicked()
            {
                export = Some(None);
            }
        });
        if let Some(scope) = export {
            self.export_bookmarks(scope);
        }
    }

    /// Saves bookmarks to a text file, or to CSV when the chosen name ends in
    /// .csv. `scope` is one book (key and label) or `None` for all of them.
    pub(super) fn export_bookmarks(&mut self, scope: Option<(String, String)>) {
        let mut rows = self.bookmark_rows();
        if let Some((key, _)) = &scope {
            rows.retain(|row| row.book_key.as_deref() == Some(key.as_str()));
        }
        if rows.is_empty() {
            self.set_brief_status("No bookmarks to export");
            return;
        }
        let Some(guard) = self.begin_picker() else {
            return;
        };
        let name = bookmark_export_name(scope.as_ref().map(|(_, label)| label.as_str()));
        let tx = self.job_sender();
        std::thread::spawn(move || {
            let _guard = guard;
            if let Some(path) = rfd::FileDialog::new()
                .set_file_name(name)
                .add_filter("Text file", &["txt"])
                .add_filter("CSV for spreadsheets", &["csv"])
                .save_file()
            {
                let csv = path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("csv"));
                let text = if csv {
                    bookmarks_csv(&rows)
                } else {
                    bookmarks_text(&rows)
                };
                let message = match std::fs::write(&path, text) {
                    Ok(()) => format!("Exported {} bookmarks", rows.len()),
                    Err(error) => format!("Bookmark export failed: {error}"),
                };
                let _ = tx.send(Event::Message(message));
            }
        });
    }
}

/// One bookmark with what is needed to put it in listening order and to export
/// it. `book_key` is `None` when its file is no longer in the library.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct BookmarkRow {
    /// Position in `Session::bookmarks`.
    pub(super) index: usize,
    pub(super) book_key: Option<String>,
    pub(super) book: String,
    pub(super) file: String,
    pub(super) number: u32,
    pub(super) path: String,
    pub(super) seconds: f64,
    pub(super) name: String,
    pub(super) note: String,
}

/// A bookmark within this many seconds after the playing position counts as
/// "here", not as the next one; the grace before the position is larger so
/// that Previous, pressed just after a jump, goes to the bookmark before it.
pub(super) const BOOKMARK_AHEAD_GRACE: f64 = 0.5;

pub(super) const BOOKMARK_BEHIND_GRACE: f64 = 2.0;

pub(super) fn bookmark_rows<'a>(
    bookmarks: &[Bookmark],
    track: impl Fn(&str) -> Option<&'a Track>,
) -> Vec<BookmarkRow> {
    bookmarks
        .iter()
        .enumerate()
        .map(|(index, bookmark)| {
            let track = track(&bookmark.track_id);
            BookmarkRow {
                index,
                book_key: track.map(Track::book_key),
                book: track.map_or_else(|| "Unavailable file".to_owned(), Track::book_label),
                file: track
                    .and_then(|track| track.path.file_name())
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                number: track.map_or(0, |track| track.track_number),
                path: track
                    .map(|track| track.path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                seconds: bookmark.seconds,
                name: bookmark.name.clone(),
                note: bookmark.note.clone(),
            }
        })
        .collect()
}

/// Listening order: book, then its files (track number, then natural path
/// order, as the file list sorts them), then position. Bookmarks whose file is
/// gone come last.
pub(super) fn sort_bookmark_rows(rows: &mut [BookmarkRow]) {
    rows.sort_by(|a, b| {
        a.book_key
            .is_none()
            .cmp(&b.book_key.is_none())
            .then_with(|| media::natural_cmp(&a.book, &b.book))
            .then_with(|| a.book_key.cmp(&b.book_key))
            .then_with(|| a.number.cmp(&b.number))
            .then_with(|| media::natural_cmp(&a.path, &b.path))
            .then_with(|| a.seconds.total_cmp(&b.seconds))
            .then_with(|| a.index.cmp(&b.index))
    });
}

/// Where a bookmark lies relative to the playing position `here` (file number,
/// file path, seconds): `Greater` is ahead, `Less` behind, `Equal` is "here".
pub(super) fn bookmark_side(row: &BookmarkRow, here: (u32, &str, f64)) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match row
        .number
        .cmp(&here.0)
        .then_with(|| media::natural_cmp(&row.path, here.1))
    {
        Ordering::Equal => {
            if row.seconds > here.2 + BOOKMARK_AHEAD_GRACE {
                Ordering::Greater
            } else if row.seconds < here.2 - BOOKMARK_BEHIND_GRACE {
                Ordering::Less
            } else {
                Ordering::Equal
            }
        }
        other => other,
    }
}

/// The next (or previous) bookmark of one book. `rows` are that book's rows in
/// listening order (see `sort_bookmark_rows`); `here` is the playing file's
/// track number and path and the position in it. No wrap-around.
pub(super) fn adjacent_bookmark(rows: &[BookmarkRow], here: (u32, &str, f64), forward: bool) -> Option<usize> {
    use std::cmp::Ordering;
    if forward {
        rows.iter()
            .position(|row| bookmark_side(row, here) == Ordering::Greater)
    } else {
        rows.iter()
            .rposition(|row| bookmark_side(row, here) == Ordering::Less)
    }
}

/// Applies a bookmark edit: the bookmark the dialog started from is found by
/// value (the list may have changed meanwhile). False when it is gone.
pub(super) fn apply_bookmark_edit(bookmarks: &mut [Bookmark], edit: &BookmarkEdit) -> bool {
    match bookmarks
        .iter_mut()
        .find(|bookmark| **bookmark == edit.original)
    {
        Some(bookmark) => {
            bookmark.name = edit.name.clone();
            bookmark.note = edit.note.clone();
            true
        }
        None => false,
    }
}

/// HH:MM:SS with at least two digits for the hours.
pub(super) fn clock_text(seconds: f64) -> String {
    let total = if seconds.is_finite() {
        seconds.max(0.) as u64
    } else {
        0
    };
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        total / 60 % 60,
        total % 60
    )
}

pub(super) fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

/// A text export: one block per bookmark, a blank line between blocks. The
/// first line is `HH:MM:SS  Book title — file name` (then the bookmark's name
/// in quotes, when it has one), the next line is the note, when there is one.
pub(super) fn bookmarks_text(rows: &[BookmarkRow]) -> String {
    let blocks: Vec<String> = rows
        .iter()
        .map(|row| {
            let mut block = format!("{}  {}", clock_text(row.seconds), one_line(&row.book));
            if !row.file.is_empty() {
                block.push_str(&format!(" — {}", one_line(&row.file)));
            }
            if !row.name.trim().is_empty() {
                block.push_str(&format!("  “{}”", one_line(row.name.trim())));
            }
            block.push('\n');
            let note = row.note.trim().replace("\r\n", "\n").replace('\r', "\n");
            if !note.is_empty() {
                block.push_str(&note);
                block.push('\n');
            }
            block
        })
        .collect();
    blocks.join("\n")
}

/// One CSV field (RFC 4180 quoting). A text that a spreadsheet would run as a
/// formula (it starts with = + - @ or a tab or carriage return) gets a leading
/// apostrophe, so tags and notes cannot inject formulas.
pub(super) fn csv_field(text: &str) -> String {
    let mut value = text.to_owned();
    if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        value.insert(0, '\'');
    }
    if value.contains([',', '"', '\n', '\r']) || value.starts_with(' ') || value.ends_with(' ') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}

/// A CSV export with the columns book, file, position_seconds, time, name and
/// note. UTF-8 with a byte-order mark so spreadsheets read the titles right.
pub(super) fn bookmarks_csv(rows: &[BookmarkRow]) -> String {
    let mut out = String::from("\u{feff}book,file,position_seconds,time,name,note\r\n");
    for row in rows {
        let seconds = if row.seconds.is_finite() {
            row.seconds.max(0.)
        } else {
            0.
        };
        let position = format!("{seconds:.3}");
        let position = position.trim_end_matches('0').trim_end_matches('.');
        out.push_str(&format!(
            "{},{},{},{},{},{}\r\n",
            csv_field(&row.book),
            csv_field(&row.file),
            position,
            clock_text(seconds),
            csv_field(&row.name),
            csv_field(&row.note),
        ));
    }
    out
}

/// The file name the export dialog suggests.
pub(super) fn bookmark_export_name(book: Option<&str>) -> String {
    let base: String = book
        .unwrap_or("")
        .chars()
        .map(|character| {
            if character.is_control() || "/\\:*?\"<>|".contains(character) {
                '_'
            } else {
                character
            }
        })
        .take(80)
        .collect();
    let base = base.trim().trim_end_matches('.').trim_end();
    if base.is_empty() {
        "bookmarks.txt".to_owned()
    } else {
        format!("{base} bookmarks.txt")
    }
}
