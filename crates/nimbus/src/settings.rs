use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gtk::gio;
use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, Button, DropDown, Entry, Label, Orientation, SpinButton, Window};

use nimbus_ipc::Settings;
use nimbusd::config::{Config, ConflictLoser, ConflictResolve, Invalid, Key, LocalDir, target};

use crate::view::{Action, Reply};

/// The largest value that a spin button takes, in seconds. The config file has
/// no upper bound, so a value above this needs the file.
const MOST_SECONDS: f64 = 86_400.0;

/// The choices for `--conflict-resolve`, with the words that the dialog shows.
/// A test holds the list equal to `ConflictResolve::ALL`.
const RESOLVE: [(ConflictResolve, &str); 7] = [
    (ConflictResolve::None, "Both"),
    (ConflictResolve::Newer, "Newer"),
    (ConflictResolve::Older, "Older"),
    (ConflictResolve::Larger, "Larger"),
    (ConflictResolve::Smaller, "Smaller"),
    (ConflictResolve::Path1, "Local"),
    (ConflictResolve::Path2, "Remote"),
];

/// The choices for `--conflict-loser`. rclone uses them only when a copy wins.
/// A live run measured the two renames. `num` takes the next free number, as
/// in `f.conflict1`. `pathname` takes the number of the origin, so
/// `f.conflict1` is the local copy and `f.conflict2` is the remote copy.
const LOSER: [(ConflictLoser, &str); 3] = [
    (ConflictLoser::Num, "Rename with number"),
    (ConflictLoser::Pathname, "Rename with origin"),
    (ConflictLoser::Delete, "Delete"),
];

fn choice<T>(pairs: &[(T, &str)]) -> DropDown {
    let words: Vec<&str> = pairs.iter().map(|(_, words)| *words).collect();
    DropDown::from_strings(&words)
}

/// The value of the selected row. The list has no empty row, so the first
/// value stands in for a selection that does not exist.
fn chosen<T: Copy>(field: &DropDown, pairs: &[(T, &str)]) -> T {
    pairs.get(field.selected() as usize).unwrap_or(&pairs[0]).0
}

/// Selects the row of a value from the wire. An unknown value keeps the row,
/// because the daemon refuses that value at start and the dialog cannot show
/// it.
fn select<T: PartialEq + TryFrom<String>>(field: &DropDown, pairs: &[(T, &str)], text: &str) {
    let Ok(value) = T::try_from(String::from(text)) else {
        return;
    };
    if let Some(at) = pairs.iter().position(|(v, _)| *v == value) {
        field.set_selected(at as u32);
    }
}

/// One labelled row. The label takes the space that every row shares, so the
/// fields all start at the same place.
fn row(label: &str, field: &impl IsA<gtk::Widget>) -> GtkBox {
    let caption = Label::new(Some(label));
    caption.set_xalign(0.0);
    caption.set_hexpand(true);
    let line = GtkBox::new(Orientation::Horizontal, 12);
    line.append(&caption);
    line.append(field);
    line
}

/// An error line in the theme's error color. It stays hidden until it has a
/// message.
fn error_line() -> Label {
    let line = Label::new(None);
    line.set_xalign(0.0);
    line.set_wrap(true);
    line.add_css_class("error");
    line.set_visible(false);
    line
}

/// A field that `check` can refuse, with the error line under it.
#[derive(Clone)]
struct Hint {
    key: Key,
    field: gtk::Widget,
    line: Label,
}

impl Hint {
    fn new(key: Key, field: &impl IsA<gtk::Widget>) -> Self {
        let line = error_line();
        // The line sits under the field, not under the caption.
        line.set_halign(Align::End);
        Self {
            key,
            field: field.clone().upcast(),
            line,
        }
    }

    /// Marks the field in the error color and shows the reason when `check`
    /// refused this key. Otherwise it clears both.
    fn show(&self, invalid: Option<&Invalid>) {
        let reason = invalid
            .filter(|i| i.key == self.key)
            .map(|i| i.reason.as_str());
        self.line.set_text(reason.unwrap_or_default());
        self.line.set_visible(reason.is_some());
        if reason.is_some() {
            self.field.add_css_class("error");
        } else {
            self.field.remove_css_class("error");
        }
    }

