//! Shared look and small Clutter helpers for shell UI.

use crate::ffi::{self, ClutterActor, CoglColor, MetaDisplay, MtkRectangle};
use glib::prelude::*;
use glib::translate::*;
use std::ffi::{CStr, CString};

pub const TEXT_COLOR: CoglColor = CoglColor::rgba(0xf0, 0xf0, 0xf0, 0xff);
pub const HINT_COLOR: CoglColor = CoglColor::rgba(0x90, 0x90, 0x98, 0xff);
pub const BOX_COLOR: CoglColor = CoglColor::rgba(0x1e, 0x1e, 0x24, 0xf8);
pub const SELECTED_COLOR: CoglColor = CoglColor::rgba(0x35, 0x84, 0xe4, 0xff);
pub const DIM_COLOR: CoglColor = CoglColor::rgba(0, 0, 0, 0xa0);

pub const ROW_FONT: &CStr = c"Sans 12";
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
