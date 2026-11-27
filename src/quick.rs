//! Quick settings: a panel button with switches for the dark style, night
//! light, Do Not Disturb and Bluetooth, and the paired Bluetooth devices.
//!
//! The first three are GNOME settings; night light itself is applied by
//! gsd-color. Bluetooth comes from BlueZ. Pairing needs GNOME Settings.

use crate::bluetooth::{Bluetooth, Device};
use crate::ffi::{self, ClutterActor, MetaDisplay};
use crate::icons;
use crate::ui::{self, gobj};
use gio::prelude::*;
use std::cell::RefCell;
use std::ffi::CStr;
use std::rc::{Rc, Weak};

pub const BUTTON_WIDTH: f32 = 32.0;
const ICON_SIZE: f32 = 16.0;
const WIDTH: f32 = 320.0;
const PADDING: f32 = 10.0;
const ROW_HEIGHT: f32 = 40.0;
const SWITCH_WIDTH: f32 = 36.0;
const SWITCH_HEIGHT: f32 = 20.0;
const KNOB_SIZE: f32 = 14.0;
const NOTE_FONT: &CStr = c"Sans 9";
const OFF_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0x50, 0x50, 0x5a, 0xff);

/// When night light is on, in words, from its settings.
fn schedule_text(automatic: bool, from: f64, to: f64) -> String {
    if automatic {
        return "Sunset to sunrise".to_owned();
    }
    let clock = |hours: f64| {
        let minutes = (hours * 60.0).round() as i64;
        format!("{:02}:{:02}", minutes.div_euclid(60).rem_euclid(24), minutes.rem_euclid(60))
    };
    format!("{} – {}", clock(from), clock(to))
}

#[derive(Clone, Copy)]
enum Toggle {
    DarkStyle,
    NightLight,
    DoNotDisturb,
}

fn settings(schema: &str) -> Option<gio::Settings> {
    gio::SettingsSchemaSource::default()
        .and_then(|s| s.lookup(schema, true))
        .map(|_| gio::Settings::new(schema))
}

#[derive(Default)]
struct State {
    grab: Option<*mut ffi::ClutterGrab>,
    open: bool,
    /// The button's left edge in the panel.
    button_x: f32,
}

pub struct QuickSettings {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    panel_height: f32,
    button: *mut ClutterActor,
    button_icon: *mut ClutterActor,
    overlay: *mut ClutterActor,
    menu: *mut ClutterActor,
    interface: Option<gio::Settings>,
    color: Option<gio::Settings>,
    notifications: Option<gio::Settings>,
    bluetooth: Rc<Bluetooth>,
    state: RefCell<State>,
}

impl QuickSettings {
    pub fn new(
        display: *mut MetaDisplay,
        stage: *mut ClutterActor,
        panel: *mut ClutterActor,
        panel_height: f32,
    ) -> Rc<Self> {
        let this = unsafe {
            let button = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(button, BUTTON_WIDTH, panel_height);
            ffi::clutter_actor_set_reactive(button, 1);
            let button_icon = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(button_icon, ICON_SIZE, ICON_SIZE);
            ffi::clutter_actor_set_position(
                button_icon,
                ((BUTTON_WIDTH - ICON_SIZE) / 2.0).floor(),
                ((panel_height - ICON_SIZE) / 2.0).floor(),
            );
            ffi::clutter_actor_add_child(button, button_icon);
            ffi::clutter_actor_add_child(panel, button);

            let overlay = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(overlay, 1);
            ffi::clutter_actor_hide(overlay);
            let menu = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(menu, &ui::BOX_COLOR);
            ffi::clutter_actor_set_reactive(menu, 1);
            ffi::clutter_actor_add_child(overlay, menu);
            ffi::clutter_actor_add_child(stage, overlay);

            Rc::new(Self {
                display,
                stage,
                panel_height,
                button,
                button_icon,
                overlay,
                menu,
                interface: settings("org.gnome.desktop.interface"),
                color: settings("org.gnome.settings-daemon.plugins.color"),
                notifications: settings("org.gnome.desktop.notifications"),
                bluetooth: Bluetooth::new(),
                state: RefCell::default(),
            })
        };
        this.connect_signals();
        this
    }

