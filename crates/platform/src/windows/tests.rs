//! Windows integration tests. They use the real desktop, so they are ignored
//! by default and must run serially from an interactive, unlocked session:
//!
//! ```text
//! cargo test -p autoclicker-platform -- --ignored --test-threads=1
//! ```
//!
//! Safety for the user's desktop: injected clicks are either swallowed by a
//! low-level mouse hook or delivered only after verifying that a test-owned
//! window is under the cursor; injected keys are sent only while a test-owned
//! window has the foreground. The cursor position and foreground window are
//! restored afterwards.

use super::diagnostics::*;
use super::*;
use autoclicker_core::backend::{ButtonAction, InputBackend};
use autoclicker_core::config::{ClickConfig, Config, HotkeyAction, HotkeyConfig, TargetMode};
use autoclicker_core::engine::{Command, ThreadExit, spawn_engine};
use autoclicker_core::geometry::{Point, Rect};
use autoclicker_core::keys::{Hotkey, Key, Modifiers};
use autoclicker_core::status::{Snapshot, StatusSlot};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const VK_CONTROL: u16 = 0x11;
const VK_SHIFT: u16 = 0x10;
const VK_F8: u16 = 0x77;
const VK_F13: u16 = 0x7C;

static SERIAL: Mutex<()> = Mutex::new(());

/// Serializes tests and sets per-monitor DPI awareness.
fn setup() -> MutexGuard<'static, ()> {
    let guard = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    enable_per_monitor_dpi_awareness().expect("per-monitor-v2 DPI awareness");
    guard
}

/// Restores the cursor position on drop.
struct CursorRestore(Point);

impl CursorRestore {
    fn save() -> Self {
        Self(cursor_position())
    }
}

impl Drop for CursorRestore {
    fn drop(&mut self) {
        let _ = WindowsInput::new().move_cursor(self.0);
    }
}

/// Restores the foreground window on drop.
struct ForegroundRestore(WindowId);

impl Drop for ForegroundRestore {
    fn drop(&mut self) {
        force_foreground(self.0);
    }
}

fn primary_center_rect(size: i32) -> Rect {
    let m = monitors()
        .into_iter()
        .find(|r| r.contains(Point::new(0, 0)))
        .expect("primary monitor");
    let (cx, cy) = ((m.left + m.right) / 2, (m.top + m.bottom) / 2);
    Rect::new(cx - size / 2, cy - size / 2, cx + size / 2, cy + size / 2)
}

fn bindings(f: impl FnOnce(&mut HotkeyConfig)) -> autoclicker_core::config::HotkeyBindings {
    let mut cfg = Config {
        hotkeys: HotkeyConfig::none(),
        ..Config::default()
    };
    f(&mut cfg.hotkeys);
    cfg.validate().expect("valid bindings").hotkeys
}

