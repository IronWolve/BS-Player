use super::*;

impl PlayerApp {
    /// The browse list (Artists, Albums or Genres) the section shows, if any.
    pub(super) fn browse_kind(&self) -> Option<BrowseKind> {
        match browse_section(&self.state.section) {
            Some(BrowseSection::List(kind)) => Some(kind),
            _ => None,
        }
    }

    /// Opens the drill-down of a group: its files in the usual list, under a
    /// heading that leads back. The list is shown whatever tab was open, and
    /// the search that found the group is done with.
    pub(super) fn open_group(&mut self, kind: BrowseKind, key: &str) {
        self.browse_pending = None;
        self.state.section = group_section(kind, key);
        self.state.tab = "Files".into();
        self.state.search.clear();
        self.state.scroll = 0.;
        self.restore_scroll = true;
        self.browse_selected = Some(key.to_owned());
        self.gui_context.request_repaint();
    }

    /// Leaves a drill-down for its list, with the group still selected and in
    /// view. Returns whether a drill-down was open.
    pub(super) fn close_drilldown(&mut self) -> bool {
        let Some(BrowseSection::Group(kind, key)) = browse_section(&self.state.section) else {
            return false;
        };
        let key = key.to_owned();
        self.state.section = kind.list_section().to_owned();
        self.state.scroll = 0.;
        self.restore_scroll = true;
        self.browse_selected = Some(key);
        self.browse_reveal = true;
        self.gui_context.request_repaint();
        true
    }

    /// A drill-down whose group is gone (a rescan or a tag edit renamed it, or
    /// its last file left the library, or the profile names a group this
    /// library never had) opens its list instead.
    pub(super) fn settle_browse_section(&mut self) {
        let Some(BrowseSection::Group(kind, key)) = browse_section(&self.state.section) else {
            return;
        };
        let catalog = self
            .gui_cache
            .catalog(&self.state.library, self.library_revision);
        if catalog.browse().group(kind, key).is_some() {
            return;
        }
        self.state.section = kind.list_section().to_owned();
        self.state.scroll = 0.;
        self.restore_scroll = true;
        self.browse_selected = None;
    }

    /// Opens the group of a click on a browse row once no second click can
    /// follow, and wakes the player for that moment.
    pub(super) fn resolve_browse_pending(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.browse_pending else {
            return;
        };
        if self.browse_kind() != Some(pending.kind) {
            self.browse_pending = None;
            return;
        }
        let left = pending.at + BROWSE_OPEN_DELAY - ctx.input(|input| input.time);
        if left > 0. {
            ctx.request_repaint_after(Duration::from_secs_f64(left));
            return;
        }
        let (kind, key) = (pending.kind, pending.key.clone());
        self.open_group(kind, &key);
    }

    /// The files of a group in drill-down order, kept for the library revision.
    pub(super) fn group_ids(&mut self, kind: BrowseKind, key: &str) -> Arc<Vec<String>> {
        if let Some((known_kind, known_key, revision, ids)) = &self.browse_ids {
            if *known_kind == kind && known_key == key && *revision == self.library_revision {
                return Arc::clone(ids);
            }
        }
        let catalog = self
            .gui_cache
            .catalog(&self.state.library, self.library_revision);
        let ids = Arc::new(catalog.group_file_ids(kind, key));
        self.browse_ids = Some((
            kind,
            key.to_owned(),
            self.library_revision,
            Arc::clone(&ids),
        ));
        ids
    }

    /// Play on a browse row: the group's files become the queue, in the order
    /// its drill-down lists them, and the first one plays.
    pub(super) fn play_group(&mut self, kind: BrowseKind, key: &str) {
        let ids = self.group_ids(kind, key);
        let Some(first) = ids.first() else {
            return;
        };
        self.state.queue = ids.to_vec();
        self.cancel_playing_drop();
        self.state.playing_workspace = false;
        self.view_revision = self.view_revision.wrapping_add(1);
        self.load_track(first, true, None);
    }

