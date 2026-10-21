# Nimbus

A cloud sync tool for Linux that wraps rclone. It syncs with any rclone remote.
A daemon runs `rclone bisync` on a schedule and when files change. A GTK4 tray
client talks to it over D-Bus.

## State

Three crates exist. The daemon and the tray both work against a live bus. The
systemd unit, the desktop entry, the autostart entry, the D-Bus service file, and
the icons exist under `data/`. The `justfile` holds every install recipe, and it
is the only copy of that file list. `packaging/` holds the three package
definitions, and it is a second copy of the same file list, because a package
tool needs the list in the form of its own metadata. `README.md` holds
the user-facing tables: the config keys, the phases and their icons, the D-Bus
API, the menu, and the troubleshooting steps. `docs/screenshot.png` shows the
window. `docs/releases/` holds one release notes file per tag. The project is in
beta at 0.1.0-beta. No CI runs the three checks, so run them yourself before
every commit.

| Crate | Role |
| --- | --- |
| `nimbus-ipc` | The D-Bus contract. Names, `State`, `Phase`, one proxy trait. No logic. |
| `nimbusd` | The daemon. Library plus binary. |
| `nimbus` | The tray. One binary: the window, the icon, and the menu. |

The binaries are `nimbusd` and `nimbus`. The daemon needs a session bus and refuses
to start when another daemon already holds `io.github.luckjmg.nimbus`. The tray
starts with no daemon and shows "The daemon is not running". The daemon stops
when no process owns `io.github.luckjmg.Nimbus`, which is the name of the tray.
See "The tray owns the daemon" below.

## Commands

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

No CI runs these. Run all three before every commit, in that order. `just check`
runs the same three. 193 tests pass today: 5 in `nimbus-ipc`, 119 in the `nimbusd`
library, 3 in the `nimbusd` binary, and 66 in `nimbus`.

```console
cargo test -p nimbusd                     # one crate
cargo test -p nimbusd --lib rclone::      # one module
cargo test -p nimbusd --lib rclone::tests::parse_progress_reads_the_byte_line
```

## The live check

The unit tests do not cover the bus, the watcher, the panel, or rclone. Those need a
real run. Set `XDG_CONFIG_HOME` to a scratch directory, because the daemon writes a
config file on first start. Set `XDG_STATE_HOME` to a scratch directory too, or the
daemon writes its history into the real state dir.

Start the tray within 60 seconds of the daemon, or the daemon stops. See "The
tray owns the daemon" below.

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
mkdir -p /tmp/scratch/cfg/nimbus /tmp/scratch/local /tmp/scratch/remote
cat > /tmp/scratch/cfg/rclone.conf <<'CONF'
[drive]
type = alias
remote = /tmp/scratch/remote
CONF
```

A bare `remote` targets the root of the remote, so the remote dir itself is
the destination and it must exist before the first run. A `remote` with a
folder, as in `drive:/sub`, targets that folder, and it must exist too. Some remotes, for example
Google Drive, create the folder themselves.

Write the scratch config before the first start. The default `remote` and
`local` are empty, so a first start with no file writes an empty config and
refuses it. The daemon exits with zero and the notification names the empty
remote first. Set the watched folder, the remote, and short timers:

```console
cat > /tmp/scratch/cfg/nimbus/config.toml <<'TOML'
remote = "drive"
local = "/tmp/scratch/local"
paused = false
interval_secs = 60
debounce_secs = 3
resync_pending = true
extra_flags = []
TOML
```

The daemon then waits in the `resync` phase. Confirm the first resync with
the Resync button, or over the bus:

```console
busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 Resync
```

Short timers matter. The defaults wait 900 seconds on the interval and 30 seconds
on the debounce, so a manual run looks broken.

```console
cargo build --workspace
XDG_CONFIG_HOME=/tmp/scratch/cfg XDG_STATE_HOME=/tmp/scratch/state RCLONE_CONFIG=/tmp/scratch/cfg/rclone.conf ./target/debug/nimbusd &
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
replace that file when a change moves anything in the window. `spectacle -b -n
-a -o` captures the active window, and the file has a transparent margin for the
shadow. Crop it to the opaque part. Seed the history file with made-up names,
because the image is public.

Five traps from the runs on a private bus:

- `spectacle` fails on a private bus. Save `DBUS_SESSION_BUS_ADDRESS` before `dbus-run-session`, and set the saved value for the `spectacle` call only.
- Stop a test daemon or tray by PID. `pkill -x nimbusd` also kills the installed daemon, and `pkill -f` with a pattern that appears in your own command line kills your own shell.
- A pipe after `dbus-run-session` hangs. Send the output to a file, and read `/dev/null` as the input.
- Run `cargo build --workspace` before the run. `clippy` does not build the binaries, so a run can use an old binary.
- The dev machine has no tool for keys or the mouse. To scroll the list, add a temporary timer that sets the `value` of the adjustment, and remove it after the run.

The listing lives in `$XDG_CONFIG_HOME/nimbus/bisync`, because `rclone.rs` passes
`--workdir`. Before that flag existed the listing stayed in the rclone cache dir,
which no daemon variable moves. A scratch run therefore keeps every file inside
the scratch tree, and no state reaches the real cache.

