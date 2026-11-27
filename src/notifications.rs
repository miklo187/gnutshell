//! Desktop notifications: the `org.freedesktop.Notifications` service and the
//! banner that shows them below the panel.
//!
//! One banner shows at a time; the rest wait in a queue. A notification is
//! closed when its banner goes away; if the user didn't act on it, it stays
//! in the notification list (`list.rs`). Critical notifications stay until
//! the user dismisses them.
//!
//! Nothing shows on the lock screen: notifications wait until unlock. While
//! the primary monitor shows a fullscreen window, only critical ones show.
//! With `org.gnome.desktop.notifications show-banners` off (Do Not Disturb),
//! non-critical notifications are closed at once and only listed. Per-app
//! settings and sounds are in `policy.rs`.

mod calendar;
mod list;
mod policy;

use crate::ffi::{self, ClutterActor, ClutterText, MetaDisplay};
use crate::icons;
use crate::ui::{self, gobj};
use gio::prelude::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::CStr;
use std::rc::{Rc, Weak};
use std::time::Duration;

const BUS_NAME: &str = "org.freedesktop.Notifications";
const OBJECT_PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";

const INTERFACE_XML: &str = r#"
<node>
  <interface name="org.freedesktop.Notifications">
    <method name="GetCapabilities">
      <arg type="as" direction="out" name="capabilities"/>
    </method>
    <method name="Notify">
      <arg type="s" direction="in" name="app_name"/>
      <arg type="u" direction="in" name="replaces_id"/>
      <arg type="s" direction="in" name="app_icon"/>
      <arg type="s" direction="in" name="summary"/>
      <arg type="s" direction="in" name="body"/>
      <arg type="as" direction="in" name="actions"/>
      <arg type="a{sv}" direction="in" name="hints"/>
      <arg type="i" direction="in" name="expire_timeout"/>
      <arg type="u" direction="out" name="id"/>
    </method>
    <method name="CloseNotification">
      <arg type="u" direction="in" name="id"/>
    </method>
    <method name="GetServerInformation">
      <arg type="s" direction="out" name="name"/>
      <arg type="s" direction="out" name="vendor"/>
      <arg type="s" direction="out" name="version"/>
      <arg type="s" direction="out" name="spec_version"/>
    </method>
    <signal name="NotificationClosed">
      <arg type="u" name="id"/>
      <arg type="u" name="reason"/>
    </signal>
    <signal name="ActionInvoked">
      <arg type="u" name="id"/>
      <arg type="s" name="action_key"/>
    </signal>
    <signal name="ActivationToken">
      <arg type="u" name="id"/>
      <arg type="s" name="activation_token"/>
    </signal>
  </interface>
</node>
"#;

const WIDTH: f32 = 420.0;
const PADDING: f32 = 14.0;
const SPACING: f32 = 8.0;
const ICON_SIZE: f32 = 40.0;
const CLOSE_SIZE: f32 = 24.0;
const COPY_ICON_SIZE: f32 = 14.0;
const BUTTON_HEIGHT: f32 = 32.0;
const MAX_BUTTONS: usize = 3;
const MAX_BODY_LINES: f32 = 3.0;
const FADE_MS: u32 = 200;
const SUMMARY_FONT: &CStr = c"Sans Bold 11";
const APP_FONT: &CStr = c"Sans 9";

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait again when the timeout hits while the pointer is over
/// the banner.
const HOVER_GRACE: Duration = Duration::from_secs(1);
/// Cap on waiting notifications, so a chatty app can't grow memory.
const MAX_QUEUE: usize = 20;
/// Cap on text taken from a client.
const MAX_TEXT: usize = 1000;
/// Largest `image-data` side accepted.
const MAX_IMAGE_SIDE: i32 = 1024;

/// `NotificationClosed` reasons from the specification.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reason {
    Expired = 1,
    Dismissed = 2,
    Closed = 3,
}

/// Pixels from the `image-data` hint.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: i32,
    pub height: i32,
    pub rowstride: i32,
    pub channels: i32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct Notification {
    pub id: u32,
    /// Unique bus name of the client; signals go only to it.
    pub sender: String,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    /// `app_icon` argument: a theme name, path or `file://` URI.
    pub app_icon: String,
    pub image_path: String,
    pub image: Option<Image>,
    pub desktop_entry: String,
    /// (key, label), without the "default" action.
    pub buttons: Vec<(String, String)>,
    pub has_default: bool,
    pub critical: bool,
    /// Stays after an action is invoked.
    pub resident: bool,
    /// Never kept in the notification list.
    pub transient: bool,
    pub expire_timeout: i32,
    /// The `sound-name`, `sound-file` and `suppress-sound` hints.
    pub sound_name: String,
    pub sound_file: String,
    pub suppress_sound: bool,
    /// Local time it arrived, as shown in the list.
    pub received: String,
    /// Counts arrivals, updates included. The list is sorted by it, because
    /// notifications do not reach the list in the order they came.
    pub arrival: u64,
}

