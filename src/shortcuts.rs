//! The apps pinned to the top bar: an icon each, right of the workspace dots.
//! Dragging an icon along the bar changes the order.

use crate::drag::{self, Press, Span};
use crate::ffi::{self, ClutterActor, MetaDisplay};
use crate::pins::{self, Pins, Place};
use crate::ui::{self, gobj};
use crate::windows::{self, MenuTarget};
use gio::prelude::*;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

const BUTTON_WIDTH: f32 = 32.0;
const ICON_SIZE: f32 = 20.0;
/// The line under the icon of a running app.
const MARK_WIDTH: f32 = 12.0;
const MARK_HEIGHT: f32 = 2.0;
/// Width kept free left of the panel's middle for the clock and its mark.
const RESERVED: f32 = 100.0;

/// How many shortcuts fit right of `left` in a panel `width` wide.
fn room(width: f32, left: f32) -> usize {
    ((width / 2.0 - RESERVED - left) / BUTTON_WIDTH).max(0.0) as usize
}

type MenuHandler = Rc<dyn Fn(MenuTarget, f32, f32)>;

#[derive(Default)]
struct State {
    /// What the buttons were built from: app ids and whether they run.
    drawn: Option<(Vec<(String, bool)>, usize)>,
    /// Where the buttons start in the panel.
    left: f32,
    panel_width: f32,
    menu: Option<MenuHandler>,
    press: Option<Press<String>>,
    /// Made anew with the buttons.
    marker: Option<*mut ClutterActor>,
}

pub struct TopShortcuts {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    container: *mut ClutterActor,
    pins: Rc<Pins>,
    height: f32,
    state: RefCell<State>,
}

impl TopShortcuts {
    pub fn new(
        display: *mut MetaDisplay,
        stage: *mut ClutterActor,
        panel: *mut ClutterActor,
        pins: Rc<Pins>,
        height: f32,
    ) -> Rc<Self> {
        let container = unsafe {
            let container = ffi::clutter_actor_new();
            ffi::clutter_actor_add_child(panel, container);
            container
        };
        Rc::new(Self { display, stage, container, pins, height, state: RefCell::default() })
    }

    /// `handler` runs on a right click on a shortcut.
    pub fn set_menu_handler(&self, handler: impl Fn(MenuTarget, f32, f32) + 'static) {
        self.state.borrow_mut().menu = Some(Rc::new(handler));
    }

    pub fn set_panel_width(&self, width: f32) {
        self.state.borrow_mut().panel_width = width;
    }

    /// Put the buttons at `left` in the panel.
    pub fn set_left(self: &Rc<Self>, left: f32) {
        self.state.borrow_mut().left = left;
        unsafe { ffi::clutter_actor_set_position(self.container, left, 0.0) };
        // How many fit may have changed.
        self.refresh();
    }

    /// Bring the buttons up to date. They are rebuilt only on change, so that
    /// a changing window title does not replace the one under the pointer.
    pub fn refresh(self: &Rc<Self>) {
        let apps = self.pins.apps(Place::TopBar);
        let running: Vec<String> = windows::list(self.display).iter().filter_map(pins::app_id_of).collect();
        let shown = {
            let state = self.state.borrow();
            room(state.panel_width, state.left).min(apps.len())
        };
        let key: Vec<(String, bool)> =
            apps.iter().map(|(id, _)| (id.clone(), running.contains(id))).collect();
        if self.state.borrow().drawn.as_ref() == Some(&(key.clone(), shown)) {
            return;
        }
        // A drag cannot go on: its button is about to go.
        {
            let mut state = self.state.borrow_mut();
            state.press = None;
            state.marker = None;
        }
        unsafe { ffi::clutter_actor_destroy_all_children(self.container) };
        let scale = ui::primary_monitor_scale(self.display);
        for (index, ((id, app), (_, runs))) in apps.iter().zip(&key).take(shown).enumerate() {
            let button = self.new_button(id, app, *runs, scale);
            unsafe { ffi::clutter_actor_set_position(button, index as f32 * BUTTON_WIDTH, 0.0) };
        }
        let width = shown as f32 * BUTTON_WIDTH;
        unsafe { ffi::clutter_actor_set_size(self.container, width, self.height) };
        self.state.borrow_mut().marker = Some(drag::new_marker(self.container));
        self.state.borrow_mut().drawn = Some((key, shown));
    }

