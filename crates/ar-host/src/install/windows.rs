//! Windows autostart: a Task Scheduler task (registered with schtasks.exe from an XML definition)
//! and inbound firewall rules (written with netsh.exe, read through the firewall's COM API).
//!
//! The two halves need different rights. The task belongs to the user it starts the host for: a
//! standard user registers their own without elevation, an administrator needs an elevated
//! terminal for "highest privileges". The firewall rules are machine-wide and per program: only an
//! administrator writes them, from any account. So a plan tags its firewall steps, and a standard
//! user's install does the task part and leaves `--firewall-only` to an administrator.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use ar_core::{Profile, Role};

use super::{
    Action, Binaries, FIREWALL_RULE, FwRule, InstallStatus, Plan, Scope, current_user_id, quote_arg, rules_for, task,
};

/// The Task Scheduler folder every profile's task lives in.
const TASK_FOLDER: &str = r"\ai-remote\";

/// `\ai-remote\host-<id>`: one task per profile, grouped in an `ai-remote` folder.
pub fn task_name(profile: &Profile) -> String {
    format!(r"ai-remote\host-{}", &profile.instance_id()[..8])
}

/// A system tool by absolute path: `PATH` on many machines has look-alikes in front.
fn system32(name: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    root.join("System32").join(name)
}

fn schtasks() -> PathBuf {
    system32("schtasks.exe")
}

fn netsh() -> PathBuf {
    system32("netsh.exe")
}

fn task_xml_path(profile: &Profile) -> Result<PathBuf> {
    Ok(profile.role_dir(Role::Host)?.join("autostart-task.xml"))
}

/// The arguments the task passes to the background host.
fn host_arguments(profile: &Profile) -> String {
    format!("--home {}", quote_arg(&profile.root().to_string_lossy()))
}

/// Runs schtasks.exe to read something (not a plan step).
fn schtasks_read(args: &[&str]) -> std::io::Result<std::process::Output> {
    let mut cmd = std::process::Command::new(schtasks());
    cmd.args(args).stdin(std::process::Stdio::null());
    crate::platform::no_console_window(&mut cmd);
    cmd.output()
}

fn query_task_xml(name: &str) -> std::io::Result<std::process::Output> {
    schtasks_read(&["/Query", "/TN", name, "/XML"])
}

/// The ai-remote host tasks in a `schtasks /Query /FO CSV /NH` listing (the first column holds the
/// full task path, quoted).
fn host_tasks_in(listing: &str) -> Vec<String> {
    let mut names: Vec<String> = listing
        .lines()
        .filter_map(|line| line.trim().strip_prefix('"')?.split('"').next())
        // Other tasks' names may be anything (non-ASCII too): compare bytes, never slice text.
        .filter(|name| {
            name.len() > TASK_FOLDER.len() && name.as_bytes()[..TASK_FOLDER.len()].eq_ignore_ascii_case(TASK_FOLDER.as_bytes())
        })
        .map(str::to_string)
        .collect();
    names.dedup();
    names
}

/// Other profiles' logon tasks that start this installation's background host: they rely on the
/// same program-scoped firewall rules.
fn other_tasks_using(hostw: &Path, own: &str) -> Result<Vec<String>> {
    let out = schtasks_read(&["/Query", "/FO", "CSV", "/NH"]).map_err(|e| anyhow!("cannot run schtasks.exe: {e}"))?;
    if !out.status.success() {
        bail!("schtasks.exe could not list the tasks: {}", super::tool_output(&out));
    }
    let own = format!(r"\{own}");
    let mut users = Vec::new();
    for name in host_tasks_in(&crate::platform::decode_console_output(&out.stdout)) {
        if name.eq_ignore_ascii_case(&own) {
            continue;
        }
        let out = query_task_xml(&name).map_err(|e| anyhow!("cannot run schtasks.exe: {e}"))?;
        let xml = crate::platform::decode_console_output(&out.stdout);
        if out.status.success() && task::read_exec(&xml).is_some_and(|(command, _)| super::same_path(&command, hostw)) {
            users.push(name);
        }
    }
    Ok(users)
}

/// The accounts that own the desktop shells (explorer.exe) running now.
fn desktop_owners() -> Vec<String> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        false,
        ProcessRefreshKind::nothing().with_user(UpdateKind::Always),
    );
    sys.processes()
        .values()
        .filter(|p| p.name().eq_ignore_ascii_case("explorer.exe"))
        .filter_map(|p| p.user_id().map(|u| u.to_string()))
        .collect()
}

/// Whether the desktop belongs to someone else than `sid`, as happens when an administrator's
/// credentials elevate a standard user's terminal.
fn desktop_owned_by_someone_else(sid: &str, owners: &[String]) -> bool {
    !owners.is_empty() && !owners.iter().any(|s| s == sid)
}

