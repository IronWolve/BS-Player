use super::*;

pub(super) fn chapter_picker_popup_id() -> egui::Id {
    egui::Id::new("now playing chapter selector")
}

pub(super) fn chapter_picker_search_id() -> egui::Id {
    egui::Id::new("now playing chapter search")
}

pub(super) fn chapter_picker_row_id(index: usize) -> egui::Id {
    egui::Id::new(("now playing chapter row", index))
}

/// State for the compact now-playing chapter selector. The filtered indices are
/// retained across frames: rebuilding them can otherwise make a large chapter
/// list cost O(chapters) every frame while its popup is open.
#[derive(Default)]
pub(super) struct ChapterPicker {
    open: bool,
    context_row: Option<usize>,
    query: String,
    selected: Option<usize>,
    chapters_identity: usize,
    filtered_query: String,
    filtered_indices: Vec<usize>,
    scroll_to_selection: bool,
}

impl ChapterPicker {
    pub(super) fn close(&mut self, ctx: &egui::Context) {
        egui::Popup::close_id(ctx, chapter_picker_popup_id());
        if let Some(index) = self.context_row {
            egui::Popup::close_id(ctx, chapter_picker_row_id(index).with("popup"));
        }
        *self = Self::default();
    }

    fn refresh_filter(&mut self, chapters: &[Chapter], identity: usize) {
        if self.chapters_identity == identity && self.filtered_query == self.query {
            return;
        }
        self.chapters_identity = identity;
        self.filtered_query.clone_from(&self.query);
        let query = self.query.trim().to_lowercase();
        self.filtered_indices = if query.is_empty() {
            (0..chapters.len()).collect()
        } else {
            chapters
                .iter()
                .enumerate()
                .filter_map(|(index, chapter)| {
                    (chapter.title.to_lowercase().contains(&query)
                        || (index + 1).to_string().contains(&query))
                    .then_some(index)
                })
                .collect()
        };
        if self
            .selected
            .is_none_or(|selected| !self.filtered_indices.contains(&selected))
        {
            self.selected = self.filtered_indices.first().copied();
        }
        self.scroll_to_selection = true;
    }
}

