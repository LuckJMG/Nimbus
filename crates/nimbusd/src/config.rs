use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use nimbus_ipc::Settings;
use serde::{Deserialize, Serialize};

/// The local folder. The value keeps the text from the file, so a leading
/// tilde survives a round trip. Call `path` for the folder that the daemon
/// opens, because rclone cannot open a path that starts with a tilde.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalDir(PathBuf);

impl LocalDir {
    /// Builds the value from the text in the file. The text keeps a tilde.
    pub fn new(raw: &Path) -> Self {
        Self(raw.to_path_buf())
    }

    /// The folder with a leading tilde replaced by the home directory.
    pub fn path(&self) -> PathBuf {
        expand_tilde(&self.0)
    }

    /// The text as the file and the dialog hold it. The tilde stays.
    pub fn text(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}

/// The daemon reads this file at start.
///
/// The struct refuses an unknown key. The daemon once read a folder in the
/// remote and synced that folder alone. It now syncs the root, so a file that
/// still carries the old key stops the daemon with a message that names it.
/// A silent start would move a user to a different set of files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub remote: String,
    pub local: LocalDir,
    /// A run that is active finishes. Later runs wait for a resume.
    pub paused: bool,
    /// The longest gap between two runs. The time counts from the end of the
    /// last run, so a slow run does not shorten the gap.
    pub interval_secs: u64,
    /// The quiet time after the last file change. A run starts when no change
    /// arrives for this many seconds.
    pub debounce_secs: u64,
    /// The next run uses the rclone flag --resync. The flag clears only after
    /// a run that ends without an error.
    pub resync_pending: bool,
    /// More rclone flags for every run, for example `--drive-skip-shortcuts`.
    /// A file without the key loads an empty list.
    #[serde(default)]
    pub extra_flags: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            remote: String::from("drive"),
            local: LocalDir::new(Path::new("~/Cloud")),
            paused: false,
            interval_secs: 900,
            debounce_secs: 30,
            resync_pending: true,
            extra_flags: Vec::new(),
        }
    }
}

impl Config {
    /// Builds the rclone target for the root of the remote, in the form
    /// `remote:/`. The daemon syncs the whole remote, so no folder is named.
    pub fn remote_path(&self) -> String {
        format!("{}:/", self.remote)
    }

    /// Rejects settings that the daemon cannot use. The function returns the
    /// first error.
    pub fn check(&self) -> Result<(), Invalid> {
        let refuse = |key, reason: String| Err(Invalid { key, reason });
        if self.remote.is_empty() {
            return refuse(
                "remote",
                String::from(
                    "The remote is empty. Set it to an rclone remote name, for example drive.",
                ),
            );
        }
        if self.interval_secs == 0 {
            return refuse(
                "interval_secs",
                String::from("interval_secs must be above zero."),
            );
        }
        if self.debounce_secs == 0 {
            return refuse(
                "debounce_secs",
                String::from("debounce_secs must be above zero."),
            );
        }
        let local = self.local.path();
        if !local.is_dir() {
            return refuse(
                "local",
                format!("The folder {} does not exist.", local.display()),
            );
        }
        Ok(())
    }
}

