//! The click engine: a single thread that owns the input backend, the
//! schedule and the record of which button is held.
//!
//! Guarantees:
//! * presses are scheduled start-to-start; the hold runs inside the interval;
//! * a late press is performed once, and slots that can no longer be honoured
//!   are skipped (counted), never replayed as a burst;
//! * the engine's `button_held` record only changes on confirmed injections;
//! * every stop, shutdown, error and panic path attempts a release, retrying
//!   before reporting [`Fault::ReleaseFailed`].

use crate::backend::{ButtonAction, InputBackend, SendError};
use crate::config::{ClickSettings, MIN_RELEASE_GAP, TargetMode};
use crate::geometry::Point;
use crate::keys::Modifiers;
use crate::schedule::Scheduler;
use crate::status::{Fault, Notice, RunState, Snapshot, StatusSlot};
use crate::wait::{CommandSender, EngineGone, Waiter, command_channel};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How often held modifiers are polled after a modifier hotkey.
pub const MODIFIER_POLL_INTERVAL: Duration = Duration::from_millis(5);
/// How long to wait for modifiers to be released before giving up.
pub const MODIFIER_RELEASE_TIMEOUT: Duration = Duration::from_millis(500);
/// Minimum spacing of metrics-only status updates.
pub const METRICS_PUBLISH_INTERVAL: Duration = Duration::from_millis(250);
/// Delays before each release attempt: first try, immediate retry, +5 ms, +20 ms.
pub const RELEASE_ATTEMPT_DELAYS: [Duration; 4] = [
    Duration::ZERO,
    Duration::ZERO,
    Duration::from_millis(5),
    Duration::from_millis(20),
];

/// Commands accepted by the engine.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Start clicking. `release_first` are the modifiers of the hotkey that
    /// triggered the start; the first click waits until they are released.
    Start {
        release_first: Modifiers,
    },
    Stop,
    Toggle {
        release_first: Modifiers,
    },
    /// Save the current cursor position as the fixed target.
    SetTargetFromCursor,
    /// Use this point as the fixed target (e.g. a target restored from the
    /// configuration).
    SetTarget(Point),
    UpdateSettings(ClickSettings),
    /// Stop, release any held button and end the engine thread.
    Shutdown,
}

#[derive(Debug)]
enum Phase {
    Idle,
    WaitingForModifiers {
        modifiers: Modifiers,
        give_up_at: Instant,
        next_poll: Instant,
    },
    /// Button up; next press due at `press_at`.
    Pending {
        press_at: Instant,
    },
    /// Button down; release due at `release_at`.
    Holding {
        release_at: Instant,
        next_press_at: Instant,
    },
    Faulted(Fault),
}

#[derive(Debug, Default)]
struct Stats {
    total_clicks: u64,
    run_clicks: u64,
    first_press: Option<Instant>,
    last_press: Option<Instant>,
    presses: u64,
    skipped: u64,
    max_lateness: Duration,
}

/// The click engine. Create with [`Engine::new`] and drive with [`Engine::run`]
/// (usually through [`spawn_engine`]).
pub struct Engine<B: InputBackend, W: Waiter> {
    backend: B,
    waiter: W,
    commands: Receiver<Command>,
    settings: ClickSettings,
    scheduler: Scheduler,
    phase: Phase,
    target: Option<Point>,
    button_held: bool,
    stats: Stats,
    status: Arc<StatusSlot>,
    wake_consumer: Box<dyn Fn() + Send>,
    notices: Vec<Notice>,
    state_changed: bool,
    metrics_changed: bool,
    last_metrics_publish: Option<Instant>,
}

impl<B: InputBackend, W: Waiter> Engine<B, W> {
    pub fn new(
        backend: B,
        waiter: W,
        commands: Receiver<Command>,
        settings: ClickSettings,
        status: Arc<StatusSlot>,
        wake_consumer: Box<dyn Fn() + Send>,
        seed: u64,
    ) -> Self {
        Self {
            backend,
            waiter,
            commands,
            settings,
            scheduler: Scheduler::new(seed),
            phase: Phase::Idle,
            target: None,
            button_held: false,
            stats: Stats::default(),
            status,
            wake_consumer,
            notices: Vec::new(),
            state_changed: true,
            metrics_changed: false,
            last_metrics_publish: None,
        }
    }

