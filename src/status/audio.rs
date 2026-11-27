//! The default output's volume through libpulse (PipeWire provides the
//! PulseAudio protocol), driven by the GLib main loop.

use super::outputs::{self, Card, Output, Port, Profile, Sink, Target};
use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
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
const SUBSCRIPTION_MASK_CARD: c_int = 0x200;
const PORT_UNAVAILABLE: c_int = 1;
const DIRECTION_OUTPUT: c_int = 1;
/// How long a profile switch may take to bring up the wanted output.
const SWITCH_TIMEOUT: Duration = Duration::from_secs(5);
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
    monitor_source: u32,
    monitor_source_name: *const c_char,
    latency: u64,
    driver: *const c_char,
    flags: c_int,
    proplist: *const c_void,
    configured_latency: u64,
    base_volume: u32,
    state: c_int,
    n_volume_steps: u32,
    card: u32,
    n_ports: u32,
    ports: *const *const NamedPort,
    active_port: *const NamedPort,
}

/// The start of `pa_sink_port_info`.
#[repr(C)]
struct NamedPort {
    name: *const c_char,
}

/// The start of `pa_card_info`.
#[repr(C)]
struct CardInfo {
    index: u32,
    name: *const c_char,
    owner_module: u32,
    driver: *const c_char,
    n_profiles: u32,
    profiles: *const c_void,
    active_profile: *const c_void,
    proplist: *const c_void,
    n_ports: u32,
    ports: *const *const CardPortInfo,
    profiles2: *const *const ProfileInfo,
    active_profile2: *const ProfileInfo,
}

/// The start of `pa_card_port_info`.
#[repr(C)]
struct CardPortInfo {
    name: *const c_char,
    description: *const c_char,
    priority: u32,
    available: c_int,
    direction: c_int,
    n_profiles: u32,
    profiles: *const c_void,
    proplist: *const c_void,
    latency_offset: i64,
    profiles2: *const *const ProfileInfo,
}

/// `pa_card_profile_info2`.
#[repr(C)]
struct ProfileInfo {
    name: *const c_char,
    description: *const c_char,
    n_sinks: u32,
    n_sources: u32,
    priority: u32,
    available: c_int,
}

// Checked against pulse/introspect.h.
const _: () = assert!(std::mem::offset_of!(SinkInfo, volume) == 172 || cfg!(not(target_pointer_width = "64")));
const _: () = assert!(std::mem::offset_of!(SinkInfo, mute) == 304 || cfg!(not(target_pointer_width = "64")));
#[cfg(target_pointer_width = "64")]
const _: () = {
    use std::mem::offset_of;
    assert!(offset_of!(SinkInfo, card) == 372);
    assert!(offset_of!(SinkInfo, ports) == 384);
    assert!(offset_of!(SinkInfo, active_port) == 392);
    assert!(offset_of!(CardInfo, proplist) == 56);
    assert!(offset_of!(CardInfo, ports) == 72);
    assert!(offset_of!(CardInfo, active_profile2) == 88);
    assert!(offset_of!(CardPortInfo, direction) == 24);
    assert!(offset_of!(CardPortInfo, proplist) == 40);
    assert!(offset_of!(CardPortInfo, profiles2) == 56);
    assert!(offset_of!(ProfileInfo, available) == 28);
};

