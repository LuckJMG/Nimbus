# Nimbus build and install recipes. Run `just` for the list.

set shell := ["bash", "-uc"]

# The two release binaries land in target/release.
prefix := "/usr"
home := env_var("HOME")

# The scratch tree for a sandbox run. No value in ~/.config or ~/.cache moves
# here, because the recipe sets XDG_CONFIG_HOME and RCLONE_CONFIG.
sandbox := "/tmp/scratch"

# List the recipes.
default:
    @just --list

# Build both binaries for release.
build:
    cargo build --release --workspace

# Run the three checks, in the order that catches the most first.
check:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# Install into /usr, then start the daemon at login.
install: build
    sudo install -Dm755 target/release/nimbusd {{prefix}}/bin/nimbusd
    sudo install -Dm755 target/release/nimbus {{prefix}}/bin/nimbus
    sudo install -Dm644 data/systemd/user/nimbusd.service {{prefix}}/lib/systemd/user/nimbusd.service
    sudo install -Dm644 data/dbus-1/services/io.github.luckjmg.Nimbus.service {{prefix}}/share/dbus-1/services/io.github.luckjmg.Nimbus.service
    sudo install -Dm644 data/applications/io.github.luckjmg.Nimbus.desktop {{prefix}}/share/applications/io.github.luckjmg.Nimbus.desktop
    sudo install -Dm644 data/icons/hicolor/scalable/apps/nimbus-sync-symbolic.svg {{prefix}}/share/icons/hicolor/scalable/apps/nimbus-sync-symbolic.svg
    install -Dm644 data/applications/io.github.luckjmg.Nimbus.desktop {{home}}/.config/autostart/io.github.luckjmg.Nimbus.desktop
    systemctl --user daemon-reload
    systemctl --user enable --now nimbusd.service
    @echo "The tray starts at the next login, or now with: just run-tray"

# Install into the home directory, with no root.
install-user: build
    # The three files carry an absolute path, so the recipe rewrites it. A
    # D-Bus service file expands neither $HOME nor %h, so that path is absolute.
    install -Dm755 target/release/nimbusd {{home}}/.local/bin/nimbusd
    install -Dm755 target/release/nimbus {{home}}/.local/bin/nimbus
    sed 's|/usr/bin/nimbusd|$HOME/.local/bin/nimbusd|' data/systemd/user/nimbusd.service \
        | sed "s|\$HOME|{{home}}|" > {{home}}/.config/systemd/user/nimbusd.service
    sed "s|/usr/bin/nimbus|{{home}}/.local/bin/nimbus|" data/dbus-1/services/io.github.luckjmg.Nimbus.service \
        > {{home}}/.local/share/dbus-1/services/io.github.luckjmg.Nimbus.service
    sed "s|/usr/bin/nimbus|{{home}}/.local/bin/nimbus|" data/applications/io.github.luckjmg.Nimbus.desktop \
        > {{home}}/.local/share/applications/io.github.luckjmg.Nimbus.desktop
    install -Dm644 data/applications/io.github.luckjmg.Nimbus.desktop {{home}}/.config/autostart/io.github.luckjmg.Nimbus.desktop
    install -Dm644 data/icons/hicolor/scalable/apps/nimbus-sync-symbolic.svg {{home}}/.local/share/icons/hicolor/scalable/apps/nimbus-sync-symbolic.svg
    systemctl --user daemon-reload
    systemctl --user enable --now nimbusd.service
    @echo "Check the rewritten paths before you trust them:"
    @echo "  grep Exec {{home}}/.config/systemd/user/nimbusd.service {{home}}/.local/share/dbus-1/services/io.github.luckjmg.Nimbus.service"

# Start the tray now, without waiting for a login.
run-tray:
    if [ -x "{{home}}/.local/bin/nimbus" ]; then exec "{{home}}/.local/bin/nimbus"; else exec "{{prefix}}/bin/nimbus"; fi

