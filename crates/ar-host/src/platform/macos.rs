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
}
