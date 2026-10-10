//! The roster-mode gate. [`RosterGate`] resolves an endpoint via the
//! installed [`RosterView`] (hot-swappable on install), degraded-aware. [`ComposedGate`] layers it
//! over the pairing [`AllowlistGate`] with explicit precedence (revocation → roster → pairing).
use std::sync::{Arc, RwLock};

use mcpmesh_net::{EndpointId, PeerIdentity, TrustGate};
use mcpmesh_trust::roster::validate::{RosterState, RosterView};

use crate::allowlist::{Admission, AllowlistGate};
use crate::util::epoch_now_i64 as now_epoch;

/// Default degraded grace window. Config can
/// override it; the gate takes the effective seconds at construction.
pub const DEFAULT_GRACE_SECS: i64 = 72 * 3600;

/// Roster-mode [`TrustGate`]: resolves an inbound endpoint to its user/device/group identity via
/// the installed roster, refusing anything absent / revoked / degraded-stopped. The installed
/// view is behind an `RwLock<Option<Arc<..>>>` so [`install`](Self::install) HOT-SWAPS it live
/// (the resolve path takes a cheap read lock, dropped before returning and NEVER held across an
/// await — `TrustGate::resolve`/`is_revoked` are sync). An empty gate (no roster) resolves nothing
/// — a pure-pairing daemon composes exactly like a pairing-only build.
pub struct RosterGate {
    view: RwLock<Option<Arc<RosterView>>>,
    grace_secs: i64,
    /// The last instant this node validated the installed roster as current via an authenticated
    /// channel — bumped by [`set_last_confirmed`](Self::set_last_confirmed) on every
    /// confirmation event. `None` imposes NO freshness constraint (`empty`/`with_grace`
    /// leave it `None`). Behind its own `RwLock` so a confirmation hot-updates it live, exactly like
    /// [`install`](Self::install) hot-swaps the view — the resolve/sever paths take a cheap read.
    last_confirmed: RwLock<Option<i64>>,
    /// The freshness bound in seconds. `i64::MAX` in the freshness-less
    /// constructors (`empty`/`with_grace`) → the staleness fold is never reached → expiry-governed only.
    max_staleness: i64,
}

impl RosterGate {
    /// A gate with no roster installed (pure-pairing default; also the startup state before load).
    /// `last_confirmed = None`, `max_staleness = i64::MAX` → NO freshness constraint
    /// (the effective state collapses to the expiry-only `state`).
    pub fn empty() -> Self {
        Self {
            view: RwLock::new(None),
            grace_secs: DEFAULT_GRACE_SECS,
            last_confirmed: RwLock::new(None),
            max_staleness: i64::MAX,
        }
    }

    /// A gate with a configured grace window but NO freshness constraint
    /// (`last_confirmed = None`, `max_staleness = i64::MAX`). Retained for callers that want the
    /// expiry-only degraded machine; the roster daemon uses [`with_freshness`](Self::with_freshness).
    pub fn with_grace(grace_secs: i64) -> Self {
        Self {
            view: RwLock::new(None),
            grace_secs,
            last_confirmed: RwLock::new(None),
            max_staleness: i64::MAX,
        }
    }

    /// The roster-daemon constructor: a configured grace window AND a freshness bound.
    /// `last_confirmed` starts `None` (no constraint until the daemon loads/bootstraps it — the one-time
    /// upgrade grace) and is armed by [`set_last_confirmed`](Self::set_last_confirmed) on every
    /// confirmation event (URL poll ≥ installed, gossip install, manual install).
    pub fn with_freshness(grace_secs: i64, max_staleness: i64) -> Self {
        Self {
            view: RwLock::new(None),
            grace_secs,
            last_confirmed: RwLock::new(None),
            max_staleness,
        }
    }

    /// Record that the installed roster was confirmed current at `epoch` (the freshness bump). Hot-
    /// updates the live gate — the very next `resolve`/`should_sever_now` sees it (no gate rebuild).
    pub fn set_last_confirmed(&self, epoch: i64) {
        *self.last_confirmed.write().expect("freshness lock") = Some(epoch);
    }

    /// The current `last_confirmed`, or `None` when never confirmed / no freshness constraint. The
    /// daemon's status/warn paths read it to compute the SAME `effective_state` the gate resolves on.
    pub fn last_confirmed(&self) -> Option<i64> {
        *self.last_confirmed.read().expect("freshness lock")
    }

    /// The gate's EFFECTIVE roster state at `now_epoch` — the MORE-degraded of expiry and staleness
    /// — or `None` when
    /// no roster is installed. Consulted by `resolve`: a DegradedStopped gate denies NEW inbound
    /// (`resolve → None → 401`), bounding NEW-connection adversarial staleness at `max_staleness +
    /// grace`. It is NOT consulted by `should_sever_now` (register-time only; cutting EXISTING
    /// staleness-degraded sessions needs the periodic sweep — see `serve_forever`).
    pub fn effective_state(&self, now_epoch: i64) -> Option<RosterState> {
        let view = self.view()?;
        Some(view.effective_state(
            now_epoch,
            self.grace_secs,
            self.last_confirmed(),
            self.max_staleness,
        ))
    }

