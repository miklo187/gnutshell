//! Keyboard layouts from `org.gnome.desktop.input-sources`.
//!
//! In GNOME, gnome-shell (not gnome-settings-daemon) turns the configured
//! input sources into an XKB keymap. gnutshell does the same: all `xkb` sources
//! (up to 4) go into one keymap, and the active one is a layout index.
//! Super+Space (`switch-input-source`) cycles; the most recently used source
//! is stored in `mru-sources`, like gnome-shell does, so it survives restarts.
//! A panel indicator shows the active layout and cycles on click.
//! IBus input methods (non-`xkb` sources) are ignored.

use crate::ffi::{self, ClutterActor, MetaDisplay};
use crate::ui::{self, gobj};
use gio::prelude::*;
use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_char};
use std::rc::{Rc, Weak};

const SCHEMA: &str = "org.gnome.desktop.input-sources";
const MAX_LAYOUTS: usize = 4;
const INDICATOR_FONT: &CStr = c"Sans Bold 10";
const HOVER_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x20);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Input source id, e.g. `de` or `us+intl`.
    pub id: String,
    pub layout: String,
    pub variant: String,
}

impl Source {
    fn from_id(id: &str) -> Self {
        let (layout, variant) = id.split_once('+').unwrap_or((id, ""));
        Self { id: id.to_owned(), layout: layout.to_owned(), variant: variant.to_owned() }
    }
}

/// The `xkb` entries of an `a(ss)` source list, at most `MAX_LAYOUTS`.
pub fn xkb_sources(list: &[(String, String)]) -> Vec<Source> {
    list.iter()
        .filter(|(kind, id)| kind == "xkb" && !id.is_empty())
        .take(MAX_LAYOUTS)
        .map(|(_, id)| Source::from_id(id))
        .collect()
}

/// Index of the most recently used source that is still configured.
pub fn current_index(sources: &[Source], mru: &[(String, String)]) -> usize {
    mru.iter()
        .filter(|(kind, _)| kind == "xkb")
        .find_map(|(_, id)| sources.iter().position(|s| &s.id == id))
        .unwrap_or(0)
}

/// `mru` with `id` moved (or added) to the front.
pub fn bump_mru(mru: &[(String, String)], id: &str) -> Vec<(String, String)> {
    let mut out = vec![("xkb".to_owned(), id.to_owned())];
    out.extend(mru.iter().filter(|(kind, i)| !(kind == "xkb" && i == id)).cloned());
    out
}

pub struct InputSources {
    display: *mut MetaDisplay,
    settings: gio::Settings,
    indicator: *mut ClutterActor,
    label: *mut ClutterActor,
    panel_height: f32,
    state: RefCell<State>,
    width: Cell<f32>,
    width_handler: RefCell<Option<Rc<dyn Fn()>>>,
}

struct State {
    sources: Vec<Source>,
    current: usize,
    /// (layouts, variants, options) of the keymap in `desc`.
    rules: Option<(String, String, String)>,
    desc: *mut ffi::MetaKeymapDescription,
    right_edge: f32,
}

impl InputSources {
    /// `None` when the input-sources schema isn't installed.
    pub fn new(display: *mut MetaDisplay, panel: *mut ClutterActor, panel_height: f32) -> Option<Rc<Self>> {
        let has_schema = gio::SettingsSchemaSource::default()
            .and_then(|src| src.lookup(SCHEMA, true))
            .is_some();
        if !has_schema {
            glib::g_warning!("gnutshell", "{SCHEMA} not installed; keeping the default keymap");
            return None;
        }
        let this = unsafe {
            let indicator = ffi::clutter_actor_new();
            ffi::clutter_actor_set_reactive(indicator, 1);
            let label = ui::label(INDICATOR_FONT, "", &ui::TEXT_COLOR);
            ffi::clutter_actor_add_child(indicator, label);
            ffi::clutter_actor_add_child(panel, indicator);
            ffi::clutter_actor_hide(indicator);
            Rc::new(Self {
                display,
                settings: gio::Settings::new(SCHEMA),
                indicator,
                label,
                panel_height,
                state: RefCell::new(State {
                    sources: Vec::new(),
                    current: 0,
                    rules: None,
                    desc: std::ptr::null_mut(),
                    right_edge: 0.0,
                }),
                width: Cell::default(),
                width_handler: RefCell::default(),
            })
        };
        this.connect_signals();
        this.install_keybindings();
        this.reload();
        Some(this)
    }

    fn connect_signals(self: &Rc<Self>) {
        for key in ["sources", "xkb-options", "mru-sources"] {
            let weak = Rc::downgrade(self);
            self.settings.connect_changed(Some(key), move |_, _| with(&weak, |s| s.reload()));
        }

        let indicator = unsafe { gobj(self.indicator) };
        let weak = Rc::downgrade(self);
        indicator.connect_local("button-press-event", false, move |_| {
            with(&weak, |s| s.switch(1));
            Some(true.to_value())
        });
        let i = self.indicator;
        indicator.connect_local("enter-event", false, move |_| {
            unsafe { ffi::clutter_actor_set_background_color(i, &HOVER_COLOR) };
            Some(false.to_value())
        });
        indicator.connect_local("leave-event", false, move |_| {
            unsafe { ffi::clutter_actor_set_background_color(i, &ui::TRANSPARENT) };
            Some(false.to_value())
        });
    }

