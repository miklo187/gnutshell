//! Panel status icons, left of the power button: volume, network and, on
//! machines that have one, battery.
//!
//! On the volume icon, scrolling changes the volume and a middle click mutes.
//! A left click on an icon opens the matching page of GNOME Settings. Volume
//! comes from libpulse (`audio.rs`), network from NetworkManager and battery
//! from UPower's combined `DisplayDevice`. An icon whose service is missing
//! is hidden.

mod audio;

use crate::ffi::{self, ClutterActor, MetaDisplay};
use crate::icons;
use crate::osd::{Osd, OsdRequest};
use crate::ui::{self, gobj};
use audio::{Audio, Volume};
use gio::prelude::*;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

const ICON_SIZE: f32 = 16.0;
const SLOT_PADDING: f32 = 6.0;
const VOLUME_STEP: f64 = 0.05;
const BUTTON_PRIMARY: u32 = 1;
const BUTTON_MIDDLE: u32 = 2;
const HOVER_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x20);

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const NM_ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const NM_ACCESS_POINT: &str = "org.freedesktop.NetworkManager.AccessPoint";
const UPOWER: &str = "org.freedesktop.UPower";
const UPOWER_DISPLAY: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const UPOWER_DEVICE: &str = "org.freedesktop.UPower.Device";

/// Slots, in panel order.
#[derive(Clone, Copy)]
enum Kind {
    Network,
    Volume,
    Battery,
}

const KINDS: [Kind; 3] = [Kind::Network, Kind::Volume, Kind::Battery];

impl Kind {
    fn settings_page(self) -> &'static str {
        match self {
            Kind::Network => "network",
            Kind::Volume => "sound",
            Kind::Battery => "power",
        }
    }
}

struct Slot {
    button: *mut ClutterActor,
    icon: *mut ClutterActor,
    text: *mut ClutterActor,
}

/// What a slot shows; no icon name means hidden.
#[derive(Clone, Default, PartialEq)]
struct Look {
    icon: Option<String>,
    text: String,
}

#[derive(Default)]
struct State {
    looks: [Look; 3],
    right_edge: f32,
    width: f32,
    scroll: f64,
    network: Option<gio::DBusProxy>,
    /// The primary connection, and the access point if it is Wi-Fi.
    active: Option<gio::DBusProxy>,
    access_point: Option<gio::DBusProxy>,
    /// Object paths those two are for.
    active_path: String,
    access_point_path: String,
    battery: Option<gio::DBusProxy>,
}

pub struct StatusIcons {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    panel_height: f32,
    slots: [Slot; 3],
    audio: RefCell<Option<Rc<Audio>>>,
    osd: Rc<Osd>,
    /// Called when the icons take more or less room.
    resized: Box<dyn Fn()>,
    state: RefCell<State>,
}

impl StatusIcons {
    pub fn new(
        display: *mut MetaDisplay,
        stage: *mut ClutterActor,
        panel: *mut ClutterActor,
        panel_height: f32,
        osd: Rc<Osd>,
        resized: Box<dyn Fn()>,
    ) -> Rc<Self> {
        let slots = KINDS.map(|_| unsafe {
            let button = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(button, 1);
            ffi::clutter_actor_hide(button);
            let icon = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(icon, ICON_SIZE, ICON_SIZE);
            ffi::clutter_actor_set_content_gravity(icon, ffi::CLUTTER_CONTENT_GRAVITY_RESIZE_ASPECT);
            ffi::clutter_actor_set_position(icon, SLOT_PADDING, ((panel_height - ICON_SIZE) / 2.0).floor());
            ffi::clutter_actor_add_child(button, icon);
            let text = ui::label(ui::BODY_FONT, "", &ui::TEXT_COLOR);
            ffi::clutter_actor_add_child(button, text);
            ffi::clutter_actor_add_child(panel, button);
            Slot { button, icon, text }
        });
        let this = Rc::new(Self {
            display,
            stage,
            panel_height,
            slots,
            audio: RefCell::default(),
            osd,
            resized,
            state: RefCell::default(),
        });
        for kind in KINDS {
            this.connect_slot(kind);
        }
        let weak = Rc::downgrade(&this);
        let audio = Audio::new(Box::new(move |volume| with(&weak, |s| s.show_volume(volume))));
        *this.audio.borrow_mut() = Some(audio);
        this.watch(NM, NM_PATH, NM, |s, proxy| {
            s.state.borrow_mut().network = Some(proxy);
        });
        this.watch(UPOWER, UPOWER_DISPLAY, UPOWER_DEVICE, |s, proxy| {
            s.state.borrow_mut().battery = Some(proxy);
        });
        this
    }

