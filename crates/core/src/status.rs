//! Engine status reporting.
//!
//! The engine never performs I/O. It writes the latest [`Snapshot`] (plus any
//! [`Notice`]s) into a [`StatusSlot`] and wakes the consumer only when no
//! wake-up is already pending, so at most one update is ever queued for the
//! UI regardless of the click rate.

use crate::backend::InputError;
use crate::config::TargetMode;
use crate::geometry::Point;
use crate::keys::Modifiers;
use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// Coarse engine state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Idle,
    /// Started by a hotkey with modifiers; waiting for them to be released.
    WaitingForModifiers,
    Running,
    /// Stopped because of an error; see [`Snapshot::fault`].
    Faulted,
}

impl fmt::Display for RunState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RunState::Idle => "stopped",
            RunState::WaitingForModifiers => "waiting for modifier keys to be released",
            RunState::Running => "running",
            RunState::Faulted => "stopped by an error",
        })
    }
}

/// An error that stopped clicking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The saved target is not on any connected monitor.
    TargetOffScreen(InputError),
    MoveFailed(InputError),
    /// The press was not injected; no button is held.
    PressFailed(InputError),
    /// Every release attempt failed; the button may still be held.
    ReleaseFailed {
        error: InputError,
        attempts: u32,
        os_reports_down: Option<bool>,
    },
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::TargetOffScreen(e) => write!(f, "the target is not on any monitor ({e})"),
            Fault::MoveFailed(e) => write!(f, "could not move the cursor ({e})"),
            Fault::PressFailed(e) => write!(f, "could not press the mouse button ({e})"),
            Fault::ReleaseFailed {
                error,
                attempts,
                os_reports_down,
            } => {
                write!(
                    f,
                    "could not release the mouse button after {attempts} attempts ({error}); "
                )?;
                match os_reports_down {
                    Some(false) => f.write_str("Windows now reports it as released"),
                    _ => f.write_str(
                        "it may still be held - click that mouse button once to clear it",
                    ),
                }
            }
        }
    }
}

/// A one-off event worth telling the user about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    TargetSet(Point),
    TargetQueryFailed(InputError),
    /// Start requested in fixed-target mode before a target was set.
    NoTarget,
    /// The start hotkey's modifiers were still held after the timeout.
    ModifiersStillHeld(Modifiers),
    ModifierQueryFailed(InputError),
    /// A release succeeded only after retrying.
    ReleaseRecovered {
        attempts: u32,
    },
    /// Clicking cannot start while a button may still be held.
    StartRefusedButtonHeld,
}

impl fmt::Display for Notice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Notice::TargetSet(p) => write!(f, "target set to {p}"),
            Notice::TargetQueryFailed(e) => write!(f, "could not read the cursor position ({e})"),
            Notice::NoTarget => f.write_str("no target set yet - set one before starting"),
            Notice::ModifiersStillHeld(m) => {
                write!(f, "start cancelled: {m} still held after 500 ms")
            }
            Notice::ModifierQueryFailed(e) => {
                write!(f, "start cancelled: could not read modifier keys ({e})")
            }
            Notice::ReleaseRecovered { attempts } => {
                write!(f, "mouse button released after {attempts} attempts")
            }
            Notice::StartRefusedButtonHeld => {
                f.write_str("cannot start while the mouse button may still be held")
            }
        }
    }
}

/// The latest engine state and metrics.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub state: RunState,
    pub mode: TargetMode,
    pub target: Option<Point>,
    /// The engine's record of whether it holds the button down.
    pub button_held: bool,
    pub fault: Option<Fault>,
    pub total_clicks: u64,
    pub run_clicks: u64,
    /// Average presses per second in the current run (needs two presses).
    pub run_cps: Option<f64>,
    /// Scheduled presses dropped because the engine fell behind.
    pub skipped: u64,
    /// Largest delay between a scheduled and an actual press in this run.
    pub max_lateness: Duration,
}

impl Snapshot {
    pub fn new(mode: TargetMode) -> Self {
        Self {
            state: RunState::Idle,
            mode,
            target: None,
            button_held: false,
            fault: None,
            total_clicks: 0,
            run_clicks: 0,
            run_cps: None,
            skipped: 0,
            max_lateness: Duration::ZERO,
        }
    }
}

