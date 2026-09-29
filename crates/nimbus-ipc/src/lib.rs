//! The D-Bus contract between the Nimbus daemon and its clients.

use serde::{Deserialize, Serialize};
use zbus::proxy;
use zbus::zvariant::{OwnedValue, Type, Value};

// The macro attributes below take string literals. The constants and the
// literals can drift apart.
pub const BUS_NAME: &str = "io.github.luckjmg.nimbus";
pub const OBJECT_PATH: &str = "/io/github/luckjmg/nimbus";
pub const INTERFACE: &str = "io.github.luckjmg.nimbus1";

/// The phase of the sync engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, Value, OwnedValue)]
#[serde(rename_all = "lowercase")]
// This attribute is required. Without it, the derive macro sends the enum as
// a u32 instead of a string.
#[zvariant(signature = "s", rename_all = "lowercase")]
pub enum Phase {
    Idle,
    Syncing,
    Paused,
    Error,
}

/// The full state of the daemon.
#[derive(Debug, Clone, Serialize, Deserialize, Type, OwnedValue)]
pub struct State {
    pub phase: Phase,
    /// The progress ratio, from 0.0 to 1.0. The value is zero when the daemon
    /// is idle.
    pub progress: f64,
    /// The Unix time of the last finished run. The value is zero before the
    /// first run.
    pub last_run: u64,
    /// The last error message. An empty string means that there is no error.
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

    /// Pauses the daemon. The current run finishes first. The daemon skips
    /// all later runs until you call the method again.
    fn set_paused(&self, paused: bool) -> zbus::Result<()>;

    // The interface has no SetMode method. The daemon runs rclone bisync.

    #[zbus(property)]
    fn state(&self) -> zbus::Result<State>;

    // The signal is named Changed. A property named State already generates
    // receive_state_changed. A signal named StateChanged generates the same
    // name twice.
    #[zbus(signal)]
    fn changed(&self, state: State) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon uses the blocking proxy. The tray uses the async proxy. This
    /// test stops compiling if a zbus upgrade removes either type.
    #[test]
    fn both_proxies_exist() {
        fn names(_: Option<NimbusProxy<'_>>, _: Option<NimbusProxyBlocking<'_>>) {}
        let _ = names;
    }

    /// The daemon emits the Changed signal directly. A field reorder in State
    /// breaks every client at runtime. The compiler does not detect the change.
    #[test]
    fn state_signature_is_stable() {
        assert_eq!(State::SIGNATURE, "(sdts)");
    }

    /// The Phase enum and the config file use the same four words.
    #[test]
    fn phase_names_match_serde() {
        use serde::Deserialize;
        use serde::de::IntoDeserializer;
        for (text, want) in [
            ("idle", Phase::Idle),
            ("syncing", Phase::Syncing),
            ("paused", Phase::Paused),
            ("error", Phase::Error),
        ] {
            let deserializer: serde::de::value::StrDeserializer<'_, serde::de::value::Error> =
                text.into_deserializer();
            let got = Phase::deserialize(deserializer).expect("valid phase");
            assert_eq!(got, want, "for {text}");
        }
    }
}
