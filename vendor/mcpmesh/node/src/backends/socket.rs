//! The `socket` backend: the daemon dials a long-running local
//! MCP server on its local endpoint (a UDS path on unix; a `\\.\pipe\…` name on
//! windows) per session and injects the resolved caller identity into the forwarded
//! `initialize` as `_meta["mcpmesh/peer"]`.
//!
//! The injection is AUTHORITATIVE — it REPLACES, never merges. The value the
//! backend writes is the daemon-authored one; a caller-forged `_meta["mcpmesh/peer"]`
//! must never survive. `select_service` strips the caller's `mcpmesh/*` keys from the
//! session's first frame, and the pump's sanitizer strips them from EVERY later frame
//! and re-attributes a later `initialize` (#164) — so this REPLACE is both
//! defense-in-depth AND the single authoritative source of the value the warm server
//! trusts. Before #164 the strip ran on frame 1 alone, and `run_session` treats frame 1
//! as the handshake whatever its method is, so a forged identity rode in on frame 2.
//!
//! Unlike the `run` backend, the server is a warm, shared process (e.g. a
//! plugin daemon), so identity cannot travel as per-process env vars; it rides in
//! the MCP `initialize` `_meta` instead. Once the socket is dialed and the
//! augmented `initialize` sent, the session is the SAME bidirectional frame pump the
//! spawn backend uses (`super::pump`, one codec).
use anyhow::{Context, Result};
use mcpmesh_net::transport::NdjsonTransport;
use mcpmesh_net::{PeerIdentity, SessionBackend, SessionTransport};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::audit::{AuditSink, RequestAuditor};

/// The `socket` backend for one registered service. `path` is the long-running
/// local MCP server's endpoint (a UDS path on unix; a `\\.\pipe\…` name on windows),
/// dialed fresh per session. The caller
/// identity is NOT a field — it is threaded per-session through
/// [`SessionBackend::run`] (`Some` iff the peer resolved through the trust gate),
/// because `serve` shares this backend across all callers.
///
/// No per-session concurrency cap (conscious decision): the cap scopes to
/// `run` (spawn) backends — one child per session, so unbounded sessions mean
/// unbounded processes. A `socket` backend dials ONE long-running server that
/// multiplexes its own sessions, so the daemon adds no per-session limit here; the
/// general per-identity defense against abuse is the request token bucket. Hence no
/// `concurrency` field, unlike [`super::spawn::SpawnBackend`].
pub struct SocketBackend {
    pub path: String,
    /// This service's name (the registry key) — recorded as `service` in audit records.
    pub service: String,
    /// The audit sink. `AuditSink::disabled()` in tests / a non-audited build.
    pub audit: AuditSink,
    /// The request limiter for THIS service (#63): per `(service, endpoint)`, not shared across backends.
    /// Consulted per proxied request line in `super::pump`. Keyed on `identity.endpoint`.
    pub limiter: std::sync::Arc<crate::limits::RateLimiter>,
}

#[async_trait::async_trait]
impl SessionBackend for SocketBackend {
    /// Drive one mesh session against a freshly dialed local MCP server. As with the
    /// spawn backend, the concrete `SessionTransport` alias is iroh-typed, so the
    /// dial + inject + pump body lives in the transport-generic [`SocketBackend::run_over`]
    /// so it is exercisable over an in-memory pipe + a stub UDS server in tests.
    async fn run(
        &self,
        identity: Option<PeerIdentity>,
        initialize: Value,
        transport: SessionTransport,
    ) -> anyhow::Result<()> {
        self.run_over(identity, initialize, transport).await
    }
}

impl SocketBackend {
    /// The transport-generic core of [`SessionBackend::run`]: dial the server's local
    /// endpoint (UDS on unix; named pipe on windows), inject the daemon-authored identity
    /// into the `initialize` `_meta`
    /// authoritatively (REPLACE-not-merge), then pump frames both ways until either
    /// side ends. Generic over the transport's byte substrate so the real path (iroh
    /// streams) and the test path (`tokio::io::duplex`) share one implementation.
    pub async fn run_over<R, W>(
        &self,
        identity: Option<PeerIdentity>,
        mut initialize: Value,
        mut transport: NdjsonTransport<R, W>,
    ) -> Result<()>
    where
        R: AsyncRead + Send + Unpin,
        W: AsyncWrite + Send + Unpin,
    {
        let server = mcpmesh_local_api::transport::connect_local(std::path::Path::new(&self.path))
            .await
            .with_context(|| format!("dial socket backend at {}", self.path))?;

        if let Some(id) = &identity {
            // ONE implementation of the identity rule, shared with every later frame (#45 gate).
            //
            // This was an inline copy until 0.50.0, and the two had drifted in three ways: it
            // invented a `params._meta` on a RESPONSE (malformed JSON-RPC), it discarded a batch
            // wholesale instead of descending it, and it attributed a frame whose `method` was not
            // a string. Two implementations of one identity rule is precisely the #164 drift class
            // this file exists to prevent, and the divergence was invisible because only frames
            // 2..N were tested against `inject_peer`.
            //
            // Whole-value overwrite, forged `groups`/`user_id` dropped rather than merged — that
            // property lives in `inject_peer` now, asserted once.
            super::inject_peer(&mut initialize, &peer_meta_value(id), 0);
        }
        // Built once and handed to the pump so a LATER frame that turns out to be the real
        // `initialize` gets the SAME authoritative value (#164). One constructor, so the
        // first-frame and later-frame paths cannot drift — that drift was the bug.
        let peer_meta = identity.as_ref().map(peer_meta_value);
        #[allow(clippy::items_after_statements)]
        fn peer_meta_value(id: &PeerIdentity) -> serde_json::Value {
            serde_json::json!({
                // The stable device principal (`eid:<hex>` of the AUTHENTICATED endpoint
                // id, #38) — the arm `peer_audiences` matches; `name` below is display-only.
                "eid": id.endpoint.principal(),
                "name": id.name,
                // The peer's self-sovereign user_id: the org roster value in roster mode, else the
                // one proven by a verified device->user binding at pairing (null only for a pairing
                // peer that presented no binding). A service (e.g. kb) may key an audience on it so
                // all of a person's devices resolve together — forge-proof, since this whole object
                // is authoritatively OVERWRITTEN here, never merged over a caller-forged one.
                "user_id": id.user_id,
                "groups": id.groups,
            })
        }

        // Session lifecycle audit: attribute to the gate-resolved identity — the roster
        // user_id when present, else the nickname (endpoint_id-keyed authenticated name, never a
        // self-asserted one). `None` only on a hypothetical no-identity path.
        let peer = identity
            .as_ref()
            .map(|id| id.user_id.clone().unwrap_or_else(|| id.name.clone()));
        // Session lifecycle via the RAII guard: it emits `session_open` now and, on drop (every exit
        // path — EOF, error, panic), emits `session_close` and removes the live-table row. Held for
        // the whole session scope, so it MUST outlive the pump below.
        let _session = self.audit.session(
            peer.clone().unwrap_or_default(),
            self.service.clone(),
            // #73: the STABLE device principal, so a live-session row is keyed on something
            // that cannot collide. `peer` above is a display name and two devices can share it.
            super::session_principal(identity.as_ref()),
        );
        // The per-request-line auditor: hashes each caller request's args and correlates
        // the response. Threaded into the shared pump.
        let auditor = RequestAuditor::new(
            self.audit.clone(),
            peer.clone(),
            self.service.clone(),
            // #57: the same stable principal the session guard stamps — every proxied line of
            // this session joins to policy without nickname matching.
            super::session_principal(identity.as_ref()),
        );

        // Split the endpoint: the read half is consumed by the pump's outbound direction
        // (its FrameReader), the write half by the inbound direction. Both are owned
        // by their respective concurrent loops; on EOF they drop here, closing the
        // connection to the server.
        let (server_read, server_write) = mcpmesh_local_api::transport::split_local(server);
        let outcome = super::pump(
            initialize,
            &mut transport,
            server_read,
            server_write,
            auditor,
            crate::limits::RateGate::new(
                self.limiter.clone(),
                identity.as_ref().map(|i| i.endpoint),
            ),
            peer_meta,
        )
        .await;
        // `_session` drops here (or on any early return above), emitting `session_close`.
        outcome
    }
}

