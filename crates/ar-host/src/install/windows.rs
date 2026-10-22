//! Windows autostart: a Task Scheduler task (registered with schtasks.exe from an XML definition)
//! or, with `--service`, a Windows service (registered with sc.exe), plus inbound firewall rules
//! (written with netsh.exe, read through the firewall's COM API).
//!
//! The halves need different rights. The task belongs to the user it starts the host for: a
//! standard user registers their own without elevation, an administrator needs an elevated
//! terminal for "highest privileges". The firewall rules are machine-wide and per program: only an
//! administrator writes them, from any account. So a plan tags its firewall steps, and a standard
//! user's install does the task part and leaves `--firewall-only` to an administrator. The service
//! is machine-wide too (it runs as LocalSystem and sets the `SoftwareSASGeneration` policy so it
//! can send Ctrl+Alt+Del): every one of its steps needs an administrator.

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

fn sc() -> PathBuf {
    system32("sc.exe")
}

fn reg() -> PathBuf {
    system32("reg.exe")
}

/// The service that runs the host as SYSTEM in the console session (see `crate::service`).
pub fn service_name(profile: &Profile) -> String {
    crate::service::service_name(profile)
}

/// The command line the service runs (`sc.exe`'s `binPath=`).
fn service_command(bins: &Binaries, profile: &Profile, owner_sid: &str) -> String {
    format!(
        "{} --service --home {} --owner-sid {owner_sid}",
        quote_arg(&bins.hostw.to_string_lossy()),
        quote_arg(&profile.root().to_string_lossy())
    )
}

/// Runs sc.exe to read something (not a plan step).
fn sc_read(args: &[&str]) -> std::io::Result<std::process::Output> {
    let mut cmd = std::process::Command::new(sc());
    cmd.args(args).stdin(std::process::Stdio::null());
    crate::platform::no_console_window(&mut cmd);
    cmd.output()
}

/// `sc qc <name>`'s output when the service exists.
fn query_service_config(name: &str) -> Option<String> {
    let out = sc_read(&["qc", name]).ok()?;
    out.status.success().then(|| crate::platform::decode_console_output(&out.stdout))
}

/// Whether `sc query <name>` reports the service running.
fn service_running(name: &str) -> Option<bool> {
    let out = sc_read(&["query", name]).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = crate::platform::decode_console_output(&out.stdout);
    Some(text.lines().any(|l| l.contains("STATE") && l.contains("RUNNING")))
}

/// The `BINARY_PATH_NAME` of an `sc qc` listing.
fn service_binary_path(qc: &str) -> Option<String> {
    qc.lines().find_map(|l| {
        l.trim().strip_prefix("BINARY_PATH_NAME").map(|rest| rest.trim_start_matches([' ', ':']).trim().to_string())
    })
}

/// The ai-remote host services in an `sc query type= service state= all` listing.
fn host_services_in(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| l.trim().strip_prefix("SERVICE_NAME:").map(str::trim))
        .filter(|n| n.len() > "ai-remote-host-".len() && n[..15].eq_ignore_ascii_case("ai-remote-host-"))
        .map(str::to_string)
        .collect()
}

/// Other profiles' services that start this installation's background host: they rely on the same
/// program-scoped firewall rules.
fn other_services_using(hostw: &Path, own: &str) -> Result<Vec<String>> {
    let out =
        sc_read(&["query", "type=", "service", "state=", "all"]).map_err(|e| anyhow!("cannot run sc.exe: {e}"))?;
    if !out.status.success() {
        bail!("sc.exe could not list the services: {}", super::tool_output(&out));
    }
    let mut users = Vec::new();
    for name in host_services_in(&crate::platform::decode_console_output(&out.stdout)) {
        if name.eq_ignore_ascii_case(own) {
            continue;
        }
        let Some(qc) = query_service_config(&name) else { continue };
        if service_binary_path(&qc).is_some_and(|p| command_names(&p, hostw)) {
            users.push(name);
        }
    }
    Ok(users)
}

