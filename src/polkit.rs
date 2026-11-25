//! Polkit authentication agent: the "Authentication Required" password dialog.
//!
//! `GnativeAgentListener` subclasses libpolkit-agent-1's `PolkitAgentListener`.
//! polkitd calls `initiate_authentication`; we show a modal Clutter dialog and
//! drive a `PolkitAgentSession`, which runs the setuid PAM helper. The password
//! goes from the entry straight to the helper; it is never stored by gnative.
//!
//! The agent registers for the login session of the gnative process. When
//! `GNATIVE_POLKIT_PID=<pid>` is set it registers for that single process
//! instead (like `pkttyagent --process`), which lets it run nested next to
//! gnome-shell, whose agent already owns the session.

use crate::ffi::{self, ClutterActor, ClutterText, CoglColor, MetaDisplay};
use crate::ui::{self, gobj};
use gio::prelude::*;
use glib::subclass::prelude::*;
use glib::translate::*;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{c_char, CStr, CString};
use std::rc::{Rc, Weak};

const OBJECT_PATH: &CStr = c"/org/gnome/gnative/AuthenticationAgent";

const DIALOG_WIDTH: f32 = 460.0;
const PADDING: f32 = 24.0;
const SPACING: f32 = 12.0;
const ENTRY_HEIGHT: f32 = 36.0;
const BUTTON_WIDTH: f32 = 130.0;
const BUTTON_HEIGHT: f32 = 36.0;

const TITLE_FONT: &CStr = c"Sans Bold 14";
const BODY_FONT: &CStr = ui::BODY_FONT;
const ENTRY_FONT: &CStr = c"Sans 12";

const ERROR_COLOR: CoglColor = CoglColor::rgba(0xff, 0x7b, 0x63, 0xff);
const ENTRY_BG: CoglColor = CoglColor::rgba(0x2a, 0x2a, 0x32, 0xff);

const BULLET: u32 = '\u{2022}' as u32;

// =============================================================================
// PolkitAgentListener subclassing
// =============================================================================

glib::wrapper! {
    pub struct AgentListener(Object<ffi::PolkitAgentListener, ffi::PolkitAgentListenerClass>);

    match fn {
        type_ => || ffi::polkit_agent_listener_get_type(),
    }
}

pub trait AgentListenerImpl: ObjectImpl + ObjectSubclass<Type: IsA<AgentListener>> {
    fn initiate_authentication(&self, request: Request);
}

unsafe impl<T: AgentListenerImpl> IsSubclassable<T> for AgentListener {
    fn class_init(class: &mut glib::Class<Self>) {
        Self::parent_class_init::<T>(class);
        let klass = class.as_mut();
        klass.initiate_authentication = Some(vfunc_initiate::<T>);
        klass.initiate_authentication_finish = Some(vfunc_initiate_finish);
    }
}

unsafe extern "C" fn vfunc_initiate<T: AgentListenerImpl>(
    listener: *mut ffi::PolkitAgentListener,
    action_id: *const c_char,
    message: *const c_char,
    _icon_name: *const c_char,
    _details: *mut ffi::PolkitDetails,
    cookie: *const c_char,
    identities: *mut glib_sys::GList,
    cancellable: *mut gio::ffi::GCancellable,
    callback: gio::ffi::GAsyncReadyCallback,
    user_data: glib_sys::gpointer,
) {
    unsafe {
        let task = Task(gio::ffi::g_task_new(
            listener as *mut gobject_sys::GObject,
            cancellable,
            callback,
            user_data,
        ));

        let mut ids = Vec::new();
        let mut node = identities;
        while !node.is_null() {
            ids.push(gobj((*node).data as *mut ffi::PolkitIdentity));
            node = (*node).next;
        }

        let request = Request {
            action_id: cstr(action_id),
            message: cstr(message),
            cookie: cstr(cookie),
            identities: ids,
            cancellable: (!cancellable.is_null()).then(|| from_glib_none(cancellable)),
            task,
        };
        let instance = &*(listener as *mut T::Instance);
        instance.imp().initiate_authentication(request);
    }
}

