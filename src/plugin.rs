//! `MetaPlugin` subclassing support plus the gnative shell plugin itself.

use crate::ffi::{self, ClutterActor, CoglColor, MetaDisplay, MtkRectangle};
use crate::launcher::Launcher;
use crate::switcher::Switcher;
use crate::ui::gobj;
use glib::prelude::*;
use glib::subclass::prelude::*;
use glib::translate::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use std::rc::Rc;

// =============================================================================
// Bindings for subclassing MetaPlugin from Rust
// =============================================================================

glib::wrapper! {
    /// Rust wrapper for the C `MetaPlugin` GObject class.
    pub struct MetaPlugin(Object<ffi::MetaPlugin, ffi::MetaPluginClass>);

    match fn {
        type_ => || ffi::meta_plugin_get_type(),
    }
}

impl MetaPlugin {
    pub fn display(&self) -> *mut MetaDisplay {
        unsafe { ffi::meta_plugin_get_display(self.as_ptr()) }
    }
}

/// Virtual methods a Rust `MetaPlugin` subclass can override.
///
/// Vfuncs left out here stay NULL in the class struct, which tells mutter to
/// perform that operation immediately without an effect.
pub trait MetaPluginImpl: ObjectImpl + ObjectSubclass<Type: IsA<MetaPlugin>> {
    fn start(&self);
    fn map(&self, actor: *mut ClutterActor);
    fn destroy(&self, actor: *mut ClutterActor);
    fn minimize(&self, actor: *mut ClutterActor);
    fn unminimize(&self, actor: *mut ClutterActor);
    fn kill_window_effects(&self, actor: *mut ClutterActor);
}

unsafe impl<T: MetaPluginImpl> IsSubclassable<T> for MetaPlugin {
    fn class_init(class: &mut glib::Class<Self>) {
        Self::parent_class_init::<T>(class);
        let klass = class.as_mut();
        klass.start = Some(vfunc_start::<T>);
        klass.map = Some(vfunc_map::<T>);
        klass.destroy = Some(vfunc_destroy::<T>);
        klass.minimize = Some(vfunc_minimize::<T>);
        klass.unminimize = Some(vfunc_unminimize::<T>);
        klass.kill_window_effects = Some(vfunc_kill_window_effects::<T>);
    }
}

unsafe fn imp<'a, T: MetaPluginImpl>(ptr: *mut ffi::MetaPlugin) -> &'a T {
    let instance = unsafe { &*(ptr as *mut T::Instance) };
    instance.imp()
}

unsafe extern "C" fn vfunc_start<T: MetaPluginImpl>(p: *mut ffi::MetaPlugin) {
    unsafe { imp::<T>(p) }.start();
}
unsafe extern "C" fn vfunc_map<T: MetaPluginImpl>(p: *mut ffi::MetaPlugin, a: *mut ClutterActor) {
    unsafe { imp::<T>(p) }.map(a);
}
unsafe extern "C" fn vfunc_destroy<T: MetaPluginImpl>(p: *mut ffi::MetaPlugin, a: *mut ClutterActor) {
    unsafe { imp::<T>(p) }.destroy(a);
}
unsafe extern "C" fn vfunc_minimize<T: MetaPluginImpl>(p: *mut ffi::MetaPlugin, a: *mut ClutterActor) {
    unsafe { imp::<T>(p) }.minimize(a);
}
unsafe extern "C" fn vfunc_unminimize<T: MetaPluginImpl>(
    p: *mut ffi::MetaPlugin,
    a: *mut ClutterActor,
) {
    unsafe { imp::<T>(p) }.unminimize(a);
}
unsafe extern "C" fn vfunc_kill_window_effects<T: MetaPluginImpl>(
    p: *mut ffi::MetaPlugin,
    a: *mut ClutterActor,
) {
    unsafe { imp::<T>(p) }.kill_window_effects(a);
}

// =============================================================================
// The gnative shell
// =============================================================================

