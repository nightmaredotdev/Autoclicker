//! Test and measurement helpers (compiled for tests or with the
//! `diagnostics` feature; never part of the application).
//!
//! * [`MouseHookRecorder`] observes this application's injected mouse events
//!   through a low-level mouse hook and can swallow them, so tests and the
//!   measurement tool never click anything on the user's desktop.
//! * [`TestWindow`] is a window on its own thread that records the mouse and
//!   keyboard messages it receives.

use super::error::Win32Error;
use super::input::INJECTION_SIGNATURE;
use cadence_core::geometry::{Point, Rect};
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, SwapMouseButton,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HHOOK,
    LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostQuitMessage,
    PostThreadMessageW, RegisterClassW, SW_MINIMIZE, SW_SHOWNOACTIVATE, SetForegroundWindow,
    SetWindowsHookExW, ShowWindow, TranslateMessage, UnhookWindowsHookEx, WH_MOUSE_LL, WM_APP,
    WM_DESTROY, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_USER, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_OVERLAPPEDWINDOW, WS_POPUP, WS_VISIBLE, WindowFromPoint,
};

const WM_APP_QUIT: u32 = WM_APP + 2;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------- mouse hook

/// A mouse message observed by the hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseMessage {
    LeftDown,
    LeftUp,
    RightDown,
    RightUp,
    Other(u32),
}

impl MouseMessage {
    fn from_raw(msg: u32) -> Self {
        match msg {
            WM_LBUTTONDOWN => Self::LeftDown,
            WM_LBUTTONUP => Self::LeftUp,
            WM_RBUTTONDOWN => Self::RightDown,
            WM_RBUTTONUP => Self::RightUp,
            other => Self::Other(other),
        }
    }

    pub fn is_down(self) -> bool {
        matches!(self, Self::LeftDown | Self::RightDown)
    }

    pub fn is_up(self) -> bool {
        matches!(self, Self::LeftUp | Self::RightUp)
    }
}

/// One event injected by this application, as seen by the hook.
#[derive(Clone, Copy, Debug)]
pub struct HookEvent {
    /// Time the hook callback ran (QueryPerformanceCounter based).
    pub at: Instant,
    pub message: MouseMessage,
    pub point: Point,
    pub injected_flag: bool,
}

struct HookShared {
    events: Mutex<Vec<HookEvent>>,
    block: bool,
    /// Record (and block) every event, not only this application's.
    all: AtomicBool,
}

static HOOK_STATE: Mutex<Option<Arc<HookShared>>> = Mutex::new(None);

unsafe extern "system" fn mouse_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_MOUSE_LL with code >= 0, lparam points to a valid MSLLHOOKSTRUCT.
        let info = unsafe { *(lparam as *const MSLLHOOKSTRUCT) };
        let shared = HOOK_STATE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(shared) = shared
            && (shared.all.load(Ordering::Relaxed) || info.dwExtraInfo == INJECTION_SIGNATURE)
        {
            shared
                .events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(HookEvent {
                    at: Instant::now(),
                    message: MouseMessage::from_raw(wparam as u32),
                    point: Point::new(info.pt.x, info.pt.y),
                    injected_flag: info.flags & LLMHF_INJECTED != 0,
                });
            if shared.block {
                return 1; // swallow: no application receives this event
            }
        }
    }
    // SAFETY: forwarding the unmodified hook arguments.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Records (and optionally swallows) mouse events carrying
/// [`INJECTION_SIGNATURE`]. Only one recorder may exist at a time.
pub struct MouseHookRecorder {
    shared: Arc<HookShared>,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

struct SendHook(HHOOK);
// SAFETY: the hook handle is only used (unhooked) on the thread that created it;
// it is moved into that thread's closure and never shared.
unsafe impl Send for SendHook {}

impl MouseHookRecorder {
    pub fn start(block: bool) -> Result<Self, Win32Error> {
        Self::start_with(block, false)
    }

    /// Records (and, with `block`, swallows) **every** mouse event, including
    /// the user's own. Hooks installed later run first, so this shows what
    /// they let through.
    pub fn start_all(block: bool) -> Result<Self, Win32Error> {
        Self::start_with(block, true)
    }

