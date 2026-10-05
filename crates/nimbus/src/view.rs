use nimbus_ipc::{Phase, Settings, State};

/// What a menu item or a button asks the tray to do. The GTK callbacks must
/// not block, so they only send on a channel.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    SyncNow,
    /// The worker resolves the toggle against the view, so the click sites
    /// never need to know the current state.
    TogglePaused,
    /// Asks the main thread to show the window. The worker owns the relay,
    /// because it is the only place that knows which thread an action needs.
    ShowWindow,
    /// Asks the worker for the keys. The blocking call stays off the GTK
    /// thread, so the answer comes back as a `Reply`.
    OpenSettings,
    /// Confirms the resync that the daemon waits for. The window sends it only
    /// after the user accepted the warning.
    Resync,
    /// Asks systemd to start the daemon, for a window that shows no daemon.
    StartDaemon,
    /// Carries the edited keys to the worker, which owns the proxy.
    SaveSettings(Settings),
}

/// What the worker sends back to the GTK thread.
///
/// The worker owns the blocking proxy, so every answer crosses a channel. The
/// dialog belongs to the main thread, so the reply decides which window moves.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// Brings the status window forward.
    Show,
    /// Rewrites the fields of the dialog with these keys.
    Settings(Settings),
    /// The daemon took the keys, so the dialog closes.
    Saved,
    /// The daemon refused. The text goes in the dialog.
    Refused(String),
}

/// What the tray shows. The daemon may not run, so the state is optional.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    /// Carries the reason that no daemon runs, with the fix.
    Offline(String),
    Ready(State),
}

/// The phase of the running daemon, or nothing when the daemon does not run.
/// The queries that need only the phase start here, so the offline case is
/// paid for once.
pub fn phase(view: &View) -> Option<Phase> {
    match view {
        View::Ready(state) => Some(state.phase),
        View::Offline(_) => None,
    }
}

pub fn is_paused(view: &View) -> bool {
    phase(view) == Some(Phase::Paused)
}

/// The full state text, for the tooltip. The tooltip has room for the error
/// message from the daemon.
pub fn status_text(view: &View) -> String {
    let View::Ready(state) = view else {
        return String::from("The daemon is not running");
    };
    match state.phase {
        Phase::Idle => String::from("Idle"),
        Phase::Syncing => format!("Syncing, {:.0}%", state.progress * 100.0),
        Phase::Paused => String::from("Paused"),
        Phase::Resync => String::from("A resync is needed"),
        Phase::Offline => String::from("No internet connection"),
        Phase::Error if state.last_error.is_empty() => String::from("The last run failed"),
        Phase::Error => state.last_error.clone(),
    }
}

/// The short state name, for the menu header. A menu row has no room for the
/// error message, and the tooltip carries it.
pub fn status_name(view: &View) -> String {
    match phase(view) {
        None => String::from("The daemon is not running"),
        Some(Phase::Error) => String::from("Error"),
        Some(_) => status_text(view),
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    /// A ready view, for the tests of the tray and the window.
    pub(crate) fn ready(phase: Phase, last_error: &str) -> View {
        View::Ready(State {
            phase,
            progress: 0.43,
            last_run: 0,
            last_error: String::from(last_error),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::ready;
    use super::*;

    #[test]
    fn an_offline_view_has_no_phase() {
        assert_eq!(phase(&View::Offline(String::new())), None);
    }

    #[test]
    fn a_ready_view_reports_its_phase() {
        assert_eq!(phase(&ready(Phase::Syncing, "")), Some(Phase::Syncing));
    }

    #[test]
    fn the_pause_state_comes_from_the_view() {
        assert!(is_paused(&ready(Phase::Paused, "")));
        assert!(!is_paused(&ready(Phase::Idle, "")));
        assert!(
            !is_paused(&View::Offline(String::new())),
            "a missing daemon is not paused"
        );
    }

    #[test]
    fn the_short_name_hides_the_error_text() {
        assert_eq!(status_name(&ready(Phase::Error, "Bisync aborted")), "Error");
        assert_eq!(
            status_text(&ready(Phase::Error, "Bisync aborted")),
            "Bisync aborted"
        );
    }
}
