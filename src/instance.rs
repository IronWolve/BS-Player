//! Hand-off of a second launch to the player that already has the profile open.
//!
//! The profile lock decides who is "the running player". A second launch with the
//! same profile does not fail any more: it drops a small request file into
//! `<profile>/requests/` and exits, and the running player picks it up from its
//! UI loop with `take_requests`. The protocol is deliberately tiny and
//! panic-free: every file is bounded in size, count and age, everything read from
//! disk is treated as untrusted, and a request is delivered at most once.
//!
//! File format (UTF-8, `\n` line ends), named `requests/<unique>.req`:
//!
//! ```text
//! BSPLAYER-REQUEST 1
//! open            (or: show)
//! <absolute path>   one per line, only after "open"
//! ```
//!
//! A poster writes `requests/.<unique>.tmp`, then renames it to `<unique>.req`, so
//! a reader never sees a half-written request.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Folder inside the profile that holds pending requests.
const REQUEST_DIR: &str = "requests";
const HEADER: &str = "BSPLAYER-REQUEST 1";
const REQUEST_SUFFIX: &str = ".req";
const TEMP_SUFFIX: &str = ".tmp";

/// Most paths one request may carry.
pub const MAX_PATHS: usize = 10_000;
/// Largest request file, in bytes.
pub const MAX_REQUEST_BYTES: u64 = 4 * 1024 * 1024;
/// Most pending request files the folder may hold before posting is refused.
pub const MAX_PENDING_FILES: usize = 64;
/// Most request files `take_requests` handles in one call.
pub const MAX_TAKE_PER_CALL: usize = 16;
/// Requests older than this are dropped unread.
pub const MAX_REQUEST_AGE: Duration = Duration::from_secs(5 * 60);
/// A pending request this old means the running player is not polling (hung, or
/// an older build that does not know the protocol): posting then reports a
/// failure so the second launch can say so instead of silently doing nothing.
pub const STALL_AFTER: Duration = Duration::from_secs(15);
/// Directory entries examined per scan; bounds the work on a flooded folder.
const SCAN_LIMIT: usize = 1024;

/// What a second launch asks the running player to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Import these files and folders (absolute paths) and show the window.
    Open(Vec<PathBuf>),
    /// Just bring the window forward (launched with no files).
    Show,
}

/// Turn command-line arguments into a request. `args` must NOT include the
/// program name (`std::env::args_os().skip(1)`). Relative paths are made
/// absolute against the current directory, because the running player has a
/// different one. `--profile X` and `--exit-after X` (value included) and every
/// other `--flag` are ignored, exactly as the player's own startup parser does.
/// No usable path means `Show`.
pub fn parse_launch_args(args: &[OsString]) -> Request {
    let base = std::env::current_dir().ok();
    parse_launch_args_in(args, base.as_deref())
}

/// `parse_launch_args` with an explicit base directory (for tests and callers
/// that already know it). Relative paths are dropped when there is no base.
pub fn parse_launch_args_in(args: &[OsString], base: Option<&Path>) -> Request {
    let mut paths = Vec::new();
    let mut skip_value = false;
    for arg in args {
        if skip_value {
            skip_value = false;
            continue;
        }
        if arg.as_os_str() == "--profile" || arg.as_os_str() == "--exit-after" {
            skip_value = true;
            continue;
        }
        if arg.is_empty() || arg.as_encoded_bytes().starts_with(b"--") {
            continue;
        }
        if paths.len() < MAX_PATHS {
            if let Some(path) = make_absolute(Path::new(arg), base) {
                paths.push(path);
            }
        }
    }
    if paths.is_empty() {
        Request::Show
    } else {
        Request::Open(paths)
    }
}

/// An absolute, lexically tidied form of `path` (`.` and doubled separators
/// removed; `..` is kept because resolving it without the file system could
/// follow a symlink the wrong way). `None` when the path is relative and there
/// is no usable absolute `base`. Never touches the file system.
pub fn make_absolute(path: &Path, base: Option<&Path>) -> Option<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base?.join(path)
    };
    // `join` with a drive-relative or root-relative Windows path can stay relative.
    if !joined.is_absolute() {
        return None;
    }
    Some(joined.components().collect())
}

/// Hand a request to the running player. Atomic: the request file appears
/// complete or not at all. Fails (and writes nothing) when the request is too
/// large, the pending folder is full, or the oldest pending request has been
/// ignored for `STALL_AFTER`. An `Open` with no paths is sent as `Show`; paths
/// that are not absolute, not UTF-8, or contain a line break are skipped, and an
/// `Open` whose paths are all unusable fails.
pub fn post_request(profile: &Path, request: &Request) -> Result<(), String> {
    let text = encode(request)?;
    let dir = profile.join(REQUEST_DIR);
    fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    check_pending(&dir)?;
    // A name clash needs the same millisecond, process and counter; retry anyway.
    for _ in 0..8 {
        let unique = unique_name();
        let temp = dir.join(format!(".{unique}{TEMP_SUFFIX}"));
        let final_path = dir.join(format!("{unique}{REQUEST_SUFFIX}"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&temp) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Could not write the request: {error}")),
        };
        let written = file.write_all(text.as_bytes()).and_then(|()| file.flush());
        drop(file);
        if let Err(error) = written {
            let _ = fs::remove_file(&temp);
            return Err(format!("Could not write the request: {error}"));
        }
        if let Err(error) = fs::rename(&temp, &final_path) {
            let _ = fs::remove_file(&temp);
            return Err(format!("Could not hand the request over: {error}"));
        }
        return Ok(());
    }
    Err("Could not choose a unique request name.".into())
}

