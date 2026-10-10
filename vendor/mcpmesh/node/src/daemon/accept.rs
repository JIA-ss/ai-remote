//! The daemon's ALPN-dispatch accept loop: one loop routing each inbound
//! connection to the mesh / pairing / ping / gossip / blob handlers, the shared
//! gate-and-register discipline for the roster-mode arms, and the hot-reload that swaps the LIVE
//! service registry the loop serves from.

use std::sync::Arc;

use mcpmesh_net::framing::write_frame;
use mcpmesh_net::{ALPN_MCP, ALPN_PAIR, ALPN_PING, Services, run_mesh_connection};
use tokio::task::JoinHandle;

use crate::pairing;

use super::{MeshState, STACK_VERSION};

/// The shared gate + CHECK-register for the roster-mode ALPN accept arms (gossip, roster-blob,
/// app-blob): resolve the remote against the composed trust gate — an unresolved peer is refused
/// 401 — then `register_checked` the connection so a revocation/roster-drop severs it live
/// (`should_sever_now`). Returns the RAII registration the arm holds for the connection's
/// lifetime, or `None` AFTER closing the connection (the arm just returns). Extracting this keeps
/// the sever discipline in exactly ONE place across ALL gated ALPNs.
///
/// The sever discriminator is ROSTER membership (`gate.roster_user`, `None` for pairing),
/// captured at resolve time — NOT `identity.user_id`, which a paired peer also carries.
///
/// `blob_conn_limit` (the app-blob arm only): the per-endpoint app-blob connection
/// rate-limit. Consulted AFTER resolve so ONLY AUTHENTICATED endpoints
/// allocate a bucket — a stranger was already refused above (SECURITY invariant 4: strangers stay
/// cheap, no allocation, no make_room work) — and BEFORE the registry insert. The real threat is a
/// valid roster member with no scope grant (a STABLE roster id) churning blob connections whose
/// GETs are denied. FAIL-SAFE: over-limit → close (the accept-time 401 + request-time Permission
/// gates are unchanged; this only bounds connection churn).
fn gate_and_register(
    mesh: &Arc<MeshState>,
    conn: &iroh::endpoint::Connection,
    blob_conn_limit: bool,
) -> Option<mcpmesh_net::registry::Registration> {
    let remote = mcpmesh_net::EndpointId::from(conn.remote_id());
    if mesh.gate.resolve(&remote).is_none() {
        conn.close(mcpmesh_net::CLOSE_UNAUTHORIZED.into(), b"unauthorized");
        return None;
    }
    if blob_conn_limit && !mesh.limits().admit_blob_conn(&remote) {
        conn.close(0u32.into(), b"blob rate limited");
        return None;
    }
    let roster_user = mesh.gate.roster_user(&remote);
    let registration = mesh
        .conn_registry
        .register_checked(conn, roster_user.clone(), |eid| {
            mesh.gate.should_sever_now(eid, roster_user.as_deref())
        });
    if registration.is_none() {
        conn.close(mcpmesh_net::CLOSE_UNAUTHORIZED.into(), b"unauthorized");
    }
    registration
}

