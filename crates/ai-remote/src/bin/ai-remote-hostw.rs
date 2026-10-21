//! `ai-remote-hostw`: the host without a console window, for starting at login. It is what the
//! entry registered by `ai-remote host install` runs. Same host as `ai-remote host`; everything
//! the foreground host prints goes to the host log (`<profile>/host/host.log`) instead, and it
//! never mints a pairing ticket by itself (a ticket is a credential, and a log is no place for it).
//!
//! On Windows the process started at login is a small supervisor that runs the host as a child
//! and starts it again when it crashes: Task Scheduler's "restart on failure" only covers a task
//! that fails to *launch*, never a program that exits with an error. A clean stop
//! (`ai-remote stop-host`) ends both, as macOS's LaunchAgent does (restart after a crash, not after
//! a clean exit). The child sits in a kill-on-close job object, so ending the task (or the
//! supervisor dying) takes the host and everything it started down with it.
//!
//! `--service` (Windows, registered by `ai-remote host install --service`) is the same supervisor
//! as a service: it runs the host as SYSTEM inside the console session (see `ar_host::service`),
//! which is what reaches the lock screen and UAC prompts.
//!
//! `--chat-runner` is started by such a SYSTEM host in the installing user's session, with the
//! user's token: it runs that host's coding agents as the user (see `ar_host::chat::runner`).

// No console window on Windows; the attribute does nothing elsewhere.
#![windows_subsystem = "windows"]

use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Mutex,
};

use anyhow::Result;
use ar_core::{Profile, Role};
use ar_host::supervise::EXIT_ALREADY_RUNNING;
use clap::Parser;
use file_rotate::{ContentLimit, FileRotate, compression::Compression, suffix::AppendCount};

#[derive(Parser)]
#[command(
    name = "ai-remote-hostw",
    version,
    about = "Runs the ai-remote host in the background, without a console window. Logs to the host log."
)]
struct Args {
    /// Profile directory (default: the platform's local data directory; env AI_REMOTE_HOME).
    #[arg(long, value_name = "DIR")]
    home: Option<PathBuf>,
    /// Run the host itself rather than supervising it (set by the supervisor for its child;
    /// elsewhere than Windows the service manager supervises).
    #[arg(long, hide = true)]
    #[cfg_attr(not(windows), allow(dead_code))]
    supervised: bool,
    /// Run as the Windows service registered by `ai-remote host install --service`: keeps the
    /// host running as SYSTEM in the console session. Only the service control manager starts
    /// this.
    #[arg(long, hide = true, conflicts_with = "supervised")]
    #[cfg_attr(not(windows), allow(dead_code))]
    service: bool,
    /// SID of the account whose CLI may talk to this host besides the account it runs as (the
    /// service passes it to the host it starts as SYSTEM).
    #[arg(long, hide = true, value_name = "SID")]
    #[cfg_attr(not(windows), allow(dead_code))]
    owner_sid: Option<String>,
    /// macOS, set by the system LaunchAgent `ai-remote host install --service` registers: the
    /// installing user's uid. The host serves the login window too, and becomes that user when
    /// launchd started it as root there (see `ar_host::platform::macos::adopt_owner`).
    #[arg(long, hide = true, value_name = "UID")]
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    owner_uid: Option<u32>,
    /// Run the coding agents of a host that runs as SYSTEM, as the user this process runs as.
    /// Started by that host (with its key in the environment), never by hand.
    #[arg(long, hide = true, conflicts_with_all = ["service", "supervised"])]
    chat_runner: bool,
}

/// The log is rotated once it passes this size, between two lines.
const LOG_ROTATE_BYTES: usize = 8 * 1024 * 1024;
/// Rotated logs kept (`host.log.1`, `host.log.2`).
const LOG_KEEP: usize = 2;