unsafe extern "C" fn vfunc_initiate_finish(
    _listener: *mut ffi::PolkitAgentListener,
    result: *mut gio::ffi::GAsyncResult,
    error: *mut *mut glib_sys::GError,
) -> glib_sys::gboolean {
    unsafe { gio::ffi::g_task_propagate_boolean(result as *mut gio::ffi::GTask, error) }
}

/// Completion handle for one polkit request. Must be resolved exactly once;
/// dropping it unresolved fails the request.
struct Task(*mut gio::ffi::GTask);

impl Task {
    fn succeed(mut self) {
        unsafe { gio::ffi::g_task_return_boolean(self.0, 1) };
        self.release();
    }

    fn fail(mut self, code: i32, message: &str) {
        self.fail_inner(code, message);
    }

    fn fail_inner(&mut self, code: i32, message: &str) {
        let msg = CString::new(message).unwrap_or_default();
        unsafe {
            let err = glib_sys::g_error_new_literal(ffi::polkit_error_quark(), code, msg.as_ptr());
            gio::ffi::g_task_return_error(self.0, err);
        }
        self.release();
    }

    fn release(&mut self) {
        if !self.0.is_null() {
            unsafe { gobject_sys::g_object_unref(self.0 as *mut gobject_sys::GObject) };
            self.0 = std::ptr::null_mut();
        }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        if !self.0.is_null() {
            self.fail_inner(ffi::POLKIT_ERROR_FAILED, "Authentication request dropped");
        }
    }
}

