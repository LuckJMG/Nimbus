//! The D-Bus contract between the Nimbus daemon and its clients.

use serde::{Deserialize, Serialize};
use zbus::proxy;
use zbus::zvariant::{OwnedValue, Type, Value};

// The macro attributes below take literals, so these constants and those
// literals can drift apart.
pub const BUS_NAME: &str = "io.github.luckjmg.nimbus";
pub const OBJECT_PATH: &str = "/io/github/luckjmg/nimbus";
pub const INTERFACE: &str = "io.github.luckjmg.nimbus1";

/// The phase of the sync engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, Value, OwnedValue)]
#[serde(rename_all = "lowercase")]
// Required. Without it the derive casts the enum to u32 rather than sending
// a string.
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
    /// Ratio from 0.0 to 1.0. Zero while idle.
    pub progress: f64,
    /// Unix time of the last finished run. Zero means never.
    pub last_run: u64,
    /// An empty string means no error.
    pub last_error: String,
}

#[proxy(
    interface = "io.github.luckjmg.nimbus1",
    default_service = "io.github.luckjmg.nimbus",
    default_path = "/io/github/luckjmg/nimbus"
)]
pub trait Nimbus {
    /// Start a run now. The call returns at once, without waiting.
    fn sync_now(&self) -> zbus::Result<()>;

    /// Stop after the current run, and skip every run until unpaused.
    fn set_paused(&self, paused: bool) -> zbus::Result<()>;

    // No SetMode method. The daemon runs rclone bisync and nothing else.

    #[zbus(property)]
    fn state(&self) -> zbus::Result<State>;

    // Named Changed because a State property already generates
    // receive_state_changed, and StateChanged would collide with it.
    #[zbus(signal)]
    fn changed(&self, state: State) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon uses the blocking proxy, the tray uses the async one.
    /// This stops compiling if a zbus upgrade drops either of them.
    #[test]
    fn both_proxies_exist() {
        fn names(_: Option<NimbusProxy<'_>>, _: Option<NimbusProxyBlocking<'_>>) {}
        let _ = names;
    }

    /// The daemon emits Changed by hand, so a field reorder here would break
    /// every client at runtime with no compile error.
    #[test]
    fn state_signature_is_stable() {
        assert_eq!(State::SIGNATURE, "(sdts)");
    }

    /// Phase and the TOML config share one spelling for each value.
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
