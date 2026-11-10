//! Owns the engine and hotkey threads and coordinates shutdown.
//!
//! Shutdown never relies on channels disconnecting (the hotkey thread holds a
//! command sender too). [`Runtime::shutdown`] runs once, in this order:
//! 1. send `Command::Shutdown` to the engine: it stops clicking, releases any
//!    held button (with retries) and exits; the thread is joined;
//! 2. post a quit message to the hotkey thread: it unregisters every hotkey on
//!    its own thread and leaves its message loop; the thread is joined.
//!
//! Each join waits at most [`JOIN_TIMEOUT`].

use autoclicker_core::config::{HotkeyAction, ValidatedConfig};
use autoclicker_core::engine::{Command, EngineHandle, ThreadExit, spawn_engine};
use autoclicker_core::status::{Snapshot, StatusSlot, StatusUpdate};
use autoclicker_core::wait::{EngineGone, PlainWaiter, Waiter};
use autoclicker_platform::{
    HighResWaiter, HotkeyEvent, HotkeyService, Registration, Win32Error, WindowsInput,
};
use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

pub const JOIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Which waiter implementation the engine uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaiterKind {
    HighResolution,
    Plain,
}

impl fmt::Display for WaiterKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            WaiterKind::HighResolution => "high-resolution waitable timer",
            WaiterKind::Plain => "plain (condition variable)",
        })
    }
}

/// Events for the main thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppEvent {
    /// A new status snapshot is available via [`Runtime::take_status`].
    Status,
    Exit(ExitReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Hotkey,
    ConsoleControl,
}

impl fmt::Display for ExitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ExitReason::Hotkey => "exit hotkey pressed",
            ExitReason::ConsoleControl => "console closed or Ctrl+C",
        })
    }
}

/// How each thread ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShutdownReport {
    pub engine: ThreadExit,
    pub hotkeys: ThreadExit,
}

impl ShutdownReport {
    pub fn is_clean(&self) -> bool {
        self.engine == ThreadExit::Clean && self.hotkeys == ThreadExit::Clean
    }
}

#[derive(Debug)]
pub enum StartError {
    Thread(std::io::Error),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::Thread(e) => write!(f, "could not start a worker thread: {e}"),
        }
    }
}

pub struct Runtime {
    engine: Option<EngineHandle>,
    hotkeys: Option<HotkeyService>,
    status: Arc<StatusSlot>,
    events: Receiver<AppEvent>,
    events_tx: Sender<AppEvent>,
    registrations: Vec<Registration>,
    waiter: WaiterKind,
    timer_fallback: Option<Win32Error>,
}

impl Runtime {
    pub fn start(config: &ValidatedConfig, waiter: WaiterKind) -> Result<Self, StartError> {
        let (events_tx, events) = mpsc::channel();
        let status = Arc::new(StatusSlot::new(Snapshot::new(config.click.mode())));
        let wake = {
            let tx = events_tx.clone();
            Box::new(move || {
                let _ = tx.send(AppEvent::Status);
            })
        };

        let mut timer_fallback = None;
        let (boxed_waiter, waiter): (Box<dyn Waiter>, WaiterKind) = match waiter {
            WaiterKind::HighResolution => match HighResWaiter::new() {
                Ok(w) => (Box::new(w), WaiterKind::HighResolution),
                Err(e) => {
                    timer_fallback = Some(e);
                    (Box::new(PlainWaiter::new()), WaiterKind::Plain)
                }
            },
            WaiterKind::Plain => (Box::new(PlainWaiter::new()), WaiterKind::Plain),
        };

        let engine = spawn_engine(
            WindowsInput::new(),
            boxed_waiter,
            config.click.clone(),
            status.clone(),
            wake,
            seed(),
        )
        .map_err(StartError::Thread)?;

        let sender = engine.sender();
        let exit_tx = events_tx.clone();
        let on_hotkey = move |e: HotkeyEvent| {
            let release_first = e.hotkey.modifiers;
            let _: Result<(), EngineGone> = match e.action {
                HotkeyAction::Toggle => sender.send(Command::Toggle { release_first }),
                HotkeyAction::Start => sender.send(Command::Start { release_first }),
                HotkeyAction::Stop => sender.send(Command::Stop),
                HotkeyAction::SetTarget => sender.send(Command::SetTargetFromCursor),
                HotkeyAction::Exit => {
                    let _ = exit_tx.send(AppEvent::Exit(ExitReason::Hotkey));
                    Ok(())
                }
            };
        };
        let (hotkeys, registrations) = match HotkeyService::start(&config.hotkeys, on_hotkey) {
            Ok(started) => started,
            Err(e) => {
                engine.shutdown(JOIN_TIMEOUT);
                return Err(StartError::Thread(e));
            }
        };

        Ok(Self {
            engine: Some(engine),
            hotkeys: Some(hotkeys),
            status,
            events,
            events_tx,
            registrations,
            waiter,
            timer_fallback,
        })
    }

    pub fn registrations(&self) -> &[Registration] {
        &self.registrations
    }

    pub fn waiter(&self) -> WaiterKind {
        self.waiter
    }

    /// Why the high-resolution timer could not be used, if it fell back.
    pub fn timer_fallback(&self) -> Option<&Win32Error> {
        self.timer_fallback.as_ref()
    }

    /// A sender for other exit sources (e.g. the console control handler).
    pub fn event_sender(&self) -> Sender<AppEvent> {
        self.events_tx.clone()
    }

    pub fn recv(&self) -> AppEvent {
        // `events_tx` is owned by `self`, so the channel never disconnects.
        self.events.recv().expect("runtime owns a sender")
    }

    #[cfg(test)]
    pub fn recv_timeout(&self, timeout: Duration) -> Option<AppEvent> {
        self.events.recv_timeout(timeout).ok()
    }