/// Collect the requests that arrived since the last call, oldest first, and
/// delete their files. Handles at most `MAX_TAKE_PER_CALL` files per call (more
/// wait for the next call). Malformed, oversized, non-regular and expired files
/// are deleted and skipped. A request is returned only if its file could be
/// deleted, so it is never delivered twice. Never panics; an absent folder (the
/// normal case) costs one failed `read_dir`.
///
/// Recommended cadence: poll every 500 ms from the UI loop, and keep polling
/// while the window is hidden to the tray (use `request_repaint_after`).
pub fn take_requests(profile: &Path) -> Vec<Request> {
    let dir = profile.join(REQUEST_DIR);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let now = SystemTime::now();
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten().take(SCAN_LIMIT) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') {
            // A temporary left behind by a poster that died between write and rename.
            if name.ends_with(TEMP_SUFFIX) {
                if let Ok(meta) = entry.metadata() {
                    if age_of(&meta, now) > MAX_REQUEST_AGE {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }
        } else if name.ends_with(REQUEST_SUFFIX) {
            files.push((name.to_owned(), entry.path()));
        }
    }
    // Names start with a zero-padded millisecond timestamp: name order is arrival order.
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut requests = Vec::new();
    for (_, path) in files.into_iter().take(MAX_TAKE_PER_CALL) {
        if let Some(request) = take_one(&path, now) {
            requests.push(request);
        }
    }
    requests
}

fn take_one(path: &Path, now: SystemTime) -> Option<Request> {
    // symlink_metadata: a link planted under a request name must not be followed.
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() {
        // Remove links and stray files; a directory cannot be removed this way
        // and is left alone (and skipped again next time).
        let _ = fs::remove_file(path);
        return None;
    }
    if meta.len() > MAX_REQUEST_BYTES || age_of(&meta, now) > MAX_REQUEST_AGE {
        let _ = fs::remove_file(path);
        return None;
    }
    let request = read_bounded(path).and_then(|bytes| String::from_utf8(bytes).ok());
    let request = request.and_then(|text| decode(&text));
    // Deliver only what was actually consumed, so a file that cannot be deleted
    // is not acted on again on every poll.
    fs::remove_file(path).ok()?;
    request
}

fn read_bounded(path: &Path) -> Option<Vec<u8>> {
    let file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_REQUEST_BYTES).then_some(bytes)
}

fn age_of(meta: &fs::Metadata, now: SystemTime) -> Duration {
    // A modified time in the future (clock change) counts as brand new.
    meta.modified()
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .unwrap_or(Duration::ZERO)
}

/// Refuse to post into a full folder or to a player that has stopped polling.
fn check_pending(dir: &Path) -> Result<(), String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    let now = SystemTime::now();
    let mut count = 0usize;
    for entry in entries.flatten().take(SCAN_LIMIT) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with('.') || !name.ends_with(REQUEST_SUFFIX) {
            continue;
        }
        count += 1;
        if entry
            .metadata()
            .is_ok_and(|meta| age_of(&meta, now) > STALL_AFTER)
        {
            return Err("The running BS Player is not picking up requests.".into());
        }
    }
    if count >= MAX_PENDING_FILES {
        return Err("Too many requests are already waiting for the running BS Player.".into());
    }
    Ok(())
}

fn unique_name() -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{millis:016}-{:08x}-{counter:08x}", std::process::id())
}

/// The path as a single request line, or `None` if it cannot be sent safely.
fn path_line(path: &Path) -> Option<&str> {
    let text = path.to_str()?;
    let usable = path.is_absolute() && !text.contains(['\n', '\r', '\0']);
    usable.then_some(text)
}

fn encode(request: &Request) -> Result<String, String> {
    let paths = match request {
        Request::Show => return Ok(format!("{HEADER}\nshow\n")),
        Request::Open(paths) if paths.is_empty() => return Ok(format!("{HEADER}\nshow\n")),
        Request::Open(paths) => paths,
    };
    if paths.len() > MAX_PATHS {
        return Err(format!("Too many files to hand over (limit {MAX_PATHS})."));
    }
    let mut text = format!("{HEADER}\nopen\n");
    let mut sent = 0usize;
    for path in paths {
        let Some(line) = path_line(path) else {
            continue;
        };
        if (text.len() + line.len() + 1) as u64 > MAX_REQUEST_BYTES {
            return Err("The list of files is too large to hand over.".into());
        }
        text.push_str(line);
        text.push('\n');
        sent += 1;
    }
    if sent == 0 {
        return Err("None of the file names can be handed to the running player.".into());
    }
    Ok(text)
}

fn decode(text: &str) -> Option<Request> {
    let mut lines = text.split('\n');
    if lines.next()? != HEADER {
        return None;
    }
    match lines.next()? {
        "show" => Some(Request::Show),
        "open" => {
            let mut paths = Vec::new();
            for line in lines {
                if line.is_empty() {
                    continue;
                }
                if paths.len() == MAX_PATHS || line.contains(['\r', '\0']) {
                    return None;
                }
                let path = Path::new(line);
                if !path.is_absolute() {
                    return None;
                }
                paths.push(path.to_path_buf());
            }
            (!paths.is_empty()).then_some(Request::Open(paths))
        }
        _ => None,
    }
}
