use std::path::Path;
use std::sync::{Arc, Mutex};

use nimbus_ipc::{Phase, State};

/// The icon file inside a theme directory.
const ICON_FILE: &str = "hicolor/scalable/apps/nimbus-sync.svg";
const ICON: &str = "nimbus-sync";
const FALLBACK: &str = "folder-sync";

/// What the tray shows. The daemon may not run, so the state is optional.
#[derive(Debug, Clone, PartialEq)]
pub enum View {
    Offline,
    Ready(State),
}

/// The icon name, plus the theme path when the host needs one to find it.
#[derive(Debug, Clone, PartialEq)]
pub struct Icon {
    pub name: String,
    pub theme_path: String,
}

impl Icon {
    pub fn resolve() -> Self {
        pick(
            Path::new(&source_theme_path()).join(ICON_FILE).is_file(),
            installed(),
        )
    }
}

/// Picks the icon. The caller supplies both answers, so a test decides them
/// without depending on what happens to be installed on the machine.
pub fn pick(source: bool, installed: bool) -> Icon {
    match (source, installed) {
        (true, _) => Icon {
            name: String::from(ICON),
            theme_path: source_theme_path(),
        },
        (false, true) => Icon {
            name: String::from(ICON),
            theme_path: String::new(),
        },
        (false, false) => Icon {
            name: String::from(FALLBACK),
            theme_path: String::new(),
        },
    }
}

/// Builds the tooltip for the panel.
pub fn tooltip(view: &View, icon: &str) -> ksni::ToolTip {
    let description = match view {
        View::Offline => String::from("The daemon is not running"),
        View::Ready(state) => match state.phase {
            Phase::Idle => String::from("Idle"),
            Phase::Syncing => format!("Syncing, {:.0}%", state.progress * 100.0),
            Phase::Paused => String::from("Paused"),
            Phase::Error if state.last_error.is_empty() => String::from("The last run failed"),
            Phase::Error => state.last_error.clone(),
        },
    };
    ksni::ToolTip {
        icon_name: String::from(icon),
        icon_pixmap: Vec::new(),
        title: String::from("Nimbus"),
        description,
    }
}

/// Returns the overlay icon. An empty name means no overlay, which is what the
/// specification asks for.
pub fn overlay(view: &View) -> String {
    match view {
        View::Ready(state) if state.phase == Phase::Error => String::from("dialog-error"),
        _ => String::new(),
    }
}

pub struct NimbusTray {
    view: Arc<Mutex<View>>,
    icon: Icon,
}

impl NimbusTray {
    pub fn new(view: Arc<Mutex<View>>, icon: Icon) -> Self {
        Self { view, icon }
    }

    fn current(&self) -> View {
        self.view.lock().expect("the view lock").clone()
    }
}

impl ksni::Tray for NimbusTray {
    fn id(&self) -> String {
        String::from("nimbus")
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
}

/// The icon directory in the source tree, so `cargo run -p nimbus` shows the
/// project icon before any package installs it. The data directory sits at the
/// root of the workspace, two levels above this crate.
fn source_theme_path() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/icons")
        .to_string_lossy()
        .into_owned()
}

/// Reports whether the icon file sits in an XDG icon directory, where the
/// package installs it.
fn installed() -> bool {
    if let Some(home) = std::env::var_os("XDG_DATA_HOME")
        && Path::new(&home).join("icons").join(ICON_FILE).is_file()
    {
        return true;
    }
    std::env::var_os("XDG_DATA_DIRS").is_some_and(|dirs| {
        std::env::split_paths(&dirs).any(|dir| dir.join("icons").join(ICON_FILE).is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(phase: Phase, last_error: &str) -> View {
        View::Ready(State {
            phase,
            progress: 0.43,
            last_run: 0,
            last_error: String::from(last_error),
        })
    }

    #[test]
    fn pick_uses_the_source_tree() {
        let icon = pick(true, false);
        assert_eq!(icon.name, ICON);
        assert!(
            icon.theme_path.contains("data/icons"),
            "the host needs the path"
        );
    }

    #[test]
    fn pick_uses_the_installed_icon() {
        let icon = pick(false, true);
        assert_eq!(icon.name, ICON);
        assert!(
            icon.theme_path.is_empty(),
            "an installed icon needs no path"
        );
    }

    #[test]
    fn pick_falls_back_when_the_file_is_absent() {
        let icon = pick(false, false);
        assert_eq!(icon.name, FALLBACK);
        assert!(icon.theme_path.is_empty());
    }

    // The theme path has to reach the data directory at the root of the
    // workspace. A wrong number of levels passes every other test here, and the
    // tray then shows the fallback icon for no visible reason.
    #[test]
    fn the_source_theme_path_holds_the_icon_file() {
        let path = Path::new(&source_theme_path()).join(ICON_FILE);
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
    fn the_error_tooltip_never_comes_up_empty() {
        let tip = tooltip(&ready(Phase::Error, ""), ICON);
        assert!(!tip.description.is_empty(), "an empty tooltip says nothing");
    }

    #[test]
    fn the_overlay_appears_only_for_an_error() {
        assert_eq!(overlay(&ready(Phase::Error, "x")), "dialog-error");
        assert_eq!(overlay(&ready(Phase::Idle, "")), "");
        assert_eq!(overlay(&ready(Phase::Syncing, "")), "");
        assert_eq!(overlay(&ready(Phase::Paused, "")), "");
        assert_eq!(overlay(&View::Offline), "");
    }
}
