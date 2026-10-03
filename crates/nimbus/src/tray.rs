use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use ksni::blocking::Handle;
use nimbus_ipc::Phase;

use crate::view::{Action, View, is_paused, phase, status_name, status_text};
use crate::window::{PAUSE_ICON, SETTINGS_ICON, SYNC_ICON};

/// One icon file that every install ships, so its presence proves the theme
/// directory. The `-symbolic` name suffix is not decoration. GTK reads it to
/// recolor the icon with the foreground of the desktop, and KDE reads the style
/// block inside the file for the same job.
const ICON_FILE: &str = "hicolor/scalable/apps/nimbus-idle-symbolic.svg";
const FALLBACK: &str = "folder-sync";

/// Finds the theme directory that holds the icons. `None` means the tray shows
/// the fallback icon from the theme of the desktop.
pub fn icon_dir() -> Option<String> {
    let source = source_icon_dir();
    if Path::new(&source).join(ICON_FILE).is_file() {
        return Some(source);
    }
    installed_icon_dir().map(|dir| dir.to_string_lossy().into_owned())
}

/// Returns the icon for the phase. The storm cloud means that the user must act,
/// so an error and a resync both take it, and the tray sets no overlay.
fn phase_icon(view: &View) -> &'static str {
    match phase(view) {
        None => "nimbus-offline-symbolic",
        Some(Phase::Idle) => "nimbus-idle-symbolic",
        Some(Phase::Syncing) => "nimbus-syncing-symbolic",
        Some(Phase::Paused) => "nimbus-paused-symbolic",
        Some(Phase::Error | Phase::Resync) => "nimbus-error-symbolic",
    }
}

/// The engine drops a Sync now request while a run is active, so the menu
/// disables the row.
fn is_syncing(view: &View) -> bool {
    phase(view) == Some(Phase::Syncing)
}

/// Builds the tooltip for the panel.
fn tooltip(view: &View, icon: &str) -> ksni::ToolTip {
    ksni::ToolTip {
        icon_name: String::from(icon),
        icon_pixmap: Vec::new(),
        title: String::from("Nimbus"),
        description: status_text(view),
    }
}

pub struct NimbusTray {
    view: Arc<Mutex<View>>,
    icon_dir: Option<String>,
    actions: Sender<Action>,
}

impl NimbusTray {
    pub fn new(view: Arc<Mutex<View>>, icon_dir: Option<String>, actions: Sender<Action>) -> Self {
        Self {
            view,
            icon_dir,
            actions,
        }
    }

    fn current(&self) -> View {
        self.view.lock().expect("the view lock").clone()
    }

    /// A menu callback that sends `action`. The callback runs on a thread of
    /// the tray service and must not block, so it only sends on the channel.
    fn send_on_activate(&self, action: Action) -> Box<dyn Fn(&mut Self) + Send> {
        let actions = self.actions.clone();
        Box::new(move |_| {
            let _ = actions.send(action.clone());
        })
    }
}

impl ksni::Tray for NimbusTray {
    fn id(&self) -> String {
        String::from("Nimbus")
    }

    fn title(&self) -> String {
        String::from("Nimbus")
    }

    /// A passive item is one that carries no information, and many hosts hide
    /// it. The tray must stay visible.
    fn status(&self) -> ksni::Status {
        ksni::Status::Active
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_name(&self) -> String {
        match self.icon_dir {
            Some(_) => String::from(phase_icon(&self.current())),
            None => String::from(FALLBACK),
        }
    }

    fn icon_theme_path(&self) -> String {
        self.icon_dir.clone().unwrap_or_default()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        tooltip(&self.current(), &self.icon_name())
    }

    /// A left click opens the window. The menu is on a right click, because
    /// MENU_ON_ACTIVATE already defaults to false.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.actions.send(Action::ShowWindow);
    }

