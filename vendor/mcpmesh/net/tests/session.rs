//! Session acceptance: a full initialize + echo round-trip through the
//! framed session; -32054 for an unknown/unauthorized service; and a stranger
//! refused by the gate before any MCP frame is exchanged.
//!
//! In-process, localhost only: `presets::Minimal` + `RelayMode::Disabled` (no
//! external network). The two-machines-on-different-NATs half is the `#[ignore]`d
//! runbook at the bottom. Each test body runs under a 30s timeout so a
//! composition hang fails cleanly instead of burning the CI budget.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use mcpmesh_net::{
    ConnRegistry, EndpointId, PeerIdentity, ServiceEntry, SessionBackend, SessionTransport,
    StaticGate, connect, serve,
};
use serde_json::json;

/// In-process backend: replies to `initialize`, then echoes `tools/call`.
struct EchoBackend;

#[async_trait::async_trait]
impl SessionBackend for EchoBackend {
    async fn run(
        &self,
        // The in-process backend ignores the injected identity (env/`_meta`
        // injection is the spawn/socket backends' concern); it is threaded
        // through the trait so every backend gets the per-caller identity.
        _identity: Option<PeerIdentity>,
        initialize: serde_json::Value,
        mut transport: SessionTransport,
    ) -> anyhow::Result<()> {
        // End-to-end proof: the reserved key was stripped before the
        // backend ever saw the frame.
        assert!(
            initialize
                .pointer("/params/_meta/mcpmesh~1service")
                .is_none(),
            "reserved _meta key reached the backend: {initialize}"
        );
        let id = initialize["id"].clone();
        transport
            .send_value(json!({"jsonrpc":"2.0","id":id,"result":{
                "protocolVersion":"2025-11-25","capabilities":{},
                "serverInfo":{"name":"echo-stub","version":"0.1.0"}}}))
            .await?;
        while let Some(frame) = transport.recv_value().await? {
            if frame["method"] == "tools/call" {
                let echoed = frame["params"]["arguments"]["text"].clone();
                transport
                    .send_value(json!({"jsonrpc":"2.0","id":frame["id"],
                        "result":{"content":[{"type":"text","text": echoed}]}}))
                    .await?;
            }
        }
        Ok(())
    }
}

/// A localhost-only iroh endpoint carrying the mcpmesh/mcp/1 ALPN.
async fn test_endpoint() -> anyhow::Result<iroh::Endpoint> {
    Ok(iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .alpns(vec![mcpmesh_net::ALPN_MCP.to_vec()])
        .bind()
        .await?)
}

/// A one-service registry: `name`, an `EchoBackend`, and the `allow` list that
/// admits callers to it. `allow` holds STABLE principals (#38): `eid:` device
/// principals, `b64u:` user_ids, or bare roster vocabulary — never nicknames.
fn service_with_allow(name: &str, allow: Vec<String>) -> mcpmesh_net::Services {
    let mut services = HashMap::new();
    services.insert(
        name.to_string(),
        ServiceEntry {
            backend: Arc::new(EchoBackend) as Arc<dyn SessionBackend>,
            allow,
            kind: mcpmesh_net::ServiceKind::Run,
            ephemeral: false,
        },
    );
    mcpmesh_net::Services::new(services)
}

/// The `echo` service, admitting the given client endpoint by its stable `eid:`
/// device principal (#38). The gate's nickname is display-only and never admits,
/// so the endpoint must be bound FIRST and its eid formatted into the allow list.
fn echo_services(client: &iroh::Endpoint) -> mcpmesh_net::Services {
    service_with_allow("echo", vec![EndpointId::from(client.id()).principal()])
}