pub struct Request {
    action_id: String,
    message: String,
    cookie: String,
    identities: Vec<glib::Object>,
    cancellable: Option<gio::Cancellable>,
    task: Task,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct GnativeAgentListener {
        pub(super) dialog: RefCell<Option<Rc<AuthDialog>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for GnativeAgentListener {
        const NAME: &'static str = "GnativeAgentListener";
        type Type = super::GnativeAgentListener;
        type ParentType = super::AgentListener;
    }

    impl ObjectImpl for GnativeAgentListener {}

    impl AgentListenerImpl for GnativeAgentListener {
        fn initiate_authentication(&self, request: Request) {
            glib::g_message!("gnative", "polkit: authentication requested for {}", request.action_id);
            let dialog = self.dialog.borrow().clone();
            match dialog {
                Some(d) => d.submit(request),
                None => request.task.fail(ffi::POLKIT_ERROR_FAILED, "No authentication UI"),
            }
        }
    }
}

glib::wrapper! {
    pub struct GnativeAgentListener(ObjectSubclass<imp::GnativeAgentListener>) @extends AgentListener;
}

/// A registered agent. Dropping it unregisters from polkitd.
pub struct Agent {
    listener: GnativeAgentListener,
    handle: glib_sys::gpointer,
}

impl Agent {
    pub fn register(display: *mut MetaDisplay, stage: *mut ClutterActor) -> Option<Agent> {
        let listener: GnativeAgentListener = glib::Object::new();
        let dialog = AuthDialog::new(display, stage);
        *listener.imp().dialog.borrow_mut() = Some(dialog);

        unsafe {
            let mut err = std::ptr::null_mut();
            let (subject, what) = match std::env::var("GNATIVE_POLKIT_PID").ok().and_then(|p| p.parse().ok()) {
                Some(pid) => (
                    ffi::polkit_unix_process_new_for_owner(pid, 0, ffi::getuid() as i32),
                    format!("process {pid}"),
                ),
                None => {
                    let subject = ffi::polkit_unix_session_new_for_process_sync(
                        std::process::id() as i32,
                        std::ptr::null_mut(),
                        &mut err,
                    );
                    // Under gnome-session, gnative is a systemd user service,
                    // outside the login session's process scope. gnome-session
                    // exports the session id, like gnome-shell relies on.
                    match std::env::var("XDG_SESSION_ID").ok().filter(|_| subject.is_null()) {
                        Some(id) => {
                            glib_sys::g_clear_error(&mut err);
                            let cid = std::ffi::CString::new(id.clone()).unwrap_or_default();
                            (ffi::polkit_unix_session_new(cid.as_ptr()), format!("login session {id}"))
                        }
                        None => (subject, "login session".to_string()),
                    }
                }
            };
            if subject.is_null() {
                let e: glib::Error = from_glib_full(err);
                glib::g_warning!("gnative", "polkit: no subject to register for: {e}");
                if let Some(d) = listener.imp().dialog.borrow_mut().take() {
                    d.destroy();
                }
                return None;
            }

            let handle = ffi::polkit_agent_listener_register(
                listener.upcast_ref::<AgentListener>().as_ptr(),
                ffi::POLKIT_AGENT_REGISTER_FLAGS_NONE,
                subject,
                OBJECT_PATH.as_ptr(),
                std::ptr::null_mut(),
                &mut err,
            );
            gobject_sys::g_object_unref(subject as *mut gobject_sys::GObject);

            if handle.is_null() {
                let e: glib::Error = from_glib_full(err);
                glib::g_warning!("gnative", "polkit: agent not registered for {what}: {e}");
                if let Some(d) = listener.imp().dialog.borrow_mut().take() {
                    d.destroy();
                }
                return None;
            }
            glib::g_message!("gnative", "polkit: authentication agent registered for {what}");
            Some(Agent { listener, handle })
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        unsafe { ffi::polkit_agent_listener_unregister(self.handle) };
        if let Some(d) = self.listener.imp().dialog.borrow_mut().take() {
            d.destroy();
        }
    }
}

// =============================================================================
// Dialog
// =============================================================================

struct AuthDialog {
    display: *mut MetaDisplay,
    stage: *mut ClutterActor,
    overlay: *mut ClutterActor,
    panel: *mut ClutterActor,
    title: *mut ClutterActor,
    message: *mut ClutterActor,
    user: *mut ClutterActor,
    prompt: *mut ClutterActor,
    entry_bg: *mut ClutterActor,
    entry: *mut ClutterActor,
    status: *mut ClutterActor,
    cancel_button: *mut ClutterActor,
    ok_button: *mut ClutterActor,
    state: RefCell<DialogState>,
}

#[derive(Default)]
struct DialogState {
    active: Option<Active>,
    queue: VecDeque<Request>,
    grab: Option<*mut ffi::ClutterGrab>,
}

struct Active {
    request: Request,
    session: Option<(glib::Object, Vec<glib::SignalHandlerId>)>,
    cancelled: bool,
    daemon_cancel: Option<gio::CancelledHandlerId>,
}

impl AuthDialog {
    fn new(display: *mut MetaDisplay, stage: *mut ClutterActor) -> Rc<Self> {
        let dialog = unsafe {
            let overlay = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(overlay, &ui::DIM_COLOR);
            ffi::clutter_actor_set_reactive(overlay, 1);
            ffi::clutter_actor_hide(overlay);

            let panel = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(panel, &ui::BOX_COLOR);
            ffi::clutter_actor_add_child(overlay, panel);

            let title = ui::label(TITLE_FONT, "Authentication Required", &ui::TEXT_COLOR);
            let message = ui::label(BODY_FONT, "", &ui::TEXT_COLOR);
            ffi::clutter_text_set_line_wrap(message as *mut ClutterText, 1);
            let user = ui::label(BODY_FONT, "", &ui::HINT_COLOR);
            let prompt = ui::label(BODY_FONT, "", &ui::TEXT_COLOR);

            let entry_bg = ffi::clutter_actor_new();
            ffi::clutter_actor_set_background_color(entry_bg, &ENTRY_BG);
            let entry = ui::label(ENTRY_FONT, "", &ui::TEXT_COLOR);
            let text = entry as *mut ClutterText;
            ffi::clutter_text_set_activatable(text, 1);
            ffi::clutter_text_set_single_line_mode(text, 1);
            ffi::clutter_text_set_cursor_color(text, &ui::TEXT_COLOR);
            ffi::clutter_actor_set_reactive(entry, 1);
            ffi::clutter_actor_add_child(entry_bg, entry);

            let status = ui::label(BODY_FONT, "", &ERROR_COLOR);
            ffi::clutter_text_set_line_wrap(status as *mut ClutterText, 1);

            let cancel_button = ui::button("Cancel", BUTTON_WIDTH, BUTTON_HEIGHT, &ui::BUTTON_COLOR);
            let ok_button = ui::button("Authenticate", BUTTON_WIDTH, BUTTON_HEIGHT, &ui::SELECTED_COLOR);

            for a in [title, message, user, prompt, entry_bg, status, cancel_button, ok_button] {
                ffi::clutter_actor_add_child(panel, a);
            }
            ffi::clutter_actor_add_child(stage, overlay);

            Rc::new(Self {
                display,
                stage,
                overlay,
                panel,
                title,
                message,
                user,
                prompt,
                entry_bg,
                entry,
                status,
                cancel_button,
                ok_button,
                state: RefCell::default(),
            })
        };
        dialog.connect_signals();
        dialog
    }

