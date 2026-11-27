//! Screenshots: gnome-shell's screenshot keybindings and its
//! `org.gnome.Shell.Screenshot` D-Bus API (used by the GNOME portal and
//! gsd-media-keys). With `org.gnome.desktop.lockdown disable-save-to-disk` no
//! files are written: the keys only copy, and the methods that save refuse.
//!
//! `Print` selects an area (Enter takes the whole screen), `Shift+Print`
//! takes the whole screen and `Alt+Print` the focused window. Those save a PNG
//! in `Pictures/Screenshots`, copy it to the clipboard and say so in a
//! notification. While selecting, `P` puts the pointer in the picture, where
//! it was when the selection started; the other keys leave it out. The
//! captured area flashes.
//!
//! The pointer usually has its own hardware plane, which painting the stage
//! doesn't include, so its image is read and drawn into the picture.
//!
//! Anyone who can read the screen can read what is on it, so like gnome-shell
//! the D-Bus methods only answer the owners of `ALLOWED_CALLERS`, and refuse
//! while the screen is locked.

use crate::ffi::{self, ClutterActor, MetaDisplay, MtkRectangle};
use crate::notifications::Notifications;
use crate::ui::{self, gobj};
use gio::prelude::*;
use glib::translate::*;
use std::cell::RefCell;
use std::ffi::{CStr, c_void};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

const BUS_NAME: &str = "org.gnome.Shell.Screenshot";
const OBJECT_PATH: &str = "/org/gnome/Shell/Screenshot";
const INTERFACE: &str = "org.gnome.Shell.Screenshot";

/// Who may call the D-Bus methods, as in gnome-shell. Only names that a
/// running session service holds belong here: owning a name proves nothing by
/// itself, and any program in the session can take one that is free. So
/// `org.gnome.Screenshot` is not listed.
const ALLOWED_CALLERS: [&str; 2] =
    ["org.freedesktop.impl.portal.desktop.gnome", "org.gnome.SettingsDaemon.MediaKeys"];
const LOCKDOWN_SCHEMA: &str = "org.gnome.desktop.lockdown";

const INTERFACE_XML: &str = r#"
<node>
  <interface name="org.gnome.Shell.Screenshot">
    <method name="InteractiveScreenshot">
      <arg type="b" direction="out" name="success"/>
      <arg type="s" direction="out" name="uri"/>
    </method>
    <method name="Screenshot">
      <arg type="b" direction="in" name="include_cursor"/>
      <arg type="b" direction="in" name="flash"/>
      <arg type="s" direction="in" name="filename"/>
      <arg type="b" direction="out" name="success"/>
      <arg type="s" direction="out" name="filename_used"/>
    </method>
    <method name="ScreenshotWindow">
      <arg type="b" direction="in" name="include_frame"/>
      <arg type="b" direction="in" name="include_cursor"/>
      <arg type="b" direction="in" name="flash"/>
      <arg type="s" direction="in" name="filename"/>
      <arg type="b" direction="out" name="success"/>
      <arg type="s" direction="out" name="filename_used"/>
    </method>
    <method name="ScreenshotArea">
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
      <arg type="i" direction="in" name="width"/>
      <arg type="i" direction="in" name="height"/>
      <arg type="b" direction="in" name="flash"/>
      <arg type="s" direction="in" name="filename"/>
      <arg type="b" direction="out" name="success"/>
      <arg type="s" direction="out" name="filename_used"/>
    </method>
    <method name="SelectArea">
      <arg type="i" direction="out" name="x"/>
      <arg type="i" direction="out" name="y"/>
      <arg type="i" direction="out" name="width"/>
      <arg type="i" direction="out" name="height"/>
    </method>
    <method name="PickColor">
      <arg type="a{sv}" direction="out" name="result"/>
    </method>
    <method name="FlashArea">
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
      <arg type="i" direction="in" name="width"/>
      <arg type="i" direction="in" name="height"/>
    </method>
  </interface>
</node>
"#;

/// gnome-shell's keybindings, from its `org.gnome.shell.keybindings` schema.
const KEYBINDINGS_SCHEMA: &str = "org.gnome.shell.keybindings";
const KEYBINDINGS: [&CStr; 3] = [c"show-screenshot-ui", c"screenshot", c"screenshot-window"];

const FLASH_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0xff);
const FLASH_OPACITY: u8 = 0xc0;
const FLASH_MS: u32 = 350;
const AREA_HINT: &str = "Drag to select an area  ·  Enter: whole screen  ·  Escape: cancel";
const PICK_HINT: &str = "Click to pick a color  ·  Escape: cancel";
/// The color preview: a framed square above its hex code.
const SWATCH_WIDTH: f32 = 84.0;
const SWATCH_HEIGHT: f32 = 62.0;
const SWATCH_COLOR_SIZE: f32 = 32.0;
const SWATCH_FRAME: f32 = 2.0;
/// The swatch's distance from the pointer: it must not cover that pixel.
const SWATCH_OFFSET: f32 = 20.0;

const SELECTION_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0x35, 0x84, 0xe4, 0x60);
/// Larger pointer images are not drawn.
const MAX_CURSOR_SIDE: u32 = 1024;
const KEY_P: u32 = 0x70;
const KEY_SHIFT_P: u32 = 0x50;
/// Smaller drags count as a stray click.
const MIN_SELECTION: i32 = 4;

/// What to do with the area being selected.
enum Purpose {
    /// The `Print` key: save, copy and notify.
    Shortcut,
    /// `SelectArea`: answer with the rectangle.
    SelectArea(gio::DBusMethodInvocation),
    /// `InteractiveScreenshot`: save and answer with the file's URI.
    Interactive(gio::DBusMethodInvocation),
    /// `PickColor`: answer with the color of the clicked pixel.
    PickColor(gio::DBusMethodInvocation),
}

struct Selection {
    purpose: Purpose,
    grab: *mut ffi::ClutterGrab,
    /// Where the drag started, in stage coordinates.
    start: Option<(f32, f32)>,
    /// `P` was pressed: the picture gets the pointer.
    pointer: bool,
    /// The pointer as it was before the selection started. During a drag it
    /// is at the area's corner, and it looks different.
    cursor: Option<Cursor>,
}

