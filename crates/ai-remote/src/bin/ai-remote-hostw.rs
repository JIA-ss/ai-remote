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

// No console window on Windows; the attribute does nothing elsewhere.
#![windows_subsystem = "windows"]

use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Mutex,
    time::Duration,
};

use anyhow::Result;
use ar_core::{Profile, Role};
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
}

/// The log is rotated once it passes this size, between two lines.
const LOG_ROTATE_BYTES: usize = 8 * 1024 * 1024;
/// Rotated logs kept (`host.log.1`, `host.log.2`).
const LOG_KEEP: usize = 2;
/// Exit code of a host that found another one running for its profile: nothing to restart.
const EXIT_ALREADY_RUNNING: u8 = 3;

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
    #[cfg(windows)]
    if !args.supervised {
        return supervise(&profile, &log_path);
    }
    run_host(profile, &log_path)
}

/// Runs the host in this process until it is stopped.
fn run_host(profile: Profile, log_path: &Path) -> ExitCode {
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
        ar_host::Claim::acquire(&profile)
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
    let opts = ar_host::HostOptions { pair_code_if_unpaired: false, background: true };
    match runtime.block_on(ar_host::run_claimed(profile, claim, opts)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("host stopped: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Runs the host as a child and restarts it after an abnormal exit; a clean stop ends both.
#[cfg(windows)]
fn supervise(profile: &Profile, log_path: &Path) -> ExitCode {
    use process_wrap::tokio::{CommandWrap, JobObject, KillOnDrop};

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
        let mut backoff = Backoff::default();
        loop {
            let mut cmd = tokio::process::Command::new(&exe);
            cmd.arg("--home").arg(profile.root()).arg("--supervised");
            // The child is a windowless program too, so it needs no console flags. Kill-on-close
            // job: if this supervisor ends (the task is ended, or it dies), so does the host.
            let mut wrap = CommandWrap::from(cmd);
            wrap.wrap(KillOnDrop).wrap(JobObject);
            let started = std::time::Instant::now();
            let status = match wrap.spawn() {
                // Wait for the host process itself; dropping `child` then kills anything it left
                // in the job (a crash leaves its children behind).
                Ok(mut child) => child.inner_mut().wait().await,
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

/// How long to wait before restarting a host that exited abnormally: 5 s, doubling up to five
/// minutes, so a host that fails at once (a broken config) does not spin. One that ran for ten
/// minutes or more counts as healthy and starts the sequence over.
#[cfg_attr(not(windows), allow(dead_code))]
struct Backoff {
    next: Duration,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Backoff {
    const FIRST: Duration = Duration::from_secs(5);
    const MAX: Duration = Duration::from_secs(300);
    const HEALTHY: Duration = Duration::from_secs(600);

    /// The delay before the next start, given how long the last run lasted.
    fn after(&mut self, ran: Duration) -> Duration {
        if ran >= Self::HEALTHY {
            self.next = Self::FIRST;
        }
        let delay = self.next;
        self.next = (self.next * 2).min(Self::MAX);
        delay
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self { next: Self::FIRST }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_back_off_and_recover() {
        let mut b = Backoff::default();
        let quick = Duration::from_secs(1);
        let delays: Vec<u64> = (0..8).map(|_| b.after(quick).as_secs()).collect();
        assert_eq!(delays, vec![5, 10, 20, 40, 80, 160, 300, 300]);
        // A run that lasted counts as healthy: back to the first delay.
        assert_eq!(b.after(Duration::from_secs(3600)).as_secs(), 5);
        assert_eq!(b.after(quick).as_secs(), 10);
    }
}
