//! Deterministic test doubles: a virtual clock, a scripted waiter and a fake
//! input backend. Engine tests run single-threaded in virtual time and never
//! sleep.

use crate::backend::{ButtonAction, InputBackend, InputError, SendError};
use crate::engine::Command;
use crate::geometry::{Point, Rect};
use crate::keys::Modifiers;
use crate::wait::{Notify, Waiter, Wake};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn ms(v: u64) -> Duration {
    Duration::from_millis(v)
}

#[derive(Clone, Debug)]
pub struct VirtualClock {
    base: Instant,
    now: Arc<Mutex<Instant>>,
}

impl VirtualClock {
    pub fn new() -> Self {
        let base = Instant::now();
        Self {
            base,
            now: Arc::new(Mutex::new(base)),
        }
    }
    pub fn now(&self) -> Instant {
        *self.now.lock().unwrap()
    }
    pub fn elapsed(&self) -> Duration {
        self.now() - self.base
    }
    pub fn at(&self, offset: Duration) -> Instant {
        self.base + offset
    }
    fn advance_to(&self, t: Instant) {
        let mut now = self.now.lock().unwrap();
        if t > *now {
            *now = t;
        }
    }
}

/// A scripted step delivered at a virtual time.
pub enum Step {
    Cmd(Command),
    /// Drop the only sender, simulating every command source going away.
    DropSender,
}

struct FlagNotify(Arc<AtomicBool>);

impl Notify for FlagNotify {
    fn notify(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

pub struct VirtualWaiter {
    clock: VirtualClock,
    script: VecDeque<(Duration, Step)>,
    tx: Option<Sender<Command>>,
    /// `(at, extra)`: the first wait whose deadline is at or after `at`
    /// oversleeps by `extra`, simulating a stalled system.
    stalls: VecDeque<(Duration, Duration)>,
    latch: Arc<AtomicBool>,
}

impl VirtualWaiter {
    pub fn new(
        clock: VirtualClock,
        tx: Sender<Command>,
        script: Vec<(Duration, Step)>,
        stalls: Vec<(Duration, Duration)>,
    ) -> Self {
        let mut script = script;
        script.sort_by_key(|(t, _)| *t);
        Self {
            clock,
            script: script.into(),
            tx: Some(tx),
            stalls: stalls.into(),
            latch: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl Waiter for VirtualWaiter {
    fn now(&self) -> Instant {
        self.clock.now()
    }

    fn wait_until(&mut self, deadline: Option<Instant>) -> Wake {
        if self.latch.swap(false, Ordering::SeqCst) {
            return Wake::Notified;
        }
        let next_step = self.script.front().map(|(t, _)| self.clock.at(*t));
        if let Some(t) = next_step
            && deadline.is_none_or(|d| t <= d)
        {
            self.clock.advance_to(t);
            match self.script.pop_front().unwrap().1 {
                Step::Cmd(c) => {
                    let _ = self.tx.as_ref().expect("sender dropped").send(c);
                }
                Step::DropSender => self.tx = None,
            }
            return Wake::Notified;
        }
        let Some(mut target) = deadline else {
            panic!("engine is waiting forever but the script is exhausted");
        };
        if let Some(&(at, extra)) = self.stalls.front()
            && target >= self.clock.at(at)
        {
            self.stalls.pop_front();
            target += extra;
        }
        self.clock.advance_to(target);
        Wake::DeadlineReached
    }

    fn notifier(&self) -> Arc<dyn Notify> {
        Arc::new(FlagNotify(self.latch.clone()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ev {
    Move(Point),
    Press,
    Release,
}

/// What the next `send_buttons` call does.
#[derive(Clone, Copy, Debug)]
pub enum Plan {
    Ok,
    Fail,
    /// Insert only the first `n` actions.
    Partial(usize),
}

pub type EventLog = Arc<Mutex<Vec<(Duration, Ev)>>>;

pub struct FakeBackend {
    clock: VirtualClock,
    pub log: EventLog,
    pub cursor: Point,
    pub monitor: Rect,
    pub send_plan: VecDeque<Plan>,
    /// Modifiers reported as held until the given virtual time.
    pub modifiers_held_until: Option<(Modifiers, Duration)>,
    pub panic_on_cursor_query: bool,
    pub fail_move: bool,
    pub off_screen: bool,
    pub os_reports_down: Option<bool>,
}

impl FakeBackend {
    pub fn new(clock: VirtualClock) -> Self {
        Self {
            clock,
            log: Arc::default(),
            cursor: Point::new(500, 400),
            monitor: Rect::new(0, 0, 1920, 1080),
            send_plan: VecDeque::new(),
            modifiers_held_until: None,
            panic_on_cursor_query: false,
            fail_move: false,
            off_screen: false,
            os_reports_down: None,
        }
    }

    fn record(&self, ev: Ev) {
        self.log.lock().unwrap().push((self.clock.elapsed(), ev));
    }
}

impl InputBackend for FakeBackend {
    fn cursor_position(&mut self) -> Result<Point, InputError> {
        assert!(!self.panic_on_cursor_query, "simulated backend panic");
        Ok(self.cursor)
    }

    fn move_cursor(&mut self, to: Point) -> Result<(), InputError> {
        if self.fail_move {
            return Err(InputError::new("SetCursorPos", "simulated failure"));
        }
        self.cursor = to;
        self.record(Ev::Move(to));
        Ok(())
    }

    fn monitor_bounds(&mut self, at: Point) -> Result<Rect, InputError> {
        if self.off_screen || !self.monitor.contains(at) {
            return Err(InputError::new("MonitorFromPoint", "no monitor"));
        }
        Ok(self.monitor)
    }

    fn send_buttons(&mut self, actions: &[ButtonAction]) -> Result<(), SendError> {
        let inserted = match self.send_plan.pop_front().unwrap_or(Plan::Ok) {
            Plan::Ok => actions.len(),
            Plan::Fail => 0,
            Plan::Partial(n) => n.min(actions.len()),
        };
        for a in &actions[..inserted] {
            self.record(match a {
                ButtonAction::Press => Ev::Press,
                ButtonAction::Release => Ev::Release,
            });
        }
        if inserted == actions.len() {
            Ok(())
        } else {
            Err(SendError {
                inserted,
                error: InputError::new("SendInput", "simulated failure"),
            })
        }
    }

    fn held_modifiers(&mut self) -> Result<Modifiers, InputError> {
        Ok(match self.modifiers_held_until {
            Some((m, until)) if self.clock.elapsed() < until => m,
            _ => Modifiers::NONE,
        })
    }

    fn button_reported_down(&mut self) -> Option<bool> {
        self.os_reports_down
    }
}