#[derive(Default)]
struct State {
    selection: Option<Selection>,
    /// The running flash and the timer that removes it.
    flash: Option<(*mut ClutterActor, glib::SourceId)>,
    connection: Option<gio::DBusConnection>,
    registration: Option<gio::RegistrationId>,
    owner: Option<gio::OwnerId>,
}

pub struct Screenshots {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    notifications: Rc<Notifications>,
    overlay: *mut ClutterActor,
    marker: *mut ClutterActor,
    hint: *mut ClutterActor,
    /// Shows the color under the pointer while picking, in `swatch_color`
    /// and as text in `swatch_code`.
    swatch: *mut ClutterActor,
    swatch_color: *mut ClutterActor,
    swatch_code: *mut ClutterActor,
    state: RefCell<State>,
}

/// The pointer's image and where it is, in stage coordinates.
struct Cursor {
    /// Premultiplied BGRA, `width` x `height`.
    image: Picture,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// Screen pixels: tightly packed premultiplied BGRA.
struct Picture {
    width: i32,
    height: i32,
    data: Vec<u8>,
}

impl Screenshots {
    pub fn new(display: *mut MetaDisplay, stage: *mut ClutterActor, notifications: Rc<Notifications>) -> Rc<Self> {
        let (overlay, marker, hint, swatch, swatch_color, swatch_code) = unsafe {
            let overlay = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(overlay, &ui::DIM_COLOR);
            ffi::clutter_actor_set_reactive(overlay, 1);
            ffi::clutter_actor_hide(overlay);
            let marker = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(marker, &SELECTION_COLOR);
            ffi::clutter_actor_add_child(overlay, marker);
            let hint = ui::label(ui::ROW_FONT, AREA_HINT, &ui::TEXT_COLOR);
            ffi::clutter_actor_add_child(overlay, hint);
            let swatch = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(swatch, SWATCH_WIDTH, SWATCH_HEIGHT);
            ffi::clutter_actor_set_background_color(swatch, &ui::BOX_COLOR);
            ffi::clutter_actor_hide(swatch);
            ffi::clutter_actor_add_child(overlay, swatch);
            // A light frame sets the color off from the same color behind it.
            let frame = ffi::clutter_actor_new();
            let framed = SWATCH_COLOR_SIZE + 2.0 * SWATCH_FRAME;
            ffi::clutter_actor_set_size(frame, framed, framed);
            ffi::clutter_actor_set_position(frame, ((SWATCH_WIDTH - framed) / 2.0).floor(), 4.0);
            ffi::clutter_actor_set_background_color(frame, &ui::TEXT_COLOR);
            ffi::clutter_actor_add_child(swatch, frame);
            let swatch_color = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(swatch_color, SWATCH_COLOR_SIZE, SWATCH_COLOR_SIZE);
            ffi::clutter_actor_set_position(swatch_color, SWATCH_FRAME, SWATCH_FRAME);
            ffi::clutter_actor_add_child(frame, swatch_color);
            let swatch_code = ui::label(ui::BODY_FONT, "#000000", &ui::TEXT_COLOR);
            ffi::clutter_actor_add_child(swatch, swatch_code);
            ffi::clutter_actor_set_position(swatch_code, 8.0, framed + 6.0);
            ffi::clutter_actor_add_child(stage, overlay);
            (overlay, marker, hint, swatch, swatch_color, swatch_code)
        };
        let this =
            Rc::new(Self {
            display,
            stage,
            notifications,
            overlay,
            marker,
            hint,
            swatch,
            swatch_color,
            swatch_code,
            state: RefCell::default(),
        });
        this.connect_overlay();
        this.install_keybindings();
        this.own_name();
        this
    }

    /// Give up any selection, e.g. because the screen locked.
    pub fn cancel(&self) {
        if let Some((purpose, _)) = self.end_selection() {
            match purpose {
                Purpose::Shortcut => {}
                Purpose::SelectArea(i) | Purpose::Interactive(i) | Purpose::PickColor(i) => {
                    i.return_dbus_error("org.freedesktop.DBus.Error.Failed", "Cancelled");
                }
            }
        }
    }

    pub fn destroy(&self) {
        self.cancel();
        self.end_flash();
        for name in KEYBINDINGS {
            unsafe { ffi::meta_display_remove_keybinding(self.display, name.as_ptr()) };
        }
        let (connection, registration, owner) = {
            let mut state = self.state.borrow_mut();
            (state.connection.take(), state.registration.take(), state.owner.take())
        };
        if let (Some(connection), Some(id)) = (connection, registration) {
            let _ = connection.unregister_object(id);
        }
        if let Some(owner) = owner {
            gio::bus_unown_name(owner);
        }
        unsafe { ffi::clutter_actor_destroy(self.overlay) };
    }

    // ------------------------------------------------------------ keybindings

    fn install_keybindings(self: &Rc<Self>) {
        let has_schema = gio::SettingsSchemaSource::default()
            .and_then(|s| s.lookup(KEYBINDINGS_SCHEMA, true))
            .is_some_and(|s| KEYBINDINGS.iter().all(|k| s.has_key(k.to_str().unwrap_or_default())));
        if !has_schema {
            glib::g_warning!("gnutshell", "no {KEYBINDINGS_SCHEMA} schema: screenshot keys are not bound");
            return;
        }
        let settings = gio::Settings::new(KEYBINDINGS_SCHEMA);
        for name in KEYBINDINGS {
            let data = Weak::into_raw(Rc::downgrade(self)) as glib_sys::gpointer;
            let action = unsafe {
                ffi::meta_display_add_keybinding(
                    self.display,
                    name.as_ptr(),
                    settings.as_ptr(),
                    0,
                    Some(on_binding),
                    data,
                    Some(drop_weak),
                )
            };
            if action == 0 {
                unsafe { drop_weak(data) };
                glib::g_warning!("gnutshell", "could not add keybinding {name:?}");
            }
        }
    }

    fn on_keybinding(self: &Rc<Self>, name: &str) {
        if crate::lockscreen::is_locked() {
            return;
        }
        match name {
            "show-screenshot-ui" => self.start_selection(Purpose::Shortcut),
            "screenshot" => self.take_for_user(self.screen_rect(), None),
            "screenshot-window" => {
                if let Some(rect) = self.focus_window_rect() {
                    self.take_for_user(rect, None);
                }
            }
            _ => {}
        }
    }

    // -------------------------------------------------------------- selection

