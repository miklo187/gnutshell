//! The bar at the bottom of the primary monitor: one entry per open window,
//! with a live preview while the pointer rests on an entry. Grouped (a switch
//! in the menu of the bar's empty part), it has one icon per app instead, and
//! the preview shows all of the app's windows to choose from.
//!
//! Pinned apps come first, in pin order. A pinned app without a window is an
//! icon that starts it; while it has windows, their entries take that place.
//! Dragging a pinned app's icon or entry along the bar changes the pin order.

use crate::drag::{self, Press, Span};
use crate::ffi::{self, ClutterActor, MetaDisplay};
use crate::pins::{self, Pins, Place};
use crate::ui::{self, gobj};
use crate::windows::{self, MenuTarget};
use gio::prelude::*;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::Duration;

pub const HEIGHT: i32 = 40;
const ITEM_MAX_WIDTH: f32 = 220.0;
const ITEM_GAP: f32 = 4.0;
const ITEM_MARGIN: f32 = 4.0;
const ICON_SIZE: f32 = 20.0;
/// Width of the icon of a pinned app without a window.
const SHORTCUT_WIDTH: f32 = 40.0;
/// Narrower entries show only their icon.
const LABEL_MIN_WIDTH: f32 = 72.0;
const MINIMIZED_OPACITY: u8 = 0x80;
const ITEM_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x10);
const HOVER_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x28);
const FOCUS_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x38);

const PREVIEW_DELAY: Duration = Duration::from_millis(350);
const PREVIEW_WIDTH: f32 = 360.0;
const PREVIEW_HEIGHT: f32 = 240.0;
const PREVIEW_PADDING: f32 = 10.0;
const PREVIEW_TITLE_HEIGHT: f32 = 24.0;
/// Grouped: the width of an entry, and the line under its icon.
const GROUP_WIDTH: f32 = 44.0;
const MARK_WIDTH: f32 = 8.0;
const FOCUSED_MARK_WIDTH: f32 = 18.0;
const MARK_HEIGHT: f32 = 3.0;
const MARK_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x90);
/// One window in a group's preview.
const TILE_MAX_WIDTH: f32 = 220.0;
const TILE_MIN_WIDTH: f32 = 60.0;
const TILE_PICTURE_HEIGHT: f32 = 130.0;
const TILE_GAP: f32 = 6.0;
const TILE_PADDING: f32 = 6.0;
const CLOSE_SIZE: f32 = 20.0;
/// How long a preview stays after the pointer left its entry: long enough to
/// reach a group's preview, which is clickable.
const LEAVE_DELAY: Duration = Duration::from_millis(80);
const GROUP_LEAVE_DELAY: Duration = Duration::from_millis(300);
const LINGER_CHECK: Duration = Duration::from_millis(150);

/// Width of each of `count` entries in a bar `bar` wide.
fn item_width(bar: f32, count: usize) -> f32 {
    if count == 0 {
        return 0.0;
    }
    let room = bar - 2.0 * ITEM_MARGIN - (count - 1) as f32 * ITEM_GAP;
    (room / count as f32).floor().clamp(1.0, ITEM_MAX_WIDTH)
}

/// One place in the bar.
#[derive(Debug, PartialEq, Eq)]
enum Slot {
    /// An index into the pinned apps: one that has no window.
    Shortcut(usize),
    /// An index into the windows.
    Window(usize),
}

/// The bar from left to right, given the pinned app ids and the app id of each
/// window.
fn arrange(pinned: &[String], windows: &[Option<String>]) -> Vec<Slot> {
    let mut out = Vec::new();
    let mut placed = vec![false; windows.len()];
    for (pin, id) in pinned.iter().enumerate() {
        let before = out.len();
        for (index, app) in windows.iter().enumerate() {
            if !placed[index] && app.as_ref() == Some(id) {
                placed[index] = true;
                out.push(Slot::Window(index));
            }
        }
        if out.len() == before {
            out.push(Slot::Shortcut(pin));
        }
    }
    out.extend((0..windows.len()).filter(|&i| !placed[i]).map(Slot::Window));
    out
}

/// One place in the grouped bar.
#[derive(Debug, PartialEq, Eq)]
enum Group {
    /// An app and the indices of its windows, none for an idle pin.
    App(String, Vec<usize>),
    /// A window whose app cannot be told.
    Window(usize),
}

/// The grouped bar from left to right: the pinned apps, then the others in the
/// order of their oldest window.
fn arrange_grouped(pinned: &[String], windows: &[Option<String>]) -> Vec<Group> {
    let mut out: Vec<Group> = pinned.iter().map(|id| Group::App(id.clone(), Vec::new())).collect();
    for (index, app) in windows.iter().enumerate() {
        let Some(app) = app else {
            out.push(Group::Window(index));
            continue;
        };
        let known = out.iter_mut().find_map(|g| match g {
            Group::App(id, members) if id == app => Some(members),
            _ => None,
        });
        match known {
            Some(members) => members.push(index),
            None => out.push(Group::App(app.clone(), vec![index])),
        }
    }
    out
}

/// The line under a grouped entry that has windows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Mark {
    several: bool,
    focused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    Window(u64),
    App(String),
}