    /// Hot-swap the installed roster view (install path / startup load). The very next `resolve`
    /// sees it — no gate rebuild. Called by the daemon after `RosterStore::install_from_file`
    /// (the install path).
    pub fn install(&self, view: RosterView) {
        *self.view.write().expect("roster gate lock not poisoned") = Some(Arc::new(view));
    }

    /// A snapshot of the installed view (for the sever set + status), or `None`. The read lock
    /// is released as the `Arc` clone returns — never held across an await.
    pub fn view(&self) -> Option<Arc<RosterView>> {
        self.view
            .read()
            .expect("roster gate lock not poisoned")
            .clone()
    }
}

impl TrustGate for RosterGate {
    /// Resolve a rostered device to `PeerIdentity{ name: user_id, user_id: Some, groups }`, or
    /// `None` (absent / revoked / degraded-STOPPED). In degraded-GRACE the roster still resolves
    /// (existing behavior continues) — the warning is surfaced daemon-side.
    fn resolve(&self, endpoint: &EndpointId) -> Option<PeerIdentity> {
        let view = self.view()?;
        // Effective degraded-stopped (expiry OR staleness past grace) → roster
        // authorizes nothing: a stale/expired roster must not admit NEW connections. This is the
        // freshness bound for NEW inbound (existing sessions are re-evaluated only on install/restart;
        // the time-triggered staleness sweep runs separately — see `serve_forever`).
        if self.effective_state(now_epoch()) == Some(RosterState::DegradedStopped) {
            return None;
        }
        // Revocation wins over any active listing (also enforced in build_view, belt-and-suspenders).
        if view.is_revoked(endpoint.as_bytes()) {
            return None;
        }
        let d = view.resolve(endpoint.as_bytes())?;
        Some(PeerIdentity {
            endpoint: *endpoint,
            name: d.user_id.clone(), // name == user_id (§6.3 / the select_service arm)
            user_id: Some(d.user_id.clone()), // Some(..) marks this a roster identity (D8 rule)
            groups: d.groups.clone(),
        })
    }

    /// Is this endpoint revoked by the installed roster? Honored regardless of degraded state
    /// (fail-closed) — even a degraded-STOPPED roster still enforces its last-known revocations.
    /// The composed gate consults this FIRST; an empty gate → `false`.
    fn is_revoked(&self, endpoint: &EndpointId) -> bool {
        self.view()
            .map(|v| v.is_revoked(endpoint.as_bytes()))
            .unwrap_or(false)
    }

    /// The roster-resolved user_id for the sever discriminator: `Some` IFF this endpoint would
    /// resolve via the ROSTER right now (absent/revoked/degraded-stopped → `None`), reusing the exact
    /// `resolve` precedence. A purely-paired endpoint (not in the roster) is `None`, so it is never
    /// severed by a roster install even though its `PeerEntry` may carry a verified binding user_id.
    fn roster_user(&self, endpoint: &EndpointId) -> Option<String> {
        self.resolve(endpoint).and_then(|id| id.user_id)
    }
}

/// The daemon's composed trust gate: pairing ∪ roster with explicit precedence.
///  1. the installed roster's `revoked_endpoints` are consulted FIRST — a revoked endpoint is
///     refused even if a pair entry exists (revocation wins over pairing, across all ALPNs);
///  2. an endpoint present (active) in the roster resolves to its ROSTER identity even if a pair
///     entry exists (roster masks the pair nickname) — unless that roster `user_id` is spelled as a
///     `b64u:` identity `peer_revoke` revoked (#218);
///  3. otherwise fall through to the pair entry (pairing mode continues untouched).
///
/// With an empty roster this is exactly the plain `AllowlistGate` behavior (falls through for
/// everything) — so every pairing test/flow is preserved.
pub struct ComposedGate {
    roster: Arc<RosterGate>,
    pairs: Arc<AllowlistGate>,
}

impl ComposedGate {
    pub fn new(roster: Arc<RosterGate>, pairs: Arc<AllowlistGate>) -> Self {
        Self { roster, pairs }
    }
    /// The roster gate handle — the daemon keeps it to hot-swap on install + compute the sever set.
    pub fn roster(&self) -> &Arc<RosterGate> {
        &self.roster
    }

