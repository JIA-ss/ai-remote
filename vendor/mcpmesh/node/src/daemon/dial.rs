//! The daemon's OUTBOUND dial machinery plus the session pipe: nickname/person →
//! endpoint resolution, the staggered person→device race, the explicit dial timeout, and the
//! control↔mesh byte pipe with its service-name injection. Split out of `daemon.rs`
//! mechanically — no API change; `daemon` re-exports the public entry points.
//!
//! Since #215 a session to a peer this node already holds a mesh connection to is a new bi-stream
//! on that connection rather than a new dial — see `conn_cache`.
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use mcpmesh_net::SessionTransport;
use mcpmesh_net::framing::{FrameReader, Inbound, write_frame};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use super::MeshState;
use crate::allowlist::PeerEntry;

/// Resolve `peer` to a session over the mesh, preferring the roster PERSON→DEVICE path
/// and falling back to the single-nickname path.
///
/// **Person→device (roster mode).** When `peer` names a roster USER that has active devices
/// (`mesh.roster.view().devices_for_user(peer)` non-empty), its devices are dialed as a STAGGERED
/// RACE ([`race_dial`]) ordered primary→mirror, then re-ordered WITHIN each role by presence recency
/// — see `order_dial_candidates`. Three safety invariants hold here (DECLARED):
///  - **Presence is ADVISORY — absence NEVER removes a candidate.** Recency only RE-ORDERS candidates
///    within a role; a device with NO presence entry is still dialed (just later in its role group).
///    If the person publishes no presence at all, ALL its devices are dialed in primary→mirror order.
///    (Rationale: presence is suppressible by an attacker; if absence removed a candidate, suppressing
///    presence would deny service.)
///  - **Revoked devices are NEVER candidates.** `devices_for_user` returns only ACTIVE endpoints
///    (revoked ones were excluded at `build_view`), so a revoked device can never be raced to.
///  - **Each candidate is authenticated by endpoint_id = pubkey.** A candidate endpoint_id IS an
///    ed25519 public key; `net::connect` establishes a QUIC/TLS session to the holder of that key, so
///    a racing candidate cannot be MITM'd — we reach the actual device or the dial fails. The peer's
///    own gate still authorizes us on their side; racing adds NO new trust decision on our side beyond
///    "this endpoint is an active roster device of the named user."
///
/// **Single-nickname fallback.** Otherwise resolve the nickname to its stored [`PeerEntry`] via the
/// allowlist store and dial an [`iroh::EndpointAddr`] carrying the entry's pairing-persisted
/// `last_addr` hint when usable (`stored_dial_addr`) — else id-only. iroh merges provided direct
/// addrs with what discovery (DNS/pkarr under the N0 preset) resolves FROM the id, so the hint makes
/// a COLD daemon able to reach a paired peer even with discovery disabled (issue #27) without ever
/// narrowing the discovery path. On LOCALHOST tests the connecting endpoint is
/// seeded via a `MemoryLookup` on `endpoint.address_lookup()`, so the same dial resolves
/// locally. The (blocking) redb read runs on `spawn_blocking` (never redb IO on a runtime worker).
///
/// [`PeerEntry`]: crate::allowlist::PeerEntry
/// Refuse to DIAL a revoked endpoint (#85 ask 4).
///
/// Revocation was inbound-only in the first cut: the gate refused a revoked device's connections,
/// but the outbound paths — `open_session`, `peer_services` — still read `PeerStore` directly and happily connected to the machine the operator had just declared
/// stolen, handing it the request. The 0.45.0 gate proved it.
///
/// This comment used to list BLOB SOURCES among the covered paths, and they were not: they, and
/// `Node::connect_protocol`, resolved through `protocol_candidates` with no filter at all until
/// #223. The paths [`dial_refused`]'s doc lists now ask it; the same doc lists the dials that do
/// NOT, so neither list is a claim of full coverage.
///
/// That made the weaker verb stronger: `peer_remove` DELETES the row, so it blocked the dial;
/// `peer_revoke`, the compromise claim, did not. Backwards, and in the direction that leaks data to
/// whoever holds the device.
///
/// Both tables (#218, `PeerStore::is_refused`): a device carrying a revoked IDENTITY is refused
/// inbound by the gate, so dialling it would be this same backwards verb one level up.
///
/// Fails CLOSED on a read error, like every other revocation read. The reads are redb, so they run on
/// the blocking pool — never on a runtime worker.
async fn refuse_if_revoked(mesh: &Arc<MeshState>, id: [u8; 32], peer: &str) -> Result<()> {
    let m = mesh.clone();
    if crate::util::blocking("join dial revocation check", move || dial_refused(&m, &id)).await? {
        return Err(revoked_refusal(peer));
    }
    Ok(())
}

/// The refusal every single-target outbound dial returns for a revoked peer — `open_session`'s
/// `eid:` and nickname paths, and `connect_protocol` when every candidate is refused (#223), so an
/// embedder sees one shape whichever verb it called.
fn revoked_refusal(peer: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{peer} is REVOKED on this node — dialling it would hand the request to a device you \
         declared compromised. Use `mcpmesh revoke undo` if that was a mistake"
    )
}

/// Would dialling this endpoint reach a device this node has REVOKED? The one OUTBOUND revocation
/// predicate (#223). Asked by: `open_session` (`refuse_if_revoked` and the race filter in
/// `hinted_addrs`), `connect_protocol` and blob sources (`protocol_candidates`), a blob ticket's
/// publisher (`blob_fetch`), `peer_services` (the only resolver caller that dials), the
/// reachability probe (`reach::probe_peer`) — and its sibling cache writer, the session path
/// watcher (`path_watch::observe`), which dials nothing but must not write the row the probe
/// refuses to — the roster-gossip blob provider
/// (`roster::distribute::on_announce`, through `DistributionHost::dial_refused`), and the gossip
/// BOOTSTRAP set (`boot::gossip_bootstrap`, through [`refused_by`]).
///
/// **Beneath all of them, the endpoint hook (#229).** A booted node's endpoint carries
/// [`MeshHooks`](super::hooks::MeshHooks), whose `before_connect` asks [`refused_by`] for every
/// dial on every ALPN except the pairing one — so iroh-gossip's LEARNED peers (ForwardJoin,
/// Shuffle), which gossip dials without asking this node, are refused too. The call-site filters
/// above stay: they refuse with a useful error before resolving anything, where the hook can only
/// answer "rejected locally". Connections already open when a revocation lands are closed through
/// the same hook's registry by `sever_principals` (`peer_revoke`, `device_revoke`,
/// `device_revocation_import`) and `install_roster_view_and_sever` (every roster install).
///
/// **Not covered** — outbound traffic that can still reach a revoked device:
///
/// - **An endpoint without the hook**: a mesh a test assembles by hand over `build_endpoint(..,
///   None)`.
/// - **`AppBlobs::fetch` / `AppBlobs::fetch_from`**, the provider's raw pub API, filter nothing
///   themselves. On a booted node the provider shares the hooked endpoint, so its dials should meet
///   the veto, but no test drives that path; the daemon verb (`blob_fetch`) filters before calling.
/// - **The roster half of the pairing dials.** `redeem_invite` and `attest_to` hold a `PeerStore`
///   and no roster, so they check [`PeerStore::is_refused`] alone: a roster-revoked endpoint, or a
///   roster device under a revoked `b64u:` identity, is not refused there.
///
/// It mirrors what the composed gate refuses INBOUND, because dialling a device the gate would
/// refuse is #85 ask 4's backwards verb:
///
/// 1. [`PeerStore::is_refused`] — the endpoint revocation table, and the identity the pair row
///    carries (#218).
/// 2. The installed roster's `revoked_endpoints` — composed-gate rule 1's roster half.
/// 3. A ROSTER device whose roster `user_id` is spelled `b64u:X` with `X` identity-revoked —
///    composed-gate rule 2, through the SAME [`PeerStore::is_roster_user_revoked`] the gate calls.
///    [`PeerStore::is_refused`] answers `Unpaired` for a roster-only device, so without this the
///    person→device race and an `eid:` dial reached a device the gate refuses.
///
/// Reads the roster VIEW rather than `RosterGate::roster_user`, which answers `None` for a
/// degraded-stopped roster: the race draws its candidates from the view whatever its state, so the
/// filter must see the same devices the race does. That can only refuse more, never less.
///
/// Blocking (redb): call it from the blocking pool. Fails CLOSED — every read it composes does.
///
/// [`PeerStore::is_refused`]: crate::allowlist::PeerStore::is_refused
/// [`PeerStore::is_roster_user_revoked`]: crate::allowlist::PeerStore::is_roster_user_revoked
pub(crate) fn dial_refused(mesh: &MeshState, id: &[u8; 32]) -> bool {
    refused_by(&mesh.store, mesh.roster.view().as_deref(), id)
}

/// [`dial_refused`] over its two inputs, for a caller that holds them before a `MeshState` exists —
/// the boot-time gossip bootstrap. The ONE definition; `dial_refused` is this over the mesh.
pub(crate) fn refused_by(
    store: &crate::allowlist::PeerStore,
    view: Option<&mcpmesh_trust::roster::validate::RosterView>,
    id: &[u8; 32],
) -> bool {
    if store.is_refused(id) {
        return true;
    }
    let Some(view) = view else {
        return false;
    };
    view.is_revoked(id)
        || view
            .resolve(id)
            .is_some_and(|d| store.is_roster_user_revoked(&d.user_id))
}

pub async fn dial_service(
    mesh: &Arc<MeshState>,
    peer: &str,
    service: &str,
) -> Result<SessionTransport> {
    dial_service_with_idle_timeout(mesh, peer, service, None).await
}

/// Say so when a RACING dial drops a caller's per-session transport config (#166 gate).
///
/// `race_dial` opens several connections and keeps the winner, so applying a transport config there
/// would apply it to dials about to be abandoned. Dropping it is the right call; dropping it
/// SILENTLY is not — a caller who asked for a 120s session and got the node default has no way to
/// tell, which is the "knob that quietly did nothing" this file refuses twice over.
///
/// Not an error: the same `open_session` call is a racing dial or not depending on how many devices
/// the peer happens to have, which the caller cannot know. Refusing would make a legal request fail
/// for a reason outside the caller's control.
fn warn_if_per_session_dropped(
    per_conn: &Option<iroh::endpoint::QuicTransportConfig>,
    peer: &str,
    why: &str,
) {
    if per_conn.is_some() {
        tracing::warn!(
            peer,
            "idle_timeout_secs was IGNORED: {why} resolves to a racing dial, which opens several \
             connections and keeps the winner. Name one device with eid: to apply it"
        );
    }
}

/// Build the COMPLETE per-session transport config for a caller-supplied idle timeout (#166).
///
/// **Returns the config rather than mutating a builder, so a test can assert what was actually
/// set** — the discipline `build_transport_config` already follows, and the reason the 0.48.0 gate
/// could prove the bug below in one line.
///
/// `ConnectOptions::with_transport_config` REPLACES the endpoint's config rather than overlaying
/// it, so this must carry the node's OWN keepalive too. The first cut built only the idle timeout:
/// a node with `[network].keep_alive_secs = 2` then got a per-session connection with iroh's 5s
/// keepalive, and a 3s idle timeout — which this function had just validated as safe against 2s —
/// severed sessions whose peers were alive and answering. The guard was checking a keepalive the
/// connection did not have.
///
/// `secs`:
/// - `None` → `Ok(None)`: inherit the endpoint's config, today's behaviour.
/// - `Some(0)` → no idle timeout FROM THIS SIDE (the peer's value still bounds the connection),
///   the same meaning `[network].idle_timeout_secs` gives it.
/// - `Some(s)` → refused at or below `keepalive_secs`, and refused if QUIC cannot encode it. Both
///   are ERRORS, never a silent fallback to the node default: a knob that quietly did nothing is
///   what the #56 gate found twice in this same area.
pub(crate) fn per_session_transport_config(
    secs: Option<u64>,
    keepalive_secs: u64,
) -> Result<Option<iroh::endpoint::QuicTransportConfig>> {
    let idle = match secs {
        None => return Ok(None),
        Some(0) => None,
        Some(s) => {
            // A keepalive arriving after the idle timer has fired severs a session whose peer is
            // alive and answering — never what "cut this one fast if it goes quiet" meant.
            // `[network].keep_alive_secs` is validated against the idle timeout at boot for exactly
            // this reason; this is the same rule at the other end. Refused rather than clamped: a
            // clamped value reads back as honoured and is not.
            anyhow::ensure!(
                s > keepalive_secs,
                "idle_timeout_secs ({s}) must be greater than this node's keepalive interval \
                 ({keepalive_secs}s): a keepalive arriving after the idle timer has fired would \
                 sever the session on a clock, even against a peer that is alive and answering"
            );
            Some(
                iroh::endpoint::IdleTimeout::try_from(std::time::Duration::from_secs(s)).map_err(
                    |e| {
                        anyhow::anyhow!(
                            "idle_timeout_secs {s} is out of the range QUIC can encode: {e}"
                        )
                    },
                )?,
            )
        }
    };
    let d = std::time::Duration::from_secs(keepalive_secs);
    Ok(Some(
        iroh::endpoint::QuicTransportConfig::builder()
            .max_idle_timeout(idle)
            // BOTH keepalives, mirroring `build_transport_config` — iroh pings on the PATH value,
            // so setting only the connection-level one leaves a 5s path ping running.
            .keep_alive_interval(d)
            .default_path_keep_alive_interval(d)
            .build(),
    ))
}

