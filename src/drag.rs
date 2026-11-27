//! Dragging pinned apps along the taskbar or the top bar to reorder them.
//!
//! A press on an entry becomes a drag once the pointer has moved a little,
//! and a click if it is released before. Clutter keeps sending the pointer's
//! events to the pressed actor until the release.

use crate::ffi::{self, ClutterActor};
use crate::ui;

/// How far the pointer moves before a press becomes a drag.
const THRESHOLD: f32 = 8.0;
/// A release further than this above or below the bar drops nothing.
const SLACK: f32 = 48.0;
const MARKER_WIDTH: f32 = 3.0;

/// A press of the primary button on an entry, not yet released.
pub struct Press<K> {
    pub key: K,
    at: (f32, f32),
    /// Whether Shift or Ctrl was held, to open a new window on a click.
    pub new_window: bool,
    pub dragging: bool,
}

impl<K> Press<K> {
    pub fn new(key: K, event: *const ffi::ClutterEvent) -> Option<Self> {
        let at = ui::event_coords(event)?;
        Some(Self { key, at, new_window: ui::wants_new_window(event), dragging: false })
    }

    /// Whether the press is a drag now that the pointer is at `now`.
    pub fn moved_to(&mut self, now: (f32, f32)) -> bool {
        self.dragging |= is_drag(self.at, now);
        self.dragging
    }
}

fn is_drag(from: (f32, f32), to: (f32, f32)) -> bool {
    (to.0 - from.0).abs().max((to.1 - from.1).abs()) >= THRESHOLD
}

/// Whether the primary button is down during `event`. A press can be left over
/// if its release went elsewhere.
pub fn button_held(event: *const ffi::ClutterEvent) -> bool {
    !event.is_null() && unsafe { ffi::clutter_event_get_state(event) } & ffi::CLUTTER_BUTTON1_MASK != 0
}

/// Whether `y` is near enough to a bar from `top` to `top + height`.
pub fn near_bar(y: f32, top: f32, height: f32) -> bool {
    y >= top - SLACK && y <= top + height + SLACK
}

/// The room a pinned app takes in a bar.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub id: String,
    pub start: f32,
    pub end: f32,
}

/// Where a drop at `x` lands among `spans`, which go from left to right: the
/// app to put it before (`None` for after the last one), and where to show
/// that.
pub fn drop_at(spans: &[Span], x: f32) -> Option<(Option<String>, f32)> {
    let index = spans.iter().filter(|s| (s.start + s.end) / 2.0 < x).count();
    match spans.get(index) {
        Some(span) => Some((Some(span.id.clone()), span.start)),
        None => spans.last().map(|last| (None, last.end)),
    }
}

/// The line that shows where the drop will land, hidden until placed.
pub fn new_marker(parent: *mut ClutterActor) -> *mut ClutterActor {
    unsafe {
        let marker = ffi::clutter_actor_new();
        ffi::clutter_actor_set_background_color(marker, &ui::SELECTED_COLOR);
        ffi::clutter_actor_hide(marker);
        ffi::clutter_actor_add_child(parent, marker);
        marker
    }
}

/// Show `marker` centered on `x` of its parent.
pub fn place_marker(marker: *mut ClutterActor, x: f32, y: f32, height: f32) {
    unsafe {
        ffi::clutter_actor_set_size(marker, MARKER_WIDTH, height);
        ffi::clutter_actor_set_position(marker, (x - MARKER_WIDTH / 2.0).max(0.0).floor(), y);
        // Above the entries, which may be newer than the marker.
        let parent = ffi::clutter_actor_get_parent(marker);
        ffi::clutter_actor_set_child_above_sibling(parent, marker, std::ptr::null_mut());
        ffi::clutter_actor_show(marker);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(id: &str, start: f32) -> Span {
        Span { id: id.to_owned(), start, end: start + 40.0 }
    }

    #[test]
    fn small_movements_stay_clicks() {
        assert!(!is_drag((10.0, 10.0), (17.0, 3.0)));
        assert!(is_drag((10.0, 10.0), (18.0, 10.0)));
        assert!(is_drag((10.0, 10.0), (10.0, 1.0)));
    }

    #[test]
    fn drops_between_the_nearest_apps() {
        let spans = [span("a", 0.0), span("b", 44.0), span("c", 88.0)];
        let before = |x| drop_at(&spans, x).unwrap().0;
        assert_eq!(before(-5.0).as_deref(), Some("a"));
        assert_eq!(before(30.0).as_deref(), Some("b"));
        assert_eq!(before(70.0).as_deref(), Some("c"));
        assert_eq!(drop_at(&spans, 500.0), Some((None, 128.0)));
        assert_eq!(drop_at(&[], 5.0), None);
    }

    #[test]
    fn a_drop_far_from_the_bar_does_not_count() {
        assert!(near_bar(700.0, 680.0, 40.0));
        assert!(near_bar(640.0, 680.0, 40.0));
        assert!(!near_bar(400.0, 680.0, 40.0));
    }
}
