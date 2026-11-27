//! The volume menu under the volume icon: mute, a slider and the audio
//! outputs, the same for the microphone if there is one, and a link to Sound
//! Settings.

use super::outputs::{Direction, Output};
use super::{ICON_SIZE, Kind, StatusIcons, VOLUME_STEP, with};
use crate::ffi::{self, ClutterActor};
use crate::icons;
use crate::ui::{self, gobj};
use glib::prelude::*;
use std::rc::Rc;

const WIDTH: f32 = 360.0;
const PADDING: f32 = 10.0;
const MUTE_SIZE: f32 = 32.0;
const PERCENT_WIDTH: f32 = 48.0;
const SLIDER_HEIGHT: f32 = 32.0;
const TRACK_HEIGHT: f32 = 6.0;
const KNOB_SIZE: f32 = 14.0;
const SLIDER_X: f32 = PADDING + MUTE_SIZE + 12.0;
const SLIDER_WIDTH: f32 = WIDTH - SLIDER_X - PERCENT_WIDTH - PADDING;
const TRACK_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0x50, 0x50, 0x5a, 0xff);

/// The menu's actors. The inner ones exist only while it is open.
pub struct Menu {
    /// Full-monitor transparent actor that holds the grab.
    overlay: *mut ClutterActor,
    panel_box: *mut ClutterActor,
}

#[derive(Default)]
pub struct MenuState {
    grab: Option<*mut ffi::ClutterGrab>,
    pub open: bool,
    sliders: Vec<Slider>,
    /// The devices shown, to rebuild only when they change.
    shown: Shown,
    /// The slider being dragged.
    dragging: Option<Direction>,
}

/// What the menu lists.
#[derive(Default, PartialEq)]
struct Shown {
    outputs: Vec<Output>,
    /// `None` without a microphone, which leaves the input part out.
    inputs: Option<Vec<Output>>,
}

/// A mute button, slider and percentage.
#[derive(Clone, Copy)]
struct Slider {
    direction: Direction,
    mute_icon: *mut ClutterActor,
    fill: *mut ClutterActor,
    knob: *mut ClutterActor,
    percent: *mut ClutterActor,
    /// Left edge of the slider in stage coordinates.
    left: f32,
}

fn microphone_icon(muted: bool, level: f64) -> &'static str {
    if muted || level <= 0.0 {
        "microphone-sensitivity-muted-symbolic"
    } else {
        "microphone-sensitivity-high-symbolic"
    }
}

impl Menu {
    pub fn new(stage: *mut ClutterActor) -> Self {
        unsafe {
            let overlay = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(overlay, 1);
            ffi::clutter_actor_hide(overlay);
            let panel_box = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(panel_box, &ui::BOX_COLOR);
            ffi::clutter_actor_set_reactive(panel_box, 1);
            ffi::clutter_actor_add_child(overlay, panel_box);
            ffi::clutter_actor_add_child(stage, overlay);
            Self { overlay, panel_box }
        }
    }

    pub fn destroy(&self) {
        unsafe { ffi::clutter_actor_destroy(self.overlay) };
    }
}

/// How full the slider is at stage position `x`, from 0 to 1.
fn slider_level(x: f32, left: f32, width: f32) -> f64 {
    f64::from(((x - left) / width).clamp(0.0, 1.0))
}

impl StatusIcons {
    pub(super) fn connect_menu(self: &Rc<Self>) {
        let overlay = unsafe { gobj(self.menu.overlay) };
        let weak = Rc::downgrade(self);
        overlay.connect_local("key-press-event", false, move |args| {
            let key = ui::key_symbol(ui::event_arg(args));
            with(&weak, |s| match key {
                ui::KEY_ESCAPE => s.close_menu(),
                ui::KEY_LEFT => {
                    s.adjust_quietly(|a| a.change_volume(Direction::Output, -VOLUME_STEP));
                }
                ui::KEY_RIGHT => {
                    s.adjust_quietly(|a| a.change_volume(Direction::Output, VOLUME_STEP));
                }
                _ => {}
            });
            Some(true.to_value())
        });
        // Click outside closes the menu.
        let weak = Rc::downgrade(self);
        overlay.connect_local("button-press-event", false, move |_| {
            with(&weak, |s| s.close_menu());
            Some(true.to_value())
        });
        // A drag that leaves the slider still moves it.
        let weak = Rc::downgrade(self);
        overlay.connect_local("motion-event", false, move |args| {
            with(&weak, |s| {
                let dragging = s.state.borrow().menu.dragging;
                if let Some(direction) = dragging {
                    s.slide_to(direction, ui::event_coords(ui::event_arg(args)));
                }
            });
            Some(false.to_value())
        });
        let weak = Rc::downgrade(self);
        overlay.connect_local("button-release-event", false, move |_| {
            with(&weak, |s| s.state.borrow_mut().menu.dragging = None);
            Some(true.to_value())
        });
        let panel_box = unsafe { gobj(self.menu.panel_box) };
        panel_box.connect_local("button-press-event", false, |_| Some(true.to_value()));
        let weak = Rc::downgrade(self);
        panel_box.connect_local("scroll-event", false, move |args| {
            with(&weak, |s| s.scroll_volume(ui::event_arg(args), false));
            Some(true.to_value())
        });
    }

