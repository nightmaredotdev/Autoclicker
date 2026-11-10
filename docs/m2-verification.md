# M2 verification record

Verification of the M2 Slint GUI, recorded on 2026-11-10 on the same
machine as [M1](m1-verification.md): Windows 10 22H2 (build 19045), one
1920×1080 monitor at 100 % scaling, dark system theme, Rust 1.97.1 (MSVC).

## Automated tests

| Suite | Command | Result |
|---|---|---|
| Core | `cargo test -p autoclicker-core` | 57 passed (Windows and Linux) |
| Platform unit tests | `cargo test -p autoclicker-platform` | 16 passed |
| App unit tests (form logic, settings store) | `cargo test -p autoclicker --lib` | 15 passed |
| Runtime integration (real desktop) | `cargo test -p autoclicker --lib -- --ignored --test-threads=1` | 5 passed |
| GUI integration, in process (real window) | `cargo test -p autoclicker --test gui -- --ignored` | 21 checks passed |
| GUI end to end, real executable | `cargo test -p autoclicker --test gui_process -- --ignored --test-threads=1` | 3 passed (2 consecutive runs) |
| Platform integration (real desktop) | `cargo test -p autoclicker-platform -- --ignored --test-threads=1` | 10 passed |

The interactive suites need an unlocked desktop and are not run in CI. They
never click outside test-owned windows. Injected clicks are swallowed by a
low-level mouse hook, and keys are injected only while a test-owned window
has the foreground. They use temporary settings files (`AUTOCLICKER_CONFIG`),
never the user's own.

### What the GUI tests cover

In process (`tests/gui.rs`): the real Slint window runs on the main thread.
Actions go through the same callbacks the controls invoke, and hotkeys are
recorded from real key presses injected into the focused window.

| Acceptance criterion | Check |
|---|---|
| GUI launches | Window visible, titled "Autoclicker", status Stopped |
| Both click modes | Current-cursor mode clicks without moving the cursor. Fixed mode clicks within the 3 px radius of the target, even with the cursor elsewhere. |
| Start/stop via the GUI | Start/Stop button: the status changes to Running, the title to "Autoclicker - Running", and back again |
| Hotkeys while unfocused | With another window in the foreground, the toggle hotkey starts and stops clicking and the GUI follows (also run against the real exe) |
| Target capture | "Pick target" countdown (3 s, real timer) and direct capture: the displayed coordinates match the cursor position, and the target is saved |
| Persistence across restarts | Interval, position jitter radius, mode, target, always-on-top and hotkeys survive a restart (in process and real exe) |
| Position jitter radius | Editing the field: 51 px is rejected by the core rule (maximum 50) and not applied. 0 px makes every click land exactly on the target. 8 px keeps clicks within 8 px and some land beyond 3 px, which proves the edited value reached the engine. The value is saved and restored after a restart |
| Invalid settings | Out-of-range interval, a hold that does not fit, and non-numeric input each give a specific message. Nothing is applied or saved; the previous settings stay active |
| Hotkey problems | A typing key without Ctrl/Alt is refused and recording continues. Esc cancels. A saved binding that another program holds is shown as "Unavailable" at startup |
| Always on top | Toggling sets and clears `WS_EX_TOPMOST` on the real window and is saved. The real exe starts topmost when the setting is on |
| Closing stops everything | Closing while clicking: the shutdown report is clean, no clicks happen afterwards, every press has a release, and the hotkeys can be registered by others again. The real exe exits with code 0 after `WM_CLOSE` and after the exit hotkey |
| No held button | Every scenario checks that each press is followed by a release |
| Corrupt settings | Moved to `*.bak`, defaults loaded, warning shown in the window |
| About | The About window opens with the `AboutSlint` attribution |

## Verified behaviour worth knowing

* **A combination that another program has registered cannot be recorded.**
  Windows delivers it only to the program that registered it, so the key
  press never reaches our window. Recording simply continues. The recording
  hint says so.
* **The rollback of a conflicting rebind** (restoring the previous bindings
  when a new one cannot be registered) is verified at the runtime level
  (`conflicting_rebind_restores_the_previous_bindings`). Through the GUI this
  path is reachable only if another program registers the combination after it
  was pressed; it was not exercised through the GUI.
* Closing the About window only hides it. Closing the main window (or the
  exit hotkey) shuts the application down, also while About is open.

## Visual verification

Screenshots of the running debug and release builds were inspected
(dark theme, 100 % scaling): the main window (Clicking and Hotkeys tabs),
invalid-input feedback, hotkey recording, the pick countdown, the Running
state and the About dialog. Problems found and fixed during the inspection:

* Hidden message boxes still took up layout space, leaving stray gaps.
* Hotkey rows stretched to fill the tab height.
* The About dialog spread its content over the whole window.
* The rate hint was shown for rejected interval values.
* The core error used Rust range syntax (`10..=3600000`). It now reads
  "10 to 3600000 ms".
* Transient notices looked like input fields and resized the window. They are
  now a fixed-height muted line.

The release build uses the Windows GUI subsystem (no console window), and it
launches and closes cleanly.

## Not verified

* Light theme (the system was in dark mode), display scaling other than
  100 %, multiple monitors and mixed DPI.
* Windows 11, Windows Terminal, and screen readers. Accessibility
  (AccessKit) is compiled in but was not tested with assistive technology.
* Physical mouse clicks on every control. The automated tests invoke the
  controls' callbacks. The visual check clicked the tabs, the "Not set"
  hotkey button, the Interval field, "Pick target", Start and About by mouse.
* Software-renderer text quality was acceptable in the screenshots, so
  femtovg was not needed.
* Linux: nothing GUI-related was built or tested.
