//! Screenshots: gnome-shell's screenshot keybindings and its
//! `org.gnome.Shell.Screenshot` D-Bus API (used by the GNOME portal and
//! gnome-screenshot).
//!
//! `Print` selects an area (Enter takes the whole screen), `Shift+Print`
//! takes the whole screen and `Alt+Print` the focused window. Those save a PNG
//! in `Pictures/Screenshots`, copy it to the clipboard and say so in a
//! notification. The pointer is never in the picture.
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

const ALLOWED_CALLERS: [&str; 3] = [
    "org.freedesktop.impl.portal.desktop.gnome",
    "org.gnome.SettingsDaemon.MediaKeys",
    "org.gnome.Screenshot",
];

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

const SELECTION_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0x35, 0x84, 0xe4, 0x60);
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
}

struct Selection {
    purpose: Purpose,
    grab: *mut ffi::ClutterGrab,
    /// Where the drag started, in stage coordinates.
    start: Option<(f32, f32)>,
}

#[derive(Default)]
struct State {
    selection: Option<Selection>,
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
    state: RefCell<State>,
}

/// Screen pixels: tightly packed premultiplied BGRA.
struct Picture {
    width: i32,
    height: i32,
    data: Vec<u8>,
}

impl Screenshots {
    pub fn new(display: *mut MetaDisplay, stage: *mut ClutterActor, notifications: Rc<Notifications>) -> Rc<Self> {
        let (overlay, marker, hint) = unsafe {
            let overlay = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(overlay, &ui::DIM_COLOR);
            ffi::clutter_actor_set_reactive(overlay, 1);
            ffi::clutter_actor_hide(overlay);
            let marker = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(marker, &SELECTION_COLOR);
            ffi::clutter_actor_add_child(overlay, marker);
            let hint = ui::label(
                ui::ROW_FONT,
                "Drag to select an area  ·  Enter: whole screen  ·  Escape: cancel",
                &ui::TEXT_COLOR,
            );
            ffi::clutter_actor_add_child(overlay, hint);
            ffi::clutter_actor_add_child(stage, overlay);
            (overlay, marker, hint)
        };
        let this =
            Rc::new(Self { display, stage, notifications, overlay, marker, hint, state: RefCell::default() });
        this.connect_overlay();
        this.install_keybindings();
        this.own_name();
        this
    }

    /// Give up any selection, e.g. because the screen locked.
    pub fn cancel(&self) {
        if let Some(purpose) = self.end_selection() {
            match purpose {
                Purpose::Shortcut => {}
                Purpose::SelectArea(i) | Purpose::Interactive(i) => {
                    i.return_dbus_error("org.freedesktop.DBus.Error.Failed", "Cancelled");
                }
            }
        }
    }