fn netsh_delete_own(program: &Path) -> Vec<String> {
    vec![
        "advfirewall".into(),
        "firewall".into(),
        "delete".into(),
        "rule".into(),
        format!("name=\"{FIREWALL_RULE}\""),
        format!("program=\"{}\"", program.display()),
    ]
}

fn netsh_delete_named(name: &str, program: &Path) -> Vec<String> {
    vec![
        "advfirewall".into(),
        "firewall".into(),
        "delete".into(),
        "rule".into(),
        format!("name=\"{name}\""),
        "dir=in".into(),
        format!("program=\"{}\"", program.display()),
    ]
}

fn netsh_allow(program: &Path, protocol: &str) -> Vec<String> {
    vec![
        "advfirewall".into(),
        "firewall".into(),
        "add".into(),
        "rule".into(),
        format!("name=\"{FIREWALL_RULE}\""),
        "dir=in".into(),
        "action=allow".into(),
        format!("program=\"{}\"", program.display()),
        format!("protocol={protocol}"),
        "profile=any".into(),
        "enable=yes".into(),
        "description=\"Peer-to-peer connections from paired ai-remote controllers\"".into(),
    ]
}

/// The firewall half of an install: drop stale block rules, then allow both host programs.
fn firewall_actions(bins: &Binaries, notes: &mut Vec<String>) -> Vec<Action> {
    // Stale block rules, typically left by an unanswered "allow access?" prompt.
    let rules = match firewall_rules() {
        Ok(r) => r,
        Err(e) => {
            notes.push(format!("Could not read the current firewall rules ({e:#}); stale block rules are not removed."));
            Vec::new()
        }
    };
    let mut actions = Vec::new();
    for program in bins.programs() {
        actions.push(Action::run_tolerant(netsh(), &netsh_delete_own(program), "remove earlier ai-remote rules").admin());
        let found = rules_for(&rules, program);
        for block in found.block.iter().filter(|n| n.as_str() != FIREWALL_RULE) {
            if block.contains('"') {
                notes.push(format!("A block rule named {block} applies to {}; remove it by hand.", program.display()));
                continue;
            }
            notes.push(format!("Removes the firewall block rule \"{block}\" for {}.", program.display()));
            actions.push(
                Action::run_tolerant(netsh(), &netsh_delete_named(block, program), "remove a stale block rule").admin(),
            );
        }
        for protocol in ["UDP", "TCP"] {
            actions.push(Action::run(netsh(), &netsh_allow(program, protocol), "allow inbound connections").admin());
        }
    }
    if let Some(rule) = rules_for(&rules, &bins.hostw).block_all.first() {
        notes.push(format!(
            "The block rule \"{rule}\" refuses inbound UDP for every program and beats these allow rules; it is \
             not changed here (review it in wf.msc)."
        ));
    }
    actions
}

pub fn install_plan(profile: &Profile, scope: Scope) -> Result<Plan> {
    let bins = Binaries::locate()?;
    let mut notes = Vec::new();
    if scope == Scope::FirewallOnly {
        let actions = firewall_actions(&bins, &mut notes);
        notes.insert(
            0,
            format!(
                "Firewall rules are machine-wide and per program: they cover {} and {} for every account and profile.",
                bins.hostw.display(),
                bins.cli.display()
            ),
        );
        return Ok(Plan {
            summary: format!("Add the inbound firewall rules for the ai-remote host programs in {}", dir_of(&bins)),
            notes,
            actions,
            needs_elevation: true,
            blocker: None,
        });
    }

    bins.require_hostw()?;
    let name = task_name(profile);
    // The SID, not the name: it survives renames and needs no domain prefix.
    let sid = current_user_id()?;
    let user = std::env::var("USERNAME").unwrap_or_else(|_| sid.clone());
    let work_dir = bins.hostw.parent().map(Path::to_path_buf).unwrap_or_default();
    let arguments = host_arguments(profile);
    let description = format!(
        "Starts the ai-remote host for {user} at logon (profile {}), so paired controllers can operate this machine.",
        profile.root().display()
    );
    let xml = task::task_xml(&task::TaskSpec {
        user_id: &sid,
        description: &description,
        command: &bins.hostw,
        arguments: &arguments,
        working_dir: &work_dir,
    });
    let xml_path = task_xml_path(profile)?;

    notes.push(format!(
        "The task \\{name} starts {} {arguments} when {user} logs on, with highest privileges in the interactive \
         desktop session.",
        bins.hostw.display()
    ));
    let blocker = desktop_owned_by_someone_else(&sid, &desktop_owners()).then(|| {
        format!(
            "This terminal runs as {user}, but the desktop belongs to another account: the task would start the host \
             only when {user} logs on, with {user}'s profile. If a standard user's terminal was elevated with an \
             administrator's credentials, run `ai-remote host install` in that user's own terminal without elevation \
             instead (it registers the task for them), and have an administrator add the firewall rules from an \
             elevated terminal with `ai-remote host install --firewall-only`. To install for {user} anyway, add \
             --force."
        )
    });

    let mut actions = vec![
        Action::WriteFile { path: xml_path.clone(), contents: task::utf16_file(&xml), preview: xml },
        Action::run(
            schtasks(),
            &["/Create".into(), "/TN".into(), quote_arg(&name), "/XML".into(), quote_arg(&xml_path.to_string_lossy()), "/F".into()],
            "register the logon task",
        ),
    ];
    actions.extend(firewall_actions(&bins, &mut notes));
    Ok(Plan {
        summary: format!("Install autostart for the ai-remote host (profile {})", profile.root().display()),
        notes,
        actions,
        needs_elevation: true,
        blocker,
    })
}

