//! Keeping the persisted dial hint honest (#124).
//!
//! `PeerEntry::last_addr` is the pairing-proven dial-back hint: the address we attach to a dial so
//! a just-paired peer is reachable without waiting on discovery (#27). Until now it was written in
//! exactly ONE place — the pairing rendezvous — and never updated again.
//!
//! That made it permanently wrong the moment a peer changed networks. `stored_dial_addr` REPLACES
//! the bare-id dial with the hint, so a stale entry means the direct-candidate set is exactly one
//! dead address: every dial and every probe offers iroh that address, fails, and falls back to the
//! relay. Forever, because nothing rewrote it. An embedder measured it as a whole fleet stuck on
//! relay at ~800ms while a freshly-paired identity on the same two machines punched direct at
//! 11-23ms, and it survived daemon restarts because the hint lives in redb (#124).
//!
//! The fix is to refresh the hint from a peer we are demonstrably talking to. A live authenticated
//! connection knows the remote's ACTUAL address; that is ground truth, and writing it back makes
//! the hint self-healing across network changes. A stale hint then costs one failed attempt rather
//! than being permanent.

use std::sync::Arc;

use super::MeshState;

/// Refresh `last_addr` for `endpoint_id` from a LIVE connection, if it has changed (#124).
///
/// Best-effort and deliberately quiet: this is cache maintenance on a path whose real job is
/// already done, so every failure — an unknown peer, an empty path snapshot, a serialize error, a
/// redb write error — leaves the stored hint exactly as it was. A stale hint is a slow dial; a
/// *wrong* write could be worse, so the bar for replacing one is a connection that is up.
///
/// **Only writes on a CHANGE**, decided inside the store's write transaction. The value is not
/// static — it flips as a session's open IP paths change, and across a restart — so this is a real
/// reduction, not the "changes approximately never" an earlier version claimed.
///
/// **Never downgrades `Some` to `None`**, matching the pairing path's rule: an empty path snapshot
/// means we learned nothing, not that the peer has no address.
pub(crate) fn refresh(mesh: &Arc<MeshState>, endpoint_id: [u8; 32], observed: String) {
    let mesh = mesh.clone();
    // Detached, and taking NO lock. The atomicity lives in the store's single write transaction —
    // a lock here would exclude only `rename_peer`, since the pairing writes and `add_peer` take
    // none. The caller is #92's path watcher, whose own job is to push a frame WHEN IT HAPPENS, so
    // it must not queue behind cache maintenance. The redb work blocks: blocking pool (fs rule).
    tokio::task::spawn_blocking(move || write_if_changed(&mesh, endpoint_id, observed));
}

/// The store half of [`refresh`], separated so the rules are testable without a live connection:
/// replace a changed hint, skip an unchanged one, and never CREATE a peer.
fn write_if_changed(mesh: &Arc<MeshState>, endpoint_id: [u8; 32], observed: String) {
    // ONE redb write transaction does the read, the change-check and the write. A lock cannot
    // substitute: the pairing writes and `add_peer` take none, so a `reload_lock`-guarded
    // resolve-then-add excludes only `rename_peer` and still lost a verified `user_id` at a
    // measured 33% under a concurrent re-pair (#124 review).
    match mesh.store.set_last_addr(&endpoint_id, &observed) {
        Ok(true) => tracing::debug!("refreshed the dial hint from a live connection (#124)"),
        Ok(false) => {} // unchanged, or not a stored peer — both are no-ops by design
        // Cache maintenance: the session is already up and unaffected.
        Err(e) => tracing::debug!(%e, "could not persist refreshed dial hint"),
    }
}