    pub(super) fn open_menu(self: &Rc<Self>) {
        if self.state.borrow().menu.open || crate::lockscreen::is_locked() {
            return;
        }
        self.state.borrow_mut().menu.open = true;
        self.build_menu();
        unsafe {
            ffi::clutter_actor_show(self.menu.overlay);
            ffi::clutter_actor_set_child_above_sibling(self.stage, self.menu.overlay, std::ptr::null_mut());
            let grab = ffi::clutter_stage_grab(self.stage as *mut ffi::ClutterStage, self.menu.overlay);
            self.state.borrow_mut().menu.grab = Some(grab);
            ffi::clutter_actor_grab_key_focus(self.menu.overlay);
        }
    }

    pub fn close_menu(&self) {
        let grab = {
            let mut state = self.state.borrow_mut();
            if !state.menu.open {
                return;
            }
            let grab = state.menu.grab.take();
            state.menu = MenuState::default();
            grab
        };
        unsafe {
            if let Some(grab) = grab {
                ffi::clutter_grab_dismiss(grab);
                gobject_sys::g_object_unref(grab as *mut gobject_sys::GObject);
            }
            ffi::clutter_actor_hide(self.menu.overlay);
            ffi::clutter_actor_destroy_all_children(self.menu.panel_box);
        }
    }

    /// Bring an open menu up to date with the sound server.
    pub(super) fn refresh_menu(self: &Rc<Self>) {
        if !self.state.borrow().menu.open {
            return;
        }
        if self.shown() != self.state.borrow().menu.shown {
            self.build_menu();
        } else {
            self.update_sliders();
        }
    }

    fn shown(&self) -> Shown {
        let Some(audio) = self.audio.borrow().clone() else { return Shown::default() };
        Shown {
            outputs: audio.devices(Direction::Output),
            inputs: audio.volume(Direction::Input).map(|_| audio.devices(Direction::Input)),
        }
    }

    fn build_menu(self: &Rc<Self>) {
        let Some(geo) = ui::primary_monitor_geometry(self.display) else { return };
        let shown = self.shown();
        let panel_box = self.menu.panel_box;
        let box_x = (geo.width as f32 - WIDTH - 4.0).max(0.0);
        let inner = WIDTH - 2.0 * PADDING;
        let left = geo.x as f32 + box_x + SLIDER_X;
        let mut sliders = Vec::new();
        unsafe {
            ffi::clutter_actor_destroy_all_children(panel_box);

            sliders.push(self.add_slider(Direction::Output, PADDING, left));
            let mut y = PADDING + SLIDER_HEIGHT + PADDING;
            y = self.add_devices("Output", Direction::Output, &shown.outputs, y);

            if let Some(inputs) = &shown.inputs {
                y = self.add_line(y);
                sliders.push(self.add_slider(Direction::Input, y, left));
                y += SLIDER_HEIGHT + PADDING;
                y = self.add_devices("Input", Direction::Input, inputs, y);
            }

            y = self.add_line(y);
            self.add_menu_row("Sound Settings", None, inner, y, |s| {
                s.close_menu();
                s.open_settings(Kind::Volume);
            });
            y += ui::ROW_HEIGHT + PADDING;

            ffi::clutter_actor_set_position(self.menu.overlay, geo.x as f32, geo.y as f32);
            ffi::clutter_actor_set_size(self.menu.overlay, geo.width as f32, geo.height as f32);
            ffi::clutter_actor_set_size(panel_box, WIDTH, y);
            ffi::clutter_actor_set_position(panel_box, box_x, self.panel_height + 4.0);
        }
        {
            let mut state = self.state.borrow_mut();
            state.menu.sliders = sliders;
            state.menu.shown = shown;
        }
        self.update_sliders();
    }

