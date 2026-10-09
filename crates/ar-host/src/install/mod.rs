//! Starting the host automatically at login, outside any terminal (`ai-remote host install`).
//!
//! The host must live in the logged-in user's interactive desktop session (capture, input and
//! approval prompts all need that desktop), so it is never a system service:
//!
//! - **Windows**: a Task Scheduler task triggered at the user's logon, run with highest privileges
//!   and an interactive token, starting the windowless `ai-remote-hostw` binary. Plus inbound
//!   firewall allow rules for the host programs: the first time a program listens, Windows asks
//!   on the desktop whether to allow it; on an unattended machine nobody answers and the prompt
//!   leaves *block* rules behind, which beat allow rules and quietly push connections onto the
//!   relay.
//! - **macOS**: a per-user LaunchAgent, which loads into the user's GUI session.
//!
//! Every change is first described as a [`Plan`]: `--dry-run` prints exactly what would run, and
//! system tools are always called by absolute path (a polluted `PATH` must not pick them).

// Both generators are plain text, compiled and tested on every platform.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod launchd;
#[cfg_attr(not(windows), allow(dead_code))]
mod task;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ar_core::Profile;
use serde::{Deserialize, Serialize};

pub use task::quote_arg;

/// File name of the windowless background host next to the `ai-remote` executable.
pub const HOSTW_NAME: &str = if cfg!(windows) {
    "ai-remote-hostw.exe"
} else {
    "ai-remote-hostw"
};
const CLI_NAME: &str = if cfg!(windows) {
    "ai-remote.exe"
} else {
    "ai-remote"
};

/// Name of the inbound firewall rules for the host programs (Windows).
pub const FIREWALL_RULE: &str = "ai-remote host";

/// The two executables of an installation, found next to the running one.
#[derive(Debug, Clone)]
pub struct Binaries {
    /// `ai-remote`: the CLI, which also runs a foreground host (`ai-remote host`).
    pub cli: PathBuf,
    /// `ai-remote-hostw`: the background host the autostart entry launches.
    pub hostw: PathBuf,
}

impl Binaries {
    pub fn locate() -> Result<Self> {
        let exe = std::env::current_exe().context("locate the running executable")?;
        let exe = dunce::simplified(&exe).to_path_buf();
        let dir = exe
            .parent()
            .context("the running executable has no directory")?;
        Ok(Self {
            cli: dir.join(CLI_NAME),
            hostw: dir.join(HOSTW_NAME),
        })
    }

    /// Both programs, background host first.
    pub fn programs(&self) -> [&Path; 2] {
        [&self.hostw, &self.cli]
    }

    fn require_hostw(&self) -> Result<()> {
        if !self.hostw.is_file() {
            bail!(
                "{} is missing; it must sit next to {} (build or install both binaries of the ai-remote package)",
                self.hostw.display(),
                self.cli.display()
            );
        }
        Ok(())
    }
}

/// One change an install or uninstall makes.
#[derive(Debug, Clone)]
pub enum Action {
    WriteFile {
        path: PathBuf,
        contents: Vec<u8>,
        /// The contents as text, for `--dry-run`.
        preview: String,
    },
    RemoveFile {
        path: PathBuf,
    },
    Run {
        program: PathBuf,
        /// Arguments. On Windows each one is passed verbatim (already quoted where needed),
        /// because netsh and schtasks parse their command lines their own way.
        args: Vec<String>,
        why: String,
        /// A failure is reported but does not stop the plan (for "delete if present" steps).
        tolerate_failure: bool,
        /// Only an administrator can do this (machine-wide firewall rules), whoever the plan is for.
        admin: bool,
    },
}

impl Action {
    fn run(program: PathBuf, args: &[String], why: &str) -> Self {
        Action::Run {
            program,
            args: args.to_vec(),
            why: why.to_string(),
            tolerate_failure: false,
            admin: false,
        }
    }

    fn run_tolerant(program: PathBuf, args: &[String], why: &str) -> Self {
        Action::Run {
            program,
            args: args.to_vec(),
            why: why.to_string(),
            tolerate_failure: true,
            admin: false,
        }
    }

