# Autoclicker
A customizable desktop autoclicker with configurable click timing, human-like input variation, target positioning, and hotkeys. Built with cross-platform support in mind.

> **Status:** early development. The click engine and Windows backend work as a headless console program. The graphical interface is not implemented yet.

## Platform support

| Platform | Status |
|---|---|
| Windows 10/11 (x86_64) | Initial target. Verified on Windows 10 22H2 only; see [docs/m1-verification.md](docs/m1-verification.md). |
| Linux | Not supported yet. The architecture keeps OS-specific code isolated so a backend can be added later. |

## Usage (headless)

```sh
autoclicker.exe [--mode fixed|cursor] [--waiter high-resolution|plain]
```

| Default hotkey | Action |
|---|---|
| F6 | Toggle clicking |
| F7 | Set the target to the current cursor position (fixed mode) |
| Ctrl+Shift+F8 | Exit |
| (unbound) | Start, Stop |

Defaults: a 100 ms start-to-start interval with Gaussian jitter (σ = 10 ms, truncated at ±3σ), a 10 ms hold, and a 3 px position-jitter radius in fixed-target mode. Ctrl+C also exits cleanly.

**Hotkeys are global.** While the program runs, Windows does not deliver the registered key combinations to the focused application; this was verified on Windows 10. If another program already uses a combination, it is reported at startup and that hotkey stays unavailable.

### Limitations

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

Integration tests that use the real desktop are ignored by default. Run them from an interactive, unlocked Windows session. They never click outside their own test windows, but they briefly move the cursor and focus a test window.

```sh
cargo test --workspace -- --ignored --test-threads=1
```

Timing measurement (plain vs high-resolution waiter, foreground vs minimized behind a fullscreen window):

```sh
cargo run -p autoclicker-platform --features diagnostics --release --example measure -- --interval-ms 100 --hold-ms 10 --clicks 200
```

## Project layout

```
crates/
  core/      OS-independent logic: configuration, scheduling, click engine (no unsafe code)
  platform/  OS-specific input, hotkeys, timing and screen queries (the only crate with unsafe code)
  app/       Desktop application (currently a headless console front end)
docs/        Verification records
```

## Third-party licences

The user interface is built with [Slint](https://slint.dev), used under the Slint Royalty-free Desktop, Mobile, and Web Applications License v2.0. The application will include the required attribution in its About screen.

## License

[MIT](LICENSE)
