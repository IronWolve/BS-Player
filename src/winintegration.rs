//! Windows shell integration: opt-in per-user file associations and global media
//! keys. Every function exists on every platform so the app compiles everywhere;
//! off Windows they are inert (associations report "not registered" and refuse,
//! media keys never arrive).
//!
//! File associations touch only `HKEY_CURRENT_USER\Software\Classes` (never
//! `HKLM`) and only on an explicit `register_associations`; `unregister_associations`
//! removes exactly what was added. BS Player is added to the "Open with" lists; it
//! does not take over anyone's default app (Windows 10/11 protect defaults: the
//! user picks BS Player in Settings, see `open_default_apps_settings`).
//!
//! Media keys use `RegisterHotKey` on the tray's hidden message window (see
//! `tray_windows.rs`); no low-level keyboard hook is installed.

// The registry and hot-key internals are used by the Windows implementation only;
// on other systems the pure helpers (and their tests) are all that is built.
#![cfg_attr(not(windows), allow(dead_code))]

use std::{
    collections::VecDeque,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

// ---------------------------------------------------------------------------
// File associations: pure helpers (tested on every platform)
// ---------------------------------------------------------------------------

/// Name of the ProgID created under `HKCU\Software\Classes`.
pub const PROG_ID: &str = "BSPlayer.Audio";
/// Name shown in Explorer's "Open with" lists.
const FRIENDLY_NAME: &str = "BS Player";
/// Description of the ProgID (what Explorer calls the file type).
const PROG_ID_DESCRIPTION: &str = "BS Player audio file";
/// Value inside our own ProgID key that records which `Applications\<exe>` key was
/// written, so unregistering still finds it if the program file was renamed.
const APPLICATION_KEY_VALUE: &str = "BSPlayerApplicationKey";
/// Extensions (no dot) that get an "Open with BS Player" entry.
pub const ASSOCIATION_EXTENSIONS: [&str; 17] = [
    "mp3", "m4a", "m4b", "mp4", "flac", "ogg", "opus", "wav", "aac", "aif", "aiff", "m3u", "m3u8",
    "pls", "mkv", "mka", "webm",
];
/// Longest program path accepted for the registry (sanity bound, well under the
/// 32 767-character registry value limit).
const MAX_EXE_PATH_CHARS: usize = 4096;
/// Registry key names are limited to 255 characters; stay well inside.
const MAX_EXE_NAME_CHARS: usize = 200;

/// Where the registered command points, relative to this program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssociationState {
    /// No BS Player ProgID is registered for this user.
    NotRegistered,
    /// Registered, and the command launches this very program file.
    Registered,
    /// Registered for a different program file (the app was moved or renamed).
    /// Register again to repoint it, or unregister to remove it.
    OtherLocation,
}

fn prog_id_key() -> String {
    format!(r"Software\Classes\{PROG_ID}")
}

fn prog_id_command_key() -> String {
    format!(r"{}\shell\open\command", prog_id_key())
}

fn extension_progids_key(extension: &str) -> String {
    format!(r"Software\Classes\.{extension}\OpenWithProgids")
}

fn application_key(exe_name: &str) -> String {
    format!(r"Software\Classes\Applications\{exe_name}")
}

/// The program path as text, or an error if it cannot be put in a registry command
/// safely: it must be UTF-8 and free of quotes (not valid in Windows paths anyway),
/// percent signs (the shell expands `%1`, `%L`, ...) and control characters.
fn checked_exe_text(exe: &Path) -> Result<&str, String> {
    let text = exe
        .to_str()
        .ok_or("The program path is not valid text, so it cannot be registered.")?;
    if text.is_empty() || text.chars().count() > MAX_EXE_PATH_CHARS {
        return Err("The program path has an unusable length.".into());
    }
    if let Some(bad) = text
        .chars()
        .find(|c| c.is_control() || *c == '"' || *c == '%')
    {
        return Err(format!(
            "The program path contains {bad:?}, which cannot be used in a file-association command."
        ));
    }
    Ok(text)
}

/// The command stored for opening files: `"<exe>" "%1"`.
fn open_command(exe: &Path) -> Result<String, String> {
    Ok(format!("\"{}\" \"%1\"", checked_exe_text(exe)?))
}

