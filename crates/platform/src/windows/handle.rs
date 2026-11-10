//! Owned kernel handles.

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

/// A kernel object handle closed on drop.
#[derive(Debug)]
pub(crate) struct OwnedHandle(HANDLE);

// SAFETY: kernel object handles (events, timers) may be used and closed from
// any thread.
unsafe impl Send for OwnedHandle {}
// SAFETY: the operations used on these handles (SetEvent, waits) are
// thread-safe.
unsafe impl Sync for OwnedHandle {}

impl OwnedHandle {
    /// Takes ownership of `handle`; returns `None` for a null handle.
    pub(crate) fn new(handle: HANDLE) -> Option<Self> {
        (!handle.is_null()).then_some(Self(handle))
    }

    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle is valid and owned exclusively by this value.
        unsafe { CloseHandle(self.0) };
    }
}