#[derive(Clone)]
enum Content {
    Window(glib::Object),
    Shortcut(gio_unix::DesktopAppInfo),
    /// Grouped: an app with at least one window.
    Group(gio_unix::DesktopAppInfo, Vec<glib::Object>),
}

impl Content {
    fn windows(&self) -> &[glib::Object] {
        match self {
            Content::Window(window) => std::slice::from_ref(window),
            Content::Shortcut(_) => &[],
            Content::Group(_, windows) => windows,
        }
    }

    fn has_window(&self, id: Option<u64>) -> bool {
        self.windows().iter().any(|w| Some(windows::id(w)) == id)
    }
}

struct Item {
    key: Key,
    content: Content,
    actor: *mut ClutterActor,
    /// The id of its app if that is pinned here.
    pin: Option<String>,
    /// What the actor was drawn from, to redraw only on change.
    drawn: (String, f32),
    /// Position and width inside the bar.
    span: (f32, f32),
}

/// Opens the menu of an entry at a point in stage coordinates.
type MenuHandler = Rc<dyn Fn(MenuTarget, f32, f32)>;

#[derive(Default)]
struct State {
    menu: Option<MenuHandler>,
    items: Vec<Item>,
    press: Option<Press<Key>>,
    /// Origin of the bar in stage coordinates, and its width.
    geometry: (f32, f32, f32),
    /// The entry under the pointer.
    hovered: Option<Key>,
    preview_timeout: Option<glib::SourceId>,
    /// The entry whose preview is on screen.
    preview_for: Option<Key>,
    /// What that preview was drawn from, to redraw only on change.
    preview_drawn: String,
    /// Whether the preview takes clicks, so the pointer may go onto it.
    preview_clickable: bool,
    focused: Option<u64>,
}

pub struct Taskbar {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    bar: *mut ClutterActor,
    preview: *mut ClutterActor,
    /// Shows where a dragged pin will land.
    marker: *mut ClutterActor,
    pins: Rc<Pins>,
    state: RefCell<State>,
}

impl Taskbar {
    pub fn new(
        display: *mut MetaDisplay,
        stage: *mut ClutterActor,
        pins: Rc<Pins>,
        color: &ffi::CoglColor,
    ) -> Rc<Self> {
        unsafe {
            let bar = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(bar, color);
            ffi::clutter_actor_set_reactive(bar, 1);
            ffi::clutter_actor_add_child(stage, bar);
            let marker = drag::new_marker(bar);

            let preview = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(preview, &ui::BOX_COLOR);
            ffi::clutter_actor_hide(preview);
            ffi::clutter_actor_add_child(stage, preview);

            let this = Rc::new(Self { display, stage, bar, preview, marker, pins, state: RefCell::default() });
            // A right click beside the entries, which keep their own clicks.
            let weak = Rc::downgrade(&this);
            gobj(bar).connect_local("button-press-event", false, move |args| {
                let event = ui::event_arg(args);
                if ffi::clutter_event_get_button(event) == 3 {
                    with(&weak, |t| t.open_bar_menu(ui::event_coords(event)));
                }
                Some(true.to_value())
            });
            this
        }
    }

    fn open_bar_menu(&self, at: Option<(f32, f32)>) {
        self.hide_preview();
        let (handler, top) = {
            let state = self.state.borrow();
            (state.menu.clone(), state.geometry.1)
        };
        if let (Some(handler), Some((x, _))) = (handler, at) {
            handler(MenuTarget::Taskbar, x, top);
        }
    }

    /// `handler` runs on a right click on an entry.
    pub fn set_menu_handler(&self, handler: impl Fn(MenuTarget, f32, f32) + 'static) {
        self.state.borrow_mut().menu = Some(Rc::new(handler));
    }

    /// Place the bar along the bottom of the monitor at `geo`.
    pub fn relayout(self: &Rc<Self>, geo: ffi::MtkRectangle) {
        let (x, y, width) = (geo.x as f32, (geo.y + geo.height - HEIGHT) as f32, geo.width as f32);
        unsafe {
            ffi::clutter_actor_set_position(self.bar, x, y);
            ffi::clutter_actor_set_size(self.bar, width, HEIGHT as f32);
        }
        self.state.borrow_mut().geometry = (x, y, width);
        self.refresh();
    }

    pub fn set_visible(&self, visible: bool) {
        unsafe {
            if visible {
                ffi::clutter_actor_show(self.bar);
            } else {
                ffi::clutter_actor_hide(self.bar);
            }
        }
        if !visible {
            self.hide_preview();
        }
    }

