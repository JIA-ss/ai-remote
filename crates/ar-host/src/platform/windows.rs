//! Windows: DPI awareness, session id, console attachment, elevation, the input desktop, and the
//! few input helpers enigo does not cover (virtual-desktop pointer moves, layout lookups, UIPI).

use std::cell::RefCell;

use anyhow::{Result, anyhow};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE, HWND, POINT},
    Globalization::{CP_OEMCP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS, MultiByteToWideChar},
    Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_ELEVATION, TOKEN_ELEVATION_TYPE,
        TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TokenElevation, TokenElevationType,
        TokenElevationTypeDefault, TokenIntegrityLevel,
    },
    System::{
        Console::GetConsoleWindow,
        RemoteDesktop::{ProcessIdToSessionId, WTSGetActiveConsoleSessionId},
        StationsAndDesktops::{
            CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CREATEMENU, DESKTOP_CREATEWINDOW, DESKTOP_ENUMERATE,
            DESKTOP_HOOKCONTROL, DESKTOP_READOBJECTS, DESKTOP_SWITCHDESKTOP, DESKTOP_WRITEOBJECTS, GetThreadDesktop,
            GetUserObjectInformationW, HDESK, OpenInputDesktop, SetThreadDesktop, UOI_NAME,
        },
        Threading::{
            GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId, OpenProcess, OpenProcessToken,
            PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
    UI::{
        HiDpi::{
            AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DPI_AWARENESS_PER_MONITOR_AWARE,
            DPI_AWARENESS_SYSTEM_AWARE, GetAwarenessFromDpiAwarenessContext, GetDpiAwarenessContextForProcess,
            SetProcessDpiAwarenessContext,
        },
        Input::KeyboardAndMouse::{
            GetKeyboardLayout, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE,
            MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput, VkKeyScanExW,
        },
        WindowsAndMessaging::{
            GA_ROOT, GA_ROOTOWNER, GetAncestor, GetCursorPos, GetForegroundWindow, GetSystemMetrics,
            GetWindowThreadProcessId, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
            SetCursorPos, WindowFromPoint,
        },
    },
};

use super::{InputTarget, InputTargetCheck, SessionState};

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
    SessionState { session_id, on_console, elevated, input_desktop, desktop_usable: problem.is_none(), problem }
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
        GetTokenInformation(
            token,
            class,
            Some(&mut value as *mut T as *mut _),
            std::mem::size_of::<T>() as u32,
            &mut len,
        )
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
    // SAFETY: the opened handle is closed before returning.
    unsafe {
        let desk = OpenInputDesktop(Default::default(), false, DESKTOP_ACCESS_FLAGS(0)).ok()?;
        let name = desktop_name(desk);
        let _ = CloseDesktop(desk);
        name
    }
}

/// The name of the desktop `desk` refers to (`Default`, `Winlogon`, a screen-saver desktop, ...),
/// or `None` if it cannot be read.
fn desktop_name(desk: HDESK) -> Option<String> {
    let mut buf = [0u16; 256];
    let mut needed = 0u32;
    // SAFETY: `desk` is a live desktop handle; buffer and length describe `buf`.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desk.0),
            UOI_NAME,
            Some(buf.as_mut_ptr() as *mut _),
            (buf.len() * 2) as u32,
            Some(&mut needed),
        )
        .ok()?;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Desktop access for opening the input desktop and attaching a thread to it: the same set the
/// capture side uses (`stream::capture::dda`).
const INPUT_DESKTOP_ACCESS: DESKTOP_ACCESS_FLAGS = DESKTOP_ACCESS_FLAGS(
    DESKTOP_CREATEMENU.0
        | DESKTOP_CREATEWINDOW.0
        | DESKTOP_ENUMERATE.0
        | DESKTOP_HOOKCONTROL.0
        | DESKTOP_WRITEOBJECTS.0
        | DESKTOP_READOBJECTS.0
        | DESKTOP_SWITCHDESKTOP.0,
);

thread_local! {
    /// The input desktop this thread is attached to, kept alive while the thread stays on it and
    /// closed only after a later switch (`CloseDesktop` refuses a desktop the thread still uses).
    static INPUT_DESKTOP: RefCell<Option<HDESK>> = const { RefCell::new(None) };
}