#[tokio::test]
async fn known_peer_completes_initialize_and_echo() -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let server = test_endpoint().await?;
        let client = test_endpoint().await?;
        let client_id = EndpointId::from(client.id());
        // Nickname "bob" is display-only; the service's allow (below) admits the
        // client by its eid: principal, the stable authz namespace (#38).
        let gate = Arc::new(StaticGate::new([(
            client_id,
            PeerIdentity::nickname("bob"),
        )]));
        let addr = server.addr();
        let services = echo_services(&client);
        let _handle = serve(server, gate, services, Arc::new(ConnRegistry::new()));

        let mut transport = connect(&client, addr, "echo").await?.0;
        transport
            .send_value(
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-11-25",
                "_meta": {"mcpmesh/service": "echo"},
                "capabilities":{}, "clientInfo":{"name":"test","version":"0"}}}),
            )
            .await?;
        let init_res = transport.recv_value().await?.unwrap();
        assert_eq!(init_res["result"]["serverInfo"]["name"], "echo-stub");

        transport
            .send_value(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"echo","arguments":{"text":"over the mesh"}}}))
            .await?;
        let call_res = transport.recv_value().await?.unwrap();
        assert_eq!(call_res["result"]["content"][0]["text"], "over the mesh");
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn unknown_service_gets_32054_with_marker() -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let server = test_endpoint().await?;
        let client = test_endpoint().await?;
        let client_id = EndpointId::from(client.id());
        let gate = Arc::new(StaticGate::new([(
            client_id,
            PeerIdentity::nickname("bob"),
        )]));
        let addr = server.addr();
        let services = echo_services(&client);
        let _handle = serve(server, gate, services, Arc::new(ConnRegistry::new()));

        let mut transport = connect(&client, addr, "nope").await?.0;
        transport
            .send_value(
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "_meta": {"mcpmesh/service": "nope"}, "capabilities":{}}}),
            )
            .await?;
        let err = transport.recv_value().await?.unwrap();
        assert_eq!(err["error"]["code"], -32054);
        assert_eq!(err["error"]["data"]["source"], "mcpmesh");
        Ok(())
    })
    .await?
}

#[tokio::test]
async fn unknown_endpoint_is_refused_before_mcp() -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let server = test_endpoint().await?;
        let stranger = test_endpoint().await?;
        let gate = Arc::new(StaticGate::new([])); // nobody is trusted
        let addr = server.addr();
        let _handle = serve(
            server,
            gate,
            mcpmesh_net::Services::new(HashMap::new()),
            Arc::new(ConnRegistry::new()),
        );

        // The gate must close the connection: either `connect` itself errors, or
        // the first read returns closed — but no MCP frame is ever exchanged.
        match connect(&stranger, addr, "echo").await.map(|(t, _)| t) {
            Err(_) => {}
            Ok(mut transport) => {
                let outcome = transport.recv_value().await;
                assert!(
                    matches!(outcome, Err(_) | Ok(None)),
                    "stranger got a frame: {outcome:?}"
                );
            }
        }
        Ok(())
    })
    .await?
}

/// Per-service `allow` is enforced, not just registry membership: `notes` allows
/// only the bare roster name `"bob"` (still-valid roster vocabulary under #38),
/// but the connecting peer's principal set is {its eid:} only — trusted enough to
/// pass the gate, yet not admitted by `notes`. It must get the same
/// `-32054`/`data.source: "mcpmesh"` refusal an unknown service gets (the
/// indistinguishability rule). The gate deliberately resolves the peer to nickname
/// `"bob"` — the SAME string the allow list holds — proving the display nickname
/// NEVER admits (#38): only eid/user_id/groups match, and this identity has none
/// of those equal to "bob".
#[tokio::test]
async fn peer_not_in_service_allow_is_refused() -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let server = test_endpoint().await?;
        let client = test_endpoint().await?;
        let client_id = EndpointId::from(client.id());
        // Trusted at the gate, nicknamed "bob" — but nickname is display-only, and
        // neither the eid nor a user_id/group matches `notes`' allow, so
        // `caller_allowed` for this peer is empty.
        let gate = Arc::new(StaticGate::new([(
            client_id,
            PeerIdentity::nickname("bob"),
        )]));
        let addr = server.addr();
        let _handle = serve(
            server,
            gate,
            service_with_allow("notes", vec!["bob".into()]),
            Arc::new(ConnRegistry::new()),
        );

        let mut transport = connect(&client, addr, "notes").await?.0;
        transport
            .send_value(
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "_meta": {"mcpmesh/service": "notes"}, "capabilities":{}}}),
            )
            .await?;
        let err = transport.recv_value().await?.unwrap();
        assert_eq!(err["error"]["code"], -32054);
        assert_eq!(err["error"]["data"]["source"], "mcpmesh");
        Ok(())
    })
    .await?
}

