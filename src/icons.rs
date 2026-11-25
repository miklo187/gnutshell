//! App icons without GTK.
//!
//! Icon lookup follows the freedesktop Icon Theme spec: the theme from
//! `org.gnome.desktop.interface icon-theme`, its `Inherits` chain, then
//! `hicolor`, then `/usr/share/pixmaps`. SVGs render through librsvg and PNGs
//! through cairo, into a premultiplied ARGB32 surface that is uploaded as a
//! Cogl texture. The resulting `ClutterContent`s are cached per icon and pixel
//! size, and can be shared by any number of actors.

use crate::ffi::{self, ClutterActor, MetaWindow};
use gio::prelude::*;
use glib::translate::*;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_char};
use std::path::{Path, PathBuf};

/// Fallback for apps and windows that have no usable icon.
const FALLBACK_ICON: &str = "application-x-executable";

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
    static SETTINGS: RefCell<Option<gio::Settings>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct Cache {
    theme: Option<IconTheme>,
    /// (icon key, pixel size) -> content; `None` remembers failed lookups.
    contents: HashMap<(String, i32), Option<glib::Object>>,
    /// window app-id candidates -> desktop app
    window_apps: HashMap<String, Option<gio_unix::DesktopAppInfo>>,
}

/// Content for `icon` at `size` logical pixels and `scale`, or the generic
/// fallback icon. `stage` is any actor on the stage (gives the Cogl context).
pub fn content(
    icon: Option<&gio::Icon>,
    size: f32,
    scale: f32,
    stage: *mut ClutterActor,
) -> Option<glib::Object> {
    let px = (size * scale.max(1.0)).ceil() as i32;
    icon.and_then(|icon| load_cached(icon, px, stage))
        .or_else(|| load_cached(&gio::ThemedIcon::new(FALLBACK_ICON).upcast(), px, stage))
}

/// Drop cached icons whenever the `icon-theme` setting changes.
pub fn watch_theme_changes() {
    let Some(settings) = interface_settings() else { return };
    settings.connect_changed(Some("icon-theme"), |_, _| {
        glib::g_debug!("gnative", "icon theme changed, clearing icon cache");
        CACHE.with_borrow_mut(|c| *c = Cache::default());
    });
    // The handler lives as long as the settings object: keep it for good.
    SETTINGS.with_borrow_mut(|s| *s = Some(settings));
}

/// Best-effort desktop app for a window: sandboxed / GTK application id,
/// Wayland app_id / WM_CLASS as a desktop file id, then `StartupWMClass`.
pub fn app_for_window(window: *mut MetaWindow) -> Option<gio_unix::DesktopAppInfo> {
    let ids: Vec<String> = unsafe {
        [
            ffi::meta_window_get_sandboxed_app_id(window),
            ffi::meta_window_get_gtk_application_id(window),
            ffi::meta_window_get_wm_class(window),
            ffi::meta_window_get_wm_class_instance(window),
        ]
    }
    .into_iter()
    .filter_map(cstr)
    .collect();
    if ids.is_empty() {
        return None;
    }
    let key = ids.join("\n");
    if let Some(hit) = CACHE.with_borrow(|c| c.window_apps.get(&key).cloned()) {
        return hit;
    }
    let app = find_app(&ids);
    CACHE.with_borrow_mut(|c| c.window_apps.insert(key, app.clone()));
    app
}

fn find_app(ids: &[String]) -> Option<gio_unix::DesktopAppInfo> {
    for id in ids {
        for candidate in [id.clone(), id.to_lowercase()] {
            if let Some(app) = gio_unix::DesktopAppInfo::new(&format!("{candidate}.desktop")) {
                return Some(app);
            }
        }
    }
    gio::AppInfo::all()
        .into_iter()
        .filter_map(|a| a.downcast::<gio_unix::DesktopAppInfo>().ok())
        .find(|a| {
            a.startup_wm_class()
                .is_some_and(|class| ids.iter().any(|id| id.eq_ignore_ascii_case(&class)))
        })
}

// ---------------------------------------------------------------- loading