pub fn uninstall_plan(profile: &Profile, scope: Scope) -> Result<Plan> {
    let bins = Binaries::locate()?;
    let name = task_name(profile);
    let mut notes = Vec::new();
    let mut actions = Vec::new();
    if scope == Scope::All {
        actions.push(Action::run_tolerant(
            schtasks(),
            &["/End".into(), "/TN".into(), quote_arg(&name)],
            "stop the task's running instance",
        ));
        actions.push(Action::run_tolerant(
            schtasks(),
            &["/Delete".into(), "/TN".into(), quote_arg(&name), "/F".into()],
            "delete the logon task",
        ));
    }
    // The rules are per program, not per profile: another profile's task that starts the same
    // background host still needs them.
    match other_tasks_using(&bins.hostw, &name) {
        Ok(others) if others.is_empty() => {
            for program in bins.programs() {
                actions.push(
                    Action::run_tolerant(netsh(), &netsh_delete_own(program), "remove the firewall rules").admin(),
                );
            }
        }
        Ok(others) => notes.push(format!(
            "Keeps the firewall rules: {} also start{} {} and need{} them.",
            others.join(", "),
            if others.len() == 1 { "s" } else { "" },
            bins.hostw.display(),
            if others.len() == 1 { "s" } else { "" },
        )),
        Err(e) => notes.push(format!(
            "Keeps the firewall rules: cannot tell whether other profiles' tasks still need them ({e:#}). If none \
             does, an administrator can remove them with: netsh advfirewall firewall delete rule name=\"{FIREWALL_RULE}\""
        )),
    }
    if scope == Scope::All {
        actions.push(Action::RemoveFile { path: task_xml_path(profile)? });
    }
    let summary = match scope {
        Scope::All => format!("Remove autostart for the ai-remote host (profile {})", profile.root().display()),
        Scope::FirewallOnly => format!("Remove the firewall rules for the ai-remote host programs in {}", dir_of(&bins)),
    };
    Ok(Plan { summary, notes, actions, needs_elevation: true, blocker: None })
}

fn dir_of(bins: &Binaries) -> String {
    bins.hostw.parent().map(|d| d.display().to_string()).unwrap_or_default()
}

pub fn start_action(profile: &Profile) -> Action {
    Action::run(schtasks(), &["/Run".into(), "/TN".into(), quote_arg(&task_name(profile))], "start the task now")
}

pub fn status(profile: &Profile) -> Result<InstallStatus> {
    let bins = Binaries::locate()?;
    let name = task_name(profile);
    let mut st = InstallStatus { kind: "Task Scheduler task".into(), name: format!(r"\{name}"), ..Default::default() };

    match query_task_xml(&name) {
        Ok(out) if out.status.success() => {
            st.installed = true;
            let xml = crate::platform::decode_console_output(&out.stdout);
            match task::read_exec(&xml) {
                Some((command, arguments)) => {
                    let expected = host_arguments(profile);
                    st.current = Some(
                        super::same_path(&command, &bins.hostw) && arguments.trim().to_lowercase() == expected.to_lowercase(),
                    );
                    st.launches = Some(format!("{} {arguments}", quote_arg(&command)));
                }
                None => st.detail = Some("the task has no program to run".into()),
            }
        }
        // schtasks has no distinct exit code for "no such task"; keep its own words.
        Ok(out) => st.detail = Some(super::tool_output(&out)),
        Err(e) => st.detail = Some(format!("cannot run schtasks.exe: {e}")),
    }

    match firewall_rules() {
        Ok(rules) => st.firewall = bins.programs().iter().map(|p| rules_for(&rules, p)).collect(),
        Err(e) => st.firewall_error = Some(format!("{e:#}")),
    }
    st.firewall_enabled = firewall_enabled().ok();
    Ok(st)
}

