//! Session menu: a power button at the right end of the panel that opens
//! Suspend / Restart / Power Off / Log Out.
//!
//! Lock and Suspend act at once. The others go through gnome-session, which
//! has `end_session.rs` show a confirmation that names the apps blocking it.
//! Without gnome-session they are confirmed here; then Restart and Power Off
//! go to systemd-logind (`org.freedesktop.login1.Manager`) with interactive
//! authorization, so polkit can ask for a password through gnutshell's own
//! agent, and Log Out ends gnutshell. Suspend always goes to logind.

mod end_session;

use crate::ffi::{self, ClutterActor, MetaDisplay};
use crate::icons;
use crate::ui::{self, gobj};
use gio::prelude::*;
use end_session::Blocker;
use std::cell::RefCell;
use std::ffi::CStr;
use std::rc::{Rc, Weak};

const MENU_WIDTH: f32 = 220.0;
const MENU_PADDING: f32 = 6.0;
pub const BUTTON_SIZE: f32 = 32.0;
const PANEL_ICON_SIZE: f32 = 16.0;
const DIALOG_WIDTH: f32 = 400.0;
const PADDING: f32 = 24.0;
const SPACING: f32 = 12.0;
const DIALOG_BUTTON_WIDTH: f32 = 120.0;
const DIALOG_BUTTON_HEIGHT: f32 = 36.0;
const TITLE_FONT: &CStr = c"Sans Bold 14";
/// Dialog text when nothing blocks the action.
const CONFIRM_BODY: &str = "Open apps will be closed. Unsaved work will be lost.";
const BLOCKED_DIALOG_WIDTH: f32 = 520.0;
const BLOCKED_BUTTON_WIDTH: f32 = 170.0;
const HOVER_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x20);

const LOGIND_NAME: &str = "org.freedesktop.login1";
const LOGIND_PATH: &str = "/org/freedesktop/login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Lock,
    Suspend,
    Restart,
    PowerOff,
    LogOut,
}

impl Action {
    const ALL: [Action; 5] = [Action::Lock, Action::Suspend, Action::Restart, Action::PowerOff, Action::LogOut];

