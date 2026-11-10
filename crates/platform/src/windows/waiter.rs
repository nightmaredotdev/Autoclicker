//! Precise, interruptible waiting: a high-resolution waitable timer plus an
//! auto-reset event used as the notification latch.
//!
//! This does not call `timeBeginPeriod` or change the timer resolution.

use super::error::Win32Error;
use super::handle::OwnedHandle;
use autoclicker_core::wait::{Notify, Waiter, Wake};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateEventW, CreateWaitableTimerExW, INFINITE,
    SetEvent, SetWaitableTimer, TIMER_ALL_ACCESS, WaitForMultipleObjects, WaitForSingleObject,
};

#[derive(Debug)]
struct EventNotify(OwnedHandle);

impl Notify for EventNotify {
    fn notify(&self) {
        // SAFETY: the event handle is valid for the lifetime of `self`.
        unsafe { SetEvent(self.0.raw()) };
    }
}

/// [`Waiter`] using a high-resolution waitable timer (Windows 10 1803+).
#[derive(Debug)]
pub struct HighResWaiter {
    timer: OwnedHandle,
    event: Arc<EventNotify>,
}

impl HighResWaiter {
    pub fn new() -> Result<Self, Win32Error> {
        // SAFETY: null attributes and name are allowed; the flag is valid on
        // Windows 10 1803 and later (an older system returns an error).
        let timer = unsafe {
            CreateWaitableTimerExW(
                std::ptr::null(),
                std::ptr::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS,
            )
        };
        let timer = OwnedHandle::new(timer).ok_or_else(Win32Error::last)?;
        // Auto-reset, initially non-signalled: a latch consumed by the wait.
        // SAFETY: null attributes and name are allowed.
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        let event = OwnedHandle::new(event).ok_or_else(Win32Error::last)?;
        Ok(Self {
            timer,
            event: Arc::new(EventNotify(event)),
        })
    }

    fn wait_event_only(&self, timeout_ms: u32) -> Wake {
        // SAFETY: the event handle is valid.
        let r = unsafe { WaitForSingleObject(self.event.0.raw(), timeout_ms) };
        if r == WAIT_OBJECT_0 {
            Wake::Notified
        } else {
            Wake::DeadlineReached
        }
    }
}

/// Converts a remaining duration to a relative due time in 100 ns units
/// (negative = relative), rounding up so the timer never fires early.
pub(crate) fn relative_due_time(remaining: Duration) -> i64 {
    let ticks = remaining.as_nanos().div_ceil(100).max(1);
    -i64::try_from(ticks).unwrap_or(i64::MAX)
}

impl Waiter for HighResWaiter {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wait_until(&mut self, deadline: Option<Instant>) -> Wake {
        let Some(deadline) = deadline else {
            return self.wait_event_only(INFINITE);
        };
        let now = Instant::now();
        if now >= deadline {
            // A pending notification stays latched for the next wait.
            return Wake::DeadlineReached;
        }
        let due = relative_due_time(deadline - now);
        // SAFETY: valid timer handle; `due` outlives the call; no completion routine.
        let armed =
            unsafe { SetWaitableTimer(self.timer.raw(), &due, 0, None, std::ptr::null(), 0) };
        if armed == 0 {
            // Degraded fallback: millisecond wait on the event alone.
            let ms = (deadline - now)
                .as_millis()
                .clamp(1, u128::from(INFINITE - 1)) as u32;
            return self.wait_event_only(ms);
        }
        // The event comes first so a notification wins if both are signalled.
        let handles: [HANDLE; 2] = [self.event.0.raw(), self.timer.raw()];
        // SAFETY: both handles are valid for the duration of the call.
        let r = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
        match r {
            WAIT_OBJECT_0 => Wake::Notified,
            _ if r == WAIT_OBJECT_0 + 1 => Wake::DeadlineReached,
            _ => {
                // WAIT_FAILED should not happen with valid handles; avoid spinning.
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
                Wake::DeadlineReached
            }
        }
    }

    fn notifier(&self) -> Arc<dyn Notify> {
        self.event.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoclicker_core::wait::stress_no_lost_wakeups;
    use std::sync::mpsc;

    #[test]
    fn due_time_is_relative_and_rounded_up() {
        assert_eq!(relative_due_time(Duration::from_millis(1)), -10_000);
        assert_eq!(relative_due_time(Duration::from_nanos(101)), -2);
        assert_eq!(relative_due_time(Duration::ZERO), -1);
    }

    #[test]
    fn notification_before_wait_is_latched() {
        let mut w = HighResWaiter::new().unwrap();
        w.notifier().notify();
        assert_eq!(w.wait_until(None), Wake::Notified);
        let d = Instant::now() + Duration::from_millis(5);
        assert_eq!(w.wait_until(Some(d)), Wake::DeadlineReached);
        assert!(
            Instant::now() >= d,
            "never wakes early on the deadline path"
        );
    }

    #[test]
    fn notification_interrupts_a_long_wait() {
        let mut w = HighResWaiter::new().unwrap();
        let n = w.notifier();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            n.notify();
        });
        let start = Instant::now();
        let r = w.wait_until(Some(start + Duration::from_secs(10)));
        assert_eq!(r, Wake::Notified);
        assert!(start.elapsed() < Duration::from_secs(2));
        t.join().unwrap();
    }

    #[test]
    fn short_deadlines_are_honoured_without_waking_early() {
        let mut w = HighResWaiter::new().unwrap();
        for _ in 0..20 {
            let d = Instant::now() + Duration::from_millis(2);
            // Spurious wake-ups are allowed by the contract; loop like the engine.
            while w.wait_until(Some(d)) == Wake::DeadlineReached && Instant::now() < d {}
            assert!(Instant::now() >= d);
        }
    }

    #[test]
    fn no_lost_wakeups_under_concurrent_senders() {
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let w = HighResWaiter::new().unwrap();
            done_tx.send(stress_no_lost_wakeups(w, 8, 2_000)).unwrap();
        });
        let received = done_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("consumer hung: a wake-up was lost");
        assert_eq!(received, 16_000);
    }
}