/// Whether a service command line starts `program` (quoted or bare).
fn command_names(command: &str, program: &Path) -> bool {
    let first = command.trim();
    let exe = if let Some(rest) = first.strip_prefix('"') {
        rest.split('"').next().unwrap_or("")
    } else {
        first.split(' ').next().unwrap_or("")
    };
    super::same_path(exe, program)
}

/// Policy that lets a service simulate the secure attention sequence (Ctrl+Alt+Del): 1 =
/// services, 3 = services and Ease of Access; absent or 0 = nobody.
const SAS_POLICY_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
const SAS_POLICY_VALUE: &str = "SoftwareSASGeneration";

/// Where an install remembers the policy value it found, so the uninstall can put it back.
fn sas_marker_path(profile: &Profile) -> Result<PathBuf> {
    Ok(profile.role_dir(Role::Host)?.join("sas-policy-before.txt"))
}

/// The current `SoftwareSASGeneration` value; `None` when absent.
fn sas_policy() -> Result<Option<u32>> {
    let mut cmd = std::process::Command::new(reg());
    cmd.args(["query", SAS_POLICY_KEY, "/v", SAS_POLICY_VALUE]).stdin(std::process::Stdio::null());
    crate::platform::no_console_window(&mut cmd);
    let out = cmd.output().map_err(|e| anyhow!("cannot run reg.exe: {e}"))?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(parse_reg_dword(&crate::platform::decode_console_output(&out.stdout)))
}

/// The value of the first `REG_DWORD` line in a `reg query` listing (`0x1`).
fn parse_reg_dword(text: &str) -> Option<u32> {
    let line = text.lines().find(|l| l.contains("REG_DWORD"))?;
    let raw = line.split_whitespace().last()?;
    let hex = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X"))?;
    u32::from_str_radix(hex, 16).ok()
}

fn reg_set_sas(value: u32) -> Vec<String> {
    vec![
        "add".into(),
        quote_arg(SAS_POLICY_KEY),
        "/v".into(),
        SAS_POLICY_VALUE.into(),
        "/t".into(),
        "REG_DWORD".into(),
        "/d".into(),
        value.to_string(),
        "/f".into(),
    ]
}

fn reg_delete_sas() -> Vec<String> {
    vec!["delete".into(), quote_arg(SAS_POLICY_KEY), "/v".into(), SAS_POLICY_VALUE.into(), "/f".into()]
}

