//! Windows: DPI awareness, session id, console attachment, elevation and the input desktop.

use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Globalization::{CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar},
    Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_ELEVATION_TYPE, TOKEN_INFORMATION_CLASS, TOKEN_QUERY,
        TokenElevation, TokenElevationType, TokenElevationTypeDefault,
    },
    System::{
        RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId},
        StationsAndDesktops::{CloseDesktop, DESKTOP_ACCESS_FLAGS, GetUserObjectInformationW, OpenInputDesktop, UOI_NAME},
        Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcessToken},
    },
    UI::HiDpi::{
        AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DPI_AWARENESS_PER_MONITOR_AWARE,
        DPI_AWARENESS_SYSTEM_AWARE, GetAwarenessFromDpiAwarenessContext, GetDpiAwarenessContextForProcess,
        SetProcessDpiAwarenessContext,
    },
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

/// Reads one fixed-size piece of this process's token. `T` must be the structure Windows returns
/// for `class` (`TOKEN_ELEVATION` for `TokenElevation`, ...).
fn token_info<T: Default>(class: TOKEN_INFORMATION_CLASS) -> Option<T> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle needs no closing; `token` is closed below.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.ok()?;
    let mut value = T::default();
    let mut len = 0u32;
    // SAFETY: buffer and length describe `value`.
    let ok = unsafe {
        GetTokenInformation(token, class, Some(&mut value as *mut T as *mut _), std::mem::size_of::<T>() as u32, &mut len)
    };
    // SAFETY: `token` was opened above.
    let _ = unsafe { CloseHandle(token) };
    ok.ok().map(|_| value)
}

fn is_elevated() -> Option<bool> {
    token_info::<TOKEN_ELEVATION>(TokenElevation).map(|e| e.TokenIsElevated != 0)
}

pub fn rights() -> super::Rights {
    use super::Rights;
    if is_elevated() == Some(true) {
        return Rights::Elevated;
    }
    // A split ("limited") token belongs to an administrator; a plain unelevated token, with UAC on,
    // to a standard user.
    match token_info::<TOKEN_ELEVATION_TYPE>(TokenElevationType) {
        Some(t) if t == TokenElevationTypeDefault => Rights::StandardUser,
        _ => Rights::AdminNotElevated,
    }
}

/// The awareness the process actually ended up with: a manifest or compatibility shim can override
/// what [`init_process`] asked for, and only per-monitor-v2 keeps capture and input in one space.
pub fn dpi_awareness() -> &'static str {
    // SAFETY: a null process handle means the current process; the returned context is a value
    // that needs no freeing.
    unsafe {
        let ctx = GetDpiAwarenessContextForProcess(HANDLE::default());
        if AreDpiAwarenessContextsEqual(ctx, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).as_bool() {
            return "per_monitor_v2";
        }
        match GetAwarenessFromDpiAwarenessContext(ctx) {
            a if a == DPI_AWARENESS_PER_MONITOR_AWARE => "per_monitor",
            a if a == DPI_AWARENESS_SYSTEM_AWARE => "system",
            _ => "unaware",
        }
    }
}

/// Decodes bytes in the console's OEM code page.
pub fn decode_oem(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return Some(String::new());
    }
    let flags = MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0);
    // SAFETY: both calls get correctly sized slices; the first only measures.
    unsafe {
        let len = MultiByteToWideChar(CP_OEMCP, flags, bytes, None);
        if len <= 0 {
            return None;
        }
        let mut wide = vec![0u16; len as usize];
        let written = MultiByteToWideChar(CP_OEMCP, flags, bytes, Some(&mut wide));
        (written > 0).then(|| String::from_utf16_lossy(&wide[..written as usize]))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rights_agree_with_elevation() {
        let r = rights();
        println!("rights: {r:?}");
        assert_eq!(r == crate::platform::Rights::Elevated, is_elevated() == Some(true));
    }
}
