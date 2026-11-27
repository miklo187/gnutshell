//! The part of gnome-shell's `org.gnome.Shell` D-Bus API that session
//! services depend on:
//!
//! - `GrabAccelerator(s)` / `UngrabAccelerator(s)` and the
//!   `AcceleratorActivated` / `AcceleratorDeactivated` signals: how
//!   gnome-settings-daemon's media-keys plugin binds volume, brightness,
//!   media and custom shortcut keys.
//! - `ShowOSD`: the volume / brightness popup.
//! - `Mode` and `OverviewActive` properties.
//!
//! Like gnome-shell, only `org.gnome.Settings`, gsd-media-keys and the GNOME
//! portal may call the methods, and a client's grabs are released when it
//! leaves the bus. Other gnome-shell methods (Eval, screenshots, …) are not
//! provided and fail with UnknownMethod.

use crate::ffi::{self, MetaDisplay};
use crate::osd::{Osd, OsdRequest};
use gio::prelude::*;
use glib::translate::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

const BUS_NAME: &str = "org.gnome.Shell";
const OBJECT_PATH: &str = "/org/gnome/Shell";
const INTERFACE: &str = "org.gnome.Shell";

/// Well-known names allowed to grab accelerators and show OSDs.
const ALLOWED_CALLERS: [&str; 3] = [
    "org.gnome.Settings",
    "org.gnome.SettingsDaemon.MediaKeys",
    "org.freedesktop.impl.portal.desktop.gnome",
];

/// gnome-shell's Shell.ActionMode values, reported with activations and
/// used by clients to say where an accelerator may fire.
const ACTION_MODE_NORMAL: u32 = 1;
const ACTION_MODE_LOCK_SCREEN: u32 = 1 << 2;

thread_local! {
    /// Accelerator actions whose owner allowed them on the lock screen.
    static LOCK_SAFE: RefCell<std::collections::HashSet<u32>> = RefCell::default();
}

/// Whether keybinding `name` is an accelerator grabbed with the
/// LOCK_SCREEN action mode (e.g. volume keys by gsd-media-keys).
pub fn allowed_on_lock_screen(name: &str) -> bool {
    name.strip_prefix("external-grab-")
        .and_then(|n| n.parse::<u32>().ok())
        .is_some_and(|action| LOCK_SAFE.with_borrow(|s| s.contains(&action)))
}
/// Meta.KeyBindingAction.NONE: grab failed.
const ACTION_NONE: u32 = 0;

const INTERFACE_XML: &str = r#"
<node>
  <interface name="org.gnome.Shell">
    <method name="ShowOSD">
      <arg type="a{sv}" direction="in" name="params"/>
    </method>
    <method name="GrabAccelerator">
      <arg type="s" direction="in" name="accelerator"/>
      <arg type="u" direction="in" name="modeFlags"/>
      <arg type="u" direction="in" name="grabFlags"/>
      <arg type="u" direction="out" name="action"/>
    </method>
    <method name="GrabAccelerators">
      <arg type="a(suu)" direction="in" name="accelerators"/>
      <arg type="au" direction="out" name="actions"/>
    </method>
    <method name="UngrabAccelerator">
      <arg type="u" direction="in" name="action"/>
      <arg type="b" direction="out" name="success"/>
    </method>
    <method name="UngrabAccelerators">
      <arg type="au" direction="in" name="action"/>
      <arg type="b" direction="out" name="success"/>
    </method>
    <signal name="AcceleratorActivated">
      <arg name="action" type="u"/>
      <arg name="parameters" type="a{sv}"/>
    </signal>
    <signal name="AcceleratorDeactivated">
      <arg name="action" type="u"/>
      <arg name="parameters" type="a{sv}"/>
    </signal>
    <property name="Mode" type="s" access="read"/>
    <property name="OverviewActive" type="b" access="read"/>
  </interface>
</node>
"#;

pub struct ShellService {
    inner: Rc<Inner>,
    owner: Option<gio::OwnerId>,
}

struct Inner {
    display: *mut MetaDisplay,
    osd: Rc<Osd>,
    connection: RefCell<Option<gio::DBusConnection>>,
    registration: RefCell<Option<gio::RegistrationId>>,
    /// allowed well-known name -> its current unique owner
    allowed: RefCell<HashMap<String, String>>,
    allowed_watchers: RefCell<Vec<gio::WatcherId>>,
    /// accelerator action -> unique name of the client that grabbed it
    grabs: RefCell<HashMap<u32, String>>,
    /// grabbing client -> watcher that ungrabs when it leaves the bus
    grabbers: RefCell<HashMap<String, gio::WatcherId>>,
    display_handlers: RefCell<Vec<glib::SignalHandlerId>>,
}

