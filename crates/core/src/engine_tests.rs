//! Deterministic engine tests in virtual time.

use super::*;
use crate::config::ClickConfig;
use crate::geometry::Rect;
use crate::status::StatusUpdate;
use crate::testkit::*;
use std::sync::Mutex;

const CTRL: Modifiers = Modifiers {
    ctrl: true,
    alt: false,
    shift: false,
};

fn settings(f: impl FnOnce(&mut ClickConfig)) -> ClickSettings {
    let mut c = ClickConfig {
        mode: TargetMode::Cursor,
        interval_ms: 100,
        interval_jitter_sigma_ms: 0.0,
        hold_ms: 10,
        position_jitter_radius_px: 0,
    };
    f(&mut c);
    c.validated().unwrap()
}

fn cmd(t: u64, c: Command) -> (Duration, Step) {
    (ms(t), Step::Cmd(c))
}

fn start() -> Command {
    Command::Start {
        release_first: Modifiers::NONE,
    }
}

fn toggle() -> Command {
    Command::Toggle {
        release_first: Modifiers::NONE,
    }
}

struct Outcome {
    log: Vec<(Duration, Ev)>,
    updates: Vec<StatusUpdate>,
}

impl Outcome {
    fn last(&self) -> &Snapshot {
        &self.updates.last().expect("at least one update").snapshot
    }
    fn notices(&self) -> Vec<Notice> {
        self.updates
            .iter()
            .flat_map(|u| u.notices.clone())
            .collect()
    }
    fn times(&self, ev: Ev) -> Vec<Duration> {
        self.log
            .iter()
            .filter(|(_, e)| *e == ev)
            .map(|(t, _)| *t)
            .collect()
    }
    fn presses(&self) -> Vec<Duration> {
        self.times(Ev::Press)
    }
    fn releases(&self) -> Vec<Duration> {
        self.times(Ev::Release)
    }
    fn moves(&self) -> Vec<Point> {
        self.log
            .iter()
            .filter_map(|(_, e)| match e {
                Ev::Move(p) => Some(*p),
                _ => None,
            })
            .collect()
    }
}

struct Harness {
    clock: VirtualClock,
    backend: FakeBackend,
    settings: ClickSettings,
    script: Vec<(Duration, Step)>,
    stalls: Vec<(Duration, Duration)>,
    seed: u64,
}

impl Harness {
    fn new(settings: ClickSettings) -> Self {
        let clock = VirtualClock::new();
        Self {
            backend: FakeBackend::new(clock.clone()),
            clock,
            settings,
            script: Vec::new(),
            stalls: Vec::new(),
            seed: 1,
        }
    }

    fn script(mut self, steps: Vec<(Duration, Step)>) -> Self {
        self.script = steps;
        self
    }

    /// Builds the engine; returns it with the shared event log and update list.
    #[allow(clippy::type_complexity)]
    fn build(
        self,
    ) -> (
        Engine<FakeBackend, VirtualWaiter>,
        EventLog,
        Arc<Mutex<Vec<StatusUpdate>>>,
    ) {
        let (tx, rx) = mpsc::channel();
        let waiter = VirtualWaiter::new(self.clock, tx, self.script, self.stalls);
        let log = self.backend.log.clone();
        let slot = Arc::new(StatusSlot::new(Snapshot::new(self.settings.mode())));
        let updates = Arc::new(Mutex::new(Vec::new()));
        let wake = {
            let slot = slot.clone();
            let updates = updates.clone();
            Box::new(move || updates.lock().unwrap().push(slot.take()))
        };
        let engine = Engine::new(
            self.backend,
            waiter,
            rx,
            self.settings,
            slot,
            wake,
            self.seed,
        );
        (engine, log, updates)
    }

    fn run(self) -> Outcome {
        let (engine, log, updates) = self.build();
        engine.run();
        let log = log.lock().unwrap().clone();
        let updates = updates.lock().unwrap().clone();
        Outcome { log, updates }
    }
}

fn every(start: u64, step: u64, count: u64) -> Vec<Duration> {
    (0..count).map(|i| ms(start + i * step)).collect()
}

