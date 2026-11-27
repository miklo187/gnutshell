//! The window overview: every open window as a tile with a live picture,
//! scrollable when the tiles do not fit.

use crate::ffi::{self, ClutterActor, MetaDisplay};
use crate::ui::{self, gobj};
use crate::windows;
use glib::prelude::*;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

const TILE_WIDTH: f32 = 320.0;
const TILE_HEIGHT: f32 = 240.0;
const TILE_GAP: f32 = 20.0;
const TILE_PADDING: f32 = 10.0;
const TILE_TITLE_HEIGHT: f32 = 28.0;
const ICON_SIZE: f32 = 20.0;
const MARGIN: f32 = 40.0;
/// Pixels per wheel notch.
const SCROLL_STEP: f32 = 80.0;
/// The grabbable width of the scrollbar and the width of what is drawn.
const SCROLLBAR_HIT: f32 = 16.0;
const SCROLLBAR_WIDTH: f32 = 6.0;
const SCROLLBAR_GAP: f32 = 12.0;
const THUMB_MIN: f32 = 32.0;
const BACKDROP: ffi::CoglColor = ffi::CoglColor::rgba(0, 0, 0, 0xd8);
const TILE_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0x2c, 0x2c, 0x34, 0xff);
const TRACK_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x18);
const THUMB_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x80);

/// Width of the panel button that opens the overview.
pub const PANEL_BUTTON_WIDTH: f32 = 36.0;
const SQUARE: f32 = 6.0;
const SQUARE_GAP: f32 = 2.0;

/// Add the four-square button at `x` in `panel`; `pressed` runs on a primary
/// click.
pub fn add_panel_button(panel: *mut ClutterActor, x: f32, height: f32, pressed: impl Fn() + 'static) {
    let side = 2.0 * SQUARE + SQUARE_GAP;
    let left = ((PANEL_BUTTON_WIDTH - side) / 2.0).floor();
    let top = ((height - side) / 2.0).floor();
    let squares: Vec<_> = (0..4)
        .map(|i| {
            let step = SQUARE + SQUARE_GAP;
            (left + (i % 2) as f32 * step, top + (i / 2) as f32 * step, SQUARE, SQUARE)
        })
        .collect();
    ui::panel_button(panel, x, PANEL_BUTTON_WIDTH, height, &squares, pressed);
}

/// How many columns of tiles `count` windows get in `width`.
fn columns(count: usize, width: f32) -> usize {
    (((width + TILE_GAP) / (TILE_WIDTH + TILE_GAP)).floor() as usize).clamp(1, count.max(1))
}

/// Height of `rows` rows of tiles.
fn rows_height(rows: usize) -> f32 {
    if rows == 0 { 0.0 } else { rows as f32 * (TILE_HEIGHT + TILE_GAP) - TILE_GAP }
}

/// The scroll position moved just enough from `offset` to show row `row`.
fn offset_to_show(offset: f32, view: f32, row: usize) -> f32 {
    let top = row as f32 * (TILE_HEIGHT + TILE_GAP);
    let bottom = top + TILE_HEIGHT;
    if top < offset {
        top
    } else if bottom > offset + view {
        (bottom - view).min(top)
    } else {
        offset
    }
}

/// Position and length of the scrollbar thumb in a track as long as the view.
fn thumb(view: f32, content: f32, offset: f32) -> (f32, f32) {
    let length = (view * view / content).max(THUMB_MIN).min(view);
    let range = (content - view).max(1.0);
    ((view - length) * (offset / range).clamp(0.0, 1.0), length)
}

/// The scroll position that puts the thumb's top at `top`.
fn offset_for_thumb(view: f32, content: f32, top: f32) -> f32 {
    let (_, length) = thumb(view, content, 0.0);
    let travel = (view - length).max(1.0);
    (top / travel).clamp(0.0, 1.0) * (content - view).max(0.0)
}

/// The tile reached from `selected` by moving `dx` or `dy` in a grid of
/// `count` tiles in `columns` columns.
fn step(selected: usize, count: usize, columns: usize, dx: isize, dy: isize) -> usize {
    if count == 0 {
        return 0;
    }
    let last = count as isize - 1;
    if dy == 0 {
        return (selected as isize + dx).clamp(0, last) as usize;
    }
    let target = selected as isize + dy * columns as isize;
    if target < 0 {
        selected
    } else if target > last {
        // Down into a short last row lands on its end.
        if (selected / columns) < (last as usize / columns) { last as usize } else { selected }
    } else {
        target as usize
    }
}

