//! End-to-end tests of the real executable (separate process).
//!
//! Interactive - run with:
//! `cargo test -p autoclicker --test gui_process -- --ignored --test-threads=1`
//!
//! The application uses a temporary settings file (`AUTOCLICKER_CONFIG`)
//! with hotkeys on F13/F14/F15. Every click it injects is swallowed by a
//! low-level mouse hook in this test process, and keys are injected only
//! while a test-owned window has the foreground.

#![cfg(windows)]

use autoclicker_core::config::{Config, HotkeyConfig};
use autoclicker_core::geometry::Rect;
use autoclicker_core::keys::{Hotkey, Key};
use autoclicker_platform::diagnostics::{
    HookEvent, MouseHookRecorder, TestWindow, WindowInfo, WindowKind, force_foreground,
    foreground_window, monitors, post_close, send_keys, windows_of_process,
};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const VK_F13: u16 = 0x7C;
const VK_F14: u16 = 0x7D;
const VK_F15: u16 = 0x7E;

fn temp_config(name: &str, always_on_top: bool) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("autoclicker-e2e-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config {
        hotkeys: HotkeyConfig {
            toggle: Some(Hotkey::plain(Key::F13)),
            start: None,
            stop: None,
            set_target: Some(Hotkey::plain(Key::F15)),
            exit: Some(Hotkey::plain(Key::F14)),
        },
        ..Config::default()
    };
    cfg.click.mode = autoclicker_core::config::TargetMode::Cursor;
    cfg.window.always_on_top = always_on_top;
    let path = dir.join("config.toml");
    std::fs::write(&path, toml::to_string(&cfg).unwrap()).unwrap();
    path
}

struct App(Child);

impl App {
    fn start(config: &Path) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_autoclicker"))
            .env("AUTOCLICKER_CONFIG", config)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("start autoclicker");
        Self(child)
    }

    fn window(&self) -> WindowInfo {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(w) = windows_of_process(self.0.id())
                .into_iter()
                .find(|w| w.visible && w.title.starts_with("Autoclicker"))
            {
                return w;
            }
            assert!(Instant::now() < end, "the main window did not appear");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn wait_title(&self, title: &str) {
        let end = Instant::now() + Duration::from_secs(5);
        while self.window().title != title {
            assert!(Instant::now() < end, "title never became {title:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(s) = self.0.try_wait().unwrap() {
                return Some(s);
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A test-owned window in the foreground, so injected keys never reach
/// another application (and the GUI is not focused).
fn foreground_test_window() -> TestWindow {
    let m = monitors()[0];
    let r = Rect::new(m.right - 260, m.bottom - 260, m.right - 60, m.bottom - 60);
    let window = TestWindow::create(r, WindowKind::TopmostPopup, "e2e keys").unwrap();
    for _ in 0..40 {
        if window.bring_to_foreground() {
            std::thread::sleep(Duration::from_millis(80));
            if foreground_window() == window.id() {
                return window;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("inconclusive: could not take the foreground");
}

fn tap(window: &TestWindow, vk: u16) {
    assert_eq!(
        foreground_window(),
        window.id(),
        "foreground changed; not injecting"
    );
    assert_eq!(send_keys(&[(vk, false), (vk, true)]), 2);
}

fn downs(e: &[HookEvent]) -> usize {
    e.iter().filter(|e| e.message.is_down()).count()
}

fn assert_clean_stop(recorder: &MouseHookRecorder) {
    let count = recorder.events().len();
    std::thread::sleep(Duration::from_millis(400));
    let events = recorder.events();
    assert_eq!(events.len(), count, "no clicks after exit");
    for pair in events.chunks(2) {
        assert!(pair[0].message.is_down(), "{events:?}");
        assert!(
            pair.get(1).is_some_and(|e| e.message.is_up()),
            "button left held: {events:?}"
        );
    }
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn launches_hotkeys_work_unfocused_and_closing_the_window_shuts_down() {
    let recorder = MouseHookRecorder::start(true).unwrap();
    let config = temp_config("close", true);
    let mut app = App::start(&config);
    let win = app.window();
    assert_eq!(win.title, "Autoclicker");
    assert!(win.topmost, "always-on-top from the settings is applied");

    let keys = foreground_test_window();
    assert_ne!(foreground_window(), win.id, "the GUI is not focused");
    tap(&keys, VK_F13);
    app.wait_title("Autoclicker - Running");
    assert!(recorder.wait_for(Duration::from_secs(3), |e| downs(e) >= 3));
    tap(&keys, VK_F13);
    app.wait_title("Autoclicker");
    std::thread::sleep(Duration::from_millis(200));
    assert_clean_stop(&recorder);

    // Close the window while clicking.
    recorder.clear();
    tap(&keys, VK_F13);
    assert!(recorder.wait_for(Duration::from_secs(3), |e| downs(e) >= 2));
    drop(keys);
    assert!(post_close(app.window().id));
    let status = app
        .wait_exit(Duration::from_secs(10))
        .expect("process exited");
    assert_eq!(status.code(), Some(0), "clean shutdown reports success");
    assert_clean_stop(&recorder);
    force_foreground(win.id); // no-op if gone
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn exit_hotkey_while_clicking_shuts_down() {
    let recorder = MouseHookRecorder::start(true).unwrap();
    let config = temp_config("exit", false);
    let mut app = App::start(&config);
    assert!(!app.window().topmost);
    let keys = foreground_test_window();
    tap(&keys, VK_F13);
    assert!(recorder.wait_for(Duration::from_secs(3), |e| downs(e) >= 2));
    tap(&keys, VK_F14);
    let status = app
        .wait_exit(Duration::from_secs(10))
        .expect("process exited");
    assert_eq!(status.code(), Some(0));
    assert_clean_stop(&recorder);
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn target_set_by_hotkey_persists_across_restarts() {
    let config = temp_config("persist", false);
    let mut app = App::start(&config);
    app.window();
    let keys = foreground_test_window();
    let before = std::fs::read_to_string(&config).unwrap();
    assert!(!before.contains("[click.target]"));
    tap(&keys, VK_F15); // set target to the current cursor position
    let end = Instant::now() + Duration::from_secs(3);
    while !std::fs::read_to_string(&config)
        .unwrap()
        .contains("[click.target]")
    {
        assert!(Instant::now() < end, "target was not saved");
        std::thread::sleep(Duration::from_millis(20));
    }
    tap(&keys, VK_F14);
    assert_eq!(
        app.wait_exit(Duration::from_secs(10)).unwrap().code(),
        Some(0)
    );
    let saved: Config = toml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert!(saved.click.target.is_some());

    // A restart keeps the file as it is (no recovery, no rewrite).
    drop(keys);
    let mut again = App::start(&config);
    again.window();
    // The new window activates on start; take the foreground back first.
    let keys = foreground_test_window();
    tap(&keys, VK_F14);
    assert_eq!(
        again.wait_exit(Duration::from_secs(10)).unwrap().code(),
        Some(0)
    );
    let reloaded: Config = toml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(reloaded, saved);
    assert!(!config.with_file_name("config.toml.bak").exists());
}
