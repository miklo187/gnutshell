//! The notification list: notifications whose banner went away without the
//! user acting on it, kept so they can be read and copied later. A click on
//! the bell at the right of the panel opens it; the bell turns blue while
//! there are unread ones. The two popups of this module share their overlay:
//! the list under the bell, and the calendar (`calendar.rs`) under the clock.
//!
//! Listed notifications are already closed as far as the sending app knows,
//! so they have no actions. The list lives in memory only.

use super::calendar;
use super::{
    APP_FONT, CLOSE_SIZE, MAX_BODY_LINES, Notification, Notifications, PADDING, RIGHT_MARGIN, SPACING, SUMMARY_FONT, WIDTH,
    with,
};
use crate::ffi::{self, ClutterActor};
use crate::icons;
use crate::ui::{self, gobj};
use gio::prelude::*;
use std::collections::VecDeque;
use std::ffi::CStr;
use std::rc::Rc;

/// Oldest entries beyond this are forgotten.
const MAX_HISTORY: usize = 50;
const HEADER_HEIGHT: f32 = 32.0;
const CLEAR_WIDTH: f32 = 90.0;
/// The line under the entries that says how many more there are.
const MORE_HEIGHT: f32 = 28.0;
const CLOCK_PADDING: f32 = 14.0;
/// Width of the bell's button in the panel, and the size of its icon.
const BELL_WIDTH: f32 = 32.0;
const BELL_ICON_SIZE: f32 = 16.0;
const HEADER_FONT: &CStr = c"Sans Bold 12";
const HOVER_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x20);
const CARD_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0x2a, 0x2a, 0x32, 0xff);

/// Closed notifications, the one that arrived last first.
#[derive(Default)]
pub struct History {
    entries: VecDeque<Notification>,
}

impl History {
    /// Add `n` in its place by arrival, replacing one with the same id.
    /// Returns `false` for transient notifications, which are never kept.
    pub fn push(&mut self, mut n: Notification) -> bool {
        self.remove(n.id);
        if n.transient {
            return false;
        }
        // The list shows no pictures, and they can be megabytes each.
        n.image = None;
        let at = self.entries.iter().position(|e| e.arrival <= n.arrival).unwrap_or(self.entries.len());
        self.entries.insert(at, n);
        self.entries.truncate(MAX_HISTORY);
        true
    }

    pub fn remove(&mut self, id: u32) -> bool {
        let before = self.entries.len();
        self.entries.retain(|n| n.id != id);
        self.entries.len() != before
    }

    /// Who sent the entry `id`.
    pub fn sender(&self, id: u32) -> Option<&str> {
        self.entries.iter().find(|n| n.id == id).map(|n| n.sender.as_str())
    }

    pub fn contains(&self, id: u32) -> bool {
        self.entries.iter().any(|n| n.id == id)
    }
}

/// The actors of the list and the calendar.
pub struct ListView {
    /// Clickable area over the panel clock.
    clock_button: *mut ClutterActor,
    clock: *mut ClutterActor,
    bell_button: *mut ClutterActor,
    bell_icon: *mut ClutterActor,
    /// Where the bell is in the panel.
    bell_x: std::cell::Cell<f32>,
    /// Full-monitor transparent actor that holds the grab.
    overlay: *mut ClutterActor,
    panel_box: *mut ClutterActor,
}

#[derive(Default)]
pub struct ListState {
    pub history: History,
    grab: Option<*mut ffi::ClutterGrab>,
    /// Whether the list is open.
    pub open: bool,
    /// Whether the calendar is open.
    pub calendar: bool,
    unread: bool,
    /// Index of the first entry shown.
    offset: usize,
    /// Whether the oldest entry is on screen.
    at_end: bool,
    /// How many entries are on screen.
    shown: usize,
    scroll: f64,
    /// Months from the current one to the one the calendar shows.
    pub month: i32,
    pub month_scroll: f64,
}

