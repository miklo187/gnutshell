//! Shared look and small Clutter helpers for shell UI.

use crate::ffi::{self, ClutterActor, CoglColor, MetaDisplay, MtkRectangle};
use gio::prelude::AppLaunchContextExt;
use glib::prelude::*;
use glib::translate::*;
use std::ffi::{CStr, CString};

pub const TEXT_COLOR: CoglColor = CoglColor::rgba(0xf0, 0xf0, 0xf0, 0xff);
pub const HINT_COLOR: CoglColor = CoglColor::rgba(0x90, 0x90, 0x98, 0xff);
pub const BOX_COLOR: CoglColor = CoglColor::rgba(0x1e, 0x1e, 0x24, 0xf8);
pub const SELECTED_COLOR: CoglColor = CoglColor::rgba(0x35, 0x84, 0xe4, 0xff);
pub const DIM_COLOR: CoglColor = CoglColor::rgba(0, 0, 0, 0xa0);
pub const BUTTON_COLOR: CoglColor = CoglColor::rgba(0x3a, 0x3a, 0x44, 0xff);
pub const DESTRUCTIVE_COLOR: CoglColor = CoglColor::rgba(0xc0, 0x1c, 0x28, 0xff);
pub const TRANSPARENT: CoglColor = CoglColor::rgba(0, 0, 0, 0);
pub const PANEL_HOVER_COLOR: CoglColor = CoglColor::rgba(0xff, 0xff, 0xff, 0x20);

pub const ROW_FONT: &CStr = c"Sans 12";
pub const BODY_FONT: &CStr = c"Sans 11";
pub const ROW_HEIGHT: f32 = 36.0;
pub const BOX_PADDING: f32 = 16.0;
pub const ROW_ICON_SIZE: f32 = 24.0;

// X keysyms
pub const KEY_ESCAPE: u32 = 0xff1b;
pub const KEY_RETURN: u32 = 0xff0d;
pub const KEY_KP_ENTER: u32 = 0xff8d;
pub const KEY_TAB: u32 = 0xff09;
pub const KEY_ISO_LEFT_TAB: u32 = 0xfe20;
pub const KEY_LEFT: u32 = 0xff51;
pub const KEY_UP: u32 = 0xff52;
pub const KEY_RIGHT: u32 = 0xff53;
pub const KEY_DOWN: u32 = 0xff54;
pub const KEY_MODIFIERS: [u32; 6] = [
    0xffe7, 0xffe8, // Meta_L, Meta_R
    0xffe9, 0xffea, // Alt_L, Alt_R
    0xffeb, 0xffec, // Super_L, Super_R
];

/// Borrow a GObject pointer as a `glib::Object` (adds a ref).
///
/// Never call on a floating (unparented) ClutterActor: `from_glib_none` sinks
/// the floating ref, and dropping the wrapper then frees the actor.
pub unsafe fn gobj<T>(ptr: *mut T) -> glib::Object {
    unsafe { from_glib_none(ptr as *mut gobject_sys::GObject) }
}

/// Geometry of the primary monitor, or `None` while there is no valid one
/// (e.g. at startup before the devkit / virtual monitor appears).
pub fn primary_monitor_geometry(display: *mut MetaDisplay) -> Option<MtkRectangle> {
    unsafe {
        let primary = ffi::meta_display_get_primary_monitor(display);
        if primary < 0 || primary >= ffi::meta_display_get_n_monitors(display) {
            return None;
        }
        let mut geo = MtkRectangle::default();
        ffi::meta_display_get_monitor_geometry(display, primary, &mut geo);
        Some(geo)
    }
}

/// Scale factor of the primary monitor (1.0 if there is none yet).
pub fn primary_monitor_scale(display: *mut MetaDisplay) -> f32 {
    unsafe {
        let primary = ffi::meta_display_get_primary_monitor(display);
        if primary < 0 || primary >= ffi::meta_display_get_n_monitors(display) {
            return 1.0;
        }
        ffi::meta_display_get_monitor_scale(display, primary)
    }
}

/// A launch context that gives the started app the right display variables
/// and an activation token for the event being handled.
pub fn launch_context(display: *mut MetaDisplay) -> gio::AppLaunchContext {
    unsafe {
        let sn = ffi::meta_display_get_startup_notification(display);
        let ctx = ffi::meta_startup_notification_create_launcher(sn);
        if ctx.is_null() {
            return gio::AppLaunchContext::new();
        }
        ffi::meta_launch_context_set_timestamp(ctx, ffi::clutter_get_current_event_time());
        from_glib_full(ctx as *mut gio::ffi::GAppLaunchContext)
    }
}

/// An XDG activation token, so an app launched or signalled by the shell may
/// take focus.
pub fn activation_token(display: *mut MetaDisplay, timestamp: u32) -> Option<String> {
    unsafe {
        let sn = ffi::meta_display_get_startup_notification(display);
        let ctx = ffi::meta_startup_notification_create_launcher(sn);
        if ctx.is_null() {
            return None;
        }
        ffi::meta_launch_context_set_timestamp(ctx, timestamp);
        let ctx: gio::AppLaunchContext = from_glib_full(ctx as *mut gio::ffi::GAppLaunchContext);
        ctx.startup_notify_id(None::<&gio::AppInfo>, &[]).map(|s| s.to_string())
    }
}

