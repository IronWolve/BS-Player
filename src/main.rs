#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod app;
mod appearance;
mod chapters;
mod container_audio;
mod cpu;
#[cfg(target_os = "linux")]
mod desktop_entry;
mod eq;
mod gui_cache;
mod instance;
mod join;
mod lyrics;
mod media;
mod opus;
mod persistence;
mod pitch;
mod playback;

mod settings;
mod silence;
mod state;
mod stream;
mod tagwrite;
mod tray;
mod tray_popup;
mod widgets;
mod themes;
mod winintegration;

fn main() -> eframe::Result {
    let profile = state::profile_directory();
    if let Err(error) = std::fs::create_dir_all(&profile) {
        startup_error(&format!(
            "Could not create profile {}: {error}. Launch with --profile <folder> to choose a writable data location.",
            profile.display()
        ));
    }
    let _profile_lock = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(profile.join("profile.lock"))
    {
        Ok(file) => match file.try_lock() {
            Ok(()) => file,
            // Another player owns this profile: hand it the files (or just ask it to
            // show itself) instead of failing, and exit quietly.
            Err(std::fs::TryLockError::WouldBlock) => hand_off_to_running_player(&profile),
            Err(std::fs::TryLockError::Error(error)) => {
                startup_error(&format!(
                    "This BS Player profile is already open or cannot be locked: {error}"
                ));
            }
        },
        Err(error) => {
            startup_error(&format!("Could not lock profile: {error}"));
        }
    };
    state::remove_stale_pending(&profile);
    if let Err(error) = state::check_compatible(&profile) {
        startup_error(&error);
    }
    let (mut state, warning) = state::load(&profile);
    if !state.settings.restore_session {
        state.clear_session();
    }
    let window = state.window.clone();
    #[cfg(target_os = "linux")]
    if state.settings.desktop_integration {
        desktop_entry::spawn_integration();
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("BS Player")
            // Wayland app id / X11 class: must equal the launcher file name so the
            // dock matches the window to its icon.
            .with_app_id("bs-player")
            .with_decorations(false)
            .with_resizable(true)
            .with_icon(widgets::application_icon())
            .with_inner_size([window.width, window.height])
            .with_min_inner_size([760.0, 520.0])
            .with_position([window.x, window.y])
            .with_maximized(window.maximized)
            .with_fullscreen(state.fullscreen),
        ..Default::default()
    };
    eframe::run_native(
        "BS Player",
        options,
        Box::new(move |cc| Ok(Box::new(app::PlayerApp::new(cc, profile, state, warning)))),
    )
}

/// A second launch with the same profile. The running player picks the request up
/// from `<profile>/requests/` (see `instance`); this process then exits with code 0
/// and no dialog. Only if the request cannot be delivered does it fall back to the
/// old "profile is already open" error.
fn hand_off_to_running_player(profile: &std::path::Path) -> ! {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let request = instance::parse_launch_args(&args);
    match instance::post_request(profile, &request) {
        Ok(()) => {
            // Lets the running player take the foreground when it shows its window.
            winintegration::allow_running_player_to_focus();
            match &request {
                instance::Request::Open(paths) => eprintln!(
                    "BS Player is already running with this profile; handed it {} path(s).",
                    paths.len()
                ),
                instance::Request::Show => {
                    eprintln!(
                        "BS Player is already running with this profile; asked it to show its window."
                    )
                }
            }
            std::process::exit(0)
        }
        Err(error) => startup_error(&format!(
            "This BS Player profile is already open, and the running player could not be asked to open your request: {error}"
        )),
    }
}

fn startup_error(message: &str) -> ! {
    eprintln!("{message}");
    if !std::env::args_os().any(|a| a == "--silent") {
        rfd::MessageDialog::new()
            .set_title("BS Player could not open")
            .set_description(message)
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
    std::process::exit(1)
}