const PANEL_HEIGHT: i32 = 32;
const PANEL_COLOR: CoglColor = CoglColor::rgba(0x16, 0x16, 0x1a, 0xff);
const BACKGROUND_COLOR: CoglColor = CoglColor::rgba(0x24, 0x2a, 0x38, 0xff);
const TEXT_COLOR: CoglColor = CoglColor::rgba(0xf0, 0xf0, 0xf0, 0xff);
const CLOCK_FONT: &std::ffi::CStr = c"Sans Bold 10";
const CLOCK_FORMAT: &str = "%a %e %b  %H:%M";

thread_local! {
    /// The running plugin instance, so `main` can tear the shell down.
    static INSTANCE: RefCell<glib::WeakRef<GnativePlugin>> = RefCell::default();
}

/// Complete pending window effects and destroy the shell actors.
/// Call after the main loop has exited.
pub fn shutdown() {
    if let Some(plugin) = INSTANCE.with(|i| i.borrow().upgrade()) {
        plugin.imp().teardown();
    }
}

#[derive(Clone, Copy, Debug)]
enum Effect {
    Map,
    Destroy,
    Minimize,
    Unminimize,
}

struct Pending {
    actor: glib::Object,
    effect: Effect,
    handler: glib::SignalHandlerId,
}

/// Actors owned by the shell. Raw pointers are owned by the Clutter scene graph
/// (the stage / window group hold the references).
struct Shell {
    display: *mut MetaDisplay,
    panel: *mut ClutterActor,
    clock: *mut ClutterActor,
    background: *mut ClutterActor,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct GnativePlugin {
        pub(super) shell: RefCell<Option<Shell>>,
        pub(super) launcher: RefCell<Option<Rc<Launcher>>>,
        pub(super) switcher: RefCell<Option<Rc<Switcher>>>,
        pub(super) pending: RefCell<HashMap<usize, Pending>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GnativePlugin {
        const NAME: &'static str = "GnativePlugin";
        type Type = super::GnativePlugin;
        type ParentType = super::MetaPlugin;
    }

    impl ObjectImpl for GnativePlugin {}

    impl MetaPluginImpl for GnativePlugin {
        fn start(&self) {
            self.build_shell();
        }

        fn map(&self, actor: *mut ClutterActor) {
            self.animate(actor, Effect::Map);
        }

        fn destroy(&self, actor: *mut ClutterActor) {
            self.animate(actor, Effect::Destroy);
        }

        fn minimize(&self, actor: *mut ClutterActor) {
            self.animate(actor, Effect::Minimize);
        }

        fn unminimize(&self, actor: *mut ClutterActor) {
            self.animate(actor, Effect::Unminimize);
        }

        fn kill_window_effects(&self, actor: *mut ClutterActor) {
            self.finish(actor as usize, true);
        }
    }
}

glib::wrapper! {
    pub struct GnativePlugin(ObjectSubclass<imp::GnativePlugin>) @extends MetaPlugin;
}

impl imp::GnativePlugin {
    fn plugin_ptr(&self) -> *mut ffi::MetaPlugin {
        self.obj().upcast_ref::<MetaPlugin>().as_ptr()
    }

    // ---------------------------------------------------------------- shell UI