/// A key that `check` refuses. The key lets the settings dialog show the
/// reason under the field that holds the key.
#[derive(Debug)]
pub struct Invalid {
    pub key: &'static str,
    pub reason: String,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for Invalid {}

/// Reads the keys that a client may change out of the config.
pub fn settings_of(cfg: &Config) -> Settings {
    Settings {
        remote: cfg.remote.clone(),
        local: cfg.local.text(),
        interval_secs: cfg.interval_secs,
        debounce_secs: cfg.debounce_secs,
        extra_flags: cfg.extra_flags.clone(),
    }
}

/// Copies the keys from a client into the config.
///
/// The pause and the resync flag stay, because the daemon needs both for its
/// own bookkeeping. rclone names its listing after the pair of paths, so a
/// moved remote or folder has no listing and the flag must go up. The
/// comparison uses the folder that rclone opens, because a text change that
/// keeps the folder must not cost a resync.
/// Returns true when the remote or the folder moved.
pub fn apply_settings(cfg: &mut Config, s: &Settings) -> bool {
    let local = LocalDir::new(Path::new(&s.local));
    let moved = cfg.remote != s.remote || cfg.local.path() != local.path();
    if moved {
        cfg.resync_pending = true;
    }
    cfg.remote = s.remote.clone();
    cfg.local = local;
    cfg.interval_secs = s.interval_secs;
    cfg.debounce_secs = s.debounce_secs;
    cfg.extra_flags = s.extra_flags.clone();
    moved
}

/// A test passes a temporary directory.
fn path_in(base: &Path) -> PathBuf {
    base.join("nimbus").join("config.toml")
}

/// The config file at the default location.
pub fn default_file() -> PathBuf {
    path_in(&config_home())
}

/// The working dir that rclone keeps its bisync listing in.
///
/// The dir sits beside the config file, so a test that sets `XDG_CONFIG_HOME`
/// also moves the listing. rclone creates the dir on the first run, and it
/// writes one set of listing files per pair of paths, so the daemon finds the
/// files by pattern instead of building rclone's own file name.
pub fn bisync_dir() -> PathBuf {
    config_home().join("nimbus").join("bisync")
}

/// Writes a new file when the file does not exist.
pub fn load() -> Result<Config> {
    load_from(&default_file())
}

/// A test passes a temporary path.
fn load_from(file: &Path) -> Result<Config> {
    if !file.exists() {
        let cfg = Config::default();
        save_to(&cfg, file)?;
        return Ok(cfg);
    }
    let text =
        std::fs::read_to_string(file).with_context(|| format!("Cannot read {}", file.display()))?;
    match toml::from_str::<Config>(&text) {
        Ok(cfg) => Ok(cfg),
        // A config from an older build carries a key that this build removed,
        // and the default error text only says that a field is missing. The
        // user cannot act on that, so the text names what to do.
        Err(err) => Err(match removed_key(&text) {
            Some(key) => anyhow::anyhow!("The config key {key} is obsolete. Delete it."),
            None => anyhow::anyhow!("Invalid config: {err}"),
        }),
    }
}

/// Returns the first key in the text that the config struct no longer holds.
///
/// The list is a literal, because the struct is the only source of truth and a
/// second list would drift from it.
fn removed_key(text: &str) -> Option<&'static str> {
    ["path"]
        .into_iter()
        .find(|key| text.lines().any(|line| line.starts_with(key)))
}

pub fn save(cfg: &Config) -> Result<()> {
    save_to(cfg, &default_file())
}

/// A test passes a temporary path.
fn save_to(cfg: &Config, file: &Path) -> Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("the daemon cannot create {}", parent.display()))?;
    }
    let text = toml::to_string_pretty(cfg)
        .with_context(|| format!("the daemon cannot format {file:?}"))?;
    std::fs::write(file, text)
        .with_context(|| format!("the daemon cannot write {}", file.display()))
}