    /// Up / Down / Home / End and Enter in a browse list: the arrows move the
    /// selected row, Enter opens its group.
    pub(super) fn browse_keys(&mut self, ctx: &egui::Context) {
        let Some(view) = self
            .gui_cache
            .browse_view(&self.state, self.library_revision)
        else {
            return;
        };
        if let Some((step, _)) = ctx.input_mut(list_step) {
            if !view.is_empty() {
                let current = self
                    .browse_selected
                    .as_deref()
                    .and_then(|key| view.row_of(key));
                let last = view.len() - 1;
                let next = match (step, current) {
                    (ListStep::Down, Some(current)) => (current + 1).min(last),
                    (ListStep::Up, Some(current)) => current.saturating_sub(1),
                    (ListStep::Home, _) => 0,
                    (ListStep::End, _) => last,
                    // Nothing selected: the first press selects the first row.
                    (_, None) => 0,
                };
                self.browse_selected = view.group(next).map(|group| group.key.clone());
                let style = ctx.global_style();
                let body_font = egui::TextStyle::Body.resolve(&style);
                let small_font = egui::TextStyle::Small.resolve(&style);
                let minimum = ctx.fonts_mut(|fonts| {
                    fonts.row_height(&body_font) + fonts.row_height(&small_font) + 8.
                });
                let pitch = self.state.settings.row_height.max(minimum) + 2.;
                if let Some(offset) = scroll_to_reveal(
                    self.state.scroll,
                    self.list_viewport,
                    next as f32 * pitch,
                    pitch,
                ) {
                    self.state.scroll = offset;
                    self.restore_scroll = true;
                }
            }
        }
        if self.error.is_none() && ctx.input_mut(|i| consume_plain_key(i, egui::Key::Enter)) {
            if let Some(key) = self.browse_selected.clone() {
                if view.row_of(&key).is_some() {
                    self.open_group(view.kind, &key);
                }
            }
        }
    }

    /// The right-click menu of a browse row.
    pub(super) fn browse_menu(&mut self, ui: &mut egui::Ui, kind: BrowseKind, key: &str) {
        let ids = self.group_ids(kind, key);
        if ui.button("Play").clicked() {
            self.actions.push(Action::PlayGroup(kind, key.to_owned()));
            ui.close();
        }
        if ui.button("Play next").clicked() {
            self.actions
                .push(Action::Bulk(BulkAction::PlayNext, ids.to_vec()));
            ui.close();
        }
        if ui.button("Add to queue").clicked() {
            self.actions
                .push(Action::Bulk(BulkAction::AddToQueue, ids.to_vec()));
            ui.close();
        }
        self.add_to_playlist_menu(ui, "Add to playlist", &ids);
        ui.separator();
        if ui.button("Open").clicked() {
            self.actions.push(Action::OpenGroup(kind, key.to_owned()));
            ui.close();
        }
    }

    /// The order selector of a browse list; the choice is remembered for that
    /// list in the profile.
    pub(super) fn browse_sort_selector(&mut self, ui: &mut egui::Ui, kind: BrowseKind) {
        let current = browse_sort_of(&self.state.browse_sort, kind);
        egui::ComboBox::from_id_salt(("browse sort", kind.list_section()))
            .selected_text(format!("Sort: {}", current.label()))
            .width(150.)
            .show_ui(ui, |ui| {
                for &option in BrowseSort::options(kind) {
                    if ui
                        .selectable_label(option == current, option.describe())
                        .clicked()
                    {
                        self.state.browse_sort =
                            with_browse_sort(&self.state.browse_sort, kind, option);
                        self.state.scroll = 0.;
                        self.restore_scroll = true;
                        ui.close();
                    }
                }
            });
    }

