//! Session backends: the two ways the daemon answers a selected
//! service. Each implements `mcpmesh_net::SessionBackend`, so `mcpmesh_net::serve`
//! hands it the stripped `initialize` frame and the by-value transport, and the
//! backend owns the session's teardown.
//!
//! * [`spawn`] — the `run` backend: fork one child MCP server per session, pump
//!   its stdio to/from the transport, inject the resolved identity as env vars.
//! * [`socket`] — the `socket` backend: dial a long-running local MCP server per
//!   session and inject the resolved identity into the forwarded `initialize`
//!   `_meta["mcpmesh/peer"]` (authoritative — overwrites, never merges).
//!
//! Both backends drive the same session shape once their local MCP server's byte
//! stream exists: forward the `initialize`, then pump frames both directions over
//! `pump` with one codec ([`mcpmesh_net::framing`]) on the server side too. The only
//! thing that differs is HOW the server stream is obtained (fork+stdio vs. dial+UDS)
//! and HOW identity is injected (env vars vs. `_meta`).
//!
//! Fidelity is Value/semantic, not byte-for-byte: every
//! frame round-trips through `serde_json::Value` (object keys re-sorted, no
//! `arbitrary_precision`) — the same caveat as the mesh transport. The platform
//! pumps and never INTERPRETS the MCP method/result semantics; it does re-serialize
//! the JSON.
use anyhow::{Context, Result};
use mcpmesh_net::errors::synthesized_limited;
use mcpmesh_net::framing::{FrameReader, Inbound, write_frame};
use mcpmesh_net::transport::NdjsonTransport;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};

pub mod socket;
pub mod spawn;

/// Per-session frame cap for the local MCP server's output (16 MiB) — the same
/// bound `mcpmesh_net::serve` applies to the mesh transport.
pub(crate) const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

// NOTE: the per-service spawn cap is fully HANDLED inside
// `SpawnBackend::run_over` — on cap it synthesizes `-32053` on the transport and returns
// `Ok(())` (a clean refusal, not a session error). There is deliberately no returned error
// type: `run_session` lives in `mcpmesh-net`, which cannot depend on a cli error type (a real
// layering constraint), so a "downcast in the daemon" was never possible. The backend owning
// the refusal is the correct seam.

/// Enforce the reserved `_meta` namespace on ONE caller→backend frame (#164).
///
/// The rule used to run on the session's first frame only, and `run_session` treats frame 1 as
/// `initialize` whatever its method actually is. So a caller spent frame 1 on a `ping` — which the
/// MCP lifecycle permits before `initialize`, and which rmcp answers — and sent its real
/// `initialize` as frame 2, which reached the backend verbatim with a forged `mcpmesh/peer`.
///
/// Three steps, because each alone leaves a hole:
///
/// 1. **Strip** every caller-supplied reserved key — `mcpmesh/*` AND
///    `tech.counterpunch.mcpmesh/*` (#49). Unconditional, both backends.
///    `mcpmesh/service` is the key `select_service` acts on, so this is authorization-relevant.
/// 2. **Remove an impersonating `io.modelcontextprotocol/clientInfo`** (#189) — one whose `name` is
///    written in mcpmesh's own `eid:`/`b64u:` principal grammar. Under MCP 2026-07-28 that key
///    lands in the same `_meta` object as the authenticated `mcpmesh/peer`; see
///    [`strip_impersonating_client_info`](mcpmesh_net::service::strip_impersonating_client_info)
///    for why the rest of `clientInfo` is left strictly alone. Returned rather than logged here, so
///    the pump can warn ONCE per session instead of once per caller-controlled frame.
/// 3. **Inject** the authoritative peer into whichever frame is actually the handshake. Stripping
///    alone would leave the backend's real `initialize` carrying no identity at all —
///    unattributable, which is the second harm the issue names.
///
/// `peer_meta` is `None` for the `run` backend, which conveys identity through `MCPMESH_PEER_*` env
/// vars and has no `_meta` seam; inventing one there would make a `run` server see a key that
/// appears on no other release. The strip still applies to it.
///
/// Non-object `params`/`_meta` are REPLACED, never indexed into — `Value`'s `IndexMut` panics on a
/// non-object base, and a caller controls this shape.
/// Returns whether an impersonating `clientInfo` was removed (#189) — the caller warns once.
#[must_use]
fn sanitize_caller_frame(frame: &mut Value, peer_meta: Option<&Value>) -> bool {
    mcpmesh_net::service::strip_reserved_meta(frame);
    // Runs for BOTH backends, including `run` — a spawned server reads `_meta` too even though its
    // identity arrives by env var, so skipping the removal there would leave the impersonation
    // reachable on exactly one backend.
    let impersonating = mcpmesh_net::service::strip_impersonating_client_info(frame);
    if let Some(peer) = peer_meta {
        inject_peer(frame, peer, 0);
    }
    impersonating
}

