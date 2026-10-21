use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use nimbus_ipc::{Entry, Settings, State};

use crate::engine::{Engine, Event};
use crate::history::History;

/// The D-Bus surface of the daemon. The service reads the state and sends
/// requests. Only the engine changes the state.
pub struct NimbusService {
    engine: Arc<Mutex<Engine>>,
    history: Arc<Mutex<History>>,
    requests: Sender<Event>,
}

impl NimbusService {
    /// Builds the service. The engine is shared with the main loop, and the
    /// history is shared with the run threads.
    pub fn new(
        engine: Arc<Mutex<Engine>>,
        history: Arc<Mutex<History>>,
        requests: Sender<Event>,
    ) -> Self {
        Self {
            engine,
            history,
            requests,
        }
    }
}

// The attribute takes a literal, so it cannot read INTERFACE from nimbus-ipc.
// The test below fails when the two differ.
#[zbus::interface(name = "io.github.luckjmg.nimbus1")]
impl NimbusService {
    fn sync_now(&self) -> zbus::fdo::Result<()> {
        self.send(Event::SyncNow)
    }

    fn set_paused(&self, paused: bool) -> zbus::fdo::Result<()> {
        self.send(Event::SetPaused(paused))
    }

    fn get_settings(&self) -> Settings {
        self.engine
            .lock()
            .expect("the engine lock")
            .config()
            .settings()
    }

    fn resync(&self) -> zbus::fdo::Result<()> {
        self.send(Event::Resync)
    }

    /// Rejects the keys before the engine sees them.
    ///
    /// The check runs on a copy, because the engine holds the only live
    /// config. The message from `check` names the key and tells the user what
    /// to do, so it reaches the dialog unchanged.
    fn set_settings(&self, settings: Settings) -> zbus::fdo::Result<()> {
        let mut candidate = self
            .engine
            .lock()
            .expect("the engine lock")
            .config()
            .clone();
        if let Err(err) = candidate.apply(&settings).and_then(|_| candidate.check()) {
            return Err(zbus::fdo::Error::InvalidArgs(format!("{err}")));
        }
        self.send(Event::SetSettings(settings))
    }

    /// Reads the history, not the engine, so a burst of entries from a run
    /// never delays a read of the state.
    fn get_history(&self, since: u64) -> Vec<Entry> {
        self.history.lock().expect("the history lock").since(since)
    }

    #[zbus(property)]
    fn state(&self) -> State {
        self.engine
            .lock()
            .expect("the engine lock")
            .snapshot()
            .clone()
    }
}

impl NimbusService {
    /// The request goes on the same channel the file watcher uses, so the
    /// engine has one path for every request.
    fn send(&self, event: Event) -> zbus::fdo::Result<()> {
        self.requests
            .send(event)
            .map_err(|_| zbus::fdo::Error::Disconnected(String::from("the daemon loop stopped")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use nimbus_ipc::{INTERFACE, Kind};
    use zbus::object_server::Interface;

    #[test]
    fn get_history_answers_from_the_history() {
        let base = std::env::temp_dir().join(format!("nimbus-test-{}-service", std::process::id()));
        let history = Arc::new(Mutex::new(History::open(base.join("history"), 24, 1000)));
        let engine = Arc::new(Mutex::new(Engine::new(Config::default())));
        let (tx, _rx) = std::sync::mpsc::channel();
        let service = NimbusService::new(engine, Arc::clone(&history), tx);
        assert!(service.get_history(0).is_empty());
        for time in [5, 7] {
            history.lock().expect("the history lock").append(
                Entry {
                    name: String::from("a.txt"),
                    folder: String::new(),
                    time,
                    kind: Kind::Uploaded,
                },
                1000,
            );
        }
        assert_eq!(service.get_history(0).len(), 2);
        assert_eq!(service.get_history(7).len(), 1);
        assert!(service.get_history(8).is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_interface_name_matches_the_constant() {
        let declared = <NimbusService as Interface>::name();
        let wanted =
            zbus::names::InterfaceName::try_from(INTERFACE).expect("the constant is valid");
        assert_eq!(declared, wanted);
    }
}