    fn connect_signals(self: &Rc<Self>) {
        let entry = unsafe { gobj(self.entry) };

        let weak = Rc::downgrade(self);
        entry.connect_local("activate", false, move |_| {
            with(&weak, |d| d.respond());
            None
        });

        let weak = Rc::downgrade(self);
        entry.connect_local("key-press-event", false, move |args| {
            let escape = ui::key_symbol(ui::event_arg(args)) == ui::KEY_ESCAPE;
            if escape {
                with(&weak, |d| d.cancel());
            }
            Some(escape.to_value())
        });

        let weak = Rc::downgrade(self);
        unsafe { gobj(self.cancel_button) }.connect_local("button-press-event", false, move |_| {
            with(&weak, |d| d.cancel());
            Some(true.to_value())
        });

        let weak = Rc::downgrade(self);
        unsafe { gobj(self.ok_button) }.connect_local("button-press-event", false, move |_| {
            with(&weak, |d| d.respond());
            Some(true.to_value())
        });

        // Modal: swallow clicks on the dimmed area.
        unsafe { gobj(self.overlay) }.connect_local("button-press-event", false, |_| Some(true.to_value()));
    }

    fn submit(self: &Rc<Self>, request: Request) {
        self.state.borrow_mut().queue.push_back(request);
        if self.state.borrow().active.is_none() {
            self.start_next();
        }
    }

    fn start_next(self: &Rc<Self>) {
        let Some(request) = self.state.borrow_mut().queue.pop_front() else { return };

        // polkitd may cancel the request (timeout, agent replaced, ...).
        let daemon_cancel = request.cancellable.as_ref().and_then(|c| {
            let weak = Rc::downgrade(self);
            c.connect_cancelled_local(move |_| {
                // Defer: the handler may run synchronously inside this call.
                let weak = weak.clone();
                glib::idle_add_local_once(move || with(&weak, |d| d.cancel()));
            })
        });

        unsafe {
            ui::set_text(self.message, &request.message);
            ui::set_text(self.user, "");
            ui::set_text(self.prompt, "");
            ui::set_text(self.status, "");
        }
        self.state.borrow_mut().active =
            Some(Active { request, session: None, cancelled: false, daemon_cancel });

        unsafe {
            self.layout();
            ffi::clutter_actor_show(self.overlay);
            if self.state.borrow().grab.is_none() {
                let grab = ffi::clutter_stage_grab(self.stage as *mut ffi::ClutterStage, self.overlay);
                self.state.borrow_mut().grab = Some(grab);
            }
            ffi::clutter_actor_grab_key_focus(self.entry);
        }
        self.begin_session();
    }

