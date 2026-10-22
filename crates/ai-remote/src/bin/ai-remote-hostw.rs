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
//! `--owner-uid` (macOS, the system LaunchAgent `ai-remote host install --service` registers)
//! runs in the login window's session and in the user's: with fast user switching both run at
//! once, and the one whose session has the screen holds the profile (see `ar_host::handoff`). The
//! other waits instead of exiting, and the holder steps aside, exiting, when its session leaves
//! the screen.
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
    sync::{Arc, Mutex},
};

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
    // The host asks a fresh copy of itself what macOS grants it (see `ar_host::platform::tcc`).
    #[cfg(target_os = "macos")]
    if std::env::var_os(ar_host::platform::tcc::PROBE_ENV).is_some() {
        println!("{}", serde_json::to_string(&ar_host::platform::tcc::grants()).unwrap_or_default());
        return ExitCode::SUCCESS;
    }
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
    // The macOS system agent becomes its owner before it touches the profile: the profile, the log
    // included, is the user's, never created by root. launchd also starts it in other users'
    // sessions, as them; they cannot (and must not) open the owner's profile, so they leave at once,
    // with 0 so that launchd does not start them again.
    #[cfg(target_os = "macos")]
    if let Some(uid) = args.owner_uid {
        if let Err(e) = ar_host::platform::macos::adopt_owner(uid) {
            last_resort(&format!("cannot become the installing user (uid {uid}): {e:#}"));
            return ExitCode::FAILURE;
        }
        if !ar_host::platform::macos::login_window_instance() && !ar_host::platform::macos::runs_as(uid) {
            return ExitCode::SUCCESS;
        }
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
    let log = LogSink::init(&log_path);
    #[cfg(windows)]
    if !args.supervised {
        return supervise(&profile);
    }
    #[cfg(target_os = "macos")]
    if let Some(uid) = args.owner_uid {
        return system_agent::run(profile, &log_path, &log, uid);
    }
    #[cfg(windows)]
    let shared_with: Vec<String> = args.owner_sid.into_iter().collect();
    #[cfg(not(windows))]
    let shared_with = Vec::new();
    run_host(profile, &log_path, &log, &shared_with)
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
    // Only the runner writes its log, so it rotates it.
    LogSink::init(&log).rotate(&log);
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
    let Some(runtime) = runtime() else { return ExitCode::FAILURE };
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

fn runtime() -> Option<tokio::runtime::Runtime> {
    match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => Some(rt),
        Err(e) => {
            tracing::error!("cannot start the async runtime: {e}");
            last_resort(&format!("cannot start the async runtime: {e}"));
            None
        }
    }
}