/// [`dial_service`] with a per-connection QUIC idle timeout (#166).
///
/// Applied on the SINGLE-TARGET paths only — an explicit `eid:` and a single stored entry. The
/// RACING paths (a roster person→device race, a multi-device `user_id`) ignore it: `race_dial`
/// opens several connections and keeps the winner, so a transport config there would be applied to
/// dials that are about to be abandoned. A caller who needs a specific timeout for a specific
/// device names that device with `eid:`, which is the answer #41 gives for every other per-device
/// concern. Documented on the param rather than silently partial.
pub async fn dial_service_with_idle_timeout(
    mesh: &Arc<MeshState>,
    peer: &str,
    service: &str,
    idle_timeout_secs: Option<u64>,
) -> Result<SessionTransport> {
    // The EFFECTIVE keepalive: this node's configured value, else iroh's default. Read from the
    // live config rather than assumed, so the refusal below tracks what the node actually does.
    let keepalive = mesh.keep_alive_secs();
    let per_conn = per_session_transport_config(idle_timeout_secs, keepalive)?;
    // #41: an explicit `eid:<hex>` DEVICE principal dials that EXACT authenticated endpoint —
    // the one the socket backend injects into `_meta` and the allow lists use. No nickname
    // ambiguity (nicknames are not unique), no person→device race: it targets one device
    // precisely, which is the whole point of dialing the verified caller back. Resolved FIRST.
    if let Some(hex) = peer.strip_prefix("eid:") {
        // #85 ask 4: an explicit device dial is the most direct way to reach a revoked machine.
        if let Ok(bytes) = data_encoding::HEXLOWER.decode(hex.as_bytes())
            && let Ok(id) = <[u8; 32]>::try_from(bytes.as_slice())
        {
            refuse_if_revoked(mesh, id, peer).await?;
        }
        return dial_by_eid(mesh, hex, service, per_conn).await;
    }

    // Person→device: `peer` names a roster user with active devices → staggered race.
    if let Some(view) = mesh.roster.view() {
        let devices = view.devices_for_user(peer);
        if !devices.is_empty() {
            let candidates = order_dial_candidates(&devices, &mesh.presence_table, peer);
            // #186: a rostered device often ALSO has a paired row, and its hint is the only address
            // anyone has on a network with no discovery.
            warn_if_per_session_dropped(&per_conn, peer, "a roster person");
            return race_or_reuse(mesh, candidates, service)
                .await
                .with_context(|| format!("dial {peer}/{service}"));
        }
    }
    // Pairing-mode fallback. `peer` is resolved to stored entries by, in order:
    //  1. a NICKNAME match (the redeemer's local name for the peer), then
    //  2. a stable `user_id` match (#30: dial by the peer's self-sovereign `b64u:` identity, so a
    //     caller can address it by an id it aligns with its own — symmetric with the `user_id`
    //     already attested INBOUND on `_meta`). A user_id can match several devices of one person;
    //     those are raced exactly like the roster person→device path.
    // A single resolved entry is dialed WITH its pairing-persisted `last_addr` hint (issue #27: a
    // cold daemon must not depend on external discovery to reach a paired peer).
    let peer_owned = peer.to_string();
    let store = mesh.store.clone();
    let (single, multi): (Option<PeerEntry>, Vec<[u8; 32]>) =
        tokio::task::spawn_blocking(move || -> Result<_> {
            if let Some(e) = store.entry_for(&peer_owned)? {
                return Ok((Some(e), Vec::new()));
            }
            let mut by_user = store.entries_for_user(&peer_owned)?;
            match by_user.len() {
                0 => Ok((None, Vec::new())),
                1 => Ok((by_user.pop(), Vec::new())),
                _ => Ok((None, by_user.iter().map(|e| e.endpoint_id).collect())),
            }
        })
        .await
        .context("join peer resolve")??;

    // Several devices share the resolved user_id → race them (bare-id, discovery-resolved),
    // mirroring the roster person→device path.
    if !multi.is_empty() {
        // #186: these came from `entries_for_user`, so the hints were literally in hand and the
        // old code kept only the ids — making a two-device person unreachable offline while a
        // one-device person was fine.
        warn_if_per_session_dropped(&per_conn, peer, "a user_id with several devices");
        return race_or_reuse(mesh, multi, service)
            .await
            .with_context(|| format!("dial {peer}/{service}"));
    }
    let entry = single.with_context(|| format!("peer '{peer}' is not in the allowlist"))?;
    refuse_if_revoked(mesh, entry.endpoint_id, peer).await?;
    let endpoint_id = iroh::EndpointId::from_bytes(&entry.endpoint_id)
        .map_err(|e| anyhow::anyhow!("stored endpoint id for '{peer}' is invalid: {e}"))?;
    let addr = stored_dial_addr(entry.last_addr.as_deref(), endpoint_id);
    dial_single(mesh, addr, service, per_conn)
        .await
        .with_context(|| format!("dial {peer}/{service}"))
}

/// ONE session to ONE device, on the connection this node already holds to it when there is one
/// (#215).
///
/// A session with a per-connection transport config (#166) is dialled as before and never cached:
/// `ConnectOptions::with_transport_config` is per CONNECTION, so a session that asked for its own
/// idle timeout needs its own connection — sharing would either apply that timeout to sessions
/// that never asked for it or silently drop it, and #166 refuses the silent drop twice over.
///
/// `DIAL_TIMEOUT` bounds the whole thing, including a wait on another caller's in-flight dial, so a
/// dead peer costs a second session the same bounded wait it always did.
async fn dial_single(
    mesh: &Arc<MeshState>,
    addr: iroh::EndpointAddr,
    service: &str,
    per_conn: Option<iroh::endpoint::QuicTransportConfig>,
) -> Result<SessionTransport> {
    if per_conn.is_some() {
        let (transport, conn) =
            connect_with_timeout(&mesh.endpoint, addr, service, DIAL_TIMEOUT, per_conn).await?;
        return Ok(watch_session(mesh, transport, conn));
    }
    let peer = *addr.id.as_bytes();
    let endpoint = mesh.endpoint.clone();
    let opened =
        mesh.conn_cache
            .session_on(
                &[peer],
                DIAL_TIMEOUT,
                |id| refused_now(mesh.clone(), id),
                || async move {
                    connect_with_timeout(&endpoint, addr, service, DIAL_TIMEOUT, None).await
                },
            )
            .await?;
    Ok(opened_session(mesh, opened))
}

/// The racing paths' share of #215: a live connection to ANY candidate device is reused without a
/// race; a dial already in flight to any candidate is waited on rather than raced beside (its
/// loser would otherwise live as long as its session); and a race's winner is cached.
///
/// The cache is consulted AFTER `hinted_addrs`, whose [`dial_refused`] filter drops revoked devices
/// before any of them can be matched against a cached connection or given a `Dialing` slot. The
/// cache asks again at the moment it hands a connection out (`refused_now`), which alone would keep
/// a revoked device's connection from being reused — so the ordering is defence in depth, not an
/// independent guard: moving the cache ahead of `hinted_addrs` fails no test on its own, and fails
/// `a_raced_dial_never_reuses_a_connection_to_a_revoked_device` together with dropping that check.
/// #229's close pass additionally closes the connection on the revoke verbs; a revocation written
/// without one closes nothing, which is why neither in-cache guard relies on it.
///
/// The deadline keeps the race's own shape: every candidate gets its stagger slot plus a full
/// `DIAL_TIMEOUT`, so a person with several devices is not cut short by a bound sized for one.
async fn race_or_reuse(
    mesh: &Arc<MeshState>,
    candidates: Vec<[u8; 32]>,
    service: &str,
) -> Result<SessionTransport> {
    let addrs = hinted_addrs(mesh, candidates).await?;
    let peers: Vec<[u8; 32]> = addrs.iter().map(|a| *a.id.as_bytes()).collect();
    let deadline = DIAL_TIMEOUT + DIAL_STAGGER * peers.len() as u32;
    let endpoint = mesh.endpoint.clone();
    let opened = mesh
        .conn_cache
        .session_on(
            &peers,
            deadline,
            |id| refused_now(mesh.clone(), id),
            || async move { race_dial(&endpoint, addrs, service).await },
        )
        .await?;
    Ok(opened_session(mesh, opened))
}

/// [`dial_refused`] on the blocking pool, for the connection cache's hand-out check. A join error is
/// an `Err`, which the cache treats as a refusal (fail closed) and reports as a failed check — not
/// as a revocation, since nothing says the device was revoked.
async fn refused_now(mesh: Arc<MeshState>, id: [u8; 32]) -> Result<bool> {
    crate::util::blocking("join reuse revocation check", move || {
        dial_refused(&mesh, &id)
    })
    .await
}

/// A session from the cache: a reused stream as is, a fresh connection with its path watcher.
///
/// One path watcher per CONNECTION: `decide` already suppresses repeat observations, so a watcher
/// per session on a shared connection would only cost tasks.
fn opened_session(mesh: &Arc<MeshState>, opened: super::conn_cache::Opened) -> SessionTransport {
    match opened {
        super::conn_cache::Opened::Reused(transport) => transport,
        super::conn_cache::Opened::Fresh(transport, conn) => watch_session(mesh, transport, conn),
    }
}

/// Attach the #92 item 2 path watcher to an OUTBOUND session and hand back the transport.
///
/// The peer id comes from the CONNECTION (`remote_id`), not from the caller's bookkeeping: the
/// racing dial does not tell its caller which candidate won, and the connection is the only thing
/// that knows for certain who is on the other end.
///
/// This is the seam that makes #92 item 2 real. Watching only the accept path would cover sessions
/// others open to us and miss every session WE open — and the reported use case is an embedder
/// rendering a privacy indicator for a call it initiated.
fn watch_session(
    mesh: &Arc<MeshState>,
    transport: SessionTransport,
    conn: iroh::endpoint::Connection,
) -> SessionTransport {
    drop(super::path_watch::spawn(
        mesh.clone(),
        *conn.remote_id().as_bytes(),
        &conn,
    ));
    transport
}

/// Dial an EXACT endpoint named by its `eid:<hex>` device principal (#41). Decodes the 64-hex
/// endpoint id, attaches the pairing-persisted `last_addr` hint when a stored [`PeerEntry`] is
/// present at that id (cold-dial reachability, issue #27), else a bare-id discovery dial. The
/// peer's own gate remains the security boundary — dialing is outbound and authorizes nothing
/// on our side. An invalid hex / wrong length is a clear resolution error, never a panic.
async fn dial_by_eid(
    mesh: &Arc<MeshState>,
    hex: &str,
    service: &str,
    per_conn: Option<iroh::endpoint::QuicTransportConfig>,
) -> Result<SessionTransport> {
    let bytes = data_encoding::HEXLOWER
        .decode(hex.as_bytes())
        .map_err(|_| anyhow::anyhow!("invalid eid principal: not lowercase hex"))?;
    let endpoint_id_bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid eid principal: expected 32 bytes (64 hex chars)"))?;
    let endpoint_id = iroh::EndpointId::from_bytes(&endpoint_id_bytes)
        .map_err(|e| anyhow::anyhow!("invalid eid principal: {e}"))?;
    // Best-effort last_addr hint from a stored peer at this exact endpoint (blocking redb read
    // on the blocking pool). An unknown eid still dials bare-id via discovery.
    let store = mesh.store.clone();
    let last_addr = tokio::task::spawn_blocking(move || store.resolve(&endpoint_id_bytes))
        .await
        .context("join eid peer resolve")?
        .ok()
        .flatten()
        .and_then(|e| e.last_addr);
    let addr = stored_dial_addr(last_addr.as_deref(), endpoint_id);
    dial_single(mesh, addr, service, per_conn)
        .await
        .with_context(|| format!("dial eid:{hex}/{service}"))
}

/// Assemble the single-nickname dial [`iroh::EndpointAddr`]: the stored `endpoint_id` plus,
/// when it is usable, the pairing-persisted `last_addr` hint (iroh merges provided direct
/// addrs with whatever discovery resolves, so attaching the hint never narrows reachability).
///
/// Addresses are dial HINTS, never identity: the hint is attached only if it parses AND its
/// embedded id EQUALS the stored `endpoint_id` — a stored address claiming a DIFFERENT id is
/// ignored (identity stays pinned to the allowlist row; TLS still authenticates whoever
/// answers). An unparseable/absent hint degrades to the bare-id, discovery-only dial.
/// Every endpoint worth trying for `peer`, best first (#67) — the candidate list a custom-protocol
/// dial walks.
///
/// Resolution order mirrors what `dial_service` considers, so `connect_protocol` reaches the same
/// peers `open_session` does rather than a narrower set:
///
/// 1. An `eid:` principal names one device directly.
/// 2. ROSTER first for a bare name: `devices_for_user` already orders primary before mirror and is
///    the only path that reaches a rostered person with no pairing entry. Omitting it — the first
///    version of this function did — made `connect_protocol("alice")` fail on a roster-mode node
///    where `open_session("alice")` works.
/// 3. Then the pairing store: an exact nickname, else every device of that `user_id` (not just the
///    first, which stranded a person whose first-stored device happened to be offline).
///
/// Deduplicated, preserving order, and FILTERED through [`dial_refused`] (#223): a refused device is
/// never in `dialable`, only counted in `refused`, so no caller can dial one by forgetting to
/// filter. Both empty means "nobody by that name"; `dialable` empty with `refused > 0` means
/// "somebody, and every device of theirs is revoked" — the callers answer those differently.
pub(crate) async fn protocol_candidates(
    mesh: &Arc<MeshState>,
    peer: &str,
) -> anyhow::Result<Candidates> {
    let resolved = resolve_candidates(mesh, peer).await?;
    let mesh = mesh.clone();
    crate::util::blocking("join dial revocation filter", move || {
        let before = resolved.len();
        let dialable: Vec<[u8; 32]> = resolved
            .into_iter()
            .filter(|id| !dial_refused(&mesh, id))
            .collect();
        Candidates {
            refused: before - dialable.len(),
            dialable,
        }
    })
    .await
}

/// What [`protocol_candidates`] resolved: the endpoints that may be dialled, and how many were
/// dropped as revoked. The refused ids themselves are deliberately not carried.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Candidates {
    pub(crate) dialable: Vec<[u8; 32]>,
    pub(crate) refused: usize,
}

