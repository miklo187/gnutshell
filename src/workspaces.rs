//! Workspaces: panel indicator, switch animation and (when
//! `org.gnome.mutter dynamic-workspaces` is on) gnome-shell's dynamic
//! workspace management.
//!
//! mutter handles workspaces themselves and the keybindings
//! (Super+PageUp/PageDown, Ctrl+Alt+arrows, move-to-workspace, …). With
//! dynamic workspaces it relies on the shell to add and remove them; like
//! gnome-shell, gnative keeps exactly one empty workspace at the end (and at
//! least two in total), removes other empty ones, and never removes the
//! active one.

use crate::ffi::{self, ClutterActor, MetaDisplay, MetaWorkspace};
use crate::ui::{self, gobj};
use gio::prelude::*;
use glib::translate::*;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::Duration;

const MIN_WORKSPACES: usize = 2;
const DOT: f32 = 8.0;
const ACTIVE_DOT: f32 = 22.0;
const DOT_GAP: f32 = 6.0;
const INDICATOR_PADDING: f32 = 10.0;
const DOT_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x70);
const SWITCH_MS: u32 = 250;

/// What to change so that `empty` (per workspace: has no windows) follows
/// gnome-shell's rules. Returns how many workspaces to append and which
/// indices to remove, highest first.
pub fn plan(empty: &[bool], active: usize) -> (usize, Vec<usize>) {
    let mut empty = empty.to_vec();
    let mut append = 0;
    if empty.last() != Some(&true) {
        empty.push(true);
        append += 1;
    }
    while empty.len() < MIN_WORKSPACES {
        empty.push(true);
        append += 1;
    }
    let last_empty = empty.iter().rposition(|e| !e).map_or(0, |i| i + 1);
    if let Some(a) = empty.get_mut(active) {
        *a = false;
    }
    let mut remaining = empty.len();
    let mut remove = Vec::new();
    for i in (0..empty.len()).rev() {
        if remaining == MIN_WORKSPACES {
            break;
        }
        if empty[i] && i != last_empty {
            remove.push(i);
            remaining -= 1;
        }
    }
    // Indices refer to the list after appending; appended ones are never
    // removed (they are at or after `last_empty`).
    (append, remove)
}

pub struct Workspaces {
    display: *mut MetaDisplay,
    indicator: *mut ClutterActor,
    mutter: Option<gio::Settings>,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    check_pending: Option<glib::SourceId>,
    /// (workspace object, its window-added / window-removed handlers)
    workspace_handlers: Vec<(glib::Object, Vec<glib::SignalHandlerId>)>,
    manager_handlers: Vec<glib::SignalHandlerId>,
    scroll_acc: f64,
}

impl Workspaces {
    pub fn new(display: *mut MetaDisplay, panel: *mut ClutterActor) -> Rc<Self> {
        let mutter = gio::SettingsSchemaSource::default()
            .and_then(|src| src.lookup("org.gnome.mutter", true))
            .map(|_| gio::Settings::new("org.gnome.mutter"));
        let ws = unsafe {
            let indicator = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(indicator, 1);
            ffi::clutter_actor_add_child(panel, indicator);
            Rc::new(Self { display, indicator, mutter, state: RefCell::default() })
        };
        ws.connect_signals();
        ws.track_workspaces();
        ws.queue_check();
        ws.render();
        ws
    }

    fn manager(&self) -> *mut ffi::MetaWorkspaceManager {
        unsafe { ffi::meta_display_get_workspace_manager(self.display) }
    }

    fn manager_obj(&self) -> glib::Object {
        unsafe { from_glib_none(self.manager() as *mut gobject_sys::GObject) }
    }

    fn connect_signals(self: &Rc<Self>) {
        let manager = self.manager_obj();
        let mut handlers = Vec::new();
        for signal in ["workspace-added", "workspace-removed", "workspace-switched", "notify::n-workspaces"] {
            let weak = Rc::downgrade(self);
            handlers.push(manager.connect_local(signal, false, move |_| {
                with(&weak, |w| {
                    w.track_workspaces();
                    w.queue_check();
                    w.render();
                });
                None
            }));
        }
        self.state.borrow_mut().manager_handlers = handlers;

        if let Some(settings) = &self.mutter {
            let weak = Rc::downgrade(self);
            settings.connect_changed(Some("dynamic-workspaces"), move |_, _| with(&weak, |w| w.queue_check()));
        }

        let indicator = unsafe { gobj(self.indicator) };
        let weak = Rc::downgrade(self);
        indicator.connect_local("scroll-event", false, move |args| {
            with(&weak, |w| {
                let mut acc = w.state.borrow().scroll_acc;
                let rows = ui::scroll_rows(ui::event_arg(args), &mut acc);
                w.state.borrow_mut().scroll_acc = acc;
                if rows != 0 {
                    w.step(rows.signum());
                }
            });
            Some(true.to_value())
        });
    }

