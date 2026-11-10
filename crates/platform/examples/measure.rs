//! Timing measurement: achieved clicks per second and start-to-start error of
//! the real engine + Windows backend, comparing the plain and high-resolution
//! waiters.
//!
//! Every injected event is timestamped by a low-level mouse hook and then
//! swallowed, so nothing on the desktop is clicked.
//!
//! Scenarios:
//! * `foreground` - this process owns a visible window;
//! * `background` - this process's window is minimized and a separate process
//!   shows a fullscreen, topmost, foreground window (borderless; not an
//!   exclusive-mode DirectX swap chain).
//!
//! ```text
//! cargo run -p autoclicker-platform --features diagnostics --release --example measure -- \
//!     [--interval-ms 100] [--hold-ms 10] [--clicks 200] [--scenario both] [--waiter both]
//! ```

#[cfg(not(windows))]
fn main() {
    eprintln!("the measurement tool only runs on Windows");
}

#[cfg(windows)]
fn main() {
    imp::main();
}

#[cfg(windows)]
mod imp {
    use autoclicker_core::config::{ClickConfig, TargetMode};
    use autoclicker_core::engine::{Command, ThreadExit, spawn_engine};
    use autoclicker_core::geometry::{Point, Rect};
    use autoclicker_core::keys::Modifiers;
    use autoclicker_core::status::{Snapshot, StatusSlot};
    use autoclicker_core::wait::{PlainWaiter, Waiter};
    use autoclicker_platform::diagnostics::{
        MouseHookRecorder, TestWindow, WindowKind, foreground_window, monitors, power_source,
    };
    use autoclicker_platform::{HighResWaiter, WindowsInput, enable_per_monitor_dpi_awareness};
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, Command as Process, Stdio};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Scenario {
        Foreground,
        Background,
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum WaiterKind {
        Plain,
        HighRes,
    }

    struct Args {
        interval_ms: u32,
        hold_ms: u32,
        clicks: usize,
        scenarios: Vec<Scenario>,
        waiters: Vec<WaiterKind>,
    }

    fn parse() -> Result<Args, String> {
        let mut a = Args {
            interval_ms: 100,
            hold_ms: 10,
            clicks: 200,
            scenarios: vec![Scenario::Foreground, Scenario::Background],
            waiters: vec![WaiterKind::Plain, WaiterKind::HighRes],
        };
        let raw: Vec<String> = std::env::args().skip(1).collect();
        let mut it = raw.iter();
        while let Some(flag) = it.next() {
            let mut value = || it.next().cloned().ok_or(format!("{flag} needs a value"));
            match flag.as_str() {
                "--cover-ms" => {
                    let ms = value()?.parse().map_err(|e| format!("{e}"))?;
                    cover(ms);
                    std::process::exit(0);
                }
                "--interval-ms" => a.interval_ms = value()?.parse().map_err(|e| format!("{e}"))?,
                "--hold-ms" => a.hold_ms = value()?.parse().map_err(|e| format!("{e}"))?,
                "--clicks" => a.clicks = value()?.parse().map_err(|e| format!("{e}"))?,
                "--scenario" => {
                    a.scenarios = match value()?.as_str() {
                        "foreground" => vec![Scenario::Foreground],
                        "background" => vec![Scenario::Background],
                        "both" => vec![Scenario::Foreground, Scenario::Background],
                        v => return Err(format!("bad scenario {v}")),
                    }
                }
                "--waiter" => {
                    a.waiters = match value()?.as_str() {
                        "plain" => vec![WaiterKind::Plain],
                        "high-resolution" => vec![WaiterKind::HighRes],
                        "both" => vec![WaiterKind::Plain, WaiterKind::HighRes],
                        v => return Err(format!("bad waiter {v}")),
                    }
                }
                f => return Err(format!("unknown argument {f}")),
            }
        }
        Ok(a)
    }

    fn primary() -> Rect {
        monitors()
            .into_iter()
            .find(|r| r.contains(Point::new(0, 0)))
            .expect("primary monitor")
    }

    /// Child process: fullscreen topmost foreground window for `ms`.
    fn cover(ms: u64) {
        let _ = enable_per_monitor_dpi_awareness();
        let window = TestWindow::create(primary(), WindowKind::TopmostPopup, "measurement cover")
            .expect("cover window");
        let mut fg = false;
        for _ in 0..20 {
            if window.bring_to_foreground() {
                fg = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        println!("ready foreground={fg}");
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_millis(ms));
    }

    fn spawn_cover(ms: u64) -> (Child, bool) {
        let exe = std::env::current_exe().expect("own path");
        let mut child = Process::new(exe)
            .args(["--cover-ms", &ms.to_string()])
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn cover process");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .expect("cover ready line");
        (child, line.contains("foreground=true"))
    }

    fn percentile(sorted: &[f64], p: f64) -> f64 {
        let idx = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len()) - 1;
        sorted[idx]
    }

    struct Trial {
        cps: f64,
        interval_err: [f64; 3], // p50, p99, max of |error| in ms
        mean_interval: f64,
        hold_err: [f64; 3],
        skipped: u64,
        max_late_ms: f64,
        presses: usize,
    }

    fn stats(mut v: Vec<f64>) -> [f64; 3] {
        v.sort_by(f64::total_cmp);
        [
            percentile(&v, 0.50),
            percentile(&v, 0.99),
            *v.last().unwrap(),
        ]
    }

    fn run_trial(args: &Args, waiter: WaiterKind) -> Trial {
        let settings = ClickConfig {
            mode: TargetMode::Cursor,
            interval_ms: args.interval_ms,
            interval_jitter_sigma_ms: 0.0,
            hold_ms: args.hold_ms,
            position_jitter_radius_px: 0,
            target: None,
        }
        .validated()
        .expect("valid settings");
        let recorder = MouseHookRecorder::start(true).expect("mouse hook");
        let slot = Arc::new(StatusSlot::new(Snapshot::new(TargetMode::Cursor)));
        let boxed: Box<dyn Waiter> = match waiter {
            WaiterKind::Plain => Box::new(PlainWaiter::new()),
            WaiterKind::HighRes => Box::new(HighResWaiter::new().expect("high-res timer")),
        };
        let engine = spawn_engine(
            WindowsInput::new(),
            boxed,
            settings,
            slot.clone(),
            Box::new(|| {}),
            1,
        )
        .expect("engine");
        engine
            .send(Command::Start {
                release_first: Modifiers::NONE,
            })
            .unwrap();
        let wanted = args.clicks + 1;
        let budget = Duration::from_millis(u64::from(args.interval_ms) * wanted as u64 * 2 + 5000);
        let complete = recorder.wait_for(budget, |e| {
            e.iter().filter(|e| e.message.is_down()).count() >= wanted
                && e.last().is_some_and(|e| e.message.is_up())
        });
        engine.send(Command::Stop).unwrap();
        assert_eq!(engine.shutdown(Duration::from_secs(2)), ThreadExit::Clean);
        assert!(complete, "trial timed out");
        let snap = slot.take().snapshot;

        let events = recorder.events();
        let downs: Vec<Instant> = events
            .iter()
            .filter(|e| e.message.is_down())
            .map(|e| e.at)
            .take(wanted)
            .collect();
        let nominal = f64::from(args.interval_ms);
        let intervals: Vec<f64> = downs
            .windows(2)
            .map(|w| (w[1] - w[0]).as_secs_f64() * 1000.0)
            .collect();
        let holds: Vec<f64> = events
            .windows(2)
            .filter(|w| w[0].message.is_down() && w[1].message.is_up())
            .map(|w| (w[1].at - w[0].at).as_secs_f64() * 1000.0 - f64::from(args.hold_ms))
            .map(f64::abs)
            .collect();
        Trial {
            cps: (downs.len() - 1) as f64 / (downs[downs.len() - 1] - downs[0]).as_secs_f64(),
            mean_interval: intervals.iter().sum::<f64>() / intervals.len() as f64,
            interval_err: stats(intervals.iter().map(|i| (i - nominal).abs()).collect()),
            hold_err: stats(holds),
            skipped: snap.skipped,
            max_late_ms: snap.max_lateness.as_secs_f64() * 1000.0,
            presses: downs.len(),
        }
    }

    pub fn main() {
        let args = match parse() {
            Ok(a) => a,
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(2);
            }
        };
        enable_per_monitor_dpi_awareness().expect("DPI awareness");
        println!(
            "interval {} ms, hold {} ms, {} intervals per trial, jitter off, power: {}",
            args.interval_ms,
            args.hold_ms,
            args.clicks,
            power_source()
        );
        println!();
        println!(
            "| scenario | waiter | presses | CPS (target {:.2}) | mean interval ms | interval error p50 / p99 / max ms | hold error p50 / p99 / max ms | skipped | max lateness ms |",
            1000.0 / f64::from(args.interval_ms)
        );
        println!("|---|---|---|---|---|---|---|---|---|");

        let r = primary();
        let app_rect = Rect::new(r.left + 40, r.top + 40, r.left + 440, r.top + 340);
        for &scenario in &args.scenarios {
            // A window owned by this process, standing in for the application window.
            let app = TestWindow::create(app_rect, WindowKind::Normal, "measurement app window")
                .expect("app window");
            let mut cover = None;
            let mut label = "foreground (own window visible)".to_string();
            if scenario == Scenario::Background {
                app.minimize();
                let budget = u64::from(args.interval_ms)
                    * (args.clicks as u64 + 1)
                    * 2
                    * args.waiters.len() as u64
                    + 20_000;
                let (child, fg) = spawn_cover(budget);
                std::thread::sleep(Duration::from_millis(500));
                let still = foreground_window() != app.id();
                label = format!(
                    "background (own window minimized; fullscreen topmost window of another process{})",
                    if fg && still {
                        " in foreground"
                    } else {
                        ", foreground NOT confirmed"
                    }
                );
                cover = Some(child);
            }
            for &waiter in &args.waiters {
                let t = run_trial(&args, waiter);
                println!(
                    "| {label} | {} | {} | {:.3} | {:.3} | {:.3} / {:.3} / {:.3} | {:.3} / {:.3} / {:.3} | {} | {:.3} |",
                    match waiter {
                        WaiterKind::Plain => "plain",
                        WaiterKind::HighRes => "high-resolution",
                    },
                    t.presses,
                    t.cps,
                    t.mean_interval,
                    t.interval_err[0],
                    t.interval_err[1],
                    t.interval_err[2],
                    t.hold_err[0],
                    t.hold_err[1],
                    t.hold_err[2],
                    t.skipped,
                    t.max_late_ms,
                );
                let _ = std::io::stdout().flush();
            }
            if let Some(mut child) = cover {
                let _ = child.kill();
                let _ = child.wait();
            }
            drop(app);
        }
    }
}
