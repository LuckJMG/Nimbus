//! The D-Bus contract between the Nimbus daemon and its clients.

use serde::Serialize;
use zbus::proxy;
use zbus::zvariant::{OwnedValue, Type, Value};
// The macro attribute below takes a string literal, so it cannot read these
// constants. A test in this crate compares the two, and a second test in
// `nimbusd` compares the service attribute against INTERFACE.
pub const BUS_NAME: &str = "io.github.luckjmg.nimbus";
pub const OBJECT_PATH: &str = "/io/github/luckjmg/nimbus";
pub const INTERFACE: &str = "io.github.luckjmg.nimbus1";

/// The phase of the sync engine.
///
/// The Serialize derive carries the signal. `emit_signal` takes a value that
/// implements Serialize, and it is the only way to build the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type, Value, OwnedValue)]
// This attribute is required. Without it, the derive macro sends the enum as
// a u32 instead of a string.
#[zvariant(signature = "s", rename_all = "lowercase")]
pub enum Phase {
    Idle,
    Syncing,
    Paused,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Type, Value, OwnedValue)]
pub struct State {
    pub phase: Phase,
    /// The value is from 0.0 to 1.0, and is zero while the daemon is idle.
    pub progress: f64,
    /// The Unix time of the last finished run. The value is zero before the
    /// first run.
    pub last_run: u64,
    /// An empty string means that there is no error.
    pub last_error: String,
}

#[proxy(
    interface = "io.github.luckjmg.nimbus1",
    default_service = "io.github.luckjmg.nimbus",
    default_path = "/io/github/luckjmg/nimbus"
)]
pub trait Nimbus {
    /// Starts a run now. The call returns before the run finishes.
    fn sync_now(&self) -> zbus::Result<()>;

    /// Pauses the daemon. The daemon skips all later runs until you call the
    /// method again.
    fn set_paused(&self, paused: bool) -> zbus::Result<()>;

    // The interface has no SetMode method. The daemon runs rclone bisync.

    #[zbus(property)]
    fn state(&self) -> zbus::Result<State>;

    // The trait declares no signal. The daemon emits Changed on its own, and
    // the tray reads the property on a timer, because a dropped signal looks
    // the same as an idle daemon.
}

#[cfg(test)]
mod tests {
    use zbus::proxy::Defaults;

    use super::*;

    /// The proxy macro takes string literals, so the names above and the names
    /// inside the attribute can drift apart. The macro keeps its own copy, and
    /// this test compares it.
    #[test]
    fn the_proxy_uses_the_constants() {
        assert_eq!(
            NimbusProxy::INTERFACE.as_ref().map(|name| name.as_str()),
            Some(INTERFACE)
        );
        assert_eq!(
            NimbusProxy::DESTINATION.as_ref().map(|name| name.as_str()),
            Some(BUS_NAME)
        );
        assert_eq!(
            NimbusProxy::PATH.as_ref().map(|path| path.as_str()),
            Some(OBJECT_PATH)
        );
    }

    /// The daemon emits the Changed signal directly. A field reorder in State
    /// breaks every client at runtime. The compiler does not detect the change.
    #[test]
    fn state_signature_is_stable() {
        assert_eq!(State::SIGNATURE, "(sdts)");
    }
}