fn load_cached(icon: &gio::Icon, px: i32, stage: *mut ClutterActor) -> Option<glib::Object> {
    let key = (IconExt::to_string(icon)?.to_string(), px);
    if let Some(hit) = CACHE.with_borrow(|c| c.contents.get(&key).cloned()) {
        return hit;
    }
    let content = resolve(icon, px).and_then(|path| load_file(&path, px, stage));
    CACHE.with_borrow_mut(|c| c.contents.insert(key, content.clone()));
    content
}

/// File path for a `GIcon` at `px` pixels.
fn resolve(icon: &gio::Icon, px: i32) -> Option<PathBuf> {
    if let Some(file_icon) = icon.downcast_ref::<gio::FileIcon>() {
        return file_icon.file().path();
    }
    let themed = icon.downcast_ref::<gio::ThemedIcon>()?;
    let names: Vec<String> = themed.names().iter().map(|n| n.to_string()).collect();
    CACHE.with_borrow_mut(|c| {
        let theme = c.theme.get_or_insert_with(IconTheme::load);
        names.iter().find_map(|name| theme.lookup(name, px))
    })
}

fn load_file(path: &Path, px: i32, stage: *mut ClutterActor) -> Option<glib::Object> {
    let surface = match path.extension().and_then(|e| e.to_str()) {
        Some("svg") => render_svg(path, px),
        Some("png") => render_png(path, px),
        _ => None,
    };
    let Some(surface) = surface else {
        glib::g_debug!("gnative", "could not load icon {}", path.display());
        return None;
    };
    let content = unsafe { upload(surface.0, stage) };
    if content.is_none() {
        glib::g_warning!("gnative", "could not upload icon {}", path.display());
    }
    content
}

/// Owned cairo image surface.
struct Surface(*mut ffi::cairo_surface_t);

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe { ffi::cairo_surface_destroy(self.0) };
    }
}

fn new_surface(px: i32) -> Option<Surface> {
    let s = Surface(unsafe { ffi::cairo_image_surface_create(ffi::CAIRO_FORMAT_ARGB32, px, px) });
    (unsafe { ffi::cairo_surface_status(s.0) } == ffi::CAIRO_STATUS_SUCCESS).then_some(s)
}

fn render_svg(path: &Path, px: i32) -> Option<Surface> {
    let cpath = CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    unsafe {
        let mut err = std::ptr::null_mut();
        let handle = ffi::rsvg_handle_new_from_file(cpath.as_ptr(), &mut err);
        if handle.is_null() {
            glib::Error::from_glib_full(err);
            return None;
        }
        let handle: glib::Object = from_glib_full(handle as *mut gobject_sys::GObject);
        let surface = new_surface(px)?;
        let cr = ffi::cairo_create(surface.0);
        // The viewport keeps the aspect ratio and centers the drawing.
        let viewport = ffi::RsvgRectangle { x: 0.0, y: 0.0, width: px as f64, height: px as f64 };
        let mut err = std::ptr::null_mut();
        let ok = ffi::rsvg_handle_render_document(
            handle.as_ptr() as *mut ffi::RsvgHandle,
            cr,
            &viewport,
            &mut err,
        );
        ffi::cairo_destroy(cr);
        if ok == 0 {
            glib::Error::from_glib_full(err);
            return None;
        }
        Some(surface)
    }
}

fn render_png(path: &Path, px: i32) -> Option<Surface> {
    let cpath = CString::new(path.as_os_str().as_encoded_bytes()).ok()?;
    unsafe {
        let src = Surface(ffi::cairo_image_surface_create_from_png(cpath.as_ptr()));
        if ffi::cairo_surface_status(src.0) != ffi::CAIRO_STATUS_SUCCESS {
            return None;
        }
        let (w, h) = (
            ffi::cairo_image_surface_get_width(src.0) as f64,
            ffi::cairo_image_surface_get_height(src.0) as f64,
        );
        if w <= 0.0 || h <= 0.0 {
            return None;
        }
        let surface = new_surface(px)?;
        let cr = ffi::cairo_create(surface.0);
        let scale = px as f64 / w.max(h);
        ffi::cairo_translate(cr, (px as f64 - w * scale) / 2.0, (px as f64 - h * scale) / 2.0);
        ffi::cairo_scale(cr, scale, scale);
        ffi::cairo_set_source_surface(cr, src.0, 0.0, 0.0);
        ffi::cairo_paint(cr);
        ffi::cairo_destroy(cr);
        Some(surface)
    }
}

