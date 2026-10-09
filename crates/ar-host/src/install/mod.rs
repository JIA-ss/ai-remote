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
pub const HOSTW_NAME: &str = if cfg!(windows) { "ai-remote-hostw.exe" } else { "ai-remote-hostw" };
const CLI_NAME: &str = if cfg!(windows) { "ai-remote.exe" } else { "ai-remote" };

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
        let dir = exe.parent().context("the running executable has no directory")?;
        Ok(Self { cli: dir.join(CLI_NAME), hostw: dir.join(HOSTW_NAME) })
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
    },
}

impl Action {
    fn run(program: PathBuf, args: &[String], why: &str) -> Self {
        Action::Run { program, args: args.to_vec(), why: why.to_string(), tolerate_failure: false }
    }

    fn run_tolerant(program: PathBuf, args: &[String], why: &str) -> Self {
        Action::Run { program, args: args.to_vec(), why: why.to_string(), tolerate_failure: true }
    }

    /// One line saying what this does.
    pub fn describe(&self) -> String {
        match self {
            Action::WriteFile { path, .. } => format!("write {}", path.display()),
            Action::RemoveFile { path } => format!("remove {}", path.display()),
            Action::Run { program, args, why, .. } => {
                let mut line = display_arg(&program.to_string_lossy());
                for a in args {
                    line.push(' ');
                    line.push_str(&display_arg(a));
                }
                format!("{why}:\n    {line}")
            }
        }
    }

    /// Performs the change. `Ok(Some(note))` reports a tolerated failure.
    pub fn execute(&self) -> Result<Option<String>> {
        match self {
            Action::WriteFile { path, contents, .. } => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
                }
                std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))?;
                Ok(None)
            }
            Action::RemoveFile { path } => match std::fs::remove_file(path) {
                Ok(()) => Ok(None),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Some("already absent".into())),
                Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
            },
            Action::Run { program, args, tolerate_failure, .. } => {
                let mut cmd = std::process::Command::new(program);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
                    for a in args {
                        cmd.raw_arg(a);
                    }
                    cmd.creation_flags(CREATE_NO_WINDOW);
                }
                #[cfg(not(windows))]
                cmd.args(args);
                let out = cmd
                    .stdin(std::process::Stdio::null())
                    .output()
                    .with_context(|| format!("run {}", program.display()))?;
                if out.status.success() {
                    return Ok(None);
                }
                let text = tool_output(&out);
                let name = program.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
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
    if a.contains(' ') && !a.contains('"') { format!("\"{a}\"") } else { a.to_string() }
}

/// What a system tool printed, as one trimmed line of text.
fn tool_output(out: &std::process::Output) -> String {
    let mut text = crate::platform::decode_console_output(&out.stdout);
    text.push(' ');
    text.push_str(&crate::platform::decode_console_output(&out.stderr));
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() { "no output".into() } else { text }
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

/// One inbound firewall rule, reduced to what matters here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FwRule {
    pub name: String,
    pub program: Option<String>,
    pub allow: bool,
    pub enabled: bool,
    /// `TCP`, `UDP`, `any`, ...
    pub protocol: String,
}

/// The enabled inbound rules that apply to one program.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProgramRules {
    pub program: String,
    /// Protocols with an allow rule.
    pub allow: Vec<String>,
    /// Names of block rules (any protocol).
    pub block: Vec<String>,
}

impl ProgramRules {
    /// The transport is QUIC over UDP: an allow rule must cover UDP.
    pub fn allows_udp(&self) -> bool {
        self.allow.iter().any(|p| p == "UDP" || p == "any")
    }
}

/// Collects the enabled inbound rules that name `program`.
pub fn rules_for(rules: &[FwRule], program: &Path) -> ProgramRules {
    let mut out = ProgramRules { program: program.display().to_string(), ..Default::default() };
    for r in rules.iter().filter(|r| r.enabled && r.program.as_deref().is_some_and(|p| same_path(p, program))) {
        if r.allow {
            if !out.allow.contains(&r.protocol) {
                out.allow.push(r.protocol.clone());
            }
        } else if !out.block.contains(&r.name) {
            out.block.push(r.name.clone());
        }
    }
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
    let uid = sys.process(pid).and_then(|p| p.user_id()).context("cannot read the current user's account id")?;
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
pub fn install_plan(profile: &Profile) -> Result<Plan> {
    #[cfg(windows)]
    {
        windows::install_plan(profile)
    }
    #[cfg(target_os = "macos")]
    {
        macos::install_plan(profile)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = profile;
        bail!("autostart is available on Windows and macOS only")
    }
}

/// The plan for `ai-remote host uninstall`.
pub fn uninstall_plan(profile: &Profile) -> Result<Plan> {
    #[cfg(windows)]
    {
        windows::uninstall_plan(profile)
    }
    #[cfg(target_os = "macos")]
    {
        macos::uninstall_plan(profile)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = profile;
        bail!("autostart is available on Windows and macOS only")
    }
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
        FwRule { name: name.into(), program: Some(program.into()), allow, enabled, protocol: protocol.into() }
    }

    #[test]
    fn collects_rules_of_one_program() {
        let exe = if cfg!(windows) { r"C:\Tools\ai-remote-hostw.exe" } else { "/opt/ai-remote-hostw" };
        let shouted = if cfg!(windows) { r"c:/TOOLS/AI-REMOTE-HOSTW.EXE" } else { "/opt/ai-remote-hostw" };
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
    fn tcp_only_allow_does_not_cover_quic() {
        let r = ProgramRules { program: "x".into(), allow: vec!["TCP".into()], block: vec![] };
        assert!(!r.allows_udp());
        let any = ProgramRules { program: "x".into(), allow: vec!["any".into()], block: vec![] };
        assert!(any.allows_udp());
    }

    #[test]
    fn describes_commands_readably() {
        let a = Action::run(PathBuf::from("C:/Windows/System32/netsh.exe"), &["advfirewall".into()], "add a rule");
        assert_eq!(a.describe(), "add a rule:\n    C:/Windows/System32/netsh.exe advfirewall");
    }
}
