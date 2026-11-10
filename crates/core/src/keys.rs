//! Portable key, modifier and hotkey representations.
//!
//! Only a deliberately small, layout-independent key set is supported so that
//! bindings mean the same thing on every keyboard layout and platform.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

macro_rules! keys {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        /// A bindable key.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Key { $($variant),+ }

        impl Key {
            /// Every supported key, in display order.
            pub const ALL: &'static [Key] = &[$(Key::$variant),+];

            /// Canonical name used in configuration files and messages.
            pub const fn name(self) -> &'static str {
                match self { $(Key::$variant => $name),+ }
            }
        }
    };
}

keys! {
    F1 => "F1", F2 => "F2", F3 => "F3", F4 => "F4", F5 => "F5", F6 => "F6",
    F7 => "F7", F8 => "F8", F9 => "F9", F10 => "F10", F11 => "F11", F12 => "F12",
    F13 => "F13", F14 => "F14", F15 => "F15", F16 => "F16", F17 => "F17", F18 => "F18",
    F19 => "F19", F20 => "F20", F21 => "F21", F22 => "F22", F23 => "F23", F24 => "F24",
    A => "A", B => "B", C => "C", D => "D", E => "E", F => "F", G => "G", H => "H",
    I => "I", J => "J", K => "K", L => "L", M => "M", N => "N", O => "O", P => "P",
    Q => "Q", R => "R", S => "S", T => "T", U => "U", V => "V", W => "W", X => "X",
    Y => "Y", Z => "Z",
    Digit0 => "0", Digit1 => "1", Digit2 => "2", Digit3 => "3", Digit4 => "4",
    Digit5 => "5", Digit6 => "6", Digit7 => "7", Digit8 => "8", Digit9 => "9",
    Numpad0 => "Numpad0", Numpad1 => "Numpad1", Numpad2 => "Numpad2", Numpad3 => "Numpad3",
    Numpad4 => "Numpad4", Numpad5 => "Numpad5", Numpad6 => "Numpad6", Numpad7 => "Numpad7",
    Numpad8 => "Numpad8", Numpad9 => "Numpad9",
    Space => "Space", Enter => "Enter", Tab => "Tab", Escape => "Escape",
    Backspace => "Backspace", Insert => "Insert", Delete => "Delete", Home => "Home",
    End => "End", PageUp => "PageUp", PageDown => "PageDown", Up => "Up", Down => "Down",
    Left => "Left", Right => "Right", Pause => "Pause", ScrollLock => "ScrollLock",
}

impl Key {
    /// Looks a key up by its canonical name (case-insensitive).
    pub fn from_name(name: &str) -> Option<Key> {
        Key::ALL
            .iter()
            .copied()
            .find(|k| k.name().eq_ignore_ascii_case(name))
    }

    /// Keys that may be bound without Ctrl or Alt because they are not used
    /// for typing or navigation. F12 is listed here but rejected separately.
    pub fn may_be_unmodified(self) -> bool {
        use Key::*;
        matches!(
            self,
            F1 | F2
                | F3
                | F4
                | F5
                | F6
                | F7
                | F8
                | F9
                | F10
                | F11
                | F12
                | F13
                | F14
                | F15
                | F16
                | F17
                | F18
                | F19
                | F20
                | F21
                | F22
                | F23
                | F24
                | Pause
                | ScrollLock
        )
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Keyboard modifiers. The Windows key is intentionally not supported:
/// shortcuts involving it are reserved by the operating system.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers {
        ctrl: false,
        alt: false,
        shift: false,
    };

    pub const fn is_empty(self) -> bool {
        !self.ctrl && !self.alt && !self.shift
    }

    /// True if any modifier is set in both.
    pub const fn intersects(self, other: Modifiers) -> bool {
        (self.ctrl && other.ctrl) || (self.alt && other.alt) || (self.shift && other.shift)
    }
}

impl fmt::Display for Modifiers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (set, name) in [
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
        ] {
            if set {
                if !first {
                    f.write_str("+")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        if first {
            f.write_str("none")?;
        }
        Ok(())
    }
}

/// A key combination, e.g. `Ctrl+Shift+F8`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub modifiers: Modifiers,
    pub key: Key,
}

impl Hotkey {
    pub const fn new(modifiers: Modifiers, key: Key) -> Self {
        Self { modifiers, key }
    }

    pub const fn plain(key: Key) -> Self {
        Self::new(Modifiers::NONE, key)
    }

    /// Checks the binding rules that apply regardless of other bindings.
    pub fn check_rules(&self) -> Result<(), HotkeyRuleError> {
        if self.key == Key::F12 {
            return Err(HotkeyRuleError::ReservedF12);
        }
        if !self.key.may_be_unmodified() && !self.modifiers.ctrl && !self.modifiers.alt {
            return Err(HotkeyRuleError::NeedsCtrlOrAlt(*self));
        }
        Ok(())
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.modifiers.is_empty() {
            write!(f, "{}+", self.modifiers)?;
        }
        f.write_str(self.key.name())
    }
}