# Refuse to start while a daemon already holds the bus name.
sandbox-guard:
    @if pgrep -x nimbusd >/dev/null; then \
        echo "A daemon is already running. Stop it first:"; \
        echo "  systemctl --user stop nimbusd.service"; \
        exit 1; \
    fi

# Run the debug binaries against a local folder that stands in for the remote.
sandbox: sandbox-guard
    rm -rf {{sandbox}}
    mkdir -p {{sandbox}}/cfg/nimbus {{sandbox}}/local {{sandbox}}/remote/Nimbus
    printf '[drive]\ntype = alias\nremote = %s/remote\n' {{sandbox}} > {{sandbox}}/cfg/rclone.conf
    printf 'remote = "drive"\npath = "Nimbus"\nlocal = "%s/local"\npaused = false\ninterval_secs = 60\ndebounce_secs = 3\nresync_pending = true\n' {{sandbox}} > {{sandbox}}/cfg/nimbus/config.toml
    # rclone writes an empty listing when both sides hold no file, and then it
    # refuses to sync from it. One file makes the first listing usable.
    echo seed > {{sandbox}}/local/seed.txt
    cargo build --workspace
    # One line, because just gives each line a shell of its own and `$!` names
    # a job of the shell that started it.
    XDG_CONFIG_HOME={{sandbox}}/cfg RCLONE_CONFIG={{sandbox}}/cfg/rclone.conf ./target/debug/nimbusd > {{sandbox}}/nimbusd.log 2>&1 & echo $! > {{sandbox}}/nimbusd.pid
    @echo "The daemon log is {{sandbox}}/nimbusd.log"
    @echo "Stop the daemon with: just sandbox-stop"
    @echo "  busctl --user get-property io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 State"
    @echo "  busctl --user call io.github.luckjmg.nimbus /io/github/luckjmg/nimbus io.github.luckjmg.nimbus1 SyncNow"
    @echo "  dbus-monitor --session \"type='signal',interface='io.github.luckjmg.nimbus1'\""
    exec ./target/debug/nimbus

# End the daemon that the sandbox recipe started.
sandbox-stop:
    @if [ -f {{sandbox}}/nimbusd.pid ]; then \
        kill "$(cat {{sandbox}}/nimbusd.pid)" && rm -f {{sandbox}}/nimbusd.pid; \
    else \
        echo "No pid file at {{sandbox}}/nimbusd.pid"; \
    fi

# Remove the files that install-user added. Needs no root.
uninstall-user:
    # The config file and the rclone remote stay, because both hold settings
    # that the user wrote.
    -systemctl --user disable --now nimbusd.service
    rm -f {{home}}/.config/autostart/io.github.luckjmg.Nimbus.desktop
    rm -f {{home}}/.config/systemd/user/nimbusd.service
    rm -f {{home}}/.local/bin/nimbusd {{home}}/.local/bin/nimbus
    rm -f {{home}}/.local/share/dbus-1/services/io.github.luckjmg.Nimbus.service
    rm -f {{home}}/.local/share/applications/io.github.luckjmg.Nimbus.desktop
    rm -f {{home}}/.local/share/icons/hicolor/scalable/apps/nimbus-sync-symbolic.svg
    systemctl --user daemon-reload

# Remove the files that install added. Needs root.
uninstall:
    sudo rm -f {{prefix}}/bin/nimbusd {{prefix}}/bin/nimbus
    sudo rm -f {{prefix}}/lib/systemd/user/nimbusd.service
    sudo rm -f {{prefix}}/share/dbus-1/services/io.github.luckjmg.Nimbus.service
    sudo rm -f {{prefix}}/share/applications/io.github.luckjmg.Nimbus.desktop
    sudo rm -f {{prefix}}/share/icons/hicolor/scalable/apps/nimbus-sync-symbolic.svg
    sudo rm -f {{home}}/.config/autostart/io.github.luckjmg.Nimbus.desktop
    systemctl --user daemon-reload