/// Replaces a leading ~ with the value of the home directory. The path stays
/// as it is when HOME is not set.
fn expand_tilde(raw: &Path) -> PathBuf {
    let Some(text) = raw.to_str() else {
        return raw.to_path_buf();
    };
    let Some(rest) = text.strip_prefix("~/") else {
        return raw.to_path_buf();
    };
    match std::env::var_os("HOME") {
        Some(home) => Path::new(&home).join(rest),
        None => raw.to_path_buf(),
    }
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_base(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("nimbus-test-{}-{tag}", std::process::id()))
    }

    #[test]
    fn a_local_dir_keeps_an_absolute_path() {
        let dir = LocalDir::new(Path::new("/srv/Nimbus"));
        assert_eq!(dir.path(), Path::new("/srv/Nimbus"));
    }

    #[test]
    fn a_local_dir_keeps_a_relative_path() {
        let dir = LocalDir::new(Path::new("data/Nimbus"));
        assert_eq!(dir.path(), Path::new("data/Nimbus"));
    }

    #[test]
    fn a_local_dir_replaces_the_home_mark() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let dir = LocalDir::new(Path::new("~/Nimbus"));
        assert_eq!(dir.path(), Path::new(&home).join("Nimbus"));
    }

    // The file must keep the tilde, because the user reads and edits the file.
    // rclone needs the real path, because it cannot open one that starts with
    // a tilde. One type keeps the text, and `path` hands out the folder.
    #[test]
    fn the_file_keeps_the_tilde_and_the_path_drops_it() {
        let base = temp_base("tilde");
        let file = path_in(&base);
        let cfg = Config::default();
        save_to(&cfg, &file).expect("the daemon wrote the config file");
        let got = load_from(&file).expect("the daemon read the config file");
        let on_disk = std::fs::read_to_string(&file).expect("the daemon read the config file");
        assert!(
            on_disk.contains("~/Cloud"),
            "the file keeps the tilde for the user"
        );
        assert_eq!(got.local, cfg.local, "the loaded value is the raw text");
        assert_eq!(got.local.path(), cfg.local.path());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_round_trip_keeps_every_field() {
        let base = temp_base("round-trip");
        let file = path_in(&base);
        let want = Config {
            remote: String::from("my-drive"),
            local: LocalDir::new(Path::new("/srv/notes")),
            paused: true,
            interval_secs: 60,
            debounce_secs: 5,
            resync_pending: false,
            extra_flags: vec![String::from("--drive-skip-shortcuts")],
        };
        save_to(&want, &file).expect("the daemon wrote the config file");
        let got = load_from(&file).expect("the daemon read the config file");
        assert_eq!(got, want);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_missing_file_produces_a_new_config() {
        let base = temp_base("missing");
        let file = path_in(&base);
        let cfg = load_from(&file).expect("the daemon created a config file");
        assert!(file.exists(), "the daemon wrote a config file");
        assert!(cfg.resync_pending, "the first run must use --resync");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_new_config_targets_the_root_of_the_remote() {
        let cfg = Config::default();
        assert_eq!(cfg.remote_path(), "drive:/");
    }

    // A removed key must stop the daemon with words the user can act on. A
    // silent start would sync the whole remote, so a user who wanted one
    // folder would find out only after the first run.
    #[test]
    fn a_removed_key_names_itself() {
        let base = temp_base("removed");
        let file = path_in(&base);
        std::fs::create_dir_all(file.parent().expect("the config has a parent"))
            .expect("the daemon created the config dir");
        std::fs::write(
            &file,
            "remote = \"gdrive\"\npath = \"Nimbus\"\nlocal = \"/srv/notes\"\n\
             paused = false\ninterval_secs = 900\ndebounce_secs = 30\nresync_pending = false\n",
        )
        .expect("the daemon wrote the config file");
        let err = load_from(&file).expect_err("the daemon must refuse the old key");
        let text = format!("{err:#}");
        assert!(
            text.contains("path") && text.contains("obsolete"),
            "the message names the key and what happened: {text}"
        );
        assert!(
            text.contains("Delete it"),
            "the message tells the user what to do: {text}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    // A file from a build before the key must still load.
    #[test]
    fn a_file_without_extra_flags_loads() {
        let base = temp_base("no-extra-flags");
        let file = path_in(&base);
        std::fs::create_dir_all(file.parent().expect("the config has a parent"))
            .expect("the daemon created the config dir");
        std::fs::write(
            &file,
            "remote = \"gdrive\"\nlocal = \"/srv/notes\"\n\
             paused = false\ninterval_secs = 900\ndebounce_secs = 30\nresync_pending = false\n",
        )
        .expect("the daemon wrote the config file");
        let cfg = load_from(&file).expect("the daemon read the older file");
        assert!(cfg.extra_flags.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    // A key that the struct still holds must not be reported as removed.
    #[test]
    fn a_current_config_is_not_reported_as_removed() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).expect("the daemon formatted the config");
        assert_eq!(removed_key(&text), None);
    }

    #[test]
    fn check_accepts_an_existing_local_directory() {
        let cfg = Config {
            local: LocalDir::new(&std::env::temp_dir()),
            ..Config::default()
        };
        cfg.check().expect("the default config is valid");
    }

    #[test]
    fn check_rejects_an_empty_remote() {
        let cfg = Config {
            remote: String::new(),
            ..Config::default()
        };
        assert_eq!(
            cfg.check().map_err(|i| i.key),
            Err("remote"),
            "an empty remote is not usable"
        );
    }

    #[test]
    fn check_rejects_a_zero_interval() {
        let cfg = Config {
            interval_secs: 0,
            ..Config::default()
        };
        assert_eq!(
            cfg.check().map_err(|i| i.key),
            Err("interval_secs"),
            "a zero interval starts runs without a pause"
        );
    }

    #[test]
    fn check_rejects_a_zero_debounce() {
        let cfg = Config {
            debounce_secs: 0,
            ..Config::default()
        };
        assert_eq!(
            cfg.check().map_err(|i| i.key),
            Err("debounce_secs"),
            "a zero debounce starts a run for every file change"
        );
    }

    #[test]
    fn check_rejects_a_missing_local_directory() {
        let cfg = Config {
            local: LocalDir::new(Path::new("/nimbus-no-such-directory")),
            ..Config::default()
        };
        // The settings dialog shows the reason under the field with this key.
        assert_eq!(
            cfg.check().map_err(|i| i.key),
            Err("local"),
            "rclone copies nothing without a local directory"
        );
    }

    fn settings() -> Settings {
        Settings {
            remote: String::from("my-drive"),
            local: String::from("/srv/notes"),
            interval_secs: 60,
            debounce_secs: 5,
            extra_flags: vec![String::from("--drive-skip-shortcuts")],
        }
    }

    /// A config that already holds the keys of `settings`, with the daemon
    /// flags in a known state.
    fn settled() -> Config {
        Config {
            remote: String::from("my-drive"),
            local: LocalDir::new(Path::new("/srv/notes")),
            paused: true,
            resync_pending: false,
            interval_secs: 60,
            debounce_secs: 5,
            extra_flags: vec![String::from("--drive-skip-shortcuts")],
        }
    }

    /// The daemon owns the pause and the resync flag. A client that writes
    /// every key would clear a flag that keeps rclone running.
    #[test]
    fn apply_settings_keeps_the_daemon_keys() {
        let mut cfg = settled();
        apply_settings(&mut cfg, &settings());
        assert!(cfg.paused, "the daemon owns the pause");
        assert!(
            !cfg.resync_pending,
            "the same paths keep the listing, so no resync"
        );
        assert_eq!(cfg.remote, "my-drive");
        assert_eq!(cfg.local.text(), "/srv/notes");
        assert_eq!(cfg.interval_secs, 60);
        assert_eq!(cfg.debounce_secs, 5);
        assert_eq!(cfg.extra_flags, ["--drive-skip-shortcuts"]);
    }

    #[test]
    fn a_round_trip_through_settings_keeps_every_key() {
        let mut cfg = settled();
        let read = settings_of(&cfg);
        apply_settings(&mut cfg, &read);
        assert_eq!(settings_of(&cfg), read);
        assert!(cfg.paused, "the pause survived the round trip");
    }

    // rclone names its listing after the pair of paths, so a moved remote or
    // folder has no listing. Without the flag every later run is refused.
    #[test]
    fn a_moved_side_asks_for_a_resync() {
        for moved in [
            Settings {
                remote: String::from("other-drive"),
                ..settings()
            },
            Settings {
                local: String::from("/srv/other"),
                ..settings()
            },
        ] {
            let mut cfg = settled();
            apply_settings(&mut cfg, &moved);
            assert!(
                cfg.resync_pending,
                "{:?} has no listing, so the next run must resync",
                moved
            );
        }
    }

    // The text can name the same folder in two ways. The daemon opens one
    // folder, so a resync would move every file for nothing.
    #[test]
    fn a_rewritten_local_path_keeps_the_resync_flag_down() {
        let mut cfg = settled();
        let same = Settings {
            local: String::from("/srv/notes/"),
            ..settings()
        };
        apply_settings(&mut cfg, &same);
        assert!(
            !cfg.resync_pending,
            "the folder did not move, so the listing still fits"
        );
    }
}