    fn connect_overlay(self: &Rc<Self>) {
        let overlay = unsafe { gobj(self.overlay) };
        let weak = Rc::downgrade(self);
        overlay.connect_local("key-press-event", false, move |args| {
            let key = ui::key_symbol(ui::event_arg(args));
            with(&weak, |s| match key {
                ui::KEY_ESCAPE => s.cancel(),
                ui::KEY_RETURN | ui::KEY_KP_ENTER if !s.picking() => s.finish_selection(s.screen_rect()),
                KEY_P | KEY_SHIFT_P => s.toggle_pointer(),
                _ => {}
            });
            Some(true.to_value())
        });
        let weak = Rc::downgrade(self);
        overlay.connect_local("button-press-event", false, move |args| {
            let at = ui::event_coords(ui::event_arg(args));
            with(&weak, |s| {
                if let Some(selection) = s.state.borrow_mut().selection.as_mut() {
                    selection.start = at;
                }
            });
            Some(true.to_value())
        });
        for signal in ["motion-event", "button-release-event"] {
            let weak = Rc::downgrade(self);
            let released = signal == "button-release-event";
            overlay.connect_local(signal, false, move |args| {
                let at = ui::event_coords(ui::event_arg(args));
                with(&weak, |s| s.drag(at, released));
                Some(true.to_value())
            });
        }
    }

    fn picking(&self) -> bool {
        matches!(self.state.borrow().selection, Some(Selection { purpose: Purpose::PickColor(_), .. }))
    }

    fn toggle_pointer(&self) {
        let pointer = {
            let mut state = self.state.borrow_mut();
            // Only screenshots can have the pointer in them.
            match state.selection.as_mut() {
                Some(s) if matches!(s.purpose, Purpose::Shortcut | Purpose::Interactive(_)) => {
                    s.pointer = !s.pointer;
                    s.pointer
                }
                _ => return,
            }
        };
        self.set_hint(&area_hint(pointer));
    }

    /// Show `text`, centered near the top of the primary monitor.
    fn set_hint(&self, text: &str) {
        let screen = self.screen_rect();
        unsafe {
            ui::set_text(self.hint, text);
            if let Some(geo) = ui::primary_monitor_geometry(self.display) {
                // Let the actor take the new text's width.
                ffi::clutter_actor_set_width(self.hint, -1.0);
                let width = ffi::clutter_actor_get_width(self.hint);
                ffi::clutter_actor_set_position(
                    self.hint,
                    ((geo.x - screen.x) as f32 + (geo.width as f32 - width) / 2.0).floor(),
                    (geo.y - screen.y) as f32 + 64.0,
                );
            }
        }
    }

    fn start_selection(self: &Rc<Self>, purpose: Purpose) {
        if self.state.borrow().selection.is_some() {
            if let Purpose::SelectArea(i) | Purpose::Interactive(i) | Purpose::PickColor(i) = purpose {
                i.return_dbus_error("org.freedesktop.DBus.Error.Failed", "A selection is already in progress");
            }
            return;
        }
        let screen = self.screen_rect();
        // Before the grab changes the pointer's shape.
        let cursor = self.read_cursor();
        let picking = matches!(purpose, Purpose::PickColor(_));
        match &purpose {
            Purpose::PickColor(_) => self.set_hint(PICK_HINT),
            Purpose::SelectArea(_) => self.set_hint(AREA_HINT),
            Purpose::Shortcut | Purpose::Interactive(_) => self.set_hint(&area_hint(false)),
        }
        // A tint would change the colors being picked.
        let dim = if picking { &ui::TRANSPARENT } else { &ui::DIM_COLOR };
        self.end_flash();
        let grab = unsafe {
            ffi::clutter_actor_set_background_color(self.overlay, dim);
            ffi::clutter_actor_hide(self.swatch);
            ffi::clutter_actor_set_position(self.overlay, screen.x as f32, screen.y as f32);
            ffi::clutter_actor_set_size(self.overlay, screen.width as f32, screen.height as f32);
            ffi::clutter_actor_hide(self.marker);
            // The hint needs its own backdrop where nothing is dimmed.
            let backdrop = if picking { &ui::BOX_COLOR } else { &ui::TRANSPARENT };
            ffi::clutter_actor_set_background_color(self.hint, backdrop);
            ffi::clutter_actor_show(self.overlay);
            ffi::clutter_actor_set_child_above_sibling(self.stage, self.overlay, std::ptr::null_mut());
            let grab = ffi::clutter_stage_grab(self.stage as *mut ffi::ClutterStage, self.overlay);
            ffi::clutter_actor_grab_key_focus(self.overlay);
            grab
        };
        self.state.borrow_mut().selection = Some(Selection { purpose, grab, start: None, pointer: false, cursor });
    }

    /// The pointer moved (or the button went up) while picking a color.
    fn pick(self: &Rc<Self>, at: (f32, f32), released: bool) {
        let pixel = MtkRectangle { x: at.0.floor() as i32, y: at.1.floor() as i32, width: 1, height: 1 };
        if released {
            let Some((Purpose::PickColor(invocation), _)) = self.end_selection() else { return };
            match self.capture(pixel, None).ok().and_then(|p| color_of(&p)) {
                Some(color) => {
                    let result = glib::VariantDict::new(None);
                    result.insert_value("color", &color.to_variant());
                    let reply = glib::Variant::tuple_from_iter([result.end()]);
                    invocation.return_value(Some(&reply));
                }
                None => invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "Could not read the pixel"),
            }
            return;
        }
        let Some((r, g, b)) = self.capture(pixel, None).ok().and_then(|p| color_of(&p)) else { return };
        let byte = |c: f64| (c * 255.0).round() as u8;
        let screen = self.screen_rect();
        // Below and right of the pointer, or the other side at an edge.
        let place = |at: f32, size: f32, end: f32| {
            if at + SWATCH_OFFSET + size > end { at - SWATCH_OFFSET - size } else { at + SWATCH_OFFSET }
        };
        unsafe {
            let color = ffi::CoglColor::rgba(byte(r), byte(g), byte(b), 0xff);
            ffi::clutter_actor_set_background_color(self.swatch_color, &color);
            ui::set_text(self.swatch_code, &format!("#{:02x}{:02x}{:02x}", byte(r), byte(g), byte(b)));
            ffi::clutter_actor_set_position(
                self.swatch,
                place(at.0, SWATCH_WIDTH, (screen.x + screen.width) as f32) - screen.x as f32,
                place(at.1, SWATCH_HEIGHT, (screen.y + screen.height) as f32) - screen.y as f32,
            );
            ffi::clutter_actor_show(self.swatch);
        }
    }

