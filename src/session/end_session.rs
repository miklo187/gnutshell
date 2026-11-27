//! gnome-shell's `org.gnome.SessionManager.EndSessionDialog`.
//!
//! The session menu asks gnome-session to log out, restart or power off.
//! gnome-session first asks the running apps, then calls `Open` here with the
//! inhibitors: apps that asked not to be interrupted, or didn't answer. The
//! dialog lists those that block logout and answers with a `Confirmed…` or
//! `Canceled` signal. `gnome-session-quit` and other clients of
//! gnome-session get the same dialog.
//!
//! Without gnome-session (nested and headless runs) the menu shows its own
//! confirmation and acts directly.

use super::{Action, SessionMenu, with};
use gio::prelude::*;
use std::rc::Rc;

const OBJECT_PATH: &str = "/org/gnome/SessionManager/EndSessionDialog";
const INTERFACE: &str = "org.gnome.SessionManager.EndSessionDialog";
const GSM_NAME: &str = "org.gnome.SessionManager";
const GSM_PATH: &str = "/org/gnome/SessionManager";
const GSM_INTERFACE: &str = "org.gnome.SessionManager";
const GSM_INHIBITOR_INTERFACE: &str = "org.gnome.SessionManager.Inhibitor";

/// `GsmInhibitorFlag`: blocks logging out (and so restart and power off).
const INHIBIT_LOGOUT: u32 = 1;
/// `GsmManagerLogoutMode`: confirm, and respect inhibitors.
const LOGOUT_MODE_NORMAL: u32 = 0;
/// Blockers named in the dialog before "and N more".
const MAX_LISTED: usize = 5;
const MAX_REASON: usize = 80;

const INTERFACE_XML: &str = r#"
<node>
  <interface name="org.gnome.SessionManager.EndSessionDialog">
    <method name="Open">
      <arg type="u" direction="in" name="type"/>
      <arg type="u" direction="in" name="timestamp"/>
      <arg type="u" direction="in" name="seconds_to_stay_open"/>
      <arg type="ao" direction="in" name="inhibitor_object_paths"/>
    </method>
    <method name="Close"/>
    <signal name="ConfirmedLogout"/>
    <signal name="ConfirmedReboot"/>
    <signal name="ConfirmedShutdown"/>
    <signal name="Canceled"/>
    <signal name="Closed"/>
  </interface>
</node>
"#;

/// An app that blocks logging out.
#[derive(Clone, Debug, PartialEq)]
pub struct Blocker {
    pub app: String,
    pub reason: String,
}

/// A dialog gnome-session is waiting on.
pub struct Request {
    /// gnome-session's unique bus name; the answer goes only to it.
    sender: String,
}

/// The action for `Open`'s `type` argument.
fn action_for(kind: u32) -> Option<Action> {
    match kind {
        0 => Some(Action::LogOut),
        1 => Some(Action::PowerOff),
        2 => Some(Action::Restart),
        _ => None,
    }
}

fn confirmed_signal(action: Action) -> &'static str {
    match action {
        Action::Restart => "ConfirmedReboot",
        Action::PowerOff => "ConfirmedShutdown",
        _ => "ConfirmedLogout",
    }
}

/// The dialog's body text.
pub fn describe(blockers: &[Blocker]) -> String {
    if blockers.is_empty() {
        return super::CONFIRM_BODY.to_owned();
    }
    let mut text = String::from("These apps asked not to be interrupted:\n");
    for b in blockers.iter().take(MAX_LISTED) {
        let mut reason: String = b.reason.replace('\n', " ").chars().take(MAX_REASON).collect();
        if reason.len() < b.reason.len() {
            reason.push('…');
        }
        text.push_str(&format!("\n•  {}: {reason}", b.app));
    }
    if blockers.len() > MAX_LISTED {
        text.push_str(&format!("\n•  and {} more", blockers.len() - MAX_LISTED));
    }
    text.push_str("\n\nUnsaved work will be lost if you continue.");
    text
}

