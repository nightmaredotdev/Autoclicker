//! User configuration, its validation and schema versioning.
//!
//! [`Config`] is the serializable form. [`Config::validate`] turns it into a
//! [`ValidatedConfig`]; invalid values are rejected with descriptive errors and
//! never silently clamped.

use crate::geometry::Point;
use crate::keys::{Hotkey, HotkeyRuleError, Key, Modifiers};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

/// Current configuration schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// Shortest allowed nominal click interval (100 clicks per second).
pub const MIN_INTERVAL_MS: u32 = 10;
/// Longest allowed nominal click interval (one hour).
pub const MAX_INTERVAL_MS: u32 = 3_600_000;
/// Largest allowed position-jitter radius.
pub const MAX_POSITION_JITTER_PX: u32 = 50;
/// Interval jitter is a Gaussian truncated at this many standard deviations.
pub const JITTER_TRUNCATION_SIGMAS: f64 = 3.0;
/// Minimum gap between a release and the next press.
pub const MIN_RELEASE_GAP: Duration = Duration::from_millis(1);

/// Where clicks are performed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetMode {
    /// Move to a saved target point (plus position jitter) before each click.
    Fixed,
    /// Click wherever the cursor currently is; the cursor is never moved.
    Cursor,
}

impl fmt::Display for TargetMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TargetMode::Fixed => "fixed position",
            TargetMode::Cursor => "current position",
        })
    }
}

/// Serializable configuration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub click: ClickConfig,
    pub hotkeys: HotkeyConfig,
    #[serde(default)]
    pub window: WindowConfig,
}

/// Click timing and positioning.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClickConfig {
    pub mode: TargetMode,
    /// Nominal start-to-start interval between presses.
    pub interval_ms: u32,
    /// Standard deviation of the Gaussian interval jitter (0 disables it).
    pub interval_jitter_sigma_ms: f64,
    /// How long the button is held down (0 sends press and release together).
    pub hold_ms: u32,
    /// Maximum distance of a click from the fixed target (0 disables it).
    pub position_jitter_radius_px: u32,
    /// The saved fixed target, if any. Whether it is still on a connected
    /// monitor is checked by the platform layer, not here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Point>,
}

/// Window preferences of the desktop application.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowConfig {
    pub always_on_top: bool,
}

/// Global hotkey bindings. `None` means unbound.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HotkeyConfig {
    pub toggle: Option<Hotkey>,
    pub set_target: Option<Hotkey>,
    pub exit: Option<Hotkey>,
    /// Separate start/stop bindings of pre-1.0 builds. Accepted when loading
    /// older files (their values are not even parsed, so a malformed old entry
    /// cannot break loading), never used, and dropped on the next save.
    #[serde(default, rename = "start", skip_serializing)]
    obsolete_start: Option<IgnoredAny>,
    #[serde(default, rename = "stop", skip_serializing)]
    obsolete_stop: Option<IgnoredAny>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            click: ClickConfig::default(),
            hotkeys: HotkeyConfig::default(),
            window: WindowConfig::default(),
        }
    }
}

impl Default for ClickConfig {
    fn default() -> Self {
        Self {
            mode: TargetMode::Fixed,
            interval_ms: 100,
            interval_jitter_sigma_ms: 10.0,
            hold_ms: 10,
            position_jitter_radius_px: 3,
            target: None,
        }
    }
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            toggle: Some(Hotkey::plain(Key::F6)),
            set_target: Some(Hotkey::plain(Key::F7)),
            exit: Some(Hotkey::new(
                Modifiers {
                    ctrl: true,
                    alt: false,
                    shift: true,
                },
                Key::F8,
            )),
            obsolete_start: None,
            obsolete_stop: None,
        }
    }
}

/// An action that can be bound to a global hotkey.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    Toggle,
    SetTarget,
    Exit,
}

impl HotkeyAction {
    pub const ALL: [HotkeyAction; 3] = [
        HotkeyAction::Toggle,
        HotkeyAction::SetTarget,
        HotkeyAction::Exit,
    ];
}

impl fmt::Display for HotkeyAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            HotkeyAction::Toggle => "toggle",
            HotkeyAction::SetTarget => "set target",
            HotkeyAction::Exit => "exit",
        })
    }
}

