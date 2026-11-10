//! Per-monitor DPI awareness.
//!
//! With per-monitor-v2 awareness, cursor and monitor APIs use physical pixels
//! on every monitor, so saved targets are exact on scaled and mixed-DPI setups.

use super::error::Win32Error;
use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
use windows_sys::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
};

/// Makes the process per-monitor-v2 DPI aware, then verifies it took effect.
/// Must run before any window is created.
pub fn enable_per_monitor_dpi_awareness() -> Result<(), Win32Error> {
    // SAFETY: called with a predefined awareness context constant.
    let ok = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    // ERROR_ACCESS_DENIED means awareness was already set (e.g. by a manifest
    // or an earlier call); the verification below decides whether that is fine.
    let set_error = (ok == 0).then(Win32Error::last);
    if is_per_monitor_v2() {
        return Ok(());
    }
    Err(match set_error {
        Some(e) if e.code != ERROR_ACCESS_DENIED => e,
        _ => Win32Error {
            code: ERROR_ACCESS_DENIED,
            message: "the process already uses a different DPI awareness mode".into(),
        },
    })
}

/// Whether the calling thread runs with per-monitor-v2 DPI awareness.
pub fn is_per_monitor_v2() -> bool {
    // SAFETY: no preconditions.
    let current = unsafe { GetThreadDpiAwarenessContext() };
    // SAFETY: both arguments are valid awareness contexts.
    unsafe {
        AreDpiAwarenessContextsEqual(current, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0
    }
}