/// The remote's DIRECT address as this connection sees it, serialized the way
/// [`crate::allowlist::PeerEntry::last_addr`] stores them.
///
/// **Filters to IP paths, and returns `None` when only a relay path is open (#124 review).** The
/// first version took every path unfiltered, which is actively harmful: persisting a relay URL
/// replaces a peer's stored DIRECT candidate with one that can never punch. Measured — a seeded
/// direct hint was overwritten with a relay URL and stayed that way for the rest of the session —
/// and it manufactures in production exactly the state this repo's own tests seed deliberately to
/// force a relayed session. A relay URL must never become a dial hint.
///
/// (The #124-era wording said a hint "replaces the bare-id dial". It does not, and the correction
/// matters for reading #140: `handle_msg_resolve_remote` inserts our addresses as candidate paths
/// with `Source::App` and then still triggers address lookup, so the hint is MERGED with discovery.
/// What it replaces is the previously STORED hint, which is why writing a relay URL over a direct
/// one is the real harm.)
///
/// `None` (empty snapshot, no IP path, or a serialize failure) means "learned nothing", which the
/// caller treats as leave-alone rather than clear.
/// The MERGE rule for a dial hint: a fresh observation refreshes it, learning nothing preserves
/// what is stored. **Never downgrades `Some` to `None`.**
///
/// One function so the rule cannot differ between the four writers — and it very nearly did. Until
/// 0.52.2 `redeem_invite`'s value was always `Some` (the invite's address list), so that call site
/// needed no guard and had none; switching its source to [`observed_for`] made the missing guard
/// load-bearing overnight, and a re-redeem completing over the relay wiped a proven direct hint
/// while five separate places asserted the opposite. The other three writers each had their own
/// copy of the `or_else`.
pub(crate) fn merge_hint(observed: Option<String>, existing: Option<String>) -> Option<String> {
    observed.or(existing)
}

pub(crate) fn observed_for(conn: &iroh::endpoint::Connection) -> Option<String> {
    let paths = conn.paths();
    // EVERY open IP path, with no preference expressed — deliberately. An earlier version ordered
    // selected-first, which reads like #64's rule but is dead code: `EndpointAddr::from_parts`
    // collects into a `BTreeSet`, so ordering is discarded and the two forms serialize
    // byte-identically (#124 review). Each open QUIC path is a validated 4-tuple we reached this
    // peer at, so storing them all is correct for a dial hint; the canonical set ordering is also
    // what makes the unchanged-check stable.
    let addrs: Vec<iroh::TransportAddr> = paths
        .iter()
        .filter(|p| p.is_ip())
        .map(|p| p.remote_addr().clone())
        .collect();
    if addrs.is_empty() {
        return None; // relay-only, or nothing open: keep whatever we already had
    }
    serde_json::to_string(&iroh::EndpointAddr::from_parts(conn.remote_id(), addrs)).ok()
}

#[cfg(test)]
mod merge_tests {
    use super::merge_hint;

    /// #203 gate: learning nothing PRESERVES a stored hint. Never `Some` -> `None`.
    ///
    /// This is the rule all four writers share, and the one `redeem_invite` was missing when 0.52.2
    /// changed its source to `observed_for`: `store.add` is a replace-on-id upsert, so a relay-only
    /// re-redeem wrote `None` straight over a proven direct hint. There is no `peer_hint_set`, and
    /// on a pair that cannot punch nothing else ever rewrites it — so that loss is permanent.
    ///
    /// A loopback ceremony always observes a direct path and therefore cannot reach the `None`
    /// branch, which is why this is asserted on the rule rather than through a pairing.
    #[test]
    fn learning_nothing_preserves_the_stored_hint() {
        const PROVEN: &str = r#"{"id":"ab","addrs":[{"Ip":"192.168.7.7:4433"}]}"#;
        const FRESH: &str = r#"{"id":"ab","addrs":[{"Ip":"10.0.0.2:4433"}]}"#;

        assert_eq!(
            merge_hint(None, Some(PROVEN.into())).as_deref(),
            Some(PROVEN),
            "a relay-only observation must not wipe a proven hint"
        );
        assert_eq!(
            merge_hint(Some(FRESH.into()), Some(PROVEN.into())).as_deref(),
            Some(FRESH),
            "a fresh observation REFRESHES — the whole point of #124"
        );
        assert_eq!(merge_hint(Some(FRESH.into()), None).as_deref(), Some(FRESH));
        assert_eq!(merge_hint(None, None), None, "nothing known stays nothing");
    }
}

