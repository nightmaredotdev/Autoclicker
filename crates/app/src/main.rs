//! Cadence desktop application entry point.

// Release builds are GUI applications without a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        autoclicker::gui::run()
    }
    #[cfg(not(windows))]
    {
        eprintln!("autoclicker: only Windows is supported at the moment");
        ExitCode::FAILURE
    }
}
