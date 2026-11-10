//! The desktop window: wires the Slint UI to the runtime and the settings.
//!
//! Threading: everything here runs on the UI thread. The engine thread wakes
//! the UI through `Weak::upgrade_in_event_loop` (at most one wake-up is ever
//! queued, thanks to the status slot), and the exit hotkey does the same.
//! Tray menu callbacks run on the UI thread too, and the system-events thread
//! (second instance, session end) also goes through the event loop, so all
//! actions are serialized with each other and with shutdown.
//!
//! Closing the window hides it to the notification area (if the tray icon
//! could be created). Every exit path - tray Exit, the Exit button, the exit
//! hotkey and the end of the Windows session - uses one shutdown
//! ([`Controller::close`]) that runs once.

#![forbid(unsafe_code)]

use crate::form::{self, KeyInput, Status, TimingFields};
use crate::runtime::{RebindError, Runtime, ShutdownReport, WaiterKind};
use crate::ui::{AboutWindow, AppTray, HotkeyRow, MainWindow, RunStatus};
use crate::{config_store, logging};
use autoclicker_core::backend::InputBackend;
use autoclicker_core::config::{ClickConfig, Config, HotkeyAction, TargetMode};
use autoclicker_core::engine::Command;
use autoclicker_core::keys::{Hotkey, Modifiers};
use autoclicker_core::status::{RunState, Snapshot};
use autoclicker_platform::{
    Acquire, SystemEvents, WindowsInput, acquire_instance, activate_own_window,
    enable_per_monitor_dpi_awareness,
};
use log::{error, info, warn};
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, SharedString, Timer, VecModel};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

/// Seconds counted down before "Pick target" reads the cursor position.
pub const PICK_COUNTDOWN_SECONDS: i32 = 3;
const NOTICE_DURATION: Duration = Duration::from_secs(6);
/// Single-instance lock name; `AUTOCLICKER_INSTANCE` overrides it (tests).
pub const INSTANCE_ENV: &str = "AUTOCLICKER_INSTANCE";
const DEFAULT_INSTANCE: &str = "Autoclicker";
/// How long the end of the Windows session waits for shutdown to finish.
const SESSION_END_GRACE: Duration = Duration::from_secs(4);

/// Set once shutdown has completed (waited on by the session-end handler).
pub type ShutdownSignal = Arc<(Mutex<bool>, Condvar)>;

/// Startup options.
#[derive(Clone, Debug)]
pub struct GuiOptions {
    /// Settings file; `None` disables persistence.
    pub config_path: Option<PathBuf>,
    pub waiter: WaiterKind,
    /// Log directory opened by "Logs" (`None` disables the button's target).
    pub log_dir: Option<PathBuf>,
}

impl Default for GuiOptions {
    fn default() -> Self {
        Self {
            config_path: config_store::default_path(),
            waiter: WaiterKind::HighResolution,
            log_dir: logging::default_dir(),
        }
    }
}

/// The running application window.
pub struct App {
    ui: MainWindow,
    ctrl: Rc<Controller>,
}

