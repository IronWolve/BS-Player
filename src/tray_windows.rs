use std::{
    mem::size_of,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
        mpsc::{Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
};

use eframe::egui;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, POINT, RECT, WPARAM},
    Graphics::Gdi::{
        GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO,
        MonitorFromPoint,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Shell::{
            NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
            NIM_SETVERSION, NIN_SELECT, NINF_KEY, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
            NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect, Shell_NotifyIconW,
        },
        WindowsAndMessaging::{
            CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
            GWLP_USERDATA, GetCursorPos, GetMessageW, GetSystemMetrics, IMAGE_ICON, KillTimer,
            LR_DEFAULTSIZE, LR_SHARED, LoadImageW, MSG, PM_NOREMOVE, PeekMessageW, PostMessageW,
            PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON,
            SetTimer, SetWindowLongPtrW, TranslateMessage, UnregisterClassW, WM_CLOSE,
            WM_CONTEXTMENU, WM_DESTROY, WM_HOTKEY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NCCREATE,
            WM_RBUTTONUP, WM_TIMER, WM_USER, WNDCLASSW, WS_OVERLAPPED,
        },
    },
};

use super::{HostProbe, TrayCommand, TrayEvent, emit, fallback_anchor, fit_tooltip};
use crate::winintegration;

const ICON_ID: u32 = 1;
// This SDK macro is not exported as a constant by windows-sys.
const NIN_KEYSELECT: u32 = NIN_SELECT | NINF_KEY;
const CALLBACK_MESSAGE: u32 = WM_USER + 0x231;
const THREAD_WAKE_MESSAGE: u32 = WM_USER + 0x232;
const ICON_RESOURCE_ID: usize = 1;
const HOST_CHECK_TIMER: usize = 1;
static CLASS_NAME: &[u16] = &[66, 83, 80, 108, 97, 121, 101, 114, 84, 114, 97, 121, 0];

struct State {
    ctx: egui::Context,
    events: Sender<TrayEvent>,
    hwnd: HWND,
    icon: isize,
    taskbar_created: u32,
    tooltip: String,
    available: bool,
    availability_reported: bool,
    availability_reason: String,
    host_probe: HostProbe,
}

pub(super) fn spawn(
    ctx: egui::Context,
    events: Sender<TrayEvent>,
    commands: Receiver<TrayCommand>,
) -> (Option<JoinHandle<()>>, Box<dyn Fn() + Send + Sync>) {
    let thread_id = Arc::new(AtomicU32::new(0));
    let thread_id_for_wake = Arc::clone(&thread_id);
    let wake = Box::new(move || {
        // SAFETY: the worker publishes its thread ID only after creating its Win32 message queue.
        unsafe {
            let id = thread_id_for_wake.load(Ordering::Acquire);
            if id != 0 {
                windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    id,
                    THREAD_WAKE_MESSAGE,
                    0,
                    0,
                );
            }
        }
    });

    let worker_thread_id = Arc::clone(&thread_id);
    let fail_ctx = ctx.clone();
    let fail_events = events.clone();
    let panic_ctx = ctx.clone();
    let panic_events = events.clone();
    let worker = match thread::Builder::new()
        .name("bs-player-tray-windows".into())
        .spawn(move || {
            let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(ctx, events, commands, worker_thread_id)
            }))
            .is_err();
            // Whatever way the pump ended, no media key is held and no window remains.
            winintegration::media_window_gone();
            if panicked {
                emit(
                    &panic_ctx,
                    &panic_events,
                    TrayEvent::AvailabilityChanged {
                        available: false,
                        reason: "Windows tray worker stopped unexpectedly.".into(),
                    },
                );
            }
        }) {
        Ok(worker) => worker,
        Err(error) => {
            emit(
                &fail_ctx,
                &fail_events,
                TrayEvent::AvailabilityChanged {
                    available: false,
                    reason: format!("Could not start the Windows tray worker: {error}"),
                },
            );
            return (None, Box::new(|| {}));
        }
    };
    (Some(worker), wake)
}