impl ListView {
    /// `clock` is the clock text inside `panel`.
    pub fn new(stage: *mut ClutterActor, panel: *mut ClutterActor, clock: *mut ClutterActor, height: f32) -> Self {
        unsafe {
            let clock_button = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(clock_button, 1);
            ffi::clutter_actor_add_child(panel, clock_button);

            let bell_button = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(bell_button, 1);
            ffi::clutter_actor_set_size(bell_button, BELL_WIDTH, height);
            let bell_icon = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(bell_icon, BELL_ICON_SIZE, BELL_ICON_SIZE);
            ffi::clutter_actor_set_position(
                bell_icon,
                ((BELL_WIDTH - BELL_ICON_SIZE) / 2.0).floor(),
                ((height - BELL_ICON_SIZE) / 2.0).floor(),
            );
            ffi::clutter_actor_add_child(bell_button, bell_icon);
            ffi::clutter_actor_add_child(panel, bell_button);

            let overlay = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(overlay, 1);
            ffi::clutter_actor_hide(overlay);
            let panel_box = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(panel_box, &ui::BOX_COLOR);
            ffi::clutter_actor_set_reactive(panel_box, 1);
            ffi::clutter_actor_add_child(overlay, panel_box);
            ffi::clutter_actor_add_child(stage, overlay);

            Self { clock_button, clock, bell_button, bell_icon, bell_x: Default::default(), overlay, panel_box }
        }
    }

    pub fn destroy(&self) {
        unsafe {
            ffi::clutter_actor_destroy(self.overlay);
            ffi::clutter_actor_destroy(self.clock_button);
            ffi::clutter_actor_destroy(self.bell_button);
        }
    }
}

impl Notifications {
    pub(super) fn connect_list(self: &Rc<Self>) {
        type Open = fn(&Rc<Notifications>);
        let buttons: [(*mut ClutterActor, Open); 2] = [
            (self.view.clock_button, Notifications::open_calendar),
            (self.view.bell_button, Notifications::open_list),
        ];
        for (button, open) in buttons {
            crate::windows::hover_highlight(button, ui::TRANSPARENT, HOVER_COLOR);
            let weak = Rc::downgrade(self);
            unsafe { gobj(button) }.connect_local("button-press-event", false, move |_| {
                with(&weak, open);
                Some(true.to_value())
            });
        }
        // Do Not Disturb changes the bell.
        if let Some(settings) = &self.settings {
            let weak = Rc::downgrade(self);
            settings.connect_changed(Some("show-banners"), move |_, _| with(&weak, |n| n.update_bell()));
        }
        self.update_bell();

        let overlay = unsafe { gobj(self.view.overlay) };
        let weak = Rc::downgrade(self);
        overlay.connect_local("key-press-event", false, move |args| {
            let key = ui::key_symbol(ui::event_arg(args));
            let mut handled = false;
            with(&weak, |n| {
                let calendar = n.state.borrow().list.calendar;
                handled = true;
                match key {
                    ui::KEY_ESCAPE => n.close_list(),
                    ui::KEY_UP if !calendar => n.scroll_list(-1),
                    ui::KEY_DOWN if !calendar => n.scroll_list(1),
                    ui::KEY_LEFT if calendar => n.turn_month(-1),
                    ui::KEY_RIGHT if calendar => n.turn_month(1),
                    ui::KEY_HOME if calendar => n.show_current_month(),
                    _ => handled = false,
                }
            });
            Some(handled.to_value())
        });
        // Click outside closes the list.
        let weak = Rc::downgrade(self);
        overlay.connect_local("button-press-event", false, move |_| {
            with(&weak, |n| n.close_list());
            Some(true.to_value())
        });
        let panel_box = unsafe { gobj(self.view.panel_box) };
        panel_box.connect_local("button-press-event", false, |_| Some(true.to_value()));
        let weak = Rc::downgrade(self);
        panel_box.connect_local("scroll-event", false, move |args| {
            with(&weak, |n| {
                let rows = ui::scroll_rows(ui::event_arg(args), &mut n.state.borrow_mut().list.scroll);
                if rows != 0 {
                    n.scroll_list(rows);
                }
            });
            Some(true.to_value())
        });
    }