/// Whether Windows Firewall is on for any network profile.
fn firewall_enabled() -> Result<bool> {
    use windows_firewall::{Profile, get_firewall_state};
    std::thread::spawn(|| {
        for profile in [Profile::Domain, Profile::Private, Profile::Public] {
            if get_firewall_state(profile).map_err(|e| anyhow!("read firewall state: {e}"))? {
                return Ok(true);
            }
        }
        Ok(false)
    })
    .join()
    .map_err(|_| anyhow!("reading the firewall state panicked"))?
}

/// Inbound rules, read through the firewall's COM API (no elevation needed). Runs on a thread of
/// its own: the API wants a fresh multithreaded COM apartment, which a reused worker thread may
/// not offer.
pub fn firewall_rules() -> Result<Vec<FwRule>> {
    use windows_firewall::{Action as FwAction, Port, Protocol};
    let rules = std::thread::spawn(windows_firewall::list_incoming_rules)
        .join()
        .map_err(|_| anyhow!("reading the firewall rules panicked"))?
        .map_err(|e| anyhow!("read firewall rules: {e}"))?;
    // The API reports "any" as nothing, an empty string or `*`, depending on how the rule was made.
    let specific = |v: &Option<String>| v.clone().filter(|s| !s.trim().is_empty() && s.trim() != "*");
    Ok(rules
        .into_iter()
        .map(|r| FwRule {
            name: r.name().clone(),
            program: specific(r.application_name()),
            allow: *r.action() == FwAction::Allow,
            enabled: *r.enabled(),
            protocol: match r.protocol() {
                None | Some(Protocol::Any) => "any".into(),
                Some(Protocol::Tcp) => "TCP".into(),
                Some(Protocol::Udp) => "UDP".into(),
                Some(other) => format!("{other:?}"),
            },
            service: specific(r.service_name()),
            all_local_ports: r.local_ports().as_ref().is_none_or(|p| p.is_empty() || p.contains(&Port::Any)),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netsh_arguments_quote_names_and_programs() {
        let exe = Path::new(r"C:\Program Files\ai-remote\ai-remote-hostw.exe");
        let add = netsh_allow(exe, "UDP").join(" ");
        assert!(add.starts_with("advfirewall firewall add rule name=\"ai-remote host\" dir=in action=allow"));
        assert!(add.contains(r#"program="C:\Program Files\ai-remote\ai-remote-hostw.exe" protocol=UDP"#));
        let del = netsh_delete_named("ai-remote-hostw", exe).join(" ");
        assert_eq!(
            del,
            r#"advfirewall firewall delete rule name="ai-remote-hostw" dir=in program="C:\Program Files\ai-remote\ai-remote-hostw.exe""#
        );
    }

    #[test]
    fn task_name_is_per_profile() {
        let dir = std::env::temp_dir().join(format!("ar-task-name-{}", std::process::id()));
        let profile = Profile::at(&dir).unwrap();
        let name = task_name(&profile);
        assert!(name.starts_with(r"ai-remote\host-") && name.len() == r"ai-remote\host-".len() + 8, "{name}");
        assert_eq!(host_arguments(&profile), format!("--home \"{}\"", profile.root().display()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn finds_host_tasks_in_a_listing() {
        let listing = "\r\n\"\\Other\",\"N/A\",\"Ready\"\r\n\"\\ai-remote\\host-1a2b3c4d\",\"N/A\",\"Running\"\r\n\
                       \"\\AI-REMOTE\\host-99999999\",\"N/A\",\"Ready\"\r\n\"\\ai-remote-like\",\"N/A\",\"Ready\"\r\n\
                       \"\\\u{8ba1}\u{5212}\u{4efb}\u{52a1}\u{66f4}\u{65b0}\",\"N/A\",\"Ready\"\r\nINFO: something\r\n";
        assert_eq!(host_tasks_in(listing), vec![r"\ai-remote\host-1a2b3c4d".to_string(), r"\AI-REMOTE\host-99999999".to_string()]);
    }

    #[test]
    fn a_desktop_without_our_account_is_someone_elses() {
        let mine = "S-1-5-21-1-2-3-1001".to_string();
        let theirs = "S-1-5-21-1-2-3-1002".to_string();
        assert!(desktop_owned_by_someone_else(&mine, std::slice::from_ref(&theirs)));
        assert!(!desktop_owned_by_someone_else(&mine, &[theirs, mine.clone()]));
        // Nobody logged on (e.g. installed over SSH before anyone logs in): nothing to compare.
        assert!(!desktop_owned_by_someone_else(&mine, &[]));
    }

    #[test]
    fn firewall_steps_are_tagged_for_an_administrator() {
        let bins = Binaries { cli: PathBuf::from(r"C:\x\ai-remote.exe"), hostw: PathBuf::from(r"C:\x\ai-remote-hostw.exe") };
        let mut notes = Vec::new();
        let actions = firewall_actions(&bins, &mut notes);
        assert!(!actions.is_empty());
        assert!(actions.iter().all(Action::needs_admin));
    }
}
