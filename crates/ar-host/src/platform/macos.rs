//! Read-only macOS privacy checks (never prompt or change TCC from a remote tool), the session the
//! host runs in, and the login-window agent's identity switch.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use core_foundation::{
    base::{CFType, TCFType},
    boolean::CFBoolean,
    dictionary::{CFDictionary, CFDictionaryRef},
    string::CFString,
};

use super::SessionState;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGSessionCopyCurrentDictionary() -> CFDictionaryRef;
}

/// Whether this process may read the screen. TCC grants are executable-specific.
pub fn screen_recording_allowed() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Whether this process may inject input or operate accessibility elements.
pub fn accessibility_allowed() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Set when this host is the system LaunchAgent that `host install --service` registers for the
/// LoginWindow session (and the Aqua one): it may serve the login window, where no user is
/// logged in, instead of refusing like a per-user agent would.
static LOGIN_WINDOW_AGENT: AtomicBool = AtomicBool::new(false);

/// Whether this host serves the login window too (see [`adopt_owner`]).
pub fn login_window_agent() -> bool {
    LOGIN_WINDOW_AGENT.load(Ordering::Relaxed)
}

/// Set when launchd started this system agent as root: the LoginWindow-session instance (the
/// Aqua-session one starts as the user).
static LOGIN_WINDOW_INSTANCE: AtomicBool = AtomicBool::new(false);

/// Whether this is the system agent's LoginWindow-session instance (see [`crate::handoff`]).
pub fn login_window_instance() -> bool {
    LOGIN_WINDOW_INSTANCE.load(Ordering::Relaxed)
}

/// What `ai-remote-hostw --owner-uid <uid>` does first: marks this host as the system agent, and
/// when it was started as root (launchd runs LoginWindow-session agents as root, Aqua-session
/// ones as the user) becomes `uid`, the installing user, so that the profile it writes (audit log,
/// pairings) stays that user's and the Screen Recording / Accessibility grants that user gave
/// this executable apply. Running as root at the login window is what RustDesk does instead;
/// `AI_REMOTE_KEEP_ROOT=1` keeps that behaviour if the identity switch turns out to lose capture.
pub fn adopt_owner(uid: u32) -> Result<()> {
    LOGIN_WINDOW_AGENT.store(true, Ordering::Relaxed);
    // SAFETY: plain queries.
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    LOGIN_WINDOW_INSTANCE.store(true, Ordering::Relaxed);
    if std::env::var_os("AI_REMOTE_KEEP_ROOT").is_some_and(|v| v == "1") {
        tracing::warn!(uid, "AI_REMOTE_KEEP_ROOT=1: staying root at the login window");
        return Ok(());
    }
    // SAFETY: getpwuid returns a pointer to a static buffer, read before any other call to it.
    let (gid, home, name) = unsafe {
        let entry = libc::getpwuid(uid);
        if entry.is_null() {
            bail!("account uid {uid} does not exist");
        }
        let e = &*entry;
        let text = |p: *const libc::c_char| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
        (e.pw_gid, text(e.pw_dir), text(e.pw_name))
    };
    let c_name = std::ffi::CString::new(name.clone()).context("account name")?;
    // SAFETY: the standard privilege-drop order: groups first, the uid last (after it nothing
    // else is permitted); every step is checked.
    unsafe {
        if libc::initgroups(c_name.as_ptr(), gid as _) != 0 {
            bail!("initgroups for {name}: {}", std::io::Error::last_os_error());
        }
        if libc::setgid(gid) != 0 {
            bail!("setgid {gid}: {}", std::io::Error::last_os_error());
        }
        if libc::setuid(uid) != 0 {
            bail!("setuid {uid}: {}", std::io::Error::last_os_error());
        }
        // Not restorable once set for real: the check RustDesk's and sudo's drop both make.
        if libc::setuid(0) == 0 {
            bail!("the privilege drop to uid {uid} could be undone");
        }
        // SAFETY: before any thread is started (the host's runtime comes later).
        std::env::set_var("HOME", &home);
        std::env::set_var("USER", &name);
    }
    tracing::info!(uid, user = name, "running as the installing user in the login-window session");
    Ok(())
}

/// Whether this process runs as `uid` (the system agent also starts in other users' sessions).
pub fn runs_as(uid: u32) -> bool {
    // SAFETY: a plain query.
    unsafe { libc::getuid() == uid }
}

/// Owner of `/dev/console`: root while the login window has the screen, otherwise the user whose
/// session has it (the test RustDesk uses for "before login"). `None` when it cannot be read.
pub fn console_device_owner() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/dev/console").ok().map(|m| m.uid())
}

/// `kCGSSessionOnConsoleKey` of this process's session: whether it has the screen (`None` when
/// the session dictionary or the key cannot be read). The lock screen keeps it true.
pub fn on_console() -> Option<bool> {
    let raw = unsafe { CGSessionCopyCurrentDictionary() };
    if raw.is_null() {
        return None;
    }
    let session: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_create_rule(raw) };
    session.find(CFString::new("kCGSSessionOnConsoleKey")).and_then(|v| v.downcast::<CFBoolean>()).map(bool::from)
}

