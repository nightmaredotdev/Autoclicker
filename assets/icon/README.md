# Application icon

`make_icon.py` is the source of the artwork: it defines the geometry (a blue
rounded square with a white mouse pointer and three "click" strokes; the
running tray variant adds a green badge) and renders it with the Python
standard library only.

Generated files (committed, so building does not need Python):

| File | Used for |
|---|---|
| `autoclicker.ico` | Executable icon (16, 20, 24, 32, 40, 48, 64, 256 px), embedded by `crates/app/build.rs` |
| `icon-256.png` | Window icon (title bar, taskbar, Alt+Tab) |
| `tray-idle.png`, `tray-running.png` | Notification-area icon (stopped / clicking) |

Regenerate after changing the script:

```sh
python3 assets/icon/make_icon.py
```

The output is deterministic: running the script without changes reproduces
identical files.