    /// Is this ROSTER `user_id` a revoked pairing identity (#218)? `org_approve` takes a free-form
    /// `user_id`, so a roster can name a user `b64u:x`; without this, rule 2 would admit a new
    /// rostered device under exactly the string `peer_revoke b64u:x` revoked, with no pair row for
    /// rule 1 to refuse. The rule itself lives in [`PeerStore::is_roster_user_revoked`], which the
    /// OUTBOUND dial filter calls too (#223) — one definition, so the directions cannot disagree.
    ///
    /// [`PeerStore::is_roster_user_revoked`]: crate::allowlist::PeerStore::is_roster_user_revoked
    fn roster_identity_revoked(&self, roster_user_id: Option<&str>) -> bool {
        roster_user_id.is_some_and(|u| self.pairs.is_roster_user_revoked(u))
    }
}

impl TrustGate for ComposedGate {
    /// The sever discriminator: roster-resolved IFF the endpoint is in the roster
    /// (delegates to `RosterGate::roster_user`). Deliberately NOT `resolve(endpoint).user_id`: a
    /// paired-only peer resolves to its PAIR identity which — since the self-sovereign-binding
    /// feature — carries a `user_id`, so keying the sever on `identity.user_id` would sever
    /// legitimately-paired peers on a roster install. Keying on roster membership keeps them.
    fn roster_user(&self, endpoint: &EndpointId) -> Option<String> {
        self.roster.roster_user(endpoint)
    }

    fn resolve(&self, endpoint: &EndpointId) -> Option<PeerIdentity> {
        // (1) revocation wins, all ALPNs — from EITHER source (#85 ask 4). The pairing revocation
        // list is not a roster concept, so asking only `self.roster` here would silently drop it on
        // any node that installed a roster: a device its owner declared stolen would be refused in
        // pairing mode and admitted the moment the operator joined an org. The pairing half is ONE
        // store transaction (#218) that also yields the pair row rule (3) needs — asking
        // `is_revoked` and then `resolve` used to cost six.
        if self.roster.is_revoked(endpoint) {
            return None;
        }
        let pair = self.pairs.admission(endpoint);
        if pair == Admission::Refused {
            return None;
        }
        // (2) roster masks the pair entry.
        if let Some(id) = self.roster.resolve(endpoint) {
            if self.roster_identity_revoked(id.user_id.as_deref()) {
                return None;
            }
            return Some(id);
        }
        // (3) fall through to pairing (also the DegradedStopped path: RosterGate::resolve returns
        //     None when stopped, so a rostered-AND-paired peer falls through to its PAIR nickname —
        //     see the DegradedStopped×paired test + DECLARE below).
        match pair {
            Admission::Admitted(e) => Some(AllowlistGate::identity_of(endpoint, e)),
            Admission::Refused | Admission::Unpaired => None,
        }
    }

    /// The check-register recheck asks the LIVE gate whether an endpoint is currently
    /// revoked. Delegate to the installed roster's revoked set — honored regardless of degraded
    /// state (fail-closed). This is what closes the TOCTOU race: a connection registering after a
    /// revoking install sees `true` here and self-closes.
    fn is_revoked(&self, endpoint: &EndpointId) -> bool {
        // EITHER source (#85 ask 4) — see `resolve`'s rule 1 — and rule 2's identity check (#218).
        self.roster.is_revoked(endpoint)
            || self.pairs.is_revoked(endpoint)
            || self.roster_identity_revoked(self.roster.roster_user(endpoint).as_deref())
    }

