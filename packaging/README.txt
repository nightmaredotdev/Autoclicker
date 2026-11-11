Cadence {{VERSION}} for Windows (portable)
==========================================

Cadence is a desktop autoclicker with precise, configurable click timing, human-like
variation, clicking at a fixed position or at the cursor, and global hotkeys.

Requirements: Windows 10 or 11 (64-bit). No installation or administrator
rights are needed.

Getting started
---------------
1. Extract this folder anywhere (for example to your Desktop or a USB drive).
2. Run cadence.exe.
3. Choose "Fixed position" or "Current position" (click wherever the
   cursor is). For a fixed position, click "Pick target", then click the
   spot on the screen (right-click cancels). Adjust the timing and press
   Start.

Default hotkeys (global, they work while other programs are focused):
  F6              Toggle start/stop clicking
  F7              Set the target to the current cursor position
  Ctrl+Shift+F8   Exit

While Cadence runs, Windows does not pass these key combinations to the
focused program. All hotkeys can be changed on the Hotkeys tab.

Notification area
-----------------
Closing the window hides it; Cadence keeps running (and keeps clicking
if it was clicking) in the notification area. Use the tray icon to show the
window, start or stop clicking, open the log folder, or exit. The icon shows
a green dot while clicking. "Exit" (tray, window or hotkey) stops clicking,
releases the mouse button and quits.

On Windows 10 a new notification-area icon may start in the overflow area
(the ^ arrow next to the clock). Drag it onto the taskbar to keep it visible.

Only one copy runs at a time: starting it again shows the running window.

Upgrading from Autoclicker 1.0.0
--------------------------------
Cadence was previously named Autoclicker.
1. Exit Autoclicker first (tray icon > Exit). The two never run at the
   same time: while Autoclicker runs, starting Cadence shows Autoclicker's
   window instead, and the other way round.
2. On its first start, Cadence copies your Autoclicker settings
   (%APPDATA%\Autoclicker\config.toml) to its own settings file. The old
   file is not changed.
3. Shortcuts and taskbar pins that point to autoclicker.exe still start the
   old program; replace them with cadence.exe.

Files
-----
Settings: %APPDATA%\Cadence\config.toml
Logs:     %LOCALAPPDATA%\Cadence\logs\cadence.log (about 2 MB max)
Nothing is written to this folder. To remove Cadence, delete this folder
and, if you like, the two folders above.

Limitations
-----------
- Windows blocks clicks into programs running as administrator unless
  Cadence is also run as administrator.
- If Cadence is killed (for example from Task Manager) while a mouse
  button is held, click that button once to release it.
- Some programs ignore simulated input.
- The executable is not code-signed, so Windows SmartScreen may ask for
  confirmation the first time.

License: MIT (see LICENSE.txt). Third-party licenses: THIRD-PARTY-NOTICES.txt.
Source: https://github.com/nightmaredotdev/Cadence