fn run(
    ctx: egui::Context,
    events: Sender<TrayEvent>,
    commands: Receiver<TrayCommand>,
    shared_thread_id: Arc<AtomicU32>,
) {
    // SAFETY: this thread creates and owns the window, message queue, and notification icon handles used here.
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        let mut state = Box::new(State {
            ctx,
            events,
            hwnd: ptr::null_mut(),
            icon: 0,
            taskbar_created: 0,
            tooltip: "BS Player".into(),
            available: false,
            availability_reported: false,
            availability_reason: String::new(),
            host_probe: HostProbe::default(),
        });
        let instance = GetModuleHandleW(ptr::null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: CLASS_NAME.as_ptr(),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            emit_unavailable(&state, "Could not register the Windows tray window class.");
            return;
        }
        state.taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        // The shell draws the tray icon at the small-icon size: ask for that
        // instead of the 32 px large-icon default so the 16 px art is used.
        let (small_width, small_height) =
            (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        let (icon_width, icon_height, size_flag) = if small_width > 0 && small_height > 0 {
            (small_width, small_height, 0)
        } else {
            (0, 0, LR_DEFAULTSIZE)
        };
        let tray_icon = LoadImageW(
            instance,
            ICON_RESOURCE_ID as *const u16,
            IMAGE_ICON,
            icon_width,
            icon_height,
            size_flag | LR_SHARED,
        );
        if tray_icon.is_null() {
            emit_unavailable(
                &state,
                "The embedded BS Player tray icon could not be loaded.",
            );
            UnregisterClassW(CLASS_NAME.as_ptr(), instance);
            return;
        }
        state.icon = tray_icon as isize;
        let state_ptr = (&mut *state) as *mut State as *mut core::ffi::c_void;
        let window = CreateWindowExW(
            0,
            CLASS_NAME.as_ptr(),
            wide("BS Player tray").as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            instance,
            state_ptr,
        );
        if window.is_null() {
            emit_unavailable(&state, "Could not create the Windows tray message window.");
            UnregisterClassW(CLASS_NAME.as_ptr(), instance);
            return;
        }
        state.hwnd = window;
        if SetTimer(window, HOST_CHECK_TIMER, 2_000, None) == 0 {
            emit_state(
                &mut state,
                false,
                "Could not monitor Windows tray host availability.",
            );
            cleanup_window(&mut state);
            UnregisterClassW(CLASS_NAME.as_ptr(), instance);
            return;
        }
        let thread_id = windows_sys::Win32::System::Threading::GetCurrentThreadId();
        // Force creation of this thread's queue before exposing its id to senders.
        PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_NOREMOVE);
        shared_thread_id.store(thread_id, Ordering::Release);
        let _ = install_icon(&mut state);
        // Media keys are registered on this thread, which owns the window.
        winintegration::media_window_ready(window);
        drain_commands(&mut state, &commands);

        while GetMessageW(&mut msg, ptr::null_mut(), 0, 0) > 0 {
            if msg.hwnd.is_null() && msg.message == THREAD_WAKE_MESSAGE {
                drain_commands(&mut state, &commands);
            } else {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        cleanup_window(&mut state);
        shared_thread_id.store(0, Ordering::Release);
        UnregisterClassW(CLASS_NAME.as_ptr(), instance);
    }
}

unsafe fn drain_commands(state: &mut State, commands: &Receiver<TrayCommand>) {
    // SAFETY: state and its HWND are owned by this message-loop thread.
    unsafe {
        loop {
            match commands.try_recv() {
                Ok(TrayCommand::UpdateTooltip(tooltip)) => {
                    state.tooltip = tooltip;
                    if state.available {
                        let data = icon_data(state);
                        if Shell_NotifyIconW(NIM_MODIFY, &data) == 0 {
                            emit_state(state, false, "Windows could not update the tray icon.");
                        }
                    }
                }
                Ok(TrayCommand::Shutdown) => {
                    if !state.hwnd.is_null() {
                        if PostMessageW(state.hwnd, WM_CLOSE, 0, 0) == 0 {
                            PostQuitMessage(0);
                        }
                    }
                    break;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }
}

unsafe fn install_icon(state: &mut State) -> bool {
    // SAFETY: state contains the live HWND and embedded icon handle used by Shell_NotifyIconW.
    unsafe {
        let mut data = icon_data(state);
        if Shell_NotifyIconW(NIM_ADD, &data) == 0 && !icon_exists(state) {
            emit_state(state, false, "Windows rejected the system tray icon.");
            return false;
        }
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        if Shell_NotifyIconW(NIM_SETVERSION, &data) == 0 {
            // Our callback parser requires V4. Never allow hide-to-tray behind
            // an icon whose activation messages we cannot interpret.
            Shell_NotifyIconW(NIM_DELETE, &data);
            emit_state(state, false, "Windows rejected the tray callback version.");
            return false;
        }
        emit_state(state, true, "");
        true
    }
}

fn icon_data(state: &State) -> NOTIFYICONDATAW {
    // SAFETY: NOTIFYICONDATAW is a plain C data struct; zero is valid before required fields are populated.
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = state.hwnd;
    data.uID = ICON_ID;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    data.uCallbackMessage = CALLBACK_MESSAGE;
    data.hIcon = state.icon as _;
    let encoded = tooltip_utf16(&fit_tooltip(&state.tooltip, 127), 127);
    let count = encoded.len().min(data.szTip.len() - 1);
    data.szTip[..count].copy_from_slice(&encoded[..count]);
    data
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    // SAFETY: Windows supplies the HWND/message arguments; GWLP_USERDATA points to State only while its window is alive.
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            return 1;
        }
        let state_ptr =
            windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(hwnd, GWLP_USERDATA)
                as *mut State;
        if state_ptr.is_null() {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        if message == CALLBACK_MESSAGE {
            let callback = lparam as u32;
            let callback_icon = callback >> 16;
            if callback_icon != ICON_ID {
                return 0;
            }
            match callback & 0xffff {
                NIN_SELECT | NIN_KEYSELECT | WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    let state = &mut *state_ptr;
                    emit(&state.ctx, &state.events, TrayEvent::Restore);
                }
                WM_CONTEXTMENU | WM_RBUTTONUP => {
                    let state = &mut *state_ptr;
                    let mut point: POINT = std::mem::zeroed();
                    // Anchor to the actual notification icon, including when
                    // the user invokes its menu from the keyboard.
                    let located = if let Some(rect) = icon_rect(state)
                        .filter(|rect| rect.right > rect.left && rect.bottom > rect.top)
                    {
                        point.x = rect.right;
                        point.y = rect.top;
                        true
                    } else {
                        GetCursorPos(&mut point) != 0
                    };
                    if located {
                        emit(
                            &state.ctx,
                            &state.events,
                            TrayEvent::OpenMenu {
                                position: [point.x as f32, point.y as f32],
                                work_area: monitor_work_area(point, 1.0),
                            },
                        );
                    } else {
                        // Neither the icon rectangle nor the cursor is known:
                        // open at the primary work area's bottom-right corner
                        // rather than at (0, 0), which is off-screen.
                        let (position, work_area) = fallback_anchor(
                            primary_work_area(),
                            // Physical pixels: room for a popup at up to 2x scale.
                            [640.0, 1040.0],
                        );
                        emit(
                            &state.ctx,
                            &state.events,
                            TrayEvent::OpenMenu {
                                position,
                                work_area,
                            },
                        );
                    }
                }
                _ => {}
            }
            return 0;
        }
        if message == WM_CLOSE {
            {
                let state = &mut *state_ptr;
                KillTimer(hwnd, HOST_CHECK_TIMER);
                remove_icon(state);
                winintegration::media_window_closing(hwnd);
            }
            DestroyWindow(hwnd);
            return 0;
        }
        if message == WM_DESTROY {
            {
                let state = &mut *state_ptr;
                KillTimer(hwnd, HOST_CHECK_TIMER);
                remove_icon(state);
                winintegration::media_window_closing(hwnd);
                state.hwnd = ptr::null_mut();
            }
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            PostQuitMessage(0);
            return 0;
        }
        let state = &mut *state_ptr;
        if message == state.taskbar_created {
            state.host_probe.reset();
            let _ = install_icon(state);
            return 0;
        }
        if message == WM_TIMER && wparam == HOST_CHECK_TIMER {
            if !state.available {
                state.host_probe.reset();
                let _ = install_icon(state);
            } else if state.host_probe.host_lost(icon_exists(state)) {
                // Only after consecutive failed probes: one transient failure
                // must not dismiss the popup or restore a hidden player.
                emit_state(
                    state,
                    false,
                    "Windows system tray host is unavailable; waiting for it to return.",
                );
            }
            return 0;
        }
        if message == winintegration::MEDIA_KEYS_SYNC_MESSAGE {
            winintegration::sync_media_keys(hwnd);
            return 0;
        }
        if message == WM_HOTKEY {
            if winintegration::handle_hotkey(wparam) {
                // Wake the UI so the key is polled even while the player is hidden.
                state.ctx.request_repaint_of(egui::ViewportId::ROOT);
            }
            return 0;
        }
        DefWindowProcW(hwnd, message, wparam, lparam)
    }
}

unsafe fn cleanup_window(state: &mut State) {
    // SAFETY: the HWND and icon belong to State and cleanup runs on their owning thread.
    unsafe {
        if state.hwnd.is_null() {
            return;
        }
        let hwnd = state.hwnd;
        KillTimer(hwnd, HOST_CHECK_TIMER);
        remove_icon(state);
        winintegration::media_window_closing(hwnd);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        state.hwnd = ptr::null_mut();
        DestroyWindow(hwnd);
    }
}

unsafe fn remove_icon(state: &mut State) {
    // SAFETY: the HWND/icon pair is owned by State and remains valid during teardown.
    unsafe {
        if state.icon != 0 && !state.hwnd.is_null() {
            let data = icon_data(state);
            Shell_NotifyIconW(NIM_DELETE, &data);
        }
        state.icon = 0;
        if state.available {
            emit_state(state, false, "System tray icon was removed.");
        }
    }
}

unsafe fn icon_exists(state: &State) -> bool {
    unsafe { icon_rect(state).is_some() }
}

unsafe fn icon_rect(state: &State) -> Option<RECT> {
    // SAFETY: State identifies a live shell notification icon for this HWND.
    unsafe {
        let mut identifier = NOTIFYICONIDENTIFIER::default();
        identifier.cbSize = size_of::<NOTIFYICONIDENTIFIER>() as u32;
        identifier.hWnd = state.hwnd;
        identifier.uID = ICON_ID;
        let mut rect: RECT = std::mem::zeroed();
        (Shell_NotifyIconGetRect(&identifier, &mut rect) >= 0).then_some(rect)
    }
}

unsafe fn monitor_work_area(point: POINT, scale: f32) -> Option<[f32; 4]> {
    // SAFETY: POINT is a valid OS screen coordinate and the returned monitor handle is used only for this query.
    unsafe { work_area_of(MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST), scale) }
}

unsafe fn primary_work_area() -> Option<[f32; 4]> {
    // SAFETY: the primary monitor's origin is always (0, 0); the handle is used only for this query.
    unsafe {
        work_area_of(
            MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY),
            1.0,
        )
    }
}