/// Attaches the calling thread to the desktop that currently receives input, so injected input
/// (`SendInput`, `SetCursorPos`) reaches it. Injection goes to the desktop the calling thread is
/// attached to; after a desktop switch — unlocking, a UAC prompt, an app opening its own desktop —
/// a thread can be left on a stale desktop where input lands nowhere. Call it right before
/// injecting.
///
/// Best-effort and idempotent: when the input desktop cannot be opened (a user-session process
/// cannot open the secure desktop) or the switch fails, the thread is left where it is and
/// injection proceeds exactly as before, so the unlocked path never regresses. Mirrors the capture
/// side (`stream::capture::dda`'s `InputDesktop`) and RustDesk's `input_service` `try_change_desktop`;
/// it earns its keep once the host runs as SYSTEM in the session and injection must follow the lock
/// screen and other secure desktops.
pub fn follow_input_desktop() {
    // SAFETY: the newly opened handle is either closed here or handed to the thread-local, which
    // closes the previous handle only after this thread has switched off it. A fresh injection
    // thread owns no windows or hooks (this runs before enigo creates anything), which
    // `SetThreadDesktop` requires.
    unsafe {
        let Ok(input) = OpenInputDesktop(Default::default(), false, INPUT_DESKTOP_ACCESS) else {
            return;
        };
        let name = desktop_name(input);
        let current = GetThreadDesktop(GetCurrentThreadId()).ok().and_then(desktop_name);
        if name == current || SetThreadDesktop(input).is_err() {
            let _ = CloseDesktop(input);
            return;
        }
        INPUT_DESKTOP.with(|cell| {
            if let Some(previous) = cell.borrow_mut().replace(input) {
                let _ = CloseDesktop(previous);
            }
        });
        tracing::debug!(desktop = name.as_deref().unwrap_or("?"), "input thread switched desktop");
    }
}

/// Bounds of the virtual desktop (all monitors) in physical pixels: `(x, y, width, height)`. The
/// origin is negative when a monitor sits left of or above the primary one.
pub fn virtual_screen() -> (i32, i32, i32, i32) {
    // SAFETY: plain metric queries.
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

/// Maps a screen pixel to the 0..=65535 range `SendInput` takes with `MOUSEEVENTF_ABSOLUTE |
/// MOUSEEVENTF_VIRTUALDESK`, where that range spans the whole virtual desktop. Assuming Windows maps
/// a value back as `floor(n * size / 65536)`, rounding up lands on the requested pixel. Microsoft
/// documents only the end points, so [`move_pointer`] checks where the pointer actually landed.
/// Points outside the desktop are clamped to its bounding box. `None` when the desktop is wider or
/// taller than 65536 pixels, so 16 bits cannot address every pixel.
pub fn virtualdesk_normalize(x: i32, y: i32, desk: (i32, i32, i32, i32)) -> Option<(i32, i32)> {
    let (vx, vy, vw, vh) = desk;
    let axis = |v: i32, origin: i32, size: i32| -> Option<i32> {
        if !(1..=65536).contains(&size) {
            return None;
        }
        let size = i64::from(size);
        let offset = (i64::from(v) - i64::from(origin)).clamp(0, size - 1);
        Some(((offset * 65536 + size - 1) / size).min(65535) as i32)
    };
    Some((axis(x, vx, vw)?, axis(y, vy, vh)?))
}

/// Moves the pointer to a screen pixel with a real injected mouse event, so hover effects, hooks and
/// raw-input apps see it. enigo's absolute move normalises against the primary monitor only and
/// does not set `MOUSEEVENTF_VIRTUALDESK`, so it cannot reliably reach the other monitors.
///
/// Then checks where the pointer landed and puts it on the exact pixel with `SetCursorPos` when the
/// normalised move missed (undocumented rounding). Fails rather than leave the pointer elsewhere (a
/// point between monitors, a cursor confined by an app), so a following click never lands on the
/// wrong spot.
pub fn move_pointer(x: i32, y: i32) -> Result<()> {
    // A desktop beyond 16-bit addressing skips straight to SetCursorPos.
    if let Some((dx, dy)) = virtualdesk_normalize(x, y, virtual_screen()) {
        let input = INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: 0,
                    dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    time: 0,
                    // The marker enigo puts on its events, so all host-injected input can be told apart.
                    dwExtraInfo: enigo::EVENT_MARKER as usize,
                },
            },
        };
        // SAFETY: one fully initialised INPUT; the size argument matches the struct.
        let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
        if sent != 1 {
            return Err(anyhow!("moving the pointer failed: {}", std::io::Error::last_os_error()));
        }
        // Injected moves are processed asynchronously; give it a moment before correcting it.
        for _ in 0..10 {
            if cursor_pos() == Some((x, y)) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    // SAFETY: plain Win32 call (physical pixels under per-monitor-v2 awareness).
    unsafe { SetCursorPos(x, y) }.map_err(|e| anyhow!("moving the pointer failed: {e}"))?;
    match cursor_pos() {
        Some(p) if p == (x, y) => Ok(()),
        Some((px, py)) => Err(anyhow!(
            "the pointer could not be placed at ({x}, {y}); it is at ({px}, {py}). The point may be on no monitor, \
             or an app (often a game) is confining the pointer."
        )),
        None => Err(anyhow!("cannot read the pointer position after moving it")),
    }
}

