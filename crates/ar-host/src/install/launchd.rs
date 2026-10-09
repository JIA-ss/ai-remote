//! The macOS LaunchAgent that starts the background host when the user logs in.
//!
//! A per-user LaunchAgent, never a LaunchDaemon: only an agent loads into the user's GUI (Aqua)
//! session, where screen capture, input and approval dialogs work. Generated with the `plist`
//! crate and kept platform-independent so it is tested everywhere.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
struct LaunchAgent {
    label: String,
    program_arguments: Vec<String>,
    run_at_load: bool,
    keep_alive: KeepAlive,
    /// Not throttled like a background daemon: the host serves interactive control.
    process_type: String,
    /// Only the GUI login session, never an SSH or background session.
    limit_load_to_session_type: String,
    /// Catches anything printed before the host's own file logging starts.
    standard_error_path: String,
}

/// Restart after a crash but not after a clean exit, so `ai-remote stop-host` stays stopped
/// until the next login instead of being undone by launchd at once.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
struct KeepAlive {
    successful_exit: bool,
}

/// The LaunchAgent property list (XML) running `program --home <home>`.
pub fn launch_agent_plist(label: &str, program: &Path, home: &Path, stderr_log: &Path) -> Result<String> {
    let agent = LaunchAgent {
        label: label.to_string(),
        program_arguments: vec![
            program.to_string_lossy().into_owned(),
            "--home".to_string(),
            home.to_string_lossy().into_owned(),
        ],
        run_at_load: true,
        keep_alive: KeepAlive { successful_exit: false },
        process_type: "Interactive".into(),
        limit_load_to_session_type: "Aqua".into(),
        standard_error_path: stderr_log.to_string_lossy().into_owned(),
    };
    let mut out = Vec::new();
    plist::to_writer_xml(&mut out, &agent).context("serialize the LaunchAgent")?;
    String::from_utf8(out).context("LaunchAgent is not UTF-8")
}

/// The program arguments of an installed LaunchAgent, if the file is one.
pub fn read_program_arguments(bytes: &[u8]) -> Option<Vec<String>> {
    plist::from_bytes::<LaunchAgent>(bytes).ok().map(|a| a.program_arguments)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn plist_runs_the_host_in_the_gui_session() {
        let xml = launch_agent_plist(
            "io.github.jia-ss.ai-remote.host.0123abcd",
            &PathBuf::from("/Applications/ai remote/ai-remote-hostw"),
            &PathBuf::from("/Users/me/Library/Application Support/ai-remote"),
            &PathBuf::from("/Users/me/Library/Application Support/ai-remote/host/host.log"),
        )
        .unwrap();
        for needle in [
            "<key>Label</key>",
            "<string>io.github.jia-ss.ai-remote.host.0123abcd</string>",
            "<key>RunAtLoad</key>\n\t<true/>",
            "<key>KeepAlive</key>",
            "<key>SuccessfulExit</key>",
            "<key>LimitLoadToSessionType</key>\n\t<string>Aqua</string>",
            "<key>ProcessType</key>\n\t<string>Interactive</string>",
        ] {
            assert!(xml.contains(needle), "missing {needle:?} in\n{xml}");
        }
    }

    #[test]
    fn program_arguments_round_trip() {
        let xml = launch_agent_plist(
            "label",
            &PathBuf::from("/opt/ai-remote-hostw"),
            &PathBuf::from("/Users/me/x & y"),
            &PathBuf::from("/tmp/host.log"),
        )
        .unwrap();
        assert_eq!(
            read_program_arguments(xml.as_bytes()).unwrap(),
            vec!["/opt/ai-remote-hostw".to_string(), "--home".into(), "/Users/me/x & y".into()]
        );
        assert_eq!(read_program_arguments(b"not a plist"), None);
    }
}
