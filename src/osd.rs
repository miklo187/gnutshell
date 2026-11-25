//! On-screen display: the short-lived popup for volume, brightness and
//! similar changes, requested over D-Bus (`org.gnome.Shell.ShowOSD`) by
//! gnome-settings-daemon.

use crate::ffi::{self, ClutterActor, MetaDisplay};
use crate::icons;
use crate::ui;
use gio::prelude::*;
use std::cell::RefCell;
use std::ffi::CStr;
use std::rc::Rc;
use std::time::Duration;

const HIDE_AFTER: Duration = Duration::from_millis(1500);
const FADE_MS: u32 = 200;
const WIDTH: f32 = 320.0;
const HEIGHT: f32 = 52.0;
const ICON_SIZE: f32 = 24.0;
const PADDING: f32 = 16.0;
const BAR_HEIGHT: f32 = 6.0;
const LABEL_FONT: &CStr = c"Sans Bold 11";
const BAR_BG: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x30);
const OVERAMP_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0x7b, 0x63, 0xff);

/// What to show. `level` is in `0..=max_level`; `max_level` above 1 means
/// over-amplification (e.g. volume above 100 %).
#[derive(Clone, Debug, Default)]
pub struct OsdRequest {
    pub icon: Option<gio::Icon>,
    pub label: Option<String>,
    pub level: Option<f64>,
    pub max_level: f64,
}

/// Fill fraction of the bar, and where 100 % sits when over-amplified.
pub fn bar_fractions(level: f64, max_level: f64) -> (f64, Option<f64>) {
    let max = if max_level > 0.0 { max_level } else { 1.0 };
    let fill = (level / max).clamp(0.0, 1.0);
    let marker = (max > 1.0).then(|| 1.0 / max);
    (fill, marker)
}

pub struct Osd {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    root: *mut ClutterActor,
    hide_timeout: RefCell<Option<glib::SourceId>>,
}

impl Osd {
    pub fn new(display: *mut MetaDisplay, stage: *mut ClutterActor) -> Rc<Self> {
        unsafe {
            let root = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(root, &ui::BOX_COLOR);
            ffi::clutter_actor_hide(root);
            ffi::clutter_actor_add_child(stage, root);
            Rc::new(Self { display, stage, root, hide_timeout: RefCell::default() })
        }
    }

