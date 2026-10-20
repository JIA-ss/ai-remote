//! macOS autostart: a per-user LaunchAgent in `~/Library/LaunchAgents`, loaded with launchctl; or,
//! with `--service`, a system LaunchAgent in `/Library/LaunchAgents` that loads into the login
//! window's session too (reboot, logout), written as root (`sudo`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ar_core::{Profile, Role};

use super::{Action, Binaries, InstallStatus, Plan, launchd};

const LAUNCHCTL: &str = "/bin/launchctl";
const SYSTEM_AGENTS: &str = "/Library/LaunchAgents";

fn label(profile: &Profile) -> String {
    format!("io.github.jia-ss.ai-remote.host.{}", &profile.instance_id()[..8])
}

/// The per-user agent of `owner`'s profile. `sudo` changes `HOME`: the owner's home comes from
/// the account database.
fn user_plist_path(profile: &Profile, owner: u32) -> Result<PathBuf> {
    let home = home_of(owner)?;
    Ok(home.join("Library").join("LaunchAgents").join(format!("{}.plist", label(profile))))
}

fn system_plist_path(profile: &Profile) -> PathBuf {
    Path::new(SYSTEM_AGENTS).join(format!("{}.plist", label(profile)))
}

/// `gui/<uid>`: the launchd domain of the owner's login session.
fn gui_domain(owner: u32) -> String {
    format!("gui/{owner}")
}

/// The account the installation is for: the profile directory's owner, which survives `sudo`
/// (the process then runs as root, and `HOME` is root's).
fn owner_uid(profile: &Profile) -> Result<u32> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata(profile.root())
        .with_context(|| format!("read the owner of {}", profile.root().display()))?
        .uid();
    if uid == 0 {
        bail!(
            "the profile {} belongs to root; create it as the user who will use the host (run `ai-remote host` once \
             without sudo)",
            profile.root().display()
        );
    }
    Ok(uid)
}

fn home_of(uid: u32) -> Result<PathBuf> {
    // SAFETY: getpwuid returns a pointer to a static buffer, read before any other call to it.
    unsafe {
        let entry = libc::getpwuid(uid);
        if entry.is_null() {
            bail!("account uid {uid} does not exist");
        }
        let dir = std::ffi::CStr::from_ptr((*entry).pw_dir).to_string_lossy().into_owned();
        Ok(PathBuf::from(dir))
    }
}

fn is_root() -> bool {
    // SAFETY: a plain query.
    unsafe { libc::geteuid() == 0 }
}

