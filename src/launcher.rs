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
use std::ffi::CStr;
use std::rc::{Rc, Weak};

const MAX_RESULTS: usize = 8;
const BOX_WIDTH: f32 = 600.0;
const ENTRY_HEIGHT: f32 = 40.0;
const ENTRY_FONT: &CStr = c"Sans 16";
const SCROLLBAR_WIDTH: f32 = 4.0;
/// Width of the panel button that opens the launcher.
pub const PANEL_BUTTON_WIDTH: f32 = 36.0;
const BAR_WIDTH: f32 = 14.0;
const BAR_HEIGHT: f32 = 2.0;
/// From the top of one bar of the button's icon to the next.
const BAR_STEP: f32 = 5.0;
/// Space between rows and the scrollbar.
const SCROLLBAR_GAP: f32 = 10.0;
const SCROLLBAR_COLOR: ffi::CoglColor = ffi::CoglColor::rgba(0xff, 0xff, 0xff, 0x50);

/// Opens the menu of an app, by desktop file id, at a point on the stage.
type MenuHandler = Rc<dyn Fn(String, f32, f32)>;

pub struct Launcher {
    display: *mut MetaDisplay,
    menu: RefCell<Option<MenuHandler>>,
    stage: *mut ClutterActor,
    overlay: *mut ClutterActor,
    panel: *mut ClutterActor,
    entry: *mut ClutterActor,
    hint: *mut ClutterActor,
    results: *mut ClutterActor,
    scrollbar: *mut ClutterActor,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    apps: Vec<gio::AppInfo>,
    matches: Vec<usize>,
    /// Index into `matches`.
    selected: usize,
    /// First visible index into `matches`.
    offset: usize,
    /// Fractional smooth-scroll rows not applied yet.
    scroll_acc: f64,
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

            let scrollbar = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(scrollbar, &SCROLLBAR_COLOR);
            ffi::clutter_actor_add_child(panel, scrollbar);

            ffi::clutter_actor_add_child(stage, overlay);

