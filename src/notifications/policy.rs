//! What a notification may do: gnome-shell's per-app settings
//! (Settings → Notifications), Do Not Disturb and sounds.
//!
//! As in gnome-shell, an app that sends a notification is added to
//! `application-children`, which is what makes Settings list it.

use super::Notification;
use crate::ffi::{self, MetaDisplay};
use gio::prelude::*;
use glib::translate::*;
use std::ffi::CString;

const GLOBAL_SCHEMA: &str = "org.gnome.desktop.notifications";
const APP_SCHEMA: &str = "org.gnome.desktop.notifications.application";
const APP_PATH: &str = "/org/gnome/desktop/notifications/application/";
const SOUND_SCHEMA: &str = "org.gnome.desktop.sound";

/// One app's settings.
#[derive(Clone, Copy, Debug)]
pub struct AppSettings {
    pub enable: bool,
    pub banners: bool,
    pub sound_alerts: bool,
}

impl Default for AppSettings {
    /// The schema's defaults, also used for notifications from no known app.
    fn default() -> Self {
        Self { enable: true, banners: true, sound_alerts: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Delivery {
    /// The app is switched off: not shown and not listed.
    Drop,
    /// Straight to the notification list.
    ListOnly,
    Banner,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Verdict {
    pub delivery: Delivery,
    /// May play the sound it asks for.
    pub sound: bool,
}

/// `banners` is Do Not Disturb being off. Critical notifications get past it,
/// and past an app's banners being off, but not past the app being off.
pub fn decide(banners: bool, app: AppSettings, critical: bool) -> Verdict {
    let delivery = if !app.enable {
        Delivery::Drop
    } else if critical || (banners && app.banners) {
        Delivery::Banner
    } else {
        Delivery::ListOnly
    };
    Verdict { delivery, sound: delivery == Delivery::Banner && app.sound_alerts }
}

fn schema_exists(id: &str) -> bool {
    // Settings::new aborts on a missing schema.
    gio::SettingsSchemaSource::default().is_some_and(|s| s.lookup(id, true).is_some())
}

/// gnome-shell's key for an app in these settings, from its desktop id.
pub fn canonical_id(desktop_id: &str) -> String {
    let stem = desktop_id.strip_suffix(".desktop").unwrap_or(desktop_id);
    let mut out = String::new();
    for c in stem.chars() {
        let c = if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' };
        if !(c == '-' && out.ends_with('-')) {
            out.push(c);
        }
    }
    out.trim_matches('-').to_owned()
}

/// The installed app that sent `n`: by the `desktop-entry` hint, or without
/// one by the app name it gave.
fn sending_app(n: &Notification) -> Option<gio_unix::DesktopAppInfo> {
    let by_id = |id: &str| {
        let id = format!("{}.desktop", id.strip_suffix(".desktop").unwrap_or(id));
        gio_unix::DesktopAppInfo::new(&id)
    };
    if !n.desktop_entry.is_empty() {
        return by_id(&n.desktop_entry);
    }
    let name = n.app_name.as_str();
    if name.is_empty() || name.contains('/') {
        return None;
    }
    by_id(name).or_else(|| {
        gio::AppInfo::all()
            .into_iter()
            .filter_map(|a| a.downcast::<gio_unix::DesktopAppInfo>().ok())
            .find(|a| a.should_show() && a.name().eq_ignore_ascii_case(name))
    })
}

/// The settings for the app that sent `n`, listing it in Settings if new.
pub fn app_settings(n: &Notification) -> AppSettings {
    if !schema_exists(APP_SCHEMA) || !schema_exists(GLOBAL_SCHEMA) {
        return AppSettings::default();
    }
    let app = sending_app(n);
    let desktop_id = app.as_ref().and_then(|a| a.id()).map(|s| s.to_string());
    let id = canonical_id(desktop_id.as_deref().unwrap_or(&n.desktop_entry));
    if id.is_empty() {
        return AppSettings::default();
    }
    let settings = gio::Settings::with_path(APP_SCHEMA, &format!("{APP_PATH}{id}/"));
    // Only installed apps are worth listing.
    if let Some(desktop_id) = desktop_id {
        register(&settings, &id, &desktop_id);
    }
    AppSettings {
        enable: settings.boolean("enable"),
        banners: settings.boolean("show-banners"),
        sound_alerts: settings.boolean("enable-sound-alerts"),
    }
}

fn register(app: &gio::Settings, id: &str, desktop_id: &str) {
    let global = gio::Settings::new(GLOBAL_SCHEMA);
    let mut children: Vec<String> = global.strv("application-children").iter().map(|s| s.to_string()).collect();
    if children.iter().any(|c| c == id) {
        return;
    }
    children.push(id.to_owned());
    let children: Vec<&str> = children.iter().map(String::as_str).collect();
    if let Err(e) = app
        .set_string("application-id", desktop_id)
        .and_then(|()| global.set_strv("application-children", children.as_slice()))
    {
        glib::g_warning!("gnutshell", "could not list {desktop_id} in notification settings: {e}");
    }
}

/// Play what `n` asks for with the `sound-file` or `sound-name` hint, unless
/// system event sounds are off. Notifications without either are silent.
pub fn play_sound(display: *mut MetaDisplay, n: &Notification) {
    if n.suppress_sound || (n.sound_file.is_empty() && n.sound_name.is_empty()) {
        return;
    }
    if schema_exists(SOUND_SCHEMA) && !gio::Settings::new(SOUND_SCHEMA).boolean("event-sounds") {
        return;
    }
    let Ok(description) = CString::new(n.summary.replace('\0', "")) else { return };
    let player = unsafe { ffi::meta_display_get_sound_player(display) };
    if !n.sound_file.is_empty() {
        let file = if n.sound_file.starts_with("file://") {
            gio::File::for_uri(&n.sound_file)
        } else if n.sound_file.starts_with('/') {
            gio::File::for_path(&n.sound_file)
        } else {
            return;
        };
        glib::g_debug!("gnutshell", "notification {}: playing a sound file", n.id);
        unsafe {
            ffi::meta_sound_player_play_from_file(
                player,
                file.to_glib_none().0,
                description.as_ptr(),
                std::ptr::null_mut(),
            );
        }
        return;
    }
    let Ok(name) = CString::new(n.sound_name.as_str()) else { return };
    glib::g_debug!("gnutshell", "notification {}: playing sound {name:?}", n.id);
    unsafe {
        ffi::meta_sound_player_play_from_theme(player, name.as_ptr(), description.as_ptr(), std::ptr::null_mut());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_ids_match_gnome_shell() {
        assert_eq!(canonical_id("org.gnome.Nautilus.desktop"), "org-gnome-nautilus");
        assert_eq!(canonical_id("com.obsproject.Studio"), "com-obsproject-studio");
        assert_eq!(canonical_id("brave-browser.desktop"), "brave-browser");
        assert_eq!(canonical_id("../../x  y"), "x-y");
        assert_eq!(canonical_id("..."), "");
    }

    #[test]
    fn delivery_rules() {
        let app = AppSettings::default();
        let verdict = |delivery, sound| Verdict { delivery, sound };
        assert_eq!(decide(true, app, false), verdict(Delivery::Banner, true));
        assert_eq!(decide(false, app, false), verdict(Delivery::ListOnly, false));
        assert_eq!(decide(false, app, true), verdict(Delivery::Banner, true));
        let quiet = AppSettings { banners: false, ..app };
        assert_eq!(decide(true, quiet, false), verdict(Delivery::ListOnly, false));
        assert_eq!(decide(true, quiet, true), verdict(Delivery::Banner, true));
        let silent = AppSettings { sound_alerts: false, ..app };
        assert_eq!(decide(true, silent, false), verdict(Delivery::Banner, false));
        let off = AppSettings { enable: false, ..app };
        assert_eq!(decide(true, off, true), verdict(Delivery::Drop, false));
    }
}