    /// Runs until [`Command::Shutdown`] or until every sender is dropped.
    pub fn run(mut self) {
        'outer: loop {
            loop {
                match self.commands.try_recv() {
                    Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => break 'outer,
                    Ok(command) => self.handle(command),
                    Err(TryRecvError::Empty) => break,
                }
            }
            self.advance();
            self.flush_status();
            let deadline = self.next_deadline();
            self.waiter.wait_until(deadline);
        }
        self.stop();
        self.state_changed = true;
        self.flush_status();
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Start { release_first } => self.start(release_first),
            Command::Toggle { release_first } => {
                if self.is_active() {
                    self.stop();
                } else {
                    self.start(release_first);
                }
            }
            Command::Stop => self.stop(),
            Command::SetTargetFromCursor => match self.backend.cursor_position() {
                Ok(p) => {
                    self.target = Some(p);
                    self.notices.push(Notice::TargetSet(p));
                    self.state_changed = true;
                }
                Err(e) => self.notices.push(Notice::TargetQueryFailed(e)),
            },
            Command::SetTarget(p) => {
                self.target = Some(p);
                self.state_changed = true;
            }
            Command::UpdateSettings(settings) => {
                self.settings = settings;
                self.state_changed = true;
            }
            Command::Shutdown => unreachable!("handled by run()"),
        }
    }

    fn is_active(&self) -> bool {
        matches!(
            self.phase,
            Phase::WaitingForModifiers { .. } | Phase::Pending { .. } | Phase::Holding { .. }
        )
    }

    fn start(&mut self, release_first: Modifiers) {
        if self.is_active() {
            return;
        }
        if self.button_held && self.release_with_recovery().is_err() {
            self.notices.push(Notice::StartRefusedButtonHeld);
            return;
        }
        if self.settings.mode == TargetMode::Fixed && self.target.is_none() {
            self.notices.push(Notice::NoTarget);
            self.set_phase(Phase::Idle);
            return;
        }
        let now = self.waiter.now();
        if release_first.is_empty() {
            self.begin_run(now);
        } else {
            self.set_phase(Phase::WaitingForModifiers {
                modifiers: release_first,
                give_up_at: now + MODIFIER_RELEASE_TIMEOUT,
                next_poll: now,
            });
        }
    }

    fn begin_run(&mut self, now: Instant) {
        self.stats.run_clicks = 0;
        self.stats.presses = 0;
        self.stats.first_press = None;
        self.stats.last_press = None;
        self.stats.skipped = 0;
        self.stats.max_lateness = Duration::ZERO;
        self.set_phase(Phase::Pending { press_at: now });
    }

    /// Stops clicking. Releases a held button first; a failed release leaves
    /// the engine in [`Fault::ReleaseFailed`]. Stopping from a fault with no
    /// button held clears the fault.
    fn stop(&mut self) {
        if self.button_held {
            if let Err(fault) = self.release_with_recovery() {
                self.set_phase(Phase::Faulted(fault));
                return;
            }
            if matches!(self.phase, Phase::Holding { .. }) {
                self.count_click();
            }
        }
        self.set_phase(Phase::Idle);
    }

    fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
        self.state_changed = true;
    }

    fn fault(&mut self, fault: Fault) {
        self.set_phase(Phase::Faulted(fault));
    }

    /// Performs every action that is due, until the next deadline is in the future.
    fn advance(&mut self) {
        loop {
            let now = self.waiter.now();
            match self.phase {
                Phase::WaitingForModifiers {
                    modifiers,
                    give_up_at,
                    next_poll,
                } if now >= next_poll => match self.backend.held_modifiers() {
                    Ok(held) if !held.intersects(modifiers) => self.begin_run(now),
                    Ok(_) if now >= give_up_at => {
                        self.notices.push(Notice::ModifiersStillHeld(modifiers));
                        self.set_phase(Phase::Idle);
                    }
                    Ok(_) => {
                        self.phase = Phase::WaitingForModifiers {
                            modifiers,
                            give_up_at,
                            next_poll: now + MODIFIER_POLL_INTERVAL,
                        }
                    }
                    Err(e) => {
                        self.notices.push(Notice::ModifierQueryFailed(e));
                        self.set_phase(Phase::Idle);
                    }
                },
                Phase::Pending { press_at } if now >= press_at => self.press(press_at),
                Phase::Holding {
                    release_at,
                    next_press_at,
                } if now >= release_at => match self.release_with_recovery() {
                    Ok(()) => {
                        self.count_click();
                        self.phase = Phase::Pending {
                            press_at: next_press_at,
                        };
                    }
                    Err(fault) => self.fault(fault),
                },
                _ => return,
            }
        }
    }

    fn press(&mut self, press_at: Instant) {
        if self.settings.mode == TargetMode::Fixed {
            let Some(target) = self.target else {
                self.notices.push(Notice::NoTarget);
                self.set_phase(Phase::Idle);
                return;
            };
            let bounds = match self.backend.monitor_bounds(target) {
                Ok(b) => b,
                Err(e) => return self.fault(Fault::TargetOffScreen(e)),
            };
            let (dx, dy) = self.scheduler.position_offset(self.settings.radius_px);
            if let Err(e) = self
                .backend
                .move_cursor(bounds.clamp(target.offset(dx, dy)))
            {
                return self.fault(Fault::MoveFailed(e));
            }
        }

        let pressed_at = self.waiter.now();
        let hold = self.settings.hold;
        if hold.is_zero() {
            match self
                .backend
                .send_buttons(&[ButtonAction::Press, ButtonAction::Release])
            {
                Ok(()) => {}
                Err(SendError { inserted: 0, error }) => {
                    return self.fault(Fault::PressFailed(error));
                }
                Err(SendError { inserted: 1, .. }) => {
                    // The press landed but the release did not.
                    self.button_held = true;
                    if let Err(fault) = self.release_with_recovery() {
                        return self.fault(fault);
                    }
                }
                Err(_) => {} // Both inserted: nothing to recover.
            }
        } else {
            if let Err(e) = self.backend.send_buttons(&[ButtonAction::Press]) {
                return self.fault(Fault::PressFailed(e.error));
            }
            self.button_held = true;
        }

        self.record_press(press_at, pressed_at);
        let next_press_at = self.schedule_next(press_at, pressed_at, hold);
        if hold.is_zero() {
            self.count_click();
            self.phase = Phase::Pending {
                press_at: next_press_at,
            };
        } else {
            self.phase = Phase::Holding {
                release_at: pressed_at + hold,
                next_press_at,
            };
        }
    }

    /// Next press, start-to-start from the nominal press time. If that would
    /// not leave room for the hold plus a gap after the actual press, the slot
    /// (and any further missed ones) is skipped and the schedule restarts from
    /// the actual press time.
    fn schedule_next(&mut self, nominal: Instant, actual: Instant, hold: Duration) -> Instant {
        let interval = self.scheduler.interval(&self.settings);
        let next = nominal + interval;
        let earliest = actual + hold + MIN_RELEASE_GAP;
        if next >= earliest {
            return next;
        }
        let behind = actual.saturating_duration_since(next).as_nanos();
        let missed = behind / self.settings.interval.as_nanos().max(1) + 1;
        self.stats.skipped += u64::try_from(missed).unwrap_or(u64::MAX);
        self.metrics_changed = true;
        actual + interval
    }

    fn record_press(&mut self, nominal: Instant, actual: Instant) {
        let late = actual.saturating_duration_since(nominal);
        self.stats.max_lateness = self.stats.max_lateness.max(late);
        self.stats.first_press.get_or_insert(actual);
        self.stats.last_press = Some(actual);
        self.stats.presses += 1;
        self.metrics_changed = true;
    }

    fn count_click(&mut self) {
        self.stats.total_clicks += 1;
        self.stats.run_clicks += 1;
        self.metrics_changed = true;
    }

    /// Releases the held button, retrying per [`RELEASE_ATTEMPT_DELAYS`].
    fn release_with_recovery(&mut self) -> Result<(), Fault> {
        let mut last_error = None;
        for (attempt, delay) in (1u32..).zip(RELEASE_ATTEMPT_DELAYS) {
            self.pause(delay);
            match self.backend.send_buttons(&[ButtonAction::Release]) {
                Ok(()) => {
                    // Routine releases are metrics, not state changes: they are
                    // published at the throttled rate.
                    self.button_held = false;
                    if attempt > 1 {
                        self.notices
                            .push(Notice::ReleaseRecovered { attempts: attempt });
                    }
                    return Ok(());
                }
                Err(e) => last_error = Some(e.error),
            }
        }
        self.state_changed = true;
        Err(Fault::ReleaseFailed {
            error: last_error.expect("at least one attempt was made"),
            attempts: RELEASE_ATTEMPT_DELAYS.len() as u32,
            os_reports_down: self.backend.button_reported_down(),
        })
    }

    /// Waits for `duration` regardless of notifications. Commands stay queued
    /// and are handled after the current operation.
    fn pause(&mut self, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        let until = self.waiter.now() + duration;
        while self.waiter.now() < until {
            self.waiter.wait_until(Some(until));
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        let phase_deadline = match self.phase {
            Phase::Idle | Phase::Faulted(_) => None,
            Phase::WaitingForModifiers { next_poll, .. } => Some(next_poll),
            Phase::Pending { press_at } => Some(press_at),
            Phase::Holding { release_at, .. } => Some(release_at),
        };
        let metrics_deadline = self
            .metrics_changed
            .then(|| {
                self.last_metrics_publish
                    .map(|t| t + METRICS_PUBLISH_INTERVAL)
            })
            .flatten();
        match (phase_deadline, metrics_deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn flush_status(&mut self) {
        let now = self.waiter.now();
        let metrics_due = self.metrics_changed
            && self
                .last_metrics_publish
                .is_none_or(|t| now >= t + METRICS_PUBLISH_INTERVAL);
        if !self.state_changed && self.notices.is_empty() && !metrics_due {
            return;
        }
        let snapshot = self.snapshot();
        let notices = std::mem::take(&mut self.notices);
        if self.status.publish(snapshot, notices) {
            (self.wake_consumer)();
        }
        self.state_changed = false;
        self.metrics_changed = false;
        self.last_metrics_publish = Some(now);
    }

    fn snapshot(&self) -> Snapshot {
        let (state, fault) = match &self.phase {
            Phase::Idle => (RunState::Idle, None),
            Phase::WaitingForModifiers { .. } => (RunState::WaitingForModifiers, None),
            Phase::Pending { .. } | Phase::Holding { .. } => (RunState::Running, None),
            Phase::Faulted(f) => (RunState::Faulted, Some(f.clone())),
        };
        let run_cps = match (self.stats.first_press, self.stats.last_press) {
            (Some(first), Some(last)) if self.stats.presses >= 2 && last > first => {
                Some((self.stats.presses - 1) as f64 / (last - first).as_secs_f64())
            }
            _ => None,
        };
        Snapshot {
            state,
            mode: self.settings.mode,
            target: self.target,
            button_held: self.button_held,
            fault,
            total_clicks: self.stats.total_clicks,
            run_clicks: self.stats.run_clicks,
            run_cps,
            skipped: self.stats.skipped,
            max_lateness: self.stats.max_lateness,
        }
    }
}

impl<B: InputBackend, W: Waiter> Drop for Engine<B, W> {
    /// Last-resort release, reached with the button held only when the engine
    /// unwinds from a panic. No waiting here: the waiter may be the culprit.
    fn drop(&mut self) {
        for _ in 0..RELEASE_ATTEMPT_DELAYS.len() {
            if !self.button_held {
                break;
            }
            if self.backend.send_buttons(&[ButtonAction::Release]).is_ok() {
                self.button_held = false;
            }
        }
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;

/// How an engine thread ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadExit {
    Clean,
    Panicked,
    /// The thread did not finish within the timeout and was left running.
    TimedOut,
}

/// Owns the engine thread.
#[derive(Debug)]
pub struct EngineHandle {
    sender: CommandSender,
    thread: Option<JoinHandle<()>>,
    done: Receiver<()>,
}

/// Sends `()` when dropped, including during unwinding.
struct DoneSignal(mpsc::Sender<()>);

impl Drop for DoneSignal {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// Starts the engine on its own thread.
pub fn spawn_engine<B, W>(
    backend: B,
    waiter: W,
    settings: ClickSettings,
    status: Arc<StatusSlot>,
    wake_consumer: Box<dyn Fn() + Send>,
    seed: u64,
) -> std::io::Result<EngineHandle>
where
    B: InputBackend + 'static,
    W: Waiter + 'static,
{
    let (sender, commands) = command_channel(waiter.notifier());
    let (done_tx, done) = mpsc::channel();
    let engine = Engine::new(
        backend,
        waiter,
        commands,
        settings,
        status,
        wake_consumer,
        seed,
    );
    let thread = std::thread::Builder::new()
        .name("click-engine".into())
        .spawn(move || {
            let _done = DoneSignal(done_tx);
            engine.run();
        })?;
    Ok(EngineHandle {
        sender,
        thread: Some(thread),
        done,
    })
}

impl EngineHandle {
    pub fn sender(&self) -> CommandSender {
        self.sender.clone()
    }

    pub fn send(&self, command: Command) -> Result<(), EngineGone> {
        self.sender.send(command)
    }

    /// Sends [`Command::Shutdown`] and joins the thread, waiting at most `timeout`.
    pub fn shutdown(mut self, timeout: Duration) -> ThreadExit {
        let _ = self.sender.send(Command::Shutdown);
        match self.done.recv_timeout(timeout) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
            Err(RecvTimeoutError::Timeout) => return ThreadExit::TimedOut,
        }
        match self.thread.take().map(JoinHandle::join) {
            Some(Err(_)) => ThreadExit::Panicked,
            _ => ThreadExit::Clean,
        }
    }
}
