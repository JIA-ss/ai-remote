//! `ai-remote-hostw`: the host without a console window, for starting at login. It is what the
//! entry registered by `ai-remote host install` runs. Same host as `ai-remote host`; everything
//! the foreground host prints goes to the host log (`<profile>/host/host.log`) instead, and it
//! never mints a pairing ticket by itself (a ticket is a credential, and a log is no place for it).

// No console window on Windows; the attribute does nothing elsewhere.
#![windows_subsystem = "windows"]

use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Mutex,
};

use anyhow::{Context, Result};
use ar_core::{Profile, Role};
use clap::Parser;

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
}

/// One earlier log is kept when the current one grows past this at start.
const LOG_ROTATE_BYTES: u64 = 8 * 1024 * 1024;

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
    let profile = match Profile::open_default() {
        Ok(p) => p,
        Err(e) => {
            last_resort(&format!("{e:#}"));
            return ExitCode::FAILURE;
        }
    };
    let log_path = match init_logging(&profile) {
        Ok(p) => p,
        Err(e) => {
            last_resort(&format!("{e:#}"));
            return ExitCode::FAILURE;
        }
    };
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
    }));
    tracing::info!(log = %log_path.display(), pid = std::process::id(), "starting the background host");

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let opts = ar_host::HostOptions { pair_code_if_unpaired: false, background: true };
    match runtime.block_on(ar_host::run(profile, opts)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("host stopped: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Logs to `<profile>/host/host.log`, keeping one rotated copy.
fn init_logging(profile: &Profile) -> Result<PathBuf> {
    let path = profile.log_path(Role::Host)?;
    rotate(&path);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    let filter = tracing_subscriber::EnvFilter::try_from_env("AI_REMOTE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(ar_host::DEFAULT_LOG_FILTER));
    tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(Mutex::new(file)).init();
    Ok(path)
}

fn rotate(path: &Path) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > LOG_ROTATE_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
}

/// Where errors go before the log exists.
fn last_resort(message: &str) {
    let path = std::env::temp_dir().join("ai-remote-hostw-error.log");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {message}", ar_core::unix_now());
    }
}
