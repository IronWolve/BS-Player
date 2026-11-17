//! Platform tray backends communicate with the player UI through events.

use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender, TryRecvError},
};

#[cfg(not(target_os = "macos"))]
use std::{
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use eframe::egui;

#[derive(Clone, Debug, PartialEq)]
pub enum TrayEvent {
    Restore,
    /// Host-provided screen anchor. Windows reports physical pixels; Linux
    /// converts SNI host pixels to Egui points (see `anchor_in_points`);
    /// macOS converts AppKit points to Egui's top-left point space.
    OpenMenu {
        position: [f32; 2],
        /// Monitor work area in the same units as `position`, when available.
        work_area: Option<[f32; 4]>,
    },
    AvailabilityChanged {
        available: bool,
        reason: String,
    },
}

pub struct TrayHandle {
    #[cfg(not(target_os = "macos"))]
    command_tx: Sender<TrayCommand>,
    event_rx: Receiver<TrayEvent>,
    #[cfg(not(target_os = "macos"))]
    wake: Box<dyn Fn() + Send + Sync>,
    #[cfg(not(target_os = "macos"))]
    worker: Option<JoinHandle<()>>,
    last_tooltip: Mutex<Option<String>>,
    disconnect_reported: AtomicBool,
    availability_reported: AtomicBool,
    last_available: AtomicBool,
    shutdown_requested: bool,
    #[cfg(target_os = "macos")]
    macos: Option<tray_macos::MacTray>,
}

#[cfg(not(target_os = "macos"))]
#[derive(Debug)]
pub(crate) enum TrayCommand {
    UpdateTooltip(String),
    Shutdown,
}

impl TrayHandle {
    pub fn new(ctx: egui::Context) -> Self {
        #[cfg(not(target_os = "macos"))]
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        #[cfg(not(target_os = "macos"))]
        let (worker, wake) = spawn_backend(ctx, event_tx, command_rx);
        #[cfg(target_os = "macos")]
        let macos = Some(tray_macos::MacTray::new(ctx, event_tx.clone()));
        Self {
            #[cfg(not(target_os = "macos"))]
            command_tx,
            event_rx,
            #[cfg(not(target_os = "macos"))]
            wake,
            #[cfg(not(target_os = "macos"))]
            worker,
            last_tooltip: Mutex::new(None),
            disconnect_reported: AtomicBool::new(false),
            availability_reported: AtomicBool::new(false),
            last_available: AtomicBool::new(false),
            shutdown_requested: false,
            #[cfg(target_os = "macos")]
            macos,
        }
    }

    pub fn poll_events(&self) -> Vec<TrayEvent> {
        let mut events = Vec::new();
        loop {
            match self.event_rx.try_recv() {
                Ok(event) => {
                    if let TrayEvent::AvailabilityChanged { available, .. } = &event {
                        self.availability_reported.store(true, Ordering::Release);
                        self.last_available.store(*available, Ordering::Release);
                    }
                    events.push(event);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    let was_available = self.last_available.swap(false, Ordering::AcqRel);
                    let was_reported = self.availability_reported.load(Ordering::Acquire);
                    if !self.disconnect_reported.swap(true, Ordering::AcqRel)
                        && (was_available || !was_reported)
                    {
                        events.push(TrayEvent::AvailabilityChanged {
                            available: false,
                            reason: "Tray backend worker stopped.".into(),
                        });
                    }
                    break;
                }
            }
        }
        events
    }

    pub fn update_tooltip(&self, text: &str) {
        if let Ok(mut previous) = self.last_tooltip.lock() {
            if previous.as_deref() == Some(text) {
                return;
            }
            *previous = Some(text.to_owned());
        }
        #[cfg(target_os = "macos")]
        if let Some(macos) = &self.macos {
            macos.update_tooltip(text);
            return;
        }
        #[cfg(not(target_os = "macos"))]
        if self
            .command_tx
            .send(TrayCommand::UpdateTooltip(text.to_owned()))
            .is_ok()
        {
            (self.wake)();
        }
    }

    /// Begin teardown without waiting on the GUI thread. Keep this handle alive
    /// until poll_shutdown returns true before creating a replacement backend.
    pub fn request_shutdown(&mut self) {
        if self.shutdown_requested {
            return;
        }
        self.shutdown_requested = true;
        #[cfg(target_os = "macos")]
        if let Some(mut macos) = self.macos.take() {
            macos.shutdown();
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = self.command_tx.send(TrayCommand::Shutdown);
            (self.wake)();
        }
    }

    pub fn poll_shutdown(&mut self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.macos.is_none()
        }
        #[cfg(not(target_os = "macos"))]
        {
            if self
                .worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
            {
                return false;
            }
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            true
        }
    }

    pub fn shutdown(&mut self) {
        self.request_shutdown();
        #[cfg(not(target_os = "macos"))]
        if let Some(worker) = self.worker.take() {
            // A broken session bus may stall authentication before it can read
            // Shutdown. Never let that block saved-state completion or exit.
            let deadline = Instant::now() + Duration::from_millis(2500);
            while !worker.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if worker.is_finished() {
                let _ = worker.join();
            }
            // Dropping an unfinished handle detaches it; the queued Shutdown
            // still removes the icon if the backend eventually recovers.
        }
    }
}

impl Drop for TrayHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}



pub(crate) fn emit(ctx: &egui::Context, tx: &Sender<TrayEvent>, event: TrayEvent) {
    if tx.send(event).is_ok() {
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }
}