If you test against a folder that an older build already synced, the listing in
`~/.cache/rclone/bisync` is now unused. Delete it after the new dir holds a
healthy `.lst`. Never sweep that dir with a wildcard such as `*scratch*`, because a
pattern over the whole name also matches entries that name a real local folder.

## rclone

The flags are the result of live runs against rclone 1.74.3. Do not change them
on the assumption they are decoration.

- Only `--stats 1s` **with** `--log-level INFO` writes progress into a pipe. Without the level, rclone writes nothing and no error appears. A test asserts both flags.
- `--stats-one-line` emits nothing in a pipe. `--progress` drops the line separators, so a line reader merges two updates into one. Both are wrong here.
- Progress arrives on stderr. The command nulls stdout, so the child can never block on a full pipe.
- `--resync` is required on the first run, after a move of the remote or the folder, and after a failure that left no listing to repair. `resync_pending` in the config tracks all three, and it clears only after a run that ends without an error. The daemon never adds the flag on its own. See "The resync gate" below.
- `parse_error` reads `ERROR : ` and `CRITICAL: ` lines. A run that stops on a critical failure, for example a remote that is not in the rclone config, prints the reason only on a `CRITICAL:` line. Without it the tray shows "rclone exited with code 1". `NOTICE` lines stay out, because none of the live runs printed a reason on one.
- The tray shows the last error line that is not `Bisync aborted`, because bisync prints the abort after the line that holds the reason. `user_message` in `rclone.rs` replaces the text of four known failures with a message for the user, and any other text passes unchanged. The markers are the texts of rclone 1.74.3, so a test per marker fails when a new rclone changes the wording. Add an entry to `USER_MESSAGES` for a new failure.
- Error lines carry ANSI colour codes even in a pipe. `strip_ansi` removes them before the text reaches the tray.
- Two lines start with `Transferred:`. Only the byte line has a percentage, and only the byte line parses.
- `Transfers` in `rclone.rs` turns the file lines into history entries. A copy line, `X: Copied (new)` or `X: Copied (replaced existing)`, does not name the direction. The header before it does. `- Path2    Do queued copies to    - Path1` means a download, and the same line with the paths swapped means an upload. `Path1` is the local folder.
- A resync prints `Resync is copying files to` in place of that header, and the line carries color codes. `Transfers` strips them first.
- A `Deleted` line needs no header. The loser of a conflict prints one before any header of the run. A line for `Updated modification time`, `Moved (server-side)`, or a folder makes no entry.
- With no header, the `Queue copy to PathN` lines give the direction. A normal run prints a full path there, and a conflict run prints a path in the root. A lookup that finds two directions for one file name makes no entry, and so does a copy with no direction at all. The daemon logs the second case.
- The line shapes come from rclone 1.74.3 and the `alias` remote. Measure them again for a cloud remote before you trust the fallback.
- The command passes `--workdir`, which points the listing at `$XDG_CONFIG_HOME/nimbus/bisync`. rclone creates the dir, so the daemon does not. The flag is also what lets `repair` find the files.
- Do not add `--recover` or `--resilient`. A live run measured both. Neither one restores a lost listing, and `--resilient` prints "retryable without --resync" and then fails on the next run anyway.

For a live test, a `local` rclone remote with no root resolves `name:path` against
the **process** working directory, and the destination folder must already exist.
Otherwise every run fails with "directory not found" and the state stays `error`.
Some remotes, for example Google Drive, create the folder themselves.

Put one file in the local folder before the first run. A resync of two empty
folders leaves an empty listing, and rclone then answers the next run with
"Empty prior Path1 listing. Cannot sync to an empty directory." The run after
that one resyncs and reaches `idle`, so a sandbox that starts empty looks broken
for two intervals. `just sandbox` writes the seed file.

## The tray owns the daemon

A panel with no Nimbus icon must mean that no daemon runs. The loop in
`nimbusd/src/main.rs` asks the bus once per tick whether a process owns
`TRAY_NAME`, and it breaks out of the loop when the answer is no. The return is
clean, so the unit exits with zero and `Restart=on-failure` leaves it stopped.
This covers the Quit row, a crash of the tray, and a daemon that someone started
by hand. A failed bus call leaves the daemon running, because the call says
nothing about the tray.

A tray that never appeared gets 60 seconds, because the unit starts at login
and the autostart entry starts the tray later. A tray that appeared and left
gets no grace. `tray_is_gone` holds both rules.

No switch turns this off. Every manual run therefore needs a tray in the same
session, or the daemon stops after 60 seconds. This applies to the live check and
to the crash test below. A test on a private bus needs a process that owns the
name, for example `gi` with `Gio.bus_own_name`.

## The offline phase

Two signals move the engine to the phase `offline`. The first is a lost
default route. The loop in `nimbusd/src/main.rs` reads `/proc/net/route` and
`/proc/net/ipv6_route` once per tick, and it sends `Event::Link` when the answer
changes. The kernel drops the route at once, so the icon changes within one
second. The engine sets the phase in the same turn, and it sets the flag that the
run thread reads between output lines, so rclone stops within about one second.
`on_finished` counts that run as offline, not as an error. A route is not the
internet, so a returning route starts one probe, not a run. A table that cannot
be read counts as a link that is up. The IPv6 table holds a reject route on `lo`
with a zero destination, and `has_default_route` skips it. While the route is
missing, the engine starts no probe, because no answer can arrive.

