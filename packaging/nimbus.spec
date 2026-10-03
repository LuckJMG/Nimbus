# The build workflow compiles the two binaries with cargo and hands this spec
# one tarball, so the spec has no BuildRequires and no %build section.
Name:           nimbus
Version:        @VERSION@
Release:        1%{?dist}
Summary:        Keep a local folder in sync with any rclone remote
License:        MIT
URL:            https://github.com/LuckJMG/Nimbus
# The workflow only has an x86_64 runner, so the package needs one.
ExclusiveArch:  x86_64

# The tray links GTK 4, so the window cannot open without it. The daemon links
# nothing but libc.
Requires:       gtk4 >= 4.10
# rclone 1.71 promoted bisync from beta to stable, so an older rclone cannot
# sync. Fedora ships a new one, so this only names the version that works.
Recommends:     rclone >= 1.71

%global debug_package %{nil}

%description
Nimbus wraps rclone bisync. A daemon runs the sync on a schedule and after
each file change, and a tray icon shows the state and takes your commands.

The daemon repairs the record that rclone left behind, so a lost connection
needs no action from you. Only a record that no spare can restore needs a
resync, and the daemon asks before it starts one.

%prep
# The release tarball holds one directory around its files. The strip drops
# that directory, so the paths below start at the payload.
tar -xf %{_sourcedir}/nimbus-src.tar.gz --strip-components=1

%install
rm -rf %{buildroot}
install -Dm755 nimbusd %{buildroot}%{_bindir}/nimbusd
install -Dm755 nimbus %{buildroot}%{_bindir}/nimbus
install -Dm644 data/systemd/user/nimbusd.service %{buildroot}/usr/lib/systemd/user/nimbusd.service
install -Dm644 data/dbus-1/services/io.github.luckjmg.Nimbus.service %{buildroot}/usr/share/dbus-1/services/io.github.luckjmg.Nimbus.service
install -Dm644 data/applications/io.github.luckjmg.Nimbus.desktop %{buildroot}/usr/share/applications/io.github.luckjmg.Nimbus.desktop
# A package cannot write into the home directory of the user, so the autostart
# entry takes the system path. Every desktop reads /etc/xdg.
install -Dm644 data/autostart/io.github.luckjmg.Nimbus.desktop %{buildroot}/etc/xdg/autostart/io.github.luckjmg.Nimbus.desktop
for name in idle syncing paused offline error; do
    install -Dm644 data/icons/hicolor/scalable/apps/nimbus-$name-symbolic.svg %{buildroot}/usr/share/icons/hicolor/scalable/apps/nimbus-$name-symbolic.svg
done
install -Dm644 LICENSE %{buildroot}/usr/share/licenses/nimbus/LICENSE
install -Dm644 README.md %{buildroot}/usr/share/doc/nimbus/README.md

%files
%{_bindir}/nimbus
%{_bindir}/nimbusd
/usr/lib/systemd/user/nimbusd.service
/usr/share/dbus-1/services/io.github.luckjmg.Nimbus.service
/usr/share/applications/io.github.luckjmg.Nimbus.desktop
/etc/xdg/autostart/io.github.luckjmg.Nimbus.desktop
/usr/share/icons/hicolor/scalable/apps/nimbus-*-symbolic.svg
%license /usr/share/licenses/nimbus/LICENSE
%doc /usr/share/doc/nimbus/README.md

%changelog
* Sat Oct 03 2026 LuckJMG <25126199+LuckJMG@users.noreply.github.com> - @VERSION@-1
- The first packaged release.