#[derive(Default)]
struct State {
    grab: Option<*mut ffi::ClutterGrab>,
    windows: Vec<glib::Object>,
    tiles: Vec<*mut ClutterActor>,
    selected: usize,
    columns: usize,
    offset: f32,
    /// Height of all rows, and of the part that is visible.
    content: f32,
    view: f32,
    scroll_acc: f64,
    /// While the thumb is dragged: how far below its top it was taken.
    drag: Option<f32>,
    /// Top of the scrollbar in stage coordinates.
    track_top: f32,
}

pub struct Overview {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    overlay: *mut ClutterActor,
    /// Clips the grid to the visible area.
    viewport: *mut ClutterActor,
    grid: *mut ClutterActor,
    scrollbar: *mut ClutterActor,
    track: *mut ClutterActor,
    thumb: *mut ClutterActor,
    empty: *mut ClutterActor,
    state: RefCell<State>,
}

impl Overview {
    /// Build the (hidden) overview as the topmost child of `stage`.
    pub fn new(display: *mut MetaDisplay, stage: *mut ClutterActor) -> Rc<Self> {
        let overview = unsafe {
            let overlay = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(overlay, &BACKDROP);
            ffi::clutter_actor_set_reactive(overlay, 1);
            ffi::clutter_actor_hide(overlay);

            let viewport = ffi::clutter_actor_new();
            ffi::clutter_actor_set_clip_to_allocation(viewport, 1);
            ffi::clutter_actor_add_child(overlay, viewport);
            let grid = ffi::clutter_actor_new();
            ffi::clutter_actor_add_child(viewport, grid);

            let scrollbar = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(scrollbar, 1);
            ffi::clutter_actor_add_child(overlay, scrollbar);
            let track = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(track, &TRACK_COLOR);
            ffi::clutter_actor_add_child(scrollbar, track);
            let thumb = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(thumb, &THUMB_COLOR);
            ffi::clutter_actor_add_child(scrollbar, thumb);

            let empty = ui::label(ui::ROW_FONT, "No open windows", &ui::HINT_COLOR);
            ffi::clutter_actor_add_child(overlay, empty);

            ffi::clutter_actor_add_child(stage, overlay);
            Rc::new(Self {
                display,
                stage,
                overlay,
                viewport,
                grid,
                scrollbar,
                track,
                thumb,
                empty,
                state: RefCell::default(),
            })
        };
        overview.connect_signals();
        overview
    }

    fn connect_signals(self: &Rc<Self>) {
        let overlay = unsafe { gobj(self.overlay) };

        let weak = Rc::downgrade(self);
        overlay.connect_local("key-press-event", false, move |args| {
            with(&weak, |o| o.on_key(ui::key_symbol(ui::event_arg(args))));
            Some(true.to_value())
        });
        // A click on anything that is not a tile or the scrollbar closes.
        let weak = Rc::downgrade(self);
        overlay.connect_local("button-press-event", false, move |_| {
            with(&weak, |o| o.hide());
            Some(true.to_value())
        });
        let weak = Rc::downgrade(self);
        overlay.connect_local("scroll-event", false, move |args| {
            with(&weak, |o| o.on_scroll(ui::event_arg(args)));
            Some(true.to_value())
        });

        let scrollbar = unsafe { gobj(self.scrollbar) };
        let weak = Rc::downgrade(self);
        scrollbar.connect_local("button-press-event", false, move |args| {
            with(&weak, |o| o.drag_start(ui::event_arg(args)));
            Some(true.to_value())
        });
        let weak = Rc::downgrade(self);
        scrollbar.connect_local("motion-event", false, move |args| {
            with(&weak, |o| o.drag_move(ui::event_arg(args)));
            Some(true.to_value())
        });
        let weak = Rc::downgrade(self);
        scrollbar.connect_local("button-release-event", false, move |_| {
            with(&weak, |o| o.state.borrow_mut().drag = None);
            Some(true.to_value())
        });
    }

    pub fn is_visible(&self) -> bool {
        self.state.borrow().grab.is_some()
    }

    pub fn toggle(self: &Rc<Self>) {
        if self.is_visible() { self.hide() } else { self.show() }
    }

    pub fn show(self: &Rc<Self>) {
        if self.is_visible() {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.offset = 0.0;
            state.scroll_acc = 0.0;
            state.drag = None;
        }
        let focused = windows::focused(self.display);
        self.build(focused);
        unsafe {
            ffi::clutter_actor_show(self.overlay);
            let grab = ffi::clutter_stage_grab(self.stage as *mut ffi::ClutterStage, self.overlay);
            self.state.borrow_mut().grab = Some(grab);
            ffi::clutter_actor_grab_key_focus(self.overlay);
        }
    }

