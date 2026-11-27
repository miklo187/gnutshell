//! The bar at the bottom of the primary monitor: one entry per open window,
//! with a live preview while the pointer rests on an entry.
//!
//! Pinned apps come first, in pin order. A pinned app without a window is an
//! icon that starts it; while it has windows, their entries take that place.

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

#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    Window(u64),
    App(String),
}

#[derive(Clone)]
enum Content {
    Window(glib::Object),
    Shortcut(gio_unix::DesktopAppInfo),
}

struct Item {
    key: Key,
    content: Content,
    actor: *mut ClutterActor,
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
    /// Origin of the bar in stage coordinates, and its width.
    geometry: (f32, f32, f32),
    /// The entry under the pointer.
    hovered: Option<Key>,
    preview_timeout: Option<glib::SourceId>,
    preview_shown: bool,
    focused: Option<u64>,
}

pub struct Taskbar {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    bar: *mut ClutterActor,
    preview: *mut ClutterActor,
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

            let preview = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(preview, &ui::BOX_COLOR);
            ffi::clutter_actor_hide(preview);
            ffi::clutter_actor_add_child(stage, preview);

            Rc::new(Self { display, stage, bar, preview, pins, state: RefCell::default() })
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
        let slots = arrange(&pinned_ids, &window_apps);

        let focused = windows::focused(self.display);
        let mut old = {
            let mut state = self.state.borrow_mut();
            state.focused = focused;
            std::mem::take(&mut state.items)
        };
        let bar_width = self.state.borrow().geometry.2;
        let shortcuts = slots.iter().filter(|s| matches!(s, Slot::Shortcut(_))).count();
        let shortcut_room = shortcuts as f32 * (SHORTCUT_WIDTH + ITEM_GAP);
        let window_width = item_width((bar_width - shortcut_room).max(0.0), windows.len());
        let scale = ui::primary_monitor_scale(self.display);

