//! Windows backend (Win32 via `windows-sys`).
//!
//! This is the only module in the workspace allowed to contain `unsafe` code.
//! Every `unsafe` block holds one operation and documents why it is sound.

mod console;
mod dpi;
mod error;
mod handle;
mod hotkeys;
mod input;
mod keys;
mod waiter;

#[cfg(any(test, feature = "diagnostics"))]
pub mod diagnostics;

#[cfg(test)]
mod tests;

pub use console::{console_shutdown_complete, install_console_exit_handler};
pub use dpi::{enable_per_monitor_dpi_awareness, is_per_monitor_v2};
pub use error::Win32Error;
pub use hotkeys::{HotkeyEvent, HotkeyService, Registration};
pub use input::{INJECTION_SIGNATURE, WindowsInput, buttons_swapped};
pub use keys::virtual_key;
pub use waiter::HighResWaiter;
