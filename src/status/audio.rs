//! The default output's volume through libpulse (PipeWire provides the
//! PulseAudio protocol), driven by the GLib main loop.

use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_char, c_int, c_void};
use std::rc::{Rc, Weak};
use std::time::Duration;

/// `PA_VOLUME_NORM`: 100 %.
const VOLUME_NORM: u32 = 0x10000;
const CHANNELS_MAX: usize = 32;
const DEFAULT_SINK: &CStr = c"@DEFAULT_SINK@";

const CONTEXT_NOFAIL: c_int = 2;
const CONTEXT_READY: c_int = 4;
const CONTEXT_FAILED: c_int = 5;
const CONTEXT_TERMINATED: c_int = 6;
const SUBSCRIPTION_MASK_SINK: c_int = 0x1;
const SUBSCRIPTION_MASK_SERVER: c_int = 0x80;
const RECONNECT_DELAY: Duration = Duration::from_secs(2);

/// `pa_cvolume`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CVolume {
    channels: u8,
    values: [u32; CHANNELS_MAX],
}

/// `pa_sample_spec`. Nested, not flattened: its tail padding matters.
#[repr(C)]
struct SampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

/// `pa_channel_map`.
#[repr(C)]
struct ChannelMap {
    channels: u8,
    map: [c_int; CHANNELS_MAX],
}

/// The start of `pa_sink_info`, as far as we read it.
#[repr(C)]
struct SinkInfo {
    name: *const c_char,
    index: u32,
    description: *const c_char,
    sample_spec: SampleSpec,
    channel_map: ChannelMap,
    owner_module: u32,
    volume: CVolume,
    mute: c_int,
}

// Checked against pulse/introspect.h.
const _: () = assert!(std::mem::offset_of!(SinkInfo, volume) == 172 || cfg!(not(target_pointer_width = "64")));
const _: () = assert!(std::mem::offset_of!(SinkInfo, mute) == 304 || cfg!(not(target_pointer_width = "64")));

type Context = c_void;
type Operation = c_void;
type ContextNotify = Option<unsafe extern "C" fn(*mut Context, *mut c_void)>;
type SubscribeNotify = Option<unsafe extern "C" fn(*mut Context, c_int, u32, *mut c_void)>;
type SinkInfoNotify = Option<unsafe extern "C" fn(*mut Context, *const SinkInfo, c_int, *mut c_void)>;
type SuccessNotify = Option<unsafe extern "C" fn(*mut Context, c_int, *mut c_void)>;

unsafe extern "C" {
    fn pa_glib_mainloop_new(context: *mut glib_sys::GMainContext) -> *mut c_void;
    fn pa_glib_mainloop_free(mainloop: *mut c_void);
    fn pa_glib_mainloop_get_api(mainloop: *mut c_void) -> *mut c_void;
    fn pa_context_new(api: *mut c_void, name: *const c_char) -> *mut Context;
    fn pa_context_connect(c: *mut Context, server: *const c_char, flags: c_int, api: *const c_void) -> c_int;
    fn pa_context_disconnect(c: *mut Context);
    fn pa_context_unref(c: *mut Context);
    fn pa_context_get_state(c: *const Context) -> c_int;
    fn pa_context_set_state_callback(c: *mut Context, cb: ContextNotify, userdata: *mut c_void);
    fn pa_context_set_subscribe_callback(c: *mut Context, cb: SubscribeNotify, userdata: *mut c_void);
    fn pa_context_subscribe(c: *mut Context, mask: c_int, cb: SuccessNotify, userdata: *mut c_void) -> *mut Operation;
    fn pa_context_get_sink_info_by_name(
        c: *mut Context,
        name: *const c_char,
        cb: SinkInfoNotify,
        userdata: *mut c_void,
    ) -> *mut Operation;
    fn pa_context_set_sink_volume_by_name(
        c: *mut Context,
        name: *const c_char,
        volume: *const CVolume,
        cb: SuccessNotify,
        userdata: *mut c_void,
    ) -> *mut Operation;
    fn pa_context_set_sink_mute_by_name(
        c: *mut Context,
        name: *const c_char,
        mute: c_int,
        cb: SuccessNotify,
        userdata: *mut c_void,
    ) -> *mut Operation;
    fn pa_operation_unref(o: *mut Operation);
}