    fn build_shell(&self) {
        INSTANCE.with(|i| i.borrow().set(Some(&*self.obj())));
        let display = self.obj().upcast_ref::<MetaPlugin>().display();

        unsafe {
            let compositor = ffi::meta_display_get_compositor(display);
            let stage = ffi::meta_compositor_get_stage(compositor) as *mut ClutterActor;
            let window_group = ffi::meta_compositor_get_window_group(compositor);

            // Solid background below every window. It must live on the stage,
            // not inside window_group: mutter restacks window_group by lowering
            // window actors to the bottom and leaves unknown children in place,
            // which would put a plain actor above all windows after a restack.
            let background = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(background, &BACKGROUND_COLOR);
            ffi::clutter_actor_insert_child_below(stage, background, window_group);

            // Top panel with a centered clock, above all windows.
            let panel = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(panel, &PANEL_COLOR);
            let clock = ffi::clutter_text_new_full(CLOCK_FONT.as_ptr(), c"".as_ptr(), &TEXT_COLOR);
            ffi::clutter_actor_add_child(panel, clock);
            ffi::clutter_actor_add_child(stage, panel);

            *self.shell.borrow_mut() = Some(Shell { display, panel, clock, background });
            *self.launcher.borrow_mut() = Some(Launcher::new(display, stage));
            *self.switcher.borrow_mut() = Some(Switcher::new(display, stage));

            self.connect_signals(display);
            self.update_clock();
            self.relayout();

            ffi::clutter_actor_show(stage);
        }

        let weak = self.obj().downgrade();
        glib::timeout_add_seconds_local(1, move || match weak.upgrade() {
            Some(plugin) => {
                plugin.imp().update_clock();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });

        #[cfg(debug_assertions)]
        {
            self.debug_launch_hook();
            self.debug_switch_hook();
            self.debug_keys_hook();
            self.debug_screenshot_hook();
        }
    }

    /// Debug builds: `GNATIVE_DEBUG_SCREENSHOT=<ms>,<ms>,...:<prefix>` paints the
    /// primary monitor at each time to `<prefix>-<ms>-<W>x<H>.bgra` (raw
    /// premultiplied BGRA). Convert: `magick -size WxH -depth 8 BGRA:f.bgra f.png`.
    #[cfg(debug_assertions)]
    fn debug_screenshot_hook(&self) {
        let Ok(spec) = std::env::var("GNATIVE_DEBUG_SCREENSHOT") else { return };
        let Some((times, prefix)) = spec.split_once(':') else { return };
        let display = self.obj().upcast_ref::<MetaPlugin>().display();
        for ms in times.split(',').filter_map(|t| t.parse::<u64>().ok()) {
            let prefix = prefix.to_string();
            glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || unsafe {
                let Some(r) = crate::ui::primary_monitor_geometry(display) else { return };
                let stride = r.width * 4;
                let mut buf = vec![0u8; (stride * r.height) as usize];
                let stage = ffi::meta_compositor_get_stage(ffi::meta_display_get_compositor(display));
                let mut err = std::ptr::null_mut();
                let ok = ffi::clutter_stage_paint_to_buffer(
                    stage, &r, 1.0, buf.as_mut_ptr(), stride,
                    ffi::COGL_PIXEL_FORMAT_BGRA_8888_PRE, std::ptr::null_mut(),
                    ffi::CLUTTER_PAINT_FLAG_CLEAR, &mut err,
                );
                if ok == 0 {
                    let e: glib::Error = from_glib_full(err);
                    glib::g_warning!("gnative", "screenshot failed: {e}");
                    return;
                }
                // Dump window actor state alongside the screenshot.
                let compositor = ffi::meta_display_get_compositor(display);
                let wg = ffi::meta_compositor_get_window_group(compositor);
                glib::g_message!("gnative", "debug: window_group visible={} mapped={} opacity={}",
                    ffi::clutter_actor_is_visible(wg), ffi::clutter_actor_is_mapped(wg), ffi::clutter_actor_get_paint_opacity(wg));
                let mut node = ffi::meta_compositor_get_window_actors(compositor);
                while !node.is_null() {
                    let a = (*node).data as *mut ClutterActor;
                    glib::g_message!("gnative",
                        "debug: actor {} visible={} mapped={} opacity={} paint_opacity={} parent_is_wg={}",
                        window_desc(ffi::meta_window_actor_get_meta_window(a)),
                        ffi::clutter_actor_is_visible(a), ffi::clutter_actor_is_mapped(a),
                        ffi::clutter_actor_get_opacity(a), ffi::clutter_actor_get_paint_opacity(a),
                        ffi::clutter_actor_get_parent(a) == wg);
                    node = (*node).next;
                }
                let path = format!("{prefix}-{ms}-{}x{}.bgra", r.width, r.height);
                match std::fs::write(&path, &buf) {
                    Ok(()) => glib::g_message!("gnative", "debug: screenshot {path}"),
                    Err(e) => glib::g_warning!("gnative", "screenshot write {path}: {e}"),
                }
            });
        }
    }