#[test]
fn presses_are_start_to_start_regardless_of_hold() {
    for hold in [0, 10, 50] {
        let out = Harness::new(settings(|c| c.hold_ms = hold))
            .script(vec![cmd(0, start()), cmd(1000, Command::Shutdown)])
            .run();
        assert_eq!(out.presses(), every(0, 100, 10), "hold {hold}");
        assert_eq!(out.releases(), every(hold.into(), 100, 10), "hold {hold}");
        assert_eq!(out.last().total_clicks, 10);
        assert_eq!(out.last().state, RunState::Idle);
        assert!(!out.last().button_held);
    }
}

#[test]
fn zero_hold_sends_press_and_release_together() {
    let out = Harness::new(settings(|c| c.hold_ms = 0))
        .script(vec![cmd(0, start()), cmd(150, Command::Shutdown)])
        .run();
    assert_eq!(
        out.log,
        vec![
            (ms(0), Ev::Press),
            (ms(0), Ev::Release),
            (ms(100), Ev::Press),
            (ms(100), Ev::Release)
        ]
    );
}

#[test]
fn stop_mid_hold_releases_at_the_same_instant() {
    let out = Harness::new(settings(|c| c.hold_ms = 50))
        .script(vec![
            cmd(0, start()),
            cmd(120, Command::Stop),
            cmd(500, Command::Shutdown),
        ])
        .run();
    assert_eq!(out.presses(), vec![ms(0), ms(100)]);
    assert_eq!(out.releases(), vec![ms(50), ms(120)]);
    assert_eq!(out.last().total_clicks, 2);
    assert_eq!(out.last().state, RunState::Idle);
}

#[test]
fn shutdown_mid_hold_releases_before_exiting() {
    let out = Harness::new(settings(|c| c.hold_ms = 50))
        .script(vec![cmd(0, start()), cmd(130, Command::Shutdown)])
        .run();
    assert_eq!(out.releases(), vec![ms(50), ms(130)]);
    assert!(!out.last().button_held);
}

#[test]
fn losing_every_sender_shuts_down_and_releases() {
    let out = Harness::new(settings(|c| c.hold_ms = 50))
        .script(vec![cmd(0, start()), (ms(30), Step::DropSender)])
        .run();
    assert_eq!(out.log, vec![(ms(0), Ev::Press), (ms(30), Ev::Release)]);
}

#[test]
fn failed_press_faults_without_holding_a_button() {
    let mut h = Harness::new(settings(|_| {}));
    h.backend.send_plan = [Plan::Fail].into();
    let out = h
        .script(vec![cmd(0, start()), cmd(500, Command::Shutdown)])
        .run();
    assert!(out.log.is_empty(), "{:?}", out.log);
    let faulted = out
        .updates
        .iter()
        .find(|u| u.snapshot.state == RunState::Faulted)
        .expect("fault was reported");
    assert!(matches!(
        faulted.snapshot.fault,
        Some(Fault::PressFailed(_))
    ));
    assert!(!faulted.snapshot.button_held);
}

#[test]
fn partial_batch_press_is_released_and_clicking_continues() {
    let mut h = Harness::new(settings(|c| c.hold_ms = 0));
    // The press of the first [Press, Release] batch lands, the release does not.
    h.backend.send_plan = [Plan::Partial(1)].into();
    let out = h
        .script(vec![cmd(0, start()), cmd(250, Command::Shutdown)])
        .run();
    assert_eq!(out.presses(), every(0, 100, 3));
    assert_eq!(
        out.releases(),
        every(0, 100, 3),
        "recovery released at once"
    );
    assert_eq!(out.last().total_clicks, 3);
    assert!(out.notices().is_empty(), "first release attempt succeeded");
}

#[test]
fn release_is_retried_immediately_then_after_5_and_20_ms() {
    let mut h = Harness::new(settings(|_| {}));
    h.backend.send_plan = [Plan::Ok, Plan::Fail, Plan::Fail, Plan::Fail, Plan::Ok].into();
    let out = h
        .script(vec![cmd(0, start()), cmd(150, Command::Shutdown)])
        .run();
    // Attempts at 10, 10, 15 fail; the fourth at 35 ms succeeds.
    assert_eq!(out.releases(), vec![ms(35), ms(110)]);
    assert_eq!(out.presses(), vec![ms(0), ms(100)], "schedule unaffected");
    assert_eq!(
        out.notices(),
        vec![Notice::ReleaseRecovered { attempts: 4 }]
    );
}

