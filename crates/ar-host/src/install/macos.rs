//! macOS autostart: a per-user LaunchAgent in `~/Library/LaunchAgents`, loaded with launchctl.

use std::path::PathBuf;

use anyhow::{Context, Result};
use ar_core::{Profile, Role};

use super::{Action, Binaries, InstallStatus, Plan, current_user_id, launchd};

const LAUNCHCTL: &str = "/bin/launchctl";

fn label(profile: &Profile) -> String {
    format!("io.github.jia-ss.ai-remote.host.{}", &profile.instance_id()[..8])
}

fn plist_path(profile: &Profile) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")?;
    Ok(home.join("Library").join("LaunchAgents").join(format!("{}.plist", label(profile))))
}

/// `gui/<uid>`: the launchd domain of this user's login session.
fn gui_domain() -> Result<String> {
    Ok(format!("gui/{}", current_user_id()?))
}

pub fn install_plan(profile: &Profile) -> Result<Plan> {
    let bins = Binaries::locate()?;
    bins.require_hostw()?;
    let path = plist_path(profile)?;
    let domain = gui_domain()?;
    let plist = launchd::launch_agent_plist(&label(profile), &bins.hostw, profile.root(), &profile.log_path(Role::Host)?)?;
    let target = format!("{domain}/{}", label(profile));
    Ok(Plan {
        summary: format!("Install autostart for the ai-remote host (profile {})", profile.root().display()),
        notes: vec![
            format!("The LaunchAgent {} starts {} at login.", label(profile), bins.hostw.display()),
            "macOS asks to allow Screen Recording and Accessibility for ai-remote-hostw the first time it captures \
             or acts; grant both in System Settings > Privacy & Security."
                .into(),
        ],
        actions: vec![
            Action::WriteFile { path: path.clone(), contents: plist.clone().into_bytes(), preview: plist },
            Action::run_tolerant(LAUNCHCTL.into(), &["bootout".into(), target], "unload an earlier copy"),
            Action::run(
                LAUNCHCTL.into(),
                &["bootstrap".into(), domain, path.to_string_lossy().into_owned()],
                "load the LaunchAgent",
            ),
        ],
        needs_elevation: false,
    })
}

pub fn uninstall_plan(profile: &Profile) -> Result<Plan> {
    let target = format!("{}/{}", gui_domain()?, label(profile));
    Ok(Plan {
        summary: format!("Remove autostart for the ai-remote host (profile {})", profile.root().display()),
        notes: Vec::new(),
        actions: vec![
            Action::run_tolerant(LAUNCHCTL.into(), &["bootout".into(), target], "unload the LaunchAgent"),
            Action::RemoveFile { path: plist_path(profile)? },
        ],
        needs_elevation: false,
    })
}

pub fn start_action(profile: &Profile) -> Result<Action> {
    let target = format!("{}/{}", gui_domain()?, label(profile));
    Ok(Action::run(LAUNCHCTL.into(), &["kickstart".into(), target], "start the LaunchAgent now"))
}

pub fn status(profile: &Profile) -> Result<InstallStatus> {
    let bins = Binaries::locate()?;
    let path = plist_path(profile)?;
    let mut st = InstallStatus { kind: "LaunchAgent".into(), name: path.display().to_string(), ..Default::default() };
    match std::fs::read(&path) {
        Ok(bytes) => {
            st.installed = true;
            match launchd::read_program_arguments(&bytes) {
                Some(args) => {
                    let expected = [
                        bins.hostw.to_string_lossy().into_owned(),
                        "--home".to_string(),
                        profile.root().to_string_lossy().into_owned(),
                    ];
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