impl HotkeyConfig {
    /// No bindings at all.
    pub fn none() -> Self {
        Self {
            toggle: None,
            set_target: None,
            exit: None,
            obsolete_start: None,
            obsolete_stop: None,
        }
    }

    pub fn get(&self, action: HotkeyAction) -> Option<Hotkey> {
        match action {
            HotkeyAction::Toggle => self.toggle,
            HotkeyAction::SetTarget => self.set_target,
            HotkeyAction::Exit => self.exit,
        }
    }

    pub fn set(&mut self, action: HotkeyAction, hotkey: Option<Hotkey>) {
        let slot = match action {
            HotkeyAction::Toggle => &mut self.toggle,
            HotkeyAction::SetTarget => &mut self.set_target,
            HotkeyAction::Exit => &mut self.exit,
        };
        *slot = hotkey;
    }

    /// Whether the loaded file contained the obsolete separate start/stop
    /// bindings (ignored; the start/stop toggle replaces them).
    pub fn had_obsolete_bindings(&self) -> bool {
        self.obsolete_start.is_some() || self.obsolete_stop.is_some()
    }

    /// Forgets the obsolete bindings (they are never saved anyway).
    pub fn clear_obsolete_bindings(&mut self) {
        self.obsolete_start = None;
        self.obsolete_stop = None;
    }
}

/// Validated click settings, consumed by the engine.
#[derive(Clone, Debug, PartialEq)]
pub struct ClickSettings {
    pub(crate) mode: TargetMode,
    pub(crate) interval: Duration,
    pub(crate) jitter_sigma_ms: f64,
    pub(crate) hold: Duration,
    pub(crate) radius_px: u32,
}

impl ClickSettings {
    pub fn mode(&self) -> TargetMode {
        self.mode
    }
    pub fn interval(&self) -> Duration {
        self.interval
    }
    pub fn jitter_sigma_ms(&self) -> f64 {
        self.jitter_sigma_ms
    }
    pub fn hold(&self) -> Duration {
        self.hold
    }
    pub fn radius_px(&self) -> u32 {
        self.radius_px
    }
    /// The shortest interval the jitter can produce.
    pub fn min_interval(&self) -> Duration {
        Duration::from_secs_f64(
            (self.interval.as_secs_f64() * 1000.0
                - JITTER_TRUNCATION_SIGMAS * self.jitter_sigma_ms)
                / 1000.0,
        )
    }
}

/// Validated, conflict-free hotkey bindings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeyBindings {
    bindings: Vec<(HotkeyAction, Hotkey)>,
}

impl HotkeyBindings {
    pub fn iter(&self) -> impl Iterator<Item = (HotkeyAction, Hotkey)> + '_ {
        self.bindings.iter().copied()
    }
    pub fn get(&self, action: HotkeyAction) -> Option<Hotkey> {
        self.iter().find(|(a, _)| *a == action).map(|(_, h)| h)
    }
}

/// A fully validated configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidatedConfig {
    pub click: ClickSettings,
    pub hotkeys: HotkeyBindings,
}

/// A single configuration problem.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("unsupported configuration version {found} (this build supports version {supported})")]
    UnsupportedVersion { found: u32, supported: u32 },
    #[error("interval of {value} ms is outside the allowed range of {min} to {max} ms")]
    IntervalOutOfRange { value: u32, min: u32, max: u32 },
    #[error("interval jitter must be a finite number of milliseconds >= 0 (got {0})")]
    InvalidJitter(f64),
    #[error(
        "interval jitter σ = {sigma_ms} ms allows intervals as short as {shortest_ms:.1} ms; \
         the minimum is {min} ms (reduce the jitter or increase the interval)"
    )]
    JitterTooLarge {
        sigma_ms: f64,
        shortest_ms: f64,
        min: u32,
    },
    #[error(
        "hold of {hold_ms} ms does not fit in the shortest possible interval of {shortest_ms:.1} ms \
         (hold + 1 ms must not exceed it)"
    )]
    HoldTooLong { hold_ms: u32, shortest_ms: f64 },
    #[error("position jitter radius of {value} px exceeds the maximum of {max} px")]
    RadiusTooLarge { value: u32, max: u32 },
    #[error("hotkey for {action}: {error}")]
    InvalidHotkey {
        action: HotkeyAction,
        error: HotkeyRuleError,
    },
    #[error("{hotkey} is bound to both {first} and {second}")]
    DuplicateHotkey {
        hotkey: Hotkey,
        first: HotkeyAction,
        second: HotkeyAction,
    },
}