    fn label(self) -> &'static str {
        match self {
            Action::Lock => "Lock",
            Action::Suspend => "Suspend",
            Action::Restart => "Restart…",
            Action::PowerOff => "Power Off…",
            Action::LogOut => "Log Out…",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Action::Lock => "system-lock-screen-symbolic",
            Action::Suspend => "media-playback-pause-symbolic",
            Action::Restart => "system-reboot-symbolic",
            Action::PowerOff => "system-shutdown-symbolic",
            Action::LogOut => "system-log-out-symbolic",
        }
    }

    /// (title, confirm button) for actions that need confirmation.
    fn confirmation(self) -> Option<(&'static str, &'static str)> {
        match self {
            Action::Lock | Action::Suspend => None,
            Action::Restart => Some(("Restart?", "Restart")),
            Action::PowerOff => Some(("Power Off?", "Power Off")),
            Action::LogOut => Some(("Log Out?", "Log Out")),
        }
    }

    /// logind method and its availability query.
    fn logind(self) -> Option<(&'static str, &'static str)> {
        match self {
            Action::Suspend => Some(("Suspend", "CanSuspend")),
            Action::Restart => Some(("Reboot", "CanReboot")),
            Action::PowerOff => Some(("PowerOff", "CanPowerOff")),
            Action::Lock | Action::LogOut => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Mode {
    Closed,
    Menu,
    Confirm { action: Action, blocked: bool },
}

pub struct SessionMenu {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    panel_height: f32,
    /// Power button inside the panel.
    button: *mut ClutterActor,
    button_icon: *mut ClutterActor,
    /// Full-monitor actor that holds the grab; transparent for the menu,
    /// dimmed for the confirmation dialog.
    overlay: *mut ClutterActor,
    menu: *mut ClutterActor,
    dialog: *mut ClutterActor,
    state: RefCell<State>,
}

struct State {
    mode: Mode,
    grab: Option<*mut ffi::ClutterGrab>,
    /// Actions offered in the menu, in order; rows match by index.
    items: Vec<Action>,
    rows: Vec<*mut ClutterActor>,
    selected: usize,
    /// Per action: does logind allow it? Log Out is always available.
    available: [bool; 5],
    bus: Option<gio::DBusConnection>,
    session_bus: Option<gio::DBusConnection>,
    registration: Option<gio::RegistrationId>,
    request: Option<end_session::Request>,
}

impl SessionMenu {
    pub fn new(
        display: *mut MetaDisplay,
        stage: *mut ClutterActor,
        panel: *mut ClutterActor,
        panel_height: f32,
    ) -> Rc<Self> {
        let menu = unsafe {
            let button = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(button, BUTTON_SIZE, panel_height);
            ffi::clutter_actor_set_reactive(button, 1);
            let button_icon = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(button_icon, PANEL_ICON_SIZE, PANEL_ICON_SIZE);
            ffi::clutter_actor_set_content_gravity(
                button_icon,
                ffi::CLUTTER_CONTENT_GRAVITY_RESIZE_ASPECT,
            );
            ffi::clutter_actor_set_position(
                button_icon,
                ((BUTTON_SIZE - PANEL_ICON_SIZE) / 2.0).floor(),
                ((panel_height - PANEL_ICON_SIZE) / 2.0).floor(),
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

            let dialog = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(dialog, &ui::BOX_COLOR);
            ffi::clutter_actor_set_reactive(dialog, 1);
            ffi::clutter_actor_add_child(overlay, dialog);

            ffi::clutter_actor_add_child(stage, overlay);

            Rc::new(Self {
                display,
                stage,
                panel_height,
                button,
                button_icon,
                overlay,
                menu,
                dialog,
                state: RefCell::new(State {
                    mode: Mode::Closed,
                    grab: None,
                    items: Vec::new(),
                    rows: Vec::new(),
                    selected: 0,
                    available: [true, false, false, false, true],
                    bus: None,
                    session_bus: None,
                    registration: None,
                    request: None,
                }),
            })
        };
        menu.connect_signals();
        menu.update_button_icon();
        menu.query_logind();
        menu.export_end_session_dialog();
        menu
    }

    fn connect_signals(self: &Rc<Self>) {
        let button = unsafe { gobj(self.button) };
        let weak = Rc::downgrade(self);
        button.connect_local("button-press-event", false, move |_| {
            with(&weak, |m| m.toggle());
            Some(true.to_value())
        });
        let b = self.button;
        button.connect_local("enter-event", false, move |_| {
            unsafe { ffi::clutter_actor_set_background_color(b, &HOVER_COLOR) };
            Some(false.to_value())
        });
        button.connect_local("leave-event", false, move |_| {
            unsafe { ffi::clutter_actor_set_background_color(b, &ui::TRANSPARENT) };
            Some(false.to_value())
        });

        let overlay = unsafe { gobj(self.overlay) };
        let weak = Rc::downgrade(self);
        overlay.connect_local("key-press-event", false, move |args| {
            let handled = weak.upgrade().is_some_and(|m| m.on_key(ui::key_symbol(ui::event_arg(args))));
            Some(handled.to_value())
        });
        // Click outside: closes the menu; the confirmation dialog is modal.
        let weak = Rc::downgrade(self);
        overlay.connect_local("button-press-event", false, move |_| {
            with(&weak, |m| {
                if m.state.borrow().mode == Mode::Menu {
                    m.close();
                }
            });
            Some(true.to_value())
        });
        for inner in [self.menu, self.dialog] {
            unsafe { gobj(inner) }.connect_local("button-press-event", false, |_| Some(true.to_value()));
        }
    }

    /// Place the power button at the right end of a panel `panel_width` wide.
    pub fn place_button(&self, panel_width: f32) {
        unsafe { ffi::clutter_actor_set_position(self.button, panel_width - BUTTON_SIZE - 4.0, 0.0) };
        self.update_button_icon();
    }

    pub fn is_open(&self) -> bool {
        self.state.borrow().mode != Mode::Closed
    }

    pub fn toggle(self: &Rc<Self>) {
        if self.is_open() { self.close() } else { self.open_menu() }
    }

    pub fn open_menu(self: &Rc<Self>) {
        if self.is_open() {
            return;
        }
        let items: Vec<Action> = {
            let state = self.state.borrow();
            Action::ALL.into_iter().filter(|a| state.available[*a as usize]).collect()
        };
        {
            let mut state = self.state.borrow_mut();
            state.items = items;
            state.selected = 0;
        }
        self.build_rows();
        unsafe {
            self.layout_overlay();
            ffi::clutter_actor_set_background_color(self.overlay, &ui::TRANSPARENT);
            ffi::clutter_actor_hide(self.dialog);
            ffi::clutter_actor_show(self.menu);
        }
        self.state.borrow_mut().mode = Mode::Menu;
        self.grab();
    }

    pub fn close(&self) {
        // Any way of closing an unanswered dialog is a cancel.
        self.cancel_request();
        let grab = {
            let mut state = self.state.borrow_mut();
            state.mode = Mode::Closed;
            state.rows.clear();
            state.grab.take()
        };
        unsafe {
            if let Some(grab) = grab {
                ffi::clutter_grab_dismiss(grab);
                gobject_sys::g_object_unref(grab as *mut gobject_sys::GObject);
            }
            ffi::clutter_actor_hide(self.overlay);
            ffi::clutter_actor_destroy_all_children(self.menu);
            ffi::clutter_actor_destroy_all_children(self.dialog);
        }
    }

    pub fn destroy(&self) {
        self.close();
        let (bus, registration) = {
            let mut state = self.state.borrow_mut();
            (state.session_bus.take(), state.registration.take())
        };
        if let (Some(bus), Some(id)) = (bus, registration) {
            let _ = bus.unregister_object(id);
        }
        unsafe {
            ffi::clutter_actor_destroy(self.overlay);
            ffi::clutter_actor_destroy(self.button);
        }
    }

    /// Run `action`, asking for confirmation first where appropriate.
    pub fn choose(self: &Rc<Self>, action: Action) {
        self.close();
        if action.confirmation().is_some() {
            self.request_end_session(action);
        } else {
            self.perform(action);
        }
    }

    // ------------------------------------------------------------------ internals

    fn grab(&self) {
        unsafe {
            let grab = ffi::clutter_stage_grab(self.stage as *mut ffi::ClutterStage, self.overlay);
            self.state.borrow_mut().grab = Some(grab);
            ffi::clutter_actor_grab_key_focus(self.overlay);
        }
    }

    fn on_key(self: &Rc<Self>, keysym: u32) -> bool {
        let mode = self.state.borrow().mode;
        match (mode, keysym) {
            (_, ui::KEY_ESCAPE) => self.close(),
            (Mode::Menu, ui::KEY_DOWN | ui::KEY_TAB) => self.move_selection(1),
            (Mode::Menu, ui::KEY_UP | ui::KEY_ISO_LEFT_TAB) => self.move_selection(-1),
            (Mode::Menu, ui::KEY_RETURN | ui::KEY_KP_ENTER) => {
                let action = {
                    let state = self.state.borrow();
                    state.items.get(state.selected).copied()
                };
                if let Some(action) = action {
                    self.choose(action);
                }
            }
            // With apps in the way, Enter must not push past them.
            (Mode::Confirm { action, blocked }, ui::KEY_RETURN | ui::KEY_KP_ENTER) => {
                if blocked {
                    self.close();
                } else {
                    self.confirm(action);
                }
            }
            _ => return false,
        }
        true
    }

    fn move_selection(&self, delta: isize) {
        let n = self.state.borrow().items.len();
        if n == 0 {
            return;
        }
        let selected = {
            let mut state = self.state.borrow_mut();
            state.selected = (state.selected as isize + delta).rem_euclid(n as isize) as usize;
            state.selected
        };
        self.highlight(selected);
    }

    fn highlight(&self, selected: usize) {
        let state = self.state.borrow();
        for (i, row) in state.rows.iter().enumerate() {
            let color = if i == selected { &ui::SELECTED_COLOR } else { &ui::TRANSPARENT };
            unsafe { ffi::clutter_actor_set_background_color(*row, color) };
        }
    }

    fn build_rows(self: &Rc<Self>) {
        let items = self.state.borrow().items.clone();
        let scale = ui::primary_monitor_scale(self.display);
        let inner_w = MENU_WIDTH - 2.0 * MENU_PADDING;
        let mut rows = Vec::new();
        unsafe {
            ffi::clutter_actor_destroy_all_children(self.menu);
            for (i, action) in items.iter().copied().enumerate() {
                let icon = icons::symbolic(action.icon(), &ui::TEXT_COLOR, ui::ROW_ICON_SIZE, scale, self.stage);
                let row = ui::list_row(action.label(), icon.as_ref(), inner_w, i == 0);
                ffi::clutter_actor_set_position(row, MENU_PADDING, MENU_PADDING + i as f32 * ui::ROW_HEIGHT);
                ffi::clutter_actor_set_reactive(row, 1);
                // Parent before wrapping: see `ui::gobj`.
                ffi::clutter_actor_add_child(self.menu, row);
                let obj = gobj(row);
                let weak = Rc::downgrade(self);
                obj.connect_local("enter-event", false, move |_| {
                    with(&weak, |m| {
                        m.state.borrow_mut().selected = i;
                        m.highlight(i);
                    });
                    Some(false.to_value())
                });
                let weak = Rc::downgrade(self);
                obj.connect_local("button-press-event", false, move |_| {
                    with(&weak, |m| m.choose(action));
                    Some(true.to_value())
                });
                rows.push(row);
            }
            let height = 2.0 * MENU_PADDING + items.len() as f32 * ui::ROW_HEIGHT;
            ffi::clutter_actor_set_size(self.menu, MENU_WIDTH, height);
        }
        self.state.borrow_mut().rows = rows;
    }

    /// The confirmation dialog. Called again while open, it is redrawn.
    fn show_confirmation(self: &Rc<Self>, action: Action, blockers: &[Blocker]) {
        let Some((title, confirm)) = action.confirmation() else { return };
        let body = end_session::describe(blockers);
        let blocked = !blockers.is_empty();
        let confirm = if blocked { format!("{confirm} Anyway") } else { confirm.to_owned() };
        let (dialog_width, button_width) = if blocked {
            (BLOCKED_DIALOG_WIDTH, BLOCKED_BUTTON_WIDTH)
        } else {
            (DIALOG_WIDTH, DIALOG_BUTTON_WIDTH)
        };
        self.state.borrow_mut().rows.clear();
        let needs_grab = self.state.borrow().grab.is_none();
        if needs_grab {
            unsafe { self.layout_overlay() };
        }
        unsafe {
            ffi::clutter_actor_destroy_all_children(self.menu);
            ffi::clutter_actor_hide(self.menu);
            ffi::clutter_actor_destroy_all_children(self.dialog);

            let width = dialog_width.min(ffi::clutter_actor_get_width(self.overlay) - 32.0);
            let inner = width - 2.0 * PADDING;
            let mut y = PADDING;
            for (font, text) in [(TITLE_FONT, title), (ui::BODY_FONT, body.as_str())] {
                let l = ui::label(font, text, &ui::TEXT_COLOR);
                ffi::clutter_text_set_line_wrap(l as *mut ffi::ClutterText, 1);
                ffi::clutter_actor_set_width(l, inner);
                ffi::clutter_actor_add_child(self.dialog, l);
                ffi::clutter_actor_set_position(l, PADDING, y);
                y += ffi::clutter_actor_get_height(l) + SPACING;
            }
            y += SPACING;

            let ok = ui::button(&confirm, button_width, DIALOG_BUTTON_HEIGHT, &ui::DESTRUCTIVE_COLOR);
            let cancel = ui::button("Cancel", button_width, DIALOG_BUTTON_HEIGHT, &ui::BUTTON_COLOR);
            ffi::clutter_actor_add_child(self.dialog, ok);
            ffi::clutter_actor_add_child(self.dialog, cancel);
            ffi::clutter_actor_set_position(ok, width - PADDING - button_width, y);
            ffi::clutter_actor_set_position(cancel, width - PADDING - 2.0 * button_width - SPACING, y);
            y += DIALOG_BUTTON_HEIGHT + PADDING;

            let weak = Rc::downgrade(self);
            gobj(ok).connect_local("button-press-event", false, move |_| {
                with(&weak, |m| m.confirm(action));
                Some(true.to_value())
            });
            let weak = Rc::downgrade(self);
            gobj(cancel).connect_local("button-press-event", false, move |_| {
                with(&weak, |m| m.close());
                Some(true.to_value())
            });

            ffi::clutter_actor_set_size(self.dialog, width, y);
            let mon_w = ffi::clutter_actor_get_width(self.overlay);
            let mon_h = ffi::clutter_actor_get_height(self.overlay);
            ffi::clutter_actor_set_position(
                self.dialog,
                ((mon_w - width) / 2.0).floor(),
                ((mon_h - y) * 0.4).floor(),
            );
            ffi::clutter_actor_set_background_color(self.overlay, &ui::DIM_COLOR);
            ffi::clutter_actor_show(self.dialog);
        }
        self.state.borrow_mut().mode = Mode::Confirm { action, blocked };
        // gnome-session can open the dialog over anything.
        unsafe { ffi::clutter_actor_set_child_above_sibling(self.stage, self.overlay, std::ptr::null_mut()) };
        if needs_grab {
            self.grab();
        } else {
            unsafe { ffi::clutter_actor_grab_key_focus(self.overlay) };
        }
    }

    unsafe fn layout_overlay(&self) {
        unsafe {
            let geo = ui::primary_monitor_geometry(self.display).unwrap_or_default();
            ffi::clutter_actor_set_position(self.overlay, geo.x as f32, geo.y as f32);
            ffi::clutter_actor_set_size(self.overlay, geo.width as f32, geo.height as f32);
            ffi::clutter_actor_set_position(
                self.menu,
                geo.width as f32 - MENU_WIDTH - 4.0,
                self.panel_height + 4.0,
            );
            ffi::clutter_actor_show(self.overlay);
        }
    }

    fn update_button_icon(&self) {
        let scale = ui::primary_monitor_scale(self.display);
        let icon = icons::symbolic(
            Action::PowerOff.icon(),
            &ui::TEXT_COLOR,
            PANEL_ICON_SIZE,
            scale,
            self.stage,
        );
        let ptr = icon.as_ref().map_or(std::ptr::null_mut(), |c| c.as_ptr() as *mut ffi::ClutterContent);
        unsafe { ffi::clutter_actor_set_content(self.button_icon, ptr) };
    }

    fn perform(&self, action: Action) {
        glib::g_message!("gnutshell", "session action: {action:?}");
        if action == Action::Lock {
            crate::lockscreen::lock_now();
            return;
        }
        let Some((method, _)) = action.logind() else {
            // Log Out without gnome-session: ending gnutshell is all it takes.
            unsafe { ffi::meta_context_terminate(ffi::meta_display_get_context(self.display)) };
            return;
        };
        let Some(bus) = self.state.borrow().bus.clone() else {
            glib::g_warning!("gnutshell", "{method}: no connection to the system bus");
            return;
        };
        bus.call(
            Some(LOGIND_NAME),
            LOGIND_PATH,
            LOGIND_MANAGER,
            method,
            Some(&(true,).to_variant()), // interactive: let polkit ask
            None,
            gio::DBusCallFlags::ALLOW_INTERACTIVE_AUTHORIZATION,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(e) = result {
                    glib::g_warning!("gnutshell", "logind {method} failed: {e}");
                }
            },
        );
    }

    /// Ask logind which power actions this user may use.
    fn query_logind(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        gio::bus_get(gio::BusType::System, gio::Cancellable::NONE, move |result| {
            let bus = match result {
                Ok(bus) => bus,
                Err(e) => {
                    glib::g_warning!("gnutshell", "session menu: no system bus: {e}");
                    return;
                }
            };
            let Some(menu) = weak.upgrade() else { return };
            menu.state.borrow_mut().bus = Some(bus.clone());
            for action in Action::ALL {
                let Some((_, query)) = action.logind() else { continue };
                let weak = Rc::downgrade(&menu);
                bus.call(
                    Some(LOGIND_NAME),
                    LOGIND_PATH,
                    LOGIND_MANAGER,
                    query,
                    None,
                    Some(glib::VariantTy::new("(s)").unwrap()),
                    gio::DBusCallFlags::NONE,
                    -1,
                    gio::Cancellable::NONE,
                    move |result| {
                        let answer = match result {
                            Ok(v) => v.get::<(String,)>().map(|(s,)| s).unwrap_or_default(),
                            Err(e) => {
                                glib::g_warning!("gnutshell", "logind {query} failed: {e}");
                                return;
                            }
                        };
                        glib::g_debug!("gnutshell", "logind {query}: {answer}");
                        with(&weak, |m| {
                            m.state.borrow_mut().available[action as usize] = is_available(&answer);
                        });
                    },
                );
            }
        });
    }
}

/// Tell GDM the session is up, as gnome-shell does: `RegisterSession` once
/// the shell starts, `RegisterDisplay` once it has drawn. A session whose
/// .desktop entry says `X-GDM-SessionRegisters=true` and never registers
/// is treated by GDM as failed. Outside GDM the calls just fail quietly.
pub fn register_with_gdm(method: &'static str) {
    glib::g_message!("gnutshell", "{method} with GDM");
    gio::bus_get(gio::BusType::System, gio::Cancellable::NONE, move |bus| {
        let Ok(bus) = bus else { return };
        bus.call(
            Some("org.gnome.DisplayManager"),
            "/org/gnome/DisplayManager/Manager",
            "org.gnome.DisplayManager.Manager",
            method,
            None,
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            -1,
            gio::Cancellable::NONE,
            move |result| {
                if let Err(e) = result {
                    glib::g_debug!("gnutshell", "GDM {method} failed: {e}");
                }
            },
        );
    });
}

/// logind `Can*` answers: "yes" and "challenge" (needs authentication) are
/// usable; "no" and "na" are not.
fn is_available(answer: &str) -> bool {
    matches!(answer, "yes" | "challenge")
}

fn with(weak: &Weak<SessionMenu>, f: impl FnOnce(&Rc<SessionMenu>)) {
    if let Some(m) = weak.upgrade() {
        f(&m);
    }
}

#[cfg(test)]
mod tests {
    use super::{is_available, Action};

    #[test]
    fn logind_answers() {
        assert!(is_available("yes"));
        assert!(is_available("challenge"));
        assert!(!is_available("no"));
        assert!(!is_available("na"));
        assert!(!is_available(""));
    }

    #[test]
    fn lock_and_suspend_skip_confirmation() {
        for a in Action::ALL {
            assert_eq!(a.confirmation().is_none(), matches!(a, Action::Lock | Action::Suspend));
        }
    }
}