// Unix-only: every stub MCP server binds a raw `UnixListener` and uses `into_split()`.
// Production `SocketBackend` dials through the cross-platform seam
// (`transport::connect_local`); these fixtures exercise it against a UDS stub. On the
// windows CI leg the seam's pipe path is covered by `transport::windows`' own tests.
#[cfg(all(test, unix))]
mod tests {
    use std::time::Duration;

    use mcpmesh_net::PeerIdentity;
    use mcpmesh_net::framing::{FrameReader, Inbound, write_frame};
    use serde_json::{Value, json};
    use tokio::io::{BufReader, duplex, split};
    use tokio::net::UnixListener;
    use tokio::time::timeout;

    use super::SocketBackend;

    const MAX_FRAME: usize = 16 * 1024 * 1024;

    /// A stub long-running MCP server on a UDS: accept one connection, read the
    /// `initialize`, reply with an InitializeResult, then echo a single `tools/call`
    /// (mirroring the frame's `text` back). Returns the observed `initialize` so the
    /// test can assert what the backend forwarded (the injected identity, or verbatim
    /// passthrough when identity is None). The whole exchange completes before the
    /// returned value resolves, so awaiting the handle sequences correctly.
    async fn stub_server(listener: UnixListener) -> Value {
        let (stream, _) = listener.accept().await.expect("stub accept");
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = FrameReader::new(BufReader::new(read_half), MAX_FRAME);

        let init = match reader
            .next()
            .await
            .expect("read initialize")
            .expect("initialize frame")
        {
            Inbound::Frame(v) => v,
            Inbound::Violation(_) => panic!("stub saw a framing violation on initialize"),
        };
        write_frame(
            &mut write_half,
            &json!({
                "jsonrpc": "2.0", "id": init["id"].clone(),
                "result": {"serverInfo": {"name": "socket-stub"}}
            }),
        )
        .await
        .expect("reply initialize");

        let call = match reader
            .next()
            .await
            .expect("read tools/call")
            .expect("tools/call frame")
        {
            Inbound::Frame(v) => v,
            Inbound::Violation(_) => panic!("stub saw a framing violation on tools/call"),
        };
        write_frame(
            &mut write_half,
            &json!({
                "jsonrpc": "2.0", "id": call["id"].clone(),
                "result": {"content": [{"text": call["params"]["arguments"]["text"].clone()}]}
            }),
        )
        .await
        .expect("echo tools/call");

        init
    }

    /// Collect `n` frames, replying to each so the pump stays live, and return them all.
    async fn stub_collecting(listener: UnixListener, n: usize) -> Vec<Value> {
        let (stream, _) = listener.accept().await.expect("stub accept");
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = FrameReader::new(BufReader::new(read_half), MAX_FRAME);
        let mut seen = Vec::new();
        for _ in 0..n {
            let f = match reader.next().await.expect("read frame").expect("frame") {
                Inbound::Frame(v) => v,
                Inbound::Violation(_) => panic!("stub saw a framing violation"),
            };
            write_frame(
                &mut write_half,
                &json!({
                    "jsonrpc": "2.0", "id": f["id"].clone(),
                    "result": {"serverInfo": {"name": "socket-stub"}}
                }),
            )
            .await
            .expect("reply");
            seen.push(f);
        }
        seen
    }

