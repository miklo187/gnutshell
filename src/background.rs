//! The desktop background from `org.gnome.desktop.background`.
//!
//! Mutter draws it: one `MetaBackgroundActor` per monitor, all showing one
//! `MetaBackground`, which handles the colors and `picture-options`. Mutter 51
//! no longer reads image files, so the picture is decoded here with glycin (the
//! library gnome-shell uses) on a worker thread and handed over as a texture.
//! Slideshows described by XML files are not supported.

use crate::ffi::{self, ClutterActor, CoglColor, MetaDisplay, MtkRectangle};
use gio::prelude::*;
use glib::translate::*;
use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_int};
use std::rc::{Rc, Weak};

const SCHEMA: &str = "org.gnome.desktop.background";
const INTERFACE_SCHEMA: &str = "org.gnome.desktop.interface";
/// Shown when the settings are missing or a color cannot be read.
const FALLBACK_COLOR: CoglColor = CoglColor::rgba(0x24, 0x2a, 0x38, 0xff);
/// Largest side an SVG is rendered at.
const MAX_SVG_SIDE: u32 = 8192;

/// `GDesktopBackgroundStyle` for a `picture-options` value.
pub fn style_from_options(options: &str) -> c_int {
    match options {
        "none" => ffi::G_DESKTOP_BACKGROUND_STYLE_NONE,
        "wallpaper" => ffi::G_DESKTOP_BACKGROUND_STYLE_WALLPAPER,
        "centered" => ffi::G_DESKTOP_BACKGROUND_STYLE_CENTERED,
        "scaled" => ffi::G_DESKTOP_BACKGROUND_STYLE_SCALED,
        "stretched" => ffi::G_DESKTOP_BACKGROUND_STYLE_STRETCHED,
        "spanned" => ffi::G_DESKTOP_BACKGROUND_STYLE_SPANNED,
        _ => ffi::G_DESKTOP_BACKGROUND_STYLE_ZOOM,
    }
}

/// `GDesktopBackgroundShading` for a `color-shading-type` value.
pub fn shading_from_type(kind: &str) -> c_int {
    match kind {
        "vertical" => ffi::G_DESKTOP_BACKGROUND_SHADING_VERTICAL,
        "horizontal" => ffi::G_DESKTOP_BACKGROUND_SHADING_HORIZONTAL,
        _ => ffi::G_DESKTOP_BACKGROUND_SHADING_SOLID,
    }
}

/// `#rgb`, `#rrggbb` or `#rrrrggggbbbb`, the forms found in these settings.
pub fn parse_color(text: &str) -> Option<CoglColor> {
    let hex = text.strip_prefix('#')?;
    if !matches!(hex.len(), 3 | 6 | 12) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let digits = hex.len() / 3;
    let channel = |i: usize| -> Option<u8> {
        let part = &hex[i * digits..(i + 1) * digits];
        let value = u16::from_str_radix(part, 16).ok()?;
        Some(match digits {
            1 => (value * 0x11) as u8,
            2 => value as u8,
            _ => (value >> 8) as u8,
        })
    };
    Some(CoglColor::rgba(channel(0)?, channel(1)?, channel(2)?, 0xff))
}

/// The size to ask for so that a `width` x `height` drawing covers `target`
/// while keeping its shape.
pub fn cover_size(width: u32, height: u32, target: (u32, u32)) -> (u32, u32) {
    if width == 0 || height == 0 {
        return target;
    }
    let scale = (target.0 as f64 / width as f64).max(target.1 as f64 / height as f64);
    let side = |v: u32| ((v as f64 * scale).ceil() as u32).clamp(1, MAX_SVG_SIDE);
    (side(width), side(height))
}

/// A decoded picture: premultiplied RGBA rows.
struct Picture {
    data: Vec<u8>,
    width: i32,
    height: i32,
    stride: i32,
}

