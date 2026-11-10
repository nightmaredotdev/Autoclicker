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
    dispatch(ctrl_type, on_exit.as_ref(), &SHUTDOWN_DONE, CLOSE_GRACE)
}

/// Handles one console control event. Returns TRUE if handled.
///
/// * Ctrl+C / Ctrl+Break: request the exit and return immediately; the process
///   keeps running and the main thread performs the normal shutdown.
/// * Close / logoff / shutdown: Windows terminates the process as soon as the
///   handler returns, so request the exit and wait (at most `grace`) until the
///   application reports that shutdown is complete.
fn dispatch(
    ctrl_type: u32,
    on_exit: &(dyn Fn() + Send + Sync),
    done: &(Mutex<bool>, Condvar),
    grace: Duration,
) -> BOOL {
    match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => {
            on_exit();
            1
        }
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
            on_exit();
            let (flag, cond) = done;
            let guard = flag.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = cond.wait_timeout_while(guard, grace, |done| !*done);
            1
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    fn counter() -> (Arc<AtomicUsize>, impl Fn() + Send + Sync) {
        let n = Arc::new(AtomicUsize::new(0));
        let c = n.clone();
        (n, move || {
            c.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn new_done() -> Arc<(Mutex<bool>, Condvar)> {
        Arc::new((Mutex::new(false), Condvar::new()))
    }

    #[test]
    fn ctrl_c_and_break_request_exit_without_blocking() {
        for event in [CTRL_C_EVENT, CTRL_BREAK_EVENT] {
            let (calls, on_exit) = counter();
            let done = new_done();
            let start = Instant::now();
            assert_eq!(dispatch(event, &on_exit, &done, Duration::from_secs(10)), 1);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(
                start.elapsed() < Duration::from_millis(100),
                "must not wait"
            );
        }
    }

    #[test]
    fn close_waits_until_shutdown_is_reported_complete() {
        for event in [CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT] {
            let (calls, on_exit) = counter();
            let done = new_done();
            let signaller = {
                let done = done.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(80));
                    *done.0.lock().unwrap() = true;
                    done.1.notify_all();
                })
            };
            let start = Instant::now();
            assert_eq!(dispatch(event, &on_exit, &done, Duration::from_secs(10)), 1);
            let waited = start.elapsed();
            signaller.join().unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(
                waited >= Duration::from_millis(70),
                "returned early: {waited:?}"
            );
            assert!(waited < Duration::from_secs(2), "did not wake: {waited:?}");
        }
    }

    #[test]
    fn close_returns_after_the_grace_period_if_shutdown_hangs() {
        let (_, on_exit) = counter();
        let done = new_done();
        let start = Instant::now();
        assert_eq!(
            dispatch(
                CTRL_CLOSE_EVENT,
                &on_exit,
                &done,
                Duration::from_millis(100)
            ),
            1
        );
        let waited = start.elapsed();
        assert!(waited >= Duration::from_millis(90) && waited < Duration::from_secs(2));
    }

    #[test]
    fn close_after_completion_returns_immediately() {
        let (_, on_exit) = counter();
        let done = new_done();
        *done.0.lock().unwrap() = true;
        let start = Instant::now();
        dispatch(CTRL_CLOSE_EVENT, &on_exit, &done, Duration::from_secs(10));
        assert!(start.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn unknown_events_are_passed_on() {
        let (calls, on_exit) = counter();
        assert_eq!(dispatch(99, &on_exit, &new_done(), Duration::ZERO), 0);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