    pub fn destroy(&self) {
        self.cancel();
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
            "screenshot" => self.take_for_user(self.screen_rect()),
            "screenshot-window" => {
                if let Some(rect) = self.focus_window_rect() {
                    self.take_for_user(rect);
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
                ui::KEY_RETURN | ui::KEY_KP_ENTER => s.finish_selection(s.screen_rect()),
                _ => {}
            });
            Some(true.to_value())
        });
        let weak = Rc::downgrade(self);
        overlay.connect_local("button-press-event", false, move |args| {
            let at = event_coords(ui::event_arg(args));
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
                let at = event_coords(ui::event_arg(args));
                with(&weak, |s| s.drag(at, released));
                Some(true.to_value())
            });
        }
    }

    fn start_selection(self: &Rc<Self>, purpose: Purpose) {
        if self.state.borrow().selection.is_some() {
            if let Purpose::SelectArea(i) | Purpose::Interactive(i) = purpose {
                i.return_dbus_error("org.freedesktop.DBus.Error.Failed", "A selection is already in progress");
            }
            return;
        }
        let screen = self.screen_rect();
        let grab = unsafe {
            ffi::clutter_actor_set_position(self.overlay, screen.x as f32, screen.y as f32);
            ffi::clutter_actor_set_size(self.overlay, screen.width as f32, screen.height as f32);
            ffi::clutter_actor_hide(self.marker);
            if let Some(geo) = ui::primary_monitor_geometry(self.display) {
                let width = ffi::clutter_actor_get_width(self.hint);
                ffi::clutter_actor_set_position(
                    self.hint,
                    ((geo.x - screen.x) as f32 + (geo.width as f32 - width) / 2.0).floor(),
                    (geo.y - screen.y) as f32 + 64.0,
                );
            }
            ffi::clutter_actor_show(self.overlay);
            ffi::clutter_actor_set_child_above_sibling(self.stage, self.overlay, std::ptr::null_mut());
            let grab = ffi::clutter_stage_grab(self.stage as *mut ffi::ClutterStage, self.overlay);
            ffi::clutter_actor_grab_key_focus(self.overlay);
            grab
        };
        self.state.borrow_mut().selection = Some(Selection { purpose, grab, start: None });
    }

    fn drag(self: &Rc<Self>, at: Option<(f32, f32)>, released: bool) {
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

    /// Take the overlay away. Returns what the selection was for.
    fn end_selection(&self) -> Option<Purpose> {
        let selection = self.state.borrow_mut().selection.take()?;
        unsafe {
            ffi::clutter_grab_dismiss(selection.grab);
            gobject_sys::g_object_unref(selection.grab as *mut gobject_sys::GObject);
            ffi::clutter_actor_hide(self.overlay);
        }
        Some(selection.purpose)
    }

    fn finish_selection(self: &Rc<Self>, rect: MtkRectangle) {
        // The overlay is hidden first, so it isn't in the picture.
        let Some(purpose) = self.end_selection() else { return };
        match purpose {
            Purpose::Shortcut => self.take_for_user(rect),
            Purpose::SelectArea(invocation) => {
                invocation.return_value(Some(&(rect.x, rect.y, rect.width, rect.height).to_variant()));
            }
            Purpose::Interactive(invocation) => match self.capture(rect) {
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

    /// Paint the part of `rect` that is on screen.
    fn capture(&self, rect: MtkRectangle) -> Result<Picture, String> {
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
                ffi::CLUTTER_PAINT_FLAG_CLEAR,
                &mut err,
            );
            if ok == 0 {
                let err: glib::Error = from_glib_full(err);
                return Err(err.to_string());
            }
        }
        data.truncate(stride as usize * height as usize);
        Ok(Picture { width, height, data })
    }

    /// A screenshot the user asked for with a key.
    fn take_for_user(self: &Rc<Self>, rect: MtkRectangle) {
        let picture = self.capture(rect);
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
        match method {
            "Screenshot" => {
                let (_, _, name) = params.get::<(bool, bool, String)>().unwrap_or_default();
                self.reply_with_file(self.capture(self.screen_rect()), name, invocation);
            }
            "ScreenshotWindow" => {
                let (_, _, _, name) = params.get::<(bool, bool, bool, String)>().unwrap_or_default();
                let picture = self
                    .focus_window_rect()
                    .ok_or_else(|| "No window has focus".to_owned())
                    .and_then(|rect| self.capture(rect));
                self.reply_with_file(picture, name, invocation);
            }
            "ScreenshotArea" => {
                let (x, y, width, height, _, name) =
                    params.get::<(i32, i32, i32, i32, bool, String)>().unwrap_or_default();
                self.reply_with_file(self.capture(MtkRectangle { x, y, width, height }), name, invocation);
            }
            "SelectArea" => self.start_selection(Purpose::SelectArea(invocation)),
            "InteractiveScreenshot" => self.start_selection(Purpose::Interactive(invocation)),
            // There is no flash effect.
            "FlashArea" => invocation.return_value(None),
            _ => invocation.return_dbus_error(
                "org.freedesktop.DBus.Error.UnknownMethod",
                &format!("{method} is not implemented by gnutshell"),
            ),
        }
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
        let path = match name.filter(|n| !n.is_empty()) {
            Some(name) => {
                let path = named_path(&folder, &name);
                write(&path, &png, false)?;
                path
            }
            None => write_new(&folder, &format!("Screenshot From {stamp}"), &png)?,
        };
        Ok((path, png))
    })
    .await
    .map_err(|_| "The PNG writer crashed".to_owned())?
}

/// Where the D-Bus `filename` argument points, as in gnome-shell: an absolute
/// path as it is; a bare name in the screenshot folder, as a `.png`.
fn named_path(folder: &Path, name: &str) -> PathBuf {
    if Path::new(name).is_absolute() {
        return PathBuf::from(name);
    }
    let name = if name.ends_with(".png") { name.to_owned() } else { format!("{name}.png") };
    folder.join(name)
}

fn write(path: &Path, png: &[u8], must_be_new: bool) -> Result<(), String> {
    use std::io::Write;
    let fail = |e: std::io::Error| format!("Could not write {}: {e}", path.display());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(fail)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if must_be_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    options.open(path).and_then(|mut f| f.write_all(png)).map_err(fail)
}

/// Write `<stem>.png` in `folder`, or `<stem> (2).png` and so on: never over
/// an existing file.
fn write_new(folder: &Path, stem: &str, png: &[u8]) -> Result<PathBuf, String> {
    let mut error = String::new();
    for n in 1..100 {
        let name = if n == 1 { format!("{stem}.png") } else { format!("{stem} ({n}).png") };
        let path = folder.join(name);
        match write(&path, png, true) {
            Ok(()) => return Ok(path),
            Err(e) if path.exists() => error = e,
            Err(e) => return Err(e),
        }
    }
    Err(error)
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

fn event_coords(event: *const ffi::ClutterEvent) -> Option<(f32, f32)> {
    if event.is_null() {
        return None;
    }
    let (mut x, mut y) = (0.0, 0.0);
    unsafe { ffi::clutter_event_get_coords(event, &mut x, &mut y) };
    Some((x, y))
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
    fn dbus_file_names() {
        let folder = Path::new("/pictures/Screenshots");
        assert_eq!(named_path(folder, "/tmp/a"), Path::new("/tmp/a"));
        assert_eq!(named_path(folder, "shot"), folder.join("shot.png"));
        assert_eq!(named_path(folder, "shot.png"), folder.join("shot.png"));
    }

    #[test]
    fn encodes_png() {
        let png = encode_png(Picture { width: 2, height: 1, data: vec![255; 8] }).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
        assert!(encode_png(Picture { width: 2, height: 2, data: vec![0; 8] }).is_err());
    }
}
