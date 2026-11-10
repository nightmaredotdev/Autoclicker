//! Headless console front end: argument parsing, banner and status output.

use crate::runtime::{AppEvent, Runtime, WaiterKind};
use autoclicker_core::config::{Config, HotkeyAction, TargetMode, ValidatedConfig};
use autoclicker_core::status::{RunState, Snapshot, StatusUpdate};
use autoclicker_platform::{
    console_shutdown_complete, enable_per_monitor_dpi_awareness, install_console_exit_handler,
};
use std::io::Write;
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "\
Usage: autoclicker [--mode fixed|cursor] [--waiter high-resolution|plain]

  --mode     fixed   click a saved target (set it with the set-target hotkey)
             cursor  click wherever the cursor is
  --waiter   timing implementation (default: high-resolution)
";

#[derive(Debug, PartialEq, Eq)]
struct Options {
    mode: TargetMode,
    waiter: WaiterKind,
}

fn parse_args(args: &[String]) -> Result<Option<Options>, String> {
    let mut opts = Options {
        mode: TargetMode::Fixed,
        waiter: WaiterKind::HighResolution,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--mode" => {
                opts.mode = match it.next().map(String::as_str) {
                    Some("fixed") => TargetMode::Fixed,
                    Some("cursor") => TargetMode::Cursor,
                    Some(other) => {
                        return Err(format!(
                            "invalid --mode \"{other}\" (expected fixed or cursor)"
                        ));
                    }
                    None => return Err("--mode needs a value (fixed or cursor)".into()),
                }
            }
            "--waiter" => {
                opts.waiter = match it.next().map(String::as_str) {
                    Some("high-resolution") => WaiterKind::HighResolution,
                    Some("plain") => WaiterKind::Plain,
                    Some(other) => {
                        return Err(format!(
                            "invalid --waiter \"{other}\" (expected high-resolution or plain)"
                        ));
                    }
                    None => return Err("--waiter needs a value (high-resolution or plain)".into()),
                }
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Some(opts))
}

