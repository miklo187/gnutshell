//! The window menu: what a right click on a title bar, Alt+Space or a right
//! click on a taskbar entry opens.

use crate::ffi::{self, ClutterActor, MetaDisplay, MtkRectangle};
use crate::ui::{self, gobj};
use crate::windows;
use glib::prelude::*;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

const WIDTH: f32 = 290.0;
const PADDING: f32 = 6.0;
const ROW_HEIGHT: f32 = 32.0;
/// Space and line between groups.
const SEPARATOR_HEIGHT: f32 = 9.0;
/// Room for the check mark left of the labels.
const CHECK_WIDTH: f32 = 24.0;
const SEPARATOR_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x28);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Minimize,
    ToggleMaximize,
    ToggleAbove,
    ToggleSticky,
    MoveLeft,
    MoveRight,
    Close,
}

/// What the entries depend on.
#[derive(Clone, Copy, Debug, Default)]
pub struct Facts {
    can_minimize: bool,
    can_maximize: bool,
    maximized: bool,
    above: bool,
    on_all: bool,
    /// On all workspaces by its nature, which cannot be switched off.
    always_on_all: bool,
    can_close: bool,
    workspace: i32,
    workspaces: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    action: Action,
    label: &'static str,
    enabled: bool,
    checked: bool,
    /// A separator goes above this entry.
    new_group: bool,
}

pub fn entries(facts: Facts) -> Vec<Entry> {
    let entry = |action, label, enabled, checked| Entry { action, label, enabled, checked, new_group: false };
    let mut out = vec![
        entry(Action::Minimize, "Minimize", facts.can_minimize, false),
        entry(
            Action::ToggleMaximize,
            if facts.maximized { "Restore" } else { "Maximize" },
            facts.maximized || facts.can_maximize,
            false,
        ),
        // Mutter does not keep a maximized window above others.
        Entry {
            new_group: true,
            ..entry(Action::ToggleAbove, "Always on Top", !facts.maximized, facts.above && !facts.maximized)
        },
        entry(Action::ToggleSticky, "Always on Visible Workspace", !facts.always_on_all, facts.on_all),
    ];
    if !facts.on_all {
        if facts.workspace > 0 {
            out.push(entry(Action::MoveLeft, "Move to Workspace Left", true, false));
        }
        if facts.workspace >= 0 && facts.workspace + 1 < facts.workspaces {
            out.push(entry(Action::MoveRight, "Move to Workspace Right", true, false));
        }
    }
    out.push(Entry { new_group: true, ..entry(Action::Close, "Close", facts.can_close, false) });
    out
}

/// The next enabled entry from `selected` in direction `delta` (1 or -1),
/// wrapping around. With nothing selected, starts at that end.
fn next_enabled(enabled: &[bool], selected: Option<usize>, delta: isize) -> Option<usize> {
    let n = enabled.len() as isize;
    if n == 0 {
        return None;
    }
    let start = match selected {
        Some(s) => s as isize,
        None if delta > 0 => -1,
        None => n,
    };
    (1..=n).map(|i| (start + i * delta).rem_euclid(n) as usize).find(|&i| enabled[i])
}

/// Where a menu of the given size asked for at `x`, `y` goes inside `monitor`:
/// there if it fits, else moved left, and above the point if there is no room
/// below.
fn position(x: f32, y: f32, width: f32, height: f32, monitor: MtkRectangle) -> (f32, f32) {
    let (left, top) = (monitor.x as f32, monitor.y as f32);
    let (right, bottom) = (left + monitor.width as f32, top + monitor.height as f32);
    let x = x.min(right - width).max(left);
    let y = if y + height > bottom { y - height } else { y };
    (x.floor(), y.min(bottom - height).max(top).floor())
}

fn facts(display: *mut MetaDisplay, window: &glib::Object) -> Facts {
    let w = windows::ptr(window);
    unsafe {
        let workspace = ffi::meta_window_get_workspace(w);
        Facts {
            can_minimize: ffi::meta_window_can_minimize(w) != 0,
            can_maximize: ffi::meta_window_can_maximize(w) != 0,
            maximized: ffi::meta_window_is_maximized(w) != 0,
            above: ffi::meta_window_is_above(w) != 0,
            on_all: ffi::meta_window_is_on_all_workspaces(w) != 0,
            always_on_all: ffi::meta_window_is_always_on_all_workspaces(w) != 0,
            can_close: ffi::meta_window_can_close(w) != 0,
            workspace: if workspace.is_null() { -1 } else { ffi::meta_workspace_index(workspace) },
            workspaces: ffi::meta_workspace_manager_get_n_workspaces(ffi::meta_display_get_workspace_manager(
                display,
            )),
        }
    }
}

