//! Live path-change watching (#92 item 2): one watcher per USER session, so a session whose
//! selected path changes mid-flight pushes a `Reachability` frame when it happens.
//!
//! #92 item (1) shipped in 0.19.0 and made `is_transition` compare `path`, so a PROBE observing a
//! changed path emits. That removed "no event, ever"; it did not give live signal. Probes are
//! TTL-gated (20s) and only run when `status`/`subscribe` asks, so a session that degrades
//! Direct→Relay mid-call stays silently mislabelled until something probes — possibly never.
//!
//! That matters because `path` is documented as a TRUTH CLAIM, not drift: `Direct` is the only
//! value supporting a locality claim. An embedder can render the indicator correctly at dial time
//! and have it become wrong for the rest of a long-lived session.
//!
//! **The decision logic here is deliberately PURE.** The network loop is a thin shell over
//! [`decide`], because the alternative — pinning the flap/`Lagged`/unchanged rules through a real
//! hole-punch — is how #110's suite ended up passing whether or not the behaviour was present.
//! Timing-dependent tests over a real network are where vacuity hides.

use std::time::Duration;

use mcpmesh_local_api::PeerPath;

/// Live watcher tasks, for the #61-shaped lifetime regression (#92 review).
///
/// A leaked watcher emits NOTHING — it parks on `events.next()` forever — so side-effects cannot
/// distinguish "ended" from "leaked". This counter can.
pub(crate) static LIVE_WATCHERS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// How long a changed path must HOLD before it is worth telling anyone (#92 item 2).
///
/// Hole-punching flaps by nature — that was #64's stated reason for excluding `path` from
/// transitions altogether. A `path_events()` watcher sees every change, including the ordinary
/// relay→direct transition of a healthy dial, so it needs its own damping. Same 600ms as
/// `reach::PATH_SETTLE` and for the same reason: it is the measured time for a loopback punch to
/// settle, and it is well inside any session's lifetime.
pub(crate) const PATH_CHANGE_SETTLE: Duration = Duration::from_millis(600);

/// Should an observation be committed and emitted?
///
/// Pure so the rules are testable without a relay. `observed` is the path the connection reports
/// now; `cached` is what the reachability cache already says (`None` when the peer has no entry).
///
/// Returns the value to commit, or `None` to stay quiet. The rule is deliberately narrow: emit
/// only when the observation DIFFERS from what a consumer already believes. A watcher that emits
/// on every event turns a flapping connection into a frame storm, which is the noise #64 avoided
/// by excluding `path` entirely.
pub(crate) fn decide(observed: &PeerPath, cached: Option<&PeerPath>) -> Option<PeerPath> {
    // `Unknown` is never worth emitting: it means "we do not know", and pushing it would replace a
    // consumer's correct belief with an absence of one. A connection tearing down reports Unknown
    // routinely (no selected path and several or no open paths), and its path is about to stop
    // mattering anyway.
    if matches!(observed, PeerPath::Unknown) {
        return None;
    }
    match cached {
        Some(known) if known == observed => None,
        _ => Some(observed.clone()),
    }
}