// ─────────────────────────────────────────────────────────────────────────────
// Two-machine real-NAT smoke — the one clause CI cannot exercise:
// "two machines on different NATs complete an MCP session over mcpmesh/mcp/1".
// BOTH tests are `#[ignore]`d so CI never runs them
// (one hangs 10 min by design; both need two machines). Drive them by hand,
// across two machines on different NATs (a maintainer release-validation step).
//
// They reuse the EchoBackend + initialize/echo path of
// `known_peer_completes_initialize_and_echo` above; only the network and the
// identity passing differ. Reconcile notes (iroh 1.0.1), declared here and in the
// runbook:
//   • Relay + discovery: these endpoints bind `presets::N0` (n0 public relays +
//     DNS/pkarr address lookup) — real NAT traversal — NOT the localhost
//     `Minimal` + `RelayMode::Disabled` that `test_endpoint` uses. Set
//     MCPMESH_SMOKE_RELAY=<url> (e.g. https://relay.runbolo.com) to override the
//     relay via `RelayMode::custom`.
//   • Address passing: `iroh::EndpointAddr` derives serde (id + a set of
//     relay/ip TransportAddrs), so `two_machine_serve` serializes its addr to
//     JSON and prints one `MCPMESH_SMOKE_PEER=<json>` line; `two_machine_connect`
//     deserializes it from the MCPMESH_SMOKE_PEER env. No node-ticket type is
//     needed — JSON round-trips cleanly.
//   • Gate pinning: the server StaticGate resolves the CONNECTOR's EndpointId
//     (default-deny), so the connector binds a STABLE secret key (the fixed dev seed
//     below, overridable via MCPMESH_SMOKE_SECRET=<64 hex>) → its EndpointId is
//     constant and pre-shareable. `two_machine_serve` allows
//     MCPMESH_SMOKE_ALLOW=<EndpointId> if set, else the built-in connector identity
//     (printed so a different peer can be pinned).
//   • Relay-forced variant: MCPMESH_SMOKE_FORCE_RELAY=1 on the connect side strips
//     direct (Ip) addrs so the session establishes over the relay path; iroh may
//     still upgrade to direct via hole-punching afterward — the PASS line reports
//     the path actually used (from `Endpoint::remote_info`).
// ─────────────────────────────────────────────────────────────────────────────

/// Fixed dev identity for the connector (NOT secret) so its EndpointId is stable
/// across runs and can be pre-shared to the serve side's StaticGate. Override with
/// `MCPMESH_SMOKE_SECRET=<64 hex>` for a fresh identity.
const SMOKE_CONNECTOR_SEED: [u8; 32] = *b"mcpmesh-dev-smoke-connector-seed";

/// The connector's iroh secret key: `MCPMESH_SMOKE_SECRET` (64 hex) if set, else the
/// fixed dev seed. A stable key lets the serve side pin this identity in advance.
fn smoke_connector_key() -> anyhow::Result<iroh::SecretKey> {
    match std::env::var("MCPMESH_SMOKE_SECRET") {
        Ok(hex) if !hex.trim().is_empty() => hex.trim().parse::<iroh::SecretKey>().map_err(|e| {
            anyhow::anyhow!(
                "MCPMESH_SMOKE_SECRET must be an iroh secret key (base32 or lowercase hex): {e}"
            )
        }),
        _ => Ok(iroh::SecretKey::from_bytes(&SMOKE_CONNECTOR_SEED)),
    }
}