The second signal is a run that dies on a dead network, which covers a router
with no upstream. `run` in `rclone.rs` decides this from the error lines: `is_network_error`
matches five texts that a live run on rclone 1.74.3 printed with no network, with
a dead resolver, and with no address. A refused connection is not on the list,
because a server that is down is not a network that is down. A wrong host name
also counts as offline, because the resolver says the same words.

While the engine is offline, `start_run_if_due` starts a probe in place of a run.
A probe is `rclone lsd --max-depth 1` on the remote, every 30 seconds, or at
once after a Sync now click. A file change and the interval start nothing. Any
answer except a network error counts as online, and the next run then reports
the real error, for example a bad token. The gate for the resync comes first, so
a failed resync still asks again and hides the offline phase.

The tray shows the same icon for `offline` and for a missing daemon, and the
first row of the menu tells them apart.

A live check of the route needs no root. Run the daemon in a private network
namespace on a private bus, with `unshare --map-user=$(id -u) --map-group=$(id -g)
--keep-caps -n`. A mapping to uid 0, as `unshare -Urn` makes, is refused by the
bus with "EXTERNAL rejected". Inside, add a `dummy` link and a default route,
then delete the route and read `State`. That run showed `offline` at the first
poll after the delete, no rclone process afterwards, and `syncing` again after
the route returned. The resync gate holds the first run, so call `Resync` once.

A live run of the error path needs a failing remote, so put a script named `rclone` on the `PATH` of the daemon. The script prints
an `ERROR : ... network is unreachable` line and exits with 1 while a flag file
exists, and it runs the real `rclone` otherwise. That run showed one `lsd` per
30 seconds, no `bisync` while the flag existed, and one `bisync` after the flag
went.

## The crash test

The repair cannot be proven by a unit test, because the failure is rclone's on-disk
behaviour. Repeat this after any change to `repair` or to the rclone flags.

Use the scratch remote from the live check. Start the daemon, then drop a file large
enough that the transfer takes seconds. `/tmp` is a small tmpfs on several
distributions, and the remote copy needs room for the file, so check the free space
with `df -h /tmp` before the run. A run that hits the limit reports "disk quota
exceeded" and looks like a repair failure that never happened:

```console
dd if=/dev/urandom of=/tmp/scratch/local/big.bin bs=1M count=1200
```

Kill rclone mid-transfer with `timeout -s KILL`, which leaves the same debris a
dropped connection does. Then restart the daemon and press Sync now once:

- The state must reach `idle`.
- The config must still hold `resync_pending = false`, which proves no resync ran.
- `sha256sum` on both sides must match.

Repeat with the `.lst` files deleted and only the `.lst-old` spares left, and then
with the whole dir emptied. The last case must stop in the `resync` phase with no
rclone process, and it must recover after one confirmed resync. The
"A lost connection" section above records the three shapes and what each one leaves
behind.

Remove `/tmp/scratch/local/big.bin` before the next case, or the next run spends its
time moving it again and the crash lands at the wrong moment.

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

## A lost connection

A dropped connection leaves the bisync listing unusable. rclone then answers every
later run with "Bisync aborted. Must run --resync to recover." Three shapes were
reproduced, and all three are measured, not guessed.

| Shape | The working dir holds | Only a resync helps |
| --- | --- | --- |
| A. the run died during transfer | `.lst`, `.lst-new`, `.lck` | no |
| B. the run died after the listing | `.lst-old`, `.lst-new` | no |
| C. the run died before any listing | `.lst-new`, `.lst-err` | yes |

`repair` in `rclone.rs` runs **before** the child process, not after the failure.
That order is the whole design. A repair after the failure cleans up for the *next*
run, so the run that follows a crash still fails once. A repair before the run
makes the run itself incremental.

The function keeps a `.lst`, restores one from a `.lst-old` spare when no `.lst`
survives, and deletes the `.lst-new`, `.lst-err`, and `.lck` debris. It returns
false only in shape C. The run then stops before rclone starts and sends
`NeedsResync`, because a resync waits for the user. The spare is never deleted,
so it stays as a backup for the next crash.

Two traps in that function:

- rclone keeps one file per side, so a pair has two spares. Keeping only the last
  one restores half the pair, and rclone still refuses. `spare_restores_a_missing_listing` covers this.
- The function must not restore a `.lst-old` over a `.lst` that exists. The present listing is the newer state, and the spare is older. `a_present_listing_wins_over_the_spare` covers this.

The engine sets `resync_pending` on `NeedsResync`, so the flag survives a
restart. That path is the fallback for shape C, and it costs one full pass over
both sides and one confirmation, which is why the repair avoids it in shapes A
and B.

## The resync gate

A resync lets the local copy overwrite a remote copy that differs, so the
daemon never starts one without a confirmation. `start_run_if_due` refuses every run
while `resync_pending` is set and `confirmed` is not, and it reports the phase
`resync`. The phase is a string on the wire, so the signature stays `(sdts)`.

Only the `Resync` method confirms a resync. The window sends it after its
dialog. A move in the settings dialog asks only about the move, and the window
then asks for the resync like any other.

In the `resync` phase, `last_error` carries the reason, so the window shows it
and the resync dialog names it. The engine keeps one of three texts in
`resync_reason`: no record, a move, or a lost record. A restart forgets the
cause, so a fresh engine uses the text that holds for every cause. A failed
resync keeps its rclone error instead, because that error says why the last
attempt did not work.

