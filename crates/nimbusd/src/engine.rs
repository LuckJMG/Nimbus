use std::time::{Duration, Instant};

use nimbus_ipc::{Phase, State};

use crate::config::Config;
use crate::rclone::Outcome;

/// A message for the engine. The watcher, the run thread, and the D-Bus
/// service send these.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Starts a run at once.
    SyncNow,
    /// A run that is active finishes. Later runs wait for a resume.
    SetPaused(bool),
    /// A read event does not send this, because rclone reads the folder
    /// during every run.
    Changed,
    /// The ratio is from 0.0 to 1.0.
    Progress(f64),
    /// The tail holds the last error messages.
    Finished { outcome: Outcome, tail: Vec<String> },
}

/// The state machine of the daemon. This struct decides when a run starts and
/// what the state reports. Every field is private, so the state cannot change
/// without this struct changing it.
pub struct Engine {
    cfg: Config,
    state: State,
    running: bool,
    run_wanted: bool,
    last_change: Option<Instant>,
    last_finish: Option<Instant>,
    dirty: bool,
}

impl Engine {
    /// Builds the engine. The first state follows the config.
    pub fn new(cfg: Config) -> Self {
        let phase = if cfg.paused {
            Phase::Paused
        } else {
            Phase::Idle
        };
        Self {
            cfg,
            state: State {
                phase,
                progress: 0.0,
                last_run: 0,
                last_error: String::new(),
            },
            running: false,
            run_wanted: false,
            last_change: None,
            last_finish: None,
            dirty: false,
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn snapshot(&self) -> &State {
        &self.state
    }

    /// The caller supplies both clocks, so a test can choose the time.
    pub fn on_event(&mut self, event: Event, now: Instant, unix: u64) {
        match event {
            Event::SyncNow => self.run_wanted = true,
            Event::SetPaused(paused) => self.set_paused(paused),
            Event::Changed => self.last_change = Some(now),
            Event::Progress(ratio) => self.on_progress(ratio),
            Event::Finished { outcome, tail } => self.on_finished(outcome, &tail, now, unix),
        }
    }

    /// Returns true when the daemon must start a run. The engine stays busy
    /// afterwards, so one turn starts at most one run.
    pub fn wants_run(&mut self, now: Instant) -> bool {
        if self.running || self.cfg.paused {
            return false;
        }
        let quiet = Duration::from_secs(self.cfg.debounce_secs);
        let interval = Duration::from_secs(self.cfg.interval_secs);
        let due = self.run_wanted
            || self
                .last_change
                .is_some_and(|at| now.duration_since(at) >= quiet)
            || self
                .last_finish
                .is_some_and(|at| now.duration_since(at) >= interval)
            || self.last_finish.is_none();
        if !due {
            return false;
        }
        self.run_wanted = false;
        self.last_change = None;
        self.running = true;
        self.state.phase = Phase::Syncing;
        self.state.progress = 0.0;
        self.state.last_error = String::new();
        true
    }

    /// The function clears the flag, so the daemon writes the file once.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    fn set_paused(&mut self, paused: bool) {
        self.cfg.paused = paused;
        self.dirty = true;
        if paused {
            self.state.phase = Phase::Paused;
            return;
        }
        // The user resumes because they want a sync, so the engine starts one
        // at once instead of waiting for the next file change.
        self.run_wanted = true;
        if !self.running {
            self.state.phase = Phase::Idle;
        }
    }

    fn on_progress(&mut self, ratio: f64) {
        if !self.running {
            return;
        }
        if whole_percent(ratio) == whole_percent(self.state.progress) {
            return;
        }
        self.state.progress = ratio;
        self.state.phase = Phase::Syncing;
    }

    fn on_finished(&mut self, outcome: Outcome, tail: &[String], now: Instant, unix: u64) {
        self.running = false;
        self.last_finish = Some(now);
        self.state.last_run = unix;
        // The contract says the ratio is zero while the daemon is idle, and a
        // full bar at rest would report a run that is not active.
        self.state.progress = 0.0;
        if self.cfg.paused {
            self.state.phase = Phase::Paused;
            return;
        }
        if outcome == Outcome::Success {
            self.state.phase = Phase::Idle;
            self.state.last_error = String::new();
            if self.cfg.resync_pending {
                // rclone keeps a listing of the last run. Without --resync the
                // next run cannot start, so the flag clears only after a run
                // that ended without an error.
                self.cfg.resync_pending = false;
                self.dirty = true;
            }
            return;
        }
        self.state.phase = Phase::Error;
        self.state.last_error = last_error_text(outcome, tail);
    }
}

/// The percentage that the clients see. The engine sends one update for each
/// whole percent, not one for each line.
fn whole_percent(ratio: f64) -> u32 {
    (ratio * 100.0).round() as u32
}

fn last_error_text(outcome: Outcome, tail: &[String]) -> String {
    if let Some(text) = tail.last() {
        return text.clone();
    }
    match outcome {
        Outcome::Failed(code) => format!("rclone exited with code {code}"),
        Outcome::Stopped => String::from("rclone stopped without an exit code"),
        Outcome::Success => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::config::LocalDir;

    use super::*;
    const UNIX: u64 = 1_700_000_000;

    fn engine(over: impl FnOnce(&mut Config)) -> Engine {
        let mut cfg = Config {
            local: LocalDir::new(Path::new("/srv/nimbus")),
            interval_secs: 900,
            debounce_secs: 30,
            ..Config::default()
        };
        over(&mut cfg);
        Engine::new(cfg)
    }

    fn done(engine: &mut Engine, at: Instant, outcome: Outcome, tail: &[&str]) {
        let tail = tail.iter().map(|line| String::from(*line)).collect();
        engine.on_event(Event::Finished { outcome, tail }, at, UNIX);
    }

    #[test]
    fn a_new_engine_is_idle() {
        let e = engine(|_| {});
        assert_eq!(e.snapshot().phase, Phase::Idle);
        assert_eq!(e.snapshot().progress, 0.0);
        assert_eq!(e.snapshot().last_run, 0);
        assert!(e.snapshot().last_error.is_empty());
    }

    #[test]
    fn a_new_engine_reports_paused() {
        let e = engine(|cfg| cfg.paused = true);
        assert_eq!(e.snapshot().phase, Phase::Paused);
    }

    #[test]
    fn a_new_engine_wants_a_first_run() {
        let mut e = engine(|_| {});
        assert!(
            e.wants_run(Instant::now()),
            "the first run must not wait for the interval"
        );
    }

    #[test]
    fn a_run_sets_the_syncing_phase() {
        let mut e = engine(|_| {});
        assert!(e.wants_run(Instant::now()));
        assert_eq!(e.snapshot().phase, Phase::Syncing);
        assert_eq!(e.snapshot().progress, 0.0);
    }

    #[test]
    fn one_turn_starts_one_run() {
        let mut e = engine(|_| {});
        assert!(e.wants_run(Instant::now()));
        assert!(
            !e.wants_run(Instant::now()),
            "one turn must not start a second run"
        );
    }

    #[test]
    fn a_change_does_not_start_a_run_at_once() {
        let at = Instant::now();
        let mut e = engine(|_| {});
        done(&mut e, at, Outcome::Success, &[]);
        e.on_event(Event::Changed, at, UNIX);
        assert!(!e.wants_run(at), "the quiet time must hold the run back");
    }

    #[test]
    fn a_change_starts_a_run_after_the_quiet_time() {
        let at = Instant::now();
        let mut e = engine(|_| {});
        done(&mut e, at, Outcome::Success, &[]);
        e.on_event(Event::Changed, at, UNIX);
        assert!(!e.wants_run(at + Duration::from_secs(29)));
        assert!(e.wants_run(at + Duration::from_secs(30)));
    }

    #[test]
    fn a_second_change_pushes_the_deadline_out() {
        let at = Instant::now();
        let mut e = engine(|_| {});
        done(&mut e, at, Outcome::Success, &[]);
        e.on_event(Event::Changed, at, UNIX);
        assert!(!e.wants_run(at + Duration::from_secs(25)));
        e.on_event(Event::Changed, at + Duration::from_secs(20), UNIX);
        assert!(
            !e.wants_run(at + Duration::from_secs(45)),
            "the second change resets the clock"
        );
        assert!(e.wants_run(at + Duration::from_secs(50)));
    }

    #[test]
    fn the_interval_starts_a_run() {
        let at = Instant::now();
        let mut e = engine(|_| {});
        done(&mut e, at, Outcome::Success, &[]);
        assert!(!e.wants_run(at + Duration::from_secs(899)));
        assert!(e.wants_run(at + Duration::from_secs(900)));
    }

    #[test]
    fn a_paused_engine_does_not_start_a_run() {
        let at = Instant::now();
        let mut e = engine(|_| {});
        e.on_event(Event::SetPaused(true), at, UNIX);
        assert!(!e.wants_run(at + Duration::from_secs(100_000)));
    }

    #[test]
    fn clearing_the_pause_starts_a_run() {
        let at = Instant::now();
        let mut e = engine(|cfg| cfg.paused = true);
        e.on_event(Event::SetPaused(false), at, UNIX);
        assert_eq!(e.snapshot().phase, Phase::Idle);
        assert!(e.wants_run(at), "the user resumes because they want a run");
    }

    #[test]
    fn a_sync_now_request_starts_a_run() {
        let at = Instant::now();
        let mut e = engine(|cfg| cfg.interval_secs = 100_000);
        done(&mut e, at, Outcome::Success, &[]);
        e.on_event(Event::SyncNow, at, UNIX);
        assert!(e.wants_run(at));
    }

    #[test]
    fn a_successful_run_records_the_time() {
        let mut e = engine(|_| {});
        done(&mut e, Instant::now(), Outcome::Success, &[]);
        assert_eq!(e.snapshot().last_run, UNIX);
        assert_eq!(e.snapshot().phase, Phase::Idle);
        assert!(e.snapshot().last_error.is_empty());
    }

    #[test]
    fn a_finished_run_clears_the_resync_flag() {
        let mut e = engine(|cfg| cfg.resync_pending = true);
        done(&mut e, Instant::now(), Outcome::Success, &[]);
        assert!(!e.config().resync_pending, "the first run used --resync");
        assert!(e.take_dirty(), "the daemon must write the config file");
    }

    #[test]
    fn a_failed_run_keeps_the_resync_flag() {
        let mut e = engine(|cfg| cfg.resync_pending = true);
        done(&mut e, Instant::now(), Outcome::Failed(7), &[]);
        assert!(e.config().resync_pending, "a failed resync must repeat");
    }

    #[test]
    fn a_failed_run_records_the_error_text() {
        let mut e = engine(|_| {});
        done(
            &mut e,
            Instant::now(),
            Outcome::Failed(7),
            &["first", "the last error"],
        );
        assert_eq!(e.snapshot().last_error, "the last error");
        assert_eq!(e.snapshot().phase, Phase::Error);
    }

    #[test]
    fn a_run_while_paused_keeps_the_paused_phase() {
        let mut e = engine(|cfg| cfg.paused = true);
        done(&mut e, Instant::now(), Outcome::Failed(7), &[]);
        assert_eq!(e.snapshot().phase, Phase::Paused);
    }

    #[test]
    fn a_stopped_process_reports_a_code() {
        let mut e = engine(|_| {});
        done(&mut e, Instant::now(), Outcome::Stopped, &[]);
        assert_eq!(
            e.snapshot().last_error,
            "rclone stopped without an exit code"
        );
    }

    #[test]
    fn progress_updates_once_for_each_whole_percent() {
        let at = Instant::now();
        let mut e = engine(|_| {});
        assert!(e.wants_run(at));
        e.on_event(Event::Progress(0.501), at, UNIX);
        e.on_event(Event::Progress(0.504), at, UNIX);
        assert_eq!(
            e.snapshot().progress,
            0.501,
            "the rounded percent did not change"
        );
        e.on_event(Event::Progress(0.512), at, UNIX);
        assert_eq!(e.snapshot().progress, 0.512);
    }

    #[test]
    fn a_finished_run_clears_the_ratio() {
        let at = Instant::now();
        let mut e = engine(|_| {});
        assert!(e.wants_run(at));
        e.on_event(Event::Progress(0.5), at, UNIX);
        done(&mut e, at, Outcome::Success, &[]);
        assert_eq!(
            e.snapshot().progress,
            0.0,
            "the contract says the ratio is zero while idle"
        );
    }
}