    /// Watch every workspace's window-added / window-removed.
    fn track_workspaces(self: &Rc<Self>) {
        let mut state = self.state.borrow_mut();
        for (obj, ids) in state.workspace_handlers.drain(..) {
            for id in ids {
                obj.disconnect(id);
            }
        }
        let wm = self.manager();
        let n = unsafe { ffi::meta_workspace_manager_get_n_workspaces(wm) };
        for i in 0..n {
            let ws = unsafe { ffi::meta_workspace_manager_get_workspace_by_index(wm, i) };
            if ws.is_null() {
                continue;
            }
            let obj: glib::Object = unsafe { from_glib_none(ws as *mut gobject_sys::GObject) };
            let ids = ["window-added", "window-removed"]
                .into_iter()
                .map(|signal| {
                    let weak = Rc::downgrade(self);
                    obj.connect_local(signal, false, move |_| {
                        with(&weak, |w| {
                            w.queue_check();
                            w.render();
                        });
                        None
                    })
                })
                .collect();
            state.workspace_handlers.push((obj, ids));
        }
    }

    fn dynamic(&self) -> bool {
        self.mutter.as_ref().is_some_and(|s| s.boolean("dynamic-workspaces"))
    }

    /// Coalesce bursts of window / workspace changes into one check.
    fn queue_check(self: &Rc<Self>) {
        if self.state.borrow().check_pending.is_some() || !self.dynamic() {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = glib::idle_add_local_once(move || {
            with(&weak, |w| {
                w.state.borrow_mut().check_pending = None;
                w.check_workspaces();
            });
        });
        self.state.borrow_mut().check_pending = Some(id);
    }

    fn check_workspaces(&self) {
        if !self.dynamic() {
            return;
        }
        let wm = self.manager();
        let n = unsafe { ffi::meta_workspace_manager_get_n_workspaces(wm) } as usize;
        let mut empty = vec![true; n];
        unsafe {
            let compositor = ffi::meta_display_get_compositor(self.display);
            let list = ffi::meta_compositor_get_window_actors(compositor);
            let mut node = list;
            while !node.is_null() {
                let window = ffi::meta_window_actor_get_meta_window((*node).data as *mut ClutterActor);
                node = (*node).next;
                if window.is_null() || ffi::meta_window_is_on_all_workspaces(window) != 0 {
                    continue;
                }
                let ws = ffi::meta_window_get_workspace(window);
                if !ws.is_null() {
                    let i = ffi::meta_workspace_index(ws);
                    if i >= 0 && (i as usize) < n {
                        empty[i as usize] = false;
                    }
                }
            }
        }
        let active = unsafe { ffi::meta_workspace_manager_get_active_workspace_index(wm) }.max(0) as usize;
        let (append, remove) = plan(&empty, active);
        if append == 0 && remove.is_empty() {
            return;
        }
        let time = unsafe { ffi::meta_display_get_current_time_roundtrip(self.display) };
        unsafe {
            for _ in 0..append {
                ffi::meta_workspace_manager_append_new_workspace(wm, 0, time);
            }
            for i in remove {
                let ws = ffi::meta_workspace_manager_get_workspace_by_index(wm, i as i32);
                if !ws.is_null() {
                    ffi::meta_workspace_manager_remove_workspace(wm, ws, time);
                }
            }
        }
    }

    /// Switch to the workspace `delta` away from the active one.
    pub fn step(&self, delta: i32) {
        let wm = self.manager();
        unsafe {
            let n = ffi::meta_workspace_manager_get_n_workspaces(wm);
            let target = ffi::meta_workspace_manager_get_active_workspace_index(wm) + delta;
            if (0..n).contains(&target) {
                self.activate(ffi::meta_workspace_manager_get_workspace_by_index(wm, target));
            }
        }
    }

    fn activate(&self, ws: *mut MetaWorkspace) {
        if ws.is_null() || crate::lockscreen::is_locked() {
            return;
        }
        unsafe {
            let time = ffi::meta_display_get_current_time_roundtrip(self.display);
            ffi::meta_workspace_activate(ws, time);
        }
    }

    /// Dots, one per workspace; the active one is a wider pill. Hidden with
    /// a single workspace.
    fn render(self: &Rc<Self>) {
        let wm = self.manager();
        let (n, active) = unsafe {
            (
                ffi::meta_workspace_manager_get_n_workspaces(wm),
                ffi::meta_workspace_manager_get_active_workspace_index(wm),
            )
        };
        unsafe {
            ffi::clutter_actor_destroy_all_children(self.indicator);
            if n < 2 {
                ffi::clutter_actor_hide(self.indicator);
                return;
            }
            let height = ffi::clutter_actor_get_height(ffi::clutter_actor_get_parent(self.indicator));
            let mut x = INDICATOR_PADDING;
            for i in 0..n {
                let w = if i == active { ACTIVE_DOT } else { DOT };
                // A taller, transparent hit area around each dot.
                let hit = ffi::clutter_actor_new();
                ffi::clutter_actor_set_reactive(hit, 1);
                ffi::clutter_actor_set_size(hit, w + DOT_GAP, height);
                ffi::clutter_actor_set_position(hit, x - DOT_GAP / 2.0, 0.0);
                let dot = ffi::clutter_actor_new();
                let color = if i == active { &ui::TEXT_COLOR } else { &DOT_COLOR };
                ffi::clutter_actor_set_background_color(dot, color);
                ffi::clutter_actor_set_size(dot, w, DOT);
                ffi::clutter_actor_set_position(dot, DOT_GAP / 2.0, ((height - DOT) / 2.0).floor());
                ffi::clutter_actor_add_child(hit, dot);
                ffi::clutter_actor_add_child(self.indicator, hit);
                let weak = Rc::downgrade(self);
                gobj(hit).connect_local("button-press-event", false, move |_| {
                    with(&weak, |w| {
                        w.activate(ffi::meta_workspace_manager_get_workspace_by_index(w.manager(), i));
                    });
                    Some(true.to_value())
                });
                x += w + DOT_GAP;
            }
            ffi::clutter_actor_set_size(self.indicator, x - DOT_GAP + INDICATOR_PADDING, height);
            ffi::clutter_actor_set_position(self.indicator, 0.0, 0.0);
            ffi::clutter_actor_show(self.indicator);
        }
    }

    pub fn relayout(self: &Rc<Self>) {
        self.render();
    }

    pub fn destroy(&self) {
        let mut state = self.state.borrow_mut();
        if let Some(id) = state.check_pending.take() {
            id.remove();
        }
        for (obj, ids) in state.workspace_handlers.drain(..) {
            for id in ids {
                obj.disconnect(id);
            }
        }
        let manager = self.manager_obj();
        for id in state.manager_handlers.drain(..) {
            manager.disconnect(id);
        }
        drop(state);
        unsafe { ffi::clutter_actor_destroy(self.indicator) };
    }
}

// ---------------------------------------------------------------- switch animation

/// A running workspace switch animation.
pub struct SwitchAnimation {
    container: *mut ClutterActor,
    window_group: *mut ClutterActor,
    done: RefCell<Option<Box<dyn FnOnce()>>>,
    safety: RefCell<Option<glib::SourceId>>,
}

impl SwitchAnimation {
    /// Slide from workspace `from` to `to` (indices). Clones of the windows
    /// on both workspaces move while the real window group is hidden; `done`
    /// runs when finished or killed. `None` means nothing to animate (call
    /// `done` yourself).
    pub fn start(
        display: *mut MetaDisplay,
        from: i32,
        to: i32,
        done: Box<dyn FnOnce()>,
    ) -> Result<Rc<Self>, Box<dyn FnOnce()>> {
        let Some(geo) = ui::primary_monitor_geometry(display) else { return Err(done) };
        if from == to || crate::lockscreen::is_locked() {
            return Err(done);
        }
        unsafe {
            let compositor = ffi::meta_display_get_compositor(display);
            let stage = ffi::meta_compositor_get_stage(compositor) as *mut ClutterActor;
            let window_group = ffi::meta_compositor_get_window_group(compositor);
            let wm = ffi::meta_display_get_workspace_manager(display);
            let from_ws = ffi::meta_workspace_manager_get_workspace_by_index(wm, from);
            let to_ws = ffi::meta_workspace_manager_get_workspace_by_index(wm, to);

            let container = ffi::clutter_actor_new();
            let sticky = ffi::clutter_actor_new();
            let out_group = ffi::clutter_actor_new();
            let in_group = ffi::clutter_actor_new();
            for g in [out_group, in_group, sticky] {
                ffi::clutter_actor_add_child(container, g);
            }

            // Window actors come bottom-to-top, so clones keep the stacking.
            let list = ffi::meta_compositor_get_window_actors(compositor);
            let mut node = list;
            while !node.is_null() {
                let actor = (*node).data as *mut ClutterActor;
                node = (*node).next;
                let window = ffi::meta_window_actor_get_meta_window(actor);
                if window.is_null() || ffi::meta_window_showing_on_its_workspace(window) == 0 {
                    continue;
                }
                let group = if ffi::meta_window_is_on_all_workspaces(window) != 0 {
                    sticky
                } else {
                    match ffi::meta_window_get_workspace(window) {
                        ws if ws == from_ws => out_group,
                        ws if ws == to_ws => in_group,
                        _ => continue,
                    }
                };
                let clone = ffi::clutter_clone_new(actor);
                let (mut x, mut y) = (0.0, 0.0);
                ffi::clutter_actor_get_position(actor, &mut x, &mut y);
                ffi::clutter_actor_set_position(clone, x, y);
                ffi::clutter_actor_add_child(group, clone);
            }

            // Next workspace (higher index) comes in from the right.
            let dx = geo.width as f32 * if to > from { 1.0 } else { -1.0 };
            ffi::clutter_actor_set_x(in_group, dx);
            ffi::clutter_actor_insert_child_above(stage, container, window_group);
            ffi::clutter_actor_hide(window_group);

            let anim = Rc::new(Self {
                container,
                window_group,
                done: RefCell::new(Some(done)),
                safety: RefCell::default(),
            });

            for (group, target) in [(out_group, -dx), (in_group, 0.0)] {
                ffi::clutter_actor_save_easing_state(group);
                ffi::clutter_actor_set_easing_mode(group, ffi::CLUTTER_EASE_OUT_CUBIC);
                ffi::clutter_actor_set_easing_duration(group, SWITCH_MS);
                ffi::clutter_actor_set_x(group, target);
                ffi::clutter_actor_restore_easing_state(group);
            }
            let weak = Rc::downgrade(&anim);
            gobj(in_group).connect_local("transitions-completed", false, move |_| {
                if let Some(a) = weak.upgrade() {
                    // Defer: this handler runs inside the actor being destroyed.
                    glib::idle_add_local_once(move || a.finish());
                }
                None
            });
            // Never leave mutter waiting, whatever happens to the transition.
            let weak = Rc::downgrade(&anim);
            let id = glib::timeout_add_local_once(Duration::from_millis(SWITCH_MS as u64 + 500), move || {
                if let Some(a) = weak.upgrade() {
                    a.safety.borrow_mut().take();
                    a.finish();
                }
            });
            *anim.safety.borrow_mut() = Some(id);
            Ok(anim)
        }
    }

