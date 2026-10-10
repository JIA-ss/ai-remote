use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use iroh::Endpoint;
use iroh_blobs::provider::events::{
    AbortReason, ConnectMode, EventMask, EventSender, ObserveMode, ProviderMessage, RequestMode,
    ThrottleMode,
};
use iroh_blobs::store::fs::FsStore;
use iroh_blobs::store::fs::options::Options as FsStoreOptions;
use iroh_blobs::store::{GcConfig, ProtectOutcome};
use iroh_blobs::ticket::BlobTicket;
use iroh_blobs::{BlobFormat, BlobsProtocol, Hash};
use mcpmesh_net::TrustGate;

use crate::audit::{AuditRecord, AuditSink, now_ts};
use crate::blobs::APP_BLOB_ALPN;
use crate::blobs::scope::ScopeStore;
use crate::daemon::RELAY_READY_TIMEOUT;

/// The request-time scope-gate `EventMask` for the serving app-blob provider.
///
/// SECURITY — deny-by-default on every non-GET request type, made EXPLICIT (not left to a vestigial
/// routing quirk). In the pinned iroh-blobs 0.103.0 the generic `EventSender::request()` reads ONLY
/// `mask.get` for EVERY request type (get/get_many/push/observe), so `get: InterceptLog` currently
/// routes all four to the drain loop, which denies the non-GET kinds explicitly.
/// To keep the deny-by-default INDEPENDENT of that single-field
/// routing, each non-GET request type is ALSO pinned to its most-refusing mask mode, so a FUTURE
/// iroh-blobs that honors the per-type fields still refuses them WITHOUT serving bytes:
///  - `get_many` / `push` = `RequestMode::Disabled`: the crate refuses this request type at the
///    protocol level with `Permission` and fires NO event — registry
///    `iroh-blobs-0.103.0/src/provider/events.rs:504-506` (`RequestMode::Disabled => return
///    Err(e!(ProgressError::Permission))`), doc at `events.rs:62-66`. Our legitimate clients only
///    ever do a single-blob `get`, so disabling these breaks nothing. (`push` is `Disabled` in
///    `EventMask::DEFAULT` already; pinning it makes the intent explicit.)
///  - `observe` = `ObserveMode::Intercept`: `ObserveMode` has NO `Disabled` variant
///    (`events.rs:34-44` — only `None`/`Notify`/`Intercept`), so the strongest available refusal is
///    `Intercept`, which fires an `ObserveRequestReceived` the drain loop denies with `Permission`.
///    `ObserveMode::None` (the default) would mean "no event, request served normally" → a silent
///    bypass, so it is explicitly the WRONG choice here.
///
/// `connected: Intercept` records the authenticated endpoint id; `get: InterceptLog` scope-checks every
/// single-blob GET (the AC fetch path — unchanged). `throttle` stays at its default
/// (`ThrottleMode::None`) — it is a transfer-throttling knob, not a request-serving gate.
const APP_BLOB_EVENT_MASK: EventMask = EventMask {
    connected: ConnectMode::Intercept,
    // `InterceptLog`, not `Intercept` (#82 ask 2): STRICTLY additive — it is Intercept plus the
    // per-request transfer-event stream. The scope check that authorizes every single-blob GET is
    // unchanged; what it adds is `msg.rx`, which the drain loop turns into `BlobTransfer` frames so
    // an embedder can draw a real progress bar instead of an indeterminate spinner.
    get: RequestMode::InterceptLog,
    get_many: RequestMode::Disabled,
    push: RequestMode::Disabled,
    observe: ObserveMode::Intercept,
    throttle: ThrottleMode::None,
};

/// [`APP_BLOB_EVENT_MASK`] with the #84a byte budget armed — identical except `throttle`.
///
/// Separate from the default because `ThrottleMode::Intercept` makes iroh-blobs round-trip an irpc
/// message PER CHUNK (~16 KiB), so a 4 GiB transfer is ~262k round-trips through the gate loop.
/// The cost is in-process and small, but a deployment that has not configured a budget should not
/// pay it at all. Chosen ONCE in `load`, so changing the config key needs a daemon restart.
const APP_BLOB_EVENT_MASK_METERED: EventMask = EventMask {
    throttle: ThrottleMode::Intercept,
    ..APP_BLOB_EVENT_MASK
};

/// Fold ONE transfer update into the coalescing state, emitting a frame when it warrants one (#82).
///
/// Returns `true` when the transfer is over (terminal event), so the caller stops draining.
///
/// A free function rather than inline in the drain task so the COALESCING RULE — the property that
/// keeps a 4 GiB transfer from pushing ~262k frames through a bounded ring — is directly testable
/// without a live provider and two endpoints.
fn apply_transfer_update(
    st: &mut Option<TransferProgressState>,
    update: &iroh_blobs::provider::events::RequestUpdate,
    bcast: &tokio::sync::broadcast::Sender<crate::daemon::BlobTransfer>,
    peer: &Option<String>,
) -> bool {
    use iroh_blobs::provider::events::RequestUpdate;
    use mcpmesh_local_api::BlobTransferState as S;

    match update {
        RequestUpdate::Started(started) => {
            let cur = TransferProgressState {
                hash: started.hash.to_hex().to_string(),
                peer: peer.clone(),
                total: Some(started.size),
                done: 0,
                last_emitted: 0,
                epochs: 0,
                in_epoch: 0,
            };
            emit_transfer(bcast, &cur, S::Started);
            *st = Some(cur);
            false
        }
        RequestUpdate::Progress(p) => {
            if let Some(cur) = st.as_mut() {
                cur.done = p.end_offset;
                // THE coalescing gate. Without it a 4 GiB transfer emits ~262k frames and every
                // subscriber lags out, losing the audit records that share their stream.
                if cur.done.saturating_sub(cur.last_emitted) >= cur.stride() {
                    cur.note_emitted();
                    emit_transfer(bcast, cur, S::Progress);
                }
            }
            false
        }
        RequestUpdate::Completed(_) => {
            if let Some(cur) = st.as_mut() {
                // The final count, ALWAYS emitted — the last `Progress` before this is usually
                // skipped by the stride, so a consumer treating it as the total stops short.
                if let Some(total) = cur.total {
                    cur.done = cur.done.max(total);
                }
                emit_transfer(bcast, cur, S::Completed);
            }
            true
        }
        RequestUpdate::Aborted(_) => {
            if let Some(cur) = st.as_ref() {
                // Reported, not a silent stop: a stalled transfer must be distinguishable from a
                // slow one, which is the issue's fourth consequence.
                emit_transfer(bcast, cur, S::Aborted);
            }
            true
        }
    }
}

/// Minimum byte advance between two coalesced `Progress` frames (#82 ask 2).
///
/// iroh-blobs reports progress per ~16 KiB chunk. Broadcasting each one would push ~262k frames for
/// a 4 GiB transfer through a bounded ring, so every subscriber would see `Lagged` and lose the
/// reachability/audit signal sharing their stream. The stride is `max(this, total / 100)`, so a
/// transfer costs at most ~102 frames whatever its size — and a SMALL blob still gets its
/// `Started`/`Completed` pair, which is what a progress bar actually needs.
const PROGRESS_STRIDE_BYTES: u64 = 1024 * 1024;

/// Coalescing state for ONE in-flight served transfer (#82).
struct TransferProgressState {
    hash: String,
    peer: Option<String>,
    total: Option<u64>,
    done: u64,
    /// `done` as of the last frame emitted — the coalescing anchor.
    last_emitted: u64,
    /// How many times the stride has doubled (unknown-total transfers only).
    epochs: u32,
    /// Frames emitted within the current epoch.
    in_epoch: u32,
}

/// Frames allowed per stride "epoch" before the stride DOUBLES (#82 gate).
///
/// Only reachable when the total is unknown — which is every FETCH, since `GetProgressItem` carries
/// no size. Without it the stride stayed at the 1 MiB floor forever, so a 4 GiB fetch emitted ~4098
/// frames into a 256-deep ring: the direction #82 is actually about was the one still flooding.
/// Doubling per epoch bounds it logarithmically — ~128 frames for 4 GiB, ~256 for 1 TiB.
const FRAMES_PER_EPOCH: u32 = 16;

impl TransferProgressState {
    /// The byte advance required before another `Progress` frame is worth sending.
    ///
    /// With a known total (the SERVE side) this is 1% of it, so the frame count is ~100 flat. With
    /// an unknown total (every FETCH) it starts at the floor and doubles every
    /// [`FRAMES_PER_EPOCH`] frames, so the count grows with the LOG of the size rather than
    /// linearly.
    fn stride(&self) -> u64 {
        match self.total {
            Some(t) => (t / 100).max(PROGRESS_STRIDE_BYTES),
            None => PROGRESS_STRIDE_BYTES
                .saturating_mul(1u64 << self.epochs.min(40))
                .max(PROGRESS_STRIDE_BYTES),
        }
    }

    /// Record that a `Progress` frame went out, widening the stride when an epoch fills.
    fn note_emitted(&mut self) {
        self.last_emitted = self.done;
        if self.total.is_none() {
            self.in_epoch += 1;
            if self.in_epoch >= FRAMES_PER_EPOCH {
                self.in_epoch = 0;
                self.epochs = self.epochs.saturating_add(1);
            }
        }
    }
}

/// [`emit_transfer`] for the FETCHING side (#82) — same frame, `direction: Fetch`, and no `peer`:
/// the counterparty is named by the ticket, not by an identity we resolved.
fn emit_fetch(
    bcast: &tokio::sync::broadcast::Sender<crate::daemon::BlobTransfer>,
    st: &TransferProgressState,
    state: mcpmesh_local_api::BlobTransferState,
) {
    let _ = bcast.send(crate::daemon::BlobTransfer {
        direction: mcpmesh_local_api::BlobDirection::Fetch,
        hash: st.hash.clone(),
        bytes_done: st.done,
        bytes_total: st.total,
        state,
        peer: None,
    });
}

/// Broadcast one transfer observation, never blocking (#82).
///
/// `send` on a `broadcast::Sender` does not await and errors only when there are no receivers, so a
/// slow or absent subscriber can never stall a transfer — preserving the `try_send` property
/// iroh-blobs itself relies on for progress.
fn emit_transfer(
    bcast: &tokio::sync::broadcast::Sender<crate::daemon::BlobTransfer>,
    st: &TransferProgressState,
    state: mcpmesh_local_api::BlobTransferState,
) {
    let _ = bcast.send(crate::daemon::BlobTransfer {
        direction: mcpmesh_local_api::BlobDirection::Serve,
        hash: st.hash.clone(),
        bytes_done: st.done,
        bytes_total: st.total,
        state,
        peer: st.peer.clone(),
    });
}

/// iroh-blobs' leaf/chunk size (`IROH_BLOCK_SIZE`, 16 KiB) — the unit a `Throttle` event reports,
/// and the amount reserved at request admission (#84a review).
const IROH_CHUNK_BYTES: u64 = 16 * 1024;

/// The gated app-blob provider. `events` is `Some` for a serving daemon (the request-time
/// scope Intercept gate is armed) and `None` for a caller-only fetcher. `scopes` is the persisted
/// scope table; a fetcher gets an empty one it never mutates.
///
/// The drain loop's `Receiver<ProviderMessage>` is moved into a task
/// spawned once in `load`. The loop lives as long as ANY `EventSender` clone lives; `AppBlobs` holds
/// one in `self.events` for the provider's lifetime (the daemon holds `AppBlobs` for its lifetime),
/// and every `protocol()` clones another into the `BlobsProtocol`. So the gate loop runs until the
/// daemon drops the provider — never terminating mid-serve.
pub struct AppBlobs {
    store: FsStore,
    endpoint: Endpoint,
    /// Where coalesced transfer progress goes (#82 ask 2). `None` on a fetcher-only or fixture
    /// provider, which then does no progress work at all.
    transfers: Option<tokio::sync::broadcast::Sender<crate::daemon::BlobTransfer>>,
    events: Option<EventSender>,
    scopes: Arc<ScopeStore>,
    /// The request-time gate loop's handle, so shutdown can END it deterministically (#61).
    ///
    /// That task owns an `Arc<dyn TrustGate>`, which on a pairing daemon holds the `PeerStore` and
    /// therefore the redb data-dir lock. It used to be a fire-and-forget `tokio::spawn` whose handle
    /// was discarded: the loop exits when the last `EventSender` drops, but only once the task is
    /// next polled, so nothing guaranteed the lock was released by the time `shutdown` returned.
    /// Unreachable while the provider was roster-only — an embedded `NodeBuilder` node never built
    /// one — and it broke `shutdown_frees_the_root_*` the moment app blobs reached pairing mode.
    gate_loop: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Wait (bounded) for the relay handshake before minting a ticket (#83 ask 3).
    ///
    /// OFF by default, switched ON by boot alone. The wait exists so a ticket carries the
    /// home-relay URL a fetcher needs across NAT; on a relay-disabled endpoint `online()` never
    /// completes, so it is a guaranteed [`RELAY_READY_TIMEOUT`] of dead time per mint. Defaulting
    /// off keeps that cost out of every test fixture (relay-disabled by construction) while
    /// production — the only place the relay URL matters — opts in explicitly.
    relay_wait: std::sync::atomic::AtomicBool,
    /// Serializes the HASH-MEMBERSHIP mutations (#104).
    ///
    /// `ScopeStore` makes each individual mutation atomic, but `republish` is a read-check-write:
    /// it verifies the blob is complete (an `.await` on the store) and only then inserts. A
    /// concurrent `blob_unpublish` landing in that gap is silently undone — both verbs return
    /// success and the operator's revocation disappears. An async lock is required because the
    /// completeness check awaits, so `ScopeStore`'s `std::sync::Mutex` cannot be held across it.
    ///
    /// Held by every verb that adds or removes a hash from a scope; grant/revoke of PRINCIPALS do
    /// not contend, since they cannot race a membership decision.
    hash_membership: tokio::sync::Mutex<()>,
    /// TEST-ONLY: pause between `republish`'s completeness check and its scope insert, so the
    /// interleaving #104 describes is deterministic rather than timing-dependent.
    #[cfg(test)]
    republish_delay: std::sync::Mutex<Option<std::time::Duration>>,
    /// TEST-ONLY: pause between `publish_scope`'s import and its scope insert (#104).
    #[cfg(test)]
    publish_delay: std::sync::Mutex<Option<std::time::Duration>>,
    /// What the background GC has done (#80). Always present; all zeros when GC is not configured,
    /// which `status` distinguishes by reading the configured interval rather than these counters.
    gc_stats: Arc<BlobGcStats>,
}

/// What a running blob GC has actually done (#80), for `status.storage.blobs_gc`.
///
/// Atomics rather than a lock: written from the protect callback on a background timer, read from
/// the synchronous `status` builder. Nothing here needs to be consistent across fields.
///
/// **There is no `bytes_reclaimed`, and that is not an oversight.** `run_gc`'s only callback fires
/// BEFORE the sweep and there is no after-callback, so any byte count printed here would be a
/// guess. `status.storage.blobs_bytes` already walks the store directory; an operator reads reclaim
/// off that, over time, which is measured rather than asserted.
#[derive(Debug, Default)]
pub struct BlobGcStats {
    /// Runs STARTED — entries to the protect callback, not finished sweeps (we are not told about
    /// those).
    ///
    /// THE load-bearing field: `run_gc` `break`s its loop on the first `gc_run_once` error rather
    /// than continuing, so one failed sweep silently ends collection for the life of the process. A
    /// counter that stops advancing is the only signal an operator gets.
    pub runs: std::sync::atomic::AtomicU64,
    /// Unix seconds of the most recent run start; 0 before the first.
    pub last_run_epoch: std::sync::atomic::AtomicI64,
    /// Hashes protected on the most recent run.
    pub last_protected: std::sync::atomic::AtomicU64,
    /// Runs ABORTED because the liveness root could not be read — each one swept nothing.
    pub aborted: std::sync::atomic::AtomicU64,
}

/// How long ONE source gets to complete the transfer before the fetch moves on (#83).
///
/// Generous, because a large blob over a slow link is not a failure — but bounded, because a source
/// that accepts the connection and then stalls would otherwise hold the whole fetch open with live
/// alternates untried, which is the failure this feature exists to prevent. A caller wanting a
/// tighter budget cancels (`blob_fetch_cancel`, #172); a caller wanting a looser one is asking for
/// an unbounded wait, which is not on offer.
const SOURCE_TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// How long a source gets to answer before the NEXT one is started alongside it (#83 follow-up).
///
/// The escape from `DIAL_TIMEOUT` (20s): trying eight sources strictly in turn behind a sleeping
/// publisher costs 160 seconds, and a user watching an indeterminate bar cannot tell that from a
/// hang. Hedging starts the next source after a second instead of after a timeout.
///
/// It is a small fraction of `DIAL_TIMEOUT` on purpose — that is the wait being escaped — but long
/// enough that a healthy LAN or relay dial finishes first, which is what keeps the common case free
/// of abandoned work.
const HEDGE_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// How many dials may be outstanding at once (#83 follow-up).
///
/// The bound on work inflicted on OTHER machines. #83's own review declined a blind parallel race
/// for exactly this reason — "every abandoned dial is work on someone else's machine" — and with
/// [`MAX_BLOB_SOURCES`] at 32 an uncapped race is a 32x amplifier any caller can trigger by being
/// generous with `from`. Hedging answers the objection; this bounds what is left of it.
///
/// [`MAX_BLOB_SOURCES`]: mcpmesh_local_api::MAX_BLOB_SOURCES
const MAX_IN_FLIGHT_DIALS: usize = 3;

/// One in-flight dial: its source index and the connection it produced.
///
/// Boxed because [`race_a_connection`](AppBlobs::race_a_connection) hands the set back to its
/// caller — `fetch_from` owns the losing dials so they survive a failed transfer, which needs a
/// nameable type.
type BoxedDial<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = (usize, Result<iroh::endpoint::Connection>)> + Send + 'a>,
>;
/// The set of dials currently in progress.
type DialFlight<'a> = n0_future::FuturesUnordered<BoxedDial<'a>>;

/// The hedged-dial SCHEDULE, with no I/O in it (#83 follow-up).
///
/// Pure so the rule is testable without waiting real seconds. The alternative — asserting on a live
/// racer — measures the machine rather than the code, and this repo has twice produced confident
/// and wrong conclusions from exactly that.
///
/// **Losing dials are never cancelled, which is what keeps this honest.** A first cut dropped the
/// rivals when one dial won and pushed them back on a retry queue. That is much worse than it
/// sounds: a publisher that is alive but more than a `HEDGE_DELAY` away — relay-mediated, or still
/// hole-punching — loses every race it enters, so it was re-dialled and abandoned once per round.
/// For a room of eight where the alternates answer fast but cannot serve, that is 15 dials against
/// the sequential walk's 8, with the publisher abandoned seven times. Keeping the losers running
/// instead means every source is dialled AT MOST ONCE, and a slow-but-good source simply finishes
/// later and wins a later round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HedgePlan {
    total: usize,
    /// The next source never yet started. Sources enter in the caller's order.
    next: usize,
    /// How many dials are outstanding. A COUNT is sufficient — and correct — precisely because no
    /// dial is ever abandoned, so the plan never has to name which sources are in flight in order
    /// to give them back.
    in_flight: usize,
}

impl HedgePlan {
    pub(crate) fn new(total: usize) -> Self {
        Self {
            total,
            next: 0,
            in_flight: 0,
        }
    }

    /// May another source be started right now? One is left AND we are under the cap.
    pub(crate) fn may_start(&self) -> bool {
        self.next < self.total && self.in_flight < MAX_IN_FLIGHT_DIALS
    }

    /// Start the next source, returning its index — `None` if [`may_start`](Self::may_start) is
    /// false. Returning the index rather than letting the caller keep its own counter is what stops
    /// the plan's idea of what is running from drifting from what actually is.
    pub(crate) fn start(&mut self) -> Option<usize> {
        if !self.may_start() {
            return None;
        }
        let idx = self.next;
        self.next += 1;
        self.in_flight += 1;
        Some(idx)
    }

    /// A dial completed — it failed, or it won and is being handed to the transfer. Either way that
    /// source is spent: a failed dial is not retried, and a winner is not re-dialled.
    pub(crate) fn finished(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
    }

    /// Nothing left to start and nothing still running.
    pub(crate) fn exhausted(&self) -> bool {
        self.next >= self.total && self.in_flight == 0
    }
}

