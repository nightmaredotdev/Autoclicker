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

#![forbid(unsafe_code)]

use autoclicker_core::config::{HotkeyAction, HotkeyBindings, ValidatedConfig};
use autoclicker_core::engine::{Command, EngineHandle, ThreadExit, spawn_engine};
use autoclicker_core::status::{Snapshot, StatusSlot, StatusUpdate};
use autoclicker_core::wait::{CommandSender, EngineGone, PlainWaiter, Waiter};
use autoclicker_platform::{
    HighResWaiter, HotkeyEvent, HotkeyService, Registration, Win32Error, WindowsInput,
};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

pub const JOIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Which waiter implementation the engine uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaiterKind {
    HighResolution,
    Plain,
}

/// Called (from the engine thread) when a new status snapshot is available.
pub type WakeFn = Box<dyn Fn() + Send>;
/// Called (from the hotkey thread) when the exit hotkey is pressed.
pub type ExitFn = Arc<dyn Fn() + Send + Sync>;

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

impl std::error::Error for StartError {}

/// A rebind that could not be applied. The previous bindings were registered
/// again.
#[derive(Debug)]
pub enum RebindError {
    /// These new bindings could not be registered (with the reason).
    Conflicts(Vec<Registration>),
    Thread(std::io::Error),
}

pub struct Runtime {
    engine: Option<EngineHandle>,
    hotkeys: Option<HotkeyService>,
    bindings: HotkeyBindings,
    registrations: Vec<Registration>,
    status: Arc<StatusSlot>,
    on_exit: ExitFn,
    waiter: WaiterKind,
    timer_fallback: Option<Win32Error>,
}

impl Runtime {
    pub fn start(
        config: &ValidatedConfig,
        waiter: WaiterKind,
        wake: WakeFn,
        on_exit: ExitFn,
    ) -> Result<Self, StartError> {
        let status = Arc::new(StatusSlot::new(Snapshot::new(config.click.mode())));

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

        let mut runtime = Self {
            engine: Some(engine),
            hotkeys: None,
            bindings: config.hotkeys.clone(),
            registrations: Vec::new(),
            status,
            on_exit,
            waiter,
            timer_fallback,
        };
        let bindings = runtime.bindings.clone();
        if let Err(e) = runtime.start_hotkeys(&bindings) {
            runtime.shutdown();
            return Err(StartError::Thread(e));
        }
        Ok(runtime)
    }

