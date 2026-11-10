//! GUI logic that does not depend on the UI toolkit: parsing and validating
//! form fields, mapping key events to bindable keys, and describing engine
//! status for display. Kept separate so it can be unit-tested anywhere.

#![forbid(unsafe_code)]

use autoclicker_core::config::{ClickConfig, HotkeyAction, MAX_INTERVAL_MS, MIN_INTERVAL_MS};
use autoclicker_core::keys::{Hotkey, Key, Modifiers};
use autoclicker_core::status::{RunState, Snapshot};

/// Raw text of the timing fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimingFields {
    pub interval: String,
    /// The unit the interval text is written in (display only; the
    /// configuration always stores whole milliseconds).
    pub interval_unit: IntervalUnit,
    pub jitter: String,
    pub hold: String,
    pub radius: String,
}

impl TimingFields {
    pub fn from_config(c: &ClickConfig, interval_unit: IntervalUnit) -> Self {
        Self {
            interval: format_interval(c.interval_ms, interval_unit),
            interval_unit,
            jitter: format_number(c.interval_jitter_sigma_ms),
            hold: c.hold_ms.to_string(),
            radius: c.position_jitter_radius_px.to_string(),
        }
    }
}

/// How the interval is shown and entered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum IntervalUnit {
    /// Milliseconds between click starts (the stored value).
    #[default]
    Millis,
    /// Clicks per second, `1000 / milliseconds`.
    Cps,
}

impl IntervalUnit {
    pub fn toggled(self) -> Self {
        match self {
            IntervalUnit::Millis => IntervalUnit::Cps,
            IntervalUnit::Cps => IntervalUnit::Millis,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            IntervalUnit::Millis => "ms",
            IntervalUnit::Cps => "CPS",
        }
    }
}

/// Milliseconds that a clicks-per-second value stands for (rounded to the
/// whole milliseconds the configuration stores).
fn cps_to_ms(cps: f64) -> f64 {
    (1000.0 / cps).round()
}

/// The interval as text in `unit`. In CPS it uses the fewest decimals (at
/// least two, trailing zeros trimmed) that convert back to exactly `ms`, so
/// showing, re-reading and toggling the value never changes it.
pub fn format_interval(ms: u32, unit: IntervalUnit) -> String {
    match unit {
        IntervalUnit::Millis => ms.to_string(),
        IntervalUnit::Cps => {
            let cps = 1000.0 / f64::from(ms.max(1));
            (2..=12)
                .map(|decimals| {
                    format!("{cps:.decimals$}")
                        .trim_end_matches('0')
                        .trim_end_matches('.')
                        .to_string()
                })
                .find(|text| {
                    text.parse::<f64>()
                        .is_ok_and(|v| v > 0.0 && cps_to_ms(v) == f64::from(ms))
                })
                .unwrap_or_else(|| format!("{cps}"))
        }
    }
}

fn parse_interval(text: &str, unit: IntervalUnit, errors: &mut Vec<String>) -> Option<u32> {
    match unit {
        IntervalUnit::Millis => parse_whole(text, "Interval", "milliseconds", errors),
        IntervalUnit::Cps => {
            let t = text.trim();
            if t.is_empty() {
                errors.push("Interval: enter a number of clicks per second.".into());
                return None;
            }
            let cps = match t.parse::<f64>() {
                Ok(v) if v.is_finite() && v > 0.0 => v,
                _ => {
                    errors.push(format!(
                        "Interval: \"{t}\" is not a positive number of clicks per second."
                    ));
                    return None;
                }
            };
            // The same limits as in milliseconds, applied to the value that
            // will actually be stored.
            let ms = cps_to_ms(cps);
            if ms < f64::from(MIN_INTERVAL_MS) {
                errors.push(format!(
                    "Interval: {t} CPS is faster than the maximum of {} CPS.",
                    1000 / MIN_INTERVAL_MS
                ));
                return None;
            }
            if ms > f64::from(MAX_INTERVAL_MS) {
                errors.push(format!(
                    "Interval: {t} CPS is slower than the minimum of one click every {} s.",
                    MAX_INTERVAL_MS / 1000
                ));
                return None;
            }
            Some(ms as u32)
        }
    }
}