    /// Fit the click area to the clock; call after it moves.
    pub fn place_clock(&self) {
        unsafe {
            let x = ffi::clutter_actor_get_x(self.view.clock);
            let width = ffi::clutter_actor_get_width(self.view.clock);
            ffi::clutter_actor_set_position(self.view.clock_button, x - CLOCK_PADDING, 0.0);
            ffi::clutter_actor_set_size(self.view.clock_button, width + 2.0 * CLOCK_PADDING, self.panel_height);
        }
    }

    /// Put the bell so that it ends at `right_edge` in the panel.
    pub fn place_bell(&self, right_edge: f32) {
        let x = right_edge - BELL_WIDTH;
        self.view.bell_x.set(x);
        unsafe { ffi::clutter_actor_set_position(self.view.bell_button, x, 0.0) };
    }

    fn popup_open(&self) -> bool {
        let state = self.state.borrow();
        state.list.open || state.list.calendar
    }

    pub fn open_list(self: &Rc<Self>) {
        if self.popup_open() || self.state.borrow().locked {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.list.open = true;
            state.list.offset = 0;
            state.list.scroll = 0.0;
        }
        // The banner and everything still waiting move into the list, oldest
        // first so that the newest ends up on top.
        let current = self.take_current();
        if let Some(n) = current {
            self.retire(n);
        }
        loop {
            let next = self.state.borrow_mut().queue.next(false);
            let Some(n) = next else { break };
            self.retire(n);
        }
        self.state.borrow_mut().list.unread = false;
        self.update_bell();
        self.build_list();
        self.show_popup();
    }

    /// Banners wait while the calendar is open, and show after it.
    pub fn open_calendar(self: &Rc<Self>) {
        if self.popup_open() || self.state.borrow().locked {
            return;
        }
        {
            let mut state = self.state.borrow_mut();
            state.list.calendar = true;
            state.list.month = 0;
            state.list.month_scroll = 0.0;
        }
        self.build_calendar();
        self.show_popup();
    }

    fn show_popup(&self) {
        unsafe {
            ffi::clutter_actor_show(self.view.overlay);
            ffi::clutter_actor_set_child_above_sibling(self.stage, self.view.overlay, std::ptr::null_mut());
            let grab = ffi::clutter_stage_grab(self.stage as *mut ffi::ClutterStage, self.view.overlay);
            self.state.borrow_mut().list.grab = Some(grab);
            ffi::clutter_actor_grab_key_focus(self.view.overlay);
        }
    }

    /// Close the list or the calendar, whichever is open.
    pub fn close_list(self: &Rc<Self>) {
        let grab = {
            let mut state = self.state.borrow_mut();
            if !state.list.open && !state.list.calendar {
                return;
            }
            state.list.open = false;
            state.list.calendar = false;
            state.list.grab.take()
        };
        self.release_list(grab);
        self.show_next();
    }

    pub(super) fn release_list(&self, grab: Option<*mut ffi::ClutterGrab>) {
        unsafe {
            if let Some(grab) = grab {
                ffi::clutter_grab_dismiss(grab);
                gobject_sys::g_object_unref(grab as *mut gobject_sys::GObject);
            }
            ffi::clutter_actor_hide(self.view.overlay);
            ffi::clutter_actor_destroy_all_children(self.view.panel_box);
        }
    }

    /// Drop the grab before shutting down.
    pub(super) fn destroy_list(&self) {
        let grab = self.state.borrow_mut().list.grab.take();
        self.release_list(grab);
        self.view.destroy();
    }

    /// Close `n` as expired and keep it in the list.
    pub(super) fn retire(self: &Rc<Self>, n: Notification) {
        self.emit_closed(&n, super::Reason::Expired);
        let open = {
            let mut state = self.state.borrow_mut();
            let kept = state.list.history.push(n);
            if kept && !state.list.open {
                state.list.unread = true;
            }
            state.list.open
        };
        self.update_bell();
        // While opening there is no grab yet, and the list is built after.
        if open && self.state.borrow().list.grab.is_some() {
            self.build_list();
        }
    }