fn perform(display: *mut MetaDisplay, window: &glib::Object, action: Action) {
    // Read again: the window may have changed while the menu was open.
    let now = facts(display, window);
    let w = windows::ptr(window);
    unsafe {
        match action {
            Action::Minimize => ffi::meta_window_minimize(w),
            Action::ToggleMaximize if now.maximized => ffi::meta_window_unmaximize(w),
            Action::ToggleMaximize => ffi::meta_window_maximize(w),
            Action::ToggleAbove if now.above => ffi::meta_window_unmake_above(w),
            Action::ToggleAbove => ffi::meta_window_make_above(w),
            Action::ToggleSticky if now.on_all => ffi::meta_window_unstick(w),
            Action::ToggleSticky => ffi::meta_window_stick(w),
            Action::MoveLeft if now.workspace > 0 => {
                ffi::meta_window_change_workspace_by_index(w, now.workspace - 1, 0)
            }
            Action::MoveRight if now.workspace >= 0 && now.workspace + 1 < now.workspaces => {
                ffi::meta_window_change_workspace_by_index(w, now.workspace + 1, 0)
            }
            Action::MoveLeft | Action::MoveRight => {}
            Action::Close => {
                ffi::meta_window_delete(w, ffi::meta_display_get_current_time_roundtrip(display))
            }
        }
    }
}

#[derive(Default)]
struct State {
    grab: Option<*mut ffi::ClutterGrab>,
    /// The window the menu is for, and the watch for it going away.
    window: Option<(glib::Object, glib::SignalHandlerId)>,
    entries: Vec<Entry>,
    rows: Vec<*mut ClutterActor>,
    selected: Option<usize>,
}

pub struct WindowMenu {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    /// Covers the whole stage, to catch clicks outside the menu.
    overlay: *mut ClutterActor,
    menu: *mut ClutterActor,
    state: RefCell<State>,
}

impl WindowMenu {
    /// Build the (hidden) menu as the topmost child of `stage`.
    pub fn new(display: *mut MetaDisplay, stage: *mut ClutterActor) -> Rc<Self> {
        let this = unsafe {
            let overlay = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(overlay, 1);
            ffi::clutter_actor_hide(overlay);
            let menu = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(menu, &ui::BOX_COLOR);
            ffi::clutter_actor_set_reactive(menu, 1);
            ffi::clutter_actor_add_child(overlay, menu);
            ffi::clutter_actor_add_child(stage, overlay);
            Rc::new(Self { display, stage, overlay, menu, state: RefCell::default() })
        };
        let overlay = unsafe { gobj(this.overlay) };
        let weak = Rc::downgrade(&this);
        overlay.connect_local("key-press-event", false, move |args| {
            with(&weak, |m| m.on_key(ui::key_symbol(ui::event_arg(args))));
            Some(true.to_value())
        });
        let weak = Rc::downgrade(&this);
        overlay.connect_local("button-press-event", false, move |_| {
            with(&weak, |m| m.close());
            Some(true.to_value())
        });
        unsafe { gobj(this.menu) }.connect_local("button-press-event", false, |_| Some(true.to_value()));
        this
    }