    pub fn destroy(&self) {
        self.audio.borrow_mut().take();
        let state = std::mem::take(&mut *self.state.borrow_mut());
        drop(state);
        for slot in &self.slots {
            unsafe { ffi::clutter_actor_destroy(slot.button) };
        }
    }

    /// Place the icons so that they end `right_edge` px from the panel's left.
    pub fn place(&self, right_edge: f32) {
        self.state.borrow_mut().right_edge = right_edge;
        self.layout();
    }

    /// How much of the panel the visible icons take.
    pub fn width(&self) -> f32 {
        self.state.borrow().width
    }

    // ------------------------------------------------------------------ input

    fn connect_slot(self: &Rc<Self>, kind: Kind) {
        let button_ptr = self.slots[kind as usize].button;
        let button = unsafe { gobj(button_ptr) };
        for (signal, color) in [("enter-event", HOVER_COLOR), ("leave-event", ui::TRANSPARENT)] {
            button.connect_local(signal, false, move |_| {
                unsafe { ffi::clutter_actor_set_background_color(button_ptr, &color) };
                Some(false.to_value())
            });
        }
        let weak = Rc::downgrade(self);
        button.connect_local("button-press-event", false, move |args| {
            let event = ui::event_arg(args);
            let pressed = if event.is_null() { 0 } else { unsafe { ffi::clutter_event_get_button(event) } };
            with(&weak, |s| match (kind, pressed) {
                (_, BUTTON_PRIMARY) => s.open_settings(kind),
                (Kind::Volume, BUTTON_MIDDLE) => s.adjust(|audio| audio.toggle_mute()),
                _ => {}
            });
            Some(true.to_value())
        });
        if matches!(kind, Kind::Volume) {
            let weak = Rc::downgrade(self);
            button.connect_local("scroll-event", false, move |args| {
                with(&weak, |s| {
                    let rows = ui::scroll_rows(ui::event_arg(args), &mut s.state.borrow_mut().scroll);
                    if rows != 0 {
                        // Scrolling up is negative rows and louder.
                        s.adjust(|audio| audio.change_volume(f64::from(-rows) * VOLUME_STEP));
                    }
                });
                Some(true.to_value())
            });
        }
    }

    /// Change the volume and show the result.
    fn adjust(&self, change: impl FnOnce(&Audio)) {
        let Some(audio) = self.audio.borrow().clone() else { return };
        change(&audio);
        let Some(volume) = audio.volume() else { return };
        self.show_volume(Some(volume));
        self.osd.show(&OsdRequest {
            icon: Some(gio::ThemedIcon::new(volume_icon(volume)).upcast()),
            label: None,
            level: Some(if volume.muted { 0.0 } else { volume.level }),
            max_level: volume.level.max(1.0),
        });
    }