    /// The register-time recheck: sever iff the endpoint is REVOKED (revocation wins, all
    /// ALPNs), OR it was roster-resolved (`roster_user.is_some()`) AND is now ABSENT from the
    /// installed roster (the dropped-from-roster half). CRITICAL: the absence test
    /// consults the ROSTER gate's own `view().resolve` (NOT the composed resolve) — a dropped
    /// endpoint that ALSO holds a pair entry must still be caught (`roster.resolve → None` even when
    /// `pairs.resolve → Some`). Mirrors `mcpmesh_net::should_sever` for one endpoint against the
    /// roster's CURRENT view; honored regardless of degraded state (revocation is fail-closed).
    ///
    /// STALENESS is deliberately NOT checked here: this recheck runs ONLY at register time (a NEW
    /// connection), and staleness is time-triggered with no event, so it cannot re-evaluate an
    /// already-established session as the clock crosses `last_confirmed + max_staleness + grace`. The
    /// freshness bound is enforced at `resolve` (NEW inbound → `None` → 401); cutting EXISTING
    /// roster-authorized sessions on staleness needs the periodic sweep (see
    /// `serve_forever`). An `effective_state == DegradedStopped` arm here would also over-reach
    /// (severing purely-paired peers, `roster_user == None`, whose authorization is independent of the
    /// org roster) while delivering nothing for roster peers (already denied at `resolve`).
    fn should_sever_now(&self, endpoint: &EndpointId, roster_user: Option<&str>) -> bool {
        self.pairs.is_revoked(endpoint)
            || self.roster.is_revoked(endpoint)
            || self.roster_identity_revoked(roster_user)
            || (roster_user.is_some()
                && self
                    .roster
                    .view()
                    .is_some_and(|v| v.resolve(endpoint.as_bytes()).is_none()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use mcpmesh_net::TrustGate;
    use mcpmesh_trust::roster::sign::mint_signed;
    use mcpmesh_trust::roster::validate::load_installed;
    use mcpmesh_trust::roster::{Roster, RosterDevice, RosterUser, encode_b64u};

    // Build a RosterView for GATE tests via `load_installed` (verifies the sig + structural rules
    // but NOT expiry/serial) — so an intentionally-EXPIRED `expires` (the degraded tests) still
    // yields a view; degraded state is then computed at resolve-time from the real clock. (The
    // install-time expiry rejection is exercised in the trust crate's tests, not here.)
    fn view(
        serial: u64,
        expires: &str,
        revoke_laptop: bool,
    ) -> mcpmesh_trust::roster::validate::RosterView {
        let root = SigningKey::from_bytes(&[9u8; 32]);
        let r = mint_signed(
            &root,
            Roster {
                format: "mcpmesh-roster/1".into(),
                org_id: "acme".into(),
                serial,
                issued_at: "2000-01-01T00:00:00Z".into(),
                expires_at: expires.into(),
                groups: vec!["team-eng".into(), "all".into()],
                users: vec![RosterUser {
                    user_id: "alice".into(),
                    display_name: "Alice".into(),
                    user_pk: encode_b64u(&[1u8; 32]),
                    groups: vec!["team-eng".into(), "all".into()],
                    devices: vec![RosterDevice {
                        endpoint_id: encode_b64u(&[2u8; 32]),
                        label: "laptop".into(),
                        role: "primary".into(),
                    }],
                }],
                revoked_endpoints: if revoke_laptop {
                    vec![encode_b64u(&[2u8; 32])]
                } else {
                    vec![]
                },
                successor_root_pk: None,
                successor_sig: None,
                sig: String::new(),
            },
        );
        load_installed(&r, &root.verifying_key()).unwrap()
    }

    #[test]
    fn resolves_rostered_device_to_user_identity() {
        let gate = RosterGate::empty();
        gate.install(view(5, "2999-01-01T00:00:00Z", false));
        let id = gate
            .resolve(&[2u8; 32].into())
            .expect("alice's laptop resolves");
        assert_eq!(id.name, "alice"); // name == user_id (§6.3)
        assert_eq!(id.user_id.as_deref(), Some("alice"));
        assert_eq!(id.groups, vec!["team-eng".to_string(), "all".to_string()]);
        // An unknown endpoint is refused.
        assert!(gate.resolve(&[42u8; 32].into()).is_none());
    }

    #[test]
    fn empty_gate_resolves_nothing_and_is_never_revoked() {
        let gate = RosterGate::empty(); // pure-pairing daemon (no roster installed)
        assert!(gate.resolve(&[2u8; 32].into()).is_none());
        assert!(!gate.is_revoked(&[2u8; 32].into()));
    }

    #[test]
    fn revoked_endpoint_does_not_resolve_and_is_flagged() {
        let gate = RosterGate::empty();
        gate.install(view(5, "2999-01-01T00:00:00Z", true)); // laptop revoked
        assert!(gate.resolve(&[2u8; 32].into()).is_none()); // revocation wins → no identity
        assert!(gate.is_revoked(&[2u8; 32].into())); // consulted first by the composed gate
    }

    #[test]
    fn is_revoked_and_resolve_honored_through_dyn_dispatch() {
        // Regression pin: production touches the gate ONLY through `Arc<dyn TrustGate>`
        // (ComposedGate, the accept loop). A concrete-receiver call would hit an inherent
        // `is_revoked` if one existed, masking a regression; the dyn path goes through the vtable,
        // so it FAILS loudly if `is_revoked` ever moves back off the trait impl (fail-OPEN).
        let gate = RosterGate::empty();
        gate.install(view(5, "2999-01-01T00:00:00Z", true)); // laptop revoked
        let g: &dyn TrustGate = &gate; // the PRODUCTION path (ComposedGate holds Arc<dyn>)
        assert!(g.is_revoked(&[2u8; 32].into())); // FAILS if is_revoked ever regresses to inherent
        assert!(g.resolve(&[2u8; 32].into()).is_none()); // revocation wins via dyn too
    }

    #[test]
    fn degraded_stopped_roster_resolves_nothing_but_still_honors_revocation() {
        // Expired long ago; with the default grace it is past-grace → DegradedStopped.
        let gate = RosterGate::empty();
        gate.install(view(5, "2001-01-01T00:00:00Z", true));
        // now is 2026 ≫ expires+grace → stopped: no roster identity is granted.
        assert!(gate.resolve(&[2u8; 32].into()).is_none());
        // But revocation is still enforced (fail-closed) even when stopped.
        assert!(gate.is_revoked(&[2u8; 32].into()));
    }

    #[test]
    fn stale_gate_stops_resolving_even_when_unexpired() {
        let gate = RosterGate::with_freshness(72 * 3600, 24 * 3600); // grace 72h, max_staleness 24h
        gate.install(view(5, "2999-01-01T00:00:00Z", false)); // never expiry-degraded
        gate.set_last_confirmed(now_epoch() - 24 * 3600 - 72 * 3600 - 10); // past max_staleness+grace
        assert!(
            gate.resolve(&[2u8; 32].into()).is_none(),
            "a stale roster stops granting identity"
        );
        gate.set_last_confirmed(now_epoch()); // fresh confirmation restores it
        assert!(
            gate.resolve(&[2u8; 32].into()).is_some(),
            "a freshly-confirmed roster resolves again"
        );
        assert!(!gate.is_revoked(&[2u8; 32].into())); // revocation honored regardless of freshness
    }

    #[test]
    fn composed_gate_precedence_is_exhaustive() {
        use crate::allowlist::{AllowlistGate, PeerEntry, PeerStore};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PeerStore::open(&dir.path().join("state.redb")).unwrap());
        // Pair entries for FOUR endpoints (nickname == "p-<n>").
        for n in [2u8, 3, 4, 5] {
            store
                .add(PeerEntry {
                    endpoint_id: [n; 32],
                    nickname: format!("p-{n}"),
                    services: vec![],
                    paired_at: None,
                    user_id: None,
                    last_addr: None,
                })
                .unwrap();
        }
        let pairs = Arc::new(AllowlistGate::new(store));

        // Roster: user "alice" owns [2;32] (also paired) and revokes [3;32] (also paired).
        let roster = Arc::new(RosterGate::empty());
        roster.install(view_with(&[(2u8, "alice")], &[3u8])); // helper below
        let composed = ComposedGate::new(roster, pairs);

        // (a) rostered + paired → the ROSTER identity (masks the pair nickname).
        let id = composed.resolve(&[2u8; 32].into()).expect("resolves");
        assert_eq!(id.user_id.as_deref(), Some("alice"));
        assert_eq!(id.name, "alice"); // NOT "p-2"

        // (b) revoked + paired → REFUSED (revocation wins over the pair entry).
        assert!(composed.resolve(&[3u8; 32].into()).is_none());

        // (c) paired only (not in roster) → the PAIR identity.
        let id = composed.resolve(&[4u8; 32].into()).expect("resolves");
        assert_eq!(id.user_id, None);
        assert_eq!(id.name, "p-4");

        // (d) neither (unknown, no pair, no roster) → REFUSED.
        assert!(composed.resolve(&[9u8; 32].into()).is_none());
        // (e) paired-only [5;32] still resolves (control: roster did not disturb it).
        assert_eq!(composed.resolve(&[5u8; 32].into()).unwrap().name, "p-5");
    }

    #[test]
    fn a_paired_peer_with_a_verified_binding_is_not_severed_by_a_roster() {
        use crate::allowlist::{AllowlistGate, PeerEntry, PeerStore};
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PeerStore::open(&dir.path().join("state.redb")).unwrap());
        // A PAIRING peer [7;32] that presented a verified device→user binding → `user_id: Some`,
        // NOT a member of the org roster. (The self-sovereign-binding feature made pairing peers
        // carry a user_id, so `identity.user_id.is_some()` no longer means "roster-resolved".)
        store
            .add(PeerEntry {
                endpoint_id: [7u8; 32],
                nickname: "bob-laptop".into(),
                services: vec![],
                paired_at: None,
                user_id: Some("b64u:BOB".into()),
                last_addr: None,
            })
            .unwrap();
        let pairs = Arc::new(AllowlistGate::new(store));
        // Roster mode: an installed roster that lists alice [2;32] only (NOT bob).
        let roster = Arc::new(RosterGate::empty());
        roster.install(view_with(&[(2u8, "alice")], &[]));
        let composed = ComposedGate::new(roster, pairs);

        // The paired peer resolves WITH its binding user_id (needed for authz/audiences)…
        let id = composed
            .resolve(&[7u8; 32].into())
            .expect("paired peer resolves");
        assert_eq!(id.user_id.as_deref(), Some("b64u:BOB"));
        // …but it is NOT roster-resolved, so the sever discriminator is None.
        assert_eq!(
            composed.roster_user(&[7u8; 32].into()),
            None,
            "a pairing-only peer must not be treated as roster-resolved"
        );
        // Therefore the register-time recheck must NOT sever it (the regression: keyed on the
        // binding user_id it WAS severed — asserted on the last line to pin the contrast).
        assert!(
            !composed.should_sever_now(
                &[7u8; 32].into(),
                composed.roster_user(&[7u8; 32].into()).as_deref()
            ),
            "a legitimately-paired peer must survive in roster mode"
        );
        // A genuinely roster-resolved peer still carries the discriminator (alice, in the roster).
        assert_eq!(
            composed.roster_user(&[2u8; 32].into()).as_deref(),
            Some("alice")
        );
        // Contrast — the OLD (buggy) discriminator (`identity.user_id`) DID sever the paired peer:
        assert!(
            composed.should_sever_now(&[7u8; 32].into(), Some("b64u:BOB")),
            "regression witness: keying the sever on identity.user_id severs a paired peer"
        );
    }

    #[test]
    fn degraded_stopped_rostered_and_paired_falls_through_to_pair_identity() {
        use crate::allowlist::{AllowlistGate, PeerEntry, PeerStore};
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PeerStore::open(&dir.path().join("state.redb")).unwrap());
        // [2;32] is BOTH a pair peer ("p-2") AND a rostered device ("alice") — but the roster is
        // DegradedStopped (expired long ago, past grace) so it stops resolving.
        store
            .add(PeerEntry {
                endpoint_id: [2u8; 32],
                nickname: "p-2".into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: None,
            })
            .unwrap();
        let pairs = Arc::new(AllowlistGate::new(store));
        let roster = Arc::new(RosterGate::empty());
        roster.install(view_with_expiry(
            &[(2u8, "alice")],
            &[],
            "2001-01-01T00:00:00Z",
        )); // stopped
        let composed = ComposedGate::new(roster, pairs);

        // DECLARE (intended behavior): a stale (DegradedStopped) org roster stops granting ROSTER
        // identity, but LOCAL pairing is independent of org-roster freshness — so the peer falls
        // through to its PAIR nickname "p-2" (not refused). Revocation still wins over this (step 1,
        // is_revoked is honored even when stopped) — a revoked-and-paired peer is still refused.
        let id = composed
            .resolve(&[2u8; 32].into())
            .expect("falls through to pairing");
        assert_eq!(id.name, "p-2");
        assert_eq!(id.user_id, None);
    }

