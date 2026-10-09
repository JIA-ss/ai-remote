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

/// The window injected input would reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputTarget {
    /// The focused window (keyboard input).
    Focused,
    /// The window under the pointer as it is now.
    Pointer,
    /// The window at a screen coordinate.
    At(i32, i32),
}

/// Where injected input aimed at a window would end up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputTargetCheck {
    /// Nothing known against sending it (also when the target cannot be determined at all).
    Allowed,
    /// Sending is allowed, but the OS may discard the input without any error; the text says why, so
    /// the caller can pass it on.
    Doubtful(String),
    /// Must not be sent; the text says why and what to do instead.
    Refused(String),
}

/// Checks the window that input aimed at `target` would reach: never the host's own windows (its
/// approval prompts), and warn when the OS would likely drop the input silently.
pub fn check_input_target(target: InputTarget) -> InputTargetCheck {
    #[cfg(windows)]
    {
        windows::check_input_target(target)
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        InputTargetCheck::Allowed
    }
}

#[cfg(windows)]
pub use self::windows::{char_key, move_pointer};
