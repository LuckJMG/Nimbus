# Nimbus

A Google Drive sync tool for Linux. A daemon runs `rclone bisync` on a schedule and
when files change. A GTK4 tray client talks to it over D-Bus.

## State

Three crates exist. The daemon and the tray both work against a live bus. The
systemd unit, the desktop entry, the D-Bus service file, and the icon exist under
`data/`. The `justfile` holds every install recipe, and it is the only copy of the
file list. `README.md` holds the user-facing tables: the config keys, the D-Bus
API, the menu, and the troubleshooting steps. `docs/screenshot.png` shows the
window. No CI runs.

| Crate | Role |
| --- | --- |
| `nimbus-ipc` | The D-Bus contract. Names, `State`, `Phase`, one proxy trait. No logic. |
| `nimbusd` | The daemon. Library plus binary. |
| `nimbus` | The tray. One binary: the window, the icon, and the menu. |

The binaries are `nimbusd` and `nimbus`. The daemon needs a session bus and refuses
to start when another daemon already holds `io.github.luckjmg.nimbus`. The tray
starts with no daemon and shows "The daemon is not running".

## Commands

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

No CI runs these. Run all three before every commit, in that order. `just check`
runs the same three. 83 tests pass today: 2 in `nimbus-ipc`, 53 in the `nimbusd`
library, 2 in the `nimbusd` binary, and 26 in `nimbus`.

```console
cargo test -p nimbusd                     # one crate
cargo test -p nimbusd --lib rclone::      # one module
cargo test -p nimbusd --lib rclone::tests::parse_progress_reads_the_byte_line
```

## The live check

The unit tests do not cover the bus, the watcher, the panel, or rclone. Those need a
real run. Set `XDG_CONFIG_HOME` to a scratch directory, because the daemon writes a
config file on first start.

Free the bus name first. One daemon owns `io.github.luckjmg.nimbus` in a session,
so stop an installed one and quit a running tray:

```console
systemctl --user stop nimbusd.service
```

Without that, the test daemon exits with "another daemon already holds" and a test
tray hands its window to the installed one, so you appear to test the old build.
`pgrep -a nimbus` must show nothing before the run.

An `alias` remote with an absolute path is the only test remote that works. It
resolves the same way from any working directory.

```console
mkdir -p /tmp/scratch/cfg/nimbus /tmp/scratch/local /tmp/scratch/remote/Nimbus
cat > /tmp/scratch/cfg/rclone.conf <<'CONF'
[drive]
type = alias
remote = /tmp/scratch/remote
CONF
```

The destination folder must exist before the first run. Google Drive creates
folders, so this affects test remotes only.

The first start writes a config with the defaults, then refuses to run, because
`~/Nimbus` does not exist. That refusal is the signal to edit the scratch config.
Set the watched folder, the remote, and short timers:

```console
sed -i 's|^remote = .*|remote = "drive"|; s|^local = .*|local = "/tmp/scratch/local"|; s|^interval_secs = .*|interval_secs = 60|; s|^debounce_secs = .*|debounce_secs = 3|' /tmp/scratch/cfg/nimbus/config.toml
```

Short timers matter. The defaults wait 900 seconds on the interval and 30 seconds
on the debounce, so a manual run looks broken.

```console
cargo build --workspace
XDG_CONFIG_HOME=/tmp/scratch/cfg RCLONE_CONFIG=/tmp/scratch/cfg/rclone.conf ./target/debug/nimbusd &
./target/debug/nimbus &

busctl --user introspect io.github.luckjmg.nimbus /io/github/luckjmg/nimbus
busctl --user get-property io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 State
busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 SyncNow
busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 SetPaused b true
```

`busctl --user monitor <name>` does not filter by name. Watch signals with:

```console
dbus-monitor --session "type='signal',interface='io.github.luckjmg.nimbus1'"
```

The window needs a display. `spectacle -b -n -o shot.png` takes one screenshot on
KDE, and `pgrep -a nimbus` confirms that a second instance of the tray exits
instead of adding a second icon. The README links `docs/screenshot.png`, so
replace that file when a change moves anything in the window.

One state file escapes the scratch tree. The bisync listing goes to
`~/.cache/rclone/bisync/`, because rclone derives it from the cache dir and
neither `RCLONE_CACHE_DIR` nor `--cache-dir` moves it. Only the rclone flag
`--workdir` moves it, and the daemon never passes that flag.

The leftover is safe. The file name is a hash of the two paths, so a scratch run
gets its own listing and cannot touch the listing of a real folder.

Sweep the files by the prefix of your own scratch folder, never by a wildcard over
the whole name. The prefix comes from the first path, so a scratch folder named
`/tmp/scratch/local` starts every file with `tmp_scratch_local..`:

```console
rm -f ~/.cache/rclone/bisync/tmp_scratch_local..*
```

A wider pattern such as `*scratch*` also matches entries that name a real local
folder, and it deletes those. A missing listing costs one `--resync` run to
rebuild, because rclone finds no prior listing for the path.

