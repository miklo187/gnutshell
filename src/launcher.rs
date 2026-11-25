//! Modal app launcher: Super opens a search field over a dimmed screen.
//!
//! Type to filter installed applications (.desktop files). Up/Down/Tab select,
//! Enter launches, Escape / Super / a click outside the box closes.

use crate::ffi::{self, ClutterActor, ClutterText, MetaDisplay};
use crate::icons;
use crate::ui::{
    self, gobj, BOX_COLOR, BOX_PADDING, DIM_COLOR, HINT_COLOR, KEY_DOWN, KEY_ESCAPE,
    KEY_ISO_LEFT_TAB, KEY_TAB, KEY_UP, ROW_HEIGHT, TEXT_COLOR,
};
use gio::prelude::*;
use glib::translate::*;
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::rc::{Rc, Weak};

const MAX_RESULTS: usize = 8;
const BOX_WIDTH: f32 = 600.0;
const ENTRY_HEIGHT: f32 = 40.0;
const ENTRY_FONT: &CStr = c"Sans 16";

pub struct Launcher {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    overlay: *mut ClutterActor,
    panel: *mut ClutterActor,
    entry: *mut ClutterActor,
    hint: *mut ClutterActor,
    results: *mut ClutterActor,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    apps: Vec<gio::AppInfo>,
    matches: Vec<usize>,
    selected: usize,
    grab: Option<*mut ffi::ClutterGrab>,
}

impl Launcher {
    /// Build the (hidden) launcher UI as the topmost child of `stage`.
    pub fn new(display: *mut MetaDisplay, stage: *mut ClutterActor) -> Rc<Self> {
        let launcher = unsafe {
            let overlay = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(overlay, &DIM_COLOR);
            ffi::clutter_actor_set_reactive(overlay, 1);
            ffi::clutter_actor_hide(overlay);

            let panel = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(panel, &BOX_COLOR);
            ffi::clutter_actor_set_reactive(panel, 1);
            ffi::clutter_actor_add_child(overlay, panel);

            let hint = ffi::clutter_text_new_full(
                ENTRY_FONT.as_ptr(),
                c"Type to search apps".as_ptr(),
                &HINT_COLOR,
            );
            ffi::clutter_actor_add_child(panel, hint);

            let entry = ffi::clutter_text_new_full(ENTRY_FONT.as_ptr(), c"".as_ptr(), &TEXT_COLOR);
            let text = entry as *mut ClutterText;
            ffi::clutter_text_set_editable(text, 1);
            ffi::clutter_text_set_activatable(text, 1);
            ffi::clutter_text_set_single_line_mode(text, 1);
            ffi::clutter_text_set_cursor_color(text, &TEXT_COLOR);
            ffi::clutter_actor_set_reactive(entry, 1);
            ffi::clutter_actor_add_child(panel, entry);

            let results = ffi::clutter_actor_new();
            ffi::clutter_actor_add_child(panel, results);

            ffi::clutter_actor_add_child(stage, overlay);

            Rc::new(Self {
                display,
                stage,
                overlay,
                panel,
                entry,
                hint,
                results,
                state: RefCell::default(),
            })
        };
        launcher.connect_signals();
        launcher
    }

    fn connect_signals(self: &Rc<Self>) {
        let entry = unsafe { gobj(self.entry) };
        let overlay = unsafe { gobj(self.overlay) };
        let panel = unsafe { gobj(self.panel) };

        let weak = Rc::downgrade(self);
        entry.connect_local("text-changed", false, move |_| {
            with(&weak, |l| l.refilter());
            None
        });

        let weak = Rc::downgrade(self);
        entry.connect_local("activate", false, move |_| {
            with(&weak, |l| l.launch_selected());
            None
        });

        // Runs before ClutterText's own (RUN_LAST) key handling.
        let weak = Rc::downgrade(self);
        entry.connect_local("key-press-event", false, move |args| {
            let handled = weak.upgrade().is_some_and(|l| l.on_key(ui::key_symbol(ui::event_arg(args))));
            Some(handled.to_value())
        });

        // Click on the dimmed area closes; clicks inside the box are swallowed.
        let weak = Rc::downgrade(self);
        overlay.connect_local("button-press-event", false, move |_| {
            with(&weak, |l| l.hide());
            Some(true.to_value())
        });
        panel.connect_local("button-press-event", false, |_| Some(true.to_value()));
    }

    pub fn is_visible(&self) -> bool {
        self.state.borrow().grab.is_some()
    }

    pub fn toggle(&self) {
        if self.is_visible() { self.hide() } else { self.show() }
    }

