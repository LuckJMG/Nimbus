use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// The daemon reads this file at start.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub remote: String,
    /// The folder in the remote.
    pub path: String,
    /// The local folder.
    pub local: PathBuf,
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
            local: PathBuf::from("~/Nimbus"),
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
        ensure!(
            self.local.is_dir(),
            "the local directory {} does not exist. Create the directory, or set the config key local to a directory that exists.",
            self.local.display()
        );
        Ok(())
    }
}

/// A test passes a temporary directory.
pub fn path_in(base: &Path) -> PathBuf {
    base.join("nimbus").join("config.toml")
}

/// The daemon prints this path when it cannot use the settings.
pub fn path() -> PathBuf {
    path_in(&config_home())
}

/// Writes a new file when the file does not exist.
pub fn load() -> Result<Config> {
    load_from(&path_in(&config_home()))
}

/// A test passes a temporary path.
pub fn load_from(file: &Path) -> Result<Config> {
    if !file.exists() {
        let mut cfg = Config::default();
        // The file keeps the tilde, because the user reads and edits it. The
        // copy in memory holds the real path, because rclone cannot open a
        // path that starts with a tilde.
        save_to(&cfg, file)?;
        cfg.local = expand_tilde(&cfg.local);
        return Ok(cfg);
    }
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("the daemon cannot read {}", file.display()))?;
    let mut cfg: Config = toml::from_str(&text)
        .with_context(|| format!("the daemon cannot parse {}", file.display()))?;
    cfg.local = expand_tilde(&cfg.local);
    Ok(cfg)
}

pub fn save(cfg: &Config) -> Result<()> {
    save_to(cfg, &path_in(&config_home()))
}

/// A test passes a temporary path.
pub fn save_to(cfg: &Config, file: &Path) -> Result<()> {
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
pub fn expand_tilde(raw: &Path) -> PathBuf {
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
    fn expand_tilde_keeps_an_absolute_path() {
        assert_eq!(
            expand_tilde(Path::new("/srv/Nimbus")),
            Path::new("/srv/Nimbus")
        );
    }

    #[test]
    fn expand_tilde_keeps_a_relative_path() {
        assert_eq!(
            expand_tilde(Path::new("data/Nimbus")),
            Path::new("data/Nimbus")
        );
    }

    #[test]
    fn expand_tilde_replaces_the_home_mark() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        assert_eq!(
            expand_tilde(Path::new("~/Nimbus")),
            Path::new(&home).join("Nimbus")
        );
    }

    #[test]
    fn a_round_trip_keeps_every_field() {
        let base = temp_base("round-trip");
        let file = path_in(&base);
        let want = Config {
            remote: String::from("my-drive"),
            path: String::from("Notes/Daily"),
            local: PathBuf::from("/srv/notes"),
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

    // The default holds a tilde. A daemon that keeps the tilde reports a path
    // that does not exist, and rclone receives a path it cannot open.
    #[test]
    fn a_new_config_expands_the_home_mark() {
        let base = temp_base("expand-new");
        let file = path_in(&base);
        let cfg = load_from(&file).expect("the daemon created a config file");
        assert_eq!(cfg.local, expand_tilde(Path::new("~/Nimbus")));
        let on_disk = std::fs::read_to_string(&file).expect("the daemon read the config file");
        assert!(
            on_disk.contains("~/Nimbus"),
            "the file keeps the tilde for the user"
        );
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
            local: std::env::temp_dir(),
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
            local: PathBuf::from("/nimbus-no-such-directory"),
            ..Config::default()
        };
        assert!(
            cfg.check().is_err(),
            "rclone copies nothing without a local directory"
        );
    }
}
