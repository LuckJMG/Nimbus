mod history;
mod rows;
mod settings;
mod tray;
mod view;
mod window;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gtk::glib::{ControlFlow, ExitCode, WeakRef};
use gtk::prelude::*;
use ksni::blocking::TrayMethods;
use nimbus_ipc::{NimbusProxyBlocking, TRAY_NAME};

use history::Watch;
use tray::NimbusTray;
use view::{Action, Reply, View, is_paused};

/// The application id. GApplication uses it to hand an activation to the
/// process that already holds the name, which keeps one icon in the panel. The
/// daemon watches the same name, so the daemon stops when the tray ends.
const APP_ID: &str = TRAY_NAME;

/// The worker reads the state on this period. The engine sends one update for
/// each whole percent, so a longer period would only add delay.
const POLL: Duration = Duration::from_secs(2);

/// The main thread checks the window on this period, so this is the delay
/// before a tray click brings the window up.
const TICK: Duration = Duration::from_millis(100);

/// The window rewrites the age of its rows on this period while it is open.
const AGE_EVERY: Duration = Duration::from_secs(10);

/// The daemon must answer within this time. The zbus default is 25 seconds,
/// which would freeze the tray while the daemon stops answering.
const TIMEOUT: Duration = Duration::from_secs(3);

fn main() -> ExitCode {
    // The autostart entry in data/autostart passes --hidden, so a login starts
    // the tray and no window. GApplication refuses an option that it does not know, so the tray
    // reads the flag here and passes no arguments on.
    let hidden = std::env::args().skip(1).any(|arg| arg == "--hidden");
    match start(hidden) {
        Ok(app) => app.run_with_args::<&str>(&[]),
        Err(err) => {
            eprintln!("nimbus: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// GTK needs the main thread, and the zbus blocking calls must not run inside
/// a main loop. So the window lives on the main thread and the tray runs on a
/// worker thread of its own.
fn start(hidden: bool) -> Result<gtk::Application> {
    let conn = zbus::blocking::connection::Builder::session()
        .context("the tray cannot reach the session bus")?
        .method_timeout(TIMEOUT)
        .build()
        .context("the tray cannot connect to the session bus")?;
    // The generated proxy borrows the connection, and the worker thread lives
    // longer than this function. The tray opens one connection for the life of
    // the process, so the leak is the lifetime the connection needs anyway.
    let conn: &'static zbus::blocking::Connection = Box::leak(Box::new(conn));
    let proxy = NimbusProxyBlocking::builder(conn)
        // The proxy caches properties by default, and it refreshes the cache
        // only when the daemon sends the standard PropertiesChanged signal.
        // The daemon sends its own signal, so a cached proxy would report a
        // stale state forever, and would never notice that the daemon stopped.
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .context("the tray cannot build the proxy")?;

    let view = Arc::new(Mutex::new(View::Offline(String::new())));
    let (action_tx, action_rx) = mpsc::channel::<Action>();
    let (reply_tx, reply_rx) = mpsc::channel::<Reply>();

    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::empty())
        .build();

    // The activate callback runs more than once, so it cannot move these out
    // on the first call. The first call takes them.
    let pending = RefCell::new(Some((
        Start {
            proxy,
            actions: action_rx,
            menu: action_tx,
            show: reply_tx,
        },
        reply_rx,
    )));
    let live: RefCell<Option<Live>> = RefCell::new(None);

    app.connect_activate(move |app| {
        if let Some(live) = live.borrow().as_ref() {
            // A second process handed its activation to this one, so only the
            // window has to come forward.
            live.window.root.present();
            return;
        }
        let (start, requests) = pending
            .borrow_mut()
            .take()
            .expect("the first activation starts the worker");
        let opened = first_activation(app, &view, &start.menu, requests);
        // A later activation comes from a second process, for example a start
        // from the app menu, so it presents the window whatever this flag says.
        if !hidden {
            opened.window.root.present();
        }
        *live.borrow_mut() = Some(opened);

        let view = Arc::clone(&view);
        if let Err(err) = thread::Builder::new()
            .name("nimbus-tray".into())
            .spawn(move || run_worker(view, start))
        {
            eprintln!("nimbus: the tray cannot start the worker thread: {err}");
        }
    });

    Ok(app)
}

/// What the first activation hands to the worker thread. The windows and the
/// worker share one channel, so both kinds of request use the same router.
struct Start {
    proxy: NimbusProxyBlocking<'static>,
    actions: Receiver<Action>,
    menu: Sender<Action>,
    show: Sender<Reply>,
}

/// What the first activation keeps on the main thread.
struct Live {
    /// The tray keeps the process alive with no window. Dropping the guard
    /// would release the hold, so the guard stays here.
    _hold: gtk::gio::ApplicationHoldGuard,
    window: Rc<window::StatusWindow>,
}

/// Builds both windows and starts the timer that feeds them.
///
/// The dialog is built once and never shown here, because it opens only from
/// the menu. It is reused after that, because a rebuild would throw away a
/// half-typed value.
fn first_activation(
    app: &gtk::Application,
    view: &Arc<Mutex<View>>,
    actions: &Sender<Action>,
    requests: Receiver<Reply>,
) -> Live {
    let hold = app.hold();
    let window = Rc::new(window::build(app, actions.clone()));
    let dialog = settings::build(app, actions.clone());
    install_css(&WidgetExt::display(&window.root));
    refresh_loop(view, &window, dialog, requests, actions.clone());
    Live {
        _hold: hold,
        window,
    }
}

/// Installs the theme fixes for both windows. A GTK 4 style provider covers
/// the whole display, so the rules sit beside the two `build` calls.
///
/// - Breeze rounds the bottom corners of the body, but the frame around it
///   stays square, so the desktop shows through two small gaps.
/// - Adwaita dims a placeholder, but Breeze draws it in the full text color,
///   so an example reads as the current value. The rule fades the theme color
///   instead of setting one.
/// - The circle in the header takes its color from the state. The named colors
///   come from the theme. The arrows of the syncing state turn without end.
/// - A link button keeps the padding of a button, which makes the line of a
///   row taller than its text. The rule takes the padding away. A folder that
///   is gone gets a gray link.
/// - The history shows the color of the window, and three sizes of text tell
///   the name, the place, and the age apart. The row draws its own color, so
///   the rule clears the row too, and a second rule gives back the hover color
///   that says a click opens the file.
fn install_css(display: &gtk::gdk::Display) {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        "window { border-radius: 0; } \
         placeholder { opacity: 0.5; } \
         .nimbus-list, .nimbus-list row { background: transparent; } \
         .nimbus-list row:hover { background: alpha(@theme_fg_color, 0.06); } \
         .nimbus-place { font-size: 87%; } \
         .nimbus-when { font-size: 75%; } \
         .nimbus-link { padding: 0; min-height: 0; min-width: 0; } \
         .nimbus-link.nimbus-gone, .nimbus-link.nimbus-gone label { color: alpha(@theme_fg_color, 0.45); } \
         .nimbus-badge { border-radius: 50%; } \
         .nimbus-badge image { color: white; } \
         .nimbus-badge.nimbus-ok { background: @success_color; } label.nimbus-ok { color: @success_color; } \
         .nimbus-badge.nimbus-syncing { background: @theme_selected_bg_color; } label.nimbus-syncing { color: @theme_selected_bg_color; } \
         .nimbus-badge.nimbus-error { background: @error_color; } \
         .nimbus-badge.nimbus-warning { background: @warning_color; } label.nimbus-warning { color: @warning_color; } \
         .nimbus-badge.nimbus-off { background: alpha(@theme_fg_color, 0.45); } label.nimbus-off { color: alpha(@theme_fg_color, 0.45); } \
         @keyframes nimbus-spin { \
             from { -gtk-icon-transform: rotate(0deg); } \
             to { -gtk-icon-transform: rotate(360deg); } \
         } \
         .nimbus-syncing image { animation: nimbus-spin 1.5s linear infinite; }",
    );
    gtk::style_context_add_provider_for_display(
        display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// The one main loop timer. It moves the windows when the worker asks, and it
/// hands the view to the status window, which skips a redraw that changes
/// nothing.
///
/// One timer serves both windows, because a second timer would need a second
/// glib source and both read the same channels.
fn refresh_loop(
    view: &Arc<Mutex<View>>,
    window: &Rc<window::StatusWindow>,
    dialog: settings::SettingsDialog,
    requests: Receiver<Reply>,
    actions: Sender<Action>,
) {
    // A window outlives the timer only if the host keeps it. A weak reference
    // lets the timer find out instead of assuming.
    let weak_root: WeakRef<gtk::Window> = WeakRef::new();
    weak_root.set(Some(&window.root));
    let view = Arc::clone(view);
    let window = Rc::clone(window);
    let mut shown = false;
    let mut aged = Instant::now();
    gtk::glib::timeout_add_local(TICK, move || {
        // The worker polls the history only while the window is open, so it
        // hears about each change once.
        let now_shown = window.root.is_visible();
        if now_shown != shown {
            shown = now_shown;
            let _ = actions.send(Action::WindowShown(shown));
        }
        // A hidden window does no work. The first snapshot after an open
        // writes the ages too.
        if now_shown && aged.elapsed() >= AGE_EVERY {
            aged = Instant::now();
            window.refresh_ages();
        }
        while let Ok(reply) = requests.try_recv() {
            deliver(reply, &weak_root, &window, &dialog);
        }
        let current = view.lock().expect("the view lock").clone();
        window.apply(&current);
        ControlFlow::Continue
    });
}

/// Carries out one reply from the worker. Every reply except `Show` belongs
/// to the dialog.
fn deliver(
    reply: Reply,
    window: &WeakRef<gtk::Window>,
    status: &window::StatusWindow,
    dialog: &settings::SettingsDialog,
) {
    match reply {
        Reply::Show => {
            if let Some(root) = window.upgrade() {
                root.present();
            }
        }
        // The rows of the window come with the drawing step.
        Reply::History(snapshot) => status.receive_history(snapshot),
        other => dialog.receive(other),
    }
}

/// Runs the tray on a thread that is not the GTK main loop.
fn run_worker(view: Arc<Mutex<View>>, start: Start) {
    let Start {
        proxy,
        actions,
        menu,
        show,
    } = start;
    let tray = NimbusTray::new(Arc::clone(&view), tray::icon_dir(), menu);
    // A desktop with no SNI host must not end the process. The icon appears
    // when a host arrives later.
    let Ok(handle) = tray.assume_sni_available(true).spawn() else {
        eprintln!("nimbus: the tray cannot register with the desktop");
        return;
    };
    let mut watch = Watch::default();
    loop {
        // A menu click wakes the loop at once, and the timeout is the poll
        // period. Sleeping instead would make every click wait up to two
        // seconds before the daemon hears it. The blocking call takes the
        // first click out of the channel, so it goes into the batch too.
        let mut batch = Vec::new();
        if let Ok(action) = actions.recv_timeout(POLL) {
            batch.push(action);
        }
        batch.extend(actions.try_iter());
        for action in batch {
            perform(action, &proxy, &view, &show, &mut watch);
        }
        let next = match proxy.state() {
            Ok(state) => View::Ready(state),
            Err(_) => offline_view(),
        };
        refresh_history(&mut watch, &proxy, &show, matches!(next, View::Ready(_)));
        if tray::refresh(&handle, &view, next) {
            // The panel dropped the item. The window keeps working, so the
            // process stays up and a panel reload brings the icon back.
            eprintln!("nimbus: the desktop closed the tray item");
        }
    }
}

/// Builds the view for a daemon that does not answer. A daemon with a bad
/// config exits after it refuses, so the tray runs the same check to find the
/// cause and names the fix. A valid config that says `paused` gives
/// `OfflinePaused`, because the daemon starts paused and the user paused it.
/// The daemon writes a default config on its first start, and the tray never
/// writes the file, so a missing file means the daemon never ran.
fn offline_view() -> View {
    match nimbusd::config::read() {
        Ok(Some(cfg)) => match cfg.check() {
            Err(err) => View::Offline(err.to_string()),
            Ok(()) if cfg.paused => View::OfflinePaused,
            Ok(()) => View::Offline(String::new()),
        },
        Ok(None) => View::Offline(String::new()),
        Err(err) => View::Offline(format!("{err:#}")),
    }
}

/// Carries out one action. The worker resolves the pause toggle, so the click
/// sites never need to know the current state. The windows belong to the main
/// thread, so those requests go across as a message.
fn perform(
    action: Action,
    proxy: &NimbusProxyBlocking<'_>,
    view: &Arc<Mutex<View>>,
    show: &Sender<Reply>,
    watch: &mut Watch,
) {
    let result = match action {
        Action::SyncNow => proxy.sync_now(),
        Action::TogglePaused
            if matches!(*view.lock().expect("the view lock"), View::OfflinePaused) =>
        {
            return resume_without_daemon(proxy);
        }
        // With no daemon, the pause slot offers to start the daemon.
        Action::TogglePaused
            if matches!(*view.lock().expect("the view lock"), View::Offline(_)) =>
        {
            return start_daemon(proxy);
        }
        Action::TogglePaused => {
            let next = !is_paused(&view.lock().expect("the view lock"));
            proxy.set_paused(next)
        }
        Action::ShowWindow => {
            let _ = show.send(Reply::Show);
            return;
        }
        Action::OpenSettings => return open_settings(proxy, show),
        Action::Resync => proxy.resync(),
        Action::SaveSettings(keys) => return save_settings(keys, proxy, show),
        Action::WindowShown(shown) => return watch.set_shown(shown),
    };
    if let Err(err) = result {
        eprintln!("nimbus: the daemon refused the request: {err}");
    }
}

/// Asks the daemon for the history while the window is open, and sends the
/// window a snapshot when it needs one. A daemon that does not answer leaves the
/// window with no rows, because old rows can make a stopped sync look healthy.
/// The settings come first, because the trim and the folder of the rows need
/// them. A view that is not ready skips both calls, because the state call
/// already failed and each more call could wait for the full timeout.
fn refresh_history(
    watch: &mut Watch,
    proxy: &NimbusProxyBlocking<'_>,
    show: &Sender<Reply>,
    ready: bool,
) {
    if !watch.is_shown() {
        return;
    }
    let asked = if ready {
        proxy.get_settings().and_then(|keys| {
            let fresh = proxy.get_history(watch.since())?;
            Ok((fresh, keys))
        })
    } else {
        Err(zbus::Error::Unsupported)
    };
    let snapshot = match asked {
        Ok((fresh, keys)) => {
            let local = nimbusd::config::LocalDir::new(Path::new(&keys.local)).path();
            let count = usize::try_from(keys.history_count).unwrap_or(usize::MAX);
            watch.update(fresh, local, count)
        }
        Err(_) => watch.lost(),
    };
    if let Some(snapshot) = snapshot {
        let _ = show.send(Reply::History(snapshot));
    }
}

/// Resumes a sync that was paused in an earlier session. No daemon runs, so
/// the tray clears the flag in the file and then starts the daemon, which
/// reads the file at start.
fn resume_without_daemon(proxy: &NimbusProxyBlocking<'_>) {
    let cleared = nimbusd::config::load().and_then(|mut cfg| {
        cfg.paused = false;
        nimbusd::config::save(&cfg)
    });
    match cleared {
        Ok(()) => start_daemon(proxy),
        Err(err) => eprintln!("nimbus: cannot resume: {err:#}"),
    }
}

/// Asks systemd to start the daemon unit, so the daemon runs under the restart
/// policy of the unit. A daemon with a bad config refuses again, and the next
/// poll shows the reason in the window.
fn start_daemon(proxy: &NimbusProxyBlocking<'_>) {
    let started = proxy.inner().connection().call_method(
        Some("org.freedesktop.systemd1"),
        "/org/freedesktop/systemd1",
        Some("org.freedesktop.systemd1.Manager"),
        "StartUnit",
        &("nimbusd.service", "replace"),
    );
    if let Err(err) = started {
        eprintln!("nimbus: systemd cannot start nimbusd.service: {err}");
    }
}

/// Reads the keys for the dialog.
fn open_settings(proxy: &NimbusProxyBlocking<'_>, show: &Sender<Reply>) {
    let reply = match proxy.get_settings() {
        Ok(keys) => Reply::Settings(keys),
        Err(err) => match refuse(err) {
            // No daemon answered, so the file holds the keys.
            Reply::Refused(text) if text.is_empty() => read_settings(),
            refused => refused,
        },
    };
    let _ = show.send(reply);
}

/// Reads the keys from the config file while no daemon runs. Without this,
/// the dialog shows the defaults, and a save writes them over the file. The
/// read does not create a missing file, so a missing file gives the defaults.
fn read_settings() -> Reply {
    match nimbusd::config::read() {
        Ok(cfg) => Reply::Settings(cfg.unwrap_or_default().settings()),
        Err(err) => Reply::Refused(format!("{err:#}")),
    }
}

/// Writes the keys. The daemon checks them, so a refusal arrives here and goes
/// into the dialog.
fn save_settings(
    keys: nimbus_ipc::Settings,
    proxy: &NimbusProxyBlocking<'_>,
    show: &Sender<Reply>,
) {
    let reply = match proxy.set_settings(keys.clone()) {
        Ok(()) => Reply::Saved,
        Err(err) => match refuse(err) {
            // No daemon answered, so no daemon writes the file either.
            Reply::Refused(text) if text.is_empty() => write_settings(&keys),
            refused => refused,
        },
    };
    let _ = show.send(reply);
}

/// Writes the keys into the config file while no daemon runs, so a save never
/// drops an edit. A move raises the resync flag, so the next daemon asks for
/// the resync. A file that does not load stays as it is, and its error goes
/// into the dialog.
fn write_settings(keys: &nimbus_ipc::Settings) -> Reply {
    let written = nimbusd::config::load().and_then(|mut cfg| {
        cfg.apply(keys)?;
        nimbusd::config::save(&cfg)
    });
    match written {
        Ok(()) => Reply::Saved,
        Err(err) => Reply::Refused(format!("{err:#}")),
    }
}

/// The error name of a daemon refusal. The bus answers a call to a missing
/// daemon with a method error too, `ServiceUnknown` with "The name is not
/// activatable", so only this name carries words from the daemon.
const REFUSED: &str = "org.freedesktop.DBus.Error.InvalidArgs";

/// Turns a failed call into a reply.
///
/// Only a refusal from the daemon carries words that the user can act on.
/// Every other failure is the bus or the daemon process. The status window
/// already says why the daemon is not running, so the dialog shows nothing.
fn refuse(err: zbus::Error) -> Reply {
    let said = match &err {
        zbus::Error::MethodError(name, detail, _) if name.as_str() == REFUSED => detail.clone(),
        zbus::Error::FDO(inner) => match inner.as_ref() {
            zbus::fdo::Error::InvalidArgs(text) => Some(text.clone()),
            _ => None,
        },
        _ => None,
    };
    eprintln!("nimbus: the request failed: {err}");
    Reply::Refused(said.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A daemon that does not run sends a name error, which carries no words for
    // the user. The status window names the cause, so the dialog shows none.
    #[test]
    fn a_missing_daemon_leaves_the_dialog_quiet() {
        assert_eq!(
            refuse(zbus::Error::NameTaken),
            Reply::Refused(String::new())
        );
    }

    // The bus answers for a daemon that is not on it. Its words name the bus,
    // not the cause, so the dialog must not show them. The daemon's own
    // refusal reaches the dialog unchanged.
    #[test]
    fn only_the_daemon_refusal_reaches_the_dialog() {
        let bus = zbus::fdo::Error::ServiceUnknown(String::from("The name is not activatable"));
        assert_eq!(
            refuse(zbus::Error::FDO(Box::new(bus))),
            Reply::Refused(String::new())
        );
        let daemon = zbus::fdo::Error::InvalidArgs(String::from("The remote is empty."));
        assert_eq!(
            refuse(zbus::Error::FDO(Box::new(daemon))),
            Reply::Refused(String::from("The remote is empty."))
        );
    }
}