            Rc::new(Self {
                menu: RefCell::default(),
                display,
                stage,
                overlay,
                panel,
                entry,
                hint,
                results,
                scrollbar,
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

        // Wheel / touchpad over the box scrolls the result list.
        let weak = Rc::downgrade(self);
        panel.connect_local("scroll-event", false, move |args| {
            with(&weak, |l| l.on_scroll(ui::event_arg(args)));
            Some(true.to_value())
        });
    }

    /// `handler` runs on a right click on an app.
    pub fn set_menu_handler(&self, handler: impl Fn(String, f32, f32) + 'static) {
        *self.menu.borrow_mut() = Some(Rc::new(handler));
    }

    /// Take the keyboard back after a menu over the launcher.
    pub fn refocus(&self) {
        if self.is_visible() {
            unsafe { ffi::clutter_actor_grab_key_focus(self.entry) };
        }
    }

    /// The right click menu for the app at `index` of the matches.
    fn open_menu(&self, index: usize, at: Option<(f32, f32)>) {
        let id = {
            let state = self.state.borrow();
            state.matches.get(index).and_then(|&i| state.apps[i].id())
        };
        let handler = self.menu.borrow().clone();
        if let (Some(id), Some(handler), Some((x, y))) = (id, handler, at) {
            handler(id.to_string(), x, y);
        }
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
    #[cfg(debug_assertions)]
    pub fn set_query(&self, query: &str) {
        let q = std::ffi::CString::new(query).unwrap_or_default();
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
            let n = state.matches.len();
            if n == 0 {
                return;
            }
            state.selected = (state.selected as isize + delta).rem_euclid(n as isize) as usize;
            state.offset = ui::scroll_to_show(state.offset, n, MAX_RESULTS, state.selected);
        }
        self.render_results();
    }

    /// Scroll the visible window; the selection stays where it is.
    fn on_scroll(&self, event: *const ffi::ClutterEvent) {
        let changed = {
            let mut state = self.state.borrow_mut();
            let mut acc = state.scroll_acc;
            let rows = ui::scroll_rows(event, &mut acc);
            state.scroll_acc = acc;
            let max_offset = state.matches.len().saturating_sub(MAX_RESULTS);
            let offset = (state.offset as isize + rows as isize).clamp(0, max_offset as isize) as usize;
            let changed = offset != state.offset;
            state.offset = offset;
            changed
        };
        if changed {
            self.render_results();
        }
    }

    fn refilter(&self) {
        let query = self.query();
        {
            let mut state = self.state.borrow_mut();
            let matches = rank(&state.apps, &query);
            state.matches = matches;
            state.selected = 0;
            state.offset = 0;
            state.scroll_acc = 0.0;
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
        let offset = state.offset;
        let total = state.matches.len();
        let shown: Vec<(String, Option<glib::Object>)> = state
            .matches
            .iter()
            .skip(offset)
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
                let index = offset + row_index;
                let row = ui::list_row(name, icon.as_ref(), width - SCROLLBAR_GAP, index == selected);
                ffi::clutter_actor_set_position(row, 0.0, row_index as f32 * ROW_HEIGHT);
                ffi::clutter_actor_set_reactive(row, 1);
                // Parent first: wrapping a still-floating actor in glib::Object
                // would sink and then drop its only reference.
                ffi::clutter_actor_add_child(self.results, row);
                let this = self as *const Self;
                // Rows are destroyed (with their handlers) on every re-render and
                // on hide, so they never outlive `self`.
                gobj(row).connect_local("button-press-event", false, move |args| {
                    let launcher = &*this;
                    let event = ui::event_arg(args);
                    if ffi::clutter_event_get_button(event) == 3 {
                        launcher.open_menu(index, ui::event_coords(event));
                    } else {
                        launcher.state.borrow_mut().selected = index;
                        launcher.launch_selected();
                    }
                    Some(true.to_value())
                });
            }

            let height = 2.0 * BOX_PADDING
                + ENTRY_HEIGHT
                + if shown.is_empty() { 0.0 } else { 8.0 + shown.len() as f32 * ROW_HEIGHT };
            ffi::clutter_actor_set_size(self.panel, width + 2.0 * BOX_PADDING, height);

            // Scrollbar thumb, only when there is more than fits.
            if total > MAX_RESULTS {
                let track = shown.len() as f32 * ROW_HEIGHT;
                let (y, h) = scrollbar_thumb(track, total, MAX_RESULTS, offset);
                let top = BOX_PADDING + ENTRY_HEIGHT + 8.0;
                ffi::clutter_actor_set_size(self.scrollbar, SCROLLBAR_WIDTH, h);
                ffi::clutter_actor_set_position(
                    self.scrollbar,
                    BOX_PADDING + width - SCROLLBAR_WIDTH,
                    top + y,
                );
                ffi::clutter_actor_show(self.scrollbar);
            } else {
                ffi::clutter_actor_hide(self.scrollbar);
            }
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
            Ok(()) => glib::g_message!("gnutshell", "launched {}", app.id().unwrap_or_default()),
            Err(e) => glib::g_warning!("gnutshell", "failed to launch {}: {e}", app.display_name()),
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

/// Add the three-bar button at the left end of `panel`; `pressed` runs on a
/// primary click.
pub fn add_panel_button(panel: *mut ClutterActor, height: f32, pressed: impl Fn() + 'static) {
    let x = ((PANEL_BUTTON_WIDTH - BAR_WIDTH) / 2.0).floor();
    let top = ((height - (2.0 * BAR_STEP + BAR_HEIGHT)) / 2.0).floor();
    let bars: Vec<_> = (0..3).map(|i| (x, top + i as f32 * BAR_STEP, BAR_WIDTH, BAR_HEIGHT)).collect();
    ui::panel_button(panel, 0.0, PANEL_BUTTON_WIDTH, height, &bars, pressed);
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

/// Scrollbar thumb (y, height) on a `track`-px track for a window of `max`
/// rows at `offset` over `total` rows.
fn scrollbar_thumb(track: f32, total: usize, max: usize, offset: usize) -> (f32, f32) {
    if total <= max {
        return (0.0, track);
    }
    let h = (track * max as f32 / total as f32).max(16.0).min(track);
    let max_offset = (total - max) as f32;
    let y = (track - h) * (offset as f32 / max_offset).min(1.0);
    (y.round(), h.round())
}

fn with(weak: &Weak<Launcher>, f: impl FnOnce(&Launcher)) {
    if let Some(l) = weak.upgrade() {
        f(&l);
    }
}

#[cfg(test)]
mod tests {
    use super::{rank_strings, scrollbar_thumb};

    #[test]
    fn scrollbar_spans_track() {
        assert_eq!(scrollbar_thumb(288.0, 8, 8, 0), (0.0, 288.0));
        assert_eq!(scrollbar_thumb(288.0, 32, 8, 0), (0.0, 72.0));
        assert_eq!(scrollbar_thumb(288.0, 32, 8, 24), (216.0, 72.0));
        assert_eq!(scrollbar_thumb(288.0, 1000, 8, 0).1, 16.0);
    }

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
