//! GUI integration test: drives the real Slint window on the main thread
//! (`harness = false`). Interactive - it needs an unlocked Windows desktop, so
//! it only runs when `--ignored` is passed:
//!
//! ```text
//! cargo test -p autoclicker --test gui -- --ignored
//! ```
//!
//! Actions are performed through the same callbacks the controls invoke, and
//! hotkeys are recorded by injecting real key presses into the focused window.
//! Every injected click is swallowed by a low-level mouse hook, and keys are
//! injected only while a window of this process has the foreground. The
//! settings file lives in a temporary directory.

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn main() {
    imp::main();
}

#[cfg(windows)]
mod imp {
    use autoclicker::config_store;
    use autoclicker::gui::{App, GuiOptions, PICK_COUNTDOWN_SECONDS};
    use autoclicker::runtime::{JOIN_TIMEOUT, WaiterKind};
    use autoclicker::ui::RunStatus;
    use autoclicker_core::backend::InputBackend;
    use autoclicker_core::config::{Config, HotkeyAction, HotkeyConfig, TargetMode};
    use autoclicker_core::engine::ThreadExit;
    use autoclicker_core::geometry::{Point, Rect};
    use autoclicker_core::keys::{Hotkey, Key, Modifiers};
    use autoclicker_platform::diagnostics::{
        HookEvent, MouseHookRecorder, TestWindow, WindowInfo, WindowKind, click_unmarked,
        client_rect, cursor_position, force_foreground, foreground_window, monitors, post_close,
        send_keys, window_at, windows_of_process,
    };
    use autoclicker_platform::{HotkeyService, WindowsInput};
    use slint::{Model, Timer, TimerMode};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    const VK_CONTROL: u16 = 0x11;
    const VK_SHIFT: u16 = 0x10;
    const VK_ESCAPE: u16 = 0x1B;
    const VK_A: u16 = 0x41;
    const VK_RETURN: u16 = 0x0D;
    const VK_0: u16 = 0x30;
    const VK_1: u16 = 0x31;
    const VK_5: u16 = 0x35;
    const VK_6: u16 = 0x36;
    const VK_7: u16 = 0x37;
    const VK_K: u16 = 0x4B;
    const VK_F13: u16 = 0x7C;
    const VK_F16: u16 = 0x7F;
    const VK_F17: u16 = 0x80;

    /// Runs the Slint event loop for `duration`.
    fn pump(duration: Duration) {
        let end = Instant::now() + duration;
        while Instant::now() < end {
            let left = end.saturating_duration_since(Instant::now());
            let t = Timer::default();
            t.start(TimerMode::SingleShot, left, || {
                let _ = slint::quit_event_loop();
            });
            slint::run_event_loop_until_quit().expect("event loop");
        }
    }

    /// Pumps the event loop until `cond` holds (or panics after `timeout`).
    fn pump_until(what: &str, timeout: Duration, cond: impl Fn() -> bool) {
        let end = Instant::now() + timeout;
        while !cond() {
            assert!(Instant::now() < end, "timed out waiting for: {what}");
            pump(Duration::from_millis(20));
        }
    }

    fn step(name: &str) {
        println!("  - {name}");
    }

    fn own_windows() -> Vec<WindowInfo> {
        windows_of_process(std::process::id())
    }

    fn main_window(title_prefix: &str) -> WindowInfo {
        own_windows()
            .into_iter()
            .find(|w| w.visible && w.title.starts_with(title_prefix))
            .unwrap_or_else(|| {
                panic!(
                    "no visible window titled {title_prefix:?}: {:?}",
                    own_windows()
                )
            })
    }

    fn downs(events: &[HookEvent]) -> usize {
        events.iter().filter(|e| e.message.is_down()).count()
    }

    fn assert_paired(events: &[HookEvent]) {
        for pair in events.chunks(2) {
            assert!(pair[0].message.is_down(), "{events:?}");
            assert!(
                pair.get(1).is_some_and(|e| e.message.is_up()),
                "button left held: {events:?}"
            );
        }
    }

    fn saved(path: &Path) -> Config {
        let text = std::fs::read_to_string(path).expect("settings file exists");
        toml::from_str(&text).expect("settings file parses")
    }

    fn options(path: &Path) -> GuiOptions {
        GuiOptions {
            config_path: Some(path.to_path_buf()),
            waiter: WaiterKind::HighResolution,
            log_dir: None,
        }
    }

