//! Console control events (Ctrl+C, Ctrl+Break, closing the console window),
//! routed into the application's normal shutdown sequence.

use super::error::Win32Error;
use std::sync::{Condvar, Mutex, OnceLock, PoisonError};
use std::time::Duration;
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};
use windows_sys::core::BOOL;

type ExitCallback = Box<dyn Fn() + Send + Sync>;

static ON_EXIT: OnceLock<ExitCallback> = OnceLock::new();
static SHUTDOWN_DONE: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

/// How long a close/logoff/shutdown event waits for the application to finish
/// its shutdown before Windows terminates the process.
const CLOSE_GRACE: Duration = Duration::from_secs(4);

/// Installs a handler that calls `on_exit` for console control events instead
/// of terminating the process. Can be installed once per process.
pub fn install_console_exit_handler(
    on_exit: impl Fn() + Send + Sync + 'static,
) -> Result<(), Win32Error> {
    if ON_EXIT.set(Box::new(on_exit)).is_err() {
        return Err(Win32Error {
            code: 0,
            message: "console exit handler already installed".into(),
        });
    }
    // SAFETY: `handler` is a valid handler routine for the process lifetime.
    if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
        return Err(Win32Error::last());
    }
    Ok(())
}

/// Signals that the application has finished shutting down, releasing a
/// close/logoff/shutdown handler that is waiting.
pub fn console_shutdown_complete() {
    let (done, cond) = &SHUTDOWN_DONE;
    *done.lock().unwrap_or_else(PoisonError::into_inner) = true;
    cond.notify_all();
}

unsafe extern "system" fn handler(ctrl_type: u32) -> BOOL {
    let Some(on_exit) = ON_EXIT.get() else {
        return 0;
    };
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            on_exit();
            1
        }
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
            // The process is terminated as soon as this returns, so wait for
            // the normal shutdown (release, unregister, join) to complete.
            on_exit();
            let (done, cond) = &SHUTDOWN_DONE;
            let guard = done.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = cond.wait_timeout_while(guard, CLOSE_GRACE, |done| !*done);
            1
        }
        _ => 0,
    }
}
