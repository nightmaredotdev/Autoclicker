//! Loading and saving the versioned TOML configuration.
//!
//! * Location: `%APPDATA%\Autoclicker\config.toml` (via the platform's
//!   per-user configuration directory). The `AUTOCLICKER_CONFIG` environment
//!   variable overrides the full path (used by tests).
//! * Saving writes a temporary file next to the target and renames it over
//!   the old one, so a crash never leaves a half-written file.
//! * Only validated configurations are saved.
//! * A file that cannot be parsed or fails validation is moved aside to
//!   `config.toml.bak` and defaults are used; the caller shows the warning.

#![forbid(unsafe_code)]

use autoclicker_core::config::Config;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const PATH_OVERRIDE_ENV: &str = "AUTOCLICKER_CONFIG";
const APP_DIR: &str = "Autoclicker";
const FILE_NAME: &str = "config.toml";

/// The configuration file path, or `None` if no per-user directory exists.
pub fn default_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(PATH_OVERRIDE_ENV) {
        return Some(PathBuf::from(p));
    }
    directories::BaseDirs::new().map(|d| d.config_dir().join(APP_DIR).join(FILE_NAME))
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
    use autoclicker_core::config::TargetMode;
    use autoclicker_core::geometry::Point;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "autoclicker-test-{name}-{}-{:?}",
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
}
