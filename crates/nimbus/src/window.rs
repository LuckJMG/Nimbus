use std::sync::mpsc::Sender;

use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, Button, Image, Label, Orientation, ProgressBar, Window};

use crate::view::{Action, View, is_paused, status_name};

/// The icon names. GTK resolves them from the icon theme of the desktop for
/// the window, and the desktop resolves the same names for the menu. All three
/// names exist in Breeze and in Adwaita.
pub const SYNC_ICON: &str = "view-refresh-symbolic";
pub const PAUSE_ICON: &str = "media-playback-pause-symbolic";
pub const RESUME_ICON: &str = "media-playback-start-symbolic";
/// The tray menu sends the same name to Plasma, so the window and the menu
/// draw one icon for one action.
pub const SETTINGS_ICON: &str = "preferences-system-symbolic";

/// States the last run in words. An absolute timestamp needs a date library
/// and a time zone, and the tray has no room for it either way.
fn ago(seconds: u64) -> String {
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
fn ago_since(unix: u64, now: u64) -> String {
    if unix == 0 {
        return String::from("no run yet");
    }
    ago(now.saturating_sub(unix))
}

fn pause_label(view: &View) -> &'static str {
    if is_paused(view) { "Resume" } else { "Pause" }
}

/// The icon that follows the same switch as the label, so a paused window
/// offers a play icon next to the word Resume.
pub fn pause_icon(view: &View) -> &'static str {
    if is_paused(view) {
        RESUME_ICON
    } else {
        PAUSE_ICON
    }
}

/// Builds a button with an icon in front of its text. The button takes a box
/// with the two parts, because the child of a button is its text. The caller
/// keeps the icon and the text, because the pause button rewrites both when the
/// view moves.
fn action_button(name: &str, text: &str) -> (Button, Image, Label) {
    let icon = Image::from_icon_name(name);
    icon.set_pixel_size(16);
    let label = Label::new(Some(text));
    let content = GtkBox::new(Orientation::Horizontal, 6);
    content.append(&icon);
    content.append(&label);
    let button = Button::new();
    button.set_child(Some(&content));
    (button, icon, label)
}

/// The error line, or `None` when the line must hide itself. A hidden line
/// leaves no gap, so a run that succeeds leaves no scar.
fn error_line(view: &View) -> Option<&str> {
    let View::Ready(state) = view else {
        return None;
    };
    (!state.last_error.is_empty()).then_some(state.last_error.as_str())
}

pub struct App {
    /// The timer holds a weak reference to this window, so it can outlive the
    /// call that built it.
    pub root: Window,
    phase: Label,
    progress: ProgressBar,
    last_run: Label,
    error: Label,
    pause_icon: Image,
    pause_text: Label,
}

impl App {
    /// Rewrites every field. The caller checks for a change first, so this runs
    /// only when the view moved. The heading takes the short name, because the
    /// error line below it carries the message from the daemon.
    pub fn apply(&self, view: &View, now: u64) {
        self.phase.set_text(&status_name(view));
        self.pause_text.set_text(pause_label(view));
        self.pause_icon.set_icon_name(Some(pause_icon(view)));
        let View::Ready(state) = view else {
            self.progress.set_fraction(0.0);
            self.progress.set_text(Some("no connection"));
            self.last_run.set_text(&ago_since(0, now));
            self.error.set_text("");
            self.error.set_visible(false);
            return;
        };
        self.progress.set_fraction(state.progress);
        self.progress
            .set_text(Some(&format!("{:.0}%", state.progress * 100.0)));
        self.last_run.set_text(&ago_since(state.last_run, now));
        self.error.set_text(error_line(view).unwrap_or_default());
        self.error.set_visible(error_line(view).is_some());
    }
}

/// Builds the window. Every widget is a stock GTK widget, so the window
/// follows the theme of the desktop. The window sets no title bar, so the
/// caption comes from the desktop and takes no room from the body.
pub fn build(app: &gtk::Application, actions: Sender<Action>) -> App {
    let root = Window::builder()
        .application(app)
        .title("Nimbus")
        .default_width(420)
        .build();
    // Closing the window hides it. The tray keeps running, and a left click
    // brings the window back.
    root.set_hide_on_close(true);

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

    let (sync, _, _) = action_button(SYNC_ICON, "Sync now");
    let sync_actions = actions.clone();
    sync.connect_clicked(move |_| {
        let _ = sync_actions.send(Action::SyncNow);
    });

    let (pause, pause_icon, pause_text) = action_button(PAUSE_ICON, "Pause");
    let pause_actions = actions.clone();
    pause.connect_clicked(move |_| {
        let _ = pause_actions.send(Action::TogglePaused);
    });

    // The button opens the same dialog as the menu row, so the window needs no
    // field of its own and no second route to the keys.
    let (settings, _, _) = action_button(SETTINGS_ICON, "Settings");
    let settings_actions = actions;
    settings.connect_clicked(move |_| {
        let _ = settings_actions.send(Action::OpenSettings);
    });

    // The two run buttons sit at the start. Settings sits at the end, because
    // it opens a dialog instead of running a sync, so it is not one of them.
    let runs = GtkBox::new(Orientation::Horizontal, 8);
    runs.set_halign(Align::Start);
    // The box takes the room that the row leaves, so the settings button moves
    // to the far end instead of sitting next to the two run buttons.
    runs.set_hexpand(true);
    runs.append(&sync);
    runs.append(&pause);

    let buttons = GtkBox::new(Orientation::Horizontal, 8);
    buttons.append(&runs);
    buttons.append(&settings);

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
        pause_icon,
        pause_text,
    }
}

#[cfg(test)]
mod tests {
    use nimbus_ipc::Phase;

    use crate::view::fixtures::ready;

    use super::*;

    fn failed() -> View {
        ready(
            Phase::Error,
            "Bisync aborted. Must run --resync to recover.",
        )
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
        assert_eq!(pause_label(&ready(Phase::Paused, "")), "Resume");
        assert_eq!(pause_label(&ready(Phase::Idle, "")), "Pause");
        assert_eq!(pause_label(&ready(Phase::Syncing, "")), "Pause");
    }

    // The label says Resume while paused, so the icon has to be a play icon.
    // A play icon next to the word Pause asks the user to pause twice.
    #[test]
    fn the_pause_icon_follows_the_view() {
        assert_eq!(pause_icon(&ready(Phase::Idle, "")), PAUSE_ICON);
        assert_eq!(pause_icon(&ready(Phase::Syncing, "")), PAUSE_ICON);
        assert_eq!(pause_icon(&ready(Phase::Paused, "")), RESUME_ICON);
        assert_eq!(pause_icon(&View::Offline), PAUSE_ICON);
    }

    // The line must hide itself when there is no message, or the window keeps
    // an empty gap where the error was.
    #[test]
    fn the_error_line_hides_itself_when_nothing_failed() {
        assert_eq!(
            error_line(&failed()),
            Some("Bisync aborted. Must run --resync to recover.")
        );
        assert_eq!(error_line(&ready(Phase::Idle, "")), None);
        assert_eq!(error_line(&View::Offline), None);
    }
}
