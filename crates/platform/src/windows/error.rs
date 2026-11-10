//! Win32 error codes and messages.

use autoclicker_core::backend::InputError;
use std::fmt;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::System::Diagnostics::Debug::{
    FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS, FormatMessageW,
};

/// A Win32 error code with its system message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Win32Error {
    pub code: u32,
    pub message: String,
}

impl Win32Error {
    /// Captures `GetLastError()`. Call immediately after the failing function.
    pub fn last() -> Self {
        // SAFETY: GetLastError has no preconditions.
        let code = unsafe { GetLastError() };
        Self::from_code(code)
    }

    pub fn from_code(code: u32) -> Self {
        let mut buf = [0u16; 512];
        // SAFETY: the buffer pointer and length describe a valid, writable
        // buffer; no insert arguments are used (FORMAT_MESSAGE_IGNORE_INSERTS).
        let len = unsafe {
            FormatMessageW(
                FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
                std::ptr::null(),
                code,
                0,
                buf.as_mut_ptr(),
                buf.len() as u32,
                std::ptr::null(),
            )
        };
        let message = String::from_utf16_lossy(&buf[..len as usize])
            .trim_end()
            .trim_end_matches('.')
            .to_string();
        let message = if message.is_empty() {
            "unknown error".to_string()
        } else {
            message
        };
        Self { code, message }
    }

    pub(crate) fn into_input_error(self, operation: &'static str) -> InputError {
        InputError::new(operation, self.to_string())
    }
}

impl fmt::Display for Win32Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (Win32 error {})", self.message, self.code)
    }
}

impl std::error::Error for Win32Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_HOTKEY_ALREADY_REGISTERED};

    #[test]
    fn formats_system_messages() {
        let e = Win32Error::from_code(ERROR_ACCESS_DENIED);
        assert_eq!(e.code, 5);
        assert!(!e.message.is_empty() && e.message != "unknown error", "{e}");
        let e = Win32Error::from_code(ERROR_HOTKEY_ALREADY_REGISTERED);
        assert!(e.to_string().contains("1409"), "{e}");
    }
}
