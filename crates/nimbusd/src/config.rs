use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub remote: String,
    /// The folder in the remote.
    pub path: String,
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            remote: String::from("gdrive"),
            path: String::from("Nimbus"),
            local: LocalDir::new(Path::new("~/Nimbus")),
            paused: false,
            interval_secs: 900,
            debounce_secs: 30,
            resync_pending: true,
        }
    }
}

impl Config {
    /// Builds the rclone target, in the form remote:path.
    pub fn remote_path(&self) -> String {
        format!("{}:{}", self.remote, self.path)
    }

    /// Rejects settings that the daemon cannot use. The function returns the
    /// first error.
    pub fn check(&self) -> Result<()> {
        ensure!(
            !self.remote.is_empty(),
            "the config key remote is empty. Set a remote name."
        );
        ensure!(
            !self.path.is_empty(),
            "the config key path is empty. Set a folder name."
        );
        ensure!(
            self.interval_secs > 0,
            "the config key interval_secs is zero. Set a time above zero."
        );
        ensure!(
            self.debounce_secs > 0,
            "the config key debounce_secs is zero. Set a time above zero."
        );
        let local = self.local.path();
        ensure!(
            local.is_dir(),
            "the local directory {} does not exist. Create the directory, or set the config key local to a directory that exists.",
            local.display()
        );
        Ok(())
    }
}

/// Reads the keys that a client may change out of the config.
pub fn settings_of(cfg: &Config) -> Settings {
    Settings {
        remote: cfg.remote.clone(),
        path: cfg.path.clone(),
        local: cfg.local.text(),
        interval_secs: cfg.interval_secs,
        debounce_secs: cfg.debounce_secs,
    }
}

/// Copies the keys from a client into the config.
///
/// The pause and the resync flag stay, because the daemon needs both for its
/// own bookkeeping. rclone names its listing after the pair of paths, so a
/// moved path has no listing and the flag must go up. The comparison uses the
/// folder that rclone opens, because a text change that keeps the folder must
/// not cost a resync.
pub fn apply_settings(cfg: &mut Config, s: &Settings) {
    let local = LocalDir::new(Path::new(&s.local));
    let moved = cfg.remote != s.remote || cfg.path != s.path || cfg.local.path() != local.path();
    if moved {
        cfg.resync_pending = true;
    }
    cfg.remote = s.remote.clone();
    cfg.path = s.path.clone();
    cfg.local = local;
    cfg.interval_secs = s.interval_secs;
    cfg.debounce_secs = s.debounce_secs;
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
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("the daemon cannot read {}", file.display()))?;
    toml::from_str(&text).with_context(|| format!("the daemon cannot parse {}", file.display()))
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
            on_disk.contains("~/Nimbus"),
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
            path: String::from("Notes/Daily"),
            local: LocalDir::new(Path::new("/srv/notes")),
            paused: true,
            interval_secs: 60,
            debounce_secs: 5,
            resync_pending: false,
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
    fn a_new_config_reports_the_remote_path() {
        let cfg = Config::default();
        assert_eq!(cfg.remote_path(), "gdrive:Nimbus");
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
        assert!(cfg.check().is_err(), "an empty remote is not usable");
    }

    #[test]
    fn check_rejects_a_zero_interval() {
        let cfg = Config {
            interval_secs: 0,
            ..Config::default()
        };
        assert!(
            cfg.check().is_err(),
            "a zero interval starts runs without a pause"
        );
    }

    #[test]
    fn check_rejects_a_zero_debounce() {
        let cfg = Config {
            debounce_secs: 0,
            ..Config::default()
        };
        assert!(
            cfg.check().is_err(),
            "a zero debounce starts a run for every file change"
        );
    }

    #[test]
    fn check_rejects_a_missing_local_directory() {
        let cfg = Config {
            local: LocalDir::new(Path::new("/nimbus-no-such-directory")),
            ..Config::default()
        };
        assert!(
            cfg.check().is_err(),
            "rclone copies nothing without a local directory"
        );
    }

    fn settings() -> Settings {
        Settings {
            remote: String::from("my-drive"),
            path: String::from("Notes"),
            local: String::from("/srv/notes"),
            interval_secs: 60,
            debounce_secs: 5,
        }
    }

    /// A config that already holds the keys of `settings`, with the daemon
    /// flags in a known state.
    fn settled() -> Config {
        Config {
            remote: String::from("my-drive"),
            path: String::from("Notes"),
            local: LocalDir::new(Path::new("/srv/notes")),
            paused: true,
            resync_pending: false,
            interval_secs: 60,
            debounce_secs: 5,
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
        assert_eq!(cfg.path, "Notes");
        assert_eq!(cfg.local.text(), "/srv/notes");
        assert_eq!(cfg.interval_secs, 60);
        assert_eq!(cfg.debounce_secs, 5);
    }

    #[test]
    fn a_round_trip_through_settings_keeps_every_key() {
        let mut cfg = settled();
        let read = settings_of(&cfg);
        apply_settings(&mut cfg, &read);
        assert_eq!(settings_of(&cfg), read);
        assert!(cfg.paused, "the pause survived the round trip");
    }

    // rclone names its listing after the pair of paths, so a moved path has no
    // listing. Without the flag every later run is refused.
    #[test]
    fn a_moved_path_asks_for_a_resync() {
        for moved in [
            Settings {
                remote: String::from("other-drive"),
                ..settings()
            },
            Settings {
                path: String::from("Other"),
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
