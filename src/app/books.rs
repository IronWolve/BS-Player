use super::*;

impl PlayerApp {
    /// The BOOKS list of the sidebar: one two-line row per book (its title and how
    /// far along it is), under collapsible author headings when there are more
    /// than eight books. Only the rows in view are drawn, so thousands of books
    /// stay smooth.
    pub(super) fn books_list(&mut self, ui: &mut egui::Ui, catalog: &LibraryCatalog) {
        let progress = self.gui_cache.book_progress(
            &self.state,
            self.library_revision,
            self.progress_revision,
        );
        let groups = use_author_groups(
            catalog.books.len(),
            self.state.settings.group_books_by_author,
        )
        .then(|| catalog.author_groups());
        let rows = book_rows(catalog.books.len(), groups, &self.state.collapsed_authors);
        let listening = self.state.settings.show_remaining_time;
        let detail_height = appearance::detail_row_height(ui);
        let heading_height = appearance::heading_row_height(ui);
        let gap = ui.spacing().item_spacing.y;
        // Where every row starts and ends below the first one.
        let mut tops = Vec::with_capacity(rows.len());
        let mut ends = Vec::with_capacity(rows.len());
        let mut y = 0.;
        for row in &rows {
            tops.push(y);
            y += match row {
                BookRow::Author(_) => heading_height,
                BookRow::Book(_) => detail_height,
            };
            ends.push(y);
            y += gap;
        }
        let total = y;
        let origin = ui.cursor().top();
        let clip = ui.clip_rect();
        let first = ends.partition_point(|&end| end < clip.top() - origin);
        let last = tops
            .partition_point(|&top| top <= clip.bottom() - origin)
            .max(first);
        let skipped = tops.get(first).copied().unwrap_or(total);
        ui.add_space(skipped);
        // Every row takes one widget id; the rows in view keep theirs as the list scrolls.
        ui.skip_ahead_auto_ids(first);
        let mut drawn_to = skipped;
        let mut finish: Option<String> = None;
        let mut reset: Option<(String, String)> = None;
        for &row in &rows[first..last] {
            let height = match row {
                BookRow::Author(_) => heading_height,
                BookRow::Book(_) => detail_height,
            };
            drawn_to += height + gap;
            match row {
                BookRow::Author(index) => {
                    let Some(group) = catalog.author_groups().get(index) else {
                        ui.add_space(height + gap);
                        continue;
                    };
                    let collapsed = self
                        .state
                        .collapsed_authors
                        .iter()
                        .any(|key| *key == group.key);
                    let label = if group.label.is_empty() {
                        "Unknown author"
                    } else {
                        group.label.as_str()
                    };
                    let response =
                        appearance::navigation_heading(ui, label, group.books.len(), !collapsed);
                    if response.clicked() {
                        if collapsed {
                            self.state.collapsed_authors.retain(|key| *key != group.key);
                        } else {
                            self.state.collapsed_authors.push(group.key.clone());
                        }
                    }
                }
                BookRow::Book(position) => {
                    let Some((book, label)) = catalog.books.get(position) else {
                        ui.add_space(height + gap);
                        continue;
                    };
                    let key = format!("Book:{book}");
                    // The list is ordered by label, so books with the same name
                    // are neighbours.
                    let shared = position > 0 && catalog.books[position - 1].1 == *label
                        || catalog
                            .books
                            .get(position + 1)
                            .is_some_and(|next| next.1 == *label);
                    let shown: std::borrow::Cow<str> = if shared {
                        disambiguated_label(label, book).into()
                    } else {
                        label.as_str().into()
                    };
                    let speed = self.book_speed_at(catalog, position);
                    let summary = progress.get(position);
                    let detail = summary
                        .map(|summary| book_row_detail(summary, speed, listening))
                        .unwrap_or_default();
                    let response = navigation_detail(
                        ui,
                        &shown,
                        &detail,
                        Icon::Book,
                        self.state.section == key,
                    )
                    .on_hover_ui(|ui| {
                        ui.set_max_width(360.);
                        match summary {
                            Some(summary) => ui.label(book_row_tooltip(
                                &shown,
                                catalog.book_author(position),
                                book,
                                summary,
                                speed,
                            )),
                            None => ui.label(book.as_str()),
                        };
                    });
                    response.context_menu(|ui| {
                        if ui.button("Mark book finished").clicked() {
                            finish = Some(book.clone());
                            ui.close();
                        }
                        if ui.button("Mark book not started…").clicked() {
                            reset = Some((book.clone(), shown.to_string()));
                            ui.close();
                        }
                    });
                    if response.clicked() {
                        self.state.section = key;
                        self.state.scroll = 0.;
                        self.restore_scroll = true;
                    }
                }
            }
        }
        ui.add_space((total - drawn_to).max(0.));
        if let Some(key) = finish {
            self.mark_book_finished(&key);
        }
        if let Some((key, label)) = reset {
            self.confirm = Some(Confirm::BookNotStarted { key, label });
        }
    }