/// The `DefaultIcon` value: the first icon of the program.
fn default_icon_value(exe: &Path) -> Result<String, String> {
    Ok(format!("\"{}\",0", checked_exe_text(exe)?))
}

/// File name used for the `Applications\<name>` key.
fn exe_file_name(exe: &Path) -> Result<String, String> {
    let name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("The program has no usable file name.")?;
    if !is_valid_key_name(name) {
        return Err("The program file name cannot be used as a registry key.".into());
    }
    Ok(name.to_owned())
}

fn is_valid_key_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_EXE_NAME_CHARS
        && !name
            .chars()
            .any(|c| c.is_control() || c == '\\' || c == '/')
}

/// True if the stored `command` is what `open_command(exe)` would write. Windows
/// paths are case-insensitive, so the comparison ignores ASCII case.
fn command_matches(command: &str, exe: &Path) -> bool {
    open_command(exe).is_ok_and(|expected| expected.eq_ignore_ascii_case(command.trim()))
}

fn classify_command(stored: Option<&str>, exe: Option<&Path>) -> AssociationState {
    match (stored, exe) {
        (None, _) => AssociationState::NotRegistered,
        (Some(command), Some(exe)) if command_matches(command, exe) => AssociationState::Registered,
        (Some(_), _) => AssociationState::OtherLocation,
    }
}

/// Whether the ProgID points at the program that is running now.
#[allow(dead_code)] // Options reads `association_state()`; kept as the yes/no form.
pub fn associations_registered() -> bool {
    association_state() == AssociationState::Registered
}

/// Registered state with the "moved app" case separated out.
pub fn association_state() -> AssociationState {
    #[cfg(windows)]
    {
        let exe = std::env::current_exe().ok();
        classify_command(
            windows_impl::read_registered_command().as_deref(),
            exe.as_deref(),
        )
    }
    #[cfg(not(windows))]
    {
        classify_command(None, None)
    }
}

/// Add BS Player to Explorer's "Open with" lists for the extensions in
/// `ASSOCIATION_EXTENSIONS`, for the current user only. `exe` is the program that
/// should be launched (pass `std::env::current_exe()`). On a partial failure
/// whatever was written is removed again. Blocks briefly (registry and shell
/// notification): call it from a worker thread, not the UI thread.
pub fn register_associations(exe: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows_impl::register(exe)
    }
    #[cfg(not(windows))]
    {
        let _ = exe;
        Err(UNSUPPORTED.into())
    }
}

/// Remove exactly what `register_associations` added, and tell Explorer.
/// Succeeds when nothing is registered. Does not touch the user's chosen defaults
/// (if BS Player was picked as a default, Windows asks again for that type).
pub fn unregister_associations() -> Result<(), String> {
    #[cfg(windows)]
    {
        windows_impl::unregister()
    }
    #[cfg(not(windows))]
    {
        Err(UNSUPPORTED.into())
    }
}