pub fn run(args: Vec<String>) -> ExitCode {
    let opts = match parse_args(&args) {
        Ok(Some(o)) => o,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(e) = enable_per_monitor_dpi_awareness() {
        eprintln!(
            "warning: per-monitor DPI awareness unavailable ({e}); positions may be off on scaled displays"
        );
    }

    let mut config = Config::default();
    config.click.mode = opts.mode;
    let config = match config.validate() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut runtime = match Runtime::start(&config, opts.waiter) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let exit_tx = runtime.event_sender();
    if let Err(e) = install_console_exit_handler(move || {
        let _ = exit_tx.send(AppEvent::Exit(crate::runtime::ExitReason::ConsoleControl));
    }) {
        eprintln!("warning: Ctrl+C will terminate without cleanup ({e})");
    }

    print_banner(&config, &runtime);

    let mut printer = StatusPrinter::default();
    let reason = loop {
        match runtime.recv() {
            AppEvent::Status => printer.print(runtime.take_status()),
            AppEvent::Exit(reason) => break reason,
        }
    };
    // Shut down before writing anything: console output can block (e.g. while
    // text is selected) or fail while the console is being closed, and neither
    // may delay stopping the engine and releasing the button.
    let report = runtime.shutdown();
    let final_status = runtime.take_status();
    console_shutdown_complete();

    // From here on output is best effort and must not panic.
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "Exiting: {reason}.");
    let s = &final_status.snapshot;
    if let Some(fault) = &s.fault {
        let _ = writeln!(out, "ERROR: {fault}");
    }
    let _ = writeln!(
        out,
        "Total clicks: {}. Mouse button held: {}.",
        s.total_clicks,
        if s.button_held {
            "possibly (see error)"
        } else {
            "no"
        }
    );
    match report {
        Some(r) => {
            let _ = writeln!(
                out,
                "Shutdown complete: engine thread {:?}, hotkey thread {:?}.",
                r.engine, r.hotkeys
            );
            if r.is_clean() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        None => ExitCode::SUCCESS,
    }
}

fn print_banner(config: &ValidatedConfig, runtime: &Runtime) {
    let c = &config.click;
    println!("Autoclicker {} (headless)", env!("CARGO_PKG_VERSION"));
    println!(
        "  mode: {} | interval: {} ms ± {} ms (σ) | hold: {} ms | position jitter: {}",
        c.mode(),
        c.interval().as_millis(),
        c.jitter_sigma_ms(),
        c.hold().as_millis(),
        if c.mode() == TargetMode::Fixed {
            format!("{} px", c.radius_px())
        } else {
            "off".into()
        }
    );
    println!("  timing: {}", runtime.waiter());
    if let Some(e) = runtime.timer_fallback() {
        println!("  warning: high-resolution timer unavailable ({e}); using the plain waiter");
    }
    println!("Hotkeys:");
    for r in runtime.registrations() {
        match &r.result {
            Ok(()) => println!("  {:<10} {}", r.action.to_string(), r.hotkey),
            Err(e) => println!(
                "  {:<10} {}  NOT AVAILABLE: {e} - another program is probably using it",
                r.action.to_string(),
                r.hotkey
            ),
        }
    }
    for action in HotkeyAction::ALL {
        if config.hotkeys.get(action).is_none() {
            println!("  {:<10} (unbound)", action.to_string());
        }
    }
    println!(
        "Note: hotkeys are global. While this program runs, the focused application does not \
         receive these key combinations."
    );
    if config.click.mode() == TargetMode::Fixed {
        println!(
            "Set a target with the set-target hotkey, then toggle clicking. Ctrl+C also exits."
        );
    } else {
        println!("Toggle clicking with the toggle hotkey. Ctrl+C also exits.");
    }
}

/// Prints state changes immediately and running metrics at most once a second.
#[derive(Default)]
struct StatusPrinter {
    last: Option<Snapshot>,
    last_metrics: Option<Instant>,
}

impl StatusPrinter {
    fn print(&mut self, update: StatusUpdate) {
        for notice in &update.notices {
            println!("  {notice}");
        }
        if update.dropped_notices > 0 {
            println!("  ({} older messages dropped)", update.dropped_notices);
        }
        let s = update.snapshot;
        let prev = self.last.as_ref();
        if prev.map(|p| p.state) != Some(s.state) || prev.map(|p| &p.fault) != Some(&s.fault) {
            match (s.state, &s.fault) {
                (RunState::Faulted, Some(f)) => eprintln!("ERROR: {f}"),
                (RunState::Idle, _) if prev.is_some_and(|p| p.state == RunState::Running) => {
                    println!("Stopped. {}", run_summary(&s));
                }
                (state, _) if prev.is_some() => println!("State: {state}"),
                _ => {}
            }
        } else if s.state == RunState::Running
            && self
                .last_metrics
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
        {
            println!("  {}", run_summary(&s));
            self.last_metrics = Some(Instant::now());
        }
        self.last = Some(s);
    }
}

fn run_summary(s: &Snapshot) -> String {
    let rate = s
        .run_cps
        .map_or_else(|| "-".to_string(), |c| format!("{c:.2}"));
    format!(
        "clicks: {} (total {}), rate: {rate}/s, skipped: {}, max lateness: {:.1} ms",
        s.run_clicks,
        s.total_clicks,
        s.skipped,
        s.max_lateness.as_secs_f64() * 1000.0
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_arguments() {
        assert_eq!(
            parse_args(&args(&[])),
            Ok(Some(Options {
                mode: TargetMode::Fixed,
                waiter: WaiterKind::HighResolution
            }))
        );
        assert_eq!(
            parse_args(&args(&["--mode", "cursor", "--waiter", "plain"])),
            Ok(Some(Options {
                mode: TargetMode::Cursor,
                waiter: WaiterKind::Plain
            }))
        );
        assert_eq!(parse_args(&args(&["--help"])), Ok(None));
        assert!(parse_args(&args(&["--mode"])).is_err());
        assert!(parse_args(&args(&["--mode", "fast"])).is_err());
        assert!(parse_args(&args(&["--bogus"])).is_err());
    }
}
