//! Hand-written FFI for the small slice of libmutter / Clutter / Cogl we use.
//!
//! Signatures are taken from /usr/include/mutter-51. libmutter has no stable
//! ABI: re-check this file whenever the mutter API version (soname) changes.

#![allow(non_camel_case_types, dead_code)]

use glib_sys::{gboolean, GError, GSList, GType};
use gobject_sys::{GObject, GObjectClass};
use std::ffi::{c_char, c_double, c_float, c_int, c_uint};

// ---------------------------------------------------------------- opaque types

macro_rules! opaque {
    ($($name:ident),* $(,)?) => {
        $(#[repr(C)] pub struct $name { _p: [u8; 0] })*
    };
}

opaque!(
    MetaContext,
    MetaDisplay,
    MetaCompositor,
    MetaBackend,
    MetaWindow,
    MetaWorkspace,
    MetaWorkspaceManager,
    MetaKeyBinding,
    MetaCloseDialog,
    MetaInhibitShortcutsDialog,
    ClutterActor,
    ClutterStage,
    ClutterText,
    ClutterGrab,
    ClutterEvent,
    MetaStartupNotification,
    MetaLaunchContext,
    ClutterContext,
    ClutterBackend,
    ClutterSeat,
    ClutterVirtualInputDevice,
    ClutterColorState,
    PolkitSubject,
    PolkitIdentity,
    PolkitDetails,
    PolkitAgentSession,
    CoglContext,
    CoglTexture,
    MetaBackground,
    MetaBackgroundContent,
    GlyLoader,
    GlyImage,
    GlyFrame,
    GlyFrameRequest,
    ClutterContent,
    RsvgHandle,
    cairo_t,
    cairo_surface_t,
    ClutterInputDevice,
    MetaKeymapDescription,
    MetaSelection,
    MetaSelectionSource,
    MetaSoundPlayer,
    MetaCursorTracker,
);

/// MetaWindowActor is a ClutterActor subclass; we only ever pass it around.
pub type MetaWindowActor = ClutterActor;

// ---------------------------------------------------------------- plain structs

/// `graphene_point_t`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GraphenePoint {
    pub x: c_float,
    pub y: c_float,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct MtkRectangle {
    pub x: c_int,
    pub y: c_int,
    pub width: c_int,
    pub height: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MetaStrut {
    pub rect: MtkRectangle,
    pub side: c_int, // MetaSide
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CoglColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl CoglColor {
    pub const fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self { red, green, blue, alpha }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RsvgRectangle {
    pub x: c_double,
    pub y: c_double,
    pub width: c_double,
    pub height: c_double,
}

// ---------------------------------------------------------------- enums

pub const META_SIDE_TOP: c_int = 1 << 2;
pub const META_SIDE_BOTTOM: c_int = 1 << 3;
pub const META_WINDOW_NORMAL: c_int = 0;
pub const CLUTTER_EASE_OUT_QUAD: c_int = 3;
pub const CLUTTER_EASE_OUT_CUBIC: c_int = 6;
pub const META_TAB_LIST_NORMAL: c_int = 0;
pub const PANGO_ELLIPSIZE_END: c_int = 3;
pub const CLUTTER_POINTER_DEVICE: c_int = 0;
pub const CLUTTER_KEYBOARD_DEVICE: c_int = 1;
pub const CLUTTER_SCROLL_SOURCE_WHEEL: c_int = 1;
pub const CLUTTER_PAINT_FLAG_NO_CURSORS: c_int = 1 << 0;
pub const CLUTTER_PAINT_FLAG_CLEAR: c_int = 1 << 2;
/// COGL_PIXEL_FORMAT_BGRA_8888_PRE = 3 | A_BIT | PREMULT_BIT | BGR_BIT
pub const COGL_PIXEL_FORMAT_BGRA_8888_PRE: c_int = 3 | (1 << 4) | (1 << 7) | (1 << 5);
pub const CLUTTER_CONTENT_GRAVITY_RESIZE_ASPECT: c_int = 10;
pub const CAIRO_FORMAT_ARGB32: c_int = 0;
pub const CAIRO_STATUS_SUCCESS: c_int = 0;
pub const CAIRO_FORMAT_RGB24: c_int = 1;
pub const META_KEY_BINDING_IS_REVERSED: c_uint = 1 << 2;
pub const G_DESKTOP_BACKGROUND_STYLE_NONE: c_int = 0;
pub const G_DESKTOP_BACKGROUND_STYLE_WALLPAPER: c_int = 1;
pub const G_DESKTOP_BACKGROUND_STYLE_CENTERED: c_int = 2;
pub const G_DESKTOP_BACKGROUND_STYLE_SCALED: c_int = 3;
pub const G_DESKTOP_BACKGROUND_STYLE_STRETCHED: c_int = 4;
pub const G_DESKTOP_BACKGROUND_STYLE_ZOOM: c_int = 5;
pub const G_DESKTOP_BACKGROUND_STYLE_SPANNED: c_int = 6;
pub const G_DESKTOP_BACKGROUND_SHADING_SOLID: c_int = 0;
pub const G_DESKTOP_BACKGROUND_SHADING_VERTICAL: c_int = 1;
pub const G_DESKTOP_BACKGROUND_SHADING_HORIZONTAL: c_int = 2;
/// COGL_PIXEL_FORMAT_RGBA_8888_PRE = 3 | A_BIT | PREMULT_BIT
pub const COGL_PIXEL_FORMAT_RGBA_8888_PRE: c_int = 3 | (1 << 4) | (1 << 7);
/// `GlyMemoryFormatSelection` and `GlyMemoryFormat`
pub const GLY_MEMORY_SELECTION_R8G8B8A8_PREMULTIPLIED: c_uint = 1 << 2;
pub const GLY_MEMORY_R8G8B8A8_PREMULTIPLIED: c_int = 2;
pub const CLUTTER_SCROLL_UP: c_int = 0;
pub const CLUTTER_SCROLL_DOWN: c_int = 1;
pub const CLUTTER_SCROLL_SMOOTH: c_int = 4;
pub const CLUTTER_KEY_STATE_RELEASED: c_int = 0;
pub const CLUTTER_KEY_STATE_PRESSED: c_int = 1;

/// `void (*MetaKeyHandlerFunc) (MetaDisplay*, MetaWindow*, const ClutterEvent*,
///                              MetaKeyBinding*, gpointer)`
pub type MetaKeyHandlerFunc = Option<
    unsafe extern "C" fn(
        *mut MetaDisplay,
        *mut MetaWindow,
        *const ClutterEvent,
        *mut MetaKeyBinding,
        glib_sys::gpointer,
    ),
>;

// ---------------------------------------------------------------- MetaPlugin

/// `G_DECLARE_DERIVABLE_TYPE (MetaPlugin, ...)`: instance is just a GObject.
#[repr(C)]
pub struct MetaPlugin {
    pub parent_instance: GObject,
}

type ActorFn = Option<unsafe extern "C" fn(*mut MetaPlugin, *mut MetaWindowActor)>;

/// Must match `struct _MetaPluginClass` in meta/meta-plugin.h field-for-field.
#[repr(C)]
pub struct MetaPluginClass {
    pub parent_class: GObjectClass,

    pub start: Option<unsafe extern "C" fn(*mut MetaPlugin)>,
    pub minimize: ActorFn,
    pub unminimize: ActorFn,
    pub size_changed: ActorFn,
    pub size_change: Option<
        unsafe extern "C" fn(
            *mut MetaPlugin,
            *mut MetaWindowActor,
            c_int, // MetaSizeChange
            *mut MtkRectangle,
            *mut MtkRectangle,
        ),
    >,
    pub map: ActorFn,
    pub destroy: ActorFn,
    pub switch_workspace:
        Option<unsafe extern "C" fn(*mut MetaPlugin, c_int, c_int, c_int /* MetaMotionDirection */)>,
    pub show_tile_preview:
        Option<unsafe extern "C" fn(*mut MetaPlugin, *mut MetaWindow, *mut MtkRectangle, c_int)>,
    pub hide_tile_preview: Option<unsafe extern "C" fn(*mut MetaPlugin)>,
    pub show_window_menu:
        Option<unsafe extern "C" fn(*mut MetaPlugin, *mut MetaWindow, c_int, c_int, c_int)>,
    pub show_window_menu_for_rect:
        Option<unsafe extern "C" fn(*mut MetaPlugin, *mut MetaWindow, c_int, *mut MtkRectangle)>,
    pub kill_window_effects: ActorFn,
    pub kill_switch_workspace: Option<unsafe extern "C" fn(*mut MetaPlugin)>,
    pub keybinding_filter:
        Option<unsafe extern "C" fn(*mut MetaPlugin, *mut MetaKeyBinding) -> gboolean>,
    pub confirm_display_change: Option<unsafe extern "C" fn(*mut MetaPlugin)>,
    pub create_close_dialog:
        Option<unsafe extern "C" fn(*mut MetaPlugin, *mut MetaWindow) -> *mut MetaCloseDialog>,
    pub create_inhibit_shortcuts_dialog: Option<
        unsafe extern "C" fn(*mut MetaPlugin, *mut MetaWindow) -> *mut MetaInhibitShortcutsDialog,
    >,
    pub locate_pointer: Option<unsafe extern "C" fn(*mut MetaPlugin)>,
}

// ---------------------------------------------------------------- PolkitAgentListener

/// `struct _PolkitAgentListener`: just a GObject.
#[repr(C)]
pub struct PolkitAgentListener {
    pub parent_instance: GObject,
}

/// Must match `struct _PolkitAgentListenerClass` in polkitagentlistener.h.
#[repr(C)]
pub struct PolkitAgentListenerClass {
    pub parent_class: GObjectClass,
    pub initiate_authentication: Option<
        unsafe extern "C" fn(
            *mut PolkitAgentListener,
            *const c_char,          // action_id
            *const c_char,          // message
            *const c_char,          // icon_name
            *mut PolkitDetails,
            *const c_char,          // cookie
            *mut glib_sys::GList,   // identities (PolkitIdentity*)
            *mut gio::ffi::GCancellable,
            gio::ffi::GAsyncReadyCallback,
            glib_sys::gpointer,
        ),
    >,
    pub initiate_authentication_finish: Option<
        unsafe extern "C" fn(
            *mut PolkitAgentListener,
            *mut gio::ffi::GAsyncResult,
            *mut *mut GError,
        ) -> gboolean,
    >,
    pub _reserved: [Option<unsafe extern "C" fn()>; 8],
}

pub const POLKIT_ERROR_FAILED: c_int = 0;
pub const POLKIT_ERROR_CANCELLED: c_int = 1;
pub const POLKIT_AGENT_REGISTER_FLAGS_NONE: c_int = 0;

pub const META_SELECTION_CLIPBOARD: c_int = 1; // MetaSelectionType

// ---------------------------------------------------------------- functions

unsafe extern "C" {
    // glib-unix.h (not wrapped by the glib crate)
    pub fn g_unix_signal_add(
        signum: c_int,
        handler: glib_sys::GSourceFunc,
        user_data: glib_sys::gpointer,
    ) -> c_uint;

    // meta-context.h
    pub fn meta_create_context(name: *const c_char) -> *mut MetaContext;
    pub fn meta_context_destroy(ctx: *mut MetaContext);
    pub fn meta_context_set_plugin_gtype(ctx: *mut MetaContext, gtype: GType);
    pub fn meta_context_set_gnome_wm_keybindings(ctx: *mut MetaContext, wm: *const c_char);
    pub fn meta_context_configure(
        ctx: *mut MetaContext,
        argc: *mut c_int,
        argv: *mut *mut *mut c_char,
        error: *mut *mut GError,
    ) -> gboolean;
    pub fn meta_context_setup(ctx: *mut MetaContext, error: *mut *mut GError) -> gboolean;
    pub fn meta_context_start(ctx: *mut MetaContext, error: *mut *mut GError) -> gboolean;
    pub fn meta_context_notify_ready(ctx: *mut MetaContext);
    pub fn meta_context_run_main_loop(ctx: *mut MetaContext, error: *mut *mut GError) -> gboolean;
    pub fn meta_context_terminate(ctx: *mut MetaContext);
    pub fn meta_context_raise_rlimit_nofile(ctx: *mut MetaContext, error: *mut *mut GError)
        -> gboolean;
    pub fn meta_context_get_backend(ctx: *mut MetaContext) -> *mut MetaBackend;

    // meta-backend.h
    pub fn meta_backend_get_monitor_manager(backend: *mut MetaBackend) -> *mut GObject;

    // meta-plugin.h
    pub fn meta_plugin_get_type() -> GType;
    pub fn meta_plugin_get_display(plugin: *mut MetaPlugin) -> *mut MetaDisplay;
    pub fn meta_plugin_map_completed(plugin: *mut MetaPlugin, actor: *mut MetaWindowActor);
    pub fn meta_plugin_destroy_completed(plugin: *mut MetaPlugin, actor: *mut MetaWindowActor);
    pub fn meta_plugin_minimize_completed(plugin: *mut MetaPlugin, actor: *mut MetaWindowActor);
    pub fn meta_plugin_unminimize_completed(plugin: *mut MetaPlugin, actor: *mut MetaWindowActor);

    // display.h
    pub fn meta_display_get_context(display: *mut MetaDisplay) -> *mut MetaContext;
    pub fn meta_display_get_compositor(display: *mut MetaDisplay) -> *mut MetaCompositor;
    pub fn meta_display_get_primary_monitor(display: *mut MetaDisplay) -> c_int;
    pub fn meta_display_get_n_monitors(display: *mut MetaDisplay) -> c_int;
    pub fn meta_display_get_monitor_geometry(
        display: *mut MetaDisplay,
        monitor: c_int,
        geometry: *mut MtkRectangle,
    );
    pub fn meta_display_get_monitor_in_fullscreen(display: *mut MetaDisplay, monitor: c_int)
        -> gboolean;
    pub fn meta_display_get_workspace_manager(display: *mut MetaDisplay)
        -> *mut MetaWorkspaceManager;

    // compositor.h
    pub fn meta_compositor_get_stage(compositor: *mut MetaCompositor) -> *mut ClutterStage;
    pub fn meta_compositor_get_window_group(compositor: *mut MetaCompositor) -> *mut ClutterActor;
    pub fn meta_compositor_get_top_window_group(compositor: *mut MetaCompositor) -> *mut ClutterActor;
    pub fn meta_compositor_disable_unredirect(compositor: *mut MetaCompositor);
    pub fn meta_compositor_enable_unredirect(compositor: *mut MetaCompositor);

    // meta-workspace-manager.h / workspace.h
    pub fn meta_workspace_manager_get_n_workspaces(wm: *mut MetaWorkspaceManager) -> c_int;
    pub fn meta_workspace_manager_get_workspace_by_index(
        wm: *mut MetaWorkspaceManager,
        index: c_int,
    ) -> *mut MetaWorkspace;
    pub fn meta_workspace_manager_append_new_workspace(
        wm: *mut MetaWorkspaceManager,
        activate: gboolean,
        timestamp: u32,
    ) -> *mut MetaWorkspace;
    pub fn meta_workspace_manager_remove_workspace(wm: *mut MetaWorkspaceManager, ws: *mut MetaWorkspace, timestamp: u32);
    pub fn meta_workspace_manager_get_active_workspace_index(wm: *mut MetaWorkspaceManager) -> c_int;
    pub fn meta_workspace_index(ws: *mut MetaWorkspace) -> c_int;
    pub fn meta_workspace_activate(ws: *mut MetaWorkspace, timestamp: u32);
    pub fn meta_window_get_workspace(window: *mut MetaWindow) -> *mut MetaWorkspace;
    pub fn meta_window_is_on_all_workspaces(window: *mut MetaWindow) -> gboolean;
    pub fn meta_window_showing_on_its_workspace(window: *mut MetaWindow) -> gboolean;
    pub fn meta_plugin_switch_workspace_completed(plugin: *mut MetaPlugin);
    pub fn clutter_clone_new(source: *mut ClutterActor) -> *mut ClutterActor;
    pub fn clutter_actor_get_position(actor: *mut ClutterActor, x: *mut c_float, y: *mut c_float);
    pub fn clutter_actor_set_x(actor: *mut ClutterActor, x: c_float);
    pub fn clutter_actor_insert_child_above(
        parent: *mut ClutterActor,
        child: *mut ClutterActor,
        sibling: *mut ClutterActor,
    );
    pub fn meta_workspace_set_builtin_struts(ws: *mut MetaWorkspace, struts: *mut GSList);

    // window.h / meta-window-actor.h
    pub fn meta_window_actor_get_meta_window(actor: *mut MetaWindowActor) -> *mut MetaWindow;
    pub fn meta_window_get_window_type(window: *mut MetaWindow) -> c_int;

    // clutter-actor.h
    pub fn clutter_actor_new() -> *mut ClutterActor;
    pub fn clutter_actor_show(actor: *mut ClutterActor);
    pub fn clutter_actor_destroy(actor: *mut ClutterActor);
    pub fn clutter_actor_hide(actor: *mut ClutterActor);
    pub fn clutter_actor_add_child(parent: *mut ClutterActor, child: *mut ClutterActor);
    pub fn clutter_actor_insert_child_below(
        parent: *mut ClutterActor,
        child: *mut ClutterActor,
        sibling: *mut ClutterActor,
    );
    pub fn clutter_actor_set_position(actor: *mut ClutterActor, x: c_float, y: c_float);
    pub fn clutter_actor_set_size(actor: *mut ClutterActor, w: c_float, h: c_float);
    pub fn clutter_actor_get_x(actor: *mut ClutterActor) -> c_float;
    pub fn clutter_actor_get_width(actor: *mut ClutterActor) -> c_float;
    pub fn clutter_actor_get_height(actor: *mut ClutterActor) -> c_float;
    pub fn clutter_actor_set_background_color(actor: *mut ClutterActor, color: *const CoglColor);
    pub fn clutter_actor_set_opacity(actor: *mut ClutterActor, opacity: u8);
    pub fn clutter_actor_set_pivot_point(actor: *mut ClutterActor, x: c_float, y: c_float);
    pub fn clutter_actor_set_scale(actor: *mut ClutterActor, sx: c_double, sy: c_double);
    pub fn clutter_actor_save_easing_state(actor: *mut ClutterActor);
    pub fn clutter_actor_restore_easing_state(actor: *mut ClutterActor);
    pub fn clutter_actor_set_easing_mode(actor: *mut ClutterActor, mode: c_int);
    pub fn clutter_actor_set_easing_duration(actor: *mut ClutterActor, msecs: c_uint);
    pub fn clutter_actor_remove_all_transitions(actor: *mut ClutterActor);
    pub fn clutter_actor_get_transition(actor: *mut ClutterActor, name: *const c_char)
        -> *mut GObject;
    pub fn clutter_actor_set_width(actor: *mut ClutterActor, w: c_float);
    pub fn clutter_actor_set_reactive(actor: *mut ClutterActor, reactive: gboolean);
    pub fn clutter_actor_grab_key_focus(actor: *mut ClutterActor);
    pub fn clutter_actor_destroy_all_children(actor: *mut ClutterActor);

    // clutter-stage.h / clutter-grab.h
    pub fn clutter_stage_grab(stage: *mut ClutterStage, actor: *mut ClutterActor) -> *mut ClutterGrab;
    pub fn clutter_grab_dismiss(grab: *mut ClutterGrab);

    // clutter-event.h
    pub fn clutter_event_get_scroll_direction(event: *const ClutterEvent) -> c_int;
    pub fn clutter_event_get_button(event: *const ClutterEvent) -> u32;
    pub fn clutter_event_get_scroll_delta(event: *const ClutterEvent, dx: *mut c_double, dy: *mut c_double);
    pub fn clutter_event_is_pointer_emulated(event: *const ClutterEvent) -> gboolean;
    pub fn clutter_event_get_key_symbol(event: *const ClutterEvent) -> c_uint;

    // pango/clutter-text.h
    pub fn clutter_text_new_full(
        font_name: *const c_char,
        text: *const c_char,
        color: *const CoglColor,
    ) -> *mut ClutterActor;
    pub fn clutter_text_set_text(text: *mut ClutterText, s: *const c_char);
    pub fn clutter_text_get_text(text: *mut ClutterText) -> *const c_char;
    pub fn clutter_text_set_editable(text: *mut ClutterText, editable: gboolean);
    pub fn clutter_text_set_activatable(text: *mut ClutterText, activatable: gboolean);
    pub fn clutter_text_set_single_line_mode(text: *mut ClutterText, single_line: gboolean);
    pub fn clutter_text_set_cursor_color(text: *mut ClutterText, color: *const CoglColor);

    // display.h / meta-startup-notification.h / meta-launch-context.h
    pub fn meta_display_get_current_time_roundtrip(display: *mut MetaDisplay) -> u32;
    pub fn meta_display_get_startup_notification(display: *mut MetaDisplay)
        -> *mut MetaStartupNotification;
    pub fn meta_startup_notification_create_launcher(sn: *mut MetaStartupNotification)
        -> *mut MetaLaunchContext;
    pub fn meta_launch_context_set_timestamp(ctx: *mut MetaLaunchContext, timestamp: u32);

    // keybindings.h
    pub fn meta_keybindings_set_custom_handler(
        name: *const c_char,
        handler: MetaKeyHandlerFunc,
        user_data: glib_sys::gpointer,
        free_data: glib_sys::GDestroyNotify,
    ) -> gboolean;
    pub fn meta_display_add_keybinding(
        display: *mut MetaDisplay,
        name: *const c_char,
        settings: *mut gio::ffi::GSettings,
        flags: c_uint, // MetaKeyBindingFlags
        handler: MetaKeyHandlerFunc,
        user_data: glib_sys::gpointer,
        free_data: glib_sys::GDestroyNotify,
    ) -> c_uint;
    pub fn meta_display_remove_keybinding(display: *mut MetaDisplay, name: *const c_char) -> gboolean;
    pub fn meta_key_binding_get_name(binding: *mut MetaKeyBinding) -> *const c_char;
    pub fn meta_key_binding_is_reversed(binding: *mut MetaKeyBinding) -> gboolean;

    // tab list / window activation
    pub fn meta_display_get_tab_list(
        display: *mut MetaDisplay,
        list_type: c_int, // MetaTabList
        workspace: *mut MetaWorkspace,
    ) -> *mut glib_sys::GList;
    pub fn meta_display_get_focus_window(display: *mut MetaDisplay) -> *mut MetaWindow;
    pub fn meta_workspace_manager_get_active_workspace(wm: *mut MetaWorkspaceManager)
        -> *mut MetaWorkspace;
    pub fn meta_window_activate(window: *mut MetaWindow, timestamp: u32);
    pub fn meta_window_get_title(window: *mut MetaWindow) -> *const c_char;
    pub fn meta_window_get_wm_class(window: *mut MetaWindow) -> *const c_char;
    pub fn meta_window_get_id(window: *mut MetaWindow) -> u64;
    // taskbar and window overview
    pub fn meta_window_get_compositor_private(window: *mut MetaWindow) -> *mut gobject_sys::GObject;
    pub fn meta_window_get_buffer_rect(window: *mut MetaWindow, rect: *mut MtkRectangle);
    pub fn meta_window_get_stable_sequence(window: *mut MetaWindow) -> u32;
    pub fn meta_window_minimize(window: *mut MetaWindow);
    pub fn clutter_actor_set_clip_to_allocation(actor: *mut ClutterActor, clip: gboolean);
    // window menu
    pub fn meta_window_can_minimize(window: *mut MetaWindow) -> gboolean;
    pub fn meta_window_can_maximize(window: *mut MetaWindow) -> gboolean;
    pub fn meta_window_can_close(window: *mut MetaWindow) -> gboolean;
    pub fn meta_window_is_maximized(window: *mut MetaWindow) -> gboolean;
    pub fn meta_window_is_above(window: *mut MetaWindow) -> gboolean;
    pub fn meta_window_is_always_on_all_workspaces(window: *mut MetaWindow) -> gboolean;
    pub fn meta_window_maximize(window: *mut MetaWindow);
    pub fn meta_window_unmaximize(window: *mut MetaWindow);
    pub fn meta_window_make_above(window: *mut MetaWindow);
    pub fn meta_window_unmake_above(window: *mut MetaWindow);
    pub fn meta_window_stick(window: *mut MetaWindow);
    pub fn meta_window_unstick(window: *mut MetaWindow);
    pub fn meta_window_change_workspace_by_index(window: *mut MetaWindow, index: c_int, append: gboolean);
    pub fn meta_window_delete(window: *mut MetaWindow, timestamp: u32);
    pub fn meta_window_get_frame_rect(window: *mut MetaWindow, rect: *mut MtkRectangle);
    pub fn clutter_text_set_ellipsize(text: *mut ClutterText, mode: c_int);
    pub fn clutter_text_set_line_wrap(text: *mut ClutterText, wrap: gboolean);
    pub fn clutter_text_set_password_char(text: *mut ClutterText, wc: u32);
    pub fn clutter_text_get_editable(text: *mut ClutterText) -> gboolean;
    pub fn clutter_text_set_cursor_visible(text: *mut ClutterText, visible: gboolean);

    // polkit-agent-1 / polkit-gobject-1
    pub fn polkit_agent_listener_get_type() -> GType;
    pub fn polkit_agent_listener_register(
        listener: *mut PolkitAgentListener,
        flags: c_int,
        subject: *mut PolkitSubject,
        object_path: *const c_char,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut GError,
    ) -> glib_sys::gpointer;
    pub fn polkit_agent_listener_unregister(handle: glib_sys::gpointer);
    pub fn polkit_agent_session_new(
        identity: *mut PolkitIdentity,
        cookie: *const c_char,
    ) -> *mut PolkitAgentSession;
    pub fn polkit_agent_session_initiate(session: *mut PolkitAgentSession);
    pub fn polkit_agent_session_response(session: *mut PolkitAgentSession, response: *const c_char);
    pub fn polkit_agent_session_cancel(session: *mut PolkitAgentSession);
    pub fn polkit_unix_session_new_for_process_sync(
        pid: c_int,
        cancellable: *mut gio::ffi::GCancellable,
        error: *mut *mut GError,
    ) -> *mut PolkitSubject;
    pub fn polkit_unix_session_new(session_id: *const c_char) -> *mut PolkitSubject;
    pub fn polkit_unix_process_new_for_owner(pid: c_int, start_time: u64, uid: c_int)
        -> *mut PolkitSubject;
    pub fn polkit_unix_user_get_type() -> GType;
    pub fn polkit_unix_user_get_uid(user: *mut PolkitIdentity) -> c_int;
    pub fn polkit_unix_user_get_name(user: *mut PolkitIdentity) -> *const c_char;
    pub fn polkit_error_quark() -> glib_sys::GQuark;

    // libc
    pub fn getuid() -> c_uint;

    // event time / seat / virtual input (debug hooks)
    pub fn clutter_get_current_event_time() -> u32;
    pub fn meta_compositor_get_window_actors(compositor: *mut MetaCompositor) -> *mut glib_sys::GList;
    pub fn clutter_actor_is_visible(actor: *mut ClutterActor) -> gboolean;
    pub fn clutter_actor_is_mapped(actor: *mut ClutterActor) -> gboolean;
    pub fn clutter_actor_get_opacity(actor: *mut ClutterActor) -> u8;
    pub fn clutter_actor_get_paint_opacity(actor: *mut ClutterActor) -> u8;
    pub fn clutter_actor_get_parent(actor: *mut ClutterActor) -> *mut ClutterActor;
    pub fn clutter_stage_paint_to_buffer(
        stage: *mut ClutterStage,
        rect: *const MtkRectangle,
        scale: c_float,
        data: *mut u8,
        stride: c_int,
        format: c_int, // CoglPixelFormat
        color_state: *mut ClutterColorState,
        paint_flags: c_int, // ClutterPaintFlag
        error: *mut *mut GError,
    ) -> gboolean;
    pub fn clutter_actor_get_context(actor: *mut ClutterActor) -> *mut ClutterContext;
    pub fn clutter_context_get_backend(ctx: *mut ClutterContext) -> *mut ClutterBackend;
    pub fn clutter_backend_get_default_seat(backend: *mut ClutterBackend) -> *mut ClutterSeat;
    pub fn clutter_seat_create_virtual_device(
        seat: *mut ClutterSeat,
        device_type: c_int, // ClutterInputDeviceType
    ) -> *mut ClutterVirtualInputDevice;
    pub fn clutter_virtual_input_device_notify_absolute_motion(
        dev: *mut ClutterVirtualInputDevice,
        time_us: u64,
        x: c_double,
        y: c_double,
    );
    pub fn clutter_virtual_input_device_notify_button(
        dev: *mut ClutterVirtualInputDevice,
        time_us: u64,
        button: u32,
        button_state: c_int, // ClutterButtonState
    );
    pub fn clutter_virtual_input_device_notify_discrete_scroll(
        dev: *mut ClutterVirtualInputDevice,
        time_us: u64,
        direction: c_int, // ClutterScrollDirection
        source: c_int,    // ClutterScrollSource
    );
    pub fn clutter_virtual_input_device_notify_keyval(
        dev: *mut ClutterVirtualInputDevice,
        time_us: u64,
        keyval: u32,
        key_state: c_int, // ClutterKeyState
    );

    // desktop background
    pub fn meta_background_group_new() -> *mut ClutterActor;
    pub fn meta_background_actor_new(display: *mut MetaDisplay, monitor: c_int) -> *mut ClutterActor;
    pub fn meta_background_new(display: *mut MetaDisplay) -> *mut MetaBackground;
    pub fn meta_background_set_gradient(
        background: *mut MetaBackground,
        shading: c_int, // GDesktopBackgroundShading
        color: *const CoglColor,
        second_color: *const CoglColor,
    );
    pub fn meta_background_set_texture(
        background: *mut MetaBackground,
        texture: *mut CoglTexture,
        style: c_int, // GDesktopBackgroundStyle
        color_state: *mut ClutterColorState,
    );
    pub fn meta_background_content_set_background(
        content: *mut MetaBackgroundContent,
        background: *mut MetaBackground,
    );
    pub fn clutter_actor_get_content(actor: *mut ClutterActor) -> *mut ClutterContent;

    // glycin: image loading
    pub fn gly_loader_new(file: *mut gio::ffi::GFile) -> *mut gobject_sys::GObject;
    pub fn gly_loader_set_accepted_memory_formats(loader: *mut GlyLoader, selection: c_uint);
    pub fn gly_loader_load(loader: *mut GlyLoader, error: *mut *mut GError) -> *mut GlyImage;
    pub fn gly_image_get_mime_type(image: *mut GlyImage) -> *const c_char;
    pub fn gly_image_get_width(image: *mut GlyImage) -> u32;
    pub fn gly_image_get_height(image: *mut GlyImage) -> u32;
    pub fn gly_image_next_frame(image: *mut GlyImage, error: *mut *mut GError) -> *mut GlyFrame;
    pub fn gly_image_get_specific_frame(
        image: *mut GlyImage,
        request: *mut GlyFrameRequest,
        error: *mut *mut GError,
    ) -> *mut GlyFrame;
    pub fn gly_frame_request_new() -> *mut GlyFrameRequest;
    pub fn gly_frame_request_set_scale(request: *mut GlyFrameRequest, width: u32, height: u32);
    pub fn gly_frame_get_width(frame: *mut GlyFrame) -> u32;
    pub fn gly_frame_get_height(frame: *mut GlyFrame) -> u32;
    pub fn gly_frame_get_stride(frame: *mut GlyFrame) -> u32;
    pub fn gly_frame_get_buf_bytes(frame: *mut GlyFrame) -> *mut glib_sys::GBytes;
    pub fn gly_frame_get_memory_format(frame: *mut GlyFrame) -> c_int;

    // app icons: window app ids, scale, textures
    pub fn meta_window_get_sandboxed_app_id(window: *mut MetaWindow) -> *const c_char;
    pub fn meta_window_get_gtk_application_id(window: *mut MetaWindow) -> *const c_char;
    pub fn meta_window_get_wm_class_instance(window: *mut MetaWindow) -> *const c_char;
    pub fn meta_display_get_monitor_scale(display: *mut MetaDisplay, monitor: c_int) -> c_float;
    pub fn clutter_backend_get_cogl_context(backend: *mut ClutterBackend) -> *mut CoglContext;
    pub fn cogl_texture_2d_new_from_data(
        ctx: *mut CoglContext,
        width: c_int,
        height: c_int,
        format: c_int, // CoglPixelFormat
        rowstride: c_int,
        data: *const u8,
        error: *mut *mut GError,
    ) -> *mut CoglTexture;
    pub fn clutter_texture_content_new_from_texture(
        texture: *mut CoglTexture,
        clip: *mut MtkRectangle,
    ) -> *mut ClutterContent;
    pub fn clutter_actor_set_child_above_sibling(
        parent: *mut ClutterActor,
        child: *mut ClutterActor,
        sibling: *mut ClutterActor,
    );
    pub fn clutter_actor_set_content(actor: *mut ClutterActor, content: *mut ClutterContent);
    pub fn clutter_actor_set_content_gravity(actor: *mut ClutterActor, gravity: c_int);

    // librsvg / cairo
    pub fn rsvg_handle_new_from_file(filename: *const c_char, error: *mut *mut GError)
        -> *mut RsvgHandle;
    pub fn rsvg_handle_set_stylesheet(
        handle: *mut RsvgHandle,
        css: *const u8,
        css_len: usize,
        error: *mut *mut GError,
    ) -> gboolean;
    pub fn rsvg_handle_render_document(
        handle: *mut RsvgHandle,
        cr: *mut cairo_t,
        viewport: *const RsvgRectangle,
        error: *mut *mut GError,
    ) -> gboolean;
    pub fn cairo_image_surface_create(format: c_int, width: c_int, height: c_int)
        -> *mut cairo_surface_t;
    pub fn cairo_image_surface_create_from_png(filename: *const c_char) -> *mut cairo_surface_t;
    pub fn cairo_image_surface_get_width(surface: *mut cairo_surface_t) -> c_int;
    pub fn cairo_image_surface_get_height(surface: *mut cairo_surface_t) -> c_int;
    pub fn cairo_image_surface_get_stride(surface: *mut cairo_surface_t) -> c_int;
    pub fn cairo_image_surface_get_data(surface: *mut cairo_surface_t) -> *mut u8;
    pub fn cairo_surface_status(surface: *mut cairo_surface_t) -> c_int;
    pub fn cairo_surface_flush(surface: *mut cairo_surface_t);
    pub fn cairo_surface_destroy(surface: *mut cairo_surface_t);
    pub fn cairo_image_surface_create_for_data(
        data: *mut u8,
        format: c_int, // cairo_format_t
        width: c_int,
        height: c_int,
        stride: c_int,
    ) -> *mut cairo_surface_t;
    pub fn cairo_surface_write_to_png_stream(
        surface: *mut cairo_surface_t,
        write: Option<unsafe extern "C" fn(*mut std::ffi::c_void, *const u8, c_uint) -> c_int>,
        closure: *mut std::ffi::c_void,
    ) -> c_int;
    pub fn clutter_event_get_coords(event: *const ClutterEvent, x: *mut c_float, y: *mut c_float);
    pub fn cairo_create(target: *mut cairo_surface_t) -> *mut cairo_t;
    pub fn cairo_destroy(cr: *mut cairo_t);
    pub fn cairo_scale(cr: *mut cairo_t, sx: c_double, sy: c_double);
    pub fn cairo_translate(cr: *mut cairo_t, tx: c_double, ty: c_double);
    pub fn cairo_set_source_surface(
        cr: *mut cairo_t,
        surface: *mut cairo_surface_t,
        x: c_double,
        y: c_double,
    );
    pub fn cairo_paint(cr: *mut cairo_t);

    // input sources / keymaps
    pub fn meta_keymap_description_new_from_rules(
        model: *const c_char,
        layout: *const c_char,
        variant: *const c_char,
        options: *const c_char,
        display_names: *mut *mut c_char,
        short_names: *mut *mut c_char,
    ) -> *mut MetaKeymapDescription;
    pub fn meta_keymap_description_unref(desc: *mut MetaKeymapDescription);
    pub fn meta_backend_set_keymap_async(
        backend: *mut MetaBackend,
        desc: *mut MetaKeymapDescription,
        layout_index: u32,
        cancellable: *mut gio::ffi::GCancellable,
        callback: gio::ffi::GAsyncReadyCallback,
        user_data: glib_sys::gpointer,
    );
    pub fn meta_backend_set_keymap_finish(
        backend: *mut MetaBackend,
        result: *mut gio::ffi::GAsyncResult,
        error: *mut *mut GError,
    ) -> gboolean;

    // accelerators for org.gnome.Shell
    pub fn meta_display_grab_accelerator(
        display: *mut MetaDisplay,
        accelerator: *const c_char,
        flags: c_uint, // MetaKeyBindingFlags
    ) -> c_uint;
    pub fn meta_display_ungrab_accelerator(display: *mut MetaDisplay, action: c_uint) -> gboolean;
    pub fn clutter_input_device_get_device_node(device: *mut ClutterInputDevice) -> *const c_char;

    // clipboard
    pub fn meta_display_get_selection(display: *mut MetaDisplay) -> *mut MetaSelection;
    pub fn meta_selection_source_memory_new(
        mimetype: *const c_char,
        content: *mut glib_sys::GBytes,
        error: *mut *mut GError,
    ) -> *mut MetaSelectionSource;
    pub fn meta_selection_set_owner(
        selection: *mut MetaSelection,
        selection_type: c_int, // MetaSelectionType
        owner: *mut MetaSelectionSource,
    );

    // sounds
    pub fn meta_display_get_sound_player(display: *mut MetaDisplay) -> *mut MetaSoundPlayer;
    pub fn meta_sound_player_play_from_theme(
        player: *mut MetaSoundPlayer,
        name: *const c_char,
        description: *const c_char,
        cancellable: *mut gio::ffi::GCancellable,
    );
    pub fn meta_sound_player_play_from_file(
        player: *mut MetaSoundPlayer,
        file: *mut gio::ffi::GFile,
        description: *const c_char,
        cancellable: *mut gio::ffi::GCancellable,
    );

    // the pointer's image and position
    pub fn meta_backend_get_cursor_tracker(backend: *mut MetaBackend) -> *mut MetaCursorTracker;
    pub fn meta_cursor_tracker_get_sprite(tracker: *mut MetaCursorTracker) -> *mut CoglTexture;
    pub fn meta_cursor_tracker_get_scale(tracker: *mut MetaCursorTracker) -> c_float;
    pub fn meta_cursor_tracker_get_hot(tracker: *mut MetaCursorTracker, x: *mut c_int, y: *mut c_int);
    pub fn meta_cursor_tracker_get_pointer(
        tracker: *mut MetaCursorTracker,
        coords: *mut GraphenePoint,
        mods: *mut c_uint, // ClutterModifierType
    );
    pub fn meta_cursor_tracker_get_pointer_visible(tracker: *mut MetaCursorTracker) -> gboolean;
    pub fn cogl_texture_get_width(texture: *mut CoglTexture) -> c_uint;
    pub fn cogl_texture_get_height(texture: *mut CoglTexture) -> c_uint;
    pub fn cogl_texture_get_data(
        texture: *mut CoglTexture,
        format: c_int, // CoglPixelFormat
        rowstride: c_uint,
        data: *mut u8,
    ) -> c_int;

    // libsystemd
    pub fn sd_notify(unset_environment: c_int, state: *const c_char) -> c_int;
}