    /// The heading of a drill-down in place of the plain title: a Back button
    /// and "Artists › Brooks & Dunn", where the list's name is a link back.
    pub(super) fn drilldown_header(
        &mut self,
        ui: &mut egui::Ui,
        kind: BrowseKind,
        title: &str,
        size: f32,
    ) {
        let mut back = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.;
            if ui
                .button("‹ Back")
                .on_hover_text("Back to the list (Esc)")
                .clicked()
            {
                back = true;
            }
            let link = ui
                .add(egui::Link::new(
                    RichText::new(kind.list_section()).font(appearance::semibold(size)),
                ))
                .on_hover_text("Back to the list");
            if link.clicked() {
                back = true;
            }
            ui.label(
                RichText::new("›")
                    .font(appearance::semibold(size))
                    .color(ui.visuals().weak_text_color()),
            );
            ui.add(
                egui::Label::new(RichText::new(title).font(appearance::semibold(size))).truncate(),
            )
            .on_hover_text(title);
        });
        if back {
            self.close_drilldown();
        }
    }

    /// The Artists, Albums or Genres list: one row per group with its counts.
    /// Only the rows in view are laid out, so thousands of groups stay smooth.
    pub(super) fn browse_ui(&mut self, ui: &mut egui::Ui, kind: BrowseKind) {
        let Some(view) = self
            .gui_cache
            .browse_view(&self.state, self.library_revision)
        else {
            return;
        };
        let font = self.state.settings.font_size;
        ui.add_space(5.);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(kind.list_section()).font(appearance::semibold(font + 14.)),
                    )
                    .truncate(),
                );
                ui.label(
                    RichText::new(format!(
                        "{} · {}",
                        browse_summary(kind, view.len(), view.total, view.tracks),
                        duration_summary(view.duration)
                    ))
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                self.browse_sort_selector(ui, kind);
            });
        });
        ui.add_space(8.);
        if view.total == 0 {
            ui.add_space(27.);
            ui.vertical_centered(|ui| {
                ui.heading(format!("No {} yet", browse_noun(kind, 2)));
                ui.label(format!(
                    "The {} come from the tags of your music files. Add music files or a folder.",
                    browse_noun(kind, 2)
                ));
                ui.add_space(12.);
                if widgets::text_icon_button(ui, Icon::Plus, "Add files").clicked() {
                    self.choose_files(false);
                }
                if widgets::text_icon_button(ui, Icon::Folder, "Add folder").clicked() {
                    self.choose_files(true);
                }
            });
            return;
        }
        if view.is_empty() {
            ui.add_space(27.);
            ui.vertical_centered(|ui| {
                ui.heading(format!("No matching {}", browse_noun(kind, 2)));
                ui.label(format!(
                    "Clear the search field to see every {}.",
                    browse_noun(kind, 1)
                ));
            });
            return;
        }
        let (titles, fixed) = browse_columns(kind);
        let fixed_total: f32 = fixed.iter().sum();
        let name_width = (ui.available_width().max(260.) - fixed_total).max(180.);
        let total_width = name_width + fixed_total;
        let widths = [name_width, fixed[0], fixed[1], fixed[2]];
        let header_height = (ui.text_style_height(&egui::TextStyle::Body) + 12.).max(28.);
        let row_height = self.state.settings.row_height.max(
            ui.text_style_height(&egui::TextStyle::Body)
                + ui.text_style_height(&egui::TextStyle::Small)
                + 8.,
        );
        let double_click_plays = self.state.settings.double_click_plays;
        egui::ScrollArea::horizontal()
            .id_salt("browse horizontal")
            .show(ui, |ui| {
                ui.set_min_width(total_width);
                let (header, _) = ui.allocate_exact_size(
                    Vec2::new(total_width, header_height),
                    egui::Sense::hover(),
                );
                let body_font = egui::TextStyle::Body.resolve(ui.style());
                let mut x = header.left();
                for (index, (title, column)) in titles.into_iter().zip(widths).enumerate() {
                    let (anchor, align) = if index == 0 {
                        (
                            egui::pos2(x + 14., header.center().y),
                            egui::Align2::LEFT_CENTER,
                        )
                    } else {
                        (
                            egui::pos2(x + column - 14., header.center().y),
                            egui::Align2::RIGHT_CENTER,
                        )
                    };
                    ui.painter().text(
                        anchor,
                        align,
                        title,
                        body_font.clone(),
                        ui.visuals().text_color(),
                    );
                    x += column;
                }
                ui.painter().line_segment(
                    [header.left_bottom(), header.right_bottom()],
                    egui::Stroke::new(1., appearance::separator(ui)),
                );
                ui.add_space(8.);
                let previous_spacing = ui.spacing().item_spacing.y;
                ui.spacing_mut().item_spacing.y = 2.;
                let thumb_side = if kind == BrowseKind::Albums && self.state.settings.show_list_art
                {
                    (row_height - 6.).max(0.)
                } else {
                    0.
                };
                if !self.state.settings.show_list_art && !self.thumbs.cache.is_empty() {
                    self.thumbs.reset();
                }
                let mut scroll = egui::ScrollArea::vertical()
                    .id_salt(("browse rows", kind.list_section()))
                    .auto_shrink([false, false])
                    .max_height(ui.available_height().max(1.));
                let revealed = self
                    .browse_reveal
                    .then(|| {
                        self.browse_selected
                            .as_deref()
                            .and_then(|key| view.row_of(key))
                    })
                    .flatten();
                self.browse_reveal = false;
                if let Some(row) = revealed {
                    // Back from a drill-down: the group it showed is in the middle.
                    let pitch = row_height + 2.;
                    scroll = scroll.vertical_scroll_offset(
                        (row as f32 * pitch - (ui.available_height() - pitch) / 2.).max(0.),
                    );
                    self.restore_scroll = false;
                } else if self.restore_scroll {
                    scroll = scroll.vertical_scroll_offset(self.state.scroll);
                    self.restore_scroll = false;
                }
                let now = Instant::now();
                let mut wanted: Vec<(ThumbKey, ThumbJob)> = Vec::new();
                let output = scroll.show_rows(ui, row_height, view.len(), |ui, range| {
                    for index in range {
                        let Some(group) = view.group(index) else {
                            continue;
                        };
                        let selected = self.browse_selected.as_deref() == Some(group.key.as_str());
                        let (rect, response) = ui.allocate_exact_size(
                            Vec2::new(total_width, row_height),
                            egui::Sense::click(),
                        );
                        ui.painter().rect_filled(
                            rect,
                            5.,
                            appearance::row_fill(ui, selected, index % 2 == 1),
                        );
                        let text = appearance::row_text(ui, selected);
                        let weak = if selected {
                            text
                        } else {
                            ui.visuals().weak_text_color()
                        };
                        // The name, with the cover and the artist for an album.
                        let mut cell = egui::Rect::from_min_size(
                            egui::pos2(rect.left() + 14., rect.top()),
                            Vec2::new((name_width - 18.).max(1.), row_height),
                        );
                        if thumb_side > 0.
                            && cell.width() >= thumb_side + THUMB_GAP + THUMB_MIN_TEXT
                        {
                            if let Some(track) = view.cover(index) {
                                self.row_thumbnail(
                                    ui,
                                    egui::Rect::from_min_size(
                                        egui::pos2(cell.left(), rect.top() + 3.),
                                        Vec2::splat(thumb_side),
                                    ),
                                    track,
                                    now,
                                    &mut wanted,
                                );
                            }
                            cell.min.x += thumb_side + THUMB_GAP;
                        }
                        {
                            let mut cell_ui = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(cell)
                                    .layout(egui::Layout::top_down(egui::Align::Min)),
                            );
                            cell_ui.style_mut().interaction.selectable_labels = false;
                            let ui = &mut cell_ui;
                            ui.set_clip_rect(cell.intersect(ui.clip_rect()));
                            ui.spacing_mut().item_spacing.y = 2.;
                            ui.add_space(3.);
                            let unnamed = group.label.is_empty();
                            let name = if unnamed {
                                kind.unknown()
                            } else {
                                group.label.as_str()
                            };
                            let name = RichText::new(name).color(if unnamed { weak } else { text });
                            if kind == BrowseKind::Albums {
                                ui.add(egui::Label::new(name).truncate());
                                let artist = if group.artist.is_empty() {
                                    BrowseKind::Artists.unknown()
                                } else {
                                    group.artist.as_str()
                                };
                                ui.add(
                                    egui::Label::new(RichText::new(artist).small().color(weak))
                                        .truncate(),
                                );
                            } else {
                                ui.add_space((row_height - 28.) / 2.);
                                ui.add(egui::Label::new(name).truncate());
                            }
                        }
                        // The counts, right-aligned.
                        let mut x = rect.left() + name_width;
                        for (cell_text, column) in
                            browse_cells(kind, group).iter().zip(&widths[1..])
                        {
                            let numbers = egui::Rect::from_min_size(
                                egui::pos2(x, rect.top()),
                                Vec2::new(*column, row_height),
                            );
                            ui.painter()
                                .with_clip_rect(numbers.intersect(ui.clip_rect()))
                                .text(
                                    egui::pos2(numbers.right() - 14., numbers.center().y),
                                    egui::Align2::RIGHT_CENTER,
                                    cell_text,
                                    body_font.clone(),
                                    text,
                                );
                            x += column;
                        }
                        if self.refocus_list && selected {
                            response.request_focus();
                            self.refocus_list = false;
                        }
                        if response.has_focus() {
                            self.list_focus = Some(response.id);
                        }
                        if response.clicked() || response.secondary_clicked() {
                            response.request_focus();
                            self.list_focus = Some(response.id);
                            self.browse_selected = Some(group.key.clone());
                        }
                        if response.double_clicked() && double_click_plays {
                            // The first click is waiting to open the group; this
                            // is the second of a double-click.
                            self.browse_pending = None;
                            self.actions
                                .push(Action::PlayGroup(kind, group.key.clone()));
                        } else if response.clicked() {
                            if double_click_plays {
                                self.browse_pending = Some(BrowsePending {
                                    kind,
                                    key: group.key.clone(),
                                    at: ui.input(|input| input.time),
                                });
                                ui.ctx().request_repaint_after(Duration::from_secs_f64(
                                    BROWSE_OPEN_DELAY,
                                ));
                            } else {
                                self.actions
                                    .push(Action::OpenGroup(kind, group.key.clone()));
                            }
                        }
                        response
                            .on_hover_ui(|ui| {
                                ui.set_max_width(360.);
                                ui.label(browse_tooltip(kind, group));
                            })
                            .context_menu(|ui| self.browse_menu(ui, kind, &group.key));
                    }
                });
                self.state.scroll = output.state.offset.y;
                self.list_viewport = output.inner_rect.height();
                ui.spacing_mut().item_spacing.y = previous_spacing;
                self.thumbs.submit(wanted, &self.tx, &self.gui_context);
            });
    }
}

