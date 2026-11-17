use super::*;

impl PlayerApp {
    pub(super) fn visible_tracks(&mut self) -> Arc<VisibleView> {
        self.gui_cache.view(
            &self.state,
            self.library_revision,
            self.view_revision,
            self.progress_revision,
        )
    }

    pub(super) fn content(&mut self, root: &mut egui::Ui) {
        self.list_focus = None;
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(appearance::workspace(root)).inner_margin(egui::Margin::symmetric(16,10))).show(root,|ui|{
  if self.state.compact{let station=self.current_station_label().map(|label|(label,self.station_line().0));ui.vertical_centered(|ui|{ui.add_space(20.);if let Some((label,line))=&station{ui.heading(label.as_str());ui.label(line.as_str());}else{ui.heading(self.state.current_track().map(|t|t.display_title()).unwrap_or("BS Player"));ui.label(self.state.current_track().map(|t|t.artist.as_str()).unwrap_or("Add files using the File menu"));}});return;}
  // The Streams section lists stations instead of files: no tabs, no file table.
  if self.state.section==state::STREAMS_SECTION{self.streams_ui(ui);return;}
  // The Artists, Albums and Genres lists show groups of files; their drill-downs are ordinary file lists.
  if let Some(kind)=self.browse_kind(){self.browse_ui(ui,kind);return;}
  self.leave_empty_chapters_tab();
  self.leave_empty_lyrics_tab();
  if self.state.section==state::PLAYING_SECTION&&self.state.tab=="Files"{self.state.tab="Queue".into();}
  let tab_names=self.workspace_tabs();
  self.sync_selection();let tracks=self.visible_tracks();let total=tracks.total_duration;let book_note=self.book_header_note();
  let title=&tracks.title;
  let very_short=ui.available_height()<220.;
  let (tabs_width, tools_width) = file_header_widths(ui, tab_names, &self.state.tab, self.state.sort_column, self.state.sort_descending);
  let compact_header=ui.available_height()<300. || ui.available_width()<tabs_width+tools_width+2.*ui.spacing().item_spacing.x+8.;
  ui.add_space(if compact_header{0.}else{5.});
  let heading_size=self.state.settings.font_size+if compact_header{4.}else{14.};
  let drill_kind=match browse_section(&self.state.section){Some(BrowseSection::Group(kind,_))=>Some(kind),_=>None};
  if let Some(kind)=drill_kind{self.drilldown_header(ui,kind,title.as_str(),heading_size);}else{ui.add(egui::Label::new(RichText::new(title.as_str()).font(appearance::semibold(heading_size))).truncate()).on_hover_text(title.as_str());}
  if !very_short{ui.label(RichText::new(format!("{} files · {}",tracks.len(),book_note.unwrap_or_else(||duration_summary(total)))).small().color(ui.visuals().weak_text_color()));}
  if self.state.section==state::PLAYING_SECTION{self.playing_tools(ui);}
  let normal_spacing=ui.spacing().item_spacing.y;ui.spacing_mut().item_spacing.y=0.;
  let tabs=ui.horizontal(|ui|{
    if compact_header {
      let selector_width = combo_width(ui, &self.state.tab, 110.);
      if selector_width+32.+2.*ui.spacing().item_spacing.x <= ui.available_width() {
        egui::ComboBox::from_id_salt("compact list view").selected_text(&self.state.tab).width(110.).show_ui(ui,|ui|{for tab in tab_names.iter().copied(){if ui.selectable_value(&mut self.state.tab,tab.into(),tab).changed(){self.state.scroll=0.;self.restore_scroll=true;self.tab_chosen(tab);}}});
      } else {
        // The list menu below also contains every view. At large zoom, reserve
        // that action first and keep the current view readable in the remainder.
        let label_width = (ui.available_width()-32.-2.*ui.spacing().item_spacing.x).max(1.);
        ui.add_sized([label_width, ui.text_style_height(&egui::TextStyle::Body)+12.], egui::Label::new(&self.state.tab).truncate())
          .on_hover_text(format!("{} — change view using List options and views", self.state.tab));
      }
    }
    else{for tab in tab_names.iter().copied(){if widgets::tab(ui,tab,self.state.tab==tab).clicked(){self.state.tab=tab.into();self.state.scroll=0.;self.restore_scroll=true;self.tab_chosen(tab);}}}
    let tools_width=if compact_header{32.}else{tools_width};ui.add_space((ui.available_width()-tools_width-ui.spacing().item_spacing.x).max(0.));
    self.list_tools(ui,&tracks,compact_header);
  });
  ui.painter().line_segment([tabs.response.rect.left_bottom(),tabs.response.rect.right_bottom()],egui::Stroke::new(1.,appearance::separator(ui)));
  if self.state.tab=="Bookmarks"{ui.spacing_mut().item_spacing.y=normal_spacing;self.bookmarks_ui(ui);return;}
  if self.state.tab==CHAPTERS_TAB{ui.spacing_mut().item_spacing.y=normal_spacing;self.chapters_ui(ui);return;}
  if self.state.tab==LYRICS_TAB{ui.spacing_mut().item_spacing.y=normal_spacing;self.lyrics_ui(ui);return;}
  if tracks.is_empty(){ui.spacing_mut().item_spacing.y=normal_spacing;let shown=self.state.settings.column_mask();let widths=file_column_widths(&self.state.column_widths,&shown,ui.available_width());let header_height=(ui.text_style_height(&egui::TextStyle::Body)+12.).max(28.);egui::ScrollArea::horizontal().id_salt("table horizontal").show(ui,|ui|{self.file_table_header(ui,&widths,&shown,header_height);});ui.add_space(35.);ui.vertical_centered(|ui|{ui.heading(if self.state.section==state::PLAYING_SECTION{"Drop audio files here"}else if self.state.library.is_empty(){"Your library starts here"}else{"No matching files"});ui.label(if self.state.section==state::PLAYING_SECTION{"Drop files or choose Add files to start your Playing playlist. Save it when you want to keep a named copy."}else if self.state.library.is_empty(){"Add your music or audiobook files. Listening positions are saved for every file."}else{"Choose another view or clear the search field."});ui.add_space(12.);if widgets::text_icon_button(ui,Icon::Plus,"Add files").clicked(){self.choose_files(false);}if widgets::text_icon_button(ui,Icon::Folder,"Add folder").clicked(){self.choose_files(true);}});return;}
  let shown=self.state.settings.column_mask();
  let widths=file_column_widths(&self.state.column_widths,&shown,ui.available_width());
  let total_width: f32=(0..COLUMN_COUNT).filter(|i|shown[*i]).map(|i|widths[i]).sum();
  let details_text_height = ui.text_style_height(&egui::TextStyle::Body)+ui.text_style_height(&egui::TextStyle::Small)+normal_spacing;
  let details_height = details_text_height+2.+2.*normal_spacing;
  let show_details=ui.available_height()>220. && ui.available_height()>details_height+100.;
  let header_height=(ui.text_style_height(&egui::TextStyle::Body)+12.).max(28.);let row_height=self.state.settings.row_height.max(ui.text_style_height(&egui::TextStyle::Body)+ui.text_style_height(&egui::TextStyle::Small)+8.);
  egui::ScrollArea::horizontal().id_salt("table horizontal").show(ui,|ui|{ui.set_min_width(total_width);
   let resize_guide=self.file_table_header(ui,&widths,&shown,header_height);
   ui.add_space(if compact_header{2.}else{8.});let mut scroll=egui::ScrollArea::vertical().id_salt(("file rows",&self.state.tab)).auto_shrink([false,false]).max_height((ui.available_height()-if show_details{details_height}else{0.}).max(1.));if self.restore_scroll{scroll=scroll.vertical_scroll_offset(self.state.scroll);self.restore_scroll=false;}
   let previous_row_spacing=ui.spacing().item_spacing.y;ui.spacing_mut().item_spacing.y=2.;
   if self.scroll_to_current {if let Some(index)=tracks.iter().position(|t|Some(&t.id)==self.state.current.as_ref()) {scroll=scroll.vertical_scroll_offset(index as f32*(row_height+ui.spacing().item_spacing.y));self.scroll_to_current=false;}}
   // Row thumbnails: a square inside the Title cell, as tall as the row minus 3 px above and below.
   let thumb_side=if self.state.settings.show_list_art{(row_height-6.).max(0.)}else{0.};
   if thumb_side==0.&&!self.thumbs.cache.is_empty(){self.thumbs.reset();}
   let now=Instant::now();let now_secs=state::now();let mut wanted:Vec<(ThumbKey,ThumbJob)>=Vec::new();
   // A playlist in its own order can be reordered by dragging; any playlist has "Remove from this playlist".
   let in_playlist=playlist_section(&self.state).map(str::to_owned);let reorder=if crate::gui_cache::playlist_order_active(&self.state){in_playlist.clone()}else{None};
   let several=self.selection.len()>1;let output=scroll.show_rows(ui,row_height,tracks.len(),|ui,range|{for index in range{let track=&tracks[index];let primary=self.state.selected.as_deref()==Some(&track.id);let selected=primary||self.selection.contains(&track.id);let current=self.state.current.as_deref()==Some(&track.id);let (rect,response)=ui.allocate_exact_size(Vec2::new(total_width,row_height),egui::Sense::click_and_drag());let bg=appearance::row_fill(ui,selected,index%2==1);ui.painter().rect_filled(rect,5.,bg);
    // With several rows selected the primary row keeps an outline.
    if several&&primary{ui.painter().rect_stroke(rect.shrink(1.),5.,egui::Stroke::new(1.5,appearance::row_primary_outline(ui)),egui::StrokeKind::Inside);}let text=appearance::row_text(ui,selected);
    // Cell text is built only for the columns that are shown, and only for the rows in view.
    let progress=if shown[columns::COL_PROGRESS]||shown[columns::COL_PLAYS]||shown[columns::COL_LAST_PLAYED]{self.state.progress.get(&track.id).cloned()}else{None};
    let input=columns::CellInput{track,row:index,progress:progress.as_ref(),rating:if shown[columns::COL_RATING]{self.state.ratings.get(&track.id).copied().unwrap_or(0)}else{0},favorite:shown[columns::COL_FAVORITE]&&tracks.is_favorite(&track.id),now:now_secs,live_position:if current&&shown[columns::COL_PROGRESS]{Some(self.position())}else{None}};let mut x=rect.left();
    for i in 0..COLUMN_COUNT{if !shown[i]{continue;}let mut cell=egui::Rect::from_min_size(egui::pos2(x+14.,rect.top()),Vec2::new((widths[i]-if COLUMNS[i].align==columns::Align::Right{28.}else{18.}).max(1.),row_height));
    if i==columns::COL_TITLE&&thumb_side>0.&&cell.width()>=thumb_side+THUMB_GAP+THUMB_MIN_TEXT{self.row_thumbnail(ui,egui::Rect::from_min_size(egui::pos2(cell.left(),rect.top()+3.),Vec2::splat(thumb_side)),track,now,&mut wanted);cell.min.x+=thumb_side+THUMB_GAP;}
    {let mut cell_ui=ui.new_child(egui::UiBuilder::new().max_rect(cell).layout(egui::Layout::top_down(if COLUMNS[i].align==columns::Align::Right{egui::Align::Max}else{egui::Align::Min})));cell_ui.style_mut().interaction.selectable_labels=false;let ui=&mut cell_ui;ui.set_clip_rect(cell.intersect(ui.clip_rect()));ui.spacing_mut().item_spacing.y=2.;ui.add_space(3.);if i==columns::COL_INDEX&&current&&self.snapshot.playing{widgets::icon(ui.painter(),egui::Rect::from_center_size(cell.center(),Vec2::splat(16.)),Icon::Pause,text);}else if i==columns::COL_TITLE{ui.add(egui::Label::new(RichText::new((COLUMNS[i].cell)(&input).into_owned()).color(text)).truncate());ui.add(egui::Label::new(RichText::new(track.path.file_name().unwrap_or_default().to_string_lossy()).small().color(if selected{text}else{ui.visuals().weak_text_color()})).truncate());}else{ui.add_space((row_height-28.)/2.);ui.add(egui::Label::new(RichText::new((COLUMNS[i].cell)(&input).into_owned()).color(text)).truncate());}}x+=widths[i];}
    if self.refocus_list&&primary{response.request_focus();self.refocus_list=false;}if response.has_focus(){self.list_focus=Some(response.id);}if response.clicked(){response.request_focus();self.list_focus=Some(response.id);let mode=select_mode(ui.input(|i|i.modifiers));self.actions.push(if mode==SelectMode::Plain{Action::Select(track.id.clone())}else{Action::SelectWith(track.id.clone(),mode)});}
    // A right-click on a row outside the selection selects it; inside, the selection stays and the row becomes the primary one.
    if response.secondary_clicked(){if !selected{self.actions.push(Action::Select(track.id.clone()));}else if several&&!primary{self.actions.push(Action::FocusInSelection(track.id.clone()));}}if response.double_clicked()&&self.state.settings.double_click_plays{self.queue_displayed(&tracks);self.actions.push(Action::Play(track.id.clone()));}
    if response.drag_started(){response.dnd_set_drag_payload(track.id.clone());}
    if self.state.tab=="Queue"||reorder.is_some(){if let Some(from)=response.dnd_release_payload::<String>(){self.actions.push(match &reorder{Some(name)=>Action::MovePlaylist(name.clone(),(*from).clone(),track.id.clone()),None=>Action::MoveQueue((*from).clone(),track.id.clone())});}}
    response.on_hover_text(format!("{}\n{}\n{}",track.display_title(),track.artist,media::display_path(&track.path))).context_menu(|ui|self.row_menu(ui,track,in_playlist.as_deref(),reorder.is_some()));
   }});self.state.scroll=output.state.offset.y;self.list_viewport=output.inner_rect.height();ui.spacing_mut().item_spacing.y=previous_row_spacing;
   self.thumbs.submit(wanted,&self.tx,&self.gui_context);
   if let Some(x)=resize_guide {ui.painter().vline(x,output.inner_rect.y_range(),egui::Stroke::new(1.,ui.visuals().hyperlink_color));}

  });
  ui.spacing_mut().item_spacing.y=normal_spacing;
  if show_details {
    let catalog = self.gui_cache.catalog(&self.state.library, self.library_revision);
    ui.add(egui::Separator::default().spacing(2.));
    let button_width = label_width(ui, "File details…", egui::TextStyle::Button)+2.*ui.spacing().button_padding.x;
    let text_width = (ui.available_width()-button_width-ui.spacing().item_spacing.x).max(1.);
    ui.horizontal(|ui| {
      ui.allocate_ui_with_layout(Vec2::new(text_width, details_text_height), egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_width(text_width);
        if let Some(track)=self.state.selected.as_deref().and_then(|id|catalog.track(id)) {
          ui.add(egui::Label::new(track.display_title()).truncate()).on_hover_text(track.display_title());
          let metadata=format!("{} · {} kbps · {} Hz · {} channels",track.path.extension().unwrap_or_default().to_string_lossy().to_uppercase(),track.bitrate,track.sample_rate,track.channels);
          ui.add(egui::Label::new(RichText::new(&metadata).small().color(ui.visuals().weak_text_color())).truncate()).on_hover_text(metadata);
        }
      });
      if ui.add_enabled(self.state.selected.is_some(), egui::Button::new("File details…")).clicked() {
        if let Some(id)=self.state.selected.clone(){self.actions.push(Action::Details(id));}
      }
    });
  }
 });
    }

    pub(super) fn file_table_header(
        &mut self,
        ui: &mut egui::Ui,
        widths: &[f32],
        shown: &[bool; COLUMN_COUNT],
        height: f32,
    ) -> Option<f32> {
        // One stable context menu belongs to the whole header strip, including
        // separators and spare space. Its ID survives hiding the clicked column.
        let origin = ui.available_rect_before_wrap().min;
        let end = origin.x
            + widths
                .iter()
                .zip(shown)
                .filter(|(_, show)| **show)
                .map(|(width, _)| *width)
                .sum::<f32>();
        let bar = egui::Rect::from_min_max(
            origin,
            egui::pos2(end.max(ui.clip_rect().right()), origin.y + height),
        )
        .intersect(ui.clip_rect());
        let over_bar = ui.rect_contains_pointer(bar);
        let open_menu = ui.input(|input| {
            if over_bar && input.pointer.button_clicked(egui::PointerButton::Secondary) {
                Some(egui::SetOpenCommand::Bool(true))
            } else if over_bar && input.pointer.button_clicked(egui::PointerButton::Primary) {
                Some(egui::SetOpenCommand::Bool(false))
            } else {
                None
            }
        });
        let menu_anchor = ui.interact(
            bar,
            ui.id().with("header-column-menu"),
            egui::Sense::hover(),
        );
        let mut headers = Vec::new();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.;
            for (index, column) in COLUMNS.iter().enumerate() {
                if !shown[index] {
                    continue;
                }
                let sortable = column.sortable && self.state.tab != "Queue";
                let response = column_header(
                    ui,
                    column.label,
                    widths[index],
                    height,
                    sortable && self.state.sort_column == index,
                    self.state.sort_descending,
                    sortable,
                    column.align,
                );
                // The separator owns the five pixels on each side of a boundary.
                let on_edge = response.interact_pointer_pos().is_some_and(|point| {
                    point.x <= response.rect.left() + 5. || point.x >= response.rect.right() - 5.
                });
                if sortable && response.clicked() && !on_edge {
                    if self.state.sort_column == index {
                        self.state.sort_descending = !self.state.sort_descending;
                    } else {
                        self.state.sort_column = index;
                        self.state.sort_descending = false;
                    }
                }
                headers.push((index, response.rect));
            }
        });
        let mut guide = None;
        // Register handles after the sort buttons so overlapping edges belong
        // to the resize interaction, with stable IDs even when columns hide.
        for (index, rect) in headers {
            let x = rect.right();
            let hit_rect = egui::Rect::from_center_size(
                egui::pos2(x, rect.center().y),
                Vec2::new(10., height),
            );
            let divider = ui.interact(
                hit_rect,
                ui.id().with(("column-divider", index)),
                egui::Sense::click_and_drag(),
            );
            if divider.dragged_by(egui::PointerButton::Primary) {
                let delta = if divider.drag_started() {
                    divider.total_drag_delta().unwrap_or_default().x
                } else {
                    divider.drag_delta().x
                };
                self.state.column_widths = widths.to_vec();
                self.state.column_widths[index] =
                    (widths[index] + delta).clamp(COLUMNS[index].min_width, 900.);
            }
            if divider.drag_stopped_by(egui::PointerButton::Primary) {
                self.save();
            }
            let active = divider.hovered() || divider.dragged();
            ui.painter().vline(
                x,
                (rect.top() + 5.)..=(rect.bottom() - 5.),
                egui::Stroke::new(
                    if active { 2. } else { 1. },
                    if active {
                        ui.visuals().hyperlink_color
                    } else {
                        appearance::separator(ui)
                    },
                ),
            );
            if active {
                guide = Some(x);
            }
            divider
                .on_hover_and_drag_cursor(egui::CursorIcon::ResizeHorizontal)
                .on_hover_text("Drag to resize column");
        }
        egui::Popup::context_menu(&menu_anchor)
            .open_memory(open_menu)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                let before = self.state.settings.column_mask();
                column_choices(ui, &mut self.state.settings);
                if before != self.state.settings.column_mask() {
                    self.save();
                }
            });
        guide
    }

    pub(super) fn list_tools(&mut self, ui: &mut egui::Ui, tracks: &VisibleView, compact: bool) {
        if !compact {
            ui.add_enabled_ui(self.state.tab != "Queue", |ui| {
                egui::ComboBox::from_id_salt("table sort")
                    .selected_text(format!(
                        "{} ({})",
                        COLUMNS[self.state.sort_column.min(COLUMN_COUNT - 1)].short_label,
                        if self.state.sort_descending {
                            "Descending"
                        } else {
                            "Ascending"
                        }
                    ))
                    .width(150.)
                    .show_ui(ui, |ui| {
                        for (i, column) in COLUMNS.iter().enumerate() {
                            if column.sortable {
                                ui.selectable_value(
                                    &mut self.state.sort_column,
                                    i,
                                    column.sort_label,
                                );
                            }
                        }
                        ui.separator();
                        ui.selectable_value(&mut self.state.sort_descending, false, "Ascending");
                        ui.selectable_value(&mut self.state.sort_descending, true, "Descending");
                    });
            });
        }
        let response = widgets::icon_button(
            ui,
            Icon::Playlist,
            "List options and views",
            false,
            false,
            32.,
        );
        ui.painter().rect_stroke(
            response.rect,
            4.,
            egui::Stroke::new(1., appearance::separator(ui)),
            egui::StrokeKind::Inside,
        );
        egui::Popup::menu(&response).show(|ui| {
            for tab in self.workspace_tabs().iter().copied() {
                if ui
                    .selectable_value(&mut self.state.tab, tab.into(), tab)
                    .changed()
                {
                    self.state.scroll = 0.;
                    self.restore_scroll = true;
                    self.tab_chosen(tab);
                    ui.close();
                }
            }
            ui.separator();
            if compact {
                ui.add_enabled_ui(self.state.tab != "Queue", |ui| {
                    ui.menu_button("Sort", |ui| {
                        for (i, column) in COLUMNS.iter().enumerate() {
                            if column.sortable {
                                ui.selectable_value(
                                    &mut self.state.sort_column,
                                    i,
                                    column.sort_label,
                                );
                            }
                        }
                        ui.checkbox(&mut self.state.sort_descending, "Descending");
                    });
                });
            }
            if self.state.tab == "Files" && ui.add_enabled(!tracks.is_empty(), egui::Button::new("Play displayed files"))
                .on_disabled_hover_text("No files match this view").clicked() {
                self.state.queue = tracks.iter().map(|t| t.id.clone()).collect();
                self.view_revision = self.view_revision.wrapping_add(1);
                if let Some(t) = tracks.first() {
                    self.actions.push(Action::Play(t.id.clone()));
                }
                ui.close();
            }
            // A book's own state: every file at once.
            if let Some(key) = self.state.section.strip_prefix("Book:").map(str::to_owned) {
                ui.separator();
                if ui.button("Mark book finished").clicked() {
                    self.mark_book_finished(&key);
                    ui.close();
                }
                if ui.button("Mark book not started…").clicked() {
                    let label = tracks.title.clone();
                    self.confirm = Some(Confirm::BookNotStarted { key, label });
                    ui.close();
                }
            }
        });
    }

    /// Ctrl+F: focus the search box and select its text.
    pub(super) fn focus_search(&mut self, ctx: &egui::Context) {
        if self.state.compact {
            self.set_brief_status("The search box is hidden in the compact player");
            return;
        }
        let id = search_box_id();
        ctx.memory_mut(|memory| memory.request_focus(id));
        let length = self.state.search.chars().count();
        let mut edit = egui::text_edit::TextEditState::load(ctx, id).unwrap_or_default();
        edit.cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(length),
            )));
        edit.store(ctx, id);
    }

    /// Keys that only matter while the search box has focus: Esc clears it and
    /// gives the focus back to the list, and its own shortcut selects the text
    /// again. Returns true when the key press was used.
    pub(super) fn search_keys(&mut self, ctx: &egui::Context) -> bool {
        let id = search_box_id();
        let (focused, had_focus) = ctx.memory(|m| (m.has_focus(id), m.had_focus_last_frame(id)));
        if !focused && !had_focus {
            return false;
        }
        // egui drops the focus on Esc before the frame runs, so the box counts
        // as focused if it was focused one frame ago.
        if ctx.input_mut(|i| consume_plain_key(i, egui::Key::Escape)) {
            self.state.search.clear();
            self.refocus_list = true;
            ctx.memory_mut(|m| m.surrender_focus(id));
            return true;
        }
        if focused {
            if let Some((modifiers, key)) =
                bound_shortcut(&self.state.settings.shortcuts, "Focus search")
            {
                if ctx.input_mut(|i| consume_binding(i, modifiers, key)) {
                    self.focus_search(ctx);
                    return true;
                }
            }
        }
        false
    }
}