        let mut items = Vec::new();
        let mut x = ITEM_MARGIN;
        for slot in slots {
            let (key, content, width) = match slot {
                Slot::Window(i) => {
                    let window = windows[i].clone();
                    (Key::Window(windows::id(&window)), Content::Window(window), window_width)
                }
                Slot::Shortcut(i) => {
                    let (id, app) = pinned[i].clone();
                    (Key::App(id), Content::Shortcut(app), SHORTCUT_WIDTH)
                }
            };
            let mut item = match old.iter().position(|i| i.key == key) {
                Some(found) => old.remove(found),
                None => Item {
                    actor: self.new_item(key.clone()),
                    key,
                    content: content.clone(),
                    drawn: (String::new(), -1.0),
                    span: (0.0, 0.0),
                },
            };
            item.content = content;
            let (title, icon, labelled, minimized) = match &item.content {
                Content::Window(window) => (
                    windows::title(window),
                    // The icon can be known only some time after the window appears.
                    windows::icon(window, ICON_SIZE, scale, self.stage),
                    width >= LABEL_MIN_WIDTH,
                    windows::is_minimized(window),
                ),
                Content::Shortcut(app) => (
                    app.display_name().to_string(),
                    crate::icons::content(app.icon().as_ref(), ICON_SIZE, scale, self.stage),
                    false,
                    false,
                ),
            };
            let drawn = (format!("{title}\u{1}{}", icon.is_some()), width);
            if item.drawn != drawn {
                draw_item(item.actor, &title, icon.as_ref(), width, labelled || icon.is_none());
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
        let hovered = self.state.borrow().hovered.clone();
        let hovered_gone = hovered.is_some_and(|h| !items.iter().any(|i| i.key == h));
        self.state.borrow_mut().items = items;
        self.repaint();
        // Not under the borrow: destroying an actor can send crossing events.
        for item in old {
            unsafe { ffi::clutter_actor_destroy(item.actor) };
        }
        if hovered_gone {
            self.state.borrow_mut().hovered = None;
            self.hide_preview();
        } else if self.state.borrow().preview_shown {
            self.show_preview();
        }
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
        object.connect_local("button-press-event", false, move |args| {
            let event = ui::event_arg(args);
            match unsafe { ffi::clutter_event_get_button(event) } {
                1 => with(&weak, |t| t.click(&key)),
                3 => with(&weak, |t| t.open_menu(&key, ui::event_coords(event))),
                _ => {}
            }
            Some(true.to_value())
        });
        actor
    }

    fn content(&self, key: &Key) -> Option<Content> {
        self.state.borrow().items.iter().find(|i| &i.key == key).map(|i| i.content.clone())
    }

    fn open_menu(&self, key: &Key, at: Option<(f32, f32)>) {
        let (handler, top) = {
            let state = self.state.borrow();
            (state.menu.clone(), state.geometry.1)
        };
        let target = match (self.content(key), key) {
            (Some(Content::Window(window)), _) => MenuTarget::Window(window),
            (Some(Content::Shortcut(_)), Key::App(id)) => MenuTarget::App(id.clone()),
            _ => return,
        };
        if let (Some(handler), Some((x, _))) = (handler, at) {
            // From the bar's top edge, so that the menu opens above it.
            handler(target, x, top);
        }
    }

    /// Start a pinned app. For a window: focus it, or minimize it if it has
    /// the focus already.
    fn click(&self, key: &Key) {
        if crate::lockscreen::is_locked() {
            return;
        }
        let Some(content) = self.content(key) else { return };
        self.hide_preview();
        match content {
            Content::Shortcut(app) => pins::launch(self.display, &app, false),
            Content::Window(window) => {
                let has_focus = self.state.borrow().focused == Some(windows::id(&window));
                if has_focus && !windows::is_minimized(&window) {
                    unsafe { ffi::meta_window_minimize(windows::ptr(&window)) };
                } else {
                    windows::activate(self.display, &window);
                }
            }
        }
    }

    fn enter(self: &Rc<Self>, key: Key) {
        self.state.borrow_mut().hovered = Some(key);
        self.repaint();
        // Moving along the bar switches an open preview at once.
        if self.state.borrow().preview_shown {
            self.show_preview();
            return;
        }
        self.cancel_timeout();
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(PREVIEW_DELAY, move || {
            with(&weak, |t| {
                t.state.borrow_mut().preview_timeout = None;
                t.show_preview();
            });
        });
        self.state.borrow_mut().preview_timeout = Some(source);
    }

    fn leave(self: &Rc<Self>, key: &Key) {
        if self.state.borrow().hovered.as_ref() != Some(key) {
            return;
        }
        self.state.borrow_mut().hovered = None;
        self.repaint();
        self.cancel_timeout();
        // Wait a moment, so that crossing the gap to the next entry does not
        // close the preview and start the delay again.
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(Duration::from_millis(80), move || {
            with(&weak, |t| {
                t.state.borrow_mut().preview_timeout = None;
                t.hide_preview();
            });
        });
        self.state.borrow_mut().preview_timeout = Some(source);
    }

    fn repaint(&self) {
        let state = self.state.borrow();
        for item in &state.items {
            let focused = matches!(item.key, Key::Window(id) if state.focused == Some(id));
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

    /// The window's title and picture, or a pinned app's name.
    fn show_preview(&self) {
        let found = {
            let state = self.state.borrow();
            let (x, y, bar_width) = state.geometry;
            state.hovered.as_ref().and_then(|hovered| {
                let item = state.items.iter().find(|i| &i.key == hovered)?;
                let center = x + item.span.0 + item.span.1 / 2.0;
                Some((item.content.clone(), center, x, y, bar_width))
            })
        };
        let Some((content, center, bar_x, bar_y, bar_width)) = found else { return };
        self.cancel_timeout();
        unsafe {
            ffi::clutter_actor_destroy_all_children(self.preview);
            let (title, thumbnail, least_width) = match &content {
                Content::Window(window) => (
                    windows::title(window),
                    windows::thumbnail(window, PREVIEW_WIDTH, PREVIEW_HEIGHT),
                    220.0,
                ),
                Content::Shortcut(app) => (app.display_name().to_string(), None, 0.0),
            };
            let title = ui::label(ui::BODY_FONT, &title, &ui::TEXT_COLOR);
            let (thumb_width, thumb_height) = thumbnail.map_or((0.0, 0.0), |(_, w, h)| (w, h));
            let inner = match content {
                Content::Window(_) => thumb_width.max(least_width),
                Content::Shortcut(_) => ffi::clutter_actor_get_width(title).min(PREVIEW_WIDTH).ceil(),
            };
            let width = inner + 2.0 * PREVIEW_PADDING;
            let height = 2.0 * PREVIEW_PADDING + PREVIEW_TITLE_HEIGHT + thumb_height;

            ffi::clutter_text_set_ellipsize(title as *mut ffi::ClutterText, ffi::PANGO_ELLIPSIZE_END);
            ffi::clutter_actor_set_width(title, inner);
            ffi::clutter_actor_set_position(title, PREVIEW_PADDING, PREVIEW_PADDING);
            ffi::clutter_actor_add_child(self.preview, title);
            if let Some((actor, w, _)) = thumbnail {
                ffi::clutter_actor_set_position(
                    actor,
                    PREVIEW_PADDING + ((inner - w) / 2.0).floor(),
                    PREVIEW_PADDING + PREVIEW_TITLE_HEIGHT,
                );
                ffi::clutter_actor_add_child(self.preview, actor);
            }
            let left = (center - width / 2.0).min(bar_x + bar_width - width - 4.0).max(bar_x + 4.0);
            ffi::clutter_actor_set_size(self.preview, width, height);
            ffi::clutter_actor_set_position(self.preview, left.floor(), bar_y - height - 6.0);
            ffi::clutter_actor_show(self.preview);
        }
        self.state.borrow_mut().preview_shown = true;
    }

    pub fn hide_preview(&self) {
        self.cancel_timeout();
        self.state.borrow_mut().preview_shown = false;
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

fn draw_item(
    actor: *mut ClutterActor,
    title: &str,
    icon: Option<&glib::Object>,
    width: f32,
    with_label: bool,
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
    fn entries_shrink_to_share_the_bar() {
        assert_eq!(item_width(1280.0, 0), 0.0);
        assert_eq!(item_width(1280.0, 2), ITEM_MAX_WIDTH);
        let w = item_width(1280.0, 20);
        assert!(20.0 * w + 19.0 * ITEM_GAP + 2.0 * ITEM_MARGIN <= 1280.0);
        assert!(w > 50.0);
    }
}