struct Controller {
    ui: slint::Weak<MainWindow>,
    runtime: RefCell<Runtime>,
    /// The last valid, applied (and saved) configuration.
    config: RefCell<Config>,
    path: Option<PathBuf>,
    snapshot: RefCell<Snapshot>,
    pick_timer: Timer,
    pick_remaining: Cell<i32>,
    notice_timer: Timer,
    capturing: Cell<Option<HotkeyAction>>,
    about: RefCell<Option<AboutWindow>>,
    report: Cell<Option<ShutdownReport>>,
    closed: Cell<bool>,
    /// `None` if the notification-area icon could not be created; closing
    /// the window then exits instead of hiding.
    tray: Option<AppTray>,
    window_visible: Cell<bool>,
    log_dir: Option<PathBuf>,
    shutdown_signal: ShutdownSignal,
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

impl App {
    pub fn new(options: GuiOptions) -> Result<Self, String> {
        // Before any window exists, so coordinates are physical pixels.
        let dpi_warning = enable_per_monitor_dpi_awareness()
            .err()
            .map(|e| format!("Per-monitor DPI awareness is unavailable ({e}); target positions may be off on scaled displays."));

        let mut warnings: Vec<String> = Vec::new();
        let mut config = match &options.config_path {
            Some(path) => {
                let loaded = config_store::load(path);
                warnings.extend(loaded.warning);
                loaded.config
            }
            None => {
                warnings.push("No settings folder was found; changes will not be saved.".into());
                Config::default()
            }
        };
        if let Some(p) = config.click.target
            && WindowsInput::new().monitor_bounds(p).is_err()
        {
            warnings.push(format!(
                "The saved target ({}, {}) is not on any connected monitor and was cleared.",
                p.x, p.y
            ));
            config.click.target = None;
        }
        warnings.extend(dpi_warning);
        for w in &warnings {
            warn!("{}", w.replace('\n', " "));
        }
        let validated = config
            .validate()
            .map_err(|e| format!("internal error: loaded settings are invalid: {e}"))?;

        let ui = MainWindow::new().map_err(|e| format!("could not create the window: {e}"))?;
        let weak = ui.as_weak();
        let wake = {
            let weak = weak.clone();
            Box::new(move || {
                let _ = weak.upgrade_in_event_loop(|ui| ui.invoke_status_ready());
            })
        };
        let on_exit = {
            let weak = weak.clone();
            Arc::new(move || {
                let _ = weak.upgrade_in_event_loop(|ui| ui.invoke_exit_requested());
            })
        };
        let runtime = Runtime::start(&validated, options.waiter, wake, on_exit)
            .map_err(|e| format!("could not start: {e}"))?;
        if let Some(e) = runtime.timer_fallback() {
            warnings.push(format!(
                "The high-resolution timer is unavailable ({e}); click timing will be less precise."
            ));
        }
        if let Some(p) = config.click.target {
            let _ = runtime.send(Command::SetTarget(p));
        }
        for r in runtime.registrations() {
            match &r.result {
                Ok(()) => info!("hotkey {} = {}", r.action, r.hotkey),
                Err(e) => warn!(
                    "hotkey {} = {} could not be registered: {e}",
                    r.action, r.hotkey
                ),
            }
        }
        let tray = match AppTray::new() {
            Ok(t) => Some(t),
            Err(e) => {
                warn!("notification-area icon unavailable ({e}); closing the window will exit");
                None
            }
        };

        let mut snapshot = Snapshot::new(config.click.mode);
        snapshot.target = config.click.target;
        let ctrl = Rc::new(Controller {
            ui: weak,
            runtime: RefCell::new(runtime),
            config: RefCell::new(config),
            path: options.config_path,
            snapshot: RefCell::new(snapshot),
            pick_timer: Timer::default(),
            pick_remaining: Cell::new(0),
            notice_timer: Timer::default(),
            capturing: Cell::new(None),
            about: RefCell::new(None),
            report: Cell::new(None),
            closed: Cell::new(false),
            tray,
            window_visible: Cell::new(false),
            log_dir: options.log_dir,
            shutdown_signal: Arc::new((Mutex::new(false), Condvar::new())),
        });
        ctrl.populate(&ui, &warnings);
        Controller::connect(&ctrl, &ui);
        Ok(Self { ui, ctrl })
    }

    pub fn ui(&self) -> &MainWindow {
        &self.ui
    }

    /// The configuration currently applied (and saved).
    pub fn config(&self) -> Config {
        self.ctrl.config.borrow().clone()
    }

    /// The latest engine snapshot received by the UI.
    pub fn snapshot(&self) -> Snapshot {
        self.ctrl.snapshot.borrow().clone()
    }

    /// Whether hotkeys are currently registered (they are suspended while a
    /// new binding is being recorded).
    pub fn hotkeys_active(&self) -> bool {
        self.ctrl.runtime.borrow().hotkeys_active()
    }

    /// Reads the cursor into the target immediately (what the countdown of
    /// "Pick target" ends with).
    pub fn capture_target_now(&self) {
        self.ctrl.finish_pick();
    }

    /// Runs the shutdown path used by closing the window.
    pub fn close(&self) -> Option<ShutdownReport> {
        self.ctrl.close()
    }