/// Build the `GcConfig` handed to `FsStore::load_with_opts` (#80).
///
/// `iroh_blobs`'s `gc_mark` roots the live set in the store's tags and temp tags, then unions
/// whatever this callback adds. mcpmesh creates **no persistent tags** — `publish_path`'s `TempTag`
/// is dropped as it returns — so without this callback the root would be empty and the first sweep
/// would delete every blob on the node. The scope table IS the root.
///
/// Three properties of upstream's `run_gc`, read out of its source rather than assumed, that an
/// operator has to know and that shape everything here:
///
/// 1. **It sleeps before its first run** (`loop { live.clear(); sleep(interval); … }`). A node with
///    `gc_interval = "24h"` reclaims nothing until it has been up 24 hours. There is no boot sweep
///    and no way to request one — see the `load_with_opts` comment for why we cannot add one.
/// 2. **One error ends collection for the process.** `if let Err(e) = gc_run_once(..) { error!(); break }`
///    — `break`, not `continue`. Hence [`BlobGcStats::runs`], whose stalling is the only signal.
/// 3. **`ProtectOutcome::Abort` skips ONE run and keeps the schedule** (`continue`). That is the
///    fail-safe, and it is why [`ScopeStore::live_hashes`] is fallible: a run that cannot read the
///    root must sweep nothing rather than sweep against an empty one.
///
/// **Lifetime.** `run_gc` is spawned onto the store's own dedicated runtime, which the fs actor
/// owns, and it holds a `Store` clone — while the actor's loop ends only when every `commands_tx`
/// sender drops. That is a cycle dropping the `FsStore` cannot break, so the collector would run
/// until the process exited. [`AppBlobs::shutdown`] closes the store explicitly, which ends the
/// actor and with it the GC task; see there, because that release turned out to matter with or
/// without a collector.
fn gc_config(
    interval: std::time::Duration,
    scopes: Arc<ScopeStore>,
    stats: Arc<BlobGcStats>,
) -> GcConfig {
    use std::sync::atomic::Ordering;
    GcConfig {
        interval,
        add_protected: Some(Arc::new(move |live: &mut HashSet<Hash>| {
            let scopes = scopes.clone();
            let stats = stats.clone();
            Box::pin(async move {
                stats.runs.fetch_add(1, Ordering::Relaxed);
                stats
                    .last_run_epoch
                    .store(crate::util::epoch_now_i64(), Ordering::Relaxed);
                let hashes = match scopes.live_hashes() {
                    Ok(h) => h,
                    Err(e) => {
                        stats.aborted.fetch_add(1, Ordering::Relaxed);
                        tracing::error!(
                            %e,
                            "blob gc: cannot read the scope table; skipping this run rather than \
                             sweeping against an empty liveness root"
                        );
                        return ProtectOutcome::Abort;
                    }
                };
                let mut protected = 0u64;
                for hex in &hashes {
                    // A hash the scope table holds but that will not parse cannot protect anything,
                    // and it must NOT abort the run: one junk row would disable collection forever
                    // while looking configured. Warn and carry on — the sweep then reclaims that
                    // blob, which is correct, since an unparseable entry authorizes nobody either
                    // (`allows` compares against canonical hex).
                    match crate::blobs::parse_blob_hash(hex) {
                        Ok(h) => {
                            live.insert(h);
                            protected += 1;
                        }
                        Err(e) => tracing::warn!(
                            hash = %hex,
                            %e,
                            "blob gc: scope table holds an unparseable hash; it protects nothing"
                        ),
                    }
                }
                stats.last_protected.store(protected, Ordering::Relaxed);
                tracing::info!(protected, "blob gc: sweeping");
                ProtectOutcome::Continue
            })
        })),
    }
}

impl AppBlobs {
    /// Protect `hash` from a garbage-collection sweep for as long as the returned tag is held
    /// (#80).
    ///
    /// The seam for "this blob is in the store, is in no scope, and something still needs it" — a
    /// just-fetched blob between the transfer and the export, for instance. A blob in a scope needs
    /// no pin: the scope table is the durable liveness root.
    ///
    /// `None` when the store cannot mint one, and that is deliberately not an error: the caller is
    /// mid-operation, and failing it because the GC bookkeeping hiccuped would be worse than the
    /// window a pin closes. On a node with no `gc_interval` there is nothing to protect against at
    /// all.
    pub async fn pin(&self, hash: Hash) -> Option<iroh_blobs::api::TempTag> {
        self.store.tags().temp_tag(hash).await.ok()
    }

    /// What the background GC has done (#80). All zeros when GC is not configured.
    pub fn gc_stats(&self) -> Arc<BlobGcStats> {
        self.gc_stats.clone()
    }

    /// End the request-time gate loop, releasing the `TrustGate` (and with it the redb handle), and
    /// CLOSE the blob store, releasing `blobs.db`. Idempotent; a fetcher has no gate loop and skips
    /// that half.
    ///
    /// The `await` after `abort` is deliberate but NOT load-bearing for the current test: dropping
    /// the provider already closes the event channel, and abort-without-await passes today. It is
    /// here so the release is deterministic rather than dependent on when the runtime reaps the
    /// task — the racy version is the kind that fails under load, not in CI.
    ///
    /// **Closing the store is load-bearing, and NOT only for a collecting one.** Without this call,
    /// dropping the provider left the fs actor running and `blobs.db` locked. Two measurements,
    /// both taken by removing this call and reopening the same directory:
    ///
    /// - After a real `publish_scope`: `gc: Some(..)` hangs, `gc: None` reopens.
    /// - On a store nothing had written: **both** hang, under a current-thread and a multi-thread
    ///   runtime alike.
    ///
    /// So the leak is not created by #80 — it predates it, and whether it bites without a collector
    /// depends on what the store was doing. GC makes it deterministic, because `run_gc` holds a
    /// `Store` clone on the store's own runtime and the actor's loop ends only when the last sender
    /// drops: a cycle dropping the `FsStore` cannot break.
    ///
    /// Recorded as measured rather than as reported — the 0.43.0 gate called this GC-introduced and
    /// GC-specific, and the second measurement above says otherwise. The fix is the same either
    /// way; the scope of what it fixes is not.
    ///
    /// Done for EVERY provider regardless — `shutdown` means "release this node's resources", and a
    /// release that depends on a config knob is the kind that is right in CI and wrong in
    /// production.
    ///
    /// After this the provider is finished — every store call fails. That already matched how
    /// `shutdown` was used (boot's teardown, and the root-release tests).
    pub async fn shutdown(&self) {
        let handle = self.gate_loop.lock().await.take();
        if let Some(h) = handle {
            h.abort();
            let _ = h.await;
        }
        // Best-effort: a store already gone is exactly the idempotent second call.
        if let Err(e) = self.store.shutdown().await {
            tracing::debug!(%e, "blob store shutdown (already closed?)");
        }
    }
}

impl AppBlobs {
    /// [`open_fetcher_with_progress`](Self::open_fetcher_with_progress) with no progress ring.
    pub async fn open_fetcher(blobs_dir: PathBuf, endpoint: Endpoint) -> Result<Arc<Self>> {
        Self::open_fetcher_with_progress(blobs_dir, endpoint, None).await
    }

    /// A caller-only fetcher: an `FsStore` + endpoint, NO scope gate (`events: None`), an empty
    /// scopes table it never persists. Used caller-side (the fetch path) and by the ungated tests.
    ///
    /// `transfers` is where fetch-side progress goes (#82); `None` does no progress work at all.
    pub async fn open_fetcher_with_progress(
        blobs_dir: PathBuf,
        endpoint: Endpoint,
        transfers: Option<tokio::sync::broadcast::Sender<crate::daemon::BlobTransfer>>,
    ) -> Result<Arc<Self>> {
        tokio::fs::create_dir_all(&blobs_dir)
            .await
            .with_context(|| format!("create blobs dir {}", blobs_dir.display()))?;
        // NEVER configure GC here (#80). A fetcher's `ScopeStore::new` is an empty table that is
        // never persisted and never mutated, so a protect callback reading it would protect NOTHING
        // and the very first sweep would delete every blob this fetcher holds. GC belongs only on
        // `load`, whose scope table is the real one.
        let store = FsStore::load(&blobs_dir)
            .await
            .with_context(|| format!("load blob store {}", blobs_dir.display()))?;
        Ok(Arc::new(Self {
            store,
            endpoint,
            transfers,
            gc_stats: Arc::new(BlobGcStats::default()),
            events: None,
            relay_wait: std::sync::atomic::AtomicBool::new(false),
            hash_membership: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            republish_delay: std::sync::Mutex::new(None),
            #[cfg(test)]
            publish_delay: std::sync::Mutex::new(None),
            scopes: Arc::new(ScopeStore::new(blobs_dir.join("scopes.json"))),
            gate_loop: tokio::sync::Mutex::new(None),
        }))
    }

    /// The GATED provider: an `FsStore` + the request-time scope Intercept `EventSender`.
    /// Spawns the drain loop ONCE, wired to the trust `gate` (resolve endpoint → identity) and
    /// `scopes` (the authz table). `FsStore::load` is async/fallible;
    /// the dir is created first.
    #[allow(clippy::too_many_arguments)] // every one is a distinct collaborator this provider owns
    pub async fn load(
        blobs_dir: PathBuf,
        scopes: Arc<ScopeStore>,
        gate: Arc<dyn TrustGate>,
        endpoint: Endpoint,
        audit: AuditSink,
        limits: Arc<crate::limits::MeshLimiters>,
        // #82 ask 2: the ring coalesced transfer progress rides. `None` for fixtures that build a
        // provider without a mesh — the gate loop then does no progress work at all.
        transfers: Option<tokio::sync::broadcast::Sender<crate::daemon::BlobTransfer>>,
        // #80: how often to garbage-collect the store, or `None` for "never" — the behavior of
        // every release up to 0.42.0 and still the default. See `spawn_gc_config`.
        gc: Option<std::time::Duration>,
    ) -> Result<Arc<Self>> {
        tokio::fs::create_dir_all(&blobs_dir)
            .await
            .with_context(|| format!("create blobs dir {}", blobs_dir.display()))?;
        let gc_stats = Arc::new(BlobGcStats::default());
        let store = match gc {
            // #80: `FsStore::load_with_opts` is the ONLY door to collection in iroh-blobs 0.103.0 —
            // `gc_run_once` is `pub` inside a PRIVATE module (`store/mod.rs:11`) and `Blobs::delete`
            // is `pub(crate)`, so there is no on-demand sweep to call and no way to add one later
            // without an upstream change. Configured at construction or not at all.
            Some(interval) => {
                let opts = FsStoreOptions {
                    gc: Some(gc_config(interval, scopes.clone(), gc_stats.clone())),
                    ..FsStoreOptions::new(&blobs_dir)
                };
                // `FsStore::load` derives `db_path = root/blobs.db` and `Options::new(root)`; the
                // low-level entry point takes both separately, so the derivation has to be
                // repeated EXACTLY or the two paths diverge and the store opens somewhere else.
                let store = FsStore::load_with_opts(blobs_dir.join("blobs.db"), opts)
                    .await
                    .with_context(|| format!("load blob store with gc {}", blobs_dir.display()))?;
                // Clear the AUTO-TAGS every release up to 0.42.0 wrote (#80).
                //
                // `add_path().await` runs `with_tag()`, which persists a tag per imported blob, and
                // `gc_mark` roots the live set in tags. So on an existing store every blob ever
                // published is permanently rooted and the first sweep would reclaim NOTHING while
                // logging a run every interval — the feature would look configured and do nothing.
                //
                // Safe because mcpmesh reads tags NOWHERE: the scope table is the sole authority on
                // what is live, and `add_protected` feeds it in on every run. This deletes a
                // redundant root, not data. Once, at construction, before the provider serves.
                //
                // Best-effort: a store that cannot clear its tags is one whose sweeps will protect
                // too much, which is the safe direction. Do not fail the boot over it.
                match store.tags().delete_all().await {
                    Ok(n) if n > 0 => tracing::info!(
                        tags = n,
                        "blob gc: cleared pre-0.43.0 auto-tags; the scope table is the liveness root"
                    ),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(
                        %e,
                        "blob gc: could not clear auto-tags; sweeps will over-protect"
                    ),
                }
                store
            }
            None => FsStore::load(&blobs_dir)
                .await
                .with_context(|| format!("load blob store {}", blobs_dir.display()))?,
        };
        // The request-time scope gate: `APP_BLOB_EVENT_MASK` intercepts connect + single-blob GET,
        // and pins every non-GET request type to deny-by-default (Disabled/Intercept — see the
        // const's SECURITY note). Since `get: InterceptLog` also routes
        // get_many/observe/push to the drain loop today; the pinned fields keep them refused even if
        // a future iroh-blobs honors the per-type fields directly.
        // Only pay the per-chunk intercept when a budget is actually configured (#84a).
        let mask = if limits.blob_bytes_enabled() {
            APP_BLOB_EVENT_MASK_METERED
        } else {
            APP_BLOB_EVENT_MASK
        };
        let (events, rx) = EventSender::channel(64, mask);
        let gate_loop = spawn_gate_loop(rx, gate, scopes.clone(), audit, limits, transfers.clone());
        Ok(Arc::new(Self {
            store,
            endpoint,
            transfers,
            gc_stats,
            events: Some(events),
            scopes,
            gate_loop: tokio::sync::Mutex::new(Some(gate_loop)),
            relay_wait: std::sync::atomic::AtomicBool::new(false),
            hash_membership: tokio::sync::Mutex::new(()),
            #[cfg(test)]
            republish_delay: std::sync::Mutex::new(None),
            #[cfg(test)]
            publish_delay: std::sync::Mutex::new(None),
        }))
    }

    /// The `BlobsProtocol` handler the accept loop dispatches `APP_BLOB_ALPN` to. Carries the scope
    /// gate when `events` is `Some` (a serving daemon); ungated for a fetcher. `&self.store`
    /// (a `&FsStore`) deref-coerces to `&Store`; `self.events.clone()` shares the ONE drain loop.
    pub fn protocol(&self) -> BlobsProtocol {
        BlobsProtocol::new(&self.store, self.events.clone())
    }

    /// TEST-ONLY: register an app-blob ALPN accept handler directly on `endpoint`, BYPASSING the
    /// accept-time trust gate (the request-time scope gate still runs via `protocol()`'s events).
    /// Production accept ALWAYS goes through the gated daemon loop (`spawn_accept_loop`'s
    /// `APP_BLOB_ALPN` arm: resolve → 401 + rate-limit + check-register); this exists only so
    /// same-file unit tests can serve blobs without assembling a daemon. `#[cfg(test)]` so it can
    /// never leak into a production accept path.
    #[cfg(test)]
    pub(crate) fn spawn_accept(&self, endpoint: &Endpoint) {
        let proto = self.protocol();
        let ep = endpoint.clone();
        tokio::spawn(async move {
            while let Some(incoming) = ep.accept().await {
                if let Ok(conn) = incoming.await
                    && conn.alpn() == APP_BLOB_ALPN
                {
                    let _ = iroh::protocol::ProtocolHandler::accept(&proto, conn).await;
                }
            }
        });
    }

    /// Import a LOCAL file and mint its ticket, returning the `TempTag` that protects it.
    ///
    /// **`.temp_tag()`, deliberately NOT `.await` on `add_path` (#80).** Awaiting `AddProgress`
    /// runs `with_tag()`, which writes a PERSISTENT auto-tag — and `gc_mark` roots the live set in
    /// the store's tags. So every blob mcpmesh ever added was permanently rooted, and a garbage
    /// collector configured over it would have been a guaranteed no-op that still logged a sweep
    /// every interval. (#80's own research asserts the opposite — "mcpmesh creates no persistent
    /// tags" — which is what `publish_path`'s temp tag WOULD have given us had it not been awaited
    /// into a permanent one. Caught by the acceptance test failing to reclaim anything.)
    ///
    /// The temp tag is the caller's to hold: it protects the blob from a sweep that lands between
    /// the import and whatever makes the blob durably live. Dropping it makes the blob eligible
    /// immediately.
    async fn import_path(&self, path: &Path) -> Result<(iroh_blobs::api::TempTag, String, String)> {
        let tag = self
            .store
            .blobs()
            .add_path(path)
            .temp_tag()
            .await
            .with_context(|| format!("add blob from {}", path.display()))?;
        let hash = tag.hash();
        let ticket = self.ticket_for(hash).await;
        Ok((tag, ticket.to_string(), hash.to_hex().to_string()))
    }

    /// Add a LOCAL file to the store (the large-blob idiom — `add_path`) and return
    /// `(ticket_string, blake3_hex)` WITHOUT touching any scope (used for the ungated round-trip).
    ///
    /// Touching no scope means naming no liveness root, so on a node with `[blobs].gc_interval` set
    /// the blob is reclaimable from the moment this returns — the temp tag drops here. That is the
    /// intended reading of "published to a ticket but shared with nobody"; `publish_scope` is the
    /// verb that makes a blob durably live.
    pub async fn publish_path(&self, path: &Path) -> Result<(String, String)> {
        let (_temp, ticket, hash_hex) = self.import_path(path).await?;
        Ok((ticket, hash_hex))
    }

    /// Publish a LOCAL file INTO a scope: add it to the store AND record its hash in the
    /// named scope (single-writer via `ScopeStore`). Returns `(ticket_string, blake3_hex)`.
    pub async fn publish_scope(&self, scope: &str, path: &Path) -> Result<(String, String)> {
        // #80: hold the import's temp tag across the scope insert. Between the import and the
        // insert the blob is named by nothing, so a sweep landing in that window would delete a
        // file the operator is in the middle of publishing — and `publish_delay` below makes that
        // window arbitrarily wide on purpose. The temp tag protects it until the scope does; it is
        // dropped as this returns, by which point the scope table is the root.
        let (_temp, ticket, hash_hex) = self.import_path(path).await?;
        // #104: membership mutations are serialized as a family, so an import that finishes while
        // an unpublish is in flight cannot interleave with it either.
        let _membership = self.hash_membership.lock().await;
        #[cfg(test)]
        {
            let d = *self
                .publish_delay
                .lock()
                .expect("publish delay lock not poisoned");
            if let Some(d) = d {
                tokio::time::sleep(d).await;
            }
        }
        self.scopes.publish_hash(scope, &hash_hex)?;
        Ok((ticket, hash_hex))
    }

