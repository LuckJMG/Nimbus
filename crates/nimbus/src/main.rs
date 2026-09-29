mod tray;

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use ksni::blocking::TrayMethods;
use nimbus_ipc::NimbusProxyBlocking;

use tray::{Icon, NimbusTray, View};

/// The tray reads the state on this period. The engine sends one update for
/// each whole percent, so a longer period would only add delay.
const POLL: Duration = Duration::from_secs(2);

/// The daemon must answer within this time. The zbus default is 25 seconds,
/// which would freeze the tray while the daemon stops answering.
const TIMEOUT: Duration = Duration::from_secs(3);

fn main() -> Result<()> {
    let conn = zbus::blocking::connection::Builder::session()
        .context("the tray cannot reach the session bus")?
        .method_timeout(TIMEOUT)
        .build()
        .context("the tray cannot connect to the session bus")?;
    let proxy = NimbusProxyBlocking::builder(&conn)
        // The proxy caches properties by default, and it refreshes the cache
        // only when the daemon sends the standard PropertiesChanged signal.
        // The daemon sends its own signal, so a cached proxy would report a
        // stale state forever, and would never notice that the daemon stopped.
        .cache_properties(zbus::proxy::CacheProperties::No)
        .build()
        .context("the tray cannot build the proxy")?;

    let view = Arc::new(Mutex::new(View::Offline));
    let icon = Icon::resolve();
    eprintln!(
        "nimbus: tray icon {} with theme path {:?}",
        icon.name, icon.theme_path
    );
    let tray = NimbusTray::new(Arc::clone(&view), icon);
    // A desktop with no SNI host must not end the process. The icon appears
    // when a host arrives later.
    let handle = tray
        .assume_sni_available(true)
        .spawn()
        .context("the tray cannot start")?;

    loop {
        let next = match proxy.state() {
            Ok(state) => View::Ready(state),
            Err(_) => View::Offline,
        };
        // The lock must be released before the update. The update makes the
        // tray service read the view again, and the service runs on another
        // thread. Holding the lock across the call would wait for itself.
        let changed = {
            let mut guard = view.lock().expect("the view lock");
            if *guard == next {
                false
            } else {
                *guard = next;
                true
            }
        };
        // The Tray methods read the view on each call. The update only makes
        // the host read them again.
        if changed && handle.update(|_: &mut NimbusTray| {}).is_none() {
            eprintln!("nimbus: the tray host closed the item");
            break;
        }
        thread::sleep(POLL);
    }
    Ok(())
}
