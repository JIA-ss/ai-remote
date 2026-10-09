//! Graded preflight: will this host actually work, and if not, what to change.
//!
//! Each check is `ok`, `warn` or `fail` with a reason and a fix; the overall grade is the worst.
//! Most "the screenshot is black", "the click did nothing" or "the controller cannot connect"
//! reports trace back to one of these, so an agent should run `doctor` first and stop on `fail`.
//!
//! Run inside the host (the `doctor` tool, `ai-remote doctor` while a host runs), the checks see
//! the host's own session, token and network. Without a running host, `ai-remote doctor` runs
//! them from the CLI's process, which shares the session but cannot see the network.

use std::path::{Path, PathBuf};

use ar_core::{Profile, Role};
use ar_net::{Mesh, NetworkStatus};
use serde::{Deserialize, Serialize};

use crate::{
    install::{self, InstallStatus, ProgramRules},
    platform::{self, SessionState},
};

/// Ordered so that the worst grade is the maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Grade {
    /// Not applicable here, or could not be checked from where it ran.
    Skip,
    Ok,
    Warn,
    Fail,
}

impl Grade {
    pub fn as_str(self) -> &'static str {
        match self {
            Grade::Skip => "skip",
            Grade::Ok => "ok",
            Grade::Warn => "warn",
            Grade::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub grade: Grade,
    /// What was found, in one sentence.
    pub summary: String,
    /// What to do about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

impl Check {
    fn new(id: &str, grade: Grade, summary: impl Into<String>) -> Self {
        Self { id: id.into(), grade, summary: summary.into(), fix: None }
    }

    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub overall: Grade,
    /// `host` when the running host ran the checks, `cli` when no host was running.
    pub checked_in: String,
    pub checks: Vec<Check>,
}

/// What the checks may look at.
pub struct Inputs<'a> {
    pub profile: &'a Profile,
    /// The running host's transport; `None` when checking without a host.
    pub mesh: Option<&'a Mesh>,
}

/// Runs every check.
pub async fn run(inputs: Inputs<'_>) -> Report {
    let session = platform::session_state();
    let mut checks = session_checks(&session);
    checks.push(dpi_check(platform::dpi_awareness()));
    checks.push(match inputs.mesh {
        Some(mesh) => relay_check(mesh.network().await.map_err(|e| format!("{e:#}"))),
        None => Check::new("relay", Grade::Skip, "not checked: no host is running (start it and run doctor again)"),
    });
    checks.push(approval_check(platform::prompt_blocker()));

    let profile = inputs.profile.clone();
    let in_host = inputs.mesh.is_some();
    let blocking = tokio::task::spawn_blocking(move || {
        let host_exe = std::env::current_exe().ok().map(|p| dunce::simplified(&p).to_path_buf());
        let status = install::status(&profile);
        let mut out = vec![
            firewall_check(&status, host_exe.as_deref(), in_host),
            autostart_check(&status),
            audit_check(&profile),
        ];
        out.push(disk_check(&profile));
        out
    })
    .await;
    match blocking {
        Ok(more) => checks.extend(more),
        Err(e) => checks.push(Check::new("internal", Grade::Warn, format!("some checks did not run: {e}"))),
    }
    Report { overall: overall(&checks), checked_in: if in_host { "host" } else { "cli" }.into(), checks }
}

/// The worst grade of all checks (`ok` when everything was skipped).
pub fn overall(checks: &[Check]) -> Grade {
    checks.iter().map(|c| c.grade).max().unwrap_or(Grade::Ok).max(Grade::Ok)
}

fn session_checks(s: &SessionState) -> Vec<Check> {
    let mut out = Vec::new();
    match s.session_id {
        Some(0) => out.push(
            Check::new(
                "session",
                Grade::Fail,
                "runs in the services session (session 0), which has no desktop: screenshots are black and input goes nowhere",
            )
            .fix("start the host from the logged-in user's session; `ai-remote host install` registers it that way"),
        ),
        Some(id) => out.push(Check::new("session", Grade::Ok, format!("interactive session {id}"))),
        None => out.push(Check::new("session", Grade::Skip, "not applicable on this platform")),
    }
    match s.on_console {
        Some(true) => out.push(Check::new("console", Grade::Ok, "attached to the physical console")),
        Some(false) => out.push(
            Check::new(
                "console",
                Grade::Warn,
                "not attached to the physical console (a remote desktop session): capture and input stop when that \
                 session disconnects, and the console is left locked",
            )
            .fix(format!(
                "return the session to the console instead of disconnecting: from an elevated prompt run `tscon {} \
                 /dest:console`",
                s.session_id.unwrap_or(1)
            )),
        ),
        None => out.push(Check::new("console", Grade::Skip, "not applicable on this platform")),
    }
    if s.desktop_usable {
        let which = s.input_desktop.as_deref().map(|d| format!(" (input desktop: {d})")).unwrap_or_default();
        out.push(Check::new("desktop", Grade::Ok, format!("desktop unlocked{which}")));
    } else {
        let problem = s.problem.clone().unwrap_or_else(|| "the desktop is not usable".into());
        out.push(
            Check::new("desktop", Grade::Fail, format!("{problem}: screenshots and input will not work"))
                .fix("unlock the screen; on an unattended machine turn off automatic locking"),
        );
    }
    match s.elevated {
        Some(true) => out.push(Check::new("elevation", Grade::Ok, "elevated: can operate windows of elevated programs")),
        Some(false) => out.push(
            Check::new(
                "elevation",
                Grade::Warn,
                "not elevated: windows of elevated programs (installers, admin tools) silently ignore its input and UI \
                 actions",
            )
            .fix("run the host with highest privileges; `ai-remote host install` registers it that way"),
        ),
        None => out.push(Check::new("elevation", Grade::Skip, "not applicable on this platform")),
    }
    out
}

fn dpi_check(awareness: Option<&str>) -> Check {
    match awareness {
        None => Check::new("dpi", Grade::Skip, "not applicable on this platform"),
        Some("per_monitor_v2") => Check::new("dpi", Grade::Ok, "per-monitor DPI aware (v2): screenshot and click coordinates agree"),
        Some(other) => Check::new(
            "dpi",
            Grade::Fail,
            format!("DPI awareness is '{other}', not per-monitor v2: clicks on scaled displays land in the wrong place"),
        )
        .fix("remove any compatibility override of high-DPI scaling on the executable (Properties > Compatibility)"),
    }
}

fn relay_check(net: Result<Option<NetworkStatus>, String>) -> Check {
    match net {
        Err(e) => Check::new("relay", Grade::Warn, format!("cannot read the network state: {e}")),
        Ok(None) => Check::new("relay", Grade::Warn, "the transport has not reported its network state yet; try again shortly"),
        Ok(Some(n)) if n.online => {
            let relay = n.home_relay.unwrap_or_else(|| "a relay".into());
            Check::new(
                "relay",
                Grade::Ok,
                format!("connected to {relay}; {} direct address(es)", n.direct_addrs.len()),
            )
        }
        Ok(Some(n)) if n.relays.is_empty() => Check::new(
            "relay",
            Grade::Warn,
            "no relay in use: only controllers that can reach this machine directly will connect",
        )
        .fix("check internet access, or list reachable relays under [network] relays in ai-remote.toml"),
        Ok(Some(n)) => {
            let urls: Vec<&str> = n.relays.iter().map(|r| r.url.as_str()).collect();
            Check::new(
                "relay",
                Grade::Warn,
                format!("not connected to any relay ({}): controllers on other networks may not reach this host", urls.join(", ")),
            )
            .fix("check internet access and outbound firewall or proxy rules; relays use HTTPS")
        }
    }
}

fn approval_check(blocker: Option<String>) -> Check {
    match blocker {
        None => Check::new("approval", Grade::Ok, "approval prompts can be shown on this desktop"),
        Some(reason) => Check::new(
            "approval",
            Grade::Warn,
            format!("approval prompts cannot be shown ({reason}); actions that need approval are refused"),
        )
        .fix("unlock the desktop, or allow trusted tools with [[rule]] entries in ai-remote.toml"),
    }
}

/// The program whose firewall rules matter: the running host, or the one autostart would run.
fn host_program(status: &InstallStatus, host_exe: Option<&Path>, in_host: bool) -> Option<PathBuf> {
    if in_host {
        return host_exe.map(Path::to_path_buf);
    }
    let bins = install::Binaries::locate().ok()?;
    Some(if status.installed || bins.hostw.is_file() { bins.hostw } else { bins.cli })
}

fn firewall_check(status: &anyhow::Result<InstallStatus>, host_exe: Option<&Path>, in_host: bool) -> Check {
    if !cfg!(windows) {
        return Check::new("firewall", Grade::Skip, "not applicable on this platform");
    }
    let status = match status {
        Ok(s) => s,
        Err(e) => return Check::new("firewall", Grade::Warn, format!("cannot read the firewall rules: {e:#}")),
    };
    if status.firewall_enabled == Some(false) {
        return Check::new(
            "firewall",
            Grade::Ok,
            "Windows Firewall is off on every network profile, so nothing blocks inbound connections (if it is turned \
             on, `ai-remote host install` provides the rules)",
        );
    }
    if let Some(e) = &status.firewall_error {
        return Check::new("firewall", Grade::Warn, format!("cannot read the firewall rules: {e}"));
    }
    let Some(program) = host_program(status, host_exe, in_host) else {
        return Check::new("firewall", Grade::Skip, "cannot tell which program runs the host");
    };
    let rules = match status.firewall.iter().find(|r| Path::new(&r.program) == program) {
        Some(r) => r.clone(),
        None => match install::firewall_rules() {
            Ok(all) => install::rules_for(&all, &program),
            Err(e) => return Check::new("firewall", Grade::Warn, format!("cannot read the firewall rules: {e:#}")),
        },
    };
    grade_firewall(&rules)
}

fn grade_firewall(rules: &ProgramRules) -> Check {
    let program = &rules.program;
    if !rules.block.is_empty() {
        return Check::new(
            "firewall",
            Grade::Fail,
            format!(
                "inbound block rule(s) {} apply to {program}: direct connections are refused and traffic falls back to the relay",
                rules.block.iter().map(|b| format!("\"{b}\"")).collect::<Vec<_>>().join(", ")
            ),
        )
        .fix("run `ai-remote host install` from an elevated terminal; it removes them");
    }
    if !rules.allows_udp() {
        return Check::new(
            "firewall",
            Grade::Warn,
            format!(
                "no inbound allow rule covers UDP for {program}: Windows will ask on the desktop at the first incoming \
                 connection, and an unanswered prompt leaves a block rule"
            ),
        )
        .fix("run `ai-remote host install` from an elevated terminal; it adds the rule");
    }
    Check::new("firewall", Grade::Ok, format!("inbound connections allowed for {program} ({})", rules.allow.join(", ")))
}

fn autostart_check(status: &anyhow::Result<InstallStatus>) -> Check {
    match status {
        Err(e) if cfg!(any(windows, target_os = "macos")) => {
            Check::new("autostart", Grade::Warn, format!("cannot read the autostart state: {e:#}"))
        }
        Err(_) => Check::new("autostart", Grade::Skip, "not available on this platform"),
        Ok(s) => grade_autostart(s),
    }
}

fn grade_autostart(s: &InstallStatus) -> Check {
    if !s.installed {
        let why = s.detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default();
        return Check::new(
            "autostart",
            Grade::Warn,
            format!("no {} starts the host at login{why}: it runs only while started by hand", s.kind),
        )
        .fix("run `ai-remote host install` (from an elevated terminal on Windows)");
    }
    match s.current {
        Some(true) => Check::new("autostart", Grade::Ok, format!("{} {} starts the background host at login", s.kind, s.name)),
        _ => Check::new(
            "autostart",
            Grade::Warn,
            format!(
                "{} {} runs {}, not this installation's background host with this profile",
                s.kind,
                s.name,
                s.launches.as_deref().unwrap_or("something else")
            ),
        )
        .fix("run `ai-remote host install` again to point it here"),
    }
}

fn audit_check(profile: &Profile) -> Check {
    let path = match profile.audit_log_path() {
        Ok(p) => p,
        Err(e) => return Check::new("audit", Grade::Fail, format!("no audit log location: {e:#}")),
    };
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(_) => Check::new("audit", Grade::Ok, format!("audit log writable: {}", path.display())),
        Err(e) => Check::new("audit", Grade::Fail, format!("cannot write the audit log {}: {e}", path.display()))
            .fix("fix the permissions of the profile directory, or choose another one with --home"),
    }
}

