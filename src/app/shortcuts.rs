use super::*;

impl PlayerApp {
    fn playback_is_audiobook(&self) -> bool {
        self.state.current.as_deref().and_then(|id| {
            self.gui_cache.track_at_revision(id, self.library_revision).or_else(|| self.state.track(id))
        }).is_some_and(|track| track.audiobook)
    }
    pub(super) fn skip_back_seconds(&self) -> f64 {
        self.state.settings.skip_steps(self.playback_is_audiobook()).0
    }

    pub(super) fn skip_forward_seconds(&self) -> f64 {
        self.state.settings.skip_steps(self.playback_is_audiobook()).1
    }
    /// Key bindings for the player window.
    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        // Mouse motion/animation frames have no keys to dispatch.
        if !ctx.input(|input| {
            input
                .events
                .iter()
                .any(|event| matches!(event, egui::Event::Key { pressed: true, .. }))
        }) {
            return;
        }
        // A popup owns its search and navigation keys, even if the underlying
        // library search still had focus when it opened.
        if egui::Popup::is_any_open(ctx) || self.search_keys(ctx) {
            return;
        }
        if ctx.text_edit_focused()
            || !ctx.input(|input| input.raw.focused)
            || self.confirm.is_some()
            || self.bookmark_dialog
            || self.bookmark_edit.is_some()
            || self.playlist_dialog.is_some()
            || self.station_dialog.is_some()
        {
            return;
        }
        // The File details dialog is a window of its own: it takes the playback
        // keys but not the list keys, and never moves the selection under it.
        let details_open = self.details.is_some() || self.tag_editor.is_some();
        let fired = pressed_bindings(
            ctx,
            &self.state.settings.shortcuts,
            self.list_focus,
            details_open,
        );
        for action in &fired {
            self.run_key_action(ctx, action);
        }
        if details_open {
            return;
        }
        let focused = ctx.memory(|m| m.focused());
        // The Streams list has its own rows: arrows move, Enter plays.
        if self.state.section == state::STREAMS_SECTION {
            if list_navigation_allowed(focused, self.list_focus) {
                self.station_keys(ctx);
            }
            return;
        }
        // So do the Artists, Albums and Genres lists: arrows move, Enter opens.
        if self.browse_kind().is_some() {
            if list_navigation_allowed(focused, self.list_focus) {
                self.browse_keys(ctx);
            }
            return;
        }
        let list_keys = matches!(self.state.tab.as_str(), "Files" | "Queue")
            && list_navigation_allowed(focused, self.list_focus);
        let step = if list_keys {
            ctx.input_mut(list_step)
        } else {
            None
        };
        if let Some((step, extend)) = step {
            self.sync_selection();
            let tracks = self.visible_tracks();
            if !tracks.is_empty() {
                let current = self
                    .state
                    .selected
                    .as_ref()
                    .and_then(|id| tracks.iter().position(|t| &t.id == id));
                let last = tracks.len() - 1;
                let next = match (step, current) {
                    (ListStep::Down, Some(current)) => (current + 1).min(last),
                    (ListStep::Up, Some(current)) => current.saturating_sub(1),
                    (ListStep::Home, _) => 0,
                    (ListStep::End, _) => last,
                    // Nothing selected: the first press selects the first row.
                    (_, None) => 0,
                };
                let id = tracks[next].id.clone();
                let mode = if extend && current.is_some() {
                    SelectMode::Range
                } else {
                    SelectMode::Plain
                };
                self.select_with(id, mode);
                let style = ctx.global_style();
                let body_font = egui::TextStyle::Body.resolve(&style);
                let small_font = egui::TextStyle::Small.resolve(&style);
                let minimum = ctx.fonts_mut(|fonts| {
                    fonts.row_height(&body_font) + fonts.row_height(&small_font) + 8.
                });
                // Scroll only when the selected row would leave the visible part.
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
        if list_keys && ctx.input_mut(|i| consume_command_key(i, egui::Key::A)) {
            // Ctrl+A (Cmd+A): every displayed row.
            self.sync_selection();
            let tracks = self.visible_tracks();
            self.selection
                .select_all(tracks.iter().map(|track| track.id.as_str()));
            self.state.selected = self.selection.primary().map(str::to_owned);
        }
        // Esc collapses several selected rows to the primary one; a message
        // window takes the key first.
        if list_keys
            && self.error.is_none()
            && self.selection.len() > 1
            && ctx.input_mut(|i| consume_plain_key(i, egui::Key::Escape))
        {
            let primary = self.selection.primary().map(str::to_owned);
            self.selection.clear_to(primary.as_deref());
        }
        // Esc in a drill-down goes back to its list (after it has collapsed
        // several selected rows, which takes the key first); a message window
        // takes the key before that.
        if self.error.is_none()
            && self.selection.len() <= 1
            && matches!(
                browse_section(&self.state.section),
                Some(BrowseSection::Group(..))
            )
            && ctx.input_mut(|i| consume_plain_key(i, egui::Key::Escape))
        {
            self.close_drilldown();
        }
        // A showing message takes Enter to dismiss it (see `dialogs`). Enter does
        // what a double-click on the row does.
        if list_keys
            && self.error.is_none()
            && ctx.input_mut(|i| consume_plain_key(i, egui::Key::Enter))
        {
            if let Some(id) = self.state.selected.clone() {
                let tracks = self.visible_tracks();
                if tracks.iter().any(|t| t.id == id) {
                    self.queue_displayed(&tracks);
                }
                self.load_track(&id, true, None);
            }
        }
    }

    /// Runs the playback bindings pressed in the Equalizer, Options or About
    /// window (see `pressed_bindings`, called from each window's frame), so
    /// Space pauses without clicking back to the player.
    pub(super) fn run_window_actions(&mut self, ctx: &egui::Context, actions: Vec<String>) {
        if actions.is_empty() {
            return;
        }
        for action in &actions {
            self.run_key_action(ctx, action);
        }
        // The status note and the transport live in the player window.
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }

    pub(super) fn run_key_action(&mut self, ctx: &egui::Context, action: &str) {
        match action {
            "Play / pause" => self.toggle_play(),
            "Stop" => self.stop_playback(),
            "Previous track" => self.next(true, false),
            "Next track" => self.next(false, false),
            "Seek backward" => self.seek(self.position() - self.skip_back_seconds()),
            "Seek forward" => self.seek(self.position() + self.skip_forward_seconds()),
            "Open files" => self.choose_files(false),
            "Open folder" => self.choose_files(true),
            "Volume up" => self.nudge_volume(true),
            "Volume down" => self.nudge_volume(false),
            "Mute" => self.toggle_mute(),
            "Speed faster" => self.nudge_speed(true),
            "Speed slower" => self.nudge_speed(false),
            "Reset speed" => self.set_speed(1.),
            "Add bookmark" => self.add_bookmark_from_key(),
            "Next bookmark" => self.step_bookmark(true),
            "Previous bookmark" => self.step_bookmark(false),
            "Next chapter" => self.step_chapter(true),
            "Previous chapter" => self.step_chapter(false),
            "Focus search" => self.focus_search(ctx),
            "Remove selected" => {
                // With a button or slider focused, Delete is not about the list.
                if list_navigation_allowed(ctx.memory(|m| m.focused()), self.list_focus) {
                    self.remove_selected();
                }
            }
            "Undo" => self.undo_last(),
            _ => {}
        }
    }

    /// A short note in the status bar for volume and speed changes. An import
    /// that is running owns the bar, so the note is skipped then.
    pub(super) fn set_brief_status(&mut self, text: impl Into<String>) {
        if self.busy > 0 {
            return;
        }
        self.set_status(text);
        self.status_at = state::instant_ago(STATUS_SHOWN.saturating_sub(BRIEF_STATUS_SHOWN));
    }

    pub(super) fn volume_note(&self) -> String {
        let percent = (self.state.volume.clamp(0., 1.) * 100.).round() as u32;
        if self.state.muted {
            format!("Volume {percent} % (muted)")
        } else {
            format!("Volume {percent} %")
        }
    }

    pub(super) fn nudge_volume(&mut self, up: bool) {
        self.state.volume = step_volume(self.state.volume, up);
        if up {
            self.state.muted = false;
        }
        self.apply_volume();
        let note = self.volume_note();
        self.set_brief_status(note);
    }

    pub(super) fn toggle_mute(&mut self) {
        self.state.muted = !self.state.muted;
        self.apply_volume();
        let note = if self.state.muted {
            "Muted".to_owned()
        } else {
            self.volume_note()
        };
        self.set_brief_status(note);
    }

    pub(super) fn nudge_speed(&mut self, faster: bool) {
        self.set_speed(step_speed(self.state.speed, faster));
    }

    pub(super) fn set_speed(&mut self, speed: f32) {
        // A live stream always plays at 1×; the file speed is left as it is.
        if self.state.current_station.is_some() {
            self.set_brief_status(LIVE_UNAVAILABLE);
            return;
        }
        if speed != self.state.speed {
            self.state.speed = speed;
            self.apply_speed();
        }
        self.set_brief_status(format!("Speed {:.2}x", self.state.speed));
    }

    /// Sends `state.speed` to the engine and remembers it for the current book.
    pub(super) fn apply_speed(&mut self) {
        self.audio.speed(self.state.speed);
        if self.state.settings.remember_book_speed {
            if let Some(track) = self.state.current_track() {
                if track.audiobook {
                    self.state
                        .book_speeds
                        .insert(track.book_key(), self.state.speed);
                }
            }
        }
    }
}

pub(super) fn list_navigation_allowed(focused: Option<egui::Id>, list_focus: Option<egui::Id>) -> bool {
    focused.is_none() || focused == list_focus
}

/// Whether `events` hold a first press of `key`, not an auto-repeat.
pub(super) fn fresh_key_press(events: &[egui::Event], key: egui::Key) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            egui::Event::Key { key: pressed, pressed: true, repeat: false, .. } if *pressed == key
        )
    })
}