    fn connect_signals(self: &Rc<Self>) {
        crate::windows::hover_highlight(self.button, ui::TRANSPARENT, ui::PANEL_HOVER_COLOR);
        let weak = Rc::downgrade(self);
        unsafe { gobj(self.button) }.connect_local("button-press-event", false, move |_| {
            with(&weak, |q| q.open());
            Some(true.to_value())
        });
        let overlay = unsafe { gobj(self.overlay) };
        let weak = Rc::downgrade(self);
        overlay.connect_local("key-press-event", false, move |args| {
            if ui::key_symbol(ui::event_arg(args)) == ui::KEY_ESCAPE {
                with(&weak, |q| q.close());
            }
            Some(true.to_value())
        });
        // A click outside closes.
        let weak = Rc::downgrade(self);
        overlay.connect_local("button-press-event", false, move |_| {
            with(&weak, |q| q.close());
            Some(true.to_value())
        });
        unsafe { gobj(self.menu) }.connect_local("button-press-event", false, |_| Some(true.to_value()));

        // Show changes made elsewhere.
        let watched = [
            (&self.interface, "color-scheme"),
            (&self.notifications, "show-banners"),
            (&self.color, "night-light-enabled"),
        ];
        for (settings, key) in watched {
            if let Some(settings) = settings {
                let weak = Rc::downgrade(self);
                settings.connect_changed(Some(key), move |_, _| with(&weak, |q| q.refresh()));
            }
        }
        let weak = Rc::downgrade(self);
        self.bluetooth.set_listener(move || with(&weak, |q| q.refresh()));
    }

    /// Place the button so that it ends at `right_edge` in the panel.
    pub fn place(&self, right_edge: f32) {
        let x = right_edge - BUTTON_WIDTH;
        self.state.borrow_mut().button_x = x;
        unsafe { ffi::clutter_actor_set_position(self.button, x, 0.0) };
        self.update_icon();
    }

    fn update_icon(&self) {
        let scale = ui::primary_monitor_scale(self.display);
        let content = icons::symbolic("emblem-system-symbolic", &ui::TEXT_COLOR, ICON_SIZE, scale, self.stage);
        let content = content.as_ref().map_or(std::ptr::null_mut(), |c| c.as_ptr() as *mut ffi::ClutterContent);
        unsafe { ffi::clutter_actor_set_content(self.button_icon, content) };
    }

    fn is_on(&self, toggle: Toggle) -> Option<bool> {
        match toggle {
            Toggle::DarkStyle => self.interface.as_ref().map(|s| s.string("color-scheme") == "prefer-dark"),
            Toggle::NightLight => self.color.as_ref().map(|s| s.boolean("night-light-enabled")),
            Toggle::DoNotDisturb => self.notifications.as_ref().map(|s| !s.boolean("show-banners")),
        }
    }

    fn flip(&self, toggle: Toggle) {
        let Some(on) = self.is_on(toggle) else { return };
        let result = match toggle {
            Toggle::DarkStyle => self.interface.as_ref().map(|s| {
                s.set_string("color-scheme", if on { "default" } else { "prefer-dark" })
            }),
            Toggle::NightLight => self.color.as_ref().map(|s| s.set_boolean("night-light-enabled", !on)),
            // Switched on means no banners.
            Toggle::DoNotDisturb => self.notifications.as_ref().map(|s| s.set_boolean("show-banners", on)),
        };
        if let Some(Err(e)) = result {
            glib::g_warning!("gnutshell", "could not change the setting: {e}");
        }
    }

    pub fn open(self: &Rc<Self>) {
        if self.state.borrow().open || crate::lockscreen::is_locked() {
            return;
        }
        self.state.borrow_mut().open = true;
        self.build();
        unsafe {
            ffi::clutter_actor_show(self.overlay);
            ffi::clutter_actor_set_child_above_sibling(self.stage, self.overlay, std::ptr::null_mut());
            let grab = ffi::clutter_stage_grab(self.stage as *mut ffi::ClutterStage, self.overlay);
            self.state.borrow_mut().grab = Some(grab);
            ffi::clutter_actor_grab_key_focus(self.overlay);
        }
    }