    /// Bring the entries up to date. Entries that stay keep their actor, so
    /// the one under the pointer goes on getting events.
    pub fn refresh(self: &Rc<Self>) {
        let windows = windows::list(self.display);
        let window_apps: Vec<Option<String>> = windows.iter().map(pins::app_id_of).collect();
        let pinned = self.pins.apps(Place::Taskbar);
        let pinned_ids: Vec<String> = pinned.iter().map(|(id, _)| id.clone()).collect();
        let focused = windows::focused(self.display);
        let mut old = {
            let mut state = self.state.borrow_mut();
            state.focused = focused;
            std::mem::take(&mut state.items)
        };
        let bar_width = self.state.borrow().geometry.2;
        let grouped = self.pins.grouped();
        let entries = if grouped {
            self.grouped_entries(&windows, &window_apps, &pinned, &pinned_ids, bar_width)
        } else {
            window_entries(&windows, &window_apps, &pinned, &pinned_ids, bar_width)
        };
        let scale = ui::primary_monitor_scale(self.display);

        let mut items = Vec::new();
        let mut x = ITEM_MARGIN;
        for entry in entries {
            let Entry { key, content, pin, width } = entry;
            let mut item = match old.iter().position(|i| i.key == key) {
                Some(found) => old.remove(found),
                None => Item {
                    actor: self.new_item(key.clone()),
                    key,
                    content: content.clone(),
                    pin: None,
                    drawn: (String::new(), -1.0),
                    span: (0.0, 0.0),
                },
            };
            item.content = content;
            item.pin = pin;
            let (title, icon) = match &item.content {
                Content::Window(window) => (
                    windows::title(window),
                    // The icon can be known only some time after the window appears.
                    windows::icon(window, ICON_SIZE, scale, self.stage),
                ),
                Content::Shortcut(app) | Content::Group(app, _) => (
                    app.display_name().to_string(),
                    crate::icons::content(app.icon().as_ref(), ICON_SIZE, scale, self.stage),
                ),
            };
            let members = item.content.windows();
            let minimized = !members.is_empty() && members.iter().all(windows::is_minimized);
            let mark = (grouped && !members.is_empty())
                .then(|| Mark { several: members.len() > 1, focused: item.content.has_window(focused) });
            let labelled = !grouped && matches!(item.content, Content::Window(_)) && width >= LABEL_MIN_WIDTH;
            let drawn = (format!("{title}\u{1}{}{mark:?}{labelled}", icon.is_some()), width);
            if item.drawn != drawn {
                // An entry without an icon is told apart by its title.
                draw_item(item.actor, &title, icon.as_ref(), width, labelled || icon.is_none(), mark);
                item.drawn = drawn;
            }
            item.span = (x, width);
            unsafe {
                ffi::clutter_actor_set_position(item.actor, x, ITEM_MARGIN);
                let opacity = if minimized { MINIMIZED_OPACITY } else { 0xff };
                ffi::clutter_actor_set_opacity(item.actor, opacity);
            }
            x += width + ITEM_GAP;
            items.push(item);
        }
        let gone = |key: &Option<Key>| key.as_ref().is_some_and(|k| !items.iter().any(|i| &i.key == k));
        let (hovered_gone, preview_gone) = {
            let state = self.state.borrow();
            (gone(&state.hovered), gone(&state.preview_for))
        };
        self.state.borrow_mut().items = items;
        self.repaint();
        // Not under the borrow: destroying an actor can send crossing events.
        for item in old {
            unsafe { ffi::clutter_actor_destroy(item.actor) };
        }
        if hovered_gone {
            self.state.borrow_mut().hovered = None;
        }
        let preview = self.state.borrow().preview_for.clone();
        if preview_gone {
            self.hide_preview();
        } else if let Some(key) = preview {
            self.show_preview(&key);
        }
    }

    fn grouped_entries(
        &self,
        windows: &[glib::Object],
        window_apps: &[Option<String>],
        pinned: &[(String, gio_unix::DesktopAppInfo)],
        pinned_ids: &[String],
        bar_width: f32,
    ) -> Vec<Entry> {
        let groups = arrange_grouped(pinned_ids, window_apps);
        let width = item_width(bar_width, groups.len()).min(GROUP_WIDTH);
        groups
            .into_iter()
            .filter_map(|group| match group {
                Group::Window(i) => Some(Entry {
                    key: Key::Window(windows::id(&windows[i])),
                    content: Content::Window(windows[i].clone()),
                    pin: None,
                    width,
                }),
                Group::App(id, members) => {
                    let known = pinned.iter().find(|(pin, _)| pin == &id).map(|(_, app)| app.clone());
                    let pin = known.is_some().then(|| id.clone());
                    let app = known.or_else(|| pins::app(&id))?;
                    let content = if members.is_empty() {
                        Content::Shortcut(app)
                    } else {
                        Content::Group(app, members.iter().map(|&i| windows[i].clone()).collect())
                    };
                    Some(Entry { key: Key::App(id), content, pin, width })
                }
            })
            .collect()
    }