const GIB: u64 = 1024 * 1024 * 1024;

fn disk_check(profile: &Profile) -> Check {
    let dir = profile.role_dir(Role::Host).unwrap_or_else(|_| profile.root().to_path_buf());
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let disk = disks
        .list()
        .iter()
        .filter(|d| starts_with_path(&dir, d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len());
    match disk {
        Some(d) => grade_disk(d.available_space(), &d.mount_point().display().to_string()),
        None => Check::new("disk", Grade::Skip, format!("cannot tell which disk holds {}", dir.display())),
    }
}

fn starts_with_path(path: &Path, prefix: &Path) -> bool {
    if cfg!(windows) {
        path.to_string_lossy().to_lowercase().starts_with(&prefix.to_string_lossy().to_lowercase())
    } else {
        path.starts_with(prefix)
    }
}

/// Logs, jobs and audit records need little, but a full disk breaks all of them at once.
fn grade_disk(free: u64, mount: &str) -> Check {
    let shown = format!("{:.1} GiB free on {mount}", free as f64 / GIB as f64);
    if free < GIB / 2 {
        Check::new("disk", Grade::Fail, format!("{shown}: logs and the audit log may stop being written"))
            .fix("free some disk space")
    } else if free < 5 * GIB {
        Check::new("disk", Grade::Warn, format!("{shown}: running low")).fix("free some disk space")
    } else {
        Check::new("disk", Grade::Ok, shown)
    }
}

#[cfg(test)]
mod tests {
    use ar_net::RelayState;

    use super::*;

    fn grades(checks: &[Check]) -> Vec<(&str, Grade)> {
        checks.iter().map(|c| (c.id.as_str(), c.grade)).collect()
    }

    #[test]
    fn overall_is_the_worst_grade() {
        let c = |g| Check::new("x", g, "");
        assert_eq!(overall(&[c(Grade::Ok), c(Grade::Warn), c(Grade::Skip)]), Grade::Warn);
        assert_eq!(overall(&[c(Grade::Fail), c(Grade::Warn)]), Grade::Fail);
        assert_eq!(overall(&[c(Grade::Skip)]), Grade::Ok);
        assert_eq!(overall(&[]), Grade::Ok);
    }

    #[test]
    fn healthy_interactive_session() {
        let s = SessionState {
            session_id: Some(1),
            on_console: Some(true),
            elevated: Some(true),
            input_desktop: Some("Default".into()),
            desktop_usable: true,
            problem: None,
        };
        assert_eq!(
            grades(&session_checks(&s)),
            vec![("session", Grade::Ok), ("console", Grade::Ok), ("desktop", Grade::Ok), ("elevation", Grade::Ok)]
        );
    }

    #[test]
    fn services_session_and_locked_desktop_fail() {
        let s = SessionState {
            session_id: Some(0),
            on_console: Some(false),
            elevated: Some(false),
            input_desktop: None,
            desktop_usable: false,
            problem: Some("the desktop is locked".into()),
        };
        let checks = session_checks(&s);
        assert_eq!(
            grades(&checks),
            vec![("session", Grade::Fail), ("console", Grade::Warn), ("desktop", Grade::Fail), ("elevation", Grade::Warn)]
        );
        assert!(checks[1].fix.as_deref().unwrap().contains("tscon 0 /dest:console"));
        assert!(checks.iter().filter(|c| c.grade != Grade::Ok).all(|c| c.fix.is_some()));
    }

    #[test]
    fn other_platforms_skip_windows_session_facts() {
        let s = SessionState { desktop_usable: true, ..Default::default() };
        assert_eq!(
            grades(&session_checks(&s)),
            vec![("session", Grade::Skip), ("console", Grade::Skip), ("desktop", Grade::Ok), ("elevation", Grade::Skip)]
        );
    }

    #[test]
    fn dpi_grades() {
        assert_eq!(dpi_check(Some("per_monitor_v2")).grade, Grade::Ok);
        assert_eq!(dpi_check(Some("system")).grade, Grade::Fail);
        assert_eq!(dpi_check(None).grade, Grade::Skip);
    }

    #[test]
    fn relay_grades() {
        let net = |online, relays: Vec<(&str, bool)>| NetworkStatus {
            online,
            home_relay: online.then(|| "https://relay.example:443".to_string()),
            relays: relays.into_iter().map(|(u, c)| RelayState { url: u.into(), connected: c }).collect(),
            direct_addrs: vec!["192.0.2.1:5000".into()],
        };
        let ok = relay_check(Ok(Some(net(true, vec![("https://relay.example:443", true)]))));
        assert_eq!(ok.grade, Grade::Ok);
        assert!(ok.summary.contains("relay.example"));
        assert_eq!(relay_check(Ok(Some(net(false, vec![("https://relay.example:443", false)])))).grade, Grade::Warn);
        assert_eq!(relay_check(Ok(Some(net(false, vec![])))).grade, Grade::Warn);
        assert_eq!(relay_check(Ok(None)).grade, Grade::Warn);
        assert_eq!(relay_check(Err("boom".into())).grade, Grade::Warn);
    }

    #[test]
    fn firewall_grades() {
        let rules = |allow: &[&str], block: &[&str]| ProgramRules {
            program: r"C:\x\ai-remote-hostw.exe".into(),
            allow: allow.iter().map(|s| s.to_string()).collect(),
            block: block.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(grade_firewall(&rules(&["UDP", "TCP"], &[])).grade, Grade::Ok);
        assert_eq!(grade_firewall(&rules(&["TCP"], &[])).grade, Grade::Warn);
        assert_eq!(grade_firewall(&rules(&[], &[])).grade, Grade::Warn);
        let blocked = grade_firewall(&rules(&["UDP"], &["ai-remote-hostw"]));
        assert_eq!(blocked.grade, Grade::Fail);
        assert!(blocked.summary.contains("\"ai-remote-hostw\""));
    }

    #[test]
    fn firewall_switched_off_blocks_nothing() {
        let off = InstallStatus { firewall_enabled: Some(false), ..Default::default() };
        let check = firewall_check(&Ok(off), None, true);
        assert_eq!(check.grade, if cfg!(windows) { Grade::Ok } else { Grade::Skip });
    }

    #[test]
    fn autostart_grades() {
        let mut s = InstallStatus { kind: "Task Scheduler task".into(), name: r"\ai-remote\host-1".into(), ..Default::default() };
        assert_eq!(grade_autostart(&s).grade, Grade::Warn);
        s.installed = true;
        s.current = Some(true);
        assert_eq!(grade_autostart(&s).grade, Grade::Ok);
        s.current = Some(false);
        s.launches = Some(r#""C:\old\ai-remote-hostw.exe" --home "C:\p""#.into());
        let stale = grade_autostart(&s);
        assert_eq!(stale.grade, Grade::Warn);
        assert!(stale.summary.contains(r"C:\old"));
    }

    #[test]
    fn disk_grades() {
        assert_eq!(grade_disk(100 * GIB, "C:\\").grade, Grade::Ok);
        assert_eq!(grade_disk(2 * GIB, "C:\\").grade, Grade::Warn);
        assert_eq!(grade_disk(GIB / 4, "C:\\").grade, Grade::Fail);
    }

    #[test]
    fn approval_grades() {
        assert_eq!(approval_check(None).grade, Grade::Ok);
        assert_eq!(approval_check(Some("the desktop is locked".into())).grade, Grade::Warn);
    }
}
