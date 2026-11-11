//! Cursor, monitor and mouse-button access via Win32.

use super::error::Win32Error;
use cadence_core::backend::{ButtonAction, InputBackend, InputError, SendError};
use cadence_core::geometry::{Point, Rect};
use cadence_core::keys::Modifiers;
use std::mem::size_of;
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_MOUSE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEINPUT, SendInput,
    VK_CONTROL, VK_LBUTTON, VK_MENU, VK_RBUTTON, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SM_SWAPBUTTON, SetCursorPos,
};

/// Marker stored in `dwExtraInfo` of every event this application injects,
/// so diagnostics and tests can tell its events apart.
pub const INJECTION_SIGNATURE: usize = 0x4143_4C4B; // "ACLK"

/// Windows implementation of [`InputBackend`].
#[derive(Debug, Default)]
pub struct WindowsInput {
    _private: (),
}

impl WindowsInput {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Whether the user has swapped the primary and secondary mouse buttons.
pub fn buttons_swapped() -> bool {
    // SAFETY: no preconditions.
    unsafe { GetSystemMetrics(SM_SWAPBUTTON) != 0 }
}

/// Flags for a logical left-button transition. `SendInput` button flags refer
/// to physical buttons, so with swapped buttons the logical (primary) button
/// is the physical right one.
pub(crate) fn button_flags(action: ButtonAction, swapped: bool) -> MOUSE_EVENT_FLAGS {
    match (action, swapped) {
        (ButtonAction::Press, false) => MOUSEEVENTF_LEFTDOWN,
        (ButtonAction::Release, false) => MOUSEEVENTF_LEFTUP,
        (ButtonAction::Press, true) => MOUSEEVENTF_RIGHTDOWN,
        (ButtonAction::Release, true) => MOUSEEVENTF_RIGHTUP,
    }
}

pub(crate) fn mouse_input(flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: INJECTION_SIGNATURE,
            },
        },
    }
}

/// Interprets a `SendInput` result: events are inserted in order, so a short
/// count means exactly the first `inserted` took effect.
pub(crate) fn check_inserted(
    requested: usize,
    inserted: u32,
    error: impl FnOnce() -> Win32Error,
) -> Result<(), SendError> {
    let inserted = inserted as usize;
    if inserted >= requested {
        return Ok(());
    }
    Err(SendError {
        inserted,
        error: InputError::new(
            "SendInput",
            format!(
                "inserted {inserted} of {requested} events; input may be blocked by another \
                 thread or the secure desktop: {}",
                error()
            ),
        ),
    })
}

fn key_down(vk: u16) -> bool {
    // SAFETY: no preconditions; any virtual-key code is accepted.
    let state = unsafe { GetAsyncKeyState(i32::from(vk)) };
    state < 0 // most significant bit set: key is down
}

impl InputBackend for WindowsInput {
    fn cursor_position(&mut self) -> Result<Point, InputError> {
        let mut p = POINT { x: 0, y: 0 };
        // SAFETY: `p` is a valid, writable POINT.
        if unsafe { GetCursorPos(&mut p) } == 0 {
            return Err(Win32Error::last().into_input_error("GetCursorPos"));
        }
        Ok(Point::new(p.x, p.y))
    }

    fn move_cursor(&mut self, to: Point) -> Result<(), InputError> {
        // SAFETY: no pointer arguments.
        if unsafe { SetCursorPos(to.x, to.y) } == 0 {
            return Err(Win32Error::last().into_input_error("SetCursorPos"));
        }
        Ok(())
    }

    fn monitor_bounds(&mut self, at: Point) -> Result<Rect, InputError> {
        let pt = POINT { x: at.x, y: at.y };
        // SAFETY: no pointer arguments.
        let monitor = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONULL) };
        if monitor.is_null() {
            return Err(InputError::new(
                "MonitorFromPoint",
                format!("{at} is not on any connected monitor"),
            ));
        }
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: `monitor` is a valid handle and `info.cbSize` is initialized.
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return Err(Win32Error::last().into_input_error("GetMonitorInfoW"));
        }
        let r = info.rcMonitor;
        Ok(Rect::new(r.left, r.top, r.right, r.bottom))
    }

    fn send_buttons(&mut self, actions: &[ButtonAction]) -> Result<(), SendError> {
        let swapped = buttons_swapped();
        let inputs: Vec<INPUT> = actions
            .iter()
            .map(|&a| mouse_input(button_flags(a, swapped)))
            .collect();
        // SAFETY: `inputs` holds `inputs.len()` initialized INPUT structures and
        // the size argument matches the structure size.
        let inserted = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                size_of::<INPUT>() as i32,
            )
        };
        check_inserted(inputs.len(), inserted, Win32Error::last)
    }

    fn held_modifiers(&mut self) -> Result<Modifiers, InputError> {
        Ok(Modifiers {
            ctrl: key_down(VK_CONTROL),
            alt: key_down(VK_MENU),
            shift: key_down(VK_SHIFT),
        })
    }

    fn button_reported_down(&mut self) -> Option<bool> {
        // GetAsyncKeyState reports physical buttons.
        let vk = if buttons_swapped() {
            VK_RBUTTON
        } else {
            VK_LBUTTON
        };
        Some(key_down(vk))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_left_maps_to_the_physical_primary_button() {
        assert_eq!(
            button_flags(ButtonAction::Press, false),
            MOUSEEVENTF_LEFTDOWN
        );
        assert_eq!(
            button_flags(ButtonAction::Release, false),
            MOUSEEVENTF_LEFTUP
        );
        assert_eq!(
            button_flags(ButtonAction::Press, true),
            MOUSEEVENTF_RIGHTDOWN
        );
        assert_eq!(
            button_flags(ButtonAction::Release, true),
            MOUSEEVENTF_RIGHTUP
        );
    }

    #[test]
    fn short_sendinput_counts_report_how_many_events_landed() {
        let err = || Win32Error::from_code(5);
        assert_eq!(check_inserted(2, 2, err), Ok(()));
        assert_eq!(check_inserted(1, 1, err), Ok(()));
        let e = check_inserted(2, 1, err).unwrap_err();
        assert_eq!(e.inserted, 1, "press landed, release did not");
        assert!(e.error.detail.contains("inserted 1 of 2"), "{}", e.error);
        assert_eq!(check_inserted(1, 0, err).unwrap_err().inserted, 0);
    }
}