    /// Keys are only injected while one of this process's windows is in front.
    fn focus(window: autoclicker_platform::diagnostics::WindowId) {
        for _ in 0..40 {
            if force_foreground(window) {
                pump(Duration::from_millis(80));
                if foreground_window() == window {
                    return;
                }
            }
            pump(Duration::from_millis(50));
        }
        panic!("inconclusive: could not bring a test-owned window to the foreground");
    }

    fn press(window: autoclicker_platform::diagnostics::WindowId, keys: &[(u16, bool)]) {
        assert_eq!(
            foreground_window(),
            window,
            "foreground changed; not injecting keys"
        );
        assert_eq!(send_keys(keys), keys.len());
    }

    fn tap(vk: u16) -> Vec<(u16, bool)> {
        vec![(vk, false), (vk, true)]
    }

    struct CursorRestore(Point);
    impl Drop for CursorRestore {
        fn drop(&mut self) {
            let _ = WindowsInput::new().move_cursor(self.0);
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("autoclicker-gui-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Hotkeys on F13-F15 so the test never collides with a running
    /// autoclicker that uses the defaults.
    fn initial_config() -> Config {
        Config {
            hotkeys: {
                let mut h = HotkeyConfig::none();
                h.toggle = Some(Hotkey::plain(Key::F13));
                h.set_target = Some(Hotkey::plain(Key::F15));
                h.exit = Some(Hotkey::plain(Key::F14));
                h
            },
            ..Config::default()
        }
    }

    /// Registers `key` from a separate hotkey thread, standing in for another
    /// program that already uses the combination.
    fn hold_hotkey(key: Key) -> HotkeyService {
        let taken = Config {
            hotkeys: {
                let mut h = HotkeyConfig::none();
                h.toggle = Some(Hotkey::plain(key));
                h
            },
            ..Config::default()
        };
        let (service, regs) =
            HotkeyService::start(&taken.validate().unwrap().hotkeys, |_| {}).unwrap();
        assert!(regs[0].result.is_ok(), "{key} must be free for the test");
        service
    }

    pub fn main() {
        let interactive = std::env::args().any(|a| a == "--ignored" || a == "--include-ignored");
        if !interactive {
            println!("gui: skipped (needs an interactive desktop; run with --ignored)");
            return;
        }
        println!("gui: running");
        let dir = temp_dir();
        let path = dir.join("config.toml");
        config_store::save(&path, &initial_config()).unwrap();
        let _cursor = CursorRestore(cursor_position());
        let previous_foreground = foreground_window();
        let recorder = MouseHookRecorder::start(true).expect("mouse hook");

        first_session(&path, &recorder);
        second_session(&path);
        startup_conflict_is_shown(&dir);
        obsolete_start_stop_bindings_are_migrated(&dir);
        corrupt_settings_are_reported(&dir);

        force_foreground(previous_foreground);
        let _ = std::fs::remove_dir_all(&dir);
        println!("gui: all checks passed");
    }

    fn first_session(path: &Path, recorder: &MouseHookRecorder) {
        step("launch");
        let app = App::new(options(path)).expect("app starts");
        let ui = app.ui();
        app.show_window();
        pump(Duration::from_millis(500));
        let win = main_window("Autoclicker");
        assert_eq!(win.title, "Autoclicker");
        assert!(!win.topmost);
        assert_eq!(ui.get_status(), RunStatus::Stopped);
        assert_eq!(ui.get_status_headline(), "Stopped");
        assert_eq!(ui.get_config_warning(), "");
        assert_eq!(ui.get_toggle_key(), "F13", "shown on the Start/Stop button");
        assert!(ui.get_fixed_mode());
        assert_eq!(ui.get_target_text(), "Not set");

        step("fixed mode without a target explains what to do");
        ui.invoke_start_stop_clicked();
        pump_until("no-target notice", Duration::from_secs(2), || {
            ui.get_notice_text()
                .to_lowercase()
                .contains("no target set")
        });
        assert_eq!(ui.get_status(), RunStatus::Stopped);
        assert_eq!(downs(&recorder.events()), 0);

        step("current-cursor mode: start and stop with the button");
        ui.invoke_mode_selected(false);
        assert!(!ui.get_fixed_mode());
        assert_eq!(app.config().click.mode, TargetMode::Cursor);
        assert_eq!(saved(path).click.mode, TargetMode::Cursor);
        let at = cursor_position();
        ui.invoke_start_stop_clicked();
        pump_until("clicks", Duration::from_secs(5), || {
            downs(&recorder.events()) >= 3
        });
        pump_until("running state", Duration::from_secs(2), || {
            ui.get_status() == RunStatus::Running
        });
        assert_eq!(main_window("Autoclicker").title, "Autoclicker - Running");
        ui.invoke_start_stop_clicked();
        pump_until("stopped state", Duration::from_secs(2), || {
            ui.get_status() == RunStatus::Stopped
        });
        pump(Duration::from_millis(200));
        let events = recorder.events();
        assert_paired(&events);
        assert!(
            events.iter().all(|e| e.point == at),
            "cursor mode must not move the cursor"
        );
        assert_eq!(main_window("Autoclicker").title, "Autoclicker");
        recorder.clear();

        step("fixed mode: pick target with the countdown");
        ui.invoke_mode_selected(true);
        let m = monitors()[0];
        let target = Point::new(
            m.left + (m.right - m.left) / 3,
            m.top + (m.bottom - m.top) / 3,
        );
        WindowsInput::new().move_cursor(target).unwrap();
        ui.invoke_pick_target_clicked();
        assert_eq!(ui.get_pick_countdown(), PICK_COUNTDOWN_SECONDS);
        pump_until("countdown finished", Duration::from_secs(6), || {
            ui.get_pick_countdown() == 0
        });
        let expected = format!("{}, {}", target.x, target.y);
        pump_until("target displayed", Duration::from_secs(2), || {
            ui.get_target_text() == expected
        });
        assert_eq!(app.config().click.target, Some(target));
        assert_eq!(saved(path).click.target, Some(target));

        step("fixed mode: clicks land on the target within the jitter radius");
        WindowsInput::new()
            .move_cursor(Point::new(target.x + 200, target.y + 100))
            .unwrap();
        ui.invoke_start_stop_clicked();
        pump_until("clicks", Duration::from_secs(5), || {
            downs(&recorder.events()) >= 5
        });
        ui.invoke_start_stop_clicked();
        pump_until("stopped", Duration::from_secs(2), || {
            ui.get_status() == RunStatus::Stopped
        });
        pump(Duration::from_millis(200));
        let events = recorder.events();
        assert_paired(&events);
        for e in events.iter().filter(|e| e.message.is_down()) {
            let (dx, dy) = (e.point.x - target.x, e.point.y - target.y);
            assert!(
                dx * dx + dy * dy <= 9,
                "{:?} too far from {target}",
                e.point
            );
        }
        recorder.clear();

        step("capture the target directly");
        let second = Point::new(target.x + 40, target.y + 30);
        WindowsInput::new().move_cursor(second).unwrap();
        app.capture_target_now();
        let expected = format!("{}, {}", second.x, second.y);
        pump_until("new target displayed", Duration::from_secs(2), || {
            ui.get_target_text() == expected
        });

        step("invalid settings are rejected with feedback and never applied");
        let before = saved(path);
        ui.set_interval_text("5".into());
        ui.invoke_settings_edited();
        assert!(
            ui.get_settings_error()
                .contains("outside the allowed range"),
            "{}",
            ui.get_settings_error()
        );
        ui.invoke_settings_commit();
        assert!(
            ui.get_settings_error().starts_with("Not applied"),
            "{}",
            ui.get_settings_error()
        );
        assert_eq!(app.config().click.interval_ms, 100);
        ui.set_interval_text("100".into());
        ui.set_hold_text("80".into());
        ui.invoke_settings_commit();
        assert!(
            ui.get_settings_error().contains("Hold of 80 ms"),
            "{}",
            ui.get_settings_error()
        );
        ui.set_hold_text("abc".into());
        ui.invoke_settings_edited();
        assert!(
            ui.get_settings_error()
                .contains("\"abc\" is not a whole number")
        );
        assert_eq!(saved(path), before, "nothing invalid was saved");

        step("valid settings are applied and saved");
        ui.set_hold_text("10".into());
        ui.set_interval_text("150".into());
        ui.invoke_settings_edited();
        assert!(ui.get_settings_pending());
        ui.invoke_settings_commit();
        assert_eq!(ui.get_settings_error(), "");
        assert!(!ui.get_settings_pending());
        assert_eq!(app.config().click.interval_ms, 150);
        assert_eq!(saved(path).click.interval_ms, 150);

        step("interval unit toggles between ms and CPS without drift");
        assert!(!ui.get_interval_in_cps());
        ui.invoke_interval_unit_clicked();
        assert!(ui.get_interval_in_cps());
        assert_eq!(ui.get_interval_text(), "6.67", "150 ms shown as CPS");
        assert_eq!(
            app.config().click.interval_ms,
            150,
            "toggling changes nothing stored"
        );
        for _ in 0..10 {
            ui.invoke_interval_unit_clicked();
            ui.invoke_interval_unit_clicked();
        }
        assert_eq!(ui.get_interval_text(), "6.67");
        assert_eq!(
            app.config().click.interval_ms,
            150,
            "no drift after repeated toggling"
        );
        // Entering a rate stores the whole-millisecond interval and shows its exact rate.
        ui.set_interval_text("14.3".into());
        ui.invoke_settings_edited();
        ui.invoke_settings_commit();
        assert_eq!(app.config().click.interval_ms, 70);
        assert_eq!(saved(path).click.interval_ms, 70);
        assert_eq!(ui.get_interval_text(), "14.29");
        ui.invoke_interval_unit_clicked();
        assert!(!ui.get_interval_in_cps());
        assert_eq!(ui.get_interval_text(), "70");
        // Limits apply in CPS too, and an invalid entry blocks switching units.
        ui.invoke_interval_unit_clicked();
        ui.set_interval_text("250".into());
        ui.invoke_settings_edited();
        assert!(
            ui.get_settings_error()
                .contains("faster than the maximum of 100 CPS"),
            "{}",
            ui.get_settings_error()
        );
        ui.invoke_interval_unit_clicked();
        assert!(
            ui.get_interval_in_cps(),
            "stays in CPS while the value is invalid"
        );
        assert!(ui.get_settings_error().starts_with("Not applied"));
        assert_eq!(app.config().click.interval_ms, 70);
        // Back to 150 ms for the rest of the test.
        ui.set_interval_text("6.67".into());
        ui.invoke_settings_commit();
        assert_eq!(app.config().click.interval_ms, 150);
        ui.invoke_interval_unit_clicked();
        assert_eq!(ui.get_interval_text(), "150");
        assert_eq!(ui.get_settings_error(), "");

        step("clicking empty space outside an edited field commits it");
        let gui = main_window("Autoclicker").id;
        let client = client_rect(gui).expect("client area");
        // Inside the window's left padding, outside every card and control.
        let empty = Point::new(client.left + 5, (client.top + client.bottom) / 2);
        let type_interval = |digits: &[u16]| {
            focus(gui);
            ui.invoke_focus_interval_field();
            pump(Duration::from_millis(100));
            let mut keys = vec![
                (VK_CONTROL, false),
                (VK_A, false),
                (VK_A, true),
                (VK_CONTROL, true),
            ];
            for &d in digits {
                keys.extend([(d, false), (d, true)]);
            }
            press(gui, &keys);
        };
        type_interval(&[VK_1, VK_7, VK_5]);
        pump_until("typed", Duration::from_secs(2), || {
            ui.get_interval_text() == "175"
        });
        assert!(ui.get_settings_pending());
        assert_eq!(
            app.config().click.interval_ms,
            150,
            "not applied while editing"
        );
        assert_eq!(window_at(empty), gui, "refusing to click: not our window");
        assert_eq!(click_unmarked(empty), 2);
        pump_until(
            "committed by the outside click",
            Duration::from_secs(2),
            || app.config().click.interval_ms == 175,
        );
        assert_eq!(saved(path).click.interval_ms, 175);
        assert_eq!(ui.get_settings_error(), "");
        assert!(!ui.get_settings_pending());

        step("an invalid value is rejected on an outside click; the previous value stays");
        type_interval(&[VK_5]);
        pump_until("typed", Duration::from_secs(2), || {
            ui.get_interval_text() == "5"
        });
        assert_eq!(window_at(empty), gui);
        click_unmarked(empty);
        pump_until("rejected", Duration::from_secs(2), || {
            ui.get_settings_error().starts_with("Not applied")
        });
        assert_eq!(app.config().click.interval_ms, 175);
        assert_eq!(saved(path).click.interval_ms, 175);

        step("Enter still commits");
        type_interval(&[VK_1, VK_5, VK_0]);
        press(gui, &tap(VK_RETURN));
        pump_until("committed by Enter", Duration::from_secs(2), || {
            app.config().click.interval_ms == 150
        });
        assert_eq!(ui.get_settings_error(), "");
        let writes_before = std::fs::metadata(path).unwrap().modified().unwrap();
        // Focus leaving the field afterwards commits nothing new (no extra write).
        click_unmarked(empty);
        pump(Duration::from_millis(300));
        assert_eq!(
            std::fs::metadata(path).unwrap().modified().unwrap(),
            writes_before
        );

        step("switching tabs commits a pending edit");
        type_interval(&[VK_1, VK_6, VK_5]);
        pump_until("typed", Duration::from_secs(2), || {
            ui.get_interval_text() == "165"
        });
        assert_eq!(app.config().click.interval_ms, 150);
        ui.invoke_release_field_focus(); // what a tab click does first
        ui.set_current_tab(1);
        pump_until(
            "committed by the tab switch",
            Duration::from_secs(2),
            || app.config().click.interval_ms == 165,
        );
        ui.set_current_tab(0);
        pump(Duration::from_millis(100));

        step("a button action commits a pending edit first");
        type_interval(&[VK_1, VK_6, VK_0]);
        pump_until("typed", Duration::from_secs(2), || {
            ui.get_interval_text() == "160"
        });
        ui.invoke_about_clicked(); // buttons do not take keyboard focus
        pump_until(
            "committed before the action",
            Duration::from_secs(2),
            || app.config().click.interval_ms == 160,
        );
        // Restore 150 for the rest of the test.
        type_interval(&[VK_1, VK_5, VK_0]);
        press(gui, &tap(VK_RETURN));
        pump_until("restored", Duration::from_secs(2), || {
            app.config().click.interval_ms == 150
        });

        step("position jitter radius: validated, applied by the engine and saved");
        ui.set_radius_text("51".into());
        ui.invoke_settings_edited();
        assert!(
            ui.get_settings_error()
                .contains("51 px exceeds the maximum of 50 px"),
            "{}",
            ui.get_settings_error()
        );
        ui.invoke_settings_commit();
        assert!(ui.get_settings_error().starts_with("Not applied"));
        assert_eq!(app.config().click.position_jitter_radius_px, 3);
        assert_eq!(saved(path).click.position_jitter_radius_px, 3);

        ui.set_radius_text("0".into());
        ui.invoke_settings_commit();
        assert_eq!(ui.get_settings_error(), "");
        assert_eq!(app.config().click.position_jitter_radius_px, 0);
        assert_eq!(saved(path).click.position_jitter_radius_px, 0);
        let target = app.config().click.target.expect("target set earlier");
        recorder.clear();
        ui.invoke_start_stop_clicked();
        pump_until("clicks", Duration::from_secs(5), || {
            downs(&recorder.events()) >= 5
        });
        ui.invoke_start_stop_clicked();
        pump_until("stopped", Duration::from_secs(2), || {
            ui.get_status() == RunStatus::Stopped
        });
        pump(Duration::from_millis(200));
        let events = recorder.events();
        assert_paired(&events);
        assert!(
            events
                .iter()
                .filter(|e| e.message.is_down())
                .all(|e| e.point == target),
            "radius 0 must click exactly on the target"
        );

        ui.set_radius_text("8".into());
        ui.invoke_settings_commit();
        assert_eq!(ui.get_settings_error(), "");
        assert_eq!(app.config().click.position_jitter_radius_px, 8);
        assert_eq!(saved(path).click.position_jitter_radius_px, 8);
        recorder.clear();
        ui.invoke_start_stop_clicked();
        pump_until("clicks", Duration::from_secs(10), || {
            downs(&recorder.events()) >= 30
        });
        ui.invoke_start_stop_clicked();
        pump_until("stopped", Duration::from_secs(2), || {
            ui.get_status() == RunStatus::Stopped
        });
        pump(Duration::from_millis(200));
        let events = recorder.events();
        assert_paired(&events);
        let distances: Vec<i32> = events
            .iter()
            .filter(|e| e.message.is_down())
            .map(|e| {
                let (dx, dy) = (e.point.x - target.x, e.point.y - target.y);
                dx * dx + dy * dy
            })
            .collect();
        assert!(
            distances.iter().all(|&d| d <= 64),
            "outside the 8 px radius: {distances:?}"
        );
        assert!(
            distances.iter().any(|&d| d > 9),
            "the larger radius was not applied (all within 3 px): {distances:?}"
        );
        recorder.clear();

        step("always on top is applied to the real window and saved");
        ui.set_keep_on_top(true);
        ui.invoke_keep_on_top_toggled(true);
        pump_until("topmost", Duration::from_secs(2), || {
            main_window("Autoclicker").topmost
        });
        assert!(saved(path).window.always_on_top);
        ui.set_keep_on_top(false);
        ui.invoke_keep_on_top_toggled(false);
        pump_until("not topmost", Duration::from_secs(2), || {
            !main_window("Autoclicker").topmost
        });
        assert!(!saved(path).window.always_on_top);

        let gui = main_window("Autoclicker").id;
        let index =
            |a: HotkeyAction| HotkeyAction::ALL.iter().position(|x| *x == a).unwrap() as i32;
        let binding = |a: HotkeyAction| {
            ui.get_hotkeys()
                .row_data(index(a) as usize)
                .unwrap()
                .binding
                .to_string()
        };

        step("only the three hotkey actions are offered");
        let labels: Vec<String> = ui
            .get_hotkeys()
            .iter()
            .map(|r| r.label.to_string())
            .collect();
        assert_eq!(labels, ["Toggle start/stop", "Set target", "Exit"]);

        step("record a Ctrl+Shift+letter hotkey by pressing it");
        focus(gui);
        ui.invoke_start_recording(index(HotkeyAction::Exit));
        pump(Duration::from_millis(100));
        assert!(
            !app.hotkeys_active(),
            "hotkeys are suspended while recording"
        );
        press(
            gui,
            &[
                (VK_CONTROL, false),
                (VK_SHIFT, false),
                (VK_K, false),
                (VK_K, true),
                (VK_SHIFT, true),
                (VK_CONTROL, true),
            ],
        );
        let ctrl_shift_k = Hotkey::new(
            Modifiers {
                ctrl: true,
                alt: false,
                shift: true,
            },
            Key::K,
        );
        pump_until("binding recorded", Duration::from_secs(2), || {
            app.config().hotkeys.exit == Some(ctrl_shift_k)
        });
        assert_eq!(ui.get_capturing_index(), -1);
        assert!(app.hotkeys_active());
        assert_eq!(binding(HotkeyAction::Exit), "Ctrl+Shift+K");
        assert_eq!(saved(path).hotkeys.exit, Some(ctrl_shift_k));

        step("a key that would block typing is refused; Esc cancels");
        focus(gui);
        ui.invoke_start_recording(index(HotkeyAction::SetTarget));
        pump(Duration::from_millis(100));
        press(gui, &tap(VK_A));
        pump_until("refusal shown", Duration::from_secs(2), || {
            ui.get_hotkeys_error().contains("would block normal typing")
        });
        assert_eq!(
            ui.get_capturing_index(),
            index(HotkeyAction::SetTarget),
            "still recording"
        );
        press(gui, &tap(VK_ESCAPE));
        pump_until("recording cancelled", Duration::from_secs(2), || {
            ui.get_capturing_index() == -1
        });
        assert_eq!(
            app.config().hotkeys.set_target,
            Some(Hotkey::plain(Key::F15))
        );
        assert!(app.hotkeys_active());

        step("a combination held by another program cannot even be recorded");
        // Windows delivers a registered combination to its owner only, so the
        // key press never reaches this window: recording simply continues.
        let other = hold_hotkey(Key::F17);
        focus(gui);
        ui.invoke_start_recording(index(HotkeyAction::SetTarget));
        pump(Duration::from_millis(100));
        press(gui, &tap(VK_F17));
        pump(Duration::from_millis(500));
        assert_eq!(ui.get_capturing_index(), index(HotkeyAction::SetTarget));
        assert_eq!(
            app.config().hotkeys.set_target,
            Some(Hotkey::plain(Key::F15))
        );
        press(gui, &tap(VK_ESCAPE));
        pump_until("recording cancelled", Duration::from_secs(2), || {
            ui.get_capturing_index() == -1
        });
        assert_eq!(binding(HotkeyAction::SetTarget), "F15");
        assert!(app.hotkeys_active());
        assert_eq!(other.shutdown(JOIN_TIMEOUT), ThreadExit::Clean);

        step("a function key is recorded");
        focus(gui);
        ui.invoke_start_recording(index(HotkeyAction::SetTarget));
        pump(Duration::from_millis(100));
        press(gui, &tap(VK_F16));
        pump_until("F16 recorded", Duration::from_secs(2), || {
            app.config().hotkeys.set_target == Some(Hotkey::plain(Key::F16))
        });

        step("clear a binding");
        ui.invoke_hotkey_clear(index(HotkeyAction::SetTarget));
        assert_eq!(app.config().hotkeys.set_target, None);
        assert_eq!(binding(HotkeyAction::SetTarget), "Not set");

        step("global hotkey works while the GUI is not focused");
        recorder.clear();
        let r = Rect::new(m.right - 260, m.bottom - 260, m.right - 60, m.bottom - 60);
        let other_window =
            TestWindow::create(r, WindowKind::TopmostPopup, "unfocused test").unwrap();
        focus(other_window.id());
        assert_ne!(foreground_window(), gui);
        press(other_window.id(), &tap(VK_F13));
        pump_until("running via hotkey", Duration::from_secs(3), || {
            ui.get_status() == RunStatus::Running && downs(&recorder.events()) >= 2
        });
        press(other_window.id(), &tap(VK_F13));
        pump_until("stopped via hotkey", Duration::from_secs(3), || {
            ui.get_status() == RunStatus::Stopped
        });
        drop(other_window);

        step("closing the window hides it to the tray; clicking continues");
        let tray = app.tray().expect("notification-area icon");
        assert!(tray.get_window_visible());
        assert!(!tray.get_running());
        recorder.clear();
        ui.invoke_start_stop_clicked();
        pump_until("clicking", Duration::from_secs(5), || {
            downs(&recorder.events()) >= 2
        });
        assert!(tray.get_running(), "tray reflects the running state");
        assert!(post_close(main_window("Autoclicker").id), "WM_CLOSE posted");
        pump_until("hidden", Duration::from_secs(2), || !app.window_visible());
        assert!(
            !own_windows()
                .iter()
                .any(|w| w.visible && w.title.starts_with("Autoclicker")),
            "main window hidden"
        );
        assert!(!tray.get_window_visible());
        let before = downs(&recorder.events());
        pump(Duration::from_millis(500));
        assert!(
            downs(&recorder.events()) > before,
            "still clicking while hidden"
        );
        assert_eq!(ui.get_status(), RunStatus::Running);

        step("global hotkey works while hidden");
        let other_window = TestWindow::create(r, WindowKind::TopmostPopup, "hidden test").unwrap();
        focus(other_window.id());
        press(other_window.id(), &tap(VK_F13));
        pump_until(
            "stopped via hotkey while hidden",
            Duration::from_secs(3),
            || ui.get_status() == RunStatus::Stopped,
        );
        assert!(!tray.get_running());
        drop(other_window);

        step("tray: start and stop clicking while hidden");
        recorder.clear();
        tray.invoke_start_stop();
        pump_until("running via tray", Duration::from_secs(3), || {
            tray.get_running() && downs(&recorder.events()) >= 2
        });
        tray.invoke_start_stop();
        pump_until("stopped via tray", Duration::from_secs(3), || {
            !tray.get_running()
        });
        pump(Duration::from_millis(200));
        assert_paired(&recorder.events());

        step("tray: show restores a usable window");
        tray.invoke_toggle_window();
        let titled = || {
            own_windows()
                .iter()
                .any(|w| w.visible && w.title.starts_with("Autoclicker"))
        };
        pump_until("shown with its title", Duration::from_secs(3), || {
            app.window_visible() && titled()
        });
        let shown = main_window("Autoclicker");
        assert_eq!(shown.title, "Autoclicker");
        assert!(tray.get_window_visible());
        assert_eq!(ui.get_status(), RunStatus::Stopped);
        tray.invoke_toggle_window();
        pump_until("hidden again", Duration::from_secs(2), || {
            !app.window_visible()
        });
        tray.invoke_show_window(); // left click on the icon
        pump_until("shown by click", Duration::from_secs(3), || {
            app.window_visible() && titled()
        });

        step("about dialog with Slint attribution");
        ui.invoke_about_clicked();
        pump_until("about window", Duration::from_secs(2), || {
            own_windows()
                .iter()
                .any(|w| w.visible && w.title == "About Autoclicker")
        });

        step("tray Exit while clicking stops the engine and shuts down completely");
        recorder.clear();
        ui.invoke_start_stop_clicked();
        pump_until("clicks", Duration::from_secs(5), || {
            downs(&recorder.events()) >= 2
        });
        tray.invoke_exit();
        let report = app.shutdown_report().expect("tray Exit shut down");
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(app.close(), Some(report), "close is idempotent");
        pump(Duration::from_millis(300));
        let count = recorder.events().len();
        pump(Duration::from_millis(300));
        let events = recorder.events();
        assert_eq!(events.len(), count, "no clicks after close");
        assert_paired(&events);
        assert!(
            !own_windows()
                .iter()
                .any(|w| w.visible && w.title.starts_with("Autoclicker")),
            "main window hidden"
        );
        assert!(
            !own_windows()
                .iter()
                .any(|w| w.visible && w.title.starts_with("About"))
        );
        // Hotkeys were unregistered: they can be registered by someone else now.
        let free = initial_config().validate().unwrap().hotkeys;
        let (probe, regs) = HotkeyService::start(&free, |_| {}).unwrap();
        assert!(regs.iter().all(|r| r.result.is_ok()), "{regs:?}");
        assert_eq!(probe.shutdown(JOIN_TIMEOUT), ThreadExit::Clean);
        drop(app);
    }

    fn second_session(path: &Path) {
        step("settings persist across restarts");
        let app = App::new(options(path)).expect("app restarts");
        let ui = app.ui();
        app.show_window();
        pump(Duration::from_millis(300));
        assert_eq!(ui.get_config_warning(), "");
        assert_eq!(ui.get_interval_text(), "150");
        assert_eq!(ui.get_radius_text(), "8");
        assert_eq!(app.config().click.position_jitter_radius_px, 8);
        assert!(ui.get_fixed_mode());
        assert!(!ui.get_keep_on_top());
        assert_ne!(ui.get_target_text(), "Not set");
        assert_eq!(app.snapshot().target, app.config().click.target);
        let rows = ui.get_hotkeys();
        assert_eq!(rows.row_count(), 3);
        assert_eq!(rows.row_data(2).unwrap().binding, "Ctrl+Shift+K", "exit");
        assert_eq!(rows.row_data(1).unwrap().binding, "Not set", "set target");
        assert!(app.close().unwrap().is_clean());
        drop(app);
    }

    fn startup_conflict_is_shown(dir: &Path) {
        step("a saved hotkey that another program uses is reported at startup");
        let path = dir.join("conflict.toml");
        let mut cfg = initial_config();
        cfg.hotkeys.set_target = Some(Hotkey::plain(Key::F17));
        config_store::save(&path, &cfg).unwrap();
        let other = hold_hotkey(Key::F17);
        let app = App::new(options(&path)).expect("app starts");
        let rows = app.ui().get_hotkeys();
        let set_target = HotkeyAction::ALL
            .iter()
            .position(|a| *a == HotkeyAction::SetTarget)
            .unwrap();
        for (i, row) in rows.iter().enumerate() {
            if i == set_target {
                assert_eq!(row.binding, "F17");
                assert!(row.problem.contains("another program"), "{}", row.problem);
            } else {
                assert_eq!(row.problem, "", "row {i}");
            }
        }
        assert!(app.close().unwrap().is_clean());
        assert_eq!(other.shutdown(JOIN_TIMEOUT), ThreadExit::Clean);
    }

    fn obsolete_start_stop_bindings_are_migrated(dir: &Path) {
        step("a settings file with obsolete Start/Stop hotkeys loads and is migrated");
        let path = dir.join("obsolete.toml");
        let mut text = toml::to_string(&initial_config()).unwrap();
        text = text.replace(
            "[hotkeys]\n",
            "[hotkeys]\nstart = \"Ctrl+Alt+S\"\nstop = \"Ctrl+Alt+D\"\n",
        );
        text = text.replace("interval_ms = 100", "interval_ms = 333");
        std::fs::write(&path, &text).unwrap();
        let app = App::new(options(&path)).expect("older file loads");
        let warning = app.ui().get_config_warning().to_string();
        assert!(warning.contains("no longer supported"), "{warning}");
        assert_eq!(app.config().click.interval_ms, 333, "other settings kept");
        assert_eq!(app.config().hotkeys.toggle, Some(Hotkey::plain(Key::F13)));
        assert_eq!(app.ui().get_hotkeys().row_count(), 3);
        let migrated = std::fs::read_to_string(&path).unwrap();
        assert!(
            !migrated.contains("start =") && !migrated.contains("stop ="),
            "{migrated}"
        );
        assert!(migrated.contains("interval_ms = 333"));
        assert!(
            !dir.join("obsolete.toml.bak").exists(),
            "not treated as corrupt"
        );
        assert!(app.close().unwrap().is_clean());
        drop(app);

        // The notice is shown only once.
        let again = App::new(options(&path)).unwrap();
        assert_eq!(again.ui().get_config_warning(), "");
        assert!(again.close().unwrap().is_clean());
    }

    fn corrupt_settings_are_reported(dir: &Path) {
        step("a corrupt settings file is moved aside with a visible warning");
        let path = dir.join("corrupt.toml");
        std::fs::write(&path, "interval_ms = [this is not valid").unwrap();
        let app = App::new(options(&path)).expect("app starts with defaults");
        let warning = app.ui().get_config_warning().to_string();
        assert!(warning.contains("not usable"), "{warning}");
        assert!(warning.contains("corrupt.toml.bak"), "{warning}");
        assert_eq!(app.config().click.interval_ms, 100);
        assert!(dir.join("corrupt.toml.bak").exists());
        assert!(app.close().unwrap().is_clean());
    }
}