/// Consumes a press of `key` made without Ctrl, Shift, Alt or Command.
/// `InputState::consume_key(NONE, ..)` would also take Shift+key and Alt+key,
/// which may be bound to something else.
pub(super) fn consume_plain_key(input: &mut egui::InputState, key: egui::Key) -> bool {
    let found = input.events.iter().position(|event| {
        matches!(
            event,
            egui::Event::Key { key: pressed, pressed: true, modifiers, .. }
                if *pressed == key && !modifiers.any()
        )
    });
    match found {
        Some(index) => {
            input.events.remove(index);
            true
        }
        None => false,
    }
}

/// A key that moves the selection in the file list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ListStep {
    Down,
    Up,
    Home,
    End,
}

/// Consumes one press of a list navigation key: Up and Down alone move the
/// selection to one row, with Shift they extend it; Shift+Home and Shift+End
/// extend it to the first and last row. The flag says whether Shift was held.
/// Ctrl/Alt combinations are left for the shortcut bindings.
pub(super) fn list_step(input: &mut egui::InputState) -> Option<(ListStep, bool)> {
    let found = input.events.iter().position(|event| {
        matches!(
            event,
            egui::Event::Key { key, pressed: true, modifiers, .. }
                if !modifiers.ctrl && !modifiers.alt && !modifiers.command && !modifiers.mac_cmd
                    && match key {
                        egui::Key::ArrowDown | egui::Key::ArrowUp => true,
                        egui::Key::Home | egui::Key::End => modifiers.shift,
                        _ => false,
                    }
        )
    })?;
    let egui::Event::Key { key, modifiers, .. } = input.events.remove(found) else {
        return None;
    };
    let step = match key {
        egui::Key::ArrowDown => ListStep::Down,
        egui::Key::ArrowUp => ListStep::Up,
        egui::Key::Home => ListStep::Home,
        _ => ListStep::End,
    };
    Some((step, modifiers.shift))
}

