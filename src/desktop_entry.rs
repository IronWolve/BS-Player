//! Linux desktop integration for the AppImage.
//!
//! A bare AppImage installs nothing, so the dock and the application menu show a
//! generic icon and the file manager draws the plain AppImage type icon. When the
//! player starts from an AppImage it adds a launcher entry and icons under the
//! user's data folder, plus a small thumbnailer that reads the icon the packager
//! appended to the AppImage. Everything written carries a mark; a file without the
//! mark belongs to the user and is never touched. Options can turn it all off.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const APP_ID: &str = "bs-player";
/// Present in every file this module writes. A file without it is left alone.
const MARK: &str = "X-BSPlayer-Managed=true";

/// The packaged launcher entry; only its `Exec` line is rewritten for the AppImage.
const DESKTOP_TEMPLATE: &str = include_str!("../assets/bs-player.desktop");
const ICONS: [(u32, &[u8]); 6] = [
    (
        256,
        include_bytes!("../assets/branding/bs-podcasts-shell-icon-256.png"),
    ),
    (
        128,
        include_bytes!("../assets/branding/bs-podcasts-shell-icon-128.png"),
    ),
    (
        64,
        include_bytes!("../assets/branding/bs-podcasts-shell-icon-64.png"),
    ),
    (
        32,
        include_bytes!("../assets/branding/bs-podcasts-shell-icon-32.png"),
    ),
    (
        16,
        include_bytes!("../assets/branding/bs-podcasts-shell-icon-16.png"),
    ),
    (
        512,
        include_bytes!("../assets/branding/bs-podcasts-shell-icon-512.png"),
    ),
];
/// Reads the appended icon without ever running the AppImage, so it also works in the
/// file manager's thumbnail sandbox, which cannot reach the home folder. The code
/// holds no double quote, backslash, dollar, backtick or percent sign so it can sit
/// inside one quoted `Exec` argument.
const THUMBNAILER: &str = "# X-BSPlayer-Managed=true\n\
[Thumbnailer Entry]\n\
TryExec=/usr/bin/python3\n\
Exec=/usr/bin/python3 -c \"import sys;f=open(sys.argv[1],'rb');f.seek(-12,2);t=f.read();n=int.from_bytes(t[:4],'big');assert t[4:]==b'BSPICN01';f.seek(-12-n,2);open(sys.argv[2],'wb').write(f.read(n))\" %i %o\n\
MimeType=application/vnd.appimage;application/x-iso9660-appimage;\n";

/// The AppImage this process was started from, if any. The AppImage runtime exports
/// its absolute path as `APPIMAGE`.
pub fn appimage_path() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("APPIMAGE")?);
    (path.is_absolute() && path.is_file()).then_some(path)
}

/// True when this process runs from an AppImage (Options shows the setting only then).
pub fn running_from_appimage() -> bool {
    appimage_path().is_some()
}

/// The user's data folder: an absolute `XDG_DATA_HOME`, else `~/.local/share`.
pub fn data_home() -> Option<PathBuf> {
    data_home_from(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

fn data_home_from(xdg: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let xdg = xdg.map(PathBuf::from).filter(|p| p.is_absolute());
    xdg.or_else(|| {
        home.map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|home| home.join(".local/share"))
    })
}

/// Adds the entry, icons and thumbnailer on a background thread so a slow file
/// system never delays the window. Does nothing outside an AppImage.
pub fn spawn_integration() {
    let (Some(appimage), Some(data_home)) = (appimage_path(), data_home()) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("desktop-entry".into())
        .spawn(move || {
            if let Err(error) = integrate(&appimage, &data_home) {
                eprintln!("BS Player: could not add the desktop menu entry: {error}");
            }
            set_file_icon(&appimage, &data_home);
        });
}

/// Writes the launcher entry, the icons and the thumbnailer under `data_home` and
/// returns the files that changed. A launcher entry the user made (no mark) stops
/// everything; a thumbnailer the user made is skipped on its own.
pub fn integrate(appimage: &Path, data_home: &Path) -> io::Result<Vec<PathBuf>> {
    let appimage_text = appimage
        .to_str()
        .filter(|text| !text.contains(['\n', '\r']))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "the AppImage path is not plain text",
            )
        })?;
    let entry = data_home.join(format!("applications/{APP_ID}.desktop"));
    if is_foreign(&entry)? {
        return Ok(Vec::new());
    }
    let mut written = Vec::new();
    let mut put = |path: PathBuf, bytes: &[u8]| -> io::Result<()> {
        if write_if_changed(&path, bytes)? {
            written.push(path);
        }
        Ok(())
    };
    put(entry, desktop_entry_text(appimage_text).as_bytes())?;
    for (size, bytes) in ICONS {
        put(icon_path(data_home, size), bytes)?;
    }
    let thumbnailer = data_home.join("thumbnailers/bs-player-appimage.thumbnailer");
    if !is_foreign(&thumbnailer)? {
        put(thumbnailer, THUMBNAILER.as_bytes())?;
    }
    Ok(written)
}

