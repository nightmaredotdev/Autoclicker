//! Loading and saving the versioned TOML configuration.
//!
//! * Location: `%APPDATA%\Cadence\config.toml` (via the platform's
//!   per-user configuration directory). The `CADENCE_CONFIG` environment
//!   variable overrides the full path (used by tests).
//! * Settings of the former product, Autoclicker 1.0.0
//!   (`%APPDATA%\Autoclicker\config.toml`), are copied once to the Cadence
//!   location if Cadence has none yet ([`migrate_legacy`]). The old file is
//!   never changed.
//! * Saving writes a temporary file next to the target and renames it over
//!   the old one, so a crash never leaves a half-written file.
//! * Only validated configurations are saved.
//! * A file that cannot be parsed or fails validation is moved aside to
//!   `config.toml.bak` and defaults are used; the caller shows the warning.

#![forbid(unsafe_code)]

use cadence_core::config::Config;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const PATH_OVERRIDE_ENV: &str = "CADENCE_CONFIG";
const APP_DIR: &str = "Cadence";
/// Settings folder of Autoclicker 1.0.0, the former name of this application.
const LEGACY_APP_DIR: &str = "Autoclicker";
const FILE_NAME: &str = "config.toml";

/// The configuration file path, or `None` if no per-user directory exists.
pub fn default_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(PATH_OVERRIDE_ENV) {
        return Some(PathBuf::from(p));
    }
    directories::BaseDirs::new().map(|d| d.config_dir().join(APP_DIR).join(FILE_NAME))
}

/// Where Autoclicker 1.0.0 kept its settings, or `None` if no per-user
/// directory exists.
pub fn legacy_path() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|d| d.config_dir().join(LEGACY_APP_DIR).join(FILE_NAME))
}

/// Outcome of [`migrate_legacy`].
#[derive(Debug, PartialEq, Eq)]
pub enum Migration {
    /// Nothing to do: Cadence already has settings, or there are no old ones.
    NotNeeded,
    /// The old settings were copied unchanged.
    Copied,
    /// The old settings exist but were not copied (unreadable or not valid
    /// settings); Cadence starts with defaults. The reason is for the log.
    Skipped(String),
}

/// Copies the settings of Autoclicker 1.0.0 (`legacy`) to `path` when `path`
/// does not exist yet. The old file is copied byte for byte, so it is loaded
/// with exactly the same meaning (including the one-time cleanup of pre-1.0
/// fields), and only if it is valid; an unreadable or malformed old file is
/// left alone and Cadence starts with defaults. Never changes the old file
/// and never replaces an existing Cadence file (the copy is created with
/// "create new" semantics).
pub fn migrate_legacy(path: &Path, legacy: &Path) -> Migration {
    match path.try_exists() {
        Ok(false) => {}
        // Exists, or cannot be determined: never risk replacing it.
        Ok(true) | Err(_) => return Migration::NotNeeded,
    }
    let bytes = match fs::read(legacy) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Migration::NotNeeded,
        Err(e) => return Migration::Skipped(format!("it could not be read ({e})")),
    };
    let usable = std::str::from_utf8(&bytes)
        .map_err(|e| e.to_string())
        .and_then(|text| toml::from_str::<Config>(text).map_err(|e| e.message().to_string()))
        .and_then(|config| {
            config
                .validate()
                .map(|_| ())
                .map_err(|e| e.to_string().trim_end().to_string())
        });
    if let Err(problem) = usable {
        return Migration::Skipped(format!("it is not valid ({problem})"));
    }
    match write_new(path, &bytes) {
        Ok(()) => Migration::Copied,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Migration::NotNeeded,
        Err(e) => Migration::Skipped(format!("the copy could not be written ({e})")),
    }
}

/// Creates `path` (and its directory) with `bytes`; fails if it exists. A
/// partly written file is removed.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .inspect_err(|_| {
            drop(fs::remove_file(path));
        })
}

/// The result of loading: always a valid configuration, plus a warning for
/// the user if something had to be recovered.
#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    pub warning: Option<String>,
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// Loads the configuration from `path`. Never fails: problems produce
/// defaults plus a warning.
pub fn load(path: &Path) -> Loaded {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Loaded {
                config: Config::default(),
                warning: None,
            };
        }
        Err(e) => {
            return Loaded {
                config: Config::default(),
                warning: Some(format!(
                    "Could not read the settings file {} ({e}). Default settings are in use, \
                     and changes will try to replace the file.",
                    path.display()
                )),
            };
        }
    };
    let problem = match toml::from_str::<Config>(&text) {
        Ok(config) => match config.validate() {
            Ok(_) => {
                return Loaded {
                    config,
                    warning: None,
                };
            }
            Err(errors) => errors.to_string().trim_end().to_string(),
        },
        Err(e) => format!("it could not be parsed: {}", e.message()),
    };
    let backup = backup_path(path);
    let moved = fs::rename(path, &backup);
    let what = match moved {
        Ok(()) => format!("It was moved to {}.", backup.display()),
        Err(e) => format!("It could not be moved aside ({e})."),
    };
    Loaded {
        config: Config::default(),
        warning: Some(format!(
            "The settings file {} was not usable and default settings were loaded. {what}\nDetails: {problem}",
            path.display()
        )),
    }
}