A run that is interrupted leaves `.lst-new` and `.lst-err` instead of `.lst`, and
every later run answers "Bisync aborted. Must run --resync to recover." The daemon
cannot recover on its own, because a clean run already cleared `resync_pending`.
Set it back and restart, as the README Troubleshooting section says.

## rclone

The flags are the result of live runs against rclone 1.74.3. Do not change them
on the assumption they are decoration.

- Only `--stats 1s` **with** `--log-level INFO` writes progress into a pipe. Without the level, rclone writes nothing and no error appears. A test asserts both flags.
- `--stats-one-line` emits nothing in a pipe. `--progress` drops the line separators, so a line reader merges two updates into one. Both are wrong here.
- Progress arrives on stderr. The command nulls stdout, so the child can never block on a full pipe.
- `--resync` is required on the first run, and on the first run after a failure. `resync_pending` in the config tracks it and clears only after a run that ends without an error.
- Error lines carry ANSI colour codes even in a pipe. `strip_ansi` removes them before the text reaches the tray.
- Two lines start with `Transferred:`. Only the byte line has a percentage, and only the byte line parses.

For a live test, a `local` rclone remote with no root resolves `name:path` against
the **process** working directory, and the destination folder must already exist.
Otherwise every run fails with "directory not found" and the state stays `error`.
Google Drive creates folders, so this affects test remotes only.

## zbus

Three traps, all hit and all fixed. Read these before touching `service.rs`.

- `Builder::name()` **discards the bus reply**, so a name another daemon owns looks like a success. Request the name with `request_name_with_flags(BUS_NAME, RequestNameFlags::DoNotQueue.into())` and check the reply. A second daemon must refuse to start.
- `#[proxy]` and `#[zbus::interface]` take a **string literal** and reject the constants. The interface name therefore exists twice. `service.rs` has a test that compares the literal with `INTERFACE`. The same collision forces the signal to be named `Changed` rather than `StateChanged`.
- Interface methods return `zbus::fdo::Result`, not `zbus::Result`, and they run on **separate tasks**, so the service needs `Send + Sync`. The daemon emits `Changed` with `Connection::emit_signal` and never uses `SignalEmitter`, whose methods are async and would need a nested runtime from a blocking thread.

`zbus` is pinned to `5.19`. The macros changed names and behaviour across minors
in the 5.x line. Do not bump without a full workspace test **and** the live check.

## Adding a field to `State`

Six derives, and the reason for four of them is not visible in the code.

- `Phase` needs `#[zvariant(signature = "s", rename_all = "lowercase")]`. Without the signature the derive puts the enum on the wire as a `u32`.
- `Phase` also needs the `Value` derive, because the `OwnedValue` derive on `State` reads and writes it.
- `Phase` and `State` need `Serialize`, because `Connection::emit_signal` takes a payload that implements it. Without the derive the daemon cannot send `Changed`.
- `State` needs `Value` for the property getter, and `PartialEq` for the change check in the main loop.
- `state_signature_is_stable` pins the wire signature to `(sdts)`. A reorder compiles cleanly and breaks every client at runtime.

## The engine lock

The blocking `recv_timeout` must stay **outside** the `engine.lock()` scope.

Inside it, the loop holds the lock for a whole tick, and a property read on a zbus
thread waits for the same lock. The loop takes it back the instant it releases, so
the first D-Bus call works and every later one times out. No unit test finds this,
because a unit test calls the engine directly and never involves a second thread.

## The tray

Two threads. GTK runs on the main thread, because a GTK loop must start on the
thread that called `gtk::init`. The tray worker runs on a thread of its own,
because the zbus blocking calls must not run inside a glib main loop. The two
threads meet at two places only: the shared view, and a channel for the request to
show the window.

Six traps, all hit and all fixed. Read these before touching `main.rs`.

- A proxy caches properties and refreshes the cache only on the standard `PropertiesChanged` signal. The daemon sends `Changed`, so the tray must set `cache_properties(CacheProperties::No)`. A cached proxy reports a stale state forever and never notices that the daemon stopped.
- The generated blocking proxy has no `new_owned`, and `builder` borrows the connection. The worker thread outlives `start()`, so the connection must last for the life of the process. The tray leaks it once with `Box::leak`.
- `handle.update()` reads the view on a thread of the tray service. It must stay **outside** the view lock. Inside it, the update waits for the lock that the update itself holds, and the first menu click hangs.
- An `activate` callback runs on a thread of the tray service. It must not block. It only sends on a channel.
- `glib::MainContext::channel` does not exist in glib 0.22. The window is woken by one `glib::timeout_add_local` timer, which also refreshes the labels.
- `glib::WeakRef::new()` takes no argument in glib 0.22. The type comes from the later `upgrade` call, so the binding needs a type annotation.

`connect_activate` takes an `Fn`, not an `FnOnce`. The first call moves the worker
into a thread, so the worker parts sit in a `RefCell` inside the captured state and
the first call takes them. An `AtomicBool` keeps the first call from starting a
second worker.

