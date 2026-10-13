//! Windows service mode (`ai-remote host install --service`): the host as a service, so the lock
//! screen, the sign-in screen after a reboot and UAC prompts can be seen and operated.
//!
//! The service process itself runs as LocalSystem in session 0, where there is no desktop. All it
//! does is keep one copy of `ai-remote-hostw --supervised` running **as SYSTEM inside the active
//! console session** (RustDesk's `run_service` / `launch_server`): a SYSTEM process in that
//! session may open the secure (`Winlogon`) desktop, which the capture thread
//! (`stream::capture::dda`) and the injection thread (`platform::follow_input_desktop`) then follow.
//! When the console session changes (log off, fast user switching, a reboot's sign-in screen) the
//! host is started again in the new one; when it exits abnormally it is restarted with the same
//! back-off as the background supervisor; when it stops cleanly (`ai-remote stop-host`) the
//! service stops too, like the supervisor does.
//!
//! Consequence, stated here as well as in the install plan: that host, and so its `shell`, `jobs`
//! and `files` tools, run as SYSTEM. The gate, levels, policies, approvals and audit log are the
//! same code, but what an admin-level (or bypassed) controller can do with them grows from "what
//! the logged-in user can do" to "what SYSTEM can do".
//!
//! Ctrl+Alt+Del: `SendSAS` works from a service when the `SoftwareSASGeneration` policy allows it
//! (the install plan sets it), so the in-session host asks this process to send it through a named
//! event (`sas_event_name`) instead of pressing the keys, which Windows would ignore.

use std::{
    ffi::c_void,
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicIsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::Profile;
use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_CALL_NOT_IMPLEMENTED, HANDLE, NO_ERROR, WAIT_OBJECT_0},
        Security::{
            Authentication::Identity::SendSAS, DuplicateTokenEx, SecurityIdentification, SetTokenInformation,
            TOKEN_ALL_ACCESS, TokenPrimary, TokenSessionId,
        },
        System::{
            Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock},
            RemoteDesktop::WTSGetActiveConsoleSessionId,
            Services::{
                RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP,
                SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING,
                SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING,
                SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
                StartServiceCtrlDispatcherW,
            },
            Threading::{
                CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateEventW, CreateProcessAsUserW,
                EVENT_MODIFY_STATE, GetCurrentProcess, GetExitCodeProcess, OpenEventW, OpenProcessToken,
                PROCESS_INFORMATION, ResumeThread, STARTUPINFOW, SetEvent, WaitForSingleObject,
            },
        },
    },
    core::{PCWSTR, PWSTR},
};

use crate::{
    install::quote_arg,
    supervise::{Backoff, EXIT_ALREADY_RUNNING},
};

/// What the service needs to know, from its command line.
#[derive(Debug, Clone)]
pub struct ServiceArgs {
    pub profile: Profile,
    /// SID of the account that installed the service: the host's control endpoint admits it
    /// besides SYSTEM, so that account's CLI and desktop app keep working.
    pub owner_sid: String,
    /// The `ai-remote-hostw` executable to start in the session (normally this one).
    pub hostw: PathBuf,
}

/// `ai-remote-host-<8 hex digits>`: one service per profile.
pub fn service_name(profile: &Profile) -> String {
    format!("ai-remote-host-{}", &profile.instance_id()[..8])
}

/// The named event the in-session host signals to have the service send Ctrl+Alt+Del.
pub fn sas_event_name(profile: &Profile) -> String {
    format!(r"Global\ai-remote-sas-{}", &profile.instance_id()[..8])
}

/// Whether the service that would send Ctrl+Alt+Del for this profile is running (its event exists
/// and this process may signal it).
pub fn secure_attention_available(profile: &Profile) -> bool {
    let name = wide(&sas_event_name(profile));
    // SAFETY: the handle, if opened, is closed at once.
    unsafe {
        match OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(name.as_ptr())) {
            Ok(h) => {
                let _ = CloseHandle(h);
                true
            }
            Err(_) => false,
        }
    }
}

/// Asks the service to send Ctrl+Alt+Del (`event` from [`sas_event_name`]).
pub fn request_secure_attention(event: &str) -> Result<()> {
    let name = wide(event);
    // SAFETY: the handle is closed after signalling.
    unsafe {
        let h = OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(name.as_ptr()))
            .map_err(|e| anyhow!("the ai-remote service is not running, so Ctrl+Alt+Del cannot be sent ({e})"))?;
        let set = SetEvent(h);
        let _ = CloseHandle(h);
        set.map_err(|e| anyhow!("cannot signal the service: {e}"))
    }
}

