//! Lock screen.
//!
//! Locking covers every monitor with an opaque layer that holds the stage
//! grab, hides all window actors (and disables unredirection, so no
//! fullscreen window can be scanned out above it), and asks for the
//! account password, checked through PAM on a worker thread.
//!
//! Ways to lock, as in gnome-shell:
//! - `org.gnome.ScreenSaver.Lock` (Super+L via gnome-settings-daemon),
//! - the session menu,
//! - logind's `Lock` signal (`loginctl lock-session`),
//! - the session going idle, and before suspend, when
//!   `org.gnome.desktop.screensaver lock-enabled` is on.
//!
//! logind's `Unlock` signal unlocks (it requires privileges). Locking is
//! refused when `org.gnome.desktop.lockdown disable-lock-screen` is set.
//! While locked, the plugin's keybinding filter blocks shortcuts, see
//! [`allows_keybinding`].

use crate::ffi::{self, ClutterActor, ClutterText, MetaDisplay, MtkRectangle};
use crate::ui::{self, gobj};
use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::ffi::CStr;
use std::os::fd::OwnedFd;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

const BACKGROUND: ffi::CoglColor = ffi::CoglColor::rgba(0x10, 0x10, 0x14, 0xff);
const ENTRY_BG: ffi::CoglColor = ffi::CoglColor::rgba(0x2a, 0x2a, 0x32, 0xff);
const ERROR_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0x7b, 0x63, 0xff);
const TIME_FONT: &CStr = c"Sans Light 64";
const DATE_FONT: &CStr = c"Sans 16";
const USER_FONT: &CStr = c"Sans Bold 14";
const ENTRY_FONT: &CStr = c"Sans 13";
const ENTRY_WIDTH: f32 = 320.0;
const ENTRY_HEIGHT: f32 = 40.0;
const BULLET: u32 = '\u{2022}' as u32;

const SCREENSAVER_SCHEMA: &str = "org.gnome.desktop.screensaver";
const LOCKDOWN_SCHEMA: &str = "org.gnome.desktop.lockdown";
/// gnome-session Presence status "idle".
const PRESENCE_IDLE: u32 = 3;

const SCREENSAVER_XML: &str = r#"
<node>
  <interface name="org.gnome.ScreenSaver">
    <method name="Lock"/>
    <method name="GetActive"><arg type="b" direction="out" name="active"/></method>
    <method name="SetActive"><arg type="b" direction="in" name="value"/></method>
    <method name="GetActiveTime"><arg type="u" direction="out" name="value"/></method>
    <signal name="ActiveChanged"><arg name="new_value" type="b"/></signal>
    <signal name="WakeUpScreen"/>
  </interface>
</node>
"#;

thread_local! {
    static INSTANCE: RefCell<Weak<LockScreen>> = RefCell::default();
    static LOCKED: Cell<bool> = const { Cell::new(false) };
}

/// Is the screen locked right now?
pub fn is_locked() -> bool {
    LOCKED.get()
}

/// Lock now, if a lock screen exists and locking isn't disabled.
pub fn lock_now() {
    if let Some(lock) = INSTANCE.with_borrow(|i| i.upgrade()) {
        lock.lock();
    }
}

/// Keybindings that keep working while locked: VT switching, keyboard
/// layout switching (to type the password) and accelerators whose owner
/// (gsd-media-keys) allowed them on the lock screen (volume, brightness, …).
pub fn allows_keybinding(name: &str) -> bool {
    name.starts_with("switch-to-session-")
        || name == "switch-input-source"
        || name == "switch-input-source-backward"
        || crate::shell_dbus::allowed_on_lock_screen(name)
}