/// `service`: the system agent that also serves the login window (needs `sudo`).
pub fn install_plan(profile: &Profile, service: bool) -> Result<Plan> {
    let bins = Binaries::locate()?;
    bins.require_hostw()?;
    let owner = owner_uid(profile)?;
    let domain = gui_domain(owner);
    let user_path = user_plist_path(profile, owner)?;
    let system_path = system_plist_path(profile);
    // Not the host log itself: the host rotates that file, and launchd would keep writing to the
    // renamed copy through the descriptor it holds.
    let stderr_log = profile.role_dir(Role::Host)?.join("host-stderr.log");
    let login_window = service.then_some(owner);
    let plist = launchd::launch_agent_plist(&label(profile), &bins.hostw, profile.root(), &stderr_log, login_window)?;
    let responsible_bundle = launchd::responsible_bundle_identifier(&bins.hostw)?;
    let privacy_note = match responsible_bundle {
        Some(id) => format!(
            "The LaunchAgent is associated with the enclosing app {id}. Verify Screen Recording and Accessibility \
             in System Settings > Privacy & Security, restart the host, then run doctor."
        ),
        None => "This standalone host has no associated app bundle. Grant Screen Recording and Accessibility to \
                 ai-remote-hostw in System Settings > Privacy & Security, restart the host, then run doctor."
            .into(),
    };
    let target = format!("{domain}/{}", label(profile));
    let (path, mut notes, blocker) = if service {
        let notes = vec![
            format!(
                "The system LaunchAgent {} starts {} in the user's session and in the login window's session \
                 (after a reboot or logout), as RustDesk does.",
                label(profile),
                bins.hostw.display()
            ),
            format!(
                "At the login window launchd starts the host as root; it then becomes uid {owner} (the owner of {}) \
                 so that the profile stays that user's and that user's privacy grants apply. If capture stays black \
                 there, retry with AI_REMOTE_KEEP_ROOT=1 in the agent's environment and report it.",
                profile.root().display()
            ),
            "Approval prompts are not shown at the lock screen or the login window; only approver devices can \
             approve while nobody is logged in."
                .into(),
            privacy_note,
        ];
        let blocker = (!is_root()).then(|| format!("writing {} needs root: run this with sudo", system_path.display()));
        (system_path.clone(), notes, blocker)
    } else {
        let notes =
            vec![format!("The LaunchAgent {} starts {} at login.", label(profile), bins.hostw.display()), privacy_note];
        (user_path.clone(), notes, None)
    };
    let mut actions = Vec::new();
    if service && user_path.exists() {
        notes.push(format!("The per-user LaunchAgent {} is replaced by the system one.", user_path.display()));
        actions.push(Action::RemoveFile { path: user_path.clone() });
    } else if !service && system_path.exists() {
        notes.push(format!(
            "A system LaunchAgent {} exists; it keeps starting the host at the login window. Remove it with \
             `sudo ai-remote host uninstall` first if that is not wanted.",
            system_path.display()
        ));
    }
    let installed = path.to_string_lossy().into_owned();
    actions.extend([
        Action::WriteFile { path, contents: plist.clone().into_bytes(), preview: plist },
        Action::run_tolerant(LAUNCHCTL.into(), &["bootout".into(), target], "unload an earlier copy"),
        Action::run(
            LAUNCHCTL.into(),
            &["bootstrap".into(), domain, installed],
            "load the LaunchAgent into the current login session (the login window loads it by itself)",
        ),
    ]);
    Ok(Plan {
        summary: format!(
            "Install autostart for the ai-remote host (profile {}{})",
            profile.root().display(),
            if service { ", with the login window" } else { "" }
        ),
        notes,
        actions,
        needs_elevation: service,
        blocker,
    })
}

pub fn uninstall_plan(profile: &Profile) -> Result<Plan> {
    let owner = owner_uid(profile)?;
    let target = format!("{}/{}", gui_domain(owner), label(profile));
    let system_path = system_plist_path(profile);
    let mut actions =
        vec![Action::run_tolerant(LAUNCHCTL.into(), &["bootout".into(), target], "unload the LaunchAgent")];
    let mut blocker = None;
    if system_path.exists() {
        if is_root() {
            actions.push(Action::RemoveFile { path: system_path });
        } else {
            blocker = Some(format!("removing {} needs root: run this with sudo", system_path.display()));
        }
    }
    actions.push(Action::RemoveFile { path: user_plist_path(profile, owner)? });
    Ok(Plan {
        summary: format!("Remove autostart for the ai-remote host (profile {})", profile.root().display()),
        notes: Vec::new(),
        actions,
        needs_elevation: blocker.is_some(),
        blocker,
    })
}

pub fn start_action(profile: &Profile) -> Result<Action> {
    let target = format!("{}/{}", gui_domain(owner_uid(profile)?), label(profile));
    Ok(Action::run(LAUNCHCTL.into(), &["kickstart".into(), target], "start the LaunchAgent now"))
}

pub fn status(profile: &Profile) -> Result<InstallStatus> {
    let bins = Binaries::locate()?;
    let owner = owner_uid(profile)?;
    let system_path = system_plist_path(profile);
    let (path, service) =
        if system_path.exists() { (system_path, true) } else { (user_plist_path(profile, owner)?, false) };
    let kind = if service { "LaunchAgent (system, login window too)" } else { "LaunchAgent" };
    let mut st = InstallStatus { kind: kind.into(), name: path.display().to_string(), ..Default::default() };
    match std::fs::read(&path) {
        Ok(bytes) => {
            st.installed = true;
            match launchd::read_program_arguments(&bytes) {
                Some(args) => {
                    let expected = launchd::program_arguments(&bins.hostw, profile.root(), service.then_some(owner));
                    st.current = Some(args == expected);
                    st.launches = Some(args.join(" "));
                }
                None => st.detail = Some("not a LaunchAgent property list".into()),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => st.detail = Some(format!("read {}: {e}", path.display())),
    }
    Ok(st)
}