type Context = c_void;
type Operation = c_void;
type ContextNotify = Option<unsafe extern "C" fn(*mut Context, *mut c_void)>;
type SubscribeNotify = Option<unsafe extern "C" fn(*mut Context, c_int, u32, *mut c_void)>;
type SinkInfoNotify = Option<unsafe extern "C" fn(*mut Context, *const SinkInfo, c_int, *mut c_void)>;
type CardInfoNotify = Option<unsafe extern "C" fn(*mut Context, *const CardInfo, c_int, *mut c_void)>;
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
    fn pa_context_get_sink_info_list(c: *mut Context, cb: SinkInfoNotify, userdata: *mut c_void) -> *mut Operation;
    fn pa_context_get_card_info_list(c: *mut Context, cb: CardInfoNotify, userdata: *mut c_void) -> *mut Operation;
    fn pa_context_set_default_sink(
        c: *mut Context,
        name: *const c_char,
        cb: SuccessNotify,
        userdata: *mut c_void,
    ) -> *mut Operation;
    fn pa_context_set_sink_port_by_name(
        c: *mut Context,
        sink: *const c_char,
        port: *const c_char,
        cb: SuccessNotify,
        userdata: *mut c_void,
    ) -> *mut Operation;
    fn pa_context_set_card_profile_by_index(
        c: *mut Context,
        index: u32,
        profile: *const c_char,
        cb: SuccessNotify,
        userdata: *mut c_void,
    ) -> *mut Operation;
    fn pa_proplist_gets(proplist: *const c_void, key: *const c_char) -> *const c_char;
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
    /// Sinks and cards, and the default sink's name. The `incoming` lists
    /// collect a reply until it is complete.
    sinks: RefCell<Vec<Sink>>,
    cards: RefCell<Vec<Card>>,
    incoming_sinks: RefCell<Vec<Sink>>,
    incoming_cards: RefCell<Vec<Card>>,
    default_sink: RefCell<String>,
    /// A (card, port) to switch to once the new profile brings up its sink.
    pending: RefCell<Option<(u32, String, std::time::Instant)>>,
    /// Called after any change; `None` while there is no sound server or no
    /// output.
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
            sinks: RefCell::default(),
            cards: RefCell::default(),
            incoming_sinks: RefCell::default(),
            incoming_cards: RefCell::default(),
            default_sink: RefCell::default(),
            pending: RefCell::default(),
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
                self.incoming_sinks.borrow_mut().clear();
                self.incoming_cards.borrow_mut().clear();
                done(pa_context_get_card_info_list(context, Some(on_card_item), self.userdata()));
                done(pa_context_get_sink_info_list(context, Some(on_sink_item), self.userdata()));
            }
        }
    }

    /// The outputs to choose from.
    pub fn outputs(&self) -> Vec<Output> {
        outputs::list(&self.cards.borrow(), &self.sinks.borrow(), &self.default_sink.borrow())
    }

    /// Make `target` the default output.
    pub fn select(&self, target: &Target) {
        self.pending.borrow_mut().take();
        match target {
            Target::Sink(name) => self.use_sink(name, None),
            Target::Port { card, port } => {
                let sink = outputs::sink_with_port(&self.sinks.borrow(), *card, port).map(|s| s.name.clone());
                match sink {
                    Some(sink) => self.use_sink(&sink, Some(port)),
                    None => self.switch_profile(*card, port),
                }
            }
        }
    }

    fn use_sink(&self, sink: &str, port: Option<&str>) {
        let Some(context) = self.ready() else { return };
        let Ok(sink) = CString::new(sink) else { return };
        unsafe {
            if let Some(port) = port.and_then(|p| CString::new(p).ok()) {
                done(pa_context_set_sink_port_by_name(
                    context,
                    sink.as_ptr(),
                    port.as_ptr(),
                    None,
                    std::ptr::null_mut(),
                ));
            }
            done(pa_context_set_default_sink(context, sink.as_ptr(), None, std::ptr::null_mut()));
        }
    }

    fn switch_profile(&self, card: u32, port: &str) {
        let Some(context) = self.ready() else { return };
        let profile = {
            let cards = self.cards.borrow();
            cards.iter().find(|c| c.index == card).and_then(|c| {
                let port = c.ports.iter().find(|p| p.name == port)?;
                outputs::profile_for(c, port).map(str::to_owned)
            })
        };
        let Some(profile) = profile.and_then(|p| CString::new(p).ok()) else {
            glib::g_warning!("gnutshell", "audio: no usable profile for {port}");
            return;
        };
        glib::g_debug!("gnutshell", "audio: switching card {card} to {profile:?} for {port}");
        *self.pending.borrow_mut() = Some((card, port.to_owned(), std::time::Instant::now()));
        unsafe {
            done(pa_context_set_card_profile_by_index(context, card, profile.as_ptr(), None, std::ptr::null_mut()));
        }
    }

    /// Finish a profile switch if its sink is there now.
    fn finish_switch(&self) {
        let Some((card, port, since)) = self.pending.borrow().clone() else { return };
        let sink = outputs::sink_with_port(&self.sinks.borrow(), card, &port).map(|s| s.name.clone());
        if since.elapsed() > SWITCH_TIMEOUT {
            self.pending.borrow_mut().take();
        } else if let Some(sink) = sink {
            self.pending.borrow_mut().take();
            self.use_sink(&sink, Some(&port));
        }
    }

    /// Change the volume by `delta` (0.05 is five points).
    pub fn change_volume(&self, delta: f64) {
        let Some((volume, _)) = *self.sink.borrow() else { return };
        self.apply(stepped(loudest(&volume), delta), delta > 0.0);
    }

    /// Set the volume, 0.0 to 1.0.
    pub fn set_level(&self, level: f64) {
        let Some((volume, _)) = *self.sink.borrow() else { return };
        let target = (level.clamp(0.0, 1.0) * f64::from(VOLUME_NORM)).round() as u32;
        self.apply(target, target > loudest(&volume));
    }

    fn apply(&self, target: u32, louder: bool) {
        let Some((volume, muted)) = *self.sink.borrow() else { return };
        let Some(context) = self.ready() else { return };
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
        if muted && louder {
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
                    SUBSCRIPTION_MASK_SINK | SUBSCRIPTION_MASK_SERVER | SUBSCRIPTION_MASK_CARD,
                    None,
                    std::ptr::null_mut(),
                ));
            }
            audio.query();
        }
        CONTEXT_FAILED | CONTEXT_TERMINATED => {
            *audio.sink.borrow_mut() = None;
            audio.sinks.borrow_mut().clear();
            audio.cards.borrow_mut().clear();
            audio.default_sink.borrow_mut().clear();
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
        audio.default_sink.borrow_mut().clear();
    } else if eol == 0 && !info.is_null() {
        let info = unsafe { &*info };
        *audio.default_sink.borrow_mut() = unsafe { text(info.name) };
        *audio.sink.borrow_mut() = Some((info.volume, info.mute != 0));
    } else {
        return;
    }
    audio.publish();
}

