use std::sync::mpsc::Sender;
use std::time::{SystemTime, UNIX_EPOCH};

use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, Button, HeaderBar, Label, Orientation, ProgressBar, Window};

use crate::tray::{Action, View, is_paused, status_name};

/// States the last run in words. An absolute timestamp needs a date library
/// and a time zone, and the tray has no room for it either way.
pub fn ago(seconds: u64) -> String {
    let (value, unit) = match seconds {
        0..=59 => return String::from("just now"),
        60..=3599 => (seconds / 60, "minute"),
        3600..=86_399 => (seconds / 3600, "hour"),
        _ => (seconds / 86_400, "day"),
    };
    let suffix = if value == 1 { "" } else { "s" };
    format!("{value} {unit}{suffix} ago")
}

/// States how long ago the last run ended. A clock that moved backwards gives
/// a difference of zero, which reads as "just now".
pub fn ago_since(unix: u64, now: u64) -> String {
    if unix == 0 {
        return String::from("no run yet");
    }
    ago(now.saturating_sub(unix))
}

pub fn pause_label(view: &View) -> &'static str {
    if is_paused(view) { "Resume" } else { "Pause" }
}

/// The error line, or `None` when the line must hide itself. A hidden line
/// leaves no gap, so a run that succeeds leaves no scar.
pub fn error_line(view: &View) -> Option<&str> {
    match view {
        View::Ready(state) if !state.last_error.is_empty() => Some(&state.last_error),
        _ => None,
    }
}

pub struct App {
    pub root: Window,
    phase: Label,
    progress: ProgressBar,
    last_run: Label,
    error: Label,
    pause: Button,
}

impl App {
    pub fn present(&self) {
        self.root.present();
    }

    /// Rewrites every field. The caller checks for a change first, so this runs
    /// only when the view moved. The heading takes the short name, because the
    /// error line below it carries the message from the daemon.
    pub fn apply(&self, view: &View, now: u64) {
        self.phase.set_text(&status_name(view));
        match view {
            View::Ready(state) => {
                self.progress.set_fraction(state.progress);
                self.progress
                    .set_text(Some(&format!("{:.0}%", state.progress * 100.0)));
            }
            View::Offline => {
                self.progress.set_fraction(0.0);
                self.progress.set_text(Some("no connection"));
            }
        }
        let last_run = match view {
            View::Ready(state) => state.last_run,
            View::Offline => 0,
        };
        self.last_run.set_text(&ago_since(last_run, now));
        self.error.set_text(error_line(view).unwrap_or_default());
        self.error.set_visible(error_line(view).is_some());
        self.pause.set_label(pause_label(view));
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// Builds the window. Every widget is a stock GTK widget, so the window
/// follows the theme of the desktop.
pub fn build(app: &gtk::Application, actions: Sender<Action>) -> App {
    let root = Window::builder()
        .application(app)
        .title("Nimbus")
        .default_width(420)
        .default_height(260)
        .build();
    // Closing the window hides it. The tray keeps running, and a left click
    // brings the window back.
    root.set_hide_on_close(true);

    let header = HeaderBar::new();
    let heading = Label::new(Some("Nimbus"));
    heading.add_css_class("title");
    header.set_title_widget(Some(&heading));
    root.set_titlebar(Some(&header));

    let phase = Label::new(None);
    phase.set_xalign(0.0);
    phase.add_css_class("heading");

    let progress = ProgressBar::new();
    progress.set_show_text(true);
    progress.set_text(Some("no connection"));

    let last_run = Label::new(None);
    last_run.set_xalign(0.0);

    let error = Label::new(None);
    error.set_xalign(0.0);
    error.set_wrap(true);
    error.set_visible(false);

    let sync = Button::with_label("Sync now");
    let sync_actions = actions.clone();
    sync.connect_clicked(move |_| {
        let _ = sync_actions.send(Action::SyncNow);
    });

    let pause = Button::with_label("Pause");
    let pause_actions = actions;
    pause.connect_clicked(move |_| {
        let _ = pause_actions.send(Action::TogglePaused);
    });

    let buttons = GtkBox::new(Orientation::Horizontal, 8);
    buttons.set_halign(Align::Start);
    buttons.append(&sync);
    buttons.append(&pause);

    let body = GtkBox::new(Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.append(&phase);
    body.append(&progress);
    body.append(&last_run);
    body.append(&error);
    body.append(&buttons);
    root.set_child(Some(&body));

    App {
        root,
        phase,
        progress,
        last_run,
        error,
        pause,
    }
}

#[cfg(test)]
mod tests {
    use nimbus_ipc::Phase;

    use super::*;

    fn ready(phase: Phase) -> View {
        View::Ready(nimbus_ipc::State {
            phase,
            progress: 0.0,
            last_run: 0,
            last_error: String::new(),
        })
    }

    fn failed() -> View {
        View::Ready(nimbus_ipc::State {
            phase: Phase::Error,
            progress: 0.0,
            last_run: 0,
            last_error: String::from("Bisync aborted. Must run --resync to recover."),
        })
    }

    #[test]
    fn ago_covers_every_bucket() {
        assert_eq!(ago(0), "just now");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(120), "2 minutes ago");
        assert_eq!(ago(3599), "59 minutes ago");
        assert_eq!(ago(3600), "1 hour ago");
        assert_eq!(ago(86_400), "1 day ago");
        assert_eq!(ago(172_800), "2 days ago");
    }

    #[test]
    fn ago_of_zero_means_no_run_yet() {
        assert_eq!(ago_since(0, 1_700_000_000), "no run yet");
    }

    #[test]
    fn ago_since_uses_the_difference() {
        assert_eq!(ago_since(1_700_000_000 - 90, 1_700_000_000), "1 minute ago");
    }

    // A clock that steps backwards must not produce a negative time.
    #[test]
    fn ago_of_the_future_stays_sensible() {
        assert_eq!(ago_since(1_700_000_090, 1_700_000_000), "just now");
    }

    #[test]
    fn the_pause_button_follows_the_view() {
        assert_eq!(pause_label(&ready(Phase::Paused)), "Resume");
        assert_eq!(pause_label(&ready(Phase::Idle)), "Pause");
        assert_eq!(pause_label(&ready(Phase::Syncing)), "Pause");
    }

    // The window shows the error in its own line. The heading must stay short,
    // or the message appears twice.
    #[test]
    fn the_heading_never_repeats_the_error_line() {
        let view = failed();
        assert_eq!(status_name(&view), "Error");
        let View::Ready(state) = &view else {
            panic!("the test needs a ready view");
        };
        assert!(!status_name(&view).contains(&state.last_error));
    }

    // The line must hide itself when there is no message, or the window keeps
    // an empty gap where the error was.
    #[test]
    fn the_error_line_hides_itself_when_nothing_failed() {
        assert_eq!(
            error_line(&failed()),
            Some("Bisync aborted. Must run --resync to recover.")
        );
        assert_eq!(error_line(&ready(Phase::Idle)), None);
        assert_eq!(error_line(&View::Offline), None);
    }
}