    fn new_button(
        self: &Rc<Self>,
        id: &str,
        app: &gio_unix::DesktopAppInfo,
        running: bool,
        scale: f32,
    ) -> *mut ClutterActor {
        let button = unsafe {
            let button = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(button, BUTTON_WIDTH, self.height);
            ffi::clutter_actor_set_reactive(button, 1);
            ffi::clutter_actor_add_child(self.container, button);
            let icon = crate::icons::content(app.icon().as_ref(), ICON_SIZE, scale, self.stage);
            windows::add_icon(
                button,
                icon.as_ref(),
                ICON_SIZE,
                ((BUTTON_WIDTH - ICON_SIZE) / 2.0).floor(),
                ((self.height - ICON_SIZE) / 2.0).floor(),
            );
            if running {
                let mark = ffi::clutter_actor_new();
                ffi::clutter_actor_set_background_color(mark, &ui::TEXT_COLOR);
                ffi::clutter_actor_set_size(mark, MARK_WIDTH, MARK_HEIGHT);
                ffi::clutter_actor_set_position(
                    mark,
                    ((BUTTON_WIDTH - MARK_WIDTH) / 2.0).floor(),
                    self.height - MARK_HEIGHT,
                );
                ffi::clutter_actor_add_child(button, mark);
            }
            button
        };
        windows::hover_highlight(button, ui::TRANSPARENT, ui::PANEL_HOVER_COLOR);
        let object = unsafe { gobj(button) };
        let weak = Rc::downgrade(self);
        let own = id.to_owned();
        object.connect_local("button-press-event", false, move |args| {
            let event = ui::event_arg(args);
            match unsafe { ffi::clutter_event_get_button(event) } {
                1 => with(&weak, |s| s.state.borrow_mut().press = Press::new(own.clone(), event)),
                3 => with(&weak, |s| {
                    let handler = s.state.borrow().menu.clone();
                    if let (Some(handler), Some((x, _))) = (handler, ui::event_coords(event)) {
                        handler(MenuTarget::App(own.clone()), x, s.top() + s.height);
                    }
                }),
                _ => {}
            }
            Some(true.to_value())
        });
        let weak = Rc::downgrade(self);
        let own = id.to_owned();
        object.connect_local("motion-event", false, move |args| {
            with(&weak, |s| s.motion(&own, ui::event_arg(args)));
            Some(false.to_value())
        });
        let weak = Rc::downgrade(self);
        let own = id.to_owned();
        object.connect_local("button-release-event", false, move |args| {
            let event = ui::event_arg(args);
            if unsafe { ffi::clutter_event_get_button(event) } == 1 {
                with(&weak, |s| s.release(&own, ui::event_coords(event)));
            }
            Some(true.to_value())
        });
        button
    }

    /// The panel's top on the stage.
    fn top(&self) -> f32 {
        ui::primary_monitor_geometry(self.display).map_or(0.0, |g| g.y as f32)
    }

    /// Where the buttons start on the stage.
    fn stage_left(&self) -> f32 {
        let monitor = ui::primary_monitor_geometry(self.display).map_or(0.0, |g| g.x as f32);
        monitor + self.state.borrow().left
    }

