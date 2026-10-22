//! Asking macOS for Screen Recording and Accessibility, on behalf of the owner at this computer.
//!
//! The rule elsewhere stays: no remote tool ever triggers a privacy prompt. These run only for
//! `request_permission` on the host's local control channel, which only the owner's account can
//! open (the desktop app's "Grant" button), never for a call from a controller.

use core_foundation::{
    base::TCFType,
    boolean::CFBoolean,
    dictionary::{CFDictionary, CFDictionaryRef},
    string::{CFString, CFStringRef},
};
use serde::Serialize;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGRequestScreenCaptureAccess() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

/// Shows the system's Screen Recording request for this process (attributed to the app that
/// contains it); whether access is already granted. A grant takes effect after a restart.
pub fn request_screen_recording() -> bool {
    // SAFETY: a plain call; it shows the request asynchronously and returns at once.
    unsafe { CGRequestScreenCaptureAccess() }
}

/// Shows the system's Accessibility request for this process; whether access is granted now.
pub fn request_accessibility() -> bool {
    // SAFETY: the key is a framework constant; the dictionary lives across the call.
    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let options = CFDictionary::from_CFType_pairs(&[(key, CFBoolean::true_value())]);
        AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef())
    }
}

/// What a fresh process of this executable is granted (`AI_REMOTE_TCC_PROBE=1`): a running process
/// may keep seeing a Screen Recording answer from before the grant until it restarts.
#[derive(Debug, Clone, Copy, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Grants {
    pub screen_recording: bool,
    pub accessibility: bool,
}

pub fn grants() -> Grants {
    Grants {
        screen_recording: super::macos::screen_recording_allowed(),
        accessibility: super::macos::accessibility_allowed(),
    }
}

/// The environment variable that makes `ai-remote-hostw` print [`grants`] as JSON and exit.
pub const PROBE_ENV: &str = "AI_REMOTE_TCC_PROBE";

/// The grants as a fresh child of this executable sees them, else this process's own view.
pub fn fresh_grants() -> (Grants, bool) {
    let probe = std::env::current_exe().ok().and_then(|exe| {
        let out = std::process::Command::new(exe)
            .env(PROBE_ENV, "1")
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        serde_json::from_slice::<Grants>(&out.stdout).ok()
    });
    match probe {
        Some(g) => (g, true),
        None => (grants(), false),
    }
}
