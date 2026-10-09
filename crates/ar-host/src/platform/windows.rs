//! Windows: DPI awareness, session id, console attachment, elevation and the input desktop.

use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
    System::{
        RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId},
        StationsAndDesktops::{CloseDesktop, DESKTOP_ACCESS_FLAGS, GetUserObjectInformationW, OpenInputDesktop, UOI_NAME},
        Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcessToken},
    },
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
};

use super::SessionState;

/// Per-monitor-v2 DPI awareness makes screen capture, UI bounds and input injection share one
/// physical-pixel coordinate space; without it every click on a scaled display lands elsewhere.
pub fn init_process() {
    // Fails harmlessly if a manifest already set the awareness.
    // SAFETY: plain Win32 call with a constant argument.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

pub fn session_state() -> SessionState {
    let session_id = current_session_id();
    // SAFETY: no arguments; returns 0xFFFFFFFF when no session is attached to the console.
    let console = unsafe { WTSGetActiveConsoleSessionId() };
    let on_console = session_id.map(|s| s == console);
    let elevated = is_elevated();
    let input_desktop = input_desktop_name();

    let problem = if session_id == Some(0) {
        Some("the host runs in the services session (session 0), which has no desktop; start it from the logged-in user's session".to_string())
    } else if input_desktop.as_deref().is_some_and(|d| !d.eq_ignore_ascii_case("Default")) || input_desktop.is_none() {
        Some("the desktop is locked or showing a security prompt".to_string())
    } else {
        None
    };
    SessionState {
        session_id,
        on_console,
        elevated,
        input_desktop,
        desktop_usable: problem.is_none(),
        problem,
    }
}

fn current_session_id() -> Option<u32> {
    let mut id = 0u32;
    // SAFETY: `id` is a valid out pointer.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id) }.ok().map(|_| id)
}

fn is_elevated() -> Option<bool> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle needs no closing; `token` is closed below.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
    let mut elevation = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    // SAFETY: buffer and length describe `elevation`.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };
    // SAFETY: `token` was opened above.
    let _ = unsafe { CloseHandle(token) };
    ok.ok().map(|_| elevation.TokenIsElevated != 0)
}

/// Name of the desktop that currently receives input, or `None` when it cannot be opened (which
/// happens while the secure desktop is active).
fn input_desktop_name() -> Option<String> {
    // SAFETY: desktop handle is closed below; buffer and length describe `buf`.
    unsafe {
        let desk = OpenInputDesktop(Default::default(), false, DESKTOP_ACCESS_FLAGS(0)).ok()?;
        let mut buf = [0u16; 256];
        let mut needed = 0u32;
        let ok = GetUserObjectInformationW(
            HANDLE(desk.0),
            UOI_NAME,
            Some(buf.as_mut_ptr() as *mut _),
            (buf.len() * 2) as u32,
            Some(&mut needed),
        );
        let _ = CloseDesktop(desk);
        ok.ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}