/// What the panel shows about the default output.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Volume {
    /// 1.0 is 100 %.
    pub level: f64,
    pub muted: bool,
}

pub struct Audio {
    mainloop: *mut c_void,
    context: Cell<*mut Context>,
    /// The sink's per-channel volumes, to keep the balance when changing it.
    sink: RefCell<Option<(CVolume, bool)>>,
    /// `None` while there is no sound server or no output.
    changed: Box<dyn Fn(Option<Volume>)>,
    reconnect: RefCell<Option<glib::SourceId>>,
    this: Weak<Audio>,
}

impl Audio {
    pub fn new(changed: Box<dyn Fn(Option<Volume>)>) -> Rc<Self> {
        let mainloop = unsafe { pa_glib_mainloop_new(std::ptr::null_mut()) };
        let audio = Rc::new_cyclic(|this| Audio {
            mainloop,
            context: Cell::new(std::ptr::null_mut()),
            sink: RefCell::default(),
            changed,
            reconnect: RefCell::default(),
            this: this.clone(),
        });
        audio.connect();
        audio
    }

    /// Callbacks get a pointer to `self`: `drop_context` runs before it goes.
    fn userdata(&self) -> *mut c_void {
        self as *const Audio as *mut c_void
    }

    fn connect(&self) {
        self.drop_context();
        unsafe {
            let context = pa_context_new(pa_glib_mainloop_get_api(self.mainloop), c"gnutshell".as_ptr());
            if context.is_null() {
                glib::g_warning!("gnutshell", "audio: could not create a PulseAudio context");
                return;
            }
            self.context.set(context);
            pa_context_set_state_callback(context, Some(on_state), self.userdata());
            // NOFAIL: wait for the server if it isn't up yet.
            if pa_context_connect(context, std::ptr::null(), CONTEXT_NOFAIL, std::ptr::null()) < 0 {
                glib::g_warning!("gnutshell", "audio: could not connect to the sound server");
            }
        }
    }

    fn drop_context(&self) {
        let context = self.context.replace(std::ptr::null_mut());
        if !context.is_null() {
            unsafe {
                pa_context_set_state_callback(context, None, std::ptr::null_mut());
                pa_context_set_subscribe_callback(context, None, std::ptr::null_mut());
                pa_context_disconnect(context);
                pa_context_unref(context);
            }
        }
    }

    /// The context, if it can take requests.
    fn ready(&self) -> Option<*mut Context> {
        let context = self.context.get();
        (!context.is_null() && unsafe { pa_context_get_state(context) } == CONTEXT_READY).then_some(context)
    }

    fn query(&self) {
        if let Some(context) = self.ready() {
            unsafe {
                done(pa_context_get_sink_info_by_name(context, DEFAULT_SINK.as_ptr(), Some(on_sink), self.userdata()));
            }
        }
    }

    /// Change the volume by `delta` (0.05 is five points).
    pub fn change_volume(&self, delta: f64) {
        let Some((volume, muted)) = *self.sink.borrow() else { return };
        let Some(context) = self.ready() else { return };
        let target = stepped(loudest(&volume), delta);
        let scaled = scale(volume, target);
        // Keep the cache current, so quick scroll steps add up.
        *self.sink.borrow_mut() = Some((scaled, muted));
        unsafe {
            done(pa_context_set_sink_volume_by_name(
                context,
                DEFAULT_SINK.as_ptr(),
                &scaled,
                None,
                std::ptr::null_mut(),
            ));
        }
        // Turning it up unmutes, as in gnome-shell.
        if muted && delta > 0.0 {
            self.set_muted(false);
        }
    }

    pub fn toggle_mute(&self) {
        let muted = self.sink.borrow().map(|(_, muted)| muted);
        if let Some(muted) = muted {
            self.set_muted(!muted);
        }
    }

    fn set_muted(&self, muted: bool) {
        let Some(context) = self.ready() else { return };
        if let Some(sink) = self.sink.borrow_mut().as_mut() {
            sink.1 = muted;
        }
        unsafe {
            done(pa_context_set_sink_mute_by_name(
                context,
                DEFAULT_SINK.as_ptr(),
                c_int::from(muted),
                None,
                std::ptr::null_mut(),
            ));
        }
    }

    /// The current volume, including changes still on their way.
    pub fn volume(&self) -> Option<Volume> {
        self.sink.borrow().map(|(v, muted)| Volume { level: f64::from(loudest(&v)) / f64::from(VOLUME_NORM), muted })
    }

