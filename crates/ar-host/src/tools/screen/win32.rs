//! Windows: bring a window to the foreground, and the window traits list_windows reports.
//!
//! `SetForegroundWindow` only works for the process that received the last input event (or that
//! the foreground process allowed); otherwise Windows just flashes the taskbar button. The host is
//! a background process, so a plain call usually fails. Escalation, stopping as soon as the window
//! is in front:
//!
//! 1. plain `SetForegroundWindow`;
//! 2. inject one empty mouse input first (no movement, no buttons) so this process owns the last
//!    input event, then try again: the trick PowerToys uses before switching windows;
//! 3. attach to the foreground thread's input state, then raise and activate the window.
//!
//! Nothing here waits on the target window's thread: restoring and raising are posted
//! (`ShowWindowAsync`, `SetWindowPos` with `SWP_ASYNCWINDOWPOS`), because their synchronous forms
//! block for as long as the window does not pump messages, and `IsHungAppWindow` only reports a
//! window after about 5 s without a response. Hung windows are still refused up front, and so is
//! attaching to a hung foreground thread. The async caller adds a timeout on top.

use std::{
    ffi::c_void,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use windows::Win32::{
    Foundation::HWND,
    System::Threading::{AttachThreadInput, GetCurrentProcessId, GetCurrentThreadId},
    UI::{
        Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_MOUSE, MOUSE_EVENT_FLAGS, MOUSEINPUT, SendInput},
        WindowsAndMessaging::{
            GA_ROOT, GA_ROOTOWNER, GWL_EXSTYLE, GetAncestor, GetForegroundWindow, GetWindowLongW,
            GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, HWND_TOP, IsHungAppWindow, IsIconic,
            IsWindow, IsWindowVisible, MSG, PM_NOREMOVE, PeekMessageW, SW_RESTORE, SWP_ASYNCWINDOWPOS, SWP_NOMOVE,
            SWP_NOSIZE, SetForegroundWindow, SetWindowPos, ShowWindowAsync, WS_EX_LAYERED, WS_EX_NOACTIVATE,
            WS_EX_TOPMOST, WS_EX_TRANSPARENT,
        },
    },
};

/// What `focus_window` did.
pub struct FocusOutcome {
    pub foreground: bool,
    /// It was minimized and a restore was requested.
    pub restored: bool,
    /// Still minimized at the end: the window has not processed the restore (busy).
    pub still_minimized: bool,
    /// The step that brought the window to the front (or the last one tried).
    pub method: &'static str,
    pub title: String,
    pub foreground_window: Option<u32>,
    pub foreground_title: String,
}

/// How long each step waits for the foreground to change.
const STEP_WAIT: Duration = Duration::from_millis(150);
/// How long to wait for a minimized window to process the posted restore.
const RESTORE_WAIT: Duration = Duration::from_millis(500);

pub fn focus_window(id: u32) -> Result<FocusOutcome> {
    let hwnd = hwnd_from_id(id);
    // SAFETY: every call below takes a window handle by value and tolerates stale handles (they
    // fail or return FALSE); out pointers point to locals.
    unsafe {
        if id == 0 || !IsWindow(Some(hwnd)).as_bool() {
            bail!("window {id} does not exist (it may have closed); call list_windows for current ids");
        }
        // Only top-level windows can be foreground; an id of a child window means its top level.
        let root = GetAncestor(hwnd, GA_ROOT);
        let hwnd = if root.is_invalid() { hwnd } else { root };
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == GetCurrentProcessId() {
            bail!("window {id} belongs to the ai-remote host itself and cannot be focused by a controller");
        }
        if !IsWindowVisible(hwnd).as_bool() {
            bail!("window {id} is hidden; only visible windows from list_windows can be focused");
        }
        if IsHungAppWindow(hwnd).as_bool() {
            bail!("window {id} is not responding; wait for it to recover, or ask the user");
        }
        let title = window_title(hwnd);

        let restored = IsIconic(hwnd).as_bool();
        if restored {
            // SetForegroundWindow does not restore a minimized window. Posted, then given a moment,
            // so the activation below applies to the restored window.
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
            wait_until(RESTORE_WAIT, || !IsIconic(hwnd).as_bool());
        }

        let mut method = "already_foreground";
        if !wait_foreground(hwnd, Duration::ZERO) {
            method = "set_foreground_window";
            let _ = SetForegroundWindow(hwnd);
        }
        if !wait_foreground(hwnd, STEP_WAIT) {
            method = "input_unlock";
            empty_input();
            let _ = SetForegroundWindow(hwnd);
            raise(hwnd);
        }
        if !wait_foreground(hwnd, STEP_WAIT) {
            method = "attach_thread_input";
            attach_and_activate(hwnd);
        }
        let foreground = wait_foreground(hwnd, STEP_WAIT);
        let fg = GetForegroundWindow();
        Ok(FocusOutcome {
            foreground,
            restored,
            still_minimized: IsIconic(hwnd).as_bool(),
            method,
            title,
            foreground_window: (!fg.is_invalid()).then(|| id_from_hwnd(fg)),
            foreground_title: if fg.is_invalid() { String::new() } else { window_title(fg) },
        })
    }
}

