# M1 verification record

Results of the M1 (headless engine + Windows backend) verification, recorded
on 2026-11-10. Anything not listed under "Verified" is an assumption or
untested.

> **Note (M2):** the headless console front end was replaced by the Slint GUI
> in M2. Its console-control tests (`crates/app/tests/console_control.rs`)
> were removed with it. The platform's console handler and its unit tests
> remain. See [m2-verification.md](m2-verification.md).

## Test environment

| | |
|---|---|
| OS | Windows 10 22H2, build 10.0.19045.6466 |
| CPU | AMD Ryzen 7 3800X |
| Display | one monitor, 1920×1080, 96 DPI (100 % scaling), 144 Hz |
| Power | AC |
| Toolchain | Rust 1.97.1, `x86_64-pc-windows-msvc` |

Not available for testing: Windows 11, multiple monitors, display scaling
other than 100 %, mixed-DPI setups, battery power, exclusive-fullscreen
(DirectX) games.

## Automated tests

| Suite | Command | Result |
|---|---|---|
| Core (deterministic, virtual time) | `cargo test -p autoclicker-core` | 54 passed (Windows and Linux) |
| Platform unit tests (no desktop needed) | `cargo test -p autoclicker-platform` | 16 passed |
| App unit tests | `cargo test -p autoclicker` | 1 passed |
| Platform integration (real desktop) | `cargo test -p autoclicker-platform -- --ignored --test-threads=1` | 10 passed |
| App runtime integration (real desktop) | `cargo test -p autoclicker -- --ignored --test-threads=1` | 4 passed |
| Console control end to end (real exe, real desktop) | `cargo test -p autoclicker --test console_control -- --ignored --test-threads=1` | 2 passed (plus a helper entry), 3 consecutive runs |

The integration tests are `#[ignore]`d by default because they need an
interactive, unlocked desktop. They were run locally; CI runs only the
non-interactive suites. They never click anything outside test-owned windows:
injected clicks are swallowed by a low-level mouse hook, or delivered only
after checking that a test window is under the cursor; injected keys are sent
only while a test window has the foreground.

## Verified behaviour (on the environment above)

