//! This computer as a host, seen from the app running on it: the tray and the "This device"
//! settings page. Everything goes over the host's local control channel (`control.sock` or the
//! named pipe), the same one `ai-remote host status`, `stop-host`, `pair-code`, `peers` and
//! `bypass` use. So the app needs none of the host's code or privileges, and a host started by the
//! LaunchAgent, the logon task or the Windows service is reached the same way (the service's
//! channel admits the account that installed it). Starting the host and the checks without a
//! running host go through the `ai-remote` command, which knows how the host was installed.

use std::{path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result, anyhow};
use ar_core::{Profile, Role};
use ar_net::ipc::Conn;
use serde_json::{Value, json};

/// What the control channel says about this profile's host.
pub enum Probe {
    Running(Value),
    /// Nothing listens (no socket or pipe, or a stale socket of a host that ended).
    Stopped,
    /// Something listens but cannot be read (refused access, a failed request): a host probably runs.
    Unknown(String),
}

/// Connects to the running host's control channel; `Ok(None)` when no host runs for this profile.
pub async fn connect(profile: &Profile) -> Result<Option<Conn>> {
    let endpoint = profile.control_endpoint(Role::Host)?;
    match Conn::connect(&endpoint).await {
        Ok(conn) => Ok(Some(conn)),
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused) => Ok(None),
        Err(e) => Err(anyhow!("cannot reach the host's control channel: {e}")),
    }
}

/// One request to the running host; `Ok(None)` when no host runs.
pub async fn request(profile: &Profile, req: Value) -> Result<Option<Value>> {
    let Some(mut conn) = connect(profile).await? else { return Ok(None) };
    conn.request(&req).await.map(Some)
}

/// Like [`request`], but "no host" is an error for the page.
pub async fn require(profile: &Profile, req: Value) -> Result<Value> {
    request(profile, req).await?.ok_or_else(|| anyhow!("ai-remote host is not running on this computer"))
}

/// The host's `status`, telling "not running" from "cannot tell".
pub async fn probe(profile: &Profile) -> Probe {
    match request(profile, json!({"op": "status"})).await {
        Ok(Some(v)) => Probe::Running(v),
        Ok(None) => Probe::Stopped,
        Err(e) => Probe::Unknown(format!("{e:#}")),
    }
}

/// Who drives this computer, as the tray shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverState {
    Agent,
    /// A person took over from this device.
    Person(String),
    Paused,
}

/// A tool call running now, or one that ended moments ago: someone is operating this computer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Busy {
    pub peer: String,
    pub tool: String,
    /// A person's call (from the desktop app after taking over), not an agent's.
    pub person: bool,
}

/// How long after its end a call still counts as "operating", so the tray's 3 s poll does not miss
/// the short calls an agent makes one after another.
pub const BUSY_AFTER_MS: u64 = 8_000;

/// What the tray shows, distilled from the host's `status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// A host runs here (or one listens but cannot be read: see `unknown`).
    pub running: bool,
    /// Why the running host cannot be read, if so.
    pub unknown: Option<String>,
    /// Windows service mode (SYSTEM): starting it again after a stop takes an administrator.
    pub system: bool,
    /// Controllers with a connection open on any channel, by name.
    pub connected: Vec<String>,
    pub driver: DriverState,
    pub busy: Option<Busy>,
}

impl Summary {
    pub const STOPPED: Summary = Summary {
        running: false,
        unknown: None,
        system: false,
        connected: vec![],
        driver: DriverState::Agent,
        busy: None,
    };

    pub fn from_probe(p: &Probe, now_ms: u64) -> Self {
        match p {
            Probe::Running(v) => Self::from_status(Some(v), now_ms),
            Probe::Stopped => Self::STOPPED,
            Probe::Unknown(why) => Summary { running: true, unknown: Some(why.clone()), ..Self::STOPPED },
        }
    }