/// Stamp the authenticated caller onto every request the backend will see, descending a JSON-RPC
/// batch.
///
/// **Every frame carrying a `method`, not just `initialize` (#45 ask 2).** Until 0.50.0 this
/// returned early for anything else, which was correct only while MCP guaranteed a session opened
/// with a handshake. Under `2026-07-28` there is no `initialize` — the first frame is an ordinary
/// request and `_meta` rides all of them — so a served backend could not identify its caller at all.
///
/// That gap was fail-closed rather than forgeable, and the distinction is worth keeping straight:
/// [`sanitize_caller_frame`] strips reserved keys from EVERY frame and always did, so a caller could
/// never supply an identity mcpmesh had not vouched for. The backend saw nothing, not something
/// attacker-controlled.
///
/// **The strip runs before this, and that ordering is the security property.** Injecting first would
/// let a caller's forged `mcpmesh/peer` survive on any frame the strip then failed to reach.
///
/// A frame with NO `method` is left alone: a caller→backend response (to a server-initiated request,
/// #91) carries `id` + `result`/`error` and no `params`, and inventing a `params` object on one
/// would be malformed JSON-RPC.
///
/// A top-level array has no `method`, so an array-wrapped request was neither stripped nor
/// attributed — the strip's own batch bypass, on the injection side (#164 gate). Depth-bounded for
/// the same reason as [`mcpmesh_net::service::strip_reserved_meta`].
fn inject_peer(frame: &mut Value, peer: &Value, depth: usize) {
    if let Some(batch) = frame.as_array_mut() {
        if depth < 8 {
            for element in batch {
                inject_peer(element, peer, depth + 1);
            }
        }
        return;
    }
    if !frame.get("method").is_some_and(Value::is_string) {
        return;
    }
    // No `!frame.is_object()` guard: only an object can carry a `method`, and the check above
    // already returned for everything else. `params`/`_meta` still need theirs — `Value`'s
    // IndexMut PANICS on a non-object base, and a non-object frame is reachable (`select_service`'s
    // key-absent default forwards one).
    match frame.get("params") {
        // Absent or null: a parameterless request legitimately has none, so build the object.
        None | Some(Value::Null) => frame["params"] = serde_json::json!({}),
        Some(v) if v.is_object() => {}
        // POSITIONAL params. JSON-RPC 2.0 permits an array and this daemon pumps rather than
        // interprets, so there is nowhere to put `_meta` without destroying the caller's arguments.
        // Leave the frame alone and inject nothing: the backend sees no identity, which is
        // fail-closed, rather than seeing arguments mcpmesh silently deleted. MCP itself mandates
        // object params, so this is the non-MCP JSON-RPC backend case (#45 gate).
        Some(_) => return,
    }
    // `_meta` is protocol metadata rather than the caller's arguments, so a malformed one is
    // replaced rather than deferred to.
    if !frame["params"]["_meta"].is_object() {
        frame["params"]["_meta"] = serde_json::json!({});
    }
    // BOTH spellings, identical value (#49). A backend is a THIRD-PARTY process reading
    // `mcpmesh/peer` — not something version-locked to this daemon — so dropping the legacy key
    // would make every existing backend silently stop seeing an identity, and one that reads "no
    // identity" as "local caller" would fail OPEN. Legacy deprecated as of 0.51.0, removed at 1.0.
    for key in mcpmesh_net::service::PEER_KEYS {
        frame["params"]["_meta"][key] = peer.clone();
    }
}