/// The one service of this process; the SCM callbacks have no other way to reach it.
struct Service {
    name: Vec<u16>,
    args: ServiceArgs,
    stop: AtomicBool,
    status: AtomicIsize,
}

static SERVICE: OnceLock<Service> = OnceLock::new();

/// Runs as the service: connects to the service control manager and returns when the service has
/// stopped. Fails at once when not started by the SCM (`sc start`), which is how `--service` run
/// from a terminal shows up.
pub fn run(args: ServiceArgs) -> Result<()> {
    let name = wide(&service_name(&args.profile));
    let service = Service { name, args, stop: AtomicBool::new(false), status: AtomicIsize::new(0) };
    let service = match SERVICE.set(service) {
        Ok(()) => SERVICE.get().expect("just set"),
        Err(_) => bail!("the service was already started in this process"),
    };
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(service.name.as_ptr().cast_mut()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW { lpServiceName: PWSTR::null(), lpServiceProc: None },
    ];
    // SAFETY: the table ends with a null entry and the name lives in a static for the whole call.
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }
        .context("connect to the service control manager (this must be started by the service, not from a terminal)")
}

impl Service {
    fn report(&self, state: SERVICE_STATUS_CURRENT_STATE) {
        let handle = SERVICE_STATUS_HANDLE(self.status.load(Ordering::SeqCst) as *mut c_void);
        if handle.0.is_null() {
            return;
        }
        let pending = state == SERVICE_STOP_PENDING;
        let status = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: state,
            dwControlsAccepted: if state == SERVICE_RUNNING {
                SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
            } else {
                0
            },
            dwWin32ExitCode: 0,
            dwServiceSpecificExitCode: 0,
            dwCheckPoint: 0,
            dwWaitHint: if pending { 5000 } else { 0 },
        };
        // SAFETY: a valid status handle from RegisterServiceCtrlHandlerExW and a complete struct.
        if let Err(e) = unsafe { SetServiceStatus(handle, &status) } {
            tracing::warn!(%e, "cannot report the service status");
        }
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let Some(service) = SERVICE.get() else { return };
    // SAFETY: the name is a NUL-terminated string that lives as long as the process.
    let handle =
        match unsafe { RegisterServiceCtrlHandlerExW(PCWSTR(service.name.as_ptr()), Some(control_handler), None) } {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(%e, "cannot register the service control handler");
                return;
            }
        };
    service.status.store(handle.0 as isize, Ordering::SeqCst);
    service.report(SERVICE_RUNNING);
    tracing::info!(pid = std::process::id(), profile = %service.args.profile.root().display(), "ai-remote service started");
    supervise(service);
    tracing::info!("ai-remote service stopped");
    service.report(SERVICE_STOPPED);
}

unsafe extern "system" fn control_handler(control: u32, _event: u32, _data: *mut c_void, _ctx: *mut c_void) -> u32 {
    let Some(service) = SERVICE.get() else { return ERROR_CALL_NOT_IMPLEMENTED.0 };
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            service.stop.store(true, Ordering::SeqCst);
            service.report(SERVICE_STOP_PENDING);
            NO_ERROR.0
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR.0,
        _ => ERROR_CALL_NOT_IMPLEMENTED.0,
    }
}

/// How long to wait before trying again while another host (one started in a terminal) holds the
/// profile.
const RETRY_WHILE_TAKEN: Duration = Duration::from_secs(30);