fn cursor_pos() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    // SAFETY: `p` is a valid out pointer.
    unsafe { GetCursorPos(&mut p) }.ok().map(|_| (p.x, p.y))
}

/// Virtual-key code and the modifiers (bit 1 shift, 2 ctrl, 4 alt) that type `c` on the keyboard
/// layout of the foreground window, or `None` when no key on that layout produces it.
pub fn char_key(c: char) -> Option<(u16, u8)> {
    let mut buf = [0u16; 2];
    let [unit] = *c.encode_utf16(&mut buf) else { return None };
    // SAFETY: plain queries; a null foreground window gives thread 0, i.e. this thread's layout.
    let scan =
        unsafe { VkKeyScanExW(unit, GetKeyboardLayout(GetWindowThreadProcessId(GetForegroundWindow(), None))) } as u16;
    let (vk, state) = ((scan & 0xff) as u8, (scan >> 8) as u8);
    // 0xff in both bytes means no key; other state bits (kana, IME states) are not keys we press.
    (vk != 0xff && state & !0x07 == 0).then_some((u16::from(vk), state))
}

/// Checks the window input aimed at `target` would reach.
///
/// Refused when it belongs to this host (its approval prompt, or the terminal it runs in): a
/// controller must never answer its own prompts or stop the host. Refused too when it belongs to a
/// more privileged process, because Windows would drop the input without any error (UIPI, F15).
/// Doubtful when that process cannot be inspected at all: such processes (SYSTEM, protected) are
/// almost always more privileged, but this is a guess, so the input is sent with a warning.
pub fn check_input_target(target: InputTarget) -> InputTargetCheck {
    // SAFETY: window queries only.
    let hwnd = unsafe {
        match target {
            InputTarget::Focused => GetForegroundWindow(),
            InputTarget::Pointer => match cursor_pos() {
                Some((x, y)) => WindowFromPoint(POINT { x, y }),
                None => return InputTargetCheck::Allowed,
            },
            InputTarget::At(x, y) => WindowFromPoint(POINT { x, y }),
        }
    };
    if hwnd.0.is_null() {
        return InputTargetCheck::Allowed;
    }
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid out pointer; the rest are plain queries.
    let (root, console, own) = unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        (GetAncestor(hwnd, GA_ROOT), GetConsoleWindow(), GetCurrentProcessId())
    };
    // Under ConPTY (Windows Terminal) the console window is a hidden pseudo window whose owner is the
    // visible terminal window, so compare against that owner too. Terminals that do not set the
    // owner (some editors' built-in terminals) cannot be recognised this way.
    let console_windows: [HWND; 2] = if console.0.is_null() {
        [HWND::default(); 2]
    } else {
        // SAFETY: plain query on a valid window handle.
        [console, unsafe { GetAncestor(console, GA_ROOTOWNER) }]
    };
    if pid == own || console_windows.iter().any(|w| !w.0.is_null() && *w == root) {
        return InputTargetCheck::Refused(
            "the input would go to one of ai-remote's own windows (an approval prompt, or the terminal the host \
             runs in). That is refused so a controller can never answer its own approval prompts or stop the \
             host. Wait for the owner to answer, or act on a different window."
                .into(),
        );
    }
    let Some(ours) = integrity_level(None) else { return InputTargetCheck::Allowed };
    let Some(theirs) = integrity_level(Some(pid)) else {
        return InputTargetCheck::Doubtful(
            "the target window belongs to a process this host cannot inspect (it may run as SYSTEM or be \
             protected), so Windows may have discarded the input without an error. Take a screenshot to check \
             whether it arrived."
                .into(),
        );
    };
    if theirs > ours {
        return InputTargetCheck::Refused(format!(
            "the target window belongs to a process running with higher privileges ({} integrity) than this \
             host ({}), so Windows would silently discard the input (UIPI). Ask the owner to run the host as \
             administrator, or work with a different window.",
            integrity_name(theirs),
            integrity_name(ours)
        ));
    }
    InputTargetCheck::Allowed
}

