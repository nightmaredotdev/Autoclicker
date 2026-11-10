//! A small rotating file logger for the `log` facade.
//!
//! * Location: `%LOCALAPPDATA%\Autoclicker\logs\autoclicker.log`
//!   (`AUTOCLICKER_LOG_DIR` overrides the directory, e.g. for tests).
//! * Size: when the file would exceed [`MAX_BYTES`] it is renamed to
//!   `autoclicker.1.log` (replacing the previous one) and a new file is
//!   started, so at most about twice that is kept. An oversized file left by a
//!   previous run is rotated at startup.
//! * Failures never affect the application: if the directory or file cannot
//!   be created or written, logging is silently disabled.
//! * Noise: this application's records at Info and above; other crates
//!   (Slint, winit, ...) only at Warn and above. Individual clicks are never
//!   logged.

#![forbid(unsafe_code)]

use log::{Level, LevelFilter, Log, Metadata, Record};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

pub const LOG_DIR_ENV: &str = "AUTOCLICKER_LOG_DIR";
pub const MAX_BYTES: u64 = 1024 * 1024;
pub const FILE_NAME: &str = "autoclicker.log";
pub const ROTATED_NAME: &str = "autoclicker.1.log";

/// The log directory, or `None` if no per-user data directory exists.
pub fn default_dir() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(LOG_DIR_ENV) {
        return Some(PathBuf::from(p));
    }
    directories::BaseDirs::new().map(|d| d.data_local_dir().join("Autoclicker").join("logs"))
}

/// Replaces the user's profile directory with `%USERPROFILE%` so logs do not
/// contain the account name.
pub fn display_path(path: &Path) -> String {
    if let Some(home) = directories::BaseDirs::new().map(|d| d.home_dir().to_path_buf())
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return format!("%USERPROFILE%\\{}", rest.display());
    }
    path.display().to_string()
}

struct State {
    file: Option<File>,
    written: u64,
}

pub struct FileLogger {
    dir: PathBuf,
    max_bytes: u64,
    state: Mutex<State>,
}

impl FileLogger {
    /// Opens (creating if needed) the log in `dir`. Never fails; check
    /// [`FileLogger::is_active`].
    pub fn open(dir: &Path, max_bytes: u64) -> Self {
        let logger = Self {
            dir: dir.to_path_buf(),
            max_bytes,
            state: Mutex::new(State {
                file: None,
                written: 0,
            }),
        };
        if fs::create_dir_all(dir).is_ok() {
            let size = fs::metadata(logger.path()).map_or(0, |m| m.len());
            let mut state = logger.lock();
            if size >= max_bytes {
                logger.rotate(&mut state);
            } else {
                state.file = logger.open_file();
                state.written = size;
            }
        }
        logger
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether records are currently being written.
    pub fn is_active(&self) -> bool {
        self.lock().file.is_some()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn open_file(&self) -> Option<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path())
            .ok()
    }

    fn rotate(&self, state: &mut State) {
        state.file = None; // close before renaming
        let rotated = self.dir.join(ROTATED_NAME);
        let _ = fs::remove_file(&rotated);
        let _ = fs::rename(self.path(), &rotated);
        state.file = self.open_file();
        state.written = 0;
    }

    fn write_line(&self, line: &str) {
        let mut state = self.lock();
        if state.file.is_none() {
            return;
        }
        let len = line.len() as u64;
        if state.written > 0 && state.written + len > self.max_bytes {
            self.rotate(&mut state);
        }
        let ok = state
            .file
            .as_mut()
            .is_some_and(|f| f.write_all(line.as_bytes()).is_ok());
        if ok {
            state.written += len;
        } else {
            state.file = None; // stop trying after a write error
        }
    }
}

fn wanted(metadata: &Metadata<'_>) -> bool {
    let ours = metadata.target().starts_with("autoclicker");
    metadata.level() <= if ours { Level::Info } else { Level::Warn }
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        wanted(metadata)
    }

    fn log(&self, record: &Record<'_>) {
        if !wanted(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} [{}] {}\r\n",
            utc_timestamp(SystemTime::now()),
            record.level(),
            record.target(),
            record.args()
        );
        self.write_line(&line);
    }

    fn flush(&self) {
        if let Some(f) = self.lock().file.as_mut() {
            let _ = f.flush();
        }
    }
}

/// Installs the global logger. Returns it (for showing its location) if a
/// directory was available; logging stays disabled otherwise. Safe to call
/// once per process.
pub fn init(dir: Option<&Path>) -> Option<&'static FileLogger> {
    let logger: &'static FileLogger = Box::leak(Box::new(FileLogger::open(dir?, MAX_BYTES)));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(LevelFilter::Info);
    }
    Some(logger)
}

