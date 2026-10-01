use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gtk::gio;
use gtk::prelude::*;
use gtk::{Align, Box as GtkBox, Button, Entry, Label, Orientation, SpinButton, Window};

use nimbus_ipc::Settings;

use crate::view::Action;

/// The largest value that a spin button takes, in seconds. The config file has
/// no upper bound, so a value above this needs the file.
const MOST_SECONDS: f64 = 86_400.0;

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
    let remote = before.remote != after.remote;
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

/// The headline and the detail for one save.
///
/// The two parts exist because `AlertDialog` takes two fields and draws them at
/// different sizes. A user reads the headline to learn what changes and the
/// detail to learn what it costs, so each part carries one of those.
pub fn warning(before: &Settings, after: &Settings) -> (String, String) {
    let move_ = moved(before, after);
    if move_ == Move::None {
        return (String::new(), String::new());
    }
    // The daemon targets the root of the remote, so the headline names the same
    // place that a run will use. A remote move names the remote, because the
    // local folder does not change. A folder move names the folder, for the
    // same reason.
    let target = match move_ {
        Move::Remote => format!("{}:/", after.remote),
        Move::Folder | Move::Both => after.local.clone(),
        Move::None => String::new(),
    };
    let headline = format!("This moves the sync to {target}.");
    let mut detail = String::from(
        "Nimbus keeps a record of what it has already copied. A new place has no \
         record, so the next run copies everything again instead of only what \
         changed. A large remote takes a while.",
    );
    match move_ {
        Move::Remote => {
            detail.push_str(" The whole remote will be compared with the local folder.");
        }
        Move::Folder => {
            detail.push_str(&format!(" {} will no longer start a run.", before.local));
        }
        Move::Both => {
            detail.push_str(&format!(
                " The whole remote will be compared with the local folder, and {} will no longer start a run.",
                before.local
            ));
        }
        Move::None => {}
    }
    (headline, detail)
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
        self.error.set_text("");
        self.error.set_visible(false);
    }

    /// Shows the message from a refusal. The daemon writes the text so the
    /// dialog and the journal say the same thing.
    pub fn refuse(&self, message: &str) {
        self.error.set_text(message);
        self.error.set_visible(true);
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
/// The dialog holds no folder in the remote. The daemon syncs the root of the
/// remote, so there is nothing to choose.
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
    // daemon writes these into the config file, so they match what a first
    // open shows. Both sides must agree, or a Save with no edit sees a move.
    let start = Settings {
        remote: String::from("gdrive"),
        local: String::from("~/Nimbus"),
        interval_secs: 900,
        debounce_secs: 30,
        extra_flags: Vec::new(),
    };

    let fields = Fields {
        remote: Entry::new(),
        local: Entry::new(),
        interval: seconds(start.interval_secs),
        debounce: seconds(start.debounce_secs),
        flags: Entry::new(),
    };
    text(&fields.remote, &start.remote);
    text(&fields.local, &start.local);
    fields
        .flags
        .set_placeholder_text(Some("--drive-skip-shortcuts"));

    // The daemon's copy of the keys. The Save button reads it to decide
    // whether the save needs a warning. The seed must match what the widgets
    // start with, so a Save with no edit compares equal and sends straight
    // through.
    let reported = Rc::new(RefCell::new(start));

    let save = Button::with_label("Save");
    let save_fields = fields.clone();
    let save_actions = actions.clone();
    let save_reported = Rc::clone(&reported);
    let save_parent = root.clone();
    save.connect_clicked(move |_| {
        let keys = save_fields.read();
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

    let buttons = GtkBox::new(Orientation::Horizontal, 8);
    buttons.set_halign(Align::Start);
    buttons.append(&save);
    buttons.append(&cancel);

    let error = Label::new(None);
    error.set_xalign(0.0);
    error.set_wrap(true);
    error.set_visible(false);

    let body = GtkBox::new(Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.append(&row("Remote", &fields.remote));
    body.append(&row("Local folder", &fields.local));
    body.append(&row("Interval in seconds", &fields.interval));
    body.append(&row("Quiet time in seconds", &fields.debounce));
    body.append(&row("Extra rclone flags", &fields.flags));
    body.append(&error);
    body.append(&buttons);
    root.set_child(Some(&body));

    App {
        root,
        fields,
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
    let (headline, detail) = warning(before, keys);
    let dialog = gtk::AlertDialog::builder()
        .message(headline)
        .detail(detail)
        .buttons(["_Cancel", "_Save anyway"])
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
            extra_flags: Vec::new(),
        }
    }

    // A save that moves nothing must not warn. A warning on every save trains
    // the user to click through, which is the whole cost of the dialog.
    #[test]
    fn a_save_of_the_same_keys_needs_no_warning() {
        let before = keys("drive", "/srv/notes");
        assert_eq!(moved(&before, &before.clone()), Move::None);
        assert_eq!(warning(&before, &before), (String::new(), String::new()));
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
        assert_eq!(warning(&before, &after).0, "");
    }

    // The headline must name where the sync goes, so the user reads the change
    // before the cost.
    #[test]
    fn a_moved_remote_names_the_new_remote() {
        let before = keys("drive", "/srv/notes");
        let after = keys("other", "/srv/notes");
        assert_eq!(moved(&before, &after), Move::Remote);
        let (headline, detail) = warning(&before, &after);
        assert!(headline.contains("other:/"), "{headline}");
        assert!(detail.contains("copies everything again"), "{detail}");
    }

    // The old folder stops starting runs, and the user typed a new one, so the
    // detail must name the folder that loses that role.
    #[test]
    fn a_moved_folder_names_the_old_one() {
        let before = keys("drive", "/srv/notes");
        let after = keys("drive", "/srv/other");
        assert_eq!(moved(&before, &after), Move::Folder);
        let (headline, detail) = warning(&before, &after);
        assert!(headline.contains("/srv/other"), "{headline}");
        assert!(detail.contains("/srv/notes"), "{detail}");
        assert!(detail.contains("no longer start a run"), "{detail}");
    }

    #[test]
    fn both_sides_named_when_both_move() {
        let before = keys("drive", "/srv/notes");
        let after = keys("other", "/srv/other");
        assert_eq!(moved(&before, &after), Move::Both);
        let (_, detail) = warning(&before, &after);
        assert!(detail.contains("whole remote"), "{detail}");
        assert!(detail.contains("/srv/notes"), "{detail}");
    }

    // A long run must be expected, because the user waits for it.
    #[test]
    fn every_warning_says_how_long_it_takes() {
        let before = keys("drive", "/srv/notes");
        for after in [
            keys("other", "/srv/notes"),
            keys("drive", "/srv/other"),
            keys("other", "/srv/other"),
        ] {
            assert!(
                warning(&before, &after).1.contains("takes a while"),
                "a long run must be expected"
            );
        }
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