    fn open_settings(&self, kind: Kind) {
        let command = format!("gnome-control-center {}", kind.settings_page());
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

    // ------------------------------------------------------------------ D-Bus

    /// Make a proxy on the system bus; `store` keeps it, and its property
    /// changes (or the service coming and going) refresh the icons.
    fn watch(
        self: &Rc<Self>,
        name: &'static str,
        path: &str,
        interface: &'static str,
        store: impl FnOnce(&StatusIcons, gio::DBusProxy) + 'static,
    ) {
        let weak = Rc::downgrade(self);
        gio::DBusProxy::for_bus(
            gio::BusType::System,
            gio::DBusProxyFlags::DO_NOT_AUTO_START,
            None,
            name,
            path,
            interface,
            gio::Cancellable::NONE,
            move |proxy| {
                let Some(this) = weak.upgrade() else { return };
                let proxy = match proxy {
                    Ok(proxy) => proxy,
                    Err(e) => {
                        glib::g_debug!("gnutshell", "status: no {name}: {e}");
                        return;
                    }
                };
                let weak = Rc::downgrade(&this);
                proxy.connect_local("g-properties-changed", false, move |_| {
                    with(&weak, |s| s.refresh());
                    None
                });
                let weak = Rc::downgrade(&this);
                proxy.connect_notify_local(Some("g-name-owner"), move |_, _| with(&weak, |s| s.refresh()));
                store(&this, proxy);
                this.refresh();
            },
        );
    }

    fn refresh(self: &Rc<Self>) {
        self.follow_connection();
        let (network, battery) = {
            let state = self.state.borrow();
            (network_look(&state), battery_look(state.battery.as_ref()))
        };
        self.set_look(Kind::Network, network);
        self.set_look(Kind::Battery, battery);
    }

    /// Keep proxies on the primary connection and, for Wi-Fi, on its access
    /// point, whose signal strength picks the icon.
    fn follow_connection(self: &Rc<Self>) {
        let (active_path, access_point_path) = {
            let state = self.state.borrow();
            let active_path = state
                .network
                .as_ref()
                .and_then(|p| object_path(p, "PrimaryConnection"))
                .unwrap_or_default();
            let wifi = state.network.as_ref().and_then(|p| property::<String>(p, "PrimaryConnectionType"))
                == Some("802-11-wireless".into());
            let access_point = state
                .active
                .as_ref()
                .filter(|_| wifi && state.active_path == active_path)
                .and_then(|p| object_path(p, "SpecificObject"))
                .unwrap_or_default();
            (active_path, access_point)
        };
        // Update the remembered path before asking, so one request goes out.
        let new_active = {
            let mut state = self.state.borrow_mut();
            let changed = state.active_path != active_path;
            if changed {
                state.active_path = active_path.clone();
                state.active = None;
            }
            changed && !active_path.is_empty()
        };
        if new_active {
            let want = active_path.clone();
            self.watch(NM, &active_path, NM_ACTIVE, move |s, proxy| {
                let mut state = s.state.borrow_mut();
                if state.active_path == want {
                    state.active = Some(proxy);
                }
            });
        }
        let new_access_point = {
            let mut state = self.state.borrow_mut();
            let changed = state.access_point_path != access_point_path;
            if changed {
                state.access_point_path = access_point_path.clone();
                state.access_point = None;
            }
            changed && !access_point_path.is_empty()
        };
        if new_access_point {
            let want = access_point_path.clone();
            self.watch(NM, &access_point_path, NM_ACCESS_POINT, move |s, proxy| {
                let mut state = s.state.borrow_mut();
                if state.access_point_path == want {
                    state.access_point = Some(proxy);
                }
            });
        }
    }

    // ------------------------------------------------------------------ display

    fn show_volume(&self, volume: Option<Volume>) {
        let look = volume.map_or_else(Look::default, |v| Look {
            icon: Some(volume_icon(v).into()),
            text: String::new(),
        });
        self.set_look(Kind::Volume, look);
    }

    fn set_look(&self, kind: Kind, look: Look) {
        if self.state.borrow().looks[kind as usize] == look {
            return;
        }
        self.state.borrow_mut().looks[kind as usize] = look;
        let before = self.width();
        self.layout();
        if self.width() != before {
            (self.resized)();
        }
    }

    fn layout(&self) {
        let scale = ui::primary_monitor_scale(self.display);
        let (looks, right_edge) = {
            let state = self.state.borrow();
            (state.looks.clone(), state.right_edge)
        };
        let widths: Vec<f32> = self
            .slots
            .iter()
            .zip(&looks)
            .map(|(slot, look)| unsafe {
                let Some(name) = &look.icon else {
                    ffi::clutter_actor_hide(slot.button);
                    return 0.0;
                };
                let content = icons::symbolic(name, &ui::TEXT_COLOR, ICON_SIZE, scale, self.stage);
                let content = content.as_ref().map_or(std::ptr::null_mut(), |c| c.as_ptr() as *mut ffi::ClutterContent);
                ffi::clutter_actor_set_content(slot.icon, content);
                ui::set_text(slot.text, &look.text);
                let mut width = ICON_SIZE + 2.0 * SLOT_PADDING;
                if !look.text.is_empty() {
                    let text_width = ffi::clutter_actor_get_width(slot.text);
                    let y = (self.panel_height - ffi::clutter_actor_get_height(slot.text)) / 2.0;
                    ffi::clutter_actor_set_position(slot.text, width - 2.0, y.floor());
                    width += text_width.ceil() + 4.0;
                }
                ffi::clutter_actor_set_size(slot.button, width, self.panel_height);
                ffi::clutter_actor_show(slot.button);
                width
            })
            .collect();
        let total: f32 = widths.iter().sum();
        let mut x = right_edge - total;
        for (slot, width) in self.slots.iter().zip(&widths) {
            unsafe { ffi::clutter_actor_set_position(slot.button, x, 0.0) };
            x += width;
        }
        self.state.borrow_mut().width = total;
    }
}

fn property<T: glib::variant::FromVariant>(proxy: &gio::DBusProxy, name: &str) -> Option<T> {
    proxy.cached_property(name).and_then(|v| v.get::<T>())
}

/// An object path property; `None` for the "no object" path `/`.
fn object_path(proxy: &gio::DBusProxy, name: &str) -> Option<String> {
    let path = proxy.cached_property(name)?.str()?.to_owned();
    (path != "/").then_some(path)
}

fn network_look(state: &State) -> Look {
    let Some(manager) = state.network.as_ref().filter(|p| p.name_owner().is_some()) else {
        return Look::default();
    };
    let icon = network_icon(
        property(manager, "State").unwrap_or(0),
        property(manager, "Connectivity").unwrap_or(0),
        &property::<String>(manager, "PrimaryConnectionType").unwrap_or_default(),
        state.access_point.as_ref().and_then(|p| property::<u8>(p, "Strength")),
    );
    Look { icon: Some(icon.into()), text: String::new() }
}

/// The icon for NetworkManager's `State`, `Connectivity`, primary connection
/// type and, on Wi-Fi, signal strength in percent.
fn network_icon(state: u32, connectivity: u32, kind: &str, strength: Option<u8>) -> &'static str {
    const CONNECTING: u32 = 40;
    const CONNECTED_LOCAL: u32 = 50;
    const PORTAL: u32 = 2;
    const LIMITED: u32 = 3;
    let wireless = kind == "802-11-wireless";
    if state < CONNECTING {
        return "network-offline-symbolic";
    }
    if state < CONNECTED_LOCAL {
        return "network-wired-acquiring-symbolic";
    }
    if connectivity == PORTAL || connectivity == LIMITED {
        return if wireless { "network-wireless-no-route-symbolic" } else { "network-wired-no-route-symbolic" };
    }
    match kind {
        "802-11-wireless" => match strength.unwrap_or(100) {
            80.. => "network-wireless-signal-excellent-symbolic",
            55.. => "network-wireless-signal-good-symbolic",
            30.. => "network-wireless-signal-ok-symbolic",
            5.. => "network-wireless-signal-weak-symbolic",
            _ => "network-wireless-signal-none-symbolic",
        },
        "vpn" | "wireguard" | "tun" => "network-vpn-symbolic",
        _ => "network-wired-symbolic",
    }
}