/// A relay+discovery endpoint for real-NAT traversal (`presets::N0`), carrying the
/// mcpmesh/mcp/1 ALPN. Contrast `test_endpoint` (localhost, relay disabled). An
/// optional fixed `secret` pins the identity; `MCPMESH_SMOKE_RELAY` overrides the
/// relay with a custom map.
async fn smoke_endpoint(secret: Option<iroh::SecretKey>) -> anyhow::Result<iroh::Endpoint> {
    let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .alpns(vec![mcpmesh_net::ALPN_MCP.to_vec()]);
    if let Some(sk) = secret {
        builder = builder.secret_key(sk);
    }
    if let Ok(url) = std::env::var("MCPMESH_SMOKE_RELAY")
        && !url.trim().is_empty()
    {
        let relay = url
            .trim()
            .parse::<iroh::RelayUrl>()
            .map_err(|e| anyhow::anyhow!("MCPMESH_SMOKE_RELAY is not a valid URL: {e}"))?;
        builder = builder.relay_mode(iroh::RelayMode::custom([relay]));
    }
    Ok(builder.bind().await?)
}

/// Serve half of the two-machine smoke (machine A). Binds a relay+discovery
/// endpoint, pins the connector's EndpointId in a StaticGate, prints its
/// EndpointAddr as one parseable `MCPMESH_SMOKE_PEER=<json>` line, and serves
/// EchoBackend for ~10 minutes. Runs 10 min by design — no wrapping timeout.
#[tokio::test]
#[ignore = "two-machine NAT smoke: run manually across two machines on different NATs"]
async fn two_machine_serve() -> anyhow::Result<()> {
    // Ephemeral server identity — it travels to the peer inside the printed addr.
    let server = smoke_endpoint(None).await?;

    // The StaticGate must resolve the connector by its EndpointId (default-deny).
    // Pin MCPMESH_SMOKE_ALLOW if set, else the built-in dev connector id.
    let allow_id = match std::env::var("MCPMESH_SMOKE_ALLOW") {
        Ok(s) if !s.trim().is_empty() => s
            .trim()
            .parse::<iroh::EndpointId>()
            .map_err(|e| anyhow::anyhow!("MCPMESH_SMOKE_ALLOW is not an EndpointId: {e}"))?,
        _ => {
            let default = smoke_connector_key()?.public();
            eprintln!(
                "MCPMESH_SMOKE_ALLOW unset — allowing the built-in dev connector identity:\n  \
                 {default}\n(set MCPMESH_SMOKE_ALLOW=<connector EndpointId> to pin a different peer.)"
            );
            default
        }
    };
    let allow_bytes = EndpointId::from(allow_id);
    let gate = Arc::new(StaticGate::new([(
        allow_bytes,
        PeerIdentity::nickname("smoke-peer"),
    )]));

    // Best-effort: wait for a relay connection so the printed addr carries a relay
    // URL for the relay-forced variant. Discovery would resolve it either way.
    let _ = tokio::time::timeout(Duration::from_secs(30), server.online()).await;
    let addr = server.addr();
    let addr_json = serde_json::to_string(&addr)?;
    // ONE parseable line — the connect side reads this into MCPMESH_SMOKE_PEER.
    println!("MCPMESH_SMOKE_PEER={addr_json}");
    eprintln!(
        "serving EchoBackend for 10 minutes on ALPN mcpmesh/mcp/1 (this endpoint id: {}); \
         Ctrl-C to stop early.",
        addr.id
    );

    // The service's own allow list must admit the connector by its STABLE `eid:`
    // principal (#38) — the "smoke-peer" nickname above is display-only and never
    // admits — or every real dial is refused
    // `-32054 unknown or unauthorized service` before ever reaching `EchoBackend`.
    // `allow_bytes` is the pinned connector identity, so its principal is exactly
    // what the gate will resolve and `caller_admits` will match.
    let services = service_with_allow("echo", vec![allow_bytes.principal()]);
    let _handle = serve(server, gate, services, Arc::new(ConnRegistry::new()));
    tokio::time::sleep(Duration::from_secs(600)).await;
    Ok(())
}

