//! Platform facts the host depends on: process setup, the desktop session it runs in, and whether
//! a person could answer an approval prompt right now.

#[cfg(windows)]
mod windows;

use serde::Serialize;

/// State of the session the host runs in. Most "the screenshot is black" or "input did nothing"
/// reports trace back to one of these fields.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SessionState {
    /// Windows session id of this process (0 = services session: no desktop at all).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u32>,
    /// Whether this session is the one attached to the physical console.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_console: Option<bool>,
    /// Whether the process runs elevated (needed to control elevated windows on Windows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elevated: Option<bool>,
    /// Name of the desktop receiving input: "Default" when unlocked; "Winlogon" when locked or
    /// on a security prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_desktop: Option<String>,
    /// True when screenshots and input can work: interactive session, unlocked desktop.
    pub desktop_usable: bool,
    /// Plain-language explanation when `desktop_usable` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// Process-wide setup; call first thing in the host.
pub fn init_process() {
    #[cfg(windows)]
    windows::init_process();
}

pub fn session_state() -> SessionState {
    #[cfg(windows)]
    {
        windows::session_state()
    }
    #[cfg(not(windows))]
    {
        SessionState { desktop_usable: true, ..Default::default() }
    }
}

/// Why an approval prompt cannot be shown right now, if it cannot.
pub fn prompt_blocker() -> Option<String> {
    let s = session_state();
    if s.desktop_usable { None } else { s.problem.or_else(|| Some("the desktop is not usable".into())) }
}
