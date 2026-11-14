# gnative

A GNOME-style shell written in Rust, running directly on **libmutter**: no GJS, no JavaScript.

gnative replaces the `gnome-shell` binary. It creates a `MetaContext` and registers a
Rust GObject subclass of `MetaPlugin` as the compositor plugin. Then it runs mutter's
main loop. Mutter handles compositing, Wayland, KMS, input and window management. gnative
supplies the shell: the UI drawn with Clutter, plus window effects.

## Current features

- Solid desktop background
- Top panel with a centered clock. The panel reserves screen space through workspace
  struts, so maximized windows stay below it. The panel hides when the primary monitor
  shows a fullscreen window.
- Window effects: fade/scale on map, destroy, minimize and unminimize
- App launcher: press Super to open a search field over a dimmed screen. Type to
  filter installed apps. Use Up/Down or Tab to select, and Enter or a click to
  launch. Escape, Super or a click outside the box closes it. Apps launch through
  mutter's `MetaLaunchContext`, so they get the correct `WAYLAND_DISPLAY`/`DISPLAY`
  and an activation token. Set `GNATIVE_LAUNCHER=<command>` to run an external
  launcher instead.
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
(`polkit-agent-1.pc`) and Rust.

```sh
cargo build --release
```

## Run

**Nested, inside your current session.** The private D-Bus session avoids clashing
with the running gnome-shell's bus names:

```sh
dbus-run-session -- ./target/release/gnative --wayland --devkit
```

Clients connect via the Wayland display name printed in the log, for example:

```sh
WAYLAND_DISPLAY=wayland-1 kgx
```

**Headless smoke test:**

```sh
./target/release/gnative --wayland --headless --virtual-monitor 1280x720 --wayland-display gnative-test
```

**Launcher test without input devices** (debug builds only). This opens the launcher,
types the query, and launches the top match:

```sh
GNATIVE_DEBUG_LAUNCH=console dbus-run-session -- ./target/debug/gnative --wayland --headless --virtual-monitor 1280x720
```

**Polkit agent while nested.** Your gnome-shell already owns the agent for the
login session, so a nested gnative registers only for one process. Start gnative
from one terminal, then run `pkexec` from another:

```sh
# terminal A: note this shell's PID
echo $$
# terminal B
GNATIVE_POLKIT_PID=<pid from A> dbus-run-session -- ./target/release/gnative --wayland --devkit
# back in terminal A: the dialog appears inside gnative
pkexec true
```

A headless test that never types a password: register for a test shell, run
`pkcheck --action-id org.freedesktop.policykit.exec --process $$ --allow-user-interaction`,
and send Escape with `GNATIVE_DEBUG_KEYS`. `pkcheck` exits with 3 (dismissed).
Don't script wrong passwords: `pam_faillock` locks the account after 3 failures.

**Switcher test** (debug builds only): `GNATIVE_DEBUG_SWITCH=<ms>` runs Alt+Tab on
its own after the given number of milliseconds and logs the focused window before and
after. Open at least two windows first.

Always use `dbus-run-session` for nested or headless runs. Otherwise D-Bus-activated
apps start on your real desktop's session bus.

**As a real session from GDM:** install the binary, then copy the session file.

```sh
sudo install -Dm755 target/release/gnative /usr/local/bin/gnative
sudo install -Dm644 dist/gnative.desktop /usr/share/wayland-sessions/gnative.desktop
```

Pick "gnative" on the GDM login screen. Keep a normal GNOME session available as a
fallback.

All mutter options work (`--help`).

## Layout

| File | Purpose |
|------|---------|
| `src/main.rs` | `MetaContext` bootstrap, signal handling |
| `src/plugin.rs` | `MetaPlugin` subclassing glue + the gnative shell |
| `src/launcher.rs` | Modal app launcher + ranking (unit-tested) |
| `src/switcher.rs` | Alt+Tab window switcher |
| `src/polkit.rs` | `PolkitAgentListener` subclass + authentication dialog |
| `src/ui.rs` | Shared colors, keysyms, Clutter helpers |
| `src/ffi.rs` | Hand-written FFI for the libmutter / Clutter / Cogl subset in use |
| `build.rs` | pkg-config linking + rpath for `/usr/lib/mutter-51` |

## Caveats

- **libmutter has no stable ABI.** The soname changes with every GNOME release
  (`libmutter-51`). After a mutter upgrade, re-check `src/ffi.rs`, especially
  `MetaPluginClass`, which must match `meta/meta-plugin.h` field for field. Bump
  `MUTTER_API` in `build.rs` too.
- **Exit skips `meta_context_destroy`.** In mutter 51, that call segfaults inside
  Clutter's teardown once any text has been rendered (a use-after-free in the Pango
  pipeline cache). The process is exiting anyway, so gnative skips the teardown. Set
  `GNATIVE_FULL_TEARDOWN=1` to run it anyway, for debugging.
- **gnative doesn't implement the `org.gnome.Shell` D-Bus APIs.** Screenshot, OSD and
  some portal and gnome-settings-daemon features that depend on them won't work.
- **No keyring prompt and no lock screen yet.** Keyring unlock prompts come from
  gcr's system prompter, which gnome-shell normally replaces.
- **The launcher shows no app icons yet.** It lists names only.