    fn drag(self: &Rc<Self>, at: Option<(f32, f32)>, released: bool) {
        if self.picking() {
            if let Some(at) = at {
                // A release without a press is left over from elsewhere.
                let pressed = self.state.borrow().selection.as_ref().is_some_and(|s| s.start.is_some());
                self.pick(at, released && pressed);
            }
            return;
        }
        let start = self.state.borrow().selection.as_ref().and_then(|s| s.start);
        let (Some(start), Some(at)) = (start, at) else { return };
        let rect = drag_rect(start, at);
        let big_enough = rect.width >= MIN_SELECTION && rect.height >= MIN_SELECTION;
        if released {
            if big_enough {
                self.finish_selection(rect);
            } else {
                // A click without a drag: keep selecting.
                if let Some(selection) = self.state.borrow_mut().selection.as_mut() {
                    selection.start = None;
                }
                unsafe { ffi::clutter_actor_hide(self.marker) };
            }
            return;
        }
        let screen = self.screen_rect();
        unsafe {
            ffi::clutter_actor_set_position(self.marker, (rect.x - screen.x) as f32, (rect.y - screen.y) as f32);
            ffi::clutter_actor_set_size(self.marker, rect.width as f32, rect.height as f32);
            ffi::clutter_actor_show(self.marker);
        }
    }

    /// Take the overlay away. Returns what the selection was for, and whether
    /// the pointer that belongs in the picture.
    fn end_selection(&self) -> Option<(Purpose, Option<Cursor>)> {
        let selection = self.state.borrow_mut().selection.take()?;
        unsafe {
            ffi::clutter_grab_dismiss(selection.grab);
            gobject_sys::g_object_unref(selection.grab as *mut gobject_sys::GObject);
            ffi::clutter_actor_hide(self.overlay);
        }
        Some((selection.purpose, selection.cursor.filter(|_| selection.pointer)))
    }

    fn finish_selection(self: &Rc<Self>, rect: MtkRectangle) {
        // The overlay is hidden first, so it isn't in the picture.
        let Some((purpose, pointer)) = self.end_selection() else { return };
        match purpose {
            Purpose::Shortcut => self.take_for_user(rect, pointer.as_ref()),
            // Picking ends in `pick`.
            Purpose::PickColor(i) => i.return_dbus_error("org.freedesktop.DBus.Error.Failed", "Cancelled"),
            Purpose::SelectArea(invocation) => {
                invocation.return_value(Some(&(rect.x, rect.y, rect.width, rect.height).to_variant()));
            }
            Purpose::Interactive(invocation) => match self.capture(rect, pointer.as_ref()) {
                Ok(picture) => {
                    glib::spawn_future_local(async move {
                        match save(picture, None).await {
                            Ok((path, _)) => {
                                let uri = gio::File::for_path(path).uri().to_string();
                                invocation.return_value(Some(&(true, uri).to_variant()));
                            }
                            Err(e) => invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", &e),
                        }
                    });
                }
                Err(e) => invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", &e),
            },
        }
    }

    // ---------------------------------------------------------------- capture

    /// The box around all monitors, in stage coordinates.
    fn screen_rect(&self) -> MtkRectangle {
        let mut bounds: Option<MtkRectangle> = None;
        unsafe {
            for i in 0..ffi::meta_display_get_n_monitors(self.display) {
                let mut r = MtkRectangle::default();
                ffi::meta_display_get_monitor_geometry(self.display, i, &mut r);
                bounds = Some(bounds.map_or(r, |b| union(b, r)));
            }
        }
        bounds.unwrap_or_default()
    }

    /// The visible window: with client-side decorations, there is no separate
    /// frame to leave out.
    fn focus_window_rect(&self) -> Option<MtkRectangle> {
        unsafe {
            let window = ffi::meta_display_get_focus_window(self.display);
            if window.is_null() {
                return None;
            }
            let mut rect = MtkRectangle::default();
            ffi::meta_window_get_frame_rect(window, &mut rect);
            Some(rect)
        }
    }

    /// The largest scale among the monitors `rect` touches.
    fn scale_for(&self, rect: MtkRectangle) -> f32 {
        let mut scale: f32 = 1.0;
        unsafe {
            for i in 0..ffi::meta_display_get_n_monitors(self.display) {
                let mut r = MtkRectangle::default();
                ffi::meta_display_get_monitor_geometry(self.display, i, &mut r);
                if intersect(r, rect).is_some() {
                    scale = scale.max(ffi::meta_display_get_monitor_scale(self.display, i));
                }
            }
        }
        scale
    }

    /// The pointer now, unless it is hidden.
    fn read_cursor(&self) -> Option<Cursor> {
        unsafe {
            let backend = ffi::meta_context_get_backend(ffi::meta_display_get_context(self.display));
            let tracker = ffi::meta_backend_get_cursor_tracker(backend);
            if ffi::meta_cursor_tracker_get_pointer_visible(tracker) == 0 {
                return None;
            }
            let texture = ffi::meta_cursor_tracker_get_sprite(tracker);
            if texture.is_null() {
                return None;
            }
            let (width, height) = (ffi::cogl_texture_get_width(texture), ffi::cogl_texture_get_height(texture));
            if !(1..=MAX_CURSOR_SIDE).contains(&width) || !(1..=MAX_CURSOR_SIDE).contains(&height) {
                return None;
            }
            let mut data = vec![0u8; width as usize * height as usize * 4];
            let format = ffi::COGL_PIXEL_FORMAT_BGRA_8888_PRE;
            if ffi::cogl_texture_get_data(texture, format, width * 4, data.as_mut_ptr()) == 0 {
                glib::g_warning!("gnutshell", "screenshot: could not read the pointer image");
                return None;
            }
            let mut at = ffi::GraphenePoint::default();
            ffi::meta_cursor_tracker_get_pointer(tracker, &mut at, std::ptr::null_mut());
            let (mut hot_x, mut hot_y) = (0, 0);
            ffi::meta_cursor_tracker_get_hot(tracker, &mut hot_x, &mut hot_y);
            // Image pixels to stage units, as in gnome-shell.
            let scale = ffi::meta_cursor_tracker_get_scale(tracker);
            Some(Cursor {
                image: Picture { width: width as i32, height: height as i32, data },
                x: at.x - hot_x as f32 * scale,
                y: at.y - hot_y as f32 * scale,
                width: width as f32 * scale,
                height: height as f32 * scale,
            })
        }
    }

