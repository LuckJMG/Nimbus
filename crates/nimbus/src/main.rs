mod settings;
mod tray;
mod view;
mod window;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use gtk::glib::{ControlFlow, ExitCode, WeakRef};
use gtk::prelude::*;
use ksni::blocking::TrayMethods;
use nimbus_ipc::NimbusProxyBlocking;

use tray::{Icon, NimbusTray};
use view::{Action, Reply, View, is_paused};

/// The seconds since the Unix epoch, which is the unit of `State::last_run`.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The application id. GApplication uses it to hand an activation to the
/// process that already holds the name, which keeps one icon in the panel.
const APP_ID: &str = "io.github.luckjmg.Nimbus";

/// The worker reads the state on this period. The engine sends one update for
/// each whole percent, so a longer period would only add delay.
const POLL: Duration = Duration::from_secs(2);

/// The main thread checks the window on this period, so this is the delay
/// before a tray click brings the window up.
const TICK: Duration = Duration::from_millis(100);

/// The daemon must answer within this time. The zbus default is 25 seconds,
/// which would freeze the tray while the daemon stops answering.
const TIMEOUT: Duration = Duration::from_secs(3);

fn main() -> ExitCode {
    // The autostart entry passes --hidden, so a login starts the tray and no
    // window. GApplication refuses an option that it does not know, so the tray
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

    let view = Arc::new(Mutex::new(View::Offline));
    let (action_tx, action_rx) = mpsc::channel::<Action>();
    let (reply_tx, reply_rx) = mpsc::channel::<Reply>();

    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::empty())
        .build();

    let ui = Rc::new(Ui {
        view: Arc::clone(&view),
        action_tx: action_tx.clone(),
        hold: RefCell::new(None),
        window: RefCell::new(None),
        dialog: RefCell::new(None),
        // The activate callback runs more than once, so it cannot move these
        // out on the first call. The first call takes them.
        start: RefCell::new(Some(Start {
            proxy,
            actions: action_rx,
            menu: action_tx,
            show: reply_tx,
            requests: reply_rx,
        })),
        started: AtomicBool::new(false),
    });

    app.connect_activate(move |app| {
        if ui.started.swap(true, Ordering::SeqCst) {
            // A second process handed its activation to this one, so only the
            // window has to come forward.
            if let Some(window) = ui.window.borrow().as_ref() {
                window.root.present();
            }
            return;
        }
        // The tray keeps the process alive with no window, so this hold has to
        // outlive the call. It lives in the captured state.
        *ui.hold.borrow_mut() = Some(app.hold());

        let window = Rc::new(window::build(app, ui.action_tx.clone()));
        *ui.window.borrow_mut() = Some(Rc::clone(&window));
        // The dialog is built once and never shown here, because it opens only
        // from the menu. A window with no values would show empty fields.
        let dialog = Rc::new(settings::build(app, ui.action_tx.clone()));
        *ui.dialog.borrow_mut() = Some(Rc::clone(&dialog));

        let start = ui
            .start
            .borrow_mut()
            .take()
            .expect("the first activation starts the worker");
        refresh_loop(&ui, &window, &dialog, start.requests);
        // A later activation comes from a second process, for example a start
        // from the app menu, so it presents the window whatever this flag says.
        if !hidden {
            window.root.present();
        }

        let worker = Worker {
            view: Arc::clone(&view),
            proxy: start.proxy,
            actions: start.actions,
            menu: start.menu,
            show: start.show,
        };
        if let Err(err) = thread::Builder::new()
            .name("nimbus-tray".into())
            .spawn(move || worker.run())
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
    requests: Receiver<Reply>,
}

/// The state the activate callback needs on the main thread.
struct Ui {
    view: Arc<Mutex<View>>,
    /// Cloned into both windows, which send the button presses.
    action_tx: Sender<Action>,
    /// Dropping the guard would release the hold, so the guard stays here.
    hold: RefCell<Option<gtk::gio::ApplicationHoldGuard>>,
    window: RefCell<Option<Rc<window::App>>>,
    /// The dialog is built beside the status window and then reused, because a
    /// rebuild would throw away a half-typed value.
    dialog: RefCell<Option<Rc<settings::App>>>,
    start: RefCell<Option<Start>>,
    started: AtomicBool,
}

/// The one main loop timer. It moves the windows when the worker asks, and it
/// rewrites the labels when the view moved.
///
/// One timer serves both windows, because a second timer would need a second
/// glib source and both read the same channels.
fn refresh_loop(
    ui: &Rc<Ui>,
    app: &Rc<window::App>,
    dialog: &Rc<settings::App>,
    requests: Receiver<Reply>,
) {
    // A window outlives the timer only if the host keeps it. A weak reference
    // lets the timer find out instead of assuming.
    let weak: WeakRef<gtk::Window> = WeakRef::new();
    weak.set(Some(&app.root));
    // The dialog needs its own handle for the fields, so the timer keeps a
    // strong reference. A weak one would leave nothing to write the keys into.
    let editor = Rc::clone(dialog);
    let view = Arc::clone(&ui.view);
    let shown = Rc::clone(app);
    let mut last: Option<View> = None;
    gtk::glib::timeout_add_local(TICK, move || {
        while let Ok(reply) = requests.try_recv() {
            deliver(reply, &weak, &editor);
        }
        let current = view.lock().expect("the view lock").clone();
        if last.as_ref() != Some(&current) {
            shown.apply(&current, unix_now());
            last = Some(current);
        }
        ControlFlow::Continue
    });
}

/// Carries out one reply from the worker.
///
/// A refusal shows the dialog, because the text belongs there. The dialog comes
/// forward with the message, because a hidden dialog cannot show it.
fn deliver(reply: Reply, window: &WeakRef<gtk::Window>, dialog: &settings::App) {
    match reply {
        Reply::Show => {
            if let Some(root) = window.upgrade() {
                root.present();
            }
        }
        Reply::Settings(keys) => {
            dialog.apply(&keys);
            dialog.root.present();
        }
        Reply::Saved => dialog.close(),
        Reply::Refused(text) => {
            dialog.refuse(&text);
            dialog.root.present();
        }
    }
}

/// Everything the tray needs, on a thread that is not the GTK main loop.
struct Worker {
    view: Arc<Mutex<View>>,
    proxy: NimbusProxyBlocking<'static>,
    actions: Receiver<Action>,
    menu: Sender<Action>,
    show: Sender<Reply>,
}

impl Worker {
    fn run(self) {
        let Worker {
            view,
            proxy,
            actions,
            menu,
            show,
        } = self;
        let tray = NimbusTray::new(Arc::clone(&view), Icon::resolve(), menu);
        // A desktop with no SNI host must not end the process. The icon appears
        // when a host arrives later.
        let Ok(handle) = tray.assume_sni_available(true).spawn() else {
            eprintln!("nimbus: the tray cannot register with the desktop");
            return;
        };
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
                send(action, &proxy, &view, &show);
            }
            let next = match proxy.state() {
                Ok(state) => View::Ready(state),
                Err(_) => View::Offline,
            };
            if tray::refresh(&handle, &view, next) {
                // The panel dropped the item. The window keeps working, so the
                // process stays up and a panel reload brings the icon back.
                eprintln!("nimbus: the desktop closed the tray item");
            }
        }
    }
}

