# Application icon

`cadence.svg` is the canonical Cadence logo. It is never edited by the
build: `make_icon.py` nests it unchanged, drawn in white instead of its
default dark colour, on a blue rounded-square tile (`#2563eb`, rim
`#1d4eba`). The tray icon for the running state adds a green badge
(`#16a34a` with a white ring). Every size is rendered directly from the
vector artwork.

Generated files (committed, so building does not need Python or resvg):

| File | Used for |
|---|---|
| `cadence.ico` | Executable icon (16, 20, 24, 32, 40, 48, 64, 256 px), embedded by `crates/app/build.rs` |
| `icon-256.png` | Window icon (title bar, taskbar, Alt+Tab) |
| `tray-idle.png`, `tray-running.png` | Notification-area icon (stopped / clicking), 32 px |

## Regenerating

Requirements: Python 3.8+ (standard library only) and the official
[resvg](https://github.com/linebender/resvg) **0.49.0** executable, a
development-time tool that is not part of the application, its build or
the repository.

1. Download `resvg-win-x86_64.zip` from the
   [v0.49.0 release](https://github.com/linebender/resvg/releases/tag/v0.49.0)
   and check it against the SHA-256 published there:

   ```powershell
   (Get-FileHash resvg-win-x86_64.zip -Algorithm SHA256).Hash
   # 38AF9631B37E61118A32A5EF31E6BE6AF083EDA63B4C17FFC4FDC63E449277DF
   ```

2. Extract `resvg.exe` anywhere outside the repository and run:

   ```powershell
   $env:RESVG = 'C:\path\to\resvg.exe'   # or put resvg.exe on PATH
   python assets\icon\make_icon.py
   ```

`make_icon.py` stops with an explanation if resvg is missing, is not the
pinned executable (it checks the SHA-256 of `resvg.exe`, listed in the
script), or reports another version. Other platforms' resvg binaries are
not pinned yet; add their executable checksums to `RESVG_EXECUTABLES` after
verifying the release archive.

The output is deterministic: running the script again with the same resvg
reproduces identical files.

At 16 px the two rings of the logo are drawn with strokes thinner than one
pixel and partly merge; this follows from the SVG's geometry, which is kept
as designed.