pub struct LockScreen {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    overlay: *mut ClutterActor,
    time: *mut ClutterActor,
    date: *mut ClutterActor,
    user: *mut ClutterActor,
    entry_bg: *mut ClutterActor,
    entry: *mut ClutterActor,
    hint: *mut ClutterActor,
    status: *mut ClutterActor,
    /// Runs with `true` when locking and `false` after unlocking.
    on_change: Box<dyn Fn(bool)>,
    screensaver: Option<gio::Settings>,
    lockdown: Option<gio::Settings>,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    grab: Option<*mut ffi::ClutterGrab>,
    authenticating: bool,
    locked_since: Option<Instant>,
    clock: Option<glib::SourceId>,
    idle_lock: Option<glib::SourceId>,
    owner: Option<gio::OwnerId>,
    session_bus: Option<gio::DBusConnection>,
    registration: Option<gio::RegistrationId>,
    system_bus: Option<gio::DBusConnection>,
    login_session: Option<String>,
    subscriptions: Vec<gio::SignalSubscription>,
    /// logind "delay" sleep inhibitor: held while lock-enabled, released
    /// once the screen is locked for suspend.
    sleep_inhibitor: Option<OwnedFd>,
}

fn settings(schema: &str) -> Option<gio::Settings> {
    gio::SettingsSchemaSource::default()
        .and_then(|src| src.lookup(schema, true))
        .map(|_| gio::Settings::new(schema))
}

impl LockScreen {
    pub fn new(display: *mut MetaDisplay, stage: *mut ClutterActor, on_change: Box<dyn Fn(bool)>) -> Rc<Self> {
        let lock = unsafe {
            let overlay = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(overlay, &BACKGROUND);
            ffi::clutter_actor_set_reactive(overlay, 1);
            ffi::clutter_actor_hide(overlay);

            let time = ui::label(TIME_FONT, "", &ui::TEXT_COLOR);
            let date = ui::label(DATE_FONT, "", &ui::TEXT_COLOR);
            let name = glib::real_name().to_string_lossy().into_owned();
            let name = if name.is_empty() || name == "Unknown" {
                glib::user_name().to_string_lossy().into_owned()
            } else {
                name
            };
            let user = ui::label(USER_FONT, &name, &ui::TEXT_COLOR);

            let entry_bg = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(entry_bg, &ENTRY_BG);
            ffi::clutter_actor_set_reactive(entry_bg, 1);
            let hint = ui::label(ENTRY_FONT, "Password", &ui::HINT_COLOR);
            let entry = ui::label(ENTRY_FONT, "", &ui::TEXT_COLOR);
            let text = entry as *mut ClutterText;
            ffi::clutter_text_set_editable(text, 1);
            ffi::clutter_text_set_activatable(text, 1);
            ffi::clutter_text_set_single_line_mode(text, 1);
            ffi::clutter_text_set_password_char(text, BULLET);
            ffi::clutter_text_set_cursor_color(text, &ui::TEXT_COLOR);
            ffi::clutter_actor_set_reactive(entry, 1);
            ffi::clutter_actor_add_child(entry_bg, hint);
            ffi::clutter_actor_add_child(entry_bg, entry);

            let status = ui::label(ui::BODY_FONT, "", &ERROR_COLOR);
            ffi::clutter_text_set_line_wrap(status as *mut ClutterText, 1);

            for a in [time, date, user, entry_bg, status] {
                ffi::clutter_actor_add_child(overlay, a);
            }
            ffi::clutter_actor_add_child(stage, overlay);

            Rc::new(Self {
                display,
                stage,
                overlay,
                time,
                date,
                user,
                entry_bg,
                entry,
                hint,
                status,
                on_change,
                screensaver: settings(SCREENSAVER_SCHEMA),
                lockdown: settings(LOCKDOWN_SCHEMA),
                state: RefCell::default(),
            })
        };
        INSTANCE.with_borrow_mut(|i| *i = Rc::downgrade(&lock));
        lock.connect_signals();
        lock.export_screensaver();
        lock.connect_logind();
        lock.connect_presence();
        lock
    }