    /// Marks every file of the book `key` finished (see `Session::mark_book_finished`).
    pub(super) fn mark_book_finished(&mut self, key: &str) {
        self.checkpoint();
        // Private listening writes no play counts or dates.
        let record_history = !self.state.settings.private_listening;
        let before = self.state.capture_book_progress(key);
        let count = self
            .state
            .mark_book_finished(key, state::now(), record_history);
        if count == 0 {
            self.set_brief_status("That book has no files to mark");
            return;
        }
        self.progress_changed();
        self.save();
        self.record_undo(
            "Mark book finished",
            state::UndoRestore::Progress {
                before,
                reposition: false,
            },
            format!(
                "Marked {count} {} finished",
                if count == 1 { "file" } else { "files" }
            ),
        );
    }

    /// Puts every file of the book `key` back to the start and not finished.
    pub(super) fn mark_book_not_started(&mut self, key: &str) {
        let playing_here = self
            .state
            .current_track()
            .is_some_and(|track| !track.superseded && track.book_key() == key);
        // What Undo puts back, with the loaded file's live position included.
        self.checkpoint();
        let before = self.state.capture_book_progress(key);
        if playing_here {
            // The loaded file goes back to its start too.
            self.seek(0.);
        }
        let reset = self.state.mark_book_not_started(key);
        self.progress_changed();
        self.save();
        let note = match reset {
            0 => "That book had no progress".to_owned(),
            1 => "Reset the progress of 1 file".to_owned(),
            count => format!("Reset the progress of {count} files"),
        };
        if reset == 0 {
            self.set_brief_status(note);
        } else {
            self.record_undo(
                "Mark book not started",
                state::UndoRestore::Progress {
                    before,
                    reposition: playing_here,
                },
                note,
            );
        }
    }

    /// The speed shown for the book at `position`: the live speed when it is
    /// the loaded book, else the speed it will start at.
    pub(super) fn book_speed_at(&self, catalog: &LibraryCatalog, position: usize) -> f32 {
        let loaded = self
            .state
            .current
            .as_deref()
            .and_then(|id| catalog.book_position_of(id));
        if loaded == Some(position) {
            self.state.speed
        } else {
            catalog
                .books
                .get(position)
                .map_or(1., |(key, _)| remembered_book_speed(&self.state, key))
        }
    }

    /// What the file list's summary line says in place of the total length when a
    /// book is shown: its time left ("3 h 12 m left") or "Finished". `None` outside
    /// a book and for a book not started, which keep the total length.
    pub(super) fn book_header_note(&mut self) -> Option<String> {
        let key = self.state.section.strip_prefix("Book:")?.to_owned();
        let catalog = self
            .gui_cache
            .catalog(&self.state.library, self.library_revision);
        let position = catalog.book_position(&key)?;
        let books = self.gui_cache.book_progress(
            &self.state,
            self.library_revision,
            self.progress_revision,
        );
        let book = books.get(position)?;
        let speed = self.book_speed_at(&catalog, position);
        match book.state {
            BookState::NotStarted => None,
            _ => Some(book_row_detail(
                book,
                speed,
                self.state.settings.show_remaining_time,
            )),
        }
    }

    /// What the player bar shows for the loaded audiobook, and its tooltip: the
    /// book's progress with the loaded file at its live position `shown`.
    pub(super) fn book_chip(&mut self, shown: f64, speed: f32, listening: bool) -> Option<(String, String)> {
        let id = self.state.current.clone()?;
        let catalog = self
            .gui_cache
            .catalog(&self.state.library, self.library_revision);
        let track = catalog.track(&id).filter(|track| track.audiobook)?;
        let position = catalog.book_position_of(&id)?;
        let books = self.gui_cache.book_progress(
            &self.state,
            self.library_revision,
            self.progress_revision,
        );
        let book = books.get(position)?.with_live_position(
            track,
            self.state.progress.get(&id),
            shown,
            speed,
        );
        let label = catalog
            .books
            .get(position)
            .map_or_else(String::new, |(_, label)| label.clone());
        Some(book_chip_texts(&label, &book, speed, listening))
    }
}