    pub fn hide(&self) {
        let Some(grab) = self.state.borrow_mut().grab.take() else { return };
        unsafe {
            ffi::clutter_grab_dismiss(grab);
            gobject_sys::g_object_unref(grab as *mut gobject_sys::GObject);
            ffi::clutter_actor_hide(self.overlay);
        }
        {
            let mut state = self.state.borrow_mut();
            state.windows.clear();
            state.tiles.clear();
            state.drag = None;
        }
        // Not under the borrow: destroying actors can send crossing events.
        unsafe { ffi::clutter_actor_destroy_all_children(self.grid) };
    }

    /// Windows opened, closed or changed: rebuild, keeping the selection.
    pub fn refresh(self: &Rc<Self>) {
        if !self.is_visible() {
            return;
        }
        let selected = {
            let state = self.state.borrow();
            state.windows.get(state.selected).map(windows::id)
        };
        self.build(selected);
    }

    pub fn destroy(&self) {
        self.hide();
        unsafe { ffi::clutter_actor_destroy(self.overlay) };
    }

    // ------------------------------------------------------------------ internals

    /// Lay out the tiles, selecting the window `select` if it is there.
    fn build(self: &Rc<Self>, select: Option<u64>) {
        let list = windows::list(self.display);
        let geo = ui::primary_monitor_geometry(self.display).unwrap_or_default();
        let (width, height) = (geo.width as f32, geo.height as f32);
        let count = list.len();
        let columns = columns(count, width - 2.0 * MARGIN - SCROLLBAR_GAP - SCROLLBAR_HIT);
        let content = rows_height(count.div_ceil(columns));
        let view = content.min(height - 2.0 * MARGIN).max(1.0);
        let grid_width = columns as f32 * (TILE_WIDTH + TILE_GAP) - TILE_GAP;
        let left = ((width - grid_width) / 2.0).floor().max(0.0);
        let top = ((height - view) / 2.0).floor();

        unsafe { ffi::clutter_actor_destroy_all_children(self.grid) };
        let scale = ui::primary_monitor_scale(self.display);
        let tiles: Vec<*mut ClutterActor> = list
            .iter()
            .enumerate()
            .map(|(index, window)| {
                let tile = self.new_tile(window, scale);
                unsafe {
                    ffi::clutter_actor_set_position(
                        tile,
                        (index % columns) as f32 * (TILE_WIDTH + TILE_GAP),
                        (index / columns) as f32 * (TILE_HEIGHT + TILE_GAP),
                    );
                }
                tile
            })
            .collect();

        unsafe {
            ffi::clutter_actor_set_position(self.overlay, geo.x as f32, geo.y as f32);
            ffi::clutter_actor_set_size(self.overlay, width, height);
            ffi::clutter_actor_set_position(self.viewport, left, top);
            ffi::clutter_actor_set_size(self.viewport, grid_width, view);
            ffi::clutter_actor_set_size(self.grid, grid_width, content);

            if content > view {
                ffi::clutter_actor_set_position(self.scrollbar, left + grid_width + SCROLLBAR_GAP, top);
                ffi::clutter_actor_set_size(self.scrollbar, SCROLLBAR_HIT, view);
                ffi::clutter_actor_set_position(self.track, (SCROLLBAR_HIT - SCROLLBAR_WIDTH) / 2.0, 0.0);
                ffi::clutter_actor_set_size(self.track, SCROLLBAR_WIDTH, view);
                ffi::clutter_actor_show(self.scrollbar);
            } else {
                ffi::clutter_actor_hide(self.scrollbar);
            }

            if count == 0 {
                let x = (width - ffi::clutter_actor_get_width(self.empty)) / 2.0;
                ffi::clutter_actor_set_position(self.empty, x.floor(), (height / 2.0).floor());
                ffi::clutter_actor_show(self.empty);
            } else {
                ffi::clutter_actor_hide(self.empty);
            }
        }

        let selected = select.and_then(|id| list.iter().position(|w| windows::id(w) == id)).unwrap_or(0);
        {
            let mut state = self.state.borrow_mut();
            state.windows = list;
            state.tiles = tiles;
            state.columns = columns;
            state.content = content;
            state.view = view;
            state.selected = selected;
            state.track_top = geo.y as f32 + top;
        }
        self.highlight();
        let offset = self.state.borrow().offset;
        self.scroll_to(offset);
        if select.is_some() {
            self.reveal_selected();
        }
    }