/// The steps that register (or update) the service and let it send Ctrl+Alt+Del.
fn service_actions(
    profile: &Profile,
    bins: &Binaries,
    owner_sid: &str,
    user: &str,
    notes: &mut Vec<String>,
) -> Result<Vec<Action>> {
    let name = service_name(profile);
    let command = service_command(bins, profile, owner_sid);
    let display = format!("ai-remote host ({})", &profile.instance_id()[..8]);
    let description = format!(
        "Keeps the ai-remote host running as SYSTEM in the console session for {user} (profile {}), so paired \
         controllers can operate this machine, its lock screen and UAC prompts included.",
        profile.root().display()
    );
    let mut actions = Vec::new();
    // The logon task would start a second, user-level host that finds the profile taken: retire it.
    let task = task_name(profile);
    if query_task_xml(&task).is_ok_and(|out| out.status.success()) {
        notes.push(format!("Replaces the logon task \\{task} with the service."));
        actions.push(Action::run_tolerant(
            schtasks(),
            &["/End".into(), "/TN".into(), quote_arg(&task)],
            "stop the logon task's running instance",
        ));
        actions.push(Action::run_tolerant(
            schtasks(),
            &["/Delete".into(), "/TN".into(), quote_arg(&task), "/F".into()],
            "delete the logon task",
        ));
        actions.push(Action::RemoveFile { path: task_xml_path(profile)? });
    }
    let settings = |verb: &str| -> Vec<String> {
        vec![
            verb.into(),
            name.clone(),
            "binPath=".into(),
            quote_arg(&command),
            "start=".into(),
            "auto".into(),
            "obj=".into(),
            "LocalSystem".into(),
            "DisplayName=".into(),
            quote_arg(&display),
        ]
    };
    if query_service_config(&name).is_some() {
        actions.push(Action::run_tolerant(sc(), &["stop".into(), name.clone()], "stop the running service").admin());
        actions.push(Action::run(sc(), &settings("config"), "update the service").admin());
    } else {
        actions.push(Action::run(sc(), &settings("create"), "register the service").admin());
    }
    actions.push(
        Action::run(sc(), &["description".into(), name.clone(), quote_arg(&description)], "describe the service")
            .admin(),
    );
    actions.push(
        Action::run(
            sc(),
            &[
                "failure".into(),
                name.clone(),
                "reset=".into(),
                "600".into(),
                "actions=".into(),
                "restart/5000/restart/10000/restart/60000".into(),
            ],
            "restart the service after a crash",
        )
        .admin(),
    );
    match sas_policy() {
        Ok(Some(1 | 3)) => notes.push(
            "The SoftwareSASGeneration policy already lets services send Ctrl+Alt+Del; it is left as it is.".into(),
        ),
        Ok(before) => {
            let remembered = before.map(|v| v.to_string()).unwrap_or_else(|| "absent".into());
            notes.push(format!(
                "Sets the SoftwareSASGeneration policy (HKLM\\...\\Policies\\System) to 1 so the service can send \
                 Ctrl+Alt+Del for the host; it was {remembered}, which `host uninstall` puts back."
            ));
            actions.push(Action::WriteFile {
                path: sas_marker_path(profile)?,
                contents: format!("{remembered}\n").into_bytes(),
                preview: remembered,
            });
            actions.push(Action::run(reg(), &reg_set_sas(1), "let services send Ctrl+Alt+Del").admin());
        }
        Err(e) => notes.push(format!(
            "Could not read the SoftwareSASGeneration policy ({e:#}); it is not changed, so Ctrl+Alt+Del may not work \
             until an administrator sets it to 1 (services) in {SAS_POLICY_KEY}."
        )),
    }
    Ok(actions)
}

/// Undoes [`service_actions`]: stops and deletes the service, restores the policy it changed.
fn service_removal_actions(profile: &Profile, notes: &mut Vec<String>) -> Result<Vec<Action>> {
    let name = service_name(profile);
    let mut actions = Vec::new();
    if query_service_config(&name).is_some() {
        actions.push(Action::run_tolerant(sc(), &["stop".into(), name.clone()], "stop the service").admin());
        actions.push(Action::run_tolerant(sc(), &["delete".into(), name.clone()], "delete the service").admin());
    }
    let marker = sas_marker_path(profile)?;
    if let Ok(before) = std::fs::read_to_string(&marker) {
        let before = before.trim();
        match before.parse::<u32>() {
            Ok(v) => {
                notes.push(format!("Puts the SoftwareSASGeneration policy back to {v}."));
                actions.push(Action::run_tolerant(reg(), &reg_set_sas(v), "restore the Ctrl+Alt+Del policy").admin());
            }
            Err(_) => {
                notes.push("Removes the SoftwareSASGeneration policy value the install added.".into());
                actions.push(Action::run_tolerant(reg(), &reg_delete_sas(), "remove the Ctrl+Alt+Del policy").admin());
            }
        }
        actions.push(Action::RemoveFile { path: marker });
    }
    Ok(actions)
}

