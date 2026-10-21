use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gtk::prelude::*;
use gtk::{
    Align, Box as GtkBox, Button, Image, Label, ListBox, Orientation, PolicyType, ScrolledWindow,
    SelectionMode, Separator, Window,
};

use crate::history::Snapshot;
use crate::rows;
use crate::view::{Action, View, can_sync_now, is_paused, phase, status_text};
use nimbus_ipc::{Entry, Phase};

/// The icon names. GTK resolves them from the icon theme of the desktop for
/// the window, and the desktop resolves the same names for the menu. All of the
/// names exist in Breeze and in Adwaita.
pub const SYNC_ICON: &str = "view-refresh-symbolic";
pub const PAUSE_ICON: &str = "media-playback-pause-symbolic";
pub const RESUME_ICON: &str = "media-playback-start-symbolic";
/// The tray menu sends the same name to Plasma, so the window and the menu
/// draw one icon for one action.
pub const SETTINGS_ICON: &str = "preferences-system-symbolic";
const FOLDER_ICON: &str = "folder-open-symbolic";

/// The icon and the CSS class of the circle in the header. The class sets the
/// color, and `install_css` in `main.rs` holds the rules. A missing daemon and
/// a missing network share one look, as they do in the tray icon.
fn badge(view: &View) -> (&'static str, &'static str) {
    match phase(view) {
        Some(Phase::Idle) => ("object-select-symbolic", "nimbus-ok"),
        Some(Phase::Syncing) => (SYNC_ICON, "nimbus-syncing"),
        Some(Phase::Error) => ("window-close-symbolic", "nimbus-error"),
        Some(Phase::Resync) => ("emblem-important-symbolic", "nimbus-warning"),
        Some(Phase::Paused) => (PAUSE_ICON, "nimbus-off"),
        Some(Phase::Offline) | None => ("network-offline-symbolic", "nimbus-off"),
    }
}

/// The text beside the circle, and whether it is an error. The message of an
/// error takes the place of the state text, and the line then takes the error
/// color of the theme. A daemon that cannot start has a reason too, and the
/// reason names the fix.
fn status_line(view: &View) -> (String, bool) {
    match view {
        View::Offline(reason) if !reason.is_empty() => (reason.clone(), true),
        View::Ready(state) if state.phase == Phase::Error => (status_text(view), true),
        _ => (status_text(view), false),
    }
}

/// The slot of the pause button. With no daemon, the same click starts the
/// daemon, so the label names that.
fn pause_label(view: &View) -> &'static str {
    match view {
        View::Offline(_) => "Start daemon",
        _ if is_paused(view) => "Resume",
        _ => "Pause",
    }
}

/// The icon that follows the same switch as the label, so a paused window
/// offers a play icon next to the word Resume.
fn pause_icon(view: &View) -> &'static str {
    if is_paused(view) || matches!(view, View::Offline(_)) {
        RESUME_ICON
    } else {
        PAUSE_ICON
    }
}