/// [`protocol_candidates`] for a DIAL of one peer — `Node::connect_protocol` (#223).
///
/// Nobody by that name is "no peer"; somebody whose every device is revoked is the SAME refusal
/// `open_session` returns ([`revoked_refusal`]), and nothing is dialled.
pub(crate) async fn connect_candidates(
    mesh: &Arc<MeshState>,
    peer: &str,
) -> anyhow::Result<Vec<[u8; 32]>> {
    let c = protocol_candidates(mesh, peer).await?;
    if c.dialable.is_empty() {
        if c.refused > 0 {
            return Err(revoked_refusal(peer));
        }
        anyhow::bail!("no peer '{peer}' — 'status' lists your peers and roster members");
    }
    Ok(c.dialable)
}

/// The UNFILTERED resolution behind [`protocol_candidates`]. Private, so nothing dials from it.
async fn resolve_candidates(mesh: &Arc<MeshState>, peer: &str) -> anyhow::Result<Vec<[u8; 32]>> {
    if let Some(hex) = peer.strip_prefix("eid:") {
        let bytes = data_encoding::HEXLOWER
            .decode(hex.as_bytes())
            .map_err(|_| anyhow::anyhow!("invalid eid principal: not lowercase hex"))?;
        let id: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid eid principal: expected 32 bytes"))?;
        return Ok(vec![id]);
    }
    let mut out: Vec<[u8; 32]> = Vec::new();
    if let Some(view) = mesh.roster.view() {
        out.extend(view.devices_for_user(peer).into_iter().map(|(eid, _)| eid));
    }
    let store = mesh.store.clone();
    let name = peer.to_string();
    let from_store = crate::util::blocking("join connect_protocol resolve", move || {
        let mut v: Vec<[u8; 32]> = Vec::new();
        if let Some(e) = store.entry_for(&name)? {
            v.push(e.endpoint_id);
        }
        v.extend(
            store
                .entries_for_user(&name)?
                .into_iter()
                .map(|e| e.endpoint_id),
        );
        Ok::<_, anyhow::Error>(v)
    })
    .await??;
    out.extend(from_store);
    let mut seen = std::collections::HashSet::new();
    out.retain(|e| seen.insert(*e));
    Ok(out)
}

/// Resolve alternate blob SOURCES to dialable addresses (#83).
///
/// Each entry is a stable principal or a paired nickname — the same vocabulary `open_session`
/// takes — expanded through [`protocol_candidates`], so naming a PERSON offers every device of
/// theirs rather than one. Each address carries the stored dial hint, exactly as a service dial
/// does, so an alternate this node has not contacted since boot is still reachable on a hermetic
/// LAN.
///
/// A name that resolves to NOBODY is an error rather than a silent skip: a caller that typed a
/// nickname wrong would otherwise watch the fetch fail on the offline publisher and never learn
/// that its fallback list was empty all along.
///
/// Order is preserved and duplicates are dropped — including a device that two named people share,
/// which would otherwise be dialled twice for one timeout each.
///
/// REVOKED devices are dropped (#223), and a name whose every device is revoked contributes
/// nothing rather than failing the fetch: the publisher and the other sources may still serve it,
/// and the name is not a typo. Such names are COUNTED in [`BlobSources::skipped_revoked`], which
/// `blob_fetch` puts in its error if the fetch then fails — the caller named them, so the caller
/// is told. Nothing is logged with the selector, which can be an `eid:`. If no source is left at
/// all — the ticket's publisher included, which `blob_fetch` filters separately — the fetch fails
/// with the provider's "no source to try".
pub(crate) async fn blob_source_addrs(
    mesh: &Arc<MeshState>,
    from: &[String],
) -> anyhow::Result<BlobSources> {
    // Bounded before any work: each name costs a store scan and each candidate a dial timeout, so
    // an unbounded list is an unbounded wait on a request holding one of the connection's in-flight
    // slots. Refused rather than truncated — silently dropping the tail would make a fetch fail
    // while the source that had the blob sat unused.
    anyhow::ensure!(
        from.len() <= mcpmesh_local_api::MAX_BLOB_SOURCES,
        "too many blob sources: {} (the cap is {})",
        from.len(),
        mcpmesh_local_api::MAX_BLOB_SOURCES
    );
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut skipped_revoked = 0usize;
    for name in from {
        let Candidates { dialable, refused } = protocol_candidates(mesh, name).await?;
        anyhow::ensure!(
            !dialable.is_empty() || refused > 0,
            "no blob source '{name}' — it must be a paired peer, a roster member, or an \
             `eid:`/`b64u:` principal"
        );
        if dialable.is_empty() {
            skipped_revoked += 1;
        }
        for eid in dialable {
            if !seen.insert(eid) {
                continue;
            }
            let Ok(id) = iroh::EndpointId::from_bytes(&eid) else {
                continue;
            };
            let store = mesh.store.clone();
            let entry =
                crate::util::blocking("join blob source resolve", move || store.resolve(&eid))
                    .await??;
            out.push(stored_dial_addr(
                entry.and_then(|e| e.last_addr).as_deref(),
                id,
            ));
        }
    }
    if skipped_revoked > 0 {
        // A count, never the selector: a caller-named source can be an `eid:`.
        tracing::warn!(
            skipped_revoked,
            "blob sources skipped: REVOKED on this node, not dialled"
        );
    }
    Ok(BlobSources {
        addrs: out,
        skipped_revoked,
    })
}

/// What [`blob_source_addrs`] resolved (#223).
#[derive(Debug)]
pub(crate) struct BlobSources {
    /// Dialable addresses, in the caller's order, deduplicated.
    pub(crate) addrs: Vec<iroh::EndpointAddr>,
    /// How many caller-NAMED sources contributed nothing because every device was revoked.
    pub(crate) skipped_revoked: usize,
}

pub(crate) fn stored_dial_addr(
    last_addr: Option<&str>,
    endpoint_id: iroh::EndpointId,
) -> iroh::EndpointAddr {
    if let Some(json) = last_addr
        && let Ok(addr) = serde_json::from_str::<iroh::EndpointAddr>(json)
        && addr.id == endpoint_id
    {
        // Every address filtered out degrades to the bare-id dial, exactly as an absent or
        // id-mismatched hint does — never a dial to nowhere.
        return dialable_only(addr);
    }
    iroh::EndpointAddr::from(endpoint_id)
}

/// Strip a remote-supplied `EndpointAddr` of everything that cannot be a QUIC peer (#203).
///
/// The counterpart to [`stored_dial_addr`] for addresses that are dialled **before** they are ever
/// stored — `redeem_invite` and `attest_to` deserialize a remote blob straight into
/// `Endpoint::connect`. The first version of this change filtered only the stored path and claimed
/// it "covers the invite path"; it does not, and the motivating scenario — a crafted invite aiming
/// this node's padded Initials at a destination of the inviter's choosing — happened in full at
/// redemption, before storage. The gate found it.
///
/// Returns the id alone when nothing survives, which for pairing means the dial falls back to
/// discovery rather than proceeding with an attacker's address list.
pub(crate) fn dialable_only(addr: iroh::EndpointAddr) -> iroh::EndpointAddr {
    let id = addr.id;
    let kept: Vec<iroh::TransportAddr> = addr.addrs.into_iter().filter(is_dialable_addr).collect();
    if kept.is_empty() {
        iroh::EndpointAddr::from(id)
    } else {
        iroh::EndpointAddr::from_parts(id, kept)
    }
}

/// Can this transport address be a QUIC PEER at all? (#203)
///
/// A dial hint is a set of destinations this node sends packets to, and iroh sends each outgoing
/// datagram to EVERY known path until one is selected. Since 0.52.2 every stored hint comes from
/// `dial_hint::observed_for` — addresses this node actually reached — so the remaining remote-supplied
/// claims are the ONE-SHOT dials at `redeem_invite` and `attest_to`, which happen before any hint
/// exists and are what this filter guards. So a crafted invite could aim a
/// node's QUIC Initials — which RFC 9000 requires be padded to ≥1200 bytes — wherever it liked.
///
/// This rejects only what can NEVER be a unicast peer, which is why it is safe to apply at the
/// single choke point every dial passes through rather than per hint source:
///
/// - **Unspecified** (`0.0.0.0`, `::`) — not a destination. On Linux `0.0.0.0:<port>` reaches
///   localhost, so this is also the cheapest half of the internal-scan concern.
/// - **Multicast** — a unicast QUIC handshake to a group address is meaningless, and sending there
///   is a way to make one host emit traffic to many.
/// - **Broadcast** (`255.255.255.255`) — same.
///
/// **Deliberately NOT rejected**, because each is legitimate and filtering it would break real
/// deployments rather than attackers:
///
/// - **Loopback.** Two nodes on one host is supported, and most of this repo's own suite pairs over
///   `127.0.0.1`.
/// - **IPv6 link-local (`fe80::`).** Ordinary LAN addressing that iroh may select.
/// - **IPv4 link-local (`169.254.0.0/16`).** APIPA is real when DHCP fails — and it is also the
///   cloud-metadata range, so it is the sharpest remaining edge. Left to #203, which is where the
///   trade belongs.
///
/// **What this does not cover, stated so it is not mistaken for coverage:**
///
/// - **Arbitrary ROUTABLE addresses.** A hostile invite can still name any real host. Bounding that
///   needs provenance on the hint — observed-on-an-open-path vs merely-reported — not a destination
///   blocklist. That is #203's actual subject.
/// - **RELAY URLs pass through unfiltered, and they are a sharper edge than the UDP case above.**
///   The first version of this comment said relay transports "are not IP destinations we choose",
///   which is false: a relay URL in a hint or an invite is chosen by the REMOTE party, and iroh
///   connects on demand to any URL it is handed. So a hostile invite naming `wss://victim:8443/`
///   makes this node open an outbound TLS/WebSocket connection to an attacker-chosen host — a
///   stronger primitive than the padded Initials this filter removes. It is not filtered here
///   because the same provenance model is what makes it decidable, and guessing a URL policy is how
///   the last two attempts at this area went wrong.
/// - **Subnet-directed broadcast** (`192.168.1.255`), which needs the local netmask to recognise
///   and is a better amplifier than `255.255.255.255` — routers drop the latter.
pub(crate) fn is_dialable_addr(a: &iroh::TransportAddr) -> bool {
    let iroh::TransportAddr::Ip(s) = a else {
        // Relay and custom transports are NOT filtered here, and that is a gap rather than a
        // judgement — see the "what this does not cover" note above.
        return true;
    };
    // A dial to port 0 is meaningless by this function's own definition of a peer.
    if s.port() == 0 {
        return false;
    }
    // CANONICALIZE FIRST. `Ipv6Addr::is_multicast` matches only `ff00::/8` and `is_unspecified`
    // only `::`, so neither sees through the IPv4-MAPPED form — `[::ffff:224.0.0.1]:1900` passed
    // every check below while iroh's own ingest (`transports::Addr::from`) and its sender
    // (`IpSender::canonical_addr`) both call `to_canonical()` and turn it back into real
    // multicast, strictly AFTER this ran. Writing the mapped form was a complete bypass of this
    // filter; the gate proved it in-tree on all three classes.
    match s.ip().to_canonical() {
        std::net::IpAddr::V4(v4) => {
            !v4.is_unspecified()
                && !v4.is_multicast()
                && !v4.is_broadcast()
                // `240.0.0.0/4`, reserved and not routable to any peer. Written as a comparison
                // on the octet, NOT `!octet >= 240` — that parses as a bitwise NOT and rejects
                // everything from `16.0.0.0/8` upward while accepting `240.0.0.0/4`, i.e. exactly
                // inverted. It compiles and it type-checks.
                && v4.octets()[0] < 240
        }
        std::net::IpAddr::V6(v6) => !v6.is_unspecified() && !v6.is_multicast(),
    }
}

/// The person→device dial STAGGER: a live candidate is not blocked waiting on a
/// dead/stalling one — the next candidate joins the race this long after the previous.
const DIAL_STAGGER: Duration = Duration::from_millis(500);

/// The explicit application-level dial timeout. Defense-in-depth over iroh's
/// transport idle timeouts — SYMMETRIC across both dial paths (the person→device race AND the
/// single-nickname fallback) so a dead/stalling peer fails a dial in a bounded, asserted window.
pub(crate) const DIAL_TIMEOUT: Duration = Duration::from_secs(20);

/// `connect` with an explicit timeout. On elapse → a typed Err (the caller surfaces
/// `-32055 unreachable` upstream). Used by BOTH `dial_one` and the single-nickname `dial_service`.
pub(crate) async fn connect_with_timeout(
    endpoint: &iroh::Endpoint,
    addr: iroh::EndpointAddr,
    service: &str,
    timeout: Duration,
    // #166: `None` inherits the endpoint's node-wide transport config. A `Some` REPLACES it, so it
    // must be complete — see `per_session_transport_config`.
    transport: Option<iroh::endpoint::QuicTransportConfig>,
) -> Result<(SessionTransport, iroh::endpoint::Connection)> {
    match tokio::time::timeout(
        timeout,
        mcpmesh_net::connect_with_transport_config(endpoint, addr, service, transport),
    )
    .await
    {
        // A typed ConnectError (dial vs open-stream) converts into the anyhow chain.
        Ok(r) => r.map_err(Into::into),
        Err(_) => anyhow::bail!("dial timed out after {timeout:?}"),
    }
}