    /// The labelled row with the error line close under it. The body spaces
    /// its rows wide, so the line needs a box of its own to stay close.
    fn row(&self, label: &str) -> GtkBox {
        let group = GtkBox::new(Orientation::Vertical, 4);
        group.append(&row(label, &self.field));
        group.append(&self.line);
        group
    }
}

/// The hints for remote, local, interval_secs, and debounce_secs.
type Hints = [Hint; 4];

fn show(hints: &Hints, invalid: Option<&Invalid>) {
    for hint in hints {
        hint.show(invalid);
    }
}

/// Runs the check of the daemon on the typed keys, so the dialog can name
/// the field before the keys leave. The daemon checks them again.
fn invalid(keys: &Settings) -> Option<Invalid> {
    let mut candidate = Config::default();
    candidate.apply(keys).and_then(|_| candidate.check()).err()
}

/// A spin button for a time in seconds. The lower bound is one, because the
/// daemon refuses zero, and the button stops the user from typing anything
/// else.
fn seconds(value: u64) -> SpinButton {
    // The adjustment carries the range and the steps, so the button cannot be
    // typed outside it.
    let adjust = gtk::Adjustment::new(bounded(value), 1.0, MOST_SECONDS, 1.0, 60.0, 0.0);
    let step = SpinButton::new(Some(&adjust), 0.0, 0);
    step.set_numeric(true);
    step
}

/// The widgets that hold the keys.
///
/// A GTK clone points at the same object, so the Save button reads the fields
/// from its own closure and the window keeps them for a refresh.
#[derive(Clone)]
struct Fields {
    remote: Entry,
    local: Entry,
    interval: SpinButton,
    debounce: SpinButton,
    resolve: DropDown,
    loser: DropDown,
    /// The extra rclone flags, separated by spaces.
    flags: Entry,
}

impl Fields {
    /// Reads the widgets into a value for the daemon. An entry holds any text,
    /// so the daemon checks it and the message comes back.
    fn read(&self) -> Settings {
        Settings {
            remote: self.remote.text().to_string(),
            local: self.local.text().to_string(),
            interval_secs: self.interval.value().max(0.0) as u64,
            debounce_secs: self.debounce.value().max(0.0) as u64,
            conflict_resolve: String::from(chosen(&self.resolve, &RESOLVE).as_str()),
            conflict_loser: String::from(chosen(&self.loser, &LOSER).as_str()),
            extra_flags: self
                .flags
                .text()
                .split_whitespace()
                .map(String::from)
                .collect(),
        }
    }

    /// Rewrites every field from the daemon's copy.
    ///
    /// A field is only set when the text moved, because typing in one field
    /// must not clear another that the user never opened.
    fn write(&self, settings: &Settings) {
        text(&self.remote, &settings.remote);
        text(&self.local, &settings.local);
        time(&self.interval, settings.interval_secs);
        time(&self.debounce, settings.debounce_secs);
        select(&self.resolve, &RESOLVE, &settings.conflict_resolve);
        select(&self.loser, &LOSER, &settings.conflict_loser);
        text(&self.flags, &settings.extra_flags.join(" "));
    }
}

/// The question for a save that moves the sync, or `None` for a save that
/// moves nothing. The question names the same target that a run will use.
///
/// A moved remote or folder has no bisync listing, so the next run carries
/// `--resync` and copies both sides again. A timer costs nothing. The folder
/// comparison uses `LocalDir::path`, the same rule as the daemon, so a
/// trailing slash or a written-out home mark is not a move.
pub fn warning(before: &Settings, after: &Settings) -> Option<String> {
    let folder = |raw: &str| LocalDir::new(Path::new(raw)).path();
    let remote = target(&after.remote);
    let target = match (
        target(&before.remote) != remote,
        folder(&before.local) != folder(&after.local),
    ) {
        (false, false) => return None,
        (true, false) => remote,
        (false, true) => after.local.clone(),
        (true, true) => format!("{} and {remote}", after.local),
    };
    Some(format!("Move the sync to {target}?"))
}