/// Put `text` on the clipboard. The shell keeps it until something else
/// takes the clipboard. `false` if mutter could not create the source.
pub fn set_clipboard(display: *mut MetaDisplay, text: &str) -> bool {
    set_clipboard_data(display, c"text/plain;charset=utf-8", text.as_bytes())
}

/// `set_clipboard` for any one MIME type.
pub fn set_clipboard_data(display: *mut MetaDisplay, mimetype: &CStr, data: &[u8]) -> bool {
    let bytes = glib::Bytes::from(data);
    unsafe {
        let mut err = std::ptr::null_mut();
        let source = ffi::meta_selection_source_memory_new(
            mimetype.as_ptr(),
            bytes.to_glib_none().0,
            &mut err,
        );
        if source.is_null() {
            let err: glib::Error = from_glib_full(err);
            glib::g_warning!("gnutshell", "could not copy to the clipboard: {err}");
            return false;
        }
        let selection = ffi::meta_display_get_selection(display);
        ffi::meta_selection_set_owner(selection, ffi::META_SELECTION_CLIPBOARD, source);
        // The selection holds its own reference.
        gobject_sys::g_object_unref(source as *mut gobject_sys::GObject);
        true
    }
}

/// An actor showing `content` (a `ClutterContent`) at `size` x `size`,
/// aspect preserved. Floating until parented.
pub unsafe fn icon_actor(content: &glib::Object, size: f32) -> *mut ClutterActor {
    unsafe {
        let actor = ffi::clutter_actor_new();
        ffi::clutter_actor_set_size(actor, size, size);
        ffi::clutter_actor_set_content_gravity(actor, ffi::CLUTTER_CONTENT_GRAVITY_RESIZE_ASPECT);
        ffi::clutter_actor_set_content(actor, content.as_ptr() as *mut ffi::ClutterContent);
        actor
    }
}

/// The `ClutterEvent*` argument of an actor event signal (`key-press-event` etc).
pub fn event_arg(args: &[glib::Value]) -> *const ffi::ClutterEvent {
    args.get(1).map_or(std::ptr::null(), |v| unsafe {
        gobject_sys::g_value_peek_pointer(v.to_glib_none().0) as *const ffi::ClutterEvent
    })
}

pub fn key_symbol(event: *const ffi::ClutterEvent) -> u32 {
    if event.is_null() { 0 } else { unsafe { ffi::clutter_event_get_key_symbol(event) } }
}

/// An event's position in stage coordinates.
pub fn event_coords(event: *const ffi::ClutterEvent) -> Option<(f32, f32)> {
    if event.is_null() {
        return None;
    }
    let (mut x, mut y) = (0.0, 0.0);
    unsafe { ffi::clutter_event_get_coords(event, &mut x, &mut y) };
    Some((x, y))
}

/// Whole rows to scroll for a scroll event (positive = down). Smooth
/// (touchpad / high-resolution wheel) deltas accumulate in `acc` until they
/// add up to a row. Discrete events that Clutter emulates from smooth ones
/// are skipped so a wheel notch doesn't count twice.
pub fn scroll_rows(event: *const ffi::ClutterEvent, acc: &mut f64) -> i32 {
    if event.is_null() || unsafe { ffi::clutter_event_is_pointer_emulated(event) } != 0 {
        return 0;
    }
    match unsafe { ffi::clutter_event_get_scroll_direction(event) } {
        ffi::CLUTTER_SCROLL_UP => -1,
        ffi::CLUTTER_SCROLL_DOWN => 1,
        ffi::CLUTTER_SCROLL_SMOOTH => {
            let (mut dx, mut dy) = (0.0, 0.0);
            unsafe { ffi::clutter_event_get_scroll_delta(event, &mut dx, &mut dy) };
            accumulate_rows(acc, dy)
        }
        _ => 0,
    }
}

/// Add `delta` rows to `acc` and take out the whole rows.
pub fn accumulate_rows(acc: &mut f64, delta: f64) -> i32 {
    *acc += delta;
    let rows = acc.trunc();
    *acc -= rows;
    rows as i32
}

/// First visible index of a `max`-row window over `len` rows, moved just
/// enough from `offset` to show `selected`.
pub fn scroll_to_show(offset: usize, len: usize, max: usize, selected: usize) -> usize {
    let offset = if selected < offset {
        selected
    } else if selected >= offset + max {
        selected + 1 - max
    } else {
        offset
    };
    offset.min(len.saturating_sub(max))
}

/// A text actor. Returned actor is floating until parented.
pub unsafe fn label(font: &CStr, text: &str, color: &CoglColor) -> *mut ClutterActor {
    let text = CString::new(text.replace('\0', "")).unwrap_or_default();
    unsafe { ffi::clutter_text_new_full(font.as_ptr(), text.as_ptr(), color) }
}