impl ShellService {
    pub fn new(display: *mut MetaDisplay, osd: Rc<Osd>) -> Self {
        let inner = Rc::new(Inner {
            display,
            osd,
            connection: RefCell::default(),
            registration: RefCell::default(),
            allowed: RefCell::default(),
            allowed_watchers: RefCell::default(),
            grabs: RefCell::default(),
            grabbers: RefCell::default(),
            display_handlers: RefCell::default(),
        });
        inner.connect_display();

        let weak = Rc::downgrade(&inner);
        let owner = gio::bus_own_name(
            gio::BusType::Session,
            BUS_NAME,
            gio::BusNameOwnerFlags::NONE,
            move |connection, _| {
                if let Some(inner) = weak.upgrade() {
                    inner.export(connection);
                }
            },
            |_, name| glib::g_debug!("gnutshell", "acquired {name}"),
            |_, name| glib::g_warning!("gnutshell", "could not own {name} (is another shell running?)"),
        );
        Self { inner, owner: Some(owner) }
    }
}

impl Drop for ShellService {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            gio::bus_unown_name(owner);
        }
        self.inner.shutdown();
    }
}

impl Inner {
    fn export(self: &Rc<Self>, connection: gio::DBusConnection) {
        let info = gio::DBusNodeInfo::for_xml(INTERFACE_XML)
            .ok()
            .and_then(|node| node.lookup_interface(INTERFACE))
            .expect("valid org.gnome.Shell interface XML");

        let weak = Rc::downgrade(self);
        let registration = connection
            .register_object(OBJECT_PATH, &info)
            .method_call(move |_, sender, _, _, method, params, invocation| {
                match weak.upgrade() {
                    Some(inner) => inner.on_method_call(sender.unwrap_or_default(), method, &params, invocation),
                    None => invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "shutting down"),
                }
            })
            .property(|_, _, _, _, property| match property {
                "Mode" => "user".to_variant(),
                _ => false.to_variant(), // OverviewActive: there is no overview
            })
            .build();
        match registration {
            Ok(id) => *self.registration.borrow_mut() = Some(id),
            Err(e) => glib::g_warning!("gnutshell", "could not export {OBJECT_PATH}: {e}"),
        }

