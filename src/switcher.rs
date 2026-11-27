//! Alt+Tab / Super+Tab window switcher.
//!
//! Takes over mutter's `switch-applications` and `switch-windows` keybindings
//! (and their `-backward` variants). While the modifier is held, Tab / arrows
//! cycle through windows in MRU order; releasing the modifier activates the
//! selection, Escape cancels. The popup appears only after a short delay so a
//! quick Alt+Tab just flips to the previous window without flashing UI.

use crate::ffi::{self, ClutterActor, MetaDisplay, MetaWindow};
use crate::ui::{self, gobj};
use crate::windows;
use glib::prelude::*;
use std::cell::RefCell;
use std::ffi::CStr;
use std::rc::{Rc, Weak};
use std::time::Duration;

const POPUP_DELAY: Duration = Duration::from_millis(120);
const BOX_WIDTH: f32 = 520.0;

const BINDINGS: [&CStr; 4] = [
    c"switch-applications",
    c"switch-applications-backward",
    c"switch-windows",
    c"switch-windows-backward",
];

pub struct Switcher {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    overlay: *mut ClutterActor,
    panel: *mut ClutterActor,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    /// Strong refs keep the MetaWindows alive while the popup is open.
    windows: Vec<glib::Object>,
    selected: usize,
    grab: Option<*mut ffi::ClutterGrab>,
    popup_timeout: Option<glib::SourceId>,
}

impl Switcher {
    pub fn new(display: *mut MetaDisplay, stage: *mut ClutterActor) -> Rc<Self> {
        let switcher = unsafe {
            // Transparent full-monitor actor: holds the grab and key focus, and
            // catches clicks outside the box.
            let overlay = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(overlay, 1);
            ffi::clutter_actor_hide(overlay);

            let panel = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(panel, &ui::BOX_COLOR);
            ffi::clutter_actor_set_reactive(panel, 1);
            ffi::clutter_actor_add_child(overlay, panel);

            ffi::clutter_actor_add_child(stage, overlay);
            Rc::new(Self { display, stage, overlay, panel, state: RefCell::default() })
        };
        switcher.connect_signals();
        switcher.install_keybindings();
        switcher
    }

    fn connect_signals(self: &Rc<Self>) {
        let overlay = unsafe { gobj(self.overlay) };
        let panel = unsafe { gobj(self.panel) };

        let weak = Rc::downgrade(self);
        overlay.connect_local("key-press-event", false, move |args| {
            let handled = weak.upgrade().is_some_and(|s| s.on_key_press(ui::key_symbol(ui::event_arg(args))));
            Some(handled.to_value())
        });

        let weak = Rc::downgrade(self);
        overlay.connect_local("key-release-event", false, move |args| {
            if ui::KEY_MODIFIERS.contains(&ui::key_symbol(ui::event_arg(args))) {
                with(&weak, |s| s.finish());
            }
            Some(true.to_value())
        });

        let weak = Rc::downgrade(self);
        overlay.connect_local("button-press-event", false, move |_| {
            with(&weak, |s| s.hide());
            Some(true.to_value())
        });
        panel.connect_local("button-press-event", false, |_| Some(true.to_value()));
    }

    fn install_keybindings(self: &Rc<Self>) {
        for name in BINDINGS {
            // Each handler owns a Weak; mutter frees it if the handler is replaced.
            let data = Weak::into_raw(Rc::downgrade(self)) as glib_sys::gpointer;
            let ok = unsafe {
                ffi::meta_keybindings_set_custom_handler(
                    name.as_ptr(),
                    Some(on_switch_binding),
                    data,
                    Some(drop_weak),
                )
            };
            if ok == 0 {
                unsafe { drop_weak(data) };
                glib::g_warning!("gnutshell", "could not take over keybinding {name:?}");
            }
        }
    }

    pub fn is_active(&self) -> bool {
        self.state.borrow().grab.is_some()
    }