    fn new_item(self: &Rc<Self>, key: Key) -> *mut ClutterActor {
        let actor = unsafe {
            let actor = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(actor, 1);
            ffi::clutter_actor_add_child(self.bar, actor);
            actor
        };
        let object = unsafe { gobj(actor) };
        let weak = Rc::downgrade(self);
        let own = key.clone();
        object.connect_local("enter-event", false, move |_| {
            with(&weak, |t| t.enter(own.clone()));
            Some(false.to_value())
        });
        let weak = Rc::downgrade(self);
        let own = key.clone();
        object.connect_local("leave-event", false, move |_| {
            with(&weak, |t| t.leave(&own));
            Some(false.to_value())
        });
        let weak = Rc::downgrade(self);
        let own = key.clone();
        object.connect_local("button-press-event", false, move |args| {
            let event = ui::event_arg(args);
            match unsafe { ffi::clutter_event_get_button(event) } {
                1 => with(&weak, |t| t.press(own.clone(), event)),
                3 => with(&weak, |t| t.open_menu(&own, ui::event_coords(event))),
                _ => {}
            }
            Some(true.to_value())
        });
        let weak = Rc::downgrade(self);
        let own = key.clone();
        object.connect_local("motion-event", false, move |args| {
            with(&weak, |t| t.motion(&own, ui::event_arg(args)));
            Some(false.to_value())
        });
        let weak = Rc::downgrade(self);
        object.connect_local("button-release-event", false, move |args| {
            let event = ui::event_arg(args);
            if unsafe { ffi::clutter_event_get_button(event) } == 1 {
                with(&weak, |t| t.release(&key, ui::event_coords(event)));
            }
            Some(true.to_value())
        });
        actor
    }

    fn press(&self, key: Key, event: *const ffi::ClutterEvent) {
        self.hide_preview();
        self.state.borrow_mut().press = Press::new(key, event);
    }

    fn motion(&self, key: &Key, event: *const ffi::ClutterEvent) {
        let Some((x, y)) = ui::event_coords(event) else { return };
        let dragging = {
            let mut state = self.state.borrow_mut();
            match state.press.as_mut() {
                Some(press) if &press.key == key && drag::button_held(event) => press.moved_to((x, y)),
                _ => false,
            }
        };
        if dragging {
            self.hide_preview();
            match self.drop_target(key, x, y) {
                Some((_, _, at)) => drag::place_marker(self.marker, at, ITEM_MARGIN, HEIGHT as f32 - 2.0 * ITEM_MARGIN),
                None => unsafe { ffi::clutter_actor_hide(self.marker) },
            }
        }
    }

    /// For a drag of `key` to `x`, `y` on the stage: the pinned app dragged,
    /// the one to put it before, and where in the bar that is. `None` if the
    /// entry is not pinned or the pointer is away from the bar.
    fn drop_target(&self, key: &Key, x: f32, y: f32) -> Option<(String, Option<String>, f32)> {
        let state = self.state.borrow();
        let (bar_x, bar_y, _) = state.geometry;
        if !drag::near_bar(y, bar_y, HEIGHT as f32) {
            return None;
        }
        let id = state.items.iter().find(|i| &i.key == key)?.pin.clone()?;
        // An app with several windows takes the room of all its entries.
        let mut spans: Vec<Span> = Vec::new();
        for item in &state.items {
            let Some(pin) = &item.pin else { continue };
            let (start, end) = (item.span.0, item.span.0 + item.span.1);
            match spans.iter_mut().find(|s| &s.id == pin) {
                Some(span) => span.end = end,
                None => spans.push(Span { id: pin.clone(), start, end }),
            }
        }
        let (before, at) = drag::drop_at(&spans, x - bar_x)?;
        // Centered in the gap between two entries.
        let at = if before.is_some() { at - ITEM_GAP / 2.0 } else { at + ITEM_GAP / 2.0 };
        Some((id, before, at))
    }

    /// End a press: drop a dragged pin, or else click the entry if the pointer
    /// is still on it.
    fn release(self: &Rc<Self>, key: &Key, at: Option<(f32, f32)>) {
        let press = self.state.borrow_mut().press.take();
        unsafe { ffi::clutter_actor_hide(self.marker) };
        let (Some(press), Some((x, y))) = (press, at) else { return };
        if &press.key != key {
            return;
        }
        if press.dragging {
            if let Some((id, before, _)) = self.drop_target(key, x, y) {
                self.pins.reorder(Place::Taskbar, &id, before.as_deref());
            }
            return;
        }
        let on_it = {
            let state = self.state.borrow();
            let (bar_x, bar_y, _) = state.geometry;
            state.items.iter().find(|i| &i.key == key).is_some_and(|i| {
                let left = bar_x + i.span.0;
                x >= left && x < left + i.span.1 && y >= bar_y && y <= bar_y + HEIGHT as f32
            })
        };
        if on_it {
            self.click(key, press.new_window);
        }
    }

    fn content(&self, key: &Key) -> Option<Content> {
        self.state.borrow().items.iter().find(|i| &i.key == key).map(|i| i.content.clone())
    }

    fn open_menu(&self, key: &Key, at: Option<(f32, f32)>) {
        let (handler, top) = {
            let state = self.state.borrow();
            (state.menu.clone(), state.geometry.1)
        };
        self.hide_preview();
        let target = match (self.content(key), key) {
            (Some(Content::Window(window)), _) => MenuTarget::Window(window),
            // A group of one has the full menu of its window.
            (Some(Content::Group(_, members)), _) if members.len() == 1 => MenuTarget::Window(members[0].clone()),
            (Some(Content::Shortcut(_) | Content::Group(..)), Key::App(id)) => MenuTarget::App(id.clone()),
            _ => return,
        };
        if let (Some(handler), Some((x, _))) = (handler, at) {
            // From the bar's top edge, so that the menu opens above it.
            handler(target, x, top);
        }
    }