/// Validates and atomically writes `config` to `path`.
pub fn save(path: &Path, config: &Config) -> io::Result<()> {
    if let Err(errors) = config.validate() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            errors.to_string(),
        ));
    }
    let text = toml::to_string_pretty(config).map_err(io::Error::other)?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    fs::write(&tmp, text)?;
    // `fs::rename` replaces an existing file on Windows (MoveFileExW with
    // MOVEFILE_REPLACE_EXISTING).
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cadence_core::config::TargetMode;
    use cadence_core::geometry::Point;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "cadence-test-{name}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn file(&self) -> PathBuf {
            self.0.join("nested").join("config.toml")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn missing_file_gives_defaults_without_warning() {
        let t = TempDir::new("missing");
        let l = load(&t.file());
        assert_eq!(l.config, Config::default());
        assert!(l.warning.is_none());
    }

    #[test]
    fn save_then_load_round_trips_and_creates_directories() {
        let t = TempDir::new("roundtrip");
        let mut cfg = Config::default();
        cfg.click.mode = TargetMode::Cursor;
        cfg.click.interval_ms = 250;
        cfg.click.target = Some(Point::new(-10, 20));
        cfg.window.always_on_top = true;
        save(&t.file(), &cfg).unwrap();
        let l = load(&t.file());
        assert_eq!(l.config, cfg);
        assert!(l.warning.is_none());
        // No temporary file is left behind.
        let leftovers: Vec<_> = fs::read_dir(t.file().parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("config.toml")]);
    }

    #[test]
    fn saving_replaces_an_existing_file() {
        let t = TempDir::new("replace");
        let mut cfg = Config::default();
        save(&t.file(), &cfg).unwrap();
        cfg.click.hold_ms = 20;
        save(&t.file(), &cfg).unwrap();
        assert_eq!(load(&t.file()).config.click.hold_ms, 20);
    }

    #[test]
    fn invalid_configurations_are_never_saved() {
        let t = TempDir::new("invalid-save");
        let mut cfg = Config::default();
        cfg.click.interval_ms = 1;
        let err = save(&t.file(), &cfg).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("interval"), "{err}");
        assert!(!t.file().exists());
    }

    #[test]
    fn corrupt_file_is_moved_aside_with_a_warning() {
        let t = TempDir::new("corrupt");
        fs::create_dir_all(t.file().parent().unwrap()).unwrap();
        fs::write(t.file(), "this is = = not toml").unwrap();
        let l = load(&t.file());
        assert_eq!(l.config, Config::default());
        let w = l.warning.expect("warning");
        assert!(w.contains("could not be parsed"), "{w}");
        assert!(w.contains("config.toml.bak"), "{w}");
        assert!(!t.file().exists());
        let bak = t.file().with_file_name("config.toml.bak");
        assert_eq!(fs::read_to_string(bak).unwrap(), "this is = = not toml");
    }

    #[test]
    fn semantically_invalid_file_is_moved_aside_with_the_reason() {
        let t = TempDir::new("semantic");
        let mut cfg = Config::default();
        cfg.click.interval_ms = 5; // parses, but fails validation
        fs::create_dir_all(t.file().parent().unwrap()).unwrap();
        fs::write(t.file(), toml::to_string(&cfg).unwrap()).unwrap();
        let l = load(&t.file());
        assert_eq!(l.config, Config::default());
        let w = l.warning.expect("warning");
        assert!(w.contains("interval of 5 ms"), "{w}");
        assert!(t.file().with_file_name("config.toml.bak").exists());
    }

    #[test]
    fn newer_schema_versions_are_not_silently_accepted() {
        let t = TempDir::new("version");
        let cfg = Config {
            version: 99,
            ..Config::default()
        };
        fs::create_dir_all(t.file().parent().unwrap()).unwrap();
        fs::write(t.file(), toml::to_string(&cfg).unwrap()).unwrap();
        let w = load(&t.file()).warning.expect("warning");
        assert!(w.contains("unsupported configuration version 99"), "{w}");
    }

    // ---- migration from Autoclicker 1.0.0

    fn legacy_file(t: &TempDir) -> PathBuf {
        t.0.join("Autoclicker").join("config.toml")
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn legacy_settings() -> String {
        let mut cfg = Config::default();
        cfg.click.interval_ms = 250;
        cfg.click.target = Some(Point::new(640, 360));
        cfg.window.always_on_top = true;
        // Hand-written touches (comment, ordering) must survive the copy.
        format!("# my settings\n{}", toml::to_string(&cfg).unwrap())
    }

    #[test]
    fn old_settings_alone_are_copied_unchanged() {
        let t = TempDir::new("migrate-old-only");
        let old = legacy_file(&t);
        let text = legacy_settings();
        write(&old, &text);
        assert_eq!(migrate_legacy(&t.file(), &old), Migration::Copied);
        assert_eq!(
            fs::read_to_string(t.file()).unwrap(),
            text,
            "byte-identical copy"
        );
        assert_eq!(
            fs::read_to_string(&old).unwrap(),
            text,
            "old file unchanged"
        );
        let loaded = load(&t.file());
        assert!(loaded.warning.is_none());
        assert_eq!(loaded.config.click.interval_ms, 250);
        assert_eq!(loaded.config.click.target, Some(Point::new(640, 360)));
        // Only once: a second start finds the Cadence file.
        assert_eq!(migrate_legacy(&t.file(), &old), Migration::NotNeeded);
    }

    #[test]
    fn existing_cadence_settings_are_never_replaced() {
        let t = TempDir::new("migrate-both");
        let old = legacy_file(&t);
        write(&old, &legacy_settings());
        write(&t.file(), "version = 1\n");
        assert_eq!(migrate_legacy(&t.file(), &old), Migration::NotNeeded);
        assert_eq!(fs::read_to_string(t.file()).unwrap(), "version = 1\n");
        assert_eq!(fs::read_to_string(&old).unwrap(), legacy_settings());
        // Even a broken Cadence file is left for `load` to handle.
        write(&t.file(), "not = = toml");
        assert_eq!(migrate_legacy(&t.file(), &old), Migration::NotNeeded);
        assert_eq!(fs::read_to_string(t.file()).unwrap(), "not = = toml");
    }

    #[test]
    fn no_settings_at_all_creates_nothing() {
        let t = TempDir::new("migrate-neither");
        assert_eq!(
            migrate_legacy(&t.file(), &legacy_file(&t)),
            Migration::NotNeeded
        );
        assert!(!t.file().exists());
        assert!(!t.file().parent().unwrap().exists());
    }

    #[test]
    fn unreadable_old_settings_are_skipped() {
        let t = TempDir::new("migrate-unreadable");
        let old = legacy_file(&t);
        fs::create_dir_all(&old).unwrap(); // a directory cannot be read as a file
        let Migration::Skipped(reason) = migrate_legacy(&t.file(), &old) else {
            panic!("expected Skipped");
        };
        assert!(reason.contains("could not be read"), "{reason}");
        assert!(!t.file().exists());
        assert!(old.is_dir(), "old path untouched");
    }

    #[test]
    fn malformed_or_invalid_old_settings_are_skipped_and_left_alone() {
        let t = TempDir::new("migrate-malformed");
        let old = legacy_file(&t);
        let mut invalid = Config::default();
        invalid.click.interval_ms = 5; // parses, but fails validation
        for text in [
            "this is = = not toml".to_string(),
            toml::to_string(&invalid).unwrap(),
        ] {
            write(&old, &text);
            let Migration::Skipped(reason) = migrate_legacy(&t.file(), &old) else {
                panic!("expected Skipped for {text:?}");
            };
            assert!(reason.contains("not valid"), "{reason}");
            assert!(!t.file().exists(), "nothing copied");
            assert_eq!(
                fs::read_to_string(&old).unwrap(),
                text,
                "old file unchanged"
            );
        }
        // Not UTF-8 either.
        fs::write(&old, [0xff, 0xfe, 0x00]).unwrap();
        assert!(matches!(
            migrate_legacy(&t.file(), &old),
            Migration::Skipped(_)
        ));
        assert!(!t.file().exists());
    }

    #[test]
    fn old_settings_with_pre_1_0_hotkeys_are_copied_as_they_are() {
        // Autoclicker before 1.0 wrote separate start/stop hotkeys; Cadence
        // loads them like Autoclicker 1.0 did (they are removed once, with
        // a notice, by the application).
        let t = TempDir::new("migrate-obsolete");
        let old = legacy_file(&t);
        let text = toml::to_string(&Config::default())
            .unwrap()
            .replace("[hotkeys]\n", "[hotkeys]\nstart = \"F9\"\nstop = \"F10\"\n");
        assert!(text.contains("start = \"F9\""));
        write(&old, &text);
        assert_eq!(migrate_legacy(&t.file(), &old), Migration::Copied);
        assert_eq!(fs::read_to_string(t.file()).unwrap(), text);
        let loaded = load(&t.file());
        assert!(loaded.warning.is_none());
        assert!(loaded.config.hotkeys.had_obsolete_bindings());
    }
}