    /// Paint the part of `rect` that is on screen, and `cursor` over it.
    fn capture(&self, rect: MtkRectangle, cursor: Option<&Cursor>) -> Result<Picture, String> {
        let rect = intersect(rect, self.screen_rect()).ok_or("The area is not on the screen")?;
        let scale = self.scale_for(rect);
        let width = (rect.width as f32 * scale).round() as i32;
        let height = (rect.height as f32 * scale).round() as i32;
        let stride = width * 4;
        // One spare row in case mutter rounds the other way.
        let mut data = vec![0u8; stride as usize * (height as usize + 1)];
        unsafe {
            let mut err = std::ptr::null_mut();
            let ok = ffi::clutter_stage_paint_to_buffer(
                self.stage as *mut ffi::ClutterStage,
                &rect,
                scale,
                data.as_mut_ptr(),
                stride,
                ffi::COGL_PIXEL_FORMAT_BGRA_8888_PRE,
                std::ptr::null_mut(),
                // A software pointer would be painted; ours goes on after.
                ffi::CLUTTER_PAINT_FLAG_CLEAR | ffi::CLUTTER_PAINT_FLAG_NO_CURSORS,
                &mut err,
            );
            if ok == 0 {
                let err: glib::Error = from_glib_full(err);
                return Err(err.to_string());
            }
        }
        data.truncate(stride as usize * height as usize);
        let mut picture = Picture { width, height, data };
        if let Some(cursor) = cursor {
            let x = (cursor.x - rect.x as f32) * scale;
            let y = (cursor.y - rect.y as f32) * scale;
            draw_over(&mut picture, &cursor.image, x, y, cursor.width * scale, cursor.height * scale)?;
        }
        Ok(picture)
    }

    /// Flash `rect` white, unless animations are off.
    fn flash(self: &Rc<Self>, rect: MtkRectangle) {
        self.end_flash();
        let Some(rect) = intersect(rect, self.screen_rect()) else { return };
        if !animations_enabled() {
            return;
        }
        let actor = unsafe {
            let actor = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(actor, &FLASH_COLOR);
            ffi::clutter_actor_set_position(actor, rect.x as f32, rect.y as f32);
            ffi::clutter_actor_set_size(actor, rect.width as f32, rect.height as f32);
            ffi::clutter_actor_set_opacity(actor, FLASH_OPACITY);
            ffi::clutter_actor_add_child(self.stage, actor);
            ffi::clutter_actor_save_easing_state(actor);
            ffi::clutter_actor_set_easing_mode(actor, ffi::CLUTTER_EASE_OUT_QUAD);
            ffi::clutter_actor_set_easing_duration(actor, FLASH_MS);
            ffi::clutter_actor_set_opacity(actor, 0);
            ffi::clutter_actor_restore_easing_state(actor);
            actor
        };
        let weak = Rc::downgrade(self);
        let timer = glib::timeout_add_local_once(std::time::Duration::from_millis(FLASH_MS.into()), move || {
            with(&weak, |s| {
                // The timer is the one running: don't remove it again.
                let flash = s.state.borrow_mut().flash.take();
                if let Some((actor, _)) = flash {
                    unsafe { ffi::clutter_actor_destroy(actor) };
                }
            });
        });
        self.state.borrow_mut().flash = Some((actor, timer));
    }

    /// Remove a flash at once, e.g. so that it isn't in the next picture.
    fn end_flash(&self) {
        let flash = self.state.borrow_mut().flash.take();
        if let Some((actor, timer)) = flash {
            timer.remove();
            unsafe { ffi::clutter_actor_destroy(actor) };
        }
    }

    /// A screenshot the user asked for with a key.
    /// For the screenshot keys while saving to disk is disabled.
    fn copy_without_saving(&self, picture: Result<Picture, String>) {
        let copied = picture
            .and_then(encode_png)
            .map(|png| ui::set_clipboard_data(self.display, c"image/png", &png));
        let (title, text) = match copied {
            Ok(true) => ("Screenshot copied", "Saving to disk is disabled"),
            _ => ("Screenshot failed", "Saving to disk is disabled, and it could not be copied"),
        };
        self.notifications.notify_shell(title, text, "camera-photo-symbolic");
    }