        for name in ALLOWED_CALLERS {
            let appeared = Rc::downgrade(self);
            let vanished = Rc::downgrade(self);
            let id = gio::bus_watch_name_on_connection(
                &connection,
                name,
                gio::BusNameWatcherFlags::NONE,
                move |_, name, owner| {
                    if let Some(inner) = appeared.upgrade() {
                        inner.allowed.borrow_mut().insert(name.to_owned(), owner.to_owned());
                    }
                },
                move |_, name| {
                    if let Some(inner) = vanished.upgrade() {
                        inner.allowed.borrow_mut().remove(name);
                    }
                },
            );
            self.allowed_watchers.borrow_mut().push(id);
        }
        *self.connection.borrow_mut() = Some(connection);
    }

    fn connect_display(self: &Rc<Self>) {
        let display: glib::Object = unsafe { from_glib_none(self.display as *mut gobject_sys::GObject) };
        let mut handlers = Vec::new();
        for (signal, dbus_signal) in [
            ("accelerator-activated", "AcceleratorActivated"),
            ("accelerator-deactivated", "AcceleratorDeactivated"),
        ] {
            let weak = Rc::downgrade(self);
            handlers.push(display.connect_local(signal, false, move |args| {
                // (MetaDisplay, guint action, ClutterInputDevice, guint timestamp)
                let action = args.get(1).and_then(|v| v.get::<u32>().ok()).unwrap_or(ACTION_NONE);
                let device = args.get(2).and_then(|v| v.get::<glib::Object>().ok());
                let timestamp = args.get(3).and_then(|v| v.get::<u32>().ok()).unwrap_or(0);
                if let Some(inner) = weak.upgrade() {
                    inner.emit_accelerator(dbus_signal, action, device.as_ref(), timestamp);
                }
                None
            }));
        }
        *self.display_handlers.borrow_mut() = handlers;
    }

    fn on_method_call(
        self: &Rc<Self>,
        sender: &str,
        method: &str,
        params: &glib::Variant,
        invocation: gio::DBusMethodInvocation,
    ) {
        if !self.is_allowed(sender) {
            invocation.return_dbus_error(
                "org.freedesktop.DBus.Error.AccessDenied",
                &format!("{method} is not allowed"),
            );
            return;
        }
        match method {
            "GrabAccelerator" => {
                let (accel, mode, flags) = params.get::<(String, u32, u32)>().unwrap_or_default();
                let action = self.grab(&accel, mode, flags, sender);
                invocation.return_value(Some(&(action,).to_variant()));
            }
            "GrabAccelerators" => {
                let (list,) = params.get::<(Vec<(String, u32, u32)>,)>().unwrap_or_default();
                let actions: Vec<u32> =
                    list.iter().map(|(accel, mode, flags)| self.grab(accel, *mode, *flags, sender)).collect();
                invocation.return_value(Some(&(actions,).to_variant()));
            }
            "UngrabAccelerator" => {
                let (action,) = params.get::<(u32,)>().unwrap_or_default();
                let ok = self.ungrab(action, sender);
                invocation.return_value(Some(&(ok,).to_variant()));
            }
            "UngrabAccelerators" => {
                let (actions,) = params.get::<(Vec<u32>,)>().unwrap_or_default();
                // Ungrab all of them even if one fails, like gnome-shell.
                let results: Vec<bool> = actions.iter().map(|a| self.ungrab(*a, sender)).collect();
                let ok = results.iter().all(|r| *r);
                invocation.return_value(Some(&(ok,).to_variant()));
            }
            "ShowOSD" => {
                let request = parse_osd(params);
                self.osd.show(&request);
                invocation.return_value(None);
            }
            _ => invocation.return_dbus_error(
                "org.freedesktop.DBus.Error.UnknownMethod",
                &format!("{method} is not implemented by gnutshell"),
            ),
        }
    }

    /// Is `sender` the current owner of one of `ALLOWED_CALLERS`? The name
    /// watchers normally know; ask the bus directly if they haven't caught
    /// up yet (a client that just acquired its name).
    fn is_allowed(&self, sender: &str) -> bool {
        if self.allowed.borrow().values().any(|owner| owner == sender) {
            return true;
        }
        let Some(connection) = self.connection.borrow().clone() else { return false };
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

    fn grab(self: &Rc<Self>, accelerator: &str, mode: u32, flags: u32, sender: &str) -> u32 {
        let Ok(c) = std::ffi::CString::new(accelerator) else { return ACTION_NONE };
        let action = unsafe { ffi::meta_display_grab_accelerator(self.display, c.as_ptr(), flags) };
        if action == ACTION_NONE {
            glib::g_debug!("gnutshell", "grab of {accelerator:?} failed");
            return ACTION_NONE;
        }
        glib::g_debug!("gnutshell", "grabbed {accelerator:?} as action {action} for {sender}");
        self.grabs.borrow_mut().insert(action, sender.to_owned());
        LOCK_SAFE.with_borrow_mut(|s| {
            if mode & ACTION_MODE_LOCK_SCREEN != 0 {
                s.insert(action);
            } else {
                s.remove(&action);
            }
        });
        self.watch_grabber(sender);
        action
    }

    fn ungrab(&self, action: u32, sender: &str) -> bool {
        if self.grabs.borrow().get(&action).map(String::as_str) != Some(sender) {
            return false;
        }
        self.grabs.borrow_mut().remove(&action);
        LOCK_SAFE.with_borrow_mut(|s| s.remove(&action));
        unsafe { ffi::meta_display_ungrab_accelerator(self.display, action) != 0 }
    }

    /// Release a client's grabs when it disconnects.
    fn watch_grabber(self: &Rc<Self>, sender: &str) {
        if self.grabbers.borrow().contains_key(sender) {
            return;
        }
        let Some(connection) = self.connection.borrow().clone() else { return };
        let weak: Weak<Inner> = Rc::downgrade(self);
        let id = gio::bus_watch_name_on_connection(
            &connection,
            sender,
            gio::BusNameWatcherFlags::NONE,
            |_, _, _| {},
            move |_, name| {
                let Some(inner) = weak.upgrade() else { return };
                let actions: Vec<u32> = inner
                    .grabs
                    .borrow()
                    .iter()
                    .filter(|(_, owner)| owner.as_str() == name)
                    .map(|(action, _)| *action)
                    .collect();
                for action in actions {
                    inner.ungrab(action, name);
                }
                glib::g_debug!("gnutshell", "{name} left the bus; released its accelerators");
                if let Some(id) = inner.grabbers.borrow_mut().remove(name) {
                    // Unwatching from inside the callback is fine in GIO, but
                    // defer it to keep the closure alive until it returns.
                    glib::idle_add_local_once(move || gio::bus_unwatch_name(id));
                }
            },
        );
        self.grabbers.borrow_mut().insert(sender.to_owned(), id);
    }

    fn emit_accelerator(&self, signal: &str, action: u32, device: Option<&glib::Object>, timestamp: u32) {
        let Some(destination) = self.grabs.borrow().get(&action).cloned() else { return };
        let Some(connection) = self.connection.borrow().clone() else { return };

        let params = glib::VariantDict::new(None);
        params.insert("timestamp", timestamp);
        let mode = if crate::lockscreen::is_locked() { ACTION_MODE_LOCK_SCREEN } else { ACTION_MODE_NORMAL };
        params.insert("action-mode", mode);
        if let Some(token) = crate::ui::activation_token(self.display, timestamp) {
            params.insert("activation-token", token);
        }
        if let Some(device) = device {
            let node = unsafe {
                ffi::clutter_input_device_get_device_node(device.as_ptr() as *mut ffi::ClutterInputDevice)
            };
            if !node.is_null() {
                let node = unsafe { std::ffi::CStr::from_ptr(node) }.to_string_lossy().into_owned();
                params.insert("device-node", node);
            }
        }
        let body = glib::Variant::tuple_from_iter([action.to_variant(), params.end()]);
        if let Err(e) = connection.emit_signal(Some(&destination), OBJECT_PATH, INTERFACE, signal, Some(&body)) {
            glib::g_warning!("gnutshell", "could not emit {signal}: {e}");
        }
    }

    fn shutdown(&self) {
        let connection = self.connection.borrow_mut().take();
        if let (Some(connection), Some(id)) = (connection.as_ref(), self.registration.borrow_mut().take()) {
            let _ = connection.unregister_object(id);
        }
        for id in self.allowed_watchers.borrow_mut().drain(..) {
            gio::bus_unwatch_name(id);
        }
        for (_, id) in self.grabbers.borrow_mut().drain() {
            gio::bus_unwatch_name(id);
        }
        let actions: Vec<u32> = self.grabs.borrow_mut().drain().map(|(a, _)| a).collect();
        for action in actions {
            unsafe { ffi::meta_display_ungrab_accelerator(self.display, action) };
        }
        LOCK_SAFE.with_borrow_mut(|s| s.clear());
        let display: glib::Object = unsafe { from_glib_none(self.display as *mut gobject_sys::GObject) };
        for id in self.display_handlers.borrow_mut().drain(..) {
            display.disconnect(id);
        }
    }
}