#[test]
fn failed_release_faults_blocks_restart_and_recovers_on_stop() {
    let mut h = Harness::new(settings(|_| {}));
    h.backend.os_reports_down = Some(true);
    h.backend.send_plan = [
        Plan::Ok, // press at 0
        Plan::Fail,
        Plan::Fail,
        Plan::Fail,
        Plan::Fail, // all four release attempts fail -> fault
        Plan::Fail,
        Plan::Fail,
        Plan::Fail,
        Plan::Fail, // start at 200 tries to release first, fails
        Plan::Ok,   // stop at 300 releases
    ]
    .into();
    let out = h
        .script(vec![
            cmd(0, start()),
            cmd(200, start()),
            cmd(300, Command::Stop),
            cmd(400, start()),
            cmd(450, Command::Shutdown),
        ])
        .run();
    let fault = out
        .updates
        .iter()
        .find_map(|u| u.snapshot.fault.clone())
        .expect("release fault reported");
    assert!(matches!(
        fault,
        Fault::ReleaseFailed {
            attempts: 4,
            os_reports_down: Some(true),
            ..
        }
    ));
    assert!(out.notices().contains(&Notice::StartRefusedButtonHeld));
    assert_eq!(out.presses(), vec![ms(0), ms(400)], "no press while held");
    assert_eq!(out.releases(), vec![ms(300), ms(410)]);
    assert!(!out.last().button_held);
    assert_eq!(out.last().state, RunState::Idle);
}

#[test]
fn panic_unwinding_releases_the_held_button() {
    let mut h = Harness::new(settings(|c| c.hold_ms = 50));
    h.backend.panic_on_cursor_query = true;
    let h = h.script(vec![cmd(0, start()), cmd(20, Command::SetTargetFromCursor)]);
    let (engine, log, _) = h.build();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| engine.run()));
    assert!(result.is_err(), "the simulated panic propagated");
    assert_eq!(
        *log.lock().unwrap(),
        vec![(ms(0), Ev::Press), (ms(20), Ev::Release)]
    );
}

#[test]
fn late_deadlines_skip_missed_slots_instead_of_bursting() {
    let mut h = Harness::new(settings(|_| {}));
    // The wait for the press due at 300 ms oversleeps until 650 ms.
    h.stalls = vec![(ms(300), ms(350))];
    let out = h
        .script(vec![cmd(0, start()), cmd(900, Command::Shutdown)])
        .run();
    // 300 is performed late; 400, 500 and 600 are skipped; then normal spacing.
    assert_eq!(
        out.presses(),
        vec![ms(0), ms(100), ms(200), ms(650), ms(750), ms(850)]
    );
    let gaps: Vec<_> = out.presses().windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps.iter().all(|g| *g >= ms(100)), "no burst: {gaps:?}");
    assert_eq!(out.last().skipped, 3);
    assert_eq!(out.last().max_lateness, ms(350));
}

#[test]
fn slightly_late_press_keeps_the_original_schedule() {
    let mut h = Harness::new(settings(|_| {}));
    h.stalls = vec![(ms(300), ms(30))];
    let out = h
        .script(vec![cmd(0, start()), cmd(550, Command::Shutdown)])
        .run();
    assert_eq!(
        out.presses(),
        vec![ms(0), ms(100), ms(200), ms(330), ms(400), ms(500)]
    );
    assert_eq!(out.last().skipped, 0);
}

#[test]
fn first_click_waits_for_hotkey_modifiers_to_be_released() {
    let mut h = Harness::new(settings(|_| {}));
    h.backend.modifiers_held_until = Some((CTRL, ms(23)));
    let out = h
        .script(vec![
            cmd(
                0,
                Command::Start {
                    release_first: CTRL,
                },
            ),
            cmd(200, Command::Shutdown),
        ])
        .run();
    // Polled every 5 ms: still held at 20, released by 25.
    assert_eq!(out.presses(), vec![ms(25), ms(125)]);
    assert!(
        out.updates
            .iter()
            .any(|u| u.snapshot.state == RunState::WaitingForModifiers)
    );
}