    /// Start a pinned app. For a window: focus it, or minimize it if it has
    /// the focus already. A group of several shows them to choose from. With
    /// `new_window`, open another window of the app instead, if it can be
    /// told.
    fn click(self: &Rc<Self>, key: &Key, new_window: bool) {
        if crate::lockscreen::is_locked() {
            return;
        }
        let Some(content) = self.content(key) else { return };
        self.hide_preview();
        if new_window {
            let app = match &content {
                Content::Window(window) => pins::app_id_of(window).and_then(|id| pins::app(&id)),
                Content::Shortcut(app) | Content::Group(app, _) => Some(app.clone()),
            };
            if let Some(app) = app {
                pins::launch(self.display, &app, true);
                return;
            }
        }
        match content {
            Content::Shortcut(app) => pins::launch(self.display, &app, false),
            Content::Group(_, members) if members.len() > 1 => self.show_preview(key),
            Content::Group(_, members) => self.click_window(&members[0]),
            Content::Window(window) => self.click_window(&window),
        }
    }

    fn click_window(&self, window: &glib::Object) {
        let has_focus = self.state.borrow().focused == Some(windows::id(window));
        if has_focus && !windows::is_minimized(window) {
            unsafe { ffi::meta_window_minimize(windows::ptr(window)) };
        } else {
            windows::activate(self.display, window);
        }
    }

    fn enter(self: &Rc<Self>, key: Key) {
        self.state.borrow_mut().hovered = Some(key.clone());
        self.repaint();
        // Moving along the bar switches an open preview at once.
        if self.state.borrow().preview_for.is_some() {
            self.cancel_timeout();
            self.show_preview(&key);
            return;
        }
        self.schedule(PREVIEW_DELAY, move |t| t.show_preview(&key));
    }

    fn leave(self: &Rc<Self>, key: &Key) {
        if self.state.borrow().hovered.as_ref() != Some(key) {
            return;
        }
        self.state.borrow_mut().hovered = None;
        self.repaint();
        // Wait a moment, so that crossing the gap to the next entry does not
        // close the preview and start the delay again.
        let delay = if self.state.borrow().preview_clickable { GROUP_LEAVE_DELAY } else { LEAVE_DELAY };
        self.schedule(delay, |t| t.close_unless_used());
    }

    /// The pointer is on no entry: close the preview, unless it is on a
    /// clickable preview or on the way there.
    fn close_unless_used(self: &Rc<Self>) {
        if self.state.borrow().preview_clickable && self.pointer_on_preview() {
            self.schedule(LINGER_CHECK, |t| t.close_unless_used());
        } else {
            self.hide_preview();
        }
    }

    /// Whether the pointer is on the preview or between it and the bar.
    fn pointer_on_preview(&self) -> bool {
        let bar_top = self.state.borrow().geometry.1;
        unsafe {
            let backend = ffi::meta_context_get_backend(ffi::meta_display_get_context(self.display));
            let tracker = ffi::meta_backend_get_cursor_tracker(backend);
            let mut at = ffi::GraphenePoint { x: 0.0, y: 0.0 };
            let mut mods = 0;
            ffi::meta_cursor_tracker_get_pointer(tracker, &mut at, &mut mods);
            let left = ffi::clutter_actor_get_x(self.preview);
            let top = ffi::clutter_actor_get_y(self.preview);
            let right = left + ffi::clutter_actor_get_width(self.preview);
            at.x >= left && at.x <= right && at.y >= top && at.y <= bar_top
        }
    }

    /// Run `then` after `delay`, in place of anything scheduled before.
    fn schedule(self: &Rc<Self>, delay: Duration, then: impl FnOnce(&Rc<Self>) + 'static) {
        self.cancel_timeout();
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(delay, move || {
            with(&weak, |t| {
                t.state.borrow_mut().preview_timeout = None;
                then(t);
            });
        });
        self.state.borrow_mut().preview_timeout = Some(source);
    }

    fn repaint(&self) {
        let state = self.state.borrow();
        for item in &state.items {
            let focused = item.content.has_window(state.focused);
            let hovered = state.hovered.as_ref() == Some(&item.key);
            let color = match &item.key {
                _ if focused => FOCUS_COLOR,
                _ if hovered => HOVER_COLOR,
                Key::App(_) => ui::TRANSPARENT,
                Key::Window(_) => ITEM_COLOR,
            };
            unsafe { ffi::clutter_actor_set_background_color(item.actor, &color) };
        }
    }

    fn cancel_timeout(&self) {
        let source = self.state.borrow_mut().preview_timeout.take();
        if let Some(source) = source {
            source.remove();
        }
    }

