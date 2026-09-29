use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher};

use nimbusd::config;
use nimbusd::engine::{Engine, Event};
use nimbusd::rclone;

/// The loop wakes at least this often. The loop also wakes on every message,
/// so the value only sets the slowest response to the quiet time.
const TICK: Duration = Duration::from_secs(1);

/// This function returns true when a file change must start a run.
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
    let (tx, rx) = mpsc::channel::<Event>();

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
        .watch(&cfg.local, RecursiveMode::Recursive)
        .with_context(|| format!("the daemon cannot watch {}", cfg.local.display()))?;

    let mut engine = Engine::new(cfg);
    loop {
        match rx.recv_timeout(TICK) {
            Ok(event) => engine.on_event(event, Instant::now(), unix_now()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Two file changes can arrive in the same tick. The drain applies both
        // before the loop reads the quiet time.
        while let Ok(event) = rx.try_recv() {
            engine.on_event(event, Instant::now(), unix_now());
        }
        if engine.wants_run(Instant::now()) {
            let cfg = engine.config().clone();
            let pause = engine.pause_flag();
            let tx = tx.clone();
            std::thread::spawn(move || rclone::run(&cfg, pause, &tx));
        }
        if engine.take_dirty() {
            config::save(engine.config()).context("the daemon cannot write the config file")?;
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