/// Why a hotkey string could not be parsed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HotkeyParseError {
    #[error("hotkey is empty")]
    Empty,
    #[error("unknown key \"{0}\"")]
    UnknownKey(String),
    #[error("unknown modifier \"{0}\" (use Ctrl, Alt or Shift)")]
    UnknownModifier(String),
    #[error("modifier \"{0}\" is listed more than once")]
    DuplicateModifier(String),
}

/// Why a syntactically valid hotkey is not allowed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HotkeyRuleError {
    #[error("F12 is reserved by Windows for debuggers and cannot be used as a global hotkey")]
    ReservedF12,
    #[error(
        "\"{0}\" would block normal typing or navigation in every application; add Ctrl or Alt"
    )]
    NeedsCtrlOrAlt(Hotkey),
}

impl FromStr for Hotkey {
    type Err = HotkeyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split('+').map(str::trim).collect();
        let (key_part, modifier_parts) = match parts.split_last() {
            Some((k, m)) if !k.is_empty() => (*k, m),
            _ => return Err(HotkeyParseError::Empty),
        };
        let key = Key::from_name(key_part)
            .ok_or_else(|| HotkeyParseError::UnknownKey(key_part.into()))?;
        let mut modifiers = Modifiers::NONE;
        for part in modifier_parts {
            let slot = match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => &mut modifiers.ctrl,
                "alt" => &mut modifiers.alt,
                "shift" => &mut modifiers.shift,
                "" => return Err(HotkeyParseError::Empty),
                _ => return Err(HotkeyParseError::UnknownModifier((*part).into())),
            };
            if *slot {
                return Err(HotkeyParseError::DuplicateModifier((*part).into()));
            }
            *slot = true;
        }
        Ok(Hotkey::new(modifiers, key))
    }
}

impl Serialize for Hotkey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Hotkey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL_SHIFT: Modifiers = Modifiers {
        ctrl: true,
        alt: false,
        shift: true,
    };

    #[test]
    fn every_key_name_round_trips() {
        for &key in Key::ALL {
            assert_eq!(Key::from_name(key.name()), Some(key));
            assert_eq!(Key::from_name(&key.name().to_lowercase()), Some(key));
            let hk = Hotkey::new(CTRL_SHIFT, key);
            assert_eq!(hk.to_string().parse::<Hotkey>(), Ok(hk));
        }
    }

    #[test]
    fn parses_modifiers_in_any_order_and_case() {
        let hk: Hotkey = " shift + CTRL + f8 ".parse().unwrap();
        assert_eq!(hk, Hotkey::new(CTRL_SHIFT, Key::F8));
        assert_eq!(hk.to_string(), "Ctrl+Shift+F8");
        assert_eq!(
            "Control+Alt+Q".parse::<Hotkey>().unwrap().to_string(),
            "Ctrl+Alt+Q"
        );
    }

    #[test]
    fn rejects_malformed_hotkeys() {
        assert_eq!("".parse::<Hotkey>(), Err(HotkeyParseError::Empty));
        assert_eq!("Ctrl+".parse::<Hotkey>(), Err(HotkeyParseError::Empty));
        assert_eq!("Ctrl++F1".parse::<Hotkey>(), Err(HotkeyParseError::Empty));
        assert_eq!(
            "Ctrl+F25".parse::<Hotkey>(),
            Err(HotkeyParseError::UnknownKey("F25".into()))
        );
        assert_eq!(
            "Win+F1".parse::<Hotkey>(),
            Err(HotkeyParseError::UnknownModifier("Win".into()))
        );
        assert_eq!(
            "Ctrl+ctrl+F1".parse::<Hotkey>(),
            Err(HotkeyParseError::DuplicateModifier("ctrl".into()))
        );
    }

    #[test]
    fn binding_rules() {
        assert_eq!(Hotkey::plain(Key::F6).check_rules(), Ok(()));
        assert_eq!(Hotkey::plain(Key::Pause).check_rules(), Ok(()));
        assert_eq!(
            Hotkey::plain(Key::F12).check_rules(),
            Err(HotkeyRuleError::ReservedF12)
        );
        assert_eq!(
            Hotkey::new(CTRL_SHIFT, Key::F12).check_rules(),
            Err(HotkeyRuleError::ReservedF12)
        );
        // Shift alone does not make a typing key safe (it types capitals).
        let shift_a = Hotkey::new(
            Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
            Key::A,
        );
        assert_eq!(
            shift_a.check_rules(),
            Err(HotkeyRuleError::NeedsCtrlOrAlt(shift_a))
        );
        assert!(Hotkey::plain(Key::Space).check_rules().is_err());
        assert!(Hotkey::plain(Key::Numpad5).check_rules().is_err());
        assert!(Hotkey::new(CTRL_SHIFT, Key::A).check_rules().is_ok());
    }

    #[test]
    fn modifier_set_operations() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        assert!(CTRL_SHIFT.intersects(ctrl));
        assert!(!ctrl.intersects(Modifiers {
            alt: true,
            ..Modifiers::NONE
        }));
        assert!(Modifiers::NONE.is_empty());
        assert_eq!(Modifiers::NONE.to_string(), "none");
    }
}