/// A readable name for an inhibitor's app id: the desktop app's name, else
/// the id without any directory.
fn app_name(app_id: &str) -> String {
    let base = app_id.rsplit('/').next().unwrap_or(app_id);
    let stem = base.trim_end_matches(".desktop");
    gio_unix::DesktopAppInfo::new(&format!("{stem}.desktop"))
        .map(|a| a.name().to_string())
        .unwrap_or_else(|| if stem.is_empty() { "Unknown app".into() } else { stem.to_owned() })
}

fn session_manager_call(
    bus: &gio::DBusConnection,
    path: &str,
    interface: &str,
    method: &str,
) -> Option<glib::Variant> {
    bus.call_sync(
        Some(GSM_NAME),
        path,
        interface,
        method,
        None,
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        500,
        gio::Cancellable::NONE,
    )
    .ok()
}

/// The inhibitors among `paths` that block logging out.
fn blockers(bus: &gio::DBusConnection, paths: &[String]) -> Vec<Blocker> {
    let get = |path: &str, method| session_manager_call(bus, path, GSM_INHIBITOR_INTERFACE, method);
    paths
        .iter()
        .filter_map(|path| {
            let (flags,) = get(path, "GetFlags")?.get::<(u32,)>()?;
            if flags & INHIBIT_LOGOUT == 0 {
                return None;
            }
            let string = |method| get(path, method).and_then(|v| v.get::<(String,)>()).map(|(s,)| s);
            Some(Blocker {
                app: app_name(&string("GetAppId").unwrap_or_default()),
                reason: string("GetReason").unwrap_or_default(),
            })
        })
        .collect()
}