/// Bidirectionally pump one session between the mesh transport and a local MCP
/// server's byte stream (a spawned child's stdio, or a dialed UDS). Shared by both
/// backends (DRY): only the server-side reader/writer types differ, so this is
/// generic over all four byte substrates.
///
/// The (already identity-augmented) `initialize` is the server's first inbound line,
/// then frames flow both ways — one codec: `write_frame`/`FrameReader` on the server
/// side too, exactly as on the mesh side.
///
/// **The two directions run CONCURRENTLY as independent loops**, not as a single
/// `select!` whose write is awaited inside the arm. That matters for correctness, not
/// just throughput: with a single loop, awaiting a blocked write in one direction
/// (e.g. the server's input pipe is full because the peer is not draining the
/// server's output) would prevent reading the other direction — a classic full-duplex
/// pipe deadlock, reachable under 16 MiB frames. Running the directions
/// concurrently means direction B keeps draining the server's output (unblocking a
/// full pipe) while direction A is blocked writing, so neither side can wedge. A
/// wedge would be doubly bad: `run_over` would never return, so the spawn backend's
/// `kill_on_drop` would never fire AND its owned concurrency permit would leak.
///
/// Whichever direction ends first (EOF, IO error, or a framing violation) tears the
/// session down: the `select!` returns, this fn returns, the caller drops the server
/// connection (killing a child), and `transport.shutdown()` flushes any final frame.
/// `FrameReader::next` and `recv_value`/`send_value` are cancellation-safe, so the
/// cancelled direction drops no committed bytes (a half-written frame on teardown is
/// fine — the session is closing).
///
/// A trusted local server that emits a malformed line is a bug, not an attack: the
/// session ends rather than interpreting it (the platform pumps, never interprets).
///
/// Every caller→backend frame passes [`sanitize_caller_frame`] first (#164).
pub(crate) async fn pump<TR, TW, SR, SW>(
    initialize: Value,
    transport: &mut NdjsonTransport<TR, TW>,
    server_read: SR,
    mut server_write: SW,
    auditor: crate::audit::RequestAuditor,
    rate: crate::limits::RateGate,
    // The daemon-authored `mcpmesh/peer` value, re-injected if a LATER frame turns out to be the
    // real `initialize` (#164). `None` = this backend has no `_meta` identity seam.
    peer_meta: Option<Value>,
) -> Result<()>
where
    TR: AsyncRead + Send + Unpin,
    TW: AsyncWrite + Send + Unpin,
    SR: AsyncRead + Send + Unpin,
    SW: AsyncWrite + Send + Unpin,
{
    write_frame(&mut server_write, &initialize)
        .await
        .context("forward initialize to local MCP server")?;
    // Wrapped so Direction A can DROP the write half at end-of-input: for a spawned child,
    // `AsyncWriteExt::shutdown` on its stdin only flushes — the fd closes (and the child sees
    // EOF) on drop; for a socket backend the drop sends the FIN just as shutdown would.
    let mut server_write = Some(server_write);

    // The outbound direction sends through a cloned writer handle so it does not need
    // `&mut transport` (which the inbound direction holds for `recv_value`). This
    // disjoint split — reader on one side, the Arc'd writer on the other — is what
    // lets the two loops run concurrently without a shared mutable borrow.
    // A second cloned writer so Direction A can send a -32053 throttle reply without borrowing the
    // transport Direction B holds (both send through the same Arc<Mutex> write half — safe).
    let throttle_writer = transport.writer();
    let transport_writer = transport.writer();
    let mut server_out = FrameReader::new(BufReader::new(server_read), MAX_FRAME_BYTES);

    // Direction A: mesh transport → local server. Owns `&mut transport` (recv) and
    // `server_write`; ends on transport EOF/error or the server's input closing.
    let to_server = async {
        // #189: warn on the FIRST impersonating `clientInfo` of the session and never again. The
        // offending field is caller-controlled and arrives per frame, so a log line per occurrence
        // is an unbounded growth vector the caller drives — the audit-DoS class again. One line
        // names the session; a second adds nothing.
        let mut warned_client_info = false;
        loop {
            match transport.recv_value().await {
                Ok(Some(mut frame)) => {
                    // #164: strip reserved keys and re-attribute a later handshake, BEFORE the
                    // rate gate, the audit hook, or the forward — `select_service`'s same "before
                    // anything acts on the frame" discipline.
                    if sanitize_caller_frame(&mut frame, peer_meta.as_ref())
                        && !std::mem::replace(&mut warned_client_info, true)
                    {
                        tracing::warn!(
                            "caller sent an `io.modelcontextprotocol/clientInfo` naming itself in \
                             mcpmesh's principal grammar (eid:/b64u:); the whole entry was removed \
                             before the backend saw it. `mcpmesh/peer` is the only authenticated \
                             identity in that object. Logged once per session."
                        );
                    }
                    // Per-identity rate limit: consult BEFORE forwarding a
                    // proxied REQUEST/notification (a method-bearing frame). FAIL-SAFE over-limit —
                    // DROP the request (never forward, never queue), reply -32053{retry_after_ms}
                    // for a request id (a notification gets no reply but IS audited, #76), and CONTINUE the session
                    // (bounded backpressure, not a close).
                    if frame.get("method").is_some()
                        && let Err(retry_after_ms) = rate.admit()
                    {
                        match frame.get("id").filter(|v| !v.is_null()).cloned() {
                            Some(id) => {
                                let _ = throttle_writer
                                    .send_value(synthesized_limited(id, retry_after_ms))
                                    .await;
                            }
                            // #76: a notification has no reply channel, so the sender cannot be
                            // told directly — but the loss must not be INVISIBLE. Recorded with
                            // `status: "rate_limited"`, so it shows up in the audit log and the
                            // subscribe stream instead of vanishing.
                            None => auditor.on_dropped(&frame),
                        }
                        continue;
                    }
                    // Proxied-request-line audit hook: hash args + record method/tool BEFORE
                    // forwarding. PRIVACY — sees raw args (the server needs them); stores only blake3.
                    auditor.on_request(&frame);
                    let Some(w) = server_write.as_mut() else {
                        break;
                    };
                    if write_frame(w, &frame).await.is_err() {
                        break; // server input closed — server is gone
                    }
                }
                Ok(None) => break, // transport EOF / clean close
                Err(_) => break,   // transport IO error or framing violation
            }
        }
        // The peer half-closed (no more requests) or the transport failed — either way this
        // direction is done, but the SESSION is not: responses to already-forwarded requests
        // may still be inside the server. Close the server's stdin so it sees end-of-input
        // and can finish, then park: only Direction B draining to the server's output EOF may
        // end the session. Winning the select! here would cancel B and drop those replies —
        // the one-shot client (`printf ... | mcpmesh connect ...`) hits exactly that race.
        if let Some(mut w) = server_write.take() {
            use tokio::io::AsyncWriteExt;
            let _ = w.shutdown().await;
        } // dropped: the child's stdin fd closes / the socket FINs — the backend sees EOF
        std::future::pending::<()>().await
    };

    // Direction B: local server → mesh transport. Owns the FrameReader and the cloned
    // writer handle; ends on the server's output EOF/error/violation or a gone peer.
    let to_transport = async {
        loop {
            match server_out.next().await {
                Ok(Some(Inbound::Frame(frame))) => {
                    // Response correlation: count the bytes going OUT to the peer (a
                    // COUNT, never the content) and let the auditor emit the completed request record.
                    let bytes_out = serde_json::to_vec(&frame)
                        .map(|v| v.len() as u64)
                        .unwrap_or(0);
                    auditor.on_response(&frame, bytes_out);
                    if transport_writer.send_value(frame).await.is_err() {
                        break; // peer is gone
                    }
                }
                Ok(Some(Inbound::Violation(_))) => break,
                Ok(None) => break, // server output EOF — server closed the session
                Err(_) => break,   // IO error reading the server
            }
        }
    };

    // Direction A parks after end-of-input instead of finishing, so B — the drain toward
    // the peer — is the only branch that can end the session (on the server's output EOF).
    tokio::select! {
        () = to_server => {}
        () = to_transport => {}
    }

    // Flush any final buffered frame (e.g. a last reply) before the write half
    // closes; a no-op once already closed.
    let _ = transport.shutdown().await;
    Ok(())
}