    pub fn show(&self) {
        if self.is_visible() {
            return;
        }
        let mut apps: Vec<gio::AppInfo> =
            gio::AppInfo::all().into_iter().filter(|a| a.should_show()).collect();
        apps.sort_by_cached_key(|a| a.display_name().to_lowercase());
        self.state.borrow_mut().apps = apps;

        unsafe {
            self.layout();
            ffi::clutter_text_set_text(self.entry as *mut ClutterText, c"".as_ptr());
            self.refilter();
            ffi::clutter_actor_show(self.overlay);
            let grab = ffi::clutter_stage_grab(self.stage as *mut ffi::ClutterStage, self.overlay);
            self.state.borrow_mut().grab = Some(grab);
            ffi::clutter_actor_grab_key_focus(self.entry);
        }
    }

    pub fn hide(&self) {
        let Some(grab) = self.state.borrow_mut().grab.take() else { return };
        unsafe {
            ffi::clutter_grab_dismiss(grab);
            gobject_sys::g_object_unref(grab as *mut gobject_sys::GObject);
            ffi::clutter_actor_hide(self.overlay);
            ffi::clutter_actor_destroy_all_children(self.results);
        }
        let mut state = self.state.borrow_mut();
        state.apps.clear();
        state.matches.clear();
    }

    /// Set the search text programmatically (used by the debug hook).
    pub fn set_query(&self, query: &str) {
        let q = CString::new(query).unwrap_or_default();
        unsafe { ffi::clutter_text_set_text(self.entry as *mut ClutterText, q.as_ptr()) };
    }

    pub fn destroy(&self) {
        self.hide();
        unsafe { ffi::clutter_actor_destroy(self.overlay) };
    }

    // ------------------------------------------------------------------ internals

    fn query(&self) -> String {
        unsafe {
            let p = ffi::clutter_text_get_text(self.entry as *mut ClutterText);
            if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
        }
    }

    fn on_key(&self, keysym: u32) -> bool {
        match keysym {
            KEY_ESCAPE => self.hide(),
            KEY_DOWN | KEY_TAB => self.move_selection(1),
            KEY_UP | KEY_ISO_LEFT_TAB => self.move_selection(-1),
            _ => return false,
        }
        true
    }

    fn move_selection(&self, delta: isize) {
        {
            let mut state = self.state.borrow_mut();
            let n = state.matches.len().min(MAX_RESULTS);
            if n == 0 {
                return;
            }
            state.selected = (state.selected as isize + delta).rem_euclid(n as isize) as usize;
        }
        self.render_results();
    }

    fn refilter(&self) {
        let query = self.query();
        {
            let mut state = self.state.borrow_mut();
            let matches = rank(&state.apps, &query);
            state.matches = matches;
            state.selected = 0;
        }
        unsafe {
            if query.is_empty() { ffi::clutter_actor_show(self.hint) } else { ffi::clutter_actor_hide(self.hint) }
        }
        self.render_results();
    }

    fn render_results(&self) {
        let state = self.state.borrow();
        let width = unsafe { ffi::clutter_actor_get_width(self.panel) } - 2.0 * BOX_PADDING;
        let scale = ui::primary_monitor_scale(self.display);
        let shown: Vec<(String, Option<glib::Object>)> = state
            .matches
            .iter()
            .take(MAX_RESULTS)
            .map(|&i| {
                let app = &state.apps[i];
                let icon = icons::content(app.icon().as_ref(), ui::ROW_ICON_SIZE, scale, self.stage);
                (app.display_name().to_string(), icon)
            })
            .collect();
        let selected = state.selected;
        drop(state);

        unsafe {
            ffi::clutter_actor_destroy_all_children(self.results);
            for (row_index, (name, icon)) in shown.iter().enumerate() {
                let row = ui::list_row(name, icon.as_ref(), width, row_index == selected);
                ffi::clutter_actor_set_position(row, 0.0, row_index as f32 * ROW_HEIGHT);
                ffi::clutter_actor_set_reactive(row, 1);
                // Parent first: wrapping a still-floating actor in glib::Object
                // would sink and then drop its only reference.
                ffi::clutter_actor_add_child(self.results, row);
                let this = self as *const Self;
                // Rows are destroyed (with their handlers) on every re-render and
                // on hide, so they never outlive `self`.
                gobj(row).connect_local("button-press-event", false, move |_| {
                    let launcher = &*this;
                    launcher.state.borrow_mut().selected = row_index;
                    launcher.launch_selected();
                    Some(true.to_value())
                });
            }

            let height = 2.0 * BOX_PADDING
                + ENTRY_HEIGHT
                + if shown.is_empty() { 0.0 } else { 8.0 + shown.len() as f32 * ROW_HEIGHT };
            ffi::clutter_actor_set_size(self.panel, width + 2.0 * BOX_PADDING, height);
        }
    }

