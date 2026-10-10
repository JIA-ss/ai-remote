//! #222 review: the admit recheck in `run_session` runs AT ITS CALL SITE, under the registry lock.
//!
//! A revoke is swap-then-sever. A session that snapshotted the service registry before the swap,
//! and registers its admitted principals only after the sever ran, would escape both: the snapshot
//! admits it and the sever found nothing to cut. `run_session` closes that by re-reading the LIVE
//! registry inside `admit_session`'s closure, under the same lock `sever_matching_admitted` takes.
//!
//! This test drives the exact interleaving through the real `run_mesh_connection`:
//!  1. the per-session `resolve` parks (a gate hook), AFTER the session took its registry snapshot;
//!  2. the test starts the sever; UNDER the registry lock it releases the gate, waits for the session
//!     to reach the lock, swaps in a registry that no longer admits the principal, and returns — the
//!     sever cuts 0 connections (nothing admitted yet);
//!  3. the session then admits against the swapped registry and must be REFUSED (-32054).
//!
//! Mutations this must catch: an admit closure that is `|| true`, and one that returns a value
//! computed BEFORE taking the lock (`let pre = still_admits(..); admit_session(p, || pre)`).
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use mcpmesh_net::{
    ConnRegistry, EndpointId, LiveServices, PeerIdentity, ServiceEntry, SessionBackend,
    SessionTransport, TrustGate, run_mesh_connection,
};
use serde_json::json;

const PRINCIPAL: &str = "b64u:alice";

struct EchoBackend;

#[async_trait::async_trait]
impl SessionBackend for EchoBackend {
    async fn run(
        &self,
        _identity: Option<PeerIdentity>,
        initialize: serde_json::Value,
        mut transport: SessionTransport,
    ) -> anyhow::Result<()> {
        transport
            .send_value(json!({"jsonrpc":"2.0","id":initialize["id"],"result":{
                "serverInfo":{"name":"echo-stub","version":"0"}}}))
            .await?;
        while transport.recv_value().await?.is_some() {}
        Ok(())
    }
}

fn services(allow: &[&str]) -> Arc<mcpmesh_net::Services> {
    let mut map = HashMap::new();
    map.insert(
        "private".to_string(),
        ServiceEntry {
            backend: Arc::new(EchoBackend),
            allow: allow.iter().map(|a| (*a).to_string()).collect(),
            kind: mcpmesh_net::ServiceKind::Run,
            ephemeral: false,
        },
    );
    Arc::new(mcpmesh_net::Services::new(map))
}

/// Resolves everyone as `b64u:alice`. The SECOND `resolve` — the per-session one (the first is the
/// connection-level check) — reports that it was entered, then parks until released.
struct ParkingGate {
    calls: AtomicUsize,
    entered: Mutex<mpsc::Sender<()>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl TrustGate for ParkingGate {
    fn resolve(&self, endpoint: &EndpointId) -> Option<PeerIdentity> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
            self.entered.lock().unwrap().send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .expect("the test releases the parked resolve");
        }
        Some(PeerIdentity {
            endpoint: *endpoint,
            name: "alice".into(),
            user_id: Some(PRINCIPAL.into()),
            groups: vec![],
        })
    }
}

async fn endpoint() -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![mcpmesh_net::ALPN_MCP.to_vec()])
        .bind()
        .await
        .expect("bind")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoke_swapped_in_while_a_session_reaches_its_admit_refuses_that_session() {
    tokio::time::timeout(Duration::from_secs(60), async {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let gate = Arc::new(ParkingGate {
            calls: AtomicUsize::new(0),
            entered: Mutex::new(entered_tx),
            release: Mutex::new(release_rx),
        });
        let live = Arc::new(LiveServices::new(services(&[PRINCIPAL])));
        let registry = Arc::new(ConnRegistry::new());

        let server = endpoint().await;
        let client = endpoint().await;
        let addr = server.addr();
        {
            let (gate, live, registry) = (gate.clone(), live.clone(), registry.clone());
            tokio::spawn(async move {
                let conn = server.accept().await.unwrap().await.unwrap();
                run_mesh_connection(conn, gate, live, registry).await;
                drop(server);
            });
        }

        let conn = client
            .connect(addr, mcpmesh_net::ALPN_MCP)
            .await
            .expect("dial");
        let (mut send, recv) = conn.open_bi().await.unwrap();
        mcpmesh_net::framing::write_frame(
            &mut send,
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "_meta":{"mcpmesh/service":"private"},"capabilities":{}}}),
        )
        .await
        .unwrap();
        let mut transport = SessionTransport::new(recv, send, mcpmesh_net::MAX_FRAME_BYTES);

        // (1) The session has snapshotted the registry (still admitting alice) and is parked in
        // its per-session resolve.
        tokio::task::spawn_blocking(move || {
            entered_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("the session reaches its per-session resolve")
        })
        .await
        .unwrap();

        // (2) The sever, with the swap performed UNDER its registry lock after the session has
        // been released and has had time to reach `admit_session`.
        let severed = {
            let (live, registry) = (live.clone(), registry.clone());
            tokio::task::spawn_blocking(move || {
                let released = Mutex::new(Some(release_tx));
                registry.sever_matching_admitted(
                    mcpmesh_net::CLOSE_UNAUTHORIZED,
                    b"access revoked",
                    |_| {
                        if let Some(tx) = released.lock().unwrap().take() {
                            tx.send(()).unwrap();
                            // Bounded wait for the session to finish its pre-admit work and block on
                            // this lock. Correct code refuses regardless of this timing; it only
                            // decides whether a value computed before the lock is caught.
                            std::thread::sleep(Duration::from_millis(500));
                            live.store(services(&[]));
                        }
                        false
                    },
                    |p| p == PRINCIPAL,
                )
            })
            .await
            .unwrap()
        };
        assert_eq!(
            severed, 0,
            "setup: the sever must find nothing — the session was not yet admitted"
        );
        assert!(conn.close_reason().is_none(), "setup: nothing was severed");

        // (3) The session admits against the LIVE registry and is refused.
        let reply = tokio::time::timeout(Duration::from_secs(10), transport.recv_value())
            .await
            .expect("the session answers")
            .expect("transport ok")
            .expect("a frame");
        assert_eq!(
            reply["error"]["code"], -32054,
            "a session whose snapshot predates a revoke the sever could not see must be refused by \
             the admit recheck under the registry lock: {reply}"
        );
    })
    .await
    .expect("admit recheck test timed out");
}