/// Connect half of the two-machine smoke (machine B). Reads the serve side's
/// serialized EndpointAddr from `MCPMESH_SMOKE_PEER`, dials it, and runs the SAME
/// initialize + `tools/call` echo assertions as
/// `known_peer_completes_initialize_and_echo`, asserting a byte-faithful echo.
#[tokio::test]
#[ignore = "two-machine NAT smoke: run manually across two machines on different NATs"]
async fn two_machine_connect() -> anyhow::Result<()> {
    let raw = std::env::var("MCPMESH_SMOKE_PEER").map_err(|_| {
        anyhow::anyhow!(
            "MCPMESH_SMOKE_PEER unset. Run two_machine_serve on the peer machine and export the \
             `MCPMESH_SMOKE_PEER=<json>` line it prints."
        )
    })?;
    // Tolerate the operator pasting the whole `MCPMESH_SMOKE_PEER=…` line or just the JSON.
    let json_str = raw
        .trim()
        .strip_prefix("MCPMESH_SMOKE_PEER=")
        .unwrap_or(raw.trim());
    let mut peer: iroh::EndpointAddr = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("MCPMESH_SMOKE_PEER is not a serialized EndpointAddr: {e}"))?;

    // Relay-forced variant: strip direct (Ip) addrs so the session establishes over
    // the relay path. iroh may still upgrade to direct via hole-punching.
    if std::env::var("MCPMESH_SMOKE_FORCE_RELAY").is_ok_and(|v| !v.trim().is_empty()) {
        peer.addrs.retain(|a| !a.is_ip());
        eprintln!("MCPMESH_SMOKE_FORCE_RELAY set — stripped direct addrs; dialing relay-only.");
    }

    let connector = smoke_connector_key()?;
    // On the zero-config path both sides share the built-in dev identity, so no
    // manual pin is needed; the hint only applies when a fresh key was supplied.
    if std::env::var("MCPMESH_SMOKE_SECRET").is_ok_and(|v| !v.trim().is_empty()) {
        eprintln!(
            "this connector's EndpointId (set MCPMESH_SMOKE_ALLOW to this on the serve machine):\n  {}",
            connector.public()
        );
    }
    let client = smoke_endpoint(Some(connector)).await?;
    let peer_id = peer.id;

    // A byte-faithful payload: unicode + escaped quote/newline/tab, proving the echo
    // round-trips exactly, not just ASCII.
    let payload = "over the real mesh — café 🌐\n\"quoted\"\ttab";

    // 60s timeout: a NAT/relay failure fails cleanly instead of hanging.
    tokio::time::timeout(Duration::from_secs(60), async {
        // SAME initialize + tools/call echo assertions as
        // known_peer_completes_initialize_and_echo, over the real network.
        let mut transport = connect(&client, peer, "echo").await?.0;
        transport
            .send_value(
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-11-25",
                "_meta": {"mcpmesh/service": "echo"},
                "capabilities":{}, "clientInfo":{"name":"smoke","version":"0"}}}),
            )
            .await?;
        let init_res = transport.recv_value().await?.unwrap();
        assert_eq!(init_res["result"]["serverInfo"]["name"], "echo-stub");

        transport
            .send_value(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"echo","arguments":{"text": payload}}}))
            .await?;
        let call_res = transport.recv_value().await?.unwrap();
        assert_eq!(
            call_res["result"]["content"][0]["text"], payload,
            "echo was not byte-faithful across the mesh"
        );
        anyhow::Ok(())
    })
    .await??;

    // Report the path actually used (direct hole-punched vs relayed).
    let path = match client.remote_info(peer_id).await {
        Some(info) => {
            let active: Vec<String> = info
                .addrs()
                .filter(|a| matches!(a.usage(), iroh::endpoint::TransportAddrUsage::Active))
                .map(|a| a.addr().to_string())
                .collect();
            let verdict = if active.iter().any(|a| a.starts_with("ip:")) {
                "DIRECT"
            } else if active.iter().any(|a| a.starts_with("relay:")) {
                "RELAY"
            } else {
                "UNKNOWN"
            };
            format!("{verdict} (active addrs: {active:?})")
        }
        None => "UNKNOWN (no remote_info)".to_string(),
    };
    println!("PASS — initialize + byte-faithful echo completed over mcpmesh/mcp/1. path={path}");
    Ok(())
}
