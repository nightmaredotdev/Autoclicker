//! Autoclicker desktop application.
//!
//! This milestone is headless: it runs from a console with the default
//! settings and global hotkeys, printing status and errors. The Slint user
//! interface is added in a later milestone.

#![forbid(unsafe_code)]

#[cfg(windows)]
mod console;
#[cfg(windows)]
mod runtime;

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(windows)]
    {
        console::run(std::env::args().skip(1).collect())
    }
    #[cfg(not(windows))]
    {
        eprintln!("autoclicker: only Windows is supported at the moment");
        ExitCode::FAILURE
    }
}