fn text(field: &Entry, value: &str) {
    if field.text() != value {
        field.set_text(value);
    }
}

fn time(field: &SpinButton, value: u64) {
    let value = bounded(value);
    if field.value() != value {
        field.set_value(value);
    }
}

/// Fits a time into the range of the button. The config file has no upper
/// bound and the daemon refuses zero, so the button is the only limit that a
/// value from the daemon or from the user can meet.
fn bounded(value: u64) -> f64 {
    value.clamp(1, MOST_SECONDS as u64) as f64
}

/// A GTK clone points at the same widgets, so a button handler keeps its own
/// clone of the dialog.
#[derive(Clone)]
pub struct SettingsDialog {
    pub root: Window,
    fields: Fields,
    hints: Hints,
    error: Label,
    /// The keys as the daemon last reported them. The Save button compares the
    /// fields with this copy, so a warning covers a real move and nothing else.
    reported: Rc<RefCell<Settings>>,
}

impl SettingsDialog {
    /// Rewrites every field and hides the last message. The caller checks for
    /// a change first, so this runs only when the keys moved.
    fn apply(&self, settings: &Settings) {
        self.fields.write(settings);
        self.reported.replace(settings.clone());
        show(&self.hints, None);
        self.error.set_text("");
        self.error.set_visible(false);
    }

    /// Shows the message from a refusal. The daemon writes the text so the
    /// dialog and the journal say the same thing. An empty message hides the
    /// line, because a daemon that does not run has its reason in the window.
    fn refuse(&self, message: &str) {
        self.error.set_text(message);
        self.error.set_visible(!message.is_empty());
    }

    /// Carries out a reply from the worker. A refusal brings the dialog
    /// forward with the message, because a hidden dialog cannot show it.
    pub fn receive(&self, reply: Reply) {
        match reply {
            Reply::Settings(keys) => {
                self.apply(&keys);
                self.root.present();
            }
            Reply::Saved => self.root.set_visible(false),
            Reply::Refused(text) => {
                self.refuse(&text);
                self.root.present();
            }
            // The status window takes this one.
            Reply::Show => {}
        }
    }
}

