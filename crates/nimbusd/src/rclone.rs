use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use crate::config::{self, Config};
use crate::engine::Event;

/// How a run ended. The three cases reach the tray as different words, so the
/// run thread names them once instead of passing a bare exit code around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// rclone exited with code zero.
    Success,
    /// rclone exited with this code.
    Failed(i32),
    /// rclone ended with no exit code, for example after a signal.
    Stopped,
}

const PROGRAM: &str = "rclone";

/// The daemon keeps this many error messages for the tray. The oldest message
/// leaves the list when the list is full.
const TAIL: usize = 10;

/// Runs rclone once. The function blocks until the run ends, so the caller
/// runs it on its own thread. It reads the pause flag between output lines,
/// and it stops the process within about one second.
pub fn run(cfg: &Config, pause: Arc<AtomicBool>, events: &Sender<Event>) {
    // A run that died before it finished leaves the listing unusable, and rclone
    // answers such a run with "Must run --resync to recover." The repair runs
    // first, so this run finds the last good listing and stays incremental.
    // When no listing survives, only a resync builds one. A resync waits for
    // the user, so the run stops here unless the engine started it as the
    // confirmed resync.
    if !repair(&config::bisync_dir()) && !cfg.resync_pending {
        let _ = events.send(Event::NeedsResync);
        return;
    }
    let mut child = match command(cfg).spawn() {
        Ok(child) => child,
        Err(err) => {
            let text = format!("the daemon cannot start {PROGRAM}: {err}");
            let _ = events.send(Event::Finished {
                outcome: Outcome::Stopped,
                tail: vec![text],
            });
            return;
        }
    };
    let mut tail: VecDeque<String> = VecDeque::with_capacity(TAIL);
    let errors = child
        .stderr
        .take()
        .expect("the command pipes the error output");
    for line in BufReader::new(errors).lines() {
        let Ok(line) = line else { break };
        if pause.load(Ordering::Relaxed) {
            let _ = child.kill();
            break;
        }
        if let Some(ratio) = handle_line(&line, &mut tail) {
            let _ = events.send(Event::Progress(ratio));
        }
    }
    let outcome = match child.wait().ok().and_then(|status| status.code()) {
        Some(0) => Outcome::Success,
        Some(code) => Outcome::Failed(code),
        None => Outcome::Stopped,
    };
    // A run that died leaves the listing unusable, and the next run would then
    // demand a resync. The repair restores the last good listing first, so the
    // next run is an ordinary incremental run.
    if outcome != Outcome::Success {
        repair(&config::bisync_dir());
    }
    let _ = events.send(Event::Finished {
        outcome,
        tail: tail.into(),
    });
}

/// Restores the listing that a failed run left unusable.
///
/// rclone keeps the listing in the working dir. A failed run replaces the good
/// listing with a `.lst-new` file and leaves a lock behind, so the next run finds
/// no prior state and demands `--resync`. The function keeps the good listing,
/// restores it from the `.lst-old` spare when the run left none, and deletes the
/// debris of the failed run.
///
/// The working dir belongs to one daemon, so every listing in it belongs to one
/// pair of paths. Returns false when no listing survives, because then only
/// `--resync` can build one.
fn repair(workdir: &Path) -> bool {
    let mut listing = false;
    // rclone keeps one file per side, so a pair has two spares. The function
    // must restore both, because rclone refuses a run that has only one.
    let mut spares: Vec<PathBuf> = Vec::new();
    let mut debris = Vec::new();
    let Ok(entries) = std::fs::read_dir(workdir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match path.extension().and_then(|ext| ext.to_str()) {
            Some("lst") => listing = true,
            // The spare holds the state from before the failed run. It becomes
            // the listing only when the failed run left none.
            Some("lst-old") => spares.push(path),
            Some("lst-new" | "lst-err" | "lck") => debris.push(path),
            _ => {}
        }
    }
    if !listing {
        if spares.is_empty() {
            return false;
        }
        let restored = spares
            .iter()
            .all(|spare| std::fs::copy(spare, spare.with_extension("lst")).is_ok());
        if !restored {
            return false;
        }
    }
    for path in debris {
        let _ = std::fs::remove_file(path);
    }
    true
}

/// Reads one line of the error output. Returns the ratio when the line carries
/// a percentage, and keeps the line in the tail when it carries an error.
fn handle_line(line: &str, tail: &mut VecDeque<String>) -> Option<f64> {
    if let Some(text) = parse_error(line) {
        if tail.len() == TAIL {
            tail.pop_front();
        }
        tail.push_back(text);
    }
    parse_progress(line)
}