    /// Marks a step that needs an administrator.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn admin(mut self) -> Self {
        if let Action::Run { admin, .. } = &mut self {
            *admin = true;
        }
        self
    }

    /// Whether only an administrator can perform this step.
    pub fn needs_admin(&self) -> bool {
        matches!(self, Action::Run { admin: true, .. })
    }

    /// One line saying what this does.
    pub fn describe(&self) -> String {
        match self {
            Action::WriteFile { path, .. } => format!("write {}", path.display()),
            Action::RemoveFile { path } => format!("remove {}", path.display()),
            Action::Run {
                program,
                args,
                why,
                admin,
                ..
            } => {
                let mut line = display_arg(&program.to_string_lossy());
                for a in args {
                    line.push(' ');
                    line.push_str(&display_arg(a));
                }
                let who = if *admin { " (administrator)" } else { "" };
                format!("{why}{who}:\n    {line}")
            }
        }
    }

    /// Performs the change. `Ok(Some(note))` reports a tolerated failure.
    pub fn execute(&self) -> Result<Option<String>> {
        match self {
            Action::WriteFile { path, contents, .. } => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)
                        .with_context(|| format!("create {}", dir.display()))?;
                }
                std::fs::write(path, contents)
                    .with_context(|| format!("write {}", path.display()))?;
                Ok(None)
            }
            Action::RemoveFile { path } => match std::fs::remove_file(path) {
                Ok(()) => Ok(None),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Ok(Some("already absent".into()))
                }
                Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
            },
            Action::Run {
                program,
                args,
                tolerate_failure,
                ..
            } => {
                let mut cmd = std::process::Command::new(program);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    for a in args {
                        cmd.raw_arg(a);
                    }
                }
                #[cfg(not(windows))]
                cmd.args(args);
                crate::platform::no_console_window(&mut cmd);
                let out = cmd
                    .stdin(std::process::Stdio::null())
                    .output()
                    .with_context(|| format!("run {}", program.display()))?;
                if out.status.success() {
                    return Ok(None);
                }
                let text = tool_output(&out);
                let name = program
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if *tolerate_failure {
                    Ok(Some(format!("{name}: {text}")))
                } else {
                    bail!("{name} failed ({}): {text}", out.status)
                }
            }
        }
    }
}

/// Quotes an argument for display only when it would otherwise be ambiguous.
fn display_arg(a: &str) -> String {
    if a.contains(' ') && !a.contains('"') {
        format!("\"{a}\"")
    } else {
        a.to_string()
    }
}

/// What a system tool printed, as one trimmed line of text.
fn tool_output(out: &std::process::Output) -> String {
    let mut text = crate::platform::decode_console_output(&out.stdout);
    text.push(' ');
    text.push_str(&crate::platform::decode_console_output(&out.stderr));
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        "no output".into()
    } else {
        text
    }
}

/// Everything an install or uninstall would do.
#[derive(Debug, Clone)]
pub struct Plan {
    pub summary: String,
    /// Things worth knowing before (or instead of) running it.
    pub notes: Vec<String>,
    pub actions: Vec<Action>,
    /// Some actions only work from an elevated (administrator) process.
    pub needs_elevation: bool,
    /// Why running this plan from here would produce a broken result (shown, and the plan is
    /// refused unless forced).
    pub blocker: Option<String>,
}

impl Plan {
    /// Whether any step needs an administrator.
    pub fn has_admin_steps(&self) -> bool {
        self.actions.iter().any(Action::needs_admin)
    }
}

/// Which part of the autostart setup a plan covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    /// The autostart entry and, on Windows, the firewall rules.
    #[default]
    All,
    /// Only the firewall rules (Windows): what an administrator adds for a standard user, whose
    /// own install registers the logon task without them.
    FirewallOnly,
}

/// Autostart and firewall state of this profile's host.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstallStatus {
    /// What starts the host: "Task Scheduler task" or "LaunchAgent".
    pub kind: String,
    /// Task name or plist path.
    pub name: String,
    pub installed: bool,
    /// The command line the entry runs, when installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launches: Option<String>,
    /// Whether that is this installation's background host with this profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<bool>,
    /// Why the entry's state could not be read (or the tool's answer when it is absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Inbound firewall rules for each host program (Windows).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub firewall: Vec<ProgramRules>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firewall_error: Option<String>,
    /// Whether the firewall is on for any network profile (Windows; `None` when unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firewall_enabled: Option<bool>,
}

/// One inbound firewall rule, reduced to what matters here. Remote-address and network-profile
/// scoping are not modelled (the reader cannot represent every profile combination).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FwRule {
    pub name: String,
    /// The program the rule is limited to, as written in the rule (may contain `%VARIABLES%`);
    /// `None` for every program.
    pub program: Option<String>,
    pub allow: bool,
    pub enabled: bool,
    /// `TCP`, `UDP`, `any`, ...
    pub protocol: String,
    /// The service the rule is limited to; `None` for any.
    #[serde(default)]
    pub service: Option<String>,
    /// The rule covers every local port.
    #[serde(default)]
    pub all_local_ports: bool,
}

