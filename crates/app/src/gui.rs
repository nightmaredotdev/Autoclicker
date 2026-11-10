//! The desktop window: wires the Slint UI to the runtime and the settings.
//!
//! Threading: everything here runs on the UI thread. The engine thread wakes
//! the UI through `Weak::upgrade_in_event_loop` (at most one wake-up is ever
//! queued, thanks to the status slot), and the exit hotkey does the same.
//! Tray menu callbacks run on the UI thread too, and the system-events thread
//! (second instance, session end) also goes through the event loop, as does
//! the outcome of "Pick target" (from the picker's hook thread), so all
//! actions are serialized with each other and with shutdown.
//!
//! Closing the window hides it to the notification area (if the tray icon
//! could be created). Every exit path - tray Exit, the Exit button, the exit
//! hotkey and the end of the Windows session - uses one shutdown
//! ([`Controller::close`]) that runs once.

#![forbid(unsafe_code)]

use crate::form::{self, IntervalUnit, KeyInput, Status, TimingFields};
use crate::runtime::{RebindError, Runtime, ShutdownReport, WaiterKind};
use crate::ui::{AboutWindow, AppTray, HotkeyRow, MainWindow, RunStatus};
use crate::{config_store, logging};
use autoclicker_core::backend::InputBackend;
use autoclicker_core::config::{ClickConfig, Config, HotkeyAction, TargetMode};
use autoclicker_core::engine::Command;
use autoclicker_core::geometry::Point;
use autoclicker_core::keys::{Hotkey, Modifiers};
use autoclicker_core::status::{RunState, Snapshot};
use autoclicker_platform::{
    Acquire, PickOutcome, PointPicker, SystemEvents, WindowsInput, acquire_instance,
    activate_own_window, enable_per_monitor_dpi_awareness,
};
use log::{error, info, warn};
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, SharedString, Timer, VecModel};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::{Rc, Weak};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

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
    /// The mouse hook of "Pick target". It may outlive the pick briefly, to
    /// swallow the release of the deciding click.
    picker: RefCell<Option<PointPicker>>,
    /// Where the current pick's outcome arrives; `Some` while picking.
    pick_slot: RefCell<Option<PickSlot>>,
    notice_timer: Timer,
    capturing: Cell<Option<HotkeyAction>>,
    /// How the interval is shown (display only; never saved).
    interval_unit: Cell<IntervalUnit>,
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