/// The STABLE device principal for a live-session row (#73).
///
/// Shared by both backends so they cannot diverge: `ActiveSession.principal` exists precisely
/// because `peer` is a display nickname and collides, and a backend quietly passing the nickname —
/// or `None` — reintroduces the bug in half the wiring. One definition, one place to get wrong.
///
/// `None` only on the no-identity path, which production never takes (`net` always passes
/// `Some(identity)`).
pub(crate) fn session_principal(identity: Option<&mcpmesh_net::PeerIdentity>) -> Option<String> {
    identity.map(|id| id.endpoint.principal())
}

#[cfg(test)]
mod tests {
    //! Pins the pump's TEARDOWN DISCIPLINE (issue #25): transport EOF ends only the
    //! REQUEST direction — Direction A closes the server's stdin and PARKS, and the
    //! session ends solely when Direction B drains the server's output to EOF. The
    //! pre-fix `select!` let Direction A's completion cancel B, dropping every reply
    //! still inside the server — exactly what a one-shot client (request, then
    //! immediate EOF) provokes. In-memory duplex on all four substrates (the
    //! `proxy::pump_stdio` test pattern); the fake server deliberately withholds its
    //! replies until it sees end-of-input, so a pump that tears down on transport EOF
    //! can never pass.
    //!
    //! Plumbing invariant: each direction is a WHOLE `DuplexStream` used one-way (the
    //! other way stays idle), never a `tokio::io::split` half — dropping a `WriteHalf`
    //! does NOT drop the underlying stream (its `ReadHalf` keeps it alive), so a
    //! split-based harness never delivers the EOFs this test is about.
    use std::time::Duration;

    use serde_json::json;
    use tokio::io::duplex;
    use tokio::time::timeout;

    use super::*;
    use crate::audit::{AuditSink, RequestAuditor};
    use crate::limits::{RateGate, RateLimiter};