#[test]
fn start_is_cancelled_if_modifiers_stay_held() {
    let mut h = Harness::new(settings(|_| {}));
    h.backend.modifiers_held_until = Some((CTRL, ms(10_000)));
    let out = h
        .script(vec![
            cmd(
                0,
                Command::Toggle {
                    release_first: CTRL,
                },
            ),
            cmd(1000, Command::Shutdown),
        ])
        .run();
    assert!(out.presses().is_empty());
    assert_eq!(out.notices(), vec![Notice::ModifiersStillHeld(CTRL)]);
}

#[test]
fn unrelated_held_modifiers_do_not_delay_the_start() {
    let mut h = Harness::new(settings(|_| {}));
    let shift = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };
    h.backend.modifiers_held_until = Some((shift, ms(10_000)));
    let out = h
        .script(vec![
            cmd(
                0,
                Command::Start {
                    release_first: CTRL,
                },
            ),
            cmd(50, Command::Shutdown),
        ])
        .run();
    assert_eq!(out.presses(), vec![ms(0)]);
}

#[test]
fn fixed_mode_requires_a_target_and_clicks_near_it() {
    let s = settings(|c| {
        c.mode = TargetMode::Fixed;
        c.position_jitter_radius_px = 3;
    });
    let mut h = Harness::new(s);
    h.backend.cursor = Point::new(800, 600);
    let out = h
        .script(vec![
            cmd(0, toggle()),
            cmd(10, Command::SetTargetFromCursor),
            cmd(20, toggle()),
            cmd(2020, toggle()),
            cmd(2100, Command::Shutdown),
        ])
        .run();
    let notices = out.notices();
    assert_eq!(notices[0], Notice::NoTarget);
    assert_eq!(notices[1], Notice::TargetSet(Point::new(800, 600)));
    assert_eq!(out.presses(), every(20, 100, 20));
    let moves = out.moves();
    assert_eq!(moves.len(), 20, "one move before every press");
    for p in &moves {
        let (dx, dy) = (p.x - 800, p.y - 600);
        assert!(dx * dx + dy * dy <= 9, "{p} outside radius");
    }
    assert!(
        moves.iter().any(|p| *p != Point::new(800, 600)),
        "jitter applied"
    );
    // Every move precedes its press.
    for w in out.log.windows(2) {
        if w[1].1 == Ev::Press {
            assert!(matches!(w[0].1, Ev::Move(_)));
        }
    }
}

#[test]
fn position_jitter_is_clamped_to_the_target_monitor() {
    let s = settings(|c| {
        c.mode = TargetMode::Fixed;
        c.position_jitter_radius_px = 3;
    });
    let mut h = Harness::new(s);
    h.backend.monitor = Rect::new(-1920, 0, 0, 1080);
    h.backend.cursor = Point::new(-1, 0); // top-right pixel of a left monitor
    let out = h
        .script(vec![
            cmd(0, Command::SetTargetFromCursor),
            cmd(0, start()),
            cmd(5000, Command::Shutdown),
        ])
        .run();
    let moves = out.moves();
    assert_eq!(moves.len(), 50);
    assert!(moves.iter().all(|p| p.x <= -1 && p.y >= 0), "{moves:?}");
    assert!(moves.iter().any(|p| p.x < -1 || p.y > 0));
}

#[test]
fn target_on_a_disconnected_monitor_faults_without_clicking() {
    let s = settings(|c| c.mode = TargetMode::Fixed);
    let mut h = Harness::new(s);
    h.backend.off_screen = true;
    let out = h
        .script(vec![
            cmd(0, Command::SetTargetFromCursor),
            cmd(1, start()),
            cmd(100, Command::Shutdown),
        ])
        .run();
    assert!(out.log.is_empty());
    assert!(
        out.updates
            .iter()
            .any(|u| matches!(u.snapshot.fault, Some(Fault::TargetOffScreen(_))))
    );
}