    /// Builds the menu. The host redraws it after every update, so the state
    /// here needs no separate refresh path.
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, MenuItem, StandardItem};
        let view = self.current();
        vec![
            MenuItem::Standard(StandardItem {
                label: status_name(&view),
                enabled: false,
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: String::from("Sync now"),
                icon_name: String::from(SYNC_ICON),
                // The engine drops a request while a run is active, so a
                // click on an enabled item would look broken.
                enabled: !is_syncing(&view),
                activate: self.send_on_activate(Action::SyncNow),
                ..Default::default()
            }),
            MenuItem::Checkmark(CheckmarkItem {
                // The row always reads Pause and shows a tick, so the icon
                // names the setting. Only the window button names the action.
                label: String::from("Pause"),
                icon_name: String::from(PAUSE_ICON),
                // The view is the only record of the pause state. A cached
                // flag would show a tick the daemon never confirmed.
                checked: is_paused(&view),
                activate: self.send_on_activate(Action::TogglePaused),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                // The row opens the dialog. It cannot open the status window,
                // because the status window has no field to change.
                label: String::from("Settings"),
                icon_name: String::from(SETTINGS_ICON),
                activate: self.send_on_activate(Action::OpenSettings),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: String::from("Quit"),
                icon_name: String::from("application-exit"),
                activate: Box::new(|_| std::process::exit(0)),
                ..Default::default()
            }),
        ]
    }
}

/// Moves the view into the tray and asks the host to read it again.
///
/// Every visible part of the tray is a `Tray` method that reads the shared
/// view, and the host calls them only after `update`. So the view write and the
/// update belong together, and they belong here rather than in the worker loop.
/// Returns false when the panel dropped the item, which the window survives.
pub fn refresh(handle: &Handle<NimbusTray>, view: &Mutex<View>, next: View) -> bool {
    // The lock must be released before the update. The update makes the tray
    // service read the view for the tooltip and rebuild the menu, and the
    // service runs on another thread. Holding the lock across the call would
    // wait for itself.
    let changed = {
        let mut guard = view.lock().expect("the view lock");
        if *guard == next {
            false
        } else {
            *guard = next;
            true
        }
    };
    changed && handle.update(|_: &mut NimbusTray| {}).is_none()
}

/// The icon directory in the source tree, so `cargo run -p nimbus` shows the
/// project icon before any package installs it. The data directory sits at the
/// root of the workspace, two levels above this crate.
fn source_icon_dir() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/icons")
        .to_string_lossy()
        .into_owned()
}