    /// Start (or restart, after a failed attempt) a PAM conversation.
    fn begin_session(self: &Rc<Self>) {
        let (identity, cookie) = {
            let state = self.state.borrow();
            let Some(active) = state.active.as_ref() else { return };
            (pick_identity(&active.request.identities), active.request.cookie.clone())
        };
        let Some(identity) = identity else {
            self.finish(Err((ffi::POLKIT_ERROR_FAILED, "No identity to authenticate as")));
            return;
        };

        unsafe {
            ui::set_text(self.user, &format!("Authenticating as {}", identity_name(&identity)));
            self.set_entry_enabled(false);
        }

        let cookie = CString::new(cookie).unwrap_or_default();
        let session: glib::Object = unsafe {
            from_glib_full(ffi::polkit_agent_session_new(
                identity.as_ptr() as *mut ffi::PolkitIdentity,
                cookie.as_ptr(),
            ) as *mut gobject_sys::GObject)
        };

        let mut handlers = Vec::new();
        let weak = Rc::downgrade(self);
        handlers.push(session.connect_local("request", false, move |args| {
            let prompt: String = args.get(1).and_then(|v| v.get().ok()).unwrap_or_default();
            let echo: bool = args.get(2).and_then(|v| v.get().ok()).unwrap_or(false);
            with(&weak, |d| d.on_request(&prompt, echo));
            None
        }));
        for (signal, is_error) in [("show-error", true), ("show-info", false)] {
            let weak = Rc::downgrade(self);
            handlers.push(session.connect_local(signal, false, move |args| {
                let text: String = args.get(1).and_then(|v| v.get().ok()).unwrap_or_default();
                with(&weak, |d| d.show_status(&text, is_error));
                None
            }));
        }
        let weak = Rc::downgrade(self);
        handlers.push(session.connect_local("completed", false, move |args| {
            let gained: bool = args.get(1).and_then(|v| v.get().ok()).unwrap_or(false);
            // Defer: "completed" can fire synchronously inside cancel().
            let weak = weak.clone();
            glib::idle_add_local_once(move || with(&weak, |d| d.on_completed(gained)));
            None
        }));

        let ptr = session.as_ptr() as *mut ffi::PolkitAgentSession;
        if let Some(active) = self.state.borrow_mut().active.as_mut() {
            active.session = Some((session, handlers));
        }
        unsafe { ffi::polkit_agent_session_initiate(ptr) };
    }

    fn on_request(&self, prompt: &str, echo: bool) {
        unsafe {
            ui::set_text(self.prompt, prompt.trim());
            ffi::clutter_text_set_password_char(self.entry as *mut ClutterText, if echo { 0 } else { BULLET });
            self.set_entry_enabled(true);
            ffi::clutter_actor_grab_key_focus(self.entry);
            self.layout();
        }
    }

    fn show_status(&self, text: &str, _is_error: bool) {
        unsafe {
            ui::set_text(self.status, text.trim());
            self.layout();
        }
    }

    /// Send the entry text to the PAM helper.
    fn respond(&self) {
        let session = {
            let state = self.state.borrow();
            state.active.as_ref().and_then(|a| a.session.as_ref()).map(|(s, _)| s.clone())
        };
        let Some(session) = session else { return };
        unsafe {
            if ffi::clutter_text_get_editable(self.entry as *mut ClutterText) == 0 {
                return; // no prompt pending
            }
            // Copy the secret out, then wipe our copy after handing it over.
            let raw = ffi::clutter_text_get_text(self.entry as *mut ClutterText);
            let mut secret = if raw.is_null() { Vec::new() } else { CStr::from_ptr(raw).to_bytes().to_vec() };
            secret.push(0);
            ui::set_text(self.entry, "");
            self.set_entry_enabled(false);
            ui::set_text(self.status, "");
            ffi::polkit_agent_session_response(
                session.as_ptr() as *mut ffi::PolkitAgentSession,
                secret.as_ptr() as *const c_char,
            );
            for b in secret.iter_mut() {
                std::ptr::write_volatile(b, 0);
            }
        }
    }