    fn connect_signals(self: &Rc<Self>) {
        let entry = unsafe { gobj(self.entry) };
        let weak = Rc::downgrade(self);
        entry.connect_local("activate", false, move |_| {
            with(&weak, |l| l.submit());
            None
        });
        let weak = Rc::downgrade(self);
        entry.connect_local("text-changed", false, move |_| {
            with(&weak, |l| l.sync_hint());
            None
        });
        let weak = Rc::downgrade(self);
        entry.connect_local("key-press-event", false, move |args| {
            let escape = ui::key_symbol(ui::event_arg(args)) == ui::KEY_ESCAPE;
            if escape {
                with(&weak, |l| unsafe { ui::set_text(l.entry, "") });
            }
            Some(escape.to_value())
        });
        // Any click refocuses the password entry; nothing gets through.
        let weak = Rc::downgrade(self);
        unsafe { gobj(self.overlay) }.connect_local("button-press-event", false, move |_| {
            with(&weak, |l| unsafe { ffi::clutter_actor_grab_key_focus(l.entry) });
            Some(true.to_value())
        });
        if let Some(s) = &self.screensaver {
            let weak = Rc::downgrade(self);
            s.connect_changed(Some("lock-enabled"), move |_, _| with(&weak, |l| l.sync_sleep_inhibitor()));
        }
    }

    pub fn is_locked(&self) -> bool {
        self.state.borrow().locked_since.is_some()
    }

    fn lock_disabled(&self) -> bool {
        self.lockdown.as_ref().is_some_and(|s| s.boolean("disable-lock-screen"))
    }

    fn lock_enabled(&self) -> bool {
        self.screensaver.as_ref().is_some_and(|s| s.boolean("lock-enabled")) && !self.lock_disabled()
    }