// ------------------------------------------------------------------- DPI / cursor

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn dpi_awareness_is_per_monitor_v2() {
    let _g = setup();
    assert!(is_per_monitor_v2());
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn cursor_round_trips_on_every_monitor_and_bounds_match() {
    let _g = setup();
    let _restore = CursorRestore::save();
    let mut input = WindowsInput::new();
    let monitors = monitors();
    assert!(!monitors.is_empty());
    for m in &monitors {
        let points = [
            Point::new(m.left, m.top),
            Point::new(m.right - 1, m.bottom - 1),
            Point::new((m.left + m.right) / 2, (m.top + m.bottom) / 2),
        ];
        for p in points {
            input.move_cursor(p).unwrap();
            assert_eq!(input.cursor_position().unwrap(), p, "monitor {m:?}");
            assert_eq!(input.monitor_bounds(p).unwrap(), *m);
        }
    }
    eprintln!("verified {} monitor(s): {monitors:?}", monitors.len());
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn points_outside_every_monitor_are_rejected() {
    let _g = setup();
    let far = Point::new(i32::MAX / 2, i32::MAX / 2);
    let err = WindowsInput::new().monitor_bounds(far).unwrap_err();
    assert!(err.detail.contains("not on any connected monitor"), "{err}");
}

// ------------------------------------------------------------------- mouse input

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn injected_buttons_arrive_in_order_with_signature() {
    let _g = setup();
    let recorder = MouseHookRecorder::start(true).expect("mouse hook"); // swallow all
    let mut input = WindowsInput::new();
    let at = input.cursor_position().unwrap();
    input.send_buttons(&[ButtonAction::Press]).unwrap();
    std::thread::sleep(Duration::from_millis(15));
    input.send_buttons(&[ButtonAction::Release]).unwrap();
    input
        .send_buttons(&[ButtonAction::Press, ButtonAction::Release])
        .unwrap();
    assert!(recorder.wait_for(Duration::from_secs(2), |e| e.len() >= 4));
    let events = recorder.events();
    let (down, up) = if buttons_swapped() {
        (MouseMessage::RightDown, MouseMessage::RightUp)
    } else {
        (MouseMessage::LeftDown, MouseMessage::LeftUp)
    };
    let kinds: Vec<_> = events.iter().map(|e| e.message).collect();
    assert_eq!(kinds, vec![down, up, down, up]);
    assert!(events.iter().all(|e| e.injected_flag && e.point == at));
    let held = events[1].at - events[0].at;
    assert!(held >= Duration::from_millis(14), "hold {held:?}");
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn swapped_buttons_still_produce_a_primary_click() {
    let _g = setup();
    let _restore = CursorRestore::save();
    let window = TestWindow::create(
        primary_center_rect(240),
        WindowKind::TopmostPopup,
        "swap test",
    )
    .expect("test window");
    let r = primary_center_rect(240);
    let center = Point::new((r.left + r.right) / 2, (r.top + r.bottom) / 2);
    let mut input = WindowsInput::new();
    input.move_cursor(center).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        window_at(center),
        window.id(),
        "refusing to click: the test window is not under the cursor"
    );

    let click = |input: &mut WindowsInput| {
        input.send_buttons(&[ButtonAction::Press]).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        input.send_buttons(&[ButtonAction::Release]).unwrap();
    };

    // Normal buttons: logical left click.
    {
        let _swap = SwapButtonsGuard::set(false);
        click(&mut input);
        assert!(window.wait_for(Duration::from_secs(2), |m| {
            m.iter().any(|m| m.is_left_down())
        }));
    }
    let before = window.messages().len();

    // Swapped buttons: premise check - an uncompensated physical left press
    // becomes a logical right click...
    {
        let _swap = SwapButtonsGuard::set(true);
        assert_eq!(window_at(center), window.id());
        send_physical_button(true, true);
        send_physical_button(true, false);
        assert!(window.wait_for(Duration::from_secs(2), |m| {
            m[before..].iter().any(|m| m.is_right_down())
        }));
        let mid = window.messages().len();
        // ...while the backend's logical press still produces a primary click.
        click(&mut input);
        assert!(window.wait_for(Duration::from_secs(2), |m| {
            m[mid..].iter().any(|m| m.is_left_down())
        }));
        assert!(!window.messages()[mid..].iter().any(|m| m.is_right_down()));
    }
    assert!(!buttons_swapped(), "swap setting restored");
}

// ------------------------------------------------------------------- hotkeys

/// Puts a test window in the foreground, or panics without injecting anything.
fn foreground_test_window() -> (TestWindow, ForegroundRestore) {
    let restore = ForegroundRestore(foreground_window());
    let r = primary_center_rect(200);
    let window =
        TestWindow::create(r, WindowKind::TopmostPopup, "hotkey test").expect("test window");
    let mut ok = false;
    for _ in 0..10 {
        if window.bring_to_foreground() {
            ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        ok,
        "inconclusive: could not make the test window the foreground window"
    );
    (window, restore)
}

fn assert_still_foreground(window: &TestWindow) {
    assert_eq!(
        foreground_window(),
        window.id(),
        "foreground changed during the test; aborting before injecting more keys"
    );
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn held_hotkey_fires_once_and_foreground_delivery_is_documented() {
    let _g = setup();
    let (tx, rx) = mpsc::channel();
    let exit = Hotkey::new(
        Modifiers {
            ctrl: true,
            alt: false,
            shift: true,
        },
        Key::F8,
    );
    let b = bindings(|h| {
        h.toggle = Some(Hotkey::plain(Key::F13));
        h.exit = Some(exit);
    });
    let (service, regs) =
        HotkeyService::start(&b, move |e| tx.send(e.action).unwrap()).expect("hotkey thread");
    for r in &regs {
        assert!(
            r.result.is_ok(),
            "{} could not be registered: {:?}",
            r.hotkey,
            r.result
        );
    }
    let (window, _restore) = foreground_test_window();

    // Auto-repeat: five key-downs without a release, then the release.
    assert_still_foreground(&window);
    let mut keys = vec![(VK_F13, false); 5];
    keys.push((VK_F13, true));
    assert_eq!(send_keys(&keys), 6);
    std::thread::sleep(Duration::from_millis(300));
    let fired: Vec<_> = rx.try_iter().collect();
    assert_eq!(
        fired,
        vec![HotkeyAction::Toggle],
        "held key must fire exactly once"
    );

    // Released and pressed again: fires again.
    assert_still_foreground(&window);
    send_keys(&[(VK_F13, false), (VK_F13, true)]);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)),
        Ok(HotkeyAction::Toggle)
    );

    // The default exit shortcut, with modifiers.
    assert_still_foreground(&window);
    send_keys(&[
        (VK_CONTROL, false),
        (VK_SHIFT, false),
        (VK_F8, false),
        (VK_F8, true),
        (VK_SHIFT, true),
        (VK_CONTROL, true),
    ]);
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(1)),
        Ok(HotkeyAction::Exit)
    );
    std::thread::sleep(Duration::from_millis(200));

    // Did the foreground window also receive the hotkey keys?
    let msgs = window.messages();
    let f13_downs = msgs.iter().filter(|m| m.is_key_down(VK_F13)).count();
    let f8_downs = msgs.iter().filter(|m| m.is_key_down(VK_F8)).count();
    let ctrl_downs = msgs.iter().filter(|m| m.is_key_down(VK_CONTROL)).count();
    eprintln!(
        "foreground window received: F13 key-downs={f13_downs}, F8 key-downs={f8_downs}, \
         Ctrl key-downs={ctrl_downs}"
    );
    assert!(
        ctrl_downs > 0,
        "control: non-hotkey keys reach the foreground window"
    );
    assert_eq!(
        f13_downs, 0,
        "registered hotkey F13 is withheld from the foreground window"
    );
    assert_eq!(
        f8_downs, 0,
        "registered Ctrl+Shift+F8 is withheld from the foreground window"
    );

    assert_eq!(service.shutdown(Duration::from_secs(2)), ThreadExit::Clean);
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn conflicting_registration_is_reported_and_shutdown_unregisters() {
    let _g = setup();
    let b = bindings(|h| h.toggle = Some(Hotkey::plain(Key::F13)));
    let (first, regs) = HotkeyService::start(&b, |_| {}).unwrap();
    assert!(regs[0].result.is_ok(), "{:?}", regs[0].result);

    let (second, regs) = HotkeyService::start(&b, |_| {}).unwrap();
    let err = regs[0]
        .result
        .clone()
        .expect_err("duplicate registration must fail");
    eprintln!("conflict reported as: {err}");
    assert_eq!(err.code, 1409, "ERROR_HOTKEY_ALREADY_REGISTERED");
    assert_eq!(second.shutdown(Duration::from_secs(2)), ThreadExit::Clean);

    let started = Instant::now();
    assert_eq!(first.shutdown(Duration::from_secs(2)), ThreadExit::Clean);
    assert!(started.elapsed() < Duration::from_millis(500));

    // After shutdown the hotkey is free again.
    let (third, regs) = HotkeyService::start(&b, |_| {}).unwrap();
    assert!(regs[0].result.is_ok(), "{:?}", regs[0].result);
    assert_eq!(third.shutdown(Duration::from_secs(2)), ThreadExit::Clean);
}