/// `ShowOSD` parameters: icon (serialized GIcon), label, level, max_level.
/// `connector` / `monitor` are ignored: the OSD shows on the primary monitor.
fn parse_osd(params: &glib::Variant) -> OsdRequest {
    let Some((dict,)) = params.get::<(glib::VariantDict,)>() else { return OsdRequest::default() };
    let string = |key: &str| dict.lookup::<String>(key).ok().flatten();
    let number = |key: &str| dict.lookup::<f64>(key).ok().flatten();
    OsdRequest {
        icon: string("icon").and_then(|s| gio::Icon::for_string(&s).ok()),
        label: string("label"),
        level: number("level"),
        max_level: number("max_level").unwrap_or(1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_osd;

    #[test]
    fn parses_show_osd_params() {
        let dict = glib::VariantDict::new(None);
        dict.insert("icon", "audio-volume-high-symbolic");
        dict.insert("label", "Speakers");
        dict.insert("level", 0.4f64);
        dict.insert("max_level", 1.5f64);
        let params = glib::Variant::tuple_from_iter([dict.end()]);
        let r = parse_osd(&params);
        assert_eq!(r.label.as_deref(), Some("Speakers"));
        assert_eq!(r.level, Some(0.4));
        assert_eq!(r.max_level, 1.5);
        assert!(r.icon.is_some());
    }

    #[test]
    fn missing_fields_default() {
        let params = glib::Variant::tuple_from_iter([glib::VariantDict::new(None).end()]);
        let r = parse_osd(&params);
        assert!(r.icon.is_none() && r.label.is_none() && r.level.is_none());
        assert_eq!(r.max_level, 1.0);
    }
}