/// Builds a flat button with the icon above one line of text. The button takes
/// a box with the two parts, because the child of a button is its text. The
/// caller keeps the icon and the text, because the pause button rewrites both
/// when the view moves.
fn tile_button(name: &str, text: &str) -> (Button, Image, Label) {
    let icon = Image::from_icon_name(name);
    icon.set_pixel_size(24);
    let label = Label::new(Some(text));
    let content = GtkBox::new(Orientation::Vertical, 4);
    content.append(&icon);
    content.append(&label);
    let button = Button::new();
    button.add_css_class("flat");
    // A short word, for example Pause, must not make a narrow button.
    button.set_size_request(96, -1);
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

pub struct StatusWindow {
    /// The timer holds a weak reference to this window, so it can outlive the
    /// call that built it.
    pub root: Window,
    badge: GtkBox,
    badge_icon: Image,
    status: Label,
    empty: Label,
    sync: Button,
    resync: Button,
    pause_icon: Image,
    pause_text: Label,
    open: Button,
    scroll: ScrolledWindow,
    list: ListBox,
    /// The entries that the rows show, newest first. A row index finds its
    /// entry here.
    entries: Rc<RefCell<Vec<Entry>>>,
    /// The label of the age in each row, in the order of `entries`. Both lists
    /// must change together.
    ages: RefCell<Vec<Label>>,
    /// The synced folder. It is empty until the daemon has told it.
    local: Rc<RefCell<PathBuf>>,
    /// The reason from the daemon in the resync phase, and the resync dialog
    /// names it.
    resync_reason: Rc<RefCell<String>>,
    /// The view that the window shows now.
    shown: RefCell<Option<View>>,
}

impl StatusWindow {
    /// Rewrites every field when the view moved. The timer calls this on every
    /// tick, and a view that did not change costs one comparison.
    pub fn apply(&self, view: &View) {
        if self.shown.borrow().as_ref() == Some(view) {
            return;
        }
        let (icon, class) = badge(view);
        self.badge_icon.set_icon_name(Some(icon));
        self.badge.set_css_classes(&["nimbus-badge", class]);
        let (text, is_error) = status_line(view);
        self.status.set_text(&text);
        // The text takes the color of the circle. An error message takes the
        // stock class instead, so the theme picks the color.
        self.status
            .set_css_classes(&[if is_error { "error" } else { class }]);
        let ready = matches!(view, View::Ready(_));
        // A window with no daemon shows no history, because old rows can make
        // a stopped sync look healthy.
        self.empty.set_visible(ready);
        let resync = phase(view) == Some(Phase::Resync);
        // A pending resync blocks every run, so the window offers the resync
        // in place of Sync now. The slot stays when the daemon is gone, so the
        // row keeps its shape.
        self.sync.set_visible(!resync);
        self.sync
            .set_sensitive(can_sync_now(view) && !matches!(view, View::Offline(_)));
        self.resync.set_visible(resync);
        self.pause_text.set_text(pause_label(view));
        self.pause_icon.set_icon_name(Some(pause_icon(view)));
        self.resync_reason.replace(match view {
            View::Ready(state) if resync => state.last_error.clone(),
            _ => String::new(),
        });
        self.shown.replace(Some(view.clone()));
    }

    /// Takes the list of the worker. A list that continues the one on show adds
    /// rows at the top and cuts rows at the end. Any other list builds every
    /// row again. The folder goes to the Open folder button too.
    pub fn receive_history(&self, snapshot: Snapshot) {
        self.open
            .set_sensitive(!snapshot.local.as_os_str().is_empty());
        let now = rows::unix_now();
        let moved = *self.local.borrow() != snapshot.local;
        let plan = if moved {
            None
        } else {
            rows::diff(&self.entries.borrow(), &snapshot.entries)
        };
        // A user who scrolled down keeps the rows in view. At the top, the
        // new rows push the old ones down.
        let scrolled = self.scroll.vadjustment();
        let value = scrolled.value();
        let away = value > scrolled.lower();
        let added = plan.unwrap_or_else(|| {
            while let Some(row) = self.list.row_at_index(0) {
                self.list.remove(&row);
            }
            self.ages.borrow_mut().clear();
            snapshot.entries.len()
        });

        // The disk is read once for each folder, not once for each row.
        let mut exists: HashMap<PathBuf, bool> = HashMap::new();
        let mut labels = Vec::with_capacity(added);
        for (position, entry) in snapshot.entries[..added].iter().enumerate() {
            let folder = rows::folder_path(&snapshot.local, &entry.folder);
            let there = *exists
                .entry(folder)
                .or_insert_with_key(|path| path.is_dir());
            let (row, when) = rows::build(entry, &snapshot.local, there, now, &self.root);
            self.list.insert(&row, position as i32);
            labels.push(when);
        }
        let len = snapshot.entries.len();
        while let Some(row) = self.list.row_at_index(len as i32) {
            self.list.remove(&row);
        }
        let mut ages = self.ages.borrow_mut();
        ages.splice(0..0, labels);
        ages.truncate(len);
        debug_assert_eq!(ages.len(), len);
        drop(ages);

        if plan.is_some() && away && added > 0 {
            let height: i32 = (0..added)
                .filter_map(|position| self.list.row_at_index(position as i32))
                .map(|row| row.measure(Orientation::Vertical, -1).1)
                .sum();
            scrolled.set_value(value + f64::from(height));
        }
        self.entries.replace(snapshot.entries);
        self.local.replace(snapshot.local);
        // An old row keeps the text from the time it was built, and the window
        // can have been hidden for an hour.
        self.refresh_ages();
    }

    /// Writes the age of every row again. A label that reads the same is not
    /// touched, so a call that changes nothing causes no redraw.
    pub fn refresh_ages(&self) {
        let now = rows::unix_now();
        for (label, entry) in self.ages.borrow().iter().zip(self.entries.borrow().iter()) {
            let text = rows::age(entry.time, now);
            if label.text() != text {
                label.set_text(&text);
            }
        }
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
///
/// The size is fixed. The list area has a set height and scrolls, so the
/// window does not grow with the history.
pub fn build(app: &gtk::Application, actions: Sender<Action>) -> StatusWindow {
    let root = Window::builder()
        .application(app)
        .title("Nimbus")
        .resizable(false)
        .build();
    // Closing the window hides it. The tray keeps running, and a left click
    // brings the window back.
    root.set_hide_on_close(true);

    let badge_icon = Image::new();
    badge_icon.set_pixel_size(11);
    badge_icon.set_hexpand(true);
    badge_icon.set_vexpand(true);
    let badge = GtkBox::new(Orientation::Horizontal, 0);
    badge.set_size_request(18, 18);
    // The expanding icon would make the box expand too, and the circle would
    // stretch into an oval.
    badge.set_hexpand(false);
    badge.set_valign(Align::Center);
    badge.append(&badge_icon);

    let status = Label::new(None);
    status.set_xalign(0.0);
    status.set_hexpand(true);
    status.set_wrap(true);
    // A wrapped label asks for the width of its whole text. The limit keeps a
    // long message from widening the window, so the text wraps instead.
    status.set_max_width_chars(1);

    // The button opens the same dialog as the menu row, so the window needs no
    // field of its own and no second route to the keys.
    let settings = Button::from_icon_name(SETTINGS_ICON);
    settings.add_css_class("flat");
    settings.set_tooltip_text(Some("Settings"));
    settings.set_valign(Align::Center);
    send_on_click(&settings, &actions, Action::OpenSettings);

    // The margins sit on the header and on the buttons, not on the body, so
    // the list and the separator reach the edges and the scrollbar sits at the
    // edge of the window.
    let header = GtkBox::new(Orientation::Horizontal, 6);
    header.set_margin_top(12);
    header.set_margin_start(12);
    header.set_margin_end(12);
    header.append(&badge);
    header.append(&status);
    header.append(&settings);

    let empty = Label::new(Some("No files synced yet"));
    empty.add_css_class("dim-label");
    empty.set_vexpand(true);
    empty.set_valign(Align::Center);
    let rows = ListBox::new();
    rows.set_selection_mode(SelectionMode::None);
    rows.add_css_class("nimbus-list");
    // The list shows this label while it holds no row.
    rows.set_placeholder(Some(&empty));
    let scroll = ScrolledWindow::new();
    scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
    scroll.set_min_content_height(360);
    scroll.set_propagate_natural_height(false);
    scroll.set_child(Some(&rows));

    let (sync, _, _) = tile_button(SYNC_ICON, "Sync now");
    send_on_click(&sync, &actions, Action::SyncNow);

    let (resync, _, _) = tile_button(SYNC_ICON, "Resync");
    resync.set_visible(false);
    let resync_reason = Rc::new(RefCell::new(String::new()));
    let resync_actions = actions.clone();
    let resync_parent = root.clone();
    let reason = Rc::clone(&resync_reason);
    resync.connect_clicked(move |_| {
        confirm_resync(&resync_parent, &reason.borrow(), &resync_actions);
    });

    let (pause, pause_icon, pause_text) = tile_button(PAUSE_ICON, "Pause");
    send_on_click(&pause, &actions, Action::TogglePaused);

    let (open, _, _) = tile_button(FOLDER_ICON, "Open folder");
    // The daemon tells the folder, so the button waits for the first answer.
    open.set_sensitive(false);
    let local = Rc::new(RefCell::new(PathBuf::new()));
    let entries = Rc::new(RefCell::new(Vec::<Entry>::new()));
    // A click on a row opens its file.
    let row_entries = Rc::clone(&entries);
    let row_local = Rc::clone(&local);
    let row_parent = root.clone();
    rows.connect_row_activated(move |_, row| {
        let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
        if let Some(entry) = row_entries.borrow().get(index) {
            rows::open_file(&rows::path_of(&row_local.borrow(), entry), &row_parent);
        }
    });
    let open_local = Rc::clone(&local);
    let open_parent = root.clone();
    open.connect_clicked(move |_| {
        let folder = gtk::gio::File::for_path(&*open_local.borrow());
        gtk::FileLauncher::new(Some(&folder)).launch(
            Some(&open_parent),
            gtk::gio::Cancellable::NONE,
            |result| {
                if let Err(err) = result {
                    eprintln!("nimbus: cannot open the folder: {err}");
                }
            },
        );
    });

    // The buttons keep their own width and sit together in the middle. Equal
    // shares of 420 pixels put them far apart.
    let buttons = GtkBox::new(Orientation::Horizontal, 6);
    buttons.set_halign(Align::Center);
    buttons.set_margin_bottom(12);
    buttons.append(&sync);
    buttons.append(&resync);
    buttons.append(&pause);
    buttons.append(&open);

    let body = GtkBox::new(Orientation::Vertical, 12);
    // GTK focuses the first focusable widget when the window opens, and Breeze
    // draws a focused button as selected. The body takes that first focus and
    // draws nothing, and Tab still reaches every button.
    body.set_focusable(true);
    body.append(&header);
    body.append(&scroll);
    body.append(&Separator::new(Orientation::Horizontal));
    body.append(&buttons);
    root.set_child(Some(&body));

    StatusWindow {
        root,
        badge,
        badge_icon,
        status,
        empty,
        sync,
        resync,
        pause_icon,
        pause_text,
        open,
        scroll,
        list: rows,
        entries,
        ages: RefCell::new(Vec::new()),
        local,
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

    // The circle must tell the seven states apart by the icon, and the two
    // gray ones must differ from every colored one.
    #[test]
    fn the_circle_follows_the_phase() {
        assert_eq!(
            badge(&ready(Phase::Idle, "")),
            ("object-select-symbolic", "nimbus-ok")
        );
        assert_eq!(
            badge(&ready(Phase::Syncing, "")),
            (SYNC_ICON, "nimbus-syncing")
        );
        assert_eq!(
            badge(&ready(Phase::Error, "x")),
            ("window-close-symbolic", "nimbus-error")
        );
        assert_eq!(
            badge(&ready(Phase::Resync, "")),
            ("emblem-important-symbolic", "nimbus-warning")
        );
        assert_eq!(badge(&ready(Phase::Paused, "")), (PAUSE_ICON, "nimbus-off"));
        assert_eq!(
            badge(&ready(Phase::Offline, "")),
            ("network-offline-symbolic", "nimbus-off")
        );
    }

    #[test]
    fn a_missing_daemon_looks_like_a_missing_network() {
        let offline = badge(&ready(Phase::Offline, ""));
        assert_eq!(badge(&View::Offline(String::new())), offline);
        // A saved pause with no daemon reads as paused, as the menu does.
        assert_eq!(
            badge(&View::OfflinePaused),
            badge(&ready(Phase::Paused, ""))
        );
    }

    #[test]
    fn the_status_line_names_the_state() {
        assert_eq!(
            status_line(&ready(Phase::Idle, "")),
            (String::from("Up to date"), false)
        );
        assert_eq!(
            status_line(&ready(Phase::Syncing, "")),
            (String::from("Syncing 43%"), false)
        );
        assert_eq!(
            status_line(&ready(Phase::Paused, "")),
            (String::from("Paused"), false)
        );
    }

    // The message from the daemon takes the place of the state text, and the
    // line takes the error color.
    #[test]
    fn an_error_message_takes_the_status_line() {
        assert_eq!(
            status_line(&failed()),
            (
                String::from("Bisync aborted. Must run --resync to recover."),
                true
            )
        );
        assert_eq!(
            status_line(&View::Offline(String::from("Fix: x"))),
            (String::from("Fix: x"), true)
        );
    }

    // A daemon that does not run, with a good config, has no message to show,
    // and the text is not an error.
    #[test]
    fn a_missing_daemon_without_a_reason_is_not_an_error() {
        assert_eq!(
            status_line(&View::Offline(String::new())),
            (String::from("The daemon is not running"), false)
        );
    }

    #[test]
    fn the_pause_slot_follows_the_view() {
        assert_eq!(pause_label(&ready(Phase::Idle, "")), "Pause");
        assert_eq!(pause_label(&ready(Phase::Syncing, "")), "Pause");
        assert_eq!(pause_label(&ready(Phase::Paused, "")), "Resume");
        assert_eq!(pause_label(&View::OfflinePaused), "Resume");
        assert_eq!(pause_label(&View::Offline(String::new())), "Start daemon");
    }

    // The label says Resume while paused, so the icon has to be a play icon.
    // A play icon next to the word Pause asks the user to pause twice.
    #[test]
    fn the_pause_icon_follows_the_view() {
        assert_eq!(pause_icon(&ready(Phase::Idle, "")), PAUSE_ICON);
        assert_eq!(pause_icon(&ready(Phase::Syncing, "")), PAUSE_ICON);
        assert_eq!(pause_icon(&ready(Phase::Paused, "")), RESUME_ICON);
        assert_eq!(pause_icon(&View::Offline(String::new())), RESUME_ICON);
    }
}