    fn take_for_user(self: &Rc<Self>, rect: MtkRectangle, cursor: Option<&Cursor>) {
        self.end_flash();
        let picture = self.capture(rect, cursor);
        if picture.is_ok() {
            self.flash(rect);
        }
        if saving_disabled() {
            self.copy_without_saving(picture);
            return;
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let saved = match picture {
                Ok(picture) => save(picture, None).await,
                Err(e) => Err(e),
            };
            let Some(this) = weak.upgrade() else { return };
            match saved {
                Ok((path, png)) => {
                    let copied = ui::set_clipboard_data(this.display, c"image/png", &png);
                    let title = if copied { "Screenshot saved and copied" } else { "Screenshot saved" };
                    let path = path.to_string_lossy();
                    this.notifications.notify_shell(title, &path, &path);
                }
                Err(e) => {
                    glib::g_warning!("gnutshell", "screenshot failed: {e}");
                    this.notifications.notify_shell("Screenshot failed", &e, "dialog-error");
                }
            }
        });
    }

    // ------------------------------------------------------------------ D-Bus

    fn own_name(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let owner = gio::bus_own_name(
            gio::BusType::Session,
            BUS_NAME,
            gio::BusNameOwnerFlags::NONE,
            move |connection, _| with(&weak, |s| s.export(connection.clone())),
            |_, name| glib::g_debug!("gnutshell", "acquired {name}"),
            |_, name| glib::g_warning!("gnutshell", "could not own {name} (is another shell running?)"),
        );
        self.state.borrow_mut().owner = Some(owner);
    }

    fn export(self: &Rc<Self>, connection: gio::DBusConnection) {
        let info = gio::DBusNodeInfo::for_xml(INTERFACE_XML)
            .ok()
            .and_then(|node| node.lookup_interface(INTERFACE))
            .expect("valid org.gnome.Shell.Screenshot interface XML");
        let weak = Rc::downgrade(self);
        let registration = connection
            .register_object(OBJECT_PATH, &info)
            .method_call(move |connection, sender, _, _, method, params, invocation| match weak.upgrade() {
                Some(this) => {
                    this.on_method_call(&connection, sender.unwrap_or_default(), method, &params, invocation)
                }
                None => invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "shutting down"),
            })
            .build();
        let mut state = self.state.borrow_mut();
        match registration {
            Ok(id) => state.registration = Some(id),
            Err(e) => glib::g_warning!("gnutshell", "could not export {OBJECT_PATH}: {e}"),
        }
        state.connection = Some(connection);
    }

    fn on_method_call(
        self: &Rc<Self>,
        connection: &gio::DBusConnection,
        sender: &str,
        method: &str,
        params: &glib::Variant,
        invocation: gio::DBusMethodInvocation,
    ) {
        if !is_allowed(connection, sender) {
            glib::g_message!("gnutshell", "refused {method} from {sender}");
            invocation
                .return_dbus_error("org.freedesktop.DBus.Error.AccessDenied", &format!("{method} is not allowed"));
            return;
        }
        if crate::lockscreen::is_locked() {
            invocation.return_dbus_error("org.freedesktop.DBus.Error.AccessDenied", "The screen is locked");
            return;
        }
        let writes_a_file =
            matches!(method, "Screenshot" | "ScreenshotWindow" | "ScreenshotArea" | "InteractiveScreenshot");
        if writes_a_file && saving_disabled() {
            invocation.return_dbus_error("org.freedesktop.DBus.Error.AccessDenied", "Saving to disk is disabled");
            return;
        }
        match method {
            "Screenshot" => {
                let (pointer, flash, name) = params.get::<(bool, bool, String)>().unwrap_or_default();
                let picture = self.capture_for_caller(Some(self.screen_rect()), pointer, flash);
                self.reply_with_file(picture, name, invocation);
            }
            "ScreenshotWindow" => {
                let (_, pointer, flash, name) = params.get::<(bool, bool, bool, String)>().unwrap_or_default();
                let picture = self.capture_for_caller(self.focus_window_rect(), pointer, flash);
                self.reply_with_file(picture, name, invocation);
            }
            "ScreenshotArea" => {
                let (x, y, width, height, flash, name) =
                    params.get::<(i32, i32, i32, i32, bool, String)>().unwrap_or_default();
                let picture = self.capture_for_caller(Some(MtkRectangle { x, y, width, height }), false, flash);
                self.reply_with_file(picture, name, invocation);
            }
            "SelectArea" => self.start_selection(Purpose::SelectArea(invocation)),
            "InteractiveScreenshot" => self.start_selection(Purpose::Interactive(invocation)),
            "PickColor" => self.start_selection(Purpose::PickColor(invocation)),
            "FlashArea" => {
                let (x, y, width, height) = params.get::<(i32, i32, i32, i32)>().unwrap_or_default();
                self.flash(MtkRectangle { x, y, width, height });
                invocation.return_value(None);
            }
            _ => invocation.return_dbus_error(
                "org.freedesktop.DBus.Error.UnknownMethod",
                &format!("{method} is not implemented by gnutshell"),
            ),
        }
    }

    /// A picture for a D-Bus method; no `rect` means no focused window.
    fn capture_for_caller(
        self: &Rc<Self>,
        rect: Option<MtkRectangle>,
        pointer: bool,
        flash: bool,
    ) -> Result<Picture, String> {
        let rect = rect.ok_or("No window has focus")?;
        self.end_flash();
        let cursor = if pointer { self.read_cursor() } else { None };
        let picture = self.capture(rect, cursor.as_ref())?;
        if flash {
            self.flash(rect);
        }
        Ok(picture)
    }

    fn reply_with_file(
        &self,
        picture: Result<Picture, String>,
        name: String,
        invocation: gio::DBusMethodInvocation,
    ) {
        glib::spawn_future_local(async move {
            let saved = match picture {
                Ok(picture) => save(picture, Some(name)).await,
                Err(e) => Err(e),
            };
            match saved {
                Ok((path, _)) => {
                    let path = path.to_string_lossy().into_owned();
                    invocation.return_value(Some(&(true, path).to_variant()));
                }
                Err(e) => invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", &e),
            }
        });
    }
}

/// Does `sender` own one of `ALLOWED_CALLERS` right now?
fn is_allowed(connection: &gio::DBusConnection, sender: &str) -> bool {
    ALLOWED_CALLERS.iter().any(|name| {
        connection
            .call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "GetNameOwner",
                Some(&(*name,).to_variant()),
                Some(glib::VariantTy::new("(s)").unwrap()),
                gio::DBusCallFlags::NONE,
                1000,
                gio::Cancellable::NONE,
            )
            .ok()
            .and_then(|v| v.get::<(String,)>())
            .is_some_and(|(owner,)| owner == sender)
    })
}

/// Whether the administrator or the user forbids saving files.
fn saving_disabled() -> bool {
    gio::SettingsSchemaSource::default()
        .and_then(|s| s.lookup(LOCKDOWN_SCHEMA, true))
        .is_some_and(|_| gio::Settings::new(LOCKDOWN_SCHEMA).boolean("disable-save-to-disk"))
}

/// Encode `picture` and write it on a worker thread, so the compositor
/// doesn't stall. `name` is the D-Bus `filename` argument; `None` or empty
/// means a new file in the screenshot folder. Returns the path and the PNG.
async fn save(picture: Picture, name: Option<String>) -> Result<(PathBuf, Vec<u8>), String> {
    let folder = glib::user_special_dir(glib::UserDirectory::Pictures)
        .unwrap_or_else(|| glib::home_dir().join("Pictures"))
        .join("Screenshots");
    let stamp = glib::DateTime::now_local()
        .and_then(|t| t.format("%Y-%m-%d %H-%M-%S"))
        .map(|s| s.to_string())
        .unwrap_or_default();
    gio::spawn_blocking(move || {
        let png = encode_png(picture)?;
        let path = store(&folder, &format!("Screenshot From {stamp}"), name.as_deref(), &png)?;
        Ok((path, png))
    })
    .await
    .map_err(|_| "The PNG writer crashed".to_owned())?
}