    pub fn shutdown_report(&self) -> Option<ShutdownReport> {
        self.ctrl.report.get()
    }

    /// The notification-area icon, if it could be created.
    pub fn tray(&self) -> Option<&AppTray> {
        self.ctrl.tray.as_ref()
    }

    pub fn window_visible(&self) -> bool {
        self.ctrl.window_visible.get()
    }

    /// Shows the main window and brings it to the front.
    pub fn show_window(&self) {
        self.ctrl.show_window();
    }

    /// Set (and notified) once shutdown has completed.
    pub fn shutdown_signal(&self) -> ShutdownSignal {
        self.ctrl.shutdown_signal.clone()
    }

    /// Shows the window and runs until an explicit exit (tray Exit, the Exit
    /// button, the exit hotkey or the end of the session). Shutdown has
    /// completed when this returns.
    pub fn run(&self) -> Result<Option<ShutdownReport>, slint::PlatformError> {
        self.ctrl.show_window();
        slint::run_event_loop_until_quit()?;
        Ok(self.ctrl.close())
    }
}

impl Controller {
    fn populate(&self, ui: &MainWindow, warnings: &[String]) {
        let config = self.config.borrow();
        ui.set_fixed_mode(config.click.mode == TargetMode::Fixed);
        ui.set_keep_on_top(config.window.always_on_top);
        self.set_fields(ui, &TimingFields::from_config(&config.click));
        ui.set_config_warning(warnings.join("\n").into());
        drop(config);
        self.refresh_hotkeys(ui);
        self.render_status(ui);
    }

    fn set_fields(&self, ui: &MainWindow, f: &TimingFields) {
        ui.set_interval_text(f.interval.as_str().into());
        ui.set_jitter_text(f.jitter.as_str().into());
        ui.set_hold_text(f.hold.as_str().into());
        ui.set_radius_text(f.radius.as_str().into());
        ui.set_rate_hint(form::rate_hint(f).into());
        ui.set_settings_error(SharedString::new());
        ui.set_settings_pending(false);
    }

    fn connect(this: &Rc<Self>, ui: &MainWindow) {
        macro_rules! on {
            ($setter:ident, |$c:ident $(, $arg:ident : $ty:ty)*| $body:expr) => {{
                let weak: Weak<Controller> = Rc::downgrade(this);
                ui.$setter(move |$($arg: $ty),*| {
                    if let Some($c) = weak.upgrade() {
                        $body
                    }
                });
            }};
        }
        on!(on_status_ready, |c| c.status_ready());
        on!(on_exit_requested, |c| {
            c.close();
        });
        on!(on_start_stop_clicked, |c| c.start_stop());
        on!(on_mode_selected, |c, fixed: bool| c.select_mode(fixed));
        on!(on_pick_target_clicked, |c| c.pick_target());
        on!(on_settings_edited, |c| c.settings_edited());
        on!(on_settings_commit, |c| c.settings_commit());
        on!(on_hotkey_record, |c, index: i32| c.hotkey_record(index));
        on!(on_hotkey_key, |c,
                            text: SharedString,
                            ctrl: bool,
                            alt: bool,
                            shift: bool| {
            c.hotkey_key(&text, Modifiers { ctrl, alt, shift })
        });
        on!(on_hotkey_cancel, |c| c.end_capture());
        on!(on_hotkey_clear, |c, index: i32| c.hotkey_clear(index));
        on!(on_keep_on_top_toggled, |c, on: bool| c.keep_on_top(on));
        on!(on_about_clicked, |c| c.show_about());
        on!(on_warning_dismissed, |c| c
            .with_ui(|ui| ui.set_config_warning(SharedString::new())));
        on!(on_open_logs_clicked, |c| c.open_logs());
        on!(on_exit_clicked, |c| {
            c.close();
        });
        on!(on_show_requested, |c| c.show_window());

        let weak = Rc::downgrade(this);
        ui.window().on_close_requested(move || {
            if let Some(c) = weak.upgrade() {
                if c.tray.is_some() && !c.closed.get() {
                    c.hide_window();
                } else {
                    c.close();
                }
            }
            CloseRequestResponse::HideWindow
        });

        if let Some(tray) = &this.tray {
            macro_rules! tray_on {
                ($setter:ident, |$c:ident| $body:expr) => {{
                    let weak: Weak<Controller> = Rc::downgrade(this);
                    tray.$setter(move || {
                        if let Some($c) = weak.upgrade() {
                            $body
                        }
                    });
                }};
            }
            tray_on!(on_show_window, |c| c.show_window());
            tray_on!(on_toggle_window, |c| {
                if c.window_visible.get() {
                    c.hide_window();
                } else {
                    c.show_window();
                }
            });
            tray_on!(on_start_stop, |c| c.start_stop());
            tray_on!(on_open_logs, |c| c.open_logs());
            tray_on!(on_exit, |c| {
                c.close();
            });
        }
    }

