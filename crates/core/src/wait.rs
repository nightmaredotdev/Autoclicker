//! Interruptible waiting and the command channel.
//!
//! # Wake-up protocol
//!
//! Commands travel through an unbounded `mpsc` channel, which is the single
//! source of truth. [`CommandSender::send`] always enqueues *before* calling
//! [`Notify::notify`], and a notification is a *latch*: it stays set until a
//! wait consumes it. The engine empties the queue after every wake-up and
//! before computing its next deadline. Therefore:
//!
//! * a command sent after the queue was emptied but before the wait leaves the
//!   latch set, so the wait returns immediately;
//! * a notification that arrives before the queue is emptied causes at most
//!   one harmless empty pass;
//! * many notifications coalesce into one wake-up that handles all commands.

use crate::engine::Command;
use std::sync::mpsc::{self, Receiver, SendError, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Instant;

/// Why [`Waiter::wait_until`] returned. The engine re-checks both the queue
/// and the clock after every wake-up, so spurious wake-ups are harmless.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    Notified,
    DeadlineReached,
}

/// Sets the waiter's latch. Must be cheap and callable from any thread.
pub trait Notify: Send + Sync {
    fn notify(&self);
}

/// A clock plus an interruptible wait.
pub trait Waiter: Send {
    fn now(&self) -> Instant;
    /// Waits until `deadline` (forever if `None`) or until notified. A pending
    /// notification makes this return immediately and clears it.
    fn wait_until(&mut self, deadline: Option<Instant>) -> Wake;
    fn notifier(&self) -> Arc<dyn Notify>;
}

/// Lets the application choose a waiter at runtime.
impl<W: Waiter + ?Sized> Waiter for Box<W> {
    fn now(&self) -> Instant {
        (**self).now()
    }
    fn wait_until(&mut self, deadline: Option<Instant>) -> Wake {
        (**self).wait_until(deadline)
    }
    fn notifier(&self) -> Arc<dyn Notify> {
        (**self).notifier()
    }
}

/// Sends commands to the engine: enqueue first, then notify.
#[derive(Clone)]
pub struct CommandSender {
    tx: Sender<Command>,
    notify: Arc<dyn Notify>,
}

impl std::fmt::Debug for CommandSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CommandSender")
    }
}

/// The engine has shut down and no longer accepts commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the click engine is no longer running")]
pub struct EngineGone;

impl CommandSender {
    pub fn send(&self, command: Command) -> Result<(), EngineGone> {
        self.tx.send(command).map_err(|SendError(_)| EngineGone)?;
        self.notify.notify();
        Ok(())
    }
}

/// Creates a command channel whose sender notifies `notify` after every send.
pub fn command_channel(notify: Arc<dyn Notify>) -> (CommandSender, Receiver<Command>) {
    let (tx, rx) = mpsc::channel();
    (CommandSender { tx, notify }, rx)
}

/// Portable waiter built on `Mutex` + `Condvar`. Its timeout precision is
/// whatever the platform's condition variable provides (on Windows this is
/// millisecond-based and tied to the system timer resolution).
#[derive(Debug, Default)]
pub struct PlainWaiter {
    latch: Arc<Latch>,
}

#[derive(Debug, Default)]
struct Latch {
    set: Mutex<bool>,
    cond: Condvar,
}

impl Notify for Latch {
    fn notify(&self) {
        *self.set.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cond.notify_one();
    }
}

impl PlainWaiter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Waiter for PlainWaiter {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn wait_until(&mut self, deadline: Option<Instant>) -> Wake {
        let mut set = self
            .latch
            .set
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            if *set {
                *set = false;
                return Wake::Notified;
            }
            set = match deadline {
                None => self
                    .latch
                    .cond
                    .wait(set)
                    .unwrap_or_else(PoisonError::into_inner),
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Wake::DeadlineReached;
                    }
                    self.latch
                        .cond
                        .wait_timeout(set, deadline - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
        }
    }

    fn notifier(&self) -> Arc<dyn Notify> {
        self.latch.clone()
    }
}

/// Shared test for any [`Waiter`]: many threads send concurrently while the
/// consumer follows the engine's drain-then-wait protocol with no deadline.
/// Returns how many commands were received; a lost wake-up shows up as the
/// consumer never finishing (callers run this with a watchdog).
#[doc(hidden)]
pub fn stress_no_lost_wakeups<W: Waiter>(
    mut waiter: W,
    threads: usize,
    per_thread: usize,
) -> usize {
    let (tx, rx) = command_channel(waiter.notifier());
    let producers: Vec<_> = (0..threads)
        .map(|_| {
            let tx = tx.clone();
            std::thread::spawn(move || {
                for _ in 0..per_thread {
                    tx.send(Command::SetTargetFromCursor).unwrap();
                }
            })
        })
        .collect();
    drop(tx);
    let mut received = 0;
    let expected = threads * per_thread;
    while received < expected {
        while rx.try_recv().is_ok() {
            received += 1;
        }
        if received < expected {
            waiter.wait_until(None);
        }
    }
    for p in producers {
        p.join().unwrap();
    }
    received
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn notification_before_wait_is_not_lost() {
        let mut w = PlainWaiter::new();
        w.notifier().notify();
        let start = Instant::now();
        assert_eq!(w.wait_until(None), Wake::Notified);
        assert!(start.elapsed() < Duration::from_secs(1));
        // The latch was consumed: the next wait times out.
        let d = Instant::now() + Duration::from_millis(20);
        assert_eq!(w.wait_until(Some(d)), Wake::DeadlineReached);
        assert!(Instant::now() >= d);
    }

    #[test]
    fn notifications_coalesce() {
        let mut w = PlainWaiter::new();
        let n = w.notifier();
        n.notify();
        n.notify();
        assert_eq!(w.wait_until(None), Wake::Notified);
        let d = Instant::now() + Duration::from_millis(5);
        assert_eq!(w.wait_until(Some(d)), Wake::DeadlineReached);
    }

    #[test]
    fn cross_thread_notify_wakes_an_infinite_wait() {
        let mut w = PlainWaiter::new();
        let n = w.notifier();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            n.notify();
        });
        assert_eq!(w.wait_until(None), Wake::Notified);
        t.join().unwrap();
    }

    #[test]
    fn send_after_engine_gone_is_an_error() {
        let w = PlainWaiter::new();
        let (tx, rx) = command_channel(w.notifier());
        drop(rx);
        assert_eq!(tx.send(Command::Stop), Err(EngineGone));
    }

    #[test]
    fn no_lost_wakeups_under_concurrent_senders() {
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            done_tx
                .send(stress_no_lost_wakeups(PlainWaiter::new(), 8, 2_000))
                .unwrap();
        });
        let received = done_rx
            .recv_timeout(Duration::from_secs(30))
            .expect("consumer hung: a wake-up was lost");
        assert_eq!(received, 16_000);
    }
}