    fn on_completed(self: &Rc<Self>, gained: bool) {
        let cancelled = {
            let mut state = self.state.borrow_mut();
            let Some(active) = state.active.as_mut() else { return };
            if let Some((session, handlers)) = active.session.take() {
                for h in handlers {
                    session.disconnect(h);
                }
            }
            active.cancelled
        };

        if gained {
            self.finish(Ok(()));
        } else if cancelled {
            self.finish(Err((ffi::POLKIT_ERROR_CANCELLED, "Authentication dialog was dismissed")));
        } else {
            unsafe {
                let current = ffi::clutter_text_get_text(self.status as *mut ClutterText);
                if current.is_null() || *current == 0 {
                    ui::set_text(self.status, "Sorry, that didn't work. Please try again.");
                }
            }
            self.begin_session();
        }
    }

    fn cancel(self: &Rc<Self>) {
        let session = {
            let mut state = self.state.borrow_mut();
            let Some(active) = state.active.as_mut() else { return };
            if active.cancelled {
                return;
            }
            active.cancelled = true;
            active.session.as_ref().map(|(s, _)| s.clone())
        };
        match session {
            // Ends with "completed"(false), which finishes the request.
            Some(s) => unsafe { ffi::polkit_agent_session_cancel(s.as_ptr() as *mut ffi::PolkitAgentSession) },
            None => self.finish(Err((ffi::POLKIT_ERROR_CANCELLED, "Authentication dialog was dismissed"))),
        }
    }

    fn finish(self: &Rc<Self>, result: Result<(), (i32, &str)>) {
        let Some(active) = self.state.borrow_mut().active.take() else { return };
        if let (Some(c), Some(id)) = (active.request.cancellable.as_ref(), active.daemon_cancel) {
            c.disconnect_cancelled(id);
        }
        match result {
            Ok(()) => {
                glib::g_message!("gnative", "polkit: authorized {}", active.request.action_id);
                active.request.task.succeed();
            }
            Err((code, msg)) => {
                glib::g_message!("gnative", "polkit: {} not authorized: {msg}", active.request.action_id);
                active.request.task.fail(code, msg);
            }
        }

        if self.state.borrow().queue.is_empty() {
            self.hide();
        } else {
            self.start_next();
        }
    }

    fn hide(&self) {
        let grab = self.state.borrow_mut().grab.take();
        unsafe {
            ui::set_text(self.entry, "");
            if let Some(grab) = grab {
                ffi::clutter_grab_dismiss(grab);
                gobject_sys::g_object_unref(grab as *mut gobject_sys::GObject);
            }
            ffi::clutter_actor_hide(self.overlay);
        }
    }

    fn destroy(&self) {
        // Fail anything still pending so polkitd isn't left waiting.
        let (active, queue) = {
            let mut state = self.state.borrow_mut();
            (state.active.take(), std::mem::take(&mut state.queue))
        };
        if let Some(active) = active {
            if let Some((s, _)) = &active.session {
                unsafe { ffi::polkit_agent_session_cancel(s.as_ptr() as *mut ffi::PolkitAgentSession) };
            }
            active.request.task.fail(ffi::POLKIT_ERROR_CANCELLED, "Shell is shutting down");
        }
        for r in queue {
            r.task.fail(ffi::POLKIT_ERROR_CANCELLED, "Shell is shutting down");
        }
        self.hide();
        unsafe { ffi::clutter_actor_destroy(self.overlay) };
    }

