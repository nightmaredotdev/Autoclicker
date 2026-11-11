//! Picking a screen position with the next mouse click.
//!
//! While a [`PointPicker`] exists, a low-level mouse hook watches the
//! physical mouse buttons:
//! * the next **primary** press outside this application's own windows
//!   gives the position (in the same physical screen coordinates as
//!   `GetCursorPos`);
//! * a **secondary** press anywhere cancels.
//!
//! The deciding press and its release are swallowed, so the click that picks
//! (or cancels) never reaches the window underneath and no context menu
//! opens. Presses on this application's own windows pass through unchanged,
//! so its buttons (Cancel, Start/Stop) keep working. Events injected by this
//! application (they carry [`INJECTION_SIGNATURE`]) are ignored, so clicking
//! can never pick or cancel. There is no timeout; dropping the picker removes
//! the hook.

use super::error::Win32Error;
use super::input::{INJECTION_SIGNATURE, buttons_swapped};
use cadence_core::geometry::Point;
use std::sync::mpsc;
use std::sync::{Mutex, PoisonError};
use std::thread::JoinHandle;
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, GetWindowThreadProcessId, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE,
    PeekMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_MOUSE_LL, WM_APP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_USER, WindowFromPoint,
};

/// Posted to the hook thread when the pick is decided or the deciding
/// button's release has been swallowed.
const WM_APP_WAKE: u32 = WM_APP + 10;
/// Posted to the hook thread to remove the hook and end.
const WM_APP_QUIT: u32 = WM_APP + 11;

/// How a pick ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickOutcome {
    /// The primary button was pressed at this screen position.
    Picked(Point),
    /// The secondary button was pressed.
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Waiting,
    /// Decided; the release of the deciding button (this message) is still
    /// to be swallowed.
    SwallowRelease(u32),
    Done,
}

struct Shared {
    thread_id: u32,
    state: State,
    outcome: Option<PickOutcome>,
}

/// The active pick, read by the hook procedure. One picker at a time.
static PICK: Mutex<Option<Shared>> = Mutex::new(None);

/// Physical button messages `(primary down, primary up, secondary down,
/// secondary up)`. Low-level hooks see physical buttons, so with swapped
/// buttons the primary one is the physical right button.
fn button_messages(swapped: bool) -> (u32, u32, u32, u32) {
    if swapped {
        (WM_RBUTTONDOWN, WM_RBUTTONUP, WM_LBUTTONDOWN, WM_LBUTTONUP)
    } else {
        (WM_LBUTTONDOWN, WM_LBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP)
    }
}

/// What the hook does with one physical button message.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Pass,
    /// Swallow; if `Some`, the pick is decided with this outcome.
    Swallow(Option<PickOutcome>),
}

/// The picking rules, separate from the hook for testing. `own_window` says
/// whether the point is over one of this application's windows.
fn decide(
    state: &mut State,
    message: u32,
    point: Point,
    swapped: bool,
    own_window: impl FnOnce() -> bool,
) -> Action {
    let (primary_down, primary_up, secondary_down, secondary_up) = button_messages(swapped);
    match *state {
        State::Waiting if message == primary_down => {
            if own_window() {
                Action::Pass
            } else {
                *state = State::SwallowRelease(primary_up);
                Action::Swallow(Some(PickOutcome::Picked(point)))
            }
        }
        State::Waiting if message == secondary_down => {
            *state = State::SwallowRelease(secondary_up);
            Action::Swallow(Some(PickOutcome::Cancelled))
        }
        State::SwallowRelease(up) if message == up => {
            *state = State::Done;
            Action::Swallow(None)
        }
        _ => Action::Pass,
    }
}

fn over_own_window(point: Point) -> bool {
    let pt = windows_sys::Win32::Foundation::POINT {
        x: point.x,
        y: point.y,
    };
    // SAFETY: no pointer arguments.
    let hwnd = unsafe { WindowFromPoint(pt) };
    if hwnd.is_null() {
        return false;
    }
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid out pointer; an invalid window yields 0.
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    // SAFETY: no preconditions.
    pid == unsafe { GetCurrentProcessId() }
}

unsafe extern "system" fn pick_hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_MOUSE_LL with code >= 0, lparam points to a valid
        // MSLLHOOKSTRUCT.
        let info = unsafe { *(lparam as *const MSLLHOOKSTRUCT) };
        if info.dwExtraInfo != INJECTION_SIGNATURE {
            let mut guard = PICK.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(shared) = guard.as_mut() {
                let point = Point::new(info.pt.x, info.pt.y);
                let action = decide(
                    &mut shared.state,
                    wparam as u32,
                    point,
                    buttons_swapped(),
                    || over_own_window(point),
                );
                if let Action::Swallow(decided) = action {
                    if decided.is_some() {
                        shared.outcome = decided;
                    }
                    // SAFETY: no pointer arguments.
                    unsafe { PostThreadMessageW(shared.thread_id, WM_APP_WAKE, 0, 0) };
                    return 1; // swallow: no application receives this event
                }
            }
        }
    }
    // SAFETY: forwarding the unmodified hook arguments.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Captures a screen position with the next primary click (see the module
