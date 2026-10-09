//! Windows autostart: a Task Scheduler task (registered with schtasks.exe from an XML definition)
//! and inbound firewall rules (written with netsh.exe, read through the firewall's COM API).

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use ar_core::{Profile, Role};

use super::{Action, Binaries, FIREWALL_RULE, FwRule, InstallStatus, Plan, current_user_id, quote_arg, rules_for, task};

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

/// Whether the desktop belongs to someone else than this process's account, as happens when an
/// administrator's credentials elevate a standard user's terminal: the task would then start at
/// the administrator's logon, not at the console user's.
fn desktop_owned_by_someone_else(sid: &str) -> bool {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        false,
        ProcessRefreshKind::nothing().with_user(UpdateKind::Always),
    );
    let shells: Vec<String> = sys
        .processes()
        .values()
        .filter(|p| p.name().eq_ignore_ascii_case("explorer.exe"))
        .filter_map(|p| p.user_id().map(|u| u.to_string()))
        .collect();
    !shells.is_empty() && !shells.iter().any(|s| s == sid)
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

pub fn install_plan(profile: &Profile) -> Result<Plan> {
    let bins = Binaries::locate()?;
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

    let mut notes = vec![format!(
        "The task \\{name} starts {} {arguments} when {user} logs on, with highest privileges in the \
         interactive desktop session.",
        bins.hostw.display()
    )];
    if desktop_owned_by_someone_else(&sid) {
        notes.push(format!(
            "This terminal runs as another account than the desktop's user, so the host would start only when \
             {user} logs on. Install from a terminal of the user who sits at this machine."
        ));
    }

    let mut actions = vec![
        Action::WriteFile { path: xml_path.clone(), contents: task::utf16_file(&xml), preview: xml },
        Action::run(
            schtasks(),
            &["/Create".into(), "/TN".into(), quote_arg(&name), "/XML".into(), quote_arg(&xml_path.to_string_lossy()), "/F".into()],
            "register the logon task",
        ),
    ];

    // Stale block rules, typically left by an unanswered "allow access?" prompt.
    let rules = match firewall_rules() {
        Ok(r) => r,
        Err(e) => {
            notes.push(format!("Could not read the current firewall rules ({e:#}); stale block rules are not removed."));
            Vec::new()
        }
    };
    for program in bins.programs() {
        actions.push(Action::run_tolerant(netsh(), &netsh_delete_own(program), "remove earlier ai-remote rules"));
        for block in rules_for(&rules, program).block.iter().filter(|n| n.as_str() != FIREWALL_RULE) {
            if block.contains('"') {
                notes.push(format!("A block rule named {block} applies to {}; remove it by hand.", program.display()));
                continue;
            }
            notes.push(format!("Removes the firewall block rule \"{block}\" for {}.", program.display()));
            actions.push(Action::run_tolerant(netsh(), &netsh_delete_named(block, program), "remove a stale block rule"));
        }
        for protocol in ["UDP", "TCP"] {
            actions.push(Action::run(netsh(), &netsh_allow(program, protocol), "allow inbound connections"));
        }
    }
    Ok(Plan {
        summary: format!("Install autostart for the ai-remote host (profile {})", profile.root().display()),
        notes,
        actions,
        needs_elevation: true,
    })
}

pub fn uninstall_plan(profile: &Profile) -> Result<Plan> {
    let bins = Binaries::locate()?;
    let name = task_name(profile);
    let mut actions = vec![
        Action::run_tolerant(schtasks(), &["/End".into(), "/TN".into(), quote_arg(&name)], "stop the task's running instance"),
        Action::run_tolerant(schtasks(), &["/Delete".into(), "/TN".into(), quote_arg(&name), "/F".into()], "delete the logon task"),
    ];
    for program in bins.programs() {
        actions.push(Action::run_tolerant(netsh(), &netsh_delete_own(program), "remove the firewall rules"));
    }
    actions.push(Action::RemoveFile { path: task_xml_path(profile)? });
    Ok(Plan {
        summary: format!("Remove autostart for the ai-remote host (profile {})", profile.root().display()),
        notes: vec![
            "Firewall rules belong to the programs, not the profile: other profiles of this installation lose them too."
                .into(),
        ],
        actions,
        needs_elevation: true,
    })
}

pub fn start_action(profile: &Profile) -> Action {
    Action::run(schtasks(), &["/Run".into(), "/TN".into(), quote_arg(&task_name(profile))], "start the task now")
}

pub fn status(profile: &Profile) -> Result<InstallStatus> {
    let bins = Binaries::locate()?;
    let name = task_name(profile);
    let mut st = InstallStatus { kind: "Task Scheduler task".into(), name: format!(r"\{name}"), ..Default::default() };

    let out = {
        use std::os::windows::process::CommandExt;
        // The background host has no console; without this flag every query would flash one.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new(schtasks())
            .args(["/Query", "/TN", &name, "/XML"])
            .stdin(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    };
    match out {
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
    use windows_firewall::{Action as FwAction, Protocol};
    let rules = std::thread::spawn(windows_firewall::list_incoming_rules)
        .join()
        .map_err(|_| anyhow!("reading the firewall rules panicked"))?
        .map_err(|e| anyhow!("read firewall rules: {e}"))?;
    Ok(rules
        .into_iter()
        .map(|r| FwRule {
            name: r.name().clone(),
            program: r.application_name().clone(),
            allow: *r.action() == FwAction::Allow,
            enabled: *r.enabled(),
            protocol: match r.protocol() {
                None | Some(Protocol::Any) => "any".into(),
                Some(Protocol::Tcp) => "TCP".into(),
                Some(Protocol::Udp) => "UDP".into(),
                Some(other) => format!("{other:?}"),
            },
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
}