/// Order a person's active devices into the dial-candidate sequence. `devices` is the
/// roster order from [`RosterView::devices_for_user`] (primary→mirror, deterministic within role);
/// this RE-ORDERS candidates WITHIN each role by presence recency (most-recent first). Presence is
/// ADVISORY: a device with NO presence entry keeps its roster position AFTER the present ones in its
/// role group — it is never dropped (absence never removes a candidate). The role grouping
/// (primary→mirror) is preserved regardless of presence, so a freshly-seen mirror never jumps ahead
/// of a primary.
///
/// [`RosterView::devices_for_user`]: mcpmesh_trust::roster::validate::RosterView::devices_for_user
fn order_dial_candidates(
    devices: &[([u8; 32], String)],
    presence: &crate::roster::presence::PresenceTable,
    user_id: &str,
) -> Vec<[u8; 32]> {
    // Recency rank: a device's position in the presence table's most-recent-first list. Devices with
    // NO entry get a rank AFTER every present one (`usize::MAX`), so they stay candidates but sort last
    // WITHIN their role — presence never removes a candidate, only reorders.
    let by_recency = presence.endpoints_for_user_by_recency(user_id);
    let recency_rank = |eid: &[u8; 32]| -> usize {
        by_recency
            .iter()
            .position(|e| e == eid)
            .unwrap_or(usize::MAX)
    };
    let mut ordered: Vec<([u8; 32], String)> = devices.to_vec();
    // Stable sort on (role rank, recency rank): role grouping wins (primary→mirror), recency orders
    // within a role, and equal keys (same role, both absent from presence) keep the deterministic
    // roster order `devices_for_user` already imposed.
    ordered.sort_by_key(|(eid, role)| (dial_role_rank(role), recency_rank(eid)));
    ordered.into_iter().map(|(eid, _)| eid).collect()
}

/// Dial-candidate role rank mirroring `trust`'s `role_rank` (primary→mirror→other). Duplicated across
/// the crate seam deliberately: `devices_for_user` already emits roster order, but the presence
/// re-order here must re-assert the primary→mirror grouping so recency cannot lift a mirror above a
/// primary. Kept tiny; `pub(crate)` only for the `presence_peers` display sort in `daemon`.
pub(crate) fn dial_role_rank(role: &str) -> u8 {
    match role {
        "primary" => 0,
        "mirror" => 1,
        _ => 2,
    }
}

/// Staggered-race dial. Dials `candidates` in order, launching the next one
/// `DIAL_STAGGER` (500 ms) after the previous if no session has won yet — OR immediately if the
/// in-flight dials have all already failed (a fast-failing candidate doesn't impose the full 500 ms
/// wait). The FIRST [`connect`] success WINS: its transport is returned and the in-flight losing
/// dials are CANCELLED — dropping the [`JoinSet`] aborts its remaining tasks (their `connect` futures
/// are dropped at the next await point — no lingering tasks or half-open connections). If EVERY
/// candidate fails, the last error is returned (the race never hangs). An empty candidate list is an
/// immediate Err.
///
/// The stagger is why a live candidate is not blocked on a dead/stalling one: a stalled primary keeps
/// its dial in flight while the mirror is launched at 500 ms and can win. Correctness rests on
/// `connect` being cancellation-safe on abort (iroh's `Endpoint::connect` future holds no external
/// state that must be torn down explicitly — aborting it abandons the in-progress handshake).
///
/// **DECLARED — `JoinSet`, not `FuturesUnordered`.** The plan sketched `FuturesUnordered`; this uses
/// tokio's native [`JoinSet`] instead — same concurrent-unordered-race semantics (first-wins, drop
/// cancels the losers) but with NO new crate dependency. Pulling `futures-util` in as a direct dep
/// measurably enlarged the daemon binary and added ~0.5 s to cold startup under the parallel-spawn
/// integration tests (a pre-existing 3 s-bound test flipped to failing). `JoinSet` is already in the
/// tree via tokio's `rt`, keeps startup unchanged, and spawns each racer as a real task (so a stalled
/// dial makes progress on a runtime worker rather than only when this future is polled).
///
/// [`JoinSet`]: tokio::task::JoinSet
pub async fn race_dial(
    endpoint: &iroh::Endpoint,
    candidates: Vec<iroh::EndpointAddr>,
    service: &str,
) -> Result<(SessionTransport, iroh::endpoint::Connection)> {
    anyhow::ensure!(!candidates.is_empty(), "no dial candidates to race");

    // Each racer is a 'static task, so it owns a cloned endpoint + service (iroh::Endpoint is a cheap
    // Arc-backed clone). Dropping the set on return ABORTS every still-running racer — the loser cancel.
    let mut set: tokio::task::JoinSet<Result<(SessionTransport, iroh::endpoint::Connection)>> =
        tokio::task::JoinSet::new();
    let spawn_dial =
        |set: &mut tokio::task::JoinSet<Result<(SessionTransport, iroh::endpoint::Connection)>>,
         addr: iroh::EndpointAddr| {
            let ep = endpoint.clone();
            let svc = service.to_string();
            set.spawn(async move { dial_one(&ep, addr, &svc).await });
        };

    let mut next = 0usize; // index of the next candidate to launch
    spawn_dial(&mut set, candidates[next].clone()); // candidate 0 immediately
    next += 1;
    let mut last_err: Option<anyhow::Error> = None;

    loop {
        if next < candidates.len() {
            // A candidate is still waiting: race the in-flight dials against the 500 ms stagger.
            // `biased` polls the join first so a ready success/failure is handled before the timer,
            // and an EMPTY set (all in-flight already failed) yields `None` immediately → launch next.
            tokio::select! {
                biased;
                joined = set.join_next() => match joined {
                    Some(Ok(Ok(t))) => return Ok(t), // first success wins; drop `set` → abort the rest
                    Some(Ok(Err(e))) => last_err = Some(e), // this candidate's dial failed; keep racing
                    Some(Err(e)) => last_err = Some(anyhow::anyhow!("dial task join error: {e}")),
                    None => {
                        // Every in-flight dial failed before the stagger: launch the next NOW.
                        spawn_dial(&mut set, candidates[next].clone());
                        next += 1;
                    }
                },
                () = tokio::time::sleep(DIAL_STAGGER) => {
                    // No winner within the stagger window → add the next candidate to the race.
                    spawn_dial(&mut set, candidates[next].clone());
                    next += 1;
                }
            }
        } else {
            // No more candidates to launch: await whatever dials remain in flight.
            match set.join_next().await {
                Some(Ok(Ok(t))) => return Ok(t),
                Some(Ok(Err(e))) => last_err = Some(e),
                Some(Err(e)) => last_err = Some(anyhow::anyhow!("dial task join error: {e}")),
                None => {
                    return Err(
                        last_err.unwrap_or_else(|| anyhow::anyhow!("all dial candidates failed"))
                    );
                }
            }
        }
    }
}

/// Attach each candidate's stored dial hint, so a RACED dial is no more discovery-dependent than a
/// single-device one (#186).
///
/// `stored_dial_addr` validates: a hint recorded for a different id is discarded rather than
/// dialled, and a peer with no stored row degrades to a bare id — which is what every raced dial
/// used to be. So this can only ever add reachability.
///
/// One store read per candidate, on the blocking pool. A race has at most a handful of candidates
/// (one person's devices), and the read happens once per dial rather than per attempt.
async fn hinted_addrs(
    mesh: &Arc<MeshState>,
    candidates: Vec<[u8; 32]>,
) -> Result<Vec<iroh::EndpointAddr>> {
    // #85 ask 4: drop revoked devices from the race. Filtering here covers BOTH racing paths (the
    // roster person→device race and the multi-device `user_id` race) at one seam — a person with
    // three devices, one of them stolen, must still be reachable on the other two. Through
    // `dial_refused` (#223), so a roster device under a revoked `b64u:` identity is dropped too.
    let mesh = mesh.clone();
    crate::util::blocking("join dial-candidate hints", move || {
        let candidates: Vec<[u8; 32]> = candidates
            .into_iter()
            .filter(|id| !dial_refused(&mesh, id))
            .collect();
        anyhow::ensure!(
            !candidates.is_empty(),
            "every device of that peer is REVOKED on this node"
        );
        let store = &mesh.store;
        let mut out = Vec::with_capacity(candidates.len());
        for eid in candidates {
            let Ok(id) = iroh::EndpointId::from_bytes(&eid) else {
                continue; // a corrupt id is skipped, not fatal — another device may work
            };
            let last = store.resolve(&eid).ok().flatten().and_then(|e| e.last_addr);
            out.push(stored_dial_addr(last.as_deref(), id));
        }
        Ok(out)
    })
    .await?
}

/// Dial ONE candidate over the mesh.
///
/// The endpoint_id inside `addr` IS the device's ed25519 pubkey, so `connect` reaches the holder of
/// that key or fails — no MITM among racers, whatever address it carries.
///
/// Takes a PREPARED address rather than a bare id (#186). It used to build
/// `EndpointAddr::from(endpoint_id)` itself, which made every raced dial discovery-only — so a
/// person's SECOND device silently made their first unreachable on a network with no discovery,
/// dropping the invariant #27 established for the single-device path three lines away. The caller
/// now attaches the stored hint, exactly as the single-device path does.
async fn dial_one(
    endpoint: &iroh::Endpoint,
    addr: iroh::EndpointAddr,
    service: &str,
) -> Result<(SessionTransport, iroh::endpoint::Connection)> {
    // The racing path: no per-connection config — see `dial_service_with_idle_timeout`.
    connect_with_timeout(endpoint, addr, service, DIAL_TIMEOUT, None).await
}

/// Pipe an established mesh session to/from the control connection. The FIRST
/// control frame — the AI client's `initialize` — is augmented with the reserved
/// `_meta["mcpmesh/service"]` naming the service (the SINGLE enumerated exception to
/// verbatim pass-through) before it is forwarded to the peer, so the far side's
/// `select_service` can route it. Then frames flow both directions verbatim until either
/// side ends. The two directions run as independent concurrent loops (one codec) — the same
/// anti-deadlock discipline as `backends::pump`; this is a sibling
/// of that pump, not a reuse, because the mesh side here is an owned [`SessionTransport`]
/// (not raw streams) and the service-name injection has no analogue there.
pub async fn pipe_session<CR, CW>(
    mut transport: SessionTransport,
    service: &str,
    mut control_reader: FrameReader<CR>,
    mut control_writer: CW,
) -> Result<()>
where
    CR: AsyncRead + Unpin + Send,
    CW: AsyncWrite + Unpin + Send,
{
    // 1. First control frame = the AI client's initialize. A clean EOF or a framing violation
    //    before it means there is no session to carry — end cleanly.
    let init = match control_reader.next().await? {
        Some(Inbound::Frame(v)) => inject_service(v, service),
        Some(Inbound::Violation(_)) | None => return Ok(()),
    };
    transport
        .send_value(init)
        .await
        .context("forward initialize to peer")?;

    // The outbound direction sends through a cloned writer handle (Arc) so it does not need
    // `&mut transport`, which the inbound direction holds for `recv_value` — the disjoint
    // split that lets the two loops run concurrently without a shared mutable borrow.
    let transport_writer = transport.writer();

    // Direction A: control (AI client via the proxy) -> mesh peer.
    let to_mesh = async {
        loop {
            match control_reader.next().await {
                Ok(Some(Inbound::Frame(frame))) => {
                    if transport_writer.send_value(frame).await.is_err() {
                        break; // peer is gone
                    }
                }
                Ok(Some(Inbound::Violation(_))) => break,
                Ok(None) | Err(_) => break, // proxy closed / IO error
            }
        }
        // The proxy half-closed (its AI client sent everything it will send) — that ends the
        // REQUEST direction, never the session. Half-close toward the peer so its backend sees
        // a clean end-of-input, then park: only the peer closing (`to_control` ending) may tear
        // the session down, mirroring the proxy pump's drain discipline. Winning the select!
        // here would cancel `to_control` and drop responses still in flight — the one-shot
        // pipe case (`printf ... | mcpmesh connect ...`) hits exactly that race.
        let _ = transport_writer.shutdown().await;
        std::future::pending::<()>().await
    };
    // Direction B: mesh peer -> control. Carries the peer's responses AND any synthesized
    // -32054 refusal, verbatim. The `while let` exits on peer EOF / a severed
    // session / a framing violation (all `recv_value` non-`Ok(Some)` outcomes).
    let to_control = async {
        while let Ok(Some(frame)) = transport.recv_value().await {
            if write_frame(&mut control_writer, &frame).await.is_err() {
                break; // proxy is gone
            }
        }
    };
    tokio::select! {
        () = to_mesh => {}
        () = to_control => {}
    }
    // Orderly teardown on BOTH halves (backends::pump discipline — "a bare drop abandons
    // data"): flush any final buffered frame before each write half closes. Benign in
    // practice (write_frame flushes each frame), but symmetric and future-proof.
    let _ = transport.shutdown().await;
    let _ = control_writer.shutdown().await;
    Ok(())
}

/// Name the target service in `params._meta` on the first frame, creating `params` and `_meta` as
/// objects if absent and REPLACING a non-object `_meta` (never merging — the rule for the
/// reserved-key injector). This is the one edit the otherwise verbatim proxy path makes to a frame.
/// A non-object frame is forwarded untouched — the platform does not interpret MCP semantics; the
/// far side rejects it.
///
/// **BOTH spellings are written** (#49), carrying the identical value: the reverse-DNS
/// `tech.counterpunch.mcpmesh/service` and the legacy `mcpmesh/service`. The remote daemon may be
/// older than this one and read only the legacy key, so emitting just the new one would refuse
/// every dial to a peer that has not upgraded. The receiving side prefers reverse-DNS and falls
/// back, so no version gate is needed on either peer.
///
/// The legacy spelling is DEPRECATED as of 0.51.0 and removed at 1.0.
pub(crate) fn inject_service(mut frame: Value, service: &str) -> Value {
    let Some(obj) = frame.as_object_mut() else {
        return frame;
    };
    let params = obj
        .entry("params")
        .or_insert_with(|| Value::Object(Default::default()));
    if !params.is_object() {
        *params = Value::Object(Default::default());
    }
    let params = params.as_object_mut().expect("params set to object above");
    let meta = params
        .entry("_meta")
        .or_insert_with(|| Value::Object(Default::default()));
    if !meta.is_object() {
        *meta = Value::Object(Default::default()); // REPLACE a non-object _meta (§6.3)
    }
    let meta = meta.as_object_mut().expect("meta set to object above");
    for key in mcpmesh_net::service::SERVICE_KEYS {
        meta.insert(key.into(), Value::String(service.to_string()));
    }
    frame
}

