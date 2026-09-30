use nimbus_ipc::{Phase, State};

/// What a menu item asks the main loop to do. The menu callback must not block,
/// so it only sends on a channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    SyncNow,
    /// The worker resolves the toggle against the view, so the click sites
    /// never need to know the current state.
    TogglePaused,
    /// Asks the main thread to show the window. The worker owns the relay,
    /// because it is the only place that knows which thread an action needs.
    ShowWindow,
}

/// What the tray shows. The daemon may not run, so the state is optional.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    Offline,
    Ready(State),
}

/// The phase of the running daemon, or nothing when the daemon does not run.
/// The queries that need only the phase start here, so the offline case is
/// paid for once.
pub fn phase(view: &View) -> Option<Phase> {
    match view {
        View::Ready(state) => Some(state.phase),
        View::Offline => None,
    }
}

pub fn is_paused(view: &View) -> bool {
    phase(view) == Some(Phase::Paused)
}

pub fn is_syncing(view: &View) -> bool {
    phase(view) == Some(Phase::Syncing)
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
        assert_eq!(phase(&View::Offline), None);
    }

    #[test]
    fn a_ready_view_reports_its_phase() {
        assert_eq!(phase(&ready(Phase::Syncing, "")), Some(Phase::Syncing));
    }

    #[test]
    fn a_missing_daemon_is_neither_paused_nor_syncing() {
        assert!(!is_paused(&View::Offline));
        assert!(!is_syncing(&View::Offline));
    }
}