/// The scroll offset that brings a row into view with the least movement, or
/// `None` when it is already fully visible (or the list is not measured yet).
pub(super) fn scroll_to_reveal(
    offset: f32,
    viewport: f32,
    row_top: f32,
    pitch: f32,
) -> Option<f32> {
    if !(viewport > 0.) {
        None
    } else if row_top < offset {
        Some(row_top)
    } else if row_top + pitch > offset + viewport {
        Some(row_top + pitch - viewport)
    } else {
        None
    }
}

/// The tabs of the file list: Chapters is offered only when the playing file has
/// chapters, and Lyrics (last) only when it has lyrics.
pub(super) fn list_tabs(has_chapters: bool, has_lyrics: bool) -> &'static [&'static str] {
    match (has_chapters, has_lyrics) {
        (false, false) => &["Files", "Queue", "Bookmarks"],
        (true, false) => &["Files", "Queue", "Bookmarks", CHAPTERS_TAB],
        (false, true) => &["Files", "Queue", "Bookmarks", LYRICS_TAB],
        (true, true) => &["Files", "Queue", "Bookmarks", CHAPTERS_TAB, LYRICS_TAB],
    }
}

/// A real header context menu: every choice is rendered, with no scrolling or
/// submenus. Short windows use balanced columns; only a very tight large-text
/// viewport needs a smaller menu font to keep the entire set reachable.
fn column_choices(ui: &mut egui::Ui, settings: &mut Settings) {
    let available = (ui.ctx().content_rect().size() - Vec2::splat(24.)).max(Vec2::splat(1.));
    let mut font = egui::TextStyle::Body.resolve(ui.style());
    let (count, rows, width, height) = loop {
        let text_height = ui.fonts_mut(|fonts| fonts.row_height(&font));
        let row_height = if font.size <= 12. {
            (text_height.max(14.) + 2.).max(16.)
        } else {
            (text_height.max(16.) + 4.).max(20.)
        };
        let capacity = ((available.y - text_height - 12.) / (row_height + 2.))
            .floor()
            .max(1.) as usize;
        let count = COLUMN_COUNT.div_ceil(capacity).clamp(1, COLUMN_COUNT);
        let width = COLUMNS
            .iter()
            .map(|column| {
                ui.painter()
                    .layout_no_wrap(column.label.into(), font.clone(), ui.visuals().text_color())
                    .size()
                    .x
            })
            .fold(0., f32::max)
            + if font.size <= 12. { 32. } else { 36. };
        if width * count as f32 + 12. * (count - 1) as f32 <= available.x || font.size <= 10. {
            break (count, COLUMN_COUNT.div_ceil(count), width, row_height);
        }
        font.size = (font.size - 1.).max(10.);
    };
    let compact = font.size <= 12.;
    {
        let style = ui.style_mut();
        style
            .text_styles
            .insert(egui::TextStyle::Body, font.clone());
        style.text_styles.insert(egui::TextStyle::Button, font);
        style.spacing.item_spacing = Vec2::new(12., 2.);
        style.spacing.button_padding = if compact { Vec2::new(4., 1.) } else { Vec2::new(6., 2.) };
        style.spacing.interact_size.y = height;
        style.spacing.icon_width = if compact { 14. } else { 16. };
        style.spacing.icon_width_inner = if compact { 10. } else { 12. };
        style.spacing.icon_spacing = if compact { 4. } else { 6. };
    }
    ui.set_width(width * count as f32 + 12. * (count - 1) as f32);
    ui.label(RichText::new("Columns").strong());
    ui.separator();
    ui.columns(count, |columns| {
        for (column, ui) in columns.iter_mut().enumerate() {
            for index in (column * rows)..((column + 1) * rows).min(COLUMN_COUNT) {
                visible_column_checkbox(ui, settings, index, COLUMNS[index].label);
            }
        }
    });
}