    pub fn close(&self) {
        let grab = {
            let mut state = self.state.borrow_mut();
            if !state.open {
                return;
            }
            state.open = false;
            state.grab.take()
        };
        unsafe {
            if let Some(grab) = grab {
                ffi::clutter_grab_dismiss(grab);
                gobject_sys::g_object_unref(grab as *mut gobject_sys::GObject);
            }
            ffi::clutter_actor_hide(self.overlay);
            ffi::clutter_actor_destroy_all_children(self.menu);
        }
    }

    pub fn destroy(&self) {
        self.close();
        self.bluetooth.destroy();
        unsafe {
            ffi::clutter_actor_destroy(self.overlay);
            ffi::clutter_actor_destroy(self.button);
        }
    }

    fn refresh(self: &Rc<Self>) {
        if self.state.borrow().open {
            self.build();
        }
    }

    fn build(self: &Rc<Self>) {
        let Some(geo) = ui::primary_monitor_geometry(self.display) else { return };
        unsafe { ffi::clutter_actor_destroy_all_children(self.menu) };
        let mut y = PADDING;

        let night_note = self.color.as_ref().filter(|s| s.boolean("night-light-enabled")).map(|s| {
            schedule_text(
                s.boolean("night-light-schedule-automatic"),
                s.double("night-light-schedule-from"),
                s.double("night-light-schedule-to"),
            )
        });
        let toggles = [
            (Toggle::DarkStyle, "Dark Style", None),
            (Toggle::NightLight, "Night Light", night_note),
            (Toggle::DoNotDisturb, "Do Not Disturb", None),
        ];
        for (toggle, name, note) in toggles {
            let Some(on) = self.is_on(toggle) else { continue };
            self.add_row(name, note.as_deref(), Some(on), y, move |q| q.flip(toggle));
            y += ROW_HEIGHT;
        }

        if let Some(adapter) = self.bluetooth.adapter() {
            y = self.add_line(y);
            self.add_row("Bluetooth", None, Some(adapter.powered), y, |q| q.bluetooth.toggle_power());
            y += ROW_HEIGHT;
            if adapter.powered {
                for device in self.bluetooth.devices() {
                    self.add_device(&device, y);
                    y += ROW_HEIGHT;
                }
            }
            self.add_row("Bluetooth Settings", None, None, y, |q| q.open_settings("bluetooth"));
            y += ROW_HEIGHT;
        }

        y = self.add_line(y);
        self.add_row("All Settings", None, None, y, |q| q.open_settings(""));
        y += ROW_HEIGHT + PADDING;

        let x = {
            let button = self.state.borrow().button_x + BUTTON_WIDTH / 2.0;
            (button - WIDTH / 2.0).min(geo.width as f32 - WIDTH - 4.0).max(4.0)
        };
        unsafe {
            ffi::clutter_actor_set_position(self.overlay, geo.x as f32, geo.y as f32);
            ffi::clutter_actor_set_size(self.overlay, geo.width as f32, geo.height as f32);
            ffi::clutter_actor_set_size(self.menu, WIDTH, y);
            ffi::clutter_actor_set_position(self.menu, x.floor(), self.panel_height + 4.0);
        }
    }

    fn add_device(self: &Rc<Self>, device: &Device, y: f32) {
        let note = match (&device.busy, device.failed, device.connected) {
            (true, _, true) => Some("Disconnecting…"),
            (true, _, false) => Some("Connecting…"),
            (false, true, _) => Some("Did not work"),
            (false, false, true) => Some("Connected"),
            (false, false, false) => None,
        };
        let path = device.path.clone();
        self.add_row(&format!("    {}", device.name), note, None, y, move |q| q.bluetooth.toggle_device(&path));
    }

