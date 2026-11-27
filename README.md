# gnutshell

A GNOME-style shell written in Rust, running directly on **libmutter**: no GJS, no JavaScript.

gnutshell replaces the `gnome-shell` binary. It creates a `MetaContext` and registers a
Rust GObject subclass of `MetaPlugin` as the compositor plugin. Then it runs mutter's
main loop. Mutter handles compositing, Wayland, KMS, input and window management. gnutshell
supplies the shell: the UI drawn with Clutter, plus window effects.

## Current features

- Solid desktop background
- Top panel with a centered clock. The panel reserves screen space through workspace
  struts, so maximized windows stay below it. The panel hides when the primary monitor
  shows a fullscreen window.
- Session menu: a power button at the right of the panel opens Suspend,
  Restart, Power Off and Log Out. Mouse or keyboard (Up/Down, Enter,
  Escape). Everything except Suspend asks for confirmation first. Power
  actions go to systemd-logind with interactive authorization, so polkit can
  ask for a password through gnutshell's agent (for example when other users
  are logged in). Log Out ends gnutshell and with it the session. Actions that
  logind reports as unavailable are hidden.
- Notifications: gnutshell is the `org.freedesktop.Notifications` service. A
  banner below the panel shows the icon or image, app name, title, up to three
  lines of text and up to three action buttons. Click the banner for the default
  action, ✕ to dismiss it, or the copy button to put the title and full body
  on the clipboard. One banner shows at a time and the rest queue.
  Banners hide after 5 s (or the app's timeout, at most 30 s), and stay while
  the pointer is over them; critical ones stay until dismissed. Nothing shows
  on the lock screen; waiting notifications show after unlock. Over a
  fullscreen window only critical ones show. With Do Not Disturb
  (`org.gnome.desktop.notifications show-banners` off), other notifications are
  not shown as banners.
- Notification list: click the clock. It keeps notifications whose banner
  timed out, including ones hidden by Do Not Disturb, newest first. A blue dot
  next to the clock marks unread ones. Each entry has a copy button and ✕;
  "Clear all" empties the list. Scroll or use Up/Down; Escape or a click
  outside closes it. Dismissed, clicked and app-withdrawn notifications are
  not listed, nor are `transient` ones. While the list is open, new
  notifications appear in it instead of as banners. It holds up to 50 entries
  in memory, so logging out clears it. Entries have no pictures or actions.
- Window effects: fade/scale on map, destroy, minimize and unminimize
- App launcher: press Super to open a search field over a dimmed screen. Type to
  filter installed apps. Scroll with the mouse wheel or touchpad; use Up/Down or Tab to select, and Enter or a click to
  launch. Escape, Super or a click outside the box closes it. Apps launch through
  mutter's `MetaLaunchContext`, so they get the correct `WAYLAND_DISPLAY`/`DISPLAY`
  and an activation token. Set `GNUTSHELL_LAUNCHER=<command>` to run an external
  launcher instead.
- Runs as a full GNOME session on gnome-session, like gnome-shell does, as the
  systemd user unit `gnutshell.service`. gnome-session starts
  gnome-settings-daemon (power, color/night light, keyboard, media keys,
  housekeeping, …) and the autostart apps.
- Media keys and custom shortcuts: gnutshell provides the accelerator part of
  gnome-shell's `org.gnome.Shell` D-Bus API, which gsd-media-keys uses to bind
  volume, brightness, media and custom shortcut keys. As in gnome-shell, only
  gsd-media-keys, GNOME Settings and the GNOME portal may call it, and a client's
  grabs are released when it leaves the bus.
- On-screen display (`ShowOSD`): the volume/brightness popup, with a level bar
  and an over-amplification mark.
- Keyboard layouts from Settings (`org.gnome.desktop.input-sources`): up to 4
  XKB layouts, Super+Space / Shift+Super+Space to switch, the active layout
  shown in the panel (click to switch). The last used layout is remembered in
  `mru-sources`, as in gnome-shell. IBus input methods are not supported yet.
- Lock screen: Super+L (through gnome-settings-daemon and
  `org.gnome.ScreenSaver.Lock`), "Lock" in the session menu, or
  `loginctl lock-session`; also on idle and before suspend when
  `org.gnome.desktop.screensaver lock-enabled` is on (after `lock-delay`).
  The password is checked by PAM (service `gnutshell`, falling back to `login`)
  on a worker thread. While locked, windows are hidden and unredirection is
  off, every shortcut is blocked except VT switching, layout switching and
  media keys that gsd allows on the lock screen, and polkit prompts wait.
  logind's LockedHint and `ActiveChanged` are kept up to date.
- Workspaces: a panel indicator on the left (one dot per workspace, the
  active one wider; click a dot to switch, scroll over it to step), and a
  slide animation when switching (Super+PageUp/PageDown, Ctrl+Alt+arrows and
  the other mutter workspace shortcuts). With `org.gnome.mutter
  dynamic-workspaces` on, workspaces are managed like in gnome-shell: always
  one empty workspace at the end, at least two, empty ones in between are
  removed, the active one is kept. With it off, mutter's fixed
  `num-workspaces` applies.
- App icons in the launcher and the switcher. Icons are looked up per the
  freedesktop icon theme spec: your `icon-theme` setting, its `Inherits`
  chain, `hicolor`, then `/usr/share/pixmaps`. They are rendered with
  librsvg/cairo, with no GTK and no gdk-pixbuf, and cached as GPU textures.
  Changing the icon theme takes effect live. In the switcher, each window is
  matched to its app by sandboxed/GTK application id, Wayland app id or
  `WM_CLASS`, or a desktop file's `StartupWMClass`.
- Window switcher: Alt+Tab or Super+Tab (your `switch-applications`/`switch-windows`
  keybindings). Windows on the current workspace are listed in most-recently-used
  order. Tab and the arrow keys cycle, Shift+Tab goes back. Releasing the modifier
  activates the selection, and Escape cancels. The popup appears only after 120 ms,
  so a quick Alt+Tab just flips windows. This version lists windows, not apps
  grouped by window.

- Polkit authentication agent: a modal "Authentication Required" dialog for
  `pkexec`, software installs, system settings and similar. It drives
  libpolkit-agent-1's `PolkitAgentSession`, which runs the setuid PAM helper.
  The password goes from the entry straight to that helper and is never stored.
  Supports retry after a wrong password, Escape/Cancel, multiple requests in a
  queue, and cancellation from polkitd. The current user is preferred as the
  identity when polkit offers it.

## Build

Requires mutter 51 development files (`libmutter-51.pc`), polkit
(`polkit-agent-1.pc`), cairo, librsvg, libsystemd and Rust. Running it as a
session needs gnome-session and gnome-settings-daemon.

```sh
cargo build --release
```

## Run

**Nested, inside your current session.** The private D-Bus session avoids clashing
with the running gnome-shell's bus names:

```sh
dbus-run-session -- ./target/release/gnutshell --wayland --devkit
```

Clients connect via the Wayland display name printed in the log, for example:

```sh
WAYLAND_DISPLAY=wayland-1 kgx
```

**Headless smoke test:**

```sh
./target/release/gnutshell --wayland --headless --virtual-monitor 1280x720 --wayland-display gnutshell-test
```

**Launcher test without input devices** (debug builds only). This opens the launcher,
types the query, and launches the top match:

```sh
GNUTSHELL_DEBUG_LAUNCH=console dbus-run-session -- ./target/debug/gnutshell --wayland --headless --virtual-monitor 1280x720
```

**Polkit agent while nested.** Your gnome-shell already owns the agent for the
login session, so a nested gnutshell registers only for one process. Start gnutshell
from one terminal, then run `pkexec` from another:

```sh
# terminal A: note this shell's PID
echo $$
# terminal B
GNUTSHELL_POLKIT_PID=<pid from A> dbus-run-session -- ./target/release/gnutshell --wayland --devkit
# back in terminal A: the dialog appears inside gnutshell
pkexec true
```

A headless test that never types a password: register for a test shell, run
`pkcheck --action-id org.freedesktop.policykit.exec --process $$ --allow-user-interaction`,
and send Escape with `GNUTSHELL_DEBUG_KEYS`. `pkcheck` exits with 3 (dismissed).
Don't script wrong passwords: `pam_faillock` locks the account after 3 failures.

**Keyboard-driven test** (debug builds only): `GNUTSHELL_DEBUG_KEYS="<ms>:<tokens>"`
types through a virtual keyboard after `<ms>`, one token every 80 ms. Tokens are
characters to type, `Super`, `Return`, `Escape`, `Tab`, `Up`, `Down`, `Alt+Tab`, `Alt-down` /
`Alt-up` (hold or release Alt), raw keysyms `0x…` (tap) or `down:0x…` / `up:0x…`, pointer `move:X,Y`, `click`, `wheel-up` / `wheel-down`, and `_` (pause). `GNUTSHELL_DEBUG_SCREENSHOT="<ms>,...:<prefix>"`
saves raw BGRA screenshots at those times.

`GNUTSHELL_DEBUG_PAM_USER=<user>` checks lock screen passwords against another
(e.g. nonexistent) user, so test attempts never count against your account.
`GNUTSHELL_DEBUG_SESSION_MENU=<ms>` opens the session menu. Only drive it toward
Log Out in tests: the other entries really suspend, restart or power off.

**Isolated settings in tests:** set `XDG_CONFIG_HOME` to a temporary directory
*before* `dbus-run-session`, not inside it. Otherwise the private bus starts
`dconf-service` with your real config, and `gsettings set` changes your own
settings.

**Switcher test** (debug builds only): `GNUTSHELL_DEBUG_SWITCH=<ms>` runs Alt+Tab on
its own after the given number of milliseconds and logs the focused window before and
after. Open at least two windows first.

Always use `dbus-run-session` for nested or headless runs. Otherwise D-Bus-activated
apps start on your real desktop's session bus.

**As a real session from GDM, on gnome-session:**

```sh
dist/install.sh            # builds, then installs with sudo
dist/install.sh uninstall  # removes it again
```

This installs:

- `/usr/local/bin/gnutshell`
- `/usr/share/wayland-sessions/gnutshell.desktop` (runs `gnome-session --session=gnutshell`)
- `/usr/share/gnome-session/sessions/gnutshell.session` (gnome-session refuses unknown sessions)
- `/etc/pam.d/gnutshell` (lock screen password check, `auth include login`)
- `/etc/systemd/user/gnutshell.service`
- `/etc/systemd/user/gnome-session@gnutshell.target.d/session.conf`, which makes the
  session start gnutshell instead of gnome-shell, plus the usual session services

Log out of GNOME first; gnome-session refuses to start a second graphical
session for the same user. Then pick "gnutshell" on the GDM login screen. If the
session fails, GDM shows the login screen again, and you can pick GNOME.
Logs: `journalctl --user -u gnutshell.service -b`.

**Quick test from a text console, without gnome-session:** log in on a VT
(Ctrl+Alt+F3) and run
`dbus-run-session -- ./target/release/gnutshell --wayland --display-server`.
No session services run in this mode.

All mutter options work (`--help`).

## Layout

| File | Purpose |
|------|---------|
| `src/main.rs` | `MetaContext` bootstrap, signal handling |
| `src/plugin.rs` | `MetaPlugin` subclassing glue + the gnutshell shell |
| `src/launcher.rs` | Modal app launcher + ranking (unit-tested) |
| `src/switcher.rs` | Alt+Tab window switcher |
| `src/polkit.rs` | `PolkitAgentListener` subclass + authentication dialog |
| `src/shell_dbus.rs` | `org.gnome.Shell` subset: accelerators, OSD, properties |
| `src/notifications.rs` | `org.freedesktop.Notifications` service and banner |
| `src/notifications/list.rs` | Notification list opened from the clock |
| `src/osd.rs` | Volume/brightness popup |
| `src/keyboard.rs` | Input sources: XKB keymap, layout switching, panel indicator |
| `src/lockscreen.rs` | Lock screen, `org.gnome.ScreenSaver`, logind lock/sleep, idle |
| `src/pam.rs` | PAM password check on a worker thread |
| `src/workspaces.rs` | Workspace indicator, switch animation, dynamic workspaces |
| `src/session.rs` | Panel power button, session menu, logind actions |
| `src/icons.rs` | Icon theme lookup, SVG/PNG loading, window-to-app matching |
| `src/ui.rs` | Shared colors, keysyms, Clutter helpers |
| `src/ffi.rs` | Hand-written FFI for the libmutter / Clutter / Cogl subset in use |
| `dist/` | GDM session entry, systemd units, `install.sh` |
| `build.rs` | pkg-config linking + rpath for `/usr/lib/mutter-51` |

## Caveats

- **libmutter has no stable ABI.** The soname changes with every GNOME release
  (`libmutter-51`). After a mutter upgrade, re-check `src/ffi.rs`, especially
  `MetaPluginClass`, which must match `meta/meta-plugin.h` field for field. Bump
  `MUTTER_API` in `build.rs` too.
- **Exit skips `meta_context_destroy`.** In mutter 51, that call segfaults inside
  Clutter's teardown once any text has been rendered (a use-after-free in the Pango
  pipeline cache). The process is exiting anyway, so gnutshell skips the teardown. Set
  `GNUTSHELL_FULL_TEARDOWN=1` to run it anyway, for debugging.
- **Only part of `org.gnome.Shell` is implemented**: accelerators, `ShowOSD`
  and the `Mode`/`OverviewActive` properties. Screenshots, screencasts, `Eval`
  and the extensions API are missing.
- **Notifications:** per-app notification settings,
  sounds and `org.gtk.Notifications` are not supported (GTK apps fall back to
  `org.freedesktop.Notifications`).
- **Log Out doesn't ask apps to save first.** Under gnome-session it requests
  a forced logout (gnutshell has no end-session dialog for inhibitors yet);
  without gnome-session it just ends gnutshell.
- **No keyring prompt yet.** Keyring unlock prompts come from
  gcr's system prompter, which gnome-shell normally replaces.
- **Icons support only SVG and PNG.** XPM files are skipped, and the generic
  fallback icon is shown instead.