impl FwRule {
    /// An enabled block rule that refuses inbound UDP for every program and port. Block rules beat
    /// allow rules, so it overrides ours.
    fn blocks_all_udp(&self) -> bool {
        self.enabled
            && !self.allow
            && self.program.is_none()
            && self.service.is_none()
            && self.all_local_ports
            && (self.protocol == "UDP" || self.protocol == "any")
    }
}

/// The enabled inbound rules that apply to one program.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProgramRules {
    pub program: String,
    /// Protocols with an allow rule.
    pub allow: Vec<String>,
    /// Names of block rules (any protocol).
    pub block: Vec<String>,
    /// Names of block rules for every program that cover inbound UDP on every port.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub block_all: Vec<String>,
}

impl ProgramRules {
    /// The transport is QUIC over UDP: an allow rule must cover UDP.
    pub fn allows_udp(&self) -> bool {
        self.allow.iter().any(|p| p == "UDP" || p == "any")
    }
}

/// Collects the enabled inbound rules that name `program` (paths written with `%VARIABLES%`, as
/// installers and group policies often write them, are expanded first), plus the block rules that
/// cover every program.
pub fn rules_for(rules: &[FwRule], program: &Path) -> ProgramRules {
    let mut out = ProgramRules {
        program: program.display().to_string(),
        ..Default::default()
    };
    let names_program = |r: &FwRule| {
        r.program
            .as_deref()
            .is_some_and(|p| same_path(&expand_env(p, |v| std::env::var(v).ok()), program))
    };
    for r in rules.iter().filter(|r| r.enabled) {
        if names_program(r) {
            if r.allow {
                if !out.allow.contains(&r.protocol) {
                    out.allow.push(r.protocol.clone());
                }
            } else if !out.block.contains(&r.name) {
                out.block.push(r.name.clone());
            }
        } else if r.blocks_all_udp() && !out.block_all.contains(&r.name) {
            out.block_all.push(r.name.clone());
        }
    }
    out
}

