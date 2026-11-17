use std::{
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, RecvTimeoutError, Sender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use eframe::egui;
use zbus::blocking::{Proxy, connection::Builder};

use super::{TrayCommand, TrayEvent, emit};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const WATCHER_INTERFACE: &str = "org.kde.StatusNotifierWatcher";
const ITEM_PATH: &str = "/StatusNotifierItem";
const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
const CHECK_INTERVAL: Duration = Duration::from_secs(2);

const WAYLAND_REASON: &str = "The system tray is not available in a Wayland session: Wayland does not let an application hide or place its own windows. Start BS Player in an X11 session to use it.";

type Pixmap = Vec<(i32, i32, Vec<u8>)>;

/// winit (and so the player) selects Wayland whenever either variable is set
/// and non-empty.
fn is_wayland_session(display: Option<&str>, socket: Option<&str>) -> bool {
    [display, socket]
        .into_iter()
        .flatten()
        .any(|value| !value.is_empty())
}

pub(super) fn spawn(
    ctx: egui::Context,
    events: Sender<TrayEvent>,
    commands: Receiver<TrayCommand>,
) -> (Option<JoinHandle<()>>, Box<dyn Fn() + Send + Sync>) {
    if is_wayland_session(
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
        std::env::var("WAYLAND_SOCKET").ok().as_deref(),
    ) {
        // Wayland clients cannot hide, position or raise their own windows, so
        // hide-to-tray would leave the player on screen and the popup would
        // appear wherever the compositor puts it. Say so instead of offering it.
        emit(
            &ctx,
            &events,
            TrayEvent::AvailabilityChanged {
                available: false,
                reason: WAYLAND_REASON.into(),
            },
        );
        return (None, Box::new(|| {}));
    }
    let worker_events = events.clone();
    let worker_ctx = ctx.clone();
    let panic_ctx = ctx.clone();
    let panic_events = events.clone();
    let worker = match thread::Builder::new()
        .name("bs-player-tray-linux-sni".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(worker_ctx, worker_events, commands)
            }))
            .is_err()
            {
                emit(
                    &panic_ctx,
                    &panic_events,
                    TrayEvent::AvailabilityChanged {
                        available: false,
                        reason: "Linux tray worker stopped unexpectedly.".into(),
                    },
                );
            }
        }) {
        Ok(worker) => worker,
        Err(error) => {
            emit(
                &ctx,
                &events,
                TrayEvent::AvailabilityChanged {
                    available: false,
                    reason: format!("Could not start the Linux tray worker: {error}"),
                },
            );
            return (None, Box::new(|| {}));
        }
    };
    (Some(worker), Box::new(|| {}))
}