impl Notification {
    /// What the copy button puts on the clipboard: the title and the whole
    /// body, including lines the banner cuts off.
    fn copy_text(&self) -> String {
        [self.summary.as_str(), self.body.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// How long the banner stays; `None` means until dismissed.
    fn timeout(&self) -> Option<Duration> {
        if self.critical {
            return None;
        }
        Some(match u64::try_from(self.expire_timeout) {
            Ok(ms) if ms > 0 => Duration::from_millis(ms).clamp(Duration::from_secs(1), MAX_TIMEOUT),
            _ => DEFAULT_TIMEOUT, // -1: server decides; 0 ("never") is for critical only
        })
    }
}

/// Notifications waiting for the banner.
#[derive(Default)]
pub struct Queue {
    waiting: VecDeque<Notification>,
}

impl Queue {
    /// Add `n`, or update the waiting one with the same id in place.
    /// Returns the one dropped if the queue overflowed.
    fn push(&mut self, n: Notification) -> Option<Notification> {
        if let Some(old) = self.waiting.iter_mut().find(|w| w.id == n.id) {
            *old = n;
            return None;
        }
        self.waiting.push_back(n);
        if self.waiting.len() <= MAX_QUEUE {
            return None;
        }
        // Drop the oldest, keeping critical ones as long as possible.
        let i = self.waiting.iter().position(|w| !w.critical).unwrap_or(0);
        self.waiting.remove(i)
    }

    /// Next one to show; in fullscreen only critical ones qualify.
    fn next(&mut self, critical_only: bool) -> Option<Notification> {
        let i = self.waiting.iter().position(|w| w.critical || !critical_only)?;
        self.waiting.remove(i)
    }

    fn remove(&mut self, id: u32) -> Option<Notification> {
        let i = self.waiting.iter().position(|w| w.id == id)?;
        self.waiting.remove(i)
    }
}

pub struct Notifications {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    panel_height: f32,
    root: *mut ClutterActor,
    view: list::ListView,
    settings: Option<gio::Settings>,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    queue: Queue,
    list: list::ListState,
    current: Option<Notification>,
    timer: Option<glib::SourceId>,
    hovered: bool,
    locked: bool,
    last_id: u32,
    /// How many notifications have arrived.
    arrivals: u64,
    connection: Option<gio::DBusConnection>,
    registration: Option<gio::RegistrationId>,
    owner: Option<gio::OwnerId>,
}

impl Notifications {
    pub fn new(
        display: *mut MetaDisplay,
        stage: *mut ClutterActor,
        panel: *mut ClutterActor,
        clock: *mut ClutterActor,
        panel_height: f32,
    ) -> Rc<Self> {
        let root = unsafe {
            let root = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(root, &ui::BOX_COLOR);
            ffi::clutter_actor_set_reactive(root, 1);
            ffi::clutter_actor_hide(root);
            ffi::clutter_actor_add_child(stage, root);
            root
        };
        let settings = gio::SettingsSchemaSource::default()
            .and_then(|s| s.lookup("org.gnome.desktop.notifications", true))
            .map(|_| gio::Settings::new("org.gnome.desktop.notifications"));
        let view = list::ListView::new(stage, panel, clock);
        let this = Rc::new(Self { display, stage, panel_height, root, view, settings, state: RefCell::default() });
        this.connect_banner();
        this.connect_list();
        this.own_name();
        this
    }

    pub fn set_locked(self: &Rc<Self>, locked: bool) {
        if locked {
            self.close_list();
        }
        self.state.borrow_mut().locked = locked;
        self.sync();
    }

    /// Re-check what may show after the lock state, fullscreen state or
    /// monitor layout changed.
    pub fn sync(self: &Rc<Self>) {
        let (locked, current) = {
            let state = self.state.borrow();
            (state.locked, state.current.as_ref().map(|n| n.critical))
        };
        let blocked = current.is_some_and(|critical| locked || (!critical && self.fullscreen()));
        if blocked {
            // Put it back; it shows again once allowed.
            let n = self.take_current();
            if let Some(n) = n {
                self.state.borrow_mut().queue.waiting.push_front(n);
            }
        } else if current.is_some() {
            self.place();
        }
        // The monitor may have changed under an open list.
        if self.state.borrow().list.open {
            self.build_list();
        }
        self.place_clock();
        self.show_next();
    }

    pub fn destroy(&self) {
        let (timer, connection, registration, owner) = {
            let mut state = self.state.borrow_mut();
            state.current = None;
            (state.timer.take(), state.connection.take(), state.registration.take(), state.owner.take())
        };
        if let Some(id) = timer {
            id.remove();
        }
        if let (Some(connection), Some(id)) = (connection, registration) {
            let _ = connection.unregister_object(id);
        }
        if let Some(owner) = owner {
            gio::bus_unown_name(owner);
        }
        self.destroy_list();
        unsafe { ffi::clutter_actor_destroy(self.root) };
    }

    // ------------------------------------------------------------------ D-Bus

    fn own_name(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let owner = gio::bus_own_name(
            gio::BusType::Session,
            BUS_NAME,
            gio::BusNameOwnerFlags::NONE,
            move |connection, _| {
                if let Some(this) = weak.upgrade() {
                    this.export(connection);
                }
            },
            |_, name| glib::g_debug!("gnutshell", "acquired {name}"),
            |_, name| glib::g_warning!("gnutshell", "could not own {name} (is another notification daemon running?)"),
        );
        self.state.borrow_mut().owner = Some(owner);
    }

    fn export(self: &Rc<Self>, connection: gio::DBusConnection) {
        let info = gio::DBusNodeInfo::for_xml(INTERFACE_XML)
            .ok()
            .and_then(|node| node.lookup_interface(INTERFACE))
            .expect("valid org.freedesktop.Notifications interface XML");
        let weak = Rc::downgrade(self);
        let registration = connection
            .register_object(OBJECT_PATH, &info)
            .method_call(move |_, sender, _, _, method, params, invocation| match weak.upgrade() {
                Some(this) => this.on_method_call(sender.unwrap_or_default(), method, &params, invocation),
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
        sender: &str,
        method: &str,
        params: &glib::Variant,
        invocation: gio::DBusMethodInvocation,
    ) {
        match method {
            "GetCapabilities" => {
                let caps = vec!["actions", "body", "icon-static", "sound"];
                invocation.return_value(Some(&(caps,).to_variant()));
            }
            "GetServerInformation" => {
                let info = ("gnutshell", "gnutshell", env!("CARGO_PKG_VERSION"), "1.2");
                invocation.return_value(Some(&info.to_variant()));
            }
            "Notify" => {
                let Some((replaces_id, mut n)) = parse_notify(params) else {
                    invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", "bad Notify arguments");
                    return;
                };
                n.sender = sender.to_owned();
                n.id = self.assign_id(replaces_id);
                // Reply first, so the client knows the id before any signal.
                invocation.return_value(Some(&(n.id,).to_variant()));
                self.add(n);
            }
            "CloseNotification" => {
                let (id,) = params.get::<(u32,)>().unwrap_or_default();
                invocation.return_value(None);
                self.close(id, Reason::Closed);
            }
            _ => invocation.return_dbus_error(
                "org.freedesktop.DBus.Error.UnknownMethod",
                &format!("{method} is not implemented by gnutshell"),
            ),
        }
    }

    /// A notification from the shell itself. `icon` is a theme name or a
    /// picture's path. With no client behind it, no signals are sent.
    pub fn notify_shell(self: &Rc<Self>, summary: &str, body: &str, icon: &str) {
        let n = Notification {
            id: self.assign_id(0),
            app_name: "gnutshell".into(),
            summary: clip(summary),
            body: clip(body),
            app_icon: icon.into(),
            expire_timeout: -1,
            ..Default::default()
        };
        self.add(n);
    }

    /// `replaces_id` if that notification is still around, else a fresh id.
    fn assign_id(&self, replaces_id: u32) -> u32 {
        let mut state = self.state.borrow_mut();
        let alive = replaces_id != 0
            && (state.current.as_ref().is_some_and(|n| n.id == replaces_id)
                || state.queue.waiting.iter().any(|n| n.id == replaces_id)
                || state.list.history.contains(replaces_id));
        if alive {
            return replaces_id;
        }
        state.last_id = state.last_id.checked_add(1).unwrap_or(1);
        state.last_id
    }

    fn emit(&self, destination: &str, signal: &str, body: &glib::Variant) {
        // An empty destination is one of the shell's own notifications.
        if destination.is_empty() {
            return;
        }
        let Some(connection) = self.state.borrow().connection.clone() else { return };
        if let Err(e) = connection.emit_signal(Some(destination), OBJECT_PATH, INTERFACE, signal, Some(body)) {
            glib::g_debug!("gnutshell", "could not emit {signal}: {e}");
        }
    }

    fn emit_closed(&self, n: &Notification, reason: Reason) {
        glib::g_debug!("gnutshell", "notification {} closed: {reason:?}", n.id);
        self.emit(&n.sender, "NotificationClosed", &(n.id, reason as u32).to_variant());
    }

    // ------------------------------------------------------------------ queue

    fn add(self: &Rc<Self>, mut n: Notification) {
        n.arrival = {
            let mut state = self.state.borrow_mut();
            state.arrivals += 1;
            state.arrivals
        };
        n.received = glib::DateTime::now_local()
            .and_then(|t| t.format("%H:%M"))
            .map(|s| s.to_string())
            .unwrap_or_default();
        // An update to a listed one supersedes that entry.
        self.forget(n.id);
        glib::g_debug!("gnutshell", "notification {} from {:?}", n.id, n.app_name);
        let banners = self.settings.as_ref().is_none_or(|s| s.boolean("show-banners"));
        let verdict = policy::decide(banners, policy::app_settings(&n), n.critical);
        if verdict.sound {
            policy::play_sound(self.display, &n);
        }
        if verdict.delivery != policy::Delivery::Banner {
            // It may replace one that is still around: drop that silently,
            // so the id is reported closed only once.
            if self.state.borrow().current.as_ref().is_some_and(|c| c.id == n.id) {
                self.take_current();
            }
            self.state.borrow_mut().queue.remove(n.id);
            if verdict.delivery == policy::Delivery::Drop {
                self.emit_closed(&n, Reason::Expired);
            } else {
                self.retire(n);
            }
            self.show_next();
            return;
        }
        // With the list open, new ones appear there instead of as banners.
        if self.state.borrow().list.open {
            self.retire(n);
            return;
        }
        let replaces_current = self.state.borrow().current.as_ref().is_some_and(|c| c.id == n.id);
        if replaces_current {
            // Update in place, with a fresh timeout.
            self.state.borrow_mut().current = Some(n);
            self.build();
            self.arm_timer();
            self.sync();
            return;
        }
        let dropped = self.state.borrow_mut().queue.push(n);
        if let Some(dropped) = dropped {
            self.retire(dropped);
        }
        self.show_next();
    }

    fn close(self: &Rc<Self>, id: u32, reason: Reason) {
        // An app withdrawing a notification takes it out of the list too.
        if reason == Reason::Closed {
            self.forget(id);
        }
        let is_current = self.state.borrow().current.as_ref().is_some_and(|c| c.id == id);
        if is_current {
            if let Some(n) = self.take_current() {
                self.finish(n, reason);
            }
            self.show_next();
            return;
        }
        let removed = self.state.borrow_mut().queue.remove(id);
        if let Some(n) = removed {
            self.finish(n, reason);
        }
    }

    fn finish(self: &Rc<Self>, n: Notification, reason: Reason) {
        if reason == Reason::Expired {
            self.retire(n);
        } else {
            self.emit_closed(&n, reason);
        }
    }

    /// Remove the banner and return what it showed.
    fn take_current(&self) -> Option<Notification> {
        let (n, timer) = {
            let mut state = self.state.borrow_mut();
            state.hovered = false;
            (state.current.take(), state.timer.take())
        };
        if let Some(id) = timer {
            id.remove();
        }
        unsafe {
            ffi::clutter_actor_remove_all_transitions(self.root);
            ffi::clutter_actor_hide(self.root);
        }
        n
    }

    fn fullscreen(&self) -> bool {
        unsafe {
            let primary = ffi::meta_display_get_primary_monitor(self.display);
            primary >= 0
                && primary < ffi::meta_display_get_n_monitors(self.display)
                && ffi::meta_display_get_monitor_in_fullscreen(self.display, primary) != 0
        }
    }

    fn show_next(self: &Rc<Self>) {
        {
            let state = self.state.borrow();
            if state.locked || state.list.open || state.current.is_some() {
                return;
            }
        }
        if ui::primary_monitor_geometry(self.display).is_none() {
            return;
        }
        let critical_only = self.fullscreen();
        let next = self.state.borrow_mut().queue.next(critical_only);
        let Some(n) = next else { return };
        self.state.borrow_mut().current = Some(n);
        self.build();
        unsafe {
            ffi::clutter_actor_set_opacity(self.root, 0);
            ffi::clutter_actor_show(self.root);
            ffi::clutter_actor_save_easing_state(self.root);
            ffi::clutter_actor_set_easing_mode(self.root, ffi::CLUTTER_EASE_OUT_QUAD);
            ffi::clutter_actor_set_easing_duration(self.root, FADE_MS);
            ffi::clutter_actor_set_opacity(self.root, 255);
            ffi::clutter_actor_restore_easing_state(self.root);
        }
        self.arm_timer();
    }

    fn arm_timer(self: &Rc<Self>) {
        let old = self.state.borrow_mut().timer.take();
        if let Some(id) = old {
            id.remove();
        }
        let Some(timeout) = self.state.borrow().current.as_ref().and_then(|n| n.timeout()) else { return };
        self.start_timer(timeout);
    }

    fn start_timer(self: &Rc<Self>, after: Duration) {
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(after, move || {
            let Some(this) = weak.upgrade() else { return };
            let (hovered, id) = {
                let mut state = this.state.borrow_mut();
                state.timer = None;
                (state.hovered, state.current.as_ref().map(|n| n.id))
            };
            if hovered {
                this.start_timer(HOVER_GRACE);
            } else if let Some(id) = id {
                this.close(id, Reason::Expired);
            }
        });
        self.state.borrow_mut().timer = Some(id);
    }

    // ------------------------------------------------------------------ banner

    fn connect_banner(self: &Rc<Self>) {
        let root = unsafe { gobj(self.root) };
        // Crossing events of the children bubble up here too: moving between
        // them gives a leave then an enter, so the last one is right.
        for (signal, hovered) in [("enter-event", true), ("leave-event", false)] {
            let weak = Rc::downgrade(self);
            root.connect_local(signal, false, move |_| {
                with(&weak, |n| n.state.borrow_mut().hovered = hovered);
                Some(false.to_value())
            });
        }
        // A click on the banner itself runs the default action, if any.
        let weak = Rc::downgrade(self);
        root.connect_local("button-press-event", false, move |_| {
            with(&weak, |n| {
                let shown = n.state.borrow().current.as_ref().map(|c| (c.id, c.has_default));
                match shown {
                    Some((id, true)) => n.invoke(id, "default"),
                    Some((id, false)) => n.close(id, Reason::Dismissed),
                    None => {}
                }
            });
            Some(true.to_value())
        });
    }

    fn invoke(self: &Rc<Self>, id: u32, key: &str) {
        let target = {
            let state = self.state.borrow();
            state.current.as_ref().filter(|c| c.id == id).map(|c| (c.sender.clone(), c.resident))
        };
        let Some((sender, resident)) = target else { return };
        // The token lets the app raise its window in response.
        let time = unsafe { ffi::clutter_get_current_event_time() };
        if let Some(token) = ui::activation_token(self.display, time) {
            self.emit(&sender, "ActivationToken", &(id, token).to_variant());
        }
        self.emit(&sender, "ActionInvoked", &(id, key).to_variant());
        if !resident {
            self.close(id, Reason::Dismissed);
        }
    }

    /// Fill the banner with the current notification.
    fn build(self: &Rc<Self>) {
        let Some(n) = self.state.borrow().current.clone() else { return };
        let Some(geo) = ui::primary_monitor_geometry(self.display) else { return };
        let scale = ui::primary_monitor_scale(self.display);
        let width = WIDTH.min(geo.width as f32 - 32.0);
        unsafe {
            ffi::clutter_actor_destroy_all_children(self.root);

            let mut x = PADDING;
            if let Some(content) = icon_for(&n, scale, self.stage) {
                let icon = ui::icon_actor(&content, ICON_SIZE);
                ffi::clutter_actor_add_child(self.root, icon);
                ffi::clutter_actor_set_position(icon, x, PADDING);
                x += ICON_SIZE + 12.0;
            }
            // Room for the copy and close buttons.
            let text_width = width - x - PADDING - 2.0 * (CLOSE_SIZE + SPACING);

            let lines = [
                (APP_FONT, n.app_name.as_str(), &ui::HINT_COLOR, 1.0),
                (SUMMARY_FONT, n.summary.as_str(), &ui::TEXT_COLOR, 1.0),
                (ui::BODY_FONT, n.body.as_str(), &ui::TEXT_COLOR, MAX_BODY_LINES),
            ];
            let mut y = add_lines(self.root, &lines, x, PADDING - 2.0, text_width);
            y = y.max(PADDING + ICON_SIZE) + PADDING - 2.0;

            let close = ui::button("✕", CLOSE_SIZE, CLOSE_SIZE, &ui::BUTTON_COLOR);
            ffi::clutter_actor_add_child(self.root, close);
            ffi::clutter_actor_set_position(close, width - CLOSE_SIZE - SPACING, SPACING);
            let weak = Rc::downgrade(self);
            let id = n.id;
            gobj(close).connect_local("button-press-event", false, move |_| {
                with(&weak, |s| s.close(id, Reason::Dismissed));
                Some(true.to_value())
            });

            self.add_copy_button(self.root, n.copy_text(), width - 2.0 * (CLOSE_SIZE + SPACING), scale);

            let count = n.buttons.len().min(MAX_BUTTONS);
            if count > 0 {
                let w = ((width - 2.0 * PADDING - (count - 1) as f32 * SPACING) / count as f32).floor();
                for (i, (key, text)) in n.buttons.iter().take(count).enumerate() {
                    let button = ui::button(text, w, BUTTON_HEIGHT, &ui::BUTTON_COLOR);
                    ffi::clutter_actor_add_child(self.root, button);
                    ffi::clutter_actor_set_position(button, PADDING + i as f32 * (w + SPACING), y);
                    let weak = Rc::downgrade(self);
                    let key = key.clone();
                    gobj(button).connect_local("button-press-event", false, move |_| {
                        with(&weak, |s| s.invoke(id, &key));
                        Some(true.to_value())
                    });
                }
                y += BUTTON_HEIGHT + PADDING;
            }

            ffi::clutter_actor_set_size(self.root, width, y);
        }
        self.place();
    }

    /// A copy button at `x` along the top of `parent`, which must already be
    /// parented. Nothing is added if there is no text.
    fn add_copy_button(self: &Rc<Self>, parent: *mut ClutterActor, text: String, x: f32, scale: f32) {
        if text.is_empty() {
            return;
        }
        unsafe {
            let copy = ui::button("", CLOSE_SIZE, CLOSE_SIZE, &ui::BUTTON_COLOR);
            ffi::clutter_actor_add_child(parent, copy);
            ffi::clutter_actor_set_position(copy, x, SPACING);
            self.set_button_icon(copy, "edit-copy-symbolic", "⧉", scale);
            let weak = Rc::downgrade(self);
            // Stays open; the check mark confirms the copy.
            gobj(copy).connect_local("button-press-event", false, move |args| {
                let button = args[0].get::<glib::Object>().ok();
                with(&weak, |s| {
                    if let Some(button) = &button
                        && ui::set_clipboard(s.display, &text)
                    {
                        let button = button.as_ptr() as *mut ClutterActor;
                        s.set_button_icon(button, "object-select-symbolic", "✓", scale);
                    }
                });
                Some(true.to_value())
            });
        }
    }

    /// Replace what is in a square header button with a symbolic icon, or
    /// with `fallback` text if the icon theme lacks it.
    fn set_button_icon(&self, button: *mut ClutterActor, name: &str, fallback: &str, scale: f32) {
        unsafe {
            ffi::clutter_actor_destroy_all_children(button);
            let icon = icons::symbolic(name, &ui::TEXT_COLOR, COPY_ICON_SIZE, scale, self.stage);
            let child = match &icon {
                Some(content) => ui::icon_actor(content, COPY_ICON_SIZE),
                None => ui::label(ui::BODY_FONT, fallback, &ui::TEXT_COLOR),
            };
            ffi::clutter_actor_add_child(button, child);
            let w = ffi::clutter_actor_get_width(child);
            let h = ffi::clutter_actor_get_height(child);
            ffi::clutter_actor_set_position(
                child,
                ((CLOSE_SIZE - w) / 2.0).floor(),
                ((CLOSE_SIZE - h) / 2.0).floor(),
            );
        }
    }

    /// Top center of the primary monitor, below the panel, above everything.
    fn place(&self) {
        let Some(geo) = ui::primary_monitor_geometry(self.display) else { return };
        unsafe {
            let width = ffi::clutter_actor_get_width(self.root);
            ffi::clutter_actor_set_position(
                self.root,
                (geo.x as f32 + (geo.width as f32 - width) / 2.0).floor(),
                geo.y as f32 + self.panel_height + SPACING,
            );
            ffi::clutter_actor_set_child_above_sibling(self.stage, self.root, std::ptr::null_mut());
        }
    }
}

fn with(weak: &Weak<Notifications>, f: impl FnOnce(&Rc<Notifications>)) {
    if let Some(n) = weak.upgrade() {
        f(&n);
    }
}

/// Stack the non-empty `(font, text, color, max lines)` labels in `parent`
/// from (`x`, `y`). Returns the y below the last one.
unsafe fn add_lines(
    parent: *mut ClutterActor,
    lines: &[(&CStr, &str, &ffi::CoglColor, f32)],
    x: f32,
    mut y: f32,
    width: f32,
) -> f32 {
    for &(font, text, color, max_lines) in lines {
        if text.is_empty() {
            continue;
        }
        unsafe {
            // The height of a one-line label caps the wrapped one.
            let probe = ui::label(font, "Ag", color);
            ffi::clutter_actor_add_child(parent, probe);
            let line = ffi::clutter_actor_get_height(probe);
            ffi::clutter_actor_destroy(probe);

            let label = ui::label(font, text, color);
            let ct = label as *mut ClutterText;
            ffi::clutter_text_set_ellipsize(ct, ffi::PANGO_ELLIPSIZE_END);
            if max_lines > 1.0 {
                ffi::clutter_text_set_line_wrap(ct, 1);
            } else {
                ffi::clutter_text_set_single_line_mode(ct, 1);
            }
            ffi::clutter_actor_add_child(parent, label);
            ffi::clutter_actor_set_width(label, width);
            let height = ffi::clutter_actor_get_height(label).min(line * max_lines);
            ffi::clutter_actor_set_size(label, width, height);
            ffi::clutter_actor_set_position(label, x, y);
            y += height + 2.0;
        }
    }
    y
}

/// The notification's picture, in the specification's priority order, then
/// the sending app's icon.
fn icon_for(n: &Notification, scale: f32, stage: *mut ClutterActor) -> Option<glib::Object> {
    if let Some(content) = n.image.as_ref().and_then(|i| {
        let pixels = to_bgra_premultiplied(i)?;
        icons::from_pixels(&pixels, i.width, i.height, stage)
    }) {
        return Some(content);
    }
    let icon: gio::Icon = [&n.image_path, &n.app_icon]
        .into_iter()
        .find_map(|s| icon_from_string(s))
        .or_else(|| {
            let id = format!("{}.desktop", n.desktop_entry.trim_end_matches(".desktop"));
            gio_unix::DesktopAppInfo::new(&id)?.icon()
        })?;
    icons::content(Some(&icon), ICON_SIZE, scale, stage)
}

fn icon_from_string(s: &str) -> Option<gio::Icon> {
    if s.is_empty() {
        None
    } else if s.starts_with('/') {
        Some(gio::FileIcon::new(&gio::File::for_path(s)).upcast())
    } else if s.starts_with("file://") {
        Some(gio::FileIcon::new(&gio::File::for_uri(s)).upcast())
    } else {
        Some(gio::ThemedIcon::new(s).upcast())
    }
}

/// Convert `image-data` pixels (RGB or straight-alpha RGBA) to the tightly
/// packed premultiplied BGRA that textures take. `None` if inconsistent.
fn to_bgra_premultiplied(image: &Image) -> Option<Vec<u8>> {
    let Image { width, height, rowstride, channels, ref data } = *image;
    let sane = (1..=MAX_IMAGE_SIDE).contains(&width)
        && (1..=MAX_IMAGE_SIDE).contains(&height)
        && (channels == 3 || channels == 4)
        && rowstride >= width * channels;
    if !sane {
        return None;
    }
    let (w, h) = (width as usize, height as usize);
    let (stride, ch) = (rowstride as usize, channels as usize);
    // The last row may stop at its last pixel, without padding.
    if data.len() < stride.checked_mul(h - 1)? + w * ch {
        return None;
    }
    let mut out = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        for p in data[row * stride..][..w * ch].chunks_exact(ch) {
            let a = if ch == 4 { p[3] } else { 255 };
            let mul = |c: u8| ((c as u16 * a as u16 + 127) / 255) as u8;
            out.extend_from_slice(&[mul(p[2]), mul(p[1]), mul(p[0]), a]);
        }
    }
    Some(out)
}

/// Remove the specification's body markup (`<b>`, `<i>`, `<u>`, `<a>`,
/// `<img>`) and decode entities. gnutshell doesn't advertise `body-markup`,
/// but many apps send it anyway. Other `<` characters are kept.
fn plain_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(['<', '&']) {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        if let Some((replacement, len)) = markup_tag(rest).or_else(|| entity(rest)) {
            out.push_str(&replacement);
            rest = &rest[len..];
        } else {
            out.push_str(&rest[..1]);
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

/// A known tag at the start of `s`: its replacement text and byte length.
fn markup_tag(s: &str) -> Option<(String, usize)> {
    let inner = s.strip_prefix('<')?;
    let end = inner.find('>')?;
    let tag = &inner[..end];
    let name = tag.trim_start_matches('/').split([' ', '/']).next()?;
    if !["b", "i", "u", "a", "img"].contains(&name) {
        return None;
    }
    // An image is replaced by its alternative text.
    let alt = (name == "img")
        .then(|| tag.split_once("alt=\"")?.1.split_once('"').map(|(alt, _)| alt.to_owned()))
        .flatten()
        .unwrap_or_default();
    Some((alt, end + 2))
}

fn entity(s: &str) -> Option<(String, usize)> {
    let inner = s.strip_prefix('&')?;
    let end = inner.bytes().take(10).position(|b| b == b';')?;
    let name = &inner[..end];
    let c = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix('x') {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((c.to_string(), end + 2))
}

fn clip(s: &str) -> String {
    s.chars().take(MAX_TEXT).collect()
}

/// `Notify` arguments: `replaces_id` and the notification (no id or sender).
fn parse_notify(params: &glib::Variant) -> Option<(u32, Notification)> {
    if params.type_().as_str() != "(susssasa{sv}i)" {
        return None;
    }
    let string = |i: usize| params.child_value(i).get::<String>().unwrap_or_default();
    let replaces_id = params.child_value(1).get::<u32>()?;
    let actions = params.child_value(5).get::<Vec<String>>()?;
    let hints = glib::VariantDict::new(Some(&params.child_value(6)));
    let hint = |key: &str| hints.lookup_value(key, None);
    let hint_string = |keys: &[&str]| {
        keys.iter().find_map(|k| hint(k)?.get::<String>()).unwrap_or_default()
    };
    let image = ["image-data", "image_data", "icon_data"].iter().find_map(|k| parse_image(&hint(k)?));

    let mut buttons = Vec::new();
    let mut has_default = false;
    for pair in actions.chunks_exact(2) {
        if pair[0] == "default" {
            has_default = true;
        } else if buttons.len() < MAX_BUTTONS {
            buttons.push((pair[0].clone(), clip(&pair[1])));
        }
    }

    let n = Notification {
        app_name: clip(&string(0)),
        app_icon: string(2),
        summary: clip(&string(3)).replace('\n', " "),
        body: plain_text(&clip(&string(4))),
        image,
        image_path: hint_string(&["image-path", "image_path"]),
        desktop_entry: hint_string(&["desktop-entry"]),
        buttons,
        has_default,
        critical: hint("urgency").and_then(|v| v.get::<u8>()) == Some(2),
        resident: hint("resident").and_then(|v| v.get::<bool>()) == Some(true),
        transient: hint("transient").and_then(|v| v.get::<bool>()) == Some(true),
        sound_name: hint_string(&["sound-name"]),
        sound_file: hint_string(&["sound-file"]),
        suppress_sound: hint("suppress-sound").and_then(|v| v.get::<bool>()) == Some(true),
        expire_timeout: params.child_value(7).get::<i32>()?,
        ..Default::default()
    };
    Some((replaces_id, n))
}

/// The `(iiibiiay)` image struct: width, height, rowstride, has alpha,
/// bits per sample, channels, pixels.
fn parse_image(v: &glib::Variant) -> Option<Image> {
    if v.type_().as_str() != "(iiibiiay)" {
        return None;
    }
    let int = |i: usize| v.child_value(i).get::<i32>();
    let (width, height) = (int(0)?, int(1)?);
    if int(4)? != 8 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
        return None;
    }
    let pixels = v.child_value(6);
    let data = pixels.fixed_array::<u8>().ok()?.to_vec();
    Some(Image { width, height, rowstride: int(2)?, channels: int(5)?, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: u32, critical: bool) -> Notification {
        Notification { id, critical, ..Default::default() }
    }

    #[test]
    fn queue_replaces_in_place() {
        let mut q = Queue::default();
        q.push(note(1, false));
        q.push(note(2, false));
        q.push(Notification { id: 1, summary: "new".into(), ..Default::default() });
        let first = q.next(false).unwrap();
        assert_eq!((first.id, first.summary.as_str()), (1, "new"));
        assert_eq!(q.next(false).unwrap().id, 2);
        assert!(q.next(false).is_none());
    }

    #[test]
    fn queue_overflow_keeps_critical() {
        let mut q = Queue::default();
        q.push(note(1, true));
        for id in 2..=MAX_QUEUE as u32 {
            assert!(q.push(note(id, false)).is_none());
        }
        assert_eq!(q.push(note(100, false)).unwrap().id, 2);
        assert_eq!(q.waiting.len(), MAX_QUEUE);
    }

    #[test]
    fn fullscreen_takes_only_critical() {
        let mut q = Queue::default();
        q.push(note(1, false));
        q.push(note(2, true));
        assert_eq!(q.next(true).unwrap().id, 2);
        assert!(q.next(true).is_none());
        assert_eq!(q.next(false).unwrap().id, 1);
    }

    #[test]
    fn timeouts() {
        let t = |critical, expire_timeout| Notification { critical, expire_timeout, ..Default::default() }.timeout();
        assert_eq!(t(false, -1), Some(DEFAULT_TIMEOUT));
        assert_eq!(t(false, 0), Some(DEFAULT_TIMEOUT));
        assert_eq!(t(false, 10), Some(Duration::from_secs(1)));
        assert_eq!(t(false, 8000), Some(Duration::from_secs(8)));
        assert_eq!(t(false, i32::MAX), Some(MAX_TIMEOUT));
        assert_eq!(t(true, 1000), None);
    }

    #[test]
    fn copies_title_and_body() {
        let t = |summary: &str, body: &str| {
            Notification { summary: summary.into(), body: body.into(), ..Default::default() }.copy_text()
        };
        assert_eq!(t("Title", "one\ntwo"), "Title\none\ntwo");
        assert_eq!(t("Title", ""), "Title");
        assert_eq!(t("", "body"), "body");
        assert_eq!(t("", ""), "");
    }

    #[test]
    fn strips_markup() {
        assert_eq!(plain_text("<b>Bold</b> &amp; <a href=\"x\">link</a>"), "Bold & link");
        assert_eq!(plain_text("<img src=\"a.png\" alt=\"cat\"/>!"), "cat!");
        assert_eq!(plain_text("1 < 2 > 0 & <x> &nope; &#65;&#x42; é<"), "1 < 2 > 0 & <x> &nope; AB é<");
        assert_eq!(plain_text("&lt;b&gt;"), "<b>");
    }

    #[test]
    fn converts_pixels() {
        let image = |channels, rowstride, data: &[u8]| Image {
            width: 1,
            height: 2,
            rowstride,
            channels,
            data: data.to_vec(),
        };
        // RGB with row padding; the last row is unpadded.
        assert_eq!(
            to_bgra_premultiplied(&image(3, 4, &[1, 2, 3, 0, 4, 5, 6])),
            Some(vec![3, 2, 1, 255, 6, 5, 4, 255])
        );
        assert_eq!(
            to_bgra_premultiplied(&image(4, 4, &[255, 0, 0, 128, 9, 9, 9, 0])),
            Some(vec![0, 0, 128, 128, 0, 0, 0, 0])
        );
        assert_eq!(to_bgra_premultiplied(&image(3, 4, &[1, 2, 3])), None);
        assert_eq!(to_bgra_premultiplied(&image(2, 4, &[0; 8])), None);
        assert_eq!(to_bgra_premultiplied(&image(4, 2, &[0; 8])), None);
    }

    #[test]
    fn parses_notify() {
        let hints = glib::VariantDict::new(None);
        hints.insert("urgency", 2u8);
        hints.insert("desktop-entry", "org.gnome.Console");
        let image = (1i32, 1i32, 3i32, false, 8i32, 3i32, vec![1u8, 2, 3]).to_variant();
        hints.insert_value("image-data", &image);
        let params = glib::Variant::tuple_from_iter([
            "app".to_variant(),
            7u32.to_variant(),
            "".to_variant(),
            "two\nlines".to_variant(),
            "<i>body</i>".to_variant(),
            vec!["default", "Open", "reply", "Reply", "odd"].to_variant(),
            hints.end(),
            (-1i32).to_variant(),
        ]);
        let (replaces, n) = parse_notify(&params).unwrap();
        assert_eq!(replaces, 7);
        assert_eq!((n.summary.as_str(), n.body.as_str()), ("two lines", "body"));
        assert!(n.critical && n.has_default && !n.resident);
        assert_eq!(n.buttons, [("reply".to_owned(), "Reply".to_owned())]);
        assert_eq!(n.image.unwrap().data, [1, 2, 3]);
    }
}