    /// gnome-shell, not mutter, registers these bindings; add them the same way.
    fn install_keybindings(self: &Rc<Self>) {
        let wm_settings = gio::Settings::new("org.gnome.desktop.wm.keybindings");
        for (name, flags) in [
            (c"switch-input-source", 0),
            (c"switch-input-source-backward", ffi::META_KEY_BINDING_IS_REVERSED),
        ] {
            let data = Weak::into_raw(Rc::downgrade(self)) as glib_sys::gpointer;
            let action = unsafe {
                ffi::meta_display_add_keybinding(
                    self.display,
                    name.as_ptr(),
                    wm_settings.as_ptr(),
                    flags,
                    Some(on_switch_binding),
                    data,
                    Some(drop_weak),
                )
            };
            if action == 0 {
                unsafe { drop_weak(data) };
                glib::g_warning!("gnutshell", "could not add keybinding {name:?}");
            }
        }
    }

    /// Cycle the active layout by `delta`.
    pub fn switch(&self, delta: isize) {
        let (id, mru) = {
            let mut state = self.state.borrow_mut();
            let n = state.sources.len();
            if n < 2 {
                return;
            }
            state.current = (state.current as isize + delta).rem_euclid(n as isize) as usize;
            (state.sources[state.current].id.clone(), self.mru())
        };
        self.apply();
        // Remember it like gnome-shell does. The resulting `changed` signal
        // reloads to the same state, which `apply` recognizes as a no-op.
        let mru = bump_mru(&mru, &id);
        if let Err(e) = self.settings.set_value("mru-sources", &mru.to_variant()) {
            glib::g_warning!("gnutshell", "could not store mru-sources: {e}");
        }
    }

    /// `handler` runs when the indicator's width changes.
    pub fn set_width_handler(&self, handler: impl Fn() + 'static) {
        *self.width_handler.borrow_mut() = Some(Rc::new(handler));
    }

    /// How much of the panel the indicator takes.
    pub fn width(&self) -> f32 {
        self.width.get()
    }

    /// Place the indicator so its right edge is `right_edge` px from the
    /// panel's left.
    pub fn place(&self, right_edge: f32) {
        self.state.borrow_mut().right_edge = right_edge;
        self.update_indicator();
    }

    pub fn destroy(&self) {
        unsafe {
            for name in [c"switch-input-source", c"switch-input-source-backward"] {
                ffi::meta_display_remove_keybinding(self.display, name.as_ptr());
            }
            ffi::clutter_actor_destroy(self.indicator);
        }
        let desc = std::mem::replace(&mut self.state.borrow_mut().desc, std::ptr::null_mut());
        if !desc.is_null() {
            unsafe { ffi::meta_keymap_description_unref(desc) };
        }
    }

    // ------------------------------------------------------------------ internals

    fn mru(&self) -> Vec<(String, String)> {
        self.settings.value("mru-sources").get().unwrap_or_default()
    }

    fn reload(&self) {
        let list: Vec<(String, String)> = self.settings.value("sources").get().unwrap_or_default();
        let sources = xkb_sources(&list);
        let current = current_index(&sources, &self.mru());
        {
            let mut state = self.state.borrow_mut();
            state.sources = sources;
            state.current = current;
        }
        self.apply();
    }

    fn apply(&self) {
        let (sources, current) = {
            let state = self.state.borrow();
            (state.sources.clone(), state.current)
        };
        self.update_indicator();
        if sources.is_empty() {
            return; // nothing configured: keep mutter's default keymap
        }
        let options: String = self.settings.strv("xkb-options").iter().map(|s| s.as_str()).collect::<Vec<_>>().join(",");
        let layouts = sources.iter().map(|s| s.layout.as_str()).collect::<Vec<_>>().join(",");
        let variants = sources.iter().map(|s| s.variant.as_str()).collect::<Vec<_>>().join(",");
        let rules = (layouts, variants, options);

        let desc = {
            let mut state = self.state.borrow_mut();
            if state.rules.as_ref() != Some(&rules) || state.desc.is_null() {
                let c = |s: &str| CString::new(s).unwrap_or_default();
                let (layouts, variants, options) = (c(&rules.0), c(&rules.1), c(&rules.2));
                let names: Vec<String> = sources.iter().map(|s| s.id.clone()).collect();
                let short: Vec<String> = sources.iter().map(|s| s.layout.clone()).collect();
                let names = glib::StrV::from(names);
                let short = glib::StrV::from(short);
                let desc = unsafe {
                    ffi::meta_keymap_description_new_from_rules(
                        c"".as_ptr(),
                        layouts.as_ptr(),
                        variants.as_ptr(),
                        options.as_ptr(),
                        names.as_ptr() as *mut *mut c_char,
                        short.as_ptr() as *mut *mut c_char,
                    )
                };
                if !state.desc.is_null() {
                    unsafe { ffi::meta_keymap_description_unref(state.desc) };
                }
                state.desc = desc;
                state.rules = Some(rules.clone());
            }
            state.desc
        };
        glib::g_debug!(
            "gnutshell",
            "keymap: layouts={} variants={} options={} index={current}",
            rules.0,
            rules.1,
            rules.2
        );
        unsafe {
            let backend = ffi::meta_context_get_backend(ffi::meta_display_get_context(self.display));
            ffi::meta_backend_set_keymap_async(
                backend,
                desc,
                current as u32,
                std::ptr::null_mut(),
                Some(on_keymap_set),
                std::ptr::null_mut(),
            );
        }
    }

