//! Session services for X11 apps, which run through Xwayland.
//!
//! When mutter brings up Xwayland, the shell has to start gnome-session's X11
//! services, as gnome-shell does. Among them is gsd-xsettings, which tells X11
//! apps the font DPI for the monitor's scale, the fonts, theme and cursor.
//! Without it, apps such as Steam draw very small text on a scaled monitor.

use gio::prelude::*;
use glib::translate::*;

const SYSTEMD: &str = "org.freedesktop.systemd1";
const SYSTEMD_PATH: &str = "/org/freedesktop/systemd1";
const SYSTEMD_MANAGER: &str = "org.freedesktop.systemd1.Manager";
/// Pulls in the services and is reached when they are up.
const READY_TARGET: &str = "gnome-session-x11-services-ready.target";
const SERVICES_TARGET: &str = "gnome-session-x11-services.target";
const TIMEOUT_MS: i32 = 15_000;

/// Call `method` on the user's systemd for `unit`, then run `done`.
fn unit_job(method: &'static str, unit: &'static str, done: impl FnOnce() + 'static) {
    let report = move |e: glib::Error| {
        // Expected without a systemd-managed gnome-session, as in a nested run.
        glib::g_message!("gnutshell", "{method} {unit}: {e}");
    };
    gio::bus_get(gio::BusType::Session, gio::Cancellable::NONE, move |bus| {
        let bus = match bus {
            Ok(bus) => bus,
            Err(e) => {
                report(e);
                return done();
            }
        };
        bus.call(
            Some(SYSTEMD),
            SYSTEMD_PATH,
            SYSTEMD_MANAGER,
            method,
            Some(&(unit, "fail").to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            TIMEOUT_MS,
            gio::Cancellable::NONE,
            move |reply| {
                match reply {
                    Ok(_) => glib::g_message!("gnutshell", "{method} {unit}: requested"),
                    Err(e) => report(e),
                }
                done();
            },
        );
    });
}

/// Start and stop the services along with mutter's X server.
pub fn connect(display: &glib::Object) {
    // Mutter waits with X11 startup until the task is completed, so complete
    // it whatever happens.
    display.connect_local("init-xserver", false, |args| {
        let task = args.get(1).and_then(|v| v.get::<glib::Object>().ok());
        let Some(task) = task else {
            return Some(false.to_value());
        };
        unit_job("StartUnit", READY_TARGET, move || {
            let task: *mut gobject_sys::GObject = task.to_glib_none().0;
            unsafe { gio::ffi::g_task_return_boolean(task as *mut gio::ffi::GTask, 1) };
        });
        // The task is ours to complete.
        Some(true.to_value())
    });
    display.connect_local("x11-display-closing", false, |_| {
        unit_job("StopUnit", SERVICES_TARGET, || {});
        None
    });
}