/// Upload a cairo ARGB32 surface as a `ClutterTextureContent`.
unsafe fn upload(surface: *mut ffi::cairo_surface_t, stage: *mut ClutterActor) -> Option<glib::Object> {
    // Cairo ARGB32 is a native-endian premultiplied u32: B,G,R,A bytes on
    // little-endian machines.
    #[cfg(target_endian = "big")]
    compile_error!("icon upload assumes a little-endian pixel layout");
    unsafe {
        ffi::cairo_surface_flush(surface);
        let backend = ffi::clutter_context_get_backend(ffi::clutter_actor_get_context(stage));
        let ctx = ffi::clutter_backend_get_cogl_context(backend);
        let mut err = std::ptr::null_mut();
        let texture = ffi::cogl_texture_2d_new_from_data(
            ctx,
            ffi::cairo_image_surface_get_width(surface),
            ffi::cairo_image_surface_get_height(surface),
            ffi::COGL_PIXEL_FORMAT_BGRA_8888_PRE,
            ffi::cairo_image_surface_get_stride(surface),
            ffi::cairo_image_surface_get_data(surface),
            &mut err,
        );
        if texture.is_null() {
            glib::Error::from_glib_full(err);
            return None;
        }
        let content = ffi::clutter_texture_content_new_from_texture(texture, std::ptr::null_mut());
        gobject_sys::g_object_unref(texture as *mut gobject_sys::GObject);
        (!content.is_null()).then(|| from_glib_full(content as *mut gobject_sys::GObject))
    }
}

// ---------------------------------------------------------------- icon theme

#[derive(Clone, Copy, Debug, PartialEq)]
enum DirKind {
    Fixed,
    Scalable { min: i32, max: i32 },
    Threshold(i32),
}

#[derive(Clone, Copy, Debug)]
struct Dir {
    size: i32,
    scale: i32,
    kind: DirKind,
}

impl Dir {
    /// Distance from the wanted pixel size (spec: DirectorySizeDistance, in
    /// device pixels so scaled directories compare correctly).
    fn distance(&self, px: i32) -> i32 {
        let s = self.scale.max(1);
        let (lo, hi) = match self.kind {
            DirKind::Fixed => (self.size * s, self.size * s),
            DirKind::Scalable { min, max } => (min * s, max * s),
            DirKind::Threshold(t) => ((self.size - t) * s, (self.size + t) * s),
        };
        if px < lo { lo - px } else if px > hi { px - hi } else { 0 }
    }
}

struct Theme {
    dirs: Vec<Dir>,
    /// icon name -> (directory index, file)
    files: HashMap<String, Vec<(usize, PathBuf)>>,
}

struct IconTheme {
    themes: Vec<Theme>,
    pixmaps: Vec<PathBuf>,
}

impl IconTheme {
    fn load() -> Self {
        let bases = base_dirs();
        let mut themes = Vec::new();
        let mut seen = HashSet::new();
        let mut queue = vec![current_theme_name()];
        while let Some(name) = queue.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if let Some((theme, inherits)) = Theme::load(&bases, &name) {
                themes.push(theme);
                // Depth-first in declared order.
                queue.extend(inherits.into_iter().rev());
            }
            if queue.is_empty() && !seen.contains("hicolor") {
                queue.push("hicolor".into());
            }
        }
        let pixmaps = data_dirs().into_iter().map(|d| d.join("pixmaps")).collect();
        Self { themes, pixmaps }
    }

    fn lookup(&self, name: &str, px: i32) -> Option<PathBuf> {
        if name.starts_with('/') {
            return Some(PathBuf::from(name));
        }
        self.themes
            .iter()
            .find_map(|t| t.lookup(name, px))
            .or_else(|| {
                self.pixmaps.iter().find_map(|dir| {
                    ["svg", "png"]
                        .iter()
                        .map(|ext| dir.join(format!("{name}.{ext}")))
                        .find(|p| p.is_file())
                })
            })
    }
}