| Behaviour | Evidence |
|---|---|
| Per-monitor-v2 DPI awareness is set and active | `dpi_awareness_is_per_monitor_v2` |
| `SetCursorPos`/`GetCursorPos` round-trip exactly; monitor bounds match | `cursor_round_trips_on_every_monitor_and_bounds_match` (1 monitor only) |
| Injected press/release arrive in order, flagged as injected, at the cursor position | `injected_buttons_arrive_in_order_with_signature` |
| With swapped buttons, an uncompensated physical *left* press becomes a logical *right* click; the backend's compensated press still produces a primary (left) click | `swapped_buttons_still_produce_a_primary_click` |
| `MOD_NOREPEAT`: five auto-repeat key-downs fire the hotkey **once**; release + press fires again | `held_hotkey_fires_once_and_foreground_delivery_is_documented` |
| **Registered hotkeys are not delivered to the foreground window** (F13 and Ctrl+Shift+F8 key-downs: 0; an unregistered Ctrl key-down: delivered) | same test |
| A conflicting registration fails with `ERROR_HOTKEY_ALREADY_REGISTERED` (1409) and is reported | `conflicting_registration_is_reported_and_shutdown_unregisters` |
| Hotkey shutdown unregisters (the same hotkey can be registered again) | same test |
| Engine presses and releases are paired; final state released | `engine_clicks_are_paired_and_end_released` |
| Shutdown during a 1.5 s hold releases the button ~0.1 ms later; thread joined in < 1 ms | `shutdown_mid_hold_releases_promptly` |
| Fixed-target clicks stay within the 3 px jitter radius | `fixed_target_clicks_land_within_the_jitter_radius` |
| Runtime shutdown joins the engine and hotkey threads, frees all hotkeys, is idempotent; dropping the runtime does the same | `shutdown_joins_both_threads_and_frees_hotkeys`, `dropping_the_runtime_also_shuts_down` |
| Runtime shutdown while holding releases the button | `shutdown_while_holding_releases_the_button` |
| Console handler dispatch: Ctrl+C/Ctrl+Break request exit without blocking; close/logoff/shutdown request exit and wait until shutdown completes (bounded by the 4 s grace period); unknown events are passed on | `windows::console::tests` (5 unit tests, run in CI) |
| A real `CTRL_C_EVENT` (`GenerateConsoleCtrlEvent` on the app's own console) while clicking: engine stopped, no button left held, both threads joined cleanly, exit code 0 | `ctrl_c_while_clicking_shuts_down_cleanly` |
| A real `CTRL_CLOSE_EVENT`, produced by posting `WM_CLOSE` to the app's classic console-host window while clicking: same checks; Windows then ends the process with `STATUS_CONTROL_C_EXIT` (0xC000013A) once the handler returns | `closing_the_console_window_while_clicking_shuts_down_cleanly` |
| The exit hotkey leads to a clean shutdown | `exit_hotkey_leads_to_a_clean_shutdown`, plus a manual run of the release exe (Ctrl+Shift+F8 sent with `SendKeys`; process exited with code 0) |

Covered by deterministic core tests only (a real `SendInput` partial failure
cannot be provoked on demand): partial `SendInput` success, release retries
(immediate, +5 ms, +20 ms) and `ReleaseFailed`, refusal to restart while a
button may be held, panic-unwinding release, late-deadline skipping, the
modifier-release wait, status throttling.

## Timing measurements

Tool: `cargo run -p autoclicker-platform --features diagnostics --release --example measure`.
The real engine and `SendInput` backend run in current-cursor mode with
jitter disabled. Each injected event is timestamped (QueryPerformanceCounter)
in a low-level mouse hook, then swallowed. The hook adds the same small
delivery latency to both waiters.

* **foreground**: the measuring process owns a visible window.
* **background**: its window is minimized and a separate process shows a
  borderless, fullscreen, topmost window in the foreground. This is not an
  exclusive-mode DirectX game.

Errors are absolute deviations from the nominal start-to-start interval or
hold, in milliseconds.

### 100 ms interval, 10 ms hold (defaults without jitter), 200 intervals

| scenario | waiter | CPS (target 10) | interval error p50 / p99 / max | hold error p50 / p99 / max | skipped |
|---|---|---|---|---|---|
| foreground | plain | 10.016 | 6.822 / 13.990 / 24.508 | 5.669 / 6.444 / 9.815 | 0 |
| foreground | high-resolution | 10.000 | 0.295 / 0.978 / 3.953 | 0.237 / 1.011 / 3.610 | 0 |
| background | plain | 9.995 | 7.152 / 11.675 / 13.419 | 5.800 / 8.723 / 9.601 | 0 |
| background | high-resolution | 10.000 | 0.286 / 2.400 / 4.561 | 0.211 / 2.015 / 4.253 | 0 |

### 20 ms interval, 10 ms hold, 300 intervals

| scenario | waiter | CPS (target 50) | interval error p50 / p99 / max | hold error p50 / p99 / max | skipped |
|---|---|---|---|---|---|
| foreground | plain | 31.752 | 11.517 / 12.242 / 12.649 | 5.828 / 6.183 / 7.186 | 300 |
| foreground | high-resolution | 49.995 | 0.231 / 0.975 / 5.167 | 0.131 / 1.031 / 5.127 | 0 |
| background | plain | 31.878 | 11.493 / 14.543 / 15.227 | 5.780 / 8.378 / 8.851 | 298 |
| background | high-resolution | 50.000 | 0.201 / 1.146 / 7.341 | 0.252 / 1.178 / 6.698 | 0 |

### 10 ms interval, 5 ms hold (fastest allowed), 300 intervals

| scenario | waiter | CPS (target 100) | interval error p50 / p99 / max | hold error p50 / p99 / max | skipped |
|---|---|---|---|---|---|
| foreground | plain | 63.337 | 5.978 / 6.167 / 6.178 | 10.609 / 11.003 / 11.463 | 631 |
| foreground | high-resolution | 99.985 | 0.244 / 0.969 / 0.992 | 0.323 / 1.021 / 1.088 | 0 |
| background | plain | 63.821 | 5.631 / 8.232 / 9.380 | 10.554 / 13.408 / 14.237 | 477 |
| background | high-resolution | 99.987 | 0.177 / 2.150 / 5.254 | 0.511 / 2.970 / 4.697 | 0 |

### Conclusions

* The high-resolution waiter hits the target rate within 0.02 % in every
  configuration, with p99 start-to-start error ≤ 2.4 ms and maximum
  lateness ≈ 1 ms.
* The plain waiter's errors are consistent with the default ~15.6 ms timer
  granularity (an inference; the timer resolution itself was not measured).
  It cannot sustain 20 ms or 10 ms intervals. In those runs the late-deadline
  logic skipped slots instead of bursting, as designed.
* On Windows 10, running minimized behind a fullscreen window of another
  process did not materially degrade the high-resolution waiter: p99 went
  from 1.0 to 2.4 ms at 100 ms intervals.
* No `timeBeginPeriod` call is involved in either waiter.

**Decision:** the high-resolution waiter is the default. The portable plain
waiter is kept as the automatic fallback if the high-resolution timer cannot
be created (Windows 10 before 1803), and for comparison (`--waiter plain`).

**Not verified:** Windows 11 behaviour for minimized or occluded processes
(the documented resolution caveat applies there), behaviour under battery or
power-saving modes, and timing behind exclusive-fullscreen games.

## Console shutdown: what is and is not verified

Verified automatically (see above): the real executable, started in its own
console with clicking active, receives a real `CTRL_C_EVENT`, or a real
`CTRL_CLOSE_EVENT` produced by closing its console window with `WM_CLOSE`. In
both cases it stops the engine, leaves no button held, unregisters its
hotkeys and joins both threads ("Shutdown complete: engine thread Clean,
hotkey thread Clean"), and injects nothing after exiting.

Not verified:

* **Closing the window by hand** (the X button sends `WM_SYSCOMMAND`/`SC_CLOSE`,
  which the console host turns into the same close handling). The tests post
  `WM_CLOSE` directly.
* **Windows Terminal** (the default terminal on Windows 11): closing a tab or
  window there. The tests used the classic console host on Windows 10.
* **Console output while the console is being destroyed.** The tests pipe the
  application's output, so writes never touch the closing console. The
  application now shuts down *before* writing anything and no longer panics on
  write errors, but writes to a dying console were not observed.
* **Logoff and system shutdown events.** These go through the same code path
  as close (covered by the unit tests) but were not delivered for real.
* **A thread that hangs during a close event.** Each join waits up to 2 s
  (4 s in total), and the close handler waits up to 4 s before Windows
  terminates the process. In that case the release may not complete.
* **Clicking a real application** with the default settings (the automated
  tests deliberately never do).