/// Receives the outcome of one pick from the picker thread.
type PickSlot = Arc<Mutex<Option<PickOutcome>>>;

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
        if config.hotkeys.had_obsolete_bindings() {
            // Pre-1.0 files may contain separate start/stop bindings: drop
            // them (once) and say so, keeping every other setting.
            config.hotkeys.clear_obsolete_bindings();
            warnings.push(
                "Separate Start and Stop hotkeys are no longer supported and were removed from \
                 your settings. Use the Toggle start/stop hotkey instead; your other settings \
                 are unchanged."
                    .into(),
            );
            if let Some(path) = &options.config_path
                && let Err(e) = config_store::save(path, &config)
            {
                warnings.push(format!("Could not update the settings file: {e}"));
            }
        }
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
            picker: RefCell::new(None),
            pick_slot: RefCell::new(None),
            notice_timer: Timer::default(),
            capturing: Cell::new(None),
            interval_unit: Cell::new(IntervalUnit::Millis),
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

    /// Whether "Pick target" is waiting for a click.
    pub fn picking(&self) -> bool {
        self.ctrl.pick_slot.borrow().is_some()
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
        self.set_fields(
            ui,
            &TimingFields::from_config(&config.click, self.interval_unit.get()),
        );
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
        ui.set_interval_in_cps(f.interval_unit == IntervalUnit::Cps);
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
        on!(on_pick_decided, |c| c.pick_decided());
        on!(on_settings_edited, |c| c.settings_edited());
        on!(on_settings_commit, |c| c.settings_commit());
        on!(on_interval_unit_clicked, |c| c.toggle_interval_unit());
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
        self.commit_pending_edits();
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
        self.commit_pending_edits();
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
            interval_unit: self.interval_unit.get(),
            jitter: ui.get_jitter_text().into(),
            hold: ui.get_hold_text().into(),
            radius: ui.get_radius_text().into(),
        }
    }

    fn settings_edited(&self) {
        self.with_ui(|ui| {
            let fields = self.fields(ui);
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

    /// Commits a timing field that is still being edited. Buttons do not take
    /// keyboard focus in Slint, so clicking one would otherwise leave the edit
    /// pending; every button and tray action calls this first. A commit with
    /// unchanged values does nothing (no write, no message).
    fn commit_pending_edits(&self) {
        if self
            .ui
            .upgrade()
            .is_some_and(|ui| ui.get_settings_pending())
        {
            self.settings_commit();
        }
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
                    // Show the stored values (e.g. a CPS entry as the exact
                    // rate of the whole-millisecond interval that was saved).
                    let config = self.config.borrow().click.clone();
                    self.set_fields(
                        &ui,
                        &TimingFields::from_config(&config, self.interval_unit.get()),
                    );
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

    /// Switches the interval between milliseconds and clicks per second. A
    /// pending edit is committed first; if it is invalid the unit stays (and
    /// the error stays visible). The shown value is always derived from the
    /// stored milliseconds, so toggling can never drift.
    fn toggle_interval_unit(&self) {
        self.commit_pending_edits();
        let Some(ui) = self.ui.upgrade() else { return };
        if ui.get_settings_pending() {
            return;
        }
        self.interval_unit.set(self.interval_unit.get().toggled());
        let config = self.config.borrow().click.clone();
        self.set_fields(
            &ui,
            &TimingFields::from_config(&config, self.interval_unit.get()),
        );
    }

    // ---------------------------------------------------------------- target

    /// "Pick target": the next primary click anywhere on the screen (outside
    /// this window) sets the fixed position; a secondary click, or the button
    /// again (then labelled Cancel), cancels. There is no timeout. The
    /// deciding click is swallowed, and the engine's own clicks are ignored
    /// (see [`PointPicker`]).
    fn pick_target(&self) {
        self.commit_pending_edits();
        if self.pick_slot.borrow().is_some() {
            self.cancel_pick();
            self.show_notice("Pick cancelled; the position is unchanged.".into());
            return;
        }
        if self.config.borrow().click.mode != TargetMode::Fixed {
            return;
        }
        // A previous picker that is still swallowing a release ends here.
        self.picker.borrow_mut().take();
        let slot: PickSlot = Arc::new(Mutex::new(None));
        let started = {
            let slot = slot.clone();
            let weak = self.ui.clone();
            PointPicker::start(move |outcome| {
                *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(outcome);
                let _ = weak.upgrade_in_event_loop(|ui| ui.invoke_pick_decided());
            })
        };
        match started {
            Ok(picker) => {
                *self.picker.borrow_mut() = Some(picker);
                *self.pick_slot.borrow_mut() = Some(slot);
                self.with_ui(|ui| ui.set_picking(true));
            }
            Err(e) => {
                warn!("could not start picking a position: {e}");
                self.show_notice(format!("Could not start picking a position ({e})."));
            }
        }
    }

    /// The picker's outcome arrived (on the UI thread). An outcome of a pick
    /// that was cancelled in the meantime finds no slot and is ignored.
    fn pick_decided(&self) {
        let outcome = {
            let current = self.pick_slot.borrow();
            match current.as_ref() {
                Some(slot) => slot.lock().unwrap_or_else(PoisonError::into_inner).take(),
                None => None,
            }
        };
        let Some(outcome) = outcome else { return };
        // The picker removes its hook by itself once the release is swallowed.
        self.pick_slot.borrow_mut().take();
        self.with_ui(|ui| ui.set_picking(false));
        match outcome {
            PickOutcome::Picked(p) => self.set_picked_target(p),
            PickOutcome::Cancelled => {
                self.show_notice("Pick cancelled; the position is unchanged.".into());
            }
        }
    }

    fn set_picked_target(&self, p: Point) {
        self.send(Command::SetTarget(p));
        // Show and save it at once; the engine's status update confirms it.
        self.snapshot.borrow_mut().target = Some(p);
        let changed = {
            let mut config = self.config.borrow_mut();
            let changed = config.click.target != Some(p);
            config.click.target = Some(p);
            changed
        };
        if changed {
            self.save();
        }
        info!("target set to {p}");
        self.show_notice(format!("Target set to {p}"));
        self.with_ui(|ui| self.render_status(ui));
    }

    /// Ends a pick in progress (and removes its hook) without changing the
    /// position.
    fn cancel_pick(&self) {
        self.pick_slot.borrow_mut().take();
        self.picker.borrow_mut().take();
        self.with_ui(|ui| ui.set_picking(false));
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
        ui.set_toggle_key(
            config
                .hotkeys
                .toggle
                .map(|h| h.to_string())
                .unwrap_or_default()
                .into(),
        );
    }

    fn hotkey_record(&self, index: i32) {
        self.commit_pending_edits();
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
        self.commit_pending_edits();
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
        self.commit_pending_edits();
        self.config.borrow_mut().window.always_on_top = on;
        self.save();
    }

    fn show_about(&self) {
        self.commit_pending_edits();
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
        self.commit_pending_edits();
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
        self.commit_pending_edits();
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
        if self.closed.get() {
            return self.report.get();
        }
        self.commit_pending_edits();
        self.closed.set(true);
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