/// Read the picture at `uri`. Blocks, so call it from a worker thread. An SVG
/// is rendered large enough to cover `svg_target`, or at its own size.
fn decode(uri: &str, svg_target: Option<(u32, u32)>) -> Result<Picture, String> {
    unsafe fn take<T: IsA<glib::Object>>(ptr: *mut gobject_sys::GObject, err: *mut glib::ffi::GError) -> Result<T, String>
    where
        T: glib::object::ObjectType,
    {
        unsafe {
            if ptr.is_null() {
                if err.is_null() {
                    return Err("no result".to_owned());
                }
                let e: glib::Error = from_glib_full(err);
                return Err(e.to_string());
            }
            let object: glib::Object = from_glib_full(ptr);
            Ok(object.unsafe_cast())
        }
    }

    let file = gio::File::for_uri(uri);
    unsafe {
        let loader: glib::Object = from_glib_full(ffi::gly_loader_new(file.as_ptr() as *mut _));
        let loader_ptr = loader.as_ptr() as *mut ffi::GlyLoader;
        ffi::gly_loader_set_accepted_memory_formats(loader_ptr, ffi::GLY_MEMORY_SELECTION_R8G8B8A8_PREMULTIPLIED);
        let mut err = std::ptr::null_mut();
        let image: glib::Object = take(ffi::gly_loader_load(loader_ptr, &mut err) as *mut _, err)?;
        let image_ptr = image.as_ptr() as *mut ffi::GlyImage;

        let mime = ffi::gly_image_get_mime_type(image_ptr);
        let is_svg = !mime.is_null() && CStr::from_ptr(mime).to_bytes().starts_with(b"image/svg");
        let mut err = std::ptr::null_mut();
        let frame = match svg_target.filter(|_| is_svg) {
            Some(target) => {
                let request: glib::Object = from_glib_full(ffi::gly_frame_request_new() as *mut gobject_sys::GObject);
                let request_ptr = request.as_ptr() as *mut ffi::GlyFrameRequest;
                let (w, h) = cover_size(ffi::gly_image_get_width(image_ptr), ffi::gly_image_get_height(image_ptr), target);
                ffi::gly_frame_request_set_scale(request_ptr, w, h);
                ffi::gly_image_get_specific_frame(image_ptr, request_ptr, &mut err)
            }
            None => ffi::gly_image_next_frame(image_ptr, &mut err),
        };
        let frame: glib::Object = take(frame as *mut _, err)?;
        let frame_ptr = frame.as_ptr() as *mut ffi::GlyFrame;

        let format = ffi::gly_frame_get_memory_format(frame_ptr);
        if format != ffi::GLY_MEMORY_R8G8B8A8_PREMULTIPLIED {
            return Err(format!("unexpected memory format {format}"));
        }
        let (width, height, stride) = (
            ffi::gly_frame_get_width(frame_ptr) as usize,
            ffi::gly_frame_get_height(frame_ptr) as usize,
            ffi::gly_frame_get_stride(frame_ptr) as usize,
        );
        let bytes: glib::Bytes = from_glib_none(ffi::gly_frame_get_buf_bytes(frame_ptr));
        if width == 0 || height == 0 || stride < width * 4 || bytes.len() < (height - 1) * stride + width * 4 {
            return Err(format!("bad picture size {width}x{height}, stride {stride}, {} bytes", bytes.len()));
        }
        let int = |v: usize| i32::try_from(v).map_err(|_| "picture too large".to_owned());
        Ok(Picture { data: bytes.to_vec(), width: int(width)?, height: int(height)?, stride: int(stride)? })
    }
}

/// What a loaded picture was made from: the URI and the size an SVG was
/// rendered for.
type Source = (String, Option<(u32, u32)>);

#[derive(Default)]
struct State {
    /// The picture now being shown or loaded.
    wanted: Option<Source>,
    /// The texture of the last picture that loaded, and where it came from.
    texture: Option<(Source, glib::Object)>,
    actors: Vec<*mut ClutterActor>,
}

pub struct Background {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    group: *mut ClutterActor,
    background: glib::Object, // MetaBackground
    settings: Option<gio::Settings>,
    interface: Option<gio::Settings>,
    style: Cell<c_int>,
    /// Counts load requests, so that a slow, outdated one is ignored.
    generation: Cell<u64>,
    state: RefCell<State>,
}

fn settings(schema: &str) -> Option<gio::Settings> {
    gio::SettingsSchemaSource::default()
        .and_then(|s| s.lookup(schema, true))
        .map(|_| gio::Settings::new(schema))
}

impl Background {
    /// Add the background to `stage`, below `sibling`. It has to live on the
    /// stage, not inside the window group: mutter restacks that group by
    /// lowering window actors to the bottom and leaves unknown children in
    /// place, which would put it above all windows after a restack.
    pub fn new(display: *mut MetaDisplay, stage: *mut ClutterActor, sibling: *mut ClutterActor) -> Rc<Self> {
        let (group, background) = unsafe {
            let group = ffi::meta_background_group_new();
            ffi::clutter_actor_insert_child_below(stage, group, sibling);
            let background: glib::Object = from_glib_full(ffi::meta_background_new(display) as *mut gobject_sys::GObject);
            (group, background)
        };
        let this = Rc::new(Self {
            display,
            stage,
            group,
            background,
            settings: settings(SCHEMA),
            interface: settings(INTERFACE_SCHEMA),
            style: Cell::new(ffi::G_DESKTOP_BACKGROUND_STYLE_ZOOM),
            generation: Cell::new(0),
            state: RefCell::default(),
        });

        if let Some(s) = &this.settings {
            let weak = Rc::downgrade(&this);
            s.connect_changed(None, move |_, _| Self::update_weak(&weak));
        }
        if let Some(s) = &this.interface {
            let weak = Rc::downgrade(&this);
            s.connect_changed(Some("color-scheme"), move |_, _| Self::update_weak(&weak));
        }
        this.relayout();
        this
    }

