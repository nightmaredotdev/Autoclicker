//! Cadence desktop application: the Slint GUI, configuration I/O and the
//! wiring between the UI, the click engine and the platform backend.
//!
//! Unsafe code: every handwritten module declares `#![forbid(unsafe_code)]`.
//! The crate as a whole cannot, because the code Slint generates for the UI
//! contains `unsafe` blocks (under its own `allow`). The workspace-wide
//! `unsafe_code = "deny"` lint still applies everywhere else.

pub mod config_store;
pub mod form;
#[cfg(windows)]
pub mod gui;
#[cfg(windows)]
pub mod logging;
#[cfg(windows)]
pub mod runtime;

/// Code generated from `ui/app.slint`. Generated code is exempt from the
/// workspace's clippy lints.
#[cfg(windows)]
#[allow(
    clippy::all,
    clippy::todo,
    clippy::undocumented_unsafe_blocks,
    clippy::multiple_unsafe_ops_per_block
)]
pub mod ui {
    slint::include_modules!();
}