unsafe fn text(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

/// A C array of `n` pointers, as references.
unsafe fn items<'a, T: 'a>(array: *const *const T, n: u32) -> impl Iterator<Item = &'a T> {
    let n = if array.is_null() { 0 } else { n as usize };
    (0..n).filter_map(move |i| unsafe { (*array.add(i)).as_ref() })
}

unsafe extern "C" fn on_sink_item(_context: *mut Context, info: *const SinkInfo, eol: c_int, userdata: *mut c_void) {
    let audio = unsafe { &*(userdata as *const Audio) };
    if eol != 0 {
        if eol > 0 {
            let sinks = std::mem::take(&mut *audio.incoming_sinks.borrow_mut());
            *audio.sinks.borrow_mut() = sinks;
            audio.finish_switch();
            audio.publish();
        }
        return;
    }
    let Some(info) = (unsafe { info.as_ref() }) else { return };
    let sink = unsafe {
        Sink {
            name: text(info.name),
            description: text(info.description),
            card: info.card,
            ports: items(info.ports, info.n_ports).map(|p| text(p.name)).collect(),
            active_port: info.active_port.as_ref().map(|p| text(p.name)).unwrap_or_default(),
        }
    };
    audio.incoming_sinks.borrow_mut().push(sink);
}

unsafe extern "C" fn on_card_item(_context: *mut Context, info: *const CardInfo, eol: c_int, userdata: *mut c_void) {
    let audio = unsafe { &*(userdata as *const Audio) };
    if eol != 0 {
        if eol > 0 {
            let cards = std::mem::take(&mut *audio.incoming_cards.borrow_mut());
            *audio.cards.borrow_mut() = cards;
        }
        return;
    }
    let Some(info) = (unsafe { info.as_ref() }) else { return };
    let property = |list: *const c_void, key: &CStr| {
        if list.is_null() { String::new() } else { unsafe { text(pa_proplist_gets(list, key.as_ptr())) } }
    };
    let ports = unsafe { items(info.ports, info.n_ports) }
        .filter(|p| p.direction == DIRECTION_OUTPUT)
        .map(|p| unsafe {
            Port {
                name: text(p.name),
                description: text(p.description),
                product: property(p.proplist, c"device.product.name"),
                plugged: p.available != PORT_UNAVAILABLE,
                profiles: items(p.profiles2, p.n_profiles)
                    .map(|q| Profile { name: text(q.name), priority: q.priority, available: q.available != 0 })
                    .collect(),
            }
        })
        .collect();
    let card = Card {
        index: info.index,
        description: property(info.proplist, c"device.description"),
        active_profile: unsafe { info.active_profile2.as_ref().map(|p| text(p.name)).unwrap_or_default() },
        ports,
    };
    audio.incoming_cards.borrow_mut().push(card);
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
