//! The Bluetooth adapter and the paired devices, from BlueZ on the system bus.

use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
/// Connecting to a device that is slow or away takes a while.
const CONNECT_TIMEOUT_MS: i32 = 30_000;

/// `GetManagedObjects`: object path to interface to property.
type Objects = HashMap<glib::variant::ObjectPath, HashMap<String, HashMap<String, glib::Variant>>>;

#[derive(Clone, Debug, PartialEq)]
pub struct Adapter {
    pub path: String,
    pub powered: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub path: String,
    pub name: String,
    pub connected: bool,
    /// A connect or disconnect is under way.
    pub busy: bool,
    /// The last one failed.
    pub failed: bool,
}

/// The first adapter, and its paired devices by name.
fn read(objects: &Objects) -> (Option<Adapter>, Vec<Device>) {
    let flag = |properties: &HashMap<String, glib::Variant>, key: &str| {
        properties.get(key).and_then(|v| v.get::<bool>()).unwrap_or(false)
    };
    let adapter = objects
        .iter()
        .filter_map(|(path, interfaces)| interfaces.get(ADAPTER).map(|p| (path.as_str(), p)))
        .min_by_key(|(path, _)| path.to_string())
        .map(|(path, p)| Adapter { path: path.to_owned(), powered: flag(p, "Powered") });
    let Some(adapter) = adapter else { return (None, Vec::new()) };

    let own = format!("{}/", adapter.path);
    let mut devices: Vec<Device> = objects
        .iter()
        .filter(|(path, _)| path.as_str().starts_with(&own))
        .filter_map(|(path, interfaces)| interfaces.get(DEVICE).map(|p| (path.as_str(), p)))
        .filter(|(_, p)| flag(p, "Paired"))
        .map(|(path, p)| Device {
            path: path.to_owned(),
            name: p.get("Alias").and_then(|v| v.get::<String>()).unwrap_or_else(|| "Unnamed device".to_owned()),
            connected: flag(p, "Connected"),
            busy: false,
            failed: false,
        })
        .collect();
    devices.sort_by(|a, b| (a.name.to_lowercase(), &a.path).cmp(&(b.name.to_lowercase(), &b.path)));
    (Some(adapter), devices)
}

#[derive(Default)]
struct State {
    bus: Option<gio::DBusConnection>,
    subscriptions: Vec<gio::SignalSubscription>,
    adapter: Option<Adapter>,
    devices: Vec<Device>,
    busy: HashSet<String>,
    failed: HashSet<String>,
}

type Listener = Rc<dyn Fn()>;

#[derive(Default)]
pub struct Bluetooth {
    state: RefCell<State>,
    listener: RefCell<Option<Listener>>,
    /// A reading is due, so that a burst of signals makes only one.
    queued: Cell<bool>,
}

impl Bluetooth {
    pub fn new() -> Rc<Self> {
        let this = Rc::new(Self::default());
        let weak = Rc::downgrade(&this);
        gio::bus_get(gio::BusType::System, gio::Cancellable::NONE, move |bus| match bus {
            Ok(bus) => with(&weak, |b| b.start(bus)),
            Err(e) => glib::g_warning!("gnutshell", "bluetooth: no system bus: {e}"),
        });
        this
    }

    /// `listener` runs after every change.
    pub fn set_listener(&self, listener: impl Fn() + 'static) {
        *self.listener.borrow_mut() = Some(Rc::new(listener));
    }

    fn start(self: &Rc<Self>, bus: gio::DBusConnection) {
        // BlueZ coming or going, objects appearing, and properties changing.
        let wanted = [
            (Some("org.freedesktop.DBus"), "org.freedesktop.DBus", "NameOwnerChanged", Some(BLUEZ)),
            (Some(BLUEZ), OBJECT_MANAGER, "InterfacesAdded", None),
            (Some(BLUEZ), OBJECT_MANAGER, "InterfacesRemoved", None),
            (Some(BLUEZ), PROPERTIES, "PropertiesChanged", None),
        ];
        let subscriptions = wanted
            .into_iter()
            .map(|(sender, interface, member, first)| {
                let weak = Rc::downgrade(self);
                bus.subscribe_to_signal(
                    sender,
                    Some(interface),
                    Some(member),
                    None,
                    first,
                    gio::DBusSignalFlags::NONE,
                    move |_| with(&weak, |b| b.queue_read()),
                )
            })
            .collect();
        {
            let mut state = self.state.borrow_mut();
            state.subscriptions = subscriptions;
            state.bus = Some(bus);
        }
        self.queue_read();
    }