#[cfg(test)]
mod tests {
    use crate::allowlist::PeerEntry;

    /// #124: a hint that has gone stale must be REPLACED by what a live connection observes.
    ///
    /// This is the bug the embedder measured: after a network change both roots held each other's
    /// pre-change address, `stored_dial_addr` handed iroh that one dead address as the entire
    /// direct-candidate set, and every session fell back to the relay — permanently, because
    /// nothing rewrote the hint. A freshly-paired identity on the same hardware punched direct.
    ///
    /// Driven through the same store the daemon uses, with the observed address supplied directly:
    /// pinning it through a real network change would need two machines and a hotspot.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_hint_is_replaced_and_an_unchanged_one_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "").unwrap();
        let mesh = crate::daemon::testutil::hermetic_mesh(cfg).await;
        let eid = [3u8; 32];

        let stale = r#"{"id":"stale","addrs":[]}"#.to_string();
        mesh.store
            .add(PeerEntry {
                endpoint_id: eid,
                nickname: "bob".into(),
                services: vec!["notes".into(), "kb".into()],
                // NON-EMPTY deliberately (#124 third review): the earlier fixture seeded these
                // empty, so mutations that clobbered `user_id`/`services`/`paired_at` all
                // SURVIVED — including the loss of a verified `user_id`, which is the entire
                // reason this code takes the care it does.
                paired_at: Some("2026-07-29T00:00:00Z".into()),
                user_id: Some("b64u:someuser".into()),
                last_addr: Some(stale.clone()),
            })
            .unwrap();

        // What a live connection would observe now — a DIFFERENT address.
        let fresh = r#"{"id":"fresh","addrs":[]}"#.to_string();
        super::write_if_changed(&mesh, eid, fresh.clone());
        assert_eq!(
            mesh.store.resolve(&eid).unwrap().unwrap().last_addr,
            Some(fresh.clone()),
            "a live observation must overwrite a stale hint — leaving it is #124: the peer is \
             dialed at a dead address forever and every session falls back to the relay"
        );

        // Re-observing the SAME address must SKIP the write. Asserting the value is unchanged
        // would be vacuous — a rewrite stores an identical value (#124 review caught exactly that)
        // — so assert on the store's own report of whether it wrote.
        assert!(
            !mesh
                .store
                .set_last_addr(&eid, &fresh)
                .expect("store readable"),
            "an unchanged observation must not write: every Selected event would otherwise cost a \
             blocking redb write"
        );
        assert!(
            mesh.store
                .set_last_addr(&eid, r#"{"id":"newer","addrs":[]}"#)
                .expect("store readable"),
            "and a CHANGED observation must write"
        );

        // An unknown peer is not invented — refresh is maintenance on a stored row, not a source
        // of trust. A bare-eid probe must never create an allowlist entry.
        super::write_if_changed(&mesh, [9u8; 32], fresh);
        assert!(
            mesh.store.resolve(&[9u8; 32]).unwrap().is_none(),
            "refreshing must never CREATE a peer — that would be an authorization path"
        );

        // EVERY identity field the pairing path protects must survive a hint refresh. `user_id`
        // above all: a resolve-then-add refresh downgraded a verified one to `None` at a measured
        // 33% under a concurrent re-pair, which is why the write is a single transaction.
        let row = mesh.store.resolve(&eid).unwrap().unwrap();
        assert_eq!(row.nickname, "bob");
        assert_eq!(row.endpoint_id, eid);
        assert_eq!(
            row.user_id.as_deref(),
            Some("b64u:someuser"),
            "a verified user_id must NEVER be downgraded by a dial-hint refresh"
        );
        assert_eq!(
            row.services,
            vec!["notes".to_string(), "kb".to_string()],
            "the dial directory must survive — clobbering it is the reverse-pairing bug"
        );
        assert_eq!(
            row.paired_at.as_deref(),
            Some("2026-07-29T00:00:00Z"),
            "the original pairing stamp stands"
        );
    }
}