    /// Debug builds: `GNATIVE_DEBUG_KEYS=<ms>:<keys>` types `<keys>` through a
    /// virtual keyboard after `<ms>` milliseconds, 80 ms per key. Tokens are
    /// separated by spaces: characters to type, one of `Super`, `Return`,
    /// `Escape`, `Tab`, `Alt+Tab`, or `_` to pause one step. Drives the real
    /// input path headless.
    #[cfg(debug_assertions)]
    fn debug_keys_hook(&self) {
        let Ok(spec) = std::env::var("GNATIVE_DEBUG_KEYS") else { return };
        let Some((delay, keys)) = spec.split_once(':') else { return };
        let Ok(delay) = delay.parse::<u64>() else { return };

        // Each step: list of (keyval, pressed).
        let mut steps: Vec<Vec<(u32, bool)>> = Vec::new();
        for tok in keys.split(' ').filter(|t| !t.is_empty()) {
            let tap = |k: u32| vec![(k, true), (k, false)];
            steps.push(match tok {
                "Super" => tap(0xffeb),
                "Return" => tap(crate::ui::KEY_RETURN),
                "Escape" => tap(crate::ui::KEY_ESCAPE),
                "Tab" => tap(crate::ui::KEY_TAB),
                "Alt+Tab" => vec![(0xffe9, true), (0xff09, true), (0xff09, false), (0xffe9, false)],
                "_" => vec![], // pause one step
                t => t.chars().flat_map(|c| tap(c as u32)).collect(),
            });
        }

        let display = self.obj().upcast_ref::<MetaPlugin>().display();
        let device: glib::Object = unsafe {
            let stage = ffi::meta_compositor_get_stage(ffi::meta_display_get_compositor(display));
            let ctx = ffi::clutter_actor_get_context(stage as *mut ClutterActor);
            let seat = ffi::clutter_backend_get_default_seat(ffi::clutter_context_get_backend(ctx));
            from_glib_full(ffi::clutter_seat_create_virtual_device(seat, ffi::CLUTTER_KEYBOARD_DEVICE)
                as *mut gobject_sys::GObject)
        };

        let mut steps = steps.into_iter();
        glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
            glib::timeout_add_local(std::time::Duration::from_millis(80), move || {
                let Some(step) = steps.next() else { return glib::ControlFlow::Break };
                for (keyval, pressed) in step {
                    let state = if pressed { ffi::CLUTTER_KEY_STATE_PRESSED } else { ffi::CLUTTER_KEY_STATE_RELEASED };
                    unsafe {
                        ffi::clutter_virtual_input_device_notify_keyval(
                            device.as_ptr() as *mut ffi::ClutterVirtualInputDevice,
                            glib::monotonic_time() as u64,
                            keyval,
                            state,
                        );
                    }
                }
                glib::g_debug!("gnative", "debug: typed step, focus = {}", window_desc(unsafe {
                    ffi::meta_display_get_focus_window(display)
                }));
                glib::ControlFlow::Continue
            });
        });
    }

    /// Debug builds: `GNATIVE_DEBUG_SWITCH=<ms>` runs Alt+Tab programmatically
    /// after `<ms>` milliseconds (start, show popup, release) and logs focus.
    #[cfg(debug_assertions)]
    fn debug_switch_hook(&self) {
        let Some(delay) = std::env::var("GNATIVE_DEBUG_SWITCH").ok().and_then(|v| v.parse().ok()) else {
            return;
        };
        let Some(switcher) = self.switcher.borrow().clone() else { return };
        let display = self.obj().upcast_ref::<MetaPlugin>().display();
        glib::timeout_add_local_once(std::time::Duration::from_millis(delay), move || {
            glib::g_message!("gnative", "debug: focus before: {}", focus_title(display));
            switcher.start(false);
            glib::g_message!("gnative", "debug: switcher active={}", switcher.is_active());
            glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
                switcher.finish();
                glib::g_message!(
                    "gnative",
                    "debug: switcher active={} focus after: {}",
                    switcher.is_active(),
                    focus_title(display)
                );
            });
        });
    }

    /// Debug builds: `GNATIVE_DEBUG_LAUNCH=<query>` opens the launcher shortly
    /// after startup, types `<query>` and launches the top match. Exercises the
    /// launcher end-to-end without real input devices (e.g. `--headless`).
    #[cfg(debug_assertions)]
    fn debug_launch_hook(&self) {
        let Ok(query) = std::env::var("GNATIVE_DEBUG_LAUNCH") else { return };
        let Some(launcher) = self.launcher.borrow().clone() else { return };
        glib::timeout_add_local_once(std::time::Duration::from_millis(1000), move || {
            launcher.show();
            launcher.set_query(&query);
            glib::g_message!("gnative", "debug: launcher visible={} query={query:?}", launcher.is_visible());
            glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                launcher.launch_selected();
                glib::g_message!("gnative", "debug: launcher visible={}", launcher.is_visible());
            });
        });
    }

    unsafe fn connect_signals(&self, display: *mut MetaDisplay) {
        let display_obj = unsafe { gobj(display) };
        let context = unsafe { ffi::meta_display_get_context(display) };
        let backend = unsafe { ffi::meta_context_get_backend(context) };
        let monitor_manager = unsafe { gobj(ffi::meta_backend_get_monitor_manager(backend)) };
        let workspace_manager = unsafe { gobj(ffi::meta_display_get_workspace_manager(display)) };

        // Diagnostics: G_MESSAGES_DEBUG=gnative
        for sig in ["window-created", "window-demands-attention", "window-marked-urgent"] {
            display_obj.connect_local(sig, false, move |args| {
                let w = args.get(1).and_then(|v| v.get::<glib::Object>().ok());
                let desc = w.map(|w| window_desc(w.as_ptr() as *mut ffi::MetaWindow)).unwrap_or_default();
                glib::g_debug!("gnative", "{sig}: {desc}");
                None
            });
        }

        display_obj.connect_notify_local(Some("focus-window"), move |_, _| {
            glib::g_debug!("gnative", "focus -> {}", unsafe {
                window_desc(ffi::meta_display_get_focus_window(display))
            });
        });

        let weak = self.obj().downgrade();
        let relayout = move |_: &[glib::Value]| {
            if let Some(p) = weak.upgrade() {
                p.imp().relayout();
            }
            None
        };
        monitor_manager.connect_local("monitors-changed", false, relayout.clone());
        workspace_manager.connect_local("workspace-added", false, relayout.clone());
        display_obj.connect_local("in-fullscreen-changed", false, relayout);

        // Super key: toggle the built-in launcher, or run $GNATIVE_LAUNCHER.
        let weak = self.obj().downgrade();
        display_obj.connect_local("overlay-key", false, move |_| {
            if let Ok(cmd) = std::env::var("GNATIVE_LAUNCHER") {
                if let Err(e) = glib::spawn_command_line_async(cmd.as_str()) {
                    glib::g_warning!("gnative", "failed to spawn launcher '{cmd}': {e}");
                }
            } else if let Some(p) = weak.upgrade() {
                let launcher = p.imp().launcher.borrow().clone();
                if let Some(l) = launcher {
                    l.toggle();
                }
            }
            None
        });
    }

    fn update_clock(&self) {
        let shell = self.shell.borrow();
        let Some(shell) = shell.as_ref() else { return };
        let text = glib::DateTime::now_local()
            .and_then(|dt| dt.format(CLOCK_FORMAT))
            .map(|s| s.to_string())
            .unwrap_or_default();
        let text = CString::new(text).unwrap_or_default();
        unsafe {
            ffi::clutter_text_set_text(shell.clock as *mut ffi::ClutterText, text.as_ptr());
            center_in_panel(shell.clock, shell.panel);
        }
    }

    fn relayout(&self) {
        let shell = self.shell.borrow();
        let Some(shell) = shell.as_ref() else { return };
        let display = shell.display;

        unsafe {
            let Some(geo) = crate::ui::primary_monitor_geometry(display) else { return };
            let primary = ffi::meta_display_get_primary_monitor(display);

            // Background covers the bounding box of all monitors.
            let n_monitors = ffi::meta_display_get_n_monitors(display);
            let mut bounds: Option<MtkRectangle> = None;
            for i in 0..n_monitors {
                let mut r = MtkRectangle::default();
                ffi::meta_display_get_monitor_geometry(display, i, &mut r);
                bounds = Some(match bounds {
                    None => r,
                    Some(b) => union(b, r),
                });
            }
            if let Some(b) = bounds {
                ffi::clutter_actor_set_position(shell.background, b.x as f32, b.y as f32);
                ffi::clutter_actor_set_size(shell.background, b.width as f32, b.height as f32);
            }

            // Panel sits on top of the primary monitor.
            ffi::clutter_actor_set_position(shell.panel, geo.x as f32, geo.y as f32);
            ffi::clutter_actor_set_size(shell.panel, geo.width as f32, PANEL_HEIGHT as f32);
            center_in_panel(shell.clock, shell.panel);

            if ffi::meta_display_get_monitor_in_fullscreen(display, primary) != 0 {
                ffi::clutter_actor_hide(shell.panel);
            } else {
                ffi::clutter_actor_show(shell.panel);
            }

            // Reserve the panel area so maximized windows don't cover it.
            let mut strut = ffi::MetaStrut {
                rect: MtkRectangle { x: geo.x, y: geo.y, width: geo.width, height: PANEL_HEIGHT },
                side: ffi::META_SIDE_TOP,
            };
            let list = glib_sys::g_slist_prepend(
                std::ptr::null_mut(),
                &mut strut as *mut _ as glib_sys::gpointer,
            );
            let wm = ffi::meta_display_get_workspace_manager(display);
            for i in 0..ffi::meta_workspace_manager_get_n_workspaces(wm) {
                let ws = ffi::meta_workspace_manager_get_workspace_by_index(wm, i);
                ffi::meta_workspace_set_builtin_struts(ws, list); // copies the list
            }
            glib_sys::g_slist_free(list);
        }
    }

    fn teardown(&self) {
        let keys: Vec<usize> = self.pending.borrow().keys().copied().collect();
        for key in keys {
            self.finish(key, true);
        }
        if let Some(launcher) = self.launcher.borrow_mut().take() {
            launcher.destroy();
        }
        if let Some(switcher) = self.switcher.borrow_mut().take() {
            switcher.destroy();
        }
        let Some(shell) = self.shell.borrow_mut().take() else { return };
        unsafe {
            ffi::clutter_actor_destroy(shell.panel); // also destroys the clock
            ffi::clutter_actor_destroy(shell.background);
        }
    }

    // ---------------------------------------------------------------- window effects

    fn animate(&self, actor: *mut ClutterActor, effect: Effect) {
        let key = actor as usize;
        glib::g_debug!("gnative", "effect {effect:?}: {}", unsafe {
            window_desc(ffi::meta_window_actor_get_meta_window(actor))
        });

        // A new effect on the same actor supersedes the old one.
        self.finish(key, true);

        let animatable = unsafe {
            let window = ffi::meta_window_actor_get_meta_window(actor);
            !window.is_null() && ffi::meta_window_get_window_type(window) == ffi::META_WINDOW_NORMAL
        };
        if !animatable {
            unsafe { complete(self.plugin_ptr(), actor, effect) };
            return;
        }

        let (duration, from, to): (u32, Option<(u8, f64)>, (u8, f64)) = match effect {
            Effect::Map => (180, Some((0, 0.94)), (255, 1.0)),
            Effect::Unminimize => (200, Some((0, 0.85)), (255, 1.0)),
            Effect::Destroy => (140, None, (0, 0.94)),
            Effect::Minimize => (180, None, (0, 0.85)),
        };

        unsafe {
            ffi::clutter_actor_set_pivot_point(actor, 0.5, 0.5);
            if let Some((opacity, scale)) = from {
                ffi::clutter_actor_set_opacity(actor, opacity);
                ffi::clutter_actor_set_scale(actor, scale, scale);
            }
            if matches!(effect, Effect::Unminimize) {
                ffi::clutter_actor_show(actor);
            }

            ffi::clutter_actor_save_easing_state(actor);
            ffi::clutter_actor_set_easing_mode(actor, ffi::CLUTTER_EASE_OUT_CUBIC);
            ffi::clutter_actor_set_easing_duration(actor, duration);
            ffi::clutter_actor_set_opacity(actor, to.0);
            ffi::clutter_actor_set_scale(actor, to.1, to.1);
            ffi::clutter_actor_restore_easing_state(actor);
        }

        let actor_obj = unsafe { gobj(actor) };
        let weak = self.obj().downgrade();
        let handler = actor_obj.connect_local("transitions-completed", false, move |_| {
            if let Some(p) = weak.upgrade() {
                p.imp().finish(key, false);
            }
            None
        });
        self.pending.borrow_mut().insert(key, Pending { actor: actor_obj, effect, handler });

        // If Clutter created no transition (value already at target, or
        // animations disabled), no signal will come: finish right away.
        let has_transition =
            unsafe { !ffi::clutter_actor_get_transition(actor, c"opacity".as_ptr()).is_null() };
        if !has_transition {
            self.finish(key, false);
        }
    }

    /// Complete the pending effect for `key`, if any. Safe to call repeatedly.
    fn finish(&self, key: usize, stop_transitions: bool) {
        let Some(pending) = self.pending.borrow_mut().remove(&key) else { return };
        pending.actor.disconnect(pending.handler);
        let actor = pending.actor.as_ptr() as *mut ClutterActor;

        unsafe {
            if stop_transitions {
                ffi::clutter_actor_remove_all_transitions(actor);
            }
            match pending.effect {
                Effect::Destroy => {}
                Effect::Minimize => {
                    ffi::clutter_actor_hide(actor);
                    reset(actor);
                }
                Effect::Map | Effect::Unminimize => reset(actor),
            }
            complete(self.plugin_ptr(), actor, pending.effect);
        }
    }
}