    fn queue_read(self: &Rc<Self>) {
        if self.queued.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        // Properties such as signal strength change often.
        glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || {
            with(&weak, |b| {
                b.queued.set(false);
                b.read();
            });
        });
    }

    fn read(self: &Rc<Self>) {
        let Some(bus) = self.state.borrow().bus.clone() else { return };
        let weak = Rc::downgrade(self);
        bus.call(
            Some(BLUEZ),
            "/",
            OBJECT_MANAGER,
            "GetManagedObjects",
            None,
            None,
            // Looking at Bluetooth must not start the service.
            gio::DBusCallFlags::NO_AUTO_START,
            -1,
            gio::Cancellable::NONE,
            move |reply| {
                let objects = reply.ok().and_then(|r| r.get::<(Objects,)>()).map(|(o,)| o);
                with(&weak, |b| b.take(&objects.unwrap_or_default()));
            },
        );
    }

    fn take(&self, objects: &Objects) {
        let (adapter, devices) = read(objects);
        let changed = {
            let mut state = self.state.borrow_mut();
            let changed = state.adapter != adapter || state.devices != devices;
            state.adapter = adapter;
            state.devices = devices;
            changed
        };
        if changed {
            self.changed();
        }
    }

    fn changed(&self) {
        let listener = self.listener.borrow().clone();
        if let Some(listener) = listener {
            listener();
        }
    }

    /// `None` without an adapter or without BlueZ.
    pub fn adapter(&self) -> Option<Adapter> {
        self.state.borrow().adapter.clone()
    }

    pub fn devices(&self) -> Vec<Device> {
        let state = self.state.borrow();
        state
            .devices
            .iter()
            .cloned()
            .map(|d| Device { busy: state.busy.contains(&d.path), failed: state.failed.contains(&d.path), ..d })
            .collect()
    }

    /// Switch the adapter on or off.
    pub fn toggle_power(self: &Rc<Self>) {
        let (Some(bus), Some(adapter)) = (self.state.borrow().bus.clone(), self.adapter()) else { return };
        let on = !adapter.powered;
        // Old failures say nothing about the next try.
        self.state.borrow_mut().failed.clear();
        // A soft block (airplane mode) would keep it off.
        if on {
            unblock();
        }
        let weak = Rc::downgrade(self);
        bus.call(
            Some(BLUEZ),
            &adapter.path,
            PROPERTIES,
            "Set",
            Some(&(ADAPTER, "Powered", on.to_variant()).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            -1,
            gio::Cancellable::NONE,
            move |reply| {
                if let Err(e) = reply {
                    glib::g_warning!("gnutshell", "bluetooth: could not switch the adapter: {e}");
                }
                with(&weak, |b| b.queue_read());
            },
        );
    }

    /// Connect the paired device at `path`, or disconnect it if connected.
    pub fn toggle_device(self: &Rc<Self>, path: &str) {
        let Some(bus) = self.state.borrow().bus.clone() else { return };
        let Some(device) = self.devices().into_iter().find(|d| d.path == path) else { return };
        if device.busy {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.busy.insert(device.path.clone());
            state.failed.remove(&device.path);
        }
        self.changed();
        let method = if device.connected { "Disconnect" } else { "Connect" };
        let weak = Rc::downgrade(self);
        bus.call(
            Some(BLUEZ),
            &device.path.clone(),
            DEVICE,
            method,
            None,
            None,
            gio::DBusCallFlags::NONE,
            CONNECT_TIMEOUT_MS,
            gio::Cancellable::NONE,
            move |reply| {
                with(&weak, |b| {
                    {
                        let mut state = b.state.borrow_mut();
                        state.busy.remove(&device.path);
                        if let Err(e) = &reply {
                            glib::g_message!("gnutshell", "bluetooth: {method} failed: {e}");
                            state.failed.insert(device.path.clone());
                        }
                    }
                    b.changed();
                    b.queue_read();
                });
            },
        );
    }

    pub fn destroy(&self) {
        self.listener.borrow_mut().take();
        // Dropping the subscriptions ends them.
        self.state.borrow_mut().subscriptions.clear();
    }
}

/// Ask gsd-rfkill to lift Bluetooth's airplane mode.
fn unblock() {
    gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, |bus| {
        let Ok(bus) = bus else { return };
        let value = false.to_variant();
        bus.call(
            Some("org.gnome.SettingsDaemon.Rfkill"),
            "/org/gnome/SettingsDaemon/Rfkill",
            PROPERTIES,
            "Set",
            Some(&("org.gnome.SettingsDaemon.Rfkill", "BluetoothAirplaneMode", value).to_variant()),
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            -1,
            gio::Cancellable::NONE,
            |reply| {
                if let Err(e) = reply {
                    glib::g_debug!("gnutshell", "bluetooth: no rfkill service: {e}");
                }
            },
        );
    });
}

fn with(weak: &Weak<Bluetooth>, f: impl FnOnce(&Rc<Bluetooth>)) {
    if let Some(bluetooth) = weak.upgrade() {
        f(&bluetooth);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(objects: &mut Objects, path: &str, interface: &str, properties: &[(&str, glib::Variant)]) {
        let properties = properties.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let path = glib::variant::ObjectPath::try_from(path.to_owned()).unwrap();
        objects.entry(path).or_default().insert(interface.to_owned(), properties);
    }

    #[test]
    fn lists_the_paired_devices_of_the_first_adapter() {
        let mut objects = Objects::new();
        object(&mut objects, "/org/bluez/hci1", ADAPTER, &[("Powered", false.to_variant())]);
        object(&mut objects, "/org/bluez/hci0", ADAPTER, &[("Powered", true.to_variant())]);
        let device = |name: &str, paired: bool, connected: bool| {
            [("Alias", name.to_variant()), ("Paired", paired.to_variant()), ("Connected", connected.to_variant())]
        };
        object(&mut objects, "/org/bluez/hci0/dev_1", DEVICE, &device("speaker", true, false));
        object(&mut objects, "/org/bluez/hci0/dev_2", DEVICE, &device("Mouse", true, true));
        object(&mut objects, "/org/bluez/hci0/dev_3", DEVICE, &device("Stranger", false, false));
        object(&mut objects, "/org/bluez/hci1/dev_4", DEVICE, &device("Other", true, false));

        let (adapter, devices) = read(&objects);
        assert_eq!(adapter, Some(Adapter { path: "/org/bluez/hci0".into(), powered: true }));
        let summary: Vec<(&str, bool)> = devices.iter().map(|d| (d.name.as_str(), d.connected)).collect();
        assert_eq!(summary, [("Mouse", true), ("speaker", false)]);
        assert_eq!(read(&Objects::new()), (None, Vec::new()));
    }
}
