use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gtk::gio;
use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, Button, DropDown, Entry, Label, Orientation, SpinButton, Window};

use nimbus_ipc::Settings;
use nimbusd::config::{Config, Invalid, apply_settings, settings_of, target};

use crate::view::Action;

/// The largest value that a spin button takes, in seconds. The config file has
/// no upper bound, so a value above this needs the file.
const MOST_SECONDS: f64 = 86_400.0;

/// The choices for `--conflict-resolve`, as the config value and the words
/// that the dialog shows. A test holds the values equal to the daemon list.
const RESOLVE: [(&str, &str); 7] = [
    ("none", "Both"),
    ("newer", "Newer"),
    ("older", "Older"),
    ("larger", "Larger"),
    ("smaller", "Smaller"),
    ("path1", "Local"),
    ("path2", "Remote"),
];

/// The choices for `--conflict-loser`. rclone uses them only when a copy wins.
/// A live run measured the two renames. `num` takes the next free number, as
/// in `f.conflict1`. `pathname` takes the number of the origin, so
/// `f.conflict1` is the local copy and `f.conflict2` is the remote copy.
const LOSER: [(&str, &str); 3] = [
    ("num", "Rename with number"),
    ("pathname", "Rename with origin"),
    ("delete", "Delete"),
];

fn choice(pairs: &[(&str, &str)]) -> DropDown {
    let words: Vec<&str> = pairs.iter().map(|(_, words)| *words).collect();
    DropDown::from_strings(&words)
}

/// The config value of the selected row. The list has no empty row, so the
/// first value stands in for a selection that does not exist.
fn chosen(field: &DropDown, pairs: &[(&str, &str)]) -> String {
    let (value, _) = pairs.get(field.selected() as usize).unwrap_or(&pairs[0]);
    String::from(*value)
}

/// Selects the row of a value. An unknown value keeps the row, because the
/// daemon refuses that value at start and the dialog cannot show it.
fn select(field: &DropDown, pairs: &[(&str, &str)], value: &str) {
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
    key: &'static str,
    field: gtk::Widget,
    line: Label,
}

impl Hint {
    fn new(key: &'static str, field: &impl IsA<gtk::Widget>) -> Self {
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
    apply_settings(&mut candidate, keys);
    candidate.check().err()
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
            conflict_resolve: chosen(&self.resolve, &RESOLVE),
            conflict_loser: chosen(&self.loser, &LOSER),
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
    fn write(&self, s: &Settings) {
        text(&self.remote, &s.remote);
        text(&self.local, &s.local);
        time(&self.interval, s.interval_secs);
        time(&self.debounce, s.debounce_secs);
        select(&self.resolve, &RESOLVE, &s.conflict_resolve);
        select(&self.loser, &LOSER, &s.conflict_loser);
        text(&self.flags, &s.extra_flags.join(" "));
    }
}

/// Names the part of the save that the user must know about.
///
/// A timer costs nothing, so a save that only moves the timers needs no
/// warning. A moved remote or folder has no bisync listing, so the next run
/// carries `--resync` and copies both sides again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    None,
    Remote,
    Folder,
    Both,
}

/// Reports what a save moves, judged the way the daemon judges it.
///
/// The folder comparison uses the folder that rclone opens, because the daemon
/// compares the same two. A trailing slash or a written-out home mark keeps the
/// folder, so the dialog must not warn about a save that costs nothing.
pub fn moved(before: &Settings, after: &Settings) -> Move {
    let remote = target(&before.remote) != target(&after.remote);
    let folder = folder_of(&before.local) != folder_of(&after.local);
    match (remote, folder) {
        (false, false) => Move::None,
        (true, false) => Move::Remote,
        (false, true) => Move::Folder,
        (true, true) => Move::Both,
    }
}

/// The folder that rclone opens for one of these texts. The rule matches
/// `LocalDir::path` in the daemon, so the two agree on what a move is.
fn folder_of(raw: &str) -> PathBuf {
    let Some(rest) = raw.strip_prefix("~/") else {
        return PathBuf::from(raw);
    };
    match std::env::var_os("HOME") {
        Some(home) => Path::new(&home).join(rest),
        None => PathBuf::from(raw),
    }
}