/// Replace a text actor's text.
pub unsafe fn set_text(actor: *mut ClutterActor, text: &str) {
    let text = CString::new(text.replace('\0', "")).unwrap_or_default();
    unsafe { ffi::clutter_text_set_text(actor as *mut ffi::ClutterText, text.as_ptr()) }
}

/// A list row: fixed size, optional highlight, icon slot, left-aligned
/// ellipsized label. Returned actor is floating; parent it before wrapping it
/// with `gobj`.
pub unsafe fn list_row(
    text: &str,
    icon: Option<&glib::Object>,
    width: f32,
    selected: bool,
) -> *mut ClutterActor {
    unsafe {
        let row = ffi::clutter_actor_new();
        ffi::clutter_actor_set_size(row, width, ROW_HEIGHT);
        if selected {
            ffi::clutter_actor_set_background_color(row, &SELECTED_COLOR);
        }
        let text = CString::new(text.replace('\0', "")).unwrap_or_default();
        let label = ffi::clutter_text_new_full(ROW_FONT.as_ptr(), text.as_ptr(), &TEXT_COLOR);
        ffi::clutter_text_set_ellipsize(label as *mut ffi::ClutterText, ffi::PANGO_ELLIPSIZE_END);
        // The icon slot is reserved even without an icon, so labels line up.
        if let Some(content) = icon {
            let icon = icon_actor(content, ROW_ICON_SIZE);
            ffi::clutter_actor_add_child(row, icon);
            ffi::clutter_actor_set_position(icon, 12.0, ((ROW_HEIGHT - ROW_ICON_SIZE) / 2.0).floor());
        }
        let x = 12.0 + ROW_ICON_SIZE + 10.0;
        ffi::clutter_actor_set_width(label, width - x - 12.0);
        ffi::clutter_actor_add_child(row, label);
        let y = (ROW_HEIGHT - ffi::clutter_actor_get_height(label)) / 2.0;
        ffi::clutter_actor_set_position(label, x, y.floor());
        row
    }
}

/// Add a button to `panel` at `x`, with an icon made of filled rectangles
/// (`x`, `y`, `width`, `height` within the button), so that it does not depend
/// on the icon theme. `pressed` runs on a primary click.
pub fn panel_button(
    panel: *mut ClutterActor,
    x: f32,
    width: f32,
    height: f32,
    shapes: &[(f32, f32, f32, f32)],
    pressed: impl Fn() + 'static,
) {
    let button = unsafe {
        let button = ffi::clutter_actor_new();
        ffi::clutter_actor_set_size(button, width, height);
        ffi::clutter_actor_set_position(button, x, 0.0);
        ffi::clutter_actor_set_reactive(button, 1);
        for &(x, y, w, h) in shapes {
            let shape = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(shape, &TEXT_COLOR);
            ffi::clutter_actor_set_size(shape, w, h);
            ffi::clutter_actor_set_position(shape, x, y);
            ffi::clutter_actor_add_child(button, shape);
        }
        ffi::clutter_actor_add_child(panel, button);
        button
    };
    crate::windows::hover_highlight(button, TRANSPARENT, PANEL_HOVER_COLOR);
    unsafe { gobj(button) }.connect_local("button-press-event", false, move |args| {
        if unsafe { ffi::clutter_event_get_button(event_arg(args)) } == 1 {
            pressed();
        }
        Some(true.to_value())
    });
}

/// A reactive button: filled box with a centered label. Floating until parented.
pub unsafe fn button(text: &str, width: f32, height: f32, color: &CoglColor) -> *mut ClutterActor {
    unsafe {
        let b = ffi::clutter_actor_new();
        ffi::clutter_actor_set_size(b, width, height);
        ffi::clutter_actor_set_background_color(b, color);
        ffi::clutter_actor_set_reactive(b, 1);
        let l = label(BODY_FONT, text, &TEXT_COLOR);
        ffi::clutter_actor_add_child(b, l);
        let x = (width - ffi::clutter_actor_get_width(l)) / 2.0;
        let y = (height - ffi::clutter_actor_get_height(l)) / 2.0;
        ffi::clutter_actor_set_position(l, x.floor(), y.floor());
        b
    }
}

#[cfg(test)]
mod tests {
    use super::{accumulate_rows, scroll_to_show};

    #[test]
    fn smooth_deltas_accumulate() {
        let mut acc = 0.0;
        assert_eq!(accumulate_rows(&mut acc, 0.4), 0);
        assert_eq!(accumulate_rows(&mut acc, 0.4), 0);
        assert_eq!(accumulate_rows(&mut acc, 0.4), 1);
        assert_eq!(accumulate_rows(&mut acc, -2.5), -2);
    }

    #[test]
    fn keeps_selection_visible() {
        assert_eq!(scroll_to_show(0, 20, 8, 3), 0);
        assert_eq!(scroll_to_show(0, 20, 8, 8), 1);
        assert_eq!(scroll_to_show(5, 20, 8, 2), 2);
        assert_eq!(scroll_to_show(0, 20, 8, 19), 12);
        assert_eq!(scroll_to_show(10, 5, 8, 0), 0);
    }
}
