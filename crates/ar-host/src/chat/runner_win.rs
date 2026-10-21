//! Windows: the endpoint a SYSTEM host's agent runner connects to, and starting that runner in the
//! installing user's session with the user's own token (see [`super::runner`]).
//!
//! - **Which session**: the one whose user is the installing account (its SID came with the
//!   service's command line). A disconnected session counts: on Windows 11, locking the screen
//!   moves the console to a new session and leaves the user's session disconnected, with the
//!   user's programs (and agents) still running in it. An active session is preferred.
//! - **Which token**: `WTSQueryUserToken`, the token Windows made for that sign-in. For an
//!   administrator under UAC that is the filtered one, so the agents run unelevated, as they would
//!   from the user's own terminal.
//! - **Out of the host's job**: started with `CREATE_BREAKAWAY_FROM_JOB` (the service's job for the
//!   host allows it), so it outlives the host when the service restarts it.
//! - **The key** travels in the environment block made for the user's token, which only that user
//!   (and administrators) can read; the runner removes it from its own environment first thing.
//!   Nothing of the host's (handles, environment) is inherited.

use std::{ffi::c_void, path::Path};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::Profile;
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE, HLOCAL, LocalFree},
        Security::{Authorization::ConvertSidToStringSidW, GetTokenInformation, TOKEN_USER, TokenUser},
        System::{
            Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock},
            RemoteDesktop::{
                WTS_CURRENT_SERVER_HANDLE, WTS_SESSION_INFOW, WTSActive, WTSDisconnected, WTSEnumerateSessionsW,
                WTSFreeMemory, WTSQueryUserToken,
            },
            Threading::{
                CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
                PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW,
            },
        },
    },
    core::{PCWSTR, PWSTR},
};

use super::runner::{self, KEY_ENV, RunnerLink, Wire};
use crate::install::quote_arg;

/// Binds the runner endpoint (admitting SYSTEM and `owner_sid`), serves the runner that connects
/// with the key, and returns the link the chat manager uses. The runner itself starts when a
/// request first needs it.
pub fn start(profile: &Profile, owner_sid: &str) -> Result<RunnerLink> {
    let key = runner::load_or_create_key(&profile.chat_runner_key_path()?)?;
    let endpoint = profile.chat_runner_endpoint()?;
    let mut listener = ar_net::ipc::Listener::bind_shared(&endpoint, &[owner_sid.to_string()])
        .with_context(|| format!("bind {}", endpoint.display()))?;
    let exe = std::env::current_exe().context("locate this program")?;
    let root = profile.root().to_path_buf();
    let sid = owner_sid.to_string();
    let link = RunnerLink::new(Box::new(move || {
        let (session, pid) = spawn(&exe, &root, &sid, &runner::key_hex(&key))?;
        tracing::info!(session, pid, "started the agent runner in the user's session");
        Ok(())
    }));
    let serving = link.clone();
    tokio::spawn(async move {
        loop {
            let conn = match listener.accept().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(%e, "agent runner connection failed");
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
            };
            let link = serving.clone();
            tokio::spawn(async move {
                let (raw, _) = conn.into_raw();
                let mut wire = Wire::new(raw);
                match runner::handshake_host(&mut wire, &key).await {
                    Ok(pid) => {
                        tracing::info!(pid, "the agent runner connected");
                        link.serve(wire).await;
                    }
                    Err(e) => tracing::warn!("refused a connection to the agent runner endpoint: {e:#}"),
                }
            });
        }
    });
    Ok(link)
}

/// Starts `hostw --home <profile> --chat-runner` in the session of `owner_sid`, as that user.
/// Returns the session and the process id.
fn spawn(exe: &Path, profile: &Path, owner_sid: &str, key_hex: &str) -> Result<(u32, u32)> {
    let (session, token) = user_session(owner_sid)?;
    let env = environment(&token, &[(KEY_ENV, key_hex)])?;
    let command =
        format!("{} --home {} --chat-runner", quote_arg(&exe.to_string_lossy()), quote_arg(&profile.to_string_lossy()));
    let cwd = wide(&exe.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());
    let base = CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW;
    match create(&token, &command, &env, &cwd, base | CREATE_BREAKAWAY_FROM_JOB) {
        Ok(pid) => Ok((session, pid)),
        // A job that does not allow breaking away (the host started some other way): the runner
        // then ends with the host, which only costs the agents on a host restart.
        Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => {
            tracing::warn!("cannot start the agent runner outside the host's job; it will stop with the host");
            let pid = create(&token, &command, &env, &cwd, base)
                .with_context(|| format!("start the agent runner in session {session}"))?;
            Ok((session, pid))
        }
        Err(e) => Err(anyhow!(e)).with_context(|| format!("start the agent runner in session {session}")),
    }
}

