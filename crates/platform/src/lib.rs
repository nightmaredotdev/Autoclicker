//! Platform-specific functionality: input injection, global hotkeys, precise
//! waiting and screen/cursor queries.
//!
//! All `unsafe` code in the workspace is confined to the OS-specific modules
//! of this crate. Every `unsafe` block must carry a `// SAFETY:` comment.

#[cfg(windows)]
#[allow(unsafe_code)] // Win32 FFI is only permitted inside this module.
mod windows;

#[cfg(windows)]
pub use windows::*;
