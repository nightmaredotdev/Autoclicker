# Autoclicker
A customizable desktop autoclicker with configurable click timing, human-like input variation, target positioning, and hotkeys. Built with cross-platform support in mind.

> **Status:** early development. The workspace scaffolding is in place; the click engine and user interface are not implemented yet.

## Platform support

| Platform | Status |
|---|---|
| Windows 10/11 (x86_64) | Initial target, in development |
| Linux | Not supported yet. The architecture keeps OS-specific code isolated so a backend can be added later. |

## Building

### Requirements (Windows)

- [Rust](https://rustup.rs/) stable with the `x86_64-pc-windows-msvc` toolchain (developed with Rust 1.97 or newer)
- Microsoft C++ Build Tools (the "Desktop development with C++" workload, which provides the MSVC linker and Windows SDK)

No other native toolchain is required: the UI uses Slint's software renderer, so no OpenGL, Skia or Qt dependencies are built.

### Commands

```sh
cargo build --release          # binary: target/release/autoclicker.exe
cargo test --workspace         # run the test suite
cargo fmt --all --check        # formatting
cargo clippy --workspace --all-targets -- -D warnings
```

## Project layout

```
crates/
  core/      OS-independent logic: configuration, scheduling, click engine (no unsafe code)
  platform/  OS-specific input, hotkeys, timing and screen queries (the only crate with unsafe code)
  app/       Desktop application (Slint UI)
```

## Third-party licences

The user interface is built with [Slint](https://slint.dev), used under the Slint Royalty-free Desktop, Mobile, and Web Applications License v2.0. The application will include the required attribution in its About screen.

## License

[MIT](LICENSE)