    pub fn show(self: &Rc<Self>, request: &OsdRequest) {
        let Some(geo) = ui::primary_monitor_geometry(self.display) else { return };
        let scale = ui::primary_monitor_scale(self.display);
        unsafe {
            ffi::clutter_actor_destroy_all_children(self.root);
            let width = WIDTH.min(geo.width as f32 - 32.0);
            let mut x = PADDING;

            if let Some(content) = request.icon.as_ref().and_then(|i| icon_content(i, scale, self.stage)) {
                let icon = ui::icon_actor(&content, ICON_SIZE);
                ffi::clutter_actor_add_child(self.root, icon);
                ffi::clutter_actor_set_position(icon, x, ((HEIGHT - ICON_SIZE) / 2.0).floor());
                x += ICON_SIZE + 14.0;
            }

            let right = width - PADDING;
            if let Some(text) = request.label.as_deref().filter(|t| !t.is_empty()) {
                let label = ui::label(LABEL_FONT, text, &ui::TEXT_COLOR);
                ffi::clutter_text_set_ellipsize(label as *mut ffi::ClutterText, ffi::PANGO_ELLIPSIZE_END);
                ffi::clutter_actor_add_child(self.root, label);
                // With a level bar the label sits above it; alone it's centered.
                let y = if request.level.is_some() { 8.0 } else {
                    ((HEIGHT - ffi::clutter_actor_get_height(label)) / 2.0).floor()
                };
                ffi::clutter_actor_set_width(label, right - x);
                ffi::clutter_actor_set_position(label, x, y);
            }

            if let Some(level) = request.level {
                let (fill, marker) = bar_fractions(level, request.max_level);
                let bar_w = right - x;
                let y = if request.label.as_deref().is_some_and(|t| !t.is_empty()) {
                    HEIGHT - 8.0 - BAR_HEIGHT - 4.0
                } else {
                    ((HEIGHT - BAR_HEIGHT) / 2.0).floor()
                };
                let bg = ffi::clutter_actor_new();
                ffi::clutter_actor_set_background_color(bg, &BAR_BG);
                ffi::clutter_actor_set_size(bg, bar_w, BAR_HEIGHT);
                ffi::clutter_actor_set_position(bg, x, y);
                ffi::clutter_actor_add_child(self.root, bg);

                let over = marker.is_some_and(|m| fill > m);
                let bar = ffi::clutter_actor_new();
                let color = if over { &OVERAMP_COLOR } else { &ui::TEXT_COLOR };
                ffi::clutter_actor_set_background_color(bar, color);
                ffi::clutter_actor_set_size(bar, (bar_w as f64 * fill).round() as f32, BAR_HEIGHT);
                ffi::clutter_actor_set_position(bar, x, y);
                ffi::clutter_actor_add_child(self.root, bar);

                if let Some(m) = marker {
                    let tick = ffi::clutter_actor_new();
                    ffi::clutter_actor_set_background_color(tick, &ui::BOX_COLOR);
                    ffi::clutter_actor_set_size(tick, 2.0, BAR_HEIGHT);
                    ffi::clutter_actor_set_position(tick, x + (bar_w as f64 * m) as f32 - 1.0, y);
                    ffi::clutter_actor_add_child(self.root, tick);
                }
            }

            ffi::clutter_actor_set_size(self.root, width, HEIGHT);
            ffi::clutter_actor_set_position(
                self.root,
                (geo.x as f32 + (geo.width as f32 - width) / 2.0).floor(),
                (geo.y as f32 + geo.height as f32 * 0.85 - HEIGHT).floor(),
            );
            // Topmost: above the panel, launcher and dialogs.
            ffi::clutter_actor_set_child_above_sibling(self.stage, self.root, std::ptr::null_mut());
            ffi::clutter_actor_remove_all_transitions(self.root);
            ffi::clutter_actor_set_opacity(self.root, 255);
            ffi::clutter_actor_show(self.root);
        }

        if let Some(id) = self.hide_timeout.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(HIDE_AFTER, move || {
            if let Some(osd) = weak.upgrade() {
                osd.hide_timeout.borrow_mut().take();
                osd.fade_out();
            }
        });
        *self.hide_timeout.borrow_mut() = Some(id);
    }

    fn fade_out(&self) {
        unsafe {
            ffi::clutter_actor_save_easing_state(self.root);
            ffi::clutter_actor_set_easing_mode(self.root, ffi::CLUTTER_EASE_OUT_QUAD);
            ffi::clutter_actor_set_easing_duration(self.root, FADE_MS);
            ffi::clutter_actor_set_opacity(self.root, 0);
            ffi::clutter_actor_restore_easing_state(self.root);
        }
    }

    pub fn destroy(&self) {
        if let Some(id) = self.hide_timeout.borrow_mut().take() {
            id.remove();
        }
        unsafe { ffi::clutter_actor_destroy(self.root) };
    }
}

/// Themed icons render symbolic (recolored); file icons as they are.
fn icon_content(icon: &gio::Icon, scale: f32, stage: *mut ClutterActor) -> Option<glib::Object> {
    if let Some(themed) = icon.downcast_ref::<gio::ThemedIcon>() {
        return themed
            .names()
            .iter()
            .find_map(|name| icons::symbolic(name, &ui::TEXT_COLOR, ICON_SIZE, scale, stage));
    }
    icons::content(Some(icon), ICON_SIZE, scale, stage)
}

#[cfg(test)]
mod tests {
    use super::bar_fractions;

    #[test]
    fn plain_level() {
        assert_eq!(bar_fractions(0.5, 1.0), (0.5, None));
        assert_eq!(bar_fractions(2.0, 1.0), (1.0, None));
        assert_eq!(bar_fractions(-1.0, 1.0), (0.0, None));
    }

    #[test]
    fn zero_max_means_one() {
        assert_eq!(bar_fractions(0.25, 0.0), (0.25, None));
    }

    #[test]
    fn overamplified_has_marker() {
        assert_eq!(bar_fractions(1.5, 1.5), (1.0, Some(1.0 / 1.5)));
    }
}