/// Keeps the host running in the active console session until the service is stopped.
fn supervise(service: &Service) {
    let sas = match SasEvent::create(&service.args.profile) {
        Ok(ev) => Some(ev),
        Err(e) => {
            tracing::warn!(%e, "cannot create the Ctrl+Alt+Del event; the host cannot send it");
            None
        }
    };
    let mut child: Option<Child> = None;
    let mut backoff = Backoff::default();
    let mut next_start = Instant::now();
    let mut waiting_for_session = false;
    while !service.stop.load(Ordering::SeqCst) {
        let session = active_console_session();
        if let Some(c) = &child {
            if Some(c.session) != session {
                tracing::info!(
                    from = c.session,
                    to = session.map(|s| s.to_string()).unwrap_or_else(|| "none".into()),
                    "the console session changed; stopping the host in the old one"
                );
                child = None;
                backoff = Backoff::default();
                next_start = Instant::now();
            } else if let Some(code) = c.exit_code() {
                let ran = c.started.elapsed();
                child = None;
                match code {
                    0 => {
                        tracing::info!("the host stopped cleanly; the service stops with it");
                        return;
                    }
                    code if code == u32::from(EXIT_ALREADY_RUNNING) => {
                        tracing::info!(
                            "another host holds this profile (started in a terminal?); trying again in {} s",
                            RETRY_WHILE_TAKEN.as_secs()
                        );
                        next_start = Instant::now() + RETRY_WHILE_TAKEN;
                    }
                    code => {
                        let delay = backoff.after(ran);
                        tracing::error!(code, "the host exited unexpectedly; restarting it in {} s", delay.as_secs());
                        next_start = Instant::now() + delay;
                    }
                }
            }
        }
        if child.is_none() {
            match session {
                None => {
                    if !waiting_for_session {
                        tracing::info!("no session is attached to the console yet; waiting");
                        waiting_for_session = true;
                    }
                }
                Some(session) if Instant::now() >= next_start => {
                    waiting_for_session = false;
                    match Child::launch(&service.args, session) {
                        Ok(c) => {
                            tracing::info!(session, pid = c.pid, "started the host in the console session");
                            child = Some(c);
                        }
                        Err(e) => {
                            let delay = backoff.after(Duration::ZERO);
                            tracing::error!(error = %format!("{e:#}"), "cannot start the host; trying again in {} s", delay.as_secs());
                            next_start = Instant::now() + delay;
                        }
                    }
                }
                Some(_) => {}
            }
        }
        // One second between checks, cut short by a Ctrl+Alt+Del request from the host.
        match &sas {
            Some(ev) if ev.wait(Duration::from_secs(1)) => {
                tracing::info!("sending Ctrl+Alt+Del at the host's request");
                // SAFETY: plain call; `false` = on behalf of a service.
                unsafe { SendSAS(false) };
            }
            Some(_) => {}
            None => std::thread::sleep(Duration::from_secs(1)),
        }
    }
    if child.is_some() {
        tracing::info!("stopping the host");
        drop(child);
    }
}

/// The session attached to the physical console, `None` when there is none (during a switch).
fn active_console_session() -> Option<u32> {
    // SAFETY: no arguments; 0xFFFFFFFF means no session.
    let id = unsafe { WTSGetActiveConsoleSessionId() };
    (id != u32::MAX).then_some(id)
}

/// The host running in a session: its process handle and the kill-on-close job it sits in, so
/// dropping this ends the host and everything it started (background jobs break away, as with the
/// supervisor).
struct Child {
    process: Handle,
    _job: win32job::Job,
    session: u32,
    pid: u32,
    started: Instant,
}

impl Child {
    /// Starts `hostw --home <profile> --supervised --owner-sid <sid>` as SYSTEM in `session`, on
    /// its default desktop: this process's own token, duplicated and re-pointed at the session.
    fn launch(args: &ServiceArgs, session: u32) -> Result<Self> {
        let mut limits = win32job::ExtendedLimitInfo::new();
        limits.limit_kill_on_job_close().limit_breakaway_ok();
        let job = win32job::Job::create_with_limit_info(&limits).map_err(|e| anyhow!("create job object: {e}"))?;

        let token = session_token(session)?;
        let env = EnvBlock::for_token(&token)?;
        let command = format!(
            "{} --home {} --supervised --owner-sid {}",
            quote_arg(&args.hostw.to_string_lossy()),
            quote_arg(&args.profile.root().to_string_lossy()),
            args.owner_sid
        );
        let mut command = wide(&command);
        let mut desktop = wide(r"winsta0\default");
        let cwd = wide(&args.hostw.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());
        let startup = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: every pointer refers to a live, NUL-terminated buffer for the call; the process
        // starts suspended so it joins the job before it can start anything.
        unsafe {
            CreateProcessAsUserW(
                Some(token.0),
                PCWSTR::null(),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | CREATE_SUSPENDED,
                Some(env.0),
                PCWSTR(cwd.as_ptr()),
                &startup,
                &mut info,
            )
            .with_context(|| format!("start {} in session {session}", args.hostw.display()))?;
        }
        let process = Handle(info.hProcess);
        let thread = Handle(info.hThread);
        let assigned =
            job.assign_process(info.hProcess.0 as isize).map_err(|e| anyhow!("assign the host to its job: {e}"));
        // SAFETY: a valid thread handle from CreateProcessAsUserW.
        unsafe { ResumeThread(thread.0) };
        assigned?;
        Ok(Self { process, _job: job, session, pid: info.dwProcessId, started: Instant::now() })
    }