    /// #91: a server-initiated NOTIFICATION reaches the peer, and is NOT metered.
    ///
    /// Direction B forwards every frame the local server writes, including method-bearing ones,
    /// with no limiter consult — which is what makes "an agent reacts to an incoming message"
    /// possible rather than requiring the peer to poll. That behaviour was emergent and untested;
    /// #45's stateless rework removes the `initialize` handshake this session shape is built
    /// around, and a property nobody wrote down is a property nobody notices removing.
    ///
    /// The rate gate here is set to a budget of ONE and then exhausted by the inbound request, so
    /// an implementation that metered Direction B against the same per-identity budget would drop
    /// or -32053 this notification instead of forwarding it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_rate_limited_notification_is_audited_rather_than_silently_dropped() {
        timeout(Duration::from_secs(10), async {
            let (mut peer_w, tr) = duplex(64 * 1024);
            let (tw, _peer_r) = duplex(64 * 1024);
            let mut transport = NdjsonTransport::new(tr, tw, MAX_FRAME_BYTES);
            let (server_write, srv_stdin) = duplex(64 * 1024);
            let (srv_stdout, server_read) = duplex(64 * 1024);

            // Count what actually reaches the server, so "dropped" is observed, not assumed.
            let server = tokio::spawn(async move {
                let _keep = srv_stdout;
                let mut reader = FrameReader::new(srv_stdin, MAX_FRAME_BYTES);
                let mut seen = 0usize;
                while let Ok(Some(Inbound::Frame(_))) = reader.next().await {
                    seen += 1;
                }
                seen
            });

            // A budget of ONE, consumed by the first notification; the second is over-limit.
            // Some(endpoint), NOT None — a None gate meters nothing and would make this vacuous
            // (see the sibling test's note).
            let rate = RateGate::new(
                std::sync::Arc::new(RateLimiter::per_minute(1, 1)),
                Some(mcpmesh_net::EndpointId::from_bytes([9u8; 32])),
            );
            let dir = tempfile::tempdir().unwrap();
            let sink = AuditSink::new(crate::audit::log::AuditLog::spawn(dir.path().to_path_buf()));
            let mut rx = sink.subscribe().expect("auditing enabled");
            let auditor =
                RequestAuditor::new(sink.clone(), Some("bob".into()), "notes".into(), None);

            let pump = tokio::spawn(async move {
                let _ = pump(
                    json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
                    &mut transport,
                    server_read,
                    server_write,
                    auditor,
                    rate,
                    None,
                )
                .await;
            });

            // Two notifications: the first fits the budget, the second does not.
            for _ in 0..2 {
                write_frame(
                    &mut peer_w,
                    &json!({"jsonrpc":"2.0","method":"notifications/progress","params":{}}),
                )
                .await
                .unwrap();
            }
            drop(peer_w);

            // The DROP must be observable. A notification has no reply channel, so nothing goes
            // back on the wire — the audit stream is the only place the loss can surface, and
            // before #76 it surfaced nowhere at all.
            let mut statuses = Vec::new();
            while let Ok(Ok(rec)) = timeout(Duration::from_secs(3), rx.recv()).await {
                if rec.method.as_deref() == Some("notifications/progress") {
                    statuses.push(rec.status.clone());
                    if statuses.len() == 2 {
                        break;
                    }
                }
            }
            assert!(
                statuses.contains(&Some("rate_limited".into())),
                "a notification dropped by the limiter must be recorded as rate_limited — \
                 otherwise the loss is invisible to the sender AND to the operator, which is the \
                 whole of #76. Saw: {statuses:?}"
            );

            let _ = pump.await;
            let forwarded = server.await.unwrap();
            assert!(
                forwarded < 3,
                "the over-limit notification must NOT have been forwarded: {forwarded}"
            );
        })
        .await
        .expect("dropped-notification audit test timed out");
    }

    /// #164 spec case 4: the `run` backend conveys identity through `MCPMESH_PEER_*` env vars and
    /// has NO `_meta` seam. It must still get the strip on every frame — but injecting a
    /// `mcpmesh/peer` there would invent a surface that backend does not have, and a `run` server
    /// would start seeing a key that appears on no other release.
    #[test]
    fn a_none_peer_meta_strips_but_never_injects() {
        let mut frame = json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"_meta":{
            "mcpmesh/peer": {"name":"attacker","groups":["admin"]},
            "mcpmesh/service": "not-yours",
            "app/keep": "yes"
        }}});
        let _ = sanitize_caller_frame(&mut frame, None);

        assert!(
            frame["params"]["_meta"].get("mcpmesh/peer").is_none(),
            "a forged peer must be STRIPPED for a run backend too: {frame}"
        );
        assert!(
            frame["params"]["_meta"].get("mcpmesh/service").is_none(),
            "and so must the key select_service acts on: {frame}"
        );
        assert_eq!(
            frame["params"]["_meta"]["app/keep"], "yes",
            "non-reserved keys survive — this is a prefix strip, not an _meta eraser: {frame}"
        );
    }

    /// #164: the injection targets the frame whose METHOD is `initialize`, not a positional guess.
    #[test]
    fn the_authoritative_peer_lands_on_every_request_not_only_the_handshake() {
        let peer = json!({"eid":"eid:aa","name":"bob","user_id":null,"groups":[]});

        // #45 ask 2: a non-`initialize` request is attributed too. Until 0.50.0 it was stripped and
        // left BARE, which was correct only while MCP guaranteed a session opened with a handshake
        // — under 2026-07-28 there is none, so the backend could not identify its caller at all.
        //
        // The assertion is REPLACEMENT, not presence. A test that only checked the key exists would
        // pass on an implementation that injects BEFORE stripping, which would ship the caller's
        // forged value on every frame the strip later failed to reach.
        let mut call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                              "params":{"_meta":{"mcpmesh/peer":
                                  {"name":"attacker","groups":["admin"],"user_id":"root"}}}});
        let _ = sanitize_caller_frame(&mut call, Some(&peer));
        assert_eq!(
            call["params"]["_meta"]["mcpmesh/peer"], peer,
            "a forged peer on an ordinary request is REPLACED with the authenticated one, \
             whole-value: {call}"
        );

        // A NOTIFICATION (no `id`) is a request too and carries `_meta` under 2026-07-28.
        let mut note = json!({"jsonrpc":"2.0","method":"notifications/progress","params":{}});
        let _ = sanitize_caller_frame(&mut note, Some(&peer));
        assert_eq!(note["params"]["_meta"]["mcpmesh/peer"], peer, "{note}");

        // A request with NO params at all gets a well-formed object rather than a scalar overwrite.
        let mut bare = json!({"jsonrpc":"2.0","id":9,"method":"ping"});
        let _ = sanitize_caller_frame(&mut bare, Some(&peer));
        assert_eq!(bare["params"]["_meta"]["mcpmesh/peer"], peer, "{bare}");

        // A RESPONSE has no `method` and must be left alone: it carries `id` + `result`, and
        // inventing a `params` object on one would be malformed JSON-RPC. This is the guard that
        // keeps "every frame" from meaning literally every frame.
        let mut resp = json!({"jsonrpc":"2.0","id":3,"result":{"ok":true}});
        let before = resp.clone();
        let _ = sanitize_caller_frame(&mut resp, Some(&peer));
        assert_eq!(
            resp, before,
            "a caller→backend response (#91's push direction) is not a request and gains nothing: \
             {resp}"
        );

        // The legacy handshake is unchanged.
        let mut init = json!({"jsonrpc":"2.0","id":2,"method":"initialize",
                              "params":{"_meta":{"mcpmesh/peer":
                                  {"name":"attacker","groups":["admin"],"user_id":"root"}}}});
        let _ = sanitize_caller_frame(&mut init, Some(&peer));
        assert_eq!(init["params"]["_meta"]["mcpmesh/peer"], peer, "{init}");
    }

    /// #189 at the SEAM: the removal runs on every caller frame, both backends, and the
    /// authoritative injection is unaffected by it.
    ///
    /// The non-first-frame case is the point. #164 was a rule that held on frame 1 only, and a
    /// caller reached the backend by spending frame 1 on a `ping`. A `clientInfo` check wired only
    /// into `select_service` would have exactly that hole — and unlike `mcpmesh/peer` this key
    /// legitimately appears on EVERY request under MCP 2026-07-28, so frame 1 is the LEAST likely
    /// place to see it.
    #[test]
    fn an_impersonating_client_info_is_removed_on_every_frame_and_both_backends() {
        const CI: &str = "io.modelcontextprotocol/clientInfo";
        let peer = json!({"eid":"eid:aa","name":"bob","user_id":null,"groups":[]});

        // A LATER frame — a `tools/call`, not the handshake.
        let mut later = json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
            "params":{"_meta":{CI:{"name":"eid:forged","version":"9"}}}});
        assert!(
            sanitize_caller_frame(&mut later, Some(&peer)),
            "the seam must report the removal so the pump can warn once per session"
        );
        assert!(
            later["params"]["_meta"].get(CI).is_none(),
            "a non-handshake frame must be cleaned too — #164's hole was exactly this: {later}"
        );

        // The `run` backend (`peer_meta: None`) conveys identity by env var and gets no `_meta`
        // injection — but it still READS `_meta`, so skipping the removal there would leave the
        // impersonation reachable on exactly one backend.
        let mut run_frame = json!({"jsonrpc":"2.0","id":8,"method":"tools/call",
            "params":{"_meta":{CI:{"name":"b64u:forged"}}}});
        assert!(sanitize_caller_frame(&mut run_frame, None));
        assert!(
            run_frame["params"]["_meta"].get(CI).is_none(),
            "{run_frame}"
        );

        // The two rules are independent: an impersonating clientInfo on the handshake is removed
        // AND the authoritative peer is still injected. A legitimate one survives beside it.
        let mut init = json!({"jsonrpc":"2.0","id":9,"method":"initialize",
            "params":{"_meta":{CI:{"name":"eid:forged"},"mcpmesh/peer":{"name":"also forged"}}}});
        assert!(sanitize_caller_frame(&mut init, Some(&peer)));
        assert!(init["params"]["_meta"].get(CI).is_none());
        assert_eq!(
            init["params"]["_meta"]["mcpmesh/peer"], peer,
            "the authenticated identity is still injected: {init}"
        );

        let mut ok = json!({"jsonrpc":"2.0","id":10,"method":"initialize",
            "params":{"_meta":{CI:{"name":"Claude Code","version":"2.0"}}}});
        assert!(
            !sanitize_caller_frame(&mut ok, Some(&peer)),
            "an ordinary clientInfo is not an impersonation"
        );
        assert_eq!(
            ok["params"]["_meta"][CI]["name"], "Claude Code",
            "…and it reaches the backend beside `mcpmesh/peer`, verbatim: {ok}"
        );
        assert_eq!(ok["params"]["_meta"]["mcpmesh/peer"], peer);
    }

    /// #164: a caller controls these shapes, and `Value`'s `IndexMut` PANICS on a non-object base.
    /// A panic on the proxy path is a remote crash, so every odd shape must survive.
    #[test]
    fn odd_shapes_survive_sanitize_without_panicking() {
        let peer = json!({"eid":"eid:aa","name":"bob","user_id":null,"groups":[]});
        // Absent, null, or a malformed `_meta` inside an OBJECT params: attributed. `_meta` is
        // protocol metadata rather than the caller's arguments, so a malformed one is replaced.
        for mut frame in [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize"}), // no params
            json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":null}),
            json!({"jsonrpc":"2.0","id":4,"method":"initialize","params":{"_meta":42}}),
            json!({"jsonrpc":"2.0","id":5,"method":"initialize","params":{"_meta":["a"]}}),
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{}}),
        ] {
            let _ = sanitize_caller_frame(&mut frame, Some(&peer));
            assert_eq!(
                frame["params"]["_meta"]["mcpmesh/peer"], peer,
                "a request with object (or absent) params must end up attributed: {frame}"
            );
        }

        // POSITIONAL params are left ALONE — arguments, not metadata (#45 gate).
        //
        // These used to be REPLACED with `{"_meta":{...}}`, silently deleting the caller's
        // arguments. That was near-dead while injection only touched `initialize` (MCP always
        // sends object params there); widening to every request would have made it fire on every
        // positional call a non-MCP JSON-RPC backend received. The daemon pumps rather than
        // interprets, so it does not get to rewrite arguments it cannot annotate — the backend
        // simply sees no identity, which is fail-closed.
        for mut frame in [
            json!({"jsonrpc":"2.0","id":3,"method":"initialize","params":["an","array"]}),
            json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":[1,2,3]}),
            json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":"a string"}),
        ] {
            let before = frame.clone();
            let _ = sanitize_caller_frame(&mut frame, Some(&peer));
            assert_eq!(
                frame, before,
                "positional/scalar params are the caller's arguments and must survive intact, \
                 un-attributed, rather than being replaced: {frame}"
            );
        }

        // A non-OBJECT, non-ARRAY frame cannot carry a method, so it is not a handshake: passed
        // through untouched rather than coerced into an object that invents an `initialize`.
        for original in [json!("a bare string frame"), json!(null), json!(7)] {
            let mut frame = original.clone();
            let _ = sanitize_caller_frame(&mut frame, Some(&peer));
            assert_eq!(
                frame, original,
                "a scalar frame is not an initialize and must pass through unchanged"
            );
        }
    }

    /// #49: BOTH spellings of the identity key are written, with the identical value.
    ///
    /// A test asserting only the reverse-DNS key would pass on an implementation that dropped the
    /// legacy one — which would make every EXISTING backend silently stop seeing an identity.
    /// Backends are third-party processes reading `mcpmesh/peer`, not something version-locked to
    /// this daemon, and one that reads "no identity" as "local caller" fails OPEN. That is the
    /// failure this dual write exists to prevent, so both halves are asserted.
    #[test]
    fn both_spellings_of_the_peer_key_are_written_with_the_same_value() {
        let peer = json!({"eid":"eid:aa","name":"bob","user_id":null,"groups":[]});
        let mut frame = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{}});
        let _ = sanitize_caller_frame(&mut frame, Some(&peer));

        let meta = &frame["params"]["_meta"];
        assert_eq!(
            meta["mcpmesh/peer"], peer,
            "the LEGACY key must keep working for existing backends: {meta}"
        );
        assert_eq!(
            meta["tech.counterpunch.mcpmesh/peer"], peer,
            "and the reverse-DNS key must be there for new ones: {meta}"
        );
        assert_eq!(
            meta["mcpmesh/peer"], meta["tech.counterpunch.mcpmesh/peer"],
            "the two must never diverge — a backend reading either gets the same answer: {meta}"
        );

        // A caller forging EITHER spelling is replaced, not merged, in both slots.
        let mut forged = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"_meta":{
            "mcpmesh/peer": {"name":"attacker","groups":["admin"]},
            "tech.counterpunch.mcpmesh/peer": {"name":"attacker","groups":["admin"]},
        }}});
        let _ = sanitize_caller_frame(&mut forged, Some(&peer));
        for k in ["mcpmesh/peer", "tech.counterpunch.mcpmesh/peer"] {
            assert_eq!(
                forged["params"]["_meta"][k], peer,
                "a forged `{k}` is replaced with the authenticated value: {forged}"
            );
        }
    }

    /// #164 gate: a JSON-RPC BATCH bypassed both halves. `pointer_mut("/params/_meta")` and
    /// `get("method")` both resolve to nothing on an array root, so wrapping the forged frame in
    /// `[ ... ]` carried it through untouched. rmcp 3.1.0 does not unwrap batches, but an older SDK
    /// or a custom NDJSON server does — and this daemon pumps rather than interprets, so the
    /// invariant cannot depend on which server is behind it.
    #[test]
    fn a_batch_cannot_smuggle_a_forged_peer_past_the_strip() {
        let peer = json!({"eid":"eid:aa","name":"bob","user_id":null,"groups":[]});
        let forged = json!({"mcpmesh/peer": {"name":"someone-else","groups":["admin"]},
                            "mcpmesh/service": "not-yours", "app/keep": "yes"});

        let mut batch = json!([
            {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"_meta": forged}},
            {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"_meta": forged}},
        ]);
        let _ = sanitize_caller_frame(&mut batch, Some(&peer));

        assert_eq!(
            batch[0]["params"]["_meta"]["mcpmesh/peer"], peer,
            "an array-wrapped initialize must still be authoritatively attributed: {batch}"
        );
        assert!(
            batch[0]["params"]["_meta"].get("mcpmesh/service").is_none(),
            "and a forged mcpmesh/service inside a batch must be stripped: {batch}"
        );
        assert_eq!(
            batch[1]["params"]["_meta"]["mcpmesh/peer"], peer,
            "since #45 ask 2 an ordinary request inside a batch is attributed too — and the forged \
             value is REPLACED, not merged: {batch}"
        );
        assert_eq!(
            batch[0]["params"]["_meta"]["app/keep"], "yes",
            "non-reserved keys survive inside a batch too: {batch}"
        );

        // Nested past the depth bound: not a request any server unwraps, and it must not recurse
        // without limit. It must still not PANIC, and the outer levels are handled.
        let mut deep = json!([[[[[[[[[[{"jsonrpc":"2.0","method":"initialize",
                                        "params":{"_meta":{"mcpmesh/peer":{"name":"x"}}}}]]]]]]]]]]);
        let _ = sanitize_caller_frame(&mut deep, Some(&peer));

        // And with no identity, a batch is still stripped.
        let mut b2 = json!([{"jsonrpc":"2.0","id":1,"method":"initialize",
                             "params":{"_meta":{"mcpmesh/peer":{"name":"attacker"}}}}]);
        let _ = sanitize_caller_frame(&mut b2, None);
        assert!(
            b2[0]["params"]["_meta"].get("mcpmesh/peer").is_none(),
            "a run backend's batch is stripped too: {b2}"
        );
        // And with no identity, an odd shape must not panic either.
        for mut frame in [json!(null), json!("s"), json!({"params": 7})] {
            let _ = sanitize_caller_frame(&mut frame, None);
        }
    }

    #[tokio::test]
    async fn a_server_initiated_notification_reaches_the_peer_unmetered() {
        timeout(Duration::from_secs(10), async {
            let (mut peer_w, tr) = duplex(64 * 1024);
            let (tw, peer_r) = duplex(64 * 1024);
            let mut transport = NdjsonTransport::new(tr, tw, MAX_FRAME_BYTES);
            let (server_write, srv_stdin) = duplex(64 * 1024);
            let (srv_stdout, server_read) = duplex(64 * 1024);

            // The server answers the request AND pushes an unsolicited notification afterwards.
            let server = tokio::spawn(async move {
                let mut srv_w = srv_stdout;
                let mut reader = FrameReader::new(srv_stdin, MAX_FRAME_BYTES);
                let mut seen = 0usize;
                while let Ok(Some(Inbound::Frame(f))) = reader.next().await {
                    seen += 1;
                    if f["method"] == "tools/call" {
                        write_frame(&mut srv_w, &json!({"jsonrpc":"2.0","id":f["id"]}))
                            .await
                            .unwrap();
                        // UNSOLICITED: no id, a method, nobody asked for it.
                        write_frame(
                            &mut srv_w,
                            &json!({"jsonrpc":"2.0","method":"notifications/message",
                                    "params":{"level":"info","data":"pushed"}}),
                        )
                        .await
                        .unwrap();
                    }
                }
                seen
            });

            // Budget of ONE request per minute — consumed by `tools/call` below.
            // Some(endpoint), NOT None: RateGate::admit_at returns Ok unconditionally for a
            // None identity, so a None gate meters NOTHING and cannot distinguish the property
            // under test from an absent limiter. The first version of this test used None and was
            // therefore vacuous — metering Direction B did not break it.
            let rate = RateGate::new(
                std::sync::Arc::new(crate::limits::RateLimiter::per_minute(1, 1)),
                Some(mcpmesh_net::EndpointId::from_bytes([9u8; 32])),
            );
            let pump_task = tokio::spawn(async move {
                pump(
                    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
                    &mut transport,
                    server_read,
                    server_write,
                    RequestAuditor::new(
                        AuditSink::disabled(),
                        Some("bob".into()),
                        "echo".into(),
                        None,
                    ),
                    rate,
                    None,
                )
                .await
            });

            write_frame(
                &mut peer_w,
                &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{}}),
            )
            .await
            .unwrap();
            // A SECOND metered request makes the exhaustion OBSERVABLE rather than assumed.
            write_frame(
                &mut peer_w,
                &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{}}),
            )
            .await
            .unwrap();

            let mut peer_reader = FrameReader::new(peer_r, MAX_FRAME_BYTES);
            let mut saw_limited = false;
            let reply = loop {
                match peer_reader.next().await.unwrap() {
                    Some(Inbound::Frame(f)) => {
                        if f["error"]["code"] == -32053 {
                            saw_limited = true;
                            continue;
                        }
                        if f["id"] == 2 {
                            break f;
                        }
                    }
                    other => panic!("expected the id=2 reply, got {other:?}"),
                }
            };
            assert_eq!(reply["id"], 2, "the solicited reply arrives");
            assert!(
                saw_limited,
                "the per-identity budget must be EXHAUSTED by now — without that this test cannot \
                 distinguish 'Direction B is unmetered' from 'there was budget left'"
            );

            let pushed = match peer_reader.next().await.unwrap() {
                Some(Inbound::Frame(f)) => f,
                other => panic!("expected the server-initiated notification, got {other:?}"),
            };
            assert_eq!(
                pushed["method"], "notifications/message",
                "an unsolicited server notification must reach the peer — this is what makes push \
                 possible instead of polling (#91): {pushed}"
            );
            assert!(
                pushed.get("id").is_none_or(|v| v.is_null()),
                "and it is a notification, not a request: {pushed}"
            );

            drop(peer_w);
            let _ = server.await;
            let _ = pump_task.await;
        })
        .await
        .expect("server-initiated notification test timed out");
    }

    /// The client sends `initialize` + one request and then EOFs the transport; the
    /// server replies to BOTH only after seeing its stdin close. Both replies must
    /// still reach the peer (the old select!-cancel dropped them) and `pump` must
    /// return Ok. Looped because the pre-fix loss was a scheduling coin flip
    /// (`select!` polls branches in random order).
    #[tokio::test]
    async fn transport_eof_does_not_drop_replies_still_inside_the_server() {
        timeout(Duration::from_secs(10), async {
            for _ in 0..25 {
                // Mesh transport, one whole DuplexStream per direction: peer→pump
                // (dropping `peer_w` = the peer's EOF) and pump→peer.
                let (mut peer_w, tr) = duplex(64 * 1024);
                let (tw, peer_r) = duplex(64 * 1024);
                let mut transport = NdjsonTransport::new(tr, tw, MAX_FRAME_BYTES);
                // The server's stdio, likewise: pump→server stdin, server stdout→pump.
                let (server_write, srv_stdin) = duplex(64 * 1024);
                let (srv_stdout, server_read) = duplex(64 * 1024);

                // The fake server: collect every inbound frame, reply ONLY after stdin
                // EOF (so any teardown racing the drain is caught), then close stdout.
                let server = tokio::spawn(async move {
                    let mut srv_w = srv_stdout;
                    let mut reader = FrameReader::new(srv_stdin, MAX_FRAME_BYTES);
                    let mut seen = Vec::new();
                    while let Ok(Some(Inbound::Frame(f))) = reader.next().await {
                        seen.push(f);
                    }
                    // stdin EOF'd — the pump's Direction A closed it. Now echo each
                    // frame back as its "reply" and close stdout (session end).
                    for f in &seen {
                        write_frame(&mut srv_w, &json!({"jsonrpc": "2.0", "id": f["id"]}))
                            .await
                            .unwrap();
                    }
                    seen.len()
                });

                let pump_task = tokio::spawn(async move {
                    pump(
                        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
                        &mut transport,
                        server_read,
                        server_write,
                        RequestAuditor::new(
                            AuditSink::disabled(),
                            Some("bob".into()),
                            "echo".into(),
                            None,
                        ),
                        RateGate::new(RateLimiter::unlimited_shared(), None),
                        None,
                    )
                    .await
                });

                // The one-shot client shape: one request behind the initialize, then EOF.
                write_frame(
                    &mut peer_w,
                    &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{}}),
                )
                .await
                .unwrap();
                drop(peer_w);

                // BOTH replies drain back to the peer, in order, then a clean EOF.
                let mut peer_reader = FrameReader::new(peer_r, MAX_FRAME_BYTES);
                for expect_id in [1, 2] {
                    match peer_reader.next().await.unwrap() {
                        Some(Inbound::Frame(f)) => assert_eq!(
                            f["id"], expect_id,
                            "the reply to request {expect_id} must survive transport EOF: {f}"
                        ),
                        other => panic!("expected the id={expect_id} reply, got {other:?}"),
                    }
                }
                assert!(
                    peer_reader.next().await.unwrap().is_none(),
                    "after the server's output EOF the session closes cleanly"
                );
                assert_eq!(
                    server.await.unwrap(),
                    2,
                    "the server saw initialize + request"
                );
                pump_task.await.unwrap().expect("pump returns Ok");
            }
        })
        .await
        .expect("pump drain test timed out");
    }
}
