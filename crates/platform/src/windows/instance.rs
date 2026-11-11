//! Single-instance enforcement and process-level system events.
//!
//! * [`acquire_instance`] uses a named mutex in the per-session `Local\`
//!   namespace as the lock. A second instance finds the mutex already
//!   existing, signals a named auto-reset "activate" event, allows the first
//!   instance to take the foreground, and exits. The event is created (or
//!   opened) *before* the mutex check by both sides, so a signal sent while the
//!   first instance is still starting is latched until it listens. Kernel
//!   objects are released when their last handle closes, including when a
//!   process is terminated, so a crashed instance never blocks a new one.
//! * [`SystemEvents`] runs a thread with a hidden top-level window that waits
//!   for the activate events of the locks it is given (an application may
//!   hold more than one name, e.g. a former product name) and receives
//!   `WM_QUERYENDSESSION`/`WM_ENDSESSION` (GUI processes get no console
//!   events at logoff/shutdown).

use super::error::Win32Error;
use super::handle::OwnedHandle;
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, GetCurrentProcessId, GetCurrentThreadId, INFINITE, SetEvent,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, MSG, MsgWaitForMultipleObjects, PM_NOREMOVE, PM_REMOVE, PeekMessageW,
    PostThreadMessageW, QS_ALLINPUT, RegisterClassW, SW_RESTORE, SetForegroundWindow, ShowWindow,
    TranslateMessage, WM_APP, WM_ENDSESSION, WM_QUERYENDSESSION, WM_QUIT, WM_USER, WNDCLASSW,
    WS_POPUP,
};

use cadence_core::engine::ThreadExit;

/// Title of the hidden window that receives session-end messages.
pub const SYSTEM_EVENTS_WINDOW_TITLE: &str = "Cadence system events";
const WM_APP_QUIT: u32 = WM_APP + 3;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Ownership of the single-instance lock. Released when dropped (or when the
/// process ends).
#[derive(Debug)]
pub struct InstanceLock {
    _mutex: OwnedHandle,
    activate: Arc<OwnedHandle>,
}

/// Result of [`acquire_instance`].
#[derive(Debug)]
pub enum Acquire {
    /// This is the only instance.
    Acquired(InstanceLock),
    /// Another instance holds the lock; it has been asked to show itself.
    AlreadyRunning,
}

/// Acquires the single-instance lock named `name` (letters, digits, `.`, `-`
/// and `_`; scoped to the current logon session).
pub fn acquire_instance(name: &str) -> Result<Acquire, Win32Error> {
    let event_name = wide(&format!("Local\\{name}.Activate"));
    // Auto-reset, initially non-signalled; created or opened.
    // SAFETY: null attributes; valid NUL-terminated name.
    let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, event_name.as_ptr()) };
    let activate = OwnedHandle::new(event).ok_or_else(Win32Error::last)?;

    let mutex_name = wide(&format!("Local\\{name}.Lock"));
    // SAFETY: null attributes; not initially owned; valid NUL-terminated name.
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr()) };
    // SAFETY: no preconditions; read immediately after CreateMutexW.
    let error = unsafe { GetLastError() };
    let mutex = OwnedHandle::new(mutex).ok_or_else(|| Win32Error::from_code(error))?;
    if error == ERROR_ALREADY_EXISTS {
        // Let the running instance come to the foreground, then wake it.
        // SAFETY: no pointer arguments.
        unsafe { AllowSetForegroundWindow(ASFW_ANY) };
        // SAFETY: valid event handle.
        unsafe { SetEvent(activate.raw()) };
        return Ok(Acquire::AlreadyRunning);
    }
    Ok(Acquire::Acquired(InstanceLock {
        _mutex: mutex,
        activate: Arc::new(activate),
    }))
}

thread_local! {
    static ON_SESSION_END: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
}

unsafe extern "system" fn events_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // Do not block logoff/shutdown.
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                // The session is ending: the process is terminated soon after
                // this returns, so shut down synchronously here.
                ON_SESSION_END.with(|cb| {
                    if let Some(cb) = cb.borrow().as_ref() {
                        cb();
                    }
                });
            }
            0
        }
        // SAFETY: default processing with the unmodified arguments.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Owns the system-events thread.
#[derive(Debug)]
pub struct SystemEvents {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
    done: mpsc::Receiver<()>,
}