/// The plan for `ai-remote host install --service`: the service instead of the logon task.
fn service_install_plan(profile: &Profile, bins: &Binaries) -> Result<Plan> {
    bins.require_hostw()?;
    let sid = current_user_id()?;
    let user = std::env::var("USERNAME").unwrap_or_else(|_| sid.clone());
    let name = service_name(profile);
    let mut notes = vec![
        format!(
            "The service {name} (LocalSystem, automatic start) runs {} and starts the host as SYSTEM inside the \
             console session: the lock screen, the sign-in screen after a reboot and UAC prompts can then be seen \
             and operated. It follows the console session across log-offs and user switches.",
            service_command(bins, profile, &sid)
        ),
        format!(
            "CONSEQUENCE: that host, and so its shell, jobs and files tools, run as SYSTEM. Levels, policies, \
             approvals and the audit log are unchanged, but what an admin-level (or bypassed) controller can do \
             grows from what {user} can do to what SYSTEM can do. Grant levels accordingly."
        ),
        format!(
            "The host's control endpoint admits {user} ({sid}) besides SYSTEM, so `ai-remote` commands and the \
             desktop app keep working for {user}; other accounts are refused as before."
        ),
        "Approval prompts cannot be shown on the lock screen: while it is locked, actions that need approval are \
         approved from approver devices only."
            .into(),
    ];
    let mut actions = service_actions(profile, bins, &sid, &user, &mut notes)?;
    actions.extend(firewall_actions(bins, &mut notes));
    Ok(Plan {
        summary: format!("Install the ai-remote host as a Windows service (profile {})", profile.root().display()),
        notes,
        actions,
        needs_elevation: true,
        blocker: None,
    })
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
            name.len() > TASK_FOLDER.len()
                && name.as_bytes()[..TASK_FOLDER.len()].eq_ignore_ascii_case(TASK_FOLDER.as_bytes())
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
            notes
                .push(format!("Could not read the current firewall rules ({e:#}); stale block rules are not removed."));
            Vec::new()
        }
    };
    let mut actions = Vec::new();
    for program in bins.programs() {
        actions
            .push(Action::run_tolerant(netsh(), &netsh_delete_own(program), "remove earlier ai-remote rules").admin());
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

pub fn install_plan(profile: &Profile, scope: Scope, service: bool) -> Result<Plan> {
    let bins = Binaries::locate()?;
    let mut notes = Vec::new();
    if service && scope != Scope::FirewallOnly {
        return service_install_plan(profile, &bins);
    }
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
            &[
                "/Create".into(),
                "/TN".into(),
                quote_arg(&name),
                "/XML".into(),
                quote_arg(&xml_path.to_string_lossy()),
                "/F".into(),
            ],
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
        actions.extend(service_removal_actions(profile, &mut notes)?);
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
    // The rules are per program, not per profile: another profile's task or service that starts
    // the same background host still needs them.
    let others = other_tasks_using(&bins.hostw, &name).and_then(|mut tasks| {
        tasks.extend(other_services_using(&bins.hostw, &service_name(profile))?);
        Ok(tasks)
    });
    match others {
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
        Scope::FirewallOnly => {
            format!("Remove the firewall rules for the ai-remote host programs in {}", dir_of(&bins))
        }
    };
    Ok(Plan { summary, notes, actions, needs_elevation: true, blocker: None })
}

fn dir_of(bins: &Binaries) -> String {
    bins.hostw.parent().map(|d| d.display().to_string()).unwrap_or_default()
}

pub fn start_action(profile: &Profile) -> Action {
    let service = service_name(profile);
    if query_service_config(&service).is_some() {
        return Action::run(sc(), &["start".into(), service], "start the service now").admin();
    }
    Action::run(schtasks(), &["/Run".into(), "/TN".into(), quote_arg(&task_name(profile))], "start the task now")
}

pub fn status(profile: &Profile) -> Result<InstallStatus> {
    let bins = Binaries::locate()?;
    let name = task_name(profile);
    let mut st = InstallStatus { kind: "Task Scheduler task".into(), name: format!(r"\{name}"), ..Default::default() };

    let service = service_name(profile);
    if let Some(qc) = query_service_config(&service) {
        st.kind = "Windows service".into();
        st.service = true;
        st.name = service.clone();
        st.installed = true;
        st.running = service_running(&service);
        match service_binary_path(&qc) {
            Some(command) => {
                let expected_home = quote_arg(&profile.root().to_string_lossy()).to_lowercase();
                st.current = Some(
                    command_names(&command, &bins.hostw)
                        && command.to_lowercase().contains("--service")
                        && command.to_lowercase().contains(&expected_home),
                );
                st.launches = Some(command);
            }
            None => st.detail = Some("the service has no program to run".into()),
        }
        match firewall_rules() {
            Ok(rules) => st.firewall = bins.programs().iter().map(|p| rules_for(&rules, p)).collect(),
            Err(e) => st.firewall_error = Some(format!("{e:#}")),
        }
        st.firewall_enabled = firewall_enabled().ok();
        return Ok(st);
    }

    match query_task_xml(&name) {
        Ok(out) if out.status.success() => {
            st.installed = true;
            let xml = crate::platform::decode_console_output(&out.stdout);
            match task::read_exec(&xml) {
                Some((command, arguments)) => {
                    let expected = host_arguments(profile);
                    st.current = Some(
                        super::same_path(&command, &bins.hostw)
                            && arguments.trim().to_lowercase() == expected.to_lowercase(),
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
        assert_eq!(
            host_tasks_in(listing),
            vec![r"\ai-remote\host-1a2b3c4d".to_string(), r"\AI-REMOTE\host-99999999".to_string()]
        );
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
    fn service_command_and_listing_parsers() {
        let dir = std::env::temp_dir().join(format!("ar-service-cmd-{}", std::process::id()));
        let profile = Profile::at(&dir).unwrap();
        let bins = Binaries {
            cli: PathBuf::from(r"C:\x y\ai-remote.exe"),
            hostw: PathBuf::from(r"C:\x y\ai-remote-hostw.exe"),
        };
        let cmd = service_command(&bins, &profile, "S-1-5-21-1-2-3-1001");
        assert!(cmd.starts_with(r#""C:\x y\ai-remote-hostw.exe" --service --home ""#), "{cmd}");
        assert!(cmd.ends_with("--owner-sid S-1-5-21-1-2-3-1001"), "{cmd}");
        assert!(command_names(&cmd, &bins.hostw));
        assert!(!command_names(&cmd, &bins.cli));
        assert!(command_names(r"C:\x\ai-remote-hostw.exe --service", Path::new(r"c:\X\AI-REMOTE-HOSTW.EXE")));
        let name = service_name(&profile);
        assert!(name.starts_with("ai-remote-host-") && name.len() == "ai-remote-host-".len() + 8);
        std::fs::remove_dir_all(&dir).unwrap();

        let qc = "[SC] QueryServiceConfig SUCCESS\r\n\r\nSERVICE_NAME: ai-remote-host-1a2b3c4d\r\n        TYPE               : 10  WIN32_OWN_PROCESS\r\n        BINARY_PATH_NAME   : \"C:\\x\\ai-remote-hostw.exe\" --service --home \"C:\\p\"\r\n        DISPLAY_NAME       : ai-remote host\r\n";
        assert_eq!(service_binary_path(qc).as_deref(), Some(r#""C:\x\ai-remote-hostw.exe" --service --home "C:\p""#));
        let listing = "\r\nSERVICE_NAME: ai-remote-host-1a2b3c4d\r\nDISPLAY_NAME: ai-remote host\r\n        STATE : 4  RUNNING\r\n\r\nSERVICE_NAME: AI-REMOTE-HOST-99999999\r\n\r\nSERVICE_NAME: ai-remote-hostx\r\nSERVICE_NAME: Spooler\r\n";
        assert_eq!(
            host_services_in(listing),
            vec!["ai-remote-host-1a2b3c4d".to_string(), "AI-REMOTE-HOST-99999999".to_string()]
        );
        assert_eq!(
            parse_reg_dword("\r\nHKEY_LOCAL_MACHINE\\...\r\n    SoftwareSASGeneration    REG_DWORD    0x3\r\n"),
            Some(3)
        );
        assert_eq!(parse_reg_dword("nothing"), None);
        assert_eq!(
            reg_set_sas(1).join(" "),
            format!(r#"add "{SAS_POLICY_KEY}" /v SoftwareSASGeneration /t REG_DWORD /d 1 /f"#)
        );
    }

    #[test]
    fn firewall_steps_are_tagged_for_an_administrator() {
        let bins =
            Binaries { cli: PathBuf::from(r"C:\x\ai-remote.exe"), hostw: PathBuf::from(r"C:\x\ai-remote-hostw.exe") };
        let mut notes = Vec::new();
        let actions = firewall_actions(&bins, &mut notes);
        assert!(!actions.is_empty());
        assert!(actions.iter().all(Action::needs_admin));
    }
}
