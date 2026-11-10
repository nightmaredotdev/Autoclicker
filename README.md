# Autoclicker
A customizable desktop autoclicker with configurable click timing, human-like input variation, target positioning, and hotkeys. Built with cross-platform support in mind.

> **Status:** stable (1.0). A portable Windows desktop utility: no installer, no administrator rights.

## Platform support

| Platform | Status |
|---|---|
| Windows 10/11 (x86_64) | Initial target. Verified on Windows 10 22H2 only; see the [M1](docs/m1-verification.md), [M2](docs/m2-verification.md) and [M3](docs/m3-verification.md) verification records. |
| Linux | Not supported yet. The architecture keeps OS-specific code isolated so a backend can be added later. |

## Download and install

1. Download `Autoclicker-<version>-windows-x64.zip` from the [Releases](https://github.com/nightmaredotdev/Autoclicker/releases) page.
2. Extract the ZIP to any folder you like (for example your Desktop or a USB drive). No installation or administrator rights are needed.
3. Run `autoclicker.exe` from the extracted folder.

**The executable is not code-signed.** The first time you run it, Windows SmartScreen may show "Windows protected your PC". Choose **More info**, then **Run anyway**, if you trust the download. Each release lists the ZIP's SHA-256 checksum so you can verify the file (`Get-FileHash Autoclicker-<version>-windows-x64.zip` in PowerShell).

To update, close Autoclicker (tray icon > Exit) and replace the folder with the new version. Your settings are kept. To uninstall, delete the folder, and optionally `%APPDATA%\Autoclicker` (settings) and `%LOCALAPPDATA%\Autoclicker` (logs).

## Usage

Start `autoclicker.exe`. The window shows the current state (Stopped / Running / Error) with a Start/Stop button.

- **Where to click:** *Fixed target* moves to a saved point before each click. Use **Pick target** (a 3-second countdown to move the cursor) or the set-target hotkey. *Current cursor* clicks wherever the cursor is, without moving it.
- **Timing:** interval (start to start), interval jitter (Gaussian σ, truncated at ±3σ), hold duration, and position-jitter radius. Changes apply when you press Enter or leave the field. Invalid values are explained and never applied.
- **Hotkeys:** click a binding and press the new combination (Esc cancels). Function keys may be used alone; letters, digits and navigation keys need Ctrl or Alt. F12 is reserved by Windows.
- **Always on top**, **Logs** (opens the log folder), **About** and **Exit** are at the bottom of the window.
- **Closing the window hides it** to the notification area; clicking and hotkeys keep working. The tray icon (green dot while clicking) can show/hide the window, start/stop clicking, open the logs and exit. On Windows 10 the icon may start in the overflow area (^ next to the clock); drag it to the taskbar to keep it visible.
- **One instance:** starting Autoclicker again brings the running window to the front.

Default settings:

| Setting | Default |
|---|---|
| Where to click | Fixed target (set it with **Pick target** or F7 before starting) |
| Interval (start to start) | 100 ms (about 10 clicks per second); allowed 10 ms to 1 hour |
| Interval jitter | Gaussian, standard deviation 10 ms, truncated at ±3σ (so 70-130 ms) |
| Hold duration | 10 ms (0 is allowed) |
| Position jitter | 3 px radius around the fixed target (0-50 px) |
| Always on top | Off |

| Default hotkey | Action |
|---|---|
| F6 | Start / stop |
| F7 | Set the target to the current cursor position |
| Ctrl+Shift+F8 | Exit |
| (unbound) | Start, Stop |

**Hotkeys are global.** While the program runs, Windows does not deliver the registered key combinations to the focused application (verified on Windows 10). A combination that another program already uses is shown as unavailable, and it cannot be recorded, because Windows does not pass that key press to this program.

Settings are saved to `%APPDATA%\Autoclicker\config.toml`. A file that cannot be read as valid settings is moved to `config.toml.bak`, and the window shows a warning. A small log (startup, shutdown, errors and state changes, never individual clicks; at most about 2 MB) is kept in `%LOCALAPPDATA%\Autoclicker\logs`.

### Limitations

- The executable is not code-signed, so Windows SmartScreen may warn the first time it runs (see above).
- Verified on Windows 10 22H2 with a single monitor at 100 % scaling. Windows 11, other scaling factors and multi-monitor setups are expected to work but have not been tested.
- Windows' privilege isolation blocks clicks into applications running as administrator unless the autoclicker is elevated too, and Windows does not report the failure.
- If the process is killed (for example from Task Manager) while a button is held down, nothing can release it. Click that mouse button once to clear it.
- Some applications deliberately ignore injected input.

## Building

### Requirements (Windows)

- [Rust](https://rustup.rs/) stable with the `x86_64-pc-windows-msvc` toolchain (developed with Rust 1.97 or newer)
- Microsoft C++ Build Tools (the "Desktop development with C++" workload, which provides the MSVC linker and Windows SDK)

No other native toolchain is required: the UI uses Slint's software renderer, so no OpenGL, Skia or Qt dependencies are built.

### Commands

```sh
cargo build --release          # binary: target/release/autoclicker.exe
cargo test --workspace         # automated tests (no desktop interaction)
cargo fmt --all --check        # formatting
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Integration tests that use the real desktop (including the GUI tests) are ignored by default. Run them from an interactive, unlocked Windows session. They never click outside their own test windows or touch your settings, but they briefly move the cursor and open and focus test windows.

```sh
cargo test --workspace -- --ignored --test-threads=1
```

Timing measurement (plain vs high-resolution waiter, foreground vs minimized behind a fullscreen window):

```sh
cargo run -p autoclicker-platform --features diagnostics --release --example measure -- --interval-ms 100 --hold-ms 10 --clicks 200
```

## Release package

`scripts\package.ps1` builds the portable `Autoclicker-<version>-windows-x64.zip` (no installer, no admin rights), and `scripts\verify-package.ps1` checks it. See [docs/release.md](docs/release.md).

## Project layout

```
crates/
  core/      OS-independent logic: configuration, scheduling, click engine (no unsafe code)
  platform/  OS-specific input, hotkeys, timing and screen queries (the only crate with handwritten unsafe code)
  app/       Desktop application: Slint UI (ui/app.slint), settings file, wiring
assets/icon/ Icon source (make_icon.py) and generated icons
packaging/   Files shipped in the release ZIP
scripts/     Packaging and package verification
docs/        Verification records, release process
```

## Third-party licences

The user interface is built with [Slint](https://slint.dev), used under the Slint Royalty-free Desktop, Mobile, and Web Applications License v2.0. The required attribution (`AboutSlint`) is shown in the application's About window.

## License

[MIT](LICENSE)