`confirmed` lives in memory and clears after every finished run. A restart or
a failed resync therefore asks again. `--resync` follows `resync_pending` only,
so the gate is the one place that decides.

A file that changes on both sides during an outage loses the older edit by
default. `conflict_resolve` is `newer` and `conflict_loser` is `delete`, so
rclone deletes the losing copy. A live run on rclone 1.74.3 confirmed it. With
`conflict_resolve = "none"`, rclone writes `name.conflict1` and
`name.conflict2`, and both copies survive on both sides, even when
`conflict_loser` is `delete`.

## The history

`history.rs` keeps the list of synced files, and `rclone::run` feeds it. See
"rclone" above for the lines that become entries. The file is
`$XDG_STATE_HOME/nimbus/history`, and `history_file` in `config.rs` builds the
path. The state dir is the place for data that the program creates, and the
config dir holds only what the user edits.

One line holds one entry: the time, the kind, the folder, and the name, split by
tabs. A line is complete only when it ends with a newline. `append` sends the
whole line in one `write_all` call, so a crash leaves either the full line or a
line with no newline. `open` drops the first bad line and every line after it,
and then rewrites the file. A last line with no newline counts as bad, even when
it parses, because a cut name can look whole.

Four rules hold the design together. A test covers each one.

- `append` turns a tab or a newline in the name or the folder into a space before it stores the entry. The list and the file then hold the same text, and the line keeps four fields.
- The list drops its oldest entry at once when it passes `history_count`. The file waits for `prune`, which `run` calls after rclone ends. The age counts from the newest entry.
- `rewrite` writes a second file, calls `sync_all`, and renames it over the first. A write into the file itself empties it first, and a crash then loses the history. Without `sync_all`, a power cut can leave an empty file.
- After a rename, the open append handle points to the replaced file. `rewrite` drops the handle, and the next append opens the file again.

`GetHistory(since)` in `service.rs` returns the entries with a time equal to or
newer than `since`, in file order, and it locks the history and not the engine.
The time has a resolution of one second, so a strict compare loses an entry that
arrives in the same second as the newest entry that a client holds. A client asks
with the time of its newest entry, or a little earlier, and removes the repeats.
A whole-entry compare finds them, because one file is copied once in a run.

A method reply goes through serde and not through zvariant. `Kind` therefore needs
`#[serde(rename_all = "lowercase")]` next to the zvariant attribute, and
`Entry` needs `Deserialize` for the proxy. Without the serde attribute, the reply
spells the kind `Uploaded`. `an_entry_goes_on_the_wire_with_a_lowercase_kind`
holds this.

A lower limit takes effect when the next run ends, because `run` prunes with the
config that it received at its start. No run starts while the daemon is paused or
offline, so the entries stay until a run starts. The dialog gives no warning, and
the tray follows `history_count` itself.

Neither limit accepts zero. A zero could mean no limit or no history, and the upper
bounds keep the list that the tray reads from growing without end. A failed write
goes to stderr once, and the sync goes on, because a full disk must not stop it.

The tray side is in `nimbus/src/history.rs`. The worker asks only while the window
is open. The timer of `refresh_loop` sends `Action::WindowShown` when `is_visible`
changes. On each poll, `Watch::since` gives the time of the newest held entry minus
`OVERLAP`, which is 2 seconds. `merge` drops the repeats, puts the newest first,
and cuts the list to `history_count`, which comes from `GetSettings`. An open
starts from zero, because the daemon can drop an entry by age while the window is
hidden. A lost daemon sends one empty snapshot. The window gets a `Snapshot` only
when the list, the folder, or the count moved, or on the first answer after an
open.

The live check of this feature needs no stop of an installed daemon. Run it on a
private bus with `dbus-run-session`, and start a process that owns `TRAY_NAME`, for
example `gi` with `Gio.bus_own_name`. Seed at least five files. A run that deletes
every file stops with "too many deletes".

## The engine lock

The blocking `recv_timeout` must stay **outside** the `engine.lock()` scope.

Inside it, the loop holds the lock for a whole tick, and a property read on a zbus
thread waits for the same lock. The loop takes it back the instant it releases, so
the first D-Bus call works and every later one times out. No unit test finds this,
because a unit test calls the engine directly and never involves a second thread.

## The settings dialog

The tray holds two windows: `window.rs` for the status and `settings.rs` for
the keys. The daemon owns the config file and stays the only writer while it
runs, because it writes that file on every pause toggle and on every resync
flag change. The dialog talks to the daemon over D-Bus instead of touching the
file.

When no daemon answers, the tray reads and writes the file itself.
`read_settings` fills the dialog from the file, and `write_settings` saves
the keys into it, so Save closes the dialog and keeps the edit. Without the
read, the dialog showed the defaults, and a save wrote them over the file in a
live run. A move raises `resync_pending`, so the next daemon asks for the
resync. A file that does not load is not written, and its error goes into the
dialog.

Four traps, all hit and all fixed. Read these before touching `settings.rs`.

- `Settings` in `nimbus-ipc` is not `Config` in `nimbusd`. The wire carries eight
  primitive keys. `Config` also holds `paused` and `resync_pending`, and a client
  that wrote either one would clear a flag that keeps rclone running.