/// Watch ONE session's selected path and emit when it settles on something new (#92 item 2).
///
/// Ends when the connection does — `path_events()` is documented to end on close — so the task is
/// bounded by the session and needs no separate shutdown path. #61 cost a release to a detached
/// task holding a lock; this one holds no lock and no strong connection handle.
///
/// **It must not hold a `Connection` FOR THE TASK'S LIFE.** Re-reading the path after the settle
/// window needs a `&Connection`, but a clone kept for the whole task would keep the session open
/// until the task exits — and the task exits when the session closes. That is a deadlock, and it
/// would make the "dies with its connection" property untestable.
///
/// A [`WeakConnectionHandle`] is upgraded per observation instead. To be exact, because an earlier
/// version of this comment overstated it (#92 review): `upgrade()` DOES yield a strong
/// `Connection`, and it is held across the `settle(...).await` — up to `PATH_CHANGE_SETTLE` per
/// event, and the full window for a relayed path since `settle` only short-circuits on `Direct`.
/// So teardown can be delayed by that bounded window; what is avoided is a handle held for the
/// task's entire lifetime, which would never resolve.
///
/// [`WeakConnectionHandle`]: iroh::endpoint::WeakConnectionHandle
/// Returns the task handle so a test can assert the task actually ENDS. The handle is dropped by
/// production callers — dropping a `JoinHandle` detaches, it does not cancel — but without it the
/// lifetime regression cannot fail: a leaked watcher parked on `events.next()` forever emits
/// nothing, which is indistinguishable from a settled connection if the test only watches for
/// side-effects (#92 review found exactly that vacuity).
pub(crate) fn spawn(
    mesh: std::sync::Arc<super::MeshState>,
    endpoint_id: [u8; 32],
    conn: &iroh::endpoint::Connection,
) -> tokio::task::JoinHandle<()> {
    // SUBSCRIBE FIRST, then read (#225). `path_events()` does not replay: a selection made before
    // this line is never delivered as an event. Taking the reading after subscribing means a change
    // can land before the reading (the reading sees it) or after (an event reports it), never in
    // between.
    let mut events = conn.path_events();
    let weak = conn.weak_handle();
    LIVE_WATCHERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    tokio::spawn(async move {
        // Decrement on EVERY exit path, including a `break`, so the counter cannot drift.
        let _guard = WatcherGuard;
        use n0_future::StreamExt as _;
        // #225: the path ALREADY selected when we subscribed. A dialed session subscribes only
        // after the dial and the stream open have returned, and a loopback hole-punch can select
        // Direct within 4-11ms of the connection — measured winning that race under load, so the
        // relay->direct move this watcher exists to report was never seen and no frame was pushed. The
        // reading goes through the same observation an event does, so it emits exactly the frame
        // a `Selected` would have, and nothing when `decide` finds the path unchanged or Unknown.
        if !observe(&mesh, endpoint_id, &weak, super::reach::selected_path).await {
            return;
        }
        while let Some(event) = events.next().await {
            match event {
                // The only event that means "application data moved" — the same semantics #64
                // settled on via `is_selected()`.
                iroh::endpoint::PathEvent::Selected { .. } => {}
                // The consumer fell behind. iroh documents the CURRENT selected path as still
                // recoverable from `Connection::paths()`, so re-read rather than skip: on a stable
                // session the event we dropped may be the only one there will ever be, and
                // skipping it is how the indicator stays wrong.
                iroh::endpoint::PathEvent::Lagged { missed, .. } => {
                    tracing::debug!(missed, "path events lagged; re-reading current path");
                }
                // Opened/Closed alone do not move application data; the Selected event that
                // follows a meaningful change is what we act on.
                _ => continue,
            }
            if !observe(&mesh, endpoint_id, &weak, super::reach::selected_path).await {
                break;
            }
        }
    })
}

/// Take ONE observation of the connection's selected path and commit it (#92 item 2).
///
/// Shared by the watcher's initial reading (#225) and every `Selected`/`Lagged` event, so the two
/// cannot diverge. Returns `false` when the connection is gone and the watcher should stop.
///
/// `read` is always [`selected_path`](super::reach::selected_path) in production. It is a
/// parameter only so a test can hand in the `Unknown` a live loopback connection never produces
/// (iroh 1.2 keeps reporting a closed connection's last selected path).
async fn observe(
    mesh: &std::sync::Arc<super::MeshState>,
    endpoint_id: [u8; 32],
    weak: &iroh::endpoint::WeakConnectionHandle,
    read: fn(&iroh::endpoint::Connection) -> PeerPath,
) -> bool {
    // Ticket FIRST, before observing — ordering is by observation START, exactly as `probe_peer`
    // does it. See `commit_observation`.
    let seq = mesh
        .probe_seq
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Let the change HOLD before believing it. `settle` returns early on `Direct`, so a
    // relay→direct recovery reports promptly, while a flap that returns to Direct inside the window
    // reports Direct and `decide` then finds nothing changed — no frame.
    let Some(strong) = weak.upgrade() else {
        return false;
    };
    let observed = super::reach::settle(PATH_CHANGE_SETTLE, || read(&strong)).await;
    // #124: the selected path just settled, so THIS is the moment the connection knows the peer's
    // real direct address — not accept time, when only the relay path exists. Refreshing here
    // means the stored dial hint tracks reality instead of being written once at pairing and going
    // permanently stale after a network change. Read now, while the strong handle is held; applied
    // after the commit, because #92's whole point is that a path change is reported WHEN IT
    // HAPPENS, so the frame must not queue behind cache maintenance (#124 review).
    let refreshed = super::dial_hint::observed_for(&strong);
    drop(strong);
    // #223: a session to a peer this node has REVOKED commits nothing. Outbound sessions are not
    // severed on revocation (#229), and an inbound sever races this task, so without this a path
    // event on such a session seeded `reachable: true` with a fresh stamp and pushed a `Session`
    // frame — the row the probe now refuses to write, written by the other writer. The watcher keeps
    // running: a revocation lifted mid-session makes the next observation count again. Fails CLOSED
    // on a join error, like every revocation read.
    let m = mesh.clone();
    let refused = tokio::task::spawn_blocking(move || super::dial::dial_refused(&m, &endpoint_id))
        .await
        .unwrap_or(true);
    if refused {
        return true;
    }
    commit_observation(mesh, endpoint_id, seq, &observed);
    if let Some(addr) = refreshed {
        super::dial_hint::refresh(mesh, endpoint_id, addr);
    }
    true
}