    /// A separator at `y`; returns where to go on.
    unsafe fn add_line(&self, y: f32) -> f32 {
        unsafe {
            let line = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(line, WIDTH - 2.0 * PADDING, 1.0);
            ffi::clutter_actor_set_position(line, PADDING, y + 4.0);
            ffi::clutter_actor_set_background_color(line, &TRACK_COLOR);
            ffi::clutter_actor_add_child(self.menu.panel_box, line);
        }
        y + 9.0
    }

    /// The mute button, slider and percentage of one direction, at `top`.
    /// `left` is the slider's left edge on the stage.
    unsafe fn add_slider(self: &Rc<Self>, direction: Direction, top: f32, left: f32) -> Slider {
        let panel_box = self.menu.panel_box;
        unsafe {
            let mute = ui::button("", MUTE_SIZE, MUTE_SIZE, &ui::BUTTON_COLOR);
            ffi::clutter_actor_add_child(panel_box, mute);
            ffi::clutter_actor_set_position(mute, PADDING, top);
            let mute_icon = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(mute_icon, ICON_SIZE, ICON_SIZE);
            ffi::clutter_actor_set_position(mute_icon, (MUTE_SIZE - ICON_SIZE) / 2.0, (MUTE_SIZE - ICON_SIZE) / 2.0);
            ffi::clutter_actor_add_child(mute, mute_icon);
            let weak = Rc::downgrade(self);
            gobj(mute).connect_local("button-press-event", false, move |_| {
                with(&weak, |s| {
                    s.adjust_quietly(|a| a.toggle_mute(direction));
                });
                Some(true.to_value())
            });

            // A tall invisible hit area around the thin track.
            let slider = ffi::clutter_actor_new();
            ffi::clutter_actor_set_size(slider, SLIDER_WIDTH, SLIDER_HEIGHT);
            ffi::clutter_actor_set_position(slider, SLIDER_X, top);
            ffi::clutter_actor_set_reactive(slider, 1);
            ffi::clutter_actor_add_child(panel_box, slider);
            let part = |width: f32, height: f32, color: &ffi::CoglColor| {
                let actor = ffi::clutter_actor_new();
                ffi::clutter_actor_set_size(actor, width, height);
                ffi::clutter_actor_set_position(actor, 0.0, (SLIDER_HEIGHT - height) / 2.0);
                ffi::clutter_actor_set_background_color(actor, color);
                ffi::clutter_actor_add_child(slider, actor);
                actor
            };
            part(SLIDER_WIDTH, TRACK_HEIGHT, &TRACK_COLOR);
            let fill = part(0.0, TRACK_HEIGHT, &ui::SELECTED_COLOR);
            let knob = part(KNOB_SIZE, KNOB_SIZE, &ui::TEXT_COLOR);
            let weak = Rc::downgrade(self);
            gobj(slider).connect_local("button-press-event", false, move |args| {
                with(&weak, |s| {
                    s.state.borrow_mut().menu.dragging = Some(direction);
                    s.slide_to(direction, ui::event_coords(ui::event_arg(args)));
                });
                Some(true.to_value())
            });
            // Scrolling elsewhere in the menu changes the output.
            if direction == Direction::Input {
                let weak = Rc::downgrade(self);
                let acc = std::cell::Cell::new(0.0);
                gobj(slider).connect_local("scroll-event", false, move |args| {
                    let mut rest = acc.get();
                    let rows = ui::scroll_rows(ui::event_arg(args), &mut rest);
                    acc.set(rest);
                    if rows != 0 {
                        with(&weak, |s| {
                            s.adjust_quietly(|a| a.change_volume(direction, f64::from(-rows) * VOLUME_STEP));
                        });
                    }
                    Some(true.to_value())
                });
            }

            let percent = ui::label(ui::BODY_FONT, "100%", &ui::TEXT_COLOR);
            ffi::clutter_actor_add_child(panel_box, percent);
            let y = top + (SLIDER_HEIGHT - ffi::clutter_actor_get_height(percent)) / 2.0;
            ffi::clutter_actor_set_position(percent, SLIDER_X + SLIDER_WIDTH + 10.0, y.floor());
            Slider { direction, mute_icon, fill, knob, percent, left }
        }
    }

