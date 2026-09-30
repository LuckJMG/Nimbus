use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
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

/// The seconds since the Unix epoch, which is the unit of `State::last_run`.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Returns true when a file change must start a run.
///
/// The function drops read events on purpose. rclone reads the local folder
/// during every run, so a read event would start the next run without end.
/// The function also drops the catch-all kind, because notify uses it for
/// events the backend does not describe. The interval covers a missed change.
fn is_trigger(kind: &EventKind) -> bool {
    kind.is_create() || kind.is_modify() || kind.is_remove()
}

/// Reports whether the bus refused the name because another daemon holds it.
/// Only that one error is a refusal, because only the user can free the name.
/// Every other error must still fail, so that systemd retries the daemon.
fn name_is_taken(err: &zbus::Error) -> bool {
    matches!(err, zbus::Error::NameTaken)
}

/// Claims the bus name. Returns false when another daemon holds it, because
/// only the user can free the name and a retry would fail the same way.
fn claim_bus_name(conn: &zbus::blocking::Connection) -> Result<bool> {
    // The builder throws away the reply from the bus, so a name that another
    // daemon already owns looks like a success. This call returns the reply, so
    // the daemon can refuse to start. Today zbus turns a taken name into an
    // error, so the reply check below is the second line of defence. The flag
    // means the daemon must own the name now, and it must not queue for it.
    let reply = match conn
        .request_name_with_flags(BUS_NAME, zbus::fdo::RequestNameFlags::DoNotQueue.into())
    {
        Ok(reply) => reply,
        // A clean stop exits with zero, which keeps systemd from restarting
        // the daemon in a loop against the daemon that holds the name.
        Err(err) if name_is_taken(&err) => {
            eprintln!("nimbusd: another daemon already holds {BUS_NAME}");
            eprintln!("nimbusd: stop that daemon, then start this one again");
            return Ok(false);
        }
        Err(err) => return Err(err).context("the daemon cannot ask for the bus name"),
    };
    if matches!(
        reply,
        zbus::fdo::RequestNameReply::PrimaryOwner | zbus::fdo::RequestNameReply::AlreadyOwner
    ) {
        return Ok(true);
    }
    eprintln!("nimbusd: another daemon already holds {BUS_NAME}, reply {reply:?}");
    eprintln!("nimbusd: stop that daemon, then start this one again");
    Ok(false)
}

/// Watches the local folder and sends an event for every write.
fn watch_folder(events: &Sender<Event>, local: &Path) -> Result<notify::RecommendedWatcher> {
    let events = events.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res
            && is_trigger(&event.kind)
        {
            let _ = events.send(Event::Changed);
        }
    })
    .context("the daemon cannot create the file watcher")?;
    watcher
        .watch(local, RecursiveMode::Recursive)
        .with_context(|| format!("the daemon cannot watch {}", local.display()))?;
    Ok(watcher)
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
        eprintln!(
            "nimbusd: the config file is {}",
            config::default_file().display()
        );
        eprintln!("nimbusd: fix the settings, then start the daemon again");
        // A clean refusal exits with zero. Only the user can fix the settings,
        // so a restart would fail in the same way and fill the journal.
        return Ok(());
    }
    eprintln!(
        "nimbusd: syncing {} with {}",
        cfg.local.path().display(),
        cfg.remote_path()
    );
    serve(cfg)
}

fn serve(cfg: config::Config) -> Result<()> {
    // The run threads read this flag and the loop is the only writer, because
    // the loop is where a SetPaused event lands.
    let pause = Arc::new(AtomicBool::new(cfg.paused));
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

    // A clean refusal exits with zero, for the same reason as a bad config. The
    // loop has not started, so the return reaches main and ends the process
    // with a zero code.
    if !claim_bus_name(&conn)? {
        return Ok(());
    }
    eprintln!("nimbusd: serving {INTERFACE} at {OBJECT_PATH}");

    let local = engine
        .lock()
        .expect("the engine lock")
        .config()
        .local
        .path();
    // The watcher must stay alive for the whole loop. When it drops, the
    // kernel closes the notify descriptor and the daemon sees no change.
    let _watcher = watch_folder(&tx, &local)?;

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
            // Two file changes can arrive in the same tick. The drain applies
            // both before the loop reads the quiet time.
            for event in first.into_iter().chain(rx.try_iter()) {
                if let Event::SetPaused(paused) = &event {
                    pause.store(*paused, Ordering::Relaxed);
                }
                e.on_event(event, Instant::now(), unix_now());
            }
            Turn {
                run: e
                    .wants_run(Instant::now())
                    .then(|| (e.config().clone(), Arc::clone(&pause))),
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
        announce(&conn, &mut last_sent, &turn.state);
    }
    Ok(())
}

/// Tells the clients when the state moved. The engine cannot send the signal,
/// because the loop must not hold the lock across a blocking bus call.
fn announce(conn: &zbus::blocking::Connection, last_sent: &mut Option<State>, state: &State) {
    // A failed send leaves the flag unset, so the next turn tries again.
    // A lost tray icon must not stop a sync.
    if last_sent.as_ref() == Some(state) {
        return;
    }
    match conn.emit_signal(None::<&str>, OBJECT_PATH, INTERFACE, "Changed", state) {
        Ok(()) => *last_sent = Some(state.clone()),
        Err(err) => eprintln!("nimbusd: the daemon cannot send the state: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode,
    };

    // rclone reads the local folder during every run, so a read event that
    // triggers a run starts the next run without end. The catch-all kinds
    // carry no description, and the interval covers a missed change.
    #[test]
    fn only_a_write_event_triggers_a_run() {
        for kind in [
            EventKind::Create(CreateKind::File),
            EventKind::Create(CreateKind::Folder),
            EventKind::Modify(ModifyKind::Name(RenameMode::Any)),
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            EventKind::Remove(RemoveKind::File),
            EventKind::Remove(RemoveKind::Folder),
        ] {
            assert!(is_trigger(&kind), "{kind:?} must start a run");
        }
        for kind in [
            EventKind::Access(AccessKind::Read),
            EventKind::Access(AccessKind::Open(AccessMode::Any)),
            EventKind::Any,
            EventKind::Other,
        ] {
            assert!(!is_trigger(&kind), "{kind:?} must not start a run");
        }
    }

    // A taken name is the one error that only the user can fix. Every other
    // error must still fail the start, or the daemon never retries.
    #[test]
    fn only_a_taken_name_is_a_refusal() {
        assert!(name_is_taken(&zbus::Error::NameTaken));
        assert!(!name_is_taken(&zbus::Error::InvalidReply));
        assert!(!name_is_taken(&zbus::Error::MissingParameter("path")));
        assert!(!name_is_taken(&zbus::Error::Failure(String::from(
            "bus is gone"
        ))));
    }
}