#[cfg(test)]
mod source_tests {
    use super::blob_source_addrs;
    use crate::daemon::testutil::hermetic_mesh;

    async fn mesh_with_peers(
        dir: &std::path::Path,
        peers: &[(&str, [u8; 32], Option<&str>)],
    ) -> std::sync::Arc<crate::daemon::MeshState> {
        let cfg = dir.join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = hermetic_mesh(cfg).await;
        for (nick, eid, user) in peers {
            mesh.store
                .add(crate::allowlist::PeerEntry {
                    endpoint_id: *eid,
                    nickname: (*nick).into(),
                    services: vec![],
                    paired_at: None,
                    user_id: user.map(|u| u.to_string()),
                    last_addr: None,
                })
                .unwrap();
        }
        mesh
    }

    fn eid_of(seed: u8) -> [u8; 32] {
        *iroh::SecretKey::from_bytes(&[seed; 32]).public().as_bytes()
    }

    /// #83: every property `blob_source_addrs`' doc claims, pinned.
    ///
    /// It had NO coverage in the first cut — nothing called it outside the handler, and the
    /// acceptance test passed literal addresses, bypassing resolution entirely. Review found that
    /// `Ok(vec![])`, a deleted typo guard and a deleted dedup all survived.
    #[tokio::test(flavor = "multi_thread")]
    async fn blob_sources_resolve_dedupe_and_refuse_a_typo() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (eid_of(41), eid_of(42));
        let mesh = mesh_with_peers(
            dir.path(),
            &[("alice", a, Some("b64u:alice-key")), ("bob", b, None)],
        )
        .await;

        // A nickname resolves.
        let out = blob_source_addrs(&mesh, &["alice".into()])
            .await
            .unwrap()
            .addrs;
        assert_eq!(out.len(), 1, "a paired nickname must resolve to its device");
        assert_eq!(*out[0].id.as_bytes(), a);

        // A `b64u:` user_id resolves to that person's device — the doc and the `--from` help both
        // promise this, and it goes through a different store lookup than the nickname.
        let out = blob_source_addrs(&mesh, &["b64u:alice-key".into()])
            .await
            .unwrap()
            .addrs;
        assert_eq!(*out[0].id.as_bytes(), a, "a b64u: user_id must resolve");

        // An `eid:` principal resolves without any store entry at all.
        let stranger = eid_of(43);
        let out = blob_source_addrs(
            &mesh,
            &[format!("eid:{}", data_encoding::HEXLOWER.encode(&stranger))],
        )
        .await
        .unwrap()
        .addrs;
        assert_eq!(*out[0].id.as_bytes(), stranger);

        // ORDER is preserved — sources are tried in it, so a reordering changes which one answers.
        let out = blob_source_addrs(&mesh, &["bob".into(), "alice".into()])
            .await
            .unwrap()
            .addrs;
        assert_eq!(
            out.iter().map(|x| *x.id.as_bytes()).collect::<Vec<_>>(),
            vec![b, a],
            "the caller's order must survive resolution"
        );

        // DEDUPED across names: the same device reached two ways is dialled once, not twice for
        // one timeout each.
        let out = blob_source_addrs(&mesh, &["alice".into(), "b64u:alice-key".into()])
            .await
            .unwrap()
            .addrs;
        assert_eq!(out.len(), 1, "one device named twice must be dialled once");

        // A name that resolves to NOBODY is an error, not an empty contribution. Otherwise the
        // caller watches the fetch fail on the offline publisher and never learns its fallback
        // list was empty all along.
        let err = blob_source_addrs(&mesh, &["nobody".into()])
            .await
            .expect_err("an unresolvable source must be refused");
        assert!(
            format!("{err:#}").contains("no blob source 'nobody'"),
            "{err:#}"
        );