    /// The exit code once the host has exited, `None` while it runs.
    fn exit_code(&self) -> Option<u32> {
        // SAFETY: a valid process handle; zero timeout only polls.
        unsafe {
            if WaitForSingleObject(self.process.0, 0) != WAIT_OBJECT_0 {
                return None;
            }
            let mut code = 0u32;
            GetExitCodeProcess(self.process.0, &mut code).ok().map(|_| code)
        }
    }
}

/// This process's (SYSTEM) token as a primary token for `session`.
fn session_token(session: u32) -> Result<Handle> {
    // SAFETY: handles are owned by `Handle` and closed on drop; the session id outlives the call.
    unsafe {
        let mut own = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own).context("open the service's token")?;
        let own = Handle(own);
        let mut dup = HANDLE::default();
        DuplicateTokenEx(own.0, TOKEN_ALL_ACCESS, None, SecurityIdentification, TokenPrimary, &mut dup)
            .context("duplicate the service's token")?;
        let dup = Handle(dup);
        SetTokenInformation(dup.0, TokenSessionId, (&session as *const u32).cast(), std::mem::size_of::<u32>() as u32)
            .with_context(|| format!("point the token at session {session}"))?;
        Ok(dup)
    }
}

/// An owned Win32 handle.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: owned, closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// The environment block of a token (SYSTEM's profile variables), freed on drop.
struct EnvBlock(*const c_void);

impl EnvBlock {
    fn for_token(token: &Handle) -> Result<Self> {
        let mut env: *mut c_void = std::ptr::null_mut();
        // SAFETY: `env` receives a block that `DestroyEnvironmentBlock` frees in `Drop`.
        unsafe { CreateEnvironmentBlock(&mut env, Some(token.0), false) }.context("build the host's environment")?;
        Ok(Self(env))
    }
}

impl Drop for EnvBlock {
    fn drop(&mut self) {
        // SAFETY: from CreateEnvironmentBlock, freed once.
        let _ = unsafe { DestroyEnvironmentBlock(self.0) };
    }
}

/// The auto-reset event the host signals for Ctrl+Alt+Del. Created by SYSTEM, so only SYSTEM and
/// administrators may signal it (the default DACL of a SYSTEM-created object).
struct SasEvent(Handle);

impl SasEvent {
    fn create(profile: &Profile) -> Result<Self> {
        let name = wide(&sas_event_name(profile));
        // SAFETY: a NUL-terminated name; the handle is owned.
        let h = unsafe { CreateEventW(None, false, false, PCWSTR(name.as_ptr())) }.context("create the event")?;
        Ok(Self(Handle(h)))
    }

    /// Waits up to `timeout` for a request; true when one arrived.
    fn wait(&self, timeout: Duration) -> bool {
        // SAFETY: a valid event handle.
        unsafe { WaitForSingleObject(self.0.0, timeout.as_millis() as u32) == WAIT_OBJECT_0 }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_per_profile_and_plain() {
        let dir = std::env::temp_dir().join(format!("ar-service-name-{}", std::process::id()));
        let profile = Profile::at(&dir).unwrap();
        let name = service_name(&profile);
        assert!(name.starts_with("ai-remote-host-") && name.len() == "ai-remote-host-".len() + 8, "{name}");
        assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
        assert_eq!(sas_event_name(&profile), format!(r"Global\ai-remote-sas-{}", &name["ai-remote-host-".len()..]));
        // No service runs for a scratch profile: nothing to signal.
        assert!(!secure_attention_available(&profile));
        assert!(request_secure_attention(&sas_event_name(&profile)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn running_outside_the_scm_fails_cleanly() {
        let dir = std::env::temp_dir().join(format!("ar-service-run-{}", std::process::id()));
        let profile = Profile::at(&dir).unwrap();
        let args = ServiceArgs { profile, owner_sid: "S-1-5-18".into(), hostw: PathBuf::from("x.exe") };
        // StartServiceCtrlDispatcherW refuses a process the SCM did not start.
        let err = run(args).unwrap_err().to_string();
        assert!(err.contains("service control manager"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