    fn start_with(block: bool, all: bool) -> Result<Self, Win32Error> {
        let shared = Arc::new(HookShared {
            events: Mutex::new(Vec::new()),
            block,
            all: AtomicBool::new(all),
        });
        {
            let mut state = HOOK_STATE.lock().unwrap_or_else(PoisonError::into_inner);
            assert!(state.is_none(), "only one MouseHookRecorder at a time");
            *state = Some(shared.clone());
        }
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let mut msg = MSG::default();
            // SAFETY: valid MSG; creates the message queue.
            unsafe {
                PeekMessageW(
                    &mut msg,
                    std::ptr::null_mut(),
                    WM_USER,
                    WM_USER,
                    PM_NOREMOVE,
                )
            };
            // SAFETY: null module name returns this executable's handle.
            let module = unsafe { GetModuleHandleW(std::ptr::null()) };
            // SAFETY: valid hook procedure; global low-level hook (thread id 0).
            let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook_proc), module, 0) };
            // SAFETY: no preconditions.
            let tid = unsafe { GetCurrentThreadId() };
            if hook.is_null() {
                let _ = ready_tx.send(Err(Win32Error::last()));
                return;
            }
            let hook = SendHook(hook);
            let _ = ready_tx.send(Ok(tid));
            // Low-level hooks are called through this thread's message loop.
            loop {
                // SAFETY: valid MSG.
                let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
                if r == 0 || r == -1 || msg.message == WM_APP_QUIT {
                    break;
                }
            }
            // SAFETY: `hook` was installed by this thread and is still installed.
            unsafe { UnhookWindowsHookEx(hook.0) };
        });
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                shared,
                thread_id,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                *HOOK_STATE.lock().unwrap_or_else(PoisonError::into_inner) = None;
                Err(e)
            }
            Err(_) => {
                *HOOK_STATE.lock().unwrap_or_else(PoisonError::into_inner) = None;
                Err(Win32Error {
                    code: 0,
                    message: "hook thread exited".into(),
                })
            }
        }
    }

    /// Switches between recording only this application's events and
    /// recording (and, if blocking, swallowing) every mouse event.
    pub fn set_record_all(&self, all: bool) {
        self.shared.all.store(all, Ordering::Relaxed);
    }

    pub fn events(&self) -> Vec<HookEvent> {
        self.shared
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn clear(&self) {
        self.shared
            .events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Waits until `pred(events)` holds or `timeout` passes.
    pub fn wait_for(&self, timeout: Duration, pred: impl Fn(&[HookEvent]) -> bool) -> bool {
        let end = Instant::now() + timeout;
        loop {
            if pred(&self.events()) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for MouseHookRecorder {
    fn drop(&mut self) {
        // SAFETY: no pointer arguments.
        unsafe { PostThreadMessageW(self.thread_id, WM_APP_QUIT, 0, 0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        *HOOK_STATE.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

// ---------------------------------------------------------------- windows

/// A message received by a [`TestWindow`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowMessage {
    pub message: u32,
    pub wparam: usize,
}

impl WindowMessage {
    pub fn is_key_down(&self, vk: u16) -> bool {
        matches!(self.message, WM_KEYDOWN | WM_SYSKEYDOWN) && self.wparam == usize::from(vk)
    }
    pub fn is_key_up(&self, vk: u16) -> bool {
        matches!(self.message, WM_KEYUP | WM_SYSKEYUP) && self.wparam == usize::from(vk)
    }
    pub fn is_left_down(&self) -> bool {
        self.message == WM_LBUTTONDOWN
    }
    pub fn is_right_down(&self) -> bool {
        self.message == WM_RBUTTONDOWN
    }
}

static WINDOW_LOG: Mutex<Vec<(usize, WindowMessage)>> = Mutex::new(Vec::new());

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_KEYDOWN | WM_KEYUP
        | WM_SYSKEYDOWN | WM_SYSKEYUP => {
            WINDOW_LOG
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((
                    hwnd as usize,
                    WindowMessage {
                        message: msg,
                        wparam,
                    },
                ));
        }
        WM_DESTROY => {
            // SAFETY: no preconditions; ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            return 0;
        }
        _ => {}
    }
    // SAFETY: default processing with the unmodified arguments.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn register_class() -> &'static [u16] {
    static CLASS: OnceLock<Vec<u16>> = OnceLock::new();
    CLASS.get_or_init(|| {
        let name = wide("AutoclickerDiagnosticsWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            // SAFETY: null module name returns this executable's handle.
            hInstance: unsafe { GetModuleHandleW(std::ptr::null()) },
            lpszClassName: name.as_ptr(),
            ..Default::default()
        };
        // SAFETY: `wc` is fully initialized and `name` lives for the program
        // (stored in the OnceLock).
        unsafe { RegisterClassW(&wc) };
        name
    })
}

/// How a [`TestWindow`] is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowKind {
    /// Borderless, always-on-top, not in the taskbar.
    TopmostPopup,
    /// A normal window with a title bar (shown without activating).
    Normal,
}

/// A window on its own message-loop thread.
pub struct TestWindow {
    hwnd: usize,
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl TestWindow {
    pub fn create(rect: Rect, kind: WindowKind, title: &str) -> Result<Self, Win32Error> {
        let title = wide(title);
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let class = register_class();
            let (ex_style, style) = match kind {
                WindowKind::TopmostPopup => {
                    (WS_EX_TOPMOST | WS_EX_TOOLWINDOW, WS_POPUP | WS_VISIBLE)
                }
                WindowKind::Normal => (0, WS_OVERLAPPEDWINDOW),
            };
            // SAFETY: null module name returns this executable's handle.
            let module = unsafe { GetModuleHandleW(std::ptr::null()) };
            // SAFETY: registered class, valid strings, no parent/menu.
            let hwnd = unsafe {
                CreateWindowExW(
                    ex_style,
                    class.as_ptr(),
                    title.as_ptr(),
                    style,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    module,
                    std::ptr::null(),
                )
            };
            // SAFETY: no preconditions.
            let tid = unsafe { GetCurrentThreadId() };
            if hwnd.is_null() {
                let _ = tx.send(Err(Win32Error::last()));
                return;
            }
            if kind == WindowKind::Normal {
                // SAFETY: valid window owned by this thread.
                unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
            }
            let _ = tx.send(Ok((hwnd as usize, tid)));
            let mut msg = MSG::default();
            loop {
                // SAFETY: valid MSG.
                let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
                if r == 0 || r == -1 {
                    break;
                }
                if msg.message == WM_APP_QUIT {
                    // SAFETY: window owned by this thread; WM_DESTROY posts WM_QUIT.
                    unsafe { DestroyWindow(hwnd) };
                    continue;
                }
                // SAFETY: valid MSG retrieved by GetMessageW.
                unsafe { TranslateMessage(&msg) };
                // SAFETY: valid MSG retrieved by GetMessageW.
                unsafe { DispatchMessageW(&msg) };
            }
        });
        match rx.recv() {
            Ok(Ok((hwnd, thread_id))) => Ok(Self {
                hwnd,
                thread_id,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => Err(Win32Error {
                code: 0,
                message: "window thread exited".into(),
            }),
        }
    }

    pub fn id(&self) -> WindowId {
        WindowId(self.hwnd)
    }

    fn hwnd(&self) -> HWND {
        self.hwnd as HWND
    }

    /// Messages received by this window so far.
    pub fn messages(&self) -> Vec<WindowMessage> {
        WINDOW_LOG
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(h, _)| *h == self.hwnd)
            .map(|(_, m)| *m)
            .collect()
    }

    pub fn wait_for(&self, timeout: Duration, pred: impl Fn(&[WindowMessage]) -> bool) -> bool {
        let end = Instant::now() + timeout;
        loop {
            if pred(&self.messages()) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    pub fn minimize(&self) {
        // SAFETY: valid window handle; ShowWindow may be called cross-thread.
        unsafe { ShowWindow(self.hwnd(), SW_MINIMIZE) };
    }

    /// Makes this the foreground window. See [`force_foreground`].
    pub fn bring_to_foreground(&self) -> bool {
        force_foreground(self.id())
    }
}

impl Drop for TestWindow {
    fn drop(&mut self) {
        // SAFETY: no pointer arguments.
        unsafe { PostThreadMessageW(self.thread_id, WM_APP_QUIT, 0, 0) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// An opaque window identity (a window handle value; never dereferenced).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowId(usize);

/// The current foreground window.
pub fn foreground_window() -> WindowId {
    // SAFETY: no preconditions.
    WindowId(unsafe { GetForegroundWindow() } as usize)
}

/// Moves `hwnd` to the foreground by temporarily attaching to the input queue
/// of the current foreground thread (a background process may not otherwise
/// take the foreground). Returns whether it succeeded. Diagnostics only.
pub fn force_foreground(window: WindowId) -> bool {
    let current = foreground_window();
    if current == window || window.0 == 0 {
        return current == window;
    }
    let hwnd = window.0 as HWND;
    // SAFETY: GetWindowThreadProcessId accepts any handle and a null process-id pointer.
    let fg_thread = unsafe { GetWindowThreadProcessId(current.0 as HWND, std::ptr::null_mut()) };
    // SAFETY: no preconditions.
    let me = unsafe { GetCurrentThreadId() };
    let attached = fg_thread != 0
        && fg_thread != me
        // SAFETY: both are live thread ids.
        && unsafe { AttachThreadInput(me, fg_thread, 1) } != 0;
    // SAFETY: the functions validate the handle; a stale one just fails.
    unsafe { BringWindowToTop(hwnd) };
    // SAFETY: as above.
    unsafe { SetForegroundWindow(hwnd) };
    if attached {
        // SAFETY: detaching the attachment made above.
        unsafe { AttachThreadInput(me, fg_thread, 0) };
    }
    foreground_window() == window
}

/// The top-level or child window under a screen point (as a [`WindowId`]).
pub fn window_at(p: Point) -> WindowId {
    // SAFETY: no pointer arguments.
    let hwnd = unsafe { WindowFromPoint(POINT { x: p.x, y: p.y }) };
    WindowId(hwnd as usize)
}

/// A top-level window, as seen from outside its process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInfo {
    pub id: WindowId,
    pub title: String,
    pub visible: bool,
    /// `WS_EX_TOPMOST` is set (always on top).
    pub topmost: bool,
}

unsafe extern "system" fn collect_window(hwnd: HWND, data: LPARAM) -> windows_sys::core::BOOL {
    // SAFETY: `data` is the `&mut Vec<usize>` passed to EnumWindows.
    let out = unsafe { &mut *(data as *mut Vec<usize>) };
    out.push(hwnd as usize);
    1
}

/// Top-level windows owned by process `pid`.
pub fn windows_of_process(pid: u32) -> Vec<WindowInfo> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GWL_EXSTYLE, GetWindowLongPtrW, GetWindowTextW, IsWindowVisible,
    };
    let mut handles: Vec<usize> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, which outlives the call.
    unsafe {
        EnumWindows(
            Some(collect_window),
            &mut handles as *mut Vec<usize> as LPARAM,
        )
    };
    handles
        .into_iter()
        .filter_map(|h| {
            let hwnd = h as HWND;
            let mut owner = 0u32;
            // SAFETY: valid out-pointer; a stale handle just yields 0.
            unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
            if owner != pid {
                return None;
            }
            let mut buf = [0u16; 256];
            // SAFETY: the buffer pointer and length describe a writable buffer.
            let len = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
            // SAFETY: no pointer arguments.
            let visible = unsafe { IsWindowVisible(hwnd) } != 0;
            // SAFETY: no pointer arguments.
            let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
            Some(WindowInfo {
                id: WindowId(h),
                title: String::from_utf16_lossy(&buf[..len.max(0) as usize]),
                visible,
                topmost: ex_style & WS_EX_TOPMOST != 0,
            })
        })
        .collect()
}

/// Simulates the end of the Windows session for `window`: sends
/// `WM_QUERYENDSESSION`, then `WM_ENDSESSION` (session ending), each with a
/// timeout. Returns the reply to the query (non-zero = may end), or `None`
/// if a message timed out or failed.
pub fn simulate_session_end(window: WindowId, timeout: Duration) -> Option<isize> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_ENDSESSION, WM_QUERYENDSESSION,
    };
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    let mut reply = 0usize;
    // SAFETY: valid out-pointer; the handle is validated by the function.
    let ok = unsafe {
        SendMessageTimeoutW(
            window.0 as HWND,
            WM_QUERYENDSESSION,
            0,
            0,
            SMTO_ABORTIFHUNG,
            ms,
            &mut reply,
        )
    };
    if ok == 0 {
        return None;
    }
    let mut ignored = 0usize;
    // SAFETY: as above; wParam 1 = the session is ending.
    let ok = unsafe {
        SendMessageTimeoutW(
            window.0 as HWND,
            WM_ENDSESSION,
            1,
            0,
            SMTO_ABORTIFHUNG,
            ms,
            &mut ignored,
        )
    };
    (ok != 0).then_some(reply as isize)
}

/// The client area of a window in screen coordinates.
pub fn client_rect(window: WindowId) -> Option<Rect> {
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;
    let hwnd = window.0 as HWND;
    let mut r = RECT::default();
    // SAFETY: valid out-pointer; the handle is validated by the function.
    if unsafe { GetClientRect(hwnd, &mut r) } == 0 {
        return None;
    }
    let mut origin = POINT { x: 0, y: 0 };
    // SAFETY: valid in/out pointer.
    if unsafe { ClientToScreen(hwnd, &mut origin) } == 0 {
        return None;
    }
    Some(Rect::new(
        origin.x,
        origin.y,
        origin.x + r.right,
        origin.y + r.bottom,
    ))
}

/// Moves the cursor to `p` and performs a primary-button click that does
/// *not* carry [`INJECTION_SIGNATURE`], so a [`MouseHookRecorder`] lets it
/// through to the window under the cursor. Callers must make sure that
/// window is their own. Returns the number of events inserted (2).
pub fn click_unmarked(p: Point) -> usize {
    click_unmarked_button(p, false)
}

/// Like [`click_unmarked`], with the secondary (`secondary = true`) or
/// primary logical button: what a user's click looks like to hooks.
pub fn click_unmarked_button(p: Point, secondary: bool) -> usize {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    };
    // SAFETY: no pointer arguments.
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::SetCursorPos(p.x, p.y) };
    let (down, up) = if super::input::buttons_swapped() != secondary {
        (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP)
    } else {
        (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP)
    };
    let mut inputs = [
        super::input::mouse_input(down),
        super::input::mouse_input(up),
    ];
    for i in &mut inputs {
        i.Anonymous.mi.dwExtraInfo = 0;
    }
    // SAFETY: two valid INPUT structures and the matching structure size.
    unsafe { SendInput(2, inputs.as_ptr(), size_of::<INPUT>() as i32) as usize }
}

/// Posts `WM_CLOSE` to a window (what the title bar's close button does).
pub fn post_close(window: WindowId) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    // SAFETY: PostMessageW validates the handle; a stale one just fails.
    unsafe { PostMessageW(window.0 as HWND, WM_CLOSE, 0, 0) != 0 }
}

// ---------------------------------------------------------------- keyboard

/// Injects key transitions (`(virtual key, is_release)`) as one batch, tagged
/// with [`INJECTION_SIGNATURE`]. Returns how many were inserted.
pub fn send_keys(keys: &[(u16, bool)]) -> usize {
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&(vk, up)| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: INJECTION_SIGNATURE,
                },
            },
        })
        .collect();
    // SAFETY: valid INPUT array and matching structure size.
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        ) as usize
    }
}