struct DoneSignal(mpsc::Sender<()>);
impl Drop for DoneSignal {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

impl SystemEvents {
    /// Starts the thread. `on_activate` runs when another instance asks this
    /// one to show itself through the activate event of any of `locks`;
    /// `on_session_end` runs (and must finish shutting down) when the Windows
    /// session ends. Both run on the events thread.
    pub fn start(
        locks: &[&InstanceLock],
        on_activate: impl Fn() + Send + 'static,
        on_session_end: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        if locks.is_empty() || locks.len() >= 64 {
            return Err(std::io::Error::other("between 1 and 63 instance locks"));
        }
        let activate: Vec<Arc<OwnedHandle>> = locks.iter().map(|l| l.activate.clone()).collect();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("system-events".into())
            .spawn(move || {
                let _done = DoneSignal(done_tx);
                events_thread(&activate, &ready_tx, &on_activate, Box::new(on_session_end));
            })?;
        let thread_id = ready_rx
            .recv()
            .map_err(|_| std::io::Error::other("system-events thread exited early"))?
            .map_err(|e: Win32Error| std::io::Error::other(e.to_string()))?;
        Ok(Self {
            thread_id,
            thread: Some(thread),
            done,
        })
    }

    /// Stops and joins the thread, waiting at most `timeout`.
    pub fn shutdown(mut self, timeout: Duration) -> ThreadExit {
        // SAFETY: no pointer arguments; failure means the thread has exited.
        unsafe { PostThreadMessageW(self.thread_id, WM_APP_QUIT, 0, 0) };
        match self.done.recv_timeout(timeout) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
            Err(RecvTimeoutError::Timeout) => return ThreadExit::TimedOut,
        }
        match self.thread.take().map(JoinHandle::join) {
            Some(Err(_)) => ThreadExit::Panicked,
            _ => ThreadExit::Clean,
        }
    }
}

fn events_thread(
    activate: &[Arc<OwnedHandle>],
    ready: &mpsc::Sender<Result<u32, Win32Error>>,
    on_activate: &dyn Fn(),
    on_session_end: Box<dyn Fn()>,
) {
    let mut msg = MSG::default();
    // SAFETY: valid MSG; creates this thread's message queue before anyone posts to it.
    unsafe {
        PeekMessageW(
            &mut msg,
            std::ptr::null_mut(),
            WM_USER,
            WM_USER,
            PM_NOREMOVE,
        )
    };
    ON_SESSION_END.with(|cb| *cb.borrow_mut() = Some(on_session_end));

    let class = wide("CadenceSystemEvents");
    let title = wide(SYSTEM_EVENTS_WINDOW_TITLE);
    // SAFETY: null module name returns this executable's handle.
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSW {
        lpfnWndProc: Some(events_wndproc),
        hInstance: module,
        lpszClassName: class.as_ptr(),
        ..Default::default()
    };
    // SAFETY: `wc` is initialized; registering twice fails harmlessly.
    unsafe { RegisterClassW(&wc) };
    // A hidden top-level window (not message-only: those get no session messages).
    // SAFETY: registered class, valid strings, no parent or menu.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        let _ = ready.send(Err(Win32Error::last()));
        return;
    }
    // SAFETY: no preconditions.
    let thread_id = unsafe { GetCurrentThreadId() };
    if ready.send(Ok(thread_id)).is_err() {
        // SAFETY: window owned by this thread.
        unsafe { DestroyWindow(hwnd) };
        return;
    }

    let handles: Vec<_> = activate.iter().map(|h| h.raw()).collect();
    let count = handles.len() as u32; // 1..=63, checked by `start`
    'outer: loop {
        // SAFETY: `count` valid handles (kept alive by `activate`); wakes for
        // any of the events or for any message.
        let r =
            unsafe { MsgWaitForMultipleObjects(count, handles.as_ptr(), 0, INFINITE, QS_ALLINPUT) };
        if (WAIT_OBJECT_0..WAIT_OBJECT_0 + count).contains(&r) {
            on_activate();
            continue;
        }
        if r != WAIT_OBJECT_0 + count {
            break; // WAIT_FAILED: should not happen with valid handles
        }
        // SAFETY: valid MSG.
        while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            if msg.message == WM_QUIT || msg.message == WM_APP_QUIT {
                break 'outer;
            }
            // SAFETY: valid MSG retrieved by PeekMessageW.
            unsafe { TranslateMessage(&msg) };
            // SAFETY: valid MSG retrieved by PeekMessageW.
            unsafe { DispatchMessageW(&msg) };
        }
    }
    // SAFETY: window owned by this thread.
    unsafe { DestroyWindow(hwnd) };
    ON_SESSION_END.with(|cb| *cb.borrow_mut() = None);
}

unsafe extern "system" fn collect_handles(hwnd: HWND, data: LPARAM) -> windows_sys::core::BOOL {
    // SAFETY: `data` is the `&mut Vec<usize>` passed to EnumWindows.
    let out = unsafe { &mut *(data as *mut Vec<usize>) };
    out.push(hwnd as usize);
    1
}