fn main() -> ExitCode {
    // Nobody sees stderr here: every early failure goes to a file.
    let args = match Args::try_parse() {
        Ok(a) => a,
        Err(e) => {
            // --help and --version still reach a console or pipe the caller attached.
            let _ = e.print();
            if e.use_stderr() {
                last_resort(&e.to_string());
            }
            return ExitCode::from(e.exit_code().clamp(0, 255) as u8);
        }
    };
    if let Some(home) = &args.home {
        // SAFETY: set before any thread is started.
        unsafe { std::env::set_var(ar_core::profile::HOME_ENV, home) };
    }
    let (profile, log_path) = match Profile::open_default().and_then(|p| Ok((p.log_path(Role::Host)?, p))) {
        Ok((log, profile)) => (profile, log),
        Err(e) => {
            last_resort(&format!("{e:#}"));
            return ExitCode::FAILURE;
        }
    };
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
    }));
    if args.chat_runner {
        return run_chat_runner(&profile);
    }
    #[cfg(windows)]
    if args.service {
        return run_service(profile, &log_path, args.owner_sid);
    }
    #[cfg(target_os = "macos")]
    if let Some(uid) = args.owner_uid
        && let Err(e) = ar_host::platform::macos::adopt_owner(uid)
    {
        last_resort(&format!("cannot become the installing user (uid {uid}): {e:#}"));
        return ExitCode::FAILURE;
    }
    #[cfg(windows)]
    if !args.supervised {
        return supervise(&profile, &log_path);
    }
    #[cfg(windows)]
    let shared_with: Vec<String> = args.owner_sid.into_iter().collect();
    #[cfg(not(windows))]
    let shared_with = Vec::new();
    run_host(profile, &log_path, &shared_with)
}

