#!/bin/sh
# Build gnutshell and install it as a GDM session that runs on gnome-session.
# Usage: dist/install.sh            install
#        dist/install.sh uninstall  remove everything this script installed
set -eu
cd "$(dirname "$0")/.."

# Run as your user: the build and `systemctl --user` must not run as root.
# The script calls sudo itself for the system files.
if [ "$(id -u)" -eq 0 ]; then
    echo "Run dist/install.sh as your normal user, without sudo." >&2
    exit 1
fi

BIN=/usr/local/bin/gnutshell
SESSION=/usr/share/wayland-sessions/gnutshell.desktop
GS_SESSION=/usr/share/gnome-session/sessions/gnutshell.session
PAM=/etc/pam.d/gnutshell
UNITS=/etc/systemd/user
DROPIN="$UNITS/gnome-session@gnutshell.target.d"

if [ "${1:-}" = uninstall ]; then
    sudo rm -f "$BIN" "$SESSION" "$GS_SESSION" "$PAM" "$UNITS/gnutshell.service" "$DROPIN/session.conf"
    sudo rmdir "$DROPIN" 2>/dev/null || true
    systemctl --user daemon-reload
    echo "gnutshell uninstalled"
    exit 0
fi

cargo build --release
sudo install -Dm755 target/release/gnutshell "$BIN"
sudo install -Dm644 dist/gnutshell.desktop "$SESSION"
sudo install -Dm644 dist/gnutshell.session "$GS_SESSION"
sudo install -Dm644 dist/pam/gnutshell "$PAM"
sudo install -Dm644 dist/systemd/gnutshell.service "$UNITS/gnutshell.service"
sudo install -Dm644 "dist/systemd/gnome-session@gnutshell.target.d/session.conf" "$DROPIN/session.conf"
# The user manager outlives logouts; make it see the new units.
systemctl --user daemon-reload
echo "gnutshell installed. Log out, then pick \"gnutshell\" on the GDM login screen."