/// Carries out one action. The worker resolves the pause toggle, so the click
/// sites never need to know the current state. The windows belong to the main
/// thread, so those requests go across as a message.
fn send(
    action: Action,
    proxy: &NimbusProxyBlocking<'_>,
    view: &Arc<Mutex<View>>,
    show: &Sender<Reply>,
) {
    let result = match action {
        Action::SyncNow => proxy.sync_now(),
        Action::TogglePaused => {
            let next = !is_paused(&view.lock().expect("the view lock"));
            proxy.set_paused(next)
        }
        Action::ShowWindow => {
            let _ = show.send(Reply::Show);
            return;
        }
        Action::OpenSettings => return open_settings(proxy, show),
        Action::SaveSettings(keys) => return save_settings(keys, proxy, show),
    };
    if let Err(err) = result {
        eprintln!("nimbus: the daemon refused the request: {err}");
    }
}

/// Reads the keys for the dialog.
///
/// A daemon that does not run answers with a refusal, so the dialog says why
/// it cannot open instead of showing empty fields.
fn open_settings(proxy: &NimbusProxyBlocking<'_>, show: &Sender<Reply>) {
    let reply = match proxy.get_settings() {
        Ok(keys) => Reply::Settings(keys),
        Err(err) => refuse(err),
    };
    let _ = show.send(reply);
}

/// Writes the keys. The daemon checks them, so a refusal arrives here and goes
/// into the dialog.
fn save_settings(
    keys: nimbus_ipc::Settings,
    proxy: &NimbusProxyBlocking<'_>,
    show: &Sender<Reply>,
) {
    let reply = match proxy.set_settings(keys) {
        Ok(()) => Reply::Saved,
        Err(err) => refuse(err),
    };
    let _ = show.send(reply);
}

/// Turns a failed call into a reply.
///
/// Only a reply from the daemon carries words that the user can act on. Every
/// other failure is the bus or the daemon process, so the dialog says that
/// the daemon is not running instead of passing on a zbus message.
fn refuse(err: zbus::Error) -> Reply {
    let said = match &err {
        zbus::Error::MethodError(_, detail, _) => detail.clone(),
        zbus::Error::FDO(inner) => Some(inner.to_string()),
        _ => None,
    };
    let text = said_or_missing(said);
    eprintln!("nimbus: the daemon refused the request: {text}");
    Reply::Refused(text)
}

/// The words for the dialog, or the missing-daemon text.
///
/// Only a refusal carries words that the user can act on. Every other failure
/// came from the bus or from the daemon process, and a zbus message there names
/// a transport problem instead of a cause.
fn said_or_missing(said: Option<String>) -> String {
    said.unwrap_or_else(|| String::from("The daemon is not running"))
}

#[cfg(test)]
mod tests {
    use super::*;

    // A daemon that does not run sends a name error, which carries no words for
    // the user. The dialog then names the cause instead of passing on a zbus
    // message about the bus.
    #[test]
    fn a_missing_daemon_says_so_in_the_dialog() {
        assert_eq!(
            refuse(zbus::Error::NameTaken),
            Reply::Refused(String::from("The daemon is not running"))
        );
    }

    // A refusal arrives as a method error, and its detail holds the words that the
    // daemon wrote. The dialog shows them without the D-Bus name in front.
    #[test]
    fn a_refusal_shows_the_words_from_the_daemon() {
        let said = said_or_missing(Some(String::from(
            "the config key remote is empty. Set a remote name.",
        )));
        assert_eq!(said, "the config key remote is empty. Set a remote name.");
    }

    // A bus failure reaches the same dialog, and it has no words to pass on.
    #[test]
    fn a_bus_failure_says_the_daemon_is_not_running() {
        assert_eq!(said_or_missing(None), "The daemon is not running");
    }
}