fn is_session_manager(bus: &gio::DBusConnection, sender: &str) -> bool {
    bus.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetNameOwner",
        Some(&(GSM_NAME,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        1000,
        gio::Cancellable::NONE,
    )
    .ok()
    .and_then(|v| v.get::<(String,)>())
    .is_some_and(|(owner,)| owner == sender)
}

impl SessionMenu {
    /// Offer the dialog on the session bus. gnome-session looks for it under
    /// `org.gnome.Shell`, which `shell_dbus` owns on this same connection.
    pub(super) fn export_end_session_dialog(self: &Rc<Self>) {
        let bus = match gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
            Ok(bus) => bus,
            Err(e) => {
                glib::g_warning!("gnutshell", "end session dialog: no session bus: {e}");
                return;
            }
        };
        let info = gio::DBusNodeInfo::for_xml(INTERFACE_XML)
            .ok()
            .and_then(|node| node.lookup_interface(INTERFACE))
            .expect("valid EndSessionDialog interface XML");
        let weak = Rc::downgrade(self);
        let registration = bus
            .register_object(OBJECT_PATH, &info)
            .method_call(move |bus, sender, _, _, method, params, invocation| {
                let sender = sender.unwrap_or_default();
                let Some(menu) = weak.upgrade() else {
                    invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "shutting down");
                    return;
                };
                if !is_session_manager(&bus, sender) {
                    invocation.return_dbus_error("org.freedesktop.DBus.Error.AccessDenied", "Not allowed");
                    return;
                }
                match method {
                    "Open" => menu.on_open(&bus, sender, &params, invocation),
                    "Close" => {
                        invocation.return_value(None);
                        // gnome-session gave up: no answer wanted.
                        if menu.state.borrow_mut().request.take().is_some() {
                            menu.close();
                            menu.emit(sender, "Closed");
                        }
                    }
                    _ => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
                }
            })
            .build();
        match registration {
            Ok(id) => {
                let mut state = self.state.borrow_mut();
                state.registration = Some(id);
                state.session_bus = Some(bus);
            }
            Err(e) => glib::g_warning!("gnutshell", "could not export {OBJECT_PATH}: {e}"),
        }
    }

    fn on_open(
        self: &Rc<Self>,
        bus: &gio::DBusConnection,
        sender: &str,
        params: &glib::Variant,
        invocation: gio::DBusMethodInvocation,
    ) {
        let kind = params.child_value(0).get::<u32>();
        let Some(action) = kind.and_then(action_for) else {
            invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", "Unknown dialog type");
            return;
        };
        if crate::lockscreen::is_locked() {
            // gnome-session then cancels, which is the safe answer.
            invocation.return_dbus_error("org.freedesktop.DBus.Error.Failed", "The screen is locked");
            return;
        }
        let paths: Vec<String> = params.child_value(3).iter().filter_map(|v| v.str().map(str::to_owned)).collect();
        // Reply first: gnome-session is waiting, and we are about to ask it
        // about the inhibitors.
        invocation.return_value(None);
        let blockers = blockers(bus, &paths);
        glib::g_message!("gnutshell", "end session dialog: {action:?}, {} blocking", blockers.len());
        self.state.borrow_mut().request = Some(Request { sender: sender.to_owned() });
        self.show_confirmation(action, &blockers);
    }

    fn emit(&self, destination: &str, signal: &str) {
        let Some(bus) = self.state.borrow().session_bus.clone() else { return };
        if let Err(e) = bus.emit_signal(Some(destination), OBJECT_PATH, INTERFACE, signal, None) {
            glib::g_warning!("gnutshell", "could not emit {signal}: {e}");
        }
    }

    /// Tell gnome-session that a dialog it opened was dismissed. Without an
    /// answer it would stay halfway through ending the session.
    pub(super) fn cancel_request(&self) {
        let request = self.state.borrow_mut().request.take();
        if let Some(request) = request {
            self.emit(&request.sender, "Canceled");
        }
    }

    /// The user pressed the dialog's confirm button.
    pub(super) fn confirm(&self, action: Action) {
        let request = self.state.borrow_mut().request.take();
        self.close();
        match request {
            Some(request) => self.emit(&request.sender, confirmed_signal(action)),
            None => self.perform(action),
        }
    }

    /// Ask gnome-session to end the session; it opens the dialog. Without
    /// gnome-session, confirm here and act directly.
    pub(super) fn request_end_session(self: &Rc<Self>, action: Action) {
        let Some(bus) = self.state.borrow().session_bus.clone() else {
            self.show_confirmation(action, &[]);
            return;
        };
        let (method, args) = match action {
            Action::Restart => ("Reboot", None),
            Action::PowerOff => ("Shutdown", None),
            _ => ("Logout", Some((LOGOUT_MODE_NORMAL,).to_variant())),
        };
        let weak = Rc::downgrade(self);
        bus.call(
            Some(GSM_NAME),
            GSM_PATH,
            GSM_INTERFACE,
            method,
            args.as_ref(),
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            // Reboot and Shutdown only reply once the user has answered.
            i32::MAX,
            gio::Cancellable::NONE,
            move |result| {
                let Err(e) = result else { return };
                if e.matches(gio::DBusError::ServiceUnknown) || e.matches(gio::DBusError::NameHasNoOwner) {
                    with(&weak, |m| m.show_confirmation(action, &[]));
                } else {
                    // Includes the user pressing Cancel.
                    glib::g_message!("gnutshell", "gnome-session {method}: {e}");
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocker(i: usize) -> Blocker {
        Blocker { app: format!("App {i}"), reason: "Unsaved file".into() }
    }

    #[test]
    fn dialog_types_and_answers() {
        for (kind, action, signal) in [
            (0, Action::LogOut, "ConfirmedLogout"),
            (1, Action::PowerOff, "ConfirmedShutdown"),
            (2, Action::Restart, "ConfirmedReboot"),
        ] {
            assert_eq!(action_for(kind), Some(action));
            assert_eq!(confirmed_signal(action), signal);
        }
        assert_eq!(action_for(3), None);
    }

    #[test]
    fn describes_blockers() {
        assert_eq!(describe(&[]), super::super::CONFIRM_BODY);
        let text = describe(&(0..7).map(blocker).collect::<Vec<_>>());
        assert!(text.contains("App 0: Unsaved file"));
        assert!(text.contains("App 4") && !text.contains("App 5"));
        assert!(text.contains("and 2 more"));
        let long = describe(&[Blocker { app: "A".into(), reason: "x".repeat(200) }]);
        assert!(long.contains(&format!("{}…", "x".repeat(MAX_REASON))));
    }
}