    pub fn take_status(&self) -> StatusUpdate {
        self.status.take()
    }

    #[cfg(test)] // the GUI milestone sends commands from its controls
    pub fn send(&self, command: Command) -> Result<(), EngineGone> {
        match &self.engine {
            Some(engine) => engine.send(command),
            None => Err(EngineGone),
        }
    }

    /// Stops the engine (releasing any held button), then unregisters the
    /// hotkeys and ends their thread. Returns `None` if already shut down.
    pub fn shutdown(&mut self) -> Option<ShutdownReport> {
        let engine = self.engine.take()?;
        let engine = engine.shutdown(JOIN_TIMEOUT);
        let hotkeys = self
            .hotkeys
            .take()
            .map_or(ThreadExit::Clean, |h| h.shutdown(JOIN_TIMEOUT));
        Some(ShutdownReport { engine, hotkeys })
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Seed for click-timing variation (not security relevant).
fn seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    nanos ^ u64::from(std::process::id()).rotate_left(32)
}

#[cfg(test)]
mod tests {
    //! Integration tests using the real desktop; run serially with
    //! `cargo test -p autoclicker -- --ignored --test-threads=1`.

    use super::*;
    use autoclicker_core::config::{ClickConfig, Config, HotkeyConfig, TargetMode};
    use autoclicker_core::geometry::Rect;
    use autoclicker_core::keys::{Hotkey, Key, Modifiers};
    use autoclicker_platform::diagnostics::{
        MouseHookRecorder, TestWindow, WindowKind, force_foreground, foreground_window, monitors,
        send_keys,
    };
    use std::sync::{Mutex, MutexGuard, PoisonError};
    use std::time::Instant;

    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// F13/F14 bindings, so tests never collide with a running autoclicker.
    fn config(hold_ms: u32, interval_ms: u32) -> ValidatedConfig {
        Config {
            click: ClickConfig {
                mode: TargetMode::Cursor,
                interval_ms,
                interval_jitter_sigma_ms: 0.0,
                hold_ms,
                position_jitter_radius_px: 0,
            },
            hotkeys: HotkeyConfig {
                toggle: Some(Hotkey::plain(Key::F13)),
                start: None,
                stop: None,
                set_target: None,
                exit: Some(Hotkey::plain(Key::F14)),
            },
            ..Config::default()
        }
        .validate()
        .unwrap()
    }

    fn hotkeys_are_free(cfg: &ValidatedConfig) -> bool {
        let (service, regs) = HotkeyService::start(&cfg.hotkeys, |_| {}).unwrap();
        let free = regs.iter().all(|r| r.result.is_ok());
        assert_eq!(service.shutdown(JOIN_TIMEOUT), ThreadExit::Clean);
        free
    }

    #[test]
    #[ignore = "needs an interactive, unlocked Windows desktop"]
    fn shutdown_joins_both_threads_and_frees_hotkeys() {
        let _g = serial();
        let cfg = config(10, 100);
        let mut rt = Runtime::start(&cfg, WaiterKind::HighResolution).unwrap();
        assert!(rt.registrations().iter().all(|r| r.result.is_ok()));
        assert!(!hotkeys_are_free(&cfg), "hotkeys are held while running");
        let started = Instant::now();
        let report = rt.shutdown().expect("first shutdown");
        assert!(report.is_clean(), "{report:?}");
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(rt.shutdown().is_none(), "shutdown is idempotent");
        assert!(hotkeys_are_free(&cfg), "hotkeys were unregistered");
    }

    #[test]
    #[ignore = "needs an interactive, unlocked Windows desktop"]
    fn dropping_the_runtime_also_shuts_down() {
        let _g = serial();
        let cfg = config(10, 100);
        drop(Runtime::start(&cfg, WaiterKind::Plain).unwrap());
        assert!(hotkeys_are_free(&cfg));
    }

    #[test]
    #[ignore = "needs an interactive, unlocked Windows desktop"]
    fn shutdown_while_holding_releases_the_button() {
        let _g = serial();
        let recorder = MouseHookRecorder::start(true).unwrap();
        let mut rt = Runtime::start(&config(1500, 2000), WaiterKind::HighResolution).unwrap();
        rt.send(Command::Start {
            release_first: Modifiers::NONE,
        })
        .unwrap();
        assert!(recorder.wait_for(Duration::from_secs(2), |e| !e.is_empty()));
        let report = rt.shutdown().unwrap();
        assert!(report.is_clean(), "{report:?}");
        assert!(recorder.wait_for(Duration::from_secs(1), |e| e.len() == 2));
        assert!(recorder.events()[1].message.is_up());
    }

    #[test]
    #[ignore = "needs an interactive, unlocked Windows desktop"]
    fn exit_hotkey_leads_to_a_clean_shutdown() {
        let _g = serial();
        let mut rt = Runtime::start(&config(10, 100), WaiterKind::HighResolution).unwrap();
        // Inject keys only while a test-owned window has the foreground.
        let previous = foreground_window();
        let m = monitors()[0];
        let r = Rect::new(m.left + 50, m.top + 50, m.left + 250, m.top + 250);
        let window = TestWindow::create(r, WindowKind::TopmostPopup, "runtime test").unwrap();
        assert!(window.bring_to_foreground(), "inconclusive: no foreground");
        send_keys(&[(0x7D, false), (0x7D, true)]); // F14
        let event = loop {
            match rt.recv_timeout(Duration::from_secs(2)) {
                Some(AppEvent::Status) => continue,
                other => break other,
            }
        };
        drop(window);
        force_foreground(previous);
        assert_eq!(event, Some(AppEvent::Exit(ExitReason::Hotkey)));
        assert!(rt.shutdown().unwrap().is_clean());
    }
}