fn format_number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn parse_whole(text: &str, label: &str, unit: &str, errors: &mut Vec<String>) -> Option<u32> {
    let t = text.trim();
    if t.is_empty() {
        errors.push(format!("{label}: enter a value in {unit}."));
        return None;
    }
    match t.parse::<u32>() {
        Ok(v) => Some(v),
        Err(_) => {
            errors.push(format!("{label}: \"{t}\" is not a whole number of {unit}."));
            None
        }
    }
}

/// Parses the fields on top of `base` and validates the result with the core
/// rules. On failure, every problem is returned as a user-facing message.
pub fn parse_timing(fields: &TimingFields, base: &ClickConfig) -> Result<ClickConfig, Vec<String>> {
    let mut errors = Vec::new();
    let interval = parse_interval(&fields.interval, fields.interval_unit, &mut errors);
    let hold = parse_whole(&fields.hold, "Hold", "milliseconds", &mut errors);
    let radius = parse_whole(&fields.radius, "Position jitter", "pixels", &mut errors);
    let jitter = match fields.jitter.trim() {
        "" => {
            errors.push("Interval jitter: enter a value in milliseconds (0 for none).".into());
            None
        }
        t => match t.parse::<f64>() {
            Ok(v) if v.is_finite() => Some(v),
            _ => {
                errors.push(format!("Interval jitter: \"{t}\" is not a number."));
                None
            }
        },
    };
    let (Some(interval), Some(jitter), Some(hold), Some(radius)) = (interval, jitter, hold, radius)
    else {
        return Err(errors);
    };
    let candidate = ClickConfig {
        interval_ms: interval,
        interval_jitter_sigma_ms: jitter,
        hold_ms: hold,
        position_jitter_radius_px: radius,
        ..base.clone()
    };
    match candidate.validated() {
        Ok(_) => Ok(candidate),
        Err(e) => Err(e
            .0
            .iter()
            .map(|e| capitalize(&e.to_string()) + ".")
            .collect()),
    }
}

/// What a key press during hotkey recording means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyInput {
    /// A modifier on its own: keep waiting for the main key.
    ModifierOnly,
    /// Escape without modifiers: cancel recording.
    Cancel,
    Key(Key),
    /// A key that cannot be bound (described for the user).
    Unsupported(String),
}

// Slint key codes (i-slint-common key_codes.rs).
const SLINT_F1: u32 = 0xF704;
const SLINT_F24: u32 = 0xF71B;

/// Interprets the text of a Slint key event.
pub fn key_from_slint(text: &str) -> KeyInput {
    let mut chars = text.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return KeyInput::Unsupported(if text.is_empty() {
            "that key".into()
        } else {
            format!("\"{text}\"")
        });
    };
    let code = c as u32;
    let key = match code {
        // Shift, Control, Alt, AltGr, CapsLock, ShiftR, ControlR, Meta, MetaR
        0x10..=0x18 => return KeyInput::ModifierOnly,
        0x1B => return KeyInput::Cancel,
        SLINT_F1..=SLINT_F24 => Key::ALL[(code - SLINT_F1) as usize],
        0x08 => Key::Backspace,
        0x09 => Key::Tab,
        0x0A | 0x0D => Key::Enter,
        0x7F => Key::Delete,
        0x20 => Key::Space,
        0xF700 => Key::Up,
        0xF701 => Key::Down,
        0xF702 => Key::Left,
        0xF703 => Key::Right,
        0xF727 => Key::Insert,
        0xF729 => Key::Home,
        0xF72B => Key::End,
        0xF72C => Key::PageUp,
        0xF72D => Key::PageDown,
        0xF72F => Key::ScrollLock,
        0xF730 => Key::Pause,
        _ if c.is_ascii_alphanumeric() => match Key::from_name(&c.to_string()) {
            Some(k) => k,
            None => return KeyInput::Unsupported(format!("\"{c}\"")),
        },
        _ if (0xF700..=0xF8FF).contains(&code) => {
            return KeyInput::Unsupported("that key".into());
        }
        _ => return KeyInput::Unsupported(format!("\"{c}\"")),
    };
    KeyInput::Key(key)
}

