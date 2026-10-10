//! On-demand reachability probing (pairing-mode liveness): the trust-gated `mcpmesh/ping/1`
//! probe, its in-memory result cache on `MeshState`, and the non-blocking `status` projection
//! that refreshes stale entries in the background.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use mcpmesh_net::ALPN_PING;
use mcpmesh_net::framing::{FrameReader, Inbound, write_frame};

use crate::util::epoch_now_i64;

use super::MeshState;

/// One cached reachability probe result (spec: pairing-mode liveness). Ephemeral, in-memory —
/// stored in `MeshState::reachability`, keyed by endpoint-id. `probed_at` is epoch seconds.
///
/// `#[non_exhaustive]` (#215): construct one inside this crate only, so the next field added is
/// not another breaking change for embedders that read these rows.
#[derive(Clone)]
#[non_exhaustive]
pub struct ReachEntry {
    pub reachable: bool,
    pub rtt_ms: Option<u64>,
    pub probed_at: i64,
    /// The peer's app metadata (#40), read from its pong — empty when it set none, or when a
    /// hostile/oversized value was dropped on receive. Advisory display data, never authz.
    pub meta: String,
    /// The services this peer currently grants US (#52), read from its pong — the discovery
    /// answer. Only services whose allow admits our principal; empty if it shares nothing.
    pub services: Vec<String>,
    /// When the pong that `meta` and `services` came from arrived (epoch seconds), or `None` when
    /// this row carries no pong at all (#215 review).
    ///
    /// A live session's path watcher seeds a `reachable: true` row with empty `meta`/`services` and
    /// a fresh `probed_at`. Without this field that row's empty `services` meant both "the peer
    /// offers you nothing" and "nobody has asked yet", and `peer_services` answered the second as
    /// the first — for a full TTL after every session-first flow, which is the ordinary embedder
    /// flow once sessions share a connection.
    pub pong_at: Option<i64>,
    /// Monotonic ticket taken when this probe STARTED (#58 review). Probes of one peer overlap
    /// routinely — `reachability_of` spawns a refresh per stale peer, and BOTH `status` and
    /// `subscribe` call it — and they complete out of order, so a slow probe that started earlier
    /// must not overwrite a fast one that started later. Without this the older (timed-out) result
    /// wins on arrival, poisoning the cache for a full TTL and, since #58, PUSHING a false "went
    /// offline" for a peer that is up.
    pub seq: u64,
    /// Monotonic ticket taken when this probe's result was COMMITTED (#176) — drawn from the same
    /// counter as [`seq`](Self::seq), so the two are directly comparable.
    ///
    /// `seq` orders probes by when they STARTED; this orders them by when their evidence actually
    /// landed. Both are needed, because #58 and #176 are the same bug facing opposite ways: a
    /// timeout must not overwrite a pong, whether that pong came from a probe that started earlier
    /// or one that started later. The question that separates them is not "which is newer" but
    /// "did the peer answer inside my window?" — and answering it needs the pong's arrival
    /// compared against the timeout's start, which is exactly `observed` vs `seq`.
    pub observed: u64,
    /// HOW the peer was reached on this probe (#64) — direct/hole-punched vs through a relay.
    /// Captured alongside `reachable`/`rtt_ms` so it shares ONE TTL and one `age_secs`, rather
    /// than inventing a second staleness rule; `Unknown` covers "never probed" for free.
    pub path: mcpmesh_local_api::PeerPath,
}

/// One reachability transition on the way to `subscribe`'s fan-out (#150): the row, plus WHICH
/// producer observed it.
///
/// The ring used to carry a bare `PeerReachability`, which left the two senders indistinguishable
/// by the time [`run_subscription`](crate::control) built the frame. Only the sender knows, so it
/// stamps the attribution — see [`ReachabilitySource`](mcpmesh_local_api::ReachabilitySource) for
/// why the difference matters to an embedder.
#[derive(Clone, Debug)]
pub struct ReachTransition {
    pub peer: mcpmesh_local_api::PeerReachability,
    pub source: mcpmesh_local_api::ReachabilitySource,
}

/// Advisory reachability TTL: a cache entry older than this is refreshed by a NON-BLOCKING
/// background probe on the next [`reachability_of`] read.
pub const REACH_TTL_SECS: i64 = 20;

/// The reachability probe's hard deadline — a peer that has not ponged within this window is
/// reported unreachable. No retries/backoff/persistence (YAGNI); reachable ⇔ a pong in time.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The ping arm's rate-limit close reason (#89 gate) — the sibling idiom of `b"pair rate
/// limited"` / `b"blob rate limited"`. ONE constant for both sides so responder and prober
/// cannot drift: the accept arm writes it, [`probe_once`] matches it, and the match is what lets
/// a throttled probe be treated as non-evidence instead of a false "peer offline".
pub(crate) const PING_THROTTLE_CLOSE: &[u8] = b"ping rate limited";

/// Marker error: the probe was REFUSED by the peer's ping rate limiter (#89 gate). A refusal is
/// not evidence of unreachability in either direction — [`probe_peer`] commits nothing for it.
#[derive(Debug)]
struct ProbeThrottled;

impl std::fmt::Display for ProbeThrottled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "probe refused: ping rate limited")
    }
}

impl std::error::Error for ProbeThrottled {}

/// Did the peer close this connection with the ping limiter's reason? Read off the CONNECTION
/// rather than parsed out of whichever stream error surfaced first — the refusal races our
/// `open_bi`/write/read, so any of them can be the failure site, and all of them leave the
/// application close on `close_reason()`.
fn throttle_closed(conn: &iroh::endpoint::Connection) -> bool {
    matches!(
        conn.close_reason(),
        Some(iroh::endpoint::ConnectionError::ApplicationClosed(ac))
            if ac.reason.as_ref() == PING_THROTTLE_CLOSE
    )
}

/// Probe one peer over [`ALPN_PING`] and cache the result. Dials the peer by id WITH its stored
/// `last_addr` hint attached when usable, exactly like `dial::dial_service`'s single-nickname
/// fallback (discovery still resolves/merges addresses; hermetic localhost tests seed a
/// `MemoryLookup` or store a hint), sends one ping frame,
/// reads the pong, and measures RTT (dial + round-trip, stamped AT THE PONG — it deliberately
/// EXCLUDES the `PATH_SETTLE` window that `settled_path` spends afterwards deciding which path we
/// are on, #123). Writes the outcome into the in-memory
/// `MeshState::reachability` cache and returns it. Reachable ⇔ a pong arrived within
/// `PROBE_TIMEOUT`; a gate refusal (no pong) or any dial/IO failure is a clean `reachable:false`.
///
/// That equivalence holds for the entry this call CONSTRUCTS as of #128, and did not before —
/// with two caveats it does not cover: a malformed/oversized pong is not a pong (it bails), and a
/// newer overlapping probe can supersede ours, in which case THAT entry is returned. Before #128
/// the equivalence failed for a much more ordinary reason: `PROBE_TIMEOUT` used to wrap the
/// `PATH_SETTLE` classification too, so a relayed peer whose pong arrived at ~2.4s timed out while
/// we were deciding which route it took, and was reported offline despite answering. The two
/// questions now get separate deadlines, and a classification failure degrades `path` to
/// `Unknown` rather than flipping `reachable`.
pub async fn probe_peer(mesh: &Arc<MeshState>, endpoint_id: [u8; 32]) -> ReachEntry {
    // Ticket FIRST, before any await: ordering is by probe START, not completion.
    let seq = mesh
        .probe_seq
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // #223: never PING a device this node revoked. `reachability_of` probes every stored row, and a
    // revocation keeps the row, so every `status` poll used to dial the device the operator had
    // declared compromised — announcing this node, and its address, to whoever holds it.
    //
    // And commit NOTHING for it, the throttle arm's rule below (#89): no dial happened, so there is
    // no evidence about the peer. Committing `reachable: false` stamped a fresh `probed_at` and
    // broadcast a `Probe`-sourced transition for a probe that never went out. The row returned is
    // uncommitted and never cached. Fails CLOSED on a join error, like every revocation read.
    let m = mesh.clone();
    let refused = tokio::task::spawn_blocking(move || super::dial::dial_refused(&m, &endpoint_id))
        .await
        .unwrap_or(true);
    if refused {
        return ReachEntry {
            reachable: false,
            rtt_ms: None,
            probed_at: epoch_now_i64(),
            meta: String::new(),
            services: Vec::new(),
            pong_at: None,
            seq,
            observed: seq,
            path: mcpmesh_local_api::PeerPath::Unknown,
        };
    }
    let started = tokio::time::Instant::now();
    let outcome = tokio::time::timeout(PROBE_TIMEOUT, probe_once(mesh, endpoint_id, started)).await;
    // #89 gate (HIGH): a probe REFUSED by the peer's rate limiter is not evidence the peer is
    // down — commit NOTHING for it. Writing `reachable: false` here gave a live, paired peer a
    // fresh false offline for a full TTL and broadcast the transition. The caller gets the
    // previous entry if one exists (the refusal proves the peer is up, but the entry stays
    // honest about when it was actually probed), else an UNCOMMITTED unreachable row — never
    // cached, so it cannot poison anything.
    //
    // Two accepted bounds (#142 second gate), stated rather than fixed:
    // - A hostile PAIRED peer that answers every probe with the throttle close keeps its entry
    //   permanently stale, so every `reachability_of` read re-spawns a refresh — no 20s
    //   quiescence, one dial per read, until it pongs or goes down. Bounded by our own poll
    //   rate, costs the peer a connection each time, and it can only pin ITS OWN row (whose
    //   meta/path/rtt it already controls); a probe backoff belongs with #89's follow-up work.
    // - The check below matches `Ok(Err(_))` only: a refusal whose CONNECTION_CLOSE arrives
    //   after PROBE_TIMEOUT surfaces as `Elapsed` and commits `reachable: false` normally — a
    //   lost close frame is indistinguishable from a dead peer, so that is the honest verdict.
    //
    // The previous entry may be a session path watcher's reachable row with no pong (#215). It is
    // returned as it is, `pong_at: None` included — that field IS the "services unknown" flag, and
    // `peer_services` refuses on it rather than answering `[]`.
    if let Ok(Err(e)) = &outcome
        && e.downcast_ref::<ProbeThrottled>().is_some()
    {
        let previous = mesh
            .reachability
            .lock()
            .expect("reachability lock not poisoned")
            .get(&endpoint_id)
            .cloned();
        return previous.unwrap_or(ReachEntry {
            reachable: false,
            rtt_ms: None,
            probed_at: epoch_now_i64(),
            meta: String::new(),
            services: Vec::new(),
            pong_at: None,
            seq,
            // Never cached (that is the point of this arm), so no other probe ever compares
            // against it. It carries our own start ticket rather than a fresh one so the row is
            // self-consistent if a caller inspects it.
            observed: seq,
            path: mcpmesh_local_api::PeerPath::Unknown,
        });
    }
    // `probe_once` returns the peer's (already length-capped) metadata on a reachable pong.
    // `path` rides the probe OUTCOME, so it is exactly as fresh as `reachable`/`rtt_ms` and an
    // unreachable peer reports `Unknown` rather than a stale route (#64 review). Reading it from
    // the endpoint's address map after the fact reported `Direct` for a peer that had just gone
    // away, because iroh leaves those entries Active for up to a minute after the connection dies.
    let (reachable, meta, services, path, rtt_ms) =
        classify(outcome, |conn| async move { settled_path(&conn).await }).await;
    // The COMMIT ticket (#176). Taken before the lock so it is drawn exactly once per probe, and
    // from the same counter as `seq` so `observed` and `seq` are comparable across probes.
    let observed = mesh
        .probe_seq
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let probed_at = epoch_now_i64();
    let entry = ReachEntry {
        reachable,
        rtt_ms,
        probed_at,
        meta,
        services,
        // A reachable verdict here always came with a pong; an unreachable one never did.
        pong_at: reachable.then_some(probed_at),
        seq,
        observed,
        path,
    };
    // Commit under ONE lock acquisition, DISCARDING a result a newer probe has already superseded
    // (#58 review): probes of one peer overlap and complete out of order.
    let outcome = {
        let mut cache = mesh
            .reachability
            .lock()
            .expect("reachability lock not poisoned");
        match cache.get_mut(&endpoint_id) {
            // A newer writer already landed. Drop our VERDICT and report THEIRS, so a caller never
            // acts on a value the cache disagrees with — but keep our PONG if theirs carries none
            // (#215 final review). The newer writer is routinely the session path watcher, whose
            // first reading (#225) takes a later ticket than the probe `peer_services` just started
            // and holds no `meta`/`services`; discarding the pong here answered `[]` right after a
            // session opened, 9 runs out of 10. Only the payload and its `pong_at` move: the row's
            // `probed_at` and tickets stay the winner's, so nothing about its freshness is forged.
            Some(newer) if !supersedes(seq, newer) => {
                if newer.reachable && newer.pong_at.is_none() && entry.pong_at.is_some() {
                    newer.meta.clone_from(&entry.meta);
                    newer.services.clone_from(&entry.services);
                    newer.pong_at = entry.pong_at;
                }
                Outcome::Superseded(newer.clone())
            }
            // #176: our probe started LATER, but it found nothing while a pong landed INSIDE our
            // window. Drop ours — see `contradicted_by`.
            Some(pong) if contradicted_by(&entry, seq, pong) => Outcome::Superseded(pong.clone()),
            other => {
                let previous = other.map(|e| e.clone());
                cache.insert(endpoint_id, entry.clone());
                Outcome::Committed(previous)
            }
        }
    };
    let previous = match outcome {
        Outcome::Superseded(newer) => return newer,
        Outcome::Committed(previous) => previous,
    };

    if is_transition(previous.as_ref(), &entry) {
        // Only for a peer the STORE knows: `probe_peer` is also reachable via `peer_services`,
        // which accepts a bare `eid:` with no stored row. Emitting for one of those would push a
        // NAMELESS frame for an endpoint the snapshot's store-driven list can never contain — the
        // stream asserting state `status` contradicts (#58 review).
        if let Some(peer) = stored_row(mesh, endpoint_id, &entry) {
            // Best-effort: `send` errors only when there are no subscribers, the common case.
            let _ = mesh.reach_bcast.send(ReachTransition {
                peer,
                // #150: a probe is a throwaway dial. It says nothing about any live connection,
                // and a consumer must be able to tell that without inferring it from `rtt_ms`.
                source: mcpmesh_local_api::ReachabilitySource::Probe,
            });
        }
    }
    entry
}