/// Replaces `%NAME%` with the value `lookup` gives; unknown names stay as written.
fn expand_env(text: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let value = after
            .find('%')
            .filter(|&end| end > 0)
            .and_then(|end| Some((end, lookup(&after[..end])?)));
        match value {
            Some((end, value)) => {
                out.push_str(&value);
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The account id of this process: a SID on Windows, a numeric uid elsewhere.
#[cfg(any(windows, target_os = "macos"))]
fn current_user_id() -> Result<String> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let pid = sysinfo::get_current_pid().map_err(|e| anyhow::anyhow!("current process id: {e}"))?;
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing().with_user(UpdateKind::Always),
    );
    let uid = sys
        .process(pid)
        .and_then(|p| p.user_id())
        .context("cannot read the current user's account id")?;
    Ok(uid.to_string())
}

/// Windows paths compare case-insensitively and with either slash.
fn same_path(a: &str, b: &Path) -> bool {
    let norm = |s: &str| {
        let s = s.trim().replace('/', "\\");
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    norm(a) == norm(&b.to_string_lossy())
}

/// Reads the inbound firewall rules (Windows; empty elsewhere).
pub fn firewall_rules() -> Result<Vec<FwRule>> {
    #[cfg(windows)]
    {
        windows::firewall_rules()
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}

/// The plan for `ai-remote host install`.
pub fn install_plan(profile: &Profile, scope: Scope) -> Result<Plan> {
    #[cfg(windows)]
    {
        windows::install_plan(profile, scope)
    }
    #[cfg(target_os = "macos")]
    {
        no_firewall_scope(scope)?;
        macos::install_plan(profile)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (profile, scope);
        bail!("autostart is available on Windows and macOS only")
    }
}

/// The plan for `ai-remote host uninstall`.
pub fn uninstall_plan(profile: &Profile, scope: Scope) -> Result<Plan> {
    #[cfg(windows)]
    {
        windows::uninstall_plan(profile, scope)
    }
    #[cfg(target_os = "macos")]
    {
        no_firewall_scope(scope)?;
        macos::uninstall_plan(profile)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (profile, scope);
        bail!("autostart is available on Windows and macOS only")
    }
}

#[cfg(target_os = "macos")]
fn no_firewall_scope(scope: Scope) -> Result<()> {
    if scope == Scope::FirewallOnly {
        bail!(
            "--firewall-only applies to Windows only; macOS needs no firewall rules for the host"
        );
    }
    Ok(())
}

/// Starts the installed autostart entry now (after an install), instead of waiting for a login.
pub fn start_action(profile: &Profile) -> Result<Action> {
    #[cfg(windows)]
    {
        Ok(windows::start_action(profile))
    }
    #[cfg(target_os = "macos")]
    {
        macos::start_action(profile)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = profile;
        bail!("autostart is available on Windows and macOS only")
    }
}

/// Reads the autostart and firewall state. Blocking (runs system tools): call it from a blocking
/// context.
pub fn status(profile: &Profile) -> Result<InstallStatus> {
    #[cfg(windows)]
    {
        windows::status(profile)
    }
    #[cfg(target_os = "macos")]
    {
        macos::status(profile)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = profile;
        bail!("autostart is available on Windows and macOS only")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(name: &str, program: &str, allow: bool, enabled: bool, protocol: &str) -> FwRule {
        FwRule {
            name: name.into(),
            program: Some(program.into()),
            allow,
            enabled,
            protocol: protocol.into(),
            service: None,
            all_local_ports: true,
        }
    }

    #[test]
    fn collects_rules_of_one_program() {
        let exe = if cfg!(windows) {
            r"C:\Tools\ai-remote-hostw.exe"
        } else {
            "/opt/ai-remote-hostw"
        };
        let shouted = if cfg!(windows) {
            r"c:/TOOLS/AI-REMOTE-HOSTW.EXE"
        } else {
            "/opt/ai-remote-hostw"
        };
        let rules = vec![
            rule("ai-remote host", exe, true, true, "UDP"),
            rule("ai-remote host", shouted, true, true, "TCP"),
            rule("ai-remote-hostw", exe, false, true, "UDP"),
            rule("old", exe, false, false, "TCP"),
            rule("other", "C:/other.exe", false, true, "any"),
        ];
        let r = rules_for(&rules, Path::new(exe));
        assert_eq!(r.allow, vec!["UDP".to_string(), "TCP".to_string()]);
        assert_eq!(r.block, vec!["ai-remote-hostw".to_string()]);
        assert!(r.allows_udp());
    }

    #[test]
    fn rule_paths_with_variables_are_expanded() {
        let lookup = |v: &str| {
            v.eq_ignore_ascii_case("ProgramFiles")
                .then(|| r"C:\Program Files".to_string())
        };
        assert_eq!(
            expand_env(r"%ProgramFiles%\ai-remote\x.exe", lookup),
            r"C:\Program Files\ai-remote\x.exe"
        );
        assert_eq!(expand_env(r"%NOPE%\x.exe", lookup), r"%NOPE%\x.exe");
        assert_eq!(expand_env("100% sure", lookup), "100% sure");
        assert_eq!(expand_env("%%", lookup), "%%");
        assert_eq!(expand_env(r"C:\plain.exe", lookup), r"C:\plain.exe");
    }

    #[test]
    fn block_rules_for_every_program_are_reported() {
        let exe = if cfg!(windows) {
            r"C:\Tools\ai-remote-hostw.exe"
        } else {
            "/opt/ai-remote-hostw"
        };
        let everything = FwRule {
            program: None,
            ..rule("block all udp", exe, false, true, "UDP")
        };
        let one_port = FwRule {
            all_local_ports: false,
            ..everything.clone()
        };
        let a_service = FwRule {
            service: Some("Dnscache".into()),
            ..everything.clone()
        };
        let tcp = FwRule {
            protocol: "TCP".into(),
            ..everything.clone()
        };
        let off = FwRule {
            enabled: false,
            ..everything.clone()
        };
        let allow = FwRule {
            allow: true,
            ..everything.clone()
        };
        let r = rules_for(
            &[everything, one_port, a_service, tcp, off, allow],
            Path::new(exe),
        );
        assert_eq!(r.block_all, vec!["block all udp".to_string()]);
        assert!(r.block.is_empty());
    }

    #[test]
    fn tcp_only_allow_does_not_cover_quic() {
        let r = ProgramRules {
            program: "x".into(),
            allow: vec!["TCP".into()],
            ..Default::default()
        };
        assert!(!r.allows_udp());
        let any = ProgramRules {
            program: "x".into(),
            allow: vec!["any".into()],
            ..Default::default()
        };
        assert!(any.allows_udp());
    }

    #[test]
    fn describes_commands_readably() {
        let a = Action::run(
            PathBuf::from("C:/Windows/System32/netsh.exe"),
            &["advfirewall".into()],
            "add a rule",
        );
        assert_eq!(
            a.describe(),
            "add a rule:\n    C:/Windows/System32/netsh.exe advfirewall"
        );
    }
}
