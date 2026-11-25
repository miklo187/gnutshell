#!/bin/sh
# Build gnative and install it as a GDM session that runs on gnome-session.
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

BIN=/usr/local/bin/gnative
SESSION=/usr/share/wayland-sessions/gnative.desktop
GS_SESSION=/usr/share/gnome-session/sessions/gnative.session
PAM=/etc/pam.d/gnative
UNITS=/etc/systemd/user
DROPIN="$UNITS/gnome-session@gnative.target.d"

if [ "${1:-}" = uninstall ]; then
    sudo rm -f "$BIN" "$SESSION" "$GS_SESSION" "$PAM" "$UNITS/gnative.service" "$DROPIN/session.conf"
    sudo rmdir "$DROPIN" 2>/dev/null || true
    systemctl --user daemon-reload
    echo "gnative uninstalled"
    exit 0
fi

cargo build --release
sudo install -Dm755 target/release/gnative "$BIN"
sudo install -Dm644 dist/gnative.desktop "$SESSION"
sudo install -Dm644 dist/gnative.session "$GS_SESSION"
sudo install -Dm644 dist/pam/gnative "$PAM"
sudo install -Dm644 dist/systemd/gnative.service "$UNITS/gnative.service"
sudo install -Dm644 "dist/systemd/gnome-session@gnative.target.d/session.conf" "$DROPIN/session.conf"
# The user manager outlives logouts; make it see the new units.
systemctl --user daemon-reload
echo "gnative installed. Log out, then pick \"gnative\" on the GDM login screen."