fn run(ctx: egui::Context, events: Sender<TrayEvent>, commands: Receiver<TrayCommand>) {
    let tooltip = Arc::new(Mutex::new("BS Player".to_owned()));
    let mut availability = Availability::default();
    let icon = icon_pixmaps();
    loop {
        match run_session(
            &ctx,
            &events,
            &commands,
            Arc::clone(&tooltip),
            &icon,
            &mut availability,
        ) {
            SessionExit::Shutdown => break,
            SessionExit::Retry(reason) => {
                availability.set(&ctx, &events, false, reason);
                match commands.recv_timeout(CHECK_INTERVAL) {
                    Ok(TrayCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                    Ok(TrayCommand::UpdateTooltip(text)) => {
                        if let Ok(mut current) = tooltip.lock() {
                            *current = text;
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
        }
    }
    availability.set(&ctx, &events, false, "Tray service stopped.".into());
}

enum SessionExit {
    Shutdown,
    Retry(String),
}

fn run_session(
    ctx: &egui::Context,
    events: &Sender<TrayEvent>,
    commands: &Receiver<TrayCommand>,
    tooltip: Arc<Mutex<String>>,
    icon: &Pixmap,
    availability: &mut Availability,
) -> SessionExit {
    let item = StatusNotifierItem {
        ctx: ctx.clone(),
        events: events.clone(),
        tooltip: Arc::clone(&tooltip),
        icon_pixmap: icon.clone(),
    };
    let connection = match Builder::session()
        .map(|builder| builder.method_timeout(Duration::from_secs(2)))
        .and_then(|builder| builder.serve_at(ITEM_PATH, item))
        .and_then(|builder| builder.build())
    {
        Ok(connection) => connection,
        Err(error) => return SessionExit::Retry(format!("Session bus unavailable: {error}")),
    };

    let unique_name = match connection.unique_name() {
        Some(name) => name.to_string(),
        None => {
            return SessionExit::Retry("Session bus gave no unique name to the tray item.".into());
        }
    };
    let mut registered = false;
    let mut watcher_owner: Option<String> = None;

    loop {
        match commands.recv_timeout(CHECK_INTERVAL) {
            Ok(TrayCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                return SessionExit::Shutdown;
            }
            Ok(TrayCommand::UpdateTooltip(text)) => {
                if let Ok(mut current) = tooltip.lock() {
                    *current = text;
                }
                let _ = connection.emit_signal(
                    None::<&str>,
                    ITEM_PATH,
                    ITEM_INTERFACE,
                    "NewToolTip",
                    &(),
                );
            }
            Err(RecvTimeoutError::Timeout) => {}
        }

        let bus = Proxy::new(
            &connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        );
        if let Ok(bus) = bus {
            let current_owner: zbus::Result<String> = bus.call("GetNameOwner", &WATCHER);
            if let Ok(owner) = current_owner {
                if watcher_owner.as_deref() != Some(owner.as_str()) {
                    watcher_owner = Some(owner);
                    registered = false;
                }
            } else {
                // Nobody owns the watcher name: do not query a property on it
                // (that can bus-activate a watcher every poll); just wait.
                watcher_owner = None;
                registered = false;
                availability.set(
                    ctx,
                    events,
                    false,
                    "No StatusNotifier tray host is registered on the session bus.".into(),
                );
                if connection.is_closed() {
                    return SessionExit::Retry("Session bus connection closed.".into());
                }
                continue;
            }
        }
        let watcher = Proxy::new(&connection, WATCHER, WATCHER_PATH, WATCHER_INTERFACE);
        let host_registered =
            watcher.and_then(|proxy| proxy.get_property::<bool>("IsStatusNotifierHostRegistered"));
        match host_registered {
            Ok(true) if !registered => {
                let proxy = match Proxy::new(&connection, WATCHER, WATCHER_PATH, WATCHER_INTERFACE)
                {
                    Ok(proxy) => proxy,
                    Err(error) => {
                        availability.set(
                            ctx,
                            events,
                            false,
                            format!("Could not contact the tray host: {error}"),
                        );
                        continue;
                    }
                };
                match proxy.call_method("RegisterStatusNotifierItem", &unique_name) {
                    Ok(_) => {
                        registered = true;
                        availability.set(ctx, events, true, "".into());
                    }
                    Err(error) => {
                        availability.set(
                            ctx,
                            events,
                            false,
                            format!("Tray host rejected BS Player: {error}"),
                        );
                    }
                }
            }
            Ok(true) => availability.set(ctx, events, true, "".into()),
            Ok(false) => {
                registered = false;
                availability.set(
                    ctx,
                    events,
                    false,
                    "No StatusNotifier tray host is registered on the session bus.".into(),
                );
            }
            Err(error) => {
                registered = false;
                availability.set(
                    ctx,
                    events,
                    false,
                    format!("StatusNotifier tray host is unavailable: {error}"),
                );
                if connection.is_closed() {
                    return SessionExit::Retry(error.to_string());
                }
            }
        }
    }
}

#[derive(Default)]
struct Availability {
    available: bool,
    reason: String,
}

impl Availability {
    fn set(
        &mut self,
        ctx: &egui::Context,
        events: &Sender<TrayEvent>,
        available: bool,
        reason: String,
    ) {
        if self.available == available && self.reason == reason {
            return;
        }
        self.available = available;
        self.reason = reason.clone();
        emit(
            ctx,
            events,
            TrayEvent::AvailabilityChanged { available, reason },
        );
    }
}

struct StatusNotifierItem {
    ctx: egui::Context,
    events: Sender<TrayEvent>,
    tooltip: Arc<Mutex<String>>,
    icon_pixmap: Pixmap,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl StatusNotifierItem {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    fn id(&self) -> &str {
        "bs-player"
    }

    #[zbus(property)]
    fn title(&self) -> &str {
        "BS Player"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }

    #[zbus(property)]
    fn window_id(&self) -> u32 {
        0
    }

    #[zbus(property)]
    fn icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Pixmap {
        self.icon_pixmap.clone()
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property, name = "ToolTip")]
    fn tool_tip(&self) -> (String, Pixmap, String, String) {
        let description = self
            .tooltip
            .lock()
            .map(|tooltip| tooltip.clone())
            .unwrap_or_else(|_| "BS Player".into());
        (
            "".into(),
            self.icon_pixmap.clone(),
            "BS Player".into(),
            description,
        )
    }

    #[zbus(name = "Activate")]
    fn activate(&self, _x: i32, _y: i32) {
        emit(&self.ctx, &self.events, TrayEvent::Restore);
    }

    #[zbus(name = "SecondaryActivate")]
    fn secondary_activate(&self, _x: i32, _y: i32) {
        emit(&self.ctx, &self.events, TrayEvent::Restore);
    }

    #[zbus(name = "ContextMenu")]
    fn context_menu(&self, x: i32, y: i32) {
        // X11 hosts report physical pixels, but the popup is positioned in
        // Egui points (winit multiplies by UI zoom and display scale).
        let (scale, screen) = display_metrics(&self.ctx);
        let (position, work_area) = super::anchor_in_points(
            [x as f32, y as f32],
            scale,
            screen,
            crate::tray_popup::DEFAULT_INNER_SIZE,
        );
        emit(
            &self.ctx,
            &self.events,
            TrayEvent::OpenMenu {
                position,
                work_area,
            },
        );
    }

    #[zbus(name = "Scroll")]
    fn scroll(&self, _delta: i32, _orientation: &str) {}
}

/// Physical pixels per Egui point (display scale x UI zoom) and the size in
/// points of the monitor the player is on, as last reported by the windowing
/// layer.
fn display_metrics(ctx: &egui::Context) -> (f32, Option<[f32; 2]>) {
    let (native, monitor) = ctx.input_for(egui::ViewportId::ROOT, |input| {
        let info = input.viewport();
        (info.native_pixels_per_point, info.monitor_size)
    });
    (
        native.unwrap_or(1.0) * ctx.zoom_factor(),
        monitor.map(|size| [size.x, size.y]),
    )
}

fn icon_pixmaps() -> Pixmap {
    let bytes = include_bytes!("../assets/branding/bs-podcasts-shell-icon-512.png");
    let Ok(decoded) = image::load_from_memory(bytes) else {
        return Vec::new();
    };
    let rgba = decoded.into_rgba8();
    [32, 64]
        .into_iter()
        .map(|size| {
            let image =
                image::imageops::resize(&rgba, size, size, image::imageops::FilterType::Triangle);
            let mut argb = Vec::with_capacity((size * size * 4) as usize);
            for pixel in image.pixels() {
                argb.extend_from_slice(&[pixel[3], pixel[0], pixel[1], pixel[2]]);
            }
            (size as i32, size as i32, argb)
        })
        .collect()
}