#[test]
fn failed_cursor_move_faults_without_clicking() {
    let s = settings(|c| c.mode = TargetMode::Fixed);
    let mut h = Harness::new(s);
    h.backend.fail_move = true;
    let out = h
        .script(vec![
            cmd(0, Command::SetTargetFromCursor),
            cmd(1, start()),
            cmd(100, Command::Shutdown),
        ])
        .run();
    assert!(out.presses().is_empty());
    assert!(
        out.updates
            .iter()
            .any(|u| matches!(u.snapshot.fault, Some(Fault::MoveFailed(_))))
    );
}

#[test]
fn a_fault_without_a_held_button_can_be_restarted() {
    let mut h = Harness::new(settings(|_| {}));
    h.backend.send_plan = [Plan::Fail].into();
    let out = h
        .script(vec![
            cmd(0, start()),
            cmd(50, start()),
            cmd(260, Command::Shutdown),
        ])
        .run();
    assert_eq!(out.presses(), vec![ms(50), ms(150), ms(250)]);
}

#[test]
fn jittered_intervals_are_bounded_and_reproducible() {
    let run = |seed| {
        let mut h = Harness::new(settings(|c| c.interval_jitter_sigma_ms = 10.0));
        h.seed = seed;
        h.script(vec![cmd(0, start()), cmd(20_000, Command::Shutdown)])
            .run()
            .presses()
    };
    let a = run(5);
    assert_eq!(a, run(5));
    assert_ne!(a, run(6));
    let gaps: Vec<f64> = a
        .windows(2)
        .map(|w| (w[1] - w[0]).as_secs_f64() * 1000.0)
        .collect();
    assert!(gaps.len() > 150);
    assert!(gaps.iter().all(|g| (70.0..=130.0).contains(g)), "{gaps:?}");
    let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
    assert!((mean - 100.0).abs() < 2.0, "mean {mean}");
}

#[test]
fn reports_the_achieved_click_rate() {
    let out = Harness::new(settings(|c| c.interval_ms = 50))
        .script(vec![
            cmd(0, start()),
            cmd(1001, Command::Stop),
            cmd(1100, Command::Shutdown),
        ])
        .run();
    let cps = out.last().run_cps.expect("rate available");
    assert!((cps - 20.0).abs() < 1e-9, "{cps}");
    assert_eq!(out.last().run_clicks, 21);
}

#[test]
fn settings_updates_apply_to_the_next_scheduled_interval() {
    let out = Harness::new(settings(|_| {}))
        .script(vec![
            cmd(0, start()),
            cmd(
                250,
                Command::UpdateSettings(settings(|c| c.interval_ms = 50)),
            ),
            cmd(460, Command::Shutdown),
        ])
        .run();
    // 300 was already scheduled with the old interval.
    assert_eq!(
        out.presses(),
        vec![ms(0), ms(100), ms(200), ms(300), ms(350), ms(400), ms(450)]
    );
}

#[test]
fn metrics_updates_are_throttled() {
    let out = Harness::new(settings(|c| {
        c.interval_ms = 10;
        c.hold_ms = 5;
    }))
    .script(vec![cmd(0, start()), cmd(2000, Command::Shutdown)])
    .run();
    assert_eq!(out.last().total_clicks, 200);
    // 200 clicks over 2 s: at most one metrics update per 250 ms plus the
    // initial state, the start and the final stop.
    assert!(
        out.updates.len() <= 2000 / 250 + 3,
        "{} updates",
        out.updates.len()
    );
}

#[test]
fn metrics_are_delivered_even_when_the_next_click_is_far_away() {
    let out = Harness::new(settings(|c| {
        c.interval_ms = 60_000;
        c.hold_ms = 10;
    }))
    .script(vec![cmd(0, start()), cmd(30_000, Command::Shutdown)])
    .run();
    // The click completing at 10 ms is published at the 250 ms throttle point,
    // not 60 s later.
    let first_click = out
        .updates
        .iter()
        .position(|u| u.snapshot.total_clicks == 1)
        .expect("click published");
    assert!(
        first_click < out.updates.len() - 1,
        "published before shutdown"
    );
}
