use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher};

use nimbus_ipc::{BUS_NAME, INTERFACE, OBJECT_PATH, State};

use nimbusd::config;
use nimbusd::engine::{Engine, Event};
use nimbusd::rclone;
use nimbusd::service::NimbusService;

/// The loop wakes at least this often. The loop also wakes on every message,
/// so the value only sets the slowest response to the quiet time.
const TICK: Duration = Duration::from_secs(1);

/// Returns true when a file change must start a run.
///
/// The function drops read events on purpose. rclone reads the local folder
/// during every run, so a read event would start the next run without end.
/// The function also drops the catch-all kind, because notify uses it for
/// events the backend does not describe. The interval covers a missed change.
fn is_trigger(kind: &EventKind) -> bool {
    kind.is_create() || kind.is_modify() || kind.is_remove()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0)
}

/// What one turn of the loop decided. The caller reads this after it unlocks
/// the engine, so no side effect ever runs while the lock is held.
struct Turn {
    run: Option<(config::Config, Arc<AtomicBool>)>,
    save: Option<config::Config>,
    state: State,
}

fn main() -> Result<()> {
    let cfg = config::load()?;
    if let Err(err) = cfg.check() {
        eprintln!("nimbusd: {err}");
        eprintln!("nimbusd: the config file is {}", config::path().display());
        eprintln!("nimbusd: fix the settings, then start the daemon again");
        std::process::exit(1);
    }
    eprintln!(
        "nimbusd: syncing {} with {}",
        cfg.local.display(),
        cfg.remote_path()
    );
    serve(cfg)
}

fn serve(cfg: config::Config) -> Result<()> {
    let engine = Arc::new(Mutex::new(Engine::new(cfg)));
    let (tx, rx) = mpsc::channel::<Event>();

    // The daemon takes the bus name before it creates the watcher. A second
    // daemon must stop here, before it starts a run on the same folder.
    let service = NimbusService::new(Arc::clone(&engine), tx.clone());
    let conn = zbus::blocking::connection::Builder::session()
        .context("the daemon cannot reach the session bus")?
        .serve_at(OBJECT_PATH, service)
        .context("the daemon cannot serve the object")?
        .build()
        .context("the daemon cannot connect to the session bus")?;

    // The builder throws away the reply from the bus, so a name that another
    // daemon already owns looks like a success. This call returns the reply, so
    // the daemon can refuse to start. Today zbus turns a taken name into an
    // error, so the check below is the second line of defence. The flag means
    // the daemon must own the name now, and it must not queue for it.
    let reply = conn
        .request_name_with_flags(BUS_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
        .context("the daemon cannot ask for the bus name")?;
    if !matches!(
        reply,
        zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner
    ) {
        eprintln!("nimbusd: another daemon already holds {BUS_NAME}, reply {reply:?}");
        eprintln!("nimbusd: stop that daemon, then start this one again");
        std::process::exit(1);
    }
    eprintln!("nimbusd: serving {INTERFACE} at {OBJECT_PATH}");

    let local = engine
        .lock()
        .expect("the engine lock")
        .config()
        .local
        .clone();
    let tx_watch = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res
            && is_trigger(&event.kind)
        {
            let _ = tx_watch.send(Event::Changed);
        }
    })
    .context("the daemon cannot create the file watcher")?;
    watcher
        .watch(&local, RecursiveMode::Recursive)
        .with_context(|| format!("the daemon cannot watch {}", local.display()))?;

    let mut last_sent: Option<State> = None;
    loop {
        // The blocking receive must run without the lock. A property read on
        // another thread waits for this lock, and a receive holds it for a
        // whole tick. The receive comes first for that reason.
        let first = match rx.recv_timeout(TICK) {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let turn = {
            let mut e = engine.lock().expect("the engine lock");
            if let Some(event) = first {
                e.on_event(event, Instant::now(), unix_now());
            }
            // Two file changes can arrive in the same tick. The drain applies
            // both before the loop reads the quiet time.
            while let Ok(event) = rx.try_recv() {
                e.on_event(event, Instant::now(), unix_now());
            }
            Turn {
                run: e
                    .wants_run(Instant::now())
                    .then(|| (e.config().clone(), e.pause_flag())),
                save: e.take_dirty().then(|| e.config().clone()),
                state: e.snapshot().clone(),
            }
        };
        if let Some((cfg, pause)) = turn.run {
            let tx = tx.clone();
            std::thread::spawn(move || rclone::run(&cfg, pause, &tx));
        }
        if let Some(cfg) = turn.save {
            config::save(&cfg).context("the daemon cannot write the config file")?;
        }
        // A failed send leaves the flag unset, so the next turn tries again.
        // A lost tray icon must not stop a sync.
        if last_sent.as_ref() != Some(&turn.state) {
            match conn.emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, "Changed", &turn.state) {
                Ok(()) => last_sent = Some(turn.state),
                Err(err) => eprintln!("nimbusd: the daemon cannot send the state: {err}"),
            }
        }
        // The watcher must stay alive for the whole loop. When it drops, the
        // kernel closes the notify descriptor and the daemon sees no change.
        let _ = &watcher;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode,
    };

    #[test]
    fn a_create_event_triggers_a_run() {
        assert!(is_trigger(&EventKind::Create(CreateKind::File)));
        assert!(is_trigger(&EventKind::Create(CreateKind::Folder)));
    }

    #[test]
    fn a_modify_event_triggers_a_run() {
        assert!(is_trigger(&EventKind::Modify(ModifyKind::Name(
            RenameMode::Any
        ))));
        assert!(is_trigger(&EventKind::Modify(ModifyKind::Data(
            DataChange::Any
        ))));
    }

    #[test]
    fn a_remove_event_triggers_a_run() {
        assert!(is_trigger(&EventKind::Remove(RemoveKind::File)));
        assert!(is_trigger(&EventKind::Remove(RemoveKind::Folder)));
    }

    // rclone reads the local folder during every run. A read event that
    // triggers a run starts the next run without end.
    #[test]
    fn a_read_event_does_not_trigger_a_run() {
        assert!(!is_trigger(&EventKind::Access(AccessKind::Read)));
        assert!(!is_trigger(&EventKind::Access(AccessKind::Open(
            AccessMode::Any
        ))));
    }

    #[test]
    fn an_unknown_event_does_not_trigger_a_run() {
        assert!(!is_trigger(&EventKind::Any));
        assert!(!is_trigger(&EventKind::Other));
    }
}