    /// Add a hash ALREADY COMPLETE in the local store to a scope (#83) — the "every recipient is a
    /// source" primitive. Returns a ticket addressed to THIS node.
    ///
    /// No filesystem round-trip: `blob_publish { scope, path }` was the only way back in, and it
    /// re-imported bytes the store already held, producing a third copy with nothing to reclaim it
    /// (#80).
    ///
    /// **Completeness is checked first, and it is load-bearing.** Recording a hash in a scope
    /// ADVERTISES it: the gate authorizes GETs for it and the returned ticket names us as the
    /// source. `Blobs::has` is true only for `BlobStatus::Complete`, so an interrupted fetch's
    /// partial bytes are refused exactly like absent ones — advertising what we cannot serve would
    /// convert the publisher going offline into a hang at every fetcher.
    ///
    /// Idempotent (the scope's hash set is a set).
    ///
    /// **Do NOT call this unconditionally after every fetch.** Republishing into a scope
    /// re-exposes the hash to every principal that scope ALREADY grants — including a hash an
    /// operator deliberately withdrew with `blob_unpublish`, which removes reachability but not
    /// the bytes, so `has()` stays true forever and a later republish silently restores access with
    /// no grant call and no warning. Republish when the user asks to share, not as fetch hygiene.
    ///
    /// **Grants nobody.** The republisher chooses a scope they already control; inheriting the
    /// original publisher's grant list would be a silent authorization transfer. Sharing is
    /// `blob_grant`'s job.
    pub async fn republish(&self, scope: &str, hash_hex: &str) -> Result<(String, String)> {
        // #104: hold the membership lock across the completeness CHECK and the scope INSERT. They
        // are a read-check-write with an `.await` between them, so a concurrent `blob_unpublish`
        // landing in the gap was silently undone — both verbs returned success and the operator's
        // revocation vanished.
        //
        // What this does NOT do: make a revocation unloseable. The mutex gives mutual exclusion in
        // LOCK-ACQUISITION order, not request-arrival order, so an unpublish that acquires FIRST
        // still has its effect erased by a republish acquiring second — both returning success.
        // That residue is the same semantic hazard the doc comment above describes (republish
        // re-adds to a scope whose grants unpublish never touched); the lock removes the
        // atomicity bug, where a decision made BEFORE the unpublish landed AFTER it. Eliminating
        // the class needs state (a per-(scope, hash) revocation generation re-validated before the
        // insert), not exclusion — tracked separately.
        let _membership = self.hash_membership.lock().await;
        // Scope first: a typo'd scope must not report as a missing blob.
        if !self.scopes.has_scope(scope) {
            anyhow::bail!(crate::daemon::NoSuchBlobScope(scope.to_string()));
        }
        // Parse (panic-safe) AND NORMALIZE before touching the scope. The gate compares against
        // the canonical lowercase hex (`msg.request.hash.to_hex()`), so inserting the caller's raw
        // string would record an entry that authorizes nothing: `blob_list` would show the file as
        // shared, every fetcher would be denied, and `blob_unpublish` — which normalizes — could
        // never remove it. That is #62's silent-no-op defect re-entered from the other side.
        // `blob_publish` is safe only because it stores `tag.hash.to_hex()`.
        let hash = crate::blobs::parse_blob_hash(hash_hex)?;
        let canonical = hash.to_hex().to_string();
        // #80: pin the blob BEFORE the completeness check and hold the pin until the scope insert
        // lands. This is a read-check-write over bytes the scope table does not yet name, so on a
        // collecting node a sweep can land inside it — and the consequence here is worse than on
        // the publish path, which loses a file: republish would still return Ok with a ticket and
        // write the hash into the scope, leaving a PERMANENT entry advertising bytes the node
        // cannot serve. That is precisely what the completeness check exists to prevent ("a hang at
        // every fetcher"), and the entry then roots itself in `live_hashes` forever.
        //
        // Ordering matters: pin, THEN check. Checking first would leave the same window, only
        // narrower. A pin on a hash the store does not hold is harmless — the check below still
        // refuses.
        //
        // Best-effort: a store that cannot mint a temp tag is one where the check below is about to
        // fail anyway, and refusing a republish because the GC bookkeeping hiccuped would be worse
        // than the narrow window it closes.
        let _pin = self.store.tags().temp_tag(hash).await.ok();
        if !self.store.blobs().has(hash).await.unwrap_or(false) {
            anyhow::bail!(crate::daemon::NoSuchBlob(canonical));
        }
        // #107: a deliberate withdrawal outranks "we still hold the bytes". Checked INSIDE the
        // membership lock, so an unpublish that lands first cannot be overtaken — which is the
        // half a lock alone could never fix, since exclusion is in acquisition order, not
        // request-arrival order.
        if self.scopes.is_withdrawn(scope, &canonical) {
            anyhow::bail!(crate::daemon::BlobWithdrawn {
                scope: scope.to_string(),
                hash: canonical,
            });
        }
        #[cfg(test)]
        {
            let d = *self
                .republish_delay
                .lock()
                .expect("republish delay lock not poisoned");
            if let Some(d) = d {
                tokio::time::sleep(d).await;
            }
        }
        self.scopes.publish_hash(scope, &canonical)?;
        // Release BEFORE minting: `ticket_for` waits up to RELAY_READY_TIMEOUT (3s) for the relay
        // handshake, and production turns that wait on. Holding the membership lock across it
        // would block every concurrent `blob_unpublish` for the full 3s on a node whose handshake
        // has not completed — making the REVOCATION path pay for the publisher's latency, which is
        // backwards on a security surface. The insert above is the last thing the lock must cover.
        drop(_membership);
        Ok((self.ticket_for(hash).await.to_string(), canonical))
    }

    /// Mint a ticket for a hash this node holds, addressed to this node.
    ///
    /// Waits (bounded by [`RELAY_READY_TIMEOUT`]) for the endpoint to come online first, so the
    /// address carries the home-relay URL a fetcher needs across NAT (#83 ask 3). `mint_invite` has
    /// done this since #4; the blob path minted immediately, so a file published shortly after boot
    /// or after a network change could yield a direct-addresses-only ticket: LAN-dialable and
    /// NAT-dead. A CAP, not a fixed wait — production returns the instant the relay handshake
    /// completes, and the relay-disabled test preset simply falls through to direct addresses.
    async fn ticket_for(&self, hash: Hash) -> BlobTicket {
        if self.relay_wait.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = tokio::time::timeout(RELAY_READY_TIMEOUT, self.endpoint.online()).await;
        }
        BlobTicket::new(self.endpoint.addr(), hash, BlobFormat::Raw)
    }

    /// Turn the relay-ready wait ON. Boot calls this; nothing else should.
    /// Is the relay-ready wait on? Test-only — production sets it and never asks (#105).
    #[cfg(test)]
    pub(crate) fn relay_wait_enabled(&self) -> bool {
        self.relay_wait.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Turn the relay-ready wait ON. Boot calls this; nothing else should.
    pub(crate) fn enable_relay_wait(&self) {
        self.relay_wait
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Grant a scope to a STABLE principal — a group name, a user_id, or an `eid:` device
    /// principal (never a display nickname, #38) — persisted single-writer.
    pub fn grant(&self, scope: &str, principal: &str) -> Result<()> {
        self.scopes.grant(scope, principal)
    }

    /// Revoke `principals` from every scope (unpair hygiene, #38) — persisted single-writer.
    /// Returns whether anything changed.
    pub fn revoke_principals(&self, principals: &[String]) -> Result<bool> {
        self.scopes.revoke_principals(principals)
    }

    /// Revoke `principals` from ONE scope (#62, `blob_revoke`) — the per-file un-share, the blob
    /// analogue of #44. Distinct from [`revoke_principals`](Self::revoke_principals), which is
    /// unpair hygiene across every scope.
    pub fn revoke_from_scope(&self, scope: &str, principals: &[String]) -> Result<bool> {
        self.scopes.revoke_from_scope(scope, principals)
    }

    /// Does this scope exist? The handlers use it to reject an unknown scope rather than acking it.
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.has_scope(scope)
    }

    /// Remove a hash from ONE scope (#62, `blob_unpublish`).
    ///
    /// This is the AUTHORIZATION half and takes effect at once for NEW requests: the scope gate
    /// requires the hash to be listed in some scope, so a subsequent GET is refused at the request
    /// hook. The BYTES remain in the store until a background sweep reclaims them, and only a node
    /// that set `[blobs].gc_interval` runs one (#80) — so do not describe this to
    /// a user as deletion. A transfer already streaming is not interrupted.
    pub async fn unpublish(&self, scope: &str, hash_hex: &str) -> Result<bool> {
        // NORMALIZE FIRST (#107 review). Since #107 this call WRITES a persistent key into the
        // withdrawn set, so a non-canonical rendering no longer merely fails to match — it records
        // a junk entry that no `republish` will ever compare equal to, in a set nothing prunes.
        // The control socket normalizes before calling, but `AppBlobs` is public API of a
        // published crate, so a library consumer passing uppercase hex must not poison the
        // sidecar. `republish` already normalizes one function away.
        let canonical = crate::blobs::parse_blob_hash(hash_hex)?
            .to_hex()
            .to_string();
        // #104: same lock as `republish`, so a revocation cannot land inside a republish's
        // check-then-insert window and be overwritten by it.
        let _membership = self.hash_membership.lock().await;
        self.scopes.unpublish_hash(scope, &canonical)
    }

    /// TEST-ONLY: pause between the import and the scope insert (#104).
    #[cfg(test)]
    pub(crate) fn set_publish_delay(&self, d: std::time::Duration) {
        *self
            .publish_delay
            .lock()
            .expect("publish delay lock not poisoned") = Some(d);
    }

    /// TEST-ONLY: pause between the completeness check and the scope insert (#104).
    #[cfg(test)]
    pub(crate) fn set_republish_delay(&self, d: std::time::Duration) {
        *self
            .republish_delay
            .lock()
            .expect("republish delay lock not poisoned") = Some(d);
    }

    /// The current scope table (name, hashes, grants) for `list`.
    /// One filtered, bounded page of the scope table (#84b).
    pub fn list_page(
        &self,
        q: &crate::blobs::scope::ListQuery,
    ) -> anyhow::Result<crate::blobs::scope::ScopePage> {
        self.scopes.list_page(q)
    }

    pub fn list(&self) -> Vec<crate::blobs::scope::ScopeRow> {
        self.scopes.list()
    }

    /// Fetch a ticket THROUGH this endpoint over `APP_BLOB_ALPN`, streaming BLAKE3-verified bytes
    /// into `self.store` (the Downloader cannot dial a custom ALPN — see [`APP_BLOB_ALPN`]).
    /// Returns the verified hash. A provider that refuses this
    /// caller (accept-time 401 or request-time Permission) surfaces here as an `Err`.
    pub async fn fetch(&self, ticket_str: &str) -> Result<Hash> {
        self.fetch_from(ticket_str, &[]).await
    }

    /// [`fetch`](Self::fetch), with ADDITIONAL sources to try when the publisher does not answer
    /// (#83).
    ///
    /// Content addressing makes every recipient a potential source, and the single-address ticket
    /// made that unusable: a file shared with a room became unfetchable the moment the sender
    /// closed their laptop, even though other people in the room already held the identical
    /// verified bytes.
    ///
    /// **Order: the ticket's own address first, then `alternates` in the caller's order.** The
    /// publisher is the authoritative source and a live one costs nothing — no alternate is dialled
    /// at all when it answers.
    ///
    /// **The dials are HEDGED, the transfer is not.** A source that has not answered within
    /// [`HEDGE_DELAY`] is overtaken rather than waited out, up to [`MAX_IN_FLIGHT_DIALS`] at a time,
    /// so an unreachable head of the list costs about a second instead of a full `DIAL_TIMEOUT`.
    /// Exactly ONE transfer runs at a time: concurrent transfers of one hash would duplicate work
    /// and interleave two byte counters into the single shared progress state. Losing dials are
    /// never cancelled — they keep running and can win a later round, which is what stops a
    /// slow-but-live source from being re-dialled and abandoned once per round.
    ///
    /// **Substitution is impossible whoever answers.** `store.remote().fetch(conn, hash)` verifies
    /// the BLAKE3 hash from the ticket against the bytes as they stream, so an alternate can serve
    /// the blob or fail — it cannot serve a different one. What an alternate CAN do is refuse: it
    /// serves only hashes it has republished into a scope granting us, so an ungranted alternate
    /// answers a permission error and the fetch moves on to the next.
    ///
    /// **Every failure mode falls through**, not only an unreachable dial: a refusal, a source that
    /// does not hold the hash, a mid-stream reset, and a transfer that stalls past
    /// [`SOURCE_TRANSFER_TIMEOUT`] all move to the next source. The first version broke out of the
    /// loop as soon as a dial SUCCEEDED, which meant one online-but-ungranted peer ended the fetch
    /// with a live alternate untried — the ordinary case for a room where only some recipients
    /// republished.
    ///
    /// Errors carry the LAST failure with every source's count, rather than only the publisher's —
    /// "dial failed" for a blob nobody could serve is a misleading thing to hand a user.
    /// The ordered source list a [`fetch_from`](Self::fetch_from) would dial.
    ///
    /// `#[doc(hidden)]` — a TEST SEAM (#203). The dial itself needs a live provider, so without
    /// this a test could only assert `dialable_only` and hope this function calls it.
    #[doc(hidden)]
    pub fn sources_for_test(
        &self,
        ticket_str: &str,
        alternates: &[iroh::EndpointAddr],
    ) -> Result<Vec<iroh::EndpointAddr>> {
        let ticket: BlobTicket = ticket_str.parse().context("parse blob ticket")?;
        Ok(Self::sources(&ticket, alternates, true))
    }

    /// The endpoint id a ticket names as its publisher — source 0 of a fetch — so a caller can
    /// decide whether it may be dialled before [`fetch_from_sources`](Self::fetch_from_sources)
    /// does (#223).
    pub(crate) fn ticket_publisher(ticket_str: &str) -> Result<[u8; 32]> {
        let ticket: BlobTicket = ticket_str.parse().context("parse blob ticket")?;
        Ok(*ticket.addr().id.as_bytes())
    }

    /// The ordered set of endpoints a fetch will try: the ticket's publisher, then the caller's
    /// alternates, deduplicated by endpoint id.
    ///
    /// `with_publisher: false` leaves the publisher out (#223: the daemon's verb passes it for a
    /// publisher this node has REVOKED). It still seeds the dedup set, so an alternate naming the
    /// same endpoint cannot bring it back.
    fn sources(
        ticket: &BlobTicket,
        alternates: &[iroh::EndpointAddr],
        with_publisher: bool,
    ) -> Vec<iroh::EndpointAddr> {
        // The publisher first, then the caller's alternates.
        let mut sources: Vec<iroh::EndpointAddr> = Vec::with_capacity(1 + alternates.len());
        // #203: the ticket is a REMOTE party's claim and this is source 0 of a real dial, so it
        // gets the same filter every other dial path has had since 0.52.1. The `alternates` were
        // already filtered — they resolve through `stored_dial_addr` — and source 0 was not, which
        // is the FOURTH site of this shape after the invite, the attestation offer, and the roster
        // announce. Found by review, not by the audit that fixed the other three.
        if with_publisher {
            sources.push(crate::daemon::dial::dialable_only(ticket.addr().clone()));
        }
        // The ticket's address is already source 0. An alternate naming the SAME endpoint would
        // otherwise be dialled twice for one timeout each — natural when a caller names the
        // publisher to reach their OTHER devices, since a person expands to all of them.
        //
        // Deduped across the WHOLE list, not just against the ticket. Two `from` entries resolving
        // to one endpoint (a nickname and its `eid:`, or two people sharing a device) used to cost
        // two sequential timeouts; since the dials are hedged they would now be two SIMULTANEOUS
        // dials to one peer, which is the one shape this feature must not produce.
        let mut seen: std::collections::HashSet<_> = std::iter::once(ticket.addr().id).collect();
        sources.extend(alternates.iter().filter(|a| seen.insert(a.id)).cloned());
        sources
    }

    pub async fn fetch_from(
        &self,
        ticket_str: &str,
        alternates: &[iroh::EndpointAddr],
    ) -> Result<Hash> {
        self.fetch_from_sources(ticket_str, alternates, true).await
    }

    /// [`fetch_from`](Self::fetch_from), optionally WITHOUT dialling the ticket's publisher (#223).
    ///
    /// `blob_fetch` passes `with_publisher: false` for a publisher this node has revoked: the
    /// ticket is the remote party's claim, and a fetch is a dial like any other. With no publisher
    /// and no alternates the fetch fails as any exhausted fetch does ("no source to try").
    pub(crate) async fn fetch_from_sources(
        &self,
        ticket_str: &str,
        alternates: &[iroh::EndpointAddr],
        with_publisher: bool,
    ) -> Result<Hash> {
        let ticket: BlobTicket = ticket_str.parse().context("parse blob ticket")?;
        let sources = Self::sources(&ticket, alternates, with_publisher);
        let total = sources.len();

        // #82 ask 2: consume the progress stream instead of dropping it on the floor. Same
        // coalescing rule as the serving side — `GetProgressItem::Progress` arrives per chunk, so
        // an uncoalesced fetch would flood the ring exactly as an uncoalesced serve would.
        //
        // `bytes_total` is NOT known here: the fetch side learns the size only as bytes arrive, so
        // the frame carries `None` and a consumer renders an indeterminate bar until `Completed`.
        // Reporting the ticket's hash as a size, or guessing, would be worse than saying so.
        let mut st = TransferProgressState {
            hash: ticket.hash().to_hex().to_string(),
            peer: None,
            total: None,
            done: 0,
            last_emitted: 0,
            epochs: 0,
            in_epoch: 0,
        };
        // `Started` fires ONCE, before any source is tried — not per attempt. A subscriber is
        // watching one logical transfer; emitting a Started/Aborted pair per failed source would
        // make a UI show a file failing several times before it succeeded.
        if let Some(b) = &self.transfers {
            emit_fetch(b, &st, mcpmesh_local_api::BlobTransferState::Started);
        }

        let mut last: Option<anyhow::Error> = None;
        let mut plan = HedgePlan::new(total);
        // The in-flight dials live HERE, not inside the racer, so a failed transfer resumes over
        // connections already in progress. See `HedgePlan` for what re-dialling them cost instead.
        let mut flight: DialFlight<'_> = n0_future::FuturesUnordered::new();
        // Dials race; the TRANSFER does not. Exactly one transfer runs at a time, so `st` keeps its
        // single writer and every progress guarantee above holds unchanged — two concurrent
        // transfers would interleave two byte counters into one stream, which a consumer renders as
        // a progress bar jumping backwards.
        loop {
            let Some((i, conn)) = self
                .race_a_connection(&sources, &mut plan, &mut flight, &mut last)
                .await
            else {
                break; // every source dialled, none answered
            };
            // EVERY failure mode falls through to the next source, not just an unreachable dial.
            // An early version broke out as soon as a dial SUCCEEDED, so a source that was online
            // but had not republished the hash — which answers a permission refusal post-connect —
            // ended the whole fetch with a live alternate sitting untried. That is the ordinary case
            // for the room-of-eight this feature exists for: some recipients republished, some did
            // not. Caught by review, by execution — and it has to survive the hedging rewrite,
            // which is why the transfer failing RESUMES the racer rather than returning.
            match self.transfer_from(conn, &ticket, &mut st).await {
                Ok(()) => {
                    if i > 0 {
                        tracing::info!(
                            source = i,
                            of = total,
                            "app-blob fetch fell back to an alternate source"
                        );
                    }
                    if let Some(b) = &self.transfers {
                        emit_fetch(b, &st, mcpmesh_local_api::BlobTransferState::Completed);
                    }
                    return Ok(ticket.hash());
                }
                Err(e) => {
                    tracing::debug!(source = i, of = total, %e, "app-blob source failed");
                    last = Some(e);
                }
            }
        }
        // Only now is the transfer over. ONE terminal frame, after every source has been tried.
        if let Some(b) = &self.transfers {
            emit_fetch(b, &st, mcpmesh_local_api::BlobTransferState::Aborted);
        }
        let e = last.unwrap_or_else(|| anyhow::anyhow!("no source to try"));
        Err(e.context(format!(
            "fetch app blob (tried {total} source{})",
            if total == 1 { "" } else { "s" }
        )))
    }

    /// Race sources for the FIRST connection, hedged (#83 follow-up).
    ///
    /// Returns the winning source's index and its connection. **`flight` is the caller's**, so the
    /// dials that lost are still running when this returns — which is the whole point: a failed
    /// TRANSFER resumes the race over connections already in progress rather than re-dialling
    /// sources it has just thrown away.
    ///
    /// The schedule is [`HedgePlan`]'s; this adds the two things that make it a race:
    ///
    /// - a source starts when a slot is free and a [`HEDGE_DELAY`] has passed, so a slow source is
    ///   overtaken rather than waited out;
    /// - a FAILED dial refills its slot at once rather than at the next tick, because the whole
    ///   point is to not spend a `DIAL_TIMEOUT` per source.
    ///
    /// **The first source is never delayed.** It starts before any hedge fires, so a live publisher
    /// is connected long before one does and the common case opens exactly ONE connection. That is
    /// the property answering #83's "every abandoned dial is work on someone else's machine", and
    /// `a_live_first_source_is_the_only_peer_dialled` is what holds it.
    ///
    /// Failures are folded into `last` so a total failure reports a real error rather than the
    /// racer's own "nobody answered".
    async fn race_a_connection<'a>(
        &'a self,
        sources: &'a [iroh::EndpointAddr],
        plan: &mut HedgePlan,
        flight: &mut DialFlight<'a>,
        last: &mut Option<anyhow::Error>,
    ) -> Option<(usize, iroh::endpoint::Connection)> {
        use n0_future::StreamExt as _;
        let dial = |i: usize| -> BoxedDial<'a> {
            let addr = sources[i].clone();
            Box::pin(async move {
                let r = tokio::time::timeout(
                    crate::daemon::dial::DIAL_TIMEOUT,
                    self.endpoint.connect(addr, APP_BLOB_ALPN),
                )
                .await
                .map_err(|_| anyhow::anyhow!("dial timed out"))
                .and_then(|r| r.context("dial app-blob provider"));
                (i, r)
            })
        };

        if flight.is_empty()
            && let Some(i) = plan.start()
        {
            flight.push(dial(i));
        }
        loop {
            if plan.exhausted() && flight.is_empty() {
                return None;
            }
            let hedge = tokio::time::sleep(HEDGE_DELAY);
            tokio::select! {
                Some((i, r)) = flight.next() => {
                    plan.finished();
                    match r {
                        Ok(conn) => return Some((i, conn)),
                        Err(e) => {
                            tracing::debug!(source = i, %e, "app-blob dial failed");
                            *last = Some(e);
                            // Refill the freed slot NOW rather than on the next tick.
                            if let Some(next) = plan.start() {
                                flight.push(dial(next));
                            }
                        }
                    }
                }
                _ = hedge, if plan.may_start() => {
                    if let Some(next) = plan.start() {
                        flight.push(dial(next));
                    }
                }
                else => return None,
            }
        }
    }

    /// Stream one blob over an already-established connection. `Ok` only on a completed, verified
    /// transfer.
    ///
    /// Bounded by [`SOURCE_TRANSFER_TIMEOUT`], because a source that accepts and then stalls would
    /// otherwise hold the fetch open forever with live alternates untried. Progress is reported into
    /// the SHARED `st`, so a transfer that resumes on a later source continues the same counter
    /// rather than restarting a consumer's progress bar.
    async fn transfer_from(
        &self,
        conn: iroh::endpoint::Connection,
        ticket: &BlobTicket,
        st: &mut TransferProgressState,
    ) -> Result<()> {
        use n0_future::StreamExt as _;
        let transfer = async {
            let mut stream =
                std::pin::pin!(self.store.remote().fetch(conn, ticket.hash()).stream());
            // Starts as an ERROR, not Ok: the old `.complete()` returned `LocalFailure("stream
            // closed without result")` when the stream ended with neither Done nor Error.
            // Defaulting to Ok turned that into a silent success — a fail-OPEN where the previous
            // code failed closed (#82 gate). Practically unreachable; the direction is the point.
            let mut outcome: Result<()> =
                Err(anyhow::anyhow!("fetch stream closed without a result"));
            while let Some(item) = stream.next().await {
                match item {
                    iroh_blobs::api::remote::GetProgressItem::Progress(done) => {
                        st.done = done;
                        if let Some(b) = &self.transfers
                            && st.done.saturating_sub(st.last_emitted) >= st.stride()
                        {
                            st.note_emitted();
                            emit_fetch(b, st, mcpmesh_local_api::BlobTransferState::Progress);
                        }
                    }
                    iroh_blobs::api::remote::GetProgressItem::Done(_) => outcome = Ok(()),
                    iroh_blobs::api::remote::GetProgressItem::Error(e) => {
                        // `{e:#}` keeps the GetError source chain that `.context(..)` used to
                        // preserve; `{e}` alone flattened it to the outermost message.
                        outcome = Err(anyhow::anyhow!("{e:#}"));
                    }
                }
            }
            outcome
        };
        tokio::time::timeout(SOURCE_TRANSFER_TIMEOUT, transfer)
            .await
            .map_err(|_| anyhow::anyhow!("transfer stalled"))?
    }

    /// Broadcast a terminal `Aborted` for a fetch that was STOPPED rather than finished (#172).
    ///
    /// [`fetch`](Self::fetch) emits every transfer frame from inside its progress loop, and
    /// cancellation works by dropping that future — so the loop simply stops, and a subscriber's
    /// progress bar would sit at its last `Progress` value forever with no terminal frame. The
    /// cancel path emits one from outside instead. `bytes_done` is not carried: the count lives in
    /// the dropped future, and reporting a stale one would be worse than reporting none.
    pub fn emit_fetch_aborted(&self, hash_hex: &str) {
        if let Some(b) = &self.transfers {
            let _ = b.send(crate::daemon::BlobTransfer {
                direction: mcpmesh_local_api::BlobDirection::Fetch,
                hash: hash_hex.to_string(),
                bytes_done: 0,
                bytes_total: None,
                state: mcpmesh_local_api::BlobTransferState::Aborted,
                peer: None,
            });
        }
    }

    /// The hash a `mcpmesh/blob/1` ticket names, read WITHOUT dialing anything (#172).
    ///
    /// `blob_fetch` needs the hash before it starts, not after: the hash is the cancellation key,
    /// so resolving it only from [`fetch`](Self::fetch)'s return value would leave the whole dial +
    /// transfer — the slow part, and the part worth cancelling — unaddressable.
    pub fn ticket_hash(ticket_str: &str) -> Result<Hash> {
        let ticket: BlobTicket = ticket_str.parse().context("parse blob ticket")?;
        Ok(ticket.hash())
    }

    /// Read a fully-present blob's bytes out of the store (callers/tests consume the fetched content).
    pub async fn read_bytes(&self, hash: Hash) -> Result<Bytes> {
        self.store
            .get_bytes(hash)
            .await
            .context("read fetched app blob")
    }

    /// STREAM a blob from the store to `dest`, returning the bytes written (#82).
    ///
    /// Peak memory is independent of blob size. The `read_bytes` + `fs::write` path this replaces
    /// materialized the whole blob as one `Bytes` first — and `get_bytes`' own iroh doc warns it
    /// *"will run out of memory when called for very large blobs"*. On a small headless node a
    /// multi-GB fetch was an OOM kill rather than a slow transfer.
    ///
    /// `ExportMode::Copy` (via `export`) writes an independent file, so the destination survives a
    /// later store reclaim. `ExportMode::TryReference` would avoid the second copy but ties the
    /// exported file's lifetime to the store — a separate decision, see #82's item 3.
    pub async fn export_to(&self, hash: Hash, dest: &Path) -> Result<u64> {
        self.store
            .blobs()
            .export(hash, dest)
            .await
            .with_context(|| format!("export app blob to {}", dest.display()))
    }
}