/// Injects a raw physical button transition *without* swapped-button
/// compensation (tagged with [`INJECTION_SIGNATURE`]). Returns events inserted.
pub fn send_physical_button(left: bool, down: bool) -> usize {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    };
    let flags = match (left, down) {
        (true, true) => MOUSEEVENTF_LEFTDOWN,
        (true, false) => MOUSEEVENTF_LEFTUP,
        (false, true) => MOUSEEVENTF_RIGHTDOWN,
        (false, false) => MOUSEEVENTF_RIGHTUP,
    };
    let input = super::input::mouse_input(flags);
    // SAFETY: one valid INPUT and the matching structure size.
    unsafe { SendInput(1, &input, size_of::<INPUT>() as i32) as usize }
}

// ---------------------------------------------------------------- console control

/// Sends a real `CTRL_C_EVENT` to every process attached to the console of
/// `pid`. **Detaches the calling process from its own console**, so only call
/// this from a dedicated helper process whose output is not a console.
pub fn send_ctrl_c_to_console_of(pid: u32) -> Result<(), Win32Error> {
    use windows_sys::Win32::System::Console::{
        AttachConsole, CTRL_C_EVENT, FreeConsole, GenerateConsoleCtrlEvent, SetConsoleCtrlHandler,
    };
    // SAFETY: no arguments; failing just means there was no console.
    unsafe { FreeConsole() };
    // SAFETY: no pointer arguments.
    if unsafe { AttachConsole(pid) } == 0 {
        return Err(Win32Error::last());
    }
    // This process is now attached too: make it ignore the Ctrl+C it sends.
    // SAFETY: a null handler with TRUE sets the "ignore Ctrl+C" attribute.
    unsafe { SetConsoleCtrlHandler(None, 1) };
    // SAFETY: no pointer arguments; group 0 = all processes on this console.
    let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) };
    let result = if sent == 0 {
        Err(Win32Error::last())
    } else {
        Ok(())
    };
    std::thread::sleep(Duration::from_millis(200)); // let delivery finish
    // SAFETY: no arguments.
    unsafe { FreeConsole() };
    result
}

