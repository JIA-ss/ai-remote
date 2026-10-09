//! `ai-remote mcp`: the process an MCP client starts. It relays stdio to the controller agent
//! (starting the agent if needed) and exits when the client closes stdin.

use std::{process::Stdio, time::Duration};

use anyhow::{Context, Result, anyhow};
use ar_core::{Profile, Role};
use ar_net::ipc::Conn;
use serde_json::json;
use tokio::io::{AsyncWriteExt, copy};

/// Connects to the profile's agent, starting it in the background when it is not running.
pub async fn connect_agent(profile: &Profile) -> Result<Conn> {
    let endpoint = profile.control_endpoint(Role::Controller)?;
    if let Ok(conn) = Conn::connect(&endpoint).await {
        return Ok(conn);
    }
    spawn_agent(profile)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        match Conn::connect(&endpoint).await {
            Ok(conn) => return Ok(conn),
            Err(e) if tokio::time::Instant::now() >= deadline => {
                let log = profile.log_path(Role::Controller)?;
                return Err(anyhow!(
                    "the controller agent did not start ({e}); see {}",
                    log.display()
                ));
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(150)).await,
        }
    }
}

fn spawn_agent(profile: &Profile) -> Result<()> {
    let exe = std::env::current_exe().context("locate the ai-remote executable")?;
    let log_path = profile.log_path(Role::Controller)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("agent")
        .env(ar_core::profile::HOME_ENV, profile.root())
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    detach(&mut cmd);
    cmd.spawn().context("start the controller agent")?;
    Ok(())
}

#[cfg(windows)]
fn detach(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    // The agent outlives this process, and a Windows child inherits every inheritable handle. If a
    // caller reads our stdout through a pipe (a script, or an MCP client that started `ai-remote
    // mcp`), the agent would keep that pipe open and the caller would never see end-of-file. The
    // agent gets its log file and NUL explicitly, so stop our own standard handles being inherited.
    use windows::Win32::{
        Foundation::{HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation},
        System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
    };
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: queries and flag changes on this process's own standard handles.
        unsafe {
            if let Ok(handle) = GetStdHandle(which)
                && !handle.is_invalid()
            {
                let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
            }
        }
    }
}

#[cfg(unix)]
fn detach(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

/// Relays stdin/stdout to an aggregated MCP session in the agent.
pub async fn run_shim(profile: &Profile) -> Result<()> {
    let mut conn = connect_agent(profile).await?;
    conn.request(&json!({"op": "mcp"})).await?;
    let (stream, buffered) = conn.into_raw();
    let (mut from_agent, mut to_agent) = tokio::io::split(stream);
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let upstream = async {
        let _ = copy(&mut stdin, &mut to_agent).await;
    };
    let downstream = async {
        if !buffered.is_empty() {
            stdout.write_all(&buffered).await?;
        }
        copy(&mut from_agent, &mut stdout).await?;
        stdout.flush().await?;
        Ok::<_, std::io::Error>(())
    };
    // Whichever side ends first ends the session; returning drops the whole pipe (never a
    // half-close, which a Windows named pipe would not deliver).
    tokio::select! {
        _ = upstream => {}
        r = downstream => { r.ok(); }
    }
    Ok(())
}