#[cfg(debug_assertions)]
fn focus_title(display: *mut MetaDisplay) -> String {
    unsafe {
        let w = ffi::meta_display_get_focus_window(display);
        if w.is_null() {
            return "<none>".into();
        }
        let t = ffi::meta_window_get_title(w);
        let title = if t.is_null() { String::new() } else { std::ffi::CStr::from_ptr(t).to_string_lossy().into_owned() };
        format!("{title} (id {})", ffi::meta_window_get_id(w))
    }
}

/// "title [id N, type T]" for logging.
fn window_desc(w: *mut ffi::MetaWindow) -> String {
    if w.is_null() {
        return "<no window>".into();
    }
    unsafe {
        let t = ffi::meta_window_get_title(w);
        let title = if t.is_null() { String::new() } else { std::ffi::CStr::from_ptr(t).to_string_lossy().into_owned() };
        let mut r = MtkRectangle::default();
        ffi::meta_window_get_frame_rect(w, &mut r);
        format!(
            "{title:?} [id {}, type {}, {}x{}+{}+{}]",
            ffi::meta_window_get_id(w),
            ffi::meta_window_get_window_type(w),
            r.width, r.height, r.x, r.y
        )
    }
}

unsafe fn reset(actor: *mut ClutterActor) {
    unsafe {
        ffi::clutter_actor_set_opacity(actor, 255);
        ffi::clutter_actor_set_scale(actor, 1.0, 1.0);
    }
}