/// Asks the console window of `pid` to close by posting `WM_CLOSE` to it,
/// which makes the console host send `CTRL_CLOSE_EVENT` to attached
/// processes. **Detaches the calling process from its own console**, so only
/// call this from a dedicated helper process. Returns an error if the console
/// has no window (e.g. a pseudo-console).
pub fn close_console_window_of(pid: u32) -> Result<(), Win32Error> {
    use windows_sys::Win32::System::Console::{AttachConsole, FreeConsole, GetConsoleWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    // SAFETY: no arguments; failing just means there was no console.
    unsafe { FreeConsole() };
    // SAFETY: no pointer arguments.
    if unsafe { AttachConsole(pid) } == 0 {
        return Err(Win32Error::last());
    }
    // SAFETY: no arguments.
    let hwnd = unsafe { GetConsoleWindow() };
    // Detach before closing so this process does not receive the event.
    // SAFETY: no arguments.
    unsafe { FreeConsole() };
    if hwnd.is_null() {
        return Err(Win32Error {
            code: 0,
            message: "the target console has no window".into(),
        });
    }
    // SAFETY: the handle came from GetConsoleWindow; PostMessageW validates it.
    if unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) } == 0 {
        return Err(Win32Error::last());
    }
    Ok(())
}

// ---------------------------------------------------------------- system state