    /// The preview of an entry: a window's title and picture, a pinned app's
    /// name, or the windows of a group.
    fn show_preview(self: &Rc<Self>, key: &Key) {
        if self.state.borrow().press.as_ref().is_some_and(|p| p.dragging) {
            return;
        }
        let found = {
            let state = self.state.borrow();
            let (x, y, bar_width) = state.geometry;
            state.items.iter().find(|i| &i.key == key).map(|item| {
                let center = x + item.span.0 + item.span.1 / 2.0;
                (item.content.clone(), center, x, y, bar_width)
            })
        };
        let Some((content, center, bar_x, bar_y, bar_width)) = found else { return };
        // Redrawn only on change, so that what is under the pointer stays.
        let mut drawn = format!("{key:?} {center} {bar_y} {bar_width}");
        for window in content.windows() {
            let mut frame = ffi::MtkRectangle::default();
            unsafe { ffi::meta_window_get_frame_rect(windows::ptr(window), &mut frame) };
            drawn += &format!(
                "\u{1}{}\u{1}{}\u{1}{}x{}",
                windows::id(window),
                windows::title(window),
                frame.width,
                frame.height
            );
        }
        if self.state.borrow().preview_for.is_some() && self.state.borrow().preview_drawn == drawn {
            return;
        }
        let clickable = matches!(content, Content::Group(..));
        unsafe {
            ffi::clutter_actor_destroy_all_children(self.preview);
            ffi::clutter_actor_set_reactive(self.preview, i32::from(clickable));
            let (width, height) = match &content {
                Content::Window(window) => self.draw_window_preview(window),
                Content::Shortcut(app) => self.draw_name(&app.display_name()),
                Content::Group(_, members) => self.draw_group_preview(members, bar_width),
            };
            let left = (center - width / 2.0).min(bar_x + bar_width - width - 4.0).max(bar_x + 4.0);
            ffi::clutter_actor_set_size(self.preview, width, height);
            ffi::clutter_actor_set_position(self.preview, left.floor(), bar_y - height - 6.0);
            ffi::clutter_actor_show(self.preview);
        }
        let mut state = self.state.borrow_mut();
        state.preview_for = Some(key.clone());
        state.preview_drawn = drawn;
        state.preview_clickable = clickable;
    }

    /// Returns the preview's size.
    unsafe fn draw_window_preview(&self, window: &glib::Object) -> (f32, f32) {
        unsafe {
            let thumbnail = windows::thumbnail(window, PREVIEW_WIDTH, PREVIEW_HEIGHT);
            let (thumb_width, thumb_height) = thumbnail.map_or((0.0, 0.0), |(_, w, h)| (w, h));
            let inner = thumb_width.max(220.0);
            self.add_preview_title(&windows::title(window), inner);
            if let Some((actor, w, _)) = thumbnail {
                ffi::clutter_actor_set_position(
                    actor,
                    PREVIEW_PADDING + ((inner - w) / 2.0).floor(),
                    PREVIEW_PADDING + PREVIEW_TITLE_HEIGHT,
                );
                ffi::clutter_actor_add_child(self.preview, actor);
            }
            (inner + 2.0 * PREVIEW_PADDING, 2.0 * PREVIEW_PADDING + PREVIEW_TITLE_HEIGHT + thumb_height)
        }
    }

    unsafe fn draw_name(&self, name: &str) -> (f32, f32) {
        unsafe {
            let probe = ui::label(ui::BODY_FONT, name, &ui::TEXT_COLOR);
            ffi::clutter_actor_add_child(self.preview, probe);
            let inner = ffi::clutter_actor_get_width(probe).min(PREVIEW_WIDTH).ceil();
            ffi::clutter_actor_destroy(probe);
            self.add_preview_title(name, inner);
            (inner + 2.0 * PREVIEW_PADDING, 2.0 * PREVIEW_PADDING + PREVIEW_TITLE_HEIGHT)
        }
    }

    unsafe fn add_preview_title(&self, text: &str, width: f32) {
        unsafe {
            let title = ui::label(ui::BODY_FONT, text, &ui::TEXT_COLOR);
            ffi::clutter_text_set_ellipsize(title as *mut ffi::ClutterText, ffi::PANGO_ELLIPSIZE_END);
            ffi::clutter_actor_set_width(title, width);
            ffi::clutter_actor_set_position(title, PREVIEW_PADDING, PREVIEW_PADDING);
            ffi::clutter_actor_add_child(self.preview, title);
        }
    }

    /// A tile per window, side by side, narrower if they would not fit.
    unsafe fn draw_group_preview(self: &Rc<Self>, members: &[glib::Object], bar_width: f32) -> (f32, f32) {
        let width = tile_width(bar_width, members.len());
        let height = CLOSE_SIZE + TILE_PICTURE_HEIGHT + 3.0 * TILE_PADDING;
        for (index, window) in members.iter().enumerate() {
            unsafe {
                let tile = self.new_tile(window, width, height);
                ffi::clutter_actor_set_position(tile, TILE_GAP + index as f32 * (width + TILE_GAP), TILE_GAP);
            }
        }
        (TILE_GAP + members.len() as f32 * (width + TILE_GAP), height + 2.0 * TILE_GAP)
    }