/// The speed a book starts at: the one remembered for it, else the audiobook
/// default (the same choice as when its first file is loaded).
pub(super) fn remembered_book_speed(state: &Session, key: &str) -> f32 {
    let speed = if state.settings.remember_book_speed {
        state
            .book_speeds
            .get(key)
            .copied()
            .unwrap_or(state.settings.book_speed)
    } else {
        state.settings.book_speed
    };
    if speed.is_finite() {
        speed.clamp(0.25, 3.)
    } else {
        1.
    }
}

/// A time left for a book, rounded to the minute: "3 h 12 m", "45 m", "<1 m".
pub(super) fn book_time_text(seconds: f64) -> String {
    let seconds = if seconds.is_finite() {
        seconds.max(0.)
    } else {
        0.
    };
    let minutes = (seconds / 60. + 0.5) as u64;
    if minutes == 0 {
        return if seconds > 0. { "<1 m" } else { "0 m" }.to_owned();
    }
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes} m"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} m"),
    }
}

/// The second line of a book's row in the sidebar and the ending of the file
/// list's summary: the time left (at `speed` when `listening`, else the content
/// time), "Finished" or "Not started".
pub(super) fn book_row_detail(book: &BookProgress, speed: f32, listening: bool) -> String {
    match book.state {
        BookState::Finished => "Finished".to_owned(),
        BookState::NotStarted => "Not started".to_owned(),
        BookState::InProgress => {
            let left = if listening {
                book.left_at(speed)
            } else {
                book.left_seconds
            };
            format!("{} left", book_time_text(left))
        }
    }
}

/// The tooltip of a book's sidebar row.
pub(super) fn book_row_tooltip(
    label: &str,
    author: &str,
    key: &str,
    book: &BookProgress,
    speed: f32,
) -> String {
    let mut text = label.to_owned();
    if !author.is_empty() {
        text.push_str(&format!("\n{author}"));
    }
    text.push_str(&format!(
        "\n{} {} · {}",
        book.files,
        if book.files == 1 { "file" } else { "files" },
        book_time_text(book.total_seconds)
    ));
    match book.state {
        BookState::NotStarted => text.push_str("\nNot started"),
        BookState::Finished => text.push_str("\nFinished"),
        BookState::InProgress => text.push_str(&format!(
            "\n{} % listened · {} left ({} at {:.2}× speed)",
            book.whole_percent(),
            book_time_text(book.left_seconds),
            book_time_text(book.left_at(speed)),
            speed
        )),
    }
    if book.unknown_files > 0 {
        text.push_str(&format!(
            "\n{} without a known length are not counted",
            if book.unknown_files == 1 {
                "1 file".to_owned()
            } else {
                format!("{} files", book.unknown_files)
            }
        ));
    }
    text.push_str(&format!("\n{key}"));
    text
}

/// The player bar's book readout `Book 42 % · 3 h 12 m left` and its tooltip.
/// The time follows the bar's own switch: listening time at `speed` when
/// `listening`, else content time.
pub(super) fn book_chip_texts(
    label: &str,
    book: &BookProgress,
    speed: f32,
    listening: bool,
) -> (String, String) {
    let left = if listening {
        book.left_at(speed)
    } else {
        book.left_seconds
    };
    let text = if book.state == BookState::Finished {
        "Book finished".to_owned()
    } else {
        format!(
            "Book {} % · {} left",
            book.whole_percent(),
            book_time_text(left)
        )
    };
    let mut tip = format!(
        "{label}\nListened to {} of {} ({} % · {} of {} files finished)\n",
        book_time_text(book.listened_seconds),
        book_time_text(book.total_seconds),
        book.whole_percent(),
        book.finished_files,
        book.files
    );
    tip.push_str(&format!(
        "{} of audio left, {} at {speed:.2}× speed.",
        book_time_text(book.left_seconds),
        book_time_text(book.left_at(speed))
    ));
    if book.unknown_files > 0 {
        tip.push_str(&format!(
            "\n{} without a known length {} not counted.",
            if book.unknown_files == 1 {
                "1 file".to_owned()
            } else {
                format!("{} files", book.unknown_files)
            },
            if book.unknown_files == 1 { "is" } else { "are" }
        ));
    }
    tip.push_str("\nClick to switch the time labels between content time and listening time.");
    (text, tip)
}