    fn publish(&self) {
        (self.changed)(self.volume());
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        if let Some(id) = self.reconnect.borrow_mut().take() {
            id.remove();
        }
        self.drop_context();
        unsafe { pa_glib_mainloop_free(self.mainloop) };
    }
}

/// We never wait for operations.
unsafe fn done(operation: *mut Operation) {
    if !operation.is_null() {
        unsafe { pa_operation_unref(operation) };
    }
}

unsafe extern "C" fn on_state(context: *mut Context, userdata: *mut c_void) {
    let audio = unsafe { &*(userdata as *const Audio) };
    match unsafe { pa_context_get_state(context) } {
        CONTEXT_READY => {
            unsafe {
                pa_context_set_subscribe_callback(context, Some(on_event), userdata);
                done(pa_context_subscribe(
                    context,
                    SUBSCRIPTION_MASK_SINK | SUBSCRIPTION_MASK_SERVER,
                    None,
                    std::ptr::null_mut(),
                ));
            }
            audio.query();
        }
        CONTEXT_FAILED | CONTEXT_TERMINATED => {
            *audio.sink.borrow_mut() = None;
            audio.publish();
            // Not from inside the callback: the context is still in use.
            if audio.reconnect.borrow().is_none() {
                let weak = audio.this.clone();
                let id = glib::timeout_add_local_once(RECONNECT_DELAY, move || {
                    if let Some(audio) = weak.upgrade() {
                        audio.reconnect.borrow_mut().take();
                        audio.connect();
                    }
                });
                *audio.reconnect.borrow_mut() = Some(id);
            }
        }
        _ => {}
    }
}

unsafe extern "C" fn on_event(_context: *mut Context, _event: c_int, _index: u32, userdata: *mut c_void) {
    unsafe { &*(userdata as *const Audio) }.query();
}

unsafe extern "C" fn on_sink(_context: *mut Context, info: *const SinkInfo, eol: c_int, userdata: *mut c_void) {
    let audio = unsafe { &*(userdata as *const Audio) };
    if eol < 0 {
        // No default output.
        *audio.sink.borrow_mut() = None;
    } else if eol == 0 && !info.is_null() {
        let info = unsafe { &*info };
        *audio.sink.borrow_mut() = Some((info.volume, info.mute != 0));
    } else {
        return;
    }
    audio.publish();
}

fn loudest(v: &CVolume) -> u32 {
    v.values.iter().take(usize::from(v.channels).min(CHANNELS_MAX)).copied().max().unwrap_or(0)
}

/// `current` moved by `delta`. Steps up stop at 100 %; a volume already above
/// that (set elsewhere) is never raised further, nor cut down by a step up.
fn stepped(current: u32, delta: f64) -> u32 {
    let target = (f64::from(current) + delta * f64::from(VOLUME_NORM)).round().max(0.0) as u32;
    if delta > 0.0 { target.min(VOLUME_NORM.max(current)) } else { target }
}

/// `volume` with its loudest channel at `target`, keeping the balance.
fn scale(mut volume: CVolume, target: u32) -> CVolume {
    let max = loudest(&volume);
    let channels = usize::from(volume.channels).min(CHANNELS_MAX);
    for value in volume.values.iter_mut().take(channels) {
        *value = if max == 0 {
            target
        } else {
            ((u64::from(*value) * u64::from(target) + u64::from(max) / 2) / u64::from(max)) as u32
        };
    }
    volume
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_stay_in_range() {
        let pct = |p: u32| VOLUME_NORM * p / 100;
        assert_eq!(stepped(pct(98), 0.05), VOLUME_NORM);
        assert_eq!(stepped(pct(2), -0.05), 0);
        assert_eq!(stepped(pct(130), 0.05), pct(130));
        assert!(stepped(pct(130), -0.05) < pct(130));
    }

    #[test]
    fn scaling_keeps_balance() {
        let mut v = CVolume { channels: 2, values: [0; CHANNELS_MAX] };
        v.values[..2].copy_from_slice(&[40000, 20000]);
        assert_eq!(scale(v, 20000).values[..3], [20000, 10000, 0]);
        v.values[..2].copy_from_slice(&[0, 0]);
        assert_eq!(scale(v, 300).values[..2], [300, 300]);
    }
}