impl Theme {
    /// Load the theme's `index.theme` and index every icon file it contains.
    /// Returns the theme and its `Inherits` list.
    fn load(bases: &[PathBuf], name: &str) -> Option<(Self, Vec<String>)> {
        let index = bases.iter().map(|b| b.join(name).join("index.theme")).find(|p| p.is_file())?;
        let kf = glib::KeyFile::new();
        // Icon theme lists are comma-separated, not GKeyFile's default ';'.
        kf.set_list_separator(glib::Char::from(b','));
        kf.load_from_file(&index, glib::KeyFileFlags::NONE).ok()?;
        let group = "Icon Theme";
        let list = |key: &str| -> Vec<String> {
            kf.string_list(group, key)
                .map(|v| v.iter().map(|s| s.to_string()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default()
        };
        let mut subdirs = list("Directories");
        subdirs.extend(list("ScaledDirectories"));
        let inherits = list("Inherits");

        let mut dirs = Vec::new();
        let mut files: HashMap<String, Vec<(usize, PathBuf)>> = HashMap::new();
        for sub in subdirs {
            let Ok(size) = kf.integer(&sub, "Size") else { continue };
            let int = |key: &str, default: i32| kf.integer(&sub, key).unwrap_or(default);
            let kind = match kf.string(&sub, "Type").as_deref().unwrap_or("Threshold") {
                "Fixed" => DirKind::Fixed,
                "Scalable" => DirKind::Scalable { min: int("MinSize", size), max: int("MaxSize", size) },
                _ => DirKind::Threshold(int("Threshold", 2)),
            };
            let dir_index = dirs.len();
            dirs.push(Dir { size, scale: int("Scale", 1), kind });
            for base in bases {
                let Ok(entries) = std::fs::read_dir(base.join(name).join(&sub)) else { continue };
                for entry in entries.flatten() {
                    let path = entry.path();
                    let ext = path.extension().and_then(|e| e.to_str());
                    if !matches!(ext, Some("svg" | "png")) {
                        continue;
                    }
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        files.entry(stem.to_owned()).or_default().push((dir_index, path));
                    }
                }
            }
        }
        Some((Self { dirs, files }, inherits))
    }

    fn lookup(&self, name: &str, px: i32) -> Option<PathBuf> {
        let candidates = self.files.get(name)?;
        candidates
            .iter()
            .min_by_key(|(dir, path)| {
                // Ties: prefer SVG, it scales cleanly.
                let png = path.extension().is_some_and(|e| e == "png");
                (self.dirs[*dir].distance(px), png)
            })
            .map(|(_, path)| path.clone())
    }
}

fn interface_settings() -> Option<gio::Settings> {
    let schema_id = "org.gnome.desktop.interface";
    // Settings::new aborts on a missing schema, so check first.
    gio::SettingsSchemaSource::default()
        .and_then(|src| src.lookup(schema_id, true))
        .map(|_| gio::Settings::new(schema_id))
}

fn current_theme_name() -> String {
    let name = interface_settings()
        .map(|s| s.string("icon-theme").to_string())
        .unwrap_or_default();
    if name.is_empty() { "Adwaita".into() } else { name }
}

/// `$XDG_DATA_HOME` then `$XDG_DATA_DIRS`.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![glib::user_data_dir()];
    dirs.extend(glib::system_data_dirs());
    dirs
}

/// Icon base directories in spec order: `~/.icons`, then `<data dir>/icons`.
fn base_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![glib::home_dir().join(".icons")];
    dirs.extend(data_dirs().into_iter().map(|d| d.join("icons")));
    dirs
}

fn cstr(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::{Dir, DirKind};

    #[test]
    fn fixed_distance() {
        let d = Dir { size: 48, scale: 1, kind: DirKind::Fixed };
        assert_eq!(d.distance(48), 0);
        assert_eq!(d.distance(24), 24);
        assert_eq!(d.distance(64), 16);
    }

    #[test]
    fn scaled_dir_uses_device_pixels() {
        let d = Dir { size: 24, scale: 2, kind: DirKind::Fixed };
        assert_eq!(d.distance(48), 0);
    }

    #[test]
    fn scalable_and_threshold_ranges() {
        let s = Dir { size: 128, scale: 1, kind: DirKind::Scalable { min: 16, max: 512 } };
        assert_eq!(s.distance(24), 0);
        assert_eq!(s.distance(8), 8);
        let t = Dir { size: 32, scale: 1, kind: DirKind::Threshold(2) };
        assert_eq!(t.distance(30), 0);
        assert_eq!(t.distance(28), 2);
    }
}

