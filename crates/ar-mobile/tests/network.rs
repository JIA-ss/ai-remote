//! The phone's Network settings: what the app reads and writes, and that the embedded controller
//! starts with them.

use ar_mobile::{MobileController, network_settings, update_network};
use serde_json::{Value, json};

fn settings(dir: &str) -> Value {
    serde_json::from_str(&network_settings(dir.into()).unwrap()).unwrap()
}

fn config_text(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("controller/ai-remote.toml")).unwrap()
}

#[test]
fn a_new_phone_is_quiet_and_automatic() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().into_owned();
    assert_eq!(
        settings(&path),
        json!({"mode":"auto", "lan_discovery":false, "port_mapping":false, "locked":false, "pair_code":true, "lan_port":47800})
    );
    // Reading writes nothing: the defaults are the absence of a file.
    assert!(!dir.path().join("controller/ai-remote.toml").exists());
}

#[test]
fn local_network_only_without_discovery_has_no_pair_code() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().into_owned();
    let lan: Value =
        serde_json::from_str(&update_network(path.clone(), Some("lan".into()), None, None).unwrap()).unwrap();
    assert_eq!(lan["mode"], "lan");
    assert_eq!(lan["pair_code"], false);
    assert!(config_text(dir.path()).contains("mode = \"lan\""));

    // A phone only looks for computers: "on" is written as "resolve", never announcing the phone.
    let found: Value = serde_json::from_str(&update_network(path.clone(), None, Some(true), None).unwrap()).unwrap();
    assert_eq!(found["lan_discovery"], true);
    assert_eq!(found["pair_code"], true);
    assert!(config_text(dir.path()).contains("lan_discovery = \"resolve\""));

    let back: Value =
        serde_json::from_str(&update_network(path.clone(), Some("auto".into()), Some(false), None).unwrap()).unwrap();
    assert_eq!(back["mode"], "auto");
    assert_eq!(back["lan_discovery"], false);
    assert!(update_network(path, Some("offline".into()), None, None).is_err());
}

#[test]
fn quiet_locks_discovery_and_port_mapping_off() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().into_owned();
    std::fs::create_dir_all(dir.path().join("controller")).unwrap();
    std::fs::write(
        dir.path().join("controller/ai-remote.toml"),
        "# set by the owner\n[network]\nquiet = true\nport_mapping = true\n",
    )
    .unwrap();
    let locked = settings(&path);
    assert_eq!(locked["locked"], true);
    assert_eq!(locked["port_mapping"], false, "quiet wins over what the file asks for");
    assert!(update_network(path.clone(), None, Some(true), None).is_err());
    assert!(update_network(path.clone(), None, None, Some(true)).is_err());
    // Turning things off and changing the mode stay allowed, and the owner's comment survives.
    let lan: Value =
        serde_json::from_str(&update_network(path, Some("lan".into()), None, Some(false)).unwrap()).unwrap();
    assert_eq!(lan["mode"], "lan");
    assert!(config_text(dir.path()).starts_with("# set by the owner\n"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_controller_starts_with_the_saved_settings_and_takes_host_addresses() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().into_owned();
    update_network(path.clone(), Some("lan".into()), None, None).unwrap();
    let controller = MobileController::connect(path).await.unwrap();
    let status: Value =
        serde_json::from_str(&controller.request(json!({"op":"status"}).to_string()).await.unwrap()).unwrap();
    assert_eq!(status["lan_direct"], true);
    // The app may set where a paired computer is dialled; here none is paired, so the request reaches
    // the controller and fails there, not at the app's request filter.
    let unknown = controller
        .request(json!({"op":"set_host_address", "host":"office-pc", "address":"192.168.1.20"}).to_string())
        .await
        .unwrap_err()
        .to_string();
    assert!(!unknown.contains("unsupported"), "{unknown}");
    controller.close().await;
}
