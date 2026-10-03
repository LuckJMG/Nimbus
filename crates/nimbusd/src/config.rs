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

/// The value for `--conflict-resolve`. `None` keeps both copies, and the other
/// values name the copy that wins. `as_str` is the only spelling, and serde
/// reads and writes through it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(try_from = "String", into = "&'static str")]
pub enum ConflictResolve {
    None,
    #[default]
    Newer,
    Older,
    Larger,
    Smaller,
    Path1,
    Path2,
}

impl ConflictResolve {
    pub const ALL: [Self; 7] = [
        Self::None,
        Self::Newer,
        Self::Older,
        Self::Larger,
        Self::Smaller,
        Self::Path1,
        Self::Path2,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Newer => "newer",
            Self::Older => "older",
            Self::Larger => "larger",
            Self::Smaller => "smaller",
            Self::Path1 => "path1",
            Self::Path2 => "path2",
        }
    }
}

impl From<ConflictResolve> for &'static str {
    fn from(value: ConflictResolve) -> Self {
        value.as_str()
    }
}

impl TryFrom<String> for ConflictResolve {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        choose(&Self::ALL, Self::as_str, "conflict_resolve", &text)
    }
}

/// The value for `--conflict-loser`. A live run on rclone 1.74.3 measured
/// `Delete`: it removes the losing copy for good. With `ConflictResolve::None`
/// there is no loser, so rclone keeps both copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(try_from = "String", into = "&'static str")]
pub enum ConflictLoser {
    Num,
    Pathname,
    #[default]
    Delete,
}

impl ConflictLoser {
    pub const ALL: [Self; 3] = [Self::Num, Self::Pathname, Self::Delete];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Num => "num",
            Self::Pathname => "pathname",
            Self::Delete => "delete",
        }
    }
}

impl From<ConflictLoser> for &'static str {
    fn from(value: ConflictLoser) -> Self {
        value.as_str()
    }
}

impl TryFrom<String> for ConflictLoser {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        choose(&Self::ALL, Self::as_str, "conflict_loser", &text)
    }
}

/// Finds the value whose spelling is `text`. The error names the key and
/// every spelling that the key takes.
fn choose<T: Copy>(
    all: &[T],
    spell: fn(T) -> &'static str,
    key: &str,
    text: &str,
) -> Result<T, String> {
    all.iter()
        .copied()
        .find(|value| spell(*value) == text)
        .ok_or_else(|| {
            let spellings: Vec<&str> = all.iter().map(|value| spell(*value)).collect();
            format!("{key} must be one of {}.", spellings.join(", "))
        })
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
    /// The copy that wins when a file changed on both sides. A file without
    /// the key loads `newer`.
    #[serde(default)]
    pub conflict_resolve: ConflictResolve,
    /// What happens to the copy that lost. A file without the key loads
    /// `delete`.
    #[serde(default)]
    pub conflict_loser: ConflictLoser,
    /// More rclone flags for every run, for example `--drive-skip-shortcuts`.
    /// A file without the key loads an empty list.
    #[serde(default)]
    pub extra_flags: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            remote: String::from("drive:/"),
            local: LocalDir::new(Path::new("~/Cloud")),
            paused: false,
            interval_secs: 900,
            debounce_secs: 30,
            resync_pending: true,
            conflict_resolve: ConflictResolve::default(),
            conflict_loser: ConflictLoser::default(),
            extra_flags: Vec::new(),
        }
    }
}

/// A key that the daemon can refuse. The settings dialog shows the reason
/// under the field that holds the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Remote,
    Local,
    IntervalSecs,
    DebounceSecs,
    ConflictResolve,
    ConflictLoser,
}

impl Config {
    /// Rejects settings that the daemon cannot use. The function returns the
    /// first error.
    pub fn check(&self) -> Result<(), Invalid> {
        let refuse = |key, reason: String| Err(Invalid { key, reason });
        if self.remote.is_empty() {
            return refuse(
                Key::Remote,
                String::from(
                    "The remote is empty. Set it to an rclone remote name, for example drive.",
                ),
            );
        }
        if self.interval_secs == 0 {
            return refuse(
                Key::IntervalSecs,
                String::from("interval_secs must be above zero."),
            );
        }
        if self.debounce_secs == 0 {
            return refuse(
                Key::DebounceSecs,
                String::from("debounce_secs must be above zero."),
            );
        }
        let local = self.local.path();
        if !local.is_dir() {
            return refuse(
                Key::Local,
                format!("The folder {} does not exist.", local.display()),
            );
        }
        Ok(())
    }