    /// Take `id` out of the list, if it is there.
    pub(super) fn forget(self: &Rc<Self>, id: u32) {
        let (removed, open) = {
            let mut state = self.state.borrow_mut();
            let removed = state.list.history.remove(id);
            if state.list.history.entries.is_empty() {
                state.list.unread = false;
            }
            (removed, state.list.open)
        };
        if removed {
            self.update_bell();
            if open {
                self.build_list();
            }
        }
    }

    fn clear_list(self: &Rc<Self>) {
        self.state.borrow_mut().list.history.entries.clear();
        self.build_list();
    }

    fn scroll_list(self: &Rc<Self>, rows: i32) {
        {
            let mut state = self.state.borrow_mut();
            let list = &mut state.list;
            if rows > 0 && list.at_end {
                return;
            }
            let last = list.history.entries.len().saturating_sub(1);
            list.offset = list.offset.saturating_add_signed(rows as isize).min(last);
        }
        self.build_list();
    }

    /// The bell is blue while there are unread notifications, and crossed
    /// out with Do Not Disturb.
    fn update_bell(&self) {
        let unread = self.state.borrow().list.unread;
        let quiet = self.settings.as_ref().is_some_and(|s| !s.boolean("show-banners"));
        let name = if quiet { "notifications-disabled-symbolic" } else { "preferences-system-notifications-symbolic" };
        let color = if unread { &ui::SELECTED_COLOR } else { &ui::TEXT_COLOR };
        let scale = ui::primary_monitor_scale(self.display);
        let content = icons::symbolic(name, color, BELL_ICON_SIZE, scale, self.stage);
        let content = content.as_ref().map_or(std::ptr::null_mut(), |c| c.as_ptr() as *mut ffi::ClutterContent);
        unsafe { ffi::clutter_actor_set_content(self.view.bell_icon, content) };
    }

    /// Lay the list out below the bell: as many entries from `offset` as fit
    /// on the primary monitor.
    pub(super) fn build_list(self: &Rc<Self>) {
        let Some(geo) = ui::primary_monitor_geometry(self.display) else { return };
        let scale = ui::primary_monitor_scale(self.display);
        let (entries, offset) = {
            let mut state = self.state.borrow_mut();
            let list = &mut state.list;
            list.offset = list.offset.min(list.history.entries.len().saturating_sub(1));
            (list.history.entries.iter().cloned().collect::<Vec<_>>(), list.offset)
        };
        let width = WIDTH.min(geo.width as f32 - 32.0);
        // Down to the taskbar, at most.
        let max_height = geo.height as f32 - self.panel_height - crate::taskbar::HEIGHT as f32 - 2.0 * SPACING;
        let panel_box = self.view.panel_box;
        let mut at_end = true;
        let mut shown = 0;
        unsafe {
            ffi::clutter_actor_destroy_all_children(panel_box);

            let title = ui::label(HEADER_FONT, "Notifications", &ui::TEXT_COLOR);
            ffi::clutter_actor_add_child(panel_box, title);
            let title_y = SPACING + (HEADER_HEIGHT - ffi::clutter_actor_get_height(title)) / 2.0;
            ffi::clutter_actor_set_position(title, PADDING, title_y.floor());
            if !entries.is_empty() {
                let clear = ui::button("Clear all", CLEAR_WIDTH, HEADER_HEIGHT, &ui::BUTTON_COLOR);
                ffi::clutter_actor_add_child(panel_box, clear);
                ffi::clutter_actor_set_position(clear, width - CLEAR_WIDTH - SPACING, SPACING);
                let weak = Rc::downgrade(self);
                gobj(clear).connect_local("button-press-event", false, move |_| {
                    with(&weak, |n| n.clear_list());
                    Some(true.to_value())
                });
            }
            let mut y = HEADER_HEIGHT + 2.0 * SPACING;

            if entries.is_empty() {
                let none = ui::label(ui::BODY_FONT, "No notifications", &ui::HINT_COLOR);
                ffi::clutter_actor_add_child(panel_box, none);
                ffi::clutter_actor_set_position(none, PADDING, y);
                y += ffi::clutter_actor_get_height(none) + SPACING;
            }
            for (i, n) in entries.iter().enumerate().skip(offset) {
                let card = card(n, width - 2.0 * SPACING);
                ffi::clutter_actor_add_child(panel_box, card);
                let height = ffi::clutter_actor_get_height(card);
                // Unless it is the last one, keep room for the line below.
                let below = if i + 1 < entries.len() { MORE_HEIGHT + SPACING } else { 0.0 };
                // Always show one, even on a tiny monitor.
                if i > offset && y + height + SPACING + below > max_height {
                    ffi::clutter_actor_destroy(card);
                    at_end = false;
                    break;
                }
                ffi::clutter_actor_set_position(card, SPACING, y);
                // Buttons are wired once the card has a parent.
                self.wire_card(card, n, width - 2.0 * SPACING, scale);
                y += height + SPACING;
                shown += 1;
            }
            if !at_end {
                y = self.add_more_line(panel_box, entries.len() - offset - shown, y, width) + SPACING;
            }

            ffi::clutter_actor_set_position(self.view.overlay, geo.x as f32, geo.y as f32);
            ffi::clutter_actor_set_size(self.view.overlay, geo.width as f32, geo.height as f32);
            ffi::clutter_actor_set_size(panel_box, width, y);
            // Centered under the bell, as far as the monitor allows.
            let centered = self.view.bell_x.get() + BELL_WIDTH / 2.0 - width / 2.0;
            self.place_popup(geo, centered.min(geo.width as f32 - width - RIGHT_MARGIN).max(RIGHT_MARGIN));
        }
        let mut state = self.state.borrow_mut();
        state.list.at_end = at_end;
        state.list.shown = shown;
    }

