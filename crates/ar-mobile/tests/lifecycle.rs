use ar_mobile::MobileController;
use serde_json::{Value, json};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_embedded_controller_closes_and_reopens_with_the_same_identity() {
    let dir = tempfile::tempdir().unwrap();
    // Mirror a real iOS sandbox path, deliberately longer than a Unix socket allows.
    let path = dir
        .path()
        .join("Containers/Data/Application/00000000-0000-0000-0000-000000000000/Library/Application Support/ai-remote")
        .to_string_lossy()
        .into_owned();
    let controller = MobileController::connect(path.clone()).await.unwrap();
    let status: Value =
        serde_json::from_str(&controller.request(json!({"op":"status"}).to_string()).await.unwrap()).unwrap();
    assert!(status["device"].as_str().unwrap().starts_with("eid:"));
    let unsupported = controller.request(json!({"op":"mcp"}).to_string()).await;
    assert!(unsupported.is_err());
    controller.close().await;
    assert!(controller.request(json!({"op":"status"}).to_string()).await.is_err());
    let second = MobileController::connect(path).await.unwrap();
    let again: Value =
        serde_json::from_str(&second.request(json!({"op":"status"}).to_string()).await.unwrap()).unwrap();
    assert_eq!(status["device"], again["device"]);
    second.close().await;
}