/// Whether the LIBRARY entry `name` is the section being shown: the entry
/// itself; for Audiobooks one of its books; for Artists, Albums and Genres one
/// of their groups.
pub(super) fn library_entry_selected(section: &str, name: &str) -> bool {
    section == name
        || name == "Audiobooks" && section.starts_with("Book:")
        || matches!(
            browse_section(section),
            Some(BrowseSection::Group(kind, _)) if kind.list_section() == name
        )
}

/// "artist" or "artists" and so on, for a count.
pub(super) fn browse_noun(kind: BrowseKind, count: usize) -> String {
    if count == 1 {
        kind.noun().to_owned()
    } else {
        format!("{}s", kind.noun())
    }
}

/// "5 artists · 61 tracks", or "2 of 5 artists · 12 tracks" while a search
/// narrows the list: the status line and the header of a browse list.
pub(super) fn browse_summary(
    kind: BrowseKind,
    shown: usize,
    total: usize,
    tracks: usize,
) -> String {
    let groups = if shown == total {
        format!("{total} {}", browse_noun(kind, total))
    } else {
        format!("{shown} of {total} {}", browse_noun(kind, total))
    };
    format!(
        "{groups} · {tracks} {}",
        if tracks == 1 { "track" } else { "tracks" }
    )
}

