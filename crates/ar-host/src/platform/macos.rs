//! Read-only macOS privacy checks. Never prompt or change TCC from a remote tool.

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
    let usable = logged_in && !locked && on_console == Some(true);
    SessionState {
        on_console,
        desktop_usable: usable,
        problem: (!usable).then(|| {
            if locked { "the macOS screen is locked" } else { "not in the logged-in macOS console session" }.into()
        }),
        ..Default::default()
    }
}