/// Where a picture goes, from the D-Bus `filename` argument.
#[derive(Debug, PartialEq, Eq)]
enum Destination<'a> {
    /// A path the caller chose; as in gnome-shell, a file there is replaced.
    Path(&'a Path),
    /// A new file in the screenshot folder, named after this.
    New(&'a str),
}

/// An absolute path is taken as it is. Anything else must be a plain file
/// name, so that it cannot lead out of the screenshot folder.
fn destination<'a>(default_stem: &'a str, name: Option<&'a str>) -> Result<Destination<'a>, String> {
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return Ok(Destination::New(default_stem));
    };
    if Path::new(name).is_absolute() {
        return Ok(Destination::Path(Path::new(name)));
    }
    let stem = name.strip_suffix(".png").unwrap_or(name);
    if matches!(stem, "" | "." | "..") || stem.contains(['/', '\0']) {
        return Err("The file name must be a plain name or an absolute path".to_owned());
    }
    Ok(Destination::New(stem))
}

/// Write `png` and return where it went.
fn store(folder: &Path, default_stem: &str, name: Option<&str>, png: &[u8]) -> Result<PathBuf, String> {
    match destination(default_stem, name)? {
        Destination::Path(path) => {
            // Written beside the target and renamed over it.
            gio::File::for_path(path)
                .replace_contents(png, None, false, gio::FileCreateFlags::NONE, gio::Cancellable::NONE)
                .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
            Ok(path.to_owned())
        }
        Destination::New(stem) => write_new(folder, stem, png),
    }
}

/// Create the file at `path`; fails if something is there, a link included.
fn create(path: &Path, png: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::OpenOptions::new().write(true).create_new(true).open(path)?.write_all(png)
}

/// Write `<stem>.png` in `folder`, or `<stem> (2).png` and so on: never over
/// an existing file.
fn write_new(folder: &Path, stem: &str, png: &[u8]) -> Result<PathBuf, String> {
    let fail = |path: &Path, e: std::io::Error| format!("Could not write {}: {e}", path.display());
    std::fs::create_dir_all(folder).map_err(|e| fail(folder, e))?;
    for n in 1..100 {
        let name = if n == 1 { format!("{stem}.png") } else { format!("{stem} ({n}).png") };
        let path = folder.join(name);
        match create(&path, png) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(fail(&path, e)),
        }
    }
    Err(format!("Too many pictures named {stem} in {}", folder.display()))
}

fn encode_png(mut picture: Picture) -> Result<Vec<u8>, String> {
    unsafe extern "C" fn append(closure: *mut c_void, data: *const u8, length: u32) -> i32 {
        unsafe {
            let out = &mut *(closure as *mut Vec<u8>);
            out.extend_from_slice(std::slice::from_raw_parts(data, length as usize));
        }
        ffi::CAIRO_STATUS_SUCCESS
    }
    let Picture { width, height, ref mut data } = picture;
    if width <= 0 || height <= 0 || data.len() < width as usize * height as usize * 4 {
        return Err("The picture is empty".into());
    }
    let mut out: Vec<u8> = Vec::new();
    unsafe {
        // The screen is opaque: RGB24 ignores the alpha byte.
        let surface = ffi::cairo_image_surface_create_for_data(
            data.as_mut_ptr(),
            ffi::CAIRO_FORMAT_RGB24,
            width,
            height,
            width * 4,
        );
        let status = ffi::cairo_surface_write_to_png_stream(surface, Some(append), &mut out as *mut _ as *mut c_void);
        ffi::cairo_surface_destroy(surface);
        if status != ffi::CAIRO_STATUS_SUCCESS {
            return Err(format!("PNG encoding failed (cairo status {status})"));
        }
    }
    Ok(out)
}