    /// Reads the keys that a client may change.
    pub fn settings(&self) -> Settings {
        Settings {
            remote: self.remote.clone(),
            local: self.local.text(),
            interval_secs: self.interval_secs,
            debounce_secs: self.debounce_secs,
            conflict_resolve: String::from(self.conflict_resolve.as_str()),
            conflict_loser: String::from(self.conflict_loser.as_str()),
            extra_flags: self.extra_flags.clone(),
        }
    }

    /// Copies the keys from a client into the config.
    ///
    /// The wire carries the conflict values as text, so an unknown value is
    /// refused before any key changes. The pause and the resync flag stay,
    /// because the daemon needs both for its own bookkeeping. rclone names its
    /// listing after the pair of paths, so a moved remote or folder has no
    /// listing and the flag must go up. The comparison uses the folder that
    /// rclone opens, because a text change that keeps the folder must not cost
    /// a resync.
    /// Returns true when the remote or the folder moved.
    pub fn apply(&mut self, settings: &Settings) -> Result<bool, Invalid> {
        let conflict_resolve = ConflictResolve::try_from(settings.conflict_resolve.clone())
            .map_err(|reason| Invalid {
                key: Key::ConflictResolve,
                reason,
            })?;
        let conflict_loser =
            ConflictLoser::try_from(settings.conflict_loser.clone()).map_err(|reason| Invalid {
                key: Key::ConflictLoser,
                reason,
            })?;
        let local = LocalDir::new(Path::new(&settings.local));
        let moved =
            target(&self.remote) != target(&settings.remote) || self.local.path() != local.path();
        if moved {
            self.resync_pending = true;
        }
        self.remote = settings.remote.clone();
        self.local = local;
        self.interval_secs = settings.interval_secs;
        self.debounce_secs = settings.debounce_secs;
        self.conflict_resolve = conflict_resolve;
        self.conflict_loser = conflict_loser;
        self.extra_flags = settings.extra_flags.clone();
        Ok(moved)
    }
}

/// A key that the daemon refuses, with the reason for the user.
#[derive(Debug)]
pub struct Invalid {
    pub key: Key,
    pub reason: String,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for Invalid {}

/// Builds the rclone target for a remote text. A bare name targets the root
/// of the remote, written `name:/`. A text with a colon already names a
/// folder, as in `gdrive:/Photos`, so it passes unchanged. The text is not
/// normalized, because `name:path` and `name:/path` differ on a `local` remote.
pub fn target(remote: &str) -> String {
    if remote.contains(':') {
        remote.to_string()
    } else {
        format!("{remote}:/")
    }
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

/// Writes a new file when the file does not exist. Only the daemon calls
/// this, because the daemon is the only writer.
pub fn load() -> Result<Config> {
    load_from(&default_file())
}

/// Reads the file without writing it. Returns `None` when the file does not
/// exist, so a client can read the keys while no daemon runs.
pub fn read() -> Result<Option<Config>> {
    read_from(&default_file())
}

/// A test passes a temporary path.
fn load_from(file: &Path) -> Result<Config> {
    if let Some(cfg) = read_from(file)? {
        return Ok(cfg);
    }
    let cfg = Config::default();
    save_to(&cfg, file)?;
    Ok(cfg)
}

/// A test passes a temporary path.
fn read_from(file: &Path) -> Result<Option<Config>> {
    if !file.exists() {
        return Ok(None);
    }
    let text =
        std::fs::read_to_string(file).with_context(|| format!("Cannot read {}", file.display()))?;
    // A key that this build removed fails as an unknown field, and serde names
    // the key in the error text.
    toml::from_str::<Config>(&text)
        .map(Some)
        .map_err(|err| anyhow::anyhow!("Invalid config: {err}"))
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
            conflict_resolve: ConflictResolve::Path2,
            conflict_loser: ConflictLoser::Num,
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
        assert_eq!(target(&Config::default().remote), "drive:/");
    }

    #[test]
    fn a_remote_with_a_folder_targets_that_folder() {
        assert_eq!(target("gdrive:/Photos"), "gdrive:/Photos");
        assert_eq!(target("gdrive:Photos"), "gdrive:Photos");
        assert_eq!(target("gdrive"), "gdrive:/");
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
            text.contains("unknown field `path`"),
            "the message names the key: {text}"
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
        assert_eq!(cfg.conflict_resolve, ConflictResolve::Newer);
        assert_eq!(cfg.conflict_loser, ConflictLoser::Delete);
        let _ = std::fs::remove_dir_all(&base);
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
            Err(Key::Remote),
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
            Err(Key::IntervalSecs),
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
            Err(Key::DebounceSecs),
            "a zero debounce starts a run for every file change"
        );
    }