    /// A row with a name, an optional note under it and an optional switch.
    fn add_row(
        self: &Rc<Self>,
        name: &str,
        note: Option<&str>,
        switch: Option<bool>,
        y: f32,
        chosen: impl Fn(&Rc<Self>) + 'static,
    ) {
        let width = WIDTH - 2.0 * PADDING;
        let row = unsafe {
            let row = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(row, width, ROW_HEIGHT);
            ffi::clutter_actor_set_position(row, PADDING, y);
            ffi::clutter_actor_set_reactive(row, 1);
            ffi::clutter_actor_add_child(self.menu, row);

            let text_width = width - 24.0 - if switch.is_some() { SWITCH_WIDTH + 8.0 } else { 0.0 };
            let line = |font: &CStr, text: &str, color: &ffi::CoglColor| {
                let label = ui::label(font, text, color);
                ffi::clutter_text_set_ellipsize(label as *mut ffi::ClutterText, ffi::PANGO_ELLIPSIZE_END);
                ffi::clutter_actor_set_width(label, text_width);
                ffi::clutter_actor_add_child(row, label);
                label
            };
            let title = line(ui::ROW_FONT, name, &ui::TEXT_COLOR);
            let note = note.map(|n| {
                let indent = &name[..name.len() - name.trim_start().len()];
                line(NOTE_FONT, &format!("{indent}{n}"), &ui::HINT_COLOR)
            });
            let height = ffi::clutter_actor_get_height(title)
                + note.map_or(0.0, |n| ffi::clutter_actor_get_height(n));
            let top = ((ROW_HEIGHT - height) / 2.0).floor();
            ffi::clutter_actor_set_position(title, 12.0, top);
            if let Some(note) = note {
                ffi::clutter_actor_set_position(note, 12.0, top + ffi::clutter_actor_get_height(title));
            }

            if let Some(on) = switch {
                let track = ffi::clutter_actor_new();
                ffi::clutter_actor_set_size(track, SWITCH_WIDTH, SWITCH_HEIGHT);
                ffi::clutter_actor_set_background_color(track, if on { &ui::SELECTED_COLOR } else { &OFF_COLOR });
                ffi::clutter_actor_set_position(track, width - SWITCH_WIDTH - 12.0, (ROW_HEIGHT - SWITCH_HEIGHT) / 2.0);
                ffi::clutter_actor_add_child(row, track);
                let knob = ffi::clutter_actor_new();
                ffi::clutter_actor_set_size(knob, KNOB_SIZE, KNOB_SIZE);
                ffi::clutter_actor_set_background_color(knob, &ui::TEXT_COLOR);
                let gap = (SWITCH_HEIGHT - KNOB_SIZE) / 2.0;
                ffi::clutter_actor_set_position(knob, if on { SWITCH_WIDTH - KNOB_SIZE - gap } else { gap }, gap);
                ffi::clutter_actor_add_child(track, knob);
            }
            row
        };
        crate::windows::hover_highlight(row, ui::TRANSPARENT, ui::PANEL_HOVER_COLOR);
        let weak = Rc::downgrade(self);
        unsafe { gobj(row) }.connect_local("button-press-event", false, move |_| {
            with(&weak, &chosen);
            Some(true.to_value())
        });
    }

    /// A separator at `y`; returns where to go on.
    fn add_line(&self, y: f32) -> f32 {
        unsafe {
            let line = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(line, WIDTH - 2.0 * PADDING, 1.0);
            ffi::clutter_actor_set_position(line, PADDING, y + 4.0);
            ffi::clutter_actor_set_background_color(line, &OFF_COLOR);
            ffi::clutter_actor_add_child(self.menu, line);
        }
        y + 9.0
    }

    /// Open GNOME Settings, on `page` unless it is empty.
    fn open_settings(&self, page: &str) {
        self.close();
        let command = format!("gnome-control-center {page}");
        let launched = gio::AppInfo::create_from_commandline(
            &command,
            None,
            gio::AppInfoCreateFlags::SUPPORTS_STARTUP_NOTIFICATION,
        )
        .and_then(|app| app.launch(&[], Some(&ui::launch_context(self.display))));
        if let Err(e) = launched {
            glib::g_warning!("gnutshell", "could not run {command}: {e}");
        }
    }
}

fn with(weak: &Weak<QuickSettings>, f: impl FnOnce(&Rc<QuickSettings>)) {
    if let Some(quick) = weak.upgrade() {
        f(&quick);
    }
}

#[cfg(test)]
mod tests {
    use super::schedule_text;

    #[test]
    fn describes_the_night_light_schedule() {
        assert_eq!(schedule_text(false, 20.0, 6.0), "20:00 – 06:00");
        assert_eq!(schedule_text(false, 21.5, 6.9999), "21:30 – 07:00");
        assert_eq!(schedule_text(false, 24.0, 0.25), "00:00 – 00:15");
        assert_eq!(schedule_text(true, 20.0, 6.0), "Sunset to sunrise");
    }
}
