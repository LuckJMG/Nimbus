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
use view::{Action, View, is_paused};

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
    match start() {
        Ok(app) => app.run(),
        Err(err) => {
            eprintln!("nimbus: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// GTK needs the main thread, and the zbus blocking calls must not run inside
/// a main loop. So the window lives on the main thread and the tray runs on a
/// worker thread of its own.
fn start() -> Result<gtk::Application> {
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
    let (show_tx, show_rx) = mpsc::channel::<()>();

    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gtk::gio::ApplicationFlags::empty())
        .build();

    let ui = Rc::new(Ui {
        view: Arc::clone(&view),
        action_tx: action_tx.clone(),
        hold: RefCell::new(None),
        window: RefCell::new(None),
        // The activate callback runs more than once, so it cannot move these
        // out on the first call. The first call takes them.
        start: RefCell::new(Some(Start {
            proxy,
            actions: action_rx,
            menu: action_tx,
            show: show_tx,
            requests: show_rx,
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

        let start = ui
            .start
            .borrow_mut()
            .take()
            .expect("the first activation starts the worker");
        refresh_loop(&ui, &window, start.requests);
        window.root.present();

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

/// What the first activation hands to the worker thread. The window and the
/// worker share one channel, so both kinds of request use the same router.
struct Start {
    proxy: NimbusProxyBlocking<'static>,
    actions: Receiver<Action>,
    menu: Sender<Action>,
    show: Sender<()>,
    requests: Receiver<()>,
}

/// The state the activate callback needs on the main thread.
struct Ui {
    view: Arc<Mutex<View>>,
    /// Cloned into the window, which sends the button presses.
    action_tx: Sender<Action>,
    /// Dropping the guard would release the hold, so the guard stays here.
    hold: RefCell<Option<gtk::gio::ApplicationHoldGuard>>,
    window: RefCell<Option<Rc<window::App>>>,
    start: RefCell<Option<Start>>,
    started: AtomicBool,
}

/// The one main loop timer. It presents the window when the worker asks, and
/// it rewrites the labels when the view moved.
fn refresh_loop(ui: &Rc<Ui>, app: &Rc<window::App>, requests: Receiver<()>) {
    // The window outlives the timer only if the host keeps it. A weak
    // reference lets the timer find out instead of assuming.
    let weak: WeakRef<gtk::Window> = WeakRef::new();
    let view = Arc::clone(&ui.view);
    let shown = Rc::clone(app);
    let mut last: Option<View> = None;
    gtk::glib::timeout_add_local(TICK, move || {
        while requests.try_recv().is_ok() {
            if let Some(root) = weak.upgrade() {
                root.present();
            }
        }
        let current = view.lock().expect("the view lock").clone();
        if last.as_ref() != Some(&current) {
            shown.apply(&current, unix_now());
            last = Some(current);
        }
        ControlFlow::Continue
    });
}

/// Everything the tray needs, on a thread that is not the GTK main loop.
struct Worker {
    view: Arc<Mutex<View>>,
    proxy: NimbusProxyBlocking<'static>,
    actions: Receiver<Action>,
    menu: Sender<Action>,
    show: Sender<()>,
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
/// sites never need to know the current state. The window belongs to the main
/// thread, so that request goes across as a message.
fn send(
    action: Action,
    proxy: &NimbusProxyBlocking<'_>,
    view: &Arc<Mutex<View>>,
    show: &Sender<()>,
) {
    let result = match action {
        Action::SyncNow => proxy.sync_now(),
        Action::TogglePaused => {
            let next = !is_paused(&view.lock().expect("the view lock"));
            proxy.set_paused(next)
        }
        Action::ShowWindow => {
            let _ = show.send(());
            return;
        }
    };
    if let Err(err) = result {
        eprintln!("nimbus: the daemon refused the request: {err}");
    }
}
