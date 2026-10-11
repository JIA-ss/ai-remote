//! The macOS LaunchAgent that starts the background host when the user logs in.
//!
//! A per-user LaunchAgent, never a LaunchDaemon: only an agent loads into the user's GUI (Aqua)
//! session, where screen capture, input and approval dialogs work. Generated with the `plist`
//! crate and kept platform-independent so it is tested everywhere.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
struct LaunchAgent {
    label: String,
    program_arguments: Vec<String>,
    /// Legacy launchd jobs must identify their enclosing app as the responsible bundle.
    /// See Apple TN3179; a standalone CLI intentionally has no associated app.
    #[serde(default, deserialize_with = "read_associated_identifiers", skip_serializing_if = "Option::is_none")]
    associated_bundle_identifiers: Option<Vec<String>>,
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

/// launchd permits either a single identifier string or an array; emit arrays but read both.
fn read_associated_identifiers<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Identifiers {
        One(String),
        Many(Vec<String>),
    }
    Ok(Option::<Identifiers>::deserialize(deserializer)?.map(|ids| match ids {
        Identifiers::One(id) => vec![id],
        Identifiers::Many(ids) => ids,
    }))
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
        associated_bundle_identifiers: responsible_bundle_identifier(program)?.map(|id| vec![id]),
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

/// Reads the real enclosing application's identity; never guesses it from a filename or label.
/// Bundle association is metadata for launchd, not a substitute for code signing or TCC consent.
pub fn responsible_bundle_identifier(program: &Path) -> Result<Option<String>> {
    let program = match program.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            // Plain-text plans can describe a standalone executable before it is installed.
            if program.ancestors().any(|p| p.extension().is_some_and(|ext| ext == "app")) {
                return Err(error).with_context(|| format!("locate bundled host {}", program.display()));
            }
            return Ok(None);
        }
    };
    let Some(bundle) = program.ancestors().find(|p| p.extension().is_some_and(|ext| ext == "app")) else {
        return Ok(None);
    };
    let binaries = bundle.join("Contents").join("MacOS");
    if !program.starts_with(&binaries) || !program.is_file() {
        bail!("the bundled host {} is not an executable in Contents/MacOS", program.display());
    }
    let info_path = bundle.join("Contents").join("Info.plist");
    let info = plist::Value::from_file(&info_path)
        .with_context(|| format!("read responsible bundle {}", info_path.display()))?;
    let info = info.as_dictionary().context("the responsible bundle Info.plist is not a dictionary")?;
    if info.get("CFBundlePackageType").and_then(plist::Value::as_string) != Some("APPL") {
        bail!("{} does not describe an application bundle", info_path.display());
    }
    let identifier = info
        .get("CFBundleIdentifier")
        .and_then(plist::Value::as_string)
        .context("the responsible bundle has no CFBundleIdentifier")?;
    if identifier.is_empty()
        || !identifier.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        || !identifier.bytes().any(|b| b.is_ascii_alphanumeric())
    {
        bail!("the responsible bundle CFBundleIdentifier is invalid");
    }
    let executable = info
        .get("CFBundleExecutable")
        .and_then(plist::Value::as_string)
        .context("the responsible bundle has no CFBundleExecutable")?;
    if Path::new(executable).file_name() != Some(std::ffi::OsStr::new(executable))
        || !binaries.join(executable).is_file()
    {
        bail!("the responsible bundle's main executable is missing or invalid");
    }
    Ok(Some(identifier.to_string()))
}