    fn with_ui(&self, f: impl FnOnce(&MainWindow)) {
        if let Some(ui) = self.ui.upgrade() {
            f(&ui);
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Err(e) = config_store::save(path, &self.config.borrow()) {
            error!(
                "could not save settings to {}: {e}",
                logging::display_path(path)
            );
            self.with_ui(|ui| {
                ui.set_config_warning(
                    format!("Could not save settings to {}: {e}", path.display()).into(),
                )
            });
        }
    }

    // ---------------------------------------------------------------- status

    fn status_ready(&self) {
        let update = self.runtime.borrow().take_status();
        let target = update.snapshot.target;
        self.log_transition(&update.snapshot);
        for notice in &update.notices {
            match notice {
                autoclicker_core::status::Notice::ReleaseRecovered { .. } => warn!("{notice}"),
                _ => info!("{notice}"),
            }
        }
        *self.snapshot.borrow_mut() = update.snapshot;
        if let Some(notice) = update.notices.last() {
            self.show_notice(capitalize(&notice.to_string()));
        }
        let target_changed = {
            let mut config = self.config.borrow_mut();
            let changed = target.is_some() && config.click.target != target;
            if changed {
                config.click.target = target;
            }
            changed
        };
        if target_changed {
            self.save();
        }
        self.with_ui(|ui| self.render_status(ui));
    }

    /// Logs state changes (not individual clicks).
    fn log_transition(&self, new: &Snapshot) {
        let old = self.snapshot.borrow();
        if old.state == new.state && old.fault == new.fault {
            return;
        }
        match (&new.state, &new.fault) {
            (RunState::Faulted, Some(fault)) => error!("clicking stopped by an error: {fault}"),
            (RunState::Running, _) => info!("clicking started ({} mode)", new.mode),
            (RunState::WaitingForModifiers, _) => {
                info!("waiting for modifier keys before starting")
            }
            (RunState::Idle, _) if old.state == RunState::Running => info!(
                "clicking stopped after {} clicks (skipped {}, max lateness {:.1} ms)",
                new.run_clicks,
                new.skipped,
                new.max_lateness.as_secs_f64() * 1000.0
            ),
            _ => {}
        }
    }

    fn render_status(&self, ui: &MainWindow) {
        let view = form::status_view(&self.snapshot.borrow());
        let active = matches!(view.status, Status::Running | Status::Waiting);
        if let Some(tray) = &self.tray {
            tray.set_running(active);
        }
        ui.set_status(match view.status {
            Status::Stopped => RunStatus::Stopped,
            Status::Waiting => RunStatus::Waiting,
            Status::Running => RunStatus::Running,
            Status::Error => RunStatus::Error,
        });
        ui.set_status_headline(view.headline.into());
        ui.set_status_detail(view.detail.into());
        ui.set_error_text(view.error.into());
        ui.set_window_title(view.window_title.into());
        ui.set_target_text(view.target.into());
    }

    fn show_notice(&self, text: String) {
        self.with_ui(|ui| ui.set_notice_text(text.into()));
        let weak = self.ui.clone();
        self.notice_timer
            .start(slint::TimerMode::SingleShot, NOTICE_DURATION, move || {
                if let Some(ui) = weak.upgrade() {
                    ui.set_notice_text(SharedString::new());
                }
            });
    }

    fn send(&self, command: Command) {
        if self.runtime.borrow().send(command).is_err() {
            self.show_notice("The click engine is not running.".into());
        }
    }

    // ---------------------------------------------------------------- actions

    fn start_stop(&self) {
        let active = matches!(
            self.snapshot.borrow().state,
            RunState::Running | RunState::WaitingForModifiers
        );
        self.send(if active {
            Command::Stop
        } else {
            Command::Start {
                release_first: Modifiers::NONE,
            }
        });
    }

    fn apply_click(&self, click: ClickConfig) -> Result<(), Vec<String>> {
        let settings = click.validated().map_err(|e| {
            e.0.iter()
                .map(|e| capitalize(&e.to_string()))
                .collect::<Vec<_>>()
        })?;
        self.send(Command::UpdateSettings(settings));
        self.config.borrow_mut().click = click;
        self.save();
        Ok(())
    }

    fn select_mode(&self, fixed: bool) {
        let mut click = self.config.borrow().click.clone();
        click.mode = if fixed {
            TargetMode::Fixed
        } else {
            TargetMode::Cursor
        };
        if click == self.config.borrow().click {
            return;
        }
        match self.apply_click(click) {
            Ok(()) => self.with_ui(|ui| ui.set_fixed_mode(fixed)),
            Err(errors) => self.with_ui(|ui| ui.set_settings_error(errors.join("\n").into())),
        }
        if !fixed {
            self.cancel_pick();
        }
    }

    fn fields(&self, ui: &MainWindow) -> TimingFields {
        TimingFields {
            interval: ui.get_interval_text().into(),
            jitter: ui.get_jitter_text().into(),
            hold: ui.get_hold_text().into(),
            radius: ui.get_radius_text().into(),
        }
    }

    fn settings_edited(&self) {
        self.with_ui(|ui| {
            let fields = self.fields(ui);
            ui.set_rate_hint(form::rate_hint(&fields).into());
            let base = self.config.borrow().click.clone();
            match form::parse_timing(&fields, &base) {
                Ok(c) => {
                    ui.set_settings_error(SharedString::new());
                    ui.set_settings_pending(c != base);
                }
                Err(errors) => {
                    ui.set_settings_error(errors.join("\n").into());
                    ui.set_settings_pending(true);
                }
            }
        });
    }

    fn settings_commit(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let fields = self.fields(&ui);
        let base = self.config.borrow().click.clone();
        match form::parse_timing(&fields, &base) {
            Ok(c) if c == base => {
                ui.set_settings_error(SharedString::new());
                ui.set_settings_pending(false);
            }
            Ok(c) => match self.apply_click(c) {
                Ok(()) => {
                    ui.set_settings_error(SharedString::new());
                    ui.set_settings_pending(false);
                }
                Err(errors) => ui.set_settings_error(errors.join("\n").into()),
            },
            Err(errors) => ui.set_settings_error(
                format!(
                    "Not applied - the previous settings stay active.\n{}",
                    errors.join("\n")
                )
                .into(),
            ),
        }
    }

    // ---------------------------------------------------------------- target

    fn pick_target(self: &Rc<Self>) {
        if self.pick_remaining.get() > 0 {
            self.cancel_pick();
            return;
        }
        self.pick_remaining.set(PICK_COUNTDOWN_SECONDS);
        self.with_ui(|ui| ui.set_pick_countdown(PICK_COUNTDOWN_SECONDS));
        let weak = Rc::downgrade(self);
        self.pick_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                let Some(c) = weak.upgrade() else { return };
                let left = c.pick_remaining.get() - 1;
                if left <= 0 {
                    c.finish_pick();
                } else {
                    c.pick_remaining.set(left);
                    c.with_ui(|ui| ui.set_pick_countdown(left));
                }
            },
        );
    }