    /// Start switching (or advance, if already active).
    pub fn start(self: &Rc<Self>, backward: bool) {
        if self.is_active() {
            self.step(if backward { -1 } else { 1 });
            return;
        }

        let windows = self.tab_list();
        match windows.len() {
            0 => return,
            1 => {
                // Nothing to switch to; still bring back a minimized window.
                self.activate(&windows[0]);
                return;
            }
            _ => {}
        }

        let n = windows.len();
        {
            let mut state = self.state.borrow_mut();
            state.selected = if backward { n - 1 } else { 1 };
            state.windows = windows;
        }

        unsafe {
            self.layout();
            ffi::clutter_actor_hide(self.panel);
            ffi::clutter_actor_show(self.overlay);
            let grab = ffi::clutter_stage_grab(self.stage as *mut ffi::ClutterStage, self.overlay);
            self.state.borrow_mut().grab = Some(grab);
            ffi::clutter_actor_grab_key_focus(self.overlay);
        }

        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(POPUP_DELAY, move || {
            with(&weak, |s| {
                s.state.borrow_mut().popup_timeout = None;
                s.render();
                unsafe { ffi::clutter_actor_show(s.panel) };
            });
        });
        self.state.borrow_mut().popup_timeout = Some(id);
    }

    /// Activate the selected window and close.
    pub fn finish(&self) {
        let window = {
            let state = self.state.borrow();
            state.windows.get(state.selected).cloned()
        };
        self.hide();
        let Some(window) = window else { return };
        // The window may have been closed while the popup was open.
        if self.tab_list().iter().any(|w| w == &window) {
            self.activate(&window);
        }
    }

    pub fn hide(&self) {
        let (grab, timeout) = {
            let mut state = self.state.borrow_mut();
            state.windows.clear();
            (state.grab.take(), state.popup_timeout.take())
        };
        if let Some(id) = timeout {
            id.remove();
        }
        let Some(grab) = grab else { return };
        unsafe {
            ffi::clutter_grab_dismiss(grab);
            gobject_sys::g_object_unref(grab as *mut gobject_sys::GObject);
            ffi::clutter_actor_hide(self.overlay);
            ffi::clutter_actor_destroy_all_children(self.panel);
        }
    }

    pub fn destroy(&self) {
        self.hide();
        unsafe { ffi::clutter_actor_destroy(self.overlay) };
    }

    // ------------------------------------------------------------------ internals

    fn on_key_press(&self, keysym: u32) -> bool {
        match keysym {
            ui::KEY_TAB | ui::KEY_RIGHT | ui::KEY_DOWN => self.step(1),
            ui::KEY_ISO_LEFT_TAB | ui::KEY_LEFT | ui::KEY_UP => self.step(-1),
            ui::KEY_RETURN | ui::KEY_KP_ENTER => self.finish(),
            ui::KEY_ESCAPE => self.hide(),
            _ => {}
        }
        true // swallow everything while switching
    }

    fn step(&self, delta: isize) {
        {
            let mut state = self.state.borrow_mut();
            let n = state.windows.len();
            if n == 0 {
                return;
            }
            state.selected = (state.selected as isize + delta).rem_euclid(n as isize) as usize;
        }
        if self.state.borrow().popup_timeout.is_none() {
            self.render();
        }
    }

    fn tab_list(&self) -> Vec<glib::Object> {
        unsafe {
            let wm = ffi::meta_display_get_workspace_manager(self.display);
            let ws = ffi::meta_workspace_manager_get_active_workspace(wm);
            let list = ffi::meta_display_get_tab_list(self.display, ffi::META_TAB_LIST_NORMAL, ws);
            let mut out = Vec::new();
            let mut node = list;
            while !node.is_null() {
                out.push(gobj((*node).data as *mut MetaWindow));
                node = (*node).next;
            }
            glib_sys::g_list_free(list);
            out
        }
    }

    fn activate(&self, window: &glib::Object) {
        unsafe {
            let time = ffi::meta_display_get_current_time_roundtrip(self.display);
            ffi::meta_window_activate(window.as_ptr() as *mut MetaWindow, time);
        }
    }