/// Escape with a modifier is a key like any other; only plain Escape cancels.
pub fn interpret_key(text: &str, modifiers: Modifiers) -> KeyInput {
    match key_from_slint(text) {
        KeyInput::Cancel if !modifiers.is_empty() => KeyInput::Key(Key::Escape),
        other => other,
    }
}

/// Display name for a binding.
pub fn binding_text(hotkey: Option<Hotkey>) -> String {
    hotkey.map_or_else(|| "Not set".to_string(), |h| h.to_string())
}

pub fn action_label(action: HotkeyAction) -> &'static str {
    match action {
        HotkeyAction::Toggle => "Toggle start/stop",
        HotkeyAction::SetTarget => "Set target",
        HotkeyAction::Exit => "Exit",
    }
}

/// Coarse state shown in the status badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Stopped,
    Waiting,
    Running,
    Error,
}

/// Everything the status area shows.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusView {
    pub status: Status,
    pub headline: &'static str,
    pub error: String,
    pub window_title: String,
    pub target: String,
}

pub const APP_NAME: &str = "Autoclicker";

pub fn status_view(s: &Snapshot) -> StatusView {
    let (status, headline) = match s.state {
        RunState::Idle => (Status::Stopped, "Stopped"),
        RunState::WaitingForModifiers => (Status::Waiting, "Release modifier keys…"),
        RunState::Running => (Status::Running, "Running"),
        RunState::Faulted => (Status::Error, "Error"),
    };
    let error = s
        .fault
        .as_ref()
        .map(|f| capitalize(&f.to_string()))
        .unwrap_or_default();
    let window_title = match status {
        Status::Stopped => APP_NAME.to_string(),
        Status::Waiting | Status::Running => format!("{APP_NAME} - Running"),
        Status::Error => format!("{APP_NAME} - Error"),
    };
    let target = s
        .target
        .map_or_else(|| "Not set".to_string(), |p| format!("{}, {}", p.x, p.y));
    StatusView {
        status,
        headline,
        error,
        window_title,
        target,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoclicker_core::backend::InputError;
    use autoclicker_core::config::TargetMode;
    use autoclicker_core::geometry::Point;
    use autoclicker_core::status::Fault;

    fn fields(i: &str, j: &str, h: &str, r: &str) -> TimingFields {
        TimingFields {
            interval: i.into(),
            interval_unit: IntervalUnit::Millis,
            jitter: j.into(),
            hold: h.into(),
            radius: r.into(),
        }
    }

    #[test]
    fn fields_round_trip_from_config() {
        let base = ClickConfig::default();
        let f = TimingFields::from_config(&base, IntervalUnit::Millis);
        assert_eq!(f, fields("100", "10", "10", "3"));
        assert_eq!(parse_timing(&f, &base), Ok(base));
        let half = ClickConfig {
            interval_jitter_sigma_ms: 2.5,
            ..ClickConfig::default()
        };
        assert_eq!(
            TimingFields::from_config(&half, IntervalUnit::Millis).jitter,
            "2.5"
        );
    }

    #[test]
    fn valid_edits_keep_mode_and_target() {
        let base = ClickConfig {
            mode: TargetMode::Cursor,
            target: Some(Point::new(5, 6)),
            ..ClickConfig::default()
        };
        let c = parse_timing(&fields(" 250 ", "0", "0", "0"), &base).unwrap();
        assert_eq!(c.interval_ms, 250);
        assert_eq!(c.mode, TargetMode::Cursor);
        assert_eq!(c.target, Some(Point::new(5, 6)));
    }

    #[test]
    fn malformed_fields_are_reported_individually() {
        let errs =
            parse_timing(&fields("", "abc", "1.5", "-1"), &ClickConfig::default()).unwrap_err();
        assert_eq!(errs.len(), 4, "{errs:?}");
        assert!(errs[0].starts_with("Interval: enter a value"));
        assert!(errs[1].contains("\"1.5\" is not a whole number"));
        assert!(errs[2].contains("\"-1\" is not a whole number of pixels"));
        assert!(errs[3].contains("\"abc\" is not a number"));
    }

    #[test]
    fn core_rules_are_applied_without_clamping() {
        let base = ClickConfig::default();
        let errs = parse_timing(&fields("5", "0", "0", "3"), &base).unwrap_err();
        assert_eq!(
            errs,
            vec!["Interval of 5 ms is outside the allowed range of 10 to 3600000 ms."]
        );
        let errs = parse_timing(&fields("100", "10", "70", "3"), &base).unwrap_err();
        assert!(
            errs[0].starts_with("Hold of 70 ms does not fit"),
            "{errs:?}"
        );
        let errs = parse_timing(&fields("100", "40", "0", "3"), &base).unwrap_err();
        assert!(errs[0].contains("jitter"), "{errs:?}");
        let errs = parse_timing(&fields("100", "10", "10", "51"), &base).unwrap_err();
        assert!(errs[0].contains("51 px"), "{errs:?}");
    }

    fn cps_fields(i: &str) -> TimingFields {
        TimingFields {
            interval_unit: IntervalUnit::Cps,
            ..fields(i, "0", "0", "3")
        }
    }

    #[test]
    fn interval_is_shown_in_cps_with_minimal_decimals() {
        assert_eq!(format_interval(70, IntervalUnit::Cps), "14.29");
        assert_eq!(format_interval(100, IntervalUnit::Cps), "10");
        assert_eq!(format_interval(10, IntervalUnit::Cps), "100");
        assert_eq!(format_interval(400, IntervalUnit::Cps), "2.5");
        assert_eq!(format_interval(70, IntervalUnit::Millis), "70");
        assert_eq!(IntervalUnit::Millis.toggled(), IntervalUnit::Cps);
        assert_eq!(IntervalUnit::Cps.toggled().label(), "ms");
    }

    #[test]
    fn cps_text_converts_back_to_the_exact_stored_interval() {
        // Every allowed interval up to 100 s, plus the slow end of the range.
        let slow = (MAX_INTERVAL_MS - 2000)..=MAX_INTERVAL_MS;
        for ms in (MIN_INTERVAL_MS..=100_000).chain(slow) {
            let text = format_interval(ms, IntervalUnit::Cps);
            let mut errors = Vec::new();
            let back = parse_interval(&text, IntervalUnit::Cps, &mut errors);
            assert_eq!(back, Some(ms), "{ms} ms shown as {text} CPS");
        }
    }

    #[test]
    fn toggling_units_repeatedly_never_drifts() {
        let mut base = ClickConfig::default();
        for ms in [70, 333, 3334, 12_345, MAX_INTERVAL_MS] {
            base.interval_ms = ms;
            base.interval_jitter_sigma_ms = 0.0;
            base.hold_ms = 0;
            let mut unit = IntervalUnit::Millis;
            for _ in 0..20 {
                unit = unit.toggled();
                let f = TimingFields {
                    interval_unit: unit,
                    ..TimingFields::from_config(&base, unit)
                };
                let parsed = parse_timing(&f, &base).unwrap();
                assert_eq!(parsed.interval_ms, ms, "{unit:?}: {}", f.interval);
                assert_eq!(parsed, base, "re-reading the shown value changes nothing");
            }
        }
    }

    #[test]
    fn cps_entries_use_the_same_limits_as_milliseconds() {
        let base = ClickConfig::default();
        assert_eq!(
            parse_timing(&cps_fields("14.29"), &base)
                .unwrap()
                .interval_ms,
            70
        );
        assert_eq!(
            parse_timing(&cps_fields("3"), &base).unwrap().interval_ms,
            333
        );
        // 100 CPS = 10 ms is the fastest allowed; 101 CPS rounds to 10 ms too.
        assert_eq!(
            parse_timing(&cps_fields("100"), &base).unwrap().interval_ms,
            10
        );
        let errs = parse_timing(&cps_fields("200"), &base).unwrap_err();
        assert_eq!(
            errs,
            vec!["Interval: 200 CPS is faster than the maximum of 100 CPS."]
        );
        let errs = parse_timing(&cps_fields("0.0001"), &base).unwrap_err();
        assert!(
            errs[0].contains("slower than the minimum of one click every 3600 s"),
            "{errs:?}"
        );
        for bad in ["0", "-2", "abc", "inf"] {
            let errs = parse_timing(&cps_fields(bad), &base).unwrap_err();
            assert!(
                errs[0].contains("not a positive number of clicks per second"),
                "{bad}: {errs:?}"
            );
        }
        let errs = parse_timing(&cps_fields(""), &base).unwrap_err();
        assert!(errs[0].starts_with("Interval: enter a number of clicks per second"));
    }

    #[test]
    fn slint_key_codes_map_to_bindable_keys() {
        let k = |c: u32| key_from_slint(&char::from_u32(c).unwrap().to_string());
        assert_eq!(k(0xF704), KeyInput::Key(Key::F1));
        assert_eq!(k(0xF709), KeyInput::Key(Key::F6));
        assert_eq!(k(0xF71B), KeyInput::Key(Key::F24));
        assert_eq!(k(0xF730), KeyInput::Key(Key::Pause));
        assert_eq!(k(0xF72F), KeyInput::Key(Key::ScrollLock));
        assert_eq!(k(0xF700), KeyInput::Key(Key::Up));
        assert_eq!(k(0x7F), KeyInput::Key(Key::Delete));
        assert_eq!(k(0x11), KeyInput::ModifierOnly);
        assert_eq!(k(0x10), KeyInput::ModifierOnly);
        assert_eq!(k(0x1B), KeyInput::Cancel);
        assert_eq!(key_from_slint("a"), KeyInput::Key(Key::A));
        assert_eq!(key_from_slint("Q"), KeyInput::Key(Key::Q));
        assert_eq!(key_from_slint("7"), KeyInput::Key(Key::Digit7));
        assert_eq!(key_from_slint(" "), KeyInput::Key(Key::Space));
        assert!(matches!(key_from_slint("!"), KeyInput::Unsupported(_)));
        assert!(matches!(key_from_slint("é"), KeyInput::Unsupported(_)));
        assert!(matches!(k(0xF72E), KeyInput::Unsupported(_))); // PrintScreen
        assert!(matches!(key_from_slint(""), KeyInput::Unsupported(_)));
    }

    #[test]
    fn escape_cancels_only_without_modifiers() {
        let esc = "\u{1b}";
        assert_eq!(interpret_key(esc, Modifiers::NONE), KeyInput::Cancel);
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        assert_eq!(interpret_key(esc, ctrl), KeyInput::Key(Key::Escape));
    }

    #[test]
    fn status_views() {
        let mut s = Snapshot::new(TargetMode::Fixed);
        let v = status_view(&s);
        assert_eq!(v.status, Status::Stopped);
        assert_eq!(v.window_title, "Autoclicker");
        assert_eq!(v.target, "Not set");

        s.state = RunState::Running;
        s.run_clicks = 1;
        s.target = Some(Point::new(-5, 10));
        let v = status_view(&s);
        assert_eq!(v.status, Status::Running);
        assert_eq!(v.window_title, "Autoclicker - Running");
        assert_eq!(v.target, "-5, 10");

        s.state = RunState::Faulted;
        s.fault = Some(Fault::PressFailed(InputError::new("SendInput", "blocked")));
        let v = status_view(&s);
        assert_eq!(v.status, Status::Error);
        assert_eq!(v.window_title, "Autoclicker - Error");
        assert!(v.error.starts_with("Could not press"), "{}", v.error);
    }
}
