//! Global hotkeys via `RegisterHotKey` with `MOD_NOREPEAT`, on a dedicated
//! thread that owns the registrations and their message loop.
//!
//! Hotkeys are registered without a window (`hWnd = NULL`), so `WM_HOTKEY` is
//! posted to the registering thread, and unregistering must happen on that
//! same thread. `MOD_NOREPEAT` suppresses keyboard auto-repeat: a held key
//! fires once, and fires again only after it is released and pressed again.

use super::error::Win32Error;
use super::keys::{hotkey_modifiers, virtual_key};
use cadence_core::config::{HotkeyAction, HotkeyBindings};
use cadence_core::engine::ThreadExit;
use cadence_core::keys::Hotkey;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::Duration;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_APP, WM_HOTKEY, WM_USER,
};

/// Posted to the hotkey thread to make it unregister and exit.
const WM_APP_QUIT: u32 = WM_APP + 1;

/// A hotkey press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HotkeyEvent {
    pub action: HotkeyAction,
    pub hotkey: Hotkey,
}

/// The outcome of registering one binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    pub action: HotkeyAction,
    pub hotkey: Hotkey,
    pub result: Result<(), Win32Error>,
}

/// Owns the hotkey thread.
#[derive(Debug)]
pub struct HotkeyService {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
    done: mpsc::Receiver<()>,
}

/// Unregisters on drop, including during unwinding, on the owning thread.
struct RegisteredIds(Vec<i32>);

impl Drop for RegisteredIds {
    fn drop(&mut self) {
        for &id in &self.0 {
            // SAFETY: `id` was registered by this thread with a null window.
            unsafe { UnregisterHotKey(std::ptr::null_mut(), id) };
        }
    }
}

struct DoneSignal(mpsc::Sender<()>);

impl Drop for DoneSignal {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

impl HotkeyService {
    /// Starts the hotkey thread and registers every binding. Returns once the
    /// thread's message queue exists and registration has finished, so it is
    /// safe to post to it. Failed registrations are reported, not fatal.
    pub fn start(
        bindings: &HotkeyBindings,
        on_hotkey: impl Fn(HotkeyEvent) + Send + 'static,
    ) -> std::io::Result<(Self, Vec<Registration>)> {
        let bindings: Vec<(HotkeyAction, Hotkey)> = bindings.iter().collect();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("hotkeys".into())
            .spawn(move || {
                let _done = DoneSignal(done_tx);
                hotkey_thread(&bindings, &ready_tx, &on_hotkey);
            })?;
        let (thread_id, registrations) = ready_rx.recv().map_err(|_| {
            std::io::Error::other("hotkey thread exited before reporting readiness")
        })?;
        Ok((
            Self {
                thread_id,
                thread: Some(thread),
                done,
            },
            registrations,
        ))
    }

    /// Asks the thread to unregister every hotkey and exit, then joins it,
    /// waiting at most `timeout`.
    pub fn shutdown(mut self, timeout: Duration) -> ThreadExit {
        // SAFETY: no pointer arguments. Failure means the thread already exited.
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

fn hotkey_thread(
    bindings: &[(HotkeyAction, Hotkey)],
    ready: &mpsc::Sender<(u32, Vec<Registration>)>,
    on_hotkey: &dyn Fn(HotkeyEvent),
) {
    let mut msg = MSG::default();
    // Create this thread's message queue before anyone can post to it.
    // SAFETY: `msg` is a valid, writable MSG; PM_NOREMOVE leaves the queue intact.
    unsafe {
        PeekMessageW(
            &mut msg,
            std::ptr::null_mut(),
            WM_USER,
            WM_USER,
            PM_NOREMOVE,
        )
    };

    let mut registered = RegisteredIds(Vec::new());
    let mut registrations = Vec::with_capacity(bindings.len());
    for (index, &(action, hotkey)) in bindings.iter().enumerate() {
        let id = index as i32 + 1; // application ids must be within 0x0000..=0xBFFF
        let modifiers = hotkey_modifiers(hotkey.modifiers) | MOD_NOREPEAT;
        let vk = u32::from(virtual_key(hotkey.key));
        // SAFETY: null window: WM_HOTKEY is posted to this thread's queue.
        let ok = unsafe { RegisterHotKey(std::ptr::null_mut(), id, modifiers, vk) };
        let result = if ok == 0 {
            Err(Win32Error::last())
        } else {
            registered.0.push(id);
            Ok(())
        };
        registrations.push(Registration {
            action,
            hotkey,
            result,
        });
    }
    // SAFETY: no preconditions.
    let thread_id = unsafe { GetCurrentThreadId() };
    if ready.send((thread_id, registrations)).is_err() {
        return;
    }

    loop {
        // SAFETY: `msg` is a valid, writable MSG.
        let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if r == 0 || r == -1 {
            break; // WM_QUIT or error
        }
        match msg.message {
            WM_HOTKEY => {
                let id = msg.wParam as i32;
                if id >= 1
                    && let Some(&(action, hotkey)) = bindings.get(id as usize - 1)
                    && registered.0.contains(&id)
                {
                    on_hotkey(HotkeyEvent { action, hotkey });
                }
            }
            WM_APP_QUIT => break,
            _ => {}
        }
    }
    drop(registered); // unregister on this thread before exiting
}