/// Restores (if minimized) and brings to the foreground the first visible
/// top-level window of this process whose title starts with `title_prefix`.
/// Returns whether such a window was found.
pub fn activate_own_window(title_prefix: &str) -> bool {
    let mut handles: Vec<usize> = Vec::new();
    // SAFETY: the callback only pushes into `handles`, which outlives the call.
    unsafe {
        EnumWindows(
            Some(collect_handles),
            &mut handles as *mut Vec<usize> as LPARAM,
        )
    };
    // SAFETY: no preconditions.
    let me = unsafe { GetCurrentProcessId() };
    for h in handles {
        let hwnd = h as HWND;
        let mut pid = 0u32;
        // SAFETY: valid out-pointer; stale handles just yield 0.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        // SAFETY: no pointer arguments.
        if pid != me || unsafe { IsWindowVisible(hwnd) } == 0 {
            continue;
        }
        let mut buf = [0u16; 256];
        // SAFETY: the buffer pointer and length describe a writable buffer.
        let len = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        let title = String::from_utf16_lossy(&buf[..len.max(0) as usize]);
        if !title.starts_with(title_prefix) {
            continue;
        }
        // SAFETY: no pointer arguments.
        if unsafe { IsIconic(hwnd) } != 0 {
            // SAFETY: valid window handle.
            unsafe { ShowWindow(hwnd, SW_RESTORE) };
        }
        // SAFETY: valid window handle.
        unsafe { SetForegroundWindow(hwnd) };
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    fn unique(name: &str) -> String {
        format!("CadenceTest.{name}.{}", std::process::id())
    }

    #[test]
    fn second_acquire_reports_already_running_until_released() {
        let name = unique("lock");
        let first = acquire_instance(&name).unwrap();
        assert!(matches!(first, Acquire::Acquired(_)));
        assert!(matches!(
            acquire_instance(&name).unwrap(),
            Acquire::AlreadyRunning
        ));
        assert!(matches!(
            acquire_instance(&name).unwrap(),
            Acquire::AlreadyRunning
        ));
        drop(first);
        let again = acquire_instance(&name).unwrap();
        assert!(
            matches!(again, Acquire::Acquired(_)),
            "lock released on drop"
        );
    }

    #[test]
    fn a_second_instance_wakes_the_first() {
        let name = unique("activate");
        let Acquire::Acquired(lock) = acquire_instance(&name).unwrap() else {
            panic!("first acquire");
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let h = hits.clone();
        let events = SystemEvents::start(
            &[&lock],
            move || {
                h.fetch_add(1, Ordering::SeqCst);
            },
            || {},
        )
        .unwrap();
        assert!(matches!(
            acquire_instance(&name).unwrap(),
            Acquire::AlreadyRunning
        ));
        let end = Instant::now() + Duration::from_secs(2);
        while hits.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < end, "activation not received");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(events.shutdown(Duration::from_secs(2)), ThreadExit::Clean);
    }

    #[test]
    fn activation_sent_before_listening_is_latched() {
        let name = unique("latched");
        let Acquire::Acquired(lock) = acquire_instance(&name).unwrap() else {
            panic!("first acquire");
        };
        assert!(matches!(
            acquire_instance(&name).unwrap(),
            Acquire::AlreadyRunning
        ));
        let (tx, rx) = mpsc::channel();
        let events = SystemEvents::start(&[&lock], move || tx.send(()).unwrap(), || {}).unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        assert_eq!(events.shutdown(Duration::from_secs(2)), ThreadExit::Clean);
    }

    #[test]
    fn an_activation_through_any_held_name_wakes_the_holder() {
        let (main, former) = (unique("main"), unique("former"));
        let Acquire::Acquired(main_lock) = acquire_instance(&main).unwrap() else {
            panic!("main acquire");
        };
        let Acquire::Acquired(former_lock) = acquire_instance(&former).unwrap() else {
            panic!("former acquire");
        };
        let (tx, rx) = mpsc::channel();
        let events = SystemEvents::start(
            &[&main_lock, &former_lock],
            move || tx.send(()).unwrap(),
            || {},
        )
        .unwrap();
        // A program that only knows the former name finds it taken and
        // signals it; the holder is woken.
        assert!(matches!(
            acquire_instance(&former).unwrap(),
            Acquire::AlreadyRunning
        ));
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        assert!(matches!(
            acquire_instance(&main).unwrap(),
            Acquire::AlreadyRunning
        ));
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        assert_eq!(events.shutdown(Duration::from_secs(2)), ThreadExit::Clean);
        assert!(SystemEvents::start(&[], || {}, || {}).is_err());
    }
}