/// Window ids are xcap's: the `HWND` truncated to 32 bits. Window handles are 32-bit values that
/// Windows sign-extends on 64-bit systems (that is what makes them shareable with 32-bit
/// processes), so sign-extending the id restores the handle exactly.
fn hwnd_from_id(id: u32) -> HWND {
    HWND(id as i32 as isize as *mut c_void)
}

fn id_from_hwnd(hwnd: HWND) -> u32 {
    hwnd.0 as isize as u32
}

/// True once `hwnd` (or a dialog it owns) is the foreground window, polling for up to `wait`.
fn wait_foreground(hwnd: HWND, wait: Duration) -> bool {
    wait_until(wait, || {
        // SAFETY: no arguments / handle by value.
        let fg = unsafe { GetForegroundWindow() };
        fg == hwnd || (!fg.is_invalid() && unsafe { GetAncestor(fg, GA_ROOTOWNER) } == hwnd)
    })
}

/// Polls `done` every 10 ms for up to `wait`; true as soon as it holds.
fn wait_until(wait: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// `BringWindowToTop` without waiting: the request is posted to the window's thread.
fn raise(hwnd: HWND) {
    // SAFETY: handle by value; no pointers.
    let _ = unsafe { SetWindowPos(hwnd, Some(HWND_TOP), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_ASYNCWINDOWPOS) };
}

/// One mouse input with no movement and no buttons. Nothing happens on screen, but this process
/// becomes the one that received the last input event, which lifts the foreground lock for it.
fn empty_input() {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: MOUSE_EVENT_FLAGS(0),
                time: 0,
                // Tag it as ours, like every event enigo injects.
                dwExtraInfo: enigo::EVENT_MARKER as usize,
            },
        },
    };
    // SAFETY: one valid INPUT and its size.
    unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
}

/// Shares the foreground thread's input state for the duration of the activation, so Windows
/// treats the call as coming from the foreground thread.
fn attach_and_activate(hwnd: HWND) {
    // SAFETY: handles by value; `msg` is a local out parameter; attach and detach are paired.
    unsafe {
        let fg = GetForegroundWindow();
        let fg_thread =
            if fg.is_invalid() || IsHungAppWindow(fg).as_bool() { 0 } else { GetWindowThreadProcessId(fg, None) };
        let me = GetCurrentThreadId();
        // AttachThreadInput needs a message queue on this thread; peeking creates one.
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool();
        raise(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(me, fg_thread, false);
        }
    }
}

/// Extended-style facts that tell an overlay from a window an agent can act on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowTraits {
    /// Stays above normal windows (`WS_EX_TOPMOST`).
    pub topmost: bool,
    /// Mouse input passes through it to the windows below (`WS_EX_LAYERED | WS_EX_TRANSPARENT`,
    /// the usual recipe for HUDs and screen overlays): never a click or focus target.
    pub click_through: bool,
    /// Clicking it does not activate it (`WS_EX_NOACTIVATE`: on-screen keyboards, toasts, some
    /// overlays), so focus_window may not keep it in front.
    pub no_activate: bool,
}

pub fn window_traits(id: u32) -> WindowTraits {
    // SAFETY: handle by value; a stale handle reads as 0 (no styles).
    let ex = unsafe { GetWindowLongW(hwnd_from_id(id), GWL_EXSTYLE) } as u32;
    traits_from_ex_style(ex)
}

fn traits_from_ex_style(ex: u32) -> WindowTraits {
    let has = |flag: u32| ex & flag == flag;
    WindowTraits {
        topmost: has(WS_EX_TOPMOST.0),
        click_through: has(WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0),
        no_activate: has(WS_EX_NOACTIVATE.0),
    }
}

fn window_title(hwnd: HWND) -> String {
    // SAFETY: the buffer outlives the call; for windows of other processes GetWindowTextW reads
    // the cached caption without sending them a message, so it cannot hang.
    unsafe {
        let len = GetWindowTextLengthW(hwnd).max(0) as usize;
        if len == 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len + 1];
        let n = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..n.min(len)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_id_round_trips_through_hwnd() {
        for id in [1u32, 0x0003_04A2, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFE] {
            assert_eq!(id_from_hwnd(hwnd_from_id(id)), id);
        }
    }

    #[test]
    fn high_ids_are_sign_extended() {
        assert_eq!(hwnd_from_id(0x8001_0000).0 as isize, 0x8001_0000u32 as i32 as isize);
        assert!((hwnd_from_id(0x8001_0000).0 as isize) < 0);
    }

    #[test]
    fn overlay_traits_come_from_the_extended_style() {
        assert_eq!(traits_from_ex_style(0), WindowTraits::default());
        let hud = traits_from_ex_style(WS_EX_TOPMOST.0 | WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0);
        assert_eq!(hud, WindowTraits { topmost: true, click_through: true, no_activate: true });
        // Layered alone (fades, rounded corners) or transparent alone does not pass clicks through.
        assert!(!traits_from_ex_style(WS_EX_LAYERED.0).click_through);
        assert!(!traits_from_ex_style(WS_EX_TRANSPARENT.0).click_through);
    }

    #[test]
    fn unknown_window_is_refused_without_side_effects() {
        // Id 0 is never a window; the check runs before any foreground call.
        let err = focus_window(0).err().unwrap().to_string();
        assert!(err.contains("does not exist"), "{err}");
    }
}