- `LocalDir` keeps a tilde for the file and drops it for rclone. The wire field
  is a plain `String`, so `LocalDir::path()` and `LocalDir::text()` are both
  needed, and the tilde must survive a save through the dialog.
- `u64` is `t` on the wire, not `u`. The pinned signature is `(ssttssastt)`, and
  `settings_signature_is_stable` holds it.
- `Config` carries `deny_unknown_fields`, so a file from a build that read a
  `path` key stops the daemon with an error that names the unknown field.
  Serde ignores an unknown key by default, and a silent start would sync the
  whole remote to a user who asked for one folder.
- `Adjustment::new` takes six arguments in GTK 4, with `page_size` last. The
  spin button carries the range, so the dialog cannot send a value the daemon
  refuses. The two history buttons take `HISTORY_AGE_HOURS` and `HISTORY_COUNT`
  from `config.rs`, so the button and `check` read one source.

A moved `remote` or `local` has no bisync listing, because rclone names its
listing after the pair of paths. So `Config::apply` raises `resync_pending`
and returns true, which costs one full pass. The engine takes that return
value to set the reason for the resync. The comparison is on the folder that rclone opens,
because a text change that keeps the folder must not cost a resync.

A refusal arrives as a `zbus::Error::MethodError` named `InvalidArgs`, and its
`detail` holds the words from `apply` or `check`. The bus also answers a call to a
missing daemon with a method error, `ServiceUnknown` with "The name is not
activatable", so `refuse` matches the name, not only the variant. Every other
error leaves the line empty, because the status window already says why the
daemon is not running.

`apply` and `check` return `Invalid`, which names the refused `Key`. The
wire carries the conflict values as text, so `apply` refuses an unknown one
before any key changes. `ConflictResolve` and `ConflictLoser` spell each value
once, in `as_str`, and serde reads and writes the file through it. The dialog
runs the same two on the typed keys before Save sends them, and `Hints` shows the
reason under the field that holds the key. The daemon checks again, because
the dialog is not the only client. A refusal that belongs to no field, such as
a daemon that does not run, goes to the line above the buttons.

`target` in `config.rs` turns the `remote` text into the rclone target. A
bare name gets `:/`, and a text with a colon passes unchanged. Do not
normalize the colon form. On a `local` remote, `name:path` resolves against
the working directory and `name:/path` does not. Both move checks compare
targets, so `drive` and `drive:/` are the same root and cost no resync.

The dialog is built once and reused. A rebuild would throw away a half-typed
value, and it would leave the panel with two windows for one click.

### The live check for the dialog

The menu row still reads "Settings", and it no longer opens the status window,
so the menu test in `tray.rs` is unchanged.

The daemon side needs no click, so read it over the bus first:

```console
busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 GetSettings
busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 SetSettings "ssttssastt" "" /tmp/scratch/local 60 3 newer delete 0 24 1000
```

The second call must fail with "There is no remote", and the config
file must be unchanged afterwards.

Then move the folder and read the log. A save that leaves `local` alone must
produce no new line, and a save that moves it must name the new folder:

```console
busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 SetSettings "ssttssastt" drive /tmp/scratch/local2 60 3 newer delete 0 24 1000
```

After that line, a file change in `/tmp/scratch/local2` must start a run, and a
file change in `/tmp/scratch/local` must not. Create `local2` first, because
`check` refuses a folder that does not exist.

## The tray

Two threads. GTK runs on the main thread, because a GTK loop must start on the
thread that called `gtk::init`. The tray worker runs on a thread of its own,
because the zbus blocking calls must not run inside a glib main loop. The two
threads meet at three places only: the shared view, the `Action` channel from the
window to the worker, and the `Reply` channel back. The list of the history
travels in a `Reply`, not in the view, because the timer copies and compares the
view ten times a second.

Six traps, all hit and all fixed. Read these before touching `main.rs`.

- A proxy caches properties and refreshes the cache only on the standard `PropertiesChanged` signal. The daemon sends `Changed`, so the tray must set `cache_properties(CacheProperties::No)`. A cached proxy reports a stale state forever and never notices that the daemon stopped.
- The generated blocking proxy has no `new_owned`, and `builder` borrows the connection. The worker thread outlives `start()`, so the connection must last for the life of the process. The tray leaks it once with `Box::leak`.
- `handle.update()` reads the view on a thread of the tray service. It must stay **outside** the view lock. Inside it, the update waits for the lock that the update itself holds, and the first menu click hangs.
- An `activate` callback runs on a thread of the tray service. It must not block. It only sends on a channel.
- `glib::MainContext::channel` does not exist in glib 0.22. The window is woken by one `glib::timeout_add_local` timer, which also refreshes the labels.
- `glib::WeakRef::new()` takes no argument in glib 0.22, so the binding needs a type annotation. It returns an **empty** reference, and `upgrade` returns `None` until `set` names an object. A tray click reached the window through this reference, so every click after the first did nothing. The comments on `refresh_loop` and on `StatusWindow::root` described the intent, and the code did not follow.
- `gtk_button_get_child` returns the label, not an internal box, on GTK 4.22. The recipe that prepends an icon into the child box adds nothing, and the button shows a text and nothing else. A live run on GTK 4.22.5 showed two plain buttons. `tile_button` builds the box instead, so the icons follow the icon theme of the desktop.

## The window