    /// Show the menu of `window` at `x`, `y` in stage coordinates.
    pub fn open(self: &Rc<Self>, window: glib::Object, x: f32, y: f32) {
        self.close();
        let entries = entries(facts(self.display, &window));
        let height = 2.0 * PADDING
            + entries.len() as f32 * ROW_HEIGHT
            + entries.iter().filter(|e| e.new_group).count() as f32 * SEPARATOR_HEIGHT;

        let mut rows = Vec::new();
        let mut top = PADDING;
        for (index, entry) in entries.iter().enumerate() {
            if entry.new_group {
                unsafe {
                    let line = ffi::clutter_actor_new();
                    ffi::clutter_actor_set_background_color(line, &SEPARATOR_COLOR);
                    ffi::clutter_actor_set_size(line, WIDTH - 2.0 * PADDING, 1.0);
                    ffi::clutter_actor_set_position(line, PADDING, top + (SEPARATOR_HEIGHT / 2.0).floor());
                    ffi::clutter_actor_add_child(self.menu, line);
                }
                top += SEPARATOR_HEIGHT;
            }
            let row = self.new_row(index, entry);
            unsafe { ffi::clutter_actor_set_position(row, PADDING, top) };
            rows.push(row);
            top += ROW_HEIGHT;
        }

        let weak = Rc::downgrade(self);
        let watch = window.connect_local("unmanaged", false, move |_| {
            with(&weak, |m| m.close());
            None
        });
        {
            let mut state = self.state.borrow_mut();
            state.window = Some((window, watch));
            state.entries = entries;
            state.rows = rows;
            state.selected = None;
        }

        unsafe {
            let (stage_width, stage_height) =
                (ffi::clutter_actor_get_width(self.stage), ffi::clutter_actor_get_height(self.stage));
            ffi::clutter_actor_set_position(self.overlay, 0.0, 0.0);
            ffi::clutter_actor_set_size(self.overlay, stage_width, stage_height);
            let (left, top) = position(x, y, WIDTH, height, self.monitor_at(x, y));
            ffi::clutter_actor_set_position(self.menu, left, top);
            ffi::clutter_actor_set_size(self.menu, WIDTH, height);
            ffi::clutter_actor_show(self.overlay);
            let grab = ffi::clutter_stage_grab(self.stage as *mut ffi::ClutterStage, self.overlay);
            self.state.borrow_mut().grab = Some(grab);
            ffi::clutter_actor_grab_key_focus(self.overlay);
        }
    }

    pub fn close(&self) {
        let (grab, window) = {
            let mut state = self.state.borrow_mut();
            state.entries.clear();
            state.rows.clear();
            state.selected = None;
            (state.grab.take(), state.window.take())
        };
        if let Some((window, watch)) = window {
            window.disconnect(watch);
        }
        let Some(grab) = grab else { return };
        unsafe {
            ffi::clutter_grab_dismiss(grab);
            gobject_sys::g_object_unref(grab as *mut gobject_sys::GObject);
            ffi::clutter_actor_hide(self.overlay);
            ffi::clutter_actor_destroy_all_children(self.menu);
        }
    }

    pub fn destroy(&self) {
        self.close();
        unsafe { ffi::clutter_actor_destroy(self.overlay) };
    }

    // ------------------------------------------------------------------ internals

    /// The monitor containing the point, or else the primary one.
    fn monitor_at(&self, x: f32, y: f32) -> MtkRectangle {
        unsafe {
            for monitor in 0..ffi::meta_display_get_n_monitors(self.display) {
                let mut r = MtkRectangle::default();
                ffi::meta_display_get_monitor_geometry(self.display, monitor, &mut r);
                let inside = |p: f32, start: i32, length: i32| p >= start as f32 && p < (start + length) as f32;
                if inside(x, r.x, r.width) && inside(y, r.y, r.height) {
                    return r;
                }
            }
        }
        ui::primary_monitor_geometry(self.display).unwrap_or_default()
    }

    fn new_row(self: &Rc<Self>, index: usize, entry: &Entry) -> *mut ClutterActor {
        let row = unsafe {
            let row = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(row, WIDTH - 2.0 * PADDING, ROW_HEIGHT);
            ffi::clutter_actor_set_reactive(row, 1);
            ffi::clutter_actor_add_child(self.menu, row);
            let color = if entry.enabled { &ui::TEXT_COLOR } else { &ui::HINT_COLOR };
            for (text, x, shown) in [("✓", 8.0, entry.checked), (entry.label, 8.0 + CHECK_WIDTH, true)] {
                if shown {
                    let label = ui::label(ui::ROW_FONT, text, color);
                    ffi::clutter_actor_add_child(row, label);
                    let y = (ROW_HEIGHT - ffi::clutter_actor_get_height(label)) / 2.0;
                    ffi::clutter_actor_set_position(label, x, y.floor());
                }
            }
            row
        };
        let object = unsafe { gobj(row) };
        let weak = Rc::downgrade(self);
        object.connect_local("enter-event", false, move |_| {
            with(&weak, |m| {
                let enabled = m.state.borrow().entries.get(index).is_some_and(|e| e.enabled);
                m.select(enabled.then_some(index));
            });
            Some(false.to_value())
        });
        let weak = Rc::downgrade(self);
        object.connect_local("button-press-event", false, move |_| {
            with(&weak, |m| m.choose(index));
            Some(true.to_value())
        });
        row
    }