/// Open Windows Settings at "Default apps" so the user can pick BS Player for a
/// file type. Returns whether Windows accepted the request.
pub fn open_default_apps_settings() -> bool {
    #[cfg(windows)]
    {
        windows_impl::open_default_apps()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Called by a second launch right after it handed its request over: lets the
/// running player take the foreground when it shows its window (Windows blocks
/// that otherwise and only flashes the taskbar button). Best effort; no-op
/// elsewhere.
pub fn allow_running_player_to_focus() {
    #[cfg(windows)]
    windows_impl::allow_foreground();
}

#[cfg(not(windows))]
const UNSUPPORTED: &str = "File associations are only available on Windows.";

// ---------------------------------------------------------------------------
// Media keys
// ---------------------------------------------------------------------------

/// A global media key press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKey {
    PlayPause,
    Next,
    Previous,
    Stop,
}

impl MediaKey {
    pub const ALL: [MediaKey; 4] = [
        MediaKey::PlayPause,
        MediaKey::Next,
        MediaKey::Previous,
        MediaKey::Stop,
    ];

    /// Position in `ALL` and in the array returned by `media_keys_held`.
    fn index(self) -> usize {
        match self {
            MediaKey::PlayPause => 0,
            MediaKey::Next => 1,
            MediaKey::Previous => 2,
            MediaKey::Stop => 3,
        }
    }

    /// The Windows virtual-key code (`VK_MEDIA_*`).
    fn virtual_key(self) -> u32 {
        match self {
            MediaKey::PlayPause => 0xB3,
            MediaKey::Next => 0xB0,
            MediaKey::Previous => 0xB1,
            MediaKey::Stop => 0xB2,
        }
    }
}

/// First hot-key id; `base + MediaKey::index()` identifies a key on the window.
const HOTKEY_ID_BASE: usize = 0x4250;
/// Most key presses waiting for the UI to read them.
const MEDIA_QUEUE_CAP: usize = 16;

static MEDIA_QUEUE: Mutex<VecDeque<MediaKey>> = Mutex::new(VecDeque::new());
static MEDIA_KEYS_WANTED: AtomicBool = AtomicBool::new(false);
static MEDIA_KEYS_HELD: [AtomicBool; 4] = [const { AtomicBool::new(false) }; 4];

fn hotkey_id(key: MediaKey) -> usize {
    HOTKEY_ID_BASE + key.index()
}

fn media_key_for_hotkey_id(id: usize) -> Option<MediaKey> {
    let index = id.checked_sub(HOTKEY_ID_BASE)?;
    MediaKey::ALL.get(index).copied()
}

/// Queue a key press unless the queue is full (then the newest press is dropped).
fn push_bounded(queue: &Mutex<VecDeque<MediaKey>>, key: MediaKey) -> bool {
    let mut queue = queue.lock().unwrap_or_else(|e| e.into_inner());
    if queue.len() >= MEDIA_QUEUE_CAP {
        return false;
    }
    queue.push_back(key);
    true
}

fn pop_front(queue: &Mutex<VecDeque<MediaKey>>) -> Option<MediaKey> {
    queue.lock().unwrap_or_else(|e| e.into_inner()).pop_front()
}

/// Called on the message-pump thread when a hot key fires. Returns whether the
/// press was queued.
#[cfg(windows)]
pub(crate) fn push_media_key(key: MediaKey) -> bool {
    push_bounded(&MEDIA_QUEUE, key)
}

/// Next pending media key press, oldest first. Drain it in the UI update loop
/// (`while let Some(key) = poll_media_key()`): it is one uncontended lock, and the
/// tray thread requests a repaint on every key press, so no timer is needed.
/// Always `None` where media keys are not supported.
pub fn poll_media_key() -> Option<MediaKey> {
    pop_front(&MEDIA_QUEUE)
}

/// Which keys this player currently holds, in `MediaKey::ALL` order
/// (play/pause, next, previous, stop). A key another program owns stays `false`;
/// that is not an error.
pub fn media_keys_held() -> [bool; 4] {
    std::array::from_fn(|i| MEDIA_KEYS_HELD[i].load(Ordering::Acquire))
}

/// Ask for the keys to be registered (`true`) or released (`false`). The work is
/// done on the tray's message thread; this only records the wish and posts a
/// message, so it is cheap and never blocks. Calling it with the value already
/// set does nothing, so a key another program owns is retried only when the
/// state is toggled off and on again. Needs the tray backend to be running (its
/// hidden window owns the hot keys); if it is not yet, the wish is applied when
/// the window appears. No-op where media keys are not supported.
pub fn set_media_keys_enabled(enabled: bool) {
    #[cfg(windows)]
    windows_impl::set_media_keys_enabled(enabled);
    #[cfg(not(windows))]
    let _ = enabled;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyAction {
    Register,
    Unregister,
    Keep,
}

/// What to do with one key given the wish and whether it is held now. Never
/// registers while the wish is off.
fn key_action(wanted: bool, held: bool) -> KeyAction {
    match (wanted, held) {
        (true, false) => KeyAction::Register,
        (false, true) => KeyAction::Unregister,
        _ => KeyAction::Keep,
    }
}

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------

#[cfg(windows)]
pub(crate) use windows_impl::{
    MEDIA_KEYS_SYNC_MESSAGE, handle_hotkey, media_window_closing, media_window_gone,
    media_window_ready, sync_media_keys,
};

#[cfg(windows)]
mod windows_impl {
    use std::{
        path::Path,
        ptr,
        sync::atomic::{AtomicIsize, Ordering},
    };

    use windows_sys::Win32::{
        Foundation::{HWND, WIN32_ERROR},
        UI::{
            Input::KeyboardAndMouse::{MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey},
            Shell::{
                SHCNE_ASSOCCHANGED, SHCNF_FLUSHNOWAIT, SHCNF_IDLIST, SHChangeNotify, ShellExecuteW,
            },
            WindowsAndMessaging::{
                ASFW_ANY, AllowSetForegroundWindow, PostMessageW, SW_SHOWNORMAL, WM_USER,
            },
        },
    };

    use super::{
        APPLICATION_KEY_VALUE, ASSOCIATION_EXTENSIONS, FRIENDLY_NAME, KeyAction, MEDIA_KEYS_HELD,
        MEDIA_KEYS_WANTED, MediaKey, PROG_ID, PROG_ID_DESCRIPTION, application_key,
        default_icon_value, exe_file_name, extension_progids_key, hotkey_id, is_valid_key_name,
        key_action, media_key_for_hotkey_id, open_command, prog_id_command_key, prog_id_key,
        push_media_key,
    };

    // Compile-time proof that the literal key codes in `MediaKey::virtual_key` match
    // the Windows definitions.
    const _: () = {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            VK_MEDIA_NEXT_TRACK, VK_MEDIA_PLAY_PAUSE, VK_MEDIA_PREV_TRACK, VK_MEDIA_STOP,
        };
        assert!(VK_MEDIA_PLAY_PAUSE as u32 == 0xB3);
        assert!(VK_MEDIA_NEXT_TRACK as u32 == 0xB0);
        assert!(VK_MEDIA_PREV_TRACK as u32 == 0xB1);
        assert!(VK_MEDIA_STOP as u32 == 0xB2);
    };

    /// Posted to the tray window to apply the current media-key wish.
    pub(crate) const MEDIA_KEYS_SYNC_MESSAGE: u32 = WM_USER + 0x233;

    /// The tray's hidden message window (as an integer), 0 while there is none.
    static MEDIA_WINDOW: AtomicIsize = AtomicIsize::new(0);

    // ----- media keys (everything except `set_media_keys_enabled` runs on the tray thread)

    pub(super) fn set_media_keys_enabled(enabled: bool) {
        if MEDIA_KEYS_WANTED.swap(enabled, Ordering::AcqRel) == enabled {
            return;
        }
        let hwnd = MEDIA_WINDOW.load(Ordering::Acquire);
        if hwnd != 0 {
            // SAFETY: posting to a stale or destroyed window handle just fails.
            unsafe {
                PostMessageW(hwnd as HWND, MEDIA_KEYS_SYNC_MESSAGE, 0, 0);
            }
        }
    }

    /// The tray window now exists: remember it and apply the current wish.
    pub(crate) fn media_window_ready(hwnd: HWND) {
        if hwnd.is_null() {
            return;
        }
        MEDIA_WINDOW.store(hwnd as isize, Ordering::Release);
        sync_media_keys(hwnd);
    }

    /// Register or release each key according to the wish. A key that cannot be
    /// registered (another program owns it, `ERROR_HOTKEY_ALREADY_REGISTERED`) is
    /// simply not held. Must run on the thread that owns `hwnd`.
    pub(crate) fn sync_media_keys(hwnd: HWND) {
        if hwnd.is_null() {
            return;
        }
        let wanted = MEDIA_KEYS_WANTED.load(Ordering::Acquire);
        for key in MediaKey::ALL {
            let held = &MEDIA_KEYS_HELD[key.index()];
            match key_action(wanted, held.load(Ordering::Acquire)) {
                KeyAction::Register => {
                    // SAFETY: hwnd is the live window of the calling thread; MOD_NOREPEAT
                    // keeps a held-down key from flooding the queue.
                    let ok = unsafe {
                        RegisterHotKey(hwnd, hotkey_id(key) as i32, MOD_NOREPEAT, key.virtual_key())
                    } != 0;
                    held.store(ok, Ordering::Release);
                }
                KeyAction::Unregister => {
                    // SAFETY: same window and id that registered it.
                    unsafe {
                        UnregisterHotKey(hwnd, hotkey_id(key) as i32);
                    }
                    held.store(false, Ordering::Release);
                }
                KeyAction::Keep => {}
            }
        }
    }

    /// `WM_HOTKEY` arrived with this id: queue the matching key. Returns whether
    /// the id was one of ours.
    pub(crate) fn handle_hotkey(id: usize) -> bool {
        match media_key_for_hotkey_id(id) {
            Some(key) => {
                push_media_key(key);
                true
            }
            None => false,
        }
    }

    /// The window is about to be destroyed: release every key and forget it.
    pub(crate) fn media_window_closing(hwnd: HWND) {
        if MEDIA_WINDOW
            .compare_exchange(hwnd as isize, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        for key in MediaKey::ALL {
            let held = &MEDIA_KEYS_HELD[key.index()];
            if held.swap(false, Ordering::AcqRel) {
                // SAFETY: same window and id that registered it.
                unsafe {
                    UnregisterHotKey(hwnd, hotkey_id(key) as i32);
                }
            }
        }
    }

    /// The tray thread is over (also after a panic): nothing is held any more
    /// (Windows drops a destroyed window's hot keys) and the handle is invalid.
    pub(crate) fn media_window_gone() {
        MEDIA_WINDOW.store(0, Ordering::Release);
        for held in &MEDIA_KEYS_HELD {
            held.store(false, Ordering::Release);
        }
    }

    // ----- file associations

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    fn io_message(code: WIN32_ERROR) -> String {
        std::io::Error::from_raw_os_error(code as i32).to_string()
    }

    pub(super) fn register(exe: &Path) -> Result<(), String> {
        if !exe.is_absolute() {
            return Err("The program path must be absolute.".into());
        }
        // Validate everything before the first registry write.
        let command = open_command(exe)?;
        let icon = default_icon_value(exe)?;
        let exe_name = exe_file_name(exe)?;
        let result = write_registration(&command, &icon, &exe_name);
        if result.is_err() {
            // Leave no half-finished registration behind.
            let _ = remove_registration(Some(&exe_name));
        }
        notify_shell();
        result
    }

    pub(super) fn unregister() -> Result<(), String> {
        let fallback = std::env::current_exe()
            .ok()
            .and_then(|exe| exe_file_name(&exe).ok());
        let result = remove_registration(fallback.as_deref());
        notify_shell();
        result
    }

    fn write_registration(command: &str, icon: &str, exe_name: &str) -> Result<(), String> {
        let prog = prog_id_key();
        let key = reg::create(&prog)?;
        key.set_string("", PROG_ID_DESCRIPTION)?;
        key.set_string(APPLICATION_KEY_VALUE, exe_name)?;
        drop(key);
        reg::create(&format!(r"{prog}\DefaultIcon"))?.set_string("", icon)?;
        reg::create(&format!(r"{prog}\shell\open"))?
            .set_string("FriendlyAppName", FRIENDLY_NAME)?;
        reg::create(&prog_id_command_key())?.set_string("", command)?;

        // Lets "Open with > Choose another app" list the program by its file name.
        let application = application_key(exe_name);
        reg::create(&application)?.set_string("FriendlyAppName", FRIENDLY_NAME)?;
        reg::create(&format!(r"{application}\shell\open\command"))?.set_string("", command)?;
        let supported = reg::create(&format!(r"{application}\SupportedTypes"))?;
        for extension in ASSOCIATION_EXTENSIONS {
            supported.set_string(&format!(".{extension}"), "")?;
        }
        drop(supported);

        // Add to the per-extension "Open with" list; the default app is not touched.
        for extension in ASSOCIATION_EXTENSIONS {
            reg::create(&extension_progids_key(extension))?.set_empty(PROG_ID)?;
        }
        Ok(())
    }

    /// Remove what `write_registration` created, in an order that keeps the record
    /// of the `Applications` key (inside the ProgID tree) until last. Keeps going
    /// after an error and returns the first one.
    fn remove_registration(fallback_exe_name: Option<&str>) -> Result<(), String> {
        let mut first_error: Option<String> = None;
        let mut note = |result: Result<(), String>| {
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        };
        let prog = prog_id_key();
        let recorded = reg::open(&prog, reg::READ_VALUE)
            .and_then(|key| key.get_string(APPLICATION_KEY_VALUE))
            .filter(|name| is_valid_key_name(name));
        let application_name = recorded.or_else(|| fallback_exe_name.map(str::to_owned));
        if let Some(name) = application_name {
            let application = application_key(&name);
            // Only delete a key we wrote: another program may use the same file name.
            let ours = reg::open(&application, reg::READ_VALUE)
                .and_then(|key| key.get_string("FriendlyAppName"))
                .is_some_and(|value| value == FRIENDLY_NAME);
            if ours {
                note(reg::delete_tree(&application));
            }
        }
        for extension in ASSOCIATION_EXTENSIONS {
            if let Some(key) = reg::open(&extension_progids_key(extension), reg::WRITE_VALUE) {
                note(key.delete_value(PROG_ID));
            }
        }
        note(reg::delete_tree(&prog));
        first_error.map_or(Ok(()), Err)
    }

    pub(super) fn read_registered_command() -> Option<String> {
        reg::open(&prog_id_command_key(), reg::READ_VALUE)?.get_string("")
    }

    fn notify_shell() {
        // SAFETY: SHCNE_ASSOCCHANGED takes no item pointers. FLUSHNOWAIT returns
        // before Explorer has finished, so a busy shell cannot stall the caller.
        unsafe {
            SHChangeNotify(
                SHCNE_ASSOCCHANGED as i32,
                SHCNF_IDLIST | SHCNF_FLUSHNOWAIT,
                ptr::null(),
                ptr::null(),
            );
        }
    }

    pub(super) fn open_default_apps() -> bool {
        let operation = wide("open");
        let target = wide("ms-settings:defaultapps");
        // SAFETY: both strings are NUL-terminated and outlive the call; no window owner.
        let result = unsafe {
            ShellExecuteW(
                ptr::null_mut(),
                operation.as_ptr(),
                target.as_ptr(),
                ptr::null(),
                ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // ShellExecuteW reports success with a value above 32.
        (result as isize) > 32
    }

    pub(super) fn allow_foreground() {
        // SAFETY: plain call; ASFW_ANY lets any process take the foreground once.
        unsafe {
            AllowSetForegroundWindow(ASFW_ANY);
        }
    }

    /// Thin, bounded wrapper over the per-user registry. Every handle is closed on
    /// drop; every string is NUL-terminated UTF-16 with its length checked.
    mod reg {
        use std::ptr;

        use windows_sys::Win32::{
            Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS},
            System::Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WRITE, REG_EXPAND_SZ,
                REG_NONE, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_SZ, RegCloseKey,
                RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
                RegSetValueExW,
            },
        };

        use super::{io_message, wide};

        pub const READ_VALUE: REG_SAM_FLAGS = KEY_QUERY_VALUE;
        pub const WRITE_VALUE: REG_SAM_FLAGS = KEY_SET_VALUE;
        /// Registry string values are at most 32 767 characters.
        const MAX_VALUE_CHARS: usize = 32_767;
        /// Largest value read back (bytes).
        const MAX_READ_BYTES: usize = 2 * (MAX_VALUE_CHARS + 1);

        fn checked(text: &str) -> Result<Vec<u16>, String> {
            if text.contains('\0') {
                return Err("A registry name contains a NUL character.".into());
            }
            Ok(wide(text))
        }

        pub struct Key(HKEY);

        impl Drop for Key {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    // SAFETY: the handle came from RegCreateKeyExW/RegOpenKeyExW and is closed once.
                    unsafe {
                        RegCloseKey(self.0);
                    }
                }
            }
        }

        /// Open or create `HKCU\<path>` for writing (creates missing parents).
        pub fn create(path: &str) -> Result<Key, String> {
            let path_w = checked(path)?;
            let mut handle: HKEY = ptr::null_mut();
            // SAFETY: path_w is NUL-terminated and alive for the call; handle is a valid out pointer.
            let status = unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    path_w.as_ptr(),
                    0,
                    ptr::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_WRITE,
                    ptr::null(),
                    &mut handle,
                    ptr::null_mut(),
                )
            };
            if status != ERROR_SUCCESS || handle.is_null() {
                return Err(format!(
                    "Could not create HKCU\\{path}: {}",
                    io_message(status)
                ));
            }
            Ok(Key(handle))
        }

        /// Open `HKCU\<path>` if it exists.
        pub fn open(path: &str, access: REG_SAM_FLAGS) -> Option<Key> {
            let path_w = checked(path).ok()?;
            let mut handle: HKEY = ptr::null_mut();
            // SAFETY: path_w is NUL-terminated and alive for the call; handle is a valid out pointer.
            let status = unsafe {
                RegOpenKeyExW(HKEY_CURRENT_USER, path_w.as_ptr(), 0, access, &mut handle)
            };
            (status == ERROR_SUCCESS && !handle.is_null()).then_some(Key(handle))
        }

        /// Delete `HKCU\<path>` and everything below it. A missing key is success.
        pub fn delete_tree(path: &str) -> Result<(), String> {
            let path_w = checked(path)?;
            // SAFETY: path_w is NUL-terminated and alive for the call.
            let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path_w.as_ptr()) };
            if status == ERROR_SUCCESS
                || status == ERROR_FILE_NOT_FOUND
                || status == ERROR_PATH_NOT_FOUND
            {
                Ok(())
            } else {
                Err(format!(
                    "Could not remove HKCU\\{path}: {}",
                    io_message(status)
                ))
            }
        }

        impl Key {
            /// Set a string value; `name` "" is the key's default value.
            pub fn set_string(&self, name: &str, value: &str) -> Result<(), String> {
                if value.encode_utf16().count() > MAX_VALUE_CHARS {
                    return Err("A registry value is too long.".into());
                }
                let name_w = checked(name)?;
                let data = checked(value)?;
                let bytes = u32::try_from(data.len() * 2)
                    .map_err(|_| "A registry value is too long.".to_string())?;
                // SAFETY: data holds `bytes` bytes (NUL included) and name_w is NUL-terminated.
                let status = unsafe {
                    RegSetValueExW(
                        self.0,
                        name_w.as_ptr(),
                        0,
                        REG_SZ,
                        data.as_ptr().cast::<u8>(),
                        bytes,
                    )
                };
                if status == ERROR_SUCCESS {
                    Ok(())
                } else {
                    Err(format!(
                        "Could not write registry value {name:?}: {}",
                        io_message(status)
                    ))
                }
            }

            /// Set an empty value of type REG_NONE (the form `OpenWithProgids` uses).
            pub fn set_empty(&self, name: &str) -> Result<(), String> {
                let name_w = checked(name)?;
                // SAFETY: a null pointer with size 0 is a valid empty value.
                let status =
                    unsafe { RegSetValueExW(self.0, name_w.as_ptr(), 0, REG_NONE, ptr::null(), 0) };
                if status == ERROR_SUCCESS {
                    Ok(())
                } else {
                    Err(format!(
                        "Could not write registry value {name:?}: {}",
                        io_message(status)
                    ))
                }
            }

            /// Delete one value. A missing value is success.
            pub fn delete_value(&self, name: &str) -> Result<(), String> {
                let name_w = checked(name)?;
                // SAFETY: name_w is NUL-terminated and alive for the call.
                let status = unsafe { RegDeleteValueW(self.0, name_w.as_ptr()) };
                if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
                    Ok(())
                } else {
                    Err(format!(
                        "Could not remove registry value {name:?}: {}",
                        io_message(status)
                    ))
                }
            }

            /// Read a string value (REG_SZ or REG_EXPAND_SZ, not expanded); `None`
            /// if missing, of another type, larger than the bound, or not valid text.
            pub fn get_string(&self, name: &str) -> Option<String> {
                let name_w = checked(name).ok()?;
                let mut kind = 0u32;
                let mut size = 0u32;
                // SAFETY: a null data pointer asks only for the type and size.
                let status = unsafe {
                    RegQueryValueExW(
                        self.0,
                        name_w.as_ptr(),
                        ptr::null(),
                        &mut kind,
                        ptr::null_mut(),
                        &mut size,
                    )
                };
                if status != ERROR_SUCCESS
                    || (kind != REG_SZ && kind != REG_EXPAND_SZ)
                    || size as usize > MAX_READ_BYTES
                {
                    return None;
                }
                // Room for the data plus a terminator the registry may omit.
                let mut buffer = vec![0u16; size as usize / 2 + 2];
                let mut capacity = u32::try_from(buffer.len() * 2).ok()?;
                // SAFETY: buffer is valid for `capacity` bytes; the call writes at most that.
                let status = unsafe {
                    RegQueryValueExW(
                        self.0,
                        name_w.as_ptr(),
                        ptr::null(),
                        &mut kind,
                        buffer.as_mut_ptr().cast::<u8>(),
                        &mut capacity,
                    )
                };
                if status != ERROR_SUCCESS {
                    return None;
                }
                let units = (capacity as usize / 2).min(buffer.len());
                let units = &buffer[..units];
                let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
                String::from_utf16(&units[..end]).ok()
            }
        }
    }
}