    /// The pinned apps shown, with their places among the buttons.
    fn spans(&self) -> Vec<Span> {
        let state = self.state.borrow();
        let Some((apps, shown)) = &state.drawn else { return Vec::new() };
        apps.iter()
            .take(*shown)
            .enumerate()
            .map(|(i, (id, _))| {
                let start = i as f32 * BUTTON_WIDTH;
                Span { id: id.clone(), start, end: start + BUTTON_WIDTH }
            })
            .collect()
    }

    fn motion(&self, id: &str, event: *const ffi::ClutterEvent) {
        let Some((x, y)) = ui::event_coords(event) else { return };
        let (dragging, marker) = {
            let mut state = self.state.borrow_mut();
            let marker = state.marker;
            let dragging = match state.press.as_mut() {
                Some(press) if press.key == id && drag::button_held(event) => press.moved_to((x, y)),
                _ => false,
            };
            (dragging, marker)
        };
        let Some(marker) = marker.filter(|_| dragging) else { return };
        let target = drag::drop_at(&self.spans(), x - self.stage_left())
            .filter(|_| drag::near_bar(y, self.top(), self.height));
        match target {
            Some((_, at)) => drag::place_marker(marker, at, 4.0, self.height - 8.0),
            None => unsafe { ffi::clutter_actor_hide(marker) },
        }
    }

    /// End a press: drop a dragged icon, or else click it if the pointer is
    /// still on it.
    fn release(&self, id: &str, at: Option<(f32, f32)>) {
        let (press, marker) = {
            let mut state = self.state.borrow_mut();
            (state.press.take(), state.marker)
        };
        if let Some(marker) = marker {
            unsafe { ffi::clutter_actor_hide(marker) };
        }
        let (Some(press), Some((x, y))) = (press, at) else { return };
        if press.key != id {
            return;
        }
        let x = x - self.stage_left();
        if press.dragging {
            if drag::near_bar(y, self.top(), self.height)
                && let Some((before, _)) = drag::drop_at(&self.spans(), x)
            {
                self.pins.reorder(Place::TopBar, id, before.as_deref());
            }
            return;
        }
        let on_it = self.spans().iter().any(|s| s.id == id && x >= s.start && x < s.end)
            && y >= self.top()
            && y <= self.top() + self.height;
        if on_it {
            self.click(id, press.new_window);
        }
    }

    /// Start the app, or go to the window of it used last, or minimize that
    /// window if it has the focus already. With `new_window`, always open
    /// another window.
    fn click(&self, id: &str, new_window: bool) {
        if crate::lockscreen::is_locked() {
            return;
        }
        if new_window {
            if let Some(app) = pins::app(id) {
                pins::launch(self.display, &app, true);
            }
            return;
        }
        let own: Vec<glib::Object> = windows::recent(self.display)
            .into_iter()
            .filter(|w| pins::app_id_of(w).as_deref() == Some(id))
            .collect();
        let focused = windows::focused(self.display);
        match own.iter().find(|w| Some(windows::id(w)) == focused) {
            Some(window) => unsafe { ffi::meta_window_minimize(windows::ptr(window)) },
            None => match own.first() {
                Some(window) => windows::activate(self.display, window),
                None => {
                    if let Some(app) = pins::app(id) {
                        pins::launch(self.display, &app, false);
                    }
                }
            },
        }
    }

    pub fn destroy(&self) {
        unsafe { ffi::clutter_actor_destroy(self.container) };
    }
}

fn with(weak: &Weak<TopShortcuts>, f: impl FnOnce(&Rc<TopShortcuts>)) {
    if let Some(shortcuts) = weak.upgrade() {
        f(&shortcuts);
    }
}

#[cfg(test)]
mod tests {
    use super::room;

    #[test]
    fn shortcuts_stay_clear_of_the_clock() {
        assert_eq!(room(1280.0, 132.0), 12);
        assert!(room(1920.0, 132.0) > room(1280.0, 132.0));
        assert!(room(1280.0, 200.0) < room(1280.0, 132.0));
        assert_eq!(room(400.0, 132.0), 0);
    }
}
