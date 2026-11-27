//! What the taskbar and the window overview share: the list of open windows,
//! change tracking, titles, icons and live thumbnails.

use crate::ffi::{self, ClutterActor, MetaDisplay, MetaWindow, MtkRectangle};
use crate::icons;
use crate::ui::{self, gobj};
use glib::prelude::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::rc::{Rc, Weak};

pub fn ptr(window: &glib::Object) -> *mut MetaWindow {
    window.as_ptr() as *mut MetaWindow
}

pub fn id(window: &glib::Object) -> u64 {
    unsafe { ffi::meta_window_get_id(ptr(window)) }
}

/// What a menu was asked for.
pub enum MenuTarget {
    Window(glib::Object),
    /// A pinned app's shortcut or a group of its windows, by desktop file id.
    App(String),
    /// The empty part of the taskbar.
    Taskbar,
}

/// The windows of all workspaces that belong in a window list, oldest first,
/// so that entries keep their place when the focus changes.
pub fn list(display: *mut MetaDisplay) -> Vec<glib::Object> {
    let mut out = recent(display);
    out.sort_by_key(|w| unsafe { ffi::meta_window_get_stable_sequence(ptr(w)) });
    out
}

/// The same windows, the one used last first.
pub fn recent(display: *mut MetaDisplay) -> Vec<glib::Object> {
    tab_list(display, std::ptr::null_mut())
}

/// Those of the active workspace, the one used last first.
pub fn on_active_workspace(display: *mut MetaDisplay) -> Vec<glib::Object> {
    let workspace = unsafe {
        let manager = ffi::meta_display_get_workspace_manager(display);
        ffi::meta_workspace_manager_get_active_workspace(manager)
    };
    tab_list(display, workspace)
}

/// Mutter's tab list of `workspace`, or of all of them if it is null.
fn tab_list(display: *mut MetaDisplay, workspace: *mut ffi::MetaWorkspace) -> Vec<glib::Object> {
    let mut out = Vec::new();
    unsafe {
        let list = ffi::meta_display_get_tab_list(display, ffi::META_TAB_LIST_NORMAL, workspace);
        let mut node = list;
        while !node.is_null() {
            out.push(gobj((*node).data as *mut MetaWindow));
            node = (*node).next;
        }
        glib_sys::g_list_free(list);
    }
    out
}

pub fn focused(display: *mut MetaDisplay) -> Option<u64> {
    unsafe {
        let window = ffi::meta_display_get_focus_window(display);
        (!window.is_null()).then(|| ffi::meta_window_get_id(window))
    }
}

pub fn is_minimized(window: &glib::Object) -> bool {
    window.property::<bool>("minimized")
}

/// Raise and focus `window`, switching workspace if needed.
pub fn activate(display: *mut MetaDisplay, window: &glib::Object) {
    unsafe {
        let time = ffi::meta_display_get_current_time_roundtrip(display);
        ffi::meta_window_activate(ptr(window), time);
    }
}

pub fn title(window: &glib::Object) -> String {
    unsafe {
        let w = ptr(window);
        let title = cstr(ffi::meta_window_get_title(w));
        let class = cstr(ffi::meta_window_get_wm_class(w));
        match (title.is_empty(), class.is_empty()) {
            (false, _) => title,
            (true, false) => class,
            (true, true) => "Untitled window".into(),
        }
    }
}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

/// The icon of the window's app as a `ClutterContent`, if it can be told.
pub fn icon(window: &glib::Object, size: f32, scale: f32, stage: *mut ClutterActor) -> Option<glib::Object> {
    let app = icons::app_for_window(ptr(window));
    let icon = app.and_then(|a| gio::prelude::AppInfoExt::icon(&a));
    icons::content(icon.as_ref(), size, scale, stage)
}

/// The factor that fits `width` x `height` into the given box without
/// enlarging it.
pub fn fit(width: f32, height: f32, max_width: f32, max_height: f32) -> f32 {
    if width <= 0.0 || height <= 0.0 {
        return 0.0;
    }
    (max_width / width).min(max_height / height).min(1.0)
}

/// A live picture of `window`, at most `max_width` x `max_height`, and its
/// size. `None` if the window has nothing to show yet. The actor is floating
/// until parented.
pub fn thumbnail(window: &glib::Object, max_width: f32, max_height: f32) -> Option<(*mut ClutterActor, f32, f32)> {
    unsafe {
        let source = ffi::meta_window_get_compositor_private(ptr(window)) as *mut ClutterActor;
        if source.is_null() {
            return None;
        }
        // The actor also holds the client's shadows, outside the frame.
        let (mut frame, mut buffer) = (MtkRectangle::default(), MtkRectangle::default());
        ffi::meta_window_get_frame_rect(ptr(window), &mut frame);
        ffi::meta_window_get_buffer_rect(ptr(window), &mut buffer);
        let scale = fit(frame.width as f32, frame.height as f32, max_width, max_height);
        if scale <= 0.0 {
            return None;
        }
        let width = (frame.width as f32 * scale).floor().max(1.0);
        let height = (frame.height as f32 * scale).floor().max(1.0);

        let holder = ffi::clutter_actor_new();
        ffi::clutter_actor_set_size(holder, width, height);
        ffi::clutter_actor_set_clip_to_allocation(holder, 1);
        let clone = ffi::clutter_clone_new(source);
        ffi::clutter_actor_set_size(clone, buffer.width as f32 * scale, buffer.height as f32 * scale);
        ffi::clutter_actor_set_position(
            clone,
            (buffer.x - frame.x) as f32 * scale,
            (buffer.y - frame.y) as f32 * scale,
        );
        ffi::clutter_actor_add_child(holder, clone);
        Some((holder, width, height))
    }
}

