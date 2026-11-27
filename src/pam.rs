//! Password check for the lock screen through PAM.
//!
//! `pam_authenticate` blocks (password hashing, `pam_faildelay`, network
//! modules), so it runs on a worker thread; the result is delivered on the
//! main loop. The conversation answers the first hidden prompt with the
//! password and the username prompt with the user name; anything else
//! (e.g. a forced password change) fails the attempt instead of hanging.
//!
//! The PAM service is `gnutshell` (installed to /etc/pam.d by
//! `dist/install.sh`, `auth include login`). If that file is missing the
//! `login` service is used, never PAM's deny-all `other` fallback, so the
//! screen can always be unlocked with the account password.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;

const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_CONV_ERR: c_int = 19;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
const PAM_REFRESH_CRED: c_int = 0x0010;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn = unsafe extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int;

#[repr(C)]
struct PamConv {
    conv: ConvFn,
    appdata_ptr: *mut c_void,
}

#[repr(C)]
struct PamHandle {
    _p: [u8; 0],
}

#[link(name = "pam")]
unsafe extern "C" {
    fn pam_start(service: *const c_char, user: *const c_char, conv: *const PamConv, handle: *mut *mut PamHandle)
        -> c_int;
    fn pam_authenticate(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_acct_mgmt(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_setcred(handle: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_end(handle: *mut PamHandle, status: c_int) -> c_int;
    fn pam_strerror(handle: *mut PamHandle, errnum: c_int) -> *const c_char;
}

unsafe extern "C" {
    fn calloc(n: usize, size: usize) -> *mut c_void;
    fn strdup(s: *const c_char) -> *mut c_char;
}

/// State the conversation callback sees (lives on the worker's stack).
struct Conversation {
    user: CString,
    password: Option<CString>,
    messages: Vec<String>,
}

unsafe extern "C" fn converse(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    if num_msg <= 0 || msg.is_null() || resp.is_null() || appdata.is_null() {
        return PAM_CONV_ERR;
    }
    let conv = unsafe { &mut *(appdata as *mut Conversation) };
    let n = num_msg as usize;
    // PAM frees the array and each `resp` string with free().
    let replies = unsafe { calloc(n, std::mem::size_of::<PamResponse>()) } as *mut PamResponse;
    if replies.is_null() {
        return PAM_BUF_ERR;
    }
    for i in 0..n {
        // Linux-PAM: `msg` is an array of pointers.
        let m = unsafe { &**msg.add(i) };
        let text = if m.msg.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(m.msg) }.to_string_lossy().into_owned()
        };
        let reply = unsafe { &mut *replies.add(i) };
        match m.msg_style {
            PAM_PROMPT_ECHO_OFF => match conv.password.take() {
                Some(pw) => {
                    reply.resp = unsafe { strdup(pw.as_ptr()) };
                    let mut bytes = pw.into_bytes();
                    wipe(&mut bytes);
                }
                // A second secret prompt (new password, OTP, …): not supported.
                None => return fail(replies, i),
            },
            PAM_PROMPT_ECHO_ON => reply.resp = unsafe { strdup(conv.user.as_ptr()) },
            PAM_ERROR_MSG | PAM_TEXT_INFO => {
                if !text.trim().is_empty() {
                    conv.messages.push(text.trim().to_owned());
                }
            }
            _ => return fail(replies, i),
        }
    }
    unsafe { *resp = replies };
    PAM_SUCCESS
}

/// Free the replies built so far and report a conversation error.
fn fail(replies: *mut PamResponse, filled: usize) -> c_int {
    unsafe extern "C" {
        fn free(p: *mut c_void);
    }
    unsafe {
        for i in 0..filled {
            let r = &mut *replies.add(i);
            if !r.resp.is_null() {
                wipe_cstr(r.resp);
                free(r.resp as *mut c_void);
            }
        }
        free(replies as *mut c_void);
    }
    PAM_CONV_ERR
}

unsafe fn wipe_cstr(p: *mut c_char) {
    let mut q = p;
    unsafe {
        while *q != 0 {
            std::ptr::write_volatile(q, 0);
            q = q.add(1);
        }
    }
}

fn wipe(bytes: &mut [u8]) {
    for b in bytes {
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

fn service() -> &'static CStr {
    let ours = ["/etc/pam.d/gnutshell", "/usr/lib/pam.d/gnutshell"];
    if ours.iter().any(|p| Path::new(p).is_file()) { c"gnutshell" } else { c"login" }
}

/// Check `password` for `user`. Blocking; call from a worker thread.
/// `Err` carries a message for the user.
pub fn authenticate(user: &str, mut password: Vec<u8>) -> Result<(), String> {
    let pw = CString::new(password.clone()).ok();
    wipe(&mut password);
    let Some(pw) = pw else { return Err("Sorry, that didn't work. Please try again.".into()) };
    let mut conv = Conversation {
        user: CString::new(user).unwrap_or_default(),
        password: Some(pw),
        messages: Vec::new(),
    };
    let pam_conv = PamConv { conv: converse, appdata_ptr: &mut conv as *mut Conversation as *mut c_void };

    let mut handle = std::ptr::null_mut();
    let user_c = CString::new(user).unwrap_or_default();
    let mut status = unsafe { pam_start(service().as_ptr(), user_c.as_ptr(), &pam_conv, &mut handle) };
    if status == PAM_SUCCESS {
        status = unsafe { pam_authenticate(handle, 0) };
    }
    if status == PAM_SUCCESS {
        // Expired / disabled accounts must not unlock.
        status = unsafe { pam_acct_mgmt(handle, 0) };
    }
    if status == PAM_SUCCESS {
        // Refresh Kerberos tickets etc.; failure here doesn't block unlocking.
        unsafe { pam_setcred(handle, PAM_REFRESH_CRED) };
    }
    let error = (status != PAM_SUCCESS).then(|| {
        let p = unsafe { pam_strerror(handle, status) };
        if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
    });
    if !handle.is_null() {
        unsafe { pam_end(handle, status) };
    }
    // Wipe the password if PAM never asked for it.
    if let Some(pw) = conv.password.take() {
        let mut bytes = pw.into_bytes();
        wipe(&mut bytes);
    }

    match error {
        None => Ok(()),
        Some(e) => {
            glib::g_message!("gnutshell", "unlock failed: {e}");
            // A module's own message (e.g. "account locked") beats the generic one.
            Err(conv.messages.pop().unwrap_or_else(|| "Sorry, that didn't work. Please try again.".into()))
        }
    }
}

/// Run `authenticate` on a worker thread; `done` runs on the main loop.
pub fn authenticate_async(user: String, password: Vec<u8>, done: impl FnOnce(Result<(), String>) + Send + 'static) {
    // Debug builds: test the flow against another (e.g. nonexistent) user,
    // so failed attempts never count against the real account.
    #[cfg(debug_assertions)]
    let user = std::env::var("GNUTSHELL_DEBUG_PAM_USER").unwrap_or(user);
    std::thread::Builder::new()
        .name("gnutshell-pam".into())
        .spawn(move || {
            let result = authenticate(&user, password);
            glib::MainContext::default().invoke(move || done(result));
        })
        .expect("spawn PAM worker thread");
}

#[cfg(test)]
mod tests {
    use super::wipe;

    #[test]
    fn wipe_zeroes() {
        let mut v = b"secret".to_vec();
        wipe(&mut v);
        assert!(v.iter().all(|b| *b == 0));
    }
}