/// Temporarily sets the swapped-mouse-buttons setting; restores it on drop
/// (including unwinding). The setting is not persisted by `SwapMouseButton`.
pub struct SwapButtonsGuard {
    previous: bool,
}

impl SwapButtonsGuard {
    pub fn set(swapped: bool) -> Self {
        // SAFETY: no pointer arguments. Returns whether buttons were swapped before.
        let previous = unsafe { SwapMouseButton(i32::from(swapped)) } != 0;
        Self { previous }
    }
}

impl Drop for SwapButtonsGuard {
    fn drop(&mut self) {
        // SAFETY: no pointer arguments.
        unsafe { SwapMouseButton(i32::from(self.previous)) };
    }
}

unsafe extern "system" fn collect_monitor(
    _monitor: HMONITOR,
    _hdc: HDC,
    rect: *mut RECT,
    data: LPARAM,
) -> windows_sys::core::BOOL {
    // SAFETY: `rect` points to the monitor rectangle, valid during the callback.
    let r = unsafe { *rect };
    // SAFETY: `data` is the `&mut Vec<Rect>` passed to EnumDisplayMonitors,
    // valid and not otherwise borrowed during the enumeration.
    let out = unsafe { &mut *(data as *mut Vec<Rect>) };
    out.push(Rect::new(r.left, r.top, r.right, r.bottom));
    1
}

/// Rectangles of all connected monitors (physical pixels when DPI aware).
pub fn monitors() -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    // SAFETY: the callback only writes into `out`, which outlives the call.
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect_monitor),
            &mut out as *mut Vec<Rect> as LPARAM,
        )
    };
    out
}

/// Human-readable power source, for measurement reports.
pub fn power_source() -> String {
    let mut s = SYSTEM_POWER_STATUS::default();
    // SAFETY: valid, writable structure.
    if unsafe { GetSystemPowerStatus(&mut s) } == 0 {
        return "unknown".into();
    }
    match s.ACLineStatus {
        0 => format!("battery ({}%)", s.BatteryLifePercent),
        1 => "AC power".into(),
        _ => "unknown".into(),
    }
}

/// Current cursor position.
pub fn cursor_position() -> Point {
    let mut p = POINT { x: 0, y: 0 };
    // SAFETY: valid, writable POINT.
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p) };
    Point::new(p.x, p.y)
}
