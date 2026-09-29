use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use crate::config::Config;
use crate::engine::Event;

/// The name of the rclone program.
const PROGRAM: &str = "rclone";

/// The daemon keeps this many error messages for the tray. The oldest message
/// leaves the list when the list is full.
const TAIL: usize = 10;

/// Runs rclone once. The function blocks until the run ends, so the caller
/// runs it on its own thread. It reads the pause flag between output lines,
/// and it stops the process within about one second.
pub fn run(cfg: &Config, pause: Arc<AtomicBool>, events: &Sender<Event>) {
    let mut child = match command(cfg).spawn() {
        Ok(child) => child,
        Err(err) => {
            let text = format!("the daemon cannot start {PROGRAM}: {err}");
            let _ = events.send(Event::Finished {
                code: None,
                tail: vec![text],
            });
            return;
        }
    };
    let mut tail: VecDeque<String> = VecDeque::with_capacity(TAIL);
    // The command pipes the error output, so the read cannot fail here.
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
        if let Some(ratio) = parse_progress(&line) {
            let _ = events.send(Event::Progress(ratio));
        }
        if let Some(text) = parse_error(&line) {
            if tail.len() == TAIL {
                tail.pop_front();
            }
            tail.push_back(text);
        }
    }
    let code = child.wait().ok().and_then(|status| status.code());
    let _ = events.send(Event::Finished {
        code,
        tail: tail.into(),
    });
}

/// Builds the rclone command for one run. The daemon reads the
/// error output from the process, and it discards the standard output.
pub fn command(cfg: &Config) -> Command {
    let mut cmd = Command::new(PROGRAM);
    cmd.arg("bisync")
        .arg("--stats")
        .arg("1s")
        .arg("--log-level")
        .arg("INFO");
    if cfg.resync_pending {
        cmd.arg("--resync");
    }
    cmd.arg(&cfg.local)
        .arg(cfg.remote_path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd
}

/// Reads the ratio from one rclone stats line. The ratio is a value from 0.0
/// to 1.0. Returns nothing when the line has no percentage.
pub fn parse_progress(line: &str) -> Option<f64> {
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
pub fn parse_error(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("ERROR : ")?;
    let clean = strip_ansi(rest);
    let text = clean.trim();
    (!text.is_empty()).then(|| String::from(text))
}

// ponytail: this function removes CSI sequences only. rclone does not emit
// another escape sequence. Add OSC support if a backend starts to color a
// path.
pub fn strip_ansi(text: &str) -> String {
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
    use std::path::PathBuf;

    use super::*;

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
            path: String::from("Nimbus"),
            local: PathBuf::from("/home/luck/Nimbus"),
            ..Config::default()
        };
        let args = args_of(&cfg);
        assert_eq!(args[args.len() - 2], "/home/luck/Nimbus");
        assert_eq!(args[args.len() - 1], "gdrive:Nimbus");
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

    #[test]
    fn strip_ansi_removes_an_escape_sequence() {
        assert_eq!(strip_ansi("\x1b[31mboom\x1b[0m"), "boom");
    }

    #[test]
    fn strip_ansi_keeps_plain_text() {
        assert_eq!(strip_ansi("plain text"), "plain text");
        assert_eq!(strip_ansi(""), "");
    }
}