/// Should a probe holding ticket `seq` overwrite `existing` (#58 review)?
///
/// Tickets are taken at probe START, so a LOWER ticket is an OLDER probe: it may complete later
/// (a 3s timeout losing to a 50ms pong) but must not win. Equal is impossible — tickets are unique
/// — and is treated as "write" so the guard can never wedge.
pub(crate) fn supersedes(seq: u64, existing: &ReachEntry) -> bool {
    seq >= existing.seq
}

/// Is `ours` — a probe that started at `seq` — contradicted by the cached `existing` entry (#176)?
///
/// True when all three hold: our verdict is `reachable: false`, the cached verdict is
/// `reachable: true`, and that pong was OBSERVED after we started (`existing.observed > seq`).
///
/// **Why a timeout is weaker evidence than a pong.** A pong is a fact about the peer at a known
/// instant. A `PROBE_TIMEOUT` elapse is a fact about *our dial* over a window — "no pong reached
/// me". When a pong demonstrably arrived inside that same window, on someone else's connection,
/// our window contained a live peer and our "unreachable" is simply wrong about it. Committing it
/// anyway is what pinned `reachable: false` for a full `REACH_TTL_SECS` on a peer that never went
/// anywhere. Same reasoning as #89's throttle arm: not every failed probe is evidence of a down
/// peer.
///
/// **Why [`supersedes`] alone could not fix it.** That rule orders by probe START, so among N
/// overlapping probes the last-started always wins — and the last-started is the most contended,
/// so it is the one most likely to time out. #58 stopped an OLD probe's timeout landing on a NEW
/// probe's pong; this stops the mirror image. Neither ordering by start nor ordering by completion
/// gets both, because the real relation is between one probe's window and another's arrival.
///
/// **It does not wedge a genuinely down peer as reachable.** This only ever refuses probes that
/// were already in flight when the last pong landed. Any probe starting AFTER it has
/// `seq > existing.observed`, so once the peer really stops answering the next probe commits
/// `reachable: false` normally. The stale-positive window is one probe cycle, not the 20s TTL.
fn contradicted_by(ours: &ReachEntry, seq: u64, existing: &ReachEntry) -> bool {
    !ours.reachable && existing.reachable && existing.observed > seq
}

/// What [`probe_peer`]'s commit step decided.
enum Outcome {
    /// Our result was written; carries the entry it replaced, if any.
    Committed(Option<ReachEntry>),
    /// A newer probe had already landed; carries THAT result.
    Superseded(ReachEntry),
}

/// How long to let a fresh connection settle before classifying its path (#64).
///
/// A dial starts on the relay and hole-punches in the background; measured on loopback the direct
/// path is SELECTED about 300 ms in. Classifying immediately after the pong therefore reported
/// `Relay` for every peer on a relay-enabled node — useless for the locality claim, and wrong in
/// the same direction as the address-map version this replaced. It is only ever spent on a
/// connection that has NOT yet selected a direct path.
///
/// It is a SEPARATE budget from `PROBE_TIMEOUT`, not a slice of it (#128). It used to be nested
/// inside, which is precisely the bug: a relayed peer whose pong landed near the deadline timed
/// out mid-classification and was reported offline while answering. A probe's worst case is now
/// `PROBE_TIMEOUT + PATH_SETTLE`.
const PATH_SETTLE: Duration = Duration::from_millis(600);

/// [`selected_path`] with a bounded wait for hole-punching to finish (#64).
///
/// Returns as soon as a DIRECT path is selected; otherwise polls until [`PATH_SETTLE`] elapses and
/// reports whatever is selected then — a genuinely relayed peer costs the full window once per
/// probe, and reports `Relay`, which is the truthful answer for it.
///
/// **This one line is NOT covered by any test (#110 review).** [`settle`]'s logic is pinned below,
/// but nothing catches this call site passing `Duration::ZERO` instead of [`PATH_SETTLE`] — that
/// mutation reintroduces the exact #64 regression (every peer reports `Relay` on a relay-enabled
/// node) with the whole suite green. Verified by mutation, not assumed. Pinning it needs a probe
/// against a real connection whose punch has not yet landed, which is precisely the timing race
/// that made this suite flaky; a latency assertion is not an option either, since timing
/// assertions on a loaded machine have lied to this repo before. Kept as a deliberate, stated gap
/// rather than a claim of coverage.
async fn settled_path(conn: &iroh::endpoint::Connection) -> mcpmesh_local_api::PeerPath {
    settle(PATH_SETTLE, || selected_path(conn)).await
}

