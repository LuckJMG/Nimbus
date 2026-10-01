use std::sync::mpsc::Sender;

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
    path: Entry,
    local: Entry,
    interval: SpinButton,
    debounce: SpinButton,
}

impl Fields {
    /// Reads the widgets into a value for the daemon. An entry holds any text,
    /// so the daemon checks it and the message comes back.
    fn read(&self) -> Settings {
        Settings {
            remote: self.remote.text().to_string(),
            path: self.path.text().to_string(),
            local: self.local.text().to_string(),
            interval_secs: self.interval.value().max(0.0) as u64,
            debounce_secs: self.debounce.value().max(0.0) as u64,
        }
    }

    /// Rewrites every field from the daemon's copy.
    ///
    /// A field is only set when the text moved, because typing in one field
    /// must not clear another that the user never opened.
    fn write(&self, s: &Settings) {
        text(&self.remote, &s.remote);
        text(&self.path, &s.path);
        text(&self.local, &s.local);
        time(&self.interval, s.interval_secs);
        time(&self.debounce, s.debounce_secs);
    }
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
}

impl App {
    /// Rewrites every field and hides the last message. The caller checks for
    /// a change first, so this runs only when the keys moved.
    pub fn apply(&self, settings: &Settings) {
        self.fields.write(settings);
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
/// The pause and the resync flag have no field. The pause is on the menu and
/// the status window, and two controls for one flag would disagree.
pub fn build(app: &gtk::Application, actions: Sender<Action>) -> App {
    let root = Window::builder()
        .application(app)
        .title("Nimbus settings")
        .default_width(460)
        .build();
    // Closing the dialog hides it. The tray keeps running, and the menu opens
    // the dialog again.
    root.set_hide_on_close(true);

    let fields = Fields {
        remote: Entry::new(),
        path: Entry::new(),
        local: Entry::new(),
        interval: seconds(900),
        debounce: seconds(30),
    };

    let save = Button::with_label("Save");
    let save_fields = fields.clone();
    let save_actions = actions.clone();
    save.connect_clicked(move |_| {
        let _ = save_actions.send(Action::SaveSettings(save_fields.read()));
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
    body.append(&row("Folder in the remote", &fields.path));
    body.append(&row("Local folder", &fields.local));
    body.append(&row("Interval in seconds", &fields.interval));
    body.append(&row("Quiet time in seconds", &fields.debounce));
    body.append(&error);
    body.append(&buttons);
    root.set_child(Some(&body));

    App {
        root,
        fields,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
