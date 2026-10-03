use std::time::{Duration, Instant};

use nimbus_ipc::{Phase, Settings, State};

use crate::config::{Config, apply_settings};
use crate::rclone::Outcome;

/// A message for the engine. The watcher, the run thread, and the D-Bus
/// service send these.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Starts a run at once.
    SyncNow,
    /// A run that is active finishes. Later runs wait for a resume.
    SetPaused(bool),
    /// Writes the keys that a client may change.
    SetSettings(Settings),
    /// A read event does not send this, because rclone reads the folder
    /// during every run.
    Changed,
    /// The ratio is from 0.0 to 1.0.
    Progress(f64),
    /// The tail holds the last error messages.
    Finished { outcome: Outcome, tail: Vec<String> },
    /// The run found no listing to repair, so it stopped before rclone
    /// started. Only a resync builds a listing, and a resync waits for the
    /// user.
    NeedsResync,
    /// The user confirmed one resync.
    Resync,
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
    /// The user confirmed the pending resync. It covers one run, and the
    /// daemon forgets it on a restart, so every resync has its own answer.
    confirmed: bool,
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
            confirmed: false,
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
            Event::SetSettings(settings) => self.set_settings(&settings),
            Event::Changed => self.last_change = Some(now),
            Event::Progress(ratio) => self.on_progress(ratio),
            Event::Finished { outcome, tail } => self.on_finished(outcome, &tail, now, unix),
            Event::NeedsResync => {
                self.running = false;
                self.last_finish = Some(now);
                self.state.progress = 0.0;
                if !self.cfg.resync_pending {
                    self.cfg.resync_pending = true;
                    self.dirty = true;
                }
            }
            Event::Resync => {
                if self.cfg.resync_pending {
                    self.confirmed = true;
                    self.run_wanted = true;
                }
            }
        }
    }

    /// Returns true when the daemon must start a run. The engine stays busy
    /// afterwards, so one turn starts at most one run.
    pub fn wants_run(&mut self, now: Instant) -> bool {
        if self.running || self.cfg.paused {
            return false;
        }
        // A resync lets the local copy overwrite a remote copy that differs,
        // so it waits for the user instead of a timer.
        if self.cfg.resync_pending && !self.confirmed {
            self.state.phase = Phase::Resync;
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

    /// Takes the keys from a client. The shared function keeps the pause and
    /// the resync flag, so the daemon owns both.
    ///
    /// A client sends a move only after the user confirmed the resync that the
    /// move needs, so the move counts as that confirmation and starts it.
    fn set_settings(&mut self, settings: &Settings) {
        if apply_settings(&mut self.cfg, settings) {
            self.confirmed = true;
            self.run_wanted = true;
        }
        self.dirty = true;
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
        // A confirmation covers one run. A failed resync asks again.
        self.confirmed = false;
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
            // A new config asks for a resync, and a resync waits for the
            // user. Most tests are about runs, so they start from a settled
            // config, and the resync tests raise the flag themselves.
            resync_pending: false,
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
    fn settings_from_a_client_reach_the_config_and_the_file() {
        let at = Instant::now();
        let mut e = engine(|_| {});
        e.on_event(
            Event::SetSettings(Settings {
                remote: String::from("my-drive"),
                local: String::from("/srv/nimbus"),
                interval_secs: 60,
                debounce_secs: 5,
                extra_flags: Vec::new(),
            }),
            at,
            UNIX,
        );
        assert_eq!(e.config().remote, "my-drive");
        assert_eq!(e.config().interval_secs, 60);
        assert_eq!(e.config().debounce_secs, 5);
        assert!(e.take_dirty(), "the daemon must write the config file");
    }

    // A client that writes every key would clear the flag that a lost
    // connection needs, so the engine must not take a resync from the wire.
    #[test]
    fn settings_from_a_client_keep_the_daemon_keys() {
        let mut e = engine(|cfg| {
            cfg.paused = true;
            cfg.resync_pending = false;
        });
        e.on_event(
            Event::SetSettings(Settings {
                remote: String::from("drive"),
                local: String::from("/srv/nimbus"),
                interval_secs: 900,
                debounce_secs: 30,
                extra_flags: Vec::new(),
            }),
            Instant::now(),
            UNIX,
        );
        assert!(e.config().paused, "the daemon owns the pause");
        assert!(
            !e.config().resync_pending,
            "the paths did not move, so no resync was needed"
        );
    }

    // The engine reads both timers on every tick, so a new value applies to
    // the next tick and not after a restart.
    #[test]
    fn a_new_timer_applies_to_the_next_tick() {
        let at = Instant::now();
        let mut e = engine(|cfg| cfg.interval_secs = 900);
        done(&mut e, at, Outcome::Success, &[]);
        e.on_event(
            Event::SetSettings(Settings {
                remote: String::from("drive"),
                local: String::from("/srv/nimbus"),
                interval_secs: 60,
                debounce_secs: 30,
                extra_flags: Vec::new(),
            }),
            at,
            UNIX,
        );
        assert!(
            !e.wants_run(at + Duration::from_secs(59)),
            "the quiet time must hold the run back"
        );
        assert!(
            e.wants_run(at + Duration::from_secs(61)),
            "the new timer must fire"
        );
    }

    // A run that left a listing behind is already recoverable, because rclone
    // compares against that listing on the next run.
    #[test]
    fn a_failed_run_keeps_the_resync_flag() {
        let mut e = engine(|cfg| cfg.resync_pending = true);
        done(&mut e, Instant::now(), Outcome::Failed(7), &[]);
        assert!(e.config().resync_pending, "a failed resync must repeat");
    }

    #[test]
    fn a_failed_run_that_kept_its_listing_does_not_ask_for_a_resync() {
        let mut e = engine(|_| {});
        done(&mut e, Instant::now(), Outcome::Failed(7), &[]);
        assert!(
            !e.config().resync_pending,
            "the repair kept a listing, so no resync is needed"
        );
        assert!(!e.take_dirty(), "the config file must not change");
    }

    // A resync lets the local copy overwrite a remote copy that differs, so
    // the daemon must never start one on a timer.
    #[test]
    fn a_pending_resync_waits_for_the_user() {
        let at = Instant::now();
        let mut e = engine(|cfg| cfg.resync_pending = true);
        assert!(!e.wants_run(at), "a resync needs a confirmation");
        assert_eq!(e.snapshot().phase, Phase::Resync);
        e.on_event(Event::SyncNow, at, UNIX);
        assert!(!e.wants_run(at), "Sync now is not a confirmation");
        e.on_event(Event::Resync, at, UNIX);
        assert!(e.wants_run(at), "the confirmation starts the resync");
    }

    // The repair fails when the run left no listing at all. rclone then
    // refuses every run until a resync builds one, and the resync waits.
    #[test]
    fn a_run_without_a_listing_asks_for_a_resync() {
        let at = Instant::now();
        let mut e = engine(|_| {});
        assert!(e.wants_run(at));
        e.on_event(Event::NeedsResync, at, UNIX);
        assert!(e.config().resync_pending, "the flag survives a restart");
        assert!(e.take_dirty(), "the daemon must write the config file");
        e.on_event(Event::SyncNow, at, UNIX);
        assert!(!e.wants_run(at), "the resync waits for the user");
        assert_eq!(e.snapshot().phase, Phase::Resync);
    }

    #[test]
    fn a_confirmed_resync_recovers_the_daemon() {
        let at = Instant::now();
        let mut e = engine(|_| {});
        assert!(e.wants_run(at));
        e.on_event(Event::NeedsResync, at, UNIX);
        e.on_event(Event::Resync, at, UNIX);
        assert!(e.wants_run(at));
        done(&mut e, at, Outcome::Success, &[]);
        assert!(!e.config().resync_pending, "the clean run cleared it");
        assert_eq!(e.snapshot().phase, Phase::Idle);
    }

    // A failed resync must not repeat on a timer, because the user agreed to
    // one run and not to every retry.
    #[test]
    fn a_confirmation_covers_one_run() {
        let at = Instant::now();
        let mut e = engine(|cfg| cfg.resync_pending = true);
        e.on_event(Event::Resync, at, UNIX);
        assert!(e.wants_run(at));
        done(&mut e, at, Outcome::Failed(7), &[]);
        e.on_event(Event::SyncNow, at, UNIX);
        assert!(!e.wants_run(at), "the failed resync asks again");
    }

    // The tray sends a move only after the user confirmed the resync in the
    // settings dialog, so a second question would ask the same thing twice.
    #[test]
    fn a_moved_folder_confirms_its_resync() {
        let at = Instant::now();
        let mut e = engine(|_| {});
        assert!(e.wants_run(at));
        done(&mut e, at, Outcome::Success, &[]);
        e.on_event(
            Event::SetSettings(Settings {
                remote: String::from("drive"),
                local: String::from("/srv/elsewhere"),
                interval_secs: 900,
                debounce_secs: 30,
                extra_flags: Vec::new(),
            }),
            at,
            UNIX,
        );
        assert!(e.config().resync_pending);
        assert!(e.wants_run(at), "the move confirms and starts the resync");
    }

    // A confirmation with nothing to confirm must not arm a later resync.
    #[test]
    fn a_confirmation_without_a_pending_resync_does_nothing() {
        let at = Instant::now();
        let mut e = engine(|_| {});
        assert!(e.wants_run(at));
        done(&mut e, at, Outcome::Success, &[]);
        e.on_event(Event::Resync, at, UNIX);
        e.on_event(Event::NeedsResync, at, UNIX);
        assert!(!e.wants_run(at), "the old confirmation must not count");
    }

    // The flag is already set when a second refusal arrives, so the config
    // file is written once instead of on every attempt.
    #[test]
    fn a_repeated_refusal_does_not_rewrite_the_config() {
        let at = Instant::now();
        let mut e = engine(|_| {});
        e.on_event(Event::NeedsResync, at, UNIX);
        assert!(e.take_dirty(), "the first refusal writes the flag");
        e.on_event(Event::NeedsResync, at, UNIX);
        assert!(!e.take_dirty(), "the flag was already set");
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