The window sets no title bar and no heading. The caption comes from the desktop,
either from KWin or from `gtk-decoration-layout` when it asks GTK to draw one. A
`HeaderBar`, or a name label above the state line, takes room from the list. Do
not add one back.

From top to bottom: a row with the colored circle, the state line, and the
Settings icon, then the list of recent files, a separator, and the buttons.
`badge` and `status_line` in `window.rs` map a view to the circle and the text.
The size is fixed. The list area is 360 pixels high and scrolls, and the width
follows the buttons. A label with a long text needs `set_max_width_chars`, or the
window grows to fit the text.

The error lines in the window and the dialog take the stock `error` class, so
the theme picks the color. A live run on KDE drew it orange. Do
not set a fixed color.

The dialog entries carry example placeholders. Breeze for GTK draws a
placeholder in the full text color, so an example read as the current value
in a live run. One CSS rule sets the placeholder opacity to 0.5, which fades
the theme color instead of replacing it. A GTK 4 style provider covers the
whole display, so `install_css` in `main.rs` installs this rule and the square
corners for both windows, next to the two `build` calls.

The buttons take their icons from the icon theme, so the names in
`window.rs` must exist in it. The check that matters is a live run on the
desktop, because no unit test can see a theme. The menu sends the same names to
Plasma, which draws them with its own icon theme. The menu row names the
setting and keeps the pause icon, because only the window button names the
action.

The circle colors come from the named colors of the theme, such as
`@success_color`, in the rules of `install_css`. Three traps with Breeze, all hit
and all fixed:

- A link that cannot be clicked keeps the link color. A row with a folder that is gone sets the class `nimbus-gone` for the gray.
- A `ListBoxRow` draws its own background, so the rule that clears the list must name `.nimbus-list row` too.
- An icon with `set_hexpand(true)` makes the box around it expand, and the circle becomes an oval. The circle box sets `set_hexpand(false)`.

The list is a `ListBox` in a `ScrolledWindow`, and `rows.rs` builds each row.
`receive_history` gets the whole list from the worker. `rows::diff` counts the
new entries at the front. A match adds only those rows at the top and cuts the
rows past the count. Any other case builds every row again: the first list, an
empty old list, a moved folder, or an old newest entry that is missing. `entries`
and `ages` must change together, because a click finds its entry by the index of
the row.

A user who scrolled down keeps the rows in view. When `value` is above `lower`,
the code adds the measured height of the new rows to `value` right after the
insert. It measures with `measure`, because the layout has not run yet. A live
run showed that GTK does not clamp the new value, also when the tail is cut at
the same time.

`refresh_ages` rewrites the age of each row. The timer of `refresh_loop` calls it
every `AGE_EVERY`, 10 seconds, while the window is visible, and `receive_history`
calls it after each list. It sets a label only when the text differs.

A click on a row opens the file with `gtk::FileLauncher`. A file with an
executable bit, or a `.desktop` file, opens its folder instead, so a click never
starts a program. `folder_path` keeps every path inside the synced folder,
because a path from the daemon takes only plain parts.

## The tray raise

A tray click raises the window only while it is hidden. Three live runs on KWin 6
measured the rest, and each of these is a measurement rather than a guess.

`gtk_window_present` ends in `gdk_toplevel_focus`, and on Wayland that reaches
`gdk_wayland_toplevel_focus`, which anchors the activation token on
`_gdk_wayland_seat_get_last_implicit_grab_serial()`. That serial comes from a
keyboard grab inside the process. A tray click lands on the panel, so there is no
grab, the token proves nothing, and KWin drops the request. The window then neither
raises nor focuses, and a minimized window stays minimized.

| State at the click | What happens | Why |
| --- | --- | --- |
| hidden | comes to the front | the map is new, and KWin places a new surface on top |
| visible, behind another window | stays where it is | a raise needs a token, and none is valid |
| minimized | stays minimized | same reason |

`set_visible(false)` followed by `present()` in one tick does not change any of it.
The probe printed `visible=true mapped=true` and `active=false` after a full unmap
and a fresh map, and the screenshot showed the window still behind the terminal. It
also flickered on every click, because `is_active()` is false even for a focused
window, so a guard on it never skips. Do not try it again.

Plasma does offer a token. `dbus-monitor` on the item interface shows the panel
sending this on every click, immediately before `Activate`:

```
member=ProvideXdgActivationToken
   string "kwin-171"
member=Activate
```

`ksni` never implemented that method, so the call returns `UnknownMethod` and the
token is lost. Calling it by hand confirms the gap:

```console
gdbus call --session --dest org.kde.StatusNotifierItem-$PID-1 \
  --object-path /StatusNotifierItem \
  --method org.kde.StatusNotifierItem.ProvideXdgActivationToken test
```

A fix would buffer the token and pass it to `gtk_window_set_startup_id` before
`present()`. `gdk_wayland_toplevel_focus` steals that value first, so no hand-rolled
Wayland is needed. It needs one of three things, and none of them is small:

- Vendor `ksni` and add the method to its interface.
- Replace `ksni` with an own SNI server.
- Wait for `ksni` upstream.

On GNOME there is no `ProvideXdgActivationToken` at all, so the buffer would stay
empty and the behaviour falls back to what it is now. Telegram Desktop takes the
first route, which is why its tray icon raises the window and an Electron one does
not.

