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

/// What this process may change on the machine (Windows; elsewhere always [`Rights::Elevated`]
/// as far as ai-remote is concerned: nothing it installs needs more).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rights {
    /// Elevated (or an administrator without UAC): may do everything.
    Elevated,
    /// An administrator's unelevated token: elevating is one prompt away.
    AdminNotElevated,
    /// A standard user: can register their own logon task, not machine-wide firewall rules.
    StandardUser,
}

/// This process's [`Rights`]; an unreadable token counts as an unelevated administrator, so the
/// caller asks for elevation rather than attempting less.
pub fn rights() -> Rights {
    #[cfg(windows)]
    {
        windows::rights()
    }
    #[cfg(not(windows))]
    {
        Rights::Elevated
    }
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

/// Prepares a child process so it opens no console window. The background host has no console of
/// its own, so on Windows every console program it starts (a shell, schtasks, netsh, a mounted MCP
/// server) would otherwise get a fresh console window popping up on the user's desktop. Every
/// child the host starts goes through this or [`process_tree`]. For a tokio command, pass
/// `cmd.as_std_mut()`.
pub fn no_console_window(cmd: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(::windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// Wraps a command the host's tools run: no console window, and dropping the child kills its whole
/// process tree (a job object on Windows, a process group elsewhere), so a timeout or the end of a
/// session leaves nothing behind.
///
/// Do not set creation flags on `cmd` itself: the job object replaces them with its own (it
/// starts the child suspended to enrol it), keeping only flags given through the wrapper here.
pub fn process_tree(cmd: tokio::process::Command) -> process_wrap::tokio::CommandWrap {
    use process_wrap::tokio::{CommandWrap, KillOnDrop};
    let mut wrap = CommandWrap::from(cmd);
    // Must come before the job object, which reads these and adds its own.
    #[cfg(windows)]
    wrap.wrap(process_wrap::tokio::CreationFlags(::windows::Win32::System::Threading::CREATE_NO_WINDOW));
    wrap.wrap(KillOnDrop);
    #[cfg(windows)]
    wrap.wrap(process_wrap::tokio::JobObject);
    #[cfg(unix)]
    wrap.wrap(process_wrap::tokio::ProcessGroup::leader());
    wrap
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