/// documentation). Dropping it removes the hook; the callback is then not
/// called if the pick was not decided yet.
pub struct PointPicker {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl PointPicker {
    /// Installs the hook. `on_done` runs once, on the picker's own thread,
    /// when the pick is decided. The hook stays until the deciding button is
    /// released (so the release is swallowed too) or the picker is dropped.
    pub fn start(on_done: impl FnOnce(PickOutcome) + Send + 'static) -> Result<Self, Win32Error> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("point-picker".into())
            .spawn(move || run_hook_thread(on_done, &ready_tx))
            .map_err(|e| Win32Error {
                code: 0,
                message: format!("could not start the picker thread: {e}"),
            })?;
        match ready_rx.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread_id,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(Win32Error {
                    code: 0,
                    message: "the picker thread ended unexpectedly".into(),
                })
            }
        }
    }
}

fn run_hook_thread(
    on_done: impl FnOnce(PickOutcome),
    ready: &mpsc::Sender<Result<u32, Win32Error>>,
) {
    let mut msg = MSG::default();
    // SAFETY: valid MSG; creates this thread's message queue so that posted
    // messages are not lost.
    unsafe {
        PeekMessageW(
            &mut msg,
            std::ptr::null_mut(),
            WM_USER,
            WM_USER,
            PM_NOREMOVE,
        )
    };
    // SAFETY: no preconditions.
    let thread_id = unsafe { GetCurrentThreadId() };
    {
        let mut guard = PICK.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.is_some() {
            let _ = ready.send(Err(Win32Error {
                code: 0,
                message: "a pick is already in progress".into(),
            }));
            return;
        }
        *guard = Some(Shared {
            thread_id,
            state: State::Waiting,
            outcome: None,
        });
    }
    // SAFETY: null module name returns this executable's handle.
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    // SAFETY: valid hook procedure; global low-level hook (thread id 0).
    let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(pick_hook_proc), module, 0) };
    if hook.is_null() {
        let error = Win32Error::last();
        *PICK.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let _ = ready.send(Err(error));
        return;
    }
    let _ = ready.send(Ok(thread_id));

    let mut on_done = Some(on_done);
    // Low-level hooks are called through this thread's message loop.
    loop {
        // SAFETY: valid MSG.
        let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if r == 0 || r == -1 || msg.message == WM_APP_QUIT {
            break;
        }
        if msg.message == WM_APP_WAKE {
            let (outcome, done) = PICK
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_mut()
                .map_or((None, true), |s| (s.outcome.take(), s.state == State::Done));
            if let (Some(outcome), Some(callback)) = (outcome, on_done.take()) {
                callback(outcome);
            }
            if done {
                break;
            }
        }
    }
    // SAFETY: `hook` was installed by this thread and is still installed.
    unsafe { UnhookWindowsHookEx(hook) };
    *PICK.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

impl Drop for PointPicker {
    fn drop(&mut self) {
        // SAFETY: no pointer arguments.
        unsafe { PostThreadMessageW(self.thread_id, WM_APP_QUIT, 0, 0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl std::fmt::Debug for PointPicker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PointPicker").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod rule_tests {
    use super::*;

    const P: Point = Point { x: 10, y: -20 };

    #[test]
    fn primary_press_picks_and_its_release_is_swallowed() {
        let mut s = State::Waiting;
        assert_eq!(
            decide(&mut s, WM_LBUTTONDOWN, P, false, || false),
            Action::Swallow(Some(PickOutcome::Picked(P)))
        );
        // Other buttons pass while the release is pending.
        assert_eq!(
            decide(&mut s, WM_RBUTTONDOWN, P, false, || false),
            Action::Pass
        );
        assert_eq!(
            decide(&mut s, WM_LBUTTONUP, P, false, || false),
            Action::Swallow(None)
        );
        assert_eq!(s, State::Done);
        // Done: everything passes.
        assert_eq!(
            decide(&mut s, WM_LBUTTONDOWN, P, false, || false),
            Action::Pass
        );
    }

    #[test]
    fn secondary_press_cancels_even_over_own_window() {
        let mut s = State::Waiting;
        assert_eq!(
            decide(&mut s, WM_RBUTTONDOWN, P, false, || true),
            Action::Swallow(Some(PickOutcome::Cancelled))
        );
        assert_eq!(
            decide(&mut s, WM_RBUTTONUP, P, false, || true),
            Action::Swallow(None)
        );
    }

    #[test]
    fn primary_press_on_own_window_passes_and_keeps_waiting() {
        let mut s = State::Waiting;
        assert_eq!(
            decide(&mut s, WM_LBUTTONDOWN, P, false, || true),
            Action::Pass
        );
        assert_eq!(
            decide(&mut s, WM_LBUTTONUP, P, false, || true),
            Action::Pass
        );
        assert_eq!(s, State::Waiting);
    }

    #[test]
    fn swapped_buttons_swap_pick_and_cancel() {
        let mut s = State::Waiting;
        assert_eq!(
            decide(&mut s, WM_RBUTTONDOWN, P, true, || false),
            Action::Swallow(Some(PickOutcome::Picked(P)))
        );
        assert_eq!(
            decide(&mut s, WM_RBUTTONUP, P, true, || false),
            Action::Swallow(None)
        );
        let mut s = State::Waiting;
        assert_eq!(
            decide(&mut s, WM_LBUTTONDOWN, P, true, || false),
            Action::Swallow(Some(PickOutcome::Cancelled))
        );
    }

    #[test]
    fn releases_and_moves_alone_do_nothing() {
        let mut s = State::Waiting;
        for m in [WM_LBUTTONUP, WM_RBUTTONUP, 0x0200 /* WM_MOUSEMOVE */] {
            assert_eq!(decide(&mut s, m, P, false, || false), Action::Pass);
        }
        assert_eq!(s, State::Waiting);
    }
}