// ------------------------------------------------------------------- engine end to end

fn engine_settings(f: impl FnOnce(&mut ClickConfig)) -> autoclicker_core::config::ClickSettings {
    let mut c = ClickConfig {
        mode: TargetMode::Cursor,
        interval_ms: 50,
        interval_jitter_sigma_ms: 0.0,
        hold_ms: 20,
        position_jitter_radius_px: 0,
        target: None,
    };
    f(&mut c);
    c.validated().unwrap()
}

fn idle_slot(mode: TargetMode) -> Arc<StatusSlot> {
    Arc::new(StatusSlot::new(Snapshot::new(mode)))
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn engine_clicks_are_paired_and_end_released() {
    let _g = setup();
    let recorder = MouseHookRecorder::start(true).expect("mouse hook");
    let slot = idle_slot(TargetMode::Cursor);
    let engine = spawn_engine(
        WindowsInput::new(),
        HighResWaiter::new().unwrap(),
        engine_settings(|_| {}),
        slot.clone(),
        Box::new(|| {}),
        1,
    )
    .unwrap();
    engine
        .send(Command::Start {
            release_first: Modifiers::NONE,
        })
        .unwrap();
    assert!(recorder.wait_for(Duration::from_secs(5), |e| {
        e.iter().filter(|e| e.message.is_down()).count() >= 6
    }));
    engine.send(Command::Stop).unwrap();
    assert_eq!(engine.shutdown(Duration::from_secs(2)), ThreadExit::Clean);
    std::thread::sleep(Duration::from_millis(50));

    let events = recorder.events();
    for pair in events.chunks(2) {
        assert!(pair[0].message.is_down(), "{events:?}");
        assert!(
            pair.get(1).is_some_and(|e| e.message.is_up()),
            "unpaired: {events:?}"
        );
    }
    let snapshot = slot.take().snapshot;
    assert_eq!(snapshot.total_clicks as usize, events.len() / 2);
    assert!(!snapshot.button_held);
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn shutdown_mid_hold_releases_promptly() {
    let _g = setup();
    let recorder = MouseHookRecorder::start(true).expect("mouse hook");
    let engine = spawn_engine(
        WindowsInput::new(),
        HighResWaiter::new().unwrap(),
        engine_settings(|c| {
            c.interval_ms = 2000;
            c.hold_ms = 1500;
        }),
        idle_slot(TargetMode::Cursor),
        Box::new(|| {}),
        1,
    )
    .unwrap();
    engine
        .send(Command::Start {
            release_first: Modifiers::NONE,
        })
        .unwrap();
    assert!(recorder.wait_for(Duration::from_secs(2), |e| !e.is_empty()));
    let shutdown_at = Instant::now();
    assert_eq!(engine.shutdown(Duration::from_secs(2)), ThreadExit::Clean);
    let joined = shutdown_at.elapsed();
    assert!(recorder.wait_for(Duration::from_secs(1), |e| e.len() >= 2));
    let up = recorder.events()[1];
    assert!(up.message.is_up());
    let release_delay = up.at.saturating_duration_since(shutdown_at);
    eprintln!("released {release_delay:?} after shutdown; thread joined after {joined:?}");
    assert!(release_delay < Duration::from_millis(100));
    assert!(joined < Duration::from_millis(200));
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn fixed_target_clicks_land_within_the_jitter_radius() {
    let _g = setup();
    let _restore = CursorRestore::save();
    let recorder = MouseHookRecorder::start(true).expect("mouse hook");
    let r = primary_center_rect(100);
    let target = Point::new((r.left + r.right) / 2, (r.top + r.bottom) / 2);
    WindowsInput::new().move_cursor(target).unwrap();
    let engine = spawn_engine(
        WindowsInput::new(),
        HighResWaiter::new().unwrap(),
        engine_settings(|c| {
            c.mode = TargetMode::Fixed;
            c.position_jitter_radius_px = 3;
        }),
        idle_slot(TargetMode::Fixed),
        Box::new(|| {}),
        1,
    )
    .unwrap();
    engine.send(Command::SetTargetFromCursor).unwrap();
    engine
        .send(Command::Start {
            release_first: Modifiers::NONE,
        })
        .unwrap();
    assert!(recorder.wait_for(Duration::from_secs(5), |e| {
        e.iter().filter(|e| e.message.is_down()).count() >= 20
    }));
    assert_eq!(engine.shutdown(Duration::from_secs(2)), ThreadExit::Clean);
    let downs: Vec<Point> = recorder
        .events()
        .iter()
        .filter(|e| e.message.is_down())
        .map(|e| e.point)
        .collect();
    for p in &downs {
        let (dx, dy) = (p.x - target.x, p.y - target.y);
        assert!(dx * dx + dy * dy <= 9, "{p} too far from {target}");
    }
    assert!(downs.iter().any(|p| *p != target), "jitter was applied");
}

// ------------------------------------------------------------------- session end

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn session_end_runs_the_callback_before_returning() {
    let _g = setup();
    let name = format!("AutoclickerTest.session.{}", std::process::id());
    let Acquire::Acquired(lock) = acquire_instance(&name).unwrap() else {
        panic!("acquire");
    };
    let ran = Arc::new(Mutex::new(None::<Instant>));
    let r = ran.clone();
    let events = SystemEvents::start(
        &lock,
        || {},
        move || {
            // Simulate a shutdown that takes a moment.
            std::thread::sleep(Duration::from_millis(150));
            *r.lock().unwrap() = Some(Instant::now());
        },
    )
    .unwrap();
    let window = windows_of_process(std::process::id())
        .into_iter()
        .find(|w| w.title == SYSTEM_EVENTS_WINDOW_TITLE)
        .expect("hidden system-events window");
    assert!(!window.visible);
    let reply = simulate_session_end(window.id, Duration::from_secs(5));
    let returned = Instant::now();
    assert_eq!(reply, Some(1), "logoff/shutdown is not blocked");
    let ran_at = ran.lock().unwrap().expect("session-end callback ran");
    assert!(
        ran_at <= returned,
        "WM_ENDSESSION returned before the shutdown finished"
    );
    assert_eq!(events.shutdown(Duration::from_secs(2)), ThreadExit::Clean);
}