/// The coding-agent runner: reads its key from the environment (and removes it there, before any
/// agent could inherit it), logs to its own file and serves the host until it is told to stop or
/// no host has come for a while.
fn run_chat_runner(profile: &Profile) -> ExitCode {
    let key = std::env::var(ar_host::chat::runner::KEY_ENV).ok();
    // SAFETY: no other thread runs yet.
    unsafe { std::env::remove_var(ar_host::chat::runner::KEY_ENV) };
    let log = match profile.chat_runner_log_path() {
        Ok(p) => p,
        Err(e) => {
            last_resort(&format!("{e:#}"));
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = log_rotating(&log) {
        last_resort(&format!("{e:#}"));
        return ExitCode::FAILURE;
    }
    let Some(key) = key.and_then(|k| ar_host::chat::runner::parse_key(&k).ok()) else {
        tracing::error!("the agent runner was started without its key; only the host starts it");
        return ExitCode::FAILURE;
    };
    let endpoint = match profile.chat_runner_endpoint() {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("{e:#}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(pid = std::process::id(), endpoint = %endpoint.display(), "agent runner started");
    use ar_host::chat::runner;
    match runtime.block_on(runner::run(endpoint, key, runner::IDLE_EMPTY, runner::IDLE_BUSY)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("agent runner stopped: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Runs the host in this process until it is stopped. `shared_with`: accounts besides this
/// process's own that may use the control endpoint (see `ar_host::Claim::acquire_shared`).
fn run_host(profile: Profile, log_path: &Path, shared_with: &[String]) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            last_resort(&format!("cannot start the async runtime: {e}"));
            return ExitCode::FAILURE;
        }
    };
    // Claim the profile first: a second start (a double-click, a script) must leave the running
    // host alone, its log included, so it only appends a line saying why it quits.
    let claim = {
        let _in_runtime = runtime.enter();
        ar_host::Claim::acquire_shared(&profile, shared_with)
    };
    let claim = match claim {
        Ok(c) => c,
        Err(e) => {
            log_appending(log_path);
            if e.downcast_ref::<ar_host::AlreadyRunning>().is_some() {
                tracing::info!("not starting: {e:#}");
                return ExitCode::from(EXIT_ALREADY_RUNNING);
            }
            tracing::error!("host stopped: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = log_rotating(log_path) {
        last_resort(&format!("{e:#}"));
        return ExitCode::FAILURE;
    }
    tracing::info!(log = %log_path.display(), pid = std::process::id(), "starting the background host");
    // The owner's SID: agents of a host running as SYSTEM run as that user.
    let owner_sid = shared_with.first().cloned();
    let opts = ar_host::HostOptions { pair_code_if_unpaired: false, background: true, owner_sid };
    match runtime.block_on(ar_host::run_claimed(profile, claim, opts)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("host stopped: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The Windows service: logs like the supervisor (it shares the file with the host it starts) and
/// hands over to `ar_host::service`, which returns once the service control manager stopped it.
#[cfg(windows)]
fn run_service(profile: Profile, log_path: &Path, owner_sid: Option<String>) -> ExitCode {
    log_appending(log_path);
    let Some(owner_sid) = owner_sid else {
        tracing::error!("--service needs --owner-sid <SID> (ai-remote host install --service registers it that way)");
        return ExitCode::FAILURE;
    };
    let hostw = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            tracing::error!("cannot locate this program to start the host: {e}");
            return ExitCode::FAILURE;
        }
    };
    match ar_host::service::run(ar_host::service::ServiceArgs { profile, owner_sid, hostw }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("service stopped: {e:#}");
            last_resort(&format!("{e:#}"));
            ExitCode::FAILURE
        }
    }
}

/// Runs the host as a child and restarts it after an abnormal exit; a clean stop ends both.
#[cfg(windows)]
fn supervise(profile: &Profile, log_path: &Path) -> ExitCode {
    log_appending(log_path);
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            tracing::error!("cannot locate this program to start the host: {e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async {
        let mut backoff = ar_host::supervise::Backoff::default();
        loop {
            let mut cmd = tokio::process::Command::new(&exe);
            cmd.arg("--home").arg(profile.root()).arg("--supervised").kill_on_drop(true);
            let started = std::time::Instant::now();
            let status = match spawn_in_job(&mut cmd) {
                // Wait for the host process itself; dropping the job then kills anything it left in
                // it (a crash leaves its children behind). Background jobs broke away and survive.
                Ok((mut child, _job)) => child.wait().await,
                Err(e) => Err(e),
            };
            let delay = backoff.after(started.elapsed());
            match status {
                Ok(s) if s.success() => return ExitCode::SUCCESS,
                Ok(s) if s.code() == Some(EXIT_ALREADY_RUNNING.into()) => return ExitCode::from(EXIT_ALREADY_RUNNING),
                Ok(s) => tracing::error!("the host exited unexpectedly ({s}); restarting it in {} s", delay.as_secs()),
                Err(e) => tracing::error!("cannot start the host ({e}); trying again in {} s", delay.as_secs()),
            }
            tokio::time::sleep(delay).await;
        }
    })
}

/// Starts the host in its own job object: kill-on-close, so the host and whatever it started die
/// with this supervisor (the task is ended, or it dies), and breakaway allowed, so background jobs
/// the host starts with `CREATE_BREAKAWAY_FROM_JOB` outlive a host restart as documented. The
/// child is windowless too, so it needs no console flags.
#[cfg(windows)]
fn spawn_in_job(cmd: &mut tokio::process::Command) -> std::io::Result<(tokio::process::Child, win32job::Job)> {
    let mut limits = win32job::ExtendedLimitInfo::new();
    limits.limit_kill_on_job_close().limit_breakaway_ok();
    let job = win32job::Job::create_with_limit_info(&limits).map_err(std::io::Error::other)?;
    let child = cmd.spawn()?;
    let handle = child.raw_handle().ok_or_else(|| std::io::Error::other("the host exited at once"))?;
    // The host spawns nothing before it has started its transport, so nothing escapes the job
    // between spawn and assignment.
    job.assign_process(handle as isize).map_err(std::io::Error::other)?;
    Ok((child, job))
}

fn log_filter() -> tracing_subscriber::EnvFilter {
    tracing_subscriber::EnvFilter::try_from_env("AI_REMOTE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(ar_host::DEFAULT_LOG_FILTER))
}

/// The host's own log: rotated by size between lines (an existing crate does the rotation),
/// keeping [`LOG_KEEP`] earlier files. Only the process that holds the profile writes this way.
fn log_rotating(path: &Path) -> Result<()> {
    let file = FileRotate::new(
        path,
        AppendCount::new(LOG_KEEP),
        ContentLimit::BytesSurpassed(LOG_ROTATE_BYTES),
        Compression::None,
        None,
    );
    tracing_subscriber::fmt().with_env_filter(log_filter()).with_ansi(false).with_writer(Mutex::new(file)).init();
    Ok(())
}

/// Appends each line to the log and closes it again, never rotating: for the supervisor and a
/// start that finds a host running, which share the file with the host that owns it.
fn log_appending(path: &Path) {
    let path = path.to_path_buf();
    let writer = move || -> Box<dyn Write> {
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => Box::new(f),
            Err(_) => Box::new(std::io::sink()),
        }
    };
    tracing_subscriber::fmt().with_env_filter(log_filter()).with_ansi(false).with_writer(writer).init();
}

/// Where errors go before the log exists.
fn last_resort(message: &str) {
    let path = std::env::temp_dir().join("ai-remote-hostw-error.log");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {message}", ar_core::unix_now());
    }
}