    /// One window of a group: click to go to it, ✕ to close it, right click
    /// for its menu.
    unsafe fn new_tile(self: &Rc<Self>, window: &glib::Object, width: f32, height: f32) -> *mut ClutterActor {
        unsafe {
            let tile = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(tile, width, height);
            ffi::clutter_actor_set_reactive(tile, 1);
            ffi::clutter_actor_set_clip_to_allocation(tile, 1);
            ffi::clutter_actor_add_child(self.preview, tile);
            let focused = self.state.borrow().focused == Some(windows::id(window));
            let color = if focused { FOCUS_COLOR } else { ITEM_COLOR };
            ffi::clutter_actor_set_background_color(tile, &color);
            windows::hover_highlight(tile, color, HOVER_COLOR);

            let title = ui::label(ui::BODY_FONT, &windows::title(window), &ui::TEXT_COLOR);
            ffi::clutter_text_set_ellipsize(title as *mut ffi::ClutterText, ffi::PANGO_ELLIPSIZE_END);
            ffi::clutter_actor_set_width(title, (width - CLOSE_SIZE - 3.0 * TILE_PADDING).max(1.0));
            ffi::clutter_actor_add_child(tile, title);
            let title_y = TILE_PADDING + (CLOSE_SIZE - ffi::clutter_actor_get_height(title)) / 2.0;
            ffi::clutter_actor_set_position(title, TILE_PADDING, title_y.floor());

            let area_width = width - 2.0 * TILE_PADDING;
            if let Some((picture, w, h)) = windows::thumbnail(window, area_width, TILE_PICTURE_HEIGHT) {
                ffi::clutter_actor_set_position(
                    picture,
                    TILE_PADDING + ((area_width - w) / 2.0).floor(),
                    CLOSE_SIZE + 2.0 * TILE_PADDING + ((TILE_PICTURE_HEIGHT - h) / 2.0).floor(),
                );
                ffi::clutter_actor_add_child(tile, picture);
            }

            let close = ui::button("✕", CLOSE_SIZE, CLOSE_SIZE, &ui::BUTTON_COLOR);
            ffi::clutter_actor_set_position(close, width - CLOSE_SIZE - TILE_PADDING, TILE_PADDING);
            ffi::clutter_actor_add_child(tile, close);

            let weak = Rc::downgrade(self);
            let target = window.clone();
            gobj(close).connect_local("button-press-event", false, move |_| {
                with(&weak, |t| t.close_window(&target));
                Some(true.to_value())
            });
            let weak = Rc::downgrade(self);
            let target = window.clone();
            gobj(tile).connect_local("button-press-event", false, move |args| {
                let event = ui::event_arg(args);
                let button = ffi::clutter_event_get_button(event);
                let at = ui::event_coords(event);
                with(&weak, |t| match button {
                    1 => {
                        t.hide_preview();
                        windows::activate(t.display, &target);
                    }
                    2 => t.close_window(&target),
                    3 => {
                        let top = ffi::clutter_actor_get_y(t.preview);
                        let handler = t.state.borrow().menu.clone();
                        t.hide_preview();
                        if let (Some(handler), Some((x, _))) = (handler, at) {
                            handler(MenuTarget::Window(target.clone()), x, top);
                        }
                    }
                    _ => {}
                });
                Some(true.to_value())
            });
            tile
        }
    }

    /// Ask `window` to close; the tracker then updates the preview.
    fn close_window(&self, window: &glib::Object) {
        unsafe {
            let time = ffi::meta_display_get_current_time_roundtrip(self.display);
            ffi::meta_window_delete(windows::ptr(window), time);
        }
    }

    pub fn hide_preview(&self) {
        self.cancel_timeout();
        {
            let mut state = self.state.borrow_mut();
            state.preview_for = None;
            state.preview_drawn.clear();
            state.preview_clickable = false;
        }
        unsafe {
            ffi::clutter_actor_hide(self.preview);
            ffi::clutter_actor_destroy_all_children(self.preview);
        }
    }

    pub fn destroy(&self) {
        self.hide_preview();
        let items = std::mem::take(&mut self.state.borrow_mut().items);
        drop(items);
        unsafe {
            ffi::clutter_actor_destroy(self.preview);
            ffi::clutter_actor_destroy(self.bar);
        }
    }
}

/// An entry to be drawn.
struct Entry {
    key: Key,
    content: Content,
    /// The id of its app if that is pinned here.
    pin: Option<String>,
    width: f32,
}

/// The entries of the bar with one per window.
fn window_entries(
    windows: &[glib::Object],
    window_apps: &[Option<String>],
    pinned: &[(String, gio_unix::DesktopAppInfo)],
    pinned_ids: &[String],
    bar_width: f32,
) -> Vec<Entry> {
    let slots = arrange(pinned_ids, window_apps);
    let shortcuts = slots.iter().filter(|s| matches!(s, Slot::Shortcut(_))).count();
    let shortcut_room = shortcuts as f32 * (SHORTCUT_WIDTH + ITEM_GAP);
    let window_width = item_width((bar_width - shortcut_room).max(0.0), windows.len());
    slots
        .into_iter()
        .map(|slot| match slot {
            Slot::Window(i) => Entry {
                key: Key::Window(windows::id(&windows[i])),
                content: Content::Window(windows[i].clone()),
                pin: window_apps[i].clone().filter(|id| pinned_ids.contains(id)),
                width: window_width,
            },
            Slot::Shortcut(i) => {
                let (id, app) = pinned[i].clone();
                Entry { key: Key::App(id.clone()), content: Content::Shortcut(app), pin: Some(id), width: SHORTCUT_WIDTH }
            }
        })
        .collect()
}