/// Commit a settled observation for `endpoint_id` and emit if it changed anything.
///
/// **The watcher is a SECOND writer to the reachability cache**, alongside `probe_peer`. That is
/// the hazard here, not the emission. `seq` must be a ticket taken BEFORE observing, exactly as
/// `probe_peer` does, so an in-flight 3s probe that started earlier cannot land later and overwrite
/// a fresher live observation — which would re-poison the cache for a full TTL and, since #58, push
/// a stale path a consumer then renders as a privacy claim.
///
/// Only `path` is the watcher's to set. `reachable`/`rtt_ms`/`meta`/`services` come from a probe's
/// pong; a live connection carrying data IS reachability evidence, but inventing an `rtt_ms` from a
/// path event would be a fabricated measurement, so a seeded entry carries `rtt_ms: None`.
pub(crate) fn commit_observation(
    mesh: &std::sync::Arc<super::MeshState>,
    endpoint_id: [u8; 32],
    seq: u64,
    observed: &PeerPath,
) -> Option<mcpmesh_local_api::PeerReachability> {
    let committed = {
        let mut cache = mesh
            .reachability
            .lock()
            .expect("reachability lock not poisoned");
        // A NEWER writer already landed — drop ours rather than moving the cache backwards.
        if let Some(existing) = cache.get(&endpoint_id)
            && !super::reach::supersedes(seq, existing)
        {
            return None;
        }
        let cached = cache.get(&endpoint_id).map(|e| e.path.clone());
        let path = decide(observed, cached.as_ref())?;
        // #176: this writer's COMMIT ticket. The watcher runs on a LIVE session, so what it writes
        // is positive evidence exactly like a pong — and a probe that started before this must not
        // be able to land a timeout over it. Stamping `observed` here is what `contradicted_by`
        // reads.
        //
        // What it does NOT do: protect a stable session. `decide` returns `None` — and this line is
        // never reached — whenever the path is UNCHANGED, which is the common case for a session
        // that is simply up. So a live session refreshes `observed` only when its path moves, and
        // an idle-but-connected peer is still protected by probes alone. That is the honest scope:
        // this closes the cross-writer hole (a timeout landing over a path observation), not
        // "a live session is always shielded from a timeout".
        let committed_at = mesh
            .probe_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        match cache.get_mut(&endpoint_id) {
            Some(entry) => {
                // `probed_at` is the timestamp for the WHOLE row, and the row still carries the
                // probe's `rtt_ms`/`meta`/`services`. Refreshing it here would stamp a 300s-old RTT
                // as `age_secs: 0` on the wire AND stop `reachability_of` scheduling the refresh
                // probe that would correct it — the TTL is gated on this field. The module doc says
                // inventing an `rtt_ms` would be a fabricated measurement; forging that
                // measurement's FRESHNESS is the same lie with an extra step (#92 review).
                entry.path = path.clone();
                entry.seq = seq;
                // The row's evidence is as of now, whatever `reachable` it already carried. (On a
                // `reachable: false` row this is inert — `contradicted_by` only reads `observed`
                // off a reachable entry — but leaving it stale would make the field mean two
                // different things depending on which branch wrote it.)
                entry.observed = committed_at;
                entry.clone()
            }
            None => {
                // First knowledge, from a LIVE session: it is up by construction — we are talking
                // to it — but we have measured no RTT and hold none of its pong metadata.
                let entry = super::ReachEntry {
                    reachable: true,
                    rtt_ms: None,
                    probed_at: crate::util::epoch_now_i64(),
                    meta: String::new(),
                    services: Vec::new(),
                    // No pong: an empty `services` here means "not asked", never "offers nothing".
                    pong_at: None,
                    seq,
                    observed: committed_at,
                    path,
                };
                cache.insert(endpoint_id, entry.clone());
                entry
            }
        }
    };
    // Same single constructor the probe path and `status` use, so the three cannot drift.
    let nickname = mesh.store.resolve(&endpoint_id).ok().flatten()?.nickname;
    let row = super::reach::reachability_row(nickname, endpoint_id, Some(&committed), Some(0));
    // Best-effort: `send` errors only when there are no subscribers, the common case.
    let _ = mesh.reach_bcast.send(super::reach::ReachTransition {
        peer: row.clone(),
        // #150: this is the producer whose claim is about the link actually in use. Note the row
        // may carry a PROBE's `rtt_ms` (the Some(entry) arm above leaves it alone deliberately),
        // so `rtt_ms` cannot stand in for this attribution.
        source: mcpmesh_local_api::ReachabilitySource::Session,
    });
    Some(row)
}