type Listener = Box<dyn Fn()>;

/// Tells its listeners when the window list, or something shown about a
/// window, changes. Bursts of changes are reported once.
pub struct Tracker {
    display: *mut MetaDisplay,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    listeners: Vec<Rc<Listener>>,
    pending: Option<glib::SourceId>,
    watched: HashMap<u64, (glib::Object, Vec<glib::SignalHandlerId>)>,
    handlers: Vec<(glib::Object, glib::SignalHandlerId)>,
}

impl Tracker {
    pub fn new(display: *mut MetaDisplay) -> Rc<Self> {
        let this = Rc::new(Self { display, state: RefCell::default() });
        let display_obj = unsafe { gobj(display) };
        let manager = unsafe { gobj(ffi::meta_display_get_workspace_manager(display)) };
        let mut handlers = Vec::new();
        let weak = Rc::downgrade(&this);
        let id = display_obj.connect_local("window-created", false, move |_| {
            queue(&weak);
            None
        });
        handlers.push((display_obj.clone(), id));
        let weak = Rc::downgrade(&this);
        let id = display_obj.connect_notify_local(Some("focus-window"), move |_, _| queue(&weak));
        handlers.push((display_obj, id));
        let weak = Rc::downgrade(&this);
        let id = manager.connect_local("workspace-switched", false, move |_| {
            queue(&weak);
            None
        });
        handlers.push((manager, id));
        this.state.borrow_mut().handlers = handlers;
        this.queue();
        this
    }

    pub fn listen(self: &Rc<Self>, listener: impl Fn() + 'static) {
        self.state.borrow_mut().listeners.push(Rc::new(Box::new(listener)));
        self.queue();
    }

    /// Report a change once the main loop is idle.
    pub fn queue(self: &Rc<Self>) {
        if self.state.borrow().pending.is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let id = glib::idle_add_local_once(move || {
            if let Some(this) = weak.upgrade() {
                this.state.borrow_mut().pending = None;
                this.watch_windows();
                let listeners = this.state.borrow().listeners.clone();
                for listener in listeners {
                    listener();
                }
            }
        });
        self.state.borrow_mut().pending = Some(id);
    }

    /// Follow the signals of exactly the windows that are listed now.
    fn watch_windows(self: &Rc<Self>) {
        let windows = list(self.display);
        let mut state = self.state.borrow_mut();
        let current: Vec<u64> = windows.iter().map(id).collect();
        state.watched.retain(|key, (window, ids)| {
            let keep = current.contains(key);
            if !keep {
                for id in ids.drain(..) {
                    window.disconnect(id);
                }
            }
            keep
        });
        for window in windows {
            state.watched.entry(id(&window)).or_insert_with(|| {
                let mut ids = Vec::new();
                for signal in ["unmanaged", "workspace-changed"] {
                    let weak = Rc::downgrade(self);
                    ids.push(window.connect_local(signal, false, move |_| {
                        queue(&weak);
                        None
                    }));
                }
                for property in ["title", "minimized", "skip-taskbar", "wm-class", "gtk-application-id"] {
                    let weak = Rc::downgrade(self);
                    ids.push(window.connect_notify_local(Some(property), move |_, _| queue(&weak)));
                }
                (window.clone(), ids)
            });
        }
    }

    pub fn destroy(&self) {
        let mut state = self.state.borrow_mut();
        if let Some(id) = state.pending.take() {
            id.remove();
        }
        state.listeners.clear();
        for (_, (window, ids)) in state.watched.drain() {
            for id in ids {
                window.disconnect(id);
            }
        }
        for (object, id) in state.handlers.drain(..) {
            object.disconnect(id);
        }
    }
}

fn queue(weak: &Weak<Tracker>) {
    if let Some(tracker) = weak.upgrade() {
        tracker.queue();
    }
}

/// Make `actor` highlight while the pointer is over it.
pub fn hover_highlight(actor: *mut ClutterActor, normal: ffi::CoglColor, hover: ffi::CoglColor) {
    let object = unsafe { gobj(actor) };
    for (signal, color) in [("enter-event", hover), ("leave-event", normal)] {
        object.connect_local(signal, false, move |_| {
            unsafe { ffi::clutter_actor_set_background_color(actor, &color) };
            Some(false.to_value())
        });
    }
}

/// An icon actor at `x`, `y` in `parent`, if there is an icon.
pub fn add_icon(parent: *mut ClutterActor, icon: Option<&glib::Object>, size: f32, x: f32, y: f32) {
    if let Some(content) = icon {
        unsafe {
            let actor = ui::icon_actor(content, size);
            ffi::clutter_actor_set_position(actor, x, y);
            ffi::clutter_actor_add_child(parent, actor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn fits_without_enlarging() {
        assert_eq!(fit(1000.0, 500.0, 500.0, 500.0), 0.5);
        assert_eq!(fit(500.0, 1000.0, 500.0, 500.0), 0.5);
        assert_eq!(fit(100.0, 50.0, 500.0, 500.0), 1.0);
        assert_eq!(fit(0.0, 50.0, 500.0, 500.0), 0.0);
    }
}