    /// `now_ms`: the time to judge "moments ago" against (same clock as the host: same machine).
    pub fn from_status(v: Option<&Value>, now_ms: u64) -> Self {
        let Some(v) = v else { return Self::STOPPED };
        let connected =
            v["connected"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect());
        let driver = match v["driver"]["state"].as_str() {
            Some("person") => DriverState::Person(v["driver"]["device"].as_str().unwrap_or("?").to_string()),
            Some("paused") => DriverState::Paused,
            _ => DriverState::Agent,
        };
        let call = |c: &Value| Busy {
            peer: c["peer"].as_str().unwrap_or("?").to_string(),
            tool: c["tool"].as_str().unwrap_or("?").to_string(),
            person: c["person"].as_bool().unwrap_or(false),
        };
        let running_call = v["calls"].as_array().and_then(|a| a.last()).map(call);
        let recent_call = v["last_call"]
            .as_object()
            .filter(|_| v["last_call"]["ended_ms"].as_u64().is_some_and(|t| now_ms.saturating_sub(t) < BUSY_AFTER_MS))
            .map(|_| call(&v["last_call"]));
        Self {
            running: true,
            unknown: None,
            system: v["system"].as_bool().unwrap_or(false),
            connected: connected.unwrap_or_default(),
            driver,
            busy: running_call.or(recent_call),
        }
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Runs `ai-remote --json <args>` without a console window and parses its output. Used where the
/// host's own installation knowledge is needed: `host start`, and `doctor` while no host runs.
pub async fn cli_json(cli: &Path, args: &[&str]) -> Result<Value> {
    let mut cmd = tokio::process::Command::new(cli);
    cmd.arg("--json").args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // tokio's Command has `creation_flags` itself (no `CommandExt` import).
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let output = tokio::time::timeout(Duration::from_secs(40), cmd.output())
        .await
        .map_err(|_| anyhow!("ai-remote {} did not finish within 40 s", args.join(" ")))?
        .with_context(|| format!("run {}", cli.display()))?;
    if !output.status.success() {
        // `doctor` prints its report and exits 1 when a check fails: the report is the answer.
        if let Ok(v) = serde_json::from_slice::<Value>(&output.stdout)
            && v.is_object()
        {
            return Ok(v);
        }
        let err = String::from_utf8_lossy(&output.stderr);
        let err = err.trim().lines().last().unwrap_or("").trim_start_matches("Error: ");
        return Err(anyhow!(
            "{}",
            if err.is_empty() { format!("ai-remote {} failed", args.join(" ")) } else { err.into() }
        ));
    }
    serde_json::from_slice(&output.stdout).with_context(|| format!("ai-remote {} printed no JSON", args.join(" ")))
}

/// The System Settings pane the "This device" page may open (macOS only: Windows has no page for
/// the host's permissions or its task / service). Only these; the app never changes a setting.
fn pane_url(pane: &str) -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        match pane {
            "screen_recording" => Some("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"),
            "accessibility" => Some("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"),
            "login_items" => Some("x-apple.systempreferences:com.apple.LoginItems-Settings.extension"),
            _ => None,
        }
    } else {
        None
    }
}

pub fn open_settings_pane(pane: &str) -> Result<()> {
    let url = pane_url(pane).ok_or_else(|| anyhow!("no settings pane for {pane} on this system"))?;
    std::process::Command::new("open").arg(url).spawn().context("open System Settings")?;
    Ok(())
}

/// Shows `file` selected in the file manager, or its folder when the file does not exist yet.
pub fn reveal(file: &Path) -> Result<()> {
    let exists = file.is_file();
    let dir = file.parent().unwrap_or(file);
    #[cfg(target_os = "macos")]
    let spawned = if exists {
        std::process::Command::new("open").arg("-R").arg(file).spawn()
    } else {
        std::process::Command::new("open").arg(dir).spawn()
    };
    #[cfg(windows)]
    let spawned = if exists {
        std::process::Command::new("explorer").arg(format!("/select,{}", file.display())).spawn()
    } else {
        std::process::Command::new("explorer").arg(dir).spawn()
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    let spawned: std::io::Result<std::process::Child> = {
        let _ = (exists, dir);
        Err(std::io::Error::other("not on this system"))
    };
    spawned.map(|_| ()).with_context(|| format!("show {}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_reads_the_host_status() {
        assert_eq!(Summary::from_status(None, 0), Summary::STOPPED);
        let v = json!({
            "connected": ["laptop", "phone"],
            "driver": {"state": "person", "device": "laptop", "since_ms": 1, "agent_may_watch": false},
        });
        let s = Summary::from_status(Some(&v), 0);
        assert!(s.running);
        assert_eq!(s.connected, vec!["laptop", "phone"]);
        assert_eq!(s.driver, DriverState::Person("laptop".into()));
        assert_eq!(s.busy, None);
        let v = json!({"connected": [], "driver": {"state": "paused", "by": "host", "since_ms": 1}});
        assert_eq!(Summary::from_status(Some(&v), 0).driver, DriverState::Paused);
        // Hosts older than these fields still answer.
        let old = Summary::from_status(Some(&json!({"name": "pc"})), 0);
        assert_eq!((old.running, old.driver, old.busy, old.system), (true, DriverState::Agent, None, false));
        let unknown = Summary::from_probe(&Probe::Unknown("refused".into()), 0);
        assert!(unknown.running && unknown.unknown.is_some(), "a host that cannot be read is not reported stopped");
        assert_eq!(Summary::from_probe(&Probe::Stopped, 0), Summary::STOPPED);
    }

    #[test]
    fn a_running_call_or_one_that_just_ended_is_busy() {
        let running = json!({"calls": [{"peer": "laptop", "tool": "screenshot", "person": false, "since_ms": 5}]});
        let busy = Summary::from_status(Some(&running), 10).busy.unwrap();
        assert_eq!((busy.peer.as_str(), busy.tool.as_str(), busy.person), ("laptop", "screenshot", false));
        let ended = json!({"calls": [], "last_call": {"peer": "laptop", "tool": "click", "person": true, "since_ms": 1, "ended_ms": 1_000}});
        assert!(Summary::from_status(Some(&ended), 1_000 + BUSY_AFTER_MS - 1).busy.is_some_and(|b| b.person));
        assert_eq!(Summary::from_status(Some(&ended), 1_000 + BUSY_AFTER_MS).busy, None);
    }
}