    /// #164: the reserved-namespace rule held on the FIRST frame only, and `run_session` treats
    /// frame 1 as `initialize` whatever its method is. So a caller spends frame 1 on a `ping` —
    /// which the MCP lifecycle permits pre-initialize — and sends its real `initialize` as frame 2,
    /// which reached the backend verbatim. A shared server reading `_meta["mcpmesh/peer"]` out of
    /// `initialize` saw whatever the caller wrote.
    #[tokio::test]
    async fn a_later_initialize_cannot_carry_a_forged_peer() {
        timeout(Duration::from_secs(30), async {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("server.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            let stub = tokio::spawn(stub_collecting(listener, 2));

            let (server_io, client_io) = duplex(64 * 1024);
            let (sr, sw) = split(server_io);
            let backend_transport = mcpmesh_net::transport::NdjsonTransport::new(sr, sw, MAX_FRAME);
            let (cr, cw) = split(client_io);
            let mut client = mcpmesh_net::transport::NdjsonTransport::new(cr, cw, MAX_FRAME);

            let backend = SocketBackend {
                path: sock.to_str().unwrap().to_string(),
                service: "test".into(),
                audit: crate::audit::AuditSink::disabled(),
                limiter: crate::limits::RateLimiter::unlimited_shared(),
            };
            let identity = Some(PeerIdentity {
                endpoint: [0u8; 32].into(),
                name: "bob".into(),
                user_id: None,
                groups: vec![],
            });

            // Frame 1 is NOT an initialize. This is what `run_session` hands the backend.
            let frame_one = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"});
            let session = tokio::spawn(async move {
                backend
                    .run_over(identity, frame_one, backend_transport)
                    .await
            });

            let _ = client.recv_value().await.unwrap().unwrap();

            // Frame 2 is the REAL initialize, carrying a forged peer naming someone else —
            // including forged AUTHORIZATION fields.
            client
                .send_value(json!({
                    "jsonrpc": "2.0", "id": 2, "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "_meta": {
                            "mcpmesh/peer": {
                                "eid": "eid:deadbeef", "name": "someone-else",
                                "user_id": "root", "groups": ["admin"]
                            },
                            "app/trace-id": "keep-me"
                        }
                    }
                }))
                .await
                .unwrap();
            let _ = client.recv_value().await.unwrap().unwrap();

            let seen = stub.await.unwrap();
            let real_init = &seen[1];
            assert_eq!(
                real_init["method"], "initialize",
                "frame 2 is the handshake"
            );
            let peer = &real_init["params"]["_meta"]["mcpmesh/peer"];
            assert_eq!(
                peer["name"], "bob",
                "the backend must see the AUTHORITATIVE identity on the frame it handshakes on, \
                 not the caller's forgery: {real_init}"
            );
            assert_eq!(
                peer["groups"],
                json!([]),
                "forged groups must not survive on a later frame either"
            );
            assert_eq!(
                peer["user_id"],
                json!(null),
                "forged user_id must not survive"
            );
            assert_eq!(
                real_init["params"]["_meta"]["app/trace-id"], "keep-me",
                "and a NON-reserved key must survive — this is a reserved-prefix strip, not an \
                 _meta eraser"
            );

            drop(client);
            let _ = session.await.unwrap();
        })
        .await
        .expect("test timed out");
    }

    /// #164: `mcpmesh/service` is the key `select_service` ACTS on, so a forged one on a later
    /// frame is authorization-relevant, not cosmetic. And the strip must be a reserved-PREFIX
    /// strip, not an `_meta` eraser — an application's own keys are the reason `_meta` exists.
    #[tokio::test]
    async fn later_frames_lose_reserved_keys_and_keep_everything_else() {
        timeout(Duration::from_secs(30), async {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("server.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            let stub = tokio::spawn(stub_collecting(listener, 2));

            let (server_io, client_io) = duplex(64 * 1024);
            let (sr, sw) = split(server_io);
            let backend_transport = mcpmesh_net::transport::NdjsonTransport::new(sr, sw, MAX_FRAME);
            let (cr, cw) = split(client_io);
            let mut client = mcpmesh_net::transport::NdjsonTransport::new(cr, cw, MAX_FRAME);

            let backend = SocketBackend {
                path: sock.to_str().unwrap().to_string(),
                service: "test".into(),
                audit: crate::audit::AuditSink::disabled(),
                limiter: crate::limits::RateLimiter::unlimited_shared(),
            };
            let identity = Some(PeerIdentity {
                endpoint: [0u8; 32].into(),
                name: "bob".into(),
                user_id: None,
                groups: vec![],
            });
            let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
            let session =
                tokio::spawn(
                    async move { backend.run_over(identity, init, backend_transport).await },
                );
            let _ = client.recv_value().await.unwrap().unwrap();

            client
                .send_value(json!({
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": {"_meta": {
                        "mcpmesh/service": "some-other-service",
                        "mcpmesh/anything": "reserved",
                        "app/trace-id": "keep-me",
                        "_private": {"nested": true}
                    }}
                }))
                .await
                .unwrap();
            let _ = client.recv_value().await.unwrap().unwrap();

            let seen = stub.await.unwrap();
            let meta = &seen[1]["params"]["_meta"];
            assert!(
                meta.get("mcpmesh/service").is_none(),
                "a forged mcpmesh/service must not reach the backend on a LATER frame — it is the \
                 key select_service acts on: {meta}"
            );
            assert!(
                meta.get("mcpmesh/anything").is_none(),
                "the whole reserved PREFIX is stripped, not an enumerated list: {meta}"
            );
            assert_eq!(
                meta["app/trace-id"], "keep-me",
                "a non-reserved key must survive: {meta}"
            );
            assert_eq!(
                meta["_private"]["nested"],
                json!(true),
                "and so must a nested non-reserved value: {meta}"
            );
            // #45 ask 2, AT THE CALL SITE.
            //
            // This assertion was the inverse until 0.50.0, and the #164 gate that wrote it gave a
            // reason worth answering rather than deleting: *"Attributing a `tools/call` as though it
            // were a handshake would tell a server the session re-identified mid-stream."*
            //
            // The answer: the injected value is CONSTANT for the life of the session. It is derived
            // once, from the TLS-authenticated endpoint of this connection (`peer_meta_value` is
            // called on one `PeerIdentity` and the result handed to the pump), and the identity
            // behind a QUIC connection cannot change under it. So a backend sees the same object on
            // every frame — a restatement, not a re-identification. What would signal
            // re-identification is a value that CHANGES mid-stream, which nothing here can produce.
            //
            // The objection was correct about the risk it named and did not apply to the constant
            // case; it is recorded here rather than removed, because the gate was also right that
            // this assertion is the only thing standing between the widening and a green suite.
            //
            // The identity is `PeerIdentity { name: "bob", .. }` from the session setup, so this
            // also pins that the value forwarded is the authoritative one rather than an echo.
            assert_eq!(
                meta["mcpmesh/peer"]["name"], "bob",
                "an ordinary request must reach the backend authoritatively attributed: {meta}"
            );
            assert!(
                meta["mcpmesh/peer"]["eid"].is_string(),
                "and carry the stable device principal, not just a display name: {meta}"
            );

            drop(client);
            let _ = session.await.unwrap();
        })
        .await
        .expect("test timed out");
    }

    /// #49: BOTH spellings reach a REAL backend, and the audit digest does not move because of them.
    ///
    /// The spec for 0.51.0 required an end-to-end test "since every gate in this session found a
    /// call site no test reached" — and then it was not written, and the gate found exactly that: a
    /// reader of `_meta` (`args_hash`) that the unit tests never traversed, which silently
    /// re-broke a property 0.50.0 had fixed. This is that test.
    #[tokio::test]
    async fn both_peer_spellings_reach_a_real_backend_with_a_stable_digest() {
        timeout(Duration::from_secs(30), async {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("server.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            let stub = tokio::spawn(stub_collecting(listener, 2));

            let (server_io, client_io) = duplex(64 * 1024);
            let (sr, sw) = split(server_io);
            let backend_transport = mcpmesh_net::transport::NdjsonTransport::new(sr, sw, MAX_FRAME);
            let (cr, cw) = split(client_io);
            let mut client = mcpmesh_net::transport::NdjsonTransport::new(cr, cw, MAX_FRAME);

            let backend = SocketBackend {
                path: sock.to_str().unwrap().to_string(),
                service: "test".into(),
                audit: crate::audit::AuditSink::disabled(),
                limiter: crate::limits::RateLimiter::unlimited_shared(),
            };
            let identity = Some(PeerIdentity {
                endpoint: [0u8; 32].into(),
                name: "bob".into(),
                user_id: None,
                groups: vec![],
            });
            let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
            let session =
                tokio::spawn(
                    async move { backend.run_over(identity, init, backend_transport).await },
                );
            let _ = client.recv_value().await.unwrap().unwrap();

            client
                .send_value(json!({
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": {"name": "read_file", "arguments": {"path": "/x"}}
                }))
                .await
                .unwrap();
            let _ = client.recv_value().await.unwrap().unwrap();

            let seen = stub.await.unwrap();
            let call = seen
                .iter()
                .find(|f| f["method"] == "tools/call")
                .expect("the call reached the backend");
            let meta = &call["params"]["_meta"];
            for key in mcpmesh_net::service::PEER_KEYS {
                assert_eq!(
                    meta[key]["name"], "bob",
                    "`{key}` must reach a real backend: {meta}"
                );
            }

            // The digest the audit trail would record for this frame must equal the digest of the
            // arguments the CALLER sent — the property the gate found broken, asserted against the
            // frame that actually crossed the socket rather than a hand-built one.
            assert_eq!(
                crate::audit::record::args_hash(&call["params"]),
                crate::audit::record::args_hash(
                    &json!({"name": "read_file", "arguments": {"path": "/x"}})
                ),
                "the injected identity must not move `args_hash`, under any spelling: {meta}"
            );

            drop(client);
            let _ = session.await.unwrap();
        })
        .await
        .expect("test timed out");
    }

    /// #45 gate: a NOTIFICATION and a BATCH element are attributed through the REAL backend, not
    /// only in a helper unit test.
    ///
    /// The gate measured it: deleting batch descent, or skipping notifications, each failed exactly
    /// ONE test — a `sanitize_caller_frame` unit test — while the whole socket path stayed green.
    /// Those are the two behaviours 0.50.0 newly introduced, so they are the two least defensible
    /// to leave pinned only at the helper.
    #[tokio::test]
    async fn notifications_and_batch_elements_are_attributed_through_the_backend() {
        timeout(Duration::from_secs(30), async {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("server.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            // init + notification + batch = 3 frames reach the stub.
            let stub = tokio::spawn(stub_collecting(listener, 3));

            let (server_io, client_io) = duplex(64 * 1024);
            let (sr, sw) = split(server_io);
            let backend_transport = mcpmesh_net::transport::NdjsonTransport::new(sr, sw, MAX_FRAME);
            let (cr, cw) = split(client_io);
            let mut client = mcpmesh_net::transport::NdjsonTransport::new(cr, cw, MAX_FRAME);

            let backend = SocketBackend {
                path: sock.to_str().unwrap().to_string(),
                service: "test".into(),
                audit: crate::audit::AuditSink::disabled(),
                limiter: crate::limits::RateLimiter::unlimited_shared(),
            };
            let identity = Some(PeerIdentity {
                endpoint: [0u8; 32].into(),
                name: "bob".into(),
                user_id: None,
                groups: vec![],
            });
            let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
            let session =
                tokio::spawn(
                    async move { backend.run_over(identity, init, backend_transport).await },
                );
            let _ = client.recv_value().await.unwrap().unwrap();

            // A notification: no `id`, and it carries a forged peer to prove REPLACEMENT.
            client
                .send_value(json!({
                    "jsonrpc": "2.0", "method": "notifications/progress",
                    "params": {"_meta": {"mcpmesh/peer": {"name": "attacker"}}, "pct": 50}
                }))
                .await
                .unwrap();

            // A batch whose element is an ordinary request.
            client
                .send_value(json!([
                    {"jsonrpc":"2.0","id":2,"method":"tools/call",
                     "params":{"_meta":{"mcpmesh/peer":{"name":"attacker"}}}}
                ]))
                .await
                .unwrap();

            let seen = stub.await.unwrap();
            let note = seen
                .iter()
                .find(|f| f["method"] == "notifications/progress")
                .expect("the notification reached the backend");
            assert_eq!(
                note["params"]["_meta"]["mcpmesh/peer"]["name"], "bob",
                "a notification carries no `id` and is still a request — it must be attributed, \
                 and the forged value replaced: {note}"
            );
            assert_eq!(note["params"]["pct"], 50, "its arguments survive: {note}");

            let batch = seen
                .iter()
                .find(|f| f.is_array())
                .expect("the batch reached the backend");
            assert_eq!(
                batch[0]["params"]["_meta"]["mcpmesh/peer"]["name"], "bob",
                "an ordinary request inside a batch must be attributed through the real backend, \
                 not only in a helper test: {batch}"
            );

            drop(client);
            let _ = session.await.unwrap();
        })
        .await
        .expect("test timed out");
    }

    /// #164: a frame with no `params`, or a non-object `params`/`_meta`, must survive the strip and
    /// the injection without panicking. `Value`'s `IndexMut` PANICS on a non-object base, and
    /// `select_service` already documents a non-object frame as REACHABLE (its key-absent default
    /// forwards one). A panic here is a remote crash on caller-supplied input.
    #[tokio::test]
    async fn odd_frame_shapes_do_not_panic_the_strip_or_the_injection() {
        timeout(Duration::from_secs(30), async {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("server.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            let stub = tokio::spawn(stub_collecting(listener, 5));

            let (server_io, client_io) = duplex(64 * 1024);
            let (sr, sw) = split(server_io);
            let backend_transport = mcpmesh_net::transport::NdjsonTransport::new(sr, sw, MAX_FRAME);
            let (cr, cw) = split(client_io);
            let mut client = mcpmesh_net::transport::NdjsonTransport::new(cr, cw, MAX_FRAME);

            let backend = SocketBackend {
                path: sock.to_str().unwrap().to_string(),
                service: "test".into(),
                audit: crate::audit::AuditSink::disabled(),
                limiter: crate::limits::RateLimiter::unlimited_shared(),
            };
            let identity = Some(PeerIdentity {
                endpoint: [0u8; 32].into(),
                name: "bob".into(),
                user_id: None,
                groups: vec![],
            });
            let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
            let session =
                tokio::spawn(
                    async move { backend.run_over(identity, init, backend_transport).await },
                );
            let _ = client.recv_value().await.unwrap().unwrap();

            for (n, odd) in [
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call"}), // no params at all
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":"a string"}),
                json!({"jsonrpc":"2.0","id":4,"method":"initialize","params":["an","array"]}),
                json!({"jsonrpc":"2.0","id":5,"method":"initialize","params":{"_meta":"not-obj"}}),
            ]
            .into_iter()
            .enumerate()
            {
                client.send_value(odd).await.unwrap();
                let r = client.recv_value().await.unwrap();
                assert!(
                    r.is_some(),
                    "frame {n} must be forwarded, not crash the session"
                );
            }

            let seen = stub.await.unwrap();
            // An odd-shaped frame is attributed when there is somewhere to put the annotation —
            // a malformed `_meta` inside an OBJECT params is replaced, since `_meta` is protocol
            // metadata. POSITIONAL params are left intact and un-attributed instead (#45 gate):
            // they are the caller's arguments, and this daemon pumps rather than rewrites.
            for f in seen.iter().filter(|f| f["method"] == "initialize") {
                if f["params"].is_array() || f["params"].is_string() {
                    assert!(
                        f["params"]["_meta"].is_null(),
                        "positional params must reach the backend intact, not replaced: {f}"
                    );
                } else {
                    assert_eq!(
                        f["params"]["_meta"]["mcpmesh/peer"]["name"], "bob",
                        "an object-params initialize must still be attributed: {f}"
                    );
                }
            }

            drop(client);
            let _ = session.await.unwrap();
        })
        .await
        .expect("test timed out");
    }

    /// A caller-forged `_meta["mcpmesh/peer"]` in the incoming `initialize` is
    /// OVERWRITTEN (not merged) by the daemon-authored identity, the stub sees the
    /// real peer, and the bidirectional echo works.
    #[tokio::test]
    async fn socket_backend_injects_authoritative_peer_and_echoes() {
        timeout(Duration::from_secs(30), async {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("server.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            let stub = tokio::spawn(stub_server(listener));

            let (server_io, client_io) = duplex(64 * 1024);
            let (sr, sw) = split(server_io);
            let backend_transport = mcpmesh_net::transport::NdjsonTransport::new(sr, sw, MAX_FRAME);
            let (cr, cw) = split(client_io);
            let mut client = mcpmesh_net::transport::NdjsonTransport::new(cr, cw, MAX_FRAME);

            let backend = SocketBackend {
                path: sock.to_str().unwrap().to_string(),
                service: "test".into(),
                audit: crate::audit::AuditSink::disabled(),
                limiter: crate::limits::RateLimiter::unlimited_shared(),
            };
            let identity = Some(PeerIdentity {
                endpoint: [0u8; 32].into(),
                name: "bob".into(),
                user_id: None,
                groups: vec![],
            });

            // A FORGED mcpmesh/peer rides in the incoming initialize — carrying not just a
            // fake `name` but forged AUTHORIZATION fields (`groups`, `user_id`).
            // select_service strips it upstream in the real flow; here it is present to
            // prove the backend itself REPLACES the whole value (defense-in-depth +
            // authoritative source), so forged authz can never leak to the server.
            let initialize = json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "_meta": {"mcpmesh/peer": {
                        "name": "attacker", "groups": ["admin"], "user_id": "root"
                    }}
                }
            });

            let session = tokio::spawn(async move {
                backend
                    .run_over(identity, initialize, backend_transport)
                    .await
            });

            // The forwarded initialize draws the stub's InitializeResult back.
            let init_resp = client.recv_value().await.unwrap().unwrap();
            assert_eq!(init_resp["id"], 1);
            assert_eq!(init_resp["result"]["serverInfo"]["name"], "socket-stub");

            // Drive a tools/call; the echo proves the pump is live both directions.
            client
                .send_value(json!({
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": {"arguments": {"text": "hello mesh"}}
                }))
                .await
                .unwrap();
            let call_resp = client.recv_value().await.unwrap().unwrap();
            assert_eq!(call_resp["id"], 2);
            assert_eq!(call_resp["result"]["content"][0]["text"], "hello mesh");

            // The stub observed the daemon-authored identity — whole-value REPLACE held:
            // the forged name AND the forged authorization fields were all dropped.
            let observed_init = stub.await.unwrap();
            let peer = &observed_init["params"]["_meta"]["mcpmesh/peer"];
            assert_eq!(
                peer["name"], "bob",
                "forged 'attacker' name must be overwritten"
            );
            assert_eq!(
                peer["groups"],
                json!([]),
                "forged groups ['admin'] must be dropped, not merged"
            );
            assert_eq!(
                peer["user_id"],
                json!(null),
                "forged user_id 'root' must be dropped (pairing-mode identity is null)"
            );

            // Closing the client EOFs the transport → the backend drops the server
            // connection and returns Ok — the session-close teardown path.
            drop(client);
            session
                .await
                .unwrap()
                .expect("run_over returns Ok on transport EOF");
        })
        .await
        .expect("socket backend test timed out");
    }

    /// `params` absent entirely: the backend builds `params` then `_meta` and injects
    /// cleanly (params may be non-object, guard it).
    #[tokio::test]
    async fn socket_backend_builds_params_when_absent() {
        timeout(Duration::from_secs(30), async {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("server.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            let stub = tokio::spawn(stub_server(listener));

            let (server_io, client_io) = duplex(64 * 1024);
            let (sr, sw) = split(server_io);
            let backend_transport = mcpmesh_net::transport::NdjsonTransport::new(sr, sw, MAX_FRAME);
            let (cr, cw) = split(client_io);
            let mut client = mcpmesh_net::transport::NdjsonTransport::new(cr, cw, MAX_FRAME);

            let backend = SocketBackend {
                path: sock.to_str().unwrap().to_string(),
                service: "test".into(),
                audit: crate::audit::AuditSink::disabled(),
                limiter: crate::limits::RateLimiter::unlimited_shared(),
            };
            let identity = Some(PeerIdentity {
                endpoint: [0u8; 32].into(),
                name: "bob".into(),
                user_id: None,
                groups: vec![],
            });

            // No `params` key at all — the backend must synthesize params + _meta.
            let initialize = json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize"
            });

            let session = tokio::spawn(async move {
                backend
                    .run_over(identity, initialize, backend_transport)
                    .await
            });

            let init_resp = client.recv_value().await.unwrap().unwrap();
            assert_eq!(init_resp["result"]["serverInfo"]["name"], "socket-stub");

            client
                .send_value(json!({
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": {"arguments": {"text": "built params"}}
                }))
                .await
                .unwrap();
            let call_resp = client.recv_value().await.unwrap().unwrap();
            assert_eq!(call_resp["result"]["content"][0]["text"], "built params");

            let observed_init = stub.await.unwrap();
            assert_eq!(
                observed_init["params"]["_meta"]["mcpmesh/peer"]["name"],
                "bob"
            );

            drop(client);
            session
                .await
                .unwrap()
                .expect("run_over returns Ok on transport EOF");
        })
        .await
        .expect("socket params-build test timed out");
    }

    /// A non-object root first frame (a bare array) with identity `Some` must NOT
    /// panic: the root guard replaces it with a fresh object and injects cleanly.
    /// Reachable once T9 wires this backend (select_service's key-absent default
    /// forwards even a non-object frame), and `Value`'s IndexMut panics on a
    /// non-object base — so the guard is load-bearing, not cosmetic.
    #[tokio::test]
    async fn socket_backend_guards_non_object_root() {
        timeout(Duration::from_secs(30), async {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("server.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            let stub = tokio::spawn(stub_server(listener));

            let (server_io, client_io) = duplex(64 * 1024);
            let (sr, sw) = split(server_io);
            let backend_transport = mcpmesh_net::transport::NdjsonTransport::new(sr, sw, MAX_FRAME);
            let (cr, cw) = split(client_io);
            let mut client = mcpmesh_net::transport::NdjsonTransport::new(cr, cw, MAX_FRAME);

            let backend = SocketBackend {
                path: sock.to_str().unwrap().to_string(),
                service: "test".into(),
                audit: crate::audit::AuditSink::disabled(),
                limiter: crate::limits::RateLimiter::unlimited_shared(),
            };
            let identity = Some(PeerIdentity {
                endpoint: [0u8; 32].into(),
                name: "bob".into(),
                user_id: None,
                groups: vec![],
            });

            // A bare JSON array as the first frame — indexing it as an object would panic without
            // a guard, which is what this test has always existed to prove.
            let initialize = json!([1, 2, 3]);

            let session = tokio::spawn(async move {
                backend
                    .run_over(identity, initialize, backend_transport)
                    .await
            });

            // The stub replies to the forwarded (rebuilt) frame with an id of null.
            let init_resp = client.recv_value().await.unwrap().unwrap();
            assert_eq!(init_resp["result"]["serverInfo"]["name"], "socket-stub");

            client
                .send_value(json!({
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": {"arguments": {"text": "fresh object"}}
                }))
                .await
                .unwrap();
            let call_resp = client.recv_value().await.unwrap().unwrap();
            assert_eq!(call_resp["result"]["content"][0]["text"], "fresh object");

            // The bare array reaches the backend UNTOUCHED (#45 gate).
            //
            // It used to be discarded and replaced with a fabricated `{"params":{"_meta":{...}}}` —
            // a frame with no `method` and no `id`, invented by mcpmesh out of nothing, that no
            // caller sent. Since frame 1 now shares `inject_peer` with every other frame, an array
            // root is treated as a JSON-RPC batch: descended, and its elements carry no `method`,
            // so nothing is attributed and nothing is destroyed. The backend gets the malformed
            // frame the caller actually sent and rejects it, which is the honest outcome for a
            // daemon that pumps rather than interprets.
            let observed_init = stub.await.unwrap();
            assert_eq!(
                observed_init,
                json!([1, 2, 3]),
                "a bare-array first frame must pass through, not be replaced by a fabricated one: \
                 {observed_init}"
            );

            drop(client);
            session
                .await
                .unwrap()
                .expect("run_over returns Ok on transport EOF");
        })
        .await
        .expect("socket non-object-root test timed out");
    }

    /// identity `None` (an unresolved/pairing-less caller): NO injection at all — the
    /// initialize is forwarded VERBATIM, including any `_meta` the caller carried. The
    /// passthrough path, matching the spawn backend's non-object-verbatim behavior.
    #[tokio::test]
    async fn socket_backend_no_identity_forwards_verbatim() {
        timeout(Duration::from_secs(30), async {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("server.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            let stub = tokio::spawn(stub_server(listener));

            let (server_io, client_io) = duplex(64 * 1024);
            let (sr, sw) = split(server_io);
            let backend_transport = mcpmesh_net::transport::NdjsonTransport::new(sr, sw, MAX_FRAME);
            let (cr, cw) = split(client_io);
            let mut client = mcpmesh_net::transport::NdjsonTransport::new(cr, cw, MAX_FRAME);

            let backend = SocketBackend {
                path: sock.to_str().unwrap().to_string(),
                service: "test".into(),
                audit: crate::audit::AuditSink::disabled(),
                limiter: crate::limits::RateLimiter::unlimited_shared(),
            };
            let identity: Option<PeerIdentity> = None;

            // Carries a `_meta` that the backend must NOT touch (no injection when None).
            let initialize = json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"capabilities": {}, "_meta": {"caller": "kept"}}
            });

            let session = tokio::spawn(async move {
                backend
                    .run_over(identity, initialize, backend_transport)
                    .await
            });

            let init_resp = client.recv_value().await.unwrap().unwrap();
            assert_eq!(init_resp["result"]["serverInfo"]["name"], "socket-stub");

            client
                .send_value(json!({
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": {"arguments": {"text": "no id"}}
                }))
                .await
                .unwrap();
            let call_resp = client.recv_value().await.unwrap().unwrap();
            assert_eq!(call_resp["result"]["content"][0]["text"], "no id");

            // Verbatim: no `mcpmesh/peer` was injected, and the caller's `_meta` survived.
            let observed_init = stub.await.unwrap();
            assert!(
                observed_init["params"]["_meta"]["mcpmesh/peer"].is_null(),
                "identity None must inject nothing"
            );
            assert_eq!(observed_init["params"]["_meta"]["caller"], "kept");

            drop(client);
            session
                .await
                .unwrap()
                .expect("run_over returns Ok on transport EOF");
        })
        .await
        .expect("socket no-identity test timed out");
    }

    /// A stub that emits a LARGE server-initiated notification BEFORE reading the
    /// client's large `tools/call` — forcing simultaneous large writes in BOTH
    /// directions. A single-loop pump (write awaited inside the `select!` arm) would
    /// wedge here: forwarding the client's large request blocks because we are not
    /// draining our large notification, and we cannot drain it because we are blocked
    /// forwarding — a classic full-duplex deadlock. The concurrent pump keeps both
    /// directions moving. Echoes the tools/call text on the way out.
    async fn deadlock_stub(listener: UnixListener, big: String) {
        let (stream, _) = listener.accept().await.expect("stub accept");
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = FrameReader::new(BufReader::new(read_half), MAX_FRAME);

        let init = match reader.next().await.unwrap().unwrap() {
            Inbound::Frame(v) => v,
            Inbound::Violation(_) => panic!("violation on initialize"),
        };
        write_frame(
            &mut write_half,
            &json!({
                "jsonrpc": "2.0", "id": init["id"].clone(),
                "result": {"serverInfo": {"name": "socket-stub"}}
            }),
        )
        .await
        .unwrap();

        // Large server-initiated notification, written before reading the request.
        write_frame(
            &mut write_half,
            &json!({
                "jsonrpc": "2.0", "method": "notifications/message",
                "params": {"data": big}
            }),
        )
        .await
        .unwrap();

        let call = match reader.next().await.unwrap().unwrap() {
            Inbound::Frame(v) => v,
            Inbound::Violation(_) => panic!("violation on tools/call"),
        };
        write_frame(
            &mut write_half,
            &json!({
                "jsonrpc": "2.0", "id": call["id"].clone(),
                "result": {"content": [{"text": call["params"]["arguments"]["text"].clone()}]}
            }),
        )
        .await
        .unwrap();
    }

    /// The pump survives simultaneous large bidirectional traffic — proving the
    /// concurrent-directions fix. A single-loop pump would hang to the 30s timeout
    /// (and, in the spawn backend, leak the child + the concurrency permit). 2 MiB
    /// exceeds both the 64 KiB transport duplex and any AF_UNIX socket buffer, so the
    /// writes genuinely block, which is the deadlock precondition.
    #[tokio::test]
    async fn pump_survives_simultaneous_large_bidirectional_traffic() {
        timeout(Duration::from_secs(30), async {
            let big = "x".repeat(2 * 1024 * 1024);
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("server.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            let stub = tokio::spawn(deadlock_stub(listener, big.clone()));

            let (server_io, client_io) = duplex(64 * 1024);
            let (sr, sw) = split(server_io);
            let backend_transport = mcpmesh_net::transport::NdjsonTransport::new(sr, sw, MAX_FRAME);
            let (cr, cw) = split(client_io);
            let mut client = mcpmesh_net::transport::NdjsonTransport::new(cr, cw, MAX_FRAME);

            let backend = SocketBackend {
                path: sock.to_str().unwrap().to_string(),
                service: "test".into(),
                audit: crate::audit::AuditSink::disabled(),
                limiter: crate::limits::RateLimiter::unlimited_shared(),
            };
            let identity = Some(PeerIdentity {
                endpoint: [0u8; 32].into(),
                name: "bob".into(),
                user_id: None,
                groups: vec![],
            });
            let initialize = json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"capabilities": {}}
            });
            let session = tokio::spawn(async move {
                backend
                    .run_over(identity, initialize, backend_transport)
                    .await
            });

            // Send the large request CONCURRENTLY with draining incoming frames (using
            // the transport's own writer split), so the TEST itself doesn't serialize
            // send-then-recv and mask the deadlock the pump must avoid.
            let client_writer = client.writer();
            let big_arg = big.clone();
            let sender = tokio::spawn(async move {
                client_writer
                    .send_value(json!({
                        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                        "params": {"arguments": {"text": big_arg}}
                    }))
                    .await
            });

            let (mut saw_init, mut saw_notification, mut saw_echo) = (false, false, false);
            while !(saw_init && saw_notification && saw_echo) {
                let frame = client.recv_value().await.unwrap().unwrap();
                if frame["method"] == "notifications/message" {
                    assert_eq!(frame["params"]["data"].as_str().unwrap().len(), big.len());
                    saw_notification = true;
                } else if frame["id"] == 2 {
                    assert_eq!(
                        frame["result"]["content"][0]["text"]
                            .as_str()
                            .unwrap()
                            .len(),
                        big.len()
                    );
                    saw_echo = true;
                } else if frame["id"] == 1 {
                    assert_eq!(frame["result"]["serverInfo"]["name"], "socket-stub");
                    saw_init = true;
                }
            }

            sender.await.unwrap().unwrap();
            stub.await.unwrap();

            drop(client);
            session
                .await
                .unwrap()
                .expect("run_over returns Ok on transport EOF");
        })
        .await
        .expect("large-frame pump deadlocked (the old single-loop pump would hang here)");
    }

    /// A driven session emits exactly one `session_open` and one `session_close` record for the
    /// resolved peer + service (the session-lifecycle audit contract). Uses a real temp-dir AuditLog.
    #[tokio::test]
    async fn socket_backend_records_session_open_and_close() {
        use crate::audit::{AuditLog, AuditSink};
        timeout(Duration::from_secs(30), async {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("server.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            let stub = tokio::spawn(stub_server(listener));

            let audit_dir = dir.path().join("audit");
            let sink = AuditSink::new(AuditLog::spawn(audit_dir.clone()));

            let (server_io, client_io) = duplex(64 * 1024);
            let (sr, sw) = split(server_io);
            let backend_transport = mcpmesh_net::transport::NdjsonTransport::new(sr, sw, MAX_FRAME);
            let (cr, cw) = split(client_io);
            let mut client = mcpmesh_net::transport::NdjsonTransport::new(cr, cw, MAX_FRAME);

            let backend = SocketBackend {
                path: sock.to_str().unwrap().to_string(),
                service: "notes".into(),
                audit: sink,
                limiter: crate::limits::RateLimiter::unlimited_shared(),
            };
            let identity = Some(PeerIdentity {
                endpoint: [0u8; 32].into(),
                name: "bob".into(),
                user_id: None,
                groups: vec![],
            });
            let initialize = json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"capabilities": {}}
            });
            let session = tokio::spawn(async move {
                backend
                    .run_over(identity, initialize, backend_transport)
                    .await
            });

            let init_resp = client.recv_value().await.unwrap().unwrap();
            assert_eq!(init_resp["result"]["serverInfo"]["name"], "socket-stub");
            // Drive one tools/call so the stub's exchange completes cleanly before teardown (the
            // stub reads exactly one tools/call after initialize). The session still opens once and
            // closes once — the lifecycle records under test are unaffected.
            client
                .send_value(json!({
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": {"arguments": {"text": "lifecycle"}}
                }))
                .await
                .unwrap();
            let call_resp = client.recv_value().await.unwrap().unwrap();
            assert_eq!(call_resp["result"]["content"][0]["text"], "lifecycle");
            drop(client); // EOF → the backend returns → session_close is recorded
            session.await.unwrap().expect("run_over Ok on EOF");
            stub.await.unwrap();

            // Poll the audit file until both lifecycle records land.
            let month = &crate::audit::now_ts()[..7];
            let file = audit_dir.join(format!("{month}.jsonl"));
            let mut opens = 0;
            let mut closes = 0;
            for _ in 0..50 {
                if let Ok(body) = std::fs::read_to_string(&file) {
                    opens = body.matches("\"kind\":\"session_open\"").count();
                    closes = body.matches("\"kind\":\"session_close\"").count();
                    if opens == 1 && closes == 1 {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(
                (opens, closes),
                (1, 1),
                "one open + one close for the session"
            );
            let body = std::fs::read_to_string(&file).unwrap();
            assert!(
                body.contains("\"peer\":\"bob\""),
                "attributed to the resolved peer"
            );
            assert!(body.contains("\"service\":\"notes\""));
        })
        .await
        .expect("session lifecycle audit test timed out");
    }

    /// While a session is live the audit sink's live-session table has exactly one row for the
    /// resolved peer + service, and it is emptied once the session ends — proving the backend drives
    /// the RAII session guard (not two bare open/close records) so the telemetry snapshot sees it.
    #[tokio::test(flavor = "multi_thread")]
    async fn socket_backend_populates_active_sessions_while_open() {
        use crate::audit::{AuditLog, AuditSink};
        timeout(Duration::from_secs(30), async {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("server.sock");
            let listener = UnixListener::bind(&sock).unwrap();
            let stub = tokio::spawn(stub_server(listener));

            let sink = AuditSink::new(AuditLog::spawn(dir.path().join("audit")));

            let (server_io, client_io) = duplex(64 * 1024);
            let (sr, sw) = split(server_io);
            let backend_transport = mcpmesh_net::transport::NdjsonTransport::new(sr, sw, MAX_FRAME);
            let (cr, cw) = split(client_io);
            let mut client = mcpmesh_net::transport::NdjsonTransport::new(cr, cw, MAX_FRAME);

            let backend = SocketBackend {
                path: sock.to_str().unwrap().to_string(),
                service: "notes".into(),
                audit: sink.clone(),
                limiter: crate::limits::RateLimiter::unlimited_shared(),
            };
            let identity = Some(PeerIdentity {
                endpoint: [7u8; 32].into(),
                name: "bob".into(),
                user_id: None,
                groups: vec![],
            });
            let initialize = json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"capabilities": {}}
            });

            assert!(
                sink.active_sessions().is_empty(),
                "no live session before the backend runs"
            );

            let session = tokio::spawn(async move {
                backend
                    .run_over(identity, initialize, backend_transport)
                    .await
            });

            // Complete the handshake so the session's guard is definitely created (it is built before
            // the pump, which forwards this initialize).
            let init_resp = client.recv_value().await.unwrap().unwrap();
            assert_eq!(init_resp["result"]["serverInfo"]["name"], "socket-stub");

            // DURING the session: exactly one live row for the resolved peer + service.
            let live = sink.active_sessions();
            assert_eq!(live.len(), 1, "one live session while open");
            assert_eq!(live[0].peer, "bob");
            assert_eq!(live[0].service, "notes");
            // #73: the row must carry the STABLE device principal, not just the nickname. Pinned
            // HERE, at the backend, because asserting it on a hand-built AuditSink::session call
            // proves the struct carries what you hand it — not that the backend hands it the right
            // thing. Passing `identity.name` here instead reintroduces #73 exactly.
            assert_eq!(
                live[0].principal.as_deref(),
                Some(
                    mcpmesh_net::EndpointId::from_bytes([7u8; 32])
                        .principal()
                        .as_str()
                ),
                "the live row must be keyed on eid:<hex>, not the collidable nickname (#73)"
            );

            // Drive one tools/call so the stub's exchange completes cleanly before teardown.
            client
                .send_value(json!({
                    "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                    "params": {"arguments": {"text": "live"}}
                }))
                .await
                .unwrap();
            let call_resp = client.recv_value().await.unwrap().unwrap();
            assert_eq!(call_resp["result"]["content"][0]["text"], "live");

            drop(client); // EOF → the backend returns → the guard drops → the row is removed
            session.await.unwrap().expect("run_over Ok on EOF");
            stub.await.unwrap();

            // AFTER the session: the live table is empty again.
            assert!(
                sink.active_sessions().is_empty(),
                "the guard drop removed the live session"
            );
        })
        .await
        .expect("active-sessions telemetry test timed out");
    }
}