/// Anchor for a host that gave none (or a meaningless (0, 0)): the bottom-right
/// corner of the work area, where the notification area normally is. Without a
/// work area, a position that keeps a popup of `popup` size on screen. Units
/// are those of `work_area` and `popup`.
#[cfg(any(target_os = "windows", target_os = "linux"))]
pub(crate) fn fallback_anchor(
    work_area: Option<[f32; 4]>,
    popup: [f32; 2],
) -> ([f32; 2], Option<[f32; 4]>) {
    match work_area {
        Some(area) => ([area[2], area[3]], Some(area)),
        None => (popup, None),
    }
}

/// Turn a host-reported anchor in physical pixels into Egui points, the unit
/// the winit position command multiplies back by zoom and display scale.
/// `screen` is the size in points of the monitor the player is on; that
/// monitor's origin is not reported, so it becomes the work area only when the
/// anchor lies inside it. A (0, 0) or non-finite anchor means "unknown".
#[cfg(target_os = "linux")]
pub(crate) fn anchor_in_points(
    raw: [f32; 2],
    pixels_per_point: f32,
    screen: Option<[f32; 2]>,
    popup: [f32; 2],
) -> ([f32; 2], Option<[f32; 4]>) {
    let scale = if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    };
    let screen = screen
        .filter(|size| size.iter().all(|value| value.is_finite() && *value > 0.0))
        .map(|size| [0.0, 0.0, size[0], size[1]]);
    if raw.iter().any(|value| !value.is_finite()) || raw == [0.0, 0.0] {
        return fallback_anchor(screen, popup);
    }
    let position = [raw[0] / scale, raw[1] / scale];
    let work_area = screen.filter(|area| {
        (area[0]..=area[2]).contains(&position[0]) && (area[1]..=area[3]).contains(&position[1])
    });
    (position, work_area)
}

/// Fit a multi-line tooltip into `limit` UTF-16 units without losing its last
/// (status) line: empty lines are dropped and the lines between the first and
/// the last share what is left, each cut with an ellipsis.
#[cfg(target_os = "windows")]
pub(crate) fn fit_tooltip(text: &str, limit: usize) -> String {
    fn units(text: &str) -> usize {
        text.encode_utf16().count()
    }
    fn cut(text: &str, max: usize) -> String {
        if units(text) <= max {
            return text.to_owned();
        }
        let mut result = String::new();
        let mut used = 0;
        for character in text.chars() {
            let width = character.len_utf16();
            if used + width + 1 > max {
                break;
            }
            result.push(character);
            used += width;
        }
        let mut result = result.trim_end().to_owned();
        if max > 0 {
            result.push('\u{2026}');
        }
        result
    }
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.len() < 3 || units(&lines.join("\n")) <= limit {
        return lines.join("\n");
    }
    let (first, last) = (lines[0], lines[lines.len() - 1]);
    let middle = &lines[1..lines.len() - 1];
    let fixed = units(first) + units(last) + lines.len() - 1;
    let mut budget = limit.saturating_sub(fixed);
    // Max-min fair shares: short lines keep their size, long ones split the rest.
    let mut order: Vec<usize> = (0..middle.len()).collect();
    order.sort_by_key(|&index| units(middle[index]));
    let mut shares = vec![0; middle.len()];
    for (rank, &index) in order.iter().enumerate() {
        let share = units(middle[index]).min(budget / (middle.len() - rank));
        shares[index] = share;
        budget -= share;
    }
    let mut kept = vec![first.to_owned()];
    kept.extend(
        middle
            .iter()
            .zip(shares)
            .map(|(line, share)| cut(line, share))
            .filter(|line| !line.is_empty()),
    );
    kept.push(last.to_owned());
    kept.join("\n")
}

/// Debounce for the periodic "is the notification icon still there" probe: a
/// single failed query (Explorer busy, session lock) must not report the host
/// gone. `TaskbarCreated` still bypasses this and re-adds the icon at once.
#[cfg(target_os = "windows")]
#[derive(Default)]
pub(crate) struct HostProbe {
    misses: u8,
}

#[cfg(target_os = "windows")]
impl HostProbe {
    const MAX_MISSES: u8 = 2;

    /// Record one probe result; true once the icon has been missing for
    /// `MAX_MISSES` consecutive probes.
    pub(crate) fn host_lost(&mut self, icon_present: bool) -> bool {
        if icon_present {
            self.misses = 0;
            return false;
        }
        self.misses = self.misses.saturating_add(1);
        self.misses >= Self::MAX_MISSES
    }

    pub(crate) fn reset(&mut self) {
        self.misses = 0;
    }
}



#[cfg(target_os = "windows")]
fn spawn_backend(
    ctx: egui::Context,
    events: Sender<TrayEvent>,
    commands: Receiver<TrayCommand>,
) -> (Option<JoinHandle<()>>, Box<dyn Fn() + Send + Sync>) {
    tray_windows::spawn(ctx, events, commands)
}

#[cfg(target_os = "linux")]
fn spawn_backend(
    ctx: egui::Context,
    events: Sender<TrayEvent>,
    commands: Receiver<TrayCommand>,
) -> (Option<JoinHandle<()>>, Box<dyn Fn() + Send + Sync>) {
    tray_linux::spawn(ctx, events, commands)
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn spawn_backend(
    ctx: egui::Context,
    events: Sender<TrayEvent>,
    _commands: Receiver<TrayCommand>,
) -> (Option<JoinHandle<()>>, Box<dyn Fn() + Send + Sync>) {
    emit(
        &ctx,
        &events,
        TrayEvent::AvailabilityChanged {
            available: false,
            reason: "System tray support is unavailable on this platform.".into(),
        },
    );
    (None, Box::new(|| {}))
}

#[cfg(target_os = "linux")]
#[path = "tray_linux.rs"]
mod tray_linux;
#[cfg(target_os = "macos")]
#[path = "tray_macos.rs"]
mod tray_macos;
#[cfg(target_os = "windows")]
#[path = "tray_windows.rs"]
mod tray_windows;