    /// End now (also used to kill the animation).
    pub fn finish(&self) {
        let Some(done) = self.done.borrow_mut().take() else { return };
        if let Some(id) = self.safety.borrow_mut().take() {
            id.remove();
        }
        unsafe {
            ffi::clutter_actor_show(self.window_group);
            ffi::clutter_actor_destroy(self.container);
        }
        done();
    }
}

fn with(weak: &Weak<Workspaces>, f: impl FnOnce(&Rc<Workspaces>)) {
    if let Some(w) = weak.upgrade() {
        f(&w);
    }
}

#[cfg(test)]
mod tests {
    use super::plan;

    #[test]
    fn appends_empty_workspace_at_end() {
        // [busy, busy] -> add one
        assert_eq!(plan(&[false, false], 0), (1, vec![]));
    }

    #[test]
    fn keeps_single_trailing_empty() {
        assert_eq!(plan(&[false, true], 0), (0, vec![]));
        // [busy, empty, empty] -> drop the extra trailing one
        assert_eq!(plan(&[false, true, true], 0), (0, vec![2]));
    }

    #[test]
    fn removes_empty_in_middle_but_not_active() {
        // [busy, empty, busy, empty], active = 0 -> remove 1
        assert_eq!(plan(&[false, true, false, true], 0), (0, vec![1]));
        // same but the empty middle one is active -> keep it
        assert_eq!(plan(&[false, true, false, true], 1), (0, vec![]));
    }

    #[test]
    fn enforces_minimum_of_two() {
        assert_eq!(plan(&[true], 0), (1, vec![]));
        assert_eq!(plan(&[true, true], 0), (0, vec![]));
    }
}