/// `YYYY-MM-DD hh:mm:ss.mmmZ` (UTC).
pub fn utc_timestamp(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "autoclicker-log-test-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&dir);
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn record(logger: &FileLogger, level: Level, target: &str, msg: &str) {
        logger.log(
            &Record::builder()
                .level(level)
                .target(target)
                .args(format_args!("{msg}"))
                .build(),
        );
    }

    #[test]
    fn timestamps_are_utc_civil_dates() {
        assert_eq!(utc_timestamp(UNIX_EPOCH), "1970-01-01 00:00:00.000Z");
        let t = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        assert_eq!(utc_timestamp(t), "2023-11-14 22:13:20.123Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(utc_timestamp(leap), "2000-02-29 00:00:00.000Z");
    }

    #[test]
    fn writes_formatted_lines_and_filters_noise() {
        let t = TempDir::new("format");
        let logger = FileLogger::open(&t.0, MAX_BYTES);
        assert!(logger.is_active());
        record(&logger, Level::Info, "autoclicker::gui", "started");
        record(&logger, Level::Debug, "autoclicker::gui", "too detailed");
        record(
            &logger,
            Level::Info,
            "winit::platform",
            "dependency chatter",
        );
        record(&logger, Level::Warn, "slint", "dependency warning");
        logger.flush();
        let text = fs::read_to_string(logger.path()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(
            lines[0].ends_with("INFO  [autoclicker::gui] started"),
            "{}",
            lines[0]
        );
        assert!(lines[1].contains("WARN  [slint] dependency warning"));
        assert!(lines[0].as_bytes()[4] == b'-' && lines[0].contains('Z'));
    }

    #[test]
    fn rotates_when_the_size_limit_is_reached() {
        let t = TempDir::new("rotate");
        let max = 400;
        let logger = FileLogger::open(&t.0, max);
        for i in 0..40 {
            record(
                &logger,
                Level::Info,
                "autoclicker",
                &format!("line {i:03} with some padding"),
            );
        }
        logger.flush();
        let current = fs::metadata(logger.path()).unwrap().len();
        let rotated_path = t.0.join(ROTATED_NAME);
        let rotated = fs::metadata(&rotated_path).unwrap().len();
        assert!(current <= max, "current {current}");
        assert!(rotated <= max, "rotated {rotated}");
        // Only the two files exist, and the newest line is in the current file.
        assert_eq!(fs::read_dir(&t.0).unwrap().count(), 2);
        assert!(
            fs::read_to_string(logger.path())
                .unwrap()
                .contains("line 039")
        );
        assert!(
            !fs::read_to_string(&rotated_path)
                .unwrap()
                .contains("line 039")
        );
    }

    #[test]
    fn oversized_log_from_a_previous_run_is_rotated_at_startup() {
        let t = TempDir::new("startup");
        fs::create_dir_all(&t.0).unwrap();
        fs::write(t.0.join(FILE_NAME), vec![b'x'; 500]).unwrap();
        let logger = FileLogger::open(&t.0, 400);
        record(&logger, Level::Info, "autoclicker", "fresh");
        logger.flush();
        assert_eq!(fs::metadata(t.0.join(ROTATED_NAME)).unwrap().len(), 500);
        assert!(fs::read_to_string(logger.path()).unwrap().contains("fresh"));
    }

    #[test]
    fn an_unusable_directory_disables_logging_without_failing() {
        let t = TempDir::new("unusable");
        fs::create_dir_all(&t.0).unwrap();
        let file_in_the_way = t.0.join("not-a-dir");
        fs::write(&file_in_the_way, b"x").unwrap();
        let logger = FileLogger::open(&file_in_the_way.join("logs"), MAX_BYTES);
        assert!(!logger.is_active());
        record(&logger, Level::Error, "autoclicker", "goes nowhere");
        logger.flush(); // no panic, no file
    }

    #[test]
    fn display_paths_hide_the_profile_directory() {
        let home = directories::BaseDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();
        let p = home.join("AppData").join("Local").join("Autoclicker");
        let shown = display_path(&p);
        assert!(shown.starts_with("%USERPROFILE%"), "{shown}");
        assert!(!shown.contains(&*home.to_string_lossy()), "{shown}");
        assert_eq!(display_path(Path::new("C:\\Tools\\x")), "C:\\Tools\\x");
    }
}