fn volume_icon(v: Volume) -> &'static str {
    if v.muted || v.level <= 0.0 {
        "audio-volume-muted-symbolic"
    } else if v.level > 1.005 {
        "audio-volume-overamplified-symbolic"
    } else if v.level > 0.66 {
        "audio-volume-high-symbolic"
    } else if v.level > 0.33 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-low-symbolic"
    }
}

fn battery_look(proxy: Option<&gio::DBusProxy>) -> Look {
    let Some(proxy) = proxy.filter(|p| p.name_owner().is_some()) else { return Look::default() };
    if property::<bool>(proxy, "IsPresent") != Some(true) {
        return Look::default();
    }
    let percent = property::<f64>(proxy, "Percentage").unwrap_or(0.0);
    Look {
        icon: Some(property::<String>(proxy, "IconName").unwrap_or_else(|| "battery-missing-symbolic".into())),
        text: format!("{}%", percent.round()),
    }
}

fn with(weak: &Weak<StatusIcons>, f: impl FnOnce(&Rc<StatusIcons>)) {
    if let Some(s) = weak.upgrade() {
        f(&s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_icons() {
        assert_eq!(network_icon(20, 1, "", None), "network-offline-symbolic");
        assert_eq!(network_icon(40, 1, "802-3-ethernet", None), "network-wired-acquiring-symbolic");
        assert_eq!(network_icon(70, 4, "802-3-ethernet", None), "network-wired-symbolic");
        assert_eq!(network_icon(60, 3, "802-3-ethernet", None), "network-wired-no-route-symbolic");
        assert_eq!(network_icon(70, 4, "802-11-wireless", Some(60)), "network-wireless-signal-good-symbolic");
        assert_eq!(network_icon(70, 4, "802-11-wireless", Some(0)), "network-wireless-signal-none-symbolic");
        assert_eq!(network_icon(70, 4, "wireguard", None), "network-vpn-symbolic");
    }

    #[test]
    fn volume_icons() {
        let icon = |level, muted| volume_icon(Volume { level, muted });
        assert_eq!(icon(0.7, true), "audio-volume-muted-symbolic");
        assert_eq!(icon(0.0, false), "audio-volume-muted-symbolic");
        assert_eq!(icon(0.2, false), "audio-volume-low-symbolic");
        assert_eq!(icon(1.0, false), "audio-volume-high-symbolic");
        assert_eq!(icon(1.2, false), "audio-volume-overamplified-symbolic");
    }
}
