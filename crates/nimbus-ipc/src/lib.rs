//! The D-Bus contract between the Nimbus daemon and its clients.
//!
//! This crate holds no logic. It holds the names, the types, and the
//! signatures that the daemon and the clients agree on.

use serde::{Deserialize, Serialize};
use zbus::proxy;
use zbus::zvariant::{OwnedValue, Type, Value};

// The macro attributes below need literals, so these three constants and
// those three literals must stay in step. A rename touches this file only.
pub const BUS_NAME: &str = "io.github.luckjmg.nimbus";
pub const OBJECT_PATH: &str = "/io/github/luckjmg/nimbus";
pub const INTERFACE: &str = "io.github.luckjmg.nimbus1";

/// The phase of the sync engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, Value, OwnedValue)]
#[serde(rename_all = "lowercase")]
// The signature attribute is required. Without it the derive casts the enum
// to u32 instead of sending a string over the bus.
#[zvariant(signature = "s", rename_all = "lowercase")]
pub enum Phase {
    Idle,
    Syncing,
    Paused,
    Error,
}

/// The full state of the daemon. The clients read this and nothing else.
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

    /// Switch between "sync" and "bisync". Any other value is rejected.
    fn set_mode(&self, mode: &str) -> zbus::Result<()>;

    #[zbus(property)]
    fn state(&self) -> zbus::Result<State>;

    // The signal is named Changed, not StateChanged. A property called
    // State already generates receive_state_changed, and a signal called
    // StateChanged would generate that same name a second time.
    #[zbus(signal)]
    fn changed(&self, state: State) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon uses the blocking proxy and the tray uses the async one.
    /// This test stops compiling if a zbus upgrade drops either of them.
    #[test]
    fn both_proxies_exist() {
        fn names(_: Option<NimbusProxy<'_>>, _: Option<NimbusProxyBlocking<'_>>) {}
        let _ = names;
    }

    /// The daemon emits the Changed signal by hand, so a field reorder here
    /// would break every client at runtime with no compile error. This test
    /// is the only thing that catches it.
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