fn icon_path(data_home: &Path, size: u32) -> PathBuf {
    data_home.join(format!("icons/hicolor/{size}x{size}/apps/{APP_ID}.png"))
}

/// The packaged entry with `Exec` pointing at this AppImage and the mark added.
fn desktop_entry_text(appimage: &str) -> String {
    let mut text = String::new();
    for line in DESKTOP_TEMPLATE.lines() {
        if line.starts_with("Exec=") {
            text.push_str(&format!("Exec={} %F\n", exec_argument(appimage)));
        } else if !line.starts_with("TryExec=") && !line.starts_with("X-BSPlayer-") {
            text.push_str(line);
            text.push('\n');
        }
    }
    text.push_str(&format!("TryExec={}\n{MARK}\n", escape_string(appimage)));
    text
}

/// An `Exec` argument as the Desktop Entry specification wants it: always double
/// quoted, a backslash before `"`, backtick, `$` and `\` inside the quotes, `%%` for a
/// literal percent sign, then the file's own string escaping doubles every backslash.
fn exec_argument(path: &str) -> String {
    let mut quoted = String::from("\"");
    for character in path.chars() {
        if matches!(character, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        if character == '%' {
            quoted.push('%');
        }
        quoted.push(character);
    }
    quoted.push('"');
    escape_string(&quoted)
}

/// String escaping of a Desktop Entry value: a backslash is written twice.
fn escape_string(value: &str) -> String {
    value.replace('\\', "\\\\")
}

/// True when `path` exists and was not written by this module.
fn is_foreign(path: &Path) -> io::Result<bool> {
    match fs::read(path) {
        Ok(bytes) => Ok(!bytes
            .windows(MARK.len())
            .any(|window| window == MARK.as_bytes())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Writes through a temporary file so a reader never sees half a file. Returns
/// whether the content changed.
fn write_if_changed(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if fs::read(path).is_ok_and(|current| current == bytes) {
        return Ok(false);
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no folder"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, bytes)?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(true)
}

/// Gives the AppImage file the player icon in file managers that honour a custom icon
/// (Caja, older GNOME Files). Others use the thumbnailer. Failure is not an error:
/// `gio` may be missing.
fn set_file_icon(appimage: &Path, data_home: &Path) {
    let icon = icon_path(data_home, 256);
    if !icon.is_file() {
        return;
    }
    let uri = file_uri(&icon);
    let key = OsStr::new("metadata::custom-icon");
    let current = gio_output(&[OsStr::new("info"), OsStr::new("-a"), key, appimage.as_os_str()]);
    if current.is_some_and(|text| text.contains(&uri)) {
        return;
    }
    let _ = gio_output(&[
        OsStr::new("set"),
        appimage.as_os_str(),
        key,
        OsStr::new(&uri),
    ]);
}

/// Runs `gio` and returns its standard output when it succeeds. The AppImage's own
/// library folders are taken off `LD_LIBRARY_PATH` first: a system tool must never
/// load the bundled libraries.
fn gio_output(arguments: &[&OsStr]) -> Option<String> {
    let mut command = Command::new("gio");
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if let (Some(bundle), Some(path)) = (
        std::env::var_os("APPDIR"),
        std::env::var_os("LD_LIBRARY_PATH"),
    ) {
        match scrubbed_library_path(&path, Path::new(&bundle)) {
            Some(kept) => command.env("LD_LIBRARY_PATH", kept),
            None => command.env_remove("LD_LIBRARY_PATH"),
        };
    }
    let output = command.output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `LD_LIBRARY_PATH` without the entries inside `bundle`; `None` when nothing is left.
fn scrubbed_library_path(value: &OsStr, bundle: &Path) -> Option<OsString> {
    let kept: Vec<PathBuf> = std::env::split_paths(value)
        .filter(|entry| !entry.as_os_str().is_empty() && !entry.starts_with(bundle))
        .collect();
    if kept.is_empty() {
        None
    } else {
        std::env::join_paths(kept).ok()
    }
}

/// A `file://` URI with everything but letters, digits and `-._~/` percent-encoded.
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}