    // The wire carries the conflict values as text, so `apply` refuses an
    // unknown one and leaves every key as it was.
    #[test]
    fn apply_rejects_an_unknown_conflict_value() {
        let mut cfg = settled();
        let latest = Settings {
            conflict_resolve: String::from("latest"),
            ..settings()
        };
        assert_eq!(
            cfg.apply(&latest).map_err(|i| i.key),
            Err(Key::ConflictResolve)
        );
        let drop = Settings {
            conflict_loser: String::from("drop"),
            ..settings()
        };
        assert_eq!(cfg.apply(&drop).map_err(|i| i.key), Err(Key::ConflictLoser));
        assert_eq!(cfg, settled(), "a refused save changes nothing");
    }

    // Serde reads the file through `as_str`, so the two spellings cannot drift.
    #[test]
    fn every_conflict_value_reads_back_from_its_spelling() {
        for value in ConflictResolve::ALL {
            assert_eq!(
                ConflictResolve::try_from(String::from(value.as_str())),
                Ok(value)
            );
        }
        for value in ConflictLoser::ALL {
            assert_eq!(
                ConflictLoser::try_from(String::from(value.as_str())),
                Ok(value)
            );
        }
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
            Err(Key::Local),
            "rclone copies nothing without a local directory"
        );
    }

    fn settings() -> Settings {
        Settings {
            remote: String::from("my-drive"),
            local: String::from("/srv/notes"),
            interval_secs: 60,
            debounce_secs: 5,
            conflict_resolve: String::from("path1"),
            conflict_loser: String::from("pathname"),
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
            conflict_resolve: ConflictResolve::Newer,
            conflict_loser: ConflictLoser::Delete,
            extra_flags: vec![String::from("--drive-skip-shortcuts")],
        }
    }

    /// The daemon owns the pause and the resync flag. A client that writes
    /// every key would clear a flag that keeps rclone running.
    #[test]
    fn apply_keeps_the_daemon_keys() {
        let mut cfg = settled();
        cfg.apply(&settings()).expect("the keys are valid");
        assert!(cfg.paused, "the daemon owns the pause");
        assert!(
            !cfg.resync_pending,
            "the same paths keep the listing, so no resync"
        );
        assert_eq!(cfg.remote, "my-drive");
        assert_eq!(cfg.local.text(), "/srv/notes");
        assert_eq!(cfg.interval_secs, 60);
        assert_eq!(cfg.debounce_secs, 5);
        assert_eq!(cfg.conflict_resolve, ConflictResolve::Path1);
        assert_eq!(cfg.conflict_loser, ConflictLoser::Pathname);
        assert_eq!(cfg.extra_flags, ["--drive-skip-shortcuts"]);
    }

    #[test]
    fn a_round_trip_through_settings_keeps_every_key() {
        let mut cfg = settled();
        let read = cfg.settings();
        cfg.apply(&read).expect("the keys are valid");
        assert_eq!(cfg.settings(), read);
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
            cfg.apply(&moved).expect("the keys are valid");
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
        cfg.apply(&same).expect("the keys are valid");
        assert!(
            !cfg.resync_pending,
            "the folder did not move, so the listing still fits"
        );
    }
}