/// Decrements [`LIVE_WATCHERS`] however the watcher task exits.
struct WatcherGuard;

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        LIVE_WATCHERS.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core rule: a change is news, a repeat is not. Without the second half a healthy session
    /// emits a frame per path event for as long as it lives.
    #[test]
    fn only_a_differing_path_is_worth_emitting() {
        let relay = PeerPath::Relay { url: None };

        assert_eq!(
            decide(&PeerPath::Direct, Some(&relay)),
            Some(PeerPath::Direct),
            "relay -> direct is the recovery a consumer must hear about"
        );
        assert_eq!(
            decide(&relay, Some(&PeerPath::Direct)),
            Some(relay.clone()),
            "direct -> relay is the DEGRADATION — the privacy indicator just became wrong"
        );
        assert_eq!(
            decide(&PeerPath::Direct, Some(&PeerPath::Direct)),
            None,
            "an unchanged path must stay quiet, or a stable session emits forever"
        );
        assert_eq!(
            decide(&PeerPath::Direct, None),
            Some(PeerPath::Direct),
            "first knowledge of a live session's path is news"
        );
    }

    /// `Unknown` means "we do not know", and the docs call rendering it as private "the one misuse
    /// that turns this field into a false privacy statement". Emitting it would overwrite a
    /// consumer's correct belief with an absence of one.
    #[test]
    fn unknown_is_never_emitted() {
        assert_eq!(decide(&PeerPath::Unknown, None), None);
        assert_eq!(decide(&PeerPath::Unknown, Some(&PeerPath::Direct)), None);
        assert_eq!(
            decide(&PeerPath::Unknown, Some(&PeerPath::Relay { url: None })),
            None
        );
    }

    /// #92 review, Finding 3: the settle window had NO test. Mutating `PATH_CHANGE_SETTLE` to zero
    /// AND turning the `Lagged` arm into a silent `continue` left every test in the branch green,
    /// while the commits claimed both were covered.
    ///
    /// The window's job is to let a change HOLD. Driven over `reach::settle`'s closure seam on
    /// tokio's test clock, so it is deterministic and instant.
    #[tokio::test(start_paused = true)]
    async fn the_settle_window_waits_for_a_degradation_to_hold() {
        let relay = PeerPath::Relay { url: None };

        // A path that reads Relay for the whole window IS a degradation: report it.
        let settled = crate::daemon::reach::settle(PATH_CHANGE_SETTLE, || relay.clone()).await;
        assert_eq!(
            settled, relay,
            "a degradation that holds for the whole window must be reported"
        );

        // A ZERO window reports whatever the first look says. That is the mutation the branch
        // shipped uncaught: with no window, a Direct->Relay blip is believed immediately.
        let mut polls = 0;
        let settled = crate::daemon::reach::settle(Duration::ZERO, || {
            polls += 1;
            if polls > 1 {
                PeerPath::Direct
            } else {
                relay.clone()
            }
        })
        .await;
        assert_eq!(
            settled, relay,
            "with no window the first observation wins — this assertion fails if \
             PATH_CHANGE_SETTLE is ever zeroed"
        );

        // A blip that RECOVERS inside the window settles on Direct, and `decide` then finds
        // nothing changed against a cached Direct — so a flap emits no frame. This is the
        // end-to-end flap rule, expressed over the two pieces that implement it.
        let mut polls = 0;
        let settled = crate::daemon::reach::settle(PATH_CHANGE_SETTLE, || {
            polls += 1;
            if polls > 2 {
                PeerPath::Direct
            } else {
                relay.clone()
            }
        })
        .await;
        assert_eq!(
            settled,
            PeerPath::Direct,
            "a recovered blip settles on Direct"
        );
        assert_eq!(
            decide(&settled, Some(&PeerPath::Direct)),
            None,
            "and a flap that returns to where it started must emit NOTHING"
        );
    }

    /// The hazard this whole module has to get right: the watcher is a SECOND writer to the
    /// reachability cache. An in-flight probe that STARTED earlier can complete later (a 3s timeout
    /// losing to a live path event), and must not move the cache backwards.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_older_writer_never_overwrites_a_newer_observation() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = crate::daemon::testutil::hermetic_mesh(cfg).await;
        let eid = [9u8; 32];
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: eid,
                nickname: "bob".into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: None,
            })
            .unwrap();

        // A NEWER writer (ticket 7) lands Direct.
        let row = commit_observation(&mesh, eid, 7, &PeerPath::Direct);
        assert!(row.is_some(), "first observation commits");

        // An OLDER writer (ticket 3) tries to report Relay — the stale value. It must be dropped.
        let row = commit_observation(&mesh, eid, 3, &PeerPath::Relay { url: None });
        assert!(
            row.is_none(),
            "an older writer must not overwrite a newer observation — this is the #58 defect \
             class, and here it would push a stale path a consumer renders as a privacy claim"
        );
        let cached = mesh
            .reachability
            .lock()
            .unwrap()
            .get(&eid)
            .map(|e| e.path.clone());
        assert_eq!(
            cached,
            Some(PeerPath::Direct),
            "the cache must still hold the NEWER value"
        );
    }

    /// First knowledge from a live session: reachable by construction (we are talking to it), but
    /// no RTT has been measured. Inventing one would be a fabricated measurement.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_seeded_entry_is_reachable_with_no_fabricated_rtt() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = crate::daemon::testutil::hermetic_mesh(cfg).await;
        let eid = [11u8; 32];
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: eid,
                nickname: "carol".into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: None,
            })
            .unwrap();

        let row = commit_observation(&mesh, eid, 1, &PeerPath::Direct).expect("seeds an entry");
        assert!(row.reachable, "a live session IS reachability evidence");
        assert_eq!(
            row.rtt_ms, None,
            "no RTT was measured — never fabricate one"
        );
        assert_eq!(row.path, PeerPath::Direct);
    }

    /// #150: the watcher stamps its frames `Session`, and it does so on the UPDATE branch — the
    /// one that fires for a peer some probe already measured.
    ///
    /// This is the case that makes `rtt_ms` unusable as a discriminator, and the reason the issue's
    /// cheap option (documenting `rtt_ms: None` as the session marker) was rejected rather than
    /// taken. `commit_observation`'s update arm deliberately leaves the probe's measurement alone,
    /// so the frame goes out `Session` AND `rtt_ms: Some(..)` — exactly the shape a consumer
    /// keying on `rtt_ms` would misfile as probe-sourced. It is also the COMMON shape for the
    /// scenario the field exists for: a peer probed at pairing time, then watched through a call.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_frame_for_an_already_probed_peer_keeps_the_probes_rtt() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = crate::daemon::testutil::hermetic_mesh(cfg).await;
        let eid = [17u8; 32];
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: eid,
                nickname: "erin".into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: None,
            })
            .unwrap();

        // A probe landed first: Direct, with a MEASURED round trip. Its `observed` is drawn from
        // the REAL counter (#176) rather than written as a literal — the whole point of the field
        // is that both writers order against one another through it, and a fixture that invents a
        // number compares the watcher's stamp against nothing.
        let probe_observed = mesh
            .probe_seq
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        mesh.reachability.lock().unwrap().insert(
            eid,
            crate::daemon::ReachEntry {
                reachable: true,
                rtt_ms: Some(12),
                probed_at: crate::util::epoch_now_i64(),
                meta: String::new(),
                services: Vec::new(),
                pong_at: Some(crate::util::epoch_now_i64()),
                seq: 1,
                observed: probe_observed,
                path: PeerPath::Direct,
            },
        );

        let mut rx = mesh.reach_bcast.subscribe();
        // Now the live session degrades under it. This is the update branch.
        let row = commit_observation(&mesh, eid, 2, &PeerPath::Relay { url: None })
            .expect("a Direct->Relay change on a live session commits");
        assert_eq!(row.path, PeerPath::Relay { url: None });

        let t = rx.try_recv().expect("the transition must be pushed");
        assert_eq!(
            t.source,
            mcpmesh_local_api::ReachabilitySource::Session,
            "a live-session observation must attribute itself to the session producer"
        );
        assert_eq!(
            t.peer.rtt_ms,
            Some(12),
            "the probe's measurement survives, so `rtt_ms: None` is NOT the session marker — this \
             frame is session-sourced AND carries an rtt"
        );

        // #176: the watcher is the SECOND writer to this cache, so its rows must carry a commit
        // ticket a concurrent probe's timeout can be compared against. Without it the row keeps
        // whatever `observed` the probe left (1 here), and an in-flight probe that started at 2
        // would be free to overwrite this live-session observation with `reachable: false` — the
        // cross-writer half of the bug.
        //
        // Asserted as "advanced past the ticket the row already had", not as a literal: the exact
        // number depends on how many probes the hermetic mesh has taken, and pinning it would make
        // this test a hostage to unrelated probe traffic.
        let stamped = mesh
            .reachability
            .lock()
            .unwrap()
            .get(&eid)
            .expect("row committed")
            .observed;
        assert!(
            stamped > probe_observed,
            "the watcher must stamp its OWN commit ticket (stamped {stamped}, the probe's was \
             {probe_observed}) — leaving it stale lets a probe that started later land a timeout \
             over this live-session observation"
        );
    }

    /// An unchanged path must not commit or emit, even with a fresh ticket — otherwise a stable
    /// session rewrites the cache and pushes a frame on every path event for its whole life.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unchanged_path_commits_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = crate::daemon::testutil::hermetic_mesh(cfg).await;
        let eid = [13u8; 32];
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: eid,
                nickname: "dave".into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: None,
            })
            .unwrap();

        assert!(commit_observation(&mesh, eid, 1, &PeerPath::Direct).is_some());
        assert!(
            commit_observation(&mesh, eid, 2, &PeerPath::Direct).is_none(),
            "a repeat observation is not news, however fresh its ticket"
        );
    }

    const TEST_ALPN: &[u8] = b"mcpmesh/path-watch/test";

    /// A live localhost connection whose selected path is ALREADY `Direct`, plus the hermetic mesh
    /// that knows its remote as `name`. Returns the guards that must outlive the test.
    #[allow(clippy::type_complexity)]
    async fn an_already_direct_connection(
        name: &str,
    ) -> (
        std::sync::Arc<crate::daemon::MeshState>,
        iroh::endpoint::Connection,
        [u8; 32],
        (tempfile::TempDir, iroh::Endpoint, iroh::Endpoint),
    ) {
        let mk = || {
            iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .alpns(vec![TEST_ALPN.to_vec()])
                .bind()
        };
        let server = mk().await.expect("bind server");
        let client = mk().await.expect("bind client");
        let accepting = server.clone();
        tokio::spawn(async move {
            while let Some(incoming) = accepting.accept().await {
                if let Ok(conn) = incoming.await {
                    // Hold the connection open until the client drops it.
                    tokio::spawn(async move { conn.closed().await });
                }
            }
        });
        let conn = client
            .connect(server.addr(), TEST_ALPN)
            .await
            .expect("connect over loopback");
        // The precondition the fix is about: the path is selected BEFORE any watcher exists.
        tokio::time::timeout(Duration::from_secs(30), async {
            while crate::daemon::reach::selected_path(&conn) != PeerPath::Direct {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("precondition: the loopback connection selects a direct path");

        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = crate::daemon::testutil::hermetic_mesh(cfg).await;
        let eid = *server.id().as_bytes();
        mesh.store
            .add(crate::allowlist::PeerEntry {
                endpoint_id: eid,
                nickname: name.into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: None,
            })
            .unwrap();
        (mesh, conn, eid, (dir, server, client))
    }

    /// How long the #225 tests give a watcher's initial reading. An already-Direct reading returns
    /// on `settle`'s first poll, so this only has to cover task scheduling — and it stays well
    /// under the ~5s at which iroh sometimes emits a later `Selected` on a loopback connection.
    const INITIAL_READING_WAIT: Duration = Duration::from_millis(1500);

    /// Drain the events already queued on a test-side `path_events()` subscription; `true` if any
    /// was a `Selected`.
    async fn saw_selected(events: &mut iroh::endpoint::PathEventStream) -> bool {
        use n0_future::StreamExt as _;
        let mut selected = false;
        while let Ok(Some(event)) = tokio::time::timeout(Duration::ZERO, events.next()).await {
            selected |= matches!(event, iroh::endpoint::PathEvent::Selected { .. });
        }
        selected
    }

    /// #225: a watcher that subscribes AFTER the path was selected must still report it.
    ///
    /// `path_events()` does not replay, so before the fix a watcher attached to a session whose
    /// relay->direct move had already happened received no `Selected` event and pushed nothing, for
    /// the life of the session. That was `live_path_events.rs` timing out at 120s under load: the
    /// dial side subscribes only after the dial returns, and the punch won the race.
    ///
    /// The path is Direct before `spawn` is called, every time. What would otherwise be luck is a
    /// LATER `Selected` event (iroh sometimes emits one ~5s in) driving the frame through the event
    /// arm instead. So the test subscribes to the same connection's events BEFORE spawning, and
    /// refuses a frame that arrived after a `Selected` it could have come from.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_path_selected_before_the_watcher_subscribes_is_still_pushed() {
        let (mesh, conn, eid, _guards) = an_already_direct_connection("frank").await;
        let mut rx = mesh.reach_bcast.subscribe();
        let mut events = conn.path_events();

        let _watcher = spawn(mesh.clone(), eid, &conn);

        let frame = tokio::time::timeout(INITIAL_READING_WAIT, rx.recv())
            .await
            .expect(
                "a watcher attached to an already-Direct session must push its path — with no \
                 initial reading it waits for a Selected event that already happened",
            )
            .expect("broadcast channel alive");
        assert!(
            !saw_selected(&mut events).await,
            "a Selected event arrived before the frame, so the frame proves nothing about the \
             initial reading"
        );
        assert_eq!(frame.peer.path, PeerPath::Direct);
        assert_eq!(frame.source, mcpmesh_local_api::ReachabilitySource::Session);
    }

    /// The INITIAL reading is deduplicated too: a second watcher on a session whose path is
    /// already reported pushes nothing. Without this, every session opened to a peer would re-push
    /// the path a consumer already holds.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_watcher_on_an_already_reported_path_emits_nothing() {
        let (mesh, conn, eid, _guards) = an_already_direct_connection("ivan").await;
        let mut rx = mesh.reach_bcast.subscribe();
        let mut events = conn.path_events();

        let _first = spawn(mesh.clone(), eid, &conn);
        let frame = tokio::time::timeout(INITIAL_READING_WAIT, rx.recv())
            .await
            .expect("precondition: the first watcher's initial reading pushes the path")
            .expect("broadcast channel alive");
        assert_eq!(frame.peer.path, PeerPath::Direct);

        let _second = spawn(mesh.clone(), eid, &conn);
        let second = tokio::time::timeout(INITIAL_READING_WAIT, rx.recv()).await;
        assert!(
            !saw_selected(&mut events).await,
            "a Selected event arrived during the test, so silence would not isolate the initial \
             reading"
        );
        assert!(
            second.is_err(),
            "the second watcher's initial reading found the path unchanged and must push nothing, \
             got {second:?}"
        );
    }

    /// The initial reading and a later `Selected` for the SAME path must not both emit: the event
    /// arm runs the same `observe`, and `decide` finds nothing changed the second time.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_selected_event_for_the_path_already_reported_emits_nothing() {
        let (mesh, conn, eid, _guards) = an_already_direct_connection("grace").await;
        let mut rx = mesh.reach_bcast.subscribe();
        let weak = conn.weak_handle();

        assert!(
            observe(&mesh, eid, &weak, crate::daemon::reach::selected_path).await,
            "the connection is alive"
        );
        let first = rx.try_recv().expect("the initial reading pushes the path");
        assert_eq!(first.peer.path, PeerPath::Direct);

        // Exactly what the watcher does on a `Selected` event for the path it already reported.
        assert!(
            observe(&mesh, eid, &weak, crate::daemon::reach::selected_path).await,
            "the connection is alive"
        );
        assert!(
            matches!(
                rx.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ),
            "a second observation of an unchanged path must not push a duplicate frame"
        );
    }

    /// An `Unknown` reading is never pushed and never seeds the cache — so the initial reading
    /// cannot turn "we do not know" (a connection with no selected path) into a frame. Driven
    /// through the real `observe` on a live connection, with only the reading substituted: a
    /// closed loopback connection still reports its last selected path, so it cannot supply one.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_reading_emits_nothing() {
        let (mesh, conn, eid, _guards) = an_already_direct_connection("heidi").await;
        let mut rx = mesh.reach_bcast.subscribe();
        let weak = conn.weak_handle();

        assert!(
            observe(&mesh, eid, &weak, |_| PeerPath::Unknown).await,
            "the connection is alive, so the reading really is taken"
        );
        assert!(
            matches!(
                rx.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ),
            "an Unknown reading must not be pushed"
        );
        assert!(
            mesh.reachability.lock().unwrap().get(&eid).is_none(),
            "an Unknown reading must not seed the cache either"
        );

        // The same call with the REAL reading does push — so the silence above is the Unknown
        // rule, not a fixture that can never emit.
        assert!(observe(&mesh, eid, &weak, crate::daemon::reach::selected_path).await);
        assert_eq!(
            rx.try_recv().expect("a Direct reading pushes").peer.path,
            PeerPath::Direct
        );
    }

    /// #223: a live session to a REVOKED peer commits no observation — no seeded `reachable: true`
    /// row, no `Session` frame. Outbound sessions are not severed on revocation (#229), so this
    /// watcher can outlive the revocation; the reachability probe refuses to write that row, and the
    /// watcher must not write it instead. The control is the same `observe` after the revocation is
    /// lifted, which pushes — so the silence is the revocation, not a fixture that cannot emit.
    ///
    /// Deleting the `dial_refused` check in `observe` fails the first two assertions.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_to_a_revoked_peer_commits_no_observation() {
        let (mesh, conn, eid, _guards) = an_already_direct_connection("mallet").await;
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
        let mut rx = mesh.reach_bcast.subscribe();
        let weak = conn.weak_handle();

        assert!(
            observe(&mesh, eid, &weak, crate::daemon::reach::selected_path).await,
            "the connection is alive, so the watcher keeps running"
        );
        assert!(
            matches!(
                rx.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ),
            "a session to a revoked peer must not push a frame"
        );
        assert!(
            mesh.reachability.lock().unwrap().get(&eid).is_none(),
            "a session to a revoked peer must not seed a reachable row"
        );

        assert!(mesh.store.unrevoke(&eid).unwrap());
        assert!(observe(&mesh, eid, &weak, crate::daemon::reach::selected_path).await);
        assert_eq!(
            rx.try_recv()
                .expect("once unrevoked, the reading pushes")
                .peer
                .path,
            PeerPath::Direct
        );
    }

    /// A relay URL change is a real change: it names WHICH relay carries the data, and moving
    /// between relays is a different operational fact than staying put.
    #[test]
    fn a_different_relay_url_is_a_change() {
        let a = PeerPath::Relay {
            url: Some("https://a.example".into()),
        };
        let b = PeerPath::Relay {
            url: Some("https://b.example".into()),
        };
        assert_eq!(decide(&b, Some(&a)), Some(b.clone()));
        assert_eq!(decide(&a, Some(&a)), None);
    }
}