/// Spawn the daemon's own ALPN-dispatch accept loop on `endpoint`, returning its task handle.
///
/// The daemon runs THIS instead of [`mcpmesh_net::serve`] so it can route each accepted
/// connection by its negotiated ALPN: `mcpmesh/mcp/1` goes through net's gated
/// per-connection handler [`run_mesh_connection`]; `mcpmesh/pair/1` goes to the pairing
/// rendezvous — GATE-EXEMPT by design, authenticated by the invite secret, NOT the trust
/// gate (that is precisely why the mesh-only `serve` is not enough). An unknown ALPN is closed
/// cleanly.
///
/// The loop is started ONCE (`serve_forever`) and then runs for the process lifetime. A
/// hot-reload no longer restarts it: `swap_services` (shared by `register_service` and the pairing
/// `grant_service_access`) swaps `mesh.services` in place, which the loop and every connection it
/// has already accepted read live (#54).
///
/// Takes `Arc<MeshState>` (not the individual parts): the arms read the gate/limits/handles off
/// it, and the `mcpmesh/pair/1` branch hands the rendezvous the narrow per-connection
/// [`InviterCtx`](crate::pairing::rendezvous::InviterCtx) the mesh composes (`inviter_ctx` —
/// store + invites + the grant hook into the reload machinery). `services` is passed alongside
/// only to seed the live handle at startup.
///
/// `pub` (like [`build_services`](crate::daemon::build_services)) so integration tests can drive the SAME accept loop the daemon
/// runs against in-process endpoints, proving mesh vs. pair ALPN routing.
pub fn spawn_accept_loop(mesh: Arc<MeshState>, services: Arc<Services>) -> JoinHandle<()> {
    // INSTALL `services` as the live handle, then serve from that handle forever. The loop
    // captures only `mesh`: a reload swaps `mesh.services` IN PLACE, so connections this loop has
    // already accepted resolve their next session against the new registry (#54). The old
    // shape captured an `Arc<Services>` here, which is why aborting + respawning the loop could
    // never reach an open connection.
    mesh.services.store(services);
    tokio::spawn(async move {
        while let Some(incoming) = mesh.endpoint.accept().await {
            let mesh = mesh.clone();
            tokio::spawn(async move {
                // Inbound-handshake discipline (preserved from net's `serve`): a failed
                // handshake drops the connection. The handshake ERROR is logged at debug (a
                // transport/TLS/ALPN-negotiation error — the handshake never completed, so it
                // carries NO peer identity; logging `%e` is thus no surface leak) and helps
                // debug pairing dials.
                let conn = match incoming.await {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::debug!(%e, "inbound handshake failed");
                        return;
                    }
                };
                // iroh 1.0.1, verified: on an accepted
                // `Connection<HandshakeCompleted>`, `alpn() -> &[u8]` returns the negotiated
                // ALPN (NOT `Option<Vec<u8>>` — that form exists only on the 0-RTT states).
                // Copy it out so `conn` is free to move into the selected handler.
                let alpn = conn.alpn().to_vec();
                match alpn.as_slice() {
                    a if a == ALPN_MCP => {
                        // #92 item 2: watch THIS session's selected path, so a mid-session
                        // degradation pushes a frame when it happens rather than waiting for a
                        // probe. This arm — NOT `gate_and_register`, which serves only the
                        // gossip/roster-blob/app-blob arms and never sees ALPN_MCP.
                        //
                        // Gated first, so a stranger never gets a task spawned on its behalf
                        // (SECURITY invariant 4: strangers stay cheap). `run_mesh_connection`
                        // re-resolves and owns the real refusal; this is only a cheap precondition
                        // for spawning, never the security boundary.
                        let remote = mcpmesh_net::EndpointId::from(conn.remote_id());
                        if mesh.gate.resolve(&remote).is_some() {
                            drop(super::path_watch::spawn(
                                mesh.clone(),
                                *remote.as_bytes(),
                                &conn,
                            ));
                        }
                        run_mesh_connection(
                            conn,
                            mesh.gate.clone(),
                            mesh.services.clone(),
                            mesh.conn_registry.clone(),
                        )
                        .await;
                    }
                    a if a == ALPN_PAIR => {
                        // Live-invite accept-gate (the pair rendezvous is only "open" while
                        // an invite is live). iroh can't cheaply toggle an advertised
                        // ALPN on a live endpoint, so the pair ALPN stays advertised and we realize
                        // the windowed-listener semantics HERE — a dial with NO outstanding invite
                        // is closed immediately (no bi-stream, no hello, no handler task spawned to
                        // consume). `count()` is advisory (any-invite-live, coarse): if another
                        // conn burns the last invite first, this one still reaches `try_redeem` and
                        // gets `Unknown` → refused — so per-invite expiry/burn stays authoritative
                        // there, and this is a cheap front-door close, not the security boundary.
                        // #87 widened the WINDOW this closes, without changing this code. A
                        // single-use invite made the front door shut seconds after minting — the
                        // first redemption burned the only invite. A multi-use one keeps
                        // `count() > 0` for its whole TTL in the normal case, so a stranger
                        // dialing ALPN_PAIR gets past this fast-close and into the rate-limited
                        // rendezvous for up to 24h. That is the one genuinely new exposure to a
                        // caller WITHOUT the secret; it is bounded by the same rate limiter and
                        // strike budget that already protect the by-design-open pair ALPN, and by
                        // the operator choosing to mint a multi-use invite at all.
                        // #85 ask 3: an ATTESTATION carries no invite, so the invite-window
                        // fast-close would refuse it before the ceremony could run. With the knob
                        // on, the pair ALPN is therefore reachable continuously — the cost of the
                        // feature, stated in `[identity].admit_attested_devices` and the release
                        // notes rather than discovered. With it off (the default) this is exactly
                        // today's behaviour.
                        //
                        // What stays: the rate limiter below, the binding verification, and the
                        // rule that an attestation can only ever admit a device of a person this
                        // node already pairs with.
                        if mesh.invites.count() == 0 && !mesh.admit_attested_devices() {
                            // The shared constant (#87b): the redeemer matches these bytes off
                            // `close_reason()` to say "expired / used / inviter restarted"
                            // instead of a bare connection failure.
                            conn.close(
                                0u32.into(),
                                crate::pairing::rendezvous::NO_LIVE_INVITE_CLOSE,
                            );
                            return;
                        }
                        // Per-connection rate-limit of the by-design-open pair ALPN.
                        // A SINGLE global bucket — the pair ALPN accepts
                        // strangers who pick fresh ids, so a per-endpoint map would be defeated by
                        // fresh ids. Placed AFTER the no-invite fast-close so it bounds only the
                        // attempts that would proceed to the (more expensive) rendezvous while an
                        // invite is live. FAIL-SAFE: over-rate → close (a client retries as tokens
                        // refill). NOT the removed per-invite attempt cap; the 32-byte secret is the
                        // security.
                        if !mesh.limits().admit_pair_accept() {
                            conn.close(0u32.into(), b"pair rate limited");
                            return;
                        }
                        // #85 ask 4: the pair ALPN is GATE-EXEMPT by design — its authentication is
                        // the invite secret, not the allowlist — which is exactly why revocation
                        // has to be checked here explicitly.
                        //
                        // The gate already refuses a revoked device on every other ALPN, so it
                        // could never actually USE a re-pairing. But without this it could still
                        // complete one: burn an invite use, overwrite its own `PeerEntry`, and —
                        // through the grant hook — get its principal appended to `[services.*].allow`
                        // in config.toml, with a SAS and "paired" reported on both sides. An
                        // operator watching that would reasonably conclude the revocation had not
                        // taken. Refusing here keeps the two halves telling the same story.
                        //
                        // Same close reason as no-live-invite: a revoked device learning WHY it was
                        // refused is a disclosure with no upside. `gate.is_revoked` covers both
                        // tables (#218): a known device whose row carries a revoked IDENTITY is
                        // closed here too; a device with no row reaches the handler, proves the
                        // secret, and gets the coded identity refusal there.
                        let remote_id = mcpmesh_net::EndpointId::from(conn.remote_id());
                        if mesh.gate.is_revoked(&remote_id) {
                            tracing::warn!(
                                peer = %remote_id.principal(),
                                "refused a pair attempt from a REVOKED endpoint"
                            );
                            conn.close(
                                0u32.into(),
                                crate::pairing::rendezvous::NO_LIVE_INVITE_CLOSE,
                            );
                            return;
                        }
                        // The real inviter-side rendezvous, run against the narrow context the
                        // mesh composes: store + invites + the grant hook, so a successful pair
                        // can also GRANT service access (config-append + reload) without the
                        // module seeing the mesh. The error is a transport/protocol error (a
                        // malformed hello, a dropped stream) or a grant failure — it carries NO
                        // peer identity, so `%e` is no surface leak. Logged at debug.
                        if let Err(e) =
                            pairing::rendezvous::handle_inviter_side(conn, mesh.inviter_ctx()).await
                        {
                            tracing::debug!(%e, "pair rendezvous error");
                        }
                    }
                    a if a == ALPN_PING => {
                        // Reachability pong (pairing-mode liveness) — TRUST-GATED: only pong to a
                        // resolvable (paired) peer, so an unpaired scanner's dial is closed with NO
                        // pong and learns nothing (no presence leak). THIS gate is the
                        // security boundary of the probe (mirrors the `gate.resolve` refusal in
                        // `gate_and_register`). The EndpointId is not logged (surface-leak discipline).
                        let remote = mcpmesh_net::EndpointId::from(conn.remote_id());
                        let Some(identity) = mesh.gate.resolve(&remote) else {
                            conn.close(mcpmesh_net::CLOSE_UNAUTHORIZED.into(), b"unauthorized");
                            return;
                        };
                        // #89 ask 2: the presence POLICY, before the limiter and before any work.
                        //
                        // The arm is otherwise gated by PAIRING alone, so `service_allow_revoke`
                        // never reached it: a peer whose every service was revoked still learned
                        // you were online right now, your RTT, your stack_version and your app
                        // metadata — on demand, forever. The only lever was a full unpair, a
                        // relationship-destroying action used to express a privacy preference.
                        //
                        // `Granted` reads `caller_admitted_services`, which this arm already
                        // computes for the pong, so an embedder's existing per-peer sharing switch
                        // now controls presence too — LIVE, since grants are, with no restart.
                        //
                        // The refusal matches the trust gate's above (same code, same bytes) so
                        // this arm does not itself distinguish "not paired" from "hidden" from "no
                        // grants". Same discipline as the pairing redemption oracle.
                        //
                        // THIS IS NOT INVISIBILITY, and the docs must not claim it is (#89 gate).
                        // A QUIC application close happens only AFTER the handshake completes, so
                        // `connect()` returning Ok already proves this node is up. `ALPN_PAIR` is
                        // advertised unconditionally and answers ANY stranger; a paired peer still
                        // gets a served `ALPN_MCP` session and an application-layer refusal frame.
                        // What this mode actually withholds is the pong PAYLOAD — `stack_version`,
                        // `meta` (#40), and the caller's admitted `services` (#52) — and it makes
                        // mcpmesh's own probe report the peer unreachable.
                        //
                        // BEFORE the limiter's VERDICT deliberately: `PING_THROTTLE_CLOSE` is
                        // distinguishable on purpose (#142 — a throttled probe must not be written
                        // down as "offline"), so answering the throttle close here would tell a
                        // prober that it is paired and the node is up. The token is still SPENT
                        // (below) so a refused peer stays metered.
                        let mode = mesh.presence_mode();
                        // Skipped entirely under `Off`: the value is unused there, and it is a live
                        // registry scan per dial on a path an unmetered peer can drive.
                        let admitted = if mode == crate::daemon::PresenceMode::Off {
                            Vec::new()
                        } else {
                            crate::daemon::caller_admitted_services(&mesh, &identity)
                        };
                        let pong_allowed = match mode {
                            crate::daemon::PresenceMode::Paired => true,
                            crate::daemon::PresenceMode::Granted => !admitted.is_empty(),
                            crate::daemon::PresenceMode::Off => false,
                        };
                        if !pong_allowed {
                            // SPEND a token, then discard the verdict. Returning early without
                            // metering left the arm completely unmetered for exactly the peers a
                            // hidden node most wants bounded — undoing #89 ask 1 for this mode
                            // (#89 gate). Discarding the verdict is what keeps the close identical
                            // whether or not the bucket is empty.
                            let _ = mesh.limits().admit_ping(&remote);
                            conn.close(mcpmesh_net::CLOSE_UNAUTHORIZED.into(), b"unauthorized");
                            return;
                        }
                        // #89: meter the probe per authenticated endpoint. The arm was gated but
                        // UNMETERED, so a paired peer could pong-flood and the only bound was its
                        // own politeness. AFTER the gate, so an unpaired scanner still allocates
                        // nothing (SECURITY invariant 4: strangers stay cheap). The refusal is
                        // DISTINGUISHABLE from the gate's (`PING_THROTTLE_CLOSE`, sibling idiom of
                        // the pair/blob limiters) so the prober can treat it as non-evidence rather
                        // than writing a false "peer offline" (#142 gate, HIGH). This leaks nothing:
                        // only an authenticated PAIRED peer can reach the limiter, and a flooding
                        // one interleaves refusals with real pongs as the bucket refills, so it
                        // already holds proof it is paired. An unpaired scanner still gets
                        // `unauthorized` above, unchanged.
                        if !mesh.limits().admit_ping(&remote) {
                            // No endpoint id in the log (surface-leak discipline); the count lives
                            // on `MeshLimiters::pings_refused` — a probe is not a session, so this
                            // is its only footprint besides the close itself.
                            tracing::debug!("ping probe refused: rate limited");
                            conn.close(0u32.into(), crate::daemon::reach::PING_THROTTLE_CLOSE);
                            return;
                        }
                        // The dialer opens the bi-stream and sends one ping frame (which is what
                        // makes `accept_bi` resolve — a silent QUIC stream is invisible to the peer);
                        // we ignore its content and write the single pong. `finish()` + `stopped()`
                        // ensure the pong is ACKed before `conn` drops (the pairing `send_reply`
                        // discipline — a bare drop could preempt the un-acked reply).
                        if let Ok((mut send, _recv)) = conn.accept_bi().await {
                            // The pong carries our stack version AND (#40) our optional app
                            // metadata — the SAME ≤256B value #39 gossips on presence, here
                            // handed to a paired peer over this AUTHENTICATED channel (no
                            // signature needed: the QUIC/TLS session already proves it is us).
                            // Omitted when empty so a metadata-less pong is byte-shape-identical
                            // to the pre-#40 pong.
                            let meta = mesh.app_metadata();
                            // #52: the pong ALSO carries the services THIS caller is currently
                            // admitted to — the discovery answer, computed on the side that owns
                            // the truth. Only the caller's own admitted services (never the full
                            // registry). Empty list omitted (keeps a no-share pong compact).
                            let services = &admitted;
                            let mut pong = serde_json::json!({ "stack_version": STACK_VERSION });
                            if !meta.is_empty() {
                                pong["meta"] = serde_json::json!(meta);
                            }
                            if !services.is_empty() {
                                pong["services"] = serde_json::json!(services);
                            }
                            if write_frame(&mut send, &pong).await.is_ok() {
                                let _ = send.finish();
                                let _ = send.stopped().await;
                            }
                        }
                    }
                    a if a == crate::roster::transport::GOSSIP_ALPN => {
                        // Roster/presence gossip. Gate + register
                        // via [`gate_and_register`] (the shared sever discipline: unresolved → 401,
                        // revocation/roster-drop severs live gossip connections too); only THEN is
                        // the connection handed to the gossip `ProtocolHandler`. A pure-pairing
                        // daemon never advertised this ALPN → `gossip` is `None` → close.
                        let Some(gossip) = mesh.gossip.clone() else {
                            conn.close(0u32.into(), b"gossip not enabled");
                            return;
                        };
                        let Some(_registration) = gate_and_register(&mesh, &conn, false) else {
                            return;
                        };
                        if let Err(e) = iroh::protocol::ProtocolHandler::accept(&gossip, conn).await
                        {
                            tracing::debug!(%e, "gossip accept error");
                        }
                    }
                    a if a == crate::roster::transport::BLOB_ALPN => {
                        // Roster-blob provider (— the signed roster document only; ungated per
                        // scope). The [`gate_and_register`] gate on THIS arm is the access
                        // boundary — same gate + register + hand-off as the gossip arm, so a revocation
                        // severs blob connections too. `None` blobs (pure-pairing) → close.
                        let Some(blobs) = mesh.blobs.clone() else {
                            conn.close(0u32.into(), b"blobs not enabled");
                            return;
                        };
                        let Some(_registration) = gate_and_register(&mesh, &conn, false) else {
                            return;
                        };
                        let blob_proto = blobs.protocol();
                        if let Err(e) =
                            iroh::protocol::ProtocolHandler::accept(&blob_proto, conn).await
                        {
                            tracing::debug!(%e, "blob accept error");
                        }
                    }
                    a if a == crate::blobs::APP_BLOB_ALPN => {
                        // The GATED per-scope app-blob provider. TWO LAYERS:
                        // (1) ACCEPT-TIME gate — the SAME [`gate_and_register`] resolve → 401 +
                        //     register_checked/should_sever_now as the roster BLOB_ALPN arm — PLUS
                        //     the per-endpoint connection rate-limit (`blob_conn_limit`, see the
                        //     helper doc): a revoked/unknown endpoint gets nothing regardless of the
                        //     ticket/hash it holds, and a revocation severs live app-blob
                        //     connections too.
                        // (2) REQUEST-TIME gate — inside the provider's Intercept drain loop:
                        //     a valid-but-ungranted caller is refused with Permission before any bytes.
                        // `None` app_blobs (pure-pairing / build failed) → close cleanly.
                        let Some(app_blobs) = mesh.app_blobs().await else {
                            conn.close(0u32.into(), b"app blobs not enabled");
                            return;
                        };
                        let Some(_registration) = gate_and_register(&mesh, &conn, true) else {
                            return;
                        };
                        let blob_proto = app_blobs.protocol();
                        if let Err(e) =
                            iroh::protocol::ProtocolHandler::accept(&blob_proto, conn).await
                        {
                            tracing::debug!(%e, "app-blob accept error");
                        }
                    }
                    // #67: an EMBEDDER-registered protocol, if one claims this ALPN.
                    //
                    // Consulted only for ALPNs none of the arms above own, so a registration can
                    // never shadow a built-in — and `register_app_protocol` additionally refuses
                    // the whole `mcpmesh/` namespace, so it cannot even try, nor be broken by a
                    // future `mcpmesh/*` protocol landing above it.
                    //
                    // Gated through the SAME `gate_and_register` as every other arm. That is the
                    // entire value of this seam over an embedder standing up its own endpoint: the
                    // custom protocol inherits authorization, the connection registry, and
                    // severing on revocation, rather than being a second door into the node.
                    other if mesh.app_protocol(other).is_some() => {
                        let Some(handler) = mesh.app_protocol(other) else {
                            return; // unregistered between the guard and here
                        };
                        let Some(_registration) = gate_and_register(&mesh, &conn, false) else {
                            return;
                        };
                        // `_registration` is held for the handler's whole life, so the connection
                        // stays in the registry and a revocation severs it mid-protocol.
                        if let Err(e) =
                            iroh::protocol::DynProtocolHandler::accept(handler.as_ref(), conn).await
                        {
                            tracing::debug!(%e, "app protocol accept error");
                        }
                    }
                    // An endpoint we never advertised should be unreachable (ALPN negotiation
                    // rejects it at handshake), but close defensively rather than hang.
                    _ => conn.close(0u32.into(), b"unknown alpn"),
                }
            });
        }
    })
}

/// Hot-swap the live service registry every accepted connection reads.
///
/// Replaces the former abort-and-respawn of the accept loop (#54): the loop reads
/// `mesh.services` per connection and `run_mesh_connection` reads it per session, so a swap
/// reaches connections that are ALREADY open — which respawning could not, because the
/// per-connection tasks are independent `tokio::spawn`s that aborting the loop never touched.
/// It also removes the brief window in which no accept loop was running.
///
/// Shared by [`register_service`] and [`grant_service_access`] so the discipline lives in exactly
/// ONE place (DRY). The CALLER holds `mesh.reload_lock` for the whole config→reload→swap section.
pub(crate) fn swap_services(mesh: &Arc<MeshState>, services: Services) {
    mesh.services.store(Arc::new(services));
}
