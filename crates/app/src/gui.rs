//! The desktop window: wires the Slint UI to the runtime and the settings.
//!
//! Threading: everything here runs on the UI thread. The engine thread wakes
//! the UI through `Weak::upgrade_in_event_loop` (at most one wake-up is ever
//! queued, thanks to the status slot), and the exit hotkey does the same.
//! Closing the window and the exit hotkey share one shutdown path
//! ([`Controller::close`]) that runs once.

#![forbid(unsafe_code)]

use crate::config_store;
use crate::form::{self, KeyInput, Status, TimingFields};
use crate::runtime::{RebindError, Runtime, ShutdownReport, WaiterKind};
use crate::ui::{AboutWindow, HotkeyRow, MainWindow, RunStatus};
use autoclicker_core::backend::InputBackend;
use autoclicker_core::config::{ClickConfig, Config, HotkeyAction, TargetMode};
use autoclicker_core::engine::Command;
use autoclicker_core::keys::{Hotkey, Modifiers};
use autoclicker_core::status::{RunState, Snapshot};
use autoclicker_platform::{WindowsInput, enable_per_monitor_dpi_awareness};
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, SharedString, Timer, VecModel};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

/// Seconds counted down before "Pick target" reads the cursor position.
pub const PICK_COUNTDOWN_SECONDS: i32 = 3;
const NOTICE_DURATION: Duration = Duration::from_secs(6);

/// Startup options.
#[derive(Clone, Debug)]
pub struct GuiOptions {
    /// Settings file; `None` disables persistence.
    pub config_path: Option<PathBuf>,
    pub waiter: WaiterKind,
}

impl Default for GuiOptions {
    fn default() -> Self {
        Self {
            config_path: config_store::default_path(),
            waiter: WaiterKind::HighResolution,
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

    /// Shows the window and runs until it is closed or the exit hotkey is
    /// pressed. Shutdown has completed when this returns.
    pub fn run(&self) -> Result<Option<ShutdownReport>, slint::PlatformError> {
        self.ui.show()?;
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

        let weak = Rc::downgrade(this);
        ui.window().on_close_requested(move || {
            if let Some(c) = weak.upgrade() {
                c.close();
            }
            CloseRequestResponse::HideWindow
        });
    }

    fn with_ui(&self, f: impl FnOnce(&MainWindow)) {
        if let Some(ui) = self.ui.upgrade() {
            f(&ui);
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Err(e) = config_store::save(path, &self.config.borrow()) {
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

    fn render_status(&self, ui: &MainWindow) {
        let view = form::status_view(&self.snapshot.borrow());
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
                *self.config.borrow_mut() = candidate;
                self.save();
                Ok(())
            }
            Err(RebindError::Conflicts(failed)) => Err(BindError::Registration(
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
            )),
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

    /// The single shutdown path (window closed or exit hotkey): stop the
    /// engine and release any held button, unregister hotkeys, join both
    /// threads, then leave the event loop. Runs once.
    fn close(&self) -> Option<ShutdownReport> {
        if self.closed.replace(true) {
            return self.report.get();
        }
        self.cancel_pick();
        self.notice_timer.stop();
        let report = self.runtime.borrow_mut().shutdown();
        self.report.set(report);
        if let Some(about) = self.about.borrow().as_ref() {
            let _ = about.hide();
        }
        self.with_ui(|ui| {
            let _ = ui.hide();
        });
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
pub fn run() -> ExitCode {
    let app = match App::new(GuiOptions::default()) {
        Ok(app) => app,
        Err(e) => {
            autoclicker_platform::show_error_message(
                "Autoclicker",
                &format!("Autoclicker could not start: {e}"),
            );
            return ExitCode::FAILURE;
        }
    };
    match app.run() {
        Ok(Some(report)) if report.is_clean() => ExitCode::SUCCESS,
        Ok(Some(_)) => ExitCode::FAILURE,
        Ok(None) => ExitCode::SUCCESS,
        Err(e) => {
            app.close();
            autoclicker_platform::show_error_message(
                "Autoclicker",
                &format!("The window failed: {e}"),
            );
            ExitCode::FAILURE
        }
    }
}
