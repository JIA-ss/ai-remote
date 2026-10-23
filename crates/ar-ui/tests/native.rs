//! Actual native control-provider test, isolated to a fixture process created by this test.
use ar_ui::{Action, Element, Node};
use std::{
    fs::File,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
struct Fixture {
    child: Child,
    evidence: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn poll(
    fixture: &mut Fixture,
    path: &Path,
    stage: &str,
    predicate: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let start = Instant::now();
    let evidence = fixture.evidence.join(format!("poll-{stage}.json"));
    let mut last_state = serde_json::Value::Null;
    let mut last_error = serde_json::Value::Null;
    let mut first_observation = true;
    loop {
        let child_state = match fixture.child.try_wait() {
            Ok(None) => "running".to_string(),
            Ok(Some(status)) => format!("exited: {status}"),
            Err(error) => format!("status unavailable: {:?}", error.kind()),
        };
        match std::fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str::<serde_json::Value>(&s) {
                Ok(value) => last_state = value,
                Err(error) => {
                    last_error = serde_json::json!({
                        "kind":"parse", "message":error.to_string(), "elapsed_ms":start.elapsed().as_millis(),
                    });
                }
            },
            Err(error) => {
                last_error = serde_json::json!({
                    "kind":"io", "error_kind":format!("{:?}", error.kind()), "elapsed_ms":start.elapsed().as_millis(),
                });
            }
        }
        let matched = !last_state.is_null() && predicate(&last_state);
        let timed_out = start.elapsed() >= Duration::from_secs(15);
        let diagnostic = serde_json::json!({
            "stage":stage, "elapsed_ms":start.elapsed().as_millis(), "child":child_state,
            "matched":matched, "last_successful_state":last_state, "last_read_error":last_error,
        });
        // Retain entry and terminal evidence without adding a second periodic file writer.
        if first_observation || matched || timed_out || child_state != "running" {
            std::fs::write(&evidence, serde_json::to_vec_pretty(&diagnostic).unwrap())
                .expect("save fixture verifier evidence");
            first_observation = false;
        }
        assert_eq!(child_state, "running", "fixture exited while verifying {stage}: {diagnostic}");
        if matched {
            return last_state;
        }
        assert!(!timed_out, "fixture state verifier timed out at {stage}: {diagnostic}");
        std::thread::sleep(Duration::from_millis(50));
    }
}
fn element<'a>(nodes: &'a [(Node, Element)], label: &str) -> &'a Element {
    let selected: Vec<_> = nodes.iter().filter(|(n, _)| n.label == label).collect();
    assert_eq!(selected.len(), 1, "expected unique native element {label}");
    &selected[0].1
}
#[test]
#[ignore = "requires a graphical session (and macOS Accessibility permission)"]
fn native_controls_without_images() {
    ar_ui::ready().expect("native accessibility permission");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis();
    let evidence_parent = root.join("target/ui-validation");
    std::fs::create_dir_all(&evidence_parent).expect("create native evidence parent");
    let evidence = evidence_parent.join(format!("native-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&evidence).expect("create independent native evidence attempt");
    let stdout = File::create(evidence.join("fixture.stdout.log")).expect("create fixture stdout evidence");
    let stderr = File::create(evidence.join("fixture.stderr.log")).expect("create fixture stderr evidence");
    let state = evidence.join("state.json");
    #[cfg(target_os = "macos")]
    let temp = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let child = Command::new("powershell.exe")
        .args(["-NoProfile", "-STA", "-File"])
        .arg(root.join("tools/ui-fixture.ps1"))
        .arg("-OutputPath")
        .arg(&state)
        .arg("-ProgressPath")
        .arg(evidence.join("fixture-stages.jsonl"))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .unwrap();
    #[cfg(target_os = "macos")]
    let child = {
        let exe = temp.path().join("fixture");
        assert!(
            Command::new("swiftc")
                .arg(root.join("tools/ui-fixture.swift"))
                .arg("-o")
                .arg(&exe)
                .status()
                .unwrap()
                .success()
        );
        Command::new(exe).arg(&state).stdout(Stdio::from(stdout)).stderr(Stdio::from(stderr)).spawn().unwrap()
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    panic!("native test requires Windows or macOS");
    #[cfg(any(windows, target_os = "macos"))]
    {
        let mut fixture = Fixture { child, evidence };
        let pid =
            poll(&mut fixture, &state, "startup_pid", |v| v["pid"].as_u64().is_some())["pid"].as_u64().unwrap() as u32;
        assert_eq!(pid, fixture.child.id());
        let start = Instant::now();
        let check = || {
            anyhow::ensure!(start.elapsed() < Duration::from_secs(10), "observation deadline");
            Ok(())
        };
        let (nodes, truncated, warnings) = ar_ui::collect(pid, 512, 12, &check).unwrap();
        assert!(!truncated, "{warnings:?}");
        let secure = ar_ui::read(element(&nodes, "Password")).unwrap();
        assert!(secure.protected && secure.value.is_none() && secure.actions.is_empty());
        let literal = "中文 literal {ENTER} $(not-a-command)";
        ar_ui::act(element(&nodes, "Input"), Action::SetValue, Some(literal)).unwrap();
        poll(&mut fixture, &state, "set_value", |v| v["text"] == literal);
        assert_eq!(ar_ui::read(element(&nodes, "Input")).unwrap().value.as_deref(), Some(literal));
        ar_ui::act(element(&nodes, "Apply"), Action::Press, None).unwrap();
        poll(&mut fixture, &state, "press_apply", |v| v["clicks"] == 1);
        let disabled = ar_ui::read(element(&nodes, "Disabled")).unwrap();
        assert_eq!(disabled.enabled, Some(false));
        let option = element(&nodes, "Enabled option");
        let node = ar_ui::read(option).unwrap();
        ar_ui::act(
            option,
            if node.actions.iter().any(|a| a == "toggle") { Action::Toggle } else { Action::Press },
            None,
        )
        .unwrap();
        let final_state = poll(&mut fixture, &state, "toggle_option", |v| v["checked"] == true);
        assert_eq!(ar_ui::read(option).unwrap().checked, Some(true));
        let (bounded, truncated, _) = ar_ui::collect(pid, 1, 12, &|| Ok(())).unwrap();
        assert_eq!(bounded.len(), 1);
        assert!(truncated);
        println!(
            "native_ui_acceptance={}",
            serde_json::json!({"source":ar_ui::SOURCE,"elements":nodes.len(),"literal_text":"verified","button":"verified","checkbox":"verified","password":"redacted","bounded_tree":"verified","fixture":final_state})
        );
    }
}
