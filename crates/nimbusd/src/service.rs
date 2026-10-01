use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use nimbus_ipc::{Settings, State};

use crate::config;
use crate::engine::{Engine, Event};

/// The D-Bus surface of the daemon. The service reads the state and sends
/// requests. Only the engine changes the state.
pub struct NimbusService {
    engine: Arc<Mutex<Engine>>,
    requests: Sender<Event>,
}

impl NimbusService {
    /// Builds the service. The engine is shared with the main loop.
    pub fn new(engine: Arc<Mutex<Engine>>, requests: Sender<Event>) -> Self {
        Self { engine, requests }
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
        config::settings_of(self.engine.lock().expect("the engine lock").config())
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
        config::apply_settings(&mut candidate, &settings);
        if let Err(err) = candidate.check() {
            return Err(zbus::fdo::Error::InvalidArgs(format!("{err}")));
        }
        self.send(Event::SetSettings(settings))
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
    use nimbus_ipc::INTERFACE;
    use zbus::object_server::Interface;

    #[test]
    fn the_interface_name_matches_the_constant() {
        let declared = <NimbusService as Interface>::name();
        let wanted =
            zbus::names::InterfaceName::try_from(INTERFACE).expect("the constant is valid");
        assert_eq!(declared, wanted);
    }
}