    fn finish_pick(&self) {
        self.cancel_pick();
        self.send(Command::SetTargetFromCursor);
    }

    fn cancel_pick(&self) {
        self.pick_timer.stop();
        self.pick_remaining.set(0);
        self.with_ui(|ui| ui.set_pick_countdown(0));
    }

    // ---------------------------------------------------------------- hotkeys

    fn refresh_hotkeys(&self, ui: &MainWindow) {
        let config = self.config.borrow();
        let runtime = self.runtime.borrow();
        let rows: Vec<HotkeyRow> = HotkeyAction::ALL
            .iter()
            .map(|&action| {
                let binding = config.hotkeys.get(action);
                let problem = runtime
                    .registrations()
                    .iter()
                    .find(|r| r.action == action && Some(r.hotkey) == binding)
                    .and_then(|r| r.result.as_ref().err())
                    .map(|e| {
                        if e.code == 1409 {
                            "Unavailable: another program uses this combination".to_string()
                        } else {
                            format!("Unavailable: {e}")
                        }
                    })
                    .unwrap_or_default();
                HotkeyRow {
                    label: form::action_label(action).into(),
                    binding: form::binding_text(binding).into(),
                    problem: problem.into(),
                    bound: binding.is_some(),
                }
            })
            .collect();
        ui.set_hotkeys(ModelRc::new(VecModel::from(rows)));
        ui.set_toggle_hint(
            config
                .hotkeys
                .toggle
                .map(|h| format!("or press {h}"))
                .unwrap_or_default()
                .into(),
        );
    }

