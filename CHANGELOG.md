# Changelog

## 0.1.2 (not yet released)

### Security

In 0.1.1 and before, any program in your session could replace or close another
program's notification, and gnutshell's own. After a replacement, clicks on it
went to the program that replaced it.

- While a notification's sender is on the session bus, only it may replace or
  close the notification, as in gnome-shell.
- Programs can't replace or close gnutshell's own notifications.
- A program that calls itself "gnutshell" is shown as "Unknown program".

## 0.1.1 (2026-11-28)

### Security

Update from 0.1.0. In 0.1.0, any program running in your session without a
sandbox could use gnutshell to capture the screen, and to overwrite your files
with that picture: it only had to take the unused bus name
`org.gnome.Screenshot`, which gnutshell trusted. gnome-shell 51 does not trust
that name.

- `org.gnome.Screenshot` may no longer call the screenshot API. As a result the
  API also refuses the `gnome-screenshot` tool.
- A file name given to the API that is not an absolute path must be a plain
  name, and never overwrites an existing file.
- `org.gnome.desktop.lockdown disable-save-to-disk` is now obeyed.

`xdg-desktop-portal-gnome` is now listed as required.

## 0.1.0 (2026-11-28)

First version. A GNOME-style shell on mutter 51, run by gnome-session.

- Panel with launcher button, window overview, workspace dots, pinned apps,
  clock with calendar, notification bell, keyboard layout, quick settings,
  network, volume and battery icons, and session menu.
- Taskbar with live window previews, pinned apps that can be reordered by
  dragging, and optional grouping by app.
- App launcher, Alt+Tab with live tiles, window menu with Always on Top and
  workspace moves.
- Notifications with a list, sounds, per-app settings and Do Not Disturb.
- Volume menu for outputs and microphone.
- Quick settings for dark style, night light, Do Not Disturb and Bluetooth.
- Screenshots and color picking, wallpaper, lock screen, polkit agent,
  end-session dialog, media keys, on-screen display and X11 app settings.

See the README for what each part does and does not do.