    unsafe fn layout(&self) {
        unsafe {
            let geo = ui::primary_monitor_geometry(self.display).unwrap_or_default();
            ffi::clutter_actor_set_position(self.overlay, geo.x as f32, geo.y as f32);
            ffi::clutter_actor_set_size(self.overlay, geo.width as f32, geo.height as f32);
        }
    }

    fn render(&self) {
        let state = self.state.borrow();
        let scale = ui::primary_monitor_scale(self.display);
        let titles: Vec<String> = state.windows.iter().map(windows::title).collect();
        let icons: Vec<Option<glib::Object>> =
            state.windows.iter().map(|w| windows::icon(w, ui::ROW_ICON_SIZE, scale, self.stage)).collect();
        let selected = state.selected;
        drop(state);

        unsafe {
            let mon_w = ffi::clutter_actor_get_width(self.overlay);
            let mon_h = ffi::clutter_actor_get_height(self.overlay);
            let box_w = BOX_WIDTH.min(mon_w - 32.0);
            let inner_w = box_w - 2.0 * ui::BOX_PADDING;

            // Show a window of rows that keeps the selection visible.
            let max_rows = (((mon_h * 0.7) / ui::ROW_HEIGHT) as usize).max(1);
            let first = visible_start(titles.len(), max_rows, selected);
            let shown = titles.len().min(max_rows);

            ffi::clutter_actor_destroy_all_children(self.panel);
            for (slot, index) in (first..first + shown).enumerate() {
                let row = ui::list_row(&titles[index], icons[index].as_ref(), inner_w, index == selected);
                ffi::clutter_actor_set_position(
                    row,
                    ui::BOX_PADDING,
                    ui::BOX_PADDING + slot as f32 * ui::ROW_HEIGHT,
                );
                ffi::clutter_actor_add_child(self.panel, row);
            }

            let box_h = 2.0 * ui::BOX_PADDING + shown as f32 * ui::ROW_HEIGHT;
            ffi::clutter_actor_set_size(self.panel, box_w, box_h);
            ffi::clutter_actor_set_position(
                self.panel,
                ((mon_w - box_w) / 2.0).floor(),
                ((mon_h - box_h) / 2.0).floor(),
            );
        }
    }
}

/// First visible index for a scrolled list of `len` rows showing `max` rows,
/// keeping `selected` roughly centered.
fn visible_start(len: usize, max: usize, selected: usize) -> usize {
    if len <= max {
        return 0;
    }
    selected.saturating_sub(max / 2).min(len - max)
}

fn with(weak: &Weak<Switcher>, f: impl FnOnce(&Rc<Switcher>)) {
    if let Some(s) = weak.upgrade() {
        f(&s);
    }
}

unsafe extern "C" fn on_switch_binding(
    _display: *mut MetaDisplay,
    _window: *mut MetaWindow,
    _event: *const ffi::ClutterEvent,
    binding: *mut ffi::MetaKeyBinding,
    data: glib_sys::gpointer,
) {
    // `data` is a `Weak::into_raw` pointer: rebuild without taking ownership.
    let weak = unsafe { Weak::from_raw(data as *const Switcher) };
    let backward = unsafe { ffi::meta_key_binding_is_reversed(binding) } != 0;
    if let Some(s) = weak.upgrade() {
        s.start(backward);
    }
    std::mem::forget(weak);
}

unsafe extern "C" fn drop_weak(data: glib_sys::gpointer) {
    drop(unsafe { Weak::from_raw(data as *const Switcher) });
}

#[cfg(test)]
mod tests {
    use super::visible_start;

    #[test]
    fn short_list_starts_at_zero() {
        assert_eq!(visible_start(3, 10, 2), 0);
    }

    #[test]
    fn long_list_keeps_selection_visible() {
        for sel in 0..20 {
            let first = visible_start(20, 5, sel);
            assert!(first <= sel && sel < first + 5, "sel {sel} first {first}");
            assert!(first + 5 <= 20);
        }
    }
}