/// Draw `image` (premultiplied BGRA) into `target` with its top left corner
/// at (`x`, `y`), scaled to `width` x `height` pixels.
fn draw_over(
    target: &mut Picture,
    image: &Picture,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Result<(), String> {
    let fits = |p: &Picture| p.width > 0 && p.height > 0 && p.data.len() >= (p.width * p.height * 4) as usize;
    if !fits(target) || !fits(image) {
        return Err("The picture is empty".into());
    }
    let mut pixels = image.data.clone();
    unsafe {
        let surface = |data: *mut u8, format, p: &Picture| {
            ffi::cairo_image_surface_create_for_data(data, format, p.width, p.height, p.width * 4)
        };
        let destination = surface(target.data.as_mut_ptr(), ffi::CAIRO_FORMAT_RGB24, target);
        let source = surface(pixels.as_mut_ptr(), ffi::CAIRO_FORMAT_ARGB32, image);
        let cr = ffi::cairo_create(destination);
        ffi::cairo_translate(cr, x.into(), y.into());
        ffi::cairo_scale(cr, (width / image.width as f32).into(), (height / image.height as f32).into());
        ffi::cairo_set_source_surface(cr, source, 0.0, 0.0);
        ffi::cairo_paint(cr);
        ffi::cairo_destroy(cr);
        ffi::cairo_surface_flush(destination);
        ffi::cairo_surface_destroy(source);
        ffi::cairo_surface_destroy(destination);
    }
    Ok(())
}

fn area_hint(pointer: bool) -> String {
    format!("{AREA_HINT}  ·  P: pointer {}", if pointer { "on" } else { "off" })
}

/// The first pixel's red, green and blue, from 0 to 1.
fn color_of(picture: &Picture) -> Option<(f64, f64, f64)> {
    let [b, g, r, _] = *picture.data.first_chunk::<4>()?;
    let part = |c: u8| f64::from(c) / 255.0;
    Some((part(r), part(g), part(b)))
}

fn animations_enabled() -> bool {
    let schema = "org.gnome.desktop.interface";
    gio::SettingsSchemaSource::default().and_then(|s| s.lookup(schema, true)).is_none()
        || gio::Settings::new(schema).boolean("enable-animations")
}

/// The rectangle between two corners of a drag, in whole pixels.
fn drag_rect(a: (f32, f32), b: (f32, f32)) -> MtkRectangle {
    let (x0, y0) = (a.0.min(b.0).floor() as i32, a.1.min(b.1).floor() as i32);
    let (x1, y1) = (a.0.max(b.0).ceil() as i32, a.1.max(b.1).ceil() as i32);
    MtkRectangle { x: x0, y: y0, width: x1 - x0, height: y1 - y0 }
}

fn union(a: MtkRectangle, b: MtkRectangle) -> MtkRectangle {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    MtkRectangle { x, y, width: right - x, height: bottom - y }
}

/// The overlap of `a` and `b`, if any. D-Bus callers can pass any numbers.
fn intersect(a: MtkRectangle, b: MtkRectangle) -> Option<MtkRectangle> {
    let (x, y) = (a.x.max(b.x), a.y.max(b.y));
    let right = a.x.saturating_add(a.width).min(b.x.saturating_add(b.width));
    let bottom = a.y.saturating_add(a.height).min(b.y.saturating_add(b.height));
    (right > x && bottom > y).then(|| MtkRectangle { x, y, width: right - x, height: bottom - y })
}

unsafe extern "C" fn on_binding(
    _display: *mut MetaDisplay,
    _window: *mut ffi::MetaWindow,
    _event: *const ffi::ClutterEvent,
    binding: *mut ffi::MetaKeyBinding,
    data: glib_sys::gpointer,
) {
    // Borrow the Weak that the binding owns, without freeing it.
    let weak = std::mem::ManuallyDrop::new(unsafe { Weak::from_raw(data as *const Screenshots) });
    let name = unsafe { ffi::meta_key_binding_get_name(binding) };
    if name.is_null() {
        return;
    }
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    with(&weak, |s| s.on_keybinding(&name));
}

unsafe extern "C" fn drop_weak(data: glib_sys::gpointer) {
    drop(unsafe { Weak::from_raw(data as *const Screenshots) });
}

fn with(weak: &Weak<Screenshots>, f: impl FnOnce(&Rc<Screenshots>)) {
    if let Some(s) = weak.upgrade() {
        f(&s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: i32, height: i32) -> MtkRectangle {
        MtkRectangle { x, y, width, height }
    }

    fn parts(r: MtkRectangle) -> (i32, i32, i32, i32) {
        (r.x, r.y, r.width, r.height)
    }

    #[test]
    fn drags_work_in_any_direction() {
        assert_eq!(parts(drag_rect((10.0, 20.0), (30.0, 25.0))), (10, 20, 20, 5));
        assert_eq!(parts(drag_rect((30.0, 25.0), (10.0, 20.0))), (10, 20, 20, 5));
    }

    #[test]
    fn areas_are_clipped_to_the_screen() {
        let screen = rect(0, 0, 100, 100);
        assert_eq!(intersect(rect(-10, 90, 30, 30), screen).map(parts), Some((0, 90, 20, 10)));
        assert!(intersect(rect(100, 0, 10, 10), screen).is_none());
        assert!(intersect(rect(0, 0, -5, 10), screen).is_none());
        assert_eq!(intersect(rect(10, 10, i32::MAX, i32::MAX), screen).map(parts), Some((10, 10, 90, 90)));
    }

    #[test]
    fn only_names_of_session_services_may_call() {
        // A name nobody holds could be taken by any program.
        assert!(!ALLOWED_CALLERS.contains(&"org.gnome.Screenshot"));
        assert_eq!(ALLOWED_CALLERS.len(), 2);
    }

    #[test]
    fn dbus_file_names() {
        let to = |name| destination("default", name);
        assert_eq!(to(None), Ok(Destination::New("default")));
        assert_eq!(to(Some("")), Ok(Destination::New("default")));
        assert_eq!(to(Some("/tmp/a")), Ok(Destination::Path(Path::new("/tmp/a"))));
        assert_eq!(to(Some("shot")), Ok(Destination::New("shot")));
        assert_eq!(to(Some("shot.png")), Ok(Destination::New("shot")));
        assert_eq!(to(Some("..hidden")), Ok(Destination::New("..hidden")));
        for escape in ["..", ".", ".png", "..png", "../x", "a/b", "a/../../b", "x/", "a\0b"] {
            assert!(to(Some(escape)).is_err(), "{escape:?}");
        }
    }

    #[test]
    fn names_never_overwrite_but_paths_replace() {
        let root = std::env::temp_dir().join(format!("gnutshell-store-{}", std::process::id()));
        let folder = root.join("Screenshots");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("keep.png"), "old").unwrap();
        // A link in the way is not followed.
        std::fs::write(root.join("victim"), "old").unwrap();
        std::os::unix::fs::symlink(root.join("victim"), folder.join("link.png")).unwrap();

        assert_eq!(store(&folder, "d", Some("keep"), b"new"), Ok(folder.join("keep (2).png")));
        assert_eq!(std::fs::read(folder.join("keep.png")).unwrap(), b"old");
        assert_eq!(store(&folder, "d", Some("link"), b"new"), Ok(folder.join("link (2).png")));
        assert_eq!(std::fs::read(root.join("victim")).unwrap(), b"old");
        assert!(store(&folder, "d", Some("../escaped"), b"new").is_err());
        assert!(!root.join("escaped.png").exists());

        let chosen = root.join("chosen.png");
        std::fs::write(&chosen, "old").unwrap();
        assert_eq!(store(&folder, "d", chosen.to_str(), b"new"), Ok(chosen.clone()));
        assert_eq!(std::fs::read(&chosen).unwrap(), b"new");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn draws_the_pointer_over_the_picture() {
        let mut target = Picture { width: 3, height: 1, data: [10, 10, 10, 255].repeat(3) };
        // One opaque white pixel and one fully transparent one.
        let image = Picture { width: 2, height: 1, data: vec![255, 255, 255, 255, 0, 0, 0, 0] };
        draw_over(&mut target, &image, 1.0, 0.0, 2.0, 1.0).unwrap();
        let blue: Vec<u8> = target.data.chunks(4).map(|p| p[0]).collect();
        assert_eq!(blue, [10, 255, 10]);
    }

    #[test]
    fn reads_colors_from_bgra() {
        let picture = Picture { width: 1, height: 1, data: vec![255, 0, 51, 255] };
        assert_eq!(color_of(&picture), Some((0.2, 0.0, 1.0)));
        assert_eq!(color_of(&Picture { width: 0, height: 0, data: vec![] }), None);
    }

    #[test]
    fn encodes_png() {
        let png = encode_png(Picture { width: 2, height: 1, data: vec![255; 8] }).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        assert!(encode_png(Picture { width: 2, height: 2, data: vec![0; 8] }).is_err());
    }
}