/// Mandatory integrity level (the RID of the token's label: 0x2000 medium, 0x3000 high, ...) of a
/// process, or of this process when `pid` is `None`. `None` when it cannot be read.
fn integrity_level(pid: Option<u32>) -> Option<u32> {
    // SAFETY: both handles are closed below; the buffer is u64-aligned and as large as the API asked.
    unsafe {
        let process = match pid {
            None => GetCurrentProcess(),
            Some(pid) => OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?,
        };
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(process, TOKEN_QUERY, &mut token);
        if pid.is_some() {
            let _ = CloseHandle(process);
        }
        opened.ok()?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        let ok = GetTokenInformation(token, TokenIntegrityLevel, Some(buf.as_mut_ptr().cast()), len, &mut len);
        let _ = CloseHandle(token);
        ok.ok()?;
        let label = &*(buf.as_ptr() as *const TOKEN_MANDATORY_LABEL);
        let count = *GetSidSubAuthorityCount(label.Label.Sid);
        Some(*GetSidSubAuthority(label.Label.Sid, u32::from(count.checked_sub(1)?)))
    }
}

fn integrity_name(level: u32) -> &'static str {
    match level {
        0..0x1000 => "untrusted",
        0x1000..0x2000 => "low",
        0x2000..0x3000 => "medium",
        0x3000..0x4000 => "high",
        _ => "system",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The inverse Windows is assumed to apply to absolute coordinates: `floor(n * size / 65536)`.
    /// Microsoft documents only its end points, so this pins the normalisation to that assumption;
    /// `move_pointer` checks the real landing pixel at run time and corrects it.
    fn back(n: i32, origin: i32, size: i32) -> i32 {
        origin + ((i64::from(n) * i64::from(size)) >> 16) as i32
    }

    #[test]
    fn follow_input_desktop_is_safe_and_idempotent() {
        // The real check is manual (inject, lock, unlock, inject again): see AGENTS.md. Here only
        // that it never panics, survives a second call, and leaves the thread on a readable desktop
        // — including on a build agent where the input desktop may not be openable (then it is a
        // no-op and the thread stays on its original desktop).
        follow_input_desktop();
        follow_input_desktop();
        // SAFETY: the handle is only read for its name, never stored or closed.
        let name = unsafe { GetThreadDesktop(GetCurrentThreadId()).ok().and_then(desktop_name) };
        assert!(name.is_some(), "the thread should stay on a readable desktop");
    }

    #[test]
    fn virtualdesk_normalize_inverts_assumed_mapping() {
        // (-1440, -9, 8000, 2560): two portrait monitors around a 5120x2160 primary.
        for desk in [(-1440, -9, 8000, 2560), (0, 0, 1920, 1080), (0, 0, 1366, 768), (-3, 5, 3, 7), (0, 0, 65536, 1)] {
            let (vx, vy, vw, vh) = desk;
            for x in vx..vx + vw {
                let (nx, _) = virtualdesk_normalize(x, vy, desk).unwrap();
                assert!((0..=65535).contains(&nx));
                assert_eq!(back(nx, vx, vw), x, "x {x} on {desk:?}");
            }
            for y in vy..vy + vh {
                let (_, ny) = virtualdesk_normalize(vx, y, desk).unwrap();
                assert_eq!(back(ny, vy, vh), y, "y {y} on {desk:?}");
            }
        }
    }

    #[test]
    fn virtualdesk_negative_origin_corners() {
        let desk = (-1440, -9, 8000, 2560);
        assert_eq!(virtualdesk_normalize(-1440, -9, desk), Some((0, 0)));
        // The primary monitor's origin sits 1440 px / 9 px into the virtual desktop.
        assert_eq!(virtualdesk_normalize(0, 0, desk), Some((11797, 231)));
        // The last pixel (6559, 2550) is 7999 / 2559 px in.
        assert_eq!(virtualdesk_normalize(6559, 2550, desk), Some((65528, 65511)));
    }

    #[test]
    fn virtualdesk_clamps_and_rejects() {
        let desk = (0, 0, 1920, 1080);
        assert_eq!(virtualdesk_normalize(-50, 5000, desk), virtualdesk_normalize(0, 1079, desk));
        assert_eq!(virtualdesk_normalize(0, 0, (0, 0, 70000, 1080)), None);
        assert_eq!(virtualdesk_normalize(0, 0, (0, 0, 0, 1080)), None);
    }

    #[test]
    fn reads_own_integrity_level() {
        // A read-only token query; every real process has an integrity label.
        let level = integrity_level(None).expect("integrity level of this process");
        assert!(level >= 0x1000, "unexpected integrity {level:#x}");
    }

    #[test]
    fn rights_agree_with_elevation() {
        let r = rights();
        println!("rights: {r:?}");
        assert_eq!(r == crate::platform::Rights::Elevated, is_elevated() == Some(true));
    }
}
