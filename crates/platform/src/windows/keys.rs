//! Mapping of portable keys to Windows virtual-key codes.

use cadence_core::keys::{Key, Modifiers};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT,
};

pub fn virtual_key(key: Key) -> u16 {
    use Key::*;
    // Positions within contiguous groups of `Key::ALL`.
    let offset = |first: Key| {
        let pos = |k: Key| Key::ALL.iter().position(|&x| x == k).unwrap_or(0);
        (pos(key) - pos(first)) as u16
    };
    match key {
        F1 | F2 | F3 | F4 | F5 | F6 | F7 | F8 | F9 | F10 | F11 | F12 | F13 | F14 | F15 | F16
        | F17 | F18 | F19 | F20 | F21 | F22 | F23 | F24 => 0x70 + offset(F1),
        A | B | C | D | E | F | G | H | I | J | K | L | M | N | O | P | Q | R | S | T | U | V
        | W | X | Y | Z => 0x41 + offset(A),
        Digit0 | Digit1 | Digit2 | Digit3 | Digit4 | Digit5 | Digit6 | Digit7 | Digit8 | Digit9 => {
            0x30 + offset(Digit0)
        }
        Numpad0 | Numpad1 | Numpad2 | Numpad3 | Numpad4 | Numpad5 | Numpad6 | Numpad7 | Numpad8
        | Numpad9 => 0x60 + offset(Numpad0),
        Space => 0x20,
        Enter => 0x0D,
        Tab => 0x09,
        Escape => 0x1B,
        Backspace => 0x08,
        Insert => 0x2D,
        Delete => 0x2E,
        Home => 0x24,
        End => 0x23,
        PageUp => 0x21,
        PageDown => 0x22,
        Left => 0x25,
        Up => 0x26,
        Right => 0x27,
        Down => 0x28,
        Pause => 0x13,
        ScrollLock => 0x91,
    }
}

/// `RegisterHotKey` modifier flags (without `MOD_NOREPEAT`).
pub fn hotkey_modifiers(m: Modifiers) -> HOT_KEY_MODIFIERS {
    let mut flags = 0;
    if m.ctrl {
        flags |= MOD_CONTROL;
    }
    if m.alt {
        flags |= MOD_ALT;
    }
    if m.shift {
        flags |= MOD_SHIFT;
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_0, VK_9, VK_A, VK_F1, VK_F8, VK_F24, VK_NUMPAD0, VK_NUMPAD9, VK_PAUSE, VK_SCROLL, VK_Z,
    };

    #[test]
    fn matches_windows_constants() {
        assert_eq!(virtual_key(Key::F1), VK_F1);
        assert_eq!(virtual_key(Key::F8), VK_F8);
        assert_eq!(virtual_key(Key::F24), VK_F24);
        assert_eq!(virtual_key(Key::A), VK_A);
        assert_eq!(virtual_key(Key::Z), VK_Z);
        assert_eq!(virtual_key(Key::Digit0), VK_0);
        assert_eq!(virtual_key(Key::Digit9), VK_9);
        assert_eq!(virtual_key(Key::Numpad0), VK_NUMPAD0);
        assert_eq!(virtual_key(Key::Numpad9), VK_NUMPAD9);
        assert_eq!(virtual_key(Key::Pause), VK_PAUSE);
        assert_eq!(virtual_key(Key::ScrollLock), VK_SCROLL);
    }

    #[test]
    fn every_key_has_a_distinct_code() {
        let codes: HashSet<u16> = Key::ALL.iter().map(|&k| virtual_key(k)).collect();
        assert_eq!(codes.len(), Key::ALL.len());
    }

    #[test]
    fn modifier_flags() {
        let m = Modifiers {
            ctrl: true,
            alt: false,
            shift: true,
        };
        assert_eq!(hotkey_modifiers(m), MOD_CONTROL | MOD_SHIFT);
        assert_eq!(hotkey_modifiers(Modifiers::NONE), 0);
    }
}