/// `struct auditinfo_addr` from `<bsm/audit.h>` (48 bytes; `ai_asid` at offset 36).
#[repr(C)]
struct AuditInfoAddr {
    auid: u32,
    mask_success: u32,
    mask_failure: u32,
    termid_port: i32,
    termid_type: u32,
    termid_addr: [u32; 4],
    asid: i32,
    flags: u64,
}

unsafe extern "C" {
    fn getaudit_addr(info: *mut AuditInfoAddr, length: libc::c_int) -> libc::c_int;
}

/// This process's audit session id: one per login session (each GUI login, the login window,
/// an SSH login), inherited by everything started in it.
pub fn audit_session_id() -> Option<i32> {
    let mut info = std::mem::MaybeUninit::<AuditInfoAddr>::zeroed();
    // SAFETY: the buffer is the size the call is told; it fills it on success.
    let rc = unsafe { getaudit_addr(info.as_mut_ptr(), std::mem::size_of::<AuditInfoAddr>() as libc::c_int) };
    // SAFETY: zero-initialised, and filled on success.
    (rc == 0).then(|| unsafe { info.assume_init() }.asid)
}

/// `kern.bootsessionuuid`: changes with every restart.
pub fn boot_session() -> Option<String> {
    let mut buf = [0u8; 64];
    let mut len = buf.len();
    // SAFETY: the name is NUL-terminated; the buffer and its length are passed together.
    let rc = unsafe {
        libc::sysctlbyname(c"kern.bootsessionuuid".as_ptr(), buf.as_mut_ptr().cast(), &mut len, std::ptr::null_mut(), 0)
    };
    if rc != 0 {
        return None;
    }
    let text = std::ffi::CStr::from_bytes_until_nul(&buf[..len.min(buf.len())]).ok()?.to_str().ok()?;
    (!text.is_empty()).then(|| text.to_string())
}

pub fn session_state() -> SessionState {
    let raw = unsafe { CGSessionCopyCurrentDictionary() };
    if raw.is_null() {
        return SessionState {
            desktop_usable: false,
            problem: Some("no macOS graphical login session is available".into()),
            ..Default::default()
        };
    }
    let session: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_create_rule(raw) };
    let flag = |name| session.find(CFString::new(name)).and_then(|v| v.downcast::<CFBoolean>()).map(bool::from);
    let on_console = flag("kCGSSessionOnConsoleKey");
    let locked = flag("CGSSessionScreenIsLocked").unwrap_or(false);
    let logged_in = flag("kCGSessionLoginDoneKey").unwrap_or(false);
    describe(on_console, logged_in, locked, login_window_agent())
}

/// The session for what the window server reports. Like Windows' secure desktop: the lock screen
/// is a desktop this host (the user's own process) can still see and type into (the password),
/// nobody is there to answer an approval prompt; the login window, where no user is logged in,
/// only the system agent reaches.
fn describe(on_console: Option<bool>, logged_in: bool, locked: bool, login_window_agent: bool) -> SessionState {
    let mut state = SessionState { on_console, ..Default::default() };
    if on_console != Some(true) {
        state.problem = Some(
            "not in the macOS console session (another user has the screen, or this is an SSH or background session)"
                .into(),
        );
    } else if !logged_in {
        if login_window_agent {
            state.desktop_usable = true;
            state.secure_desktop = true;
            state.input_desktop = Some("login window".into());
        } else {
            state.problem = Some(
                "the macOS login window is showing, which only a host installed with `host install --service` reaches"
                    .into(),
            );
        }
    } else if locked {
        state.desktop_usable = true;
        state.secure_desktop = true;
        state.input_desktop = Some("lock screen".into());
    } else {
        state.desktop_usable = true;
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_screen_is_a_reachable_secure_desktop_and_the_login_window_needs_the_system_agent() {
        let unlocked = describe(Some(true), true, false, false);
        assert!(unlocked.desktop_usable && !unlocked.secure_desktop && unlocked.problem.is_none());
        let locked = describe(Some(true), true, true, false);
        assert!(locked.desktop_usable && locked.secure_desktop);
        assert_eq!(locked.input_desktop.as_deref(), Some("lock screen"));
        let login_window = describe(Some(true), false, false, false);
        assert!(!login_window.desktop_usable);
        assert!(login_window.problem.as_deref().unwrap().contains("--service"));
        let served = describe(Some(true), false, false, true);
        assert!(served.desktop_usable && served.secure_desktop);
        let elsewhere = describe(Some(false), true, false, true);
        assert!(!elsewhere.desktop_usable);
        assert!(elsewhere.problem.as_deref().unwrap().contains("console"));
    }

    #[test]
    fn the_console_device_owner_is_readable() {
        assert!(console_device_owner().is_some());
    }

    #[test]
    fn the_session_ids_the_stop_marker_uses_are_readable() {
        assert_eq!(std::mem::size_of::<AuditInfoAddr>(), 48);
        assert!(audit_session_id().is_some());
        let boot = boot_session().expect("kern.bootsessionuuid");
        assert_eq!(boot.len(), 36, "{boot}");
    }
}