        // …and the fan-out is CAPPED rather than truncated: dropping the tail would make a fetch
        // fail while the source that had the blob sat unused.
        let many: Vec<String> = (0..mcpmesh_local_api::MAX_BLOB_SOURCES + 1)
            .map(|_| "alice".to_string())
            .collect();
        let err = blob_source_addrs(&mesh, &many)
            .await
            .expect_err("over the cap must be refused");
        assert!(
            format!("{err:#}").contains("too many blob sources"),
            "{err:#}"
        );
    }

    fn revoke(mesh: &crate::daemon::MeshState, eid: [u8; 32]) {
        mesh.store
            .revoke(crate::allowlist::RevokedEntry {
                endpoint_id: eid,
                revoked_at: 1,
                reason: None,
                source: "local".into(),
                signer_user_id: None,
                issued_at: None,
            })
            .unwrap();
    }

    fn revoke_identity(mesh: &crate::daemon::MeshState, user_id: &str) {
        mesh.store
            .revoke_user(
                user_id,
                &crate::allowlist::RevokedEntry {
                    endpoint_id: [0u8; 32],
                    revoked_at: 1,
                    reason: None,
                    source: "local".into(),
                    signer_user_id: None,
                    issued_at: None,
                },
            )
            .unwrap();
    }

    fn eid_principal(eid: [u8; 32]) -> String {
        format!("eid:{}", data_encoding::HEXLOWER.encode(&eid))
    }

    /// #223: `connect_protocol`'s candidates are filtered through the outbound revocation predicate.
    ///
    /// `protocol_candidates` applied no filter at all, so `Node::connect_protocol` dialled a device
    /// this node had revoked on any app ALPN, by every selector. Both sides of the boundary are
    /// seeded in ONE store: bob's stolen laptop (revoked) next to his phone and carol (live), so a
    /// filter that refuses everything fails the live assertions and one that refuses nothing fails
    /// the revoked ones.
    ///
    /// Deleting the `dial_refused` filter in `protocol_candidates` fails the first `expect_err`;
    /// making `connect_candidates` bail "no peer" for a refused-only set fails the `REVOKED`
    /// substring assertion.
    #[tokio::test(flavor = "multi_thread")]
    async fn connect_protocol_candidates_never_include_a_revoked_device() {
        let dir = tempfile::tempdir().unwrap();
        let (laptop, phone, carol) = (eid_of(51), eid_of(52), eid_of(53));
        let mesh = mesh_with_peers(
            dir.path(),
            &[
                ("bob-laptop", laptop, Some("b64u:bob")),
                ("bob-phone", phone, Some("b64u:bob")),
                ("carol", carol, None),
            ],
        )
        .await;
        // Precondition: every selector resolves BEFORE the revoke.
        assert_eq!(
            super::connect_candidates(&mesh, "bob-laptop")
                .await
                .unwrap(),
            vec![laptop]
        );
        revoke(&mesh, laptop);

        for sel in ["bob-laptop".to_string(), eid_principal(laptop)] {
            let e = super::connect_candidates(&mesh, &sel)
                .await
                .expect_err("a revoked device must never be a connect_protocol candidate");
            // The SAME refusal `open_session` gives (`refuse_if_revoked`), not "no peer".
            assert!(
                format!("{e:#}").contains(&format!("{sel} is REVOKED on this node")),
                "{sel}: {e:#}"
            );
        }
        // A person with one stolen device is still reachable on the other.
        assert_eq!(
            super::connect_candidates(&mesh, "b64u:bob").await.unwrap(),
            vec![phone],
            "the revoked laptop is dropped, the phone stays"
        );
        // The control: an unrevoked peer is untouched.
        assert_eq!(
            super::connect_candidates(&mesh, "carol").await.unwrap(),
            vec![carol]
        );
        // Nobody by that name is still "no peer", NOT a revocation claim.
        let e = super::connect_candidates(&mesh, "nobody")
            .await
            .expect_err("nobody resolves");
        let msg = format!("{e:#}");
        assert!(
            msg.contains("no peer 'nobody'") && !msg.contains("REVOKED"),
            "{msg}"
        );

        // Every device of the person revoked → the revocation refusal for the person.
        revoke(&mesh, phone);
        let e = super::connect_candidates(&mesh, "b64u:bob")
            .await
            .expect_err("every device revoked");
        assert!(
            format!("{e:#}").contains("b64u:bob is REVOKED on this node"),
            "{e:#}"
        );
    }

    /// #223: a revoked device is dropped from a blob fetch's source set, and a name whose every
    /// device is revoked contributes nothing — it is not a typo, so it is not the typo error.
    ///
    /// Deleting the `dial_refused` filter in `protocol_candidates` fails the first assertion; making
    /// the typo guard ignore `refused` fails the `stolen`-only one.
    #[tokio::test(flavor = "multi_thread")]
    async fn blob_sources_drop_a_revoked_device() {
        let dir = tempfile::tempdir().unwrap();
        let (stolen, carol) = (eid_of(54), eid_of(55));
        let mesh = mesh_with_peers(
            dir.path(),
            &[("stolen", stolen, None), ("carol", carol, None)],
        )
        .await;
        revoke(&mesh, stolen);

        let out = blob_source_addrs(&mesh, &["stolen".into(), "carol".into()])
            .await
            .unwrap()
            .addrs;
        assert_eq!(
            out.iter().map(|a| *a.id.as_bytes()).collect::<Vec<_>>(),
            vec![carol],
            "a revoked source must never reach the fetch's dial set"
        );
        let out = blob_source_addrs(&mesh, &[eid_principal(stolen)])
            .await
            .expect("a revoked-only source is dropped, not an error")
            .addrs;
        assert!(out.is_empty(), "{out:?}");
    }

    /// #223 (review, item 5 + item 2): `peer_services` — the one resolver caller that DIALS —
    /// refuses a revoked roster device by every selector, while `peer_diagnostics` and
    /// `peer_hint_clear`, which dial nothing, keep working on the same device.
    ///
    /// Fixture: `gone` is PAIRED (a nickname row) and its endpoint is in the roster's
    /// `revoked_endpoints`, so the store alone admits it; `mallory` is a roster device under the
    /// identity-revoked `b64u:mallory` with no row. Alice is the dialled control.
    ///
    /// Deleting the `dial_refused` check in `peer_services` fails the `REVOKED` assertions for all
    /// three selectors — including the NICKNAME one, which the resolver's store check never
    /// refused. Moving that check back into `resolve_peer_endpoint` fails the diagnostics and
    /// hint-clear assertions.
    #[tokio::test(flavor = "multi_thread")]
    async fn peer_services_refuses_revoked_roster_devices_while_read_only_verbs_work() {
        let dir = tempfile::tempdir().unwrap();
        let (mallory, alice, gone) = (eid_of(71), eid_of(72), eid_of(73));
        let mesh = mesh_with_peers(dir.path(), &[("gone", gone, None)]).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        mesh.roster.install(roster_view(
            &[(mallory, "b64u:mallory"), (alice, "alice")],
            &[gone],
        ));
        revoke_identity(&mesh, "b64u:mallory");
        assert!(
            !mesh.store.is_refused(&gone),
            "fixture: the store alone admits `gone`"
        );

        for sel in [
            "gone".to_string(),
            eid_principal(gone),
            eid_principal(mallory),
        ] {
            let e = crate::daemon::handlers::peer_services(&state, sel.clone())
                .await
                .expect_err("peer_services dials, so it must refuse a revoked roster device");
            assert!(
                format!("{e:#}").contains("is REVOKED on this node"),
                "{sel}: {e:#}"
            );
        }
        // The control: alice is dialled (and is unreachable on a hermetic mesh), never refused.
        let e = crate::daemon::handlers::peer_services(&state, eid_principal(alice))
            .await
            .expect_err("alice is not running");
        assert!(!format!("{e:#}").contains("REVOKED"), "{e:#}");

        // The read-only verbs on the SAME revoked-but-paired device.
        crate::daemon::handlers::peer_diagnostics(&state, "gone")
            .await
            .expect("peer_diagnostics dials nothing, so a roster revocation must not block it");
        crate::daemon::handlers::peer_hint_clear(&state, "gone")
            .await
            .expect("clearing a stored hint dials nothing, so it must not be blocked either");
    }

    /// A bare endpoint on `ALPN_PING` that counts completed handshakes.
    async fn counting_ping_peer() -> (
        iroh::Endpoint,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![mcpmesh_net::ALPN_PING.to_vec()])
            .bind()
            .await
            .unwrap();
        let n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (e, c) = (ep.clone(), n.clone());
        let task = tokio::spawn(async move {
            while let Some(incoming) = e.accept().await {
                if incoming.await.is_ok() {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            }
        });
        (ep, n, task)
    }

    /// #223 (review): the reachability probe never PINGs a revoked device. `reachability_of` probes
    /// every stored row and a revocation keeps the row, so each `status` poll dialled the device the
    /// operator declared compromised. Both peers are real endpoints reachable through their stored
    /// hint; the unrevoked one is the control that a probe lands at all.
    ///
    /// And a refused probe COMMITS NOTHING (#89: a probe that never went out is not evidence): the
    /// stolen peer's cached row keeps its old `probed_at` and verdict, and no `Probe` frame is sent
    /// for it — through `probe_peer` and through `status`'s background refresh.
    ///
    /// Deleting the refused early-return in `probe_peer` fails the zero count; committing a
    /// `reachable: false` row there fails the unchanged-row assertion; broadcasting a transition
    /// there fails the no-frame assertion; asking `store.is_refused` instead of `dial_refused` there
    /// fails the zero count for the two roster-refused peers. The trailing `status.revoked`
    /// assertions fail when `roster_revocations` is dropped from `status_result`.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_reachability_probe_never_dials_a_revoked_peer() {
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            let dir = tempfile::tempdir().unwrap();
            let mesh = mesh_with_peers(dir.path(), &[]).await;
            // Three refused peers, one per clause of `dial_refused`: store-revoked, ROSTER-revoked,
            // and a roster device under a revoked `b64u:` user. The last two are refused by nothing
            // `PeerStore::is_refused` reads, so a probe that asked only the store dials them.
            let (stolen_ep, stolen, _t1) = counting_ping_peer().await;
            let (gone_ep, gone, _t2) = counting_ping_peer().await;
            let (mal_ep, mal, _t3) = counting_ping_peer().await;
            let (live_ep, live, _t4) = counting_ping_peer().await;
            for (ep, nick) in [
                (&stolen_ep, "stolen"),
                (&gone_ep, "gone"),
                (&mal_ep, "mal"),
                (&live_ep, "live"),
            ] {
                mesh.store
                    .add(crate::allowlist::PeerEntry {
                        endpoint_id: *ep.id().as_bytes(),
                        nickname: nick.into(),
                        services: vec![],
                        paired_at: None,
                        user_id: None,
                        last_addr: Some(serde_json::to_string(&ep.addr()).unwrap()),
                    })
                    .unwrap();
            }
            let id = |ep: &iroh::Endpoint| *ep.id().as_bytes();
            revoke(&mesh, id(&stolen_ep));
            mesh.roster.install(roster_view(
                &[(id(&mal_ep), "b64u:mallory"), (id(&live_ep), "alice")],
                &[id(&gone_ep)],
            ));
            revoke_identity(&mesh, "b64u:mallory");
            let refused = [
                ("stolen", id(&stolen_ep), &stolen),
                ("gone", id(&gone_ep), &gone),
                ("mal", id(&mal_ep), &mal),
            ];
            // An OLD verdict from before the revocation: reachable, probed at epoch 1. A committed
            // refusal would flip it to `false` with a fresh stamp — a transition, and a frame.
            // The live peer gets the SAME old row: it answers no pong, so its real probe commits
            // `reachable: false` and broadcasts — the control that this harness can see a frame.
            for eid in refused.iter().map(|r| r.1).chain([id(&live_ep)]) {
                mesh.reachability.lock().unwrap().insert(
                    eid,
                    crate::daemon::reach::ReachEntry {
                        reachable: true,
                        rtt_ms: Some(1),
                        probed_at: 1,
                        meta: String::new(),
                        services: vec![],
                        pong_at: Some(1),
                        seq: 0,
                        observed: 0,
                        path: mcpmesh_local_api::PeerPath::Unknown,
                    },
                );
            }
            let mut frames = mesh.reach_bcast.subscribe();

            crate::daemon::reach::probe_peer(&mesh, id(&live_ep)).await;
            for (_, eid, _) in &refused {
                crate::daemon::reach::probe_peer(&mesh, *eid).await;
            }
            // `status`: the cached rows are stale (epoch 1), so this spawns background refreshes.
            let _ = crate::daemon::reach::reachability_of(&mesh);
            assert!(
                live.load(std::sync::atomic::Ordering::SeqCst) > 0,
                "control: a probe of an unrevoked peer must land"
            );
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            for (name, eid, count) in &refused {
                assert_eq!(
                    count.load(std::sync::atomic::Ordering::SeqCst),
                    0,
                    "a revoked peer must never be probed ({name})"
                );
                let cache = mesh.reachability.lock().unwrap();
                let row = cache.get(eid).expect("the old row is still there");
                assert!(
                    row.reachable && row.probed_at == 1,
                    "a probe that never dialled must not freshen or flip the cached row ({name})"
                );
            }
            let refused_principals: Vec<String> = refused
                .iter()
                .map(|r| mcpmesh_net::EndpointId::from_bytes(r.1).principal())
                .collect();
            let mut live_frames = 0;
            while let Ok(f) = frames.try_recv() {
                assert!(
                    !refused_principals
                        .iter()
                        .any(|p| f.peer.principal.as_deref() == Some(p.as_str())),
                    "no Probe frame may be sent for a probe that never went out: {:?}",
                    f.peer.principal
                );
                live_frames += 1;
            }
            assert!(
                live_frames > 0,
                "control: the live peer's real probe flips its row, so a frame IS sent for it"
            );

            // #223 review, item 2: each of those stale rows can be matched to its revocation —
            // `status.revoked` names all three, the roster two with their roster sources.
            let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
            let status = crate::control::status_result(&state).unwrap();
            let source_of = |eid: [u8; 32]| {
                let p = mcpmesh_net::EndpointId::from_bytes(eid).principal();
                status
                    .revoked
                    .iter()
                    .find(|r| r.principal == p)
                    .map(|r| r.source.clone())
            };
            assert_eq!(source_of(id(&stolen_ep)).as_deref(), Some("local"));
            assert_eq!(source_of(id(&gone_ep)).as_deref(), Some("roster"));
            assert_eq!(source_of(id(&mal_ep)).as_deref(), Some("roster_identity"));
            assert_eq!(
                source_of(id(&live_ep)),
                None,
                "an unrevoked peer is not listed"
            );
        })
        .await
        .expect("probe test timed out");
    }

    /// #223 (review, item 1): the gossip BOOTSTRAP set excludes every device the outbound predicate
    /// refuses. `device_endpoints()` dropped only the roster's own revocations, so gossip dialled a
    /// `peer_revoke`d device and every device of a roster user spelled as a revoked `b64u:` identity.
    ///
    /// Fixture: alice (live), mallory (roster user `b64u:mallory`, identity-revoked, no row), bob's
    /// device (store-revoked), and this node itself. Before the revocations all three peers are in
    /// the set, so the expected `{alice}` cannot be met by refusing everything or by dropping only
    /// ourselves. Deleting the `refused_by` filter in `gossip_bootstrap` fails the second assertion.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_gossip_bootstrap_excludes_refused_devices() {
        let dir = tempfile::tempdir().unwrap();
        let mesh = mesh_with_peers(dir.path(), &[]).await;
        let (alice, mallory, bob, me) = (eid_of(81), eid_of(82), eid_of(83), eid_of(84));
        let view = roster_view(
            &[
                (alice, "alice"),
                (mallory, "b64u:mallory"),
                (bob, "bob"),
                (me, "me"),
            ],
            &[],
        );
        let ids = |b: crate::daemon::boot::bootstrap::GossipBootstrap| {
            let mut v: Vec<[u8; 32]> = b.ids().iter().map(|e| *e.as_bytes()).collect();
            v.sort();
            v
        };
        let mut everyone = vec![alice, mallory, bob];
        everyone.sort();
        assert_eq!(
            ids(crate::daemon::boot::bootstrap::gossip_bootstrap(
                &mesh.store,
                Some(&view),
                &me
            )),
            everyone,
            "fixture: before any revocation every other device bootstraps"
        );
        revoke_identity(&mesh, "b64u:mallory");
        revoke(&mesh, bob);
        assert_eq!(
            ids(crate::daemon::boot::bootstrap::gossip_bootstrap(
                &mesh.store,
                Some(&view),
                &me
            )),
            vec![alice],
            "gossip must not bootstrap from a device this node refuses to dial"
        );
    }

    /// #223 review, item 4: the bootstrap set the daemon SUBSCRIBES with is the filtered one. The
    /// plan is what `compose_roster_transport` consumes — it takes no roster and no store — so this
    /// pins the list the daemon actually builds, not only the helper.
    ///
    /// Passing `None` for the view inside `bootstrap::for_roster` fails the `{alice}` assertion.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_roster_transport_plan_bootstraps_without_refused_devices() {
        let dir = tempfile::tempdir().unwrap();
        let mesh = mesh_with_peers(dir.path(), &[]).await;
        let (alice, mallory, bob, me) = (eid_of(91), eid_of(92), eid_of(93), eid_of(94));
        mesh.roster.install(roster_view(
            &[
                (alice, "alice"),
                (mallory, "b64u:mallory"),
                (bob, "bob"),
                (me, "me"),
            ],
            &[],
        ));
        revoke_identity(&mesh, "b64u:mallory");
        revoke(&mesh, bob);
        let cfg = crate::config::Config::default();
        let our_id = iroh::EndpointId::from_bytes(&me).unwrap();
        assert!(
            crate::daemon::boot::plan_roster_transport(
                &mesh.roster,
                &mesh.store,
                &cfg,
                false,
                &our_id
            )
            .await
            .is_none(),
            "a pure-pairing daemon plans no transport"
        );
        let plan = crate::daemon::boot::plan_roster_transport(
            &mesh.roster,
            &mesh.store,
            &cfg,
            true,
            &our_id,
        )
        .await
        .expect("roster mode with an installed roster plans a transport");
        assert_eq!(plan.org_id, "acme");
        let ids: Vec<[u8; 32]> = plan.bootstrap.ids().iter().map(|e| *e.as_bytes()).collect();
        assert_eq!(
            ids,
            vec![alice],
            "the daemon must subscribe gossip with the filtered bootstrap set"
        );
    }

    /// A mesh whose roster-blob transport EXISTS, so `on_announce` gets as far as the fetch.
    async fn mesh_with_roster_blobs(
        dir: &std::path::Path,
    ) -> std::sync::Arc<crate::daemon::MeshState> {
        use std::sync::Arc;
        let store = Arc::new(crate::allowlist::PeerStore::open(&dir.join("state.redb")).unwrap());
        let pairs = Arc::new(crate::allowlist::AllowlistGate::new(store.clone()));
        let roster = Arc::new(crate::roster::gate::RosterGate::empty());
        let gate: Arc<dyn mcpmesh_net::TrustGate> = Arc::new(
            crate::roster::gate::ComposedGate::new(roster.clone(), pairs),
        );
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let blobs = crate::roster::transport::RosterBlobs::new(&endpoint);
        crate::daemon::MeshState::new(
            endpoint,
            gate,
            store,
            Arc::new(crate::pairing::LiveInvites::new()),
            "test".into(),
            dir.join("config.toml"),
            roster,
            Arc::new(mcpmesh_net::registry::ConnRegistry::new()),
            None,
            Some(blobs),
            None,
            None,
        )
    }

    /// A bare endpoint on the ROSTER blob ALPN that counts completed handshakes, then closes.
    async fn counting_roster_provider() -> (
        iroh::Endpoint,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![crate::roster::transport::BLOB_ALPN.to_vec()])
            .bind()
            .await
            .unwrap();
        let n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (e, c) = (ep.clone(), n.clone());
        let task = tokio::spawn(async move {
            while let Some(incoming) = e.accept().await {
                if let Ok(conn) = incoming.await {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    conn.close(0u32.into(), b"test");
                }
            }
        });
        (ep, n, task)
    }

    /// #223 (review, item 3): a roster announce whose blob PROVIDER is revoked here is not fetched.
    /// The announce is unsigned gossip; the roster signature protects the bytes, not the dial, which
    /// tells the provider this node's id, address and that it is online.
    ///
    /// The control is the same announce naming an UNREVOKED provider: that one is dialled, so a zero
    /// count for the revoked one means "refused", not "never got as far as a fetch". Deleting the
    /// `dial_refused` check in `on_announce` fails the first `expect`: the fetch then runs, dials
    /// the revoked provider, and errors. Asking `store.is_refused` in `MeshState::dial_refused`
    /// fails it for the two roster-refused providers; noting the address before the check fails
    /// the address-book assertions (the mesh carries a real `RosterAddrBook`).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_roster_announce_from_a_revoked_provider_is_not_fetched() {
        use std::sync::atomic::Ordering;
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            let dir = tempfile::tempdir().unwrap();
            let mesh = mesh_with_roster_blobs(dir.path()).await;
            // A REAL address book, as boot registers one, so "not even noted" is observable.
            let book = std::sync::Arc::new(crate::roster::transport::RosterAddrBook::register(
                &mesh.endpoint,
                64,
            ));
            assert!(mesh.roster_addr_book.set(book.clone()).is_ok());
            // One refused provider per clause of `dial_refused`: store-revoked, ROSTER-revoked, and
            // a roster device under a revoked `b64u:` user — the last two invisible to the store.
            let (stolen_ep, stolen, _t1) = counting_roster_provider().await;
            let (gone_ep, gone, _t2) = counting_roster_provider().await;
            let (mal_ep, mal, _t3) = counting_roster_provider().await;
            let (live_ep, live, _t4) = counting_roster_provider().await;
            let id = |ep: &iroh::Endpoint| *ep.id().as_bytes();
            revoke(&mesh, id(&stolen_ep));
            mesh.roster.install(roster_view(
                &[(id(&mal_ep), "b64u:mallory")],
                &[id(&gone_ep)],
            ));
            revoke_identity(&mesh, "b64u:mallory");
            let announce = |ep: &iroh::Endpoint| crate::roster::transport::RosterAnnounce {
                serial: 99,
                roster_hash: "blake3:00".into(),
                blob_ticket: iroh_blobs::ticket::BlobTicket::new(
                    ep.addr(),
                    iroh_blobs::Hash::new(b"roster"),
                    iroh_blobs::BlobFormat::Raw,
                )
                .to_string(),
            };

            for ep in [&stolen_ep, &gone_ep, &mal_ep] {
                crate::roster::distribute::on_announce(&mesh, announce(ep))
                    .await
                    .expect("a refused provider is a fail-safe no-op, not an error");
            }

            // The control, spawned: a fetch from a provider that closes on it may retry until the
            // gossip fetch timeout, and only the DIAL matters here.
            let (m, a) = (mesh.clone(), announce(&live_ep));
            let control = tokio::spawn(async move {
                let _ = crate::roster::distribute::on_announce(&m, a).await;
            });
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            while live.load(Ordering::SeqCst) == 0 {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "control: an unrevoked provider must be dialled"
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            control.abort();
            assert!(
                book.recorded_for(&id(&live_ep)).is_some(),
                "control: an unrevoked provider's address IS noted"
            );
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            for (name, ep, count) in [
                ("stolen", &stolen_ep, &stolen),
                ("gone", &gone_ep, &gone),
                ("mal", &mal_ep, &mal),
            ] {
                assert_eq!(
                    count.load(Ordering::SeqCst),
                    0,
                    "a revoked roster-blob provider must never be contacted ({name})"
                );
                assert!(
                    book.recorded_for(&id(ep)).is_none(),
                    "a refused provider's address must not even be noted ({name})"
                );
            }
        })
        .await
        .expect("announce test timed out");
    }

    /// A roster view whose users each own one primary device, plus revoked endpoints.
    fn roster_view(
        active: &[([u8; 32], &str)],
        revoked: &[[u8; 32]],
    ) -> mcpmesh_trust::roster::validate::RosterView {
        use mcpmesh_trust::roster::{Roster, RosterDevice, RosterUser, encode_b64u};
        let root = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let users = active
            .iter()
            .map(|(eid, uid)| RosterUser {
                user_id: (*uid).into(),
                display_name: (*uid).into(),
                user_pk: encode_b64u(&[1u8; 32]),
                groups: vec!["team".into()],
                devices: vec![RosterDevice {
                    endpoint_id: encode_b64u(eid),
                    label: "d".into(),
                    role: "primary".into(),
                }],
            })
            .collect();
        let r = mcpmesh_trust::roster::sign::mint_signed(
            &root,
            Roster {
                format: "mcpmesh-roster/1".into(),
                org_id: "acme".into(),
                serial: 3,
                issued_at: "2000-01-01T00:00:00Z".into(),
                expires_at: "2999-01-01T00:00:00Z".into(),
                groups: vec!["team".into()],
                users,
                revoked_endpoints: revoked.iter().map(|e| encode_b64u(e)).collect(),
                successor_root_pk: None,
                successor_sig: None,
                sig: String::new(),
            },
        );
        mcpmesh_trust::roster::validate::load_installed(&r, &root.verifying_key()).unwrap()
    }

    /// #223 item 2: the OUTBOUND twin of composed-gate rule 2. A roster device whose roster `user_id`
    /// is spelled `b64u:mallory`, with that identity revoked and NO pair row, is refused by the
    /// person→device race, by an `eid:` session dial, and by `connect_protocol` — as the gate
    /// refuses it inbound.
    ///
    /// Fixture discriminates: alice is rostered alongside (never refused), and the identity table
    /// also holds the bare string `alice`, which only the `b64u:` spelling rule keeps from refusing
    /// her. A roster-REVOKED endpoint (rule 1's roster half) is refused on the `eid:` paths too.
    ///
    /// Deleting the roster-identity clause in `dial_refused` fails the first `expect_err`; dropping
    /// the `b64u:` prefix check in `PeerStore::is_roster_user_revoked` fails the alice control;
    /// deleting `view.is_revoked` in `dial_refused` fails the roster-revoked `eid:` assertions.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_roster_device_under_a_revoked_b64u_identity_is_refused_outbound() {
        let dir = tempfile::tempdir().unwrap();
        let mesh = mesh_with_peers(dir.path(), &[]).await;
        let (mallory, alice, gone) = (eid_of(61), eid_of(62), eid_of(63));
        mesh.roster.install(roster_view(
            &[(mallory, "b64u:mallory"), (alice, "alice")],
            &[gone],
        ));
        revoke_identity(&mesh, "b64u:mallory");
        revoke_identity(&mesh, "alice");
        // Fixture: no pair row for mallory, so `PeerStore::is_refused` alone reads `Unpaired`; and
        // the inbound gate refuses her, which is the side this must agree with.
        assert!(mesh.store.resolve(&mallory).unwrap().is_none());
        assert!(
            !mesh.store.is_refused(&mallory),
            "fixture: the store alone admits no refusal"
        );
        assert!(
            mesh.gate.resolve(&mallory.into()).is_none(),
            "fixture: rule 2 refuses inbound"
        );
        assert!(
            mesh.gate.resolve(&alice.into()).is_some(),
            "fixture: alice is admitted inbound"
        );

        const BOUND: std::time::Duration = std::time::Duration::from_secs(30);

        // The person→device race.
        let e =
            match tokio::time::timeout(BOUND, super::dial_service(&mesh, "b64u:mallory", "notes"))
                .await
                .expect("the refusal is immediate, not a dial timeout")
            {
                Ok(_) => panic!("the race must refuse a device of a revoked roster identity"),
                Err(e) => e,
            };
        assert!(
            format!("{e:#}").contains("every device of that peer is REVOKED"),
            "{e:#}"
        );
        // The `eid:` session dial, for mallory AND for the roster-revoked endpoint.
        for eid in [mallory, gone] {
            let sel = eid_principal(eid);
            let e = match tokio::time::timeout(BOUND, super::dial_service(&mesh, &sel, "notes"))
                .await
                .expect("immediate")
            {
                Ok(_) => panic!("an eid: dial must refuse {sel}"),
                Err(e) => e,
            };
            assert!(
                format!("{e:#}").contains(&format!("{sel} is REVOKED on this node")),
                "{e:#}"
            );
            let e = super::connect_candidates(&mesh, &sel)
                .await
                .expect_err("connect_protocol must refuse it too");
            assert!(
                format!("{e:#}").contains("is REVOKED on this node"),
                "{e:#}"
            );
        }
        let e = super::connect_candidates(&mesh, "b64u:mallory")
            .await
            .expect_err("connect_protocol by roster person");
        assert!(
            format!("{e:#}").contains("is REVOKED on this node"),
            "{e:#}"
        );

        // The control: alice still resolves and is DIALLED — whatever the hermetic dial then does,
        // it must not be the revocation refusal.
        assert_eq!(
            super::connect_candidates(&mesh, "alice").await.unwrap(),
            vec![alice]
        );
        for sel in ["alice".to_string(), eid_principal(alice)] {
            if let Err(e) = tokio::time::timeout(BOUND, super::dial_service(&mesh, &sel, "notes"))
                .await
                .expect("a hermetic id-only dial fails fast")
            {
                assert!(
                    !format!("{e:#}").contains("REVOKED"),
                    "a live roster device must be dialled, not refused ({sel}): {e:#}"
                );
            }
        }

        // Lifting the identity revocation hands mallory back to the outbound path.
        assert!(mesh.store.unrevoke_user("b64u:mallory").unwrap());
        assert_eq!(
            super::connect_candidates(&mesh, "b64u:mallory")
                .await
                .unwrap(),
            vec![mallory]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The v4-mapped fixtures in the test below must actually REACH the filter as v6.
    ///
    /// They did not in the first version: removing `to_canonical()` — the gate's CRITICAL, and the
    /// whole point of those cases — left the suite green, because something between the JSON and
    /// the filter had already flattened them. A fixture that cannot express the hostile input is
    /// the same empty-fixture failure this repo keeps finding, wearing a new hat. This pins the
    /// fixture itself, so the bypass cases stay real.
    #[test]
    fn a_v4_mapped_address_survives_the_hint_round_trip_as_v6() {
        let id = iroh::SecretKey::from_bytes(&[8u8; 32]).public();
        let mapped: std::net::SocketAddr = "[::ffff:224.0.0.1]:1900".parse().unwrap();
        assert!(mapped.is_ipv6(), "precondition: Rust parses this as v6");

        let json = serde_json::to_string(&iroh::EndpointAddr::from_parts(
            id,
            [iroh::TransportAddr::Ip(mapped)],
        ))
        .unwrap();
        let back: iroh::EndpointAddr = serde_json::from_str(&json).unwrap();
        let iroh::TransportAddr::Ip(got) = back.addrs.iter().next().expect("one address") else {
            panic!("expected an Ip transport address");
        };
        assert!(
            got.is_ipv6(),
            "the mapped form must survive serde as v6, or the bypass cases below test nothing: \
             {got:?}"
        );
        assert!(
            got.ip().to_canonical().is_ipv4(),
            "and canonicalize to the v4 address iroh will actually send to: {got:?}"
        );
    }

    /// #203: a hint's undialable addresses are dropped before any dial sees them.
    ///
    /// The dial hint is where a REMOTE party's claim becomes a destination this node sends packets
    /// to — `rendezvous` stores an invite's `inviter_addr_json` verbatim — and iroh sends each
    /// outgoing datagram to every known path until one is selected. A crafted invite could aim this
    /// node's QUIC Initials, which RFC 9000 requires be padded to >=1200 bytes, at a chosen victim.
    ///
    /// Asserted at `stored_dial_addr` because that is the single function every dial path calls to
    /// turn a stored hint into addresses. Filtering per hint-source would leave whichever source
    /// someone adds next unprotected.
    #[test]
    fn a_stored_hint_cannot_aim_a_dial_at_a_non_peer() {
        let id = iroh::SecretKey::from_bytes(&[7u8; 32]).public();
        let hint = |addrs: Vec<&str>| {
            serde_json::to_string(&iroh::EndpointAddr::from_parts(
                id,
                addrs
                    .into_iter()
                    .map(|a| iroh::TransportAddr::Ip(a.parse().unwrap()))
                    .collect::<Vec<_>>(),
            ))
            .unwrap()
        };

        // Never a unicast peer: dropped.
        for bad in [
            "0.0.0.0:4433", // on Linux this reaches localhost
            "[::]:4433",
            "224.0.0.1:1900", // multicast — one packet, many hosts
            "239.255.255.250:1900",
            "255.255.255.255:80", // broadcast
            "[ff02::1]:4433",     // IPv6 all-nodes multicast
            // IPv4-MAPPED forms of the classes above. The gate proved these bypassed the filter
            // entirely: `Ipv6Addr::is_multicast` matches only `ff00::/8` and `is_unspecified` only
            // `::`, while iroh calls `to_canonical()` on ingest AND in its sender — so a mapped
            // address became real multicast strictly AFTER the check ran.
            "[::ffff:0.0.0.0]:4433",
            "[::ffff:224.0.0.1]:1900",
            "[::ffff:239.255.255.250]:1900",
            "[::ffff:255.255.255.255]:80",
            "224.0.0.0:4433",       // first multicast
            "239.255.255.255:4433", // last multicast
            "240.0.0.0:4433",       // first of the 240/4 reserved range
            "240.1.2.3:4433",
            "1.2.3.4:0", // a dial to port 0 is not a dial
            "[::ffff:1.2.3.4]:0",
        ] {
            let got = super::stored_dial_addr(Some(&hint(vec![bad])), id);
            assert!(
                got.addrs.is_empty(),
                "`{bad}` can never be a QUIC peer and must not become a dial target: {got:?}"
            );
            assert_eq!(got.id, id, "and the dial still names the peer: {got:?}");
        }

        // Legitimate addressing SURVIVES — filtering these would break real deployments rather
        // than attackers. Loopback in particular: two nodes on one host is supported, and most of
        // this repo's own suite pairs over 127.0.0.1.
        for good in [
            "127.0.0.1:4433", // same-host peers, and the test suite
            "[::1]:4433",
            "192.168.1.5:4433",
            "[fe80::1]:4433",   // IPv6 link-local: ordinary LAN addressing
            "169.254.3.4:4433", // APIPA — real when DHCP fails; see #203
            "1.2.3.4:4433",
            "16.0.0.1:4433", // the octet check must be a COMPARISON, not a bitwise NOT
            "223.255.255.254:4433", // last unicast address below the 224/4 multicast range
            "[::ffff:192.168.1.5]:4433", // a mapped LEGITIMATE address still survives
        ] {
            let got = super::stored_dial_addr(Some(&hint(vec![good])), id);
            assert_eq!(
                got.addrs.len(),
                1,
                "`{good}` is a legitimate peer address and must survive: {got:?}"
            );
        }

        // A MIXED hint keeps the good and drops the bad, rather than discarding the whole thing.
        let mixed = hint(vec!["0.0.0.0:1", "192.168.1.5:4433", "224.0.0.1:2"]);
        let got = super::stored_dial_addr(Some(&mixed), id);
        assert_eq!(
            got.addrs.len(),
            1,
            "only the dialable one survives: {got:?}"
        );

        // A hint of NOTHING BUT bad addresses degrades to the bare-id dial — the same fallback an
        // absent or id-mismatched hint takes — never a dial with no addresses and no discovery.
        let all_bad = hint(vec!["0.0.0.0:1", "224.0.0.1:2"]);
        let got = super::stored_dial_addr(Some(&all_bad), id);
        assert!(got.addrs.is_empty());
        assert_eq!(got.id, id);
    }

    /// `inject_service` names the service in `params._meta`, creating/replacing a non-object
    /// `params`/`_meta` and leaving a non-object frame untouched.
    ///
    /// **BOTH spellings are asserted every time (#49).** Emitting only the reverse-DNS key would
    /// make every dial to a peer running <= 0.50.0 refuse — that daemon reads the legacy key and
    /// would see an unnamed service. Checking one spelling would not catch it.
    #[test]
    fn inject_service_sets_both_meta_spellings_across_shapes() {
        use serde_json::json;
        let both = |f: &serde_json::Value, want: &str| {
            for key in mcpmesh_net::service::SERVICE_KEYS {
                assert_eq!(
                    f["params"]["_meta"][key], want,
                    "`{key}` must name the service: {f}"
                );
            }
        };
        // Object frame with no params → params._meta is created; other keys kept.
        let f = inject_service(json!({"method": "initialize"}), "kb");
        both(&f, "kb");
        assert_eq!(f["method"], "initialize");
        // Existing params object is preserved; _meta is added.
        let f = inject_service(json!({"params": {"x": 1}}), "loc");
        assert_eq!(f["params"]["x"], 1);
        both(&f, "loc");
        // A non-object `params` is REPLACED with an object.
        both(&inject_service(json!({"params": 7}), "kb"), "kb");
        // A non-object `_meta` is REPLACED (never merged into a scalar).
        both(
            &inject_service(json!({"params": {"_meta": "nope"}}), "kb"),
            "kb",
        );
        // A non-object frame is returned unchanged.
        assert_eq!(inject_service(json!("scalar"), "kb"), json!("scalar"));

        // The two must agree — a receiving daemon reading either gets the same answer, and a
        // MISMATCHED pair is refused by `select_service`, so emitting one would break every dial.
        let f = inject_service(json!({"method": "initialize"}), "kb");
        assert_eq!(
            f["params"]["_meta"][mcpmesh_net::service::SERVICE_KEYS[0]],
            f["params"]["_meta"][mcpmesh_net::service::SERVICE_KEYS[1]],
            "the spellings must never diverge: {f}"
        );
    }

    /// Pins `pipe_session`'s TEARDOWN DISCIPLINE (issue #25): control-side EOF ends only
    /// the REQUEST direction — it half-closes toward the peer (`TransportWriter::shutdown`)
    /// and PARKS, and the session ends solely when the peer closes. The pre-fix `select!`
    /// let the control direction's completion cancel the mesh→control drain, resetting the
    /// stream before the response (sometimes before the request itself) crossed the wire —
    /// the one-shot pipe shape. The control side is in-memory duplex; the mesh side is a
    /// REAL localhost iroh pair, because `SessionTransport` is concretely iroh-typed (an
    /// honest subset: the fake peer echoes raw frames — no gate, no backend; the full
    /// daemon-to-daemon path is `one_shot_connect.rs`). The peer deliberately withholds
    /// its echo until it sees the dialer's half-close, so a `pipe_session` that tears down
    /// on control EOF can never pass.
    #[tokio::test(flavor = "multi_thread")]
    async fn pipe_session_delivers_the_echo_after_control_eof() {
        use mcpmesh_net::framing::{FrameReader, Inbound, write_frame};
        use serde_json::json;
        use tokio::io::duplex;

        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            // The fake peer: a localhost accept side that collects frames until the
            // dialer's half-close (recv EOF), THEN echoes them back and closes.
            let server_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .alpns(vec![mcpmesh_net::ALPN_MCP.to_vec()])
                .bind()
                .await
                .unwrap();
            let server_addr = server_ep.addr();
            // Holds the peer's connection open until the dialer has DRAINED the echo:
            // `shutdown` only queues the FIN, and dropping the Connection/Endpoint right
            // behind it sends CONNECTION_CLOSE, which discards the buffered echo — the
            // test would then hang on transport loss instead of exercising the drain.
            let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
            let peer = tokio::spawn(async move {
                let incoming = server_ep.accept().await.expect("one inbound connection");
                let conn = incoming.await.expect("handshake");
                // `accept_bi` fires only once the dialer's first frame flushes — pre-fix,
                // the cancelled drain could reset the stream before even that.
                let (send, recv) = conn.accept_bi().await.expect("session bi-stream");
                let mut t = mcpmesh_net::SessionTransport::new(recv, send, 1024 * 1024);
                let mut seen = Vec::new();
                // Ok(None) = the dialer's clean half-close (its write half finished
                // while its read half stays open — the shutdown() under test).
                while let Ok(Some(f)) = t.recv_value().await {
                    seen.push(f);
                }
                for f in &seen {
                    t.send_value(f.clone()).await.unwrap();
                }
                t.shutdown().await.unwrap(); // finish the stream: the drain's clean end
                let _ = done_rx.await; // keep conn + endpoint alive until the test is done
                seen
            });

            let client_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .alpns(vec![mcpmesh_net::ALPN_MCP.to_vec()])
                .bind()
                .await
                .unwrap();
            let transport = mcpmesh_net::connect(&client_ep, server_addr, "echo")
                .await
                .unwrap()
                .0;

            // Control side, one whole DuplexStream per direction (dropping `ctl_in_w`
            // is the control-side EOF; a split half would keep the stream alive).
            let (mut ctl_in_w, ctl_in_r) = duplex(64 * 1024);
            let (ctl_out_w, ctl_out_test_r) = duplex(64 * 1024);
            let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
            write_frame(&mut ctl_in_w, &init).await.unwrap();
            drop(ctl_in_w);

            let session = tokio::spawn(pipe_session(
                transport,
                "echo",
                FrameReader::new(ctl_in_r, 1024 * 1024),
                ctl_out_w,
            ));

            // The echo must reach the control writer BEFORE teardown: the peer only sent
            // it after our half-close, so a request-direction-wins teardown drops it.
            let mut ctl_out = FrameReader::new(ctl_out_test_r, 1024 * 1024);
            match ctl_out.next().await.unwrap() {
                Some(Inbound::Frame(f)) => {
                    assert_eq!(f["id"], 1, "the echoed initialize answers our id: {f}");
                    assert_eq!(
                        f["params"]["_meta"]["mcpmesh/service"], "echo",
                        "the peer saw the service-injected initialize (the one enumerated \
                         edit), echoed verbatim: {f}"
                    );
                }
                other => panic!("the echo must reach the control side, got {other:?}"),
            }
            assert!(
                ctl_out.next().await.unwrap().is_none(),
                "the peer closing ends the session cleanly (control-side EOF)"
            );
            session.await.unwrap().expect("pipe_session returns Ok");
            let _ = done_tx.send(()); // release the peer's hold-open
            assert_eq!(
                peer.await.unwrap(),
                vec![inject_service(init, "echo")],
                "the peer received exactly the injected initialize before the half-close"
            );
        })
        .await
        .expect("pipe_session drain test timed out");
    }

    /// #186: a RACED dial must carry the same stored hints a single-device dial does.
    ///
    /// The single-entry path attached `last_addr` citing #27 — "a cold daemon must not depend on
    /// external discovery to reach a paired peer" — and the multi-device path three lines away
    /// kept only the endpoint ids, throwing away hints it had literally just read out of the store.
    /// So one person with one device worked offline and the same person with two did not, with
    /// nothing saying the second device had changed that.
    ///
    /// Asserted on the ADDRESSES the race would dial, which is where the information was lost.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_raced_dial_carries_the_stored_hints() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = crate::daemon::testutil::hermetic_mesh(cfg).await;

        let first = *iroh::SecretKey::from_bytes(&[61u8; 32]).public().as_bytes();
        let second = *iroh::SecretKey::from_bytes(&[62u8; 32]).public().as_bytes();
        let sock: std::net::SocketAddr = "127.0.0.1:4455".parse().unwrap();
        let hint = serde_json::to_string(&iroh::EndpointAddr::from_parts(
            iroh::EndpointId::from_bytes(&first).unwrap(),
            [iroh::TransportAddr::Ip(sock)],
        ))
        .unwrap();

        // One person, two devices: the first has a persisted hint, the second never dialled.
        for (eid, nick, last) in [
            (first, "alice-laptop", Some(hint.clone())),
            (second, "alice-phone", None),
        ] {
            mesh.store
                .add(crate::allowlist::PeerEntry {
                    endpoint_id: eid,
                    nickname: nick.into(),
                    services: vec![],
                    paired_at: None,
                    user_id: Some("b64u:alice".into()),
                    last_addr: last,
                })
                .unwrap();
        }

        let addrs = super::hinted_addrs(&mesh, vec![first, second])
            .await
            .expect("candidates resolve");
        assert_eq!(addrs.len(), 2, "every candidate stays a candidate");

        // The device WITH a hint must be dialable without discovery.
        assert!(
            !addrs[0].addrs.is_empty(),
            "a raced candidate must carry its stored hint — a bare id is discovery-only, which is \
             exactly what made a two-device person unreachable on a LAN with no discovery"
        );
        assert_eq!(
            *addrs[0].id.as_bytes(),
            first,
            "and it must be THAT device's address"
        );

        // The device WITHOUT one degrades to a bare id rather than being dropped: presence and
        // hints never remove a candidate, they only change how it is reached.
        assert!(addrs[1].addrs.is_empty());
        assert_eq!(*addrs[1].id.as_bytes(), second);
    }

    /// `stored_dial_addr` attaches the persisted hint only when it parses AND names the
    /// stored id; anything else degrades to the bare-id, discovery-only dial (addresses are
    /// hints, never identity).
    #[test]
    fn stored_dial_addr_attaches_validates_and_degrades() {
        // Real curve points (arbitrary raw bytes are not valid ed25519 public keys).
        let id = iroh::SecretKey::from_bytes(&[7u8; 32]).public();
        let other = iroh::SecretKey::from_bytes(&[8u8; 32]).public();
        let sock: std::net::SocketAddr = "127.0.0.1:4444".parse().unwrap();
        let stored = iroh::EndpointAddr::from_parts(id, [iroh::TransportAddr::Ip(sock)]);
        let stored_json = serde_json::to_string(&stored).unwrap();

        // Stored addr with the MATCHING id → attached verbatim (id + direct addrs).
        let addr = stored_dial_addr(Some(&stored_json), id);
        assert_eq!(addr, stored, "a matching-id hint is dialed as stored");

        // No stored addr → bare id (discovery-only).
        assert_eq!(stored_dial_addr(None, id), iroh::EndpointAddr::from(id));

        // Unparseable stored addr → bare id (graceful degradation, never an error).
        assert_eq!(
            stored_dial_addr(Some("not json"), id),
            iroh::EndpointAddr::from(id)
        );

        // Stored addr claiming a DIFFERENT id → IGNORED (bare id): an addr is a dial hint,
        // never identity — a poisoned hint must not redirect the dial's identity pin.
        let mismatched = serde_json::to_string(&iroh::EndpointAddr::from_parts(
            other,
            [iroh::TransportAddr::Ip(sock)],
        ))
        .unwrap();
        assert_eq!(
            stored_dial_addr(Some(&mismatched), id),
            iroh::EndpointAddr::from(id)
        );
    }

    #[tokio::test]
    async fn connect_with_timeout_fails_fast_on_an_unreachable_peer() {
        // A relay-disabled localhost endpoint dialing a random, unresolved id can never connect; the
        // explicit timeout must return Err WELL before iroh's own idle timeouts (defense-in-depth).
        let ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![mcpmesh_net::ALPN_MCP.to_vec()])
            .bind()
            .await
            .unwrap();
        let dead = iroh::EndpointAddr::from(iroh::EndpointId::from_bytes(&[3u8; 32]).unwrap());
        let start = std::time::Instant::now();
        let r = super::connect_with_timeout(
            &ep,
            dead,
            "svc",
            std::time::Duration::from_millis(300),
            None,
        )
        .await;
        assert!(r.is_err(), "an unreachable dial times out to Err");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(3),
            "the explicit timeout fired fast"
        );
    }

    /// #166: the per-session config is COMPLETE — it carries this node's keepalive, not iroh's.
    ///
    /// **This is the test that would have caught the 0.48.0 gate's first finding**, and the reason
    /// this function returns a config instead of mutating a builder. `ConnectOptions::with_transport_config`
    /// REPLACES the endpoint's config rather than overlaying it, so a config built from a fresh
    /// builder resets everything the caller did not name. The first cut set only the idle timeout:
    /// a node with `keep_alive_secs = 2` got a per-session connection with a **5s** keepalive, so a
    /// 3s idle timeout — which this same function had just validated as safe against 2s — severed
    /// sessions whose peers were alive and answering. The guard checked a keepalive the connection
    /// did not have.
    ///
    /// Asserted on `Debug`, the same instrument `iroh_transport_defaults_are_what_the_docs_claim`
    /// and `build_transport_config`'s tests use: a knob that was never applied is invisible to
    /// "did it connect".
    #[test]
    fn the_per_session_config_carries_this_nodes_keepalive_not_irohs_default() {
        // A node on a lossy link: keepalive 2s, well under iroh's 5s default.
        let cfg = super::per_session_transport_config(Some(3), 2)
            .expect("3s is legal against a 2s keepalive")
            .expect("a value was supplied, so a config is built");
        let d = format!("{cfg:?}");
        assert!(
            d.contains("max_idle_timeout: Some(3000)"),
            "the caller's idle timeout must be set: {d}"
        );
        assert!(
            d.contains("keep_alive_interval: Some(2s)"),
            "this NODE's keepalive must survive — a fresh builder resets it to iroh's 5s, which \
             would make the 3s idle timeout sever a healthy session: {d}"
        );
        assert!(
            d.contains("default_path_keep_alive_interval: Some(2s)"),
            "…including the PATH keepalive, which is the one iroh actually pings on: {d}"
        );

        // `0` = no idle timeout from this side, and the keepalive still survives.
        let cfg = super::per_session_transport_config(Some(0), 2)
            .unwrap()
            .unwrap();
        let d = format!("{cfg:?}");
        assert!(d.contains("max_idle_timeout: None"), "{d}");
        assert!(d.contains("keep_alive_interval: Some(2s)"), "{d}");
    }

    /// #166: resolution and every refusal, which must be an error and never a silent fallback.
    ///
    /// A knob that quietly did nothing is what the #56 gate found TWICE in this same area.
    #[test]
    fn a_per_session_idle_timeout_resolves_or_refuses_but_never_silently_ignores() {
        // Absent → inherit the node-wide config: no per-connection config is built at all, which is
        // what tells `connect_with_transport_config` to take the plain `connect` path.
        assert!(
            super::per_session_transport_config(None, 5)
                .unwrap()
                .is_none()
        );

        // An ordinary value builds one.
        assert!(
            super::per_session_transport_config(Some(30), 5)
                .unwrap()
                .is_some()
        );

        // AT or BELOW the keepalive is refused — a keepalive arriving after the idle timer has
        // fired severs a session whose peer is alive and answering.
        for bad in [1u64, 4, 5] {
            let e = super::per_session_transport_config(Some(bad), 5)
                .expect_err("at or below the keepalive must be refused");
            let msg = format!("{e:#}");
            assert!(
                msg.contains("keepalive") && msg.contains(&bad.to_string()),
                "the refusal must name the value and the reason: {msg}"
            );
        }
        // …and the boundary is the node's ACTUAL keepalive, not a constant.
        assert!(super::per_session_transport_config(Some(3), 2).is_ok());
        assert!(super::per_session_transport_config(Some(2), 2).is_err());

        // Out of QUIC's encodable range is an ERROR, never a fallback to the node default.
        let e = super::per_session_transport_config(Some(u64::MAX), 5)
            .expect_err("an unencodable value must be refused");
        assert!(
            format!("{e:#}").contains("out of the range QUIC can encode"),
            "{e:#}"
        );
    }
}