`connect_activate` takes an `Fn`, not an `FnOnce`. The first call moves the worker
into a thread, so the worker parts sit in a `RefCell` inside the closure and the
first call takes them. The first call also stores the windows in a second
`RefCell`. A later call finds them there, so it starts no second worker.

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
that was installed after the last rebuild. So `icon_dir` returns the installed
**directory**, not an empty theme path. A live run on KDE showed a blank tray item
until this was fixed, and no unit test could have found it.

The icon has no color of its own, and the tray cannot give it one. SNI sends a name
and a path, and no host reads a color from either, so each host recolors the file
instead. Both need a hook in the SVG.

- KDE rewrites the `<style>` element whose id is `current-color-scheme`, when the icon theme sets `FollowsColorScheme`, which Breeze does. The paths take the color through `.ColorScheme-Text` and `stroke="currentColor"`.
- GTK marks an icon symbolic from the `-symbolic` file name suffix. It then overrides `fill` and `stroke` from the `class` attribute, and it ignores the presentation attributes. So the class list must carry `transparent-fill` and `foreground-stroke`, or the outline renders filled or not at all.

`currentColor` alone resolves to black in a standalone file, on every desktop. A
live run on KDE showed a black icon on a dark panel until this was fixed.

The state line of the window takes `status_text`, and an error takes the message
from `state.last_error`. The tooltip takes `status_text` too. `status_name` is
the short name, and only the header row of the menu uses it, because a menu row
has no room for the message.

## Packaging and releases

`.github/workflows/release.yml` runs on a tag push of `v*`. It is the only
workflow in the tree, and it does not run the three checks. A tag that
disagrees with the `version` in `Cargo.toml` stops the build, because a wrong
tag puts wrong metadata in the name of every package.

The workflow runs one cargo build and three package builds. The build job
writes one tarball, `nimbus-<version>-x86_64-unknown-linux-gnu.tar.gz`, which
holds the two binaries, the `data/` tree, the license, and the readme. Each
package job runs in the container of its own distribution, so the package tool
and the file layout are the ones of that distribution. The release job attaches
the tarball and the three packages.

The payload sits in one `nimbus` directory inside the tarball, and no version
names it. That is what lets each package tool reach it from its own name for
the version. The deb and the rpm take the payload with `--strip-components=1`,
and `makepkg` reaches it through `$srcdir/nimbus`.

The version changes form for each tool, so the workflow derives it from the tag
in one line each:

| Format | Derivation | Result for `v0.1.0-beta` |
| --- | --- | --- |
| Debian | `version` with the first `-` as `~`, then `-1` | `0.1.0~beta-1` |
| Fedora | `version` with the first `-` as `~` | `0.1.0~beta` |
| Arch | `version` with every `-` as `.` | `0.1.0.beta` |

A tilde sorts before the final release, and a dot sorts before it under
`vercmp`, so a beta never replaces a final release in a package manager. A tag
with no hyphen needs no change, because each form then matches the tag.

Each file under `packaging/` carries a placeholder, and the workflow fills it
from the tag: `@VERSION@` in `deb/control` and `nimbus.spec`, and `@PKGVER@`,
`@TAGVER@`, and `@SHA256@` in `PKGBUILD`. The placeholders are the only reason
a release needs no edit to a packaging file. Do not put a literal version in
one of these files.

The three package layouts are the layout of `just install`, with two changes.
The autostart entry goes to `/etc/xdg/autostart`, because a package cannot
write into the home directory of the user, and every desktop reads that path.
The license and the readme go to `/usr/share`. Every other path is the path that
the recipe uses, and the files come from `data/` unchanged.

No package enables the daemon unit, so the user runs
`systemctl --user enable --now nimbusd.service` once. An install runs as root
with no session bus, so `systemctl --user enable` fails there. A `--no-reload`
flag does not help, because the connect to the bus happens before it, which a
live run confirmed. The README and the release notes name the command.

Three traps in the package jobs, all hit and all fixed. Read these before
touching the workflow.

- `makepkg` refuses to run as root, and a container job runs as root. The Arch
  job makes a user and builds in the home directory of that user. It also needs
  `fakeroot` and `debugedit` installed, and the image package database needs a
  full sync with `pacman -Syy` before it finds them. `debugedit` is its own
  package in `core`, not a part of `elfutils`.
- `makepkg` names its source file after `pkgver`, and the release names its
  asset after the tag. The two names differ, because the Arch form of a
  pre-release uses a dot. The PKGBUILD therefore holds `@TAGVER@` next to
  `pkgver`, and it names the source with the `name::url` form. The workflow
  writes the tag form and the workflow puts the tarball next to the PKGBUILD,
  so makepkg reads the file and never fetches the URL.
- A container job runs each step with `/bin/sh`, which is dash on Debian and
  bash on Fedora and Arch. `${version/-/\~}` is bash only, so the workflow sets
  `defaults.run.shell: bash`. Without it, the deb job fails with "Bad
  substitution" and the other two pass, which looks like a bug in the deb
  branch.
- `actions/checkout` needs `git`, and none of the three base images has it. Each
  package job installs it.
- `${version/-/~}` reads the tilde as the home directory of the user on bash
  5.2, which is the bash of the Debian and the Arch image. It returns
  `0.1.0/rootbeta`, and the extra slash then breaks the `sed` that writes the
  version into the packaging file. The tilde needs a backslash, so the line is
  `${version/-/\~}`. A container job runs as root, so the home directory is
  `/root`.