/// Consumes a press of `key` with Ctrl (Cmd on a Mac) held and no Shift or Alt.
pub(super) fn consume_command_key(input: &mut egui::InputState, key: egui::Key) -> bool {
    let found = input.events.iter().position(|event| {
        matches!(
            event,
            egui::Event::Key { key: pressed, pressed: true, modifiers, .. }
                if *pressed == key && modifiers.command && !modifiers.shift && !modifiers.alt
        )
    });
    match found {
        Some(index) => {
            input.events.remove(index);
            true
        }
        None => false,
    }
}

/// The widget id of the toolbar search box.
pub(super) fn search_box_id() -> egui::Id {
    egui::Id::new("bs-player-search-box")
}

/// The modifiers and key bound to `action`, if it has a valid binding.
pub(super) fn bound_shortcut(
    bindings: &[settings::Shortcut],
    action: &str,
) -> Option<(egui::Modifiers, egui::Key)> {
    let key = bound_key(bindings, action)?;
    settings::parse_shortcut(key).ok()
}

/// Actions that act on playback only. They are the ones that also work in the
/// Equalizer, Options and About windows and over the File details dialog.
pub(super) fn is_playback_action(action: &str) -> bool {
    matches!(
        action,
        "Play / pause"
            | "Stop"
            | "Previous track"
            | "Next track"
            | "Seek backward"
            | "Seek forward"
            | "Volume up"
            | "Volume down"
            | "Mute"
            | "Speed faster"
            | "Speed slower"
            | "Reset speed"
    )
}