unsafe fn complete(plugin: *mut ffi::MetaPlugin, actor: *mut ClutterActor, effect: Effect) {
    unsafe {
        match effect {
            Effect::Map => ffi::meta_plugin_map_completed(plugin, actor),
            Effect::Destroy => ffi::meta_plugin_destroy_completed(plugin, actor),
            Effect::Minimize => ffi::meta_plugin_minimize_completed(plugin, actor),
            Effect::Unminimize => ffi::meta_plugin_unminimize_completed(plugin, actor),
        }
    }
}

unsafe fn center_in_panel(child: *mut ClutterActor, panel: *mut ClutterActor) {
    unsafe {
        let x = (ffi::clutter_actor_get_width(panel) - ffi::clutter_actor_get_width(child)) / 2.0;
        let y = (PANEL_HEIGHT as f32 - ffi::clutter_actor_get_height(child)) / 2.0;
        ffi::clutter_actor_set_position(child, x.floor(), y.floor());
    }
}

fn union(a: MtkRectangle, b: MtkRectangle) -> MtkRectangle {
    let x1 = a.x.min(b.x);
    let y1 = a.y.min(b.y);
    let x2 = (a.x + a.width).max(b.x + b.width);
    let y2 = (a.y + a.height).max(b.y + b.height);
    MtkRectangle { x: x1, y: y1, width: x2 - x1, height: y2 - y1 }
}