    pub(super) fn build_calendar(self: &Rc<Self>) {
        let Some(geo) = ui::primary_monitor_geometry(self.display) else { return };
        let width = calendar::width();
        let panel_box = self.view.panel_box;
        unsafe { ffi::clutter_actor_destroy_all_children(panel_box) };
        let height = self.add_calendar(panel_box);
        unsafe { ffi::clutter_actor_set_size(panel_box, width, height) };
        // Centered like the clock.
        self.place_popup(geo, ((geo.width as f32 - width) / 2.0).max(0.0));
    }

    /// Cover the monitor with the overlay, and put the box `x` from its left.
    fn place_popup(&self, geo: ffi::MtkRectangle, x: f32) {
        unsafe {
            ffi::clutter_actor_set_position(self.view.overlay, geo.x as f32, geo.y as f32);
            ffi::clutter_actor_set_size(self.view.overlay, geo.width as f32, geo.height as f32);
            ffi::clutter_actor_set_position(self.view.panel_box, x.floor(), self.panel_height + SPACING);
        }
    }

    /// "▾ 5 more" at `y`; a click goes on to them. Returns its bottom.
    unsafe fn add_more_line(self: &Rc<Self>, parent: *mut ClutterActor, count: usize, y: f32, width: f32) -> f32 {
        unsafe {
            let line = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(line, 1);
            ffi::clutter_actor_add_child(parent, line);
            ffi::clutter_actor_set_position(line, SPACING, y);
            let line_width = width - 2.0 * SPACING;
            ffi::clutter_actor_set_size(line, line_width, MORE_HEIGHT);
            let text = ui::label(ui::BODY_FONT, &format!("▾ {count} more"), &ui::HINT_COLOR);
            ffi::clutter_actor_add_child(line, text);
            ffi::clutter_actor_set_position(
                text,
                ((line_width - ffi::clutter_actor_get_width(text)) / 2.0).floor(),
                ((MORE_HEIGHT - ffi::clutter_actor_get_height(text)) / 2.0).floor(),
            );
            crate::windows::hover_highlight(line, ui::TRANSPARENT, HOVER_COLOR);
            let weak = Rc::downgrade(self);
            gobj(line).connect_local("button-press-event", false, move |_| {
                with(&weak, |n| {
                    let page = n.state.borrow().list.shown.max(1);
                    n.scroll_list(page as i32);
                });
                Some(true.to_value())
            });
            y + MORE_HEIGHT
        }
    }