/// Holding a key repeats it; these actions run once per press.
pub(super) fn is_single_shot(action: &str) -> bool {
    matches!(
        action,
        "Play / pause"
            | "Stop"
            | "Open files"
            | "Open folder"
            | "Mute"
            | "Speed faster"
            | "Speed slower"
            | "Reset speed"
            | "Add bookmark"
            | "Next bookmark"
            | "Previous bookmark"
            | "Next chapter"
            | "Previous chapter"
            | "Focus search"
            | "Remove selected"
            | "Undo"
    )
}

/// A binding with no modifier on a letter, digit, Delete or Backspace must not
/// fire when Shift or Alt is also held: egui's logical matching would accept
/// Shift+M for a plain `M`. Punctuation keys keep the logical rule because many
/// layouts type them with Shift or AltGr.
pub(super) fn exact_binding(modifiers: egui::Modifiers, key: egui::Key) -> bool {
    if modifiers.any() {
        return false;
    }
    let name = key.name();
    (name.len() == 1 && name.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        || matches!(key, egui::Key::Delete | egui::Key::Backspace)
}

/// Consumes a press of `key` that matches the binding's modifiers.
pub(super) fn consume_binding(
    input: &mut egui::InputState,
    modifiers: egui::Modifiers,
    key: egui::Key,
) -> bool {
    if exact_binding(modifiers, key) {
        consume_plain_key(input, key)
    } else {
        input.consume_key(modifiers, key)
    }
}

/// The actions whose bindings were pressed in `ctx`'s window this frame, in
/// dispatch order, with their key presses consumed. Nothing fires while a text
/// field has focus, the window is not focused or a popup is open. A focused
/// button, slider or list row keeps Space, Enter and the navigation keys.
/// `playback_only` limits it to the actions in `is_playback_action`.
pub(super) fn pressed_bindings(
    ctx: &egui::Context,
    bindings: &[settings::Shortcut],
    list_focus: Option<egui::Id>,
    playback_only: bool,
) -> Vec<String> {
    let mut fired = Vec::new();
    // Mouse motion/animation frames have no keys to dispatch. Avoid sorting
    // and parsing every shortcut on each of those frames.
    if !ctx.input(|input| {
        input
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::Key { pressed: true, .. }))
    }) {
        return fired;
    }
    if ctx.text_edit_focused()
        || !ctx.input(|input| input.raw.focused)
        || egui::Popup::is_any_open(ctx)
    {
        return fired;
    }
    let focused = ctx.memory(|m| m.focused());
    // User bindings come first: a binding such as Enter or Shift+ArrowDown
    // must not be swallowed by list navigation.
    for index in settings::shortcut_dispatch_order(bindings) {
        let shortcut = &bindings[index];
        if playback_only && !is_playback_action(&shortcut.action) {
            continue;
        }
        let Ok((modifiers, key)) = settings::parse_shortcut(&shortcut.key) else {
            continue;
        };
        if focused_widget_owns_key(focused, list_focus, modifiers, key) {
            continue;
        }
        let fresh = !is_single_shot(&shortcut.action)
            || ctx.input(|input| fresh_key_press(&input.events, key));
        let pressed = ctx.input_mut(|input| consume_binding(input, modifiers, key));
        if pressed && fresh {
            fired.push(shortcut.action.clone());
        }
    }
    fired
}