    fn update_weak(weak: &Weak<Self>) {
        if let Some(this) = weak.upgrade() {
            this.update();
        }
    }

    fn background_ptr(&self) -> *mut ffi::MetaBackground {
        self.background.as_ptr() as *mut ffi::MetaBackground
    }

    /// The monitors changed: make one actor for each.
    pub fn relayout(self: &Rc<Self>) {
        // Not kept borrowed: destroying actors can run other code.
        let old = std::mem::take(&mut self.state.borrow_mut().actors);
        let mut actors = Vec::new();
        unsafe {
            for actor in old {
                ffi::clutter_actor_destroy(actor);
            }
            for monitor in 0..ffi::meta_display_get_n_monitors(self.display) {
                let mut r = MtkRectangle::default();
                ffi::meta_display_get_monitor_geometry(self.display, monitor, &mut r);
                let actor = ffi::meta_background_actor_new(self.display, monitor);
                ffi::clutter_actor_set_position(actor, r.x as f32, r.y as f32);
                ffi::clutter_actor_set_size(actor, r.width as f32, r.height as f32);
                let content = ffi::clutter_actor_get_content(actor) as *mut ffi::MetaBackgroundContent;
                ffi::meta_background_content_set_background(content, self.background_ptr());
                ffi::clutter_actor_add_child(self.group, actor);
                actors.push(actor);
            }
        }
        self.state.borrow_mut().actors = actors;
        self.update();
    }

    /// The pixel size an SVG has to cover: the largest monitor, or all of them
    /// together for a spanned picture.
    fn svg_target(&self, style: c_int) -> Option<(u32, u32)> {
        if style == ffi::G_DESKTOP_BACKGROUND_STYLE_CENTERED || style == ffi::G_DESKTOP_BACKGROUND_STYLE_WALLPAPER {
            return None;
        }
        let (mut width, mut height) = (0.0f32, 0.0f32);
        unsafe {
            for monitor in 0..ffi::meta_display_get_n_monitors(self.display) {
                let mut r = MtkRectangle::default();
                ffi::meta_display_get_monitor_geometry(self.display, monitor, &mut r);
                let scale = ffi::meta_display_get_monitor_scale(self.display, monitor);
                let (w, h) = (r.width as f32 * scale, r.height as f32 * scale);
                if style == ffi::G_DESKTOP_BACKGROUND_STYLE_SPANNED {
                    width = width.max((r.x + r.width) as f32 * scale);
                    height = height.max((r.y + r.height) as f32 * scale);
                } else {
                    width = width.max(w);
                    height = height.max(h);
                }
            }
        }
        (width >= 1.0 && height >= 1.0).then_some((width.ceil() as u32, height.ceil() as u32))
    }

    /// The URI of the picture for the current color scheme.
    fn picture_uri(&self, settings: &gio::Settings) -> String {
        let dark = self.interface.as_ref().is_some_and(|i| i.string("color-scheme") == "prefer-dark");
        let has_dark_key = settings.settings_schema().is_some_and(|s| s.has_key("picture-uri-dark"));
        let key = if dark && has_dark_key { "picture-uri-dark" } else { "picture-uri" };
        settings.string(key).to_string()
    }

    /// Apply the settings: colors and style at once, a new picture when it has
    /// loaded. The old picture stays until then.
    fn update(self: &Rc<Self>) {
        let Some(settings) = &self.settings else {
            unsafe {
                ffi::meta_background_set_gradient(
                    self.background_ptr(),
                    ffi::G_DESKTOP_BACKGROUND_SHADING_SOLID,
                    &FALLBACK_COLOR,
                    &FALLBACK_COLOR,
                )
            };
            return;
        };
        let color = |key: &str| parse_color(&settings.string(key)).unwrap_or(FALLBACK_COLOR);
        let (primary, secondary) = (color("primary-color"), color("secondary-color"));
        let shading = shading_from_type(&settings.string("color-shading-type"));
        unsafe { ffi::meta_background_set_gradient(self.background_ptr(), shading, &primary, &secondary) };

        let style = style_from_options(&settings.string("picture-options"));
        self.style.set(style);
        let uri = self.picture_uri(settings);
        if style == ffi::G_DESKTOP_BACKGROUND_STYLE_NONE || uri.is_empty() {
            self.generation.set(self.generation.get() + 1);
            let mut state = self.state.borrow_mut();
            state.wanted = None;
            state.texture = None;
            drop(state);
            self.show_texture();
            return;
        }

        let svg_target = if uri.to_ascii_lowercase().ends_with(".svg") { self.svg_target(style) } else { None };
        let source: Source = (uri, svg_target);
        self.show_texture(); // the style may have changed
        if self.state.borrow().wanted.as_ref() == Some(&source) {
            return;
        }
        self.state.borrow_mut().wanted = Some(source.clone());
        self.load(source);
    }