/// Which `blob_fetch` status to record, and whether to record at all (#84a fourth review).
///
/// Derived from the DECISION, never from a flag that excludes one variant: an earlier version used
/// `!matches!(decision, Err(RateLimited))`, so a GET refused with `Permission` was audited as a
/// successful fetch. The wire answer was right and the audit trail — the surface an operator
/// investigates with — lied.
///
/// `None` means "say nothing": a budget refusal is reported ONCE per endpoint until it fetches
/// successfully again. Refusals are cheap now that they precede any bytes, so recording every one
/// trades an uplink DoS for an audit-log DoS (measured ~2250 records/s).
fn audit_status(
    decision: &Result<(), AbortReason>,
    endpoint: Option<mcpmesh_net::EndpointId>,
    reported: &mut HashSet<mcpmesh_net::EndpointId>,
) -> Option<&'static str> {
    match decision {
        Err(AbortReason::Permission) => Some("denied"),
        Err(AbortReason::RateLimited) => match endpoint {
            // Already told the operator about this peer; stay quiet until it recovers.
            Some(eid) if !reported.insert(eid) => None,
            _ => Some("rate_limited"),
        },
        Ok(()) => {
            if let Some(eid) = endpoint {
                reported.remove(&eid); // recovered: a future refusal is news again
            }
            Some("ok")
        }
    }
}

/// The full GET-admission decision: authz first, then budget (#84a review).
///
/// Exists because pinning `request_budget_ok` alone left the CRITICAL fix unpinned — deleting the
/// budget check from the GET arm passed every test while a probe measured the full 94x regression.
/// That is verbatim the critique this branch made of the event mask, and it applied to the fix
/// itself.
fn get_admission(
    allow: bool,
    endpoint: Option<&mcpmesh_net::EndpointId>,
    limits: &crate::limits::MeshLimiters,
) -> Result<(), AbortReason> {
    if !allow {
        return Err(AbortReason::Permission);
    }
    // An unattributable connection is an ATTRIBUTION failure, not a budget one — same rule and
    // same reason code as `throttle_decision`. Reporting RateLimited here would tell a peer
    // "try again later" about a condition that will never clear.
    let Some(eid) = endpoint else {
        return Err(AbortReason::Permission);
    };
    if !request_budget_ok(Some(eid), limits) {
        return Err(AbortReason::RateLimited);
    }
    Ok(())
}

/// Is there budget to ADMIT a new GET request (#84a review)?
///
/// Separate from [`throttle_decision`] because the per-chunk hook is not sufficient on its own:
/// iroh-blobs writes the chunk BEFORE the hook runs, and a refusal resets only the stream, so a
/// peer that ignores the abort collects one free chunk per request forever. This is the gate that
/// runs before any bytes.
///
/// Reserves [`IROH_CHUNK_BYTES`] rather than peeking: a zero-cost check always passes, and
/// reserving makes an opened-but-undrained request cost the peer something. The side effect worth
/// knowing: the budget therefore also caps GETs at about `blob_bytes_per_min / 16384` per minute
/// REGARDLESS of blob size — a 4 MiB/min budget is ~256 fetches/min even for 100-byte blobs.
///
/// **Fails CLOSED** on `None` (no `ClientConnected` record), matching [`throttle_decision`].
fn request_budget_ok(
    endpoint: Option<&mcpmesh_net::EndpointId>,
    limits: &crate::limits::MeshLimiters,
) -> bool {
    // FAIL CLOSED on an unattributable connection, matching `throttle_decision`. The first version
    // used `is_none_or`, i.e. the inverse of its sibling's documented rule — masked today because
    // the caller short-circuits on `!allow`, but a latent trap for the next edit (#84a review).
    endpoint.is_some_and(|eid| limits.admit_blob_bytes(eid, IROH_CHUNK_BYTES))
}

/// The app-blob byte-budget decision for one CHUNK, mid-transfer (#84a).
///
/// The top-up to [`request_budget_ok`], not the gate: iroh-blobs writes the chunk before this
/// runs, and a refusal resets only the stream. Pure, so both rules are testable without a live
/// transfer; the async arm is a thin shell over it.
fn throttle_decision(
    endpoint: Option<&mcpmesh_net::EndpointId>,
    size: u64,
    limits: &crate::limits::MeshLimiters,
) -> Result<(), AbortReason> {
    match endpoint {
        // FAIL CLOSED. A chunk for a connection we cannot attribute must not be metered against
        // nobody — that is the same bypass as metering per connection, by another route.
        // `ClientConnected` already refuses an endpoint-less connection, so reaching here means
        // something is wrong and the safe answer is to refuse.
        None => Err(AbortReason::Permission),
        // Over budget: RateLimited, never Permission. The peer IS authorized; pacing failed.
        // Conflating them would make a bandwidth event read as an authz denial in the audit trail,
        // and iroh-blobs documents RateLimited as "OK to try again later" — which is true here and
        // false for a permission failure.
        Some(eid) if !limits.admit_blob_bytes(eid, size) => Err(AbortReason::RateLimited),
        Some(_) => Ok(()),
    }
}

