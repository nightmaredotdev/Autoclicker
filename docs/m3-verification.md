# M3 verification record

Verification of M3 (Windows polish and release readiness), recorded on
2026-11-10 on the same machine as [M1](m1-verification.md) and
[M2](m2-verification.md): Windows 10 22H2 (build 19045), one 1920×1080
monitor at 100 % scaling, dark theme, Rust 1.97.1 (MSVC).

## Automated tests

| Suite | Command | Result |
|---|---|---|
| Core | `cargo test -p autoclicker-core` | 57 passed (Windows and Linux) |
| Platform unit tests, incl. single instance | `cargo test -p autoclicker-platform` | 19 passed |
| App unit tests, incl. logging | `cargo test -p autoclicker --lib` | 21 passed |
| Platform integration (desktop), incl. session end | `cargo test -p autoclicker-platform -- --ignored --test-threads=1` | 11 passed |
| Runtime integration (desktop) | `cargo test -p autoclicker --lib -- --ignored --test-threads=1` | 5 passed |
| GUI integration, in process | `cargo test -p autoclicker --test gui -- --ignored` | 25 steps passed |
| Real executable, end to end | `cargo test -p autoclicker --test gui_process -- --ignored --test-threads=1` | 7 passed |
| Package | `scripts\package.ps1`, then `scripts\verify-package.ps1 -Reproducible -Launch` | all checks passed |

The interactive suites never click outside test-owned windows, and they use
their own settings files, log directories and single-instance names.

## Acceptance criteria

### Single instance

| Criterion | Evidence |
|---|---|
| First instance starts normally | All end-to-end tests |
| Second instance creates no engine and exits cleanly | `a_second_instance_shows_the_first_and_exits`: exit code 0, no window of its own, logged as such. The lock check happens before any engine or hotkey is created (`gui::run`) |
| The existing window is shown | Same test: the first instance's hidden window becomes visible again |
| Startup race | `activation_sent_before_listening_is_latched`: a signal sent before the first instance listens is not lost (the named event is created before the mutex check by both sides) |
| Lock released after shutdown | `second_acquire_reports_already_running_until_released`, plus the end-to-end test (a third instance starts after the first exits). On termination, Windows closes the handles, which releases the lock |

### Tray

| Criterion | Evidence |
|---|---|
| Closing the window hides it | GUI test (`WM_CLOSE` to the real window) and end-to-end test: the window is hidden and the process keeps running |
| Clicking continues while hidden | Both tests count new clicks after hiding |
| Hotkeys work while hidden | GUI test and end-to-end test: the toggle hotkey stops clicking while the window is hidden |
| Tray Start/Stop | GUI test: starts and stops while hidden; every press has a release |
| Show restores a usable window | GUI test: tray toggle and icon click show the window with correct title and status |
| Tray Exit is a complete shutdown | GUI test: tray Exit while clicking gives a clean report, no further clicks, released button, freed hotkeys, hidden windows. Also clicked in the real tray menu by mouse: the log shows "shutting down", "engine and hotkey threads stopped cleanly", "exited" |
| Icon reflects the state | `AppTray.running` is checked in the GUI test. Screenshots show the green badge while clicking |

### Logging

| Criterion | Evidence |
|---|---|
| Startup and shutdown logged | `the_log_records_startup_problems_and_shutdown` |
| Errors and conflicts logged | Same test: a hotkey held by another program is logged at WARN. Corrupt settings, save errors and engine faults go through the same logger |
| Rotation | `rotates_when_the_size_limit_is_reached`, `oversized_log_from_a_previous_run_is_rotated_at_startup` |
| Logging failure does not prevent startup or shutdown | `an_unusable_log_directory_disables_logging_without_failing` and the end-to-end test with an unusable log directory (clean exit, no held button) |
| No noise or sensitive data | Fewer than 30 lines for a full session. No per-click lines. The profile path never appears (`%USERPROFILE%` is substituted) |

### Packaging

| Criterion | Evidence |
|---|---|
| Reproducible ZIP | Packaging twice gives the same SHA-256. Rebuilding the app crate gives a byte-identical exe (`/Brepro`) |
| Works from a different directory | Extracted into a new temp folder whose path contains spaces; it starts and exits cleanly |
| Settings stay in `%APPDATA%` | The log of the extracted run shows `%USERPROFILE%\AppData\Roaming\Autoclicker\config.toml`; nothing is written next to the executable |
| Metadata and icon | Version information (product name, description, versions, copyright, original file name), embedded icon (pixel check), manifest (`asInvoker`, per-monitor-v2, version) |

### Session end

A simulated `WM_QUERYENDSESSION`/`WM_ENDSESSION` sent to the hidden
system-events window:
* is not blocked (reply 1);
* runs the full shutdown before returning (platform test);
* in the real executable, stops clicking with no held button and exits
  with code 0 (end-to-end test, and the package `-Launch` check).

## Visual verification

Screenshots of the running application:

* The main window shows the new title-bar icon. The footer now has Logs,
  About and Exit.
* The tray icon appears in the notification area. While clicking it shows a
  green badge.
* The tray menu, opened by right-clicking the real icon, shows Hide window,
  Start clicking, Open log folder and Exit. Clicking Exit shut the
  application down cleanly.
* Icons at 16, 32 and 256 px were inspected on light and dark backgrounds.

Observed: on Windows 10 the icon later moved into the overflow area (the ^
next to the clock). That is the operating system's per-application setting.
The README explains how to keep it on the taskbar.

## Not verified

* Real logoff, restart or shutdown (only the simulated messages above).
* Windows 11, Windows Terminal, light theme, scaling other than 100 %,
  multiple monitors.
* Explorer restarts. Slint re-adds the icon on `TaskbarCreated` according to
  its source, but this was not tested.
* Byte-identical executables across different machines or toolchain
  versions.
* The single-instance lock is per logon session (`Local\`). Two different
  users on the same machine can each run one instance, by design.
* 15 crates ship no license file in their package. They are listed with
  their declared license and source repository.
* Linux: nothing in M3 is built or tested on Linux beyond the existing core
  and platform checks.