/// Finds the XDG icon directory that holds the file, where the package installs
/// it. The function returns the directory, not the file, because a host looks
/// an icon name up in its own cache, and the cache does not know an icon that
/// was installed after the last rebuild. The path makes the host read the file
/// instead.
fn installed_icon_dir() -> Option<PathBuf> {
    let dirs = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .into_iter()
        .chain(
            std::env::var_os("XDG_DATA_DIRS")
                .map(|dirs| std::env::split_paths(&dirs).collect::<Vec<_>>())
                .unwrap_or_default(),
        );
    dirs.map(|dir| dir.join("icons"))
        .find(|dir| dir.join(ICON_FILE).is_file())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use ksni::Tray;

    use super::*;
    use crate::view::fixtures::ready;

    fn tray(view: View) -> NimbusTray {
        let (tx, _rx) = mpsc::channel();
        NimbusTray::new(Arc::new(Mutex::new(view)), Some(source_icon_dir()), tx)
    }

    fn labels(menu: &[ksni::MenuItem<NimbusTray>]) -> Vec<String> {
        use ksni::MenuItem;
        menu.iter()
            .map(|item| match item {
                MenuItem::Standard(item) => item.label.clone(),
                MenuItem::Checkmark(item) => item.label.clone(),
                MenuItem::SubMenu(item) => item.label.clone(),
                MenuItem::RadioGroup(_) => String::from("a radio group"),
                MenuItem::Separator => String::from("a separator"),
            })
            .collect()
    }

    // Each phase names its own file. A missing file passes every other test
    // here, and the host then shows a blank item for that phase only.
    #[test]
    fn the_source_icon_dir_holds_every_phase_icon() {
        let views = [
            View::Offline(String::new()),
            ready(Phase::Idle, ""),
            ready(Phase::Syncing, ""),
            ready(Phase::Paused, ""),
            ready(Phase::Error, "x"),
            ready(Phase::Resync, "x"),
        ];
        let dir = Path::new(&source_icon_dir()).join("hicolor/scalable/apps");
        for view in &views {
            let path = dir.join(format!("{}.svg", phase_icon(view)));
            assert!(path.is_file(), "the icon is missing at {}", path.display());
        }
        assert!(Path::new(&source_icon_dir()).join(ICON_FILE).is_file());
    }

    #[test]
    fn the_icon_follows_the_phase() {
        assert_eq!(
            tray(ready(Phase::Idle, "")).icon_name(),
            "nimbus-idle-symbolic"
        );
        assert_eq!(
            tray(ready(Phase::Syncing, "")).icon_name(),
            "nimbus-syncing-symbolic"
        );
        assert_eq!(
            tray(ready(Phase::Paused, "")).icon_name(),
            "nimbus-paused-symbolic"
        );
        assert_eq!(
            tray(ready(Phase::Error, "x")).icon_name(),
            "nimbus-error-symbolic"
        );
        assert_eq!(
            tray(ready(Phase::Resync, "x")).icon_name(),
            "nimbus-error-symbolic"
        );
        assert_eq!(
            tray(View::Offline(String::new())).icon_name(),
            "nimbus-offline-symbolic"
        );
    }

    #[test]
    fn the_icon_falls_back_without_a_theme_dir() {
        let (tx, _rx) = mpsc::channel();
        let tray = NimbusTray::new(Arc::new(Mutex::new(ready(Phase::Idle, ""))), None, tx);
        assert_eq!(tray.icon_name(), FALLBACK);
        assert!(tray.icon_theme_path().is_empty());
    }

    #[test]
    fn the_offline_tooltip_names_the_daemon() {
        let tip = tooltip(&View::Offline(String::new()), "x");
        assert_eq!(tip.description, "The daemon is not running");
        assert_eq!(tip.title, "Nimbus");
    }

    #[test]
    fn the_idle_tooltip_says_idle() {
        assert_eq!(tooltip(&ready(Phase::Idle, ""), "x").description, "Idle");
    }

    #[test]
    fn the_paused_tooltip_says_paused() {
        assert_eq!(
            tooltip(&ready(Phase::Paused, ""), "x").description,
            "Paused"
        );
    }

    #[test]
    fn the_syncing_tooltip_shows_the_ratio() {
        let tip = tooltip(&ready(Phase::Syncing, ""), "x");
        assert_eq!(tip.description, "Syncing, 43%");
    }

    #[test]
    fn the_error_tooltip_shows_the_daemon_text() {
        let tip = tooltip(&ready(Phase::Error, "Bisync aborted"), "x");
        assert_eq!(tip.description, "Bisync aborted");
    }

    #[test]
    fn the_sync_state_comes_from_the_view() {
        assert!(is_syncing(&ready(Phase::Syncing, "")));
        assert!(!is_syncing(&ready(Phase::Idle, "")));
        assert!(!is_syncing(&View::Offline(String::new())));
    }

    #[test]
    fn the_menu_lists_the_items_in_order() {
        let menu = tray(View::Offline(String::new())).menu();
        assert_eq!(
            labels(&menu),
            [
                "The daemon is not running",
                "a separator",
                "Sync now",
                "Pause",
                "a separator",
                "Settings",
                "Quit",
            ]
        );
    }

    #[test]
    fn the_menu_header_is_not_clickable() {
        use ksni::MenuItem;
        let menu = tray(View::Offline(String::new())).menu();
        let MenuItem::Standard(header) = &menu[0] else {
            panic!("the first item is the status line");
        };
        assert!(!header.enabled, "the status line must not swallow clicks");
    }

    #[test]
    fn sync_now_is_disabled_while_syncing() {
        use ksni::MenuItem;
        let menu = tray(ready(Phase::Syncing, "")).menu();
        let MenuItem::Standard(sync) = &menu[2] else {
            panic!("the third item is Sync now");
        };
        assert!(!sync.enabled, "the engine drops a request during a run");
        let menu = tray(ready(Phase::Idle, "")).menu();
        let MenuItem::Standard(sync) = &menu[2] else {
            panic!("the third item is Sync now");
        };
        assert!(sync.enabled);
    }

    #[test]
    fn the_pause_checkmark_follows_the_view() {
        use ksni::MenuItem;
        let menu = tray(ready(Phase::Paused, "")).menu();
        let MenuItem::Checkmark(pause) = &menu[3] else {
            panic!("the fourth item is Pause");
        };
        assert!(pause.checked);
        let menu = tray(ready(Phase::Idle, "")).menu();
        let MenuItem::Checkmark(pause) = &menu[3] else {
            panic!("the fourth item is Pause");
        };
        assert!(!pause.checked);
    }
}
