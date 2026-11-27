//! The bar at the bottom of the primary monitor: one entry per open window,
//! with a live preview while the pointer rests on an entry.

use crate::ffi::{self, ClutterActor, MetaDisplay};
use crate::ui::{self, gobj};
use crate::windows;
use glib::prelude::*;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::Duration;

pub const HEIGHT: i32 = 40;
const ITEM_MAX_WIDTH: f32 = 220.0;
const ITEM_GAP: f32 = 4.0;
const ITEM_MARGIN: f32 = 4.0;
const ICON_SIZE: f32 = 20.0;
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

struct Item {
    window: glib::Object,
    actor: *mut ClutterActor,
    /// What the actor was drawn from, to redraw only on change.
    drawn: (String, f32),
}

#[derive(Default)]
struct State {
    items: Vec<Item>,
    /// Origin of the bar in stage coordinates, and its width.
    geometry: (f32, f32, f32),
    /// The window under the pointer.
    hovered: Option<u64>,
    preview_timeout: Option<glib::SourceId>,
    preview_shown: bool,
    focused: Option<u64>,
}

pub struct Taskbar {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    bar: *mut ClutterActor,
    preview: *mut ClutterActor,
    state: RefCell<State>,
}

impl Taskbar {
    pub fn new(display: *mut MetaDisplay, stage: *mut ClutterActor, color: &ffi::CoglColor) -> Rc<Self> {
        unsafe {
            let bar = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(bar, color);
            ffi::clutter_actor_set_reactive(bar, 1);
            ffi::clutter_actor_add_child(stage, bar);

            let preview = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(preview, &ui::BOX_COLOR);
            ffi::clutter_actor_hide(preview);
            ffi::clutter_actor_add_child(stage, preview);

            Rc::new(Self { display, stage, bar, preview, state: RefCell::default() })
        }
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

    /// Bring the entries up to date. Entries of windows that stay keep their
    /// actor, so the one under the pointer goes on getting events.
    pub fn refresh(self: &Rc<Self>) {
        let list = windows::list(self.display);
        let focused = windows::focused(self.display);
        let mut old = {
            let mut state = self.state.borrow_mut();
            state.focused = focused;
            std::mem::take(&mut state.items)
        };
        let bar_width = self.state.borrow().geometry.2;
        let width = item_width(bar_width, list.len());
        let scale = ui::primary_monitor_scale(self.display);

        let mut items = Vec::new();
        for (index, window) in list.into_iter().enumerate() {
            let id = windows::id(&window);
            let mut item = match old.iter().position(|i| windows::id(&i.window) == id) {
                Some(found) => old.remove(found),
                None => Item { actor: self.new_item(id), window, drawn: (String::new(), -1.0) },
            };
            let title = windows::title(&item.window);
            // The icon can be known only some time after the window appears.
            let icon = windows::icon(&item.window, ICON_SIZE, scale, self.stage);
            let key = (format!("{title}\u{1}{}", icon.is_some()), width);
            if item.drawn != key {
                draw_item(item.actor, &title, icon.as_ref(), width);
                item.drawn = key;
            }
            unsafe {
                ffi::clutter_actor_set_position(
                    item.actor,
                    ITEM_MARGIN + index as f32 * (width + ITEM_GAP),
                    ITEM_MARGIN,
                );
                let opacity = if windows::is_minimized(&item.window) { MINIMIZED_OPACITY } else { 0xff };
                ffi::clutter_actor_set_opacity(item.actor, opacity);
            }
            items.push(item);
        }
        let hovered = self.state.borrow().hovered;
        for item in &items {
            self.paint_item(item.actor, windows::id(&item.window), focused, hovered);
        }
        let hovered_gone = hovered.is_some_and(|h| !items.iter().any(|i| windows::id(&i.window) == h));
        self.state.borrow_mut().items = items;
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

    fn paint_item(&self, actor: *mut ClutterActor, id: u64, focused: Option<u64>, hovered: Option<u64>) {
        let color = if focused == Some(id) {
            FOCUS_COLOR
        } else if hovered == Some(id) {
            HOVER_COLOR
        } else {
            ITEM_COLOR
        };
        unsafe { ffi::clutter_actor_set_background_color(actor, &color) };
    }

    fn new_item(self: &Rc<Self>, id: u64) -> *mut ClutterActor {
        let actor = unsafe {
            let actor = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(actor, 1);
            ffi::clutter_actor_add_child(self.bar, actor);
            actor
        };
        let object = unsafe { gobj(actor) };
        let weak = Rc::downgrade(self);
        object.connect_local("enter-event", false, move |_| {
            with(&weak, |t| t.enter(id));
            Some(false.to_value())
        });
        let weak = Rc::downgrade(self);
        object.connect_local("leave-event", false, move |_| {
            with(&weak, |t| t.leave(id));
            Some(false.to_value())
        });
        let weak = Rc::downgrade(self);
        object.connect_local("button-press-event", false, move |args| {
            if unsafe { ffi::clutter_event_get_button(ui::event_arg(args)) } == 1 {
                with(&weak, |t| t.click(id));
            }
            Some(true.to_value())
        });
        actor
    }

    /// Focus the window, or minimize it if it has the focus already.
    fn click(&self, id: u64) {
        if crate::lockscreen::is_locked() {
            return;
        }
        let found = {
            let state = self.state.borrow();
            let item = state.items.iter().find(|i| windows::id(&i.window) == id);
            item.map(|i| (i.window.clone(), state.focused == Some(id)))
        };
        let Some((window, has_focus)) = found else { return };
        self.hide_preview();
        if has_focus && !windows::is_minimized(&window) {
            unsafe { ffi::meta_window_minimize(windows::ptr(&window)) };
        } else {
            windows::activate(self.display, &window);
        }
    }

    fn enter(self: &Rc<Self>, id: u64) {
        self.state.borrow_mut().hovered = Some(id);
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

    fn leave(self: &Rc<Self>, id: u64) {
        if self.state.borrow().hovered != Some(id) {
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
            self.paint_item(item.actor, windows::id(&item.window), state.focused, state.hovered);
        }
    }

    fn cancel_timeout(&self) {
        let source = self.state.borrow_mut().preview_timeout.take();
        if let Some(source) = source {
            source.remove();
        }
    }

    fn show_preview(&self) {
        let found = {
            let state = self.state.borrow();
            let (x, y, bar_width) = state.geometry;
            state.hovered.and_then(|hovered| {
                let index = state.items.iter().position(|i| windows::id(&i.window) == hovered)?;
                let item = &state.items[index];
                let width = item_width(bar_width, state.items.len());
                let center = x + ITEM_MARGIN + index as f32 * (width + ITEM_GAP) + width / 2.0;
                Some((item.window.clone(), center, x, y, bar_width))
            })
        };
        let Some((window, center, bar_x, bar_y, bar_width)) = found else { return };
        self.cancel_timeout();
        unsafe {
            ffi::clutter_actor_destroy_all_children(self.preview);
            let thumbnail = windows::thumbnail(&window, PREVIEW_WIDTH, PREVIEW_HEIGHT);
            let (thumb_width, thumb_height) = thumbnail.map_or((0.0, 0.0), |(_, w, h)| (w, h));
            let inner = thumb_width.max(220.0);
            let width = inner + 2.0 * PREVIEW_PADDING;
            let height = 2.0 * PREVIEW_PADDING + PREVIEW_TITLE_HEIGHT + thumb_height;

            let title = ui::label(ui::BODY_FONT, &windows::title(&window), &ui::TEXT_COLOR);
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

fn draw_item(actor: *mut ClutterActor, title: &str, icon: Option<&glib::Object>, width: f32) {
    let height = HEIGHT as f32 - 2.0 * ITEM_MARGIN;
    unsafe {
        ffi::clutter_actor_destroy_all_children(actor);
        ffi::clutter_actor_set_size(actor, width, height);
        ffi::clutter_actor_set_clip_to_allocation(actor, 1);
        let with_label = width >= LABEL_MIN_WIDTH || icon.is_none();
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
    fn entries_shrink_to_share_the_bar() {
        assert_eq!(item_width(1280.0, 0), 0.0);
        assert_eq!(item_width(1280.0, 2), ITEM_MAX_WIDTH);
        let w = item_width(1280.0, 20);
        assert!(20.0 * w + 19.0 * ITEM_GAP + 2.0 * ITEM_MARGIN <= 1280.0);
        assert!(w > 50.0);
    }
}
