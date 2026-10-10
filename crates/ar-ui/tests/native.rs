//! Actual native control-provider test, isolated to a fixture process created by this test.
use ar_ui::{Action, Element, Node};
use std::{
    path::PathBuf,
    process::{Child, Command},
    time::{Duration, Instant},
};
struct Fixture(Child);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn poll(path: &std::path::Path, predicate: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
    let start = Instant::now();
    loop {
        if let Ok(s) = std::fs::read_to_string(path)
            && let Ok(v) = serde_json::from_str(&s)
            && predicate(&v)
        {
            return v;
        }
        assert!(start.elapsed() < Duration::from_secs(15), "fixture state verifier timed out");
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
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("state.json");
    #[cfg(windows)]
    let child = Command::new("powershell.exe")
        .args(["-NoProfile", "-STA", "-File"])
        .arg(root.join("tools/ui-fixture.ps1"))
        .arg("-OutputPath")
        .arg(&state)
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
        Command::new(exe).arg(&state).spawn().unwrap()
    };
    #[cfg(not(any(windows, target_os = "macos")))]
    panic!("native test requires Windows or macOS");
    #[cfg(any(windows, target_os = "macos"))]
    {
        let fixture = Fixture(child);
        let pid = poll(&state, |v| v["pid"].as_u64().is_some())["pid"].as_u64().unwrap() as u32;
        assert_eq!(pid, fixture.0.id());
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
        poll(&state, |v| v["text"] == literal);
        assert_eq!(ar_ui::read(element(&nodes, "Input")).unwrap().value.as_deref(), Some(literal));
        ar_ui::act(element(&nodes, "Apply"), Action::Press, None).unwrap();
        poll(&state, |v| v["clicks"] == 1);
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
        let final_state = poll(&state, |v| v["checked"] == true);
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