/// The question for a save that moves the sync, or `None` for a save that
/// moves nothing. The question names the same target that a run will use.
pub fn warning(before: &Settings, after: &Settings) -> Option<String> {
    let remote = target(&after.remote);
    let target = match moved(before, after) {
        Move::None => return None,
        Move::Remote => remote,
        Move::Folder => after.local.clone(),
        Move::Both => format!("{} and {remote}", after.local),
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

pub struct App {
    pub root: Window,
    fields: Fields,
    hints: Hints,
    error: Label,
    /// The keys as the daemon last reported them. The Save button compares the
    /// fields with this copy, so a warning covers a real move and nothing else.
    reported: Rc<RefCell<Settings>>,
}

impl App {
    /// Rewrites every field and hides the last message. The caller checks for
    /// a change first, so this runs only when the keys moved.
    pub fn apply(&self, settings: &Settings) {
        self.fields.write(settings);
        self.reported.replace(settings.clone());
        show(&self.hints, None);
        self.error.set_text("");
        self.error.set_visible(false);
    }

    /// Shows the message from a refusal. The daemon writes the text so the
    /// dialog and the journal say the same thing. An empty message hides the
    /// line, because a daemon that does not run has its reason in the window.
    pub fn refuse(&self, message: &str) {
        self.error.set_text(message);
        self.error.set_visible(!message.is_empty());
    }

    /// Closes the dialog after the daemon took the keys.
    pub fn close(&self) {
        self.root.set_visible(false);
    }
}

/// Builds the dialog. Every widget is stock GTK, so the dialog follows the
/// theme of the desktop. The window sets no title bar, for the same reason as
/// the status window.
///
/// The Remote path field takes a bare name, which syncs the root of the remote, or
/// a name with a folder, as in `gdrive:/Photos`, which syncs that folder.
///
/// The pause and the resync flag have no field either. The pause is on the menu
/// and the status window, and two controls for one flag would disagree.
pub fn build(app: &gtk::Application, actions: Sender<Action>) -> App {
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
    let start = settings_of(&Config::default());

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
        loser.set_sensitive(chosen(resolve, &RESOLVE) != "none");
    });
    // The handler runs on a change only, so the first state needs a call.
    fields.loser.set_sensitive(start.conflict_resolve != "none");
    select(&fields.resolve, &RESOLVE, &start.conflict_resolve);
    select(&fields.loser, &LOSER, &start.conflict_loser);
    text(&fields.remote, &start.remote);
    text(&fields.local, &start.local);
    // A placeholder shows only in an empty field. Adwaita dims it, but Breeze
    // draws it in the full text color, so it reads as the current value. The
    // rule fades the theme color instead of setting one.
    let dim = gtk::CssProvider::new();
    dim.load_from_data("placeholder { opacity: 0.5; }");
    gtk::style_context_add_provider_for_display(
        &WidgetExt::display(&root),
        &dim,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    fields.remote.set_placeholder_text(Some("drive:/"));
    fields.local.set_placeholder_text(Some("~/Cloud"));
    fields
        .flags
        .set_placeholder_text(Some("--drive-skip-shortcuts"));

    // The daemon's copy of the keys. The Save button reads it to decide
    // whether the save needs a warning. The seed must match what the widgets
    // start with, so a Save with no edit compares equal and sends straight
    // through.
    let reported = Rc::new(RefCell::new(start));

    let hints: Hints = [
        Hint::new("remote", &fields.remote),
        Hint::new("local", &fields.local),
        Hint::new("interval_secs", &fields.interval),
        Hint::new("debounce_secs", &fields.debounce),
    ];

    let save = Button::with_label("Save");
    let save_hints = hints.clone();
    let save_fields = fields.clone();
    let save_actions = actions.clone();
    let save_reported = Rc::clone(&reported);
    let save_parent = root.clone();
    save.connect_clicked(move |_| {
        let keys = save_fields.read();
        let refused = invalid(&keys);
        show(&save_hints, refused.as_ref());
        if refused.is_some() {
            return;
        }
        let move_ = moved(&save_reported.borrow(), &keys);
        if move_ == Move::None {
            let _ = save_actions.send(Action::SaveSettings(keys));
            return;
        }
        // A cancel leaves the dialog open with the typed text, so a misclick
        // costs one click instead of everything the user wrote.
        confirm(&save_parent, &save_reported.borrow(), &keys, &save_actions);
    });

    let cancel = Button::with_label("Cancel");
    let close = root.clone();
    cancel.connect_clicked(move |_| close.set_visible(false));

    // The line for a failure that belongs to no field, for example a daemon
    // that does not run.
    let error = error_line();

    // The file holds every key, also the ones the dialog has no field for,
    // and it is the only fix while the daemon refuses to start. The desktop
    // picks the editor for the file type.
    let open_file = Button::with_label("Open config file");
    open_file.set_hexpand(true);
    open_file.set_halign(Align::End);
    let open_parent = root.clone();
    let open_error = error.clone();
    open_file.connect_clicked(move |_| {
        let file = gio::File::for_path(nimbusd::config::default_file());
        let shown = open_error.clone();
        gtk::FileLauncher::new(Some(&file)).launch(
            Some(&open_parent),
            gio::Cancellable::NONE,
            move |opened| {
                if let Err(err) = opened {
                    shown.set_text(&format!("Cannot open the config file: {err}"));
                    shown.set_visible(true);
                }
            },
        );
    });

    let buttons = GtkBox::new(Orientation::Horizontal, 8);
    buttons.append(&save);
    buttons.append(&cancel);
    buttons.append(&open_file);

    let body = GtkBox::new(Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.append(&hints[0].row("Remote path"));
    body.append(&hints[1].row("Local folder"));
    body.append(&hints[2].row("Interval in seconds"));
    body.append(&hints[3].row("Quiet time in seconds"));
    body.append(&row("Copy to keep on conflict", &fields.resolve));
    body.append(&row("Action for the losing copy", &fields.loser));
    body.append(&row("Extra rclone flags", &fields.flags));
    body.append(&error);
    body.append(&buttons);
    root.set_child(Some(&body));

    App {
        root,
        fields,
        hints,
        error,
        reported,
    }
}

/// Asks once before a save that moves a remote or a folder.
///
/// `AlertDialog` draws its two text fields at different sizes, so the headline
/// and the detail carry one idea each. A save that moves nothing never reaches
/// this function, because the caller checks with `moved` first.
fn confirm(parent: &Window, before: &Settings, keys: &Settings, actions: &Sender<Action>) {
    let Some(question) = warning(before, keys) else {
        return;
    };
    let dialog = gtk::AlertDialog::builder()
        .message(question)
        .buttons(["_Cancel", "_Save"])
        .default_button(0)
        .cancel_button(0)
        .build();
    let actions = actions.clone();
    let keys = keys.clone();
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
            interval_secs: 900,
            debounce_secs: 30,
            conflict_resolve: String::from("newer"),
            conflict_loser: String::from("delete"),
            extra_flags: Vec::new(),
        }
    }

    // The dialog sends the value of a row, and the daemon refuses a value
    // that is not in its own list.
    #[test]
    fn the_choices_match_the_daemon_lists() {
        let values = |pairs: &[(&'static str, &str)]| -> Vec<&'static str> {
            pairs.iter().map(|(v, _)| *v).collect()
        };
        assert_eq!(values(&RESOLVE), nimbusd::config::CONFLICT_RESOLVE);
        assert_eq!(values(&LOSER), nimbusd::config::CONFLICT_LOSER);
    }

    // A save that moves nothing must not warn. A warning on every save trains
    // the user to click through, which is the whole cost of the dialog.
    #[test]
    fn a_save_of_the_same_keys_needs_no_warning() {
        let before = keys("drive", "/srv/notes");
        assert_eq!(moved(&before, &before.clone()), Move::None);
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
        assert_eq!(moved(&before, &after), Move::None);
        assert_eq!(warning(&before, &after), None);
    }

    // The headline must name where the sync goes, so the user reads the change
    // before the cost.
    #[test]
    fn a_moved_remote_names_the_new_remote() {
        let before = keys("drive", "/srv/notes");
        let after = keys("other", "/srv/notes");
        assert_eq!(moved(&before, &after), Move::Remote);
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
        assert_eq!(moved(&before, &keys("drive:/", "/srv/notes")), Move::None);
        let after = keys("drive:/Photos", "/srv/notes");
        assert_eq!(moved(&before, &after), Move::Remote);
        assert_eq!(
            warning(&before, &after).as_deref(),
            Some("Move the sync to drive:/Photos?")
        );
    }

    #[test]
    fn a_moved_folder_names_the_new_folder() {
        let before = keys("drive", "/srv/notes");
        let after = keys("drive", "/srv/other");
        assert_eq!(moved(&before, &after), Move::Folder);
        assert_eq!(
            warning(&before, &after).as_deref(),
            Some("Move the sync to /srv/other?")
        );
    }

    #[test]
    fn both_sides_named_when_both_move() {
        let before = keys("drive", "/srv/notes");
        let after = keys("other", "/srv/other");
        assert_eq!(moved(&before, &after), Move::Both);
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
        assert_eq!(moved(&before, &after), Move::None);
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
        assert_eq!(moved(&before, &after), Move::None);
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