/// Runs the host in this process until it is stopped. `shared_with`: accounts besides this
/// process's own that may use the control endpoint (see `ar_host::Claim::acquire_shared`).
fn run_host(profile: Profile, log_path: &Path, log: &LogSink, shared_with: &[String]) -> ExitCode {
    let Some(runtime) = runtime() else { return ExitCode::FAILURE };
    // Claim the profile first: a second start (a double-click, a script) must leave the running
    // host alone, its log included, so it only appends a line saying why it quits.
    let claim = {
        let _in_runtime = runtime.enter();
        ar_host::Claim::acquire_shared(&profile, shared_with)
    };
    let claim = match claim {
        Ok(c) => c,
        Err(e) => {
            if e.downcast_ref::<ar_host::AlreadyRunning>().is_some() {
                tracing::info!("not starting: {e:#}");
                return ExitCode::from(EXIT_ALREADY_RUNNING);
            }
            tracing::error!("host stopped: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    log.rotate(log_path);
    tracing::info!(log = %log_path.display(), pid = std::process::id(), "starting the background host");
    // The owner's SID: agents of a host running as SYSTEM run as that user.
    let owner_sid = shared_with.first().cloned();
    let opts = ar_host::HostOptions { pair_code_if_unpaired: false, background: true, step_aside: None, owner_sid };
    match runtime.block_on(ar_host::run_claimed(profile, claim, opts)) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("host stopped: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The macOS system agent (`--owner-uid`): waits until its session has the console and the
/// profile is free, serves, and steps aside by exiting when its session leaves the console. See
/// `ar_host::handoff` for the rule; this is the process side of it.
#[cfg(target_os = "macos")]
mod system_agent {
    use std::{
        path::Path,
        process::ExitCode,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use ar_core::Profile;
    use ar_host::{
        CancellationToken, Claim, StopReason,
        handoff::{self, ClaimState, Decision, Situation, WaitLog},
        platform::macos,
    };

    use super::{LogSink, runtime};

    pub fn run(profile: Profile, log_path: &Path, log: &LogSink, owner_uid: u32) -> ExitCode {
        let Some(runtime) = runtime() else { return ExitCode::FAILURE };
        let login_window = macos::login_window_instance();
        // Another user's Aqua session: launchd starts the agent there too, as that user. (The
        // LoginWindow instance is the owner's even when AI_REMOTE_KEEP_ROOT keeps it root.)
        let foreign_user = !login_window && !macos::runs_as(owner_uid);
        let signals = Signals::install(&runtime);
        let marker_path = match handoff::marker_path(&profile) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("host stopped: {e:#}");
                return ExitCode::FAILURE;
            }
        };
        let boot = macos::boot_session();
        let asid = macos::audit_session_id();
        let stopped = || {
            let (marker, corrupt) = match handoff::read_marker(&marker_path) {
                Ok(m) => (m, false),
                Err(e) => {
                    tracing::warn!("ignoring the stop marker: {e:#}");
                    (None, true)
                }
            };
            if handoff::stale(marker.as_ref(), corrupt, boot.as_deref()) {
                // From an earlier boot, or unreadable: it means nothing to anyone. A marker of this
                // boot this instance does not honour stays: it may be meant for the other instance.
                let _ = handoff::clear_marker(&marker_path);
                return false;
            }
            handoff::honored(marker.as_ref(), boot.as_deref(), asid, login_window)
        };
        let raw = Arc::new(RawConsoleLog::default());
        let mut mesh_busy_since: Option<std::time::Instant> = None;
        let mut mesh_busy_logged = false;
        let mut wait_log = WaitLog::default();
        loop {
            // Wait for this instance's turn.
            let claim = loop {
                if signals.terminated() {
                    tracing::info!("terminated (SIGTERM) while waiting");
                    return ExitCode::from(handoff::EXIT_TERMINATED);
                }
                let look = |claim| Situation {
                    foreign_user,
                    stopped: !foreign_user && stopped(),
                    console: raw.read(login_window),
                    login_window,
                    claim,
                };
                let mut held: Option<Claim> = None;
                let mut decision = handoff::decide(&look(ClaimState::NotTried));
                if decision == Decision::TryClaim {
                    let attempt = {
                        let _in_runtime = runtime.enter();
                        Claim::acquire(&profile)
                    };
                    decision = match attempt {
                        // Look again with the slot held: a stop recorded just before the holder let
                        // go, or a switch in the meantime, still counts.
                        Ok(c) => {
                            held = Some(c);
                            handoff::decide(&look(ClaimState::Held))
                        }
                        Err(e) if e.downcast_ref::<ar_host::AlreadyRunning>().is_some() => {
                            handoff::decide(&look(ClaimState::Taken))
                        }
                        Err(e) => {
                            tracing::error!("host stopped: {e:#}");
                            return ExitCode::FAILURE;
                        }
                    };
                }
                if let Some(line) = wait_log.note(decision) {
                    tracing::info!(pid = std::process::id(), login_window, "{line}");
                }
                match decision {
                    Decision::NotOurs | Decision::Stopped => return ExitCode::SUCCESS,
                    Decision::Wait(w) => {
                        drop(held);
                        // A real wait ends any busy-transport episode: the next one counts afresh.
                        mesh_busy_since = None;
                        mesh_busy_logged = false;
                        std::thread::sleep(w.interval());
                    }
                    Decision::Serve => break held.expect("serve only with the slot held"),
                    Decision::TryClaim => unreachable!("decided with a claim outcome"),
                }
            };
            log.rotate(log_path);
            tracing::info!(log = %log_path.display(), pid = std::process::id(), login_window, "starting the background host");
            match serve(&runtime, &profile, claim, login_window, &signals, &raw) {
                Served::Stopped => return ExitCode::SUCCESS,
                Served::Leave { claim, code } => {
                    if code == handoff::EXIT_STEPPED_ASIDE {
                        tracing::info!(
                            "stepping aside: this session left the console; the instance that has it takes over"
                        );
                    } else {
                        tracing::info!("terminated (SIGTERM)");
                    }
                    // Exit with the slot still held: the kernel frees it together with the transport's
                    // database lock and socket, so the next holder never finds the transport busy.
                    let _ = std::io::Write::flush(&mut log.clone());
                    let _hold = claim;
                    std::process::exit(code.into());
                }
                Served::MeshBusy(e) => {
                    log.append(log_path);
                    if signals.terminated() {
                        tracing::info!("terminated (SIGTERM)");
                        return ExitCode::from(handoff::EXIT_TERMINATED);
                    }
                    // Usually the previous holder still on its way out: give the slot back and retry
                    // for a while. The transport reports any failure to open its database this way,
                    // so past that it is a failure (exit 1; launchd starts the agent again, throttled).
                    let since = *mesh_busy_since.get_or_insert_with(std::time::Instant::now);
                    if since.elapsed() >= MESH_BUSY_LIMIT {
                        tracing::error!("host stopped: {e:#}");
                        return ExitCode::FAILURE;
                    }
                    if !mesh_busy_logged {
                        tracing::info!("waiting: {e:#}");
                        mesh_busy_logged = true;
                    }
                    std::thread::sleep(handoff::Wait::Taken.interval());
                    continue;
                }
                Served::Failed => return ExitCode::FAILURE,
            }
        }
    }

    /// How long a busy transport database is retried before it counts as a failure: a predecessor
    /// that is exiting frees it within a second or two.
    const MESH_BUSY_LIMIT: std::time::Duration = std::time::Duration::from_secs(15);

    /// Reads the console and logs the raw read whenever it changes, so the log shows what each
    /// decision rested on (the LoginWindow instance's read after its identity switch is not verified).
    #[derive(Default)]
    struct RawConsoleLog(Mutex<Option<handoff::ConsoleRead>>);

    impl RawConsoleLog {
        fn read(&self, login_window: bool) -> handoff::ConsoleRead {
            let now = handoff::read_console();
            let mut last = self.0.lock().unwrap_or_else(|p| p.into_inner());
            if *last != Some(now) {
                tracing::info!(
                    login_window,
                    session_on_console = ?now.session,
                    console_device_owner = ?now.device_owner,
                    counts_as_on_console = handoff::on_console(now, login_window),
                    "console read"
                );
                *last = Some(now);
            }
            now
        }
    }

    enum Served {
        /// Stopped on request (`stop-host`, Ctrl+C): exit 0.
        Stopped,
        /// Exit with `code` while still holding `claim`.
        Leave {
            claim: Claim,
            code: u8,
        },
        /// The transport's database could not be opened (usually: the previous holder is still
        /// exiting).
        MeshBusy(anyhow::Error),
        Failed,
    }

    /// SIGTERM: launchd ends the LoginWindow instance with it when the user logs in, and either one
    /// on `launchctl bootout` / `kickstart -k`. While serving, the host first closes the
    /// controllers' connections; while waiting there is nothing to close, so the process exits.
    struct Signals {
        terminated: Arc<AtomicBool>,
        serving: Arc<Mutex<Option<CancellationToken>>>,
    }

    impl Signals {
        fn install(runtime: &tokio::runtime::Runtime) -> Self {
            let terminated = Arc::new(AtomicBool::new(false));
            let serving = Arc::new(Mutex::new(None::<CancellationToken>));
            let _in_runtime = runtime.enter();
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut term) => {
                    let (terminated, serving) = (terminated.clone(), serving.clone());
                    runtime.spawn(async move {
                        // Every SIGTERM is handled: once one is registered the default action never
                        // comes back. A second one while the host is still closing exits at once.
                        while term.recv().await.is_some() {
                            terminated.store(true, Ordering::SeqCst);
                            let host = serving.lock().unwrap_or_else(|p| p.into_inner()).take();
                            match host {
                                Some(leave) => leave.cancel(),
                                None => {
                                    tracing::info!("terminated (SIGTERM)");
                                    std::process::exit(handoff::EXIT_TERMINATED.into());
                                }
                            }
                        }
                    });
                }
                Err(e) => tracing::warn!("cannot handle SIGTERM: {e}"),
            }
            Self { terminated, serving }
        }

        fn terminated(&self) -> bool {
            self.terminated.load(Ordering::SeqCst)
        }

        fn serving(&self, leave: Option<CancellationToken>) {
            *self.serving.lock().unwrap_or_else(|p| p.into_inner()) = leave;
        }
    }

    fn serve(
        runtime: &tokio::runtime::Runtime,
        profile: &Profile,
        claim: Claim,
        login_window: bool,
        signals: &Signals,
        raw: &Arc<RawConsoleLog>,
    ) -> Served {
        let leave = CancellationToken::new();
        signals.serving(Some(leave.clone()));
        let watcher = runtime.spawn({
            let leave = leave.clone();
            let raw = raw.clone();
            async move {
                handoff::watch_console(login_window, move || raw.read(login_window)).await;
                leave.cancel();
            }
        });
        let opts = ar_host::HostOptions {
            pair_code_if_unpaired: false,
            background: true,
            step_aside: Some(leave),
            owner_sid: None,
        };
        let result = runtime.block_on(ar_host::run_claimed(profile.clone(), claim, opts));
        signals.serving(None);
        watcher.abort();
        match result {
            Ok(stopped) => match stopped.reason {
                StopReason::Requested => Served::Stopped,
                StopReason::SteppedAside => {
                    let code =
                        if signals.terminated() { handoff::EXIT_TERMINATED } else { handoff::EXIT_STEPPED_ASIDE };
                    Served::Leave { claim: stopped.claim, code }
                }
            },
            Err(e) if e.downcast_ref::<ar_net::MeshBusy>().is_some() => Served::MeshBusy(e),
            Err(e) => {
                tracing::error!("host stopped: {e:#}");
                Served::Failed
            }
        }
    }
}

/// The Windows service: logs like the supervisor (it shares the file with the host it starts) and
/// hands over to `ar_host::service`, which returns once the service control manager stopped it.
#[cfg(windows)]
fn run_service(profile: Profile, log_path: &Path, owner_sid: Option<String>) -> ExitCode {
    LogSink::init(log_path);
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
fn supervise(profile: &Profile) -> ExitCode {
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

/// The process's log, set up once (a global tracing subscriber cannot be replaced): it appends
/// each line and closes the file again until this process holds the profile, then writes through
/// the rotating writer. The supervisor, the Windows service and a start that finds a host running
/// share the file with the host that owns it, so they never rotate it; the system agent's waiting
/// instance goes back to appending when it gives the profile up.
#[derive(Clone)]
struct LogSink(Arc<Mutex<Sink>>);

enum Sink {
    Appending(PathBuf),
    /// Rotated by size between lines (an existing crate does the rotation), keeping
    /// [`LOG_KEEP`] earlier files.
    Rotating(FileRotate<AppendCount>),
}

impl LogSink {
    fn appending(path: &Path) -> Self {
        Self(Arc::new(Mutex::new(Sink::Appending(path.to_path_buf()))))
    }

    /// The global subscriber, appending to `path`.
    fn init(path: &Path) -> Self {
        let sink = Self::appending(path);
        tracing_subscriber::fmt().with_env_filter(log_filter()).with_ansi(false).with_writer(sink.clone()).init();
        sink
    }

    /// Writes through the rotating writer: only for the process that holds the profile.
    fn rotate(&self, path: &Path) {
        let file = FileRotate::new(
            path,
            AppendCount::new(LOG_KEEP),
            ContentLimit::BytesSurpassed(LOG_ROTATE_BYTES),
            Compression::None,
            None,
        );
        *self.lock() = Sink::Rotating(file);
    }

    /// Back to appending (after giving the profile up).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn append(&self, path: &Path) {
        *self.lock() = Sink::Appending(path.to_path_buf());
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Sink> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Write for LogSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match &mut *self.lock() {
            Sink::Appending(path) => {
                // A log that cannot be opened loses the line rather than the host.
                if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                    let _ = f.write_all(buf);
                }
                Ok(buf.len())
            }
            Sink::Rotating(file) => file.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match &mut *self.lock() {
            Sink::Appending(_) => Ok(()),
            Sink::Rotating(file) => file.flush(),
        }
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogSink {
    type Writer = LogSink;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Where errors go before the log exists.
fn last_resort(message: &str) {
    let path = std::env::temp_dir().join("ai-remote-hostw-error.log");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {message}", ar_core::unix_now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_subscriber_writes_through_appending_rotating_and_appending_again() {
        let dir = std::env::temp_dir().join(format!("ai-remote-hostw-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("host.log");
        let sink = LogSink::appending(&path);
        let subscriber = tracing_subscriber::fmt().with_ansi(false).with_writer(sink.clone()).finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("first, appending");
            sink.rotate(&path);
            tracing::info!("second, rotating");
            sink.append(&path);
            tracing::info!("third, appending again");
        });
        let text = std::fs::read_to_string(&path).unwrap();
        let order: Vec<usize> = ["first, appending", "second, rotating", "third, appending again"]
            .iter()
            .map(|l| text.find(l).unwrap_or_else(|| panic!("missing {l:?} in {text:?}")))
            .collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
