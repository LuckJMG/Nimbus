<p align="center"><img src="docs/logo.svg" alt="The Nimbus logo" width="128"></p>

# Nimbus

Let it pour your files from the cloud. A linux native cloud sync tray.

<https://github.com/LuckJMG/Nimbus>, maintained by LuckJMG.

> **Disclaimer:** Nimbus is in beta. AI generated 100% of this project. That
> fact does not excuse its faults. Use Nimbus at your own risk. Any help to
> improve it is welcome.

## Project description

Nimbus is a wrapper around [rclone](https://rclone.org/). With Nimbus, you can
keep a local folder in sync with any rclone remote on Linux. Sync the whole
remote, or one folder in it. A daemon runs `rclone bisync` on a schedule and
after each file change. A tray icon shows the state and takes your commands.

Nimbus helps you see the sync at a glance. The window shows the phase, the
progress, the time since the last run, and the last error.

Nimbus recovers from most rclone errors. A change of network, a lost
connection, or a shutdown in the middle of a sync needs no action from you.
Before the next run, the daemon repairs the record that rclone left behind,
so the retry stays incremental. If the record cannot be repaired, Nimbus asks
you for an explicit resync.

Nimbus never starts a resync without your confirmation. A resync lets the
local copy overwrite each remote copy that differs, so the window asks first.

If a file changes on both sides, you choose what happens. By default, the
newer edit wins and the older edit is deleted. With
`conflict_resolve = "none"`, both copies stay, as `name.conflict1` and
`name.conflict2`.

![The Nimbus window](docs/screenshot.png)

Nimbus has two programs. The daemon, `nimbusd`, owns the sync. It watches the
local folder, keeps an interval, and serves one D-Bus interface. The tray,
`nimbus`, starts with no daemon and waits for one. The daemon stops when the
tray ends, so a panel with no Nimbus icon means that no daemon runs.

## Who this project is for

This project is for Linux desktop users who want a cloud storage remote as
a folder on disk. You must be able to set up an rclone remote and run commands
in a terminal.

The project is also for developers who want to write their own client. Any
program can read the state and send commands over D-Bus.

## Project dependencies

A package brings its own build, so these apply only to the path that builds
from the repository.

Before you build Nimbus, make sure that you have:

- An account on a storage service that rclone supports.
- Rust 1.92 or later. The `gtk4` bindings declare it. The daemon alone needs 1.87, from `zbus`.
- GTK 4.10 or later, with the development package. The build enables the `v4_10` feature.
- rclone 1.71 or later. rclone 1.71 promoted `bisync` from beta to stable.
- `just`, which runs the install recipes.
- A systemd user session and a D-Bus session bus.

Whichever way you install Nimbus, you need rclone 1.71 or later, a systemd
user session, and a D-Bus session bus. Each package lists the versions that
work for the daemon and the window.

The builds below are known to work:

| Distribution | GTK4 | rclone | Builds |
| --- | --- | --- | --- |
| Arch Linux | `gtk4` 4.22.5 | `rclone` 1.75.1 | yes |
| Fedora 44 | `gtk4-devel` 4.22.5 | `rclone` 1.74.3 | yes |
| Debian 13 | `libgtk-4-dev` 4.18.6 | see below | yes |
| Debian 12 | `libgtk-4-dev` 4.8.3 | see below | no, GTK is below 4.10 |

Debian ships rclone 1.60.1, which is older than stable `bisync`. Before the
first run, install a newer rclone from the rclone apt repository or from a
static build.

```console
# Debian, after you add the rclone apt repository
sudo apt install libgtk-4-dev rclone
```

## Instructions to use Nimbus

To start, create a remote in rclone. Then install Nimbus, and set the folder to
sync.

### Install Nimbus

Two ways to install Nimbus exist. Install a package, or build the programs
from the repository.

#### Install a package

1. Download the package for your distribution from the
   [releases page](https://github.com/LuckJMG/Nimbus/releases).

   | Distribution | File to install |
   | --- | --- |
   | Debian 13 | `nimbus_<version>_amd64.deb` |
   | Fedora | `nimbus-<version>-1.fc*.x86_64.rpm` |
   | Arch | `nimbus-<version>-1-x86_64.pkg.tar.zst` |

2. Install the file with the tool of your distribution.

    ```console
    # Debian
    sudo apt install ./nimbus_<version>_amd64.deb

    # Fedora
    sudo dnf install ./nimbus-<version>-1.fc*.x86_64.rpm

    # Arch
    sudo pacman -U ./nimbus-<version>-1-x86_64.pkg.tar.zst
    ```

    Each package installs the two programs, the systemd user unit, the D-Bus
    service file, the app menu entry, the five icons, and an autostart entry in
    `/etc/xdg/autostart`.

3. Enable the daemon. An install runs with no session bus, so only you can
   enable the unit.

    ```console
    systemctl --user enable --now nimbusd.service
    ```

    The tray starts at the next login. To start it now, run it from the app
    menu.

#### Build from the repository

1. Create a remote in rclone. Name it `drive`, because the default remote
   path in Nimbus is `drive:/`. A different name works too, if you set it in
   the config.

    ```console
    rclone config
    ```

2. Clone the repository and build the two programs.

    ```console
    git clone https://github.com/LuckJMG/Nimbus
    cd Nimbus
    cargo build --release
    ```

3. Install Nimbus with one of the two recipes.

    ```console
    just install            # into /usr, needs root
    just install-user       # into your home directory, needs no root
    ```

    Both recipes enable the daemon as a systemd user unit and start it. The
    `justfile` lists each file and where it goes.

4. If you used `just install-user`, read the paths that the recipe prints.

    The recipe rewrites the absolute paths in the unit and the service file.
    A home directory install also needs an icon cache rebuild. See
    [Troubleshoot Nimbus](#troubleshoot-nimbus).

5. Start the tray now, or log in again.

    ```console
    just run-tray
    ```

    The recipe finds the installed binary after either install.

The two programs start in two ways. The daemon runs as a systemd user unit,
because it needs no display and a restart policy matters. The tray starts from
the autostart entry, because the login session owns `WAYLAND_DISPLAY`.

The autostart entry passes `--hidden`, so a login starts the tray icon and no
window. A start from the app menu opens the window.

### Configure Nimbus

The daemon reads one file, `~/.config/nimbus/config.toml`. On the first start,
it writes the file with the default settings. While the daemon runs, it is the
only program that writes the file.

1. Create the local folder. The daemon refuses a folder that does not exist.
2. Open the window, and then click Settings.
3. In Remote path, type the name of your rclone remote. To sync one folder only,
   add the folder, as in `drive:/Documents`.
4. In Local folder, type the path of the folder.
5. Click Save. If the dialog asks about a move of the sync, click to confirm.
6. If the window shows Start daemon, click it.
7. In the window, click Resync, and then confirm. The first run of a new
   config is a resync.

The dialog works when the daemon does not run. It then reads and writes the
config file itself, and the daemon reads the file on its next start.

| Key | Default | Meaning |
| --- | --- | --- |
| `remote` | `drive:/` | The rclone remote, from your rclone config. A bare name, as in `drive`, syncs the whole remote. A name with a folder, as in `drive:/Documents`, syncs that folder only. |
| `local` | `~/Cloud` | The local folder. The daemon watches this folder. |
| `paused` | `false` | A run that is active finishes. Later runs wait for a resume. |
| `interval_secs` | `900` | The longest gap between two runs, counted from the end of the last run. |
| `debounce_secs` | `30` | The quiet time after the last file change. |
| `resync_pending` | `true` | The next run uses the rclone flag `--resync`, after you confirm it. The flag clears only after a run that ends without an error. |
| `conflict_resolve` | `newer` | The copy that wins when a file changed on both sides: `none`, `newer`, `older`, `larger`, `smaller`, `path1` (local), or `path2` (remote). `none` keeps both copies. |
| `conflict_loser` | `delete` | What happens to the copy that lost. `num` renames it with the next free number, as in `name.conflict1`. `pathname` renames it with the number of its origin: `name.conflict1` is local and `name.conflict2` is remote. `delete` removes it for good. With `none`, no copy loses, so rclone keeps both. |
| `extra_flags` | `[]` | More rclone flags for every run, for example `["--drive-skip-shortcuts", "--drive-acknowledge-abuse"]` for a Google Drive remote. |

Nimbus passes a `remote` with a colon to rclone unchanged. On a `local`
remote, `name:path` and `name:/path` are different folders, so write the
folder the way rclone expects it.

An older build read a separate `path` key. The daemon refuses a config file
that still holds `path`, and it prints the line to delete. To keep the same
folder, add it to `remote`, as in `drive:/Documents`. So an upgrade never
moves to a different set of files.

The daemon keeps its record of your files in `bisync/`, next to the config
file.

> Do not delete the `bisync/` directory while the daemon runs. The daemon
> needs the record to keep each run incremental.

A run starts when one of these conditions is true:

- The last file change is at least `debounce_secs` old.
- The last run finished at least `interval_secs` ago.
- You asked for a run with Sync now.
- No run has finished yet.

The daemon watches only the local folder. A change from another machine
arrives on the next interval.

A lost connection needs no manual fix. The daemon keeps a record of what both
sides agreed on. It restores that record before the next run, so the retry
stays incremental. Only a crash that left no record needs a full resync.

A resync compares every file on both sides. Where a file differs, the local
copy replaces the remote copy. So the daemon never starts one on its own. The
window says "A resync is needed" and shows a Resync button. The resync starts
after you confirm it.

### Run Nimbus

The tray icon sits in the panel. A left click opens the window. A right click
opens the menu.

On Wayland, a left click brings the window to the front only when the window
is closed. A window that is open keeps its place, because the compositor
refuses a raise without a click of its own. The taskbar entry flashes instead.

The window shows the phase, the progress, the time since the last run, and
the last error. Each button shows only when it applies.

The tray icon and the first row of the menu both show the phase:

| Phase | Icon | What it means |
| --- | --- | --- |
| `idle` | <img src="data/icons/hicolor/scalable/apps/nimbus-idle-symbolic.svg" width="22" alt="The idle icon"> | The daemon runs and waits. No run is active. |
| `syncing` | <img src="data/icons/hicolor/scalable/apps/nimbus-syncing-symbolic.svg" width="22" alt="The syncing icon"> | A run is active. The window shows the progress of the run. |
| `paused` | <img src="data/icons/hicolor/scalable/apps/nimbus-paused-symbolic.svg" width="22" alt="The paused icon"> | You paused Nimbus. Later runs wait until you resume. |
| `error` | <img src="data/icons/hicolor/scalable/apps/nimbus-error-symbolic.svg" width="22" alt="The error icon"> | The last run failed. The line below the heading names the error. |
| `resync` | <img src="data/icons/hicolor/scalable/apps/nimbus-error-symbolic.svg" width="22" alt="The resync icon"> | The next run must compare every file. Click Resync, then confirm. |
| `offline` | <img src="data/icons/hicolor/scalable/apps/nimbus-offline-symbolic.svg" width="22" alt="The offline icon"> | The network is down. The daemon starts no run. The next run starts when the network returns. |
| no phase | <img src="data/icons/hicolor/scalable/apps/nimbus-offline-symbolic.svg" width="22" alt="The offline icon"> | No daemon answers on the bus. The window shows a Start daemon button. |

The last row has no phase on the bus. No daemon runs, so nothing sends one.
The two rows share one icon. The first row of the menu tells them apart:
"No internet connection" or "The daemon is not running".

The daemon moves to `offline` within one second after the machine loses its
default route, for example when you turn off Wi-Fi or pull a cable. A run that
is active stops at once. A run that fails with a network error also moves the
daemon to `offline`, and the error line shows the rclone text. This covers a
router that has no connection to the internet.

While the phase is `offline`, a file change or the interval does not start a
run. When the route returns, or when a run found a dead network, the daemon
checks the remote every 30 seconds. A Sync now click makes the check at once.
The sync starts when the check works.

The icon has no color of its own, so each desktop paints it with its own
palette. A host that looks up an icon name in a cache needs a cache rebuild
after an install. See [Troubleshoot Nimbus](#troubleshoot-nimbus).

| Button | What it does |
| --- | --- |
| Sync now | Starts a run. The call returns before the run finishes. |
| Resync | Shows in the `resync` phase only, in place of Sync now. Asks you to confirm, and then starts a resync. |
| Pause | Skips later runs. A run that is active finishes first. The button reads Resume while Nimbus is paused. |
| Start daemon | Shows only when the daemon does not run, in place of Sync now and Pause. Starts the systemd unit. |
| Settings | Opens the Settings dialog. |

The menu has the same commands:

| Item | What it does |
| --- | --- |
| The first row | The phase. The row is not a command. |
| Sync now | Starts a run. The item is disabled while a run is active. |
| Pause | A checkmark item. A tick shows that Nimbus is paused. Click it to pause or resume. |
| Settings | Opens the Settings dialog. |
| Quit | Stops the tray. The daemon stops within one second. The window close button only hides the window. |

The Settings dialog edits seven keys of the config file:

| Field | Key |
| --- | --- |
| Remote path | `remote` |
| Local folder | `local` |
| Interval in seconds | `interval_secs` |
| Quiet time in seconds | `debounce_secs` |
| Copy to keep on conflict | `conflict_resolve` |
| Action for the losing copy | `conflict_loser` |
| Extra rclone flags | `extra_flags` |

Type the flags on one line, with spaces between them. The two times stop at
86400, one day. A larger value needs the file. Pause and the resync flag have
no field, because the window and the menu control them.

The dialog checks the keys before Save sends them. If a key is wrong, the
reason shows under its field. The daemon checks the keys again and takes them
at once.

A moved `remote` or `local` has no bisync listing, so the next run is a full
resync. The dialog asks before it saves the move. The window then asks for the
resync, as for every other resync.

To edit the config file by hand:

1. Stop the daemon.

    ```console
    systemctl --user stop nimbusd.service
    ```

2. In the Settings dialog, click Open config file. The file opens in the
   editor that the desktop picks.
3. Save your edit.
4. In the window, click Start daemon.

> Stop the daemon before you edit the file. A daemon that runs does not read the
> file again. It writes the file on every pause, so a later pause overwrites
> your edit.

### Control Nimbus over D-Bus

Any client can read the state and send the same commands as the tray. The
daemon stops when no tray owns the name `io.github.luckjmg.Nimbus` on the bus.
A daemon that finds no tray stops after 60 seconds. A client must run while the
tray runs.

| | |
| --- | --- |
| Bus name | `io.github.luckjmg.nimbus` |
| Object path | `/io/github/luckjmg/nimbus` |
| Interface | `io.github.luckjmg.nimbus1` |
| Property | `State`, signature `(sdts)` |
| Methods | `SyncNow`, `SetPaused(b)`, `GetSettings`, `SetSettings(ssttssas)`, `Resync` |
| Signal | `Changed(State)` |

The `State` signature holds four values:

- The phase, as a string. The phase is `idle`, `syncing`, `paused`, `error`, `resync`, or `offline`.
- The progress, as a double from 0.0 to 1.0. It is zero while the daemon is idle.
- The time of the last finished run, as a Unix timestamp.
- The last error, as a string.

In the `resync` phase, the daemon waits until a client calls `Resync`. The
error string then holds the reason for the resync.

```console
busctl --user get-property io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 State
busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 SyncNow
busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 SetPaused b true
busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 SetPaused b false
busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 GetSettings
busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 SetSettings ssttssas drive '~/Cloud' 900 30 newer delete 0
```

The values of `ssttssas` are `remote`, `local`, `interval_secs`,
`debounce_secs`, `conflict_resolve`, `conflict_loser`, and `extra_flags`. The
`0` sends an empty list of flags. A refusal names the problem, for example:

```console
Call failed: The remote is empty. Set it to an rclone remote name, for example drive.
```

`busctl --user monitor` does not filter by name. To watch the signal, use
`dbus-monitor`:

```console
dbus-monitor --session "type='signal',interface='io.github.luckjmg.nimbus1'"
```

If you write a client in Rust, set `cache_properties(CacheProperties::No)` on
the proxy. A proxy refreshes its cache only on the standard
`PropertiesChanged` signal, and the daemon does not send it. A cached proxy
reports a stale state forever.

### Troubleshoot Nimbus

1. Read the line below the heading in the window. It names the last error.
2. Read the daemon log.

    ```console
    systemctl --user status nimbusd.service
    journalctl --user -u nimbusd.service
    ```

3. Find the issue in the table below.

The window shows each message below the heading. When the daemon refuses to
start, the heading reads "The daemon is not running". The daemon then also
sends a desktop notification, "Nimbus did not start". If Do Not Disturb is on,
look in the notification history.

A bad config stops the daemon on purpose. The unit stays inactive and does not
restart, because only you can fix the config. After the fix, click Start
daemon in the window.

| Message | Solution |
| --- | --- |
| The folder … does not exist. | Create the folder. Or, in the Settings dialog, set Local folder to a folder that exists. |
| The remote is empty. Set it to an rclone remote name, for example drive. | In the Settings dialog, set Remote path to the name of a remote from `rclone config`. |
| `interval_secs` must be above zero. | Set the interval to 1 or more. The same solution applies to `debounce_secs`. |
| Invalid config: `conflict_resolve` must be one of … | Set the key to one of the values that the message names. The same solution applies to `conflict_loser`. |
| Invalid config: unknown field `path` … | An older build wrote this key. Click Open config file in the Settings dialog, and then delete the `path` line. |
| Invalid config: … | The config file is not valid TOML, or a key has a wrong type. Click Open config file, and then fix the line that the message names. |
| nimbusd: another daemon already holds io.github.luckjmg.nimbus | This line is in the daemon log. Two daemons run, and only one can own the name. Stop the other daemon, and then start this one again. |

In the `resync` phase, the heading reads "A resync is needed". The line below
it gives one of three reasons. For each reason, click Resync and confirm. The
cost is one full pass over both sides.

| Reason | Cause |
| --- | --- |
| Nimbus has no record of a sync between this folder and the remote. | No sync between the two sides finished yet. A new config always starts here. A restart also shows this reason, because the daemon forgets the cause. |
| The folder or the remote changed, so Nimbus has no record of a sync between them. | You moved `local` or `remote`. rclone names its record after the pair of paths. |
| The last sync stopped before it saved its record of the files. | A run stopped, for example on a lost connection, before rclone wrote any record. The daemon repairs every other lost connection without a resync. |

Other issues:

| Issue | Solution |
| --- | --- |
| The heading reads "Error". | Read the rclone message below the heading, and the daemon log. The daemon tries again on the next interval. |
| A failed run takes a long time to retry. | A run that fails with a network error does not wait for `interval_secs`. The daemon checks the remote every 30 seconds, and it retries when the check works. Any other failed run waits for `interval_secs`. Set `interval_secs` to a low value, for example `300`, if such errors are frequent. |
| A run on a test remote fails with "directory not found". | Create the destination folder before the first run. Some remotes, for example Google Drive, create the folder themselves. Use an `alias` remote with an absolute path. It is the only test remote that resolves the same way from any working directory. |
| The tray shows a blank icon. | Rebuild the icon cache. See the commands below the table. |

A host looks up an icon name in its own cache. The cache does not know an icon
that you installed after the last rebuild. Nimbus sends the installed
directory in `IconThemePath`, but some hosts still need a cache rebuild:

```console
gtk4-update-icon-cache -f -t ~/.local/share/icons/hicolor
kiconcache6 -f ~/.local/share/icons        # KDE, the tool is called kiconcache5 on older releases
```

A `just install` into `/usr/share/icons` usually needs neither command,
because the distribution builds the cache for that directory. A
`just install-user` into your home directory needs one.

### Uninstall Nimbus

1. Remove Nimbus with the tool of your distribution, or run the recipe that
   matches your build.

    ```console
    # A package
    sudo apt remove nimbus           # Debian
    sudo dnf remove nimbus           # Fedora
    sudo pacman -R nimbus            # Arch

    # A build
    just uninstall          # after just install, needs root
    just uninstall-user     # after just install-user, needs no root
    ```

2. Disable the daemon unit if a package installed it.

    ```console
    systemctl --user disable --now nimbusd.service
    ```

    A package does not enable the unit, so this step needs nothing.

3. If you want to remove your settings too, delete `~/.config/nimbus/`.

    Every recipe and every package keeps the config file and the rclone remote,
    because both hold settings that you wrote. The directory also holds the
    `bisync/` record. A fresh install then starts with no record and rebuilds it
    on the first run.

## Contributing guidelines

Run the three checks before every commit, in this order. No CI runs them.
`just check` runs the same three.

```console
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

To cut a release, raise the `version` in `Cargo.toml`, write the release notes
in `docs/releases/<tag>.md`, and push the tag. The tag must start with a `v`.
A tag that disagrees with `Cargo.toml` stops the build.

```console
git tag v0.1.0-beta
git push origin main v0.1.0-beta
```

The workflow builds the binaries, packs them for Debian, Fedora, and Arch, and
publishes the release with the notes for the tag.

The unit tests do not cover the bus, the watcher, the panel, or rclone. Those
need a live run. `AGENTS.md` holds the procedure for the live run and the
traps that live runs found. It also holds the crash test for the listing
repair, which no unit test can cover.

`just sandbox` builds both binaries and runs them against a local folder that
stands in for the remote. So a run never reaches a real remote. The recipe stops
while a daemon holds the bus name, because one daemon owns
`io.github.luckjmg.nimbus` in a session. `just sandbox-stop` stops the daemon
that the recipe started.

| Crate | Role |
| --- | --- |
| `nimbus-ipc` | The D-Bus contract. Names, `State`, `Phase`, one proxy trait. No logic. |
| `nimbusd` | The daemon. Library plus binary. |
| `nimbus` | The tray. One binary: the window, the icon, and the menu. |

Write commit messages as Conventional Commits. Use the crate name as the
scope, for example `feat(daemon): add the watcher and the engine loop`.

## Additional documentation

For more information:

- [`AGENTS.md`](AGENTS.md): the live check, the crash test, the packaging rules, the design limits, and the traps in zbus, GTK, and rclone.
- [`justfile`](justfile): every build, install, and sandbox recipe, and the only list of installed files.
- [`packaging/`](packaging): the deb, the rpm, and the Arch package definitions.
- [`docs/releases/`](docs/releases): the release notes for each tag.
- [rclone bisync](https://rclone.org/bisync/): the rclone command that Nimbus runs.

## How to get help

- [GitHub issues](https://github.com/LuckJMG/Nimbus/issues): report a bug or ask a question. Attach the output of `journalctl --user -u nimbusd.service`.
- [Troubleshoot Nimbus](#troubleshoot-nimbus): the known issues and their solutions.

## Terms of use

Nimbus is licensed under the [MIT License](LICENSE).