/// The request-time scope Intercept drain loop (the security core). Single-consumer: this
/// task owns `rx`, so the `connection_id → endpoint_id` map is loop-local with NO lock
/// — FIFO delivery guarantees `ClientConnected(conn)` precedes any
/// `GetRequestReceived(conn)` on that connection. SECURITY-CRITICAL:
///  - `ClientConnected`: record the AUTHENTICATED `endpoint_id` (QUIC/TLS) → reply `Ok(())` to admit
///    (the accept-time gate already vetted the endpoint; the GET hook is the per-hash boundary). A
///    missing endpoint id (never on an authenticated conn) is denied defensively.
///  - `GetRequestReceived`: resolve the endpoint via the trust gate to its identity and ALLOW iff a
///    scope contains the hash AND grants one of the caller's principals — `groups ∪ {eid} ∪
///    {user_id}`, the shared `principal_set` (nicknames excluded, #38) — else `Permission`,
///    BEFORE any bytes (the Intercept path blocks the transfer on the provider's `rx.await??`).
///  - get_many/observe/push (all routed through `mask.get`): DENY
///    explicitly — deny-by-default, the store is not a general filesystem surface. Belt-and-suspenders
///    with `APP_BLOB_EVENT_MASK`, which ALSO pins these types (get_many/push = `Disabled`, observe =
///    `Intercept`): if a future iroh-blobs delivers them as events instead of refusing at the mask,
///    they are still denied here.
fn spawn_gate_loop(
    mut rx: tokio::sync::mpsc::Receiver<ProviderMessage>,
    gate: Arc<dyn TrustGate>,
    scopes: Arc<ScopeStore>,
    audit: AuditSink,
    limits: Arc<crate::limits::MeshLimiters>,
    // #82 ask 2: where coalesced transfer progress goes. `None` in fixtures that do not care.
    transfers: Option<tokio::sync::broadcast::Sender<crate::daemon::BlobTransfer>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut conns: HashMap<u64, mcpmesh_net::EndpointId> = HashMap::new();
        // #84a review: endpoints already audited for a budget refusal. A refusal is CHEAP —
        // measured 2250 records/s — so recording every one trades an uplink DoS for an audit-log
        // DoS, which is strictly worse because the attacker no longer has to move bytes. The spec
        // said "first only, or a peer hammering the budget writes an unbounded audit log" (#88);
        // that had not shipped. Cleared when the endpoint next fetches successfully, so a peer
        // that recovers and re-offends is reported again.
        let mut budget_reported: HashSet<mcpmesh_net::EndpointId> = HashSet::new();
        while let Some(msg) = rx.recv().await {
            match msg {
                ProviderMessage::ClientConnected(msg) => {
                    let res = match msg.endpoint_id {
                        Some(eid) => {
                            conns.insert(msg.connection_id, (*eid.as_bytes()).into());
                            Ok(())
                        }
                        None => Err(AbortReason::Permission),
                    };
                    msg.tx.send(res).await.ok();
                }
                // #84a: meter BYTES per authenticated endpoint. The connection limiter counts
                // connections, which cannot see one granted peer re-pulling a 4 GB blob on each of
                // 60 connections a minute.
                //
                // `Throttle` names a CONNECTION, so the endpoint comes from the same loop-local
                // map `ClientConnected` populates — metering per connection would hand a peer a
                // fresh budget per connection, which IS the bypass.
                ProviderMessage::Throttle(msg) => {
                    let res =
                        throttle_decision(conns.get(&msg.connection_id), msg.size, limits.as_ref());
                    msg.tx.send(res).await.ok();
                }
                ProviderMessage::GetRequestReceived(msg) => {
                    // Resolve the authenticated caller for BOTH the authz decision and the audit
                    // attribution (peer is the gate-resolved identity, not self-asserted).
                    let identity = conns
                        .get(&msg.connection_id)
                        .and_then(|eid| gate.resolve(eid));
                    let hash_hex = msg.request.hash.to_hex().to_string();
                    let allow = msg.request.ranges.is_blob()
                        && identity.as_ref().is_some_and(|identity| {
                            // The grant namespace is THE flat principal set —
                            // groups ∪ {eid} ∪ {user_id} — via the ONE shared
                            // `principal_set` (same expansion as the mesh allow check and
                            // the plugin seam). Nicknames are deliberately EXCLUDED (#38):
                            // scope grants are written as stable principals at grant time,
                            // so a pairing-mode peer is granted (and fetches) by its
                            // `eid:` device principal; legacy nickname-audience grants
                            // stop matching BY DESIGN (the doctor lint + release notes
                            // cover the migration). Default-deny is untouched: an unlisted
                            // principal still gets `Permission` before any bytes.
                            let eid = identity.endpoint.principal();
                            let principals: HashSet<&str> = mcpmesh_local_api::principal_set(
                                Some(&eid),
                                identity.user_id.as_deref(),
                                &identity.groups,
                            )
                            .into_iter()
                            .collect();
                            scopes.snapshot().allows(&hash_hex, &principals)
                        });
                    // Audit the fetch: peer + hash + status (ok/denied). A COUNT/ref only —
                    // never blob content. Attributes to the resolved user_id/nickname, or "unknown".
                    let peer = identity
                        .as_ref()
                        .map(|i| i.user_id.clone().unwrap_or_else(|| i.name.clone()));
                    // #84a: enforce the byte budget HERE, before any bytes, as well as per chunk.
                    // The per-chunk `Throttle` hook fires AFTER iroh-blobs has written the chunk,
                    // and a refusal resets only the STREAM — the connection survives and nothing
                    // bounds requests per connection. So a peer ignoring the abort gets one free
                    // ~16 KiB chunk per request, indefinitely: measured at ~1800x the configured
                    // rate from a single connection. Refusing the REQUEST is what bounds that.
                    //
                    // Reserves one chunk rather than peeking: a zero-cost check would always pass
                    // (`tokens >= 0.0`), and reserving means a peer that opens many requests it
                    // never drains still pays for them. Evaluated ONCE — calling it twice would
                    // double-charge.
                    let decision =
                        get_admission(allow, conns.get(&msg.connection_id), limits.as_ref());
                    // #84a: a refusal is REPORTED, not silent — the issue's complaint was that
                    // mcpmesh "neither refuses it nor reports it happened". But only the FIRST per
                    // endpoint until it succeeds again: see `budget_reported`.
                    let conn_eid = conns.get(&msg.connection_id).copied();
                    // Derived from the DECISION, not by excluding one variant. The first version
                    // computed `budget_ok = !matches!(decision, Err(RateLimited))`, so a GET
                    // refused with `Permission` — an unattributable connection — was audited as a
                    // successful fetch. The wire answer was right; the audit trail lied, which is
                    // the surface an operator investigates with (#84a third review).
                    let status = audit_status(&decision, conn_eid, &mut budget_reported);
                    if let Some(status) = status {
                        audit.record(AuditRecord::blob_fetch(
                            now_ts(),
                            peer,
                            hash_hex,
                            status.into(),
                            // #57 second surface: the record of who fetched which BYTES is the
                            // one where two-devices-one-nickname is most likely the actual
                            // question — attribute the authenticated endpoint, not the display
                            // name alone.
                            conn_eid.map(|eid| eid.principal()),
                        ));
                    }
                    let admitted = decision.is_ok();
                    msg.tx.send(decision).await.ok();
                    // #82 ask 2: `InterceptLog` hands us this request's transfer-event stream.
                    // Drained in its OWN task — draining inline would block the gate loop for the
                    // whole transfer, and the gate loop is what authorizes every OTHER request.
                    // Only for an ADMITTED request: a refused one transfers nothing, so its stream
                    // yields nothing and a `Started` frame would be a lie.
                    // The update receiver MUST be consumed, whether or not anyone wants the
                    // frames. Dropping it makes the provider's own `transfer_started` send fail,
                    // which ABORTS the transfer — an admitted, authorized fetch then errors with
                    // "fetch app blob". Only spawning this when a broadcast existed is exactly that
                    // bug: every fixture built with `transfers: None` broke.
                    if admitted {
                        let bcast = transfers.clone();
                        let peer_principal = conn_eid.map(|eid| eid.principal());
                        // Drained in its OWN task: doing it inline would block the gate loop —
                        // which authorizes every OTHER request — for the whole transfer. The
                        // receiver's type is irpc-internal, so it is captured rather than named.
                        //
                        // DETACHED, and `shutdown()` does not join it (#82 gate). It holds only a
                        // broadcast sender clone and this request's receiver, so it ends when the
                        // transfer does or when the provider drops its side — it holds no store or
                        // redb handle, which is what `shutdown`'s determinism guarantee is about.
                        // An in-flight one can still outlive `Node::shutdown` by the length of a
                        // transfer; tracking them belongs with the cancellation work in #172.
                        let mut updates = msg.rx;
                        tokio::spawn(async move {
                            let mut st = None;
                            while let Ok(Some(update)) = updates.recv().await {
                                // Drained unconditionally; only the FRAMES are optional.
                                let terminal = match &bcast {
                                    Some(b) => {
                                        apply_transfer_update(&mut st, &update, b, &peer_principal)
                                    }
                                    None => matches!(
                                        update,
                                        iroh_blobs::provider::events::RequestUpdate::Completed(_)
                                            | iroh_blobs::provider::events::RequestUpdate::Aborted(
                                                _
                                            )
                                    ),
                                };
                                if terminal {
                                    return;
                                }
                            }
                            // The stream ended with no terminal event (peer vanished, tracker
                            // dropped). A consumer waiting on Completed/Aborted would hang, so
                            // synthesize Aborted rather than leave a transfer open forever.
                            if let (Some(b), Some(cur)) = (&bcast, st.as_ref()) {
                                emit_transfer(
                                    b,
                                    cur,
                                    mcpmesh_local_api::BlobTransferState::Aborted,
                                );
                            }
                        });
                    }
                }
                // Deny-by-default for every non-GET request type.
                ProviderMessage::GetManyRequestReceived(msg) => {
                    msg.tx.send(Err(AbortReason::Permission)).await.ok();
                }
                ProviderMessage::PushRequestReceived(msg) => {
                    msg.tx.send(Err(AbortReason::Permission)).await.ok();
                }
                ProviderMessage::ObserveRequestReceived(msg) => {
                    msg.tx.send(Err(AbortReason::Permission)).await.ok();
                }
                ProviderMessage::ConnectionClosed(msg) => {
                    conns.remove(&msg.connection_id);
                }
                _ => {}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        HedgePlan, MAX_IN_FLIGHT_DIALS, PROGRESS_STRIDE_BYTES, TransferProgressState,
        apply_transfer_update,
    };

    /// #203: a blob ticket's OWN address is filtered before it becomes source 0 of a dial.
    ///
    /// The ticket is a remote party's claim. The `alternates` were already filtered — they resolve
    /// through `stored_dial_addr` — but source 0 was not, so a ticket naming `0.0.0.0:53` or a
    /// multicast group reached `Endpoint::connect` untouched. Fourth site of this shape, after the
    /// pairing invite, the attestation offer and the roster announce.
    ///
    /// Asserted on the SOURCE LIST rather than on `dialable_only`: the helper passing says nothing
    /// about whether this call site invokes it, which is how the previous three attempts in this
    /// area shipped unpinned.
    #[tokio::test]
    async fn a_blob_tickets_own_address_is_filtered_before_it_is_dialled() {
        let dir = tempfile::tempdir().unwrap();
        let ep = crate::daemon::boot::build_endpoint(
            iroh::SecretKey::from_bytes(&[44u8; 32]),
            &crate::config::NetworkCfg {
                relay_mode: "disabled".into(),
                ..Default::default()
            },
            false,
            None,
        )
        .await
        .unwrap();
        let blobs = AppBlobs::open_fetcher(dir.path().join("b"), ep)
            .await
            .unwrap();

        // A ticket whose address carries one real entry and three that can never be a peer.
        let provider = iroh::SecretKey::from_bytes(&[45u8; 32]).public();
        let hash = iroh_blobs::Hash::new(b"x");
        let addr = iroh::EndpointAddr::from_parts(
            provider,
            [
                iroh::TransportAddr::Ip("0.0.0.0:53".parse().unwrap()),
                iroh::TransportAddr::Ip("224.0.0.1:1900".parse().unwrap()),
                iroh::TransportAddr::Ip("192.168.9.9:4433".parse().unwrap()),
                iroh::TransportAddr::Ip("255.255.255.255:80".parse().unwrap()),
            ],
        );
        let ticket = BlobTicket::new(addr, hash, iroh_blobs::BlobFormat::Raw);

        let sources = blobs.sources_for_test(&ticket.to_string(), &[]).unwrap();
        assert_eq!(sources.len(), 1, "one source (the ticket's): {sources:?}");
        assert_eq!(
            sources[0].addrs.len(),
            1,
            "only the dialable address may reach the dial: {:?}",
            sources[0]
        );
        assert_eq!(sources[0].id, provider, "and it still names the provider");
    }

    /// The common case must be untouched: ONE source means one dial and no hedging at all.
    ///
    /// #83's review declined a blind parallel race because "every abandoned dial is work on someone
    /// else's machine". A fetch with no alternates must not pay anything for a feature it is not
    /// using.
    #[test]
    fn a_single_source_starts_once_and_is_then_exhausted() {
        let mut p = HedgePlan::new(1);
        assert_eq!(p.start(), Some(0));
        assert!(!p.may_start(), "nothing left to start: {p:?}");
        assert!(!p.exhausted(), "source 0 is still dialling: {p:?}");
        p.finished();
        assert!(p.exhausted(), "{p:?}");
    }

    /// Sources enter in the CALLER's order — the publisher (index 0) first — and the in-flight cap
    /// holds at every step rather than only at the end.
    #[test]
    fn sources_start_in_order_and_the_cap_holds_throughout() {
        let mut p = HedgePlan::new(8);
        for expected in 0..MAX_IN_FLIGHT_DIALS {
            assert_eq!(p.start(), Some(expected), "{p:?}");
        }
        assert!(
            !p.may_start(),
            "the cap is {MAX_IN_FLIGHT_DIALS}, so a 4th must not start: {p:?}"
        );
        assert_eq!(p.start(), None, "{p:?}");
        // A slot frees exactly one more, never a burst.
        p.finished();
        assert_eq!(p.start(), Some(MAX_IN_FLIGHT_DIALS), "{p:?}");
        assert_eq!(p.start(), None, "back at the cap: {p:?}");
    }

    /// Every source is eventually tried, and none twice — the walk terminates.
    #[test]
    fn every_source_is_started_exactly_once() {
        let mut p = HedgePlan::new(7);
        let mut seen = Vec::new();
        while let Some(i) = p.start() {
            seen.push(i);
            p.finished();
        }
        assert_eq!(seen, vec![0, 1, 2, 3, 4, 5, 6], "{p:?}");
        assert!(p.exhausted(), "{p:?}");
    }

    /// **The bug this plan was rewritten to prevent, and the reason no dial is ever cancelled.**
    ///
    /// A first cut requeued the rivals a winner displaced. That reads like carefulness and is the
    /// opposite: a source alive but more than a `HEDGE_DELAY` away loses every race it enters, so
    /// it was re-dialled and abandoned once per round. For the room-of-eight this feature exists
    /// for — alternates that answer fast but have not republished the hash — that is 15 dials
    /// against the sequential walk's 8, with the publisher abandoned seven times. It made the
    /// "abandoned dials" cost the design was built to bound STRICTLY WORSE than doing nothing.
    ///
    /// The rule that replaced it: a source is started at most once, ever. Losers keep running and
    /// win a later round.
    #[test]
    fn no_source_is_ever_started_twice_however_the_rounds_fall() {
        let mut p = HedgePlan::new(8);
        let mut seen = Vec::new();
        // Round after round: fill to the cap, let one "win", and keep going. The winner and the
        // losers alike are never re-offered.
        while !p.exhausted() {
            while let Some(i) = p.start() {
                seen.push(i);
            }
            p.finished(); // one dial completes; its rivals stay in flight
            if seen.len() >= 8 {
                // Drain the rest so `exhausted` can become true.
                for _ in 0..MAX_IN_FLIGHT_DIALS {
                    p.finished();
                }
            }
        }
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen,
            vec![0, 1, 2, 3, 4, 5, 6, 7],
            "every source exactly once, none twice: {p:?}"
        );
    }

    /// A dial that completes retires its source whether it won or failed — the plan never re-offers
    /// it, so the outer loop cannot spin.
    #[test]
    fn a_completed_dial_is_not_reoffered() {
        let mut p = HedgePlan::new(2);
        assert_eq!(p.start(), Some(0));
        p.finished();
        assert_eq!(p.start(), Some(1));
        p.finished();
        assert_eq!(p.start(), None, "both are spent: {p:?}");
        assert!(p.exhausted(), "{p:?}");
    }

    /// No sources at all is exhausted immediately rather than a loop that never starts anything.
    #[test]
    fn an_empty_plan_is_exhausted() {
        let mut p = HedgePlan::new(0);
        assert!(p.exhausted(), "{p:?}");
        assert_eq!(p.start(), None);
    }

    use iroh_blobs::provider::events::{RequestUpdate, TransferProgress, TransferStarted};
    use mcpmesh_local_api::BlobTransferState as S;

    fn started(size: u64) -> RequestUpdate {
        RequestUpdate::Started(TransferStarted {
            index: 0,
            hash: iroh_blobs::Hash::new(b"blob"),
            size,
        })
    }
    fn progress(end_offset: u64) -> RequestUpdate {
        RequestUpdate::Progress(TransferProgress { end_offset })
    }

    /// Drive a sequence of updates and return every frame that came out.
    fn frames_for(size: u64, chunk: u64) -> Vec<crate::daemon::BlobTransfer> {
        let (tx, mut rx) = tokio::sync::broadcast::channel(4096);
        let mut st = None;
        let peer = Some("eid:abc".to_string());
        apply_transfer_update(&mut st, &started(size), &tx, &peer);
        let mut at = 0;
        while at < size {
            at = (at + chunk).min(size);
            apply_transfer_update(&mut st, &progress(at), &tx, &peer);
        }
        apply_transfer_update(
            &mut st,
            &RequestUpdate::Completed(iroh_blobs::provider::events::TransferCompleted {
                stats: Box::new(iroh_blobs::provider::TransferStats {
                    payload_bytes_sent: 0,
                    other_bytes_sent: 0,
                    other_bytes_read: 0,
                    duration: std::time::Duration::ZERO,
                }),
            }),
            &tx,
            &peer,
        );
        let mut out = Vec::new();
        while let Ok(f) = rx.try_recv() {
            out.push(f);
        }
        out
    }

    /// #82 ask 2: COALESCING is the property that keeps the ring usable.
    ///
    /// iroh-blobs reports progress per ~16 KiB chunk. A 4 GiB transfer is ~262k updates; emitting a
    /// frame for each would overrun a bounded ring many times over and every subscriber would see
    /// `Lagged`, losing the audit records that share their stream.
    #[test]
    fn progress_frames_are_coalesced_not_one_per_chunk() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let chunks = 4 * GIB / (16 * 1024);
        let frames = frames_for(4 * GIB, 16 * 1024);

        assert!(
            frames.len() <= 110,
            "a 4 GiB transfer produced {} frames from {chunks} chunks — the stride must bound this \
             to ~102 (Started + ~100 Progress + Completed), or every subscriber lags out",
            frames.len()
        );
        assert!(
            frames.len() >= 3,
            "…but it must still report PROGRESS, not just start and end: {}",
            frames.len()
        );
        assert_eq!(frames.first().unwrap().state, S::Started);
        assert_eq!(frames.last().unwrap().state, S::Completed);
        assert!(
            frames
                .windows(2)
                .all(|w| w[0].bytes_done <= w[1].bytes_done),
            "bytes_done must never go backwards"
        );
        assert_eq!(
            frames.last().unwrap().bytes_done,
            4 * GIB,
            "Completed must carry the FINAL count — the last Progress is skipped by the stride, so \
             a consumer treating it as the total would stop short of 100%"
        );
    }

    /// #82: `Completed` reports the FINAL count even when the last `Progress` fell short.
    ///
    /// The stride skips the tail of a transfer, and a provider need not emit a progress event for
    /// the final chunk — so a consumer that renders the last `Progress` as the total stops short of
    /// 100% and the bar never fills. Asserted with a deliberately LAGGING last progress, because a
    /// fixture whose chunks land exactly on the size makes this a no-op: the first version of this
    /// test did that and the mutation escaped.
    #[test]
    fn completed_reports_the_total_even_when_the_last_progress_lagged() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        let mut st = None;
        apply_transfer_update(&mut st, &started(1000), &tx, &None);
        apply_transfer_update(&mut st, &progress(400), &tx, &None);
        apply_transfer_update(
            &mut st,
            &RequestUpdate::Completed(iroh_blobs::provider::events::TransferCompleted {
                stats: Box::new(iroh_blobs::provider::TransferStats {
                    payload_bytes_sent: 0,
                    other_bytes_sent: 0,
                    other_bytes_read: 0,
                    duration: std::time::Duration::ZERO,
                }),
            }),
            &tx,
            &None,
        );
        let frames: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        let last = frames.last().unwrap();
        assert_eq!(last.state, S::Completed);
        assert_eq!(
            last.bytes_done, 1000,
            "Completed must report the total, not the 400 the last Progress reached — otherwise \
             the consumer's bar stops at 40% on a fully successful transfer"
        );
    }

    /// A SMALL blob must still get its Started/Completed pair — a progress bar needs both ends even
    /// when no Progress frame ever clears the stride.
    #[test]
    fn a_small_transfer_still_reports_both_ends() {
        let frames = frames_for(1024, 512);
        assert_eq!(frames.first().unwrap().state, S::Started);
        assert_eq!(frames.last().unwrap().state, S::Completed);
        assert_eq!(frames.last().unwrap().bytes_done, 1024);
        assert_eq!(
            frames.first().unwrap().bytes_total,
            Some(1024),
            "bytes_total is known from Started onward"
        );
        assert_eq!(
            frames.first().unwrap().peer.as_deref(),
            Some("eid:abc"),
            "the SERVING side attributes the stable principal (#38), never a nickname"
        );
    }

    /// The stride scales with size, so a big transfer does not emit proportionally more frames.
    #[test]
    fn the_stride_scales_with_the_transfer_size() {
        let small = TransferProgressState {
            hash: "h".into(),
            peer: None,
            total: Some(1024),
            done: 0,
            last_emitted: 0,
            epochs: 0,
            in_epoch: 0,
        };
        assert_eq!(
            small.stride(),
            PROGRESS_STRIDE_BYTES,
            "a tiny transfer floors at the fixed stride rather than emitting per byte"
        );
        let big = TransferProgressState {
            total: Some(4 * 1024 * 1024 * 1024),
            ..small
        };
        assert_eq!(
            big.stride(),
            4 * 1024 * 1024 * 1024 / 100,
            "a big one uses 1% so the frame COUNT stays bounded instead of the byte gap"
        );
    }

    /// #82 gate: the FETCH side never learns the total, so the stride must widen on its own.
    ///
    /// It did not: `stride()` fell to the fixed 1 MiB floor forever, so a 4 GiB fetch emitted ~4098
    /// frames into a 256-deep ring — the direction #82 is actually about was the one still
    /// flooding, while three doc sites claimed "~102 whatever its size".
    #[test]
    fn an_unknown_total_still_bounds_the_frame_count() {
        const GIB: u64 = 1024 * 1024 * 1024;
        let mut st = TransferProgressState {
            hash: "h".into(),
            peer: None,
            total: None, // every fetch
            done: 0,
            last_emitted: 0,
            epochs: 0,
            in_epoch: 0,
        };
        let mut frames = 0u32;
        let mut at = 0u64;
        let chunk = 16 * 1024;
        while at < 4 * GIB {
            at += chunk;
            st.done = at;
            if st.done.saturating_sub(st.last_emitted) >= st.stride() {
                st.note_emitted();
                frames += 1;
            }
        }
        assert!(
            frames <= 200,
            "a 4 GiB FETCH emitted {frames} progress frames into a {}-deep ring — the stride must \
             widen when the total is unknown, or a subscriber lags out on the very transfer this \
             feature exists to show",
            256
        );
        assert!(
            frames >= 20,
            "…but it must still report meaningfully often: {frames}"
        );
    }

    /// #82: a transfer that ends without a terminal event must still be reported ABORTED — a
    /// consumer waiting on Completed/Aborted would otherwise wait forever, which is the "stalled is
    /// indistinguishable from slow" complaint.
    #[test]
    fn an_aborted_transfer_is_reported_not_silently_dropped() {
        let (tx, mut rx) = tokio::sync::broadcast::channel(16);
        let mut st = None;
        apply_transfer_update(&mut st, &started(4096), &tx, &None);
        let terminal = apply_transfer_update(
            &mut st,
            &RequestUpdate::Aborted(iroh_blobs::provider::events::TransferAborted {
                stats: Box::new(iroh_blobs::provider::TransferStats {
                    payload_bytes_sent: 0,
                    other_bytes_sent: 0,
                    other_bytes_read: 0,
                    duration: std::time::Duration::ZERO,
                }),
            }),
            &tx,
            &None,
        );
        assert!(terminal, "Aborted must end the drain");
        let frames: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].state, S::Aborted);
    }

    /// #84a fourth review: the three audit statuses, and the dedup.
    ///
    /// All three of these survived a fully green 595-test suite as mutations: recording "ok" for a
    /// Permission refusal (a verbatim regression of the blocker the previous round fixed), and
    /// deleting the dedup entirely. Nothing asserted a "denied" or "rate_limited" blob_fetch
    /// record anywhere in the tree — not even the pre-existing "denied".
    #[test]
    fn the_audit_status_follows_the_decision_and_reports_a_refusal_once() {
        use mcpmesh_net::EndpointId;
        let eid = EndpointId::from_bytes([2u8; 32]);
        let mut seen = HashSet::new();

        // An authz refusal is "denied" — NOT "ok". This is the exact regression the third review
        // caught: status derived by excluding RateLimited recorded a Permission refusal as success.
        assert_eq!(
            super::audit_status(&Err(AbortReason::Permission), Some(eid), &mut seen),
            Some("denied"),
            "a refused GET must never be audited as a successful fetch"
        );
        // Unattributable connections take the same path.
        assert_eq!(
            super::audit_status(&Err(AbortReason::Permission), None, &mut seen),
            Some("denied")
        );

        // A budget refusal is distinct from an authz denial, and reported ONCE.
        assert_eq!(
            super::audit_status(&Err(AbortReason::RateLimited), Some(eid), &mut seen),
            Some("rate_limited"),
            "the first refusal is news — the issue's complaint was that nothing reported it"
        );
        for _ in 0..500 {
            assert_eq!(
                super::audit_status(&Err(AbortReason::RateLimited), Some(eid), &mut seen),
                None,
                "and every later one is silent — refusals are cheap, so recording each would \
                 trade an uplink DoS for an audit-log DoS (~2250 records/s measured)"
            );
        }

        // Recovering re-arms the report, so an ongoing attack is not invisible forever.
        assert_eq!(
            super::audit_status(&Ok(()), Some(eid), &mut seen),
            Some("ok")
        );
        assert_eq!(
            super::audit_status(&Err(AbortReason::RateLimited), Some(eid), &mut seen),
            Some("rate_limited"),
            "a peer that recovered and re-offended must be reported again"
        );

        // A second endpoint is tracked independently.
        let other = EndpointId::from_bytes([3u8; 32]);
        assert_eq!(
            super::audit_status(&Err(AbortReason::RateLimited), Some(other), &mut seen),
            Some("rate_limited")
        );
    }

    /// #84a fourth review: `request_budget_ok` must fail CLOSED on an unattributable connection.
    ///
    /// Reverting it to `is_none_or` — fail OPEN, the inverse of `throttle_decision`'s rule —
    /// survived the whole suite, because every other test passes `Some(..)` and the two forms
    /// differ only on `None`.
    #[test]
    fn request_budget_ok_fails_closed_on_an_unattributable_connection() {
        use crate::config::LimitsCfg;
        use crate::limits::MeshLimiters;

        // Even with NO budget configured, an unattributable connection is refused: fail-closed is
        // about attribution, not about the budget being on.
        let off = MeshLimiters::from_config(&LimitsCfg::default());
        assert!(
            !super::request_budget_ok(None, &off),
            "a connection with no ClientConnected record must be refused — metering it against \
             nobody is the per-connection bypass by another route"
        );

        let on = MeshLimiters::from_config(&LimitsCfg {
            blob_bytes_per_min: super::IROH_CHUNK_BYTES * 4,
            ..Default::default()
        });
        assert!(!super::request_budget_ok(None, &on));
    }

    /// #84a review: GET admission must consult the budget, not only authz.
    ///
    /// This is THE critical fix, and nothing pinned it: deleting the budget check from the GET arm
    /// passed every test while a probe measured 3 MB delivered against a 32 KiB/min budget (94x).
    /// Testing `request_budget_ok` in isolation proved the helper worked, not that anything called
    /// it — the same vacuity this branch called out in the event mask.
    #[test]
    fn get_admission_refuses_on_budget_as_well_as_authz() {
        use crate::config::LimitsCfg;
        use crate::limits::MeshLimiters;
        use mcpmesh_net::EndpointId;

        let eid = EndpointId::from_bytes([6u8; 32]);
        let lim = MeshLimiters::from_config(&LimitsCfg {
            blob_bytes_per_min: super::IROH_CHUNK_BYTES * 2,
            ..Default::default()
        });

        // Authz denial wins and reports Permission, whatever the budget says.
        assert!(matches!(
            super::get_admission(false, Some(&eid), &lim),
            Err(AbortReason::Permission)
        ));

        // An authorized caller is admitted until its budget is spent, then RateLimited — before
        // any bytes. Two chunks of budget = two admissions.
        assert!(super::get_admission(true, Some(&eid), &lim).is_ok());
        assert!(super::get_admission(true, Some(&eid), &lim).is_ok());
        assert!(
            matches!(
                super::get_admission(true, Some(&eid), &lim),
                Err(AbortReason::RateLimited)
            ),
            "an over-budget REQUEST must be refused before any bytes — metering only per chunk \
             let a peer take one free chunk per request forever"
        );

        // Unattributable: fail closed, and as an authz failure rather than a budget one.
        assert!(matches!(
            super::get_admission(true, None, &lim),
            Err(AbortReason::Permission)
        ));
    }

    /// #84a review: the documented floor must actually serve a blob.
    ///
    /// The first version of this doc told operators the minimum was 16384 — which admits a request
    /// (reserving one chunk) and then has nothing left for the transfer's own chunks, so it serves
    /// zero bytes. A doc that recommends the value it warns against is worse than no doc.
    #[test]
    fn the_documented_minimum_budget_admits_a_request_and_a_chunk() {
        use crate::config::LimitsCfg;
        use crate::limits::MeshLimiters;
        use mcpmesh_net::EndpointId;

        let eid = EndpointId::from_bytes([8u8; 32]);
        const DOCUMENTED_MIN: u64 = 32_768;

        let lim = MeshLimiters::from_config(&LimitsCfg {
            blob_bytes_per_min: DOCUMENTED_MIN,
            ..Default::default()
        });
        assert!(
            super::request_budget_ok(Some(&eid), &lim),
            "the documented minimum must admit a request"
        );
        assert!(
            super::throttle_decision(Some(&eid), super::IROH_CHUNK_BYTES, &lim).is_ok(),
            "and must still have budget for the first CHUNK — otherwise the value we tell \
             operators to use serves zero bytes, which is the state the doc warns against"
        );

        // A sub-floor value is FLOORED, not honoured (#84a fourth review). Documenting a floor
        // and not enforcing it left an operator with a daemon that silently capped every servable
        // blob at `budget - 16384` bytes; the repo idiom is `max_sessions.max(1)`.
        let floored = MeshLimiters::from_config(&LimitsCfg {
            blob_bytes_per_min: super::IROH_CHUNK_BYTES, // one chunk: below the floor
            ..Default::default()
        });
        assert!(super::request_budget_ok(Some(&eid), &floored));
        assert!(
            super::throttle_decision(Some(&eid), super::IROH_CHUNK_BYTES, &floored).is_ok(),
            "a sub-floor budget must be raised to a usable one, not honoured into a daemon that \
             admits a request and then truncates every blob"
        );
    }

    /// #84a review: the default mask must be UNCHANGED, and the metered one must differ in
    /// exactly one field.
    ///
    /// Nothing pinned this: mutating the code to always use the metered mask survived the whole
    /// suite, because the only tests that could notice are network suites that flake on this
    /// machine. A const assertion is deterministic and instant.
    #[test]
    fn the_metered_mask_differs_from_the_default_in_throttle_alone() {
        let d = super::APP_BLOB_EVENT_MASK;
        let m = super::APP_BLOB_EVENT_MASK_METERED;

        assert_eq!(
            d.throttle,
            ThrottleMode::None,
            "a deployment with no budget must not arm the per-chunk intercept"
        );
        assert_eq!(m.throttle, ThrottleMode::Intercept);

        // Every OTHER field identical — the metered mask must not relax an authz decision.
        assert_eq!(d.connected, m.connected, "connect gate");
        assert_eq!(d.get, m.get, "the GET scope gate");
        assert_eq!(d.get_many, m.get_many, "get_many stays denied");
        assert_eq!(d.push, m.push, "push stays denied");
        assert_eq!(d.observe, m.observe, "observe stays intercepted");
    }

    /// #84a review: the budget must refuse the REQUEST, not only the chunk.
    ///
    /// The per-chunk hook fires after iroh-blobs has written the chunk, and a `RateLimited` abort
    /// resets only the stream — the connection survives and nothing caps requests per connection.
    /// Measured before this gate existed: ~1800x the configured rate from ONE connection, because
    /// every new request collected a free ~16 KiB chunk. Metering only per chunk does not bound an
    /// adversarial peer, only a polite one.
    #[test]
    fn a_request_is_refused_once_the_endpoint_budget_is_spent() {
        use crate::config::LimitsCfg;
        use crate::limits::MeshLimiters;
        use mcpmesh_net::EndpointId;

        let eid = EndpointId::from_bytes([4u8; 32]);
        // Exactly two chunks of budget.
        let lim = MeshLimiters::from_config(&LimitsCfg {
            blob_bytes_per_min: super::IROH_CHUNK_BYTES * 2,
            ..Default::default()
        });

        assert!(super::request_budget_ok(Some(&eid), &lim), "first request");
        assert!(super::request_budget_ok(Some(&eid), &lim), "second request");
        assert!(
            !super::request_budget_ok(Some(&eid), &lim),
            "the THIRD request must be refused before any bytes — metering only per chunk lets a \
             peer take one free chunk per request forever, which is ~1800x the budget in practice"
        );

        // With no budget configured, admission is never blocked.
        let off = MeshLimiters::from_config(&LimitsCfg::default());
        for _ in 0..100 {
            assert!(super::request_budget_ok(Some(&eid), &off));
        }
    }

    /// #84a: the two rules that decide whether a chunk goes out.
    ///
    /// Extracted as a pure function because the live path is an async irpc arm firing per ~16 KiB
    /// chunk — pinning these through a real transfer is how a test ends up asserting nothing.
    #[test]
    fn a_chunk_is_refused_over_budget_and_when_it_cannot_be_attributed() {
        use crate::config::LimitsCfg;
        use crate::limits::MeshLimiters;
        use mcpmesh_net::EndpointId;

        let eid = EndpointId::from_bytes([1u8; 32]);
        let lim = MeshLimiters::from_config(&LimitsCfg {
            blob_bytes_per_min: 32_768, // == the enforced floor (two chunks)
            ..Default::default()
        });

        // 32768 == two chunks, so two fit and the third does not.
        assert!(
            super::throttle_decision(Some(&eid), 16_384, &lim).is_ok(),
            "the first chunk is inside the budget"
        );
        assert!(
            super::throttle_decision(Some(&eid), 16_384, &lim).is_ok(),
            "and the second"
        );
        assert!(
            matches!(
                super::throttle_decision(Some(&eid), 16_384, &lim),
                Err(AbortReason::RateLimited)
            ),
            "over budget must be RateLimited — the peer IS authorized and pacing failed, so \
             reporting Permission would put a bandwidth event in the audit trail as an authz denial"
        );

        // FAIL CLOSED: a chunk we cannot attribute is refused, not waved through.
        assert!(
            matches!(
                super::throttle_decision(None, 16_384, &lim),
                Err(AbortReason::Permission)
            ),
            "an unattributable chunk must be REFUSED — metering it against nobody is the same \
             bypass as metering per connection"
        );

        // With no budget configured nothing is metered, but an unattributable chunk is STILL
        // refused: fail-closed is about attribution, not about the budget being on.
        let off = MeshLimiters::from_config(&LimitsCfg::default());
        assert!(super::throttle_decision(Some(&eid), u64::MAX, &off).is_ok());
        assert!(
            super::throttle_decision(None, 1, &off).is_err(),
            "fail-closed does not depend on a budget being configured"
        );
    }

    use super::*;
    use crate::blobs::APP_BLOB_ALPN;
    use crate::blobs::scope::ScopeStore;
    use mcpmesh_net::{EndpointId, PeerIdentity, StaticGate};
    use std::sync::Arc;

    /// #83: republishing a hash the store does NOT hold COMPLETE must fail, and must leave the
    /// scope untouched.
    ///
    /// Putting a hash in a scope ADVERTISES it — the gate will authorize GETs for it and the
    /// returned ticket names us as the source. Advertising bytes we cannot serve converts the
    /// original sender going offline into a hang at every fetcher, which is strictly worse than the
    /// failure #83 reports. Partial bytes (an interrupted fetch leaves them) must fail the same way
    /// as absent ones, which is why the predicate is `Blobs::has` (true only for
    /// `BlobStatus::Complete`) rather than "do we know this hash".
    #[tokio::test]
    async fn republishing_a_blob_we_do_not_hold_fails_and_leaves_the_scope_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let provider = AppBlobs::open_fetcher(dir.path().join("blobs"), ep().await)
            .await
            .unwrap();
        provider.grant("room", "b64u:alice").unwrap();

        // A well-formed hash the store has never seen.
        let absent = blake3::hash(b"never fetched").to_hex().to_string();
        let err = provider
            .republish("room", &absent)
            .await
            .expect_err("republishing a blob we do not hold must fail");
        assert!(
            err.downcast_ref::<crate::daemon::NoSuchBlob>().is_some(),
            "must be NoSuchBlob so the client can tell it apart from a bad scope, got: {err}"
        );
        let hashes: Vec<String> = provider
            .list()
            .into_iter()
            .flat_map(|(_, hashes, _, _)| hashes)
            .collect();
        assert!(
            !hashes.contains(&absent),
            "a FAILED republish must not half-advertise the hash, got {hashes:?}"
        );
    }

    /// The check ORDER: an unknown scope reports `NoSuchBlobScope`, even when the hash is also
    /// absent. A typo'd scope must not be reported as a missing blob — the client's remedy differs.
    #[tokio::test]
    async fn an_unknown_scope_outranks_a_missing_blob() {
        let dir = tempfile::tempdir().unwrap();
        let provider = AppBlobs::open_fetcher(dir.path().join("blobs"), ep().await)
            .await
            .unwrap();
        let absent = blake3::hash(b"nope").to_hex().to_string();
        let err = provider
            .republish("no-such-scope", &absent)
            .await
            .expect_err("unknown scope must fail");
        assert!(
            err.downcast_ref::<crate::daemon::NoSuchBlobScope>()
                .is_some(),
            "an unknown scope outranks a missing blob, got: {err}"
        );
    }

    /// #83's exact scenario, end to end: a fetched blob becomes servable FROM THE FETCHER, and a
    /// third peer gets it while the ORIGINAL PUBLISHER IS OFFLINE.
    ///
    /// "Someone posts a file to a room of eight and closes their laptop." Before republish, the
    /// only address anyone held pointed at the sleeping publisher, so the remaining peers failed
    /// even though complete, byte-identical bytes sat on three machines.
    ///
    /// B is a GATED provider (`AppBlobs::load`), which is what makes this test mean anything. An
    /// ungated fetcher serves every hash it holds, so the scope insert republish performs is never
    /// exercised and the test passes with republish recording nothing — verified by mutation.
    #[tokio::test]
    async fn a_fetched_blob_is_servable_from_the_fetcher_after_the_publisher_goes_away() {
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            let c_ep = ep().await;
            let c_eid = EndpointId::from_bytes(*c_ep.id().as_bytes());
            let mut entries = HashMap::new();
            entries.insert(
                c_eid,
                PeerIdentity {
                    endpoint: c_eid,
                    name: "carol".into(),
                    user_id: Some("carol".into()),
                    groups: vec![],
                },
            );
            let b_gate: Arc<dyn mcpmesh_net::TrustGate> = Arc::new(StaticGate::new(entries));

            // A publishes (ungated — A's gate is not what is under test).
            let adir = tempfile::tempdir().unwrap();
            let a_ep = ep().await;
            let a = AppBlobs::open_fetcher(adir.path().join("blobs"), a_ep.clone())
                .await
                .unwrap();
            a.spawn_accept(&a_ep);
            let src = adir.path().join("shared.bin");
            std::fs::write(&src, b"the file everyone wants").unwrap();
            let (a_ticket, hash_hex) = a.publish_path(&src).await.unwrap();

            // B fetches it, and is GATED when it serves.
            let bdir = tempfile::tempdir().unwrap();
            let b_ep = ep().await;
            let b = AppBlobs::load(
                bdir.path().join("blobs"),
                Arc::new(ScopeStore::new(bdir.path().join("scopes.json"))),
                b_gate,
                b_ep.clone(),
                crate::audit::AuditSink::disabled(),
                crate::limits::MeshLimiters::unlimited(),
                None,
                None,
            )
            .await
            .unwrap();
            b.spawn_accept(&b_ep);
            b.fetch(&a_ticket).await.unwrap();

            // B republishes into a scope IT controls and grants C.
            b.grant("b-room", "carol").unwrap();
            let (b_ticket, _canon) = b.republish("b-room", &hash_hex).await.unwrap();
            assert_ne!(b_ticket, a_ticket, "the ticket must name B, not A");

            // A goes away — the laptop closes.
            a_ep.close().await;

            // C fetches from B regardless.
            let cdir = tempfile::tempdir().unwrap();
            let c = AppBlobs::open_fetcher(cdir.path().join("blobs"), c_ep)
                .await
                .unwrap();
            let got = c
                .fetch(&b_ticket)
                .await
                .expect("C must fetch from B with A offline — the whole point of #83");
            assert_eq!(
                &c.read_bytes(got).await.unwrap()[..],
                b"the file everyone wants"
            );
        })
        .await
        .expect("republish round-trip timed out");
    }

    /// Republish must NOT inherit the original publisher's grants. A principal A shared with, but
    /// B did not, is refused by B — otherwise republishing would silently widen access to everyone
    /// the previous holder had shared with, which no one asked for and no one would see.
    #[tokio::test]
    async fn republish_does_not_inherit_the_publishers_grants() {
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            let m_ep = ep().await;
            let m_eid = EndpointId::from_bytes(*m_ep.id().as_bytes());
            let mut entries = HashMap::new();
            entries.insert(
                m_eid,
                PeerIdentity {
                    endpoint: m_eid,
                    name: "mallory".into(),
                    user_id: Some("mallory".into()),
                    groups: vec![],
                },
            );
            let b_gate: Arc<dyn mcpmesh_net::TrustGate> = Arc::new(StaticGate::new(entries));

            // A publishes and grants mallory.
            let adir = tempfile::tempdir().unwrap();
            let a_ep = ep().await;
            let a = AppBlobs::open_fetcher(adir.path().join("blobs"), a_ep.clone())
                .await
                .unwrap();
            a.spawn_accept(&a_ep);
            let src = adir.path().join("f.bin");
            std::fs::write(&src, b"a's file").unwrap();
            let (a_ticket, hash_hex) = a.publish_path(&src).await.unwrap();
            a.grant("a-room", "mallory").unwrap();

            // B fetches and republishes into ITS scope, granting nobody.
            let bdir = tempfile::tempdir().unwrap();
            let b_ep = ep().await;
            let b = AppBlobs::load(
                bdir.path().join("blobs"),
                Arc::new(ScopeStore::new(bdir.path().join("scopes.json"))),
                b_gate,
                b_ep.clone(),
                crate::audit::AuditSink::disabled(),
                crate::limits::MeshLimiters::unlimited(),
                None,
                None,
            )
            .await
            .unwrap();
            b.spawn_accept(&b_ep);
            b.fetch(&a_ticket).await.unwrap();
            b.grant("b-room", "someone-else").unwrap();
            let (b_ticket, _canon) = b.republish("b-room", &hash_hex).await.unwrap();

            // mallory — granted by A, never by B — is refused by B.
            let mdir = tempfile::tempdir().unwrap();
            let mallory = AppBlobs::open_fetcher(mdir.path().join("blobs"), m_ep)
                .await
                .unwrap();
            // A DENIED fetch does not fail fast (the gate refuses at accept and the fetcher
            // retries), so bound it: both "errored" and "never completed" are denials — only
            // SUCCESS is a failure of this property.
            let res =
                tokio::time::timeout(std::time::Duration::from_secs(10), mallory.fetch(&b_ticket))
                    .await;
            assert!(
                !matches!(res, Ok(Ok(_))),
                "republishing must not transfer A's grants to B's copy — that would silently widen \
                 access to everyone the previous holder shared with (got {res:?})"
            );
        })
        .await
        .expect("grant-isolation test timed out");
    }

    /// #83 review: a NON-CANONICAL rendering of a hash must not create an entry that authorizes
    /// nothing. The gate compares against canonical lowercase hex, so recording the caller's raw
    /// string (a valid 52-char base32 form, or uppercase hex) would put a row in `blob_list` that
    /// looks shared, denies every fetcher, and cannot be removed — `blob_unpublish` normalizes and
    /// would find nothing to delete, acking a no-op. That is #62's defect from the other side.
    #[tokio::test]
    async fn a_non_canonical_hash_is_normalized_before_it_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let provider = AppBlobs::open_fetcher(dir.path().join("blobs"), ep().await)
            .await
            .unwrap();
        provider.grant("room", "b64u:alice").unwrap();
        let src = dir.path().join("f.bin");
        std::fs::write(&src, b"canonical me").unwrap();
        let (_t, canonical) = provider.publish_path(&src).await.unwrap();

        // The SAME hash in its base32 rendering — what `Hash`'s Display produces, and a form a
        // client can legitimately hold. (Uppercase HEX is not an alternative spelling: iroh's
        // parser rejects it outright, which the review's own probe confirmed.)
        let parsed = crate::blobs::parse_blob_hash(&canonical).unwrap();
        let base32 = data_encoding::BASE32_NOPAD
            .encode(parsed.as_bytes())
            .to_ascii_lowercase();
        assert_ne!(base32, canonical, "the fixture must actually differ");
        let (_ticket, returned) = provider
            .republish("room", &base32)
            .await
            .expect("an alternative rendering of a held hash must republish");

        assert_eq!(
            returned, canonical,
            "the RESULT must carry canonical hex — blob_publish does, and the docs promise the two \
             are interchangeable"
        );
        let recorded: Vec<String> = provider
            .list()
            .into_iter()
            .filter(|(name, _, _, _)| name == "room")
            .flat_map(|(_, hashes, _, _)| hashes)
            .collect();
        assert_eq!(
            recorded,
            vec![canonical],
            "the SCOPE must record canonical hex — the gate compares against it, so a raw-string \
             entry would authorize nobody and be unremovable"
        );
    }

    /// #104: a `blob_unpublish` concurrent with a `blob_republish` must not be silently undone.
    ///
    /// `republish` is a read-check-write — it verifies completeness (an `.await`) and only then
    /// inserts. Without a lock spanning both, an unpublish landing in that gap removes the hash,
    /// republish then re-inserts it, and BOTH verbs report success: the operator was told the file
    /// was withdrawn while it is being served.
    ///
    /// Driven deterministically via the test-only delay seam rather than hoping for the
    /// interleaving. With the lock, unpublish blocks until republish finishes and therefore
    /// serializes AFTER it — the revocation is the last word, which is the outcome an operator
    /// expects. Without it, unpublish slips into the gap and is overwritten.
    #[tokio::test]
    async fn a_concurrent_unpublish_is_not_lost_to_a_republish() {
        // 120s: these fixtures bind real endpoints, which costs ~20s on a loaded machine, and the
        // guard exists to catch a HANG (a deadlock on the new membership lock), not slowness.
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            let dir = tempfile::tempdir().unwrap();
            let provider = AppBlobs::open_fetcher(dir.path().join("blobs"), ep().await)
                .await
                .unwrap();
            provider.grant("room", "b64u:alice").unwrap();
            let src = dir.path().join("f.bin");
            std::fs::write(&src, b"contested").unwrap();
            // Already published into the scope, so the unpublish below has something to remove.
            let (_t, hash_hex) = provider.publish_scope("room", &src).await.unwrap();

            provider.set_republish_delay(std::time::Duration::from_millis(600));
            let p2 = provider.clone();
            let h2 = hash_hex.clone();
            let republish =
                tokio::spawn(async move { p2.republish("room", &h2).await.map(|_| ()) });

            // Let republish get past its completeness check and into the gap.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let removed = provider.unpublish("room", &hash_hex).await.unwrap();
            republish.await.unwrap().unwrap();

            assert!(removed, "the unpublish must actually have removed the hash");
            let hashes: Vec<String> = provider
                .list()
                .into_iter()
                .flat_map(|(_, hashes, _, _)| hashes)
                .collect();
            assert!(
                !hashes.contains(&hash_hex),
                "the revocation must survive — a republish that overwrites a concurrent unpublish \
                 tells the operator the file was withdrawn while it is still being served (scope \
                 now holds {hashes:?})"
            );
        })
        .await
        .expect("republish/unpublish race test timed out");
    }

    /// #104: `publish_scope` takes the same membership lock, and nothing tested it — removing that
    /// lock alone passed the whole suite, so a refactor could drop it silently.
    ///
    /// Same mechanism as the republish race: `add_path` is a slow async import, and the scope
    /// insert that follows is unconditional. A `blob_unpublish` of a hash the import is about to
    /// re-add loses its effect. Reachable whenever two clients hold the same bytes — which is
    /// ordinary, since the hash is the content.
    #[tokio::test]
    async fn a_concurrent_unpublish_is_not_lost_to_a_publish() {
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            let dir = tempfile::tempdir().unwrap();
            let provider = AppBlobs::open_fetcher(dir.path().join("blobs"), ep().await)
                .await
                .unwrap();
            provider.grant("room", "b64u:alice").unwrap();
            let src = dir.path().join("f.bin");
            std::fs::write(&src, b"contested by publish").unwrap();
            let (_t, hash_hex) = provider.publish_scope("room", &src).await.unwrap();

            // Re-publishing the SAME bytes races an unpublish of the same hash.
            provider.set_publish_delay(std::time::Duration::from_millis(600));
            let p2 = provider.clone();
            let src2 = src.clone();
            let publish =
                tokio::spawn(async move { p2.publish_scope("room", &src2).await.map(|_| ()) });

            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let removed = provider.unpublish("room", &hash_hex).await.unwrap();
            publish.await.unwrap().unwrap();

            assert!(removed, "the unpublish must actually have removed the hash");
            let hashes: Vec<String> = provider
                .list()
                .into_iter()
                .flat_map(|(_, hashes, _, _)| hashes)
                .collect();
            assert!(
                !hashes.contains(&hash_hex),
                "a re-publish of identical bytes must not overwrite a concurrent revocation \
                 (scope now holds {hashes:?})"
            );
        })
        .await
        .expect("publish/unpublish race test timed out");
    }

    /// #105: the relay-ready wait is a CAP, and it actually RUNS.
    ///
    /// The first version of this test asserted neither. On a relay-disabled endpoint the minted
    /// ticket is byte-identical with and without the wait — no relay URL appears either way — so
    /// the ONLY observable difference is elapsed time. Deleting the wait from `ticket_for`
    /// entirely left both #105 tests passing (in 0.65s instead of 9.3s). Guarding the flag is not
    /// guarding the behaviour the flag exists to produce.
    ///
    /// Because `online()` never completes with relays disabled, an enabled wait MUST consume the
    /// full cap. So the elapsed time is a two-sided assertion: the lower bound fails if the wait
    /// is removed or skipped, the upper bound fails if it becomes unbounded or is lengthened.
    #[tokio::test]
    async fn the_relay_wait_actually_runs_and_is_capped() {
        let dir = tempfile::tempdir().unwrap();
        let provider_ep = ep().await;
        let provider = AppBlobs::open_fetcher(dir.path().join("blobs"), provider_ep.clone())
            .await
            .unwrap();
        // F3: pin the DEFAULT too. Without this, flipping `relay_wait`'s initial value to `true`
        // would make the boot guard in `boot.rs` stop failing when its one call is deleted — the
        // whole point of #105 would evaporate silently.
        assert!(
            !provider.relay_wait_enabled(),
            "the wait must default OFF — every hand-built fixture would otherwise pay the full cap \
             per mint, and the boot guard would stop guarding anything"
        );
        provider.enable_relay_wait();
        provider.spawn_accept(&provider_ep);

        let src = dir.path().join("capped.bin");
        std::fs::write(&src, b"capped").unwrap();

        let started = std::time::Instant::now();
        let published = provider.publish_path(&src).await.unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed >= crate::daemon::RELAY_READY_TIMEOUT,
            "the wait must actually RUN — `online()` never completes on a relay-disabled endpoint, \
             so an enabled wait consumes the full cap. Minting in {elapsed:?} means the wait was \
             skipped or removed"
        );
        assert!(
            elapsed < crate::daemon::RELAY_READY_TIMEOUT + std::time::Duration::from_secs(2),
            "and it must be CAPPED — minting took {elapsed:?}, so the bound is longer than \
             RELAY_READY_TIMEOUT or the wait is unbounded"
        );

        // F5: the fetch is bounded too — an unbounded one hangs the whole test binary with no
        // failing test name, since libtest has no per-test timeout.
        let cdir = tempfile::tempdir().unwrap();
        let caller = AppBlobs::open_fetcher(cdir.path().join("blobs"), ep().await)
            .await
            .unwrap();
        let hash = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            caller.fetch(&published.0),
        )
        .await
        .expect("fetch timed out")
        .expect("the fallback direct-address ticket must still round-trip");
        assert_eq!(&caller.read_bytes(hash).await.unwrap()[..], b"capped");
    }

    /// #107: the race #104's lock could NOT close. A mutex orders by ACQUISITION, not by request
    /// arrival, so an unpublish that acquires first is still erased by a republish acquiring
    /// second — both returning success, operator told the file was withdrawn while it is served.
    ///
    /// Closed with state rather than exclusion: unpublish records a withdrawal, and republish
    /// refuses it. Asserted in the ORDER THAT USED TO LOSE — unpublish completes first, then
    /// republish runs — which is exactly the interleaving a lock cannot help with.
    #[tokio::test]
    async fn a_completed_unpublish_is_not_undone_by_a_later_republish() {
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            let dir = tempfile::tempdir().unwrap();
            let provider = AppBlobs::open_fetcher(dir.path().join("blobs"), ep().await)
                .await
                .unwrap();
            provider.grant("room", "b64u:alice").unwrap();
            let src = dir.path().join("f.bin");
            std::fs::write(&src, b"withdrawn content").unwrap();
            let (_t, hash_hex) = provider.publish_scope("room", &src).await.unwrap();

            assert!(provider.unpublish("room", &hash_hex).await.unwrap());

            // The bytes are still in the store (#80: no reclaim), so `has()` is true and the ONLY
            // thing standing between the operator's revocation and its silent undoing is #107.
            let err = provider
                .republish("room", &hash_hex)
                .await
                .expect_err("a withdrawn hash must not republish");
            assert!(
                err.downcast_ref::<crate::daemon::BlobWithdrawn>().is_some(),
                "must be BlobWithdrawn so a client can tell it from 'fetch it first', got: {err}"
            );

            let hashes: Vec<String> = provider
                .list()
                .into_iter()
                .flat_map(|(_, hashes, _, _)| hashes)
                .collect();
            assert!(
                !hashes.contains(&hash_hex),
                "and the scope must still not list it (got {hashes:?})"
            );
        })
        .await
        .expect("durable revocation test timed out");
    }

    /// The deliberate re-share still works: `blob_publish` from a FILE clears the withdrawal, and
    /// a republish afterwards is allowed again. Without this, a withdrawal would be permanent and
    /// an operator could never re-share the same content into that scope.
    #[tokio::test]
    async fn publishing_from_the_file_again_lifts_the_withdrawal() {
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            let dir = tempfile::tempdir().unwrap();
            let provider = AppBlobs::open_fetcher(dir.path().join("blobs"), ep().await)
                .await
                .unwrap();
            provider.grant("room", "b64u:alice").unwrap();
            let src = dir.path().join("f.bin");
            std::fs::write(&src, b"re-shared on purpose").unwrap();
            let (_t, hash_hex) = provider.publish_scope("room", &src).await.unwrap();
            provider.unpublish("room", &hash_hex).await.unwrap();
            provider.republish("room", &hash_hex).await.unwrap_err();

            // The deliberate act: name the FILE again.
            provider.publish_scope("room", &src).await.unwrap();
            provider
                .republish("room", &hash_hex)
                .await
                .expect("after a deliberate re-publish, republish is allowed again");
        })
        .await
        .expect("un-withdraw test timed out");
    }

    /// Republish is idempotent (the scope hash set is a set), so a client may call it
    /// unconditionally after every fetch without special-casing the second time.
    #[tokio::test]
    async fn republishing_twice_is_not_an_error_and_records_one_entry() {
        let dir = tempfile::tempdir().unwrap();
        let provider = AppBlobs::open_fetcher(dir.path().join("blobs"), ep().await)
            .await
            .unwrap();
        provider.grant("room", "b64u:alice").unwrap();
        let src = dir.path().join("f.bin");
        std::fs::write(&src, b"dupe").unwrap();
        let (_t, hash_hex) = provider.publish_path(&src).await.unwrap();

        provider.republish("room", &hash_hex).await.unwrap();
        provider.republish("room", &hash_hex).await.unwrap();

        // Constrain the SCOPE NAME too: without it, a mutation inserting into a hardcoded scope,
        // or into every scope, passes.
        let rooms: Vec<(String, Vec<String>)> = provider
            .list()
            .into_iter()
            .map(|(name, hashes, _, _)| (name, hashes))
            .collect();
        assert_eq!(
            rooms,
            vec![("room".to_string(), vec![hash_hex.clone()])],
            "exactly one entry, in the NAMED scope, not two and not elsewhere"
        );
    }

    /// Lock the exact serving mask: single-blob GET is scope-checked (`Intercept`); every other
    /// request type is pinned to deny-by-default so the refusal does NOT rely on 0.103.0's
    /// `mask.get`-routes-all quirk. A regression that loosens any of these fails here.
    #[test]
    fn app_blob_event_mask_pins_non_get_request_types_to_deny_by_default() {
        assert_eq!(APP_BLOB_EVENT_MASK.connected, ConnectMode::Intercept);
        // #82 ask 2: `InterceptLog`, NOT `Intercept` — and the distinction is the security one
        // worth pinning. `InterceptLog` is Intercept PLUS transfer events, so the scope check that
        // authorizes every single-blob GET still runs. Anything that merely NOTIFIES
        // (`Notify`/`NotifyLog`) would give up the veto and serve bytes to an ungranted caller.
        assert_eq!(APP_BLOB_EVENT_MASK.get, RequestMode::InterceptLog);

        // get_many/push refuse at the protocol level with Permission (events.rs:504-506), no event.
        assert_eq!(APP_BLOB_EVENT_MASK.get_many, RequestMode::Disabled);
        assert_eq!(APP_BLOB_EVENT_MASK.push, RequestMode::Disabled);
        // observe has no `Disabled` variant; `Intercept` routes it to the drain loop's deny arm.
        assert_eq!(APP_BLOB_EVENT_MASK.observe, ObserveMode::Intercept);
        // throttle is a transfer knob, not a request gate — left at its default.
        assert_eq!(APP_BLOB_EVENT_MASK.throttle, ThrottleMode::None);
    }

    async fn ep() -> iroh::Endpoint {
        iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![APP_BLOB_ALPN.to_vec()])
            .bind()
            .await
            .expect("bind endpoint")
    }

    #[tokio::test]
    async fn ungated_fetcher_still_round_trips() {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let pdir = tempfile::tempdir().unwrap();
            let provider_ep = ep().await;
            let provider = AppBlobs::open_fetcher(pdir.path().join("blobs"), provider_ep.clone())
                .await
                .unwrap();
            provider.spawn_accept(&provider_ep);
            let src = pdir.path().join("p.bin");
            std::fs::write(&src, b"hello scopes").unwrap();
            let (ticket, _hash) = provider.publish_path(&src).await.unwrap();

            let cdir = tempfile::tempdir().unwrap();
            let caller_ep = ep().await;
            let caller = AppBlobs::open_fetcher(cdir.path().join("blobs"), caller_ep.clone())
                .await
                .unwrap();
            let hash = caller.fetch(&ticket).await.unwrap();
            assert_eq!(&caller.read_bytes(hash).await.unwrap()[..], b"hello scopes");
        })
        .await
        .expect("timed out");
    }

    #[tokio::test]
    async fn granted_caller_fetches_but_ungranted_and_uncontained_are_denied() {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            // Two callers: alice (granted) and bob (rostered but ungranted for this scope).
            let alice_ep = ep().await;
            let bob_ep = ep().await;
            let alice_id: EndpointId = alice_ep.id().into();
            let bob_id: EndpointId = bob_ep.id().into();

            // Provider gate resolves BOTH (both pass the accept-time gate); scope grants only alice.
            let mut entries = std::collections::HashMap::new();
            entries.insert(
                alice_id,
                PeerIdentity {
                    endpoint: [0u8; 32].into(),
                    name: "alice".into(),
                    user_id: Some("alice".into()),
                    groups: vec!["team-eng".into()],
                },
            );
            entries.insert(
                bob_id,
                PeerIdentity {
                    endpoint: [0u8; 32].into(),
                    name: "bob".into(),
                    user_id: Some("bob".into()),
                    groups: vec!["team-eng".into()],
                },
            );
            let gate: Arc<dyn mcpmesh_net::TrustGate> = Arc::new(StaticGate::new(entries));

            let pdir = tempfile::tempdir().unwrap();
            let scopes = Arc::new(ScopeStore::new(pdir.path().join("scopes.json")));
            let provider_ep = ep().await;
            let provider = AppBlobs::load(
                pdir.path().join("blobs"),
                scopes,
                gate,
                provider_ep.clone(),
                crate::audit::AuditSink::disabled(),
                crate::limits::MeshLimiters::unlimited(),
                None,
                None,
            )
            .await
            .unwrap();
            provider.spawn_accept(&provider_ep);

            // Publish into scope "docs" and grant it to the user_id "alice" ONLY (not team-eng).
            let src = pdir.path().join("secret.bin");
            std::fs::write(&src, b"top secret bytes").unwrap();
            let (ticket, _hash) = provider.publish_scope("docs", &src).await.unwrap();
            provider.grant("docs", "alice").unwrap();

            // GRANTED (alice) → fetch succeeds + verifies.
            let cdir = tempfile::tempdir().unwrap();
            let alice = AppBlobs::open_fetcher(cdir.path().join("a"), alice_ep.clone())
                .await
                .unwrap();
            let hash = alice.fetch(&ticket).await.expect("granted alice fetches");
            assert_eq!(
                &alice.read_bytes(hash).await.unwrap()[..],
                b"top secret bytes"
            );

            // UNGRANTED (bob — rostered, team-eng, but "docs" grants only alice) → the request hook
            // denies with Permission BEFORE any bytes; the fetch errors.
            let bob = AppBlobs::open_fetcher(cdir.path().join("b"), bob_ep.clone())
                .await
                .unwrap();
            let bob_res =
                tokio::time::timeout(std::time::Duration::from_secs(10), bob.fetch(&ticket)).await;
            assert!(
                matches!(bob_res, Ok(Err(_))),
                "ungranted bob is refused: {bob_res:?}"
            );
        })
        .await
        .expect("timed out");
    }

    /// The #38 inversion for the blob-scope gate — grants hold STABLE principals only:
    /// a PAIRING-MODE peer (unbound: `user_id: None`, no groups) granted by its `eid:`
    /// device principal CAN fetch; a peer whose only "grant" names its display NICKNAME
    /// is DENIED (nicknames are self-asserted/rewritable and never admit). Identities
    /// carry their REAL authenticated endpoint bytes so the eid arm is honest.
    #[tokio::test]
    async fn pairing_mode_eid_grant_admits_and_nickname_grant_stays_denied() {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let carol_ep = ep().await; // pairing-mode: granted by her eid: device principal
            let mallory_ep = ep().await; // "granted" only by nickname — must stay denied
            let carol_id: EndpointId = carol_ep.id().into();
            let mallory_id: EndpointId = mallory_ep.id().into();

            let mut entries = std::collections::HashMap::new();
            entries.insert(
                carol_id,
                PeerIdentity {
                    endpoint: carol_id, // the REAL authenticated bytes — the eid arm is honest
                    name: "carol".into(),
                    user_id: None, // no device→user binding — eid: is the ONLY principal
                    groups: vec![],
                },
            );
            entries.insert(
                mallory_id,
                PeerIdentity {
                    endpoint: mallory_id,
                    name: "mallory".into(),
                    user_id: None,
                    groups: vec![],
                },
            );
            let gate: Arc<dyn mcpmesh_net::TrustGate> = Arc::new(StaticGate::new(entries));

            let pdir = tempfile::tempdir().unwrap();
            let scopes = Arc::new(ScopeStore::new(pdir.path().join("scopes.json")));
            let provider_ep = ep().await;
            let provider = AppBlobs::load(
                pdir.path().join("blobs"),
                scopes,
                gate,
                provider_ep.clone(),
                crate::audit::AuditSink::disabled(),
                crate::limits::MeshLimiters::unlimited(),
                None,
                None,
            )
            .await
            .unwrap();
            provider.spawn_accept(&provider_ep);

            let src = pdir.path().join("attach.bin");
            std::fs::write(&src, b"eid-scoped bytes").unwrap();
            let (ticket, _hash) = provider
                .publish_scope("kb-attach-carol", &src)
                .await
                .unwrap();
            // Grant by the STABLE eid: device principal (iroh EndpointId Display is the same
            // hex-lower encoding as `EndpointId::principal()`).
            provider
                .grant("kb-attach-carol", &format!("eid:{}", carol_ep.id()))
                .unwrap();
            // A NICKNAME entry on the same scope — display names must NEVER admit (#38), so
            // this grants mallory nothing even though her resolved identity is named "mallory".
            provider.grant("kb-attach-carol", "mallory").unwrap();

            let cdir = tempfile::tempdir().unwrap();
            let carol = AppBlobs::open_fetcher(cdir.path().join("c"), carol_ep.clone())
                .await
                .unwrap();
            let hash = carol
                .fetch(&ticket)
                .await
                .expect("a pairing-mode peer granted by its eid: principal fetches");
            assert_eq!(
                &carol.read_bytes(hash).await.unwrap()[..],
                b"eid-scoped bytes"
            );

            // NICKNAME NEVER ADMITS: mallory resolves at accept time and the scope lists the
            // bare string "mallory", but her nickname is not a principal → Permission.
            let mallory = AppBlobs::open_fetcher(cdir.path().join("m"), mallory_ep.clone())
                .await
                .unwrap();
            let res =
                tokio::time::timeout(std::time::Duration::from_secs(10), mallory.fetch(&ticket))
                    .await;
            assert!(
                matches!(res, Ok(Err(_))),
                "a nickname-only grant is refused: {res:?}"
            );
        })
        .await
        .expect("eid-grant test timed out");
    }

    /// A served GET records a `blob_fetch` audit line attributed to the authenticated peer, with the
    /// hash and status=ok ("each blob fetch — peer + hash + …"). Uses a real temp AuditLog.
    #[tokio::test]
    async fn served_get_records_blob_fetch_audit() {
        use crate::audit::{AuditLog, AuditSink};
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let alice_ep = ep().await;
            let alice_id: EndpointId = alice_ep.id().into();
            let mut entries = std::collections::HashMap::new();
            entries.insert(
                alice_id,
                PeerIdentity {
                    endpoint: [0u8; 32].into(),
                    name: "alice".into(),
                    user_id: Some("alice".into()),
                    groups: vec![],
                },
            );
            let gate: Arc<dyn mcpmesh_net::TrustGate> = Arc::new(StaticGate::new(entries));

            let pdir = tempfile::tempdir().unwrap();
            let audit_dir = pdir.path().join("audit");
            let sink = AuditSink::new(AuditLog::spawn(audit_dir.clone()));
            let scopes = Arc::new(ScopeStore::new(pdir.path().join("scopes.json")));
            let provider_ep = ep().await;
            let provider = AppBlobs::load(
                pdir.path().join("blobs"),
                scopes,
                gate,
                provider_ep.clone(),
                sink,
                crate::limits::MeshLimiters::unlimited(),
                None,
                None,
            )
            .await
            .unwrap();
            provider.spawn_accept(&provider_ep);

            let src = pdir.path().join("doc.bin");
            std::fs::write(&src, b"auditable bytes").unwrap();
            let (ticket, hash_hex) = provider.publish_scope("docs", &src).await.unwrap();
            provider.grant("docs", "alice").unwrap();

            let cdir = tempfile::tempdir().unwrap();
            let alice = AppBlobs::open_fetcher(cdir.path().join("a"), alice_ep.clone())
                .await
                .unwrap();
            let _ = alice.fetch(&ticket).await.expect("granted alice fetches");

            let month = &crate::audit::now_ts()[..7];
            let file = audit_dir.join(format!("{month}.jsonl"));
            let mut ok = false;
            for _ in 0..50 {
                let alice_eid = format!("eid:{}", alice_ep.id());
                if let Ok(b) = std::fs::read_to_string(&file)
                    && b.contains("\"kind\":\"blob_fetch\"")
                    && b.contains("\"peer\":\"alice\"")
                    // #57 second surface: who fetched which BYTES is the record where
                    // two-devices-one-nickname is most likely the actual question.
                    && b.contains(&format!("\"principal\":\"{alice_eid}\""))
                    && b.contains(&hash_hex)
                {
                    ok = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            assert!(
                ok,
                "a served GET records blob_fetch(peer=alice, hash, status)"
            );
        })
        .await
        .expect("blob_fetch audit test timed out");
    }

    /// `shutdown` must RELEASE the blob store, so the same directory can be opened again.
    ///
    /// A leak that predates #80 and was found while gating it: dropping the provider never closed
    /// the fs actor, so `blobs.db` stayed locked for the life of the process. Removing the fix
    /// makes the `gc: Some(..)` arm here hang; a variant of this test that publishes nothing makes
    /// **both** arms hang, so the release was unreliable without a collector too and merely
    /// deterministic with one.
    ///
    /// **Both arms are kept for that reason.** Running only `Some` would encode the narrower story
    /// the gate reported and leave the older half untested.
    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_releases_the_blob_store_with_and_without_a_collector() {
        for gc in [None, Some(std::time::Duration::from_secs(60))] {
            let dir = tempfile::tempdir().unwrap();
            let blobs_dir = dir.path().join("blobs");
            {
                let provider = AppBlobs::load(
                    blobs_dir.clone(),
                    Arc::new(ScopeStore::new(dir.path().join("scopes.json"))),
                    Arc::new(StaticGate::new(std::collections::HashMap::new())),
                    ep().await,
                    crate::audit::AuditSink::disabled(),
                    crate::limits::MeshLimiters::unlimited(),
                    None,
                    gc,
                )
                .await
                .unwrap();
                // Do real work first: an empty store could plausibly release for reasons a used
                // one would not.
                let src = dir.path().join("f.bin");
                std::fs::write(&src, b"some bytes").unwrap();
                provider.publish_scope("room", &src).await.unwrap();
                provider.shutdown().await;
            }
            // The proof is a REOPEN, not an absence of panics: the lock is held by a task, so
            // nothing observable happens until someone else wants the directory.
            tokio::time::timeout(
                std::time::Duration::from_secs(15),
                AppBlobs::open_fetcher(blobs_dir.clone(), ep().await),
            )
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "reopening the store after shutdown hung (gc = {gc:?}); the fs actor was \
                        never closed, so blobs.db stayed locked for the life of the process"
                )
            })
            .expect("reopen must succeed");
        }
    }

    /// #80: a sweep landing inside `republish`'s check-then-insert must not leave the scope
    /// advertising bytes the node no longer has.
    ///
    /// `publish_scope` got a temp tag held across its insert; `republish` has the identical window
    /// — `has()` → `is_withdrawn` → `publish_hash` — and its failure is worse. A publish that loses
    /// its blob fails. A republish returned `Ok` WITH A TICKET and wrote the hash into the scope,
    /// so `blob_list` showed the file as shared, every fetcher hung or errored, and the phantom
    /// entry then rooted itself in `live_hashes` forever — permanent, self-protecting, and exactly
    /// what the completeness check exists to prevent ("a hang at every fetcher").
    ///
    /// `set_republish_delay` widens the window past several sweeps, so this is deterministic rather
    /// than a race the test hopes to lose.
    #[tokio::test]
    async fn a_sweep_during_a_republish_leaves_no_phantom_scope_entry() {
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            let dir = tempfile::tempdir().unwrap();
            let scopes = Arc::new(ScopeStore::new(dir.path().join("scopes.json")));
            let provider = AppBlobs::load(
                dir.path().join("blobs"),
                scopes.clone(),
                Arc::new(StaticGate::new(std::collections::HashMap::new())),
                ep().await,
                crate::audit::AuditSink::disabled(),
                crate::limits::MeshLimiters::unlimited(),
                None,
                Some(std::time::Duration::from_secs(2)),
            )
            .await
            .unwrap();

            // A scope must exist for republish to target. Give it its own live blob, so the sweeps
            // that run during the window are real ones with a non-empty root.
            let other = dir.path().join("other.bin");
            std::fs::write(&other, b"an unrelated published blob").unwrap();
            provider.publish_scope("room", &other).await.unwrap();

            // The target: in the store, in NO scope — a fetched-and-not-republished blob, which is
            // the state `blob_republish` exists to act on, and the state nothing protects.
            //
            // Created LAST and republished IMMEDIATELY, on purpose. An earlier version set this up
            // first and slept before republishing, so a sweep reclaimed the target during SETUP and
            // `republish` bailed on the completeness check without ever entering the window it was
            // meant to exercise — a test that would have passed whatever the code did. The only
            // unprotected gap left is the microseconds between `publish_path` dropping its temp tag
            // and `republish` taking its pin; the 5s delay below guarantees a sweep inside the
            // window that actually matters.
            use std::sync::atomic::Ordering;
            let stats = provider.gc_stats();
            let src = dir.path().join("held.bin");
            std::fs::write(&src, b"held but unshared").unwrap();
            let (_t, hex) = provider.publish_path(&src).await.unwrap();
            let hash = crate::blobs::parse_blob_hash(&hex).unwrap();

            let before = stats.runs.load(Ordering::Relaxed);
            provider.set_republish_delay(std::time::Duration::from_secs(5));
            let res = provider.republish("room", &hex).await;

            assert!(
                stats.runs.load(Ordering::Relaxed) > before,
                "precondition: at least one sweep ran INSIDE the republish window ({before} -> {})",
                stats.runs.load(Ordering::Relaxed)
            );
            // Whatever the verb answers, the two states it may leave must AGREE. A refusal that
            // touched no scope is fine; a success whose bytes are gone is the defect.
            let listed = scopes
                .snapshot()
                .scopes
                .get("room")
                .is_some_and(|sc| sc.hashes.contains(&hex));
            let held = provider.store.blobs().has(hash).await.unwrap();
            assert!(
                !listed || held,
                "the scope must never advertise a hash the store no longer holds — republish said \
                 {res:?}, scope lists it = {listed}, store holds it = {held}"
            );
            assert!(
                res.is_ok() && held,
                "and the pin should make the republish SUCCEED across the sweep rather than merely \
                 fail consistently: {res:?}"
            );
        })
        .await
        .expect("timed out");
    }

    /// #80: one unparseable hash in the scope table must not disable collection.
    ///
    /// The protect callback warns and carries on rather than aborting. Aborting would let a single
    /// junk row — reachable from a hand-edited sidecar or a pre-#62 one — stop reclaim forever
    /// while `status` still reported a configured collector. The comment claiming that was
    /// undefended until this test.
    #[tokio::test]
    async fn a_junk_hash_in_the_scope_table_does_not_disable_collection() {
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            let dir = tempfile::tempdir().unwrap();
            let scopes = Arc::new(ScopeStore::new(dir.path().join("scopes.json")));
            // Straight into the table, as a hand-edited sidecar would be: not a blake3 hex.
            scopes.publish_hash("room", "not-a-hash").unwrap();
            let provider = AppBlobs::load(
                dir.path().join("blobs"),
                scopes,
                Arc::new(StaticGate::new(std::collections::HashMap::new())),
                ep().await,
                crate::audit::AuditSink::disabled(),
                crate::limits::MeshLimiters::unlimited(),
                None,
                Some(std::time::Duration::from_secs(1)),
            )
            .await
            .unwrap();

            let kept_src = dir.path().join("kept.bin");
            std::fs::write(&kept_src, b"a real published blob").unwrap();
            let (_t, kept_hex) = provider.publish_scope("room", &kept_src).await.unwrap();
            let kept = crate::blobs::parse_blob_hash(&kept_hex).unwrap();
            let swept_src = dir.path().join("swept.bin");
            std::fs::write(&swept_src, b"named by nothing").unwrap();
            let (_t, swept_hex) = provider.publish_path(&swept_src).await.unwrap();
            let swept = crate::blobs::parse_blob_hash(&swept_hex).unwrap();

            let mut gone = false;
            for _ in 0..100 {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                if !provider.store.blobs().has(swept).await.unwrap() {
                    gone = true;
                    break;
                }
            }
            use std::sync::atomic::Ordering;
            let stats = provider.gc_stats();
            assert!(
                gone,
                "collection must keep running past a junk row — aborting on one would stop reclaim \
                 forever while status still showed a configured collector (aborted = {})",
                stats.aborted.load(Ordering::Relaxed)
            );
            assert_eq!(
                stats.aborted.load(Ordering::Relaxed),
                0,
                "a junk row is not a failure to READ the root, so it must not count as an abort"
            );
            assert_eq!(
                stats.last_protected.load(Ordering::Relaxed),
                1,
                "the junk row protects nothing, and the real one still does"
            );
            assert!(
                provider.store.blobs().has(kept).await.unwrap(),
                "…and the real published blob survives"
            );
        })
        .await
        .expect("timed out");
    }

    /// #80 FAIL-SAFE: a run that cannot read the liveness root must sweep NOTHING.
    ///
    /// The protect callback runs BEFORE the sweep and its only job is to hand over the live set. If
    /// it returned `Continue` after failing to build one, iroh would sweep against an **empty**
    /// root and delete every blob on the node — every published file, every scope's contents, on a
    /// background timer, with the operator's only warning a log line. `ProtectOutcome::Abort` skips
    /// the run and keeps the schedule.
    ///
    /// The failure is provoked the way it happens in production: a poisoned scope lock, i.e. a
    /// thread that panicked part-way through a mutation.
    #[tokio::test]
    async fn a_run_that_cannot_read_the_scope_table_sweeps_nothing() {
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            let dir = tempfile::tempdir().unwrap();
            let scopes = Arc::new(ScopeStore::new(dir.path().join("scopes.json")));
            let gate: Arc<dyn mcpmesh_net::TrustGate> =
                Arc::new(StaticGate::new(std::collections::HashMap::new()));
            let provider = AppBlobs::load(
                dir.path().join("blobs"),
                scopes.clone(),
                gate,
                ep().await,
                crate::audit::AuditSink::disabled(),
                crate::limits::MeshLimiters::unlimited(),
                None,
                Some(std::time::Duration::from_secs(1)),
            )
            .await
            .unwrap();

            let src = dir.path().join("published.bin");
            std::fs::write(&src, b"a published blob a scope names").unwrap();
            let (_t, hex) = provider.publish_scope("room", &src).await.unwrap();
            let hash = crate::blobs::parse_blob_hash(&hex).unwrap();

            scopes.poison_for_test();
            assert!(
                scopes.live_hashes().is_err(),
                "precondition: the root really is unreadable now"
            );

            use std::sync::atomic::Ordering;
            let stats = provider.gc_stats();
            let mut aborted = false;
            for _ in 0..100 {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                if stats.aborted.load(Ordering::Relaxed) >= 2 {
                    aborted = true;
                    break;
                }
            }
            assert!(
                aborted,
                "an unreadable root must ABORT the run and keep the schedule — one abort then \
                 silence would mean collection had stopped, not that it was failing safe"
            );
            assert!(
                provider.store.blobs().has(hash).await.unwrap(),
                "a run that could not build the live set must delete NOTHING; sweeping against an \
                 empty root would take every blob on the node"
            );
        })
        .await
        .expect("timed out");
    }

    /// #80 END TO END, through a real `FsStore` on a real timer: a blob NAMED by a scope survives a
    /// sweep and a blob the store merely holds does not.
    ///
    /// **Both halves are load-bearing.** "The unreferenced blob is gone" alone passes on a
    /// collector that deletes everything — which is precisely the failure mode an empty liveness
    /// root produces, and the reason `live_hashes` is fallible at all. "The published blob survives"
    /// alone passes on a collector that never runs.
    ///
    /// The unscoped blob stands in for the ordinary case this feature exists for: a blob this node
    /// FETCHED and never republished, or one an operator withdrew with `blob_unpublish` — bytes no
    /// scope names, which before 0.43.0 stayed on disk for the life of the node.
    #[tokio::test]
    async fn a_sweep_reclaims_what_no_scope_names_and_keeps_what_one_does() {
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            let dir = tempfile::tempdir().unwrap();
            let scopes = Arc::new(ScopeStore::new(dir.path().join("scopes.json")));
            let gate: Arc<dyn mcpmesh_net::TrustGate> =
                Arc::new(StaticGate::new(std::collections::HashMap::new()));
            let provider = AppBlobs::load(
                dir.path().join("blobs"),
                scopes,
                gate,
                ep().await,
                crate::audit::AuditSink::disabled(),
                crate::limits::MeshLimiters::unlimited(),
                None,
                // Far below the `[blobs].gc_interval` floor on purpose: the floor is an OPERATOR
                // policy in config, not a property of the store, so a test may ask for a fast one.
                Some(std::time::Duration::from_secs(1)),
            )
            .await
            .unwrap();

            // KEPT: published into a scope, so the scope table names it.
            let kept_src = dir.path().join("kept.bin");
            std::fs::write(&kept_src, b"a blob some scope names").unwrap();
            let (_t, kept_hex) = provider.publish_scope("room", &kept_src).await.unwrap();
            let kept = crate::blobs::parse_blob_hash(&kept_hex).unwrap();

            // SWEPT: in the store, in no scope. `publish_path` deliberately touches no scope.
            let swept_src = dir.path().join("swept.bin");
            std::fs::write(&swept_src, b"a blob no scope names").unwrap();
            let (_t, swept_hex) = provider.publish_path(&swept_src).await.unwrap();
            let swept = crate::blobs::parse_blob_hash(&swept_hex).unwrap();
            assert_ne!(kept, swept, "precondition: two distinct blobs");

            let has = async |h| provider.store.blobs().has(h).await.unwrap();
            assert!(
                has(kept).await && has(swept).await,
                "precondition: the store holds both before any sweep"
            );

            // The collector SLEEPS before its first run, so nothing is reclaimable immediately.
            // Poll rather than sleeping a fixed multiple: a bounded, sleeping wait, not a spin.
            let mut swept_gone = false;
            for _ in 0..100 {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                if !has(swept).await {
                    swept_gone = true;
                    break;
                }
            }
            assert!(
                swept_gone,
                "a blob no scope names must be reclaimed once the collector runs"
            );
            assert!(
                has(kept).await,
                "…and a blob a scope DOES name must survive — without this half the test passes on \
                 a collector that deletes everything, which is exactly what an empty liveness root \
                 produces"
            );

            let stats = provider.gc_stats();
            use std::sync::atomic::Ordering;
            assert!(
                stats.runs.load(Ordering::Relaxed) >= 1,
                "the run counter is the only signal an operator gets that collection is alive"
            );
            assert_eq!(
                stats.aborted.load(Ordering::Relaxed),
                0,
                "a healthy run must not report as aborted"
            );
            assert_eq!(
                stats.last_protected.load(Ordering::Relaxed),
                1,
                "exactly the one scoped hash was protected"
            );
            assert!(
                stats.last_run_epoch.load(Ordering::Relaxed) > 0,
                "a run that happened must carry a timestamp"
            );
        })
        .await
        .expect("timed out");
    }

    /// With NO gc configured — the default, and every release up to 0.42.0 — an unreferenced blob
    /// stays put and the counters stay at zero.
    ///
    /// This is the control for the test above: without it, "swept is gone" could be reporting a
    /// store that drops unreferenced blobs on its own, with the `GcConfig` doing nothing.
    #[tokio::test]
    async fn without_a_configured_interval_nothing_is_reclaimed() {
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let dir = tempfile::tempdir().unwrap();
            let scopes = Arc::new(ScopeStore::new(dir.path().join("scopes.json")));
            let gate: Arc<dyn mcpmesh_net::TrustGate> =
                Arc::new(StaticGate::new(std::collections::HashMap::new()));
            let provider = AppBlobs::load(
                dir.path().join("blobs"),
                scopes,
                gate,
                ep().await,
                crate::audit::AuditSink::disabled(),
                crate::limits::MeshLimiters::unlimited(),
                None,
                None, // no collection
            )
            .await
            .unwrap();

            let src = dir.path().join("orphan.bin");
            std::fs::write(&src, b"nobody names this").unwrap();
            let (_t, hex) = provider.publish_path(&src).await.unwrap();
            let hash = crate::blobs::parse_blob_hash(&hex).unwrap();

            // Comfortably longer than the 1s interval the configured test above relies on.
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            assert!(
                provider.store.blobs().has(hash).await.unwrap(),
                "an unconfigured node must never reclaim — that is the documented default"
            );
            use std::sync::atomic::Ordering;
            assert_eq!(
                provider.gc_stats().runs.load(Ordering::Relaxed),
                0,
                "no collector means no runs"
            );
        })
        .await
        .expect("timed out");
    }

    /// How many tags the store holds — the GC root #80 has to clear.
    async fn tag_count(store: &FsStore) -> usize {
        use n0_future::StreamExt;
        let mut n = 0;
        let mut s = std::pin::pin!(store.tags().list().await.unwrap());
        while let Some(t) = s.next().await {
            t.unwrap();
            n += 1;
        }
        n
    }

    /// #80: a store written by an EARLIER release must become collectable.
    ///
    /// Up to 0.42.0 every import awaited `add_path`, which runs `with_tag()` and persists an
    /// auto-tag per blob — and `gc_mark` roots the live set in tags. So on any existing store the
    /// first sweep would have reclaimed NOTHING while logging a run every interval: configured,
    /// and silently doing nothing. This pins the one-time clear that fixes it.
    ///
    /// The pre-0.43.0 store is CONSTRUCTED, not simulated: the awaited `add_path` below is
    /// byte-for-byte what `publish_path` used to do.
    #[tokio::test]
    async fn a_pre_0_43_store_full_of_auto_tags_becomes_collectable() {
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            let dir = tempfile::tempdir().unwrap();
            let blobs_dir = dir.path().join("blobs");
            let src = dir.path().join("legacy.bin");
            std::fs::write(&src, b"imported by an older release").unwrap();

            // A store as 0.42.0 left it: one blob, one PERSISTENT auto-tag, no scope naming it.
            let hash = {
                let old = AppBlobs::open_fetcher(blobs_dir.clone(), ep().await)
                    .await
                    .unwrap();
                let tag = old.store.blobs().add_path(&src).await.unwrap();
                assert_eq!(
                    tag_count(&old.store).await,
                    1,
                    "precondition: the old import path leaves a persistent tag"
                );
                old.store.shutdown().await.unwrap();
                tag.hash
            };

            // Reopen with collection on.
            let scopes = Arc::new(ScopeStore::new(dir.path().join("scopes.json")));
            let gate: Arc<dyn mcpmesh_net::TrustGate> =
                Arc::new(StaticGate::new(std::collections::HashMap::new()));
            let provider = AppBlobs::load(
                blobs_dir,
                scopes,
                gate,
                ep().await,
                crate::audit::AuditSink::disabled(),
                crate::limits::MeshLimiters::unlimited(),
                None,
                Some(std::time::Duration::from_secs(1)),
            )
            .await
            .unwrap();
            assert_eq!(
                tag_count(&provider.store).await,
                0,
                "the legacy auto-tags must be cleared, or every sweep protects everything"
            );

            let mut gone = false;
            for _ in 0..100 {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                if !provider.store.blobs().has(hash).await.unwrap() {
                    gone = true;
                    break;
                }
            }
            assert!(
                gone,
                "a blob left by an older release, named by no scope, must be reclaimable"
            );
        })
        .await
        .expect("timed out");
    }

    /// #80: a sweep landing MID-PUBLISH must not delete the file being published.
    ///
    /// `publish_scope` imports and then inserts into the scope; between those the blob is named by
    /// nothing, so the scope table cannot protect it. The import's temp tag has to, and is held
    /// across the insert for exactly that reason.
    ///
    /// `publish_delay` widens the window to several sweep intervals, so this is deterministic
    /// rather than a race the test hopes to lose.
    #[tokio::test]
    async fn a_sweep_during_a_publish_does_not_eat_the_blob_being_published() {
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            let dir = tempfile::tempdir().unwrap();
            let scopes = Arc::new(ScopeStore::new(dir.path().join("scopes.json")));
            let gate: Arc<dyn mcpmesh_net::TrustGate> =
                Arc::new(StaticGate::new(std::collections::HashMap::new()));
            let provider = AppBlobs::load(
                dir.path().join("blobs"),
                scopes,
                gate,
                ep().await,
                crate::audit::AuditSink::disabled(),
                crate::limits::MeshLimiters::unlimited(),
                None,
                Some(std::time::Duration::from_secs(1)),
            )
            .await
            .unwrap();
            // The scope must already exist and hold an UNRELATED live hash, so the sweeps that run
            // during the window are real sweeps with a real (non-empty) root — not runs that
            // happen to protect nothing and delete nothing.
            let other = dir.path().join("other.bin");
            std::fs::write(&other, b"an unrelated published blob").unwrap();
            provider.publish_scope("room", &other).await.unwrap();

            // Wait out one interval so at least one sweep has already run before we start.
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            provider.set_publish_delay(std::time::Duration::from_secs(4));

            let src = dir.path().join("slow.bin");
            std::fs::write(&src, b"published across several sweeps").unwrap();
            let (_t, hex) = provider
                .publish_scope("room", &src)
                .await
                .expect("the publish itself must succeed");
            let hash = crate::blobs::parse_blob_hash(&hex).unwrap();

            assert!(
                provider
                    .gc_stats()
                    .runs
                    .load(std::sync::atomic::Ordering::Relaxed)
                    >= 2,
                "precondition: sweeps really did run across the publish window"
            );
            assert!(
                provider.store.blobs().has(hash).await.unwrap(),
                "the blob must survive a sweep that lands between its import and its scope insert"
            );
        })
        .await
        .expect("timed out");
    }
}
