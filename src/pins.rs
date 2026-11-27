//! Apps pinned as shortcuts to the taskbar and to the top bar, kept in
//! `~/.config/gnutshell/pinned.ini`.
//!
//! This is gnutshell's own list: `org.gnome.shell favorite-apps` would fill
//! the bars with GNOME's defaults and change the dash of a GNOME session.

use crate::ffi::MetaDisplay;
use crate::icons;
use crate::ui;
use crate::windows;
use gio::prelude::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

const GROUP: &str = "pinned";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Taskbar,
    TopBar,
}

impl Place {
    const ALL: [Place; 2] = [Place::Taskbar, Place::TopBar];

    fn key(self) -> &'static str {
        match self {
            Place::Taskbar => "taskbar",
            Place::TopBar => "topbar",
        }
    }
}

/// `list` with `id` removed if it is there, else added at the end.
fn toggled(list: &[String], id: &str) -> Vec<String> {
    let mut out: Vec<String> = list.iter().filter(|i| *i != id).cloned().collect();
    if out.len() == list.len() {
        out.push(id.to_owned());
    }
    out
}

type Listener = Rc<dyn Fn()>;

pub struct Pins {
    path: PathBuf,
    /// Desktop file ids in the order they were pinned, indexed by `Place`.
    lists: RefCell<[Vec<String>; 2]>,
    listener: RefCell<Option<Listener>>,
}

impl Pins {
    pub fn load() -> Rc<Self> {
        let path = glib::user_config_dir().join("gnutshell").join("pinned.ini");
        let file = glib::KeyFile::new();
        if let Err(e) = file.load_from_file(&path, glib::KeyFileFlags::NONE)
            && !e.matches(glib::FileError::Noent)
        {
            glib::g_warning!("gnutshell", "could not read {}: {e}", path.display());
        }
        let lists = Place::ALL.map(|place| {
            file.string_list(GROUP, place.key())
                .map(|list| list.iter().map(|s| s.to_string()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default()
        });
        Rc::new(Self { path, lists: RefCell::new(lists), listener: RefCell::new(None) })
    }

    /// `listener` runs after every change.
    pub fn set_listener(&self, listener: impl Fn() + 'static) {
        *self.listener.borrow_mut() = Some(Rc::new(listener));
    }

    pub fn contains(&self, place: Place, id: &str) -> bool {
        self.lists.borrow()[place as usize].iter().any(|i| i == id)
    }

    /// The pinned apps that are installed, in order. Others stay on the list.
    pub fn apps(&self, place: Place) -> Vec<(String, gio_unix::DesktopAppInfo)> {
        let ids = self.lists.borrow()[place as usize].clone();
        ids.into_iter().filter_map(|id| app(&id).map(|app| (id, app))).collect()
    }

    pub fn toggle(&self, place: Place, id: &str) {
        let list = toggled(&self.lists.borrow()[place as usize], id);
        self.lists.borrow_mut()[place as usize] = list;
        self.save();
        let listener = self.listener.borrow().clone();
        if let Some(listener) = listener {
            listener();
        }
    }

    fn save(&self) {
        let file = glib::KeyFile::new();
        for place in Place::ALL {
            let list = self.lists.borrow()[place as usize].clone();
            // Desktop file ids don't contain `;`, the list separator.
            let value: String = list.iter().map(|id| format!("{id};")).collect();
            file.set_string(GROUP, place.key(), &value);
        }
        let result = self
            .path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .map_err(|e| e.to_string())
            .and_then(|()| file.save_to_file(&self.path).map_err(|e| e.to_string()));
        if let Err(e) = result {
            glib::g_warning!("gnutshell", "could not store {}: {e}", self.path.display());
        }
    }
}

pub fn app(id: &str) -> Option<gio_unix::DesktopAppInfo> {
    gio_unix::DesktopAppInfo::new(id)
}

/// The desktop file id of the app a window belongs to, if it can be told.
pub fn app_id_of(window: &glib::Object) -> Option<String> {
    icons::app_for_window(windows::ptr(window)).and_then(|app| app.id()).map(|id| id.to_string())
}

/// Start `app`. With `new_window`, use the app's own "new window" action if it
/// has one, because many apps only raise their window when started again.
pub fn launch(display: *mut MetaDisplay, app: &gio_unix::DesktopAppInfo, new_window: bool) {
    let context = ui::launch_context(display);
    if new_window && app.list_actions().iter().any(|a| a.as_str() == "new-window") {
        app.launch_action("new-window", Some(&context));
        return;
    }
    if let Err(e) = app.launch(&[], Some(&context)) {
        glib::g_warning!("gnutshell", "failed to launch {}: {e}", app.display_name());
    }
}

#[cfg(test)]
mod tests {
    use super::toggled;

    #[test]
    fn pins_at_the_end_and_unpins() {
        let list = vec!["a".to_owned(), "b".to_owned()];
        assert_eq!(toggled(&list, "c"), ["a", "b", "c"]);
        assert_eq!(toggled(&list, "a"), ["b"]);
        assert_eq!(toggled(&[], "a"), ["a"]);
    }
}