unsafe fn work_area_of(monitor: HMONITOR, scale: f32) -> Option<[f32; 4]> {
    // SAFETY: the monitor handle comes from MonitorFromPoint and is used only for this query.
    unsafe {
        if monitor.is_null() {
            return None;
        }
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        Some([
            info.rcWork.left as f32 / scale,
            info.rcWork.top as f32 / scale,
            info.rcWork.right as f32 / scale,
            info.rcWork.bottom as f32 / scale,
        ])
    }
}

fn emit_state(state: &mut State, available: bool, reason: &str) {
    if state.availability_reported
        && state.available == available
        && state.availability_reason == reason
    {
        return;
    }
    state.available = available;
    state.availability_reported = true;
    state.availability_reason = reason.to_owned();
    emit(
        &state.ctx,
        &state.events,
        TrayEvent::AvailabilityChanged {
            available,
            reason: reason.to_owned(),
        },
    );
}

fn emit_unavailable(state: &State, reason: &str) {
    emit(
        &state.ctx,
        &state.events,
        TrayEvent::AvailabilityChanged {
            available: false,
            reason: reason.to_owned(),
        },
    );
}

fn tooltip_utf16(text: &str, limit: usize) -> Vec<u16> {
    let mut result = Vec::with_capacity(limit);
    for character in text.chars() {
        let mut encoded = [0u16; 2];
        let units = character.encode_utf16(&mut encoded);
        if result.len() + units.len() > limit {
            break;
        }
        result.extend_from_slice(units);
    }
    result
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