    unsafe fn set_entry_enabled(&self, enabled: bool) {
        unsafe {
            let text = self.entry as *mut ClutterText;
            ffi::clutter_text_set_editable(text, enabled as i32);
            ffi::clutter_text_set_cursor_visible(text, enabled as i32);
            ffi::clutter_actor_set_opacity(self.ok_button, if enabled { 255 } else { 128 });
        }
    }

    unsafe fn layout(&self) {
        unsafe {
            let geo = ui::primary_monitor_geometry(self.display).unwrap_or_default();
            ffi::clutter_actor_set_position(self.overlay, geo.x as f32, geo.y as f32);
            ffi::clutter_actor_set_size(self.overlay, geo.width as f32, geo.height as f32);

            let width = DIALOG_WIDTH.min(geo.width as f32 - 32.0);
            let inner = width - 2.0 * PADDING;
            let mut y = PADDING;

            for (actor, gap) in [(self.title, SPACING), (self.message, SPACING), (self.user, SPACING), (self.prompt, 6.0)] {
                ffi::clutter_actor_set_width(actor, inner);
                ffi::clutter_actor_set_position(actor, PADDING, y);
                y += ffi::clutter_actor_get_height(actor) + gap;
            }

            ffi::clutter_actor_set_position(self.entry_bg, PADDING, y);
            ffi::clutter_actor_set_size(self.entry_bg, inner, ENTRY_HEIGHT);
            ffi::clutter_actor_set_width(self.entry, inner - 20.0);
            let ty = (ENTRY_HEIGHT - ffi::clutter_actor_get_height(self.entry)) / 2.0;
            ffi::clutter_actor_set_position(self.entry, 10.0, ty.floor());
            y += ENTRY_HEIGHT + SPACING;

            ffi::clutter_actor_set_width(self.status, inner);
            ffi::clutter_actor_set_position(self.status, PADDING, y);
            let status_h = ffi::clutter_actor_get_height(self.status);
            if status_h > 1.0 {
                y += status_h + SPACING;
            }

            ffi::clutter_actor_set_position(self.ok_button, width - PADDING - BUTTON_WIDTH, y);
            ffi::clutter_actor_set_position(
                self.cancel_button,
                width - PADDING - 2.0 * BUTTON_WIDTH - SPACING,
                y,
            );
            y += BUTTON_HEIGHT + PADDING;

            ffi::clutter_actor_set_size(self.panel, width, y);
            ffi::clutter_actor_set_position(
                self.panel,
                ((geo.width as f32 - width) / 2.0).floor(),
                ((geo.height as f32 - y) * 0.4).floor(),
            );
        }
    }
}

/// Prefer the current user (typical for admin users in `wheel`), else the
/// first unix user polkit offers (e.g. root).
fn pick_identity(identities: &[glib::Object]) -> Option<glib::Object> {
    let unix_user = unsafe { glib::Type::from_glib(ffi::polkit_unix_user_get_type()) };
    let uid = unsafe { ffi::getuid() } as i32;
    let users: Vec<&glib::Object> = identities.iter().filter(|i| i.type_().is_a(unix_user)).collect();
    users
        .iter()
        .find(|u| unsafe { ffi::polkit_unix_user_get_uid(u.as_ptr() as *mut ffi::PolkitIdentity) } == uid)
        .or(users.first())
        .map(|u| (*u).clone())
}

fn identity_name(identity: &glib::Object) -> String {
    unsafe {
        let p = identity.as_ptr() as *mut ffi::PolkitIdentity;
        let name = ffi::polkit_unix_user_get_name(p);
        if name.is_null() {
            format!("uid {}", ffi::polkit_unix_user_get_uid(p))
        } else {
            cstr(name)
        }
    }
}

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

fn with(weak: &Weak<AuthDialog>, f: impl FnOnce(&Rc<AuthDialog>)) {
    if let Some(d) = weak.upgrade() {
        f(&d);
    }
}