    fn new_tile(self: &Rc<Self>, window: &glib::Object, scale: f32) -> *mut ClutterActor {
        let tile = unsafe {
            let tile = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(tile, TILE_WIDTH, TILE_HEIGHT);
            ffi::clutter_actor_set_background_color(tile, &TILE_COLOR);
            ffi::clutter_actor_set_reactive(tile, 1);
            ffi::clutter_actor_add_child(self.grid, tile);

            let icon = windows::icon(window, ICON_SIZE, scale, self.stage);
            windows::add_icon(tile, icon.as_ref(), ICON_SIZE, TILE_PADDING, TILE_PADDING);
            let text_x = TILE_PADDING + if icon.is_some() { ICON_SIZE + 8.0 } else { 0.0 };
            let title = ui::label(ui::BODY_FONT, &windows::title(window), &ui::TEXT_COLOR);
            ffi::clutter_text_set_ellipsize(title as *mut ffi::ClutterText, ffi::PANGO_ELLIPSIZE_END);
            ffi::clutter_actor_set_width(title, TILE_WIDTH - text_x - TILE_PADDING);
            ffi::clutter_actor_set_position(title, text_x, TILE_PADDING);
            ffi::clutter_actor_add_child(tile, title);

            let area_width = TILE_WIDTH - 2.0 * TILE_PADDING;
            let area_height = TILE_HEIGHT - 2.0 * TILE_PADDING - TILE_TITLE_HEIGHT;
            if let Some((picture, w, h)) = windows::thumbnail(window, area_width, area_height) {
                ffi::clutter_actor_set_position(
                    picture,
                    TILE_PADDING + ((area_width - w) / 2.0).floor(),
                    TILE_PADDING + TILE_TITLE_HEIGHT + ((area_height - h) / 2.0).floor(),
                );
                ffi::clutter_actor_add_child(tile, picture);
            }
            tile
        };
        let object = unsafe { gobj(tile) };
        let id = windows::id(window);
        let weak = Rc::downgrade(self);
        object.connect_local("button-press-event", false, move |args| {
            if unsafe { ffi::clutter_event_get_button(ui::event_arg(args)) } == 1 {
                with(&weak, |o| o.choose(id));
            }
            Some(true.to_value())
        });
        // The pointer and the keys move the same selection.
        let weak = Rc::downgrade(self);
        object.connect_local("motion-event", false, move |_| {
            with(&weak, |o| {
                let changed = {
                    let mut state = o.state.borrow_mut();
                    match state.windows.iter().position(|w| windows::id(w) == id) {
                        Some(index) if index != state.selected => {
                            state.selected = index;
                            true
                        }
                        _ => false,
                    }
                };
                if changed {
                    o.highlight();
                }
            });
            Some(false.to_value())
        });
        tile
    }

    fn choose(&self, id: u64) {
        let window = self.state.borrow().windows.iter().find(|w| windows::id(w) == id).cloned();
        self.hide();
        // It may have been closed since the tiles were built.
        if let Some(window) = window.filter(|_| windows::list(self.display).iter().any(|w| windows::id(w) == id)) {
            windows::activate(self.display, &window);
        }
    }

    fn highlight(&self) {
        let state = self.state.borrow();
        for (index, tile) in state.tiles.iter().enumerate() {
            let color = if index == state.selected { &ui::SELECTED_COLOR } else { &TILE_COLOR };
            unsafe { ffi::clutter_actor_set_background_color(*tile, color) };
        }
    }

    fn scroll_to(&self, offset: f32) {
        let (offset, view, content) = {
            let mut state = self.state.borrow_mut();
            state.offset = offset.clamp(0.0, (state.content - state.view).max(0.0)).round();
            (state.offset, state.view, state.content)
        };
        unsafe {
            ffi::clutter_actor_set_position(self.grid, 0.0, -offset);
            if content > view {
                let (top, length) = thumb(view, content, offset);
                ffi::clutter_actor_set_position(self.thumb, (SCROLLBAR_HIT - SCROLLBAR_WIDTH) / 2.0, top);
                ffi::clutter_actor_set_size(self.thumb, SCROLLBAR_WIDTH, length);
            }
        }
    }

    fn reveal_selected(&self) {
        let offset = {
            let state = self.state.borrow();
            offset_to_show(state.offset, state.view, state.selected / state.columns.max(1))
        };
        self.scroll_to(offset);
    }