    pub fn lock(self: &Rc<Self>) {
        if self.is_locked() {
            return;
        }
        if self.lock_disabled() {
            glib::g_message!("gnative", "locking is disabled by org.gnome.desktop.lockdown");
            return;
        }
        glib::g_message!("gnative", "locking the screen");
        LOCKED.set(true);
        self.state.borrow_mut().locked_since = Some(Instant::now());
        // Close popups and park dialogs first: they hold their own grabs.
        (self.on_change)(true);

        unsafe {
            let compositor = ffi::meta_display_get_compositor(self.display);
            ffi::meta_compositor_disable_unredirect(compositor);
            ffi::clutter_actor_hide(ffi::meta_compositor_get_window_group(compositor));
            ffi::clutter_actor_hide(ffi::meta_compositor_get_top_window_group(compositor));

            ui::set_text(self.entry, "");
            ui::set_text(self.status, "");
            ffi::clutter_text_set_editable(self.entry as *mut ClutterText, 1);
            self.relayout();
            ffi::clutter_actor_set_child_above_sibling(self.stage, self.overlay, std::ptr::null_mut());
            ffi::clutter_actor_show(self.overlay);
            let grab = ffi::clutter_stage_grab(self.stage as *mut ffi::ClutterStage, self.overlay);
            self.state.borrow_mut().grab = Some(grab);
            ffi::clutter_actor_grab_key_focus(self.entry);
        }
        self.sync_hint();
        self.update_clock();
        let weak = Rc::downgrade(self);
        let clock = glib::timeout_add_seconds_local(1, move || match weak.upgrade() {
            Some(l) => {
                l.update_clock();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        self.state.borrow_mut().clock = Some(clock);
        self.announce(true);
    }

    fn unlock(self: &Rc<Self>) {
        if !self.is_locked() {
            return;
        }
        glib::g_message!("gnative", "unlocked");
        let (grab, clock) = {
            let mut state = self.state.borrow_mut();
            state.locked_since = None;
            state.authenticating = false;
            (state.grab.take(), state.clock.take())
        };
        if let Some(id) = clock {
            id.remove();
        }
        unsafe {
            ui::set_text(self.entry, "");
            if let Some(grab) = grab {
                ffi::clutter_grab_dismiss(grab);
                gobject_sys::g_object_unref(grab as *mut gobject_sys::GObject);
            }
            ffi::clutter_actor_hide(self.overlay);
            let compositor = ffi::meta_display_get_compositor(self.display);
            ffi::clutter_actor_show(ffi::meta_compositor_get_window_group(compositor));
            ffi::clutter_actor_show(ffi::meta_compositor_get_top_window_group(compositor));
            ffi::meta_compositor_enable_unredirect(compositor);
        }
        LOCKED.set(false);
        self.announce(false);
        (self.on_change)(false);
        self.sync_sleep_inhibitor();
    }

    fn submit(self: &Rc<Self>) {
        if !self.is_locked() || self.state.borrow().authenticating {
            return;
        }
        let password: Vec<u8> = unsafe {
            let p = ffi::clutter_text_get_text(self.entry as *mut ClutterText);
            if p.is_null() { Vec::new() } else { CStr::from_ptr(p).to_bytes().to_vec() }
        };
        if password.is_empty() {
            return;
        }
        self.state.borrow_mut().authenticating = true;
        unsafe {
            ui::set_text(self.entry, "");
            ffi::clutter_text_set_editable(self.entry as *mut ClutterText, 0);
            ui::set_text(self.status, "");
            ui::set_text(self.hint, "Checking…");
        }
        let user = glib::user_name().to_string_lossy().into_owned();
        crate::pam::authenticate_async(user, password, |result| {
            if let Some(lock) = INSTANCE.with_borrow(|i| i.upgrade()) {
                lock.on_auth_result(result);
            }
        });
    }

    fn on_auth_result(self: &Rc<Self>, result: Result<(), String>) {
        if !self.state.borrow().authenticating {
            return; // unlocked meanwhile (logind Unlock)
        }
        self.state.borrow_mut().authenticating = false;
        match result {
            Ok(()) => self.unlock(),
            Err(message) => unsafe {
                ui::set_text(self.status, &message);
                ui::set_text(self.hint, "Password");
                ffi::clutter_text_set_editable(self.entry as *mut ClutterText, 1);
                ffi::clutter_actor_grab_key_focus(self.entry);
                self.relayout();
            },
        }
        self.sync_hint();
    }

    fn sync_hint(&self) {
        unsafe {
            let p = ffi::clutter_text_get_text(self.entry as *mut ClutterText);
            let empty = p.is_null() || *p == 0;
            if empty { ffi::clutter_actor_show(self.hint) } else { ffi::clutter_actor_hide(self.hint) }
        }
    }

    fn update_clock(&self) {
        let Ok(now) = glib::DateTime::now_local() else { return };
        let time = now.format("%H:%M").map(|s| s.to_string()).unwrap_or_default();
        let date = now.format("%A, %e %B").map(|s| s.to_string()).unwrap_or_default();
        unsafe {
            ui::set_text(self.time, &time);
            ui::set_text(self.date, date.replace("  ", " ").trim());
        }
        self.relayout();
    }

    /// Cover all monitors; center the prompt on the primary one.
    pub fn relayout(&self) {
        unsafe {
            let n = ffi::meta_display_get_n_monitors(self.display);
            let mut bounds: Option<MtkRectangle> = None;
            for i in 0..n {
                let mut r = MtkRectangle::default();
                ffi::meta_display_get_monitor_geometry(self.display, i, &mut r);
                bounds = Some(match bounds {
                    None => r,
                    Some(b) => {
                        let (x1, y1) = (b.x.min(r.x), b.y.min(r.y));
                        let (x2, y2) = ((b.x + b.width).max(r.x + r.width), (b.y + b.height).max(r.y + r.height));
                        MtkRectangle { x: x1, y: y1, width: x2 - x1, height: y2 - y1 }
                    }
                });
            }
            let Some(b) = bounds else { return };
            ffi::clutter_actor_set_position(self.overlay, b.x as f32, b.y as f32);
            ffi::clutter_actor_set_size(self.overlay, b.width as f32, b.height as f32);

            let geo = ui::primary_monitor_geometry(self.display).unwrap_or(b);
            let (ox, oy) = ((geo.x - b.x) as f32, (geo.y - b.y) as f32);
            let w = geo.width as f32;
            let center = |a: *mut ClutterActor, y: f32| {
                let x = ox + (w - ffi::clutter_actor_get_width(a)) / 2.0;
                ffi::clutter_actor_set_position(a, x.floor(), y.floor());
                y + ffi::clutter_actor_get_height(a)
            };
            let mut y = oy + geo.height as f32 * 0.18;
            y = center(self.time, y);
            y = center(self.date, y + 4.0);
            y = center(self.user, y + geo.height as f32 * 0.12);

            let entry_w = ENTRY_WIDTH.min(w - 32.0);
            ffi::clutter_actor_set_size(self.entry_bg, entry_w, ENTRY_HEIGHT);
            let ty = ((ENTRY_HEIGHT - ffi::clutter_actor_get_height(self.entry)) / 2.0).floor();
            ffi::clutter_actor_set_width(self.entry, entry_w - 24.0);
            ffi::clutter_actor_set_position(self.entry, 12.0, ty);
            ffi::clutter_actor_set_position(self.hint, 12.0, ty);
            y = center(self.entry_bg, y + 16.0);

            ffi::clutter_actor_set_width(self.status, entry_w);
            ffi::clutter_actor_set_position(self.status, (ox + (w - entry_w) / 2.0).floor(), y + 12.0);
        }
    }

    pub fn destroy(&self) {
        let mut state = self.state.borrow_mut();
        if let Some(id) = state.clock.take() {
            id.remove();
        }
        if let Some(id) = state.idle_lock.take() {
            id.remove();
        }
        let registration = state.registration.take();
        if let (Some(bus), Some(id)) = (state.session_bus.as_ref(), registration) {
            let _ = bus.unregister_object(id);
        }
        if let Some(owner) = state.owner.take() {
            gio::bus_unown_name(owner);
        }
        state.subscriptions.clear();
        state.sleep_inhibitor = None;
        if let Some(grab) = state.grab.take() {
            unsafe {
                ffi::clutter_grab_dismiss(grab);
                gobject_sys::g_object_unref(grab as *mut gobject_sys::GObject);
            }
        }
        drop(state);
        unsafe { ffi::clutter_actor_destroy(self.overlay) };
        LOCKED.set(false);
    }

    // ------------------------------------------------------------------ D-Bus

    /// Tell listeners: ScreenSaver ActiveChanged and logind's LockedHint.
    fn announce(&self, locked: bool) {
        let state = self.state.borrow();
        if let Some(bus) = &state.session_bus {
            let _ = bus.emit_signal(
                None,
                "/org/gnome/ScreenSaver",
                "org.gnome.ScreenSaver",
                "ActiveChanged",
                Some(&(locked,).to_variant()),
            );
        }
        if let (Some(bus), Some(path)) = (&state.system_bus, &state.login_session) {
            bus.call(
                Some("org.freedesktop.login1"),
                path,
                "org.freedesktop.login1.Session",
                "SetLockedHint",
                Some(&(locked,).to_variant()),
                None,
                gio::DBusCallFlags::NONE,
                -1,
                gio::Cancellable::NONE,
                |r| {
                    if let Err(e) = r {
                        glib::g_debug!("gnative", "SetLockedHint failed: {e}");
                    }
                },
            );
        }
    }

    /// `org.gnome.ScreenSaver` on the session bus.
    fn export_screensaver(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let owner = gio::bus_own_name(
            gio::BusType::Session,
            "org.gnome.ScreenSaver",
            gio::BusNameOwnerFlags::NONE,
            move |connection, _| {
                let Some(lock) = weak.upgrade() else { return };
                let info = gio::DBusNodeInfo::for_xml(SCREENSAVER_XML)
                    .ok()
                    .and_then(|n| n.lookup_interface("org.gnome.ScreenSaver"))
                    .expect("valid org.gnome.ScreenSaver XML");
                let weak = Rc::downgrade(&lock);
                let reg = connection
                    .register_object("/org/gnome/ScreenSaver", &info)
                    .method_call(move |_, _, _, _, method, params, invocation| {
                        let Some(lock) = weak.upgrade() else {
                            invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "shutting down");
                            return;
                        };
                        match method {
                            "Lock" => {
                                lock.lock();
                                invocation.return_value(None);
                            }
                            "GetActive" => invocation.return_value(Some(&(lock.is_locked(),).to_variant())),
                            "SetActive" => {
                                // Activating the screensaver locks only if
                                // locking is enabled; deactivating never
                                // unlocks without a password.
                                let (value,) = params.get::<(bool,)>().unwrap_or_default();
                                if value && lock.lock_enabled() {
                                    lock.lock();
                                }
                                invocation.return_value(None);
                            }
                            "GetActiveTime" => {
                                let secs = lock
                                    .state
                                    .borrow()
                                    .locked_since
                                    .map_or(0, |t| t.elapsed().as_secs() as u32);
                                invocation.return_value(Some(&(secs,).to_variant()));
                            }
                            _ => invocation.return_dbus_error(
                                "org.freedesktop.DBus.Error.UnknownMethod",
                                "unknown method",
                            ),
                        }
                    })
                    .build();
                let mut state = lock.state.borrow_mut();
                match reg {
                    Ok(id) => state.registration = Some(id),
                    Err(e) => glib::g_warning!("gnative", "could not export /org/gnome/ScreenSaver: {e}"),
                }
                state.session_bus = Some(connection);
            },
            |_, _| {},
            |_, name| glib::g_warning!("gnative", "could not own {name}"),
        );
        self.state.borrow_mut().owner = Some(owner);
    }

    /// logind: Lock/Unlock signals for our session, PrepareForSleep.
    fn connect_logind(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        gio::bus_get(gio::BusType::System, gio::Cancellable::NONE, move |bus| {
            let Ok(bus) = bus else { return };
            let Some(lock) = weak.upgrade() else { return };
            lock.state.borrow_mut().system_bus = Some(bus.clone());

            let weak = Rc::downgrade(&lock);
            let sleep = bus.subscribe_to_signal(
                Some("org.freedesktop.login1"),
                Some("org.freedesktop.login1.Manager"),
                Some("PrepareForSleep"),
                Some("/org/freedesktop/login1"),
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    let (start,) = signal.parameters.get::<(bool,)>().unwrap_or_default();
                    with(&weak, |l| l.on_prepare_for_sleep(start));
                },
            );
            lock.state.borrow_mut().subscriptions.push(sleep);
            lock.sync_sleep_inhibitor();

            // Find our login session: by id (set by gnome-session / GDM), else by pid.
            let (method, args) = match std::env::var("XDG_SESSION_ID") {
                Ok(id) if !id.is_empty() => ("GetSession", (id,).to_variant()),
                _ => ("GetSessionByPID", (std::process::id(),).to_variant()),
            };
            let weak = Rc::downgrade(&lock);
            let bus2 = bus.clone();
            bus.call(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
                method,
                Some(&args),
                Some(glib::VariantTy::new("(o)").unwrap()),
                gio::DBusCallFlags::NONE,
                -1,
                gio::Cancellable::NONE,
                move |result| {
                    let path = match result {
                        Ok(v) => v.child_value(0).str().map(str::to_owned),
                        Err(e) => {
                            glib::g_debug!("gnative", "no logind session ({e}); lock signals unavailable");
                            None
                        }
                    };
                    let (Some(path), Some(lock)) = (path, weak.upgrade()) else { return };
                    let weak = Rc::downgrade(&lock);
                    let sub = bus2.subscribe_to_signal(
                        Some("org.freedesktop.login1"),
                        Some("org.freedesktop.login1.Session"),
                        None,
                        Some(&path),
                        None,
                        gio::DBusSignalFlags::NONE,
                        move |signal| match signal.signal_name {
                            "Lock" => with(&weak, |l| l.lock()),
                            "Unlock" => with(&weak, |l| l.unlock()),
                            _ => {}
                        },
                    );
                    let mut state = lock.state.borrow_mut();
                    state.subscriptions.push(sub);
                    state.login_session = Some(path);
                },
            );
        });
    }

    /// gnome-session's presence: lock when the session goes idle (it
    /// already honors idle-delay and idle inhibitors such as video players).
    fn connect_presence(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |bus| {
            let Ok(bus) = bus else { return };
            let Some(lock) = weak.upgrade() else { return };
            let weak = Rc::downgrade(&lock);
            let sub = bus.subscribe_to_signal(
                Some("org.gnome.SessionManager"),
                Some("org.gnome.SessionManager.Presence"),
                Some("StatusChanged"),
                Some("/org/gnome/SessionManager/Presence"),
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    let (status,) = signal.parameters.get::<(u32,)>().unwrap_or_default();
                    with(&weak, |l| l.on_presence(status));
                },
            );
            lock.state.borrow_mut().subscriptions.push(sub);
        });
    }

    fn on_presence(self: &Rc<Self>, status: u32) {
        if let Some(id) = self.state.borrow_mut().idle_lock.take() {
            id.remove();
        }
        if status != PRESENCE_IDLE || !self.lock_enabled() || self.is_locked() {
            return;
        }
        let delay = self.screensaver.as_ref().map_or(0, |s| s.uint("lock-delay"));
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_secs(delay as u64), move || {
            with(&weak, |l| {
                l.state.borrow_mut().idle_lock = None;
                l.lock();
            });
        });
        self.state.borrow_mut().idle_lock = Some(id);
    }

    fn on_prepare_for_sleep(self: &Rc<Self>, start: bool) {
        if !start {
            // Resumed: take the inhibitor again for the next suspend.
            self.sync_sleep_inhibitor();
            return;
        }
        if self.lock_enabled() {
            self.lock();
        }
        // Let the locked frame reach the screen, then allow the suspend.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(100), move || {
            with(&weak, |l| l.state.borrow_mut().sleep_inhibitor = None);
        });
    }

    /// Hold a logind "delay" inhibitor for sleep while lock-enabled, like
    /// gnome-shell, so the screen is locked before the system suspends.
    fn sync_sleep_inhibitor(self: &Rc<Self>) {
        let want = self.lock_enabled();
        let (have, bus) = {
            let state = self.state.borrow();
            (state.sleep_inhibitor.is_some(), state.system_bus.clone())
        };
        if !want {
            self.state.borrow_mut().sleep_inhibitor = None;
            return;
        }
        let Some(bus) = bus.filter(|_| !have) else { return };
        let weak = Rc::downgrade(self);
        bus.call_with_unix_fd_list(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1",
            "org.freedesktop.login1.Manager",
            "Inhibit",
            Some(&("sleep", "gnative", "Lock the screen before suspending", "delay").to_variant()),
            Some(glib::VariantTy::new("(h)").unwrap()),
            gio::DBusCallFlags::NONE,
            -1,
            None::<&gio::UnixFDList>,
            gio::Cancellable::NONE,
            move |result| {
                let fd = match result {
                    Ok((reply, Some(fds))) => {
                        let index = reply.child_value(0).get::<glib::variant::Handle>().map_or(0, |h| h.0);
                        fds.get(index).ok()
                    }
                    Ok(_) => None,
                    Err(e) => {
                        glib::g_debug!("gnative", "sleep inhibitor unavailable: {e}");
                        None
                    }
                };
                with(&weak, |l| l.state.borrow_mut().sleep_inhibitor = fd);
            },
        );
    }
}

fn with(weak: &Weak<LockScreen>, f: impl FnOnce(&Rc<LockScreen>)) {
    if let Some(l) = weak.upgrade() {
        f(&l);
    }
}

#[cfg(test)]
mod tests {
    use super::allows_keybinding;

    #[test]
    fn lock_screen_keybinding_policy() {
        assert!(allows_keybinding("switch-to-session-3"));
        assert!(allows_keybinding("switch-input-source"));
        assert!(!allows_keybinding("switch-applications"));
        assert!(!allows_keybinding("panel-run-dialog"));
        assert!(!allows_keybinding("external-grab-42")); // not registered as lock-screen safe
    }
}