/// All problems found in a configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigErrors(pub Vec<ConfigError>);

impl fmt::Display for ConfigErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "invalid configuration:")?;
        for e in &self.0 {
            writeln!(f, "  - {e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigErrors {}

impl Config {
    /// Validates every field and returns all problems at once.
    pub fn validate(&self) -> Result<ValidatedConfig, ConfigErrors> {
        let mut errors = Vec::new();
        if self.version != SCHEMA_VERSION {
            errors.push(ConfigError::UnsupportedVersion {
                found: self.version,
                supported: SCHEMA_VERSION,
            });
        }
        let click = self.click.validate(&mut errors);
        let hotkeys = self.hotkeys.validate(&mut errors);
        match (click, errors.is_empty()) {
            (Some(click), true) => Ok(ValidatedConfig { click, hotkeys }),
            _ => Err(ConfigErrors(errors)),
        }
    }
}

impl ClickConfig {
    /// Validates the click settings on their own.
    pub fn validated(&self) -> Result<ClickSettings, ConfigErrors> {
        let mut errors = Vec::new();
        match self.validate(&mut errors) {
            Some(s) if errors.is_empty() => Ok(s),
            _ => Err(ConfigErrors(errors)),
        }
    }

    fn validate(&self, errors: &mut Vec<ConfigError>) -> Option<ClickSettings> {
        let before = errors.len();
        if !(MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&self.interval_ms) {
            errors.push(ConfigError::IntervalOutOfRange {
                value: self.interval_ms,
                min: MIN_INTERVAL_MS,
                max: MAX_INTERVAL_MS,
            });
        }
        let sigma = self.interval_jitter_sigma_ms;
        if !sigma.is_finite() || sigma < 0.0 {
            errors.push(ConfigError::InvalidJitter(sigma));
        }
        if errors.len() == before {
            let shortest = f64::from(self.interval_ms) - JITTER_TRUNCATION_SIGMAS * sigma;
            if shortest < f64::from(MIN_INTERVAL_MS) {
                errors.push(ConfigError::JitterTooLarge {
                    sigma_ms: sigma,
                    shortest_ms: shortest,
                    min: MIN_INTERVAL_MS,
                });
            }
            let gap_ms = MIN_RELEASE_GAP.as_secs_f64() * 1000.0;
            if f64::from(self.hold_ms) + gap_ms > shortest {
                errors.push(ConfigError::HoldTooLong {
                    hold_ms: self.hold_ms,
                    shortest_ms: shortest,
                });
            }
        }
        if self.position_jitter_radius_px > MAX_POSITION_JITTER_PX {
            errors.push(ConfigError::RadiusTooLarge {
                value: self.position_jitter_radius_px,
                max: MAX_POSITION_JITTER_PX,
            });
        }
        (errors.len() == before).then(|| ClickSettings {
            mode: self.mode,
            interval: Duration::from_millis(self.interval_ms.into()),
            jitter_sigma_ms: sigma,
            hold: Duration::from_millis(self.hold_ms.into()),
            radius_px: self.position_jitter_radius_px,
        })
    }
}

impl HotkeyConfig {
    fn validate(&self, errors: &mut Vec<ConfigError>) -> HotkeyBindings {
        let mut bindings: Vec<(HotkeyAction, Hotkey)> = Vec::new();
        for action in HotkeyAction::ALL {
            let Some(hotkey) = self.get(action) else {
                continue;
            };
            if let Err(error) = hotkey.check_rules() {
                errors.push(ConfigError::InvalidHotkey { action, error });
                continue;
            }
            if let Some(&(first, _)) = bindings.iter().find(|(_, h)| *h == hotkey) {
                errors.push(ConfigError::DuplicateHotkey {
                    hotkey,
                    first,
                    second: action,
                });
                continue;
            }
            bindings.push((action, hotkey));
        }
        HotkeyBindings { bindings }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(f: impl FnOnce(&mut ClickConfig)) -> Result<ClickSettings, ConfigErrors> {
        let mut c = ClickConfig::default();
        f(&mut c);
        c.validated()
    }

    #[test]
    fn defaults_match_the_approved_plan_and_are_valid() {
        let cfg = Config::default();
        let v = cfg.validate().expect("defaults must be valid");
        assert_eq!(v.click.mode(), TargetMode::Fixed);
        assert_eq!(v.click.interval(), Duration::from_millis(100));
        assert_eq!(v.click.jitter_sigma_ms(), 10.0);
        assert_eq!(v.click.hold(), Duration::from_millis(10));
        assert_eq!(v.click.radius_px(), 3);
        let hk = |a| v.hotkeys.get(a).map(|h| h.to_string());
        assert_eq!(hk(HotkeyAction::Toggle).as_deref(), Some("F6"));
        assert_eq!(hk(HotkeyAction::SetTarget).as_deref(), Some("F7"));
        assert_eq!(hk(HotkeyAction::Exit).as_deref(), Some("Ctrl+Shift+F8"));
        assert_eq!(
            HotkeyAction::ALL.len(),
            3,
            "only toggle, set target and exit"
        );
    }

    #[test]
    fn rejects_out_of_range_intervals_without_clamping() {
        for bad in [0, 9, 3_600_001] {
            let errs = click(|c| {
                c.interval_ms = bad;
                c.interval_jitter_sigma_ms = 0.0;
                c.hold_ms = 0;
            })
            .unwrap_err();
            assert!(matches!(
                errs.0[0],
                ConfigError::IntervalOutOfRange { value, .. } if value == bad
            ));
        }
        assert!(
            click(|c| {
                c.interval_ms = 10;
                c.interval_jitter_sigma_ms = 0.0;
                c.hold_ms = 9;
            })
            .is_ok()
        );
    }

    #[test]
    fn rejects_invalid_or_excessive_jitter() {
        for bad in [-1.0, f64::NAN, f64::INFINITY] {
            let errs = click(|c| c.interval_jitter_sigma_ms = bad).unwrap_err();
            assert!(matches!(errs.0[0], ConfigError::InvalidJitter(_)));
        }
        // 100 - 3 * 31 = 7 ms < 10 ms minimum.
        let errs = click(|c| {
            c.interval_jitter_sigma_ms = 31.0;
            c.hold_ms = 0;
        })
        .unwrap_err();
        assert!(matches!(errs.0[0], ConfigError::JitterTooLarge { .. }));
        // 100 - 3 * 30 = 10 ms: allowed with a hold of at most 9 ms.
        assert!(
            click(|c| {
                c.interval_jitter_sigma_ms = 30.0;
                c.hold_ms = 9;
            })
            .is_ok()
        );
    }

    #[test]
    fn hold_must_fit_in_the_shortest_interval() {
        // Shortest interval is 70 ms: hold 69 fits (69 + 1 = 70), 70 does not.
        assert!(click(|c| c.hold_ms = 69).is_ok());
        let errs = click(|c| c.hold_ms = 70).unwrap_err();
        assert!(matches!(
            errs.0[0],
            ConfigError::HoldTooLong { hold_ms: 70, .. }
        ));
        assert!(click(|c| c.hold_ms = 0).is_ok());
    }

    #[test]
    fn rejects_large_position_jitter() {
        assert!(click(|c| c.position_jitter_radius_px = 50).is_ok());
        let errs = click(|c| c.position_jitter_radius_px = 51).unwrap_err();
        assert_eq!(
            errs.0,
            vec![ConfigError::RadiusTooLarge { value: 51, max: 50 }]
        );
    }

    #[test]
    fn reports_every_problem_at_once() {
        let mut cfg = Config {
            version: 2,
            ..Config::default()
        };
        cfg.click.interval_ms = 5;
        cfg.click.position_jitter_radius_px = 99;
        cfg.hotkeys.exit = Some(Hotkey::plain(Key::F12));
        cfg.hotkeys.set_target = Some(Hotkey::plain(Key::F6));
        let errs = cfg.validate().unwrap_err().0;
        assert_eq!(errs.len(), 5, "{errs:?}");
        assert!(errs.contains(&ConfigError::UnsupportedVersion {
            found: 2,
            supported: 1
        }));
        assert!(errs.contains(&ConfigError::DuplicateHotkey {
            hotkey: Hotkey::plain(Key::F6),
            first: HotkeyAction::Toggle,
            second: HotkeyAction::SetTarget,
        }));
        assert!(errs.iter().any(|e| matches!(
            e,
            ConfigError::InvalidHotkey {
                action: HotkeyAction::Exit,
                error: HotkeyRuleError::ReservedF12
            }
        )));
    }

    #[test]
    fn same_key_for_two_actions_is_rejected() {
        let mut cfg = Config::default();
        cfg.hotkeys.exit = Some(Hotkey::plain(Key::F7));
        let errs = cfg.validate().unwrap_err().0;
        assert!(matches!(
            errs[0],
            ConfigError::DuplicateHotkey {
                first: HotkeyAction::SetTarget,
                second: HotkeyAction::Exit,
                ..
            }
        ));
    }

    #[test]
    fn files_with_obsolete_start_stop_bindings_still_load() {
        // Written by pre-1.0 builds, including a value that would not even parse.
        let old = r#"
version = 1

[click]
mode = "cursor"
interval_ms = 250
interval_jitter_sigma_ms = 5.0
hold_ms = 20
position_jitter_radius_px = 0

[hotkeys]
toggle = "F9"
start = "Ctrl+Alt+S"
stop = "Ctrl+Nonsense"
set_target = "F10"
exit = "Ctrl+Shift+F11"
"#;
        let cfg: Config = toml::from_str(old).expect("older file loads");
        assert!(cfg.hotkeys.had_obsolete_bindings());
        let v = cfg.validate().expect("still valid");
        assert_eq!(
            v.hotkeys.get(HotkeyAction::Toggle),
            Some(Hotkey::plain(Key::F9))
        );
        assert_eq!(
            v.hotkeys.get(HotkeyAction::SetTarget),
            Some(Hotkey::plain(Key::F10))
        );
        assert_eq!(
            v.hotkeys.iter().count(),
            3,
            "obsolete bindings are not registered"
        );
        assert_eq!(cfg.click.interval_ms, 250, "other settings are kept");

        // They are never written back.
        let saved = toml::to_string(&cfg).unwrap();
        assert!(
            !saved.contains("start") && !saved.contains("stop ="),
            "{saved}"
        );
        let reloaded: Config = toml::from_str(&saved).unwrap();
        assert!(!reloaded.hotkeys.had_obsolete_bindings());

        // A file without them is unaffected.
        let current: Config =
            toml::from_str(&toml::to_string(&Config::default()).unwrap()).unwrap();
        assert!(!current.hotkeys.had_obsolete_bindings());
    }

    #[test]
    fn toml_round_trip_and_schema_strictness() {
        let cfg = Config::default();
        let text = toml::to_string(&cfg).unwrap();
        assert!(text.contains("exit = \"Ctrl+Shift+F8\""), "{text}");
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, cfg);

        let unknown = text.replace("[click]", "[click]\nspeed = 3");
        assert!(toml::from_str::<Config>(&unknown).is_err());

        let bad_key = text.replace("\"F6\"", "\"Ctrl+Nope\"");
        let err = toml::from_str::<Config>(&bad_key).unwrap_err().to_string();
        assert!(err.contains("unknown key"), "{err}");
    }

    #[test]
    fn target_and_window_settings_round_trip_and_are_optional() {
        let mut cfg = Config::default();
        cfg.click.target = Some(Point::new(-1200, 340));
        cfg.window.always_on_top = true;
        let text = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back, cfg);

        // Files without the optional parts still load.
        let plain = toml::to_string(&Config::default()).unwrap();
        assert!(!plain.contains("[click.target]"), "{plain}");
        let stripped: String = plain.split("[window]").next().unwrap().to_string();
        let back: Config = toml::from_str(&stripped).unwrap();
        assert_eq!(back, Config::default());
    }

    #[test]
    fn hotkey_set_and_get_are_symmetric() {
        let mut h = HotkeyConfig::default();
        for action in HotkeyAction::ALL {
            h.set(action, Some(Hotkey::plain(Key::F20)));
            assert_eq!(h.get(action), Some(Hotkey::plain(Key::F20)));
            h.set(action, None);
            assert_eq!(h.get(action), None);
        }
    }

    #[test]
    fn min_interval_accounts_for_truncated_jitter() {
        let s = ClickConfig::default().validated().unwrap();
        assert_eq!(s.min_interval(), Duration::from_millis(70));
    }
}