/// The speeds the speed menu offers, slowest first.
pub(super) const SPEED_STEPS: [f32; 10] = [0.5, 0.75, 1., 1.1, 1.25, 1.5, 1.75, 2., 2.5, 3.];

/// The next speed in `SPEED_STEPS` above or below `current`. At either end, or
/// for a speed beyond the list, the speed stays as it is.
pub(super) fn step_speed(current: f32, faster: bool) -> f32 {
    const EPSILON: f32 = 1e-3;
    if !current.is_finite() {
        return 1.;
    }
    let next = if faster {
        SPEED_STEPS.iter().copied().find(|s| *s > current + EPSILON)
    } else {
        SPEED_STEPS
            .iter()
            .rev()
            .copied()
            .find(|s| *s < current - EPSILON)
    };
    next.unwrap_or(current)
}

/// Volume moves in 5 % steps; a value between two steps goes to the next one
/// in the direction of the key. Always within 0 to 1.
pub(super) fn step_volume(volume: f32, up: bool) -> f32 {
    const STEPS: f32 = 20.;
    const EPSILON: f32 = 1e-3;
    let units = if volume.is_finite() {
        volume.clamp(0., 1.) * STEPS
    } else {
        STEPS
    };
    let stepped = if up {
        (units + EPSILON).floor() + 1.
    } else {
        (units - EPSILON).ceil() - 1.
    };
    stepped.clamp(0., STEPS) / STEPS
}

pub(super) fn focused_widget_owns_key(
    focused: Option<egui::Id>,
    list_focus: Option<egui::Id>,
    modifiers: egui::Modifiers,
    key: egui::Key,
) -> bool {
    focused.is_some()
        && focused != list_focus
        && !modifiers.ctrl
        && !modifiers.alt
        && !modifiers.mac_cmd
        && !modifiers.command
        && matches!(
            key,
            egui::Key::Space
                | egui::Key::Enter
                | egui::Key::ArrowLeft
                | egui::Key::ArrowRight
                | egui::Key::ArrowUp
                | egui::Key::ArrowDown
                | egui::Key::Home
                | egui::Key::End
                | egui::Key::PageUp
                | egui::Key::PageDown
        )
}

pub(super) fn in_player_window<'a>(
    ctx: &egui::Context,
    title: &'a str,
    preferred_width: f32,
) -> egui::Window<'a> {
    let bounds = ctx.content_rect().shrink(8.);
    let size = bounds.size().max(Vec2::splat(32.));
    egui::Window::new(title)
        .default_width(preferred_width.min(size.x))
        .min_size([size.x.min(200.), size.y.min(80.)])
        .max_size(size)
        .constrain_to(bounds)
        .scroll([true, true])
}
