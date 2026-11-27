//! Alt+Tab / Super+Tab window switcher.
//!
//! Takes over mutter's `switch-applications` and `switch-windows` keybindings
//! (and their `-backward` variants) and shows the window tiles of the active
//! workspace (see `overview`). While the modifier is held, Tab / arrows move
//! the selection; releasing the modifier activates it, Escape cancels. The
//! tiles appear only after a short delay so a quick Alt+Tab just flips to the
//! previous window without flashing UI.

use crate::ffi::{self, ClutterActor, MetaDisplay, MetaWindow};
use crate::overview::{Kind, Overview};
use crate::windows;
use std::cell::RefCell;
use std::ffi::CStr;
use std::rc::{Rc, Weak};
use std::time::Duration;

const POPUP_DELAY: Duration = Duration::from_millis(120);

const BINDINGS: [&CStr; 4] = [
    c"switch-applications",
    c"switch-applications-backward",
    c"switch-windows",
    c"switch-windows-backward",
];

pub struct Switcher {
    display: *mut MetaDisplay,
    tiles: Rc<Overview>,
    /// Runs until the tiles are painted.
    delay: RefCell<Option<glib::SourceId>>,
}

impl Switcher {
    pub fn new(display: *mut MetaDisplay, stage: *mut ClutterActor) -> Rc<Self> {
        let tiles = Overview::new(display, stage, Kind::Switcher);
        let switcher = Rc::new(Self { display, tiles, delay: RefCell::new(None) });
        switcher.install_keybindings();
        switcher
    }

    fn install_keybindings(self: &Rc<Self>) {
        for name in BINDINGS {
            // Each handler owns a Weak; mutter frees it if the handler is replaced.
            let data = Weak::into_raw(Rc::downgrade(self)) as glib_sys::gpointer;
            let ok = unsafe {
                ffi::meta_keybindings_set_custom_handler(
                    name.as_ptr(),
                    Some(on_switch_binding),
                    data,
                    Some(drop_weak),
                )
            };
            if ok == 0 {
                unsafe { drop_weak(data) };
                glib::g_warning!("gnutshell", "could not take over keybinding {name:?}");
            }
        }
    }

    pub fn is_active(&self) -> bool {
        self.tiles.is_visible()
    }

    /// Start switching (or advance, if already active).
    pub fn start(self: &Rc<Self>, backward: bool) {
        if self.is_active() {
            self.tiles.cycle(if backward { -1 } else { 1 });
            return;
        }

        let list = self.tiles.windows();
        let select = match list.len() {
            0 => return,
            // Nothing to switch to; still bring back a minimized window.
            1 => return windows::activate(self.display, &list[0]),
            n => &list[if backward { n - 1 } else { 1 }],
        };

        self.cancel_delay();
        self.tiles.set_painted(false);
        self.tiles.present(Some(windows::id(select)));

        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(POPUP_DELAY, move || {
            if let Some(s) = weak.upgrade() {
                s.delay.borrow_mut().take();
                s.tiles.set_painted(true);
            }
        });
        *self.delay.borrow_mut() = Some(id);
    }

    /// Activate the selected window and close, as releasing the modifier
    /// does (used by the debug hook).
    #[cfg(debug_assertions)]
    pub fn finish(&self) {
        self.cancel_delay();
        self.tiles.activate_selected();
    }

    pub fn hide(&self) {
        self.cancel_delay();
        self.tiles.hide();
    }

    /// Windows or monitors changed.
    pub fn refresh(&self) {
        self.tiles.refresh();
    }

    fn cancel_delay(&self) {
        let id = self.delay.borrow_mut().take();
        if let Some(id) = id {
            id.remove();
        }
    }

    pub fn destroy(&self) {
        self.cancel_delay();
        self.tiles.destroy();
    }
}

unsafe extern "C" fn on_switch_binding(
    _display: *mut MetaDisplay,
    _window: *mut MetaWindow,
    _event: *const ffi::ClutterEvent,
    binding: *mut ffi::MetaKeyBinding,
    data: glib_sys::gpointer,
) {
    // `data` is a `Weak::into_raw` pointer: rebuild without taking ownership.
    let weak = unsafe { Weak::from_raw(data as *const Switcher) };
    let backward = unsafe { ffi::meta_key_binding_is_reversed(binding) } != 0;
    if let Some(s) = weak.upgrade() {
        s.start(backward);
    }
    std::mem::forget(weak);
}

unsafe extern "C" fn drop_weak(data: glib_sys::gpointer) {
    drop(unsafe { Weak::from_raw(data as *const Switcher) });
}