/// Builds the dialog. Every widget is stock GTK, so the dialog follows the
/// theme of the desktop. The window sets no title bar, for the same reason as
/// the status window. The display-wide CSS comes from `install_css`, next to
/// the call that builds this dialog.
///
/// The Remote path field takes a bare name, which syncs the root of the remote, or
/// a name with a folder, as in `gdrive:/Photos`, which syncs that folder.
///
/// The pause and the resync flag have no field either. The pause is on the menu
/// and the status window, and two controls for one flag would disagree.
pub fn build(app: &gtk::Application, actions: Sender<Action>) -> SettingsDialog {
    let root = Window::builder()
        .application(app)
        .title("Nimbus settings")
        .default_width(460)
        .build();
    // Closing the dialog hides it. The tray keeps running, and the menu opens
    // the dialog again.
    root.set_hide_on_close(true);

    // The values that the widgets and the reported copy start with. A fresh
    // daemon writes the same defaults into the config file, so they match
    // what a first open shows. Both sides must agree, or a Save with no edit
    // sees a move.
    let start = Config::default().settings();

    let fields = Fields {
        remote: Entry::new(),
        local: Entry::new(),
        interval: seconds(start.interval_secs),
        debounce: seconds(start.debounce_secs),
        resolve: choice(&RESOLVE),
        loser: choice(&LOSER),
        flags: Entry::new(),
    };
    // With Both no copy wins, so no copy loses and the loser row does
    // nothing. rclone keeps both copies under new names.
    let loser = fields.loser.clone();
    fields.resolve.connect_selected_notify(move |resolve| {
        loser.set_sensitive(chosen(resolve, &RESOLVE) != ConflictResolve::None);
    });
    // The handler runs on a change only, so the first state needs a call.
    fields
        .loser
        .set_sensitive(start.conflict_resolve != ConflictResolve::None.as_str());
    fields.write(&start);
    // The placeholders are examples. `install_css` fades them, because Breeze
    // draws a placeholder in the full text color.
    fields.remote.set_placeholder_text(Some("drive:/"));
    fields.local.set_placeholder_text(Some("~/Cloud"));
    fields
        .flags
        .set_placeholder_text(Some("--drive-skip-shortcuts"));

    let hints: Hints = [
        Hint::new(Key::Remote, &fields.remote),
        Hint::new(Key::Local, &fields.local),
        Hint::new(Key::IntervalSecs, &fields.interval),
        Hint::new(Key::DebounceSecs, &fields.debounce),
    ];

    let dialog = SettingsDialog {
        root,
        fields,
        hints,
        // The line for a failure that belongs to no field, for example a
        // daemon that does not run.
        error: error_line(),
        // The seed must match what the widgets start with, so a Save with no
        // edit compares equal and sends straight through.
        reported: Rc::new(RefCell::new(start)),
    };

    let cancel = Button::with_label("Cancel");
    let close = dialog.root.clone();
    cancel.connect_clicked(move |_| close.set_visible(false));

    let buttons = GtkBox::new(Orientation::Horizontal, 8);
    buttons.append(&save_button(&dialog, actions));
    buttons.append(&cancel);
    buttons.append(&open_file_button(&dialog.root, &dialog.error));

    let body = GtkBox::new(Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.append(&dialog.hints[0].row("Remote path"));
    body.append(&dialog.hints[1].row("Local folder"));
    body.append(&dialog.hints[2].row("Interval in seconds"));
    body.append(&dialog.hints[3].row("Quiet time in seconds"));
    body.append(&row("Copy to keep on conflict", &dialog.fields.resolve));
    body.append(&row("Action for the losing copy", &dialog.fields.loser));
    body.append(&row("Extra rclone flags", &dialog.fields.flags));
    body.append(&dialog.error);
    body.append(&buttons);
    dialog.root.set_child(Some(&body));
    dialog
}

/// The Save button. It runs the check of the daemon on the typed keys first,
/// and it asks before a save that moves the sync.
fn save_button(dialog: &SettingsDialog, actions: Sender<Action>) -> Button {
    let save = Button::with_label("Save");
    let dialog = dialog.clone();
    save.connect_clicked(move |_| {
        let keys = dialog.fields.read();
        let refused = invalid(&keys);
        show(&dialog.hints, refused.as_ref());
        if refused.is_some() {
            return;
        }
        let Some(question) = warning(&dialog.reported.borrow(), &keys) else {
            let _ = actions.send(Action::SaveSettings(keys));
            return;
        };
        // A cancel leaves the dialog open with the typed text, so a misclick
        // costs one click instead of everything the user wrote.
        confirm(&dialog.root, question, keys, &actions);
    });
    save
}

/// The button that opens the config file. The file holds every key, also the
/// ones the dialog has no field for, and it is the only fix while the daemon
/// refuses to start. The desktop picks the editor for the file type.
fn open_file_button(parent: &Window, error: &Label) -> Button {
    let open_file = Button::with_label("Open config file");
    open_file.set_hexpand(true);
    open_file.set_halign(Align::End);
    let parent = parent.clone();
    let error = error.clone();
    open_file.connect_clicked(move |_| {
        let file = gio::File::for_path(nimbusd::config::default_file());
        let shown = error.clone();
        gtk::FileLauncher::new(Some(&file)).launch(
            Some(&parent),
            gio::Cancellable::NONE,
            move |opened| {
                if let Err(err) = opened {
                    shown.set_text(&format!("Cannot open the config file: {err}"));
                    shown.set_visible(true);
                }
            },
        );
    });
    open_file
}

/// Asks once before a save that moves a remote or a folder.
///
/// `AlertDialog` draws its two text fields at different sizes, so the headline
/// and the detail carry one idea each.
fn confirm(parent: &Window, question: String, keys: Settings, actions: &Sender<Action>) {
    let dialog = gtk::AlertDialog::builder()
        .message(question)
        .buttons(["_Cancel", "_Save"])
        .default_button(0)
        .cancel_button(0)
        .build();
    let actions = actions.clone();
    dialog.choose(Some(parent), gio::Cancellable::NONE, move |answer| {
        // The first button is the cancel one. A missing answer is a closed
        // dialog, which is the same as a cancel. The dialog stays open either
        // way, so a cancel costs one click and nothing else.
        if matches!(answer, Ok(1)) {
            let _ = actions.send(Action::SaveSettings(keys.clone()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(remote: &str, local: &str) -> Settings {
        Settings {
            remote: String::from(remote),
            local: String::from(local),
            ..Config::default().settings()
        }
    }

    // The dialog sends the value of a row, and the daemon refuses a value
    // that is not in its own list.
    #[test]
    fn the_choices_match_the_daemon_lists() {
        assert_eq!(RESOLVE.map(|(value, _)| value), ConflictResolve::ALL);
        assert_eq!(LOSER.map(|(value, _)| value), ConflictLoser::ALL);
    }

    // A save that moves nothing must not warn. A warning on every save trains
    // the user to click through, which is the whole cost of the dialog.
    #[test]
    fn a_save_of_the_same_keys_needs_no_warning() {
        let before = keys("drive", "/srv/notes");
        assert_eq!(warning(&before, &before), None);
    }

    // A timer change costs nothing, because the listing still fits.
    #[test]
    fn a_timer_change_needs_no_warning() {
        let before = keys("drive", "/srv/notes");
        let after = Settings {
            interval_secs: 120,
            ..before.clone()
        };
        assert_eq!(warning(&before, &after), None);
    }

    // The headline must name where the sync goes, so the user reads the change
    // before the cost.
    #[test]
    fn a_moved_remote_names_the_new_remote() {
        let before = keys("drive", "/srv/notes");
        let after = keys("other", "/srv/notes");
        assert_eq!(
            warning(&before, &after).as_deref(),
            Some("Move the sync to other:/?")
        );
    }

    // A folder in the remote is a different listing, so it costs a resync. A
    // bare name and the same name with `:/` open the same root.
    #[test]
    fn a_folder_in_the_remote_is_a_move() {
        let before = keys("drive", "/srv/notes");
        assert_eq!(warning(&before, &keys("drive:/", "/srv/notes")), None);
        let after = keys("drive:/Photos", "/srv/notes");
        assert_eq!(
            warning(&before, &after).as_deref(),
            Some("Move the sync to drive:/Photos?")
        );
    }

    #[test]
    fn a_moved_folder_names_the_new_folder() {
        let before = keys("drive", "/srv/notes");
        let after = keys("drive", "/srv/other");
        assert_eq!(
            warning(&before, &after).as_deref(),
            Some("Move the sync to /srv/other?")
        );
    }

    #[test]
    fn both_sides_named_when_both_move() {
        let before = keys("drive", "/srv/notes");
        let after = keys("other", "/srv/other");
        assert_eq!(
            warning(&before, &after).as_deref(),
            Some("Move the sync to /srv/other and other:/?")
        );
    }

    // The daemon compares the folder that rclone opens, so a trailing slash
    // keeps the folder. A warning here would name a resync that never happens.
    #[test]
    fn a_rewritten_folder_needs_no_warning() {
        let before = keys("drive", "/srv/notes");
        let after = keys("drive", "/srv/notes/");
        assert_eq!(warning(&before, &after), None);
    }

    // The home mark is a second spelling of the same folder. The daemon expands
    // it, so the dialog must expand it too.
    #[test]
    fn a_home_mark_and_a_written_out_folder_agree() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let before = keys("drive", "~/Nimbus");
        let after = keys(
            "drive",
            &Path::new(&home).join("Nimbus").display().to_string(),
        );
        assert_eq!(warning(&before, &after), None);
    }

    // The daemon refuses a zero, and the button stops below one.
    #[test]
    fn a_time_below_one_becomes_one() {
        assert_eq!(bounded(0), 1.0);
    }

    // A value in the file can be above the bound. The dialog must show the
    // bound, because sending the raw value back would move it.
    #[test]
    fn a_time_above_the_bound_becomes_the_bound() {
        assert_eq!(bounded(999_999), MOST_SECONDS);
    }

    #[test]
    fn a_time_in_range_stays() {
        assert_eq!(bounded(900), 900.0);
        assert_eq!(bounded(1), 1.0);
        assert_eq!(bounded(86_400), MOST_SECONDS);
    }
}