    #[test]
    fn composed_is_revoked_honored_through_dyn_dispatch() {
        use crate::allowlist::{AllowlistGate, PeerEntry, PeerStore};
        use std::sync::Arc;
        // [3;32] is revoked+paired (the exhaustive test's arm (b) setup). We consult the gate ONLY
        // through `&dyn TrustGate` — the PRODUCTION path: the daemon holds `Arc<dyn TrustGate>` and
        // The check-register recheck calls `is_revoked` through the vtable to close the
        // register-after-revoke TOCTOU race. The other two ComposedGate tests only call `resolve()`,
        // so `ComposedGate::is_revoked` itself is otherwise untested; without this pin an inherent
        // regression would silently hit the trait default `false` = revocation fail-OPEN on the recheck.
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PeerStore::open(&dir.path().join("state.redb")).unwrap());
        store
            .add(PeerEntry {
                endpoint_id: [3u8; 32],
                nickname: "p-3".into(),
                services: vec![],
                paired_at: None,
                user_id: None,
                last_addr: None,
            })
            .unwrap();
        let pairs = Arc::new(AllowlistGate::new(store));
        let roster = Arc::new(RosterGate::empty());
        roster.install(view_with(&[(2u8, "alice")], &[3u8])); // roster revokes [3;32]
        let composed = ComposedGate::new(roster, pairs);