    fn select(&self, selected: Option<usize>) {
        let mut state = self.state.borrow_mut();
        state.selected = selected;
        for (index, row) in state.rows.iter().enumerate() {
            let color = if selected == Some(index) { &ui::SELECTED_COLOR } else { &ui::TRANSPARENT };
            unsafe { ffi::clutter_actor_set_background_color(*row, color) };
        }
    }

    fn choose(&self, index: usize) {
        let chosen = {
            let state = self.state.borrow();
            let entry = state.entries.get(index).filter(|e| e.enabled);
            entry.zip(state.window.as_ref()).map(|(e, (w, _))| (e.action, w.clone()))
        };
        let Some((action, window)) = chosen else { return };
        // First, so that the window can take the focus.
        self.close();
        perform(self.display, &window, action);
    }

    fn on_key(&self, keysym: u32) {
        match keysym {
            ui::KEY_ESCAPE => self.close(),
            ui::KEY_UP | ui::KEY_DOWN => {
                let next = {
                    let state = self.state.borrow();
                    let enabled: Vec<bool> = state.entries.iter().map(|e| e.enabled).collect();
                    next_enabled(&enabled, state.selected, if keysym == ui::KEY_UP { -1 } else { 1 })
                };
                self.select(next);
            }
            ui::KEY_RETURN | ui::KEY_KP_ENTER => {
                let selected = self.state.borrow().selected;
                if let Some(index) = selected {
                    self.choose(index);
                }
            }
            _ => {}
        }
    }
}

fn with(weak: &Weak<WindowMenu>, f: impl FnOnce(&Rc<WindowMenu>)) {
    if let Some(menu) = weak.upgrade() {
        f(&menu);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL: Facts = Facts {
        can_minimize: true,
        can_maximize: true,
        maximized: false,
        above: false,
        on_all: false,
        always_on_all: false,
        can_close: true,
        workspace: 1,
        workspaces: 3,
    };

    fn find(facts: Facts, action: Action) -> Option<Entry> {
        entries(facts).into_iter().find(|e| e.action == action)
    }

    #[test]
    fn offers_the_moves_that_exist() {
        assert!(find(NORMAL, Action::MoveLeft).is_some());
        assert!(find(NORMAL, Action::MoveRight).is_some());
        assert!(find(Facts { workspace: 0, ..NORMAL }, Action::MoveLeft).is_none());
        assert!(find(Facts { workspace: 2, ..NORMAL }, Action::MoveRight).is_none());
        let sticky = Facts { on_all: true, ..NORMAL };
        assert!(find(sticky, Action::MoveLeft).is_none());
        assert!(find(sticky, Action::ToggleSticky).unwrap().checked);
    }

    #[test]
    fn a_maximized_window_cannot_be_kept_on_top() {
        let above = find(Facts { above: true, ..NORMAL }, Action::ToggleAbove).unwrap();
        assert!(above.enabled && above.checked);
        let maximized = Facts { above: true, maximized: true, ..NORMAL };
        let above = find(maximized, Action::ToggleAbove).unwrap();
        assert!(!above.enabled && !above.checked);
        assert_eq!(find(maximized, Action::ToggleMaximize).unwrap().label, "Restore");
    }

    #[test]
    fn keys_skip_disabled_entries() {
        let enabled = [false, true, false, true];
        assert_eq!(next_enabled(&enabled, None, 1), Some(1));
        assert_eq!(next_enabled(&enabled, None, -1), Some(3));
        assert_eq!(next_enabled(&enabled, Some(1), 1), Some(3));
        assert_eq!(next_enabled(&enabled, Some(3), 1), Some(1));
        assert_eq!(next_enabled(&[false], None, 1), None);
    }

    #[test]
    fn the_menu_stays_on_the_monitor() {
        let monitor = MtkRectangle { x: 100, y: 0, width: 1000, height: 700 };
        assert_eq!(position(200.0, 100.0, 300.0, 200.0, monitor), (200.0, 100.0));
        assert_eq!(position(1000.0, 680.0, 300.0, 200.0, monitor), (800.0, 480.0));
        assert_eq!(position(0.0, 100.0, 300.0, 900.0, monitor), (100.0, 0.0));
    }
}