/// Maximum number of undelivered notices kept; older ones are dropped.
const MAX_PENDING_NOTICES: usize = 32;

/// Latest-value mailbox between the engine and its consumer.
#[derive(Debug)]
pub struct StatusSlot {
    inner: Mutex<SlotInner>,
    wake_pending: AtomicBool,
}

#[derive(Debug)]
struct SlotInner {
    snapshot: Snapshot,
    notices: VecDeque<Notice>,
    dropped_notices: u64,
}

/// What the consumer receives from [`StatusSlot::take`].
#[derive(Clone, Debug, PartialEq)]
pub struct StatusUpdate {
    pub snapshot: Snapshot,
    pub notices: Vec<Notice>,
    pub dropped_notices: u64,
}

impl StatusSlot {
    pub fn new(initial: Snapshot) -> Self {
        Self {
            inner: Mutex::new(SlotInner {
                snapshot: initial,
                notices: VecDeque::new(),
                dropped_notices: 0,
            }),
            wake_pending: AtomicBool::new(false),
        }
    }

    /// Stores the snapshot and notices. Returns `true` if the caller must wake
    /// the consumer (no wake-up was pending).
    pub fn publish(&self, snapshot: Snapshot, notices: impl IntoIterator<Item = Notice>) -> bool {
        {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            inner.snapshot = snapshot;
            for n in notices {
                if inner.notices.len() == MAX_PENDING_NOTICES {
                    inner.notices.pop_front();
                    inner.dropped_notices += 1;
                }
                inner.notices.push_back(n);
            }
        }
        !self.wake_pending.swap(true, Ordering::SeqCst)
    }

    /// Takes the latest state. The pending flag is cleared *before* reading, so
    /// a publish racing with this call either is included in the result or
    /// triggers a new wake-up.
    pub fn take(&self) -> StatusUpdate {
        self.wake_pending.store(false, Ordering::SeqCst);
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        StatusUpdate {
            snapshot: inner.snapshot.clone(),
            notices: inner.notices.drain(..).collect(),
            dropped_notices: std::mem::take(&mut inner.dropped_notices),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(clicks: u64) -> Snapshot {
        Snapshot {
            total_clicks: clicks,
            ..Snapshot::new(TargetMode::Cursor)
        }
    }

    #[test]
    fn only_the_first_publish_requests_a_wakeup_until_taken() {
        let slot = StatusSlot::new(snap(0));
        assert!(slot.publish(snap(1), []));
        assert!(!slot.publish(snap(2), [Notice::NoTarget]));
        assert!(!slot.publish(snap(3), [Notice::TargetSet(Point::new(1, 2))]));
        let u = slot.take();
        assert_eq!(u.snapshot.total_clicks, 3, "latest snapshot wins");
        assert_eq!(
            u.notices,
            vec![Notice::NoTarget, Notice::TargetSet(Point::new(1, 2))],
            "notices are not coalesced away"
        );
        assert!(
            slot.publish(snap(4), []),
            "after take, a new wake-up is needed"
        );
    }

    #[test]
    fn notices_are_bounded() {
        let slot = StatusSlot::new(snap(0));
        for _ in 0..(MAX_PENDING_NOTICES + 5) {
            slot.publish(snap(0), [Notice::NoTarget]);
        }
        let u = slot.take();
        assert_eq!(u.notices.len(), MAX_PENDING_NOTICES);
        assert_eq!(u.dropped_notices, 5);
    }

    #[test]
    fn concurrent_publish_and_take_never_strands_an_update() {
        use std::sync::Arc;
        use std::sync::mpsc;
        let slot = Arc::new(StatusSlot::new(snap(0)));
        let (wake_tx, wake_rx) = mpsc::channel::<()>();
        let producer = {
            let slot = slot.clone();
            std::thread::spawn(move || {
                for i in 1..=50_000 {
                    if slot.publish(snap(i), []) {
                        wake_tx.send(()).unwrap();
                    }
                }
            })
        };
        let mut last = 0;
        // Consume until the producer is done and every wake-up is handled.
        while wake_rx.recv().is_ok() {
            last = slot.take().snapshot.total_clicks;
        }
        producer.join().unwrap();
        assert_eq!(last, 50_000, "the final state must always be delivered");
    }
}