    fn hotkey_callback(&self) -> impl Fn(HotkeyEvent) + Send + 'static {
        let sender: Option<CommandSender> = self.engine.as_ref().map(EngineHandle::sender);
        let on_exit = self.on_exit.clone();
        move |e: HotkeyEvent| {
            let release_first = e.hotkey.modifiers;
            let command = match e.action {
                HotkeyAction::Toggle => Command::Toggle { release_first },
                HotkeyAction::Start => Command::Start { release_first },
                HotkeyAction::Stop => Command::Stop,
                HotkeyAction::SetTarget => Command::SetTargetFromCursor,
                HotkeyAction::Exit => return on_exit(),
            };
            if let Some(sender) = &sender {
                let _: Result<(), EngineGone> = sender.send(command);
            }
        }
    }

    /// Starts a hotkey thread for `bindings`. Failed registrations are kept
    /// in [`Runtime::registrations`], not treated as errors.
    fn start_hotkeys(&mut self, bindings: &HotkeyBindings) -> std::io::Result<()> {
        let (service, registrations) = HotkeyService::start(bindings, self.hotkey_callback())?;
        self.hotkeys = Some(service);
        self.registrations = registrations;
        Ok(())
    }

    fn stop_hotkeys(&mut self) -> ThreadExit {
        self.hotkeys
            .take()
            .map_or(ThreadExit::Clean, |h| h.shutdown(JOIN_TIMEOUT))
    }

    /// Outcome of registering each current binding.
    pub fn registrations(&self) -> &[Registration] {
        &self.registrations
    }

    pub fn hotkeys_active(&self) -> bool {
        self.hotkeys.is_some()
    }

    /// Unregisters every hotkey (while the user records a new one, so the key
    /// presses reach the window instead of triggering actions).
    pub fn suspend_hotkeys(&mut self) {
        self.stop_hotkeys();
    }

    /// Registers the current bindings again after [`Runtime::suspend_hotkeys`].
    pub fn resume_hotkeys(&mut self) -> std::io::Result<()> {
        if self.hotkeys.is_some() || self.engine.is_none() {
            return Ok(());
        }
        let bindings = self.bindings.clone();
        self.start_hotkeys(&bindings)
    }

    /// Replaces the bindings. If any new binding cannot be registered, the
    /// previous bindings are registered again and the failures are returned.
    pub fn rebind(&mut self, bindings: HotkeyBindings) -> Result<(), RebindError> {
        self.stop_hotkeys();
        let error = match self.start_hotkeys(&bindings) {
            Ok(()) => {
                let failed: Vec<Registration> = self
                    .registrations
                    .iter()
                    .filter(|r| r.result.is_err())
                    .cloned()
                    .collect();
                if failed.is_empty() {
                    self.bindings = bindings;
                    return Ok(());
                }
                RebindError::Conflicts(failed)
            }
            Err(e) => RebindError::Thread(e),
        };
        self.stop_hotkeys();
        let previous = self.bindings.clone();
        let _ = self.start_hotkeys(&previous);
        Err(error)
    }

    pub fn waiter(&self) -> WaiterKind {
        self.waiter
    }

    /// Why the high-resolution timer could not be used, if it fell back.
    pub fn timer_fallback(&self) -> Option<&Win32Error> {
        self.timer_fallback.as_ref()
    }

    pub fn take_status(&self) -> StatusUpdate {
        self.status.take()
    }

    pub fn send(&self, command: Command) -> Result<(), EngineGone> {
        match &self.engine {
            Some(engine) => engine.send(command),
            None => Err(EngineGone),
        }
    }

    pub fn is_running(&self) -> bool {
        self.engine.is_some()
    }

    /// Stops the engine (releasing any held button), then unregisters the
    /// hotkeys and ends their thread. Returns `None` if already shut down.
    pub fn shutdown(&mut self) -> Option<ShutdownReport> {
        let engine = self.engine.take()?;
        let engine = engine.shutdown(JOIN_TIMEOUT);
        let hotkeys = self.stop_hotkeys();
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
    //! `cargo test -p autoclicker --lib -- --ignored --test-threads=1`.

    use super::*;
    use autoclicker_core::config::{ClickConfig, Config, HotkeyConfig, TargetMode};
    use autoclicker_core::geometry::Rect;
    use autoclicker_core::keys::{Hotkey, Key, Modifiers};
    use autoclicker_platform::diagnostics::{
        MouseHookRecorder, TestWindow, WindowKind, force_foreground, foreground_window, monitors,
        send_keys,
    };
    use std::sync::mpsc;
    use std::sync::{Mutex, MutexGuard, PoisonError};
    use std::time::Instant;

    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bindings(keys: &[(HotkeyAction, Key)]) -> HotkeyBindings {
        let mut hotkeys = HotkeyConfig {
            toggle: None,
            start: None,
            stop: None,
            set_target: None,
            exit: None,
        };
        for &(action, key) in keys {
            hotkeys.set(action, Some(Hotkey::plain(key)));
        }
        Config {
            hotkeys,
            ..Config::default()
        }
        .validate()
        .unwrap()
        .hotkeys
    }

    /// F13/F14 bindings, so tests never collide with a running autoclicker.
    fn config(hold_ms: u32, interval_ms: u32) -> ValidatedConfig {
        let mut cfg = Config {
            click: ClickConfig {
                mode: TargetMode::Cursor,
                interval_ms,
                interval_jitter_sigma_ms: 0.0,
                hold_ms,
                position_jitter_radius_px: 0,
                target: None,
            },
            ..Config::default()
        };
        cfg.hotkeys.toggle = Some(Hotkey::plain(Key::F13));
        cfg.hotkeys.set_target = None;
        cfg.hotkeys.exit = Some(Hotkey::plain(Key::F14));
        cfg.validate().unwrap()
    }

    fn start(cfg: &ValidatedConfig) -> (Runtime, mpsc::Receiver<&'static str>) {
        let (tx, rx) = mpsc::channel();
        let wake_tx = tx.clone();
        let rt = Runtime::start(
            cfg,
            WaiterKind::HighResolution,
            Box::new(move || {
                let _ = wake_tx.send("status");
            }),
            Arc::new(move || {
                let _ = tx.send("exit");
            }),
        )
        .unwrap();
        (rt, rx)
    }

    fn hotkeys_are_free(bindings: &HotkeyBindings) -> bool {
        let (service, regs) = HotkeyService::start(bindings, |_| {}).unwrap();
        let free = regs.iter().all(|r| r.result.is_ok());
        assert_eq!(service.shutdown(JOIN_TIMEOUT), ThreadExit::Clean);
        free
    }

    #[test]
    #[ignore = "needs an interactive, unlocked Windows desktop"]
    fn shutdown_joins_both_threads_and_frees_hotkeys() {
        let _g = serial();
        let cfg = config(10, 100);
        let (mut rt, _rx) = start(&cfg);
        assert!(rt.registrations().iter().all(|r| r.result.is_ok()));
        assert!(
            !hotkeys_are_free(&cfg.hotkeys),
            "hotkeys are held while running"
        );
        let started = Instant::now();
        let report = rt.shutdown().expect("first shutdown");
        assert!(report.is_clean(), "{report:?}");
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(rt.shutdown().is_none(), "shutdown is idempotent");
        assert!(hotkeys_are_free(&cfg.hotkeys), "hotkeys were unregistered");
    }

    #[test]
    #[ignore = "needs an interactive, unlocked Windows desktop"]
    fn dropping_the_runtime_also_shuts_down() {
        let _g = serial();
        let cfg = config(10, 100);
        drop(start(&cfg).0);
        assert!(hotkeys_are_free(&cfg.hotkeys));
    }

    #[test]
    #[ignore = "needs an interactive, unlocked Windows desktop"]
    fn shutdown_while_holding_releases_the_button() {
        let _g = serial();
        let recorder = MouseHookRecorder::start(true).unwrap();
        let (mut rt, _rx) = start(&config(1500, 2000));
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
    fn exit_hotkey_invokes_the_exit_callback() {
        let _g = serial();
        let (mut rt, rx) = start(&config(10, 100));
        // Inject keys only while a test-owned window has the foreground.
        let previous = foreground_window();
        let m = monitors()[0];
        let r = Rect::new(m.left + 50, m.top + 50, m.left + 250, m.top + 250);
        let window = TestWindow::create(r, WindowKind::TopmostPopup, "runtime test").unwrap();
        assert!(window.bring_to_foreground(), "inconclusive: no foreground");
        send_keys(&[(0x7D, false), (0x7D, true)]); // F14
        let got = loop {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok("status") => continue,
                other => break other,
            }
        };
        drop(window);
        force_foreground(previous);
        assert_eq!(got, Ok("exit"));
        assert!(rt.shutdown().unwrap().is_clean());
    }

    #[test]
    #[ignore = "needs an interactive, unlocked Windows desktop"]
    fn conflicting_rebind_restores_the_previous_bindings() {
        let _g = serial();
        let cfg = config(10, 100);
        let (mut rt, _rx) = start(&cfg);

        // Another "program" holds F15.
        let taken = bindings(&[(HotkeyAction::Toggle, Key::F15)]);
        let (other, regs) = HotkeyService::start(&taken, |_| {}).unwrap();
        assert!(regs[0].result.is_ok());

        let wanted = bindings(&[
            (HotkeyAction::Toggle, Key::F16),
            (HotkeyAction::Start, Key::F15),
            (HotkeyAction::Exit, Key::F14),
        ]);
        let Err(RebindError::Conflicts(failed)) = rt.rebind(wanted) else {
            panic!("rebind onto a taken key must fail");
        };
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].hotkey, Hotkey::plain(Key::F15));
        assert_eq!(failed[0].result.as_ref().unwrap_err().code, 1409);
        // The old bindings (F13, F14) are active again; F16 is not held.
        assert!(rt.registrations().iter().all(|r| r.result.is_ok()));
        assert!(!hotkeys_are_free(&cfg.hotkeys));
        assert!(hotkeys_are_free(&bindings(&[(
            HotkeyAction::Toggle,
            Key::F16
        )])));
        assert_eq!(other.shutdown(JOIN_TIMEOUT), ThreadExit::Clean);

        // A successful rebind takes effect.
        let moved = bindings(&[(HotkeyAction::Toggle, Key::F17)]);
        rt.rebind(moved.clone()).unwrap();
        assert!(!hotkeys_are_free(&moved));
        assert!(hotkeys_are_free(&cfg.hotkeys));

        // Suspend frees everything; resume restores it.
        rt.suspend_hotkeys();
        assert!(hotkeys_are_free(&moved));
        rt.resume_hotkeys().unwrap();
        assert!(!hotkeys_are_free(&moved));
        assert!(rt.shutdown().unwrap().is_clean());
    }
}