fn create(
    token: &Handle,
    command: &str,
    env: &[u16],
    cwd: &[u16],
    flags: PROCESS_CREATION_FLAGS,
) -> windows::core::Result<u32> {
    let mut command = wide(command);
    let mut desktop = wide(r"winsta0\default");
    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        lpDesktop: PWSTR(desktop.as_mut_ptr()),
        ..Default::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: every buffer is live and NUL-terminated (the environment block double-NUL) for the
    // call; no handle is inherited; the returned handles are closed at once.
    unsafe {
        CreateProcessAsUserW(
            Some(token.0),
            PCWSTR::null(),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            flags,
            Some(env.as_ptr().cast()),
            PCWSTR(cwd.as_ptr()),
            &startup,
            &mut info,
        )?;
        let _ = CloseHandle(info.hThread);
        let _ = CloseHandle(info.hProcess);
    }
    Ok(info.dwProcessId)
}

/// The session of the account `sid` (active preferred, disconnected accepted) and its user token.
fn user_session(sid: &str) -> Result<(u32, Handle)> {
    let mut list: *mut WTS_SESSION_INFOW = std::ptr::null_mut();
    let mut count = 0u32;
    // SAFETY: the list is freed with WTSFreeMemory; entries are read within its bounds.
    unsafe {
        WTSEnumerateSessionsW(Some(WTS_CURRENT_SERVER_HANDLE), 0, 1, &mut list, &mut count)
            .context("list the sessions")?;
        let sessions = std::slice::from_raw_parts(list, count as usize).to_vec();
        WTSFreeMemory(list as *mut c_void);
        let mut found: Option<(u32, Handle, bool)> = None;
        for s in sessions {
            let active = s.State == WTSActive;
            if !(active || s.State == WTSDisconnected) {
                continue;
            }
            let mut token = HANDLE::default();
            if WTSQueryUserToken(s.SessionId, &mut token).is_err() {
                continue; // no user signed in there
            }
            let token = Handle(token);
            if !token_sid(&token).is_some_and(|t| t.eq_ignore_ascii_case(sid)) {
                continue;
            }
            if found.as_ref().is_none_or(|(_, _, a)| !a && active) {
                found = Some((s.SessionId, token, active));
            }
        }
        match found {
            Some((id, token, _)) => Ok((id, token)),
            None => bail!(
                "the account that installed the service ({sid}) is not signed in on this computer; coding agents \
                 run as that user and need a session (an active or disconnected one)"
            ),
        }
    }
}

/// The SID of a token's user, as `S-1-…`.
fn token_sid(token: &Handle) -> Option<String> {
    // SAFETY: the buffer is u64-aligned and as large as the API asked; the string from
    // ConvertSidToStringSidW is freed with LocalFree.
    unsafe {
        let mut len = 0u32;
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
        GetTokenInformation(token.0, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len).ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut s).ok()?;
        let out = s.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(s.0.cast())));
        out
    }
}

/// The user's environment block (as Windows makes it at sign-in) with `extra` variables added.
fn environment(token: &Handle, extra: &[(&str, &str)]) -> Result<Vec<u16>> {
    let mut block: *mut c_void = std::ptr::null_mut();
    // SAFETY: the block is read up to its double NUL and freed with DestroyEnvironmentBlock.
    let mut out = unsafe {
        CreateEnvironmentBlock(&mut block, Some(token.0), false).context("build the user's environment")?;
        let p = block as *const u16;
        let mut i = 0usize;
        while !(*p.add(i) == 0 && *p.add(i + 1) == 0) {
            i += 1;
        }
        let v = std::slice::from_raw_parts(p, i + 1).to_vec(); // entries, each NUL-terminated
        let _ = DestroyEnvironmentBlock(block);
        v
    };
    if out == [0] {
        out.clear(); // an empty block is just its terminator
    }
    for (k, v) in extra {
        out.extend(format!("{k}={v}").encode_utf16());
        out.push(0);
    }
    out.push(0);
    Ok(out)
}

/// An owned handle.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: owned, closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_token_has_a_sid_and_an_environment() {
        use windows::Win32::{
            Security::TOKEN_QUERY,
            System::Threading::{GetCurrentProcess, OpenProcessToken},
        };
        let mut t = HANDLE::default();
        // SAFETY: plain query of this process's token.
        unsafe {
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | windows::Win32::Security::TOKEN_DUPLICATE, &mut t)
        }
        .unwrap();
        let token = Handle(t);
        let sid = token_sid(&token).unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
        let env = environment(&token, &[("AI_REMOTE_TEST_EXTRA", "x")]).unwrap();
        assert_eq!(&env[env.len() - 2..], &[0, 0]);
        let text = String::from_utf16_lossy(&env);
        assert!(text.split('\0').any(|e| e == "AI_REMOTE_TEST_EXTRA=x"));
        assert!(text.split('\0').any(|e| e.to_ascii_uppercase().starts_with("PATH=")), "{text}");
    }

    #[test]
    fn nobody_is_found_for_an_unknown_account() {
        // Listing sessions works for any user; querying their tokens needs SYSTEM, so as a normal
        // user every session is skipped and the account is "not signed in".
        let e = user_session("S-1-5-21-0-0-0-4242").err().expect("not found").to_string();
        assert!(e.contains("not signed in"), "{e}");
    }
}