The arch package needs no build dependency check, because the binaries come from
the build job. `makepkg --nodeps` skips that check. The Arch package also sets
`!debug`, because a debug split of a cargo release build holds nothing.

The packages are x86_64 only, because the workflow has one runner
architecture. The rpm says so with `ExclusiveArch`, and the deb says so with
`Architecture`.

`docs/releases/<tag>.md` holds the release notes for that tag, and the release
job reads it as the body of the release. A release needs that file, and `gh`
fails without it, so a missing file stops the release instead of publishing an
empty body. The notes follow the Good Docs template and ASD-STE100 simple
English.

## Installing

The files in `data/` are the package payload. The `justfile` holds the recipe,
and it is the only copy of that list. The files under `packaging/` are a second
copy, one per package tool:

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
from `data/autostart`, because the login session owns `WAYLAND_DISPLAY` and a
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

A config that does not parse is a bad config too. It takes the same refusal as
a config that fails `check`.

The daemon starts at login with no terminal, so a bad config also sends a
desktop notification through `org.freedesktop.Notifications`. The timeout is
zero, so the notification stays until the user closes it. On Plasma the name
is activatable through `plasma_waitforname`, so a call made before the panel
starts waits for it. A desktop with no notification server loses the
notification, and the line on stderr stays.

The window shows the same text. The daemon has exited, so the tray cannot ask
it. When no daemon answers, the tray runs `config::load` and `check` from the
`nimbusd` library and carries the result in `View::Offline`. The tray reads
the config only when the file exists, because `load` writes a default file and
the daemon is the only writer. The message carries no file path and no
command. The window offers a Start daemon button instead, which calls
`StartUnit` on the systemd user manager, so the daemon runs under the restart
policy of its unit.

Each message from `check` and `load` names the error in one short sentence. A
second sentence names the fix only when the fix is not obvious from the error.

The name check needs care. `request_name_with_flags` returns
`zbus::Error::NameTaken` for a taken name, not a reply, so the reply check that
follows it never runs. `claim_bus_name` matches that error, and the reply check
stays as a second line of defence.

## Design limits, not bugs

- A tray and a daemon from different releases cannot talk about the settings, because the signature of `Settings` differs. A live run measured that an old daemon answers a new `SetSettings` with the error name `org.freedesktop.zbus.Error`. `refuse` in the tray shows words only for `InvalidArgs`, so the tray reads this as a missing daemon and writes the config file itself. A release restart pairs the two, because a tray that quits stops its daemon, so the case is rare.
- A clock that steps back gives a new history entry an old time, and a client that asks with a later time does not get the entry until it asks from zero. The tray cannot follow the age limit, so a row can outlive its entry in the daemon, until the window starts from zero.
- A lost connection is repaired on the next run, and only shape C costs a resync, which waits for a confirmation. The repair restores the listing, so a retry is incremental. See "A lost connection" above.
- A failed run waits for the next `interval_secs` before it retries, unless the failure is a dead network. Then the daemon probes every 30 seconds, and a lost default route stops the runs within one second. See "The offline phase" above.
- The watcher sees only the local folder. A change made from another machine arrives on `interval_secs` (default 900), not sooner.
- `is_trigger` drops `Access` and `Any` events. rclone reads the local folder during every run, so a filter that accepts reads never stops syncing.
- Pause does not stop a running sync. It skips later runs. The run thread checks the flag between output lines, so a pause lands within about a second.
- The `sync` mode was removed on purpose. `rclone sync` deletes remote files that are missing locally, and `bisync` reports conflicts instead.
- The tray reads the state every 2 seconds instead of listening for `Changed`. A failed read is how the tray learns that the daemon stopped, because a dropped signal looks the same as an idle daemon.
- The settings dialog stops the two timers at 86400 seconds, one day. The config file has no upper bound, so a value above that needs the file. A lower bound of one exists because `check` refuses zero.
- The daemon moves its file watcher when `local` moves, because the kernel holds the watch and the config does not. The move sits behind a comparison on the expanded path, so a settings save that left `local` alone rebuilds nothing. The comparison is the `watch` field of `Turn` in `nimbusd/src/main.rs`.
- The panel can drop the tray item, for example on a panel reload. The tray logs the event and stays up, because the window still works. A panel reload brings the icon back.
- A row checks its folder when the window builds it. A folder that appears or goes later keeps its look until the row is built again.
- A list that does not continue the old one builds every row. The first fill is the worst case. A debug build took 149 ms for 1000 rows and 746 ms for 5000 rows. 10,000 rows, the upper bound of `history_count`, are not measured. `ListView` builds only the rows in view, and a `ListBox` does not. Change to it if the first fill is slow.
- The ages move every 10 seconds, so a row can read up to 10 seconds late.
- A tray click raises the window only while it is hidden. A visible window keeps its place, and a minimized one stays minimized. The compositor decides this, and it needs an `xdg_activation_token_v1`. The details are in "The tray raise" below.

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
crate depends on the `nimbusd` library for the config check, and it links GTK4
through the `gtk4` bindings, so the build needs the GTK4
development package. The crate sets `default-features = false` on `ksni`, because
the default feature pulls in `tokio`. That keeps a second async runtime out of the
tree.
