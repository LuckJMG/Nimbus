use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, Button, Image, Label, Orientation, ProgressBar, Window};

use crate::view::{Action, View, is_paused, phase, status_name};
use nimbus_ipc::Phase;

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
fn synced(unix: u64, now: u64) -> String {
    if unix == 0 {
        return String::from("not synced yet");
    }
    format!("synced {}", ago(now.saturating_sub(unix)))
}

/// The fill and the text of the progress bar. Only a run fills the bar and
/// shows a percent. In every other phase the bar is empty and names the last
/// run, so a paused run does not leave a half-full bar behind.
fn bar(view: &View, now: u64) -> (f64, String) {
    match view {
        View::Ready(state) if state.phase == Phase::Syncing => {
            (state.progress, format!("{:.0}%", state.progress * 100.0))
        }
        View::Ready(state) => (0.0, synced(state.last_run, now)),
        View::Offline(_) => (0.0, String::from("no connection")),
    }
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

/// Sends `action` on every click. A GTK callback must not block, so it only
/// sends on the channel.
fn send_on_click(button: &Button, actions: &Sender<Action>, action: Action) {
    let actions = actions.clone();
    button.connect_clicked(move |_| {
        let _ = actions.send(action.clone());
    });
}

/// The error line, or `None` when the line must hide itself. A hidden line
/// leaves no gap, so a run that succeeds leaves no scar. With no daemon, the
/// line explains why and names the fix.
fn error_line(view: &View) -> Option<&str> {
    let text = match view {
        View::Ready(state) => &state.last_error,
        View::Offline(reason) => reason,
    };
    (!text.is_empty()).then_some(text.as_str())
}

pub struct StatusWindow {
    /// The timer holds a weak reference to this window, so it can outlive the
    /// call that built it.
    pub root: Window,
    phase: Label,
    progress: ProgressBar,
    error: Label,
    sync: Button,
    resync: Button,
    pause: Button,
    start: Button,
    pause_icon: Image,
    pause_text: Label,
    /// The text of the error line. In the resync phase it holds the reason
    /// from the daemon, and the resync dialog names it.
    resync_reason: Rc<RefCell<String>>,
    /// The view and the bar text that the window shows now.
    shown: RefCell<Option<(View, String)>>,
}

impl StatusWindow {
    /// Rewrites every field when the view or the bar text moved. The timer
    /// calls this on every tick, because the time of the last sync ages while
    /// the view stays the same. The heading takes the short name, because the
    /// error line below it carries the message from the daemon.
    pub fn apply(&self, view: &View, now: u64) {
        let (fraction, text) = bar(view, now);
        if let Some((shown_view, shown_text)) = &*self.shown.borrow()
            && shown_view == view
            && *shown_text == text
        {
            return;
        }
        self.phase.set_text(&status_name(view));
        self.pause_text.set_text(pause_label(view));
        self.pause_icon.set_icon_name(Some(pause_icon(view)));
        // Sync and pause need a daemon, so a window with no daemon offers
        // the start button in their place.
        let offline = matches!(view, View::Offline(_));
        // A pending resync blocks every run, so the window offers the resync
        // in place of Sync now.
        let resync = phase(view) == Some(Phase::Resync);
        self.sync.set_visible(!offline && !resync);
        self.resync.set_visible(resync);
        self.pause.set_visible(!offline);
        self.start.set_visible(offline);
        self.progress.set_fraction(fraction);
        self.progress.set_text(Some(&text));
        let error = error_line(view);
        self.error.set_text(error.unwrap_or_default());
        self.error.set_visible(error.is_some());
        self.resync_reason
            .replace(String::from(error.unwrap_or_default()));
        self.shown.replace(Some((view.clone(), text)));
    }
}

/// Asks before a resync. The reason comes first, because it says why the
/// question exists. Cancel is the default, so a stray Enter does nothing.
fn confirm_resync(parent: &Window, reason: &str, actions: &Sender<Action>) {
    let dialog = gtk::AlertDialog::builder()
        .message("Resync both sides?")
        .detail(format!(
            "{reason} Files on one side only are copied to the other. Where a \
             file differs, the local copy replaces the remote copy."
        ))
        .buttons(["_Cancel", "_Resync"])
        .default_button(0)
        .cancel_button(0)
        .build();
    let actions = actions.clone();
    dialog.choose(Some(parent), gtk::gio::Cancellable::NONE, move |answer| {
        // A closed dialog is the same as a cancel.
        if matches!(answer, Ok(1)) {
            let _ = actions.send(Action::Resync);
        }
    });
}

/// Builds the window. Every widget is a stock GTK widget, so the window
/// follows the theme of the desktop. The window sets no title bar, so the
/// caption comes from the desktop and takes no room from the body. The
/// display-wide CSS comes from `install_css`, next to the call that builds
/// this window.
pub fn build(app: &gtk::Application, actions: Sender<Action>) -> StatusWindow {
    let root = Window::builder()
        .application(app)
        .title("Nimbus")
        .default_width(420)
        // A fixed window still follows its content, so the height grows and
        // shrinks with the error line.
        .resizable(false)
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

    let error = Label::new(None);
    error.set_xalign(0.0);
    error.set_wrap(true);
    // The stock class takes the error color of the theme.
    error.add_css_class("error");
    // A wrapped label asks for the width of its whole text. The limit keeps a
    // long message from widening the window, so the text wraps instead.
    error.set_max_width_chars(1);
    error.set_visible(false);

    let (sync, _, _) = action_button(SYNC_ICON, "Sync now");
    send_on_click(&sync, &actions, Action::SyncNow);

    let (resync, _, _) = action_button(SYNC_ICON, "Resync");
    resync.set_visible(false);
    let resync_reason = Rc::new(RefCell::new(String::new()));
    let resync_actions = actions.clone();
    let resync_parent = root.clone();
    let reason = Rc::clone(&resync_reason);
    resync.connect_clicked(move |_| {
        confirm_resync(&resync_parent, &reason.borrow(), &resync_actions);
    });

    let (pause, pause_icon, pause_text) = action_button(PAUSE_ICON, "Pause");
    send_on_click(&pause, &actions, Action::TogglePaused);

    let (start, _, _) = action_button(RESUME_ICON, "Start daemon");
    start.set_visible(false);
    send_on_click(&start, &actions, Action::StartDaemon);

    // The button opens the same dialog as the menu row, so the window needs no
    // field of its own and no second route to the keys.
    let (settings, _, _) = action_button(SETTINGS_ICON, "Settings");
    send_on_click(&settings, &actions, Action::OpenSettings);

    // The two run buttons sit at the start. Settings sits at the end, because
    // it opens a dialog instead of running a sync, so it is not one of them.
    let runs = GtkBox::new(Orientation::Horizontal, 8);
    runs.set_halign(Align::Start);
    // The box takes the room that the row leaves, so the settings button moves
    // to the far end instead of sitting next to the two run buttons.
    runs.set_hexpand(true);
    runs.append(&sync);
    runs.append(&resync);
    runs.append(&pause);
    runs.append(&start);

    let buttons = GtkBox::new(Orientation::Horizontal, 8);
    buttons.append(&runs);
    buttons.append(&settings);

    let body = GtkBox::new(Orientation::Vertical, 12);
    // GTK focuses the first focusable widget when the window opens, and Breeze
    // draws a focused button as selected. The body takes that first focus and
    // draws nothing, and Tab still reaches every button.
    body.set_focusable(true);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.append(&phase);
    body.append(&progress);
    body.append(&error);
    body.append(&buttons);
    root.set_child(Some(&body));

    StatusWindow {
        root,
        phase,
        progress,
        error,
        sync,
        resync,
        pause,
        start,
        pause_icon,
        pause_text,
        resync_reason,
        shown: RefCell::new(None),
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
    fn synced_of_zero_means_no_run_yet() {
        assert_eq!(synced(0, 1_700_000_000), "not synced yet");
    }

    #[test]
    fn synced_uses_the_difference() {
        assert_eq!(
            synced(1_700_000_000 - 90, 1_700_000_000),
            "synced 1 minute ago"
        );
    }

    // A clock that steps backwards must not produce a negative time.
    #[test]
    fn synced_in_the_future_stays_sensible() {
        assert_eq!(synced(1_700_000_090, 1_700_000_000), "synced just now");
    }

    // Only a run fills the bar. The fixture carries a ratio of 0.43, which a
    // paused run can leave behind, and the bar must still be empty.
    #[test]
    fn only_a_run_fills_the_bar() {
        assert_eq!(
            bar(&ready(Phase::Syncing, ""), 0),
            (0.43, String::from("43%"))
        );
        for phase in [Phase::Idle, Phase::Paused, Phase::Error, Phase::Resync] {
            assert_eq!(
                bar(&ready(phase, ""), 0),
                (0.0, String::from("not synced yet")),
                "{phase:?}"
            );
        }
        assert_eq!(
            bar(&View::Offline(String::new()), 0),
            (0.0, String::from("no connection"))
        );
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
        assert_eq!(pause_icon(&View::Offline(String::new())), PAUSE_ICON);
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
        assert_eq!(error_line(&View::Offline(String::new())), None);
        assert_eq!(
            error_line(&View::Offline(String::from("Fix: x"))),
            Some("Fix: x")
        );
    }
}