/// Builds the rclone command for one run. The daemon reads the
/// error output from the process, and it discards the standard output.
fn command(cfg: &Config) -> Command {
    let mut cmd = Command::new(PROGRAM);
    cmd.arg("bisync")
        .arg("--stats")
        .arg("1s")
        .arg("--log-level")
        .arg("INFO")
        // The flag moves the listing out of the rclone cache and into a dir
        // that the daemon owns, so the daemon can repair it after a failed run.
        .arg("--workdir")
        .arg(config::bisync_dir());
    // The flag covers the first run, a moved remote or folder, and a run that
    // found no listing. The engine starts such a run only after the user
    // confirmed it.
    if cfg.resync_pending {
        cmd.arg("--resync");
    }
    // The extra flags come later, so a value there overrides these two.
    cmd.arg("--conflict-resolve")
        .arg(&cfg.conflict_resolve)
        .arg("--conflict-loser")
        .arg(&cfg.conflict_loser)
        .args(&cfg.extra_flags)
        .arg(cfg.local.path())
        .arg(cfg.remote_path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd
}

/// Reads the ratio from one rclone stats line. The ratio is a value from 0.0
/// to 1.0. Returns nothing when the line has no percentage.
fn parse_progress(line: &str) -> Option<f64> {
    let rest = line.strip_prefix("Transferred:")?;
    let (left, _) = rest.split_once("%, ")?;
    left.rsplit(' ')
        .next()?
        .parse::<f64>()
        .ok()
        .map(|percent| percent / 100.0)
}

/// Reads the error text from one rclone log line. Returns nothing when the
/// line has no error.
fn parse_error(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("ERROR : ")?;
    let clean = strip_ansi(rest);
    let text = clean.trim();
    (!text.is_empty()).then(|| String::from(text))
}

// ponytail: this function removes CSI sequences only. rclone does not emit
// another escape sequence. Add OSC support if a backend starts to color a
// path.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        for ch in chars.by_ref() {
            if ch.is_ascii_alphabetic() {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::LocalDir;

    // These lines come from a rclone 1.74.3 run. The byte line and the count
    // line both start with Transferred, and only the byte line has a
    // percentage after the ratio.
    const BYTE_LINE: &str = "Transferred:   \t    1.027 MiB / 19.073 MiB, 5%, 1.027 MiB/s, ETA 17s";
    const COUNT_LINE: &str = "Transferred:            0 / 1, 0%";
    const UNKNOWN_TOTAL_LINE: &str = "Transferred:   \t          0 B / 0 B, -, 0 B/s, ETA -";
    const DONE_LINE: &str =
        "Transferred:   \t   19.073 MiB / 19.073 MiB, 100%, 1.001 MiB/s, ETA 0s";
    const FILE_LINE: &str =
        " *                                      huge.bin:  5% / 19.073 MiB, 1.027 MiB/s, 17s";
    const NOTICE_LINE: &str = "2026/09/29 17:41:41 NOTICE: Initializing bisync v2";
    const ERROR_LINE: &str =
        "2026/09/29 17:41:41 ERROR : Local file system at /nope: directory not found";
    const COLOR_ERROR_LINE: &str =
        "2026/09/29 17:41:41 ERROR : \x1b[31mBisync critical error: directory not found\x1b[0m";

    fn error(text: &str) -> String {
        format!("2026/09/29 17:41:41 ERROR : {text}")
    }

    /// A working dir under the temp dir, so a test never touches the real one.
    fn workdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nimbus-test-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the test created the working dir");
        dir
    }

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).expect("the test wrote the file");
    }

    fn read(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).expect("the test read the file")
    }

    fn exists(dir: &Path, name: &str) -> bool {
        dir.join(name).exists()
    }

    fn gone(dir: &Path, name: &str) -> bool {
        !exists(dir, name)
    }

    fn args_of(cfg: &Config) -> Vec<String> {
        command(cfg)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn the_command_uses_bisync() {
        let args = args_of(&Config::default());
        assert_eq!(args[0], "bisync");
    }

    // This test records a finding from a live run. The rclone build emits
    // stats only at the INFO level, and a pipe suppresses the default level.
    // Without both flags the progress reader receives nothing at all.
    #[test]
    fn the_command_asks_for_stats_at_the_info_level() {
        let args = args_of(&Config::default());
        let stats = args.iter().position(|arg| arg == "--stats");
        assert_eq!(
            stats.map(|i| args[i + 1].as_str()),
            Some("1s"),
            "the stats flag is missing"
        );
        let level = args.iter().position(|arg| arg == "--log-level");
        assert_eq!(
            level.map(|i| args[i + 1].as_str()),
            Some("INFO"),
            "the log level is missing"
        );
    }

    #[test]
    fn the_command_adds_resync_on_the_first_run() {
        let args = args_of(&Config {
            resync_pending: true,
            ..Config::default()
        });
        assert!(
            args.iter().any(|arg| arg == "--resync"),
            "the first run must use --resync"
        );
    }

    #[test]
    fn the_command_omits_resync_after_the_first_run() {
        let args = args_of(&Config {
            resync_pending: false,
            ..Config::default()
        });
        assert!(
            !args.iter().any(|arg| arg == "--resync"),
            "a later run must not resync"
        );
    }

    #[test]
    fn the_command_names_the_local_folder_and_the_remote() {
        let cfg = Config {
            remote: String::from("gdrive"),
            local: LocalDir::new(Path::new("/home/luck/Nimbus")),
            ..Config::default()
        };
        let args = args_of(&cfg);
        assert_eq!(args[args.len() - 2], "/home/luck/Nimbus");
        assert_eq!(args[args.len() - 1], "gdrive:/");
    }

    #[test]
    fn the_command_passes_the_extra_flags_before_the_paths() {
        let cfg = Config {
            extra_flags: vec![
                String::from("--drive-skip-shortcuts"),
                String::from("--drive-acknowledge-abuse"),
            ],
            ..Config::default()
        };
        let args = args_of(&cfg);
        let n = args.len();
        assert_eq!(
            args[n - 4..n - 2],
            ["--drive-skip-shortcuts", "--drive-acknowledge-abuse"]
        );
    }

    #[test]
    fn the_command_passes_the_conflict_choice() {
        let cfg = Config {
            conflict_resolve: String::from("path2"),
            conflict_loser: String::from("num"),
            ..Config::default()
        };
        let args = args_of(&cfg);
        let at = |flag: &str| {
            let i = args.iter().position(|arg| arg == flag).expect(flag);
            args[i + 1].clone()
        };
        assert_eq!(at("--conflict-resolve"), "path2");
        assert_eq!(at("--conflict-loser"), "num");
    }

    // rclone cannot open a path that starts with a tilde, so the command must
    // carry the expanded folder.
    #[test]
    fn the_command_expands_the_home_mark() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let cfg = Config {
            local: LocalDir::new(Path::new("~/Nimbus")),
            ..Config::default()
        };
        let args = args_of(&cfg);
        assert_eq!(
            args[args.len() - 2],
            Path::new(&home).join("Nimbus").to_str().unwrap()
        );
    }

    #[test]
    fn parse_progress_reads_the_byte_line() {
        assert_eq!(parse_progress(BYTE_LINE), Some(0.05));
    }

    #[test]
    fn parse_progress_reads_a_finished_run() {
        assert_eq!(parse_progress(DONE_LINE), Some(1.0));
    }

    #[test]
    fn parse_progress_ignores_the_file_count_line() {
        assert_eq!(parse_progress(COUNT_LINE), None);
    }

    #[test]
    fn parse_progress_ignores_an_unknown_total() {
        assert_eq!(parse_progress(UNKNOWN_TOTAL_LINE), None);
    }

    #[test]
    fn parse_progress_ignores_a_file_line() {
        assert_eq!(parse_progress(FILE_LINE), None);
    }

    #[test]
    fn parse_progress_ignores_a_log_line() {
        assert_eq!(parse_progress(NOTICE_LINE), None);
    }

    #[test]
    fn parse_progress_ignores_an_empty_line() {
        assert_eq!(parse_progress(""), None);
    }

    #[test]
    fn parse_error_removes_the_time_and_the_level() {
        assert_eq!(
            parse_error(ERROR_LINE).as_deref(),
            Some("Local file system at /nope: directory not found")
        );
    }

    #[test]
    fn parse_error_removes_the_color_codes() {
        assert_eq!(
            parse_error(COLOR_ERROR_LINE).as_deref(),
            Some("Bisync critical error: directory not found")
        );
    }

    #[test]
    fn parse_error_ignores_a_notice_line() {
        assert_eq!(parse_error(NOTICE_LINE), None);
    }

    // Without the flag the listing stays in the rclone cache, where the daemon
    // cannot repair it after a failed run.
    #[test]
    fn the_command_points_the_listing_at_the_bisync_dir() {
        let args = args_of(&Config::default());
        let at = args.iter().position(|arg| arg == "--workdir");
        let want = config::bisync_dir();
        assert_eq!(
            at.map(|i| PathBuf::from(&args[i + 1])),
            Some(want),
            "the daemon must own the dir that holds the listing"
        );
        assert!(
            !args.iter().any(|arg| arg == "--resilient"),
            "the flag lies about a retryable error"
        );
        assert!(
            !args.iter().any(|arg| arg == "--recover"),
            "the flag does not recover a lost listing"
        );
    }

    #[test]
    fn a_good_listing_survives_and_the_debris_goes() {
        let dir = workdir("good-listing");
        write(&dir, "pair.path1.lst", "one");
        write(&dir, "pair.path2.lst", "two");
        write(&dir, "pair.path1.lst-new", "half");
        write(&dir, "pair.path2.lst-err", "boom");
        write(&dir, "pair.lck", "lock");
        assert!(repair(&dir), "the good listing is enough");
        assert_eq!(read(&dir, "pair.path1.lst"), "one");
        assert_eq!(read(&dir, "pair.path2.lst"), "two");
        assert!(gone(&dir, "pair.path1.lst-new"), "the debris must go");
        assert!(gone(&dir, "pair.path2.lst-err"), "the debris must go");
        assert!(gone(&dir, "pair.lck"), "the stale lock must go");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A failed run can leave no listing at all, and only the spare holds the
    // state from before the run.
    #[test]
    fn the_spare_restores_a_missing_listing() {
        let dir = workdir("spare");
        write(&dir, "pair.path1.lst-old", "one");
        write(&dir, "pair.path2.lst-old", "two");
        write(&dir, "pair.path1.lst-new", "half");
        assert!(repair(&dir), "the spare is a listing");
        assert_eq!(read(&dir, "pair.path1.lst"), "one");
        assert_eq!(read(&dir, "pair.path2.lst"), "two");
        assert!(gone(&dir, "pair.path1.lst-new"), "the debris must go");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A present listing is newer than the spare, so the spare stays a spare.
    #[test]
    fn a_present_listing_wins_over_the_spare() {
        let dir = workdir("newer");
        write(&dir, "pair.path1.lst", "current");
        write(&dir, "pair.path1.lst-old", "older");
        assert!(repair(&dir));
        assert_eq!(read(&dir, "pair.path1.lst"), "current");
        assert!(
            exists(&dir, "pair.path1.lst-old"),
            "the spare stays as a backup"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // No listing and no spare leaves nothing to compare against. Only a resync
    // can rebuild the state, so the function reports false.
    #[test]
    fn no_listing_and_no_spare_needs_a_resync() {
        let dir = workdir("empty");
        write(&dir, "pair.path1.lst-new", "half");
        write(&dir, "pair.lck", "lock");
        assert!(!repair(&dir), "only --resync can rebuild this");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_workdir_needs_a_resync() {
        let dir = workdir("gone");
        assert!(!repair(&dir), "the dir does not exist");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The lock belongs to the working dir that one daemon owns, so a lock in it
    // can only be the lock of a run that died.
    #[test]
    fn a_partial_file_is_not_touched() {
        let dir = workdir("partial");
        write(&dir, "pair.path1.lst", "one");
        write(&dir, "pair.path1.lst-new", "half");
        write(&dir, "big.bin.7f415a38.partial", "in flight");
        assert!(repair(&dir));
        assert!(
            exists(&dir, "big.bin.7f415a38.partial"),
            "rclone owns the partial file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_ansi_removes_an_escape_sequence() {
        assert_eq!(strip_ansi("\x1b[31mboom\x1b[0m"), "boom");
    }

    #[test]
    fn strip_ansi_keeps_plain_text() {
        assert_eq!(strip_ansi("plain text"), "plain text");
        assert_eq!(strip_ansi(""), "");
    }

    // The tray shows one error line, so the tail must drop the oldest message
    // when it is full.
    #[test]
    fn the_tail_keeps_the_last_ten_errors() {
        let mut tail = VecDeque::new();
        for n in 0..TAIL + 5 {
            handle_line(&error(&format!("error {n}")), &mut tail);
        }
        assert_eq!(tail.len(), TAIL, "the tail must not grow past the limit");
        assert_eq!(tail.front().map(String::as_str), Some("error 5"));
        assert_eq!(tail.back().map(String::as_str), Some("error 14"));
    }

    #[test]
    fn a_stats_line_carries_no_error() {
        let mut tail = VecDeque::new();
        assert_eq!(handle_line(BYTE_LINE, &mut tail), Some(0.05));
        assert!(tail.is_empty(), "a progress line must not reach the tray");
    }
}