    /// Add the copy and remove buttons to a parented card.
    unsafe fn wire_card(self: &Rc<Self>, card: *mut ClutterActor, n: &Notification, width: f32, scale: f32) {
        unsafe {
            self.add_copy_button(card, n.copy_text(), width - 2.0 * (CLOSE_SIZE + SPACING), scale);
            let remove = ui::button("✕", CLOSE_SIZE, CLOSE_SIZE, &ui::BUTTON_COLOR);
            ffi::clutter_actor_add_child(card, remove);
            ffi::clutter_actor_set_position(remove, width - CLOSE_SIZE - SPACING, SPACING);
            let weak = Rc::downgrade(self);
            let id = n.id;
            gobj(remove).connect_local("button-press-event", false, move |_| {
                with(&weak, |s| s.forget(id));
                Some(true.to_value())
            });
        }
    }
}

/// One entry's text. Floating until parented.
unsafe fn card(n: &Notification, width: f32) -> *mut ClutterActor {
    unsafe {
        let card = ffi::clutter_actor_new();
        ffi::clutter_actor_set_background_color(card, &CARD_COLOR);
        let text_width = width - 2.0 * SPACING - 2.0 * (CLOSE_SIZE + SPACING);
        let heading = match (n.app_name.as_str(), n.received.as_str()) {
            ("", time) => time.to_owned(),
            (app, time) => format!("{app} · {time}"),
        };
        let lines = [
            (APP_FONT, heading.as_str(), &ui::HINT_COLOR, 1.0),
            (SUMMARY_FONT, n.summary.as_str(), &ui::TEXT_COLOR, 1.0),
            (ui::BODY_FONT, n.body.as_str(), &ui::TEXT_COLOR, MAX_BODY_LINES),
        ];
        let y = super::add_lines(card, &lines, SPACING, SPACING - 2.0, text_width);
        ffi::clutter_actor_set_size(card, width, y.max(CLOSE_SIZE + SPACING) + SPACING - 2.0);
        card
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: u32) -> Notification {
        Notification { id, ..Default::default() }
    }

    fn ids(h: &History) -> Vec<u32> {
        h.entries.iter().map(|n| n.id).collect()
    }

    #[test]
    fn newest_first_and_replaced_ids_move_up() {
        let mut h = History::default();
        for id in [1, 2, 3] {
            assert!(h.push(note(id)));
        }
        h.push(note(1));
        assert_eq!(ids(&h), [1, 3, 2]);
        assert!(h.contains(3));
        assert!(h.remove(3));
        assert!(!h.remove(3));
        assert_eq!(ids(&h), [1, 2]);
    }

    #[test]
    fn sorts_by_arrival_however_they_get_there() {
        let mut h = History::default();
        // The banner queue overflows: later ones can get here first.
        for arrival in [2, 3, 1, 4] {
            h.push(Notification { id: arrival as u32, arrival, ..Default::default() });
        }
        assert_eq!(ids(&h), [4, 3, 2, 1]);
        // An update arrives anew and moves to the top.
        h.push(Notification { id: 2, arrival: 5, ..Default::default() });
        assert_eq!(ids(&h), [2, 4, 3, 1]);
    }

    #[test]
    fn a_full_list_forgets_the_oldest_arrival() {
        let mut h = History::default();
        for arrival in (2..=MAX_HISTORY as u64 + 1).chain([1]) {
            h.push(Notification { id: arrival as u32, arrival, ..Default::default() });
        }
        assert_eq!(h.entries.len(), MAX_HISTORY);
        assert!(!h.contains(1));
        assert!(h.contains(2));
    }

    #[test]
    fn skips_transient_and_stays_bounded() {
        let mut h = History::default();
        assert!(!h.push(Notification { id: 1, transient: true, ..Default::default() }));
        assert!(ids(&h).is_empty());
        for id in 0..MAX_HISTORY as u32 + 5 {
            h.push(note(id));
        }
        assert_eq!(h.entries.len(), MAX_HISTORY);
        assert!(!h.contains(4));
        assert!(h.contains(5));
    }
}
