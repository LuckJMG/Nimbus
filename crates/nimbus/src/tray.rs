use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use ksni::blocking::Handle;
use nimbus_ipc::Phase;

use crate::view::{Action, View, is_paused, is_syncing, phase, status_name, status_text};

/// The icon file inside a theme directory. The `-symbolic` name suffix is not
/// decoration. GTK reads it to recolor the icon with the foreground of the
/// desktop, and KDE reads the style block inside the file for the same job.
const ICON_FILE: &str = "hicolor/scalable/apps/nimbus-sync-symbolic.svg";
const ICON: &str = "nimbus-sync-symbolic";
const FALLBACK: &str = "folder-sync";

/// The icon name, plus the theme directory when the host needs one to find it.
#[derive(Debug, Clone, PartialEq)]
pub struct Icon {
    pub name: String,
    pub theme_path: String,
}

impl Icon {
    pub fn resolve() -> Self {
        let source = source_icon_dir();
        if Path::new(&source).join(ICON_FILE).is_file() {
            return pick(Some(source), None);
        }
        pick(None, installed_icon_dir())
    }
}

/// Picks the icon. The caller supplies both candidates, so a test decides them
/// without depending on what happens to be installed on the machine.
pub fn pick(source: Option<String>, installed: Option<PathBuf>) -> Icon {
    let (name, theme_path) = match (source, installed) {
        (Some(dir), _) => (ICON, dir),
        (None, Some(dir)) => (ICON, dir.to_string_lossy().into_owned()),
        (None, None) => (FALLBACK, String::new()),
    };
    Icon {
        name: String::from(name),
        theme_path,
    }
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

/// Returns the overlay icon. An empty name means no overlay, which is what the
/// specification asks for.
fn overlay(view: &View) -> String {
    match phase(view) {
        Some(Phase::Error) => String::from("dialog-error"),
        _ => String::new(),
    }
}

pub struct NimbusTray {
    view: Arc<Mutex<View>>,
    icon: Icon,
    actions: Sender<Action>,
}

impl NimbusTray {
    pub fn new(view: Arc<Mutex<View>>, icon: Icon, actions: Sender<Action>) -> Self {
        Self {
            view,
            icon,
            actions,
        }
    }

    fn current(&self) -> View {
        self.view.lock().expect("the view lock").clone()
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
        self.icon.name.clone()
    }

    fn icon_theme_path(&self) -> String {
        self.icon.theme_path.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        tooltip(&self.current(), &self.icon.name)
    }

    fn overlay_icon_name(&self) -> String {
        overlay(&self.current())
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
        let sync_actions = self.actions.clone();
        let pause_actions = self.actions.clone();
        vec![
            MenuItem::Standard(StandardItem {
                label: status_name(&view),
                enabled: false,
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: String::from("Sync now"),
                // The engine drops a request while a run is active, so a
                // click on an enabled item would look broken.
                enabled: !is_syncing(&view),
                activate: Box::new(move |_: &mut Self| {
                    let _ = sync_actions.send(Action::SyncNow);
                }),
                ..Default::default()
            }),
            MenuItem::Checkmark(CheckmarkItem {
                label: String::from("Pause"),
                // The view is the only record of the pause state. A cached
                // flag would show a tick the daemon never confirmed.
                checked: is_paused(&view),
                activate: Box::new(move |_| {
                    let _ = pause_actions.send(Action::TogglePaused);
                }),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: String::from("Settings"),
                activate: {
                    let settings_actions = self.actions.clone();
                    Box::new(move |_| {
                        let _ = settings_actions.send(Action::ShowWindow);
                    })
                },
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
        NimbusTray::new(
            Arc::new(Mutex::new(view)),
            pick(Some(source_icon_dir()), None),
            tx,
        )
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

    #[test]
    fn pick_uses_the_source_directory() {
        let icon = pick(Some(String::from("/src/data/icons")), None);
        assert_eq!(icon.name, ICON);
        assert_eq!(icon.theme_path, "/src/data/icons");
    }

    // A host looks an icon name up in its own cache, and the cache does not
    // know an icon that was installed after the last rebuild. The path makes
    // the host read the file. A live run on KDE showed a blank item without it.
    #[test]
    fn pick_sends_the_installed_directory_to_the_host() {
        let icon = pick(None, Some(PathBuf::from("/usr/share/icons")));
        assert_eq!(icon.name, ICON);
        assert_eq!(icon.theme_path, "/usr/share/icons");
    }

    #[test]
    fn pick_falls_back_when_the_file_is_absent() {
        let icon = pick(None, None);
        assert_eq!(icon.name, FALLBACK);
        assert!(icon.theme_path.is_empty());
    }

    // The icon directory has to reach the data directory at the root of the
    // workspace. A wrong number of levels passes every other test here, and the
    // tray then shows the fallback icon for no visible reason.
    #[test]
    fn the_source_icon_dir_holds_the_icon_file() {
        let path = Path::new(&source_icon_dir()).join(ICON_FILE);
        assert!(
            path.is_file(),
            "the icon file is missing at {}",
            path.display()
        );
    }

    #[test]
    fn the_offline_tooltip_names_the_daemon() {
        let tip = tooltip(&View::Offline, ICON);
        assert_eq!(tip.description, "The daemon is not running");
        assert_eq!(tip.title, "Nimbus");
    }

    #[test]
    fn the_idle_tooltip_says_idle() {
        assert_eq!(tooltip(&ready(Phase::Idle, ""), ICON).description, "Idle");
    }

    #[test]
    fn the_paused_tooltip_says_paused() {
        assert_eq!(
            tooltip(&ready(Phase::Paused, ""), ICON).description,
            "Paused"
        );
    }

    #[test]
    fn the_syncing_tooltip_shows_the_ratio() {
        let tip = tooltip(&ready(Phase::Syncing, ""), ICON);
        assert_eq!(tip.description, "Syncing, 43%");
    }

    #[test]
    fn the_error_tooltip_shows_the_daemon_text() {
        let tip = tooltip(&ready(Phase::Error, "Bisync aborted"), ICON);
        assert_eq!(tip.description, "Bisync aborted");
    }

    #[test]
    fn the_overlay_appears_only_for_an_error() {
        assert_eq!(overlay(&ready(Phase::Error, "x")), "dialog-error");
        assert_eq!(overlay(&ready(Phase::Idle, "")), "");
        assert_eq!(overlay(&ready(Phase::Syncing, "")), "");
        assert_eq!(overlay(&ready(Phase::Paused, "")), "");
        assert_eq!(overlay(&View::Offline), "");
    }

    #[test]
    fn the_pause_state_comes_from_the_view() {
        assert!(is_paused(&ready(Phase::Paused, "")));
        assert!(!is_paused(&ready(Phase::Idle, "")));
        assert!(!is_paused(&View::Offline), "a missing daemon is not paused");
    }

    #[test]
    fn the_sync_state_comes_from_the_view() {
        assert!(is_syncing(&ready(Phase::Syncing, "")));
        assert!(!is_syncing(&ready(Phase::Idle, "")));
        assert!(!is_syncing(&View::Offline));
    }

    #[test]
    fn the_menu_header_shortens_the_error() {
        assert_eq!(status_name(&ready(Phase::Error, "Bisync aborted")), "Error");
        assert_eq!(
            status_text(&ready(Phase::Error, "Bisync aborted")),
            "Bisync aborted"
        );
    }

    #[test]
    fn the_menu_lists_the_items_in_order() {
        let menu = tray(View::Offline).menu();
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
        let menu = tray(View::Offline).menu();
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