        let g: &dyn TrustGate = &composed; // the PRODUCTION path (daemon holds Arc<dyn TrustGate>)
        assert!(g.is_revoked(&[3u8; 32].into())); // FAILS if ComposedGate::is_revoked regresses to inherent
        assert!(g.resolve(&[3u8; 32].into()).is_none()); // revocation wins via dyn too
    }

    /// #218: rule 1 refuses a pair row whose IDENTITY is revoked — and it has to be rule 1, not the
    /// rule-3 fallthrough. [2;32] is BOTH rostered (active, alice) and paired with a row carrying
    /// `user_id = b64u:mallory`, so a gate that only learned about identity revocation inside
    /// `AllowlistGate::resolve` would never reach it: rule 2 returns alice's roster identity first.
    /// Consulted through `&dyn TrustGate`, the production path.
    ///
    /// Deleting the `identity_revoked` clause in `AllowlistGate::is_revoked` fails the first
    /// two assertions (rule 2 resolves the endpoint to `alice`).
    #[test]
    fn composed_rule_1_refuses_a_rostered_endpoint_whose_pair_identity_is_revoked() {
        use crate::allowlist::{AllowlistGate, PeerEntry, PeerStore, RevokedEntry};
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PeerStore::open(&dir.path().join("state.redb")).unwrap());
        for (b, uid) in [(2u8, "b64u:mallory"), (4u8, "b64u:alice")] {
            store
                .add(PeerEntry {
                    endpoint_id: [b; 32],
                    nickname: format!("p-{b}"),
                    services: vec![],
                    paired_at: None,
                    user_id: Some(uid.into()),
                    last_addr: None,
                })
                .unwrap();
        }
        store
            .revoke_user(
                "b64u:mallory",
                &RevokedEntry {
                    endpoint_id: [0u8; 32],
                    revoked_at: 1,
                    reason: None,
                    source: "local".into(),
                    signer_user_id: None,
                    issued_at: None,
                },
            )
            .unwrap();
        let roster = Arc::new(RosterGate::empty());
        roster.install(view_with(&[(2u8, "alice")], &[])); // [2;32] ACTIVE in the roster
        let composed = ComposedGate::new(roster, Arc::new(AllowlistGate::new(store.clone())));
        let g: &dyn TrustGate = &composed;

        assert!(
            g.is_revoked(&[2u8; 32].into()),
            "rule 1 must see the pair row's revoked identity"
        );
        assert!(
            g.resolve(&[2u8; 32].into()).is_none(),
            "…and refuse before rule 2 hands out the roster identity"
        );
        assert!(g.should_sever_now(&[2u8; 32].into(), Some("alice")));
        // The live identity's row is untouched by mallory's revocation.
        assert_eq!(
            g.resolve(&[4u8; 32].into())
                .expect("alice's row resolves")
                .name,
            "p-4"
        );
        assert!(!g.is_revoked(&[4u8; 32].into()));

        // The inverse: lifting the identity revocation hands [2;32] back to rule 2.
        assert!(store.unrevoke_user("b64u:mallory").unwrap());
        assert!(!g.is_revoked(&[2u8; 32].into()));
        assert_eq!(
            g.resolve(&[2u8; 32].into()).expect("resolves again").name,
            "alice",
            "the roster masks the pair entry once nothing refuses it"
        );
    }

    /// #218: rule 2 refuses a ROSTER identity spelled as a revoked pairing identity. `org_approve`
    /// takes a free-form `user_id`, so a roster can list a NEW device under `b64u:mallory` with no
    /// pair row at all — rule 1 has no row to read the identity from, and rule 2 used to hand out
    /// the roster identity. A roster user named anything else never touches the identity table.
    ///
    /// Deleting the `roster_identity_revoked` check in `ComposedGate::resolve` fails the first
    /// `is_none`; in `is_revoked` / `should_sever_now`, the assertions after it.
    #[test]
    fn composed_rule_2_refuses_a_roster_user_spelled_as_a_revoked_identity() {
        use crate::allowlist::{AllowlistGate, PeerStore, RevokedEntry};
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(PeerStore::open(&dir.path().join("state.redb")).unwrap());
        store
            .revoke_user(
                "b64u:mallory",
                &RevokedEntry {
                    endpoint_id: [0u8; 32],
                    revoked_at: 1,
                    reason: None,
                    source: "local".into(),
                    signer_user_id: None,
                    issued_at: None,
                },
            )
            .unwrap();
        let roster = Arc::new(RosterGate::empty());
        // [2;32] rostered as `b64u:mallory`, [4;32] as `alice`; NEITHER has a pair row.
        roster.install(view_with(&[(2u8, "b64u:mallory"), (4u8, "alice")], &[]));
        let composed = ComposedGate::new(roster, Arc::new(AllowlistGate::new(store.clone())));
        let g: &dyn TrustGate = &composed;
        let mallory: EndpointId = [2u8; 32].into();

        assert!(
            g.resolve(&mallory).is_none(),
            "rule 2 must not admit a roster user spelled as a revoked identity"
        );
        assert!(g.is_revoked(&mallory), "the check-register recheck sees it");
        assert!(g.should_sever_now(&mallory, Some("b64u:mallory")));
        assert_eq!(
            g.resolve(&[4u8; 32].into()).expect("alice resolves").name,
            "alice",
            "a roster user not spelled as a revoked identity is unaffected"
        );
        assert!(!g.is_revoked(&[4u8; 32].into()));

        assert!(store.unrevoke_user("b64u:mallory").unwrap());
        assert_eq!(
            g.resolve(&mallory).expect("resolves once unrevoked").name,
            "b64u:mallory"
        );
        assert!(!g.is_revoked(&mallory));
        assert!(!g.should_sever_now(&mallory, Some("b64u:mallory")));
    }

    #[test]
    fn should_sever_now_selects_revoked_and_dropped_through_dyn() {
        use std::sync::Arc;
        // A roster: alice owns [2;32] (active), revokes [3;32]. [4;32] is NOT in the roster.
        let roster = Arc::new(RosterGate::empty());
        roster.install(view_with(&[(2u8, "alice")], &[3u8]));
        let dir = tempfile::tempdir().unwrap();
        let store =
            Arc::new(crate::allowlist::PeerStore::open(&dir.path().join("s.redb")).unwrap());
        let composed = ComposedGate::new(
            roster,
            Arc::new(crate::allowlist::AllowlistGate::new(store)),
        );
        let g: &dyn TrustGate = &composed; // PRODUCTION path (daemon holds Arc<dyn TrustGate>)

        // Revoked → sever (regardless of source).
        assert!(g.should_sever_now(&[3u8; 32].into(), Some("alice")));
        assert!(g.should_sever_now(&[3u8; 32].into(), None));
        // Roster-resolved AND still active → KEEP.
        assert!(!g.should_sever_now(&[2u8; 32].into(), Some("alice")));
        // Was roster-resolved (roster_user Some) but now ABSENT from the roster → SEVER (the dropped
        // half; [4;32] is in NO user entry).
        assert!(g.should_sever_now(&[4u8; 32].into(), Some("bob")));
        // Pairing-only (roster_user None), not revoked, not in roster → KEEP (never severed by roster).
        assert!(!g.should_sever_now(&[4u8; 32].into(), None));
    }

    // Test helper: a RosterView with the given (endpoint_byte, user_id) active devices + revoked
    // bytes, valid far into the future. Built via `load_installed` (sig + structural rules only,
    // no rule-3 rejection) so `view_with_expiry` can also hand it an EXPIRED timestamp.
    fn view_with(
        active: &[(u8, &str)],
        revoked: &[u8],
    ) -> mcpmesh_trust::roster::validate::RosterView {
        view_with_expiry(active, revoked, "2999-01-01T00:00:00Z")
    }

    fn view_with_expiry(
        active: &[(u8, &str)],
        revoked: &[u8],
        expires: &str,
    ) -> mcpmesh_trust::roster::validate::RosterView {
        let root = SigningKey::from_bytes(&[9u8; 32]);
        let users = active
            .iter()
            .map(|(b, uid)| RosterUser {
                user_id: (*uid).into(),
                display_name: (*uid).into(),
                user_pk: encode_b64u(&[1u8; 32]),
                groups: vec!["team-eng".into()],
                devices: vec![RosterDevice {
                    endpoint_id: encode_b64u(&[*b; 32]),
                    label: "d".into(),
                    role: "primary".into(),
                }],
            })
            .collect();
        let r = mint_signed(
            &root,
            Roster {
                format: "mcpmesh-roster/1".into(),
                org_id: "acme".into(),
                serial: 5,
                issued_at: "2000-01-01T00:00:00Z".into(),
                expires_at: expires.into(),
                groups: vec!["team-eng".into()],
                users,
                revoked_endpoints: revoked.iter().map(|b| encode_b64u(&[*b; 32])).collect(),
                successor_root_pk: None,
                successor_sig: None,
                sig: String::new(),
            },
        );
        load_installed(&r, &root.verifying_key()).unwrap()
    }

    /// #85 ask 4: the COMPOSED gate must honour a PAIRING revocation, not only the roster's.
    ///
    /// The available mistake, and the one the design flagged before it was written: `ComposedGate`
    /// delegated all three revocation methods to `self.roster` alone. Left that way, a device its
    /// owner declared stolen is refused in pairing mode and **admitted the moment the operator
    /// joins an org** — the composition silently drops the newer list.
    #[test]
    fn the_composed_gate_honours_a_pairing_revocation_as_well_as_the_rosters() {
        use crate::allowlist::{PeerEntry, PeerStore, RevokedEntry};
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(PeerStore::open(&dir.path().join("p.redb")).unwrap());
        let eid = [21u8; 32];
        store
            .add(PeerEntry {
                endpoint_id: eid,
                nickname: "bob-laptop".into(),
                services: vec!["notes".into()],
                paired_at: None,
                user_id: Some("b64u:BOB".into()),
                last_addr: None,
            })
            .unwrap();
        let pairs = std::sync::Arc::new(crate::allowlist::AllowlistGate::new(store.clone()));
        // An EMPTY roster — the point is that the roster knows nothing about this endpoint, so
        // every refusal below has to come from the pairing list.
        let roster = std::sync::Arc::new(RosterGate::empty());
        let composed = ComposedGate::new(roster, pairs);
        let id: EndpointId = eid.into();

        assert!(
            composed.resolve(&id).is_some(),
            "precondition: the paired peer resolves through the composition first"
        );

        store
            .revoke(RevokedEntry {
                endpoint_id: eid,
                revoked_at: 1,
                reason: None,
                source: "signed".into(),
                signer_user_id: Some("b64u:BOB".into()),
                issued_at: Some(1),
            })
            .unwrap();

        assert!(
            composed.resolve(&id).is_none(),
            "a pairing revocation must refuse through the COMPOSED gate — this is the one a \
             roster-mode node actually uses"
        );
        assert!(
            composed.is_revoked(&id),
            "…including the check-register recheck, or a connection racing the revoke survives it"
        );
        assert!(
            composed.should_sever_now(&id, None),
            "…and the live-session sever, which is what makes revocation immediate (#54)"
        );
    }
}
