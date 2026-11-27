//! The apps pinned to the top bar: an icon each, right of the panel buttons.

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
/// Width kept free left of the panel's middle for the workspace dots and the
/// clock.
const RESERVED: f32 = 250.0;

/// How many shortcuts fit right of `left` in a panel `width` wide.
fn room(width: f32, left: f32) -> usize {
    ((width / 2.0 - RESERVED - left) / BUTTON_WIDTH).max(0.0) as usize
}

type MenuHandler = Rc<dyn Fn(MenuTarget, f32, f32)>;

#[derive(Default)]
struct State {
    /// What the buttons were built from: app ids and whether they run.
    drawn: Option<(Vec<(String, bool)>, usize)>,
    width: f32,
    panel_width: f32,
    menu: Option<MenuHandler>,
}

pub struct TopShortcuts {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    container: *mut ClutterActor,
    pins: Rc<Pins>,
    left: f32,
    height: f32,
    state: RefCell<State>,
}

impl TopShortcuts {
    pub fn new(
        display: *mut MetaDisplay,
        stage: *mut ClutterActor,
        panel: *mut ClutterActor,
        pins: Rc<Pins>,
        left: f32,
        height: f32,
    ) -> Rc<Self> {
        let container = unsafe {
            let container = ffi::clutter_actor_new();
            ffi::clutter_actor_set_position(container, left, 0.0);
            ffi::clutter_actor_add_child(panel, container);
            container
        };
        Rc::new(Self { display, stage, container, pins, left, height, state: RefCell::default() })
    }

    /// `handler` runs on a right click on a shortcut.
    pub fn set_menu_handler(&self, handler: impl Fn(MenuTarget, f32, f32) + 'static) {
        self.state.borrow_mut().menu = Some(Rc::new(handler));
    }

    pub fn set_panel_width(&self, width: f32) {
        self.state.borrow_mut().panel_width = width;
    }

    /// How much of the panel the shortcuts take.
    pub fn width(&self) -> f32 {
        self.state.borrow().width
    }

    /// Bring the buttons up to date. They are rebuilt only on change, so that
    /// a changing window title does not replace the one under the pointer.
    pub fn refresh(self: &Rc<Self>) {
        let apps = self.pins.apps(Place::TopBar);
        let running: Vec<String> = windows::list(self.display).iter().filter_map(pins::app_id_of).collect();
        let shown = room(self.state.borrow().panel_width, self.left).min(apps.len());
        let key: Vec<(String, bool)> =
            apps.iter().map(|(id, _)| (id.clone(), running.contains(id))).collect();
        if self.state.borrow().drawn.as_ref() == Some(&(key.clone(), shown)) {
            return;
        }
        unsafe { ffi::clutter_actor_destroy_all_children(self.container) };
        let scale = ui::primary_monitor_scale(self.display);
        for (index, ((id, app), (_, runs))) in apps.iter().zip(&key).take(shown).enumerate() {
            let button = self.new_button(id, app, *runs, scale);
            unsafe { ffi::clutter_actor_set_position(button, index as f32 * BUTTON_WIDTH, 0.0) };
        }
        let width = shown as f32 * BUTTON_WIDTH;
        unsafe { ffi::clutter_actor_set_size(self.container, width, self.height) };
        let mut state = self.state.borrow_mut();
        state.drawn = Some((key, shown));
        state.width = width;
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
        let weak = Rc::downgrade(self);
        let id = id.to_owned();
        unsafe { gobj(button) }.connect_local("button-press-event", false, move |args| {
            let event = ui::event_arg(args);
            match unsafe { ffi::clutter_event_get_button(event) } {
                1 => with(&weak, |s| s.click(&id)),
                3 => with(&weak, |s| {
                    let handler = s.state.borrow().menu.clone();
                    if let (Some(handler), Some((x, _))) = (handler, ui::event_coords(event)) {
                        let top = ui::primary_monitor_geometry(s.display).map_or(0, |g| g.y);
                        handler(MenuTarget::App(id.clone()), x, top as f32 + s.height);
                    }
                }),
                _ => {}
            }
            Some(true.to_value())
        });
        button
    }

    /// Start the app, or go to the window of it used last, or minimize that
    /// window if it has the focus already.
    fn click(&self, id: &str) {
        if crate::lockscreen::is_locked() {
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
        assert_eq!(room(1280.0, 72.0), 9);
        assert!(room(1920.0, 72.0) > room(1280.0, 72.0));
        assert_eq!(room(400.0, 72.0), 0);
    }
}