/// The polling half of [`settled_path`], over a closure rather than a live connection (#110).
///
/// Split out because the property "keep looking until Direct, then stop; give up at the deadline"
/// is pure timing logic, and pinning it through a real hole-punch made the integration suite
/// depend on CI network luck. The e2e test proves we read the SELECTED path; this proves we WAIT
/// for it. Neither test can cover both without one of them going vacuous — reordering the e2e test
/// to be non-flaky silently stopped it catching a zeroed `PATH_SETTLE`.
///
/// Note the seam covers the WINDOW's behaviour, not the window's USE — see [`settled_path`].
pub(crate) async fn settle<F>(window: Duration, mut probe: F) -> mcpmesh_local_api::PeerPath
where
    F: FnMut() -> mcpmesh_local_api::PeerPath,
{
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let path = probe();
        if path == mcpmesh_local_api::PeerPath::Direct || tokio::time::Instant::now() >= deadline {
            return path;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Which path is this connection actually using (#64)?
///
/// `Path::is_selected()` is iroh's own answer to "selected for application data transmission" —
/// the only signal that means traffic. Every other view (the endpoint's remote-address map,
/// `TransportAddrUsage::Active`) reports *open* paths, and iroh deliberately keeps a relay path
/// open as a standby for the life of the connection, so those views call a hole-punched connection
/// relayed forever.
///
/// **Except on the accepting side of a second connection to the same peer (#213).** iroh keeps
/// ONE selected four-tuple per remote endpoint, not per connection. When two connections to the
/// same peer settle on different local/remote address pairs — routine on a dual-stack host, and
/// the measured case for a mesh session one way plus an app-protocol connection the other — the
/// endpoint-level selection can match only one of them, a server cannot open the selected tuple
/// on the other (only clients open paths), and iroh (1.0.3; 1.2.0 unchanged, re-measured) then
/// records that connection's selected path as `None` for as long as the mismatch lasts
/// (`remote_state.rs` `apply_selected_path` →
/// `path_watcher.rs` `record_selected`). `is_selected()` is false on its only open path while
/// every byte flows over it.
///
/// So [`classify_paths`] adds one inference: a connection with exactly ONE open path sends
/// everything on it, selected or not. "Open path" here means iroh's list of the connection's
/// validated paths, which is complete unless iroh's actor lagged (`remote_state.rs`
/// `handle_path_event`'s `Lagged` arm drops the event with no recovery). Several open paths and
/// none selected is genuinely ambiguous (all are `Backup`, noq may use any) and stays `Unknown`,
/// never a guess. See [`measured_path`] for the reading that resolves that case by measurement.
///
/// **No close check here, deliberately** (#213 review). A probe classifies a connection the ping
/// responder closes right after its pong, so by the end of `settled_path`'s window the dialer's
/// connection is routinely closed — and the last path it used is exactly the answer the probe
/// wants. Checking `close_reason()` here made every probe of a RELAYED peer report `Unknown`,
/// lost the relay URL, and emitted a spurious Relay→Unknown frame. The "is this connection still
/// carrying data" question belongs to [`measured_path`], which does check.
pub(crate) fn selected_path(conn: &iroh::endpoint::Connection) -> mcpmesh_local_api::PeerPath {
    let paths = conn.paths();
    classify_paths(
        paths
            .iter()
            .map(|p| (path_kind(p.remote_addr()), p.is_selected())),
    )
}

/// The structural rule behind [`selected_path`], over `(kind, is_selected)` pairs so it is
/// testable without a connection:
///
/// 1. a selected path → its kind;
/// 2. otherwise exactly one open path → its kind (the only place the bytes can go);
/// 3. otherwise `Unknown`.
pub(crate) fn classify_paths<I>(paths: I) -> mcpmesh_local_api::PeerPath
where
    I: IntoIterator<Item = (mcpmesh_local_api::PeerPath, bool)>,
{
    let mut only: Option<mcpmesh_local_api::PeerPath> = None;
    let mut count = 0usize;
    for (kind, selected) in paths {
        if selected {
            return kind;
        }
        count += 1;
        only = Some(kind);
    }
    match (count, only) {
        (1, Some(kind)) => kind,
        _ => mcpmesh_local_api::PeerPath::Unknown,
    }
}

/// The [`PeerPath`](mcpmesh_local_api::PeerPath) a path of this remote address would report.
/// A transport mcpmesh does not model is `Unknown`: say so rather than guess.
fn path_kind(addr: &iroh::TransportAddr) -> mcpmesh_local_api::PeerPath {
    match addr {
        iroh::TransportAddr::Relay(url) => mcpmesh_local_api::PeerPath::Relay {
            url: Some(sanitize_relay_url(url)),
        },
        iroh::TransportAddr::Ip(_) => mcpmesh_local_api::PeerPath::Direct,
        _ => mcpmesh_local_api::PeerPath::Unknown,
    }
}

/// How long [`measured_path`] watches a connection's per-path counters before answering.
///
/// Long enough that a live media or MCP session moves at least one frame on the path it uses
/// (bolo's #213 measurement sends audio every 20ms; a keepalive-only mesh session moves nothing,
/// which is the idle case the structural rule covers), short enough for a call-setup gate.
pub(crate) const PATH_MEASURE_WINDOW: Duration = Duration::from_millis(250);

/// How many [`PATH_MEASURE_WINDOW`]s [`measured_path`] will spend before accepting `Unknown`.
pub(crate) const PATH_MEASURE_WINDOWS: usize = 3;
// One window cannot retry an idle `Unknown`, which is the only reason the bound exists (#213).
const _: () = assert!(
    PATH_MEASURE_WINDOWS > 1,
    "PATH_MEASURE_WINDOWS must allow a retry"
);

/// Application frames (STREAM + DATAGRAM, sent and received) a path has carried so far.
///
/// Only those two frame types are counted, on purpose: iroh keeps a standby relay path alive
/// with PINGs and ACKs, so any byte or packet count would show a relay path "moving" while it
/// carries no user data at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct AppFrames(pub(crate) u64);

impl AppFrames {
    /// One path's application frames so far.
    pub(crate) fn of(stats: &iroh::endpoint::PathStats) -> Self {
        Self(
            stats.frame_tx.stream
                + stats.frame_tx.datagram
                + stats.frame_rx.stream
                + stats.frame_rx.datagram,
        )
    }

    fn since(self, then: Self) -> u64 {
        self.0.saturating_sub(then.0)
    }
}

/// One open path as [`measured_path`] samples it: id, kind, application frames so far.
pub(crate) type PathSample = (
    iroh::endpoint::PathId,
    mcpmesh_local_api::PeerPath,
    AppFrames,
);

/// iroh's [`PathEvent`](iroh::endpoint::PathEvent), projected one-to-one with NO policy.
///
/// Exists because iroh marks every `PathEvent` variant `#[non_exhaustive]`, so no test outside
/// iroh can construct one (E0639). The projection below is the only code that touches iroh's enum
/// and it decides nothing; every decision — which events matter, what a lost or unrecognised event
/// means — is in [`WindowEvent::from_observed`], which is tested for every variant.
#[derive(Debug, Clone)]
pub(crate) enum ObservedEvent {
    Opened {
        id: iroh::endpoint::PathId,
    },
    Closed {
        id: iroh::endpoint::PathId,
        remote_addr: iroh::TransportAddr,
        // Boxed as iroh boxes it: `PathStats` dwarfs every other variant.
        last_stats: Box<iroh::endpoint::PathStats>,
    },
    Selected,
    Lagged,
    /// A variant this build of mcpmesh does not know (`PathEvent` is `#[non_exhaustive]`).
    Unrecognised,
}

impl From<iroh::endpoint::PathEvent> for ObservedEvent {
    fn from(event: iroh::endpoint::PathEvent) -> Self {
        match event {
            iroh::endpoint::PathEvent::Opened { id, .. } => Self::Opened { id },
            iroh::endpoint::PathEvent::Closed {
                id,
                remote_addr,
                last_stats,
                ..
            } => Self::Closed {
                id,
                remote_addr,
                last_stats,
            },
            iroh::endpoint::PathEvent::Selected { .. } => Self::Selected,
            iroh::endpoint::PathEvent::Lagged { .. } => Self::Lagged,
            _ => Self::Unrecognised,
        }
    }
}

/// A path event seen during a measurement window, reduced to what attribution needs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WindowEvent {
    /// A path was added to the connection's path list inside the window.
    Opened { id: iroh::endpoint::PathId },
    /// A path closed; `frames` is its lifetime application-frame count from `last_stats`.
    Closed {
        id: iroh::endpoint::PathId,
        kind: mcpmesh_local_api::PeerPath,
        frames: AppFrames,
    },
    /// The subscriber missed events: something happened that this window cannot see.
    Lagged,
    /// An event kind this build does not know: it may mean anything, so the window cannot vouch
    /// for what it saw.
    Unrecognised,
}

impl WindowEvent {
    /// The policy over [`ObservedEvent`]. Only `Selected` is dropped — selection moves no frames,
    /// and the counters, not the selection, are what this reading trusts.
    pub(crate) fn from_observed(event: ObservedEvent) -> Option<Self> {
        match event {
            ObservedEvent::Opened { id } => Some(Self::Opened { id }),
            ObservedEvent::Closed {
                id,
                remote_addr,
                last_stats,
            } => Some(Self::Closed {
                id,
                kind: path_kind(&remote_addr),
                frames: AppFrames::of(&last_stats),
            }),
            ObservedEvent::Selected => None,
            ObservedEvent::Lagged => Some(Self::Lagged),
            ObservedEvent::Unrecognised => Some(Self::Unrecognised),
        }
    }
}

/// What ONE measurement window observed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WindowReading {
    /// Application frames moved on paths of a known kind, and nothing this window could observe
    /// is unaccounted for: `Direct` or `Relay`. (Not "every frame": see [`measured_path`]'s blind
    /// spots.)
    Moved(mcpmesh_local_api::PeerPath),
    /// Nothing moved: the structural reading, which may itself be `Unknown`.
    Idle(mcpmesh_local_api::PeerPath),
    /// Frames may have moved somewhere this window cannot see — events were lost or unrecognised, a
    /// path vanished with no `Closed` event, an unmodelled transport moved, the event stream ended,
    /// or the connection closed.
    Unobservable,
}

/// Classify one window (#213).
///
/// `before`/`after` are per-path samples taken at the window's edges; `events` are the path
/// events delivered between them, from a subscription taken BEFORE `before`. iroh adds a path to
/// the list before sending `Opened` (`path_watcher.rs` `record_opened`) and removes it before
/// sending `Closed` (`record_abandoned`), which gives three exact facts about any path id:
///
/// - **in `before`**: it existed at the baseline, and its movement is its later count (the
///   `after` sample, or its `Closed { last_stats }`) minus the baseline;
/// - **not in `before`, but `Opened` in the window**: it opened after the baseline, so its whole
///   count is movement — whether it is still in `after` or closed with a `Closed` event;
/// - **`Closed` with no baseline and no `Opened`**: it was removed from the list BEFORE the
///   baseline and only its `Closed` crossed the subscription. None of its frames are in this
///   window, so it is ignored — counting its lifetime total read a pre-window close as traffic.
///
/// A path in `after` with no baseline and no `Opened` event is counted whole too (the list gained
/// it after the baseline, so its `Opened` is in flight; for a relay path that is the safe
/// direction).
///
/// Then: any relay path moved → `Moved(Relay)`, whatever else happened — one frame over a relay
/// falsifies "private". `Lagged`, an unrecognised event, a moving unmodelled path, or a path that
/// DISAPPEARED — in `before` or `Opened`, but neither in `after` nor `Closed` (its `Closed` has
/// not arrived, so what it carried is unknown) → `Unobservable`. Otherwise a direct path moved →
/// `Moved(Direct)`; otherwise `Idle(fallback)`.
///
/// There is no aggregate cross-check against `Connection::stats()`: per-path and connection
/// stats are read under separate lock acquisitions, and frames landing between the two reads made
/// that comparison both mask real movement and invent phantom movement under load (#213 review,
/// measured).
pub(crate) fn classify_window(
    before: &[PathSample],
    after: &[PathSample],
    events: &[WindowEvent],
    fallback: mcpmesh_local_api::PeerPath,
) -> WindowReading {
    let baseline =
        |id: &iroh::endpoint::PathId| before.iter().find(|(b, _, _)| b == id).map(|(_, _, f)| *f);
    let opened: Vec<iroh::endpoint::PathId> = events
        .iter()
        .filter_map(|e| match e {
            WindowEvent::Opened { id } => Some(*id),
            _ => None,
        })
        .collect();
    let mut relay: Option<mcpmesh_local_api::PeerPath> = None;
    let mut direct_moved = false;
    let mut unobservable = false;
    let mut mark = |kind: &mcpmesh_local_api::PeerPath| match kind {
        mcpmesh_local_api::PeerPath::Relay { .. } => {
            relay.get_or_insert_with(|| kind.clone());
        }
        mcpmesh_local_api::PeerPath::Direct => direct_moved = true,
        _ => unobservable = true,
    };

    let mut closed = Vec::new();
    let mut untrusted_stream = false;
    for event in events {
        match event {
            WindowEvent::Closed { id, kind, frames } => {
                closed.push(*id);
                let from = match baseline(id) {
                    Some(base) => base,
                    None if opened.contains(id) => AppFrames::default(),
                    // Removed before the baseline; only the event crossed into the window.
                    None => continue,
                };
                if frames.since(from) > 0 {
                    mark(kind);
                }
            }
            WindowEvent::Lagged | WindowEvent::Unrecognised => untrusted_stream = true,
            WindowEvent::Opened { .. } => {}
        }
    }
    for (id, kind, now) in after {
        if now.since(baseline(id).unwrap_or_default()) > 0 {
            mark(kind);
        }
    }
    let accounted =
        |id: &iroh::endpoint::PathId| after.iter().any(|(a, _, _)| a == id) || closed.contains(id);
    let disappeared =
        before.iter().any(|(id, _, _)| !accounted(id)) || opened.iter().any(|id| !accounted(id));

    if let Some(relay) = relay {
        return WindowReading::Moved(relay);
    }
    if unobservable || untrusted_stream || disappeared {
        return WindowReading::Unobservable;
    }
    if direct_moved {
        WindowReading::Moved(mcpmesh_local_api::PeerPath::Direct)
    } else {
        WindowReading::Idle(fallback)
    }
}

/// Combine up to `windows` readings into one answer (#213). Pure over `measure`, so the retry
/// policy is testable with scripted readings rather than timing.
///
/// - `Moved(path)` answers immediately — a `Relay` window is never retried away.
/// - `Unobservable` answers `Unknown` immediately. A later window must not turn a window that may
///   have carried relayed frames into a `Direct` answer, so there is no retry after one.
/// - `Idle(path)` answers `path` when it is known. `Idle(Unknown)` — an idle connection with
///   several open paths and none selected — is the ONLY reading retried: nothing moved, so a later
///   window that sees traffic is new evidence rather than a second opinion.
pub(crate) async fn combine_windows<F, Fut>(
    windows: usize,
    mut measure: F,
) -> mcpmesh_local_api::PeerPath
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = WindowReading>,
{
    for _ in 0..windows {
        match measure().await {
            WindowReading::Moved(path) => return path,
            WindowReading::Unobservable => return mcpmesh_local_api::PeerPath::Unknown,
            WindowReading::Idle(mcpmesh_local_api::PeerPath::Unknown) => continue,
            WindowReading::Idle(path) => return path,
        }
    }
    mcpmesh_local_api::PeerPath::Unknown
}

/// What one measurement window needs from a connection. A seam so the ORDER of operations in
/// [`measure_window`] — subscribe before the baseline, drain during the window, drain again after
/// the final sample — is testable with a scripted source instead of live timing.
pub(crate) trait WindowSource {
    fn is_closed(&self) -> bool;
    fn subscribe(&self) -> n0_future::boxed::BoxStream<ObservedEvent>;
    fn sample(&self) -> Vec<PathSample>;
    fn fallback(&self) -> mcpmesh_local_api::PeerPath;
}

impl WindowSource for iroh::endpoint::Connection {
    fn is_closed(&self) -> bool {
        // iroh keeps a closed connection's path list and counters (`PathStateSender::close`
        // leaves the list populated), so they would read as a live `Direct`.
        self.close_reason().is_some()
    }
    fn subscribe(&self) -> n0_future::boxed::BoxStream<ObservedEvent> {
        use n0_future::StreamExt as _;
        Box::pin(self.path_events().map(ObservedEvent::from))
    }
    fn sample(&self) -> Vec<PathSample> {
        self.paths()
            .iter()
            .map(|p| {
                (
                    p.id(),
                    path_kind(p.remote_addr()),
                    AppFrames::of(&p.stats()),
                )
            })
            .collect()
    }
    fn fallback(&self) -> mcpmesh_local_api::PeerPath {
        selected_path(self)
    }
}

/// Measure ONE window.
pub(crate) async fn measure_window<S: WindowSource>(source: &S, window: Duration) -> WindowReading {
    use n0_future::StreamExt as _;
    if source.is_closed() {
        return WindowReading::Unobservable;
    }
    // Subscribe BEFORE the baseline, so no path can close between the two unseen.
    let mut events = source.subscribe();
    let before = source.sample();
    let mut seen = Vec::new();
    // Drain WHILE the window runs: the broadcast holds 8 events, and a hole-punch round opens and
    // abandons more than that — reading only at the end would turn most punches into `Lagged`.
    let deadline = tokio::time::Instant::now() + window;
    loop {
        match tokio::time::timeout_at(deadline, events.next()).await {
            Ok(Some(event)) => seen.extend(WindowEvent::from_observed(event)),
            // The stream ended before the window did: the connection is going away, and the rest
            // of this window is unobserved — not a shorter window.
            Ok(None) => return WindowReading::Unobservable,
            Err(_elapsed) => break,
        }
    }
    if source.is_closed() {
        return WindowReading::Unobservable;
    }
    let after = source.sample();
    // Events already queued when `after` was taken: a path removed from the list just before the
    // sample has its `Closed` sent just after (`record_abandoned` sends outside its lock).
    //
    // `unconstrained`: the stream is backed by a tokio broadcast, whose `recv` is cooperative. With
    // the task's coop budget spent, a poll returns Pending even with events queued, and a
    // single-poll drain would stop early — dropping exactly the `Closed` this drain exists for.
    loop {
        match n0_future::future::now_or_never(tokio::task::coop::unconstrained(events.next())) {
            Some(Some(event)) => seen.extend(WindowEvent::from_observed(event)),
            Some(None) => return WindowReading::Unobservable,
            None => break,
        }
    }
    classify_window(&before, &after, &seen, source.fallback())
}

/// The per-connection reading an app protocol can act on (#213): MEASURE where the application
/// frames went, window by window, and fall back to the structural [`selected_path`] only when
/// nothing moved. See [`classify_window`] for one window and [`combine_windows`] for how up to
/// [`PATH_MEASURE_WINDOWS`] of them become one answer.
///
/// Measurement is what makes this usable on the accepting side, where `is_selected()` can be
/// false on every open path for the life of a connection (see [`selected_path`]). It is also the
/// only reading that can tell, when such a connection holds a relay path AND a direct path with
/// neither selected, which one noq actually used.
///
/// A closed connection — before or after any window — answers `Unknown`.
///
/// **Best-effort evidence, not a guarantee.** It sees only what iroh's path list and path events
/// report, and two gaps are outside anything mcpmesh can observe:
///
/// - noq can send on a newly validated path before iroh's actor has recorded it (a sub-millisecond
///   gap between noq's `Established` and iroh's `record_opened`); frames sent in that gap are never
///   attributed to any path;
/// - when iroh's own actor lags on noq's path events (`remote_state.rs` `handle_path_event`'s
///   `Lagged` arm drops the event with no recovery), a path can carry frames without ever being
///   listed or producing an event.
///
/// So a `Direct` answer means "no relay frames were observed", not "no frame transited a relay".
pub(crate) async fn measured_path(
    conn: &iroh::endpoint::Connection,
    window: Duration,
) -> mcpmesh_local_api::PeerPath {
    combine_windows(PATH_MEASURE_WINDOWS, || measure_window(conn, window)).await
}

/// Render a relay URL for the wire WITHOUT its userinfo (#64 review).
///
/// Relay URLs are operator-supplied (`[network].relay_urls`, `set_relays`), so a
/// `https://user:token@relay.internal/` would otherwise ship that token to every local-API client.
/// Scheme + host + port only: enough to name which relay is in use, carrying no credential and no
/// path/query.
pub fn sanitize_relay_url(url: &iroh::RelayUrl) -> String {
    let u: &url::Url = url; // RelayUrl derefs to Url
    match (u.host_str(), u.port()) {
        (Some(host), Some(port)) => format!("{}://{host}:{port}", u.scheme()),
        (Some(host), None) => format!("{}://{host}", u.scheme()),
        (None, _) => u.scheme().to_string(),
    }
}

/// Normalize a relay URL STRING to the same rendering [`sanitize_relay_url`] produces, so a raw
/// operator-configured value can be compared with a live [`RelayInfo`](mcpmesh_local_api::RelayInfo)
/// URL. `None` when it does not parse as a relay URL at all.
///
/// Exists so the PORCELAIN can do that comparison (#125's `doctor` relay check) without taking a
/// runtime dependency on iroh — the shipped `mcpmesh` binary reaches iroh through this crate on
/// purpose. Sharing the one function is also what stops the two renderings drifting: a prefix-match
/// substitute reported a dead relay as connected whenever it was a strict prefix of a healthy one.
pub fn normalize_relay_url(raw: &str) -> Option<String> {
    raw.parse::<iroh::RelayUrl>()
        .ok()
        .map(|u| sanitize_relay_url(&u))
}

/// Did this probe CHANGE what a subscriber already believes (#58)?
///
/// The baseline is the SNAPSHOT, not the cache: a peer with no cache entry is reported
/// `reachable: false` there (see [`reachability_of`]'s `None` arm). So first knowledge is news only
/// when the peer turns out to be UP — a first probe confirming "down" merely restates the snapshot,
/// and emitting it produced a burst of spurious "is now offline" frames on every daemon restart,
/// immediately after a snapshot that had just said exactly that (#58 review).
///
/// Deliberately NOT sensitive to `rtt_ms`/`meta`/`services`: those drift on every refresh and are
/// advisory detail, so treating them as transitions would emit a frame per TTL refresh for a peer
/// that is simply staying up — chatty, and not a decision point for any consumer.
fn is_transition(previous: Option<&ReachEntry>, current: &ReachEntry) -> bool {
    match previous {
        None => current.reachable,
        // #92: `path` counts, `rtt_ms`/`meta` do not. #64 grouped path with the advisory drift
        // fields, which was wrong — and the same release's own docs are what prove it. `Direct` is
        // the only value that supports a locality claim, and rendering `Unknown` as private is
        // documented as "the one misuse that turns this field into a false privacy statement". A
        // field carrying a truth claim about WHERE USER DATA WENT cannot be advisory: without this,
        // a session that degrades Direct -> Relay stays silently mislabelled as private for its
        // whole duration, and the only fallback is polling a 20s-TTL cache.
        //
        // Flapping was #64's stated reason for excluding it. `settled_path` already applies the
        // 600ms PATH_SETTLE window, so the cached value is post-hole-punch, and probes are TTL-gated
        // — a peer cannot emit more than once per refresh regardless.
        Some(prev) => prev.reachable != current.reachable || prev.path != current.path,
    }
}

/// Build the wire row for ONE peer. THE single constructor of `PeerReachability` — both the
/// `status`/snapshot list and the #58 transition event go through it, so the two genuinely cannot
/// drift (an earlier version merely CLAIMED this while `reachability_of` built its rows inline).
///
/// `entry == None` is a peer never probed: `reachable: false` with no age, which a consumer renders
/// as "checking…". `age_secs` is the caller's, since the snapshot computes it from `probed_at`
/// while a transition event is fresh by construction.
pub(crate) fn reachability_row(
    nickname: String,
    endpoint_id: [u8; 32],
    entry: Option<&ReachEntry>,
    age_secs: Option<u64>,
) -> mcpmesh_local_api::PeerReachability {
    mcpmesh_local_api::PeerReachability {
        path: entry.map(|e| e.path.clone()).unwrap_or_default(),
        name: nickname,
        reachable: entry.is_some_and(|e| e.reachable),
        rtt_ms: entry.and_then(|e| e.rtt_ms),
        age_secs,
        meta: entry.map(|e| e.meta.clone()).unwrap_or_default(),
        // #42: the eid: device principal, so a row joins to the authenticated endpoint rather
        // than the non-unique nickname.
        principal: Some(mcpmesh_net::EndpointId::from_bytes(endpoint_id).principal()),
    }
}

/// The transition event's row, or `None` when the peer has no stored entry — see the call site.
/// A point read, not the O(n) scan an earlier version used.
fn stored_row(
    mesh: &Arc<MeshState>,
    endpoint_id: [u8; 32],
    entry: &ReachEntry,
) -> Option<mcpmesh_local_api::PeerReachability> {
    let nickname = mesh.store.resolve(&endpoint_id).ok().flatten()?.nickname;
    Some(reachability_row(
        nickname,
        endpoint_id,
        Some(entry),
        Some(0),
    ))
}

/// The dial → ping → pong half of [`probe_peer`], separated so the whole exchange is one timeout
/// unit. Reuses the real iroh 1.0.1 call shapes from `dial.rs`/`pairing::rendezvous`
/// (`endpoint.connect`, `open_bi`, `write_frame`, `finish`, a framed read).
async fn probe_once(
    mesh: &Arc<MeshState>,
    endpoint_id: [u8; 32],
    started: tokio::time::Instant,
) -> Result<(String, Vec<String>, iroh::endpoint::Connection, u64)> {
    let id = iroh::EndpointId::from_bytes(&endpoint_id)
        .map_err(|e| anyhow::anyhow!("invalid endpoint id: {e}"))?;
    // Attach the pairing-persisted `last_addr` hint, exactly as `dial::dial_service` does
    // (issue #27): a just-paired peer is reachable at the address the handshake PROVED, so the
    // probe must not sit waiting on discovery to resolve the bare id. Without this the redeemer's
    // first probe blows `PROBE_TIMEOUT` and `status` calls a freshly-paired peer offline. A
    // missing/unparseable/id-mismatched hint degrades to the bare-id dial (see `stored_dial_addr`).
    // The redb read blocks, so it runs on the blocking pool (the fs house rule).
    let store = mesh.store.clone();
    let last_addr = tokio::task::spawn_blocking(move || store.resolve(&endpoint_id))
        .await
        .map_err(|e| anyhow::anyhow!("join peer resolve for probe: {e}"))?
        .ok()
        .flatten()
        .and_then(|e| e.last_addr);
    let addr = super::dial::stored_dial_addr(last_addr.as_deref(), id);
    let conn = mesh.endpoint.connect(addr, ALPN_PING).await?;
    let exchanged = exchange(&conn, started).await;
    // A limiter refusal (#89 gate) surfaces as whichever stream error lost the race with the
    // peer's close; the close reason on the CONNECTION is the reliable signal. Rewrite it to the
    // marker so `probe_peer` can refuse to treat it as evidence. Checked only on failure: a
    // successful exchange got a pong, throttled or not.
    match exchanged {
        Ok((meta, services, rtt_ms)) => Ok((meta, services, conn, rtt_ms)),
        Err(e) if throttle_closed(&conn) => Err(e.context(ProbeThrottled)),
        Err(e) => Err(e),
    }
}

/// The stream half of [`probe_once`], over an established connection: open the bi-stream, send
/// one ping frame, read the pong. Split from the dial so its failure can be classified against
/// the connection's close reason ([`throttle_closed`]) at ONE site rather than per stream call.
/// `started` is the PROBE's start — before the dial — so `rtt_ms` keeps meaning dial + round
/// trip (#123), not just this stream half.
async fn exchange(
    conn: &iroh::endpoint::Connection,
    started: tokio::time::Instant,
) -> Result<(String, Vec<String>, u64)> {
    // We open the bi-stream and send one ping frame — the write is what makes the responder's
    // `accept_bi` resolve (a silent QUIC stream is invisible to the peer). We say nothing
    // meaningful; the responder speaks the pong. `finish()` closes our (empty) send direction.
    let (mut send, recv) = conn.open_bi().await?;
    write_frame(&mut send, &serde_json::json!({ "ping": true })).await?;
    let _ = send.finish();
    let mut reader = FrameReader::new(
        tokio::io::BufReader::new(recv),
        mcpmesh_net::framing::MAX_FRAME_BYTES,
    );
    match reader.next().await? {
        // Any well-formed pong frame ⇒ reachable. It MAY carry the peer's app metadata (#40);
        // extract it, but — defense in depth against a compromised paired peer — enforce the
        // SAME ≤256B cap the sender applies (an oversized/absent/non-string value ⇒ empty).
        // The channel is authenticated (this is a trust-gated paired peer), so no signature is
        // needed; the cap is the only receive-side hardening required.
        // Classify the path from THIS connection, while it is still open (#64). It must come
        // from `Connection::paths()` + `Path::is_selected()` — the path actually carrying
        // application data — and NOT from the endpoint's remote-address map: iroh deliberately
        // keeps a relay path OPEN as a standby after hole-punching succeeds ("Relay and custom
        // paths are kept open", `remote_state.rs`), so an address-usage view reports a relay for
        // a connection whose data provably flows direct. That version made `Direct` unreachable
        // on any relay-enabled node — i.e. every real deployment — which is the opposite of the
        // truthful-locality-claim this field exists to support.
        Some(Inbound::Frame(v)) => {
            // Stamp the RTT HERE, at the pong — before any classification work (#123). Measuring
            // afterwards meant a relayed peer could never report under 600ms, because most of the
            // number was our own deliberate wait. An embedder read ~820ms on two machines one LAN
            // hop apart and filed it as a 66x fleet latency degradation; ~73% of that figure was
            // the settle window. `rtt_ms` means what its name says: dial + round trip.
            //
            // The CONNECTION is returned rather than a classified path (#128): classifying it
            // spends up to `PATH_SETTLE`, and that must happen OUTSIDE `PROBE_TIMEOUT` — see
            // `probe_peer`.
            let rtt_ms = started.elapsed().as_millis() as u64;
            Ok((pong_meta(&v), pong_services(&v), rtt_ms))
        }
        _ => anyhow::bail!("no pong from peer"),
    }
}

/// What the timed exchange produced: the pong's metadata, admitted services, the live connection
/// and the pong-stamped RTT — or a timeout.
type ExchangeOutcome<C> =
    std::result::Result<Result<(String, Vec<String>, C, u64)>, tokio::time::error::Elapsed>;

/// Turn a probe exchange outcome into the reported facts, classifying the path OUTSIDE the
/// exchange deadline (#128) and WITHOUT touching `rtt_ms` (#123).
///
/// A seam, and it earns its keep twice over — both hazards it guards were shipped and caught:
///
/// - **#128:** `reachable` is decided by the EXCHANGE alone. Folding classification into
///   `PROBE_TIMEOUT` meant a relayed peer whose pong landed after ~2.4s timed out DURING
///   classification and was reported offline while it was answering. Failing to classify degrades
///   `path` to `Unknown`, never `reachable` to false.
/// - **#123:** `rtt_ms` arrives already stamped at the pong and is passed through UNTOUCHED. The
///   fix for #128 moved the settle window into the same scope as `started`, four lines away, so
///   re-deriving the RTT after classification is a one-line edit that silently restores #123. An
///   earlier version of this commit deleted #123's pin and called the ordering "structural"; it
///   was not, and the adversarial gate reproduced the bug with the whole suite green.
///
/// Generic over the settle future so both properties are testable on a paused clock, with no live
/// connection and no wall-clock assertion.
async fn classify<C, F, Fut>(
    outcome: ExchangeOutcome<C>,
    settle: F,
) -> (
    bool,
    String,
    Vec<String>,
    mcpmesh_local_api::PeerPath,
    Option<u64>,
)
where
    F: FnOnce(C) -> Fut,
    Fut: std::future::Future<Output = mcpmesh_local_api::PeerPath>,
{
    match outcome {
        Ok(Ok((meta, services, conn, rtt_ms))) => {
            // `rtt_ms` is threaded through, never recomputed — see #123 above.
            let path = settle(conn).await;
            (true, meta, services, path, Some(rtt_ms))
        }
        _ => (
            false,
            String::new(),
            Vec::new(),
            mcpmesh_local_api::PeerPath::Unknown,
            None,
        ),
    }
}

/// Extract the optional app metadata from a pong frame, applying the receive cap (#40): a
/// missing / non-string / over-`APP_METADATA_MAX_BYTES` value yields empty. Control-character
/// hygiene is applied at RENDER time (`--json` carries it raw, JSON-escaped).
/// Extract the caller-admitted service names from a pong (#52) — a JSON array of strings, or
/// empty for a peer that shares nothing / an older peer without the field. Never panics on
/// hostile shapes.
fn pong_services(pong: &serde_json::Value) -> Vec<String> {
    pong.get("services")
        .and_then(|s| s.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn pong_meta(pong: &serde_json::Value) -> String {
    pong.get("meta")
        .and_then(|m| m.as_str())
        .filter(|s| s.len() <= crate::roster::presence::APP_METADATA_MAX_BYTES)
        .unwrap_or_default()
        .to_string()
}

/// Build the `status` reachability list from the probe cache, and fire a NON-BLOCKING background
/// refresh for any paired peer whose cache entry is missing or older than [`REACH_TTL_SECS`].
/// NEVER blocks the caller on a probe: it returns the current cached view immediately and each
/// refresh runs as its own spawned task (parallel probes, no join helper / new dependency needed —
/// each `probe_peer` writes its own cache entry, read by the NEXT call).
///
/// Surface discipline: the cache is keyed by endpoint-id INTERNALLY, but every returned
/// [`mcpmesh_local_api::PeerReachability`] carries only the peer's NICKNAME — never the endpoint-id.
/// The services `identity` is currently admitted to on THIS node (#52) — the discovery answer
/// the ping pong carries. Computes the caller's flat principal set (the SAME
/// `mcpmesh_local_api::principal_set` admission uses) and returns every persistent OR ephemeral
/// service whose `allow` names one of those principals. ONLY the caller's own admitted services
/// — never the full registry. A best-effort config read (an unreadable config → empty).
pub(crate) fn caller_admitted_services(
    mesh: &Arc<MeshState>,
    identity: &mcpmesh_net::PeerIdentity,
) -> Vec<String> {
    use std::collections::HashSet;
    let eid = identity.endpoint.principal();
    let principals: HashSet<&str> =
        mcpmesh_local_api::principal_set(Some(&eid), identity.user_id.as_deref(), &identity.groups)
            .into_iter()
            .collect();
    let admits = |allow: &[String]| allow.iter().any(|a| principals.contains(a.as_str()));

    // #100: answer from the LIVE registry — the same `Services` the accept path authorizes from.
    // Reading `config.toml` + the ephemeral map instead meant a hand-added service that had not
    // been reloaded was reported as usable and then refused, which a caller cannot distinguish
    // from a network failure. The registry is already keyed by name with the overlay having won at
    // build time, so no merge and no dedup is needed here.
    let mut out: Vec<String> = mesh
        .services
        .get()
        .iter()
        .filter(|(_, entry)| admits(&entry.allow))
        .map(|(name, _)| name.clone())
        .collect();
    out.sort();
    out
}

/// Probe `endpoint_id` only if the cache has nothing fresher than [`REACH_TTL_SECS`] (#89).
///
/// `probe_peer` is unconditional, which made `peer_services` the one caller with no TTL: a client
/// polling it faster than the ping limiter's sustained refill would be REFUSED, and a refusal is
/// reported as `reachable: false` and written to the cache. That turns a rate limit into a fresh
/// false "peer is offline" for the next [`REACH_TTL_SECS`] — a healthy, paired peer reported down,
/// which is worse than the flood the limiter exists to stop. Reusing a fresh entry keeps the verb
/// off the limiter entirely under any realistic poll rate, so the two cannot fight.
///
/// This trades the docstring's "a fresh probe" for "no staler than `status` would report", which is
/// the same freshness contract every other consumer of this cache already gets.
pub(crate) async fn probe_peer_cached(mesh: &Arc<MeshState>, endpoint_id: [u8; 32]) -> ReachEntry {
    let fresh = {
        let cache = mesh
            .reachability
            .lock()
            .expect("reachability lock not poisoned");
        cache.get(&endpoint_id).and_then(|e| {
            let age = (epoch_now_i64() - e.probed_at).max(0);
            // #215 review: a REACHABLE row must also carry a pong, because this verb's answer IS
            // the pong's `services`. A live session's path watcher seeds a reachable row with none.
            // An unreachable row has no services to be wrong about, so it keeps the plain TTL (#89).
            let has_payload = !e.reachable || e.pong_at.is_some();
            (age <= REACH_TTL_SECS && has_payload).then(|| e.clone())
        })
    };
    match fresh {
        Some(e) => e,
        None => probe_peer(mesh, endpoint_id).await,
    }
}

pub fn reachability_of(mesh: &Arc<MeshState>) -> Vec<mcpmesh_local_api::PeerReachability> {
    let now = epoch_now_i64();
    // (nickname, endpoint_id) for every paired peer — reuse the allowlist store's peer scan
    // (fail-open: a corrupt row is skipped, not fatal). The store IS the paired-peer set.
    let peers: Vec<(String, [u8; 32])> = mesh
        .store
        .list()
        .unwrap_or_default()
        .into_iter()
        .map(|e| (e.nickname, e.endpoint_id))
        .collect();
    let cache = mesh
        .reachability
        .lock()
        .expect("reachability lock not poisoned")
        .clone();
    let mut stale: Vec<[u8; 32]> = Vec::new();
    let mut out = Vec::with_capacity(peers.len());
    for (nickname, eid) in peers {
        match cache.get(&eid) {
            Some(e) => {
                let age = (now - e.probed_at).max(0);
                if age > REACH_TTL_SECS {
                    stale.push(eid);
                }
                out.push(reachability_row(nickname, eid, Some(e), Some(age as u64)));
            }
            None => {
                stale.push(eid);
                // Never probed → `age_secs: None`, which a consumer renders as "checking…".
                out.push(reachability_row(nickname, eid, None, None));
            }
        }
    }
    // #176: ONE background refresh per peer at a time. The v1 note here called the missing dedup a
    // bounded tradeoff because the overlap self-limits once a result is cached — true of the probe
    // COUNT, not of the damage. A caller polling `status` faster than `PROBE_TIMEOUT` spawned a
    // fresh dial every poll, and that contention is what made the last-started probe (whose verdict
    // the ordering rule used to prefer) the one most likely to time out. `contradicted_by` fixes
    // the verdict; this removes the storm that produced it.
    //
    // Reads are unaffected: a peer already being refreshed still gets its cached row in `out`, just
    // no second dial. `probe_peer` itself stays unconditional — `peer_services` and the tests call
    // it directly and mean it.
    for (eid, guard) in claim_refreshes(mesh, stale) {
        let mesh = mesh.clone();
        tokio::spawn(async move {
            let _guard = guard; // released on completion, panic, or cancellation
            probe_peer(&mesh, eid).await;
        });
    }
    out
}

/// Which of `stale` this call may refresh (#176), each paired with the claim that reserves it.
///
/// Split from the spawn loop so the dedup decision is testable without a runtime and without a
/// timing assertion: the second call over the same peer returns an empty list, deterministically,
/// for as long as the first call's guards are alive. The loop above then holds nothing but a
/// `spawn`.
fn claim_refreshes(mesh: &Arc<MeshState>, stale: Vec<[u8; 32]>) -> Vec<([u8; 32], InFlight)> {
    stale
        .into_iter()
        .filter_map(|eid| InFlight::claim(mesh, eid).map(|g| (eid, g)))
        .collect()
}

/// A claim on "this peer is being refreshed" (#176), released on drop.
///
/// A drop guard rather than a `remove` at the end of the spawned task: a probe that panics, or a
/// task dropped when the runtime shuts down, would otherwise leave the peer marked in-flight
/// forever and never refresh it again. That failure is silent and permanent, which is a strictly
/// worse bug than the duplicate dials this exists to prevent.
struct InFlight {
    mesh: Arc<MeshState>,
    endpoint_id: [u8; 32],
}

impl InFlight {
    /// `Some` if no refresh for `endpoint_id` was already running — the claim. `None` means one is.
    fn claim(mesh: &Arc<MeshState>, endpoint_id: [u8; 32]) -> Option<Self> {
        mesh.probes_inflight
            .lock()
            .expect("probes_inflight lock not poisoned")
            .insert(endpoint_id)
            .then(|| Self {
                mesh: mesh.clone(),
                endpoint_id,
            })
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.mesh
            .probes_inflight
            .lock()
            .expect("probes_inflight lock not poisoned")
            .remove(&self.endpoint_id);
    }
}

#[cfg(test)]
mod tests {
    use super::pong_meta;
    use super::{
        AppFrames, ObservedEvent, ReachEntry, WindowEvent, WindowReading, WindowSource,
        classify_paths, classify_window, combine_windows, contradicted_by, is_transition,
        measure_window, sanitize_relay_url, supersedes,
    };
    use crate::roster::presence::APP_METADATA_MAX_BYTES;
    use mcpmesh_local_api::PeerPath;

    fn entry(reachable: bool, rtt_ms: Option<u64>) -> ReachEntry {
        ReachEntry {
            reachable,
            rtt_ms,
            probed_at: 1_700_000_000,
            meta: String::new(),
            services: Vec::new(),
            pong_at: None,
            seq: 0,
            observed: 0,
            path: mcpmesh_local_api::PeerPath::Unknown,
        }
    }

    fn relay() -> PeerPath {
        PeerPath::Relay {
            url: Some("https://relay.example".into()),
        }
    }

    /// #213: the structural rule. A selected path wins; with none selected, ONE open path is the
    /// only place the bytes can go and is reported as such; several unselected paths are
    /// ambiguous and stay `Unknown`.
    #[test]
    fn a_single_unselected_path_is_where_the_bytes_go() {
        // The measured #213 shape: the only open path is direct and `is_selected()` is false.
        assert_eq!(
            classify_paths([(PeerPath::Direct, false)]),
            PeerPath::Direct
        );
        // Symmetric for a relay: a relayed connection must never hide behind Unknown either.
        assert_eq!(classify_paths([(relay(), false)]), relay());
        // Several open, none selected: noq may send on any of them — never a guess.
        assert_eq!(
            classify_paths([(PeerPath::Direct, false), (relay(), false)]),
            PeerPath::Unknown,
            "two unselected paths must NOT be resolved to either"
        );
        // A selected path wins regardless of how many are open.
        assert_eq!(
            classify_paths([(PeerPath::Direct, false), (relay(), true)]),
            relay()
        );
        assert_eq!(
            classify_paths([(relay(), false), (PeerPath::Direct, true)]),
            PeerPath::Direct
        );
        // No paths (teardown snapshot) and an unmodelled transport stay Unknown.
        assert_eq!(classify_paths([]), PeerPath::Unknown);
        assert_eq!(
            classify_paths([(PeerPath::Unknown, false)]),
            PeerPath::Unknown
        );
    }

    fn ids() -> (
        iroh::endpoint::PathId,
        iroh::endpoint::PathId,
        iroh::endpoint::PathId,
    ) {
        use iroh::endpoint::PathId;
        (PathId::ZERO, PathId::from(1u32), PathId::from(2u32))
    }

    /// #213: the measured rule on sampled paths alone. The path whose APPLICATION frames advanced
    /// carried the data; a relay path moving is decisive; nothing moving defers to the structural
    /// reading.
    #[test]
    fn the_path_that_moved_application_frames_carried_the_data() {
        let (p0, p1, _) = ids();
        let before = [
            (p0, PeerPath::Direct, AppFrames(10)),
            (p1, relay(), AppFrames(4)),
        ];

        // Only the direct path moved: Direct, even though the structural reading says Unknown
        // (the #213 accept side with a standby relay path and nothing selected).
        let after = [
            (p0, PeerPath::Direct, AppFrames(12)),
            (p1, relay(), AppFrames(4)),
        ];
        assert_eq!(
            classify_window(&before, &after, &[], PeerPath::Unknown),
            WindowReading::Moved(PeerPath::Direct)
        );

        // The relay moved too: Relay. One frame over a relay is enough to falsify "private".
        let after = [
            (p0, PeerPath::Direct, AppFrames(12)),
            (p1, relay(), AppFrames(5)),
        ];
        assert_eq!(
            classify_window(&before, &after, &[], PeerPath::Direct),
            WindowReading::Moved(relay())
        );

        // Nothing moved (idle): the fallback, whatever it is — including Unknown.
        assert_eq!(
            classify_window(&before, &before, &[], PeerPath::Unknown),
            WindowReading::Idle(PeerPath::Unknown)
        );
        assert_eq!(
            classify_window(&before, &before, &[], PeerPath::Direct),
            WindowReading::Idle(PeerPath::Direct)
        );
    }

    /// #213 review round 3, MUST-FIX 1: a relay path that carried frames and CLOSED inside the
    /// window is attributed from its own `Closed { last_stats }` event. The aggregate cross-check
    /// this replaces read this exact state as Direct in ~1 window in 4 under load, because
    /// per-path and connection stats are read under separate locks.
    #[test]
    fn a_relay_path_that_closed_mid_window_is_attributed_from_its_closed_event() {
        let (p0, p1, _) = ids();
        let before = [
            (p0, PeerPath::Direct, AppFrames(1000)),
            (p1, relay(), AppFrames(4)),
        ];
        let after = [(p0, PeerPath::Direct, AppFrames(1100))];
        let closed = [WindowEvent::Closed {
            id: p1,
            kind: relay(),
            frames: AppFrames(60),
        }];
        assert_eq!(
            classify_window(&before, &after, &closed, PeerPath::Direct),
            WindowReading::Moved(relay()),
            "56 frames over a relay that closed mid-window must read Relay, never Direct"
        );
        // The same close with NO movement since baseline is not movement.
        let idle_close = [WindowEvent::Closed {
            id: p1,
            kind: relay(),
            frames: AppFrames(4),
        }];
        assert_eq!(
            classify_window(&before, &after, &idle_close, PeerPath::Unknown),
            WindowReading::Moved(PeerPath::Direct)
        );
    }

    /// #213 review round 3: a lost event means the window cannot see everything that happened.
    #[test]
    fn lagged_path_events_make_the_window_unobservable() {
        let (p0, _, _) = ids();
        let before = [(p0, PeerPath::Direct, AppFrames(10))];
        let after = [(p0, PeerPath::Direct, AppFrames(12))];
        assert_eq!(
            classify_window(&before, &after, &[WindowEvent::Lagged], PeerPath::Direct),
            WindowReading::Unobservable,
            "a Lagged window must never read Direct"
        );
    }

    /// #213 review round 3, SHOULD-FIX 3: a baseline-less path that moved says what IT carried,
    /// and nothing about paths that disappeared. A relay path gone from `after` with no `Closed`
    /// event is unaccounted for.
    #[test]
    fn a_path_that_vanished_without_a_closed_event_is_unobservable() {
        let (p0, _, p2) = ids();
        let before = [(p0, relay(), AppFrames(4))];
        let after = [(p2, PeerPath::Direct, AppFrames(500))];
        assert_eq!(
            classify_window(&before, &after, &[], PeerPath::Direct),
            WindowReading::Unobservable,
            "no evidence the vanished relay did not move: not Direct"
        );
    }

    /// #213 review round 2: a path with NO baseline opened inside the window, and its whole count
    /// is movement. Skipping it answered Direct while 99 frames went over a relay.
    #[test]
    fn a_relay_path_that_opened_mid_window_and_carried_frames_is_relay() {
        let (p0, _, p2) = ids();
        let before = [(p0, PeerPath::Direct, AppFrames(10))];
        let after = [
            (p0, PeerPath::Direct, AppFrames(12)),
            (p2, relay(), AppFrames(99)),
        ];
        assert_eq!(
            classify_window(&before, &after, &[], PeerPath::Direct),
            WindowReading::Moved(relay())
        );
        // It also counts when the path opened AND closed inside the window.
        let after = [(p0, PeerPath::Direct, AppFrames(12))];
        let closed = [
            WindowEvent::Opened { id: p2 },
            WindowEvent::Closed {
                id: p2,
                kind: relay(),
                frames: AppFrames(99),
            },
        ];
        assert_eq!(
            classify_window(&before, &after, &closed, PeerPath::Direct),
            WindowReading::Moved(relay())
        );
        // A baseline-less path that carried NOTHING is not movement.
        let after = [
            (p0, PeerPath::Direct, AppFrames(12)),
            (p2, relay(), AppFrames(0)),
        ];
        assert_eq!(
            classify_window(&before, &after, &[], PeerPath::Unknown),
            WindowReading::Moved(PeerPath::Direct)
        );
    }

    /// #213 review round 2: a moving path of a transport mcpmesh does not model is not Direct.
    #[test]
    fn a_moving_unmodelled_path_is_unobservable() {
        let (p0, p1, _) = ids();
        let before = [
            (p0, PeerPath::Direct, AppFrames(10)),
            (p1, PeerPath::Unknown, AppFrames(0)),
        ];
        let after = [
            (p0, PeerPath::Direct, AppFrames(12)),
            (p1, PeerPath::Unknown, AppFrames(3)),
        ];
        assert_eq!(
            classify_window(&before, &after, &[], PeerPath::Direct),
            WindowReading::Unobservable
        );
    }

    /// #213 round 4, MUST-FIX 1: iroh removes a path from the list, reads its stats, THEN sends
    /// `Closed` (`record_abandoned`). A subscription and baseline that land in that gap see a
    /// `Closed` for a path with no baseline and no `Opened` — a close from BEFORE the window.
    /// Counting its lifetime total read 1000 old frames as live Direct traffic.
    #[test]
    fn a_close_from_before_the_window_is_not_traffic() {
        let (p0, p1, _) = ids();
        let paths = [(p1, relay(), AppFrames(4))];
        let stale_close = [WindowEvent::Closed {
            id: p0,
            kind: PeerPath::Direct,
            frames: AppFrames(1000),
        }];
        assert_eq!(
            classify_window(&paths, &paths, &stale_close, relay()),
            WindowReading::Idle(relay()),
            "a pre-window close must not read as Direct traffic"
        );
    }

    /// #213 round 5: the main success case for a path that opened in the window — it is still in
    /// `after`, its `Opened` was seen, and its whole count is movement of its own kind. A rule that
    /// treated every `Opened` path as unaccounted for survived the round-4 suite 27/27.
    #[test]
    fn a_path_opened_in_the_window_and_still_open_is_attributed() {
        let (p0, _, p2) = ids();
        let before = [(p0, PeerPath::Direct, AppFrames(10))];
        let opened = [WindowEvent::Opened { id: p2 }];

        let after = [
            (p0, PeerPath::Direct, AppFrames(10)),
            (p2, PeerPath::Direct, AppFrames(5)),
        ];
        assert_eq!(
            classify_window(&before, &after, &opened, PeerPath::Unknown),
            WindowReading::Moved(PeerPath::Direct),
            "a direct path that opened and carried frames is Direct"
        );

        let after = [
            (p0, PeerPath::Direct, AppFrames(10)),
            (p2, relay(), AppFrames(5)),
        ];
        assert_eq!(
            classify_window(&before, &after, &opened, PeerPath::Direct),
            WindowReading::Moved(relay()),
            "a relay path that opened and carried frames is Relay"
        );
    }

    /// #213 round 4, MUST-FIX 2: a path that OPENED in the window and is neither in `after` nor
    /// `Closed` carried an unknown number of frames — its `Closed` is still in flight.
    #[test]
    fn a_path_opened_in_the_window_that_disappeared_is_unobservable() {
        let (p0, _, p2) = ids();
        let before = [(p0, PeerPath::Direct, AppFrames(10))];
        let after = [(p0, PeerPath::Direct, AppFrames(12))];
        assert_eq!(
            classify_window(
                &before,
                &after,
                &[WindowEvent::Opened { id: p2 }],
                PeerPath::Direct
            ),
            WindowReading::Unobservable,
            "an opened path that vanished may have been a relay carrying frames"
        );
    }

    /// #213 round 4, SHOULD-FIX 3: the policy for every event kind. `Selected` is the only one
    /// dropped; a lost or unrecognised event is never silently ignored.
    ///
    /// Tested on the projection, not on iroh's `PathEvent`: iroh marks every variant
    /// `#[non_exhaustive]`, so no test outside iroh can construct one (E0639, checked). The
    /// `From<PathEvent>` projection is policy-free and is the stated untested residual.
    #[test]
    fn every_path_event_kind_has_an_explicit_policy() {
        let (p0, _, _) = ids();
        assert_eq!(
            WindowEvent::from_observed(ObservedEvent::Opened { id: p0 }),
            Some(WindowEvent::Opened { id: p0 })
        );
        let mut stats = iroh::endpoint::PathStats::default();
        stats.frame_tx.datagram = 7;
        assert_eq!(
            WindowEvent::from_observed(ObservedEvent::Closed {
                id: p0,
                remote_addr: iroh::TransportAddr::Ip("127.0.0.1:1".parse().unwrap()),
                last_stats: Box::new(stats),
            }),
            Some(WindowEvent::Closed {
                id: p0,
                kind: PeerPath::Direct,
                frames: AppFrames(7),
            })
        );
        assert_eq!(WindowEvent::from_observed(ObservedEvent::Selected), None);
        assert_eq!(
            WindowEvent::from_observed(ObservedEvent::Lagged),
            Some(WindowEvent::Lagged),
            "a lost event must reach classification"
        );
        assert_eq!(
            WindowEvent::from_observed(ObservedEvent::Unrecognised),
            Some(WindowEvent::Unrecognised)
        );
        // And an unrecognised event makes the window unobservable.
        let paths = [(p0, PeerPath::Direct, AppFrames(10))];
        let moved = [(p0, PeerPath::Direct, AppFrames(12))];
        assert_eq!(
            classify_window(
                &paths,
                &moved,
                &[WindowEvent::Unrecognised],
                PeerPath::Direct
            ),
            WindowReading::Unobservable
        );
    }

    /// A scripted [`WindowSource`]. Events published before `subscribe()` are dropped, exactly as a
    /// broadcast drops them; each `sample()` returns the next scripted path list and THEN publishes
    /// that step's events — the "path changed right after it was sampled" race.
    #[derive(Clone, Default)]
    struct Script(std::sync::Arc<std::sync::Mutex<ScriptState>>);

    #[derive(Default)]
    struct ScriptState {
        subscribed: bool,
        queue: std::collections::VecDeque<ObservedEvent>,
        steps: std::collections::VecDeque<(Vec<super::PathSample>, Vec<ObservedEvent>)>,
        stream_ends: bool,
    }

    impl Script {
        fn new(steps: Vec<(Vec<super::PathSample>, Vec<ObservedEvent>)>) -> Self {
            let script = Self::default();
            script.0.lock().unwrap().steps = steps.into();
            script
        }
    }

    struct ScriptStream(Script);

    impl n0_future::Stream for ScriptStream {
        type Item = ObservedEvent;
        fn poll_next(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<ObservedEvent>> {
            let mut st = self.0.0.lock().unwrap();
            match st.queue.pop_front() {
                Some(event) => std::task::Poll::Ready(Some(event)),
                None if st.stream_ends => std::task::Poll::Ready(None),
                // No waker registered: the window's deadline is what wakes the drain loop, and
                // every scripted event is queued synchronously inside `sample()`.
                None => std::task::Poll::Pending,
            }
        }
    }

    impl WindowSource for Script {
        fn is_closed(&self) -> bool {
            false
        }
        fn subscribe(&self) -> n0_future::boxed::BoxStream<ObservedEvent> {
            self.0.lock().unwrap().subscribed = true;
            Box::pin(ScriptStream(self.clone()))
        }
        fn sample(&self) -> Vec<super::PathSample> {
            let mut st = self.0.lock().unwrap();
            let (paths, publish) = st.steps.pop_front().expect("scripted sample");
            if st.subscribed {
                st.queue.extend(publish);
            }
            paths
        }
        fn fallback(&self) -> PeerPath {
            PeerPath::Unknown
        }
    }

    fn relay_closed(id: iroh::endpoint::PathId, frames: u64) -> ObservedEvent {
        let mut last_stats = iroh::endpoint::PathStats::default();
        last_stats.frame_tx.datagram = frames;
        ObservedEvent::Closed {
            id,
            remote_addr: iroh::TransportAddr::Relay(
                "https://relay.example".parse().expect("relay url"),
            ),
            last_stats: Box::new(last_stats),
        }
    }

    /// #213 round 4: the relay path closes right AFTER the baseline sample. Subscribing first is
    /// what makes its `Closed` visible; subscribing after the baseline loses it and the window can
    /// only say Unobservable.
    #[tokio::test(start_paused = true)]
    async fn the_subscription_is_taken_before_the_baseline() {
        let (p0, p1, _) = ids();
        let script = Script::new(vec![
            (
                vec![
                    (p0, PeerPath::Direct, AppFrames(10)),
                    (p1, relay(), AppFrames(4)),
                ],
                vec![relay_closed(p1, 60)],
            ),
            (vec![(p0, PeerPath::Direct, AppFrames(12))], vec![]),
        ]);
        let reading = measure_window(&script, std::time::Duration::from_millis(250)).await;
        assert!(
            matches!(reading, WindowReading::Moved(PeerPath::Relay { .. })),
            "a relay close right after the baseline must be seen: {reading:?}"
        );
    }

    /// #213 round 4: the relay path's `Closed` is sent right AFTER the final sample
    /// (`record_abandoned` removes, then sends). Only the final drain catches it.
    #[tokio::test(start_paused = true)]
    async fn events_queued_after_the_final_sample_are_drained() {
        let (p0, p1, _) = ids();
        let script = Script::new(vec![
            (
                vec![
                    (p0, PeerPath::Direct, AppFrames(10)),
                    (p1, relay(), AppFrames(4)),
                ],
                vec![],
            ),
            (
                vec![(p0, PeerPath::Direct, AppFrames(12))],
                vec![relay_closed(p1, 60)],
            ),
        ]);
        let reading = measure_window(&script, std::time::Duration::from_millis(250)).await;
        assert!(
            matches!(reading, WindowReading::Moved(PeerPath::Relay { .. })),
            "a Closed queued just after the final sample must be drained: {reading:?}"
        );
    }

    /// A [`WindowSource`] whose events travel through a REAL `tokio::sync::broadcast`, as iroh's
    /// `PathEventStream` does (`BroadcastStream` over `Receiver::recv`, which is cooperative).
    /// A step can spend the task's whole coop budget inside `sample()` before publishing.
    /// One scripted step: the path list `sample()` returns, the events it publishes afterwards,
    /// and whether it spends the coop budget first.
    type BroadcastStep = (Vec<super::PathSample>, Vec<ObservedEvent>, bool);

    struct BroadcastSource {
        tx: tokio::sync::broadcast::Sender<ObservedEvent>,
        steps: std::sync::Mutex<std::collections::VecDeque<BroadcastStep>>,
        /// How many ready polls the burn managed before the budget ran out.
        burned: std::sync::atomic::AtomicUsize,
    }

    impl WindowSource for BroadcastSource {
        fn is_closed(&self) -> bool {
            false
        }
        fn subscribe(&self) -> n0_future::boxed::BoxStream<ObservedEvent> {
            Box::pin(n0_future::stream::unfold(
                self.tx.subscribe(),
                |mut rx| async move {
                    match rx.recv().await {
                        Ok(event) => Some((event, rx)),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            Some((ObservedEvent::Lagged, rx))
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
                    }
                },
            ))
        }
        fn sample(&self) -> Vec<super::PathSample> {
            let (paths, publish, burn) = self.steps.lock().unwrap().pop_front().expect("step");
            if burn {
                // Spend the coop budget: every READY poll of a cooperative `recv` costs one unit,
                // and once the budget is gone the poll returns Pending with messages still queued.
                const SIDE: usize = 1000;
                let (side_tx, mut side_rx) = tokio::sync::broadcast::channel::<u8>(SIDE);
                for _ in 0..SIDE {
                    side_tx.send(0).unwrap();
                }
                let mut polls = 0;
                while let Some(Ok(_)) = n0_future::future::now_or_never(side_rx.recv()) {
                    polls += 1;
                }
                self.burned
                    .store(polls, std::sync::atomic::Ordering::Relaxed);
            }
            for event in publish {
                self.tx.send(event).expect("subscribed");
            }
            paths
        }
        fn fallback(&self) -> PeerPath {
            PeerPath::Unknown
        }
    }

    /// #213 round 5: the final drain must not stop because the task's coop budget is spent. A
    /// relay path opens, carries frames and closes; its `Opened` and `Closed` are queued right after
    /// the final sample, on a task with no budget left. A budget-limited single poll reads nothing,
    /// and the window answers Direct over a relay that carried 60 frames.
    #[tokio::test(start_paused = true)]
    async fn the_final_drain_is_not_cut_short_by_the_coop_budget() {
        let (p0, _, p2) = ids();
        let (tx, _keep) = tokio::sync::broadcast::channel(16);
        let source = BroadcastSource {
            tx,
            steps: std::sync::Mutex::new(
                vec![
                    (vec![(p0, PeerPath::Direct, AppFrames(10))], vec![], false),
                    (
                        vec![(p0, PeerPath::Direct, AppFrames(12))],
                        vec![ObservedEvent::Opened { id: p2 }, relay_closed(p2, 60)],
                        true,
                    ),
                ]
                .into(),
            ),
            burned: std::sync::atomic::AtomicUsize::new(0),
        };
        let reading = measure_window(&source, std::time::Duration::from_millis(250)).await;
        let burned = source.burned.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            burned < 1000,
            "fixture: the burn must actually exhaust the coop budget (read {burned} of 1000), or \
             this test measures nothing"
        );
        assert!(
            matches!(reading, WindowReading::Moved(PeerPath::Relay { .. })),
            "events queued on a budget-exhausted task must still be drained: {reading:?}"
        );
    }

    /// #213 round 4, NIT 5: the event stream ending while the window is still open is not a
    /// shorter window — the rest of it went unobserved.
    #[tokio::test(start_paused = true)]
    async fn an_event_stream_that_ends_early_is_unobservable() {
        let (p0, _, _) = ids();
        let script = Script::new(vec![
            (vec![(p0, PeerPath::Direct, AppFrames(10))], vec![]),
            (vec![(p0, PeerPath::Direct, AppFrames(12))], vec![]),
        ]);
        script.0.lock().unwrap().stream_ends = true;
        assert_eq!(
            measure_window(&script, std::time::Duration::from_millis(250)).await,
            WindowReading::Unobservable
        );
        assert_eq!(
            script.0.lock().unwrap().steps.len(),
            1,
            "the window must stop when the stream ends, not carry on to a final sample"
        );
    }

    /// Run `combine_windows` over a script; return the answer and how many windows it used.
    async fn run_script(windows: usize, script: &[WindowReading]) -> (PeerPath, usize) {
        let mut used = 0usize;
        let path = combine_windows(windows, || {
            let reading = script[used].clone();
            used += 1;
            async move { reading }
        })
        .await;
        (path, used)
    }

    /// #213 review round 3, SHOULD-FIX 4 + NIT: the retry policy, over scripted readings.
    #[tokio::test]
    async fn only_an_idle_unknown_window_is_retried() {
        let idle_unknown = WindowReading::Idle(PeerPath::Unknown);

        // An idle-Unknown window is retried, and a later window that sees traffic answers.
        // (Fails with PATH_MEASURE_WINDOWS-as-1 behaviour: one window, then Unknown.)
        assert_eq!(
            run_script(
                3,
                &[idle_unknown.clone(), WindowReading::Moved(PeerPath::Direct)]
            )
            .await,
            (PeerPath::Direct, 2)
        );

        // A Direct window answers at once — it is never traded for a later reading.
        assert_eq!(
            run_script(
                3,
                &[
                    WindowReading::Moved(PeerPath::Direct),
                    WindowReading::Moved(relay())
                ]
            )
            .await,
            (PeerPath::Direct, 1),
            "Moved(Direct) must not be retried"
        );
        // Nor is Relay.
        assert_eq!(
            run_script(
                3,
                &[
                    WindowReading::Moved(relay()),
                    WindowReading::Moved(PeerPath::Direct)
                ]
            )
            .await,
            (relay(), 1)
        );
        // Nor is a known idle reading.
        assert_eq!(
            run_script(
                3,
                &[
                    WindowReading::Idle(PeerPath::Direct),
                    WindowReading::Moved(relay())
                ]
            )
            .await,
            (PeerPath::Direct, 1)
        );

        // An unobservable window may have carried relayed frames: the call is Unknown, and a later
        // Direct window must never be reached.
        assert_eq!(
            run_script(
                3,
                &[
                    WindowReading::Unobservable,
                    WindowReading::Moved(PeerPath::Direct)
                ]
            )
            .await,
            (PeerPath::Unknown, 1),
            "no Direct after an unobservable window"
        );

        // Bounded: three idle-Unknown windows and the call gives up, never reaching a fourth.
        assert_eq!(
            run_script(
                3,
                &[
                    idle_unknown.clone(),
                    idle_unknown.clone(),
                    idle_unknown,
                    WindowReading::Moved(PeerPath::Direct)
                ]
            )
            .await,
            (PeerPath::Unknown, 3)
        );

        // The production bound (> 1) is a compile-time assertion beside PATH_MEASURE_WINDOWS.
    }

    /// #213: only STREAM and DATAGRAM frames count as application data. iroh keeps a standby
    /// relay path alive with PINGs and ACKs, so counting packets or bytes would report every
    /// hole-punched connection as relayed — the exact #64 regression, measured a second way.
    ///
    /// EVERY other frame counter is seeded non-zero (review: seeding only ping/acks let a
    /// `+ path_acks` mutation survive), then the two application counters are zeroed.
    #[test]
    fn keepalives_on_a_standby_relay_path_are_not_application_data() {
        fn every_counter(v: u64) -> iroh::endpoint::FrameStats {
            let mut f = iroh::endpoint::FrameStats::default();
            f.acks = v;
            f.path_acks = v;
            f.ack_frequency = v;
            f.crypto = v;
            f.connection_close = v;
            f.data_blocked = v;
            f.datagram = v;
            f.handshake_done = 1;
            f.immediate_ack = v;
            f.max_data = v;
            f.max_stream_data = v;
            f.max_streams_bidi = v;
            f.max_streams_uni = v;
            f.new_connection_id = v;
            f.path_new_connection_id = v;
            f.new_token = v;
            f.path_challenge = v;
            f.path_response = v;
            f.ping = v;
            f.reset_stream = v;
            f.retire_connection_id = v;
            f.path_retire_connection_id = v;
            f.stream_data_blocked = v;
            f.streams_blocked_bidi = v;
            f.streams_blocked_uni = v;
            f.stop_sending = v;
            f.stream = v;
            f.observed_addr = v;
            f.path_abandon = v;
            f.path_status_available = v;
            f.path_status_backup = v;
            f.max_path_id = v;
            f.paths_blocked = v;
            f.path_cids_blocked = v;
            f.add_address = v;
            f.reach_out = v;
            f.remove_address = v;
            f
        }
        let mut idle = iroh::endpoint::PathStats::default();
        idle.frame_tx = every_counter(3);
        idle.frame_rx = every_counter(5);
        idle.frame_tx.stream = 0;
        idle.frame_tx.datagram = 0;
        idle.frame_rx.stream = 0;
        idle.frame_rx.datagram = 0;
        idle.udp_tx.bytes = 4096;
        idle.udp_tx.datagrams = 40;
        idle.udp_rx.bytes = 4096;
        idle.udp_rx.datagrams = 40;
        idle.lost_packets = 2;
        assert_eq!(
            AppFrames::of(&idle),
            AppFrames(0),
            "no non-application counter may leak into the reading"
        );

        let mut busy = idle;
        busy.frame_tx.stream = 3;
        busy.frame_rx.datagram = 2;
        assert_eq!(
            AppFrames::of(&busy),
            AppFrames(5),
            "stream + datagram, both directions"
        );
    }

    /// #128 AND #123, in one test, because they are the same coupling from two sides.
    ///
    /// The previous version of this test was VACUOUS: it built its own timeout over its own sleep
    /// and never touched production control flow, so re-nesting the settle inside `PROBE_TIMEOUT`
    /// — restoring #128 verbatim — left the whole suite green. The adversarial gate proved it.
    ///
    /// This drives the real seam on a paused clock. It fails on BOTH mutations:
    ///   - re-nesting classification inside the exchange deadline (#128)
    ///   - re-deriving `rtt_ms` from `started` after classification (#123)
    #[tokio::test(start_paused = true)]
    async fn classification_costs_neither_the_verdict_nor_the_rtt() {
        use mcpmesh_local_api::PeerPath;
        use std::time::Duration;

        let started = tokio::time::Instant::now();
        // A pong landing LATE but inside the exchange budget: this is the #128 peer.
        let late = super::PROBE_TIMEOUT - Duration::from_millis(200);
        tokio::time::sleep(late).await;
        let rtt_at_pong = started.elapsed().as_millis() as u64;

        // Classification then spends the full window, as a genuinely relayed peer does. Together
        // these exceed super::PROBE_TIMEOUT — which is exactly what used to flip `reachable` to false.
        let outcome: super::ExchangeOutcome<()> = Ok(Ok((
            "meta".to_string(),
            vec!["echo".to_string()],
            (),
            rtt_at_pong,
        )));
        let (reachable, _meta, _services, path, rtt_ms) = super::classify(outcome, |()| async {
            tokio::time::sleep(super::PATH_SETTLE).await;
            PeerPath::Relay { url: None }
        })
        .await;

        assert!(
            late + super::PATH_SETTLE > super::PROBE_TIMEOUT,
            "the fixture must exceed the OLD combined budget, or it proves nothing"
        );
        assert!(
            reachable,
            "a pong inside super::PROBE_TIMEOUT must survive classification — reporting a peer offline \
             while it is answering is #128, and it is wrong in the dangerous direction"
        );
        assert_eq!(path, PeerPath::Relay { url: None });
        assert_eq!(
            rtt_ms,
            Some(2800),
            "rtt_ms must be the pong stamp, NOT restamped after the settle window — restamping \
             is a one-line edit away and silently restores #123"
        );

        // An exchange that timed out reports unreachable with no route and no measurement.
        let timed_out: super::ExchangeOutcome<()> =
            tokio::time::timeout(Duration::ZERO, std::future::pending()).await;
        let (reachable, _m, _s, path, rtt_ms) =
            super::classify(timed_out, |()| async { PeerPath::Direct }).await;
        assert!(!reachable);
        assert_eq!(path, PeerPath::Unknown, "never a guessed route");
        assert_eq!(rtt_ms, None, "never a fabricated measurement");
    }

    /// #64/#110: the probe WAITS for hole-punching rather than classifying the instant the pong
    /// lands. A fresh dial starts on the relay, so a zero settle window reports `Relay` for every
    /// peer on a relay-enabled node — the bug #64 shipped and then fixed.
    ///
    /// This lives here, not in the e2e suite, because the e2e version could only observe it by
    /// racing a real hole-punch: that made it flaky on Windows and Linux CI (#110), and the
    /// reordering that de-flaked it stopped catching this mutation entirely. Time is driven by
    /// tokio's test clock, so it is deterministic and instant.
    #[tokio::test(start_paused = true)]
    async fn the_probe_waits_for_the_direct_path_instead_of_classifying_immediately() {
        use mcpmesh_local_api::PeerPath;
        use std::time::Duration;

        // A connection that is relayed for the first three polls, then hole-punches.
        let mut polls = 0;
        let path = super::settle(super::PATH_SETTLE, || {
            polls += 1;
            if polls > 3 {
                PeerPath::Direct
            } else {
                PeerPath::Relay { url: None }
            }
        })
        .await;
        assert_eq!(
            path,
            PeerPath::Direct,
            "the settle window must outlast a punch that takes a few polls — classifying on the \
             first look is what made Direct unreachable on every relay-enabled node"
        );
        assert_eq!(polls, 4, "it must stop polling as soon as Direct appears");

        // A ZEROED window is the mutation the e2e suite used to catch: it returns whatever the
        // very first look says, which for a fresh dial is always the relay.
        let mut polls = 0;
        let path = super::settle(Duration::ZERO, || {
            polls += 1;
            if polls > 3 {
                PeerPath::Direct
            } else {
                PeerPath::Relay { url: None }
            }
        })
        .await;
        assert_eq!(
            path,
            PeerPath::Relay { url: None },
            "with no settle window the relay answer wins — this assertion is what fails if \
             PATH_SETTLE is ever zeroed or the wait is removed"
        );

        // A genuinely relayed peer pays the whole window and truthfully reports Relay.
        let path = super::settle(super::PATH_SETTLE, || PeerPath::Relay { url: None }).await;
        assert_eq!(
            path,
            PeerPath::Relay { url: None },
            "a peer with no direct path must report Relay, not Unknown or Direct"
        );
    }

    /// #64 review: a relay URL reaching the wire must carry NO credential. Relay URLs are
    /// operator-supplied (`[network].relay_urls`, `set_relays`), so a `https://user:token@host/`
    /// would otherwise ship that token to every local-API client on every reachability row.
    #[test]
    fn sanitize_relay_url_drops_credentials_and_path() {
        let u = |s: &str| -> iroh::RelayUrl { s.parse().expect("relay url") };

        assert_eq!(
            sanitize_relay_url(&u("https://user:token@relay.internal/")),
            "https://relay.internal",
            "userinfo must never reach the wire"
        );
        assert_eq!(
            sanitize_relay_url(&u("https://relay.example:4433/some/path?x=1")),
            "https://relay.example:4433",
            "port is kept (it names the relay); path/query are not"
        );
        assert_eq!(
            sanitize_relay_url(&u("https://relay.example/")),
            "https://relay.example"
        );
        // A self-hosted relay on a LAN address still renders — the field names WHICH relay is in
        // use, and an operator who points at an IP has already chosen to expose it.
        assert_eq!(
            sanitize_relay_url(&u("http://192.168.1.5:4433/")),
            "http://192.168.1.5:4433"
        );
    }

    /// #58 review: a SLOW probe that started earlier must not overwrite a FAST one that started
    /// later. Probes of one peer overlap routinely — `reachability_of` spawns a refresh per stale
    /// peer and both `status` and `subscribe` call it — and they complete out of order. Without
    /// this the timed-out older result wins on arrival, poisoning the cache for a full TTL AND
    /// pushing a false "went offline" for a peer that is up.
    #[test]
    fn a_probe_never_overwrites_a_newer_one() {
        let mut newer = entry(true, Some(50));
        newer.seq = 7;

        assert!(
            !supersedes(3, &newer),
            "an older probe (ticket 3) must not overwrite ticket 7's result"
        );
        assert!(
            supersedes(9, &newer),
            "a newer probe (ticket 9) must overwrite"
        );
        assert!(
            supersedes(7, &newer),
            "equal tickets cannot happen, but must not wedge the guard"
        );
    }

    /// #176: the mirror of the case above, which `supersedes` alone cannot express.
    ///
    /// `supersedes` orders by probe START, so the LAST-started probe always wins. Among
    /// overlapping probes that one is the most contended and therefore the likeliest to hit
    /// `PROBE_TIMEOUT` — so its `reachable: false` overwrote a pong that had already arrived, and
    /// the peer read offline for a full 20s TTL while demonstrably up.
    ///
    /// The rule is about EVIDENCE, not recency: a timeout says "no pong reached ME over my
    /// window"; a pong inside that same window says the peer was up. Every assertion below pins one
    /// conjunct, so dropping any of the three flips exactly one of them.
    #[test]
    fn a_timeout_never_overwrites_a_pong_that_landed_inside_its_window() {
        // Our probe started at ticket 5 and timed out.
        let ours = entry(false, None);

        // THE CASE. A pong committed at ticket 9 — after we started, so it landed inside our
        // window. Dropping `existing.observed > seq` (or comparing against `existing.seq`, which
        // is 0 here) makes this false and reinstates #176.
        let mut pong_inside = entry(true, Some(50));
        pong_inside.observed = 9;
        assert!(
            contradicted_by(&ours, 5, &pong_inside),
            "a pong observed at 9 is inside the window of a probe that started at 5 — our timeout \
             says nothing about a peer that answered someone else meanwhile"
        );

        // …and the peer is NOT wedged reachable. A probe that starts AFTER that pong carries a
        // higher ticket, so it commits normally — which is how a genuinely down peer is reported
        // down, one probe cycle later rather than never.
        assert!(
            !contradicted_by(&ours, 11, &pong_inside),
            "a probe that started after the last pong must be free to commit reachable: false"
        );

        // A REACHABLE result is never refused. Without the `!ours.reachable` conjunct this would
        // block a fresh pong from landing over an older one.
        let ours_ok = entry(true, Some(12));
        assert!(
            !contradicted_by(&ours_ok, 5, &pong_inside),
            "the guard is about timeouts; a pong must always be free to commit"
        );

        // Nor does an UNREACHABLE cached entry protect anything — there is no positive evidence to
        // contradict. Without the `existing.reachable` conjunct a down peer's row would freeze.
        let mut cached_down = entry(false, None);
        cached_down.observed = 9;
        assert!(
            !contradicted_by(&ours, 5, &cached_down),
            "an unreachable cached row is not evidence the peer answered; ours must commit"
        );
    }

    /// #176 at the CALL SITE, not the helper: `probe_peer` must actually consult `contradicted_by`
    /// before it writes.
    ///
    /// Deterministic, with no timing assertion. The peer id is unroutable on a hermetic mesh, so
    /// the probe is guaranteed to produce `reachable: false`; the cache is seeded with a reachable
    /// row whose `observed` is above every ticket the probe can draw, which is exactly "a pong
    /// landed after this probe started". `seq: 0` keeps the #58 rule satisfied, so this test can
    /// only fail on the #176 arm.
    ///
    /// Deleting that match arm makes `probe_peer` return `reachable: false` here and overwrite the
    /// cache — the reported bug, caught.
    #[tokio::test(flavor = "multi_thread")]
    async fn probe_peer_does_not_commit_a_timeout_over_a_pong_from_its_own_window() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = crate::daemon::testutil::hermetic_mesh(cfg).await;
        let eid = [0x5Au8; 32];

        let mut pong = entry(true, Some(42));
        pong.seq = 0;
        pong.observed = u64::MAX; // observed after any ticket this probe can take
        mesh.reachability.lock().unwrap().insert(eid, pong);

        let got = super::probe_peer(&mesh, eid).await;
        assert!(
            got.reachable,
            "the unreachable probe must be DISCARDED and the surviving pong reported"
        );
        assert_eq!(
            got.rtt_ms,
            Some(42),
            "and it must be that pong's row, not a synthesised one"
        );
        assert!(
            mesh.reachability
                .lock()
                .unwrap()
                .get(&eid)
                .expect("row still cached")
                .reachable,
            "the CACHE must be left holding the pong — this is what pinned reachable:false for a \
             full TTL"
        );

        // The negative control, same call site: with the pong observed BEFORE this probe started,
        // the timeout is honest evidence and must commit. Without it the assertions above are also
        // satisfied by a `probe_peer` that never writes `reachable: false` at all.
        let mut stale_pong = entry(true, Some(42));
        stale_pong.seq = 0;
        stale_pong.observed = 0;
        mesh.reachability.lock().unwrap().insert(eid, stale_pong);
        let got = super::probe_peer(&mesh, eid).await;
        assert!(
            !got.reachable,
            "a peer that stopped answering must still be reported down — the guard only refuses \
             probes that were already in flight when the last pong landed"
        );
    }

    /// #176: `reachability_of` spawns ONE refresh per peer, not one per poll.
    ///
    /// The old v1 note called the missing dedup bounded because each probe self-limits once its
    /// result is cached. That bounds the probe COUNT and not the damage: a caller polling `status`
    /// faster than `PROBE_TIMEOUT` spawned a dial per poll, and the contention that produced is
    /// what made the last-started probe — the one the ordering rule preferred — the likeliest to
    /// time out.
    ///
    /// Asserted through `claim_refreshes`, the function the spawn loop calls, so this needs no
    /// runtime and no "wait and see if a probe started" timing assertion.
    #[tokio::test]
    async fn a_peer_already_being_refreshed_is_not_probed_again() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = crate::daemon::testutil::hermetic_mesh(cfg).await;
        let (a, b) = ([1u8; 32], [2u8; 32]);

        let first = super::claim_refreshes(&mesh, vec![a, b]);
        assert_eq!(
            first.len(),
            2,
            "both peers are unclaimed, so both are refreshed"
        );

        // While those claims are alive, a second poll gets nothing.
        assert!(
            super::claim_refreshes(&mesh, vec![a, b]).is_empty(),
            "a peer with a refresh already in flight must not be dialled again"
        );
        // …and a peer that is NOT in flight still is — the dedup must be per peer, not a global
        // latch. Without this, dropping the whole claim set would also pass the assertion above.
        assert_eq!(
            super::claim_refreshes(&mesh, vec![[3u8; 32]]).len(),
            1,
            "an unclaimed peer must still be refreshed while others are in flight"
        );

        // Released on drop — including on a panicking or cancelled probe task, which is why the
        // claim is a guard rather than a `remove` at the end of the spawned future. A leaked claim
        // would silently stop refreshing that peer forever.
        drop(first);
        assert_eq!(
            super::claim_refreshes(&mesh, vec![a, b]).len(),
            2,
            "claims must be released when the refresh ends"
        );
    }

    /// #176 at the CALL SITE: `reachability_of` must HOLD the claim for the life of the probe, not
    /// merely take one.
    ///
    /// Found by mutation review, and this is why the helper test above is not enough: inserting
    /// `drop(guard);` before the `tokio::spawn` disables the dedup completely — one dial per poll,
    /// exactly the behaviour #176 removes — and leaves every other test in this module green.
    /// Nothing else in the tree names `probes_inflight`.
    ///
    /// **The claim must still be held when the call returns, and that needs the probe to still be
    /// RUNNING** — which is why this peer is given a routable-but-silent address.
    ///
    /// The first version claimed to be deterministic because "the claim is taken synchronously
    /// before the spawn". That reasoning is wrong, and CI proved it: the claim is taken
    /// synchronously, but on a multi-thread runtime the spawned task can RUN TO COMPLETION and drop
    /// the guard before the assertion executes. A peer with `last_addr: None` has no address to
    /// dial, so the probe fails instantly — the fastest possible loser of that race. It passed 5/5
    /// locally and failed on ubuntu.
    ///
    /// Pointing `last_addr` at a loopback port nothing listens on makes the dial hang until
    /// `PROBE_TIMEOUT` (3s), so the window the assertion reads is three seconds wide instead of
    /// nanoseconds. That is a bounded, sleeping wait in the code under test rather than a `sleep`
    /// in the test, and it keeps the mutation coverage the timing version was written for.
    #[tokio::test(flavor = "multi_thread")]
    async fn reachability_of_holds_its_refresh_claim_across_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = crate::daemon::testutil::hermetic_mesh(cfg).await;
        // A REAL endpoint id — `EndpointAddr` will not carry an arbitrary 32 bytes — pointed at a
        // port nothing is bound to.
        let peer_key = iroh::SecretKey::generate();
        let peer_id = peer_key.public();
        let eid = *peer_id.as_bytes();
        let black_hole: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
        let addr = iroh::EndpointAddr::from_parts(peer_id, [iroh::TransportAddr::Ip(black_hole)]);
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: eid,
                nickname: "never-probed".into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: Some(serde_json::to_string(&addr).unwrap()),
            })
            .unwrap();

        // Never probed → stale → a refresh is scheduled.
        let rows = super::reachability_of(&mesh);
        assert_eq!(rows.len(), 1, "the stored peer must be projected");
        assert!(
            mesh.probes_inflight.lock().unwrap().contains(&eid),
            "the refresh claim must still be HELD while the probe runs — a claim taken and \
             released before the spawn dedups nothing"
        );

        // …and while it is held, a second poll schedules nothing for that peer. This is the
        // property, read through the real entry point rather than the helper.
        assert!(
            super::claim_refreshes(&mesh, vec![eid]).is_empty(),
            "a second status poll inside the probe window must not spawn another dial"
        );
    }

    /// #58: the emit rule, exhaustively. A transition is a CHANGE in the `reachable` verdict (or
    /// first knowledge of the peer) — never a refresh that merely re-confirms it, which would emit
    /// a frame per TTL refresh for a peer that is simply staying up.
    #[test]
    fn only_a_change_in_the_reachable_verdict_is_a_transition() {
        // First knowledge is news ONLY when the peer is UP. The snapshot already reports an
        // unprobed peer as `reachable: false`, so a first probe confirming "down" restates it —
        // emitting that produced a spurious offline burst on every daemon restart (#58 review).
        assert!(
            is_transition(None, &entry(true, Some(9))),
            "first probe finds the peer UP — news"
        );
        assert!(
            !is_transition(None, &entry(false, None)),
            "first probe confirms DOWN — the snapshot already said so"
        );

        // The two real flips.
        assert!(
            is_transition(Some(&entry(false, None)), &entry(true, Some(9))),
            "came back online"
        );
        assert!(
            is_transition(Some(&entry(true, Some(9))), &entry(false, None)),
            "went offline"
        );

        // A refresh confirming the same verdict is NOT a transition, even when the advisory
        // detail moved — this is what keeps a healthy peer from emitting once per TTL.
        assert!(
            !is_transition(Some(&entry(true, Some(9))), &entry(true, Some(9))),
            "unchanged refresh"
        );
        assert!(
            !is_transition(Some(&entry(true, Some(9))), &entry(true, Some(120))),
            "rtt drift alone is not a transition"
        );
        assert!(
            !is_transition(Some(&entry(false, None)), &entry(false, None)),
            "still offline"
        );

        // #92: a PATH change IS a transition, even with the verdict unchanged — unlike rtt/meta
        // drift above. `Direct` is a truth claim about where the traffic went, so a silent
        // Direct -> Relay leaves an embedder rendering "private" about a relayed session.
        let mut relayed = entry(true, Some(9));
        relayed.path = mcpmesh_local_api::PeerPath::Relay { url: None };
        let mut direct = entry(true, Some(9));
        direct.path = mcpmesh_local_api::PeerPath::Direct;
        assert!(
            is_transition(Some(&direct), &relayed),
            "Direct -> Relay must emit: the privacy indicator just became wrong"
        );
        assert!(
            is_transition(Some(&relayed), &direct),
            "and Relay -> Direct, so a recovered session can be relabelled"
        );
        assert!(
            !is_transition(Some(&direct), &direct.clone()),
            "but an unchanged path still does not emit once per TTL"
        );

        // Metadata drift alone is likewise not a transition.
        let mut with_meta = entry(true, Some(9));
        with_meta.meta = "status: away".into();
        assert!(
            !is_transition(Some(&entry(true, Some(9))), &with_meta),
            "meta drift alone is not a transition"
        );
    }

    /// The probe RECEIVE cap (#40): a pong's `meta` is surfaced only when it is a string within
    /// the ≤256B cap — a missing, non-string, or OVERSIZED value (a compromised paired peer
    /// signing nothing, just sending bytes over the authenticated channel) yields empty, so the
    /// prober never caches or surfaces an unbounded/garbage blob.
    #[test]
    fn pong_services_parses_the_array_and_tolerates_hostile_shapes() {
        use super::pong_services;
        assert_eq!(
            pong_services(&serde_json::json!({"services": ["notes", "kb"]})),
            vec!["notes".to_string(), "kb".to_string()]
        );
        assert!(pong_services(&serde_json::json!({"stack_version": "1"})).is_empty());
        assert!(pong_services(&serde_json::json!({"services": 42})).is_empty());
        assert!(pong_services(&serde_json::json!({"services": [1, {"x": 2}]})).is_empty());
    }

    /// #52: `caller_admitted_services` returns exactly the services whose allow admits the
    /// caller's principal — its eid, user_id, or a group — never the full registry.
    #[tokio::test(flavor = "multi_thread")]
    async fn caller_admitted_services_returns_only_admitted() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let caller_eid = mcpmesh_net::EndpointId::from_bytes([7u8; 32]).principal();
        std::fs::write(
            &config_path,
            format!(
                "[services.shared]\nsocket = \"/run/a.sock\"\nallow = [\"{caller_eid}\"]\n                 [services.grouped]\nsocket = \"/run/b.sock\"\nallow = [\"team-eng\"]\n                 [services.private]\nsocket = \"/run/c.sock\"\nallow = [\"eid:other\"]\n"
            ),
        )
        .unwrap();
        let mesh = crate::daemon::testutil::hermetic_mesh(config_path).await;

        // The caller: its eid admits `shared`; its group admits `grouped`; `private` is neither.
        let identity = mcpmesh_net::PeerIdentity {
            endpoint: mcpmesh_net::EndpointId::from_bytes([7u8; 32]),
            name: "bob".into(),
            user_id: None,
            groups: vec!["team-eng".into()],
        };
        let admitted = super::caller_admitted_services(&mesh, &identity);
        assert_eq!(admitted, vec!["grouped".to_string(), "shared".to_string()]);
        assert!(
            !admitted.contains(&"private".to_string()),
            "never a non-admitted service"
        );
    }

    /// #215 review: the ordinary embedder flow — open a session, THEN ask `peer_services`. The
    /// session's path watcher seeds a fresh, reachable row with no pong; before the fix the verb
    /// returned that row's empty list, indistinguishable from "offers nothing". The row is seeded
    /// through `path_watch::commit_observation`, the real writer, so the fixture holds exactly what a
    /// live daemon holds. Mutation: `probe_peer_cached` ignoring `pong_at` → `[]`, no ping dial.
    #[tokio::test(flavor = "multi_thread")]
    async fn peer_services_after_a_session_opens_still_fetches_the_pong() {
        use crate::daemon::conn_cache::testpeer::{
            dialer_id, dialer_mesh, dialer_principal, loopback_peer,
        };
        use std::sync::atomic::Ordering;
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(dir.path(), 52, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir.path(), &peer).await;

        let _session = crate::daemon::dial::dial_service(&mesh, "bob", "echo")
            .await
            .expect("session");
        let seq = mesh
            .probe_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::daemon::path_watch::commit_observation(
            &mesh,
            peer.id,
            seq,
            &mcpmesh_local_api::PeerPath::Direct,
        )
        .expect("precondition: the watcher seeds a fresh, pong-less row");

        let got = super::probe_peer_cached(&mesh, peer.id).await;
        assert_eq!(
            got.services,
            vec!["echo".to_string()],
            "a row with no pong must not answer peer_services — `[]` here reads as 'offers nothing'"
        );
        assert_eq!(
            peer.ping_accepts.load(Ordering::SeqCst),
            1,
            "the payload can only come from a ping dial"
        );
        assert!(
            got.pong_at.is_some(),
            "and the refreshed row now carries its pong"
        );
    }

    /// #215 final review: the probe `peer_services` triggers LOSES the commit race to the path
    /// watcher's reading, which takes a later ticket — so the pong it fetched was thrown away and
    /// the caller got the watcher's pong-less row, `[]`. Deterministic: the watcher's row is seeded
    /// through `commit_observation` with a ticket above anything the probe can draw, so the probe
    /// is superseded every time. The pong must be merged into that row — without forging its
    /// freshness. Mutation: drop the merge in the `Superseded` arm → `[]`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_superseded_probe_still_delivers_its_pong() {
        use crate::daemon::conn_cache::testpeer::{
            dialer_id, dialer_mesh, dialer_principal, loopback_peer,
        };
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer(dir.path(), 54, dialer_id(), &[("echo", &[me.as_str()])]).await;
        let mesh = dialer_mesh(dir.path(), &peer).await;

        let watcher_seq = 1u64 << 40;
        crate::daemon::path_watch::commit_observation(
            &mesh,
            peer.id,
            watcher_seq,
            &mcpmesh_local_api::PeerPath::Direct,
        )
        .expect(
            "precondition: the watcher seeds a pong-less row with a ticket the probe cannot beat",
        );
        let seeded = mesh
            .reachability
            .lock()
            .unwrap()
            .get(&peer.id)
            .cloned()
            .unwrap();

        let got = super::probe_peer_cached(&mesh, peer.id).await;
        assert_eq!(
            got.services,
            vec!["echo".to_string()],
            "a superseded probe must still hand its pong to the caller"
        );
        let row = mesh
            .reachability
            .lock()
            .unwrap()
            .get(&peer.id)
            .cloned()
            .unwrap();
        assert_eq!(row.services, vec!["echo".to_string()], "and to the cache");
        assert!(row.pong_at.is_some(), "the cached row now carries a pong");
        assert_eq!(
            (row.seq, row.probed_at, row.observed),
            (seeded.seq, seeded.probed_at, seeded.observed),
            "the merge adds the payload only — it must not re-stamp the winning row's freshness or \
             its ordering tickets"
        );
    }

    /// The natural shape of the same race, as the reviewer ran it: open a session, then at once
    /// ask for services. The path watcher's first reading (#225) races the probe; before the fix
    /// this returned `[]` in 9 runs out of 10. Ten runs, each on a fresh node and peer.
    #[tokio::test(flavor = "multi_thread")]
    async fn peer_services_right_after_a_session_opens_returns_the_services_every_time() {
        use crate::daemon::conn_cache::testpeer::{
            dialer_id, dialer_mesh, dialer_principal, loopback_peer,
        };
        let me = dialer_principal();
        for run in 0..10u8 {
            let dir = tempfile::tempdir().unwrap();
            let peer = loopback_peer(
                dir.path(),
                90 + run,
                dialer_id(),
                &[("echo", &[me.as_str()])],
            )
            .await;
            let mesh = dialer_mesh(dir.path(), &peer).await;
            let _session = crate::daemon::dial::dial_service(&mesh, "bob", "echo")
                .await
                .expect("session");
            let got = super::probe_peer_cached(&mesh, peer.id).await;
            assert_eq!(
                got.services,
                vec!["echo".to_string()],
                "run {run}: services right after a session opens"
            );
        }
    }

    /// The throttle arm fetches no pong, so the row it returns may be the watcher's pong-less one.
    /// `peer_services` must not answer `[]` from it — "offers nothing" is a claim this node cannot
    /// make — but refuse, retryably. The peer answers every ping with the limiter's close.
    /// Mutation: drop the pong check in `handlers::peer_services` → `Ok([])`.
    #[tokio::test(flavor = "multi_thread")]
    async fn peer_services_never_answers_empty_from_a_throttled_probe() {
        use crate::daemon::conn_cache::testpeer::{
            dialer_id, dialer_mesh, dialer_principal, loopback_peer_with,
        };
        let dir = tempfile::tempdir().unwrap();
        let me = dialer_principal();
        let peer = loopback_peer_with(
            dir.path(),
            55,
            dialer_id(),
            &[("echo", &[me.as_str()])],
            None,
            true,
        )
        .await;
        let mesh = dialer_mesh(dir.path(), &peer).await;
        let state = crate::control::DaemonState::with_mesh("test", mesh.clone());
        let seq = mesh
            .probe_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        crate::daemon::path_watch::commit_observation(
            &mesh,
            peer.id,
            seq,
            &mcpmesh_local_api::PeerPath::Direct,
        )
        .expect("precondition: a pong-less reachable row");

        let got = crate::daemon::handlers::peer_services(&state, "bob".into()).await;
        match got {
            Ok(r) => panic!(
                "a throttled probe fetched no services; answering {:?} claims the peer offers \
                 nothing",
                r.services
            ),
            Err(e) => {
                let msg = format!("{e:#}");
                assert!(
                    msg.contains("could not be fetched"),
                    "the refusal says the answer is unknown, not empty: {msg}"
                );
            }
        }
    }

    #[test]
    fn pong_meta_extracts_within_cap_and_drops_the_rest() {
        assert_eq!(
            pong_meta(&serde_json::json!({"stack_version": "1", "meta": "v=1.2.3"})),
            "v=1.2.3"
        );
        // No meta field → empty.
        assert_eq!(pong_meta(&serde_json::json!({"stack_version": "1"})), "");
        // Non-string meta → empty (never panics on hostile shapes).
        assert_eq!(pong_meta(&serde_json::json!({"meta": 42})), "");
        assert_eq!(pong_meta(&serde_json::json!({"meta": {"x": 1}})), "");
        // Exactly at the cap is kept; one over is dropped.
        let at = "x".repeat(APP_METADATA_MAX_BYTES);
        assert_eq!(pong_meta(&serde_json::json!({"meta": at.clone()})), at);
        let over = "x".repeat(APP_METADATA_MAX_BYTES + 1);
        assert_eq!(
            pong_meta(&serde_json::json!({"meta": over})),
            "",
            "oversized meta dropped"
        );
    }
}