    fn hotkey_record(&self, index: i32) {
        let Some(&action) = usize::try_from(index)
            .ok()
            .and_then(|i| HotkeyAction::ALL.get(i))
        else {
            return;
        };
        self.capturing.set(Some(action));
        // Unregister so the keys reach this window instead of triggering actions.
        self.runtime.borrow_mut().suspend_hotkeys();
        self.with_ui(|ui| ui.set_hotkeys_error(SharedString::new()));
    }

    fn end_capture(&self) {
        if self.capturing.take().is_none() {
            return;
        }
        if let Err(e) = self.runtime.borrow_mut().resume_hotkeys() {
            self.with_ui(|ui| {
                ui.set_hotkeys_error(format!("Could not restore the hotkeys: {e}").into())
            });
        }
        self.with_ui(|ui| {
            ui.set_capturing_index(-1);
            self.refresh_hotkeys(ui);
        });
    }

    fn hotkey_key(&self, text: &str, modifiers: Modifiers) {
        let Some(action) = self.capturing.get() else {
            return;
        };
        match form::interpret_key(text, modifiers) {
            KeyInput::ModifierOnly => {}
            KeyInput::Cancel => self.end_capture(),
            KeyInput::Unsupported(what) => self.with_ui(|ui| {
                ui.set_hotkeys_error(
                    format!(
                        "{} can't be used as a hotkey. Use F1-F24, Pause, or a letter, digit or \
                         navigation key together with Ctrl or Alt.",
                        capitalize(&what)
                    )
                    .into(),
                )
            }),
            KeyInput::Key(key) => {
                let hotkey = Hotkey::new(modifiers, key);
                match self.try_bind(action, Some(hotkey)) {
                    Ok(()) => self.end_capture(),
                    Err(BindError::Invalid(msg)) => {
                        // Keep recording so the user can try another combination.
                        self.with_ui(|ui| ui.set_hotkeys_error(msg.into()));
                    }
                    Err(BindError::Registration(msg)) => {
                        // The previous bindings are active again; stop recording.
                        self.capturing.set(None);
                        self.with_ui(|ui| {
                            ui.set_capturing_index(-1);
                            self.refresh_hotkeys(ui);
                            ui.set_hotkeys_error(msg.into());
                        });
                    }
                }
            }
        }
    }

    fn hotkey_clear(&self, index: i32) {
        let Some(&action) = usize::try_from(index)
            .ok()
            .and_then(|i| HotkeyAction::ALL.get(i))
        else {
            return;
        };
        let result = self.try_bind(action, None);
        self.with_ui(|ui| {
            self.refresh_hotkeys(ui);
            ui.set_hotkeys_error(match result {
                Ok(()) => SharedString::new(),
                Err(BindError::Invalid(m) | BindError::Registration(m)) => m.into(),
            });
        });
    }