impl PlayerApp {
    /// Draws the compact chapter button under the now-playing title and its
    /// searchable, virtualized chapter popup.
    pub(super) fn chapter_picker(&mut self, ui: &mut egui::Ui, position: f64) {
        let chapters = Arc::clone(&self.current_chapters);
        if chapters.is_empty() {
            return;
        }

        let current = self.chapter_cursor.locate(&chapters, position);
        let title = if let Some(index) = current {
            if self.chapter_line.as_ref().map(|(made_for, _)| *made_for) != Some(index) {
                if let Some(chapter) = chapters.get(index) {
                    self.chapter_line =
                        Some((index, chapter_text(index, chapters.len(), &chapter.title)));
                }
            }
            self.chapter_line
                .as_ref()
                .filter(|(made_for, _)| *made_for == index)
                .map(|(_, line)| line.clone())
                .unwrap_or_else(|| format!("Chapter {} of {}", index + 1, chapters.len()))
        } else {
            format!("Chapters ({})", chapters.len())
        };
        let available_width = ui.available_width();
        let response = widgets::text_menu_button_bounded(ui, &title, available_width);
        response.clone().on_hover_text(&title);
        let ctx = ui.ctx().clone();
        let popup_id = chapter_picker_popup_id();
        let context_open = self.chapter_picker_state.context_row.is_some_and(|index| {
            egui::Popup::is_id_open(&ctx, chapter_picker_row_id(index).with("popup"))
        });
        if context_open && response.clicked() {
            self.chapter_picker_state.close(&ctx);
            return;
        }
        let opening = response.clicked() && !egui::Popup::is_id_open(&ctx, popup_id);
        if opening {
            let state = &mut self.chapter_picker_state;
            state.query.clear();
            state.selected = current;
            state.scroll_to_selection = true;
        }

        let state = &mut self.chapter_picker_state;
        let identity = Arc::as_ptr(&chapters) as usize;
        if state.chapters_identity != 0 && state.chapters_identity != identity {
            state.query.clear();
            state.selected = current;
            state.open = false;
            egui::Popup::close_id(&ctx, popup_id);
        }
        state.open = egui::Popup::is_id_open(&ctx, popup_id);
        state.refresh_filter(&chapters, identity);
        if !state.open && !context_open && !response.clicked() {
            return;
        }

        let screen = ctx.content_rect();
        let max_popup_height = (screen.height() - 20.)
            .clamp(48., 460.)
            .min(screen.height());
        let popup_width = response
            .rect
            .width()
            .max(400.)
            .min((screen.width() - 16.).max(1.));
        let row_height =
            (ui.text_style_height(&egui::TextStyle::Body) + 12.).max(ui.spacing().interact_size.y);
        let mut actions = Vec::new();
        let track_id = self.state.current.clone().unwrap_or_default();
        let duration = self
            .state
            .current_track()
            .map(|track| track.duration)
            .unwrap_or_default();

        let popup = egui::Popup::new(popup_id, ctx.clone(), &response, response.layer_id);
        // egui has one memory-tracked root popup. A row context menu temporarily
        // owns that slot; keep its parent drawn without taking the slot back.
        let popup = if context_open {
            popup.open(true)
        } else {
            popup.open_memory(response.clicked().then_some(egui::SetOpenCommand::Toggle))
        };
        popup
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .align(egui::RectAlign::TOP_START)
            .gap(4.)
            .width(popup_width)
            .frame(egui::Frame::popup(ui.style()))
            .show(|popup_ui| {
                let content_top = popup_ui.cursor().top();
                popup_ui.set_max_height(max_popup_height - 16.);
                popup_ui.set_max_width(popup_width);
                popup_ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Chapters").strong());
                    ui.label(
                        egui::RichText::new(format!("{} chapters", chapters.len()))
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                });
                popup_ui.horizontal(|ui| {
                    let nav_width =
                        ((ui.available_width() - ui.spacing().item_spacing.x) / 2.).max(1.);
                    let nav_size = egui::vec2(nav_width, row_height);
                    let previous = egui::Button::new("‹ Previous · Alt+Up")
                        .truncate()
                        .fill(ui.visuals().widgets.inactive.bg_fill);
                    if ui.add_sized(nav_size, previous).clicked() {
                        actions.push(PickerAction::Step(false));
                    }
                    let next = egui::Button::new("Next · Alt+Down ›")
                        .truncate()
                        .fill(ui.visuals().widgets.inactive.bg_fill);
                    if ui.add_sized(nav_size, next).clicked() {
                        actions.push(PickerAction::Step(true));
                    }
                });

                let alt_down = popup_ui.input_mut(|input| {
                    !context_open && input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowDown)
                });
                let alt_up = popup_ui.input_mut(|input| {
                    !context_open && input.consume_key(egui::Modifiers::ALT, egui::Key::ArrowUp)
                });
                if alt_up {
                    actions.push(PickerAction::Step(false));
                }
                if alt_down {
                    actions.push(PickerAction::Step(true));
                }
                let down = popup_ui.input_mut(|input| {
                    !context_open && input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                });
                let up = popup_ui.input_mut(|input| {
                    !context_open && input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                });
                let enter = popup_ui.input_mut(|input| {
                    !context_open && input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                });
                if popup_ui.input_mut(|input| {
                    !context_open && input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
                }) {
                    state.open = false;
                    egui::Popup::close_id(&ctx, popup_id);
                }

                let search = popup_ui.add_enabled(
                    !context_open,
                    egui::TextEdit::singleline(&mut state.query)
                        .id(chapter_picker_search_id())
                        .hint_text("Search chapters")
                        .desired_width(f32::INFINITY),
                );
                if opening {
                    search.request_focus();
                }
                let query_changed = search.changed();
                if query_changed {
                    state.refresh_filter(&chapters, identity);
                }
                // Text and Enter can arrive in one frame. Choose from the
                // refreshed filter, never the previous query's selection.
                let selected_row = state
                    .selected
                    .and_then(|index| state.filtered_indices.iter().position(|&i| i == index));
                if (down || up) && !state.filtered_indices.is_empty() {
                    let count = state.filtered_indices.len();
                    let row = if down {
                        selected_row.map_or(0, |row| (row + 1).min(count - 1))
                    } else {
                        selected_row.unwrap_or(0).saturating_sub(1)
                    };
                    state.selected = Some(state.filtered_indices[row]);
                    state.scroll_to_selection = true;
                }
                if enter {
                    if let Some(index) = state.selected {
                        if let Some(chapter) = chapters.get(index) {
                            actions.push(PickerAction::Seek(index, chapter.start));
                        }
                    }
                }

                if state.filtered_indices.is_empty() {
                    popup_ui.label("No matching chapters");
                } else {
                    let row_step = row_height + popup_ui.spacing().item_spacing.y;
                    let footer_height = popup_ui.text_style_height(&egui::TextStyle::Small) * 2.
                        + popup_ui.spacing().item_spacing.y * 3.
                        + 12.;
                    let chrome_height = popup_ui.cursor().top() - content_top;
                    let max_list_height = (max_popup_height - 24. - chrome_height - footer_height)
                        .max(row_height)
                        .min(row_step * 8.);
                    let initial_scroll = if state.scroll_to_selection {
                        let selected_row = state
                            .selected
                            .and_then(|index| {
                                state.filtered_indices.iter().position(|&i| i == index)
                            })
                            .unwrap_or(0);
                        row_step * selected_row as f32
                    } else {
                        0.
                    };
                    let mut scroll = egui::ScrollArea::vertical()
                        .id_salt("now playing chapter rows")
                        .max_height(max_list_height);
                    if context_open {
                        // Keep the context row alive while its menu is drawn.
                        scroll = scroll.scroll_source(egui::scroll_area::ScrollSource::NONE);
                    }
                    if state.scroll_to_selection {
                        scroll = scroll.vertical_scroll_offset(initial_scroll);
                    }
                    scroll.show_rows(
                        popup_ui,
                        row_height,
                        state.filtered_indices.len(),
                        |rows_ui, visible| {
                            for row in visible {
                                let index = state.filtered_indices[row];
                                let Some(chapter) = chapters.get(index) else {
                                    continue;
                                };
                                let is_current = current == Some(index);
                                let is_selected = state.selected == Some(index);
                                let label = format!("{:02}  {}", index + 1, chapter.title);
                                let blue = rows_ui.visuals().hyperlink_color;
                                let (rect, _) = rows_ui.allocate_exact_size(
                                    egui::vec2(rows_ui.available_width(), row_height),
                                    egui::Sense::hover(),
                                );
                                let row_response = rows_ui.interact(
                                    rect,
                                    chapter_picker_row_id(index),
                                    egui::Sense::click(),
                                );
                                let row_fill = if is_current {
                                    blue
                                } else if row_response.hovered() {
                                    rows_ui.visuals().widgets.hovered.bg_fill
                                } else {
                                    egui::Color32::TRANSPARENT
                                };
                                if row_fill != egui::Color32::TRANSPARENT {
                                    rows_ui.painter().rect_filled(rect, 3., row_fill);
                                }
                                let row_color = if is_current {
                                    appearance::accent_foreground(blue)
                                } else {
                                    rows_ui.visuals().text_color()
                                };
                                if is_selected && !is_current {
                                    rows_ui.painter().rect_stroke(
                                        rect.shrink(1.),
                                        3.,
                                        egui::Stroke::new(1.5, blue),
                                        egui::StrokeKind::Inside,
                                    );
                                }
                                let time = time_text(chapter.start);
                                let time_font = egui::TextStyle::Small.resolve(rows_ui.style());
                                let time_galley =
                                    rows_ui.painter().layout_no_wrap(time, time_font, row_color);
                                let title_right = (rect.right() - time_galley.size().x - 20.)
                                    .max(rect.left() + 8.);
                                // Paint text without a selectable Label widget
                                // intercepting the row's left/right clicks.
                                let mut title_job = egui::text::LayoutJob::simple_singleline(
                                    label,
                                    egui::TextStyle::Body.resolve(rows_ui.style()),
                                    row_color,
                                );
                                title_job.wrap = egui::text::TextWrapping::truncate_at_width(
                                    (title_right - rect.left() - 8.).max(0.),
                                );
                                let title_galley = rows_ui.painter().layout_job(title_job);
                                rows_ui.painter().galley(
                                    egui::pos2(
                                        rect.left() + 8.,
                                        rect.center().y - title_galley.size().y / 2.,
                                    ),
                                    title_galley,
                                    row_color,
                                );
                                rows_ui.painter().galley(
                                    egui::pos2(
                                        rect.right() - 8. - time_galley.size().x,
                                        rect.center().y - time_galley.size().y / 2.,
                                    ),
                                    time_galley,
                                    row_color,
                                );
                                if row_response.clicked() {
                                    actions.push(PickerAction::Seek(index, chapter.start));
                                }
                                if row_response.secondary_clicked() {
                                    state.context_row = Some(index);
                                }
                                row_response
                                    .on_hover_text(chapter_tooltip(index, &chapters))
                                    .context_menu(|menu| {
                                        if menu.button("Add bookmark at chapter start").clicked() {
                                            actions.push(PickerAction::Bookmark(
                                                index,
                                                chapter.start,
                                                chapter.title.clone(),
                                            ));
                                            menu.close();
                                        }
                                        if menu.button("Sleep at this chapter end").clicked() {
                                            actions.push(PickerAction::Sleep(index));
                                            menu.close();
                                        }
                                    });
                            }
                        },
                    );
                    state.scroll_to_selection = false;
                }

                popup_ui.separator();
                if let Some(index) =
                    current.and_then(|index| chapters.get(index).map(|c| (index, c)))
                {
                    let elapsed = (position - index.1.start).max(0.);
                    let end = index
                        .1
                        .end
                        .or_else(|| chapters.get(index.0 + 1).map(|next| next.start))
                        .or((duration.is_finite() && duration > 0.).then_some(duration));
                    let remaining = end.map(|end| (end - position).max(0.));
                    let timing = if let Some(remaining) = remaining {
                        format!(
                            "Chapter time {} / {} · {} left",
                            time_text(elapsed),
                            time_text(elapsed + remaining),
                            time_text(remaining)
                        )
                    } else {
                        format!("Chapter time {} elapsed", time_text(elapsed))
                    };
                    popup_ui.add(egui::Label::new(egui::RichText::new(timing).small()).wrap());
                }
            });

        if context_open || egui::Popup::is_id_open(&ctx, popup_id) {
            ui.painter().rect_stroke(
                response.rect.shrink(0.5),
                4.,
                egui::Stroke::new(1.5, ui.visuals().hyperlink_color),
                egui::StrokeKind::Inside,
            );
        }

        for action in actions {
            match action {
                PickerAction::Step(forward) => {
                    self.step_chapter(forward);
                    let target = chapters::chapter_at(&chapters, self.position());
                    self.chapter_picker_state.selected = target
                        .filter(|index| self.chapter_picker_state.filtered_indices.contains(index));
                    self.chapter_picker_state.scroll_to_selection = true;
                }
                PickerAction::Seek(index, start) => {
                    self.actions
                        .push(Action::SeekChapter(track_id.clone(), start));
                    self.chapter_picker_state.selected = Some(index);
                    self.chapter_picker_state.open = false;
                    egui::Popup::close_id(&ctx, popup_id);
                }
                PickerAction::Bookmark(_index, start, name) => {
                    self.actions
                        .push(Action::ChapterBookmark(track_id.clone(), start, name));
                }
                PickerAction::Sleep(index) => {
                    self.chapter_picker_state.open = false;
                    egui::Popup::close_id(&ctx, popup_id);
                    self.sleep_at_chapter_end(&track_id, index);
                }
            }
        }
    }
}

enum PickerAction {
    Step(bool),
    Seek(usize, f64),
    Bookmark(usize, f64, String),
    Sleep(usize),
}