    fn update_indicator(&self) {
        let width = self.draw_indicator();
        if self.width.replace(width) != width {
            let handler = self.width_handler.borrow().clone();
            if let Some(handler) = handler {
                handler();
            }
        }
    }

    /// Returns the width taken.
    fn draw_indicator(&self) -> f32 {
        let state = self.state.borrow();
        unsafe {
            if state.sources.len() < 2 {
                ffi::clutter_actor_hide(self.indicator);
                return 0.0;
            }
            let text = state.sources.get(state.current).map_or("", |s| s.layout.as_str());
            ui::set_text(self.label, text);
            let label_w = ffi::clutter_actor_get_width(self.label);
            let width = (label_w + 20.0).ceil().max(32.0);
            ffi::clutter_actor_set_size(self.indicator, width, self.panel_height);
            ffi::clutter_actor_set_position(self.indicator, state.right_edge - width, 0.0);
            let label_h = ffi::clutter_actor_get_height(self.label);
            ffi::clutter_actor_set_position(
                self.label,
                ((width - label_w) / 2.0).floor(),
                ((self.panel_height - label_h) / 2.0).floor(),
            );
            ffi::clutter_actor_show(self.indicator);
            width
        }
    }
}

unsafe extern "C" fn on_keymap_set(
    source: *mut gobject_sys::GObject,
    result: *mut gio::ffi::GAsyncResult,
    _data: glib_sys::gpointer,
) {
    let mut err = std::ptr::null_mut();
    let ok = unsafe { ffi::meta_backend_set_keymap_finish(source as *mut ffi::MetaBackend, result, &mut err) };
    if ok == 0 {
        let e: glib::Error = unsafe { glib::translate::from_glib_full(err) };
        glib::g_warning!("gnutshell", "could not set keymap: {e}");
    }
}

fn with(weak: &Weak<InputSources>, f: impl FnOnce(&Rc<InputSources>)) {
    if let Some(s) = weak.upgrade() {
        f(&s);
    }
}

unsafe extern "C" fn on_switch_binding(
    _display: *mut MetaDisplay,
    _window: *mut ffi::MetaWindow,
    _event: *const ffi::ClutterEvent,
    binding: *mut ffi::MetaKeyBinding,
    data: glib_sys::gpointer,
) {
    // `data` is a `Weak::into_raw` pointer: rebuild without taking ownership.
    let weak = unsafe { Weak::from_raw(data as *const InputSources) };
    let backward = unsafe { ffi::meta_key_binding_is_reversed(binding) } != 0;
    if let Some(s) = weak.upgrade() {
        s.switch(if backward { -1 } else { 1 });
    }
    std::mem::forget(weak);
}

unsafe extern "C" fn drop_weak(data: glib_sys::gpointer) {
    drop(unsafe { Weak::from_raw(data as *const InputSources) });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn parses_xkb_sources_and_variants() {
        let s = xkb_sources(&pairs(&[("xkb", "de"), ("ibus", "anthy"), ("xkb", "us+intl")]));
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].layout.as_str(), s[0].variant.as_str()), ("de", ""));
        assert_eq!((s[1].layout.as_str(), s[1].variant.as_str()), ("us", "intl"));
    }

    #[test]
    fn caps_at_four_layouts() {
        let many = pairs(&[("xkb", "a"), ("xkb", "b"), ("xkb", "c"), ("xkb", "d"), ("xkb", "e")]);
        assert_eq!(xkb_sources(&many).len(), 4);
    }

    #[test]
    fn current_from_mru() {
        let s = xkb_sources(&pairs(&[("xkb", "de"), ("xkb", "sy")]));
        assert_eq!(current_index(&s, &pairs(&[("xkb", "sy"), ("xkb", "de")])), 1);
        assert_eq!(current_index(&s, &pairs(&[("xkb", "fr"), ("xkb", "de")])), 0);
        assert_eq!(current_index(&s, &[]), 0);
    }

    #[test]
    fn bump_moves_to_front() {
        let mru = pairs(&[("xkb", "de"), ("ibus", "x"), ("xkb", "sy")]);
        assert_eq!(bump_mru(&mru, "sy"), pairs(&[("xkb", "sy"), ("xkb", "de"), ("ibus", "x")]));
    }
}