`gtk::Application` needs a `.service` file to be activatable. Without one, a call to
`org.freedesktop.Application.Activate` fails with "The name ... was not provided by
any .service files". A second instance of the binary still hands its activation
over and exits, because `g_application_register` finds the owner of the name.

The destination of that call is the application name, not the interface:

```console
busctl --user call io.github.luckjmg.Nimbus /io/github/luckjmg/Nimbus org.freedesktop.Application Activate a{sv} 0
```

Passing `org.freedesktop.Application` as the destination asks the bus about a
different name, and the error then says nothing about the tray.

A host looks an icon name up in its own cache, and the cache does not know an icon
that was installed after the last rebuild. So `Icon::resolve` returns the installed
**directory**, not an empty theme path. A live run on KDE showed a blank tray item
until this was fixed, and no unit test could have found it.

The heading of the window takes `status_name`, which is the short name. The line
below it takes the error from `state.last_error`. The tooltip takes `status_text`,
which is the full text. Only the tooltip has the room for the message.

## Installing

The files in `data/` are the package payload. The `justfile` holds the recipe,
and it is the only copy:

```console
just install            # into /usr, needs root
just install-user       # into the home directory, needs no root
```

Both recipes list every file and where it lands. Do not repeat those paths in a
document. The name says the scope: `install` writes into `/usr`, and
`install-user` writes into the home directory. `uninstall` and `uninstall-user`
remove what the matching recipe added.

The paths are absolute because a D-Bus service file expands neither `$HOME` nor
`%h`. A systemd unit and a desktop entry do expand `%h`, so a user install needs
the service file rewritten instead.

Two start methods, one for each program. The daemon runs as a systemd user unit,
because it needs no display and because a restart policy matters. The tray starts
from the autostart entry, because the login session owns `WAYLAND_DISPLAY` and a
systemd user unit does not. A unit for the tray with
`WantedBy=graphical-session.target` would never start on GNOME, and that target
does not exist there.

`run-tray` starts the tray before the next login, because both installs write the
autostart entry and neither starts the tray. The two recipes install to two paths,
so the recipe probes for the home path first and falls back to `/usr`. It runs the
installed binary, not `target/release`, so `just run-tray` does not test your
build. The live check starts `./target/debug/nimbus` for that reason.

The D-Bus service file exists to make the name activatable. It is not a start
method. The bus passes the activation environment, which has no `WAYLAND_DISPLAY`,
so a tray started by the bus stops at once.

The user directory `~/.local/share/dbus-1/services` works with `dbus-daemon`,
because `standard_session_servicedirs` includes the XDG data directories. The
session bus on a current Fedora and Arch runs `dbus-broker`, and its built-in
service list did not pick the file up in a live run. Ship to
`/usr/share/dbus-1/services`, which every implementation scans.

## A clean refusal exits with zero

Two start failures can only be fixed by the user: a bad config, and a bus name
that another daemon holds. Both exit with zero, so `Restart=on-failure` leaves
the unit stopped instead of restarting it every few seconds. Every other start
failure keeps its non-zero code, so systemd retries.

The name check needs care. `request_name_with_flags` returns
`zbus::Error::NameTaken` for a taken name, not a reply, so the reply check that
follows it never runs. `name_is_taken` matches the error, and the reply check
stays as a second line of defence.

## Design limits, not bugs

- The watcher sees only the local folder. A change made from another machine arrives on `interval_secs` (default 900), not sooner.
- `is_trigger` drops `Access` and `Any` events. rclone reads the local folder during every run, so a filter that accepts reads never stops syncing.
- Pause does not stop a running sync. It skips later runs. The run thread checks the flag between output lines, so a pause lands within about a second.
- The `sync` mode was removed on purpose. `rclone sync` deletes remote files that are missing locally, and `bisync` reports conflicts instead.
- The tray reads the state every 2 seconds instead of listening for `Changed`. A failed read is how the tray learns that the daemon stopped, because a dropped signal looks the same as an idle daemon.
- The panel can drop the tray item, for example on a panel reload. The tray logs the event and stays up, because the window still works. A panel reload brings the icon back.

## Comments

A comment stays only when the name and the type do not explain the code. Delete
every comment that repeats the name it sits on. Never open a doc comment with
"This function" or "This method". State what the code does.

Doc comments follow ASD-STE100 simple English: simple present, complete
sentences, no contractions, no semicolons, and none of "should", "would", "may",
"might", "could". Keep the comment next to the code it explains.

## Commits

Conventional Commits, using the `git-commit` skill. The scope is the crate name,
for example `feat(daemon): add the watcher and the engine loop`.

## Dependencies

`nimbus-ipc` and `nimbusd` need no C library on Linux, apart from a linker. The
`inotify-sys` crate in the tree links `libinotify` only on NetBSD and OpenBSD.
`async-io` comes in through `zbus`; there is no `tokio` in the tree. The `nimbus`
crate links GTK4 through the `gtk4` bindings, so the build needs the GTK4
development package. The crate sets `default-features = false` on `ksni`, because
the default feature pulls in `tokio`. That keeps a second async runtime out of the
tree.