    /// A heading and a row per device, from `y`; returns where to go on.
    unsafe fn add_devices(
        self: &Rc<Self>,
        title: &str,
        direction: Direction,
        devices: &[Output],
        mut y: f32,
    ) -> f32 {
        let scale = ui::primary_monitor_scale(self.display);
        unsafe {
            let heading = ui::label(ui::BODY_FONT, title, &ui::HINT_COLOR);
            ffi::clutter_actor_add_child(self.menu.panel_box, heading);
            ffi::clutter_actor_set_position(heading, PADDING + 12.0, y);
            y += ffi::clutter_actor_get_height(heading) + 4.0;

            let check = icons::symbolic("object-select-symbolic", &ui::TEXT_COLOR, ui::ROW_ICON_SIZE, scale, self.stage);
            for device in devices {
                let icon = check.as_ref().filter(|_| device.active);
                let target = device.target.clone();
                self.add_menu_row(&device.label, icon, WIDTH - 2.0 * PADDING, y, move |s| {
                    if let Some(audio) = s.audio.borrow().as_ref() {
                        audio.select(direction, &target);
                    }
                });
                y += ui::ROW_HEIGHT;
            }
        }
        y
    }

    unsafe fn add_menu_row(
        self: &Rc<Self>,
        text: &str,
        icon: Option<&glib::Object>,
        width: f32,
        y: f32,
        chosen: impl Fn(&Rc<StatusIcons>) + 'static,
    ) {
        unsafe {
            let row = ui::list_row(text, icon, width, false);
            ffi::clutter_actor_set_position(row, PADDING, y);
            ffi::clutter_actor_set_reactive(row, 1);
            // Parent before wrapping: see `ui::gobj`.
            ffi::clutter_actor_add_child(self.menu.panel_box, row);
            let object = gobj(row);
            for (signal, color) in [("enter-event", super::HOVER_COLOR), ("leave-event", ui::TRANSPARENT)] {
                object.connect_local(signal, false, move |_| {
                    ffi::clutter_actor_set_background_color(row, &color);
                    Some(false.to_value())
                });
            }
            let weak = Rc::downgrade(self);
            object.connect_local("button-press-event", false, move |_| {
                with(&weak, |s| chosen(s));
                Some(true.to_value())
            });
        }
    }

    pub(super) fn update_sliders(&self) {
        let sliders = self.state.borrow().menu.sliders.clone();
        let Some(audio) = self.audio.borrow().clone() else { return };
        let scale = ui::primary_monitor_scale(self.display);
        for slider in sliders {
            let Some(volume) = audio.volume(slider.direction) else { continue };
            let shown = volume.level.clamp(0.0, 1.0) as f32;
            let name = match slider.direction {
                Direction::Output => super::volume_icon(volume),
                Direction::Input => microphone_icon(volume.muted, volume.level),
            };
            unsafe {
                let color = if volume.muted { &ui::HINT_COLOR } else { &ui::SELECTED_COLOR };
                ffi::clutter_actor_set_background_color(slider.fill, color);
                ffi::clutter_actor_set_width(slider.fill, (SLIDER_WIDTH * shown).round());
                ffi::clutter_actor_set_position(
                    slider.knob,
                    ((SLIDER_WIDTH - KNOB_SIZE) * shown).round(),
                    (SLIDER_HEIGHT - KNOB_SIZE) / 2.0,
                );
                ui::set_text(slider.percent, &format!("{}%", (volume.level * 100.0).round()));
                let content = icons::symbolic(name, &ui::TEXT_COLOR, ICON_SIZE, scale, self.stage);
                let content = content.as_ref().map_or(std::ptr::null_mut(), |c| c.as_ptr() as *mut ffi::ClutterContent);
                ffi::clutter_actor_set_content(slider.mute_icon, content);
            }
        }
    }

    fn slide_to(&self, direction: Direction, at: Option<(f32, f32)>) {
        let slider = self.state.borrow().menu.sliders.iter().find(|s| s.direction == direction).copied();
        let (Some(slider), Some((x, _))) = (slider, at) else { return };
        let level = slider_level(x, slider.left, SLIDER_WIDTH);
        self.adjust_quietly(|a| a.set_level(direction, level));
    }
}

#[cfg(test)]
mod tests {
    use super::slider_level;

    #[test]
    fn slider_positions() {
        assert_eq!(slider_level(150.0, 100.0, 200.0), 0.25);
        assert_eq!(slider_level(0.0, 100.0, 200.0), 0.0);
        assert_eq!(slider_level(900.0, 100.0, 200.0), 1.0);
    }
}