/// The program arguments of an installed LaunchAgent, if the file is one.
pub fn read_program_arguments(bytes: &[u8]) -> Option<Vec<String>> {
    plist::from_bytes::<LaunchAgent>(bytes).ok().map(|a| a.program_arguments)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn bundle_fixture(root: &Path, identifier: &str) -> PathBuf {
        let bundle = root.join("ai remote 测试.app");
        let binaries = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&binaries).unwrap();
        std::fs::write(binaries.join("desktop"), b"main executable fixture").unwrap();
        let program = binaries.join("ai-remote-hostw");
        std::fs::write(&program, b"host executable fixture").unwrap();
        let mut info = plist::Dictionary::new();
        info.insert("CFBundlePackageType".into(), "APPL".into());
        info.insert("CFBundleIdentifier".into(), identifier.into());
        info.insert("CFBundleExecutable".into(), "desktop".into());
        plist::Value::Dictionary(info).to_file_xml(bundle.join("Contents/Info.plist")).unwrap();
        program
    }

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

    #[test]
    fn bundled_host_association_comes_from_real_info_plist() {
        let directory = tempfile::tempdir().unwrap();
        let program = bundle_fixture(directory.path(), "com.example.Responsible-Desktop");
        assert_eq!(
            responsible_bundle_identifier(&program).unwrap().as_deref(),
            Some("com.example.Responsible-Desktop")
        );
        let xml = launch_agent_plist(
            "a.different.launchd.label",
            &program,
            directory.path(),
            &directory.path().join("stderr.log"),
        )
        .unwrap();
        let agent: LaunchAgent = plist::from_bytes(xml.as_bytes()).unwrap();
        assert_eq!(agent.associated_bundle_identifiers, Some(vec!["com.example.Responsible-Desktop".into()]));
        assert_eq!(read_program_arguments(xml.as_bytes()).unwrap()[0], program.to_string_lossy());
        assert!(xml.contains("<key>AssociatedBundleIdentifiers</key>"));
    }

    #[test]
    fn standalone_host_has_no_bundle_association() {
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("ai-remote-hostw");
        std::fs::write(&program, b"standalone fixture").unwrap();
        assert_eq!(responsible_bundle_identifier(&program).unwrap(), None);
        let xml =
            launch_agent_plist("a.launchd.label", &program, directory.path(), &directory.path().join("stderr.log"))
                .unwrap();
        assert!(!xml.contains("AssociatedBundleIdentifiers"));
        assert_eq!(plist::from_bytes::<LaunchAgent>(xml.as_bytes()).unwrap().associated_bundle_identifiers, None);
    }

    #[test]
    fn existing_agents_with_a_single_associated_identifier_can_be_read() {
        let directory = tempfile::tempdir().unwrap();
        let program = bundle_fixture(directory.path(), "com.example.Responsible");
        let xml =
            launch_agent_plist("label", &program, directory.path(), &directory.path().join("stderr.log")).unwrap();
        let mut value = plist::Value::from_reader_xml(xml.as_bytes()).unwrap();
        value
            .as_dictionary_mut()
            .unwrap()
            .insert("AssociatedBundleIdentifiers".into(), "com.example.Responsible".into());
        let mut out = Vec::new();
        value.to_writer_xml(&mut out).unwrap();
        assert_eq!(read_program_arguments(&out).unwrap()[0], program.to_string_lossy());
    }

    #[test]
    fn malformed_bundle_identity_or_main_executable_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let program = bundle_fixture(directory.path(), "com.example.Valid");
        let info_path = program.parent().unwrap().parent().unwrap().join("Info.plist");
        let original = plist::Value::from_file(&info_path).unwrap();
        for (key, value) in [
            ("CFBundleIdentifier", ""),
            ("CFBundleIdentifier", "com.example with spaces"),
            ("CFBundlePackageType", "FMWK"),
            ("CFBundleExecutable", "missing"),
            ("CFBundleExecutable", "../outside"),
        ] {
            let mut info = original.clone();
            info.as_dictionary_mut().unwrap().insert(key.into(), value.into());
            info.to_file_xml(&info_path).unwrap();
            assert!(responsible_bundle_identifier(&program).is_err(), "accepted invalid {key}={value}");
        }
        std::fs::remove_file(&info_path).unwrap();
        assert!(responsible_bundle_identifier(&program).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_does_not_assign_an_unrelated_bundle_to_standalone_code() {
        let directory = tempfile::tempdir().unwrap();
        let program = bundle_fixture(directory.path(), "com.example.Unrelated");
        let standalone = directory.path().join("standalone-host");
        std::fs::write(&standalone, b"standalone fixture").unwrap();
        std::fs::remove_file(&program).unwrap();
        std::os::unix::fs::symlink(&standalone, &program).unwrap();
        assert_eq!(responsible_bundle_identifier(&program).unwrap(), None);
    }
}