/// The titles of a browse list's columns (the first is the name) and the fixed
/// widths of the three count columns after it.
pub(super) fn browse_columns(kind: BrowseKind) -> ([&'static str; 4], [f32; 3]) {
    match kind {
        BrowseKind::Artists => (["Artist", "Albums", "Tracks", "Duration"], [88., 88., 120.]),
        BrowseKind::Albums => (["Album", "Year", "Tracks", "Duration"], [72., 84., 120.]),
        BrowseKind::Genres => (["Genre", "Tracks", "Albums", "Duration"], [88., 88., 120.]),
    }
}

/// The count cells of a row, in the order of `browse_columns` after the name.
/// The text is made for the rows in view only.
pub(super) fn browse_cells(kind: BrowseKind, group: &BrowseGroup) -> [String; 3] {
    let tracks = group.tracks().to_string();
    let duration = duration_summary(group.duration);
    match kind {
        BrowseKind::Artists => [group.albums.to_string(), tracks, duration],
        BrowseKind::Albums => [
            if group.year == 0 {
                String::new()
            } else {
                group.year.to_string()
            },
            tracks,
            duration,
        ],
        BrowseKind::Genres => [tracks, group.albums.to_string(), duration],
    }
}

/// What the tooltip of a browse row says.
pub(super) fn browse_tooltip(kind: BrowseKind, group: &BrowseGroup) -> String {
    let name = if group.label.is_empty() {
        kind.unknown()
    } else {
        group.label.as_str()
    };
    let mut lines = vec![name.to_owned()];
    if kind == BrowseKind::Albums {
        lines.push(if group.artist.is_empty() {
            BrowseKind::Artists.unknown().to_owned()
        } else {
            group.artist.clone()
        });
    }
    let mut counts = Vec::new();
    if kind != BrowseKind::Albums {
        counts.push(format!(
            "{} {}",
            group.albums,
            if group.albums == 1 { "album" } else { "albums" }
        ));
    }
    counts.push(format!(
        "{} {}",
        group.tracks(),
        if group.tracks() == 1 {
            "track"
        } else {
            "tracks"
        }
    ));
    counts.push(duration_summary(group.duration));
    lines.push(counts.join(" · "));
    lines.join("\n")
}
