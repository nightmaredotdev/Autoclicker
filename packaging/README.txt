Autoclicker {{VERSION}} for Windows (portable)
==============================================

A desktop autoclicker with precise, configurable click timing, human-like
variation, clicking at a fixed position or at the cursor, and global hotkeys.

Requirements: Windows 10 or 11 (64-bit). No installation or administrator
rights are needed.

Getting started
---------------
1. Extract this folder anywhere (for example to your Desktop or a USB drive).
2. Run autoclicker.exe.
3. Choose "Fixed position" or "Current position" (click wherever the
   cursor is). For a fixed position, click "Pick target", then click the
   spot on the screen (right-click cancels). Adjust the timing and press
   Start.

Default hotkeys (global, they work while other programs are focused):
  F6              Toggle start/stop clicking
  F7              Set the target to the current cursor position
  Ctrl+Shift+F8   Exit

While Autoclicker runs, Windows does not pass these key combinations to the
focused program. All hotkeys can be changed on the Hotkeys tab.

Notification area
-----------------
Closing the window hides it; Autoclicker keeps running (and keeps clicking
if it was clicking) in the notification area. Use the tray icon to show the
window, start or stop clicking, open the log folder, or exit. The icon shows
a green dot while clicking. "Exit" (tray, window or hotkey) stops clicking,
releases the mouse button and quits.

On Windows 10 a new notification-area icon may start in the overflow area
(the ^ arrow next to the clock). Drag it onto the taskbar to keep it visible.

Only one copy runs at a time: starting it again shows the running window.

Files
-----
Settings: %APPDATA%\Autoclicker\config.toml
Logs:     %LOCALAPPDATA%\Autoclicker\logs\autoclicker.log (about 2 MB max)
Nothing is written to this folder. To remove Autoclicker, delete this folder
and, if you like, the two folders above.

Limitations
-----------
- Windows blocks clicks into programs running as administrator unless
  Autoclicker is also run as administrator.
- If Autoclicker is killed (for example from Task Manager) while a mouse
  button is held, click that button once to release it.
- Some programs ignore simulated input.
- The executable is not code-signed, so Windows SmartScreen may ask for
  confirmation the first time.

License: MIT (see LICENSE.txt). Third-party licenses: THIRD-PARTY-NOTICES.txt.
Source: https://github.com/nightmaredotdev/Autoclicker