    fn on_scroll(&self, event: *const ffi::ClutterEvent) {
        let (rows, offset) = {
            let mut state = self.state.borrow_mut();
            let mut acc = state.scroll_acc;
            let rows = ui::scroll_rows(event, &mut acc);
            state.scroll_acc = acc;
            (rows, state.offset)
        };
        if rows != 0 {
            self.scroll_to(offset + rows as f32 * SCROLL_STEP);
        }
    }

    /// A press on the thumb takes hold of it; elsewhere on the track the
    /// thumb jumps to the pointer first.
    fn drag_start(&self, event: *const ffi::ClutterEvent) {
        let Some((_, y)) = ui::event_coords(event) else { return };
        {
            let mut state = self.state.borrow_mut();
            let y = y - state.track_top;
            let (top, length) = thumb(state.view, state.content, state.offset);
            let grip = if y >= top && y <= top + length { y - top } else { length / 2.0 };
            state.drag = Some(grip);
        }
        self.drag_move(event);
    }

    fn drag_move(&self, event: *const ffi::ClutterEvent) {
        let Some((_, y)) = ui::event_coords(event) else { return };
        let offset = {
            let state = self.state.borrow();
            state.drag.map(|grip| offset_for_thumb(state.view, state.content, y - state.track_top - grip))
        };
        if let Some(offset) = offset {
            self.scroll_to(offset);
        }
    }

    fn on_key(&self, keysym: u32) {
        let (dx, dy) = match keysym {
            ui::KEY_ESCAPE => return self.hide(),
            ui::KEY_RETURN | ui::KEY_KP_ENTER => {
                let id = {
                    let state = self.state.borrow();
                    state.windows.get(state.selected).map(windows::id)
                };
                if let Some(id) = id {
                    self.choose(id);
                }
                return;
            }
            ui::KEY_LEFT | ui::KEY_ISO_LEFT_TAB => (-1, 0),
            ui::KEY_RIGHT | ui::KEY_TAB => (1, 0),
            ui::KEY_UP => (0, -1),
            ui::KEY_DOWN => (0, 1),
            _ => return,
        };
        {
            let mut state = self.state.borrow_mut();
            state.selected = step(state.selected, state.windows.len(), state.columns.max(1), dx, dy);
        }
        self.highlight();
        self.reveal_selected();
    }
}

fn with(weak: &Weak<Overview>, f: impl FnOnce(&Rc<Overview>)) {
    if let Some(overview) = weak.upgrade() {
        f(&overview);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_follow_width_and_count() {
        assert_eq!(columns(10, 1200.0), 3);
        assert_eq!(columns(2, 1200.0), 2);
        assert_eq!(columns(0, 1200.0), 1);
        assert_eq!(columns(10, 100.0), 1);
    }

    #[test]
    fn thumb_spans_the_track() {
        let (top, length) = thumb(500.0, 1000.0, 0.0);
        assert_eq!((top, length), (0.0, 250.0));
        assert_eq!(thumb(500.0, 1000.0, 500.0), (250.0, 250.0));
        assert_eq!(thumb(500.0, 100000.0, 0.0).1, THUMB_MIN);
    }

    #[test]
    fn dragging_the_thumb_covers_the_whole_content() {
        assert_eq!(offset_for_thumb(500.0, 1000.0, -20.0), 0.0);
        assert_eq!(offset_for_thumb(500.0, 1000.0, 125.0), 250.0);
        assert_eq!(offset_for_thumb(500.0, 1000.0, 900.0), 500.0);
    }

    #[test]
    fn scrolls_just_enough_to_show_a_row() {
        assert_eq!(offset_to_show(0.0, 640.0, 0), 0.0);
        assert_eq!(offset_to_show(0.0, 640.0, 2), 2.0 * 260.0 + 240.0 - 640.0);
        assert_eq!(offset_to_show(400.0, 640.0, 0), 0.0);
        // A view lower than a tile shows the tile's top.
        assert_eq!(offset_to_show(0.0, 100.0, 1), 260.0);
    }

    #[test]
    fn arrows_move_in_the_grid() {
        // 8 tiles in 3 columns: rows 012 345 67
        assert_eq!(step(0, 8, 3, -1, 0), 0);
        assert_eq!(step(2, 8, 3, 1, 0), 3);
        assert_eq!(step(1, 8, 3, 0, 1), 4);
        assert_eq!(step(5, 8, 3, 0, 1), 7);
        assert_eq!(step(6, 8, 3, 0, 1), 6);
        assert_eq!(step(1, 8, 3, 0, -1), 1);
        assert_eq!(step(0, 0, 1, 1, 0), 0);
    }
}
