//! gnative: a GNOME-style shell written in Rust on top of libmutter.
//!
//! This binary replaces `gnome-shell`: it creates a `MetaContext`, installs a
//! Rust `MetaPlugin` subclass as the compositor plugin, and runs mutter's main
//! loop. No GJS is involved.

mod ffi;
mod icons;
mod keyboard;
mod launcher;
mod lockscreen;
mod osd;
mod pam;
mod plugin;
mod polkit;
mod session;
mod shell_dbus;
mod switcher;
mod ui;

use glib::prelude::*;
use glib::translate::*;
use std::ffi::{c_char, c_int, CString};
use std::os::unix::ffi::OsStringExt;
use std::process::ExitCode;

const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;

unsafe fn check(ok: glib_sys::gboolean, err: *mut glib_sys::GError, what: &str) -> Result<(), String> {
    if ok != 0 {
        return Ok(());
    }
    if err.is_null() {
        return Err(format!("{what} failed"));
    }
    let err: glib::Error = unsafe { from_glib_full(err) };
    Err(format!("{what} failed: {err}"))
}

unsafe extern "C" fn on_terminate_signal(ctx: glib_sys::gpointer) -> glib_sys::gboolean {
    unsafe { ffi::meta_context_terminate(ctx as *mut ffi::MetaContext) };
    glib_sys::G_SOURCE_REMOVE
}

fn run() -> Result<(), String> {
    // GOption needs a mutable C argv.
    let args: Vec<CString> = std::env::args_os()
        .map(|a| CString::new(a.into_vec()).expect("argument contains NUL byte"))
        .collect();
    let mut argv: Vec<*mut c_char> = args.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    argv.push(std::ptr::null_mut());
    let mut argc = args.len() as c_int;
    let mut argv_ptr = argv.as_mut_ptr();

    unsafe {
        let ctx = ffi::meta_create_context(c"gnative".as_ptr());
        ffi::meta_context_set_plugin_gtype(ctx, plugin::GnativePlugin::static_type().into_glib());
        ffi::meta_context_set_gnome_wm_keybindings(ctx, c"Mutter,GNOME Shell".as_ptr());

        let result = (|| {
            let mut err = std::ptr::null_mut();
            check(ffi::meta_context_configure(ctx, &mut argc, &mut argv_ptr, &mut err), err, "configure")?;
            check(ffi::meta_context_setup(ctx, &mut err), err, "setup")?;
            check(ffi::meta_context_start(ctx, &mut err), err, "start")?;
            if ffi::meta_context_raise_rlimit_nofile(ctx, &mut err) == 0 {
                let e: glib::Error = from_glib_full(err);
                eprintln!("gnative: could not raise RLIMIT_NOFILE: {e}");
                err = std::ptr::null_mut();
            }

            for sig in [SIGINT, SIGTERM] {
                ffi::g_unix_signal_add(sig, Some(on_terminate_signal), ctx as glib_sys::gpointer);
            }

            ffi::meta_context_notify_ready(ctx);
            // Under systemd (gnome-session), signal that the shell is up.
            // No-op when not started by systemd.
            ffi::sd_notify(0, c"READY=1".as_ptr());
            check(ffi::meta_context_run_main_loop(ctx, &mut err), err, "main loop")
        })();

        plugin::shutdown();

        // mutter 51 segfaults inside clutter_context_destroy() once any text
        // has been rendered (use-after-free in Clutter's Pango pipeline cache,
        // reached from Cogl context teardown). The process is exiting anyway;
        // the kernel reclaims DRM, sockets and memory, so skip the in-process
        // teardown unless explicitly requested for debugging.
        if std::env::var_os("GNATIVE_FULL_TEARDOWN").is_some() {
            ffi::meta_context_destroy(ctx);
        }
        result
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gnative: {e}");
            ExitCode::FAILURE
        }
    }
}