    fn load(self: &Rc<Self>, source: Source) {
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let (uri, svg_target) = source.clone();
            let result = gio::spawn_blocking(move || decode(&uri, svg_target))
                .await
                .unwrap_or_else(|_| Err("the loader thread panicked".to_owned()));
            let Some(this) = weak.upgrade() else { return };
            if this.generation.get() != generation {
                return;
            }
            let texture = result.and_then(|picture| this.upload(&picture));
            match texture {
                Ok(texture) => {
                    glib::g_debug!("gnutshell", "background: loaded {}", source.0);
                    this.state.borrow_mut().texture = Some((source, texture));
                }
                Err(e) => {
                    glib::g_warning!("gnutshell", "background {}: {e}", source.0);
                    this.state.borrow_mut().texture = None;
                }
            }
            this.show_texture();
        });
    }

    fn upload(&self, picture: &Picture) -> Result<glib::Object, String> {
        unsafe {
            let backend = ffi::clutter_context_get_backend(ffi::clutter_actor_get_context(self.stage));
            let ctx = ffi::clutter_backend_get_cogl_context(backend);
            let mut err = std::ptr::null_mut();
            let texture = ffi::cogl_texture_2d_new_from_data(
                ctx,
                picture.width,
                picture.height,
                ffi::COGL_PIXEL_FORMAT_RGBA_8888_PRE,
                picture.stride,
                picture.data.as_ptr(),
                &mut err,
            );
            if texture.is_null() {
                let e: glib::Error = from_glib_full(err);
                return Err(e.to_string());
            }
            Ok(from_glib_full(texture as *mut gobject_sys::GObject))
        }
    }

    fn show_texture(&self) {
        let texture = self.state.borrow().texture.as_ref().map(|(_, t)| t.clone());
        let ptr = texture
            .as_ref()
            .map_or(std::ptr::null_mut(), |t| t.as_ptr() as *mut ffi::CoglTexture);
        unsafe {
            ffi::meta_background_set_texture(self.background_ptr(), ptr, self.style.get(), std::ptr::null_mut());
        }
    }

    pub fn destroy(&self) {
        self.generation.set(self.generation.get() + 1);
        self.state.borrow_mut().actors.clear();
        unsafe { ffi::clutter_actor_destroy(self.group) }; // and its actors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(text: &str) -> Option<(u8, u8, u8)> {
        parse_color(text).map(|c| (c.red, c.green, c.blue))
    }

    #[test]
    fn reads_the_color_forms() {
        assert_eq!(rgb("#023c88"), Some((0x02, 0x3c, 0x88)));
        assert_eq!(rgb("#fa0"), Some((0xff, 0xaa, 0x00)));
        assert_eq!(rgb("#02023c3c8888"), Some((0x02, 0x3c, 0x88)));
        assert_eq!(rgb("023c88"), None);
        assert_eq!(rgb("#12345"), None);
        assert_eq!(rgb("#+f+f+f"), None);
        assert_eq!(rgb("#ééé"), None);
    }

    #[test]
    fn maps_the_settings_values() {
        assert_eq!(style_from_options("none"), ffi::G_DESKTOP_BACKGROUND_STYLE_NONE);
        assert_eq!(style_from_options("spanned"), ffi::G_DESKTOP_BACKGROUND_STYLE_SPANNED);
        assert_eq!(style_from_options("zoom"), ffi::G_DESKTOP_BACKGROUND_STYLE_ZOOM);
        assert_eq!(shading_from_type("vertical"), ffi::G_DESKTOP_BACKGROUND_SHADING_VERTICAL);
        assert_eq!(shading_from_type("solid"), ffi::G_DESKTOP_BACKGROUND_SHADING_SOLID);
    }

    #[test]
    fn renders_an_svg_large_enough_to_cover_the_monitor() {
        assert_eq!(cover_size(100, 100, (1920, 1080)), (1920, 1920));
        assert_eq!(cover_size(400, 100, (1920, 1080)), (4320, 1080));
        assert_eq!(cover_size(0, 0, (1920, 1080)), (1920, 1080));
        assert_eq!(cover_size(10000, 1, (1920, 1080)), (MAX_SVG_SIDE, 1080));
    }
}
