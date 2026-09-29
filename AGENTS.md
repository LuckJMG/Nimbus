# Nimbus

A Google Drive sync tool for Linux. A daemon runs `rclone bisync` on a schedule and
when files change. A GTK4 tray client talks to it over D-Bus. The client does not
exist yet.

## State

Two crates exist. `nimbus` (the tray) is not written. There is no README, no
systemd unit, no desktop entry, no icon, and no packaging. The daemon is finished
and verified against a live bus.

| Crate | Role |
| --- | --- |
| `nimbus-ipc` | The D-Bus contract. Names, `State`, `Phase`, one proxy trait. No logic. |
| `nimbusd` | The daemon. Library plus binary. |

The binary is `nimbusd`. The daemon needs a session bus and refuses to start when
another daemon already holds `io.github.luckjmg.nimbus`.

## Commands

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

No CI runs these. Run all three before every commit, in that order. 59 tests pass
today: 3 in `nimbus-ipc`, 51 in the `nimbusd` library, 5 in the `nimbusd` binary.

```console
cargo test -p nimbusd                     # one crate
cargo test -p nimbusd --lib rclone::      # one module
cargo test -p nimbusd --lib rclone::tests::parse_progress_reads_the_byte_line
```

## The live check

The unit tests do not cover the bus, the watcher, or rclone. Those need a real
run. Set `XDG_CONFIG_HOME` to a scratch directory, because the daemon writes a
config file on first start.

```console
cargo build -p nimbusd
XDG_CONFIG_HOME=/tmp/scratch ./target/debug/nimbusd &

busctl --user introspect io.github.luckjmg.nimbus /io/github/luckjmg/nimbus
busctl --user get-property io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 State
busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 SyncNow
busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 SetPaused b true
```

`busctl --user monitor <name>` does not filter by name. Watch signals with:

```console
dbus-monitor --session "type='signal',interface='io.github.luckjmg.nimbus1'"
```

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

Four derives, and the reason for three of them is not visible in the code.

- `Phase` needs `#[zvariant(signature = "s", rename_all = "lowercase")]`. Without the signature the derive puts the enum on the wire as a `u32`.
- `Phase` also needs the `Value` derive, because the `OwnedValue` derive on `State` reads and writes it.
- `State` needs `Value` for the property getter, and `PartialEq` for the change check in the main loop.
- `state_signature_is_stable` pins the wire signature to `(sdts)`. A reorder compiles cleanly and breaks every client at runtime.

## The engine lock

The blocking `recv_timeout` must stay **outside** the `engine.lock()` scope.

Inside it, the loop holds the lock for a whole tick, and a property read on a zbus
thread waits for the same lock. The loop takes it back the instant it releases, so
the first D-Bus call works and every later one times out. No unit test finds this,
because a unit test calls the engine directly and never involves a second thread.

## Design limits, not bugs

- The watcher sees only the local folder. A change made from another machine arrives on `interval_secs` (default 900), not sooner.
- `is_trigger` drops `Access` and `Any` events. rclone reads the local folder during every run, so a filter that accepts reads never stops syncing.
- Pause does not stop a running sync. It skips later runs. The run thread checks the flag between output lines, so a pause lands within about a second.
- The `sync` mode was removed on purpose. `rclone sync` deletes remote files that are missing locally, and `bisync` reports conflicts instead.

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
`async-io` comes in through `zbus`; there is no `tokio` in the tree. The future
`nimbus` crate will need `gtk4`.
