use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use nimbus_ipc::State;

use crate::engine::{Command, Engine, Event};

/// The D-Bus surface of the daemon. The service reads the state and sends
/// commands. Only the engine changes the state.
pub struct NimbusService {
    engine: Arc<Mutex<Engine>>,
    commands: Sender<Event>,
}

impl NimbusService {
    /// Builds the service. The engine is shared with the main loop.
    pub fn new(engine: Arc<Mutex<Engine>>, commands: Sender<Event>) -> Self {
        Self { engine, commands }
    }
}

// The attribute takes a literal, so it must stay in step with the constant in
// nimbus-ipc. A test below fails when the two differ.
#[zbus::interface(name = "io.github.luckjmg.nimbus1")]
impl NimbusService {
    fn sync_now(&self) -> zbus::fdo::Result<()> {
        self.send(Command::SyncNow)
    }

    fn set_paused(&self, paused: bool) -> zbus::fdo::Result<()> {
        self.send(Command::SetPaused(paused))
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
    /// The command goes on the same channel the file watcher uses, so the
    /// engine has one path for every request.
    fn send(&self, command: Command) -> zbus::fdo::Result<()> {
        self.commands
            .send(Event::Command(command))
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