/// Width of each of `count` tiles in a preview over a bar `bar` wide.
fn tile_width(bar: f32, count: usize) -> f32 {
    let count = count.max(1) as f32;
    ((bar - 8.0 - (count + 1.0) * TILE_GAP) / count).floor().clamp(TILE_MIN_WIDTH, TILE_MAX_WIDTH)
}

fn draw_item(
    actor: *mut ClutterActor,
    title: &str,
    icon: Option<&glib::Object>,
    width: f32,
    with_label: bool,
    mark: Option<Mark>,
) {
    let height = HEIGHT as f32 - 2.0 * ITEM_MARGIN;
    unsafe {
        ffi::clutter_actor_destroy_all_children(actor);
        ffi::clutter_actor_set_size(actor, width, height);
        ffi::clutter_actor_set_clip_to_allocation(actor, 1);
        let icon_x = if with_label { 8.0 } else { ((width - ICON_SIZE) / 2.0).floor() };
        windows::add_icon(actor, icon, ICON_SIZE, icon_x, ((height - ICON_SIZE) / 2.0).floor());
        if with_label {
            let x = if icon.is_some() { 8.0 + ICON_SIZE + 8.0 } else { 8.0 };
            let label = ui::label(ui::BODY_FONT, title, &ui::TEXT_COLOR);
            ffi::clutter_text_set_ellipsize(label as *mut ffi::ClutterText, ffi::PANGO_ELLIPSIZE_END);
            ffi::clutter_actor_set_width(label, (width - x - 8.0).max(1.0));
            ffi::clutter_actor_add_child(actor, label);
            let y = (height - ffi::clutter_actor_get_height(label)) / 2.0;
            ffi::clutter_actor_set_position(label, x, y.floor());
        }
        if let Some(mark) = mark {
            let color = if mark.focused { &ui::SELECTED_COLOR } else { &MARK_COLOR };
            let main = if mark.focused { FOCUSED_MARK_WIDTH } else { MARK_WIDTH };
            // A second, short piece says there are more windows.
            let second = if mark.several { 4.0 } else { 0.0 };
            let gap = if mark.several { 3.0 } else { 0.0 };
            let mut x = ((width - main - gap - second) / 2.0).floor();
            for piece in [main, second] {
                if piece > 0.0 {
                    let line = ffi::clutter_actor_new();
                    ffi::clutter_actor_set_background_color(line, color);
                    ffi::clutter_actor_set_size(line, piece, MARK_HEIGHT);
                    ffi::clutter_actor_set_position(line, x, height - MARK_HEIGHT);
                    ffi::clutter_actor_add_child(actor, line);
                    x += piece + gap;
                }
            }
        }
    }
}

fn with(weak: &Weak<Taskbar>, f: impl FnOnce(&Rc<Taskbar>)) {
    if let Some(taskbar) = weak.upgrade() {
        f(&taskbar);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_apps_come_first_and_give_way_to_their_windows() {
        let pinned = ["files".to_owned(), "mail".to_owned()];
        let app = |id: &str| Some(id.to_owned());
        let windows = [app("editor"), app("files"), None, app("files")];
        assert_eq!(
            arrange(&pinned, &windows),
            [Slot::Window(1), Slot::Window(3), Slot::Shortcut(1), Slot::Window(0), Slot::Window(2)]
        );
        assert_eq!(arrange(&pinned, &[]), [Slot::Shortcut(0), Slot::Shortcut(1)]);
        assert_eq!(arrange(&[], &[None]), [Slot::Window(0)]);
    }

    #[test]
    fn groups_hold_all_windows_of_an_app() {
        let pinned = ["files".to_owned(), "mail".to_owned()];
        let app = |id: &str| Some(id.to_owned());
        let windows = [app("editor"), app("files"), None, app("files"), app("editor")];
        let group = |id: &str, members: &[usize]| Group::App(id.to_owned(), members.to_vec());
        assert_eq!(
            arrange_grouped(&pinned, &windows),
            [group("files", &[1, 3]), group("mail", &[]), group("editor", &[0, 4]), Group::Window(2)]
        );
        assert_eq!(arrange_grouped(&[], &[]), []);
    }

    #[test]
    fn preview_tiles_shrink_to_fit() {
        assert_eq!(tile_width(1280.0, 2), TILE_MAX_WIDTH);
        let w = tile_width(1280.0, 10);
        assert!(w < TILE_MAX_WIDTH && 10.0 * w + 11.0 * TILE_GAP <= 1272.0);
        assert_eq!(tile_width(1280.0, 100), TILE_MIN_WIDTH);
    }

    #[test]
    fn entries_shrink_to_share_the_bar() {
        assert_eq!(item_width(1280.0, 0), 0.0);
        assert_eq!(item_width(1280.0, 2), ITEM_MAX_WIDTH);
        let w = item_width(1280.0, 20);
        assert!(20.0 * w + 19.0 * ITEM_GAP + 2.0 * ITEM_MARGIN <= 1280.0);
        assert!(w > 50.0);
    }
}
