//! End-to-end tests of console control handling, using the real executable.
//!
//! The application is started in its own new console with its output piped,
//! clicking is started with the toggle hotkey, and then a real console control
//! event is delivered:
//! * `CTRL_C_EVENT` via `GenerateConsoleCtrlEvent` on the application's console;
//! * `CTRL_CLOSE_EVENT` by posting `WM_CLOSE` to the application's console
//!   window (as the console host does when its window is closed).
//!
//! Both are sent from a helper process (this test binary re-invoked), because
//! attaching to another console detaches the caller from its own.
//!
//! Every click the application injects is swallowed by a low-level mouse hook,
//! and the hotkey is only injected while a test-owned window has the
//! foreground, so nothing on the desktop is clicked or typed into.
//!
//! Run with: `cargo test -p autoclicker --test console_control -- --ignored --test-threads=1`

#![cfg(windows)]

use autoclicker_core::geometry::Rect;
use autoclicker_platform::diagnostics::{
    MouseHookRecorder, TestWindow, WindowKind, close_console_window_of, force_foreground,
    foreground_window, monitors, send_ctrl_c_to_console_of, send_keys,
};
use std::io::{BufRead, BufReader};
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const HELPER_ENV: &str = "AUTOCLICKER_CONSOLE_HELPER";
const VK_F6: u16 = 0x75;
/// Exit status of a process terminated by a console control event.
const STATUS_CONTROL_C_EXIT: u32 = 0xC000_013A;

/// Not a real test: the body of the helper process. Without the environment
/// variable (a normal test run) it does nothing.
#[test]
#[ignore = "helper process for the console control tests"]
fn console_control_helper() {
    let Ok(spec) = std::env::var(HELPER_ENV) else {
        return;
    };
    let (action, pid) = spec.split_once(':').expect("action:pid");
    let pid: u32 = pid.parse().expect("pid");
    match action {
        "ctrl-c" => send_ctrl_c_to_console_of(pid).expect("send Ctrl+C"),
        "close" => close_console_window_of(pid).expect("close console window"),
        other => panic!("unknown helper action {other}"),
    }
}

fn run_helper(action: &str, pid: u32) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "console_control_helper",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(HELPER_ENV, format!("{action}:{pid}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run helper");
    assert!(
        out.status.success(),
        "helper failed: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

struct App {
    child: Child,
    lines: Receiver<String>,
    output: Vec<String>,
}

impl App {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_autoclicker"))
            .args(["--mode", "cursor"])
            .creation_flags(CREATE_NEW_CONSOLE)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start autoclicker");
        let (tx, lines) = mpsc::channel();
        for stream in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn std::io::Read + Send>,
            Box::new(child.stderr.take().unwrap()),
        ] {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    let _ = tx.send(line);
                }
            });
        }
        Self {
            child,
            lines,
            output: Vec::new(),
        }
    }

    fn wait_for_line(&mut self, needle: &str, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if let Ok(line) = self.lines.recv_timeout(Duration::from_millis(50)) {
                let hit = line.contains(needle);
                self.output.push(line);
                if hit {
                    return true;
                }
            }
        }
        false
    }

    fn wait_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                // Drain the remaining output.
                std::thread::sleep(Duration::from_millis(100));
                self.output.extend(self.lines.try_iter());
                return Some(status);
            }
            if Instant::now() >= end {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn transcript(&self) -> String {
        self.output.join("\n")
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Starts the app, starts clicking via the F6 hotkey, delivers `action`, and
/// checks that the app shut down cleanly with no button left held.
fn exercise(action: &str) -> (Option<ExitStatus>, String) {
    let recorder = MouseHookRecorder::start(true).expect("mouse hook");
    let mut app = App::start();
    assert!(
        app.wait_for_line("Toggle clicking", Duration::from_secs(10)),
        "app did not start:\n{}",
        app.transcript()
    );
    assert!(
        !app.transcript().contains("NOT AVAILABLE"),
        "inconclusive: a default hotkey is taken by another program:\n{}",
        app.transcript()
    );

    // Start clicking: inject F6 only while a test-owned window is in front.
    let previous = foreground_window();
    let m = monitors()[0];
    let window = TestWindow::create(
        Rect::new(m.left + 60, m.top + 60, m.left + 260, m.top + 260),
        WindowKind::TopmostPopup,
        "console control test",
    )
    .expect("test window");
    // The new console window may activate asynchronously after start-up, so
    // retry, and check again immediately before injecting.
    let mut in_front = false;
    for _ in 0..40 {
        if window.bring_to_foreground() {
            std::thread::sleep(Duration::from_millis(100));
            if foreground_window() == window.id() {
                in_front = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(in_front, "inconclusive: could not take the foreground");
    assert_eq!(send_keys(&[(VK_F6, false), (VK_F6, true)]), 2);
    let clicking = recorder.wait_for(Duration::from_secs(5), |e| {
        e.iter().filter(|e| e.message.is_down()).count() >= 3
    });
    drop(window);
    force_foreground(previous);
    assert!(clicking, "clicking did not start:\n{}", app.transcript());

    run_helper(action, app.child.id());
    let status = app.wait_exit(Duration::from_secs(10));
    let exited_at = Instant::now();
    let transcript = app.transcript();
    assert!(status.is_some(), "app did not exit:\n{transcript}");

    // The engine stopped: no further injected events after exit.
    let count = recorder.events().len();
    std::thread::sleep(Duration::from_millis(300));
    let events = recorder.events();
    assert_eq!(events.len(), count, "events after exit");
    assert!(events.iter().all(|e| e.at <= exited_at));
    // No button left held: every press is followed by its release.
    for pair in events.chunks(2) {
        assert!(pair[0].message.is_down(), "{events:?}");
        assert!(
            pair.get(1).is_some_and(|e| e.message.is_up()),
            "button left held:\n{events:?}"
        );
    }
    (status, transcript)
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn ctrl_c_while_clicking_shuts_down_cleanly() {
    let (status, transcript) = exercise("ctrl-c");
    eprintln!("--- app output ---\n{transcript}\n--- exit: {status:?}");
    assert!(
        transcript.contains("Exiting: console closed or Ctrl+C."),
        "{transcript}"
    );
    assert!(
        transcript.contains("Shutdown complete: engine thread Clean, hotkey thread Clean."),
        "{transcript}"
    );
    assert!(
        transcript.contains("Mouse button held: no."),
        "{transcript}"
    );
    assert_eq!(status.unwrap().code(), Some(0), "Ctrl+C exits normally");
}

#[test]
#[ignore = "needs an interactive, unlocked Windows desktop"]
fn closing_the_console_window_while_clicking_shuts_down_cleanly() {
    let (status, transcript) = exercise("close");
    eprintln!("--- app output ---\n{transcript}\n--- exit: {status:?}");
    assert!(
        transcript.contains("Exiting: console closed or Ctrl+C."),
        "{transcript}"
    );
    assert!(
        transcript.contains("Shutdown complete: engine thread Clean, hotkey thread Clean."),
        "{transcript}"
    );
    assert!(
        transcript.contains("Mouse button held: no."),
        "{transcript}"
    );
    // Either main returned first (0) or Windows terminated the process once
    // the handler returned after shutdown completed.
    let code = status.unwrap().code().map(|c| c as u32);
    assert!(
        matches!(code, Some(0) | Some(STATUS_CONTROL_C_EXIT)),
        "unexpected exit code {code:x?}"
    );
}