    /// Validates the new binding with the core rules, registers it with the
    /// platform (restoring the previous bindings on failure) and saves.
    fn try_bind(&self, action: HotkeyAction, hotkey: Option<Hotkey>) -> Result<(), BindError> {
        let mut candidate = self.config.borrow().clone();
        candidate.hotkeys.set(action, hotkey);
        let validated = candidate.validate().map_err(|e| {
            BindError::Invalid(
                e.0.iter()
                    .map(|e| capitalize(&e.to_string()) + ".")
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
        })?;
        let rebind = self.runtime.borrow_mut().rebind(validated.hotkeys);
        match rebind {
            Ok(()) => {
                info!("hotkey {action} set to {}", form::binding_text(hotkey));
                *self.config.borrow_mut() = candidate;
                self.save();
                Ok(())
            }
            Err(RebindError::Conflicts(failed)) => {
                for r in &failed {
                    if let Err(e) = &r.result {
                        warn!(
                            "hotkey {} = {} could not be registered: {e}; previous bindings restored",
                            r.action, r.hotkey
                        );
                    }
                }
                Err(BindError::Registration(
                failed
                    .iter()
                    .map(|r| match &r.result {
                        Err(e) if e.code == 1409 => format!(
                            "{} is already used by another program. Your previous hotkeys are still active.",
                            r.hotkey
                        ),
                        Err(e) => format!("{} could not be registered: {e}.", r.hotkey),
                        Ok(()) => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ))
            }
            Err(RebindError::Thread(e)) => Err(BindError::Registration(format!(
                "The hotkeys could not be updated: {e}."
            ))),
        }
    }

    // ---------------------------------------------------------------- window

    fn keep_on_top(&self, on: bool) {
        self.config.borrow_mut().window.always_on_top = on;
        self.save();
    }

    fn show_about(&self) {
        let mut about = self.about.borrow_mut();
        if about.is_none() {
            let Ok(window) = AboutWindow::new() else {
                self.show_notice("The About window could not be opened.".into());
                return;
            };
            window.set_version(env!("CARGO_PKG_VERSION").into());
            let weak = window.as_weak();
            window.on_close_clicked(move || {
                if let Some(w) = weak.upgrade() {
                    let _ = w.hide();
                }
            });
            *about = Some(window);
        }
        if let Some(window) = about.as_ref() {
            let _ = window.show();
        }
    }

    fn show_window(&self) {
        if self.closed.get() {
            return;
        }
        let Some(ui) = self.ui.upgrade() else { return };
        self.render_status(&ui);
        if let Err(e) = ui.show() {
            error!("could not show the window: {e}");
            return;
        }
        self.window_visible.set(true);
        if let Some(tray) = &self.tray {
            tray.set_window_visible(true);
        }
        activate_own_window(form::APP_NAME);
    }

    /// Hides the window to the notification area. Clicking, hotkeys and the
    /// tray keep working.
    fn hide_window(&self) {
        if self.capturing.get().is_some() {
            self.end_capture();
        }
        self.cancel_pick();
        self.with_ui(|ui| {
            let _ = ui.hide();
        });
        self.window_visible.set(false);
        if let Some(tray) = &self.tray {
            tray.set_window_visible(false);
        }
        info!("window hidden to the notification area");
    }

    fn open_logs(&self) {
        let Some(dir) = &self.log_dir else {
            self.show_notice("Logging is not available.".into());
            return;
        };
        let _ = std::fs::create_dir_all(dir);
        if let Err(e) = std::process::Command::new("explorer").arg(dir).spawn() {
            warn!("could not open the log folder: {e}");
            self.show_notice(format!("Could not open the log folder: {e}"));
        }
    }

    /// The single shutdown path (tray Exit, Exit button, exit hotkey, end of
    /// session): stop the engine and release any held button, unregister
    /// hotkeys and join their threads, remove the tray icon, close the
    /// windows and leave the event loop. Runs once.
    fn close(&self) -> Option<ShutdownReport> {
        if self.closed.replace(true) {
            return self.report.get();
        }
        info!("shutting down");
        self.cancel_pick();
        self.notice_timer.stop();
        let report = self.runtime.borrow_mut().shutdown();
        self.report.set(report);
        match report {
            Some(r) if r.is_clean() => info!("engine and hotkey threads stopped cleanly"),
            Some(r) => warn!("shutdown was not clean: {r:?}"),
            None => {}
        }
        if let Some(tray) = &self.tray {
            let _ = tray.hide();
        }
        if let Some(about) = self.about.borrow().as_ref() {
            let _ = about.hide();
        }
        self.with_ui(|ui| {
            let _ = ui.hide();
        });
        self.window_visible.set(false);
        let (done, cond) = &*self.shutdown_signal;
        *done.lock().unwrap_or_else(PoisonError::into_inner) = true;
        cond.notify_all();
        let _ = slint::quit_event_loop();
        report
    }
}

enum BindError {
    /// Rejected by the core rules (nothing was changed).
    Invalid(String),
    /// Rejected by Windows; the previous bindings were restored.
    Registration(String),
}

/// Entry point of the desktop application.
///
/// Startup: logging, single-instance check (a second instance asks the first
/// to show itself and exits), the window and engine, the system-events
/// thread. Shutdown (after an explicit exit): engine and hotkeys (inside the
/// controller), tray icon, system-events thread, instance lock, log flush.
pub fn run() -> ExitCode {
    let log_dir = logging::default_dir();
    let logger = logging::init(log_dir.as_deref());
    install_panic_hook();
    info!(
        "Autoclicker {} starting (log {})",
        env!("CARGO_PKG_VERSION"),
        logger.map_or_else(
            || "disabled".to_string(),
            |l| logging::display_path(&l.path())
        )
    );

    let instance = std::env::var(INSTANCE_ENV).unwrap_or_else(|_| DEFAULT_INSTANCE.to_string());
    let lock = match acquire_instance(&instance) {
        Ok(Acquire::Acquired(lock)) => Some(lock),
        Ok(Acquire::AlreadyRunning) => {
            info!("another instance is already running; asked it to show its window");
            log::logger().flush();
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            warn!("single-instance check failed ({e}); continuing without it");
            None
        }
    };

    let options = GuiOptions {
        log_dir,
        ..GuiOptions::default()
    };
    if let Some(p) = &options.config_path {
        info!("settings file {}", logging::display_path(p));
    }
    let app = match App::new(options) {
        Ok(app) => app,
        Err(e) => {
            error!("could not start: {e}");
            log::logger().flush();
            autoclicker_platform::show_error_message(
                "Autoclicker",
                &format!("Autoclicker could not start: {e}"),
            );
            return ExitCode::FAILURE;
        }
    };

    let events = lock.as_ref().and_then(|lock| {
        let show = app.ui().as_weak();
        let exit = app.ui().as_weak();
        let done = app.shutdown_signal();
        let started = SystemEvents::start(
            lock,
            move || {
                let _ = show.upgrade_in_event_loop(|ui| ui.invoke_show_requested());
            },
            move || {
                info!("the Windows session is ending");
                let _ = exit.upgrade_in_event_loop(|ui| ui.invoke_exit_requested());
                let (flag, cond) = &*done;
                let guard = flag.lock().unwrap_or_else(PoisonError::into_inner);
                let _ = cond.wait_timeout_while(guard, SESSION_END_GRACE, |done| !*done);
                log::logger().flush();
            },
        );
        started
            .inspect_err(|e| warn!("system-events thread unavailable ({e})"))
            .ok()
    });

    let result = app.run();
    if let Some(events) = events {
        let exit = events.shutdown(crate::runtime::JOIN_TIMEOUT);
        if exit != autoclicker_core::engine::ThreadExit::Clean {
            warn!("system-events thread: {exit:?}");
        }
    }
    drop(app); // drops the tray icon and windows
    drop(lock);
    let code = match result {
        Ok(Some(report)) if report.is_clean() => ExitCode::SUCCESS,
        Ok(Some(_)) => ExitCode::FAILURE,
        Ok(None) => ExitCode::SUCCESS,
        Err(e) => {
            error!("event loop failed: {e}");
            ExitCode::FAILURE
        }
    };
    info!("exited");
    log::logger().flush();
    code
}

/// Logs panics (with location) before the default handler runs.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        error!("panic: {info}");
        log::logger().flush();
        previous(info);
    }));
}
