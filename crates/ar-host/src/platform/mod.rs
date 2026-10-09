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

/// This process's DPI awareness on Windows: `per_monitor_v2`, `per_monitor`, `system` or
/// `unaware`. `None` where the question does not apply.
pub fn dpi_awareness() -> Option<&'static str> {
    #[cfg(windows)]
    {
        Some(windows::dpi_awareness())
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Text printed by a console tool (schtasks, netsh): those write the console's OEM code page, not
/// UTF-8, so non-ASCII paths and localised messages would otherwise turn into mojibake.
pub fn decode_console_output(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    #[cfg(windows)]
    if let Some(s) = windows::decode_oem(bytes) {
        return s;
    }
    String::from_utf8_lossy(bytes).into_owned()
}