    pub fn launch_selected(&self) {
        let app = {
            let state = self.state.borrow();
            state.matches.get(state.selected).map(|&i| state.apps[i].clone())
        };
        let Some(app) = app else { return };
        self.hide();

        let result = unsafe {
            let sn = ffi::meta_display_get_startup_notification(self.display);
            let ctx = ffi::meta_startup_notification_create_launcher(sn);
            ffi::meta_launch_context_set_timestamp(
                ctx,
                ffi::meta_display_get_current_time_roundtrip(self.display),
            );
            let ctx: gio::AppLaunchContext = from_glib_full(ctx as *mut gio::ffi::GAppLaunchContext);
            app.launch(&[], Some(&ctx))
        };
        match result {
            Ok(()) => glib::g_message!("gnative", "launched {}", app.id().unwrap_or_default()),
            Err(e) => glib::g_warning!("gnative", "failed to launch {}: {e}", app.display_name()),
        }
    }

    unsafe fn layout(&self) {
        unsafe {
            let geo = ui::primary_monitor_geometry(self.display).unwrap_or_default();
            ffi::clutter_actor_set_position(self.overlay, geo.x as f32, geo.y as f32);
            ffi::clutter_actor_set_size(self.overlay, geo.width as f32, geo.height as f32);

            let box_w = BOX_WIDTH.min(geo.width as f32 - 32.0);
            ffi::clutter_actor_set_size(self.panel, box_w, 2.0 * BOX_PADDING + ENTRY_HEIGHT);
            ffi::clutter_actor_set_position(
                self.panel,
                ((geo.width as f32 - box_w) / 2.0).floor(),
                (geo.height as f32 * 0.18).floor(),
            );

            let inner_w = box_w - 2.0 * BOX_PADDING;
            let text_y = BOX_PADDING + (ENTRY_HEIGHT - ffi::clutter_actor_get_height(self.hint)) / 2.0;
            ffi::clutter_actor_set_position(self.hint, BOX_PADDING + 4.0, text_y.floor());
            ffi::clutter_actor_set_position(self.entry, BOX_PADDING + 4.0, text_y.floor());
            ffi::clutter_actor_set_width(self.entry, inner_w - 8.0);
            ffi::clutter_actor_set_position(self.results, BOX_PADDING, BOX_PADDING + ENTRY_HEIGHT + 8.0);
        }
    }
}

/// Rank apps against `query`. Empty query lists everything alphabetically.
/// Name prefix beats word prefix beats substring beats id/executable match.
fn rank(apps: &[gio::AppInfo], query: &str) -> Vec<usize> {
    let names: Vec<String> = apps.iter().map(|a| a.display_name().to_string()).collect();
    let extras: Vec<String> = apps
        .iter()
        .map(|a| {
            format!(
                "{} {}",
                a.id().map(|s| s.to_string()).unwrap_or_default(),
                a.executable().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
            )
        })
        .collect();
    rank_strings(&names, &extras, query)
}

fn rank_strings(names: &[String], extras: &[String], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return (0..names.len()).collect();
    }
    let mut scored: Vec<(u8, usize)> = names
        .iter()
        .zip(extras)
        .enumerate()
        .filter_map(|(i, (name, extra))| {
            let name = name.to_lowercase();
            let score = if name.starts_with(&q) {
                0
            } else if name.split_whitespace().any(|w| w.starts_with(&q)) {
                1
            } else if name.contains(&q) {
                2
            } else if extra.to_lowercase().contains(&q) {
                3
            } else {
                return None;
            };
            Some((score, i))
        })
        .collect();
    scored.sort(); // stable on (score, alphabetical index)
    scored.into_iter().map(|(_, i)| i).collect()
}

fn with(weak: &Weak<Launcher>, f: impl FnOnce(&Launcher)) {
    if let Some(l) = weak.upgrade() {
        f(&l);
    }
}

#[cfg(test)]
mod tests {
    use super::rank_strings;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn empty_query_lists_all() {
        assert_eq!(rank_strings(&s(&["a", "b"]), &s(&["", ""]), "  "), vec![0, 1]);
    }

    #[test]
    fn prefix_beats_word_beats_substring_beats_extra() {
        let names = s(&["Files", "Text Editor", "Firefox", "Console", "Settings"]);
        let extras = s(&["nautilus", "gnome-text-editor", "firefox", "kgx", "gnome-control-center"]);
        assert_eq!(rank_strings(&names, &extras, "fi"), vec![0, 2]);
        assert_eq!(rank_strings(&names, &extras, "edit"), vec![1]);
        assert_eq!(rank_strings(&names, &extras, "ole"), vec![3]);
        assert_eq!(rank_strings(&names, &extras, "kgx"), vec![3]);
        assert_eq!(rank_strings(&names, &extras, "t"), vec![1, 4, 0]);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(rank_strings(&s(&["Firefox"]), &s(&[""]), "FIRE"), vec![0]);
    }
}
