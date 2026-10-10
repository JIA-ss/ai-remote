//! The six validation rules (all MUST) + the resolvable [`RosterView`] a gate reads +
//! [`RosterState`] (degraded-mode computation). Rule 1 (signature) lives in
//! [`sign::verify`](crate::roster::sign::verify); this module runs it plus rules 2–6. Pure:
//! `now` and `installed_serial` are PARAMETERS (no clock, no I/O) so every rule is unit-testable.
use std::collections::{HashMap, HashSet};

use ed25519_dalek::VerifyingKey;

use super::sign::verify;
use super::{
    ROSTER_FORMAT, ROSTER_FORMAT_ROTATION, Roster, RosterError, SKEW_SECS, decode_endpoint_id,
};

/// A validated, resolvable roster — the lookup maps a `RosterGate` holds (built once at
/// install/load). Net-free: `resolve` returns `(user_id, groups)`; the cli maps that to a
/// `PeerIdentity`.
#[derive(Debug, Clone)]
pub struct RosterView {
    org_id: String,
    serial: u64,
    expires_at_epoch: i64,
    /// Active (NON-revoked) device endpoint → its owner's identity.
    devices: HashMap<[u8; 32], ResolvedDevice>,
    /// Every revoked endpoint (revocation wins over any active listing).
    revoked: HashSet<[u8; 32]>,
    /// The org's DECLARED group namespace (`roster.groups`), verbatim and in document order (#93).
    ///
    /// Kept because an embedder in roster mode has managed group membership it could not display:
    /// the view knew which groups each device carries but not which groups EXIST, so a UI could
    /// not offer the set to pick from. Rule 5b already validates every user group against this
    /// list; this just stops it being discarded afterwards.
    groups: Vec<String>,
    /// Every PERSON the roster carries, in document order — including one whose devices are all
    /// revoked (#93).
    ///
    /// Separate from `devices` because that map answers "which endpoint resolves to whom" and is
    /// therefore keyed by device, so a person with no active device does not exist in it. That is
    /// right for the GATE and wrong for a member list: revoking someone's only device is an
    /// ordinary operation that leaves their user entry in the signed roster, and deriving members
    /// from the device map made them indistinguishable from removed.
    users: Vec<RosterMemberEntry>,
}

/// One person in a [`RosterView`], for the membership read (#93). Device-independent: a member with
/// no active device still appears.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterMemberEntry {
    pub user_id: String,
    pub display_name: String,
    pub groups: Vec<String>,
}

/// A rostered device's resolved identity (the roster-mode half of a `PeerIdentity`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDevice {
    pub user_id: String,
    /// The owner's human display name (`RosterUser.display_name`) (#93). Display-only, exactly like
    /// [`label`](Self::label) — never an authorization input, which stays keyed on `user_id` and
    /// `groups`.
    ///
    /// Resolved here rather than left in the document because the roster is daemon-owned: an
    /// embedder that had to read `<root>/config/roster.json` to render a name would be hand-parsing
    /// state it is told not to touch.
    pub display_name: String,
    pub groups: Vec<String>,
    /// The device's role in its user's device set (`"primary"` | `"mirror"`; free-form otherwise).
    /// Feeds the person→device dial candidate ORDER (`devices_for_user`) — primary before mirror
    /// — never an authorization decision.
    pub role: String,
    /// The device's human label (`RosterDevice.label`). Display-only — the advisory
    /// presence read (`status`) renders it; never an authorization input.
    pub label: String,
}

/// Roster liveness (degraded mode). Computed from `expires_at` + a grace window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterState {
    /// now ≤ expires_at: full authority.
    Approved,
    /// expires_at < now ≤ expires_at + grace: keep serving, warn.
    DegradedGrace,
    /// now > expires_at + grace: inbound serving stops (roster authorizes nothing).
    DegradedStopped,
}

impl RosterView {
    pub fn org_id(&self) -> &str {
        &self.org_id
    }
    pub fn serial(&self) -> u64 {
        self.serial
    }
    pub fn expires_at_epoch(&self) -> i64 {
        self.expires_at_epoch
    }

    /// Resolve an ACTIVE (non-revoked) rostered device to its identity, else `None`.
    pub fn resolve(&self, endpoint: &[u8; 32]) -> Option<&ResolvedDevice> {
        self.devices.get(endpoint)
    }

    /// Is this endpoint revoked? Honored regardless of degraded state (fail-closed): a stale
    /// roster's last-known revocation list is strictly safer to keep than to drop.
    pub fn is_revoked(&self, endpoint: &[u8; 32]) -> bool {
        self.revoked.contains(endpoint)
    }

    /// All active device endpoints (the sever rule's "still in the new roster" set).
    pub fn device_endpoints(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.devices.keys()
    }

    /// Every active (non-revoked) device with its resolved identity + display fields — the
    /// advisory presence read (`status`) enumerates these and marks each `online` from
    /// the presence table. Revoked endpoints are absent (excluded at `build_view`). Iteration order
    /// is the underlying map's (unordered) — the caller sorts for a stable display.
    pub fn devices(&self) -> impl Iterator<Item = (&[u8; 32], &ResolvedDevice)> {
        self.devices.iter()
    }

    /// The ACTIVE (non-revoked) device endpoints + roles owned by `user_id`, ordered `"primary"`
    /// before `"mirror"` (any other role last), with endpoint bytes as a DETERMINISTIC within-role
    /// tiebreak (the `devices` map is unordered, so a total order is needed for a reproducible result).
    /// Empty for an unknown user.
    ///
    /// This is the person→device dial's CANDIDATE ORDER. Two invariants the dial
    /// leans on: (1) a REVOKED endpoint is NEVER returned — `build_view` already excludes revoked
    /// endpoints from `devices`, so a revoked device can never be a dial candidate; (2) EVERY active
    /// device of the user is returned regardless of presence — the dial then re-orders WITHIN a role
    /// by presence recency, but presence is ADVISORY (a device with no presence entry stays a
    /// candidate; absence never removes one). Net-free — the cli races these endpoints.
    pub fn devices_for_user(&self, user_id: &str) -> Vec<([u8; 32], String)> {
        let mut out: Vec<([u8; 32], String)> = self
            .devices
            .iter()
            .filter(|(_, d)| d.user_id == user_id)
            .map(|(eid, d)| (*eid, d.role.clone()))
            .collect();
        out.sort_by(|(a_eid, a_role), (b_eid, b_role)| {
            role_rank(a_role)
                .cmp(&role_rank(b_role))
                .then_with(|| a_eid.cmp(b_eid))
        });
        out
    }

    /// Every revoked endpoint (the sever rule's revoked set).
    pub fn revoked_endpoints(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.revoked.iter()
    }

    /// Every PERSON the roster carries, in document order — INCLUDING one whose devices are all
    /// revoked (#93).
    ///
    /// Use this for a member list; use [`devices`](Self::devices) for "which endpoint is whom".
    /// Building a member list from the device map omits anyone with no active device, and revoking
    /// someone's only device is an ordinary operation that leaves their user entry in the signed
    /// roster — so it makes "revoked their last device" indistinguishable from "removed".
    ///
    /// Display data. Authorization resolves through [`resolve`](Self::resolve), which is
    /// device-keyed on purpose.
    pub fn users(&self) -> &[RosterMemberEntry] {
        &self.users
    }

    /// The org's DECLARED group namespace, in document order (#93).
    ///
    /// This is the set an `allow` entry may name — rule 5b rejects any user group outside it — so
    /// it is what a UI offers when assigning membership. Display/authoring input, not an
    /// authorization decision: naming a group here grants nothing.
    pub fn groups(&self) -> &[String] {
        &self.groups
    }

    /// The EXPIRY-driven degraded-mode state machine. [`effective_state`](Self::effective_state)
    /// layers the `last_confirmed`/`max_staleness` staleness poll onto the SAME `RosterState`
    /// (a stale-but-unexpired roster degrades identically). Grace is a config window, NOT the
    /// install-time ±skew — freshness vs. liveness are separate concerns.
    pub fn state(&self, now_epoch: i64, grace_secs: i64) -> RosterState {
        if now_epoch <= self.expires_at_epoch {
            RosterState::Approved
        } else if now_epoch <= self.expires_at_epoch + grace_secs {
            RosterState::DegradedGrace
        } else {
            RosterState::DegradedStopped
        }
    }
}

impl RosterState {
    /// Degradation severity for the effective-state fold (Approved < DegradedGrace < DegradedStopped).
    fn severity(self) -> u8 {
        match self {
            RosterState::Approved => 0,
            RosterState::DegradedGrace => 1,
            RosterState::DegradedStopped => 2,
        }
    }
}

impl RosterView {
    /// The EFFECTIVE degraded state: the MORE-degraded of the expiry state
    /// ([`state`](Self::state)) and the freshness/staleness state. Freshness: `last_confirmed` is the
    /// last instant this node validated the roster as current via an authenticated channel (a TLS URL
    /// poll ≥ installed, a gossip-delivered roster passing validation, or manual install). If
    /// `now - last_confirmed > max_staleness` the node degrades exactly like expiry — warnings within
    /// `grace`, then serving stops — bounding adversarial staleness at `max_staleness + grace`
    /// independent of `expires_at`. `last_confirmed = None` imposes NO freshness constraint
    /// (a node with no freshness tracking configured is expiry-governed only — back-compat).
    pub fn effective_state(
        &self,
        now_epoch: i64,
        grace_secs: i64,
        last_confirmed: Option<i64>,
        max_staleness_secs: i64,
    ) -> RosterState {
        let expiry = self.state(now_epoch, grace_secs);
        let staleness = match last_confirmed {
            None => RosterState::Approved,
            Some(lc) => {
                let stale = now_epoch.saturating_sub(lc);
                if stale <= max_staleness_secs {
                    RosterState::Approved
                } else if stale <= max_staleness_secs + grace_secs {
                    RosterState::DegradedGrace
                } else {
                    RosterState::DegradedStopped
                }
            }
        };
        if staleness.severity() >= expiry.severity() {
            staleness
        } else {
            expiry
        }
    }
}

/// Parse an RFC3339 UTC timestamp to epoch seconds. A malformed timestamp is a typed
/// [`RosterError::BadTimestamp`] — NEVER a panic (`chrono::DateTime::parse_from_rfc3339` is in
/// chrono core and works under the crate's `default-features = false, features = ["alloc"]` set).
fn parse_rfc3339(s: &str) -> Result<i64, RosterError> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.timestamp())
        .map_err(|e| RosterError::BadTimestamp(format!("{s:?}: {e}")))
}

/// What a successful install did to this node's trust anchor (#93 ask c).
///
/// Returned alongside the view so the caller can PERSIST an adopted successor. A rotation that
/// lived only in memory would revert on restart and re-strand the node — the "reads as durable,
/// then silently reverts" failure #107's withdrawal tombstone was designed against.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnchorChange {
    /// The successor the caller must now pin, `b64u:`. `None` when the roster verified directly.
    pub adopted_root_pk: Option<String>,
}

/// Resolve which key verifies this roster's BODY, adopting a cross-signed successor if the pinned
/// anchor no longer signs directly (#93 ask c).
///
/// Order is the security of the whole feature:
///
/// 1. The pinned root is tried FIRST. A roster that verifies against it is never re-anchored, even
///    if it carries a successor pair — otherwise an ordinary roster could silently move a node's
///    anchor, which is the mechanism an attacker would want.
/// 2. Only on failure is the successor considered, and only when `successor_sig` verifies **with
///    the pinned root** over `domain ∥ org_id ∥ successor_pk`. An attacker who can serve a roster
///    still cannot introduce their own root: they would need the current root's signature over it.
/// 3. `org_id` is inside that signature, so a rotation statement for one org cannot be replayed
///    into another that happens to share an operator.
///
/// One rotation of slack, deliberately: a node two rotations behind cannot chain, because chaining
/// would mean carrying a history. An operator whose machine was off across two rotations hands it
/// an `org_join`.
fn resolve_verifier(
    roster: &Roster,
    pinned: &VerifyingKey,
) -> Result<(VerifyingKey, AnchorChange), RosterError> {
    if verify(roster, pinned).is_ok() {
        return Ok((*pinned, AnchorChange::default()));
    }
    let (Some(succ_pk_b64u), Some(succ_sig_b64u)) = (
        roster.successor_root_pk.as_deref(),
        roster.successor_sig.as_deref(),
    ) else {
        // No bridge on offer: the original failure stands, reported as the signature error it is.
        return Err(RosterError::BadSignature);
    };
    let succ_pk: [u8; 32] = crate::roster::decode_b64u(succ_pk_b64u)?
        .as_slice()
        .try_into()
        .map_err(|_| RosterError::BadSignature)?;
    let succ_sig = crate::roster::decode_b64u(succ_sig_b64u)?;
    let pinned_bytes = pinned.to_bytes();
    crate::roster::sign::verify_org_rotation(&pinned_bytes, &roster.org_id, &succ_pk, &succ_sig)?;
    let succ = VerifyingKey::from_bytes(&succ_pk).map_err(|_| RosterError::BadSignature)?;
    // The body must ALSO verify under the adopted key — the cross-signature says who may sign, not
    // that this document is signed.
    verify(roster, &succ)?;
    Ok((
        succ,
        AnchorChange {
            adopted_root_pk: Some(succ_pk_b64u.to_string()),
        },
    ))
}

/// [`validate_for_install`], additionally reporting an adopted successor (#93 ask c).
pub fn validate_for_install_with_anchor(
    roster: &Roster,
    root_pk: &VerifyingKey,
    installed_serial: u64,
    now_epoch: i64,
) -> Result<(RosterView, AnchorChange), RosterError> {
    // `/2` is the ROTATION format (#93 ask c). Accepted here and refused by every pre-0.47.0
    // binary with a legible "unexpected roster format" rather than "unknown field".
    if roster.format != ROSTER_FORMAT && roster.format != ROSTER_FORMAT_ROTATION {
        return Err(RosterError::BadFormat(roster.format.clone()));
    }
    // A `/2` document MUST carry the pair it exists to declare, and a `/1` document must NOT — the
    // format is a promise about the field set, so letting either drift would make the version
    // meaningless and reintroduce the ambiguity `deny_unknown_fields` exists to remove.
    let has_rotation = roster.successor_root_pk.is_some() || roster.successor_sig.is_some();
    if has_rotation != (roster.format == ROSTER_FORMAT_ROTATION) {
        return Err(RosterError::BadFormat(roster.format.clone()));
    }
    let (_verifier, anchor) = resolve_verifier(roster, root_pk)?;
    // Rule 2: strictly-increasing serial. Adopting a successor does NOT reset it — rollback
    // protection is orthogonal to which key signs, and resetting would make a rotation a way to
    // replay an old membership list.
    if roster.serial <= installed_serial {
        return Err(RosterError::StaleSerial {
            got: roster.serial,
            installed: installed_serial,
        });
    }
    let issued = parse_rfc3339(&roster.issued_at)?;
    let expires = parse_rfc3339(&roster.expires_at)?;
    if now_epoch < issued - SKEW_SECS || now_epoch > expires + SKEW_SECS {
        return Err(RosterError::OutOfValidity);
    }
    Ok((build_view(roster, expires)?, anchor))
}

/// Full validation for INSTALLING a new roster (rules 1–6, all MUST). On success
/// returns the resolvable [`RosterView`]. `installed_serial` is the current installed serial (0
/// if none); `now_epoch` is wall-clock seconds (a parameter — the caller supplies `epoch_now`).
///
/// [`validate_for_install_with_anchor`], discarding the anchor change.
///
/// **Kept as a thin delegate, not a second implementation.** It was left as its own copy in
/// 0.47.0's first cut and immediately became a trap: it had no production caller, understood no
/// rotation, and would refuse a rotated roster — so an embedder reaching for the obvious name got a
/// bridge-blind validator with nothing saying so. Delegating means it cannot drift.
///
/// Prefer [`validate_for_install_with_anchor`]: discarding the [`AnchorChange`] means a successor
/// is adopted for THIS document and then forgotten, so the next boot re-adopts it. Correct, but it
/// never re-pins.
pub fn validate_for_install(
    roster: &Roster,
    root_pk: &VerifyingKey,
    installed_serial: u64,
    now_epoch: i64,
) -> Result<RosterView, RosterError> {
    validate_for_install_with_anchor(roster, root_pk, installed_serial, now_epoch).map(|(v, _)| v)
}

/// Re-verify + rebuild the view for LOADING an already-installed roster at startup. Verifies the
/// signature (rule 1) and structural rules (4, 5), but NOT expiry/serial — a legitimately-expired
/// installed roster loads into degraded mode (the install-vs-load distinction).
pub fn load_installed(roster: &Roster, root_pk: &VerifyingKey) -> Result<RosterView, RosterError> {
    if roster.format != ROSTER_FORMAT && roster.format != ROSTER_FORMAT_ROTATION {
        return Err(RosterError::BadFormat(roster.format.clone()));
    }
    // #93 ask c: honour the rotation bridge here too. A node that adopted a successor writes the
    // new anchor to config, so on the next boot this normally verifies directly — but if that write
    // failed, or the roster on disk is newer than the pinned key, refusing here would strand a node
    // that was working seconds earlier.
    let _ = resolve_verifier(roster, root_pk)?;
    let expires = parse_rfc3339(&roster.expires_at)?;
    build_view(roster, expires)
}

/// Dial-candidate ordering rank for a device role: `"primary"` first, then `"mirror"`,
/// then any other role. Pure — an unrecognized role sorts last rather than erroring (the roster is
/// org-root-signed; an unknown role is a forward-compat value, not an attack).
fn role_rank(role: &str) -> u8 {
    match role {
        "primary" => 0,
        "mirror" => 1,
        _ => 2,
    }
}

/// Rules 4 (dup/conflicting endpoints) + 5 (flat-namespace disjointness + declared groups) +
/// assemble the view.
fn build_view(roster: &Roster, expires_at_epoch: i64) -> Result<RosterView, RosterError> {
    // Rule 5 (MUST, spec §4.3): user_ids ∪ groups is ONE flat, DECLARED namespace. Two checks:
    //   (5a) no user_id equals a top-level group name (disjointness);
    //   (5b) every group a user carries is DECLARED in the top-level `roster.groups` — so a signed
    //        roster cannot give user A an ad-hoc `groups:["X"]` while user B has `user_id:"X"`,
    //        which would make `allow=["X"]` ambiguous. The full namespace is exactly
    //        `roster.groups ∪ {user_id}` and every reference resolves into it.
    let group_set: HashSet<&str> = roster.groups.iter().map(String::as_str).collect();
    let mut seen_users: HashSet<&str> = HashSet::new();
    for u in &roster.users {
        // Defensive completeness (beyond §4.3 rules 1–6, parallel to rule 4's endpoint uniqueness):
        // a repeated `user_id` makes `allow = ["alice"]` ambiguous (which alice?). The roster is
        // org-root-signed so this is an integrity footgun, not an attack — reject it like a dup
        // endpoint for a single, unambiguous identity per name.
        if !seen_users.insert(u.user_id.as_str()) {
            return Err(RosterError::DuplicateUser(u.user_id.clone()));
        }
        // (5a) disjointness.
        if group_set.contains(u.user_id.as_str()) {
            return Err(RosterError::NamespaceCollision(u.user_id.clone()));
        }
        // (5b) every user group must be declared top-level.
        for g in &u.groups {
            if !group_set.contains(g.as_str()) {
                return Err(RosterError::UndeclaredGroup(g.clone()));
            }
        }
    }

    // Rule 4: revoked set first (revocation wins), then active devices EXCLUDING revoked ones.
    let mut revoked: HashSet<[u8; 32]> = HashSet::new();
    for e in &roster.revoked_endpoints {
        revoked.insert(decode_endpoint_id(e)?);
    }

    let mut devices: HashMap<[u8; 32], ResolvedDevice> = HashMap::new();
    for u in &roster.users {
        for d in &u.devices {
            let eid = decode_endpoint_id(&d.endpoint_id)?;
            if revoked.contains(&eid) {
                // Rule 4b: listed under a user AND revoked → revocation wins; warn, skip as active.
                tracing::warn!(user = %u.user_id, "roster: endpoint is both active and revoked — revocation wins");
                continue;
            }
            // Rule 4a: at most once across users.
            if devices.contains_key(&eid) {
                return Err(RosterError::DuplicateEndpoint);
            }
            devices.insert(
                eid,
                ResolvedDevice {
                    user_id: u.user_id.clone(),
                    display_name: u.display_name.clone(),
                    groups: u.groups.clone(),
                    role: d.role.clone(),
                    label: d.label.clone(),
                },
            );
        }
    }

    Ok(RosterView {
        org_id: roster.org_id.clone(),
        serial: roster.serial,
        expires_at_epoch,
        devices,
        revoked,
        groups: roster.groups.clone(),
        // Every user in the document, whether or not any of their devices survived the revoked
        // filter above — see `RosterView::users`.
        users: roster
            .users
            .iter()
            .map(|u| RosterMemberEntry {
                user_id: u.user_id.clone(),
                display_name: u.display_name.clone(),
                groups: u.groups.clone(),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roster::sign::mint_signed;
    use crate::roster::{Roster, RosterDevice, RosterUser, encode_b64u};
    use ed25519_dalek::SigningKey;

    fn root() -> SigningKey {
        SigningKey::from_bytes(&[9u8; 32])
    }

    // A valid body: serial 5, wide validity window, alice(team-eng,all)+laptop, nothing revoked.
    fn body(serial: u64) -> Roster {
        Roster {
            format: "mcpmesh-roster/1".into(),
            org_id: "acme".into(),
            serial,
            issued_at: "2000-01-01T00:00:00Z".into(),
            expires_at: "2999-01-01T00:00:00Z".into(),
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
            revoked_endpoints: vec![],
            successor_root_pk: None,
            successor_sig: None,
            sig: String::new(),
        }
    }
    const NOW: i64 = 1_760_000_000; // inside [2000, 2999]

    #[test] // Rule 1 (belt-and-suspenders over T2): a bad sig fails validate_for_install.
    fn rule1_bad_signature_rejected() {
        let mut r = mint_signed(&root(), body(5));
        r.serial = 6; // tamper AFTER signing → sig no longer matches
        assert!(matches!(
            validate_for_install(&r, &root().verifying_key(), 0, NOW),
            Err(RosterError::BadSignature)
        ));
    }

    #[test] // Rule 2: serial must be strictly greater than installed.
    fn rule2_serial_must_strictly_increase() {
        let r = mint_signed(&root(), body(5));
        // installed=5 → 5 is NOT > 5.
        assert!(matches!(
            validate_for_install(&r, &root().verifying_key(), 5, NOW),
            Err(RosterError::StaleSerial {
                got: 5,
                installed: 5
            })
        ));
        // installed=4 → 5 > 4 passes.
        assert!(validate_for_install(&r, &root().verifying_key(), 4, NOW).is_ok());
    }

    #[test] // Rule 3: now must be within [issued-skew, expires+skew].
    fn rule3_validity_window_with_skew() {
        let mut b = body(5);
        b.issued_at = "2026-07-03T12:00:00Z".into();
        b.expires_at = "2026-07-03T12:00:00Z".into(); // issued == expires
        let issued = 1_783_080_000; // 2026-07-03T12:00:00Z epoch (verified: 2026 is not a leap year)
        let r = mint_signed(&root(), b);
        let pk = root().verifying_key();
        // within skew of the instant → ok.
        assert!(validate_for_install(&r, &pk, 0, issued).is_ok());
        assert!(validate_for_install(&r, &pk, 0, issued + super::super::SKEW_SECS).is_ok());
        // beyond skew (expired) → OutOfValidity.
        assert!(matches!(
            validate_for_install(&r, &pk, 0, issued + super::super::SKEW_SECS + 1),
            Err(RosterError::OutOfValidity)
        ));
        // before issued beyond skew → OutOfValidity.
        assert!(matches!(
            validate_for_install(&r, &pk, 0, issued - super::super::SKEW_SECS - 1),
            Err(RosterError::OutOfValidity)
        ));
    }

    #[test] // Rule 4a: an endpoint_id appearing under two users is rejected.
    fn rule4_duplicate_endpoint_across_users_rejected() {
        let mut b = body(5);
        b.users.push(RosterUser {
            user_id: "bob".into(),
            display_name: "Bob".into(),
            user_pk: encode_b64u(&[3u8; 32]),
            groups: vec!["all".into()],
            devices: vec![RosterDevice {
                endpoint_id: encode_b64u(&[2u8; 32]), // SAME as alice's
                label: "dup".into(),
                role: "primary".into(),
            }],
        });
        let r = mint_signed(&root(), b);
        assert!(matches!(
            validate_for_install(&r, &root().verifying_key(), 0, NOW),
            Err(RosterError::DuplicateEndpoint)
        ));
    }

    #[test] // Rule 4b: an endpoint both under a user AND revoked → accepted (warn), revocation wins.
    fn rule4_revoked_overlap_is_accepted_and_revocation_wins() {
        let mut b = body(5);
        b.revoked_endpoints = vec![encode_b64u(&[2u8; 32])]; // alice's laptop, also revoked
        let r = mint_signed(&root(), b);
        let view =
            validate_for_install(&r, &root().verifying_key(), 0, NOW).expect("accepted with warn");
        // Revocation wins: the endpoint resolves to NOTHING (not an active device) but IS revoked.
        assert!(view.resolve(&[2u8; 32]).is_none());
        assert!(view.is_revoked(&[2u8; 32]));
    }

    #[test] // Rule 5a: user_ids and top-level groups must be disjoint (no name is both).
    fn rule5_user_id_and_group_names_must_be_disjoint() {
        let mut b = body(5);
        b.groups.push("alice".into()); // "alice" is already a user_id
        let r = mint_signed(&root(), b);
        assert!(matches!(
            validate_for_install(&r, &root().verifying_key(), 0, NOW),
            Err(RosterError::NamespaceCollision(n)) if n == "alice"
        ));
    }

    #[test] // Rule 5b: each user's `groups` MUST be a subset of the top-level `roster.groups`.
    fn rule5_user_groups_must_be_declared_in_top_level_groups() {
        // Ad-hoc group "X" on alice, NOT declared in roster.groups → the whole roster is rejected.
        // This closes the ambiguity where user A gets an ad-hoc `groups:["X"]` while some other user
        // has `user_id:"X"`, making `allow=["X"]` mean two things. One flat, DECLARED namespace.
        let mut b = body(5);
        b.users[0].groups.push("X".into()); // "X" ∉ roster.groups (["team-eng","all"])
        let r = mint_signed(&root(), b);
        assert!(matches!(
            validate_for_install(&r, &root().verifying_key(), 0, NOW),
            Err(RosterError::UndeclaredGroup(n)) if n == "X"
        ));
        // Control: a user group that IS declared passes (alice already has team-eng+all, both declared).
        assert!(
            validate_for_install(
                &mint_signed(&root(), body(5)),
                &root().verifying_key(),
                0,
                NOW
            )
            .is_ok()
        );
    }

    #[test] // The view resolves an active device to (user_id, groups); degraded state tracks expiry.
    fn view_resolves_and_computes_degraded_state() {
        let r = mint_signed(&root(), body(5));
        let view = validate_for_install(&r, &root().verifying_key(), 0, NOW).unwrap();
        let d = view.resolve(&[2u8; 32]).expect("alice's laptop resolves");
        assert_eq!(d.user_id, "alice");
        assert_eq!(d.groups, vec!["team-eng".to_string(), "all".to_string()]);
        assert!(view.resolve(&[42u8; 32]).is_none()); // unknown endpoint

        // Degraded state: expires_at is 2999; grace 72h. now inside → Approved.
        assert_eq!(view.state(NOW, 72 * 3600), RosterState::Approved);
        // Force expiry: now just past expires_at but within grace → DegradedGrace.
        let exp = view.expires_at_epoch();
        assert_eq!(view.state(exp + 1, 72 * 3600), RosterState::DegradedGrace);
        // Past expires_at + grace → DegradedStopped.
        assert_eq!(
            view.state(exp + 72 * 3600 + 1, 72 * 3600),
            RosterState::DegradedStopped
        );
    }

    #[test] // load_installed accepts an EXPIRED-but-valid roster into degraded mode; install rejects it.
    fn load_installed_accepts_expired_into_degraded_but_install_rejects() {
        // A roster whose validity window is entirely in the PAST (issued+expires both < NOW).
        let mut b = body(5);
        b.issued_at = "2000-01-01T00:00:00Z".into();
        b.expires_at = "2020-01-01T00:00:00Z".into(); // long expired relative to NOW (2025-ish)
        let r = mint_signed(&root(), b);
        let pk = root().verifying_key();

        // load_installed: sig + structure valid → SUCCEEDS even though expired (fail-closed load).
        let view =
            load_installed(&r, &pk).expect("expired-but-valid roster loads into degraded mode");
        let exp = view.expires_at_epoch();
        // Past expires_at but within grace → DegradedGrace (keeps serving, warns).
        assert_eq!(view.state(exp + 1, 72 * 3600), RosterState::DegradedGrace);
        // Past expires_at + grace → DegradedStopped (inbound serving stops).
        assert_eq!(
            view.state(exp + 72 * 3600 + 1, 72 * 3600),
            RosterState::DegradedStopped
        );
        // Sanity: it never reports Approved once past expiry.
        assert_ne!(view.state(NOW, 72 * 3600), RosterState::Approved);

        // Control — install-vs-load distinction: validate_for_install on the SAME roster REJECTS
        // (rule 3 validity window; install requires currently-valid, load tolerates expired).
        assert!(matches!(
            validate_for_install(&r, &pk, 0, NOW),
            Err(RosterError::OutOfValidity)
        ));
    }

    #[test] // Defensive completeness (beyond §4.3 1-6): a repeated user_id is rejected (ambiguous identity).
    fn duplicate_user_id_across_entries_rejected() {
        // Two DISTINCT user entries both `user_id="alice"` (distinct endpoints so it's the user_id,
        // not rule-4's endpoint check, that fires) → `allow=["alice"]` would be ambiguous → reject.
        let mut b = body(5);
        b.users.push(RosterUser {
            user_id: "alice".into(), // SAME user_id as the first entry
            display_name: "Alice Two".into(),
            user_pk: encode_b64u(&[3u8; 32]),
            groups: vec!["all".into()],
            devices: vec![RosterDevice {
                endpoint_id: encode_b64u(&[3u8; 32]), // DIFFERENT endpoint than the first alice
                label: "phone".into(),
                role: "primary".into(),
            }],
        });
        let r = mint_signed(&root(), b);
        assert!(matches!(
            validate_for_install(&r, &root().verifying_key(), 0, NOW),
            Err(RosterError::DuplicateUser(n)) if n == "alice"
        ));

        // Control: a second user with a DISTINCT user_id still passes.
        let mut b2 = body(5);
        b2.users.push(RosterUser {
            user_id: "bob".into(),
            display_name: "Bob".into(),
            user_pk: encode_b64u(&[3u8; 32]),
            groups: vec!["all".into()],
            devices: vec![RosterDevice {
                endpoint_id: encode_b64u(&[3u8; 32]),
                label: "phone".into(),
                role: "primary".into(),
            }],
        });
        assert!(
            validate_for_install(&mint_signed(&root(), b2), &root().verifying_key(), 0, NOW)
                .is_ok()
        );
    }

    #[test]
    fn devices_for_user_lists_active_devices_with_roles_primary_first() {
        // alice with a primary laptop [2;32] and a mirror desktop [3;32]; a revoked [4;32] excluded.
        let mut b = body(5);
        b.users[0].devices.push(RosterDevice {
            endpoint_id: encode_b64u(&[3u8; 32]),
            label: "desktop".into(),
            role: "mirror".into(),
        });
        b.users[0].devices.push(RosterDevice {
            endpoint_id: encode_b64u(&[4u8; 32]),
            label: "old".into(),
            role: "primary".into(),
        });
        b.revoked_endpoints = vec![encode_b64u(&[4u8; 32])]; // [4;32] revoked → excluded
        let view = validate_for_install(&mint_signed(&root(), b), &root().verifying_key(), 0, NOW)
            .unwrap();
        let devs = view.devices_for_user("alice");
        assert_eq!(
            devs,
            vec![
                ([2u8; 32], "primary".to_string()),
                ([3u8; 32], "mirror".to_string())
            ]
        );
        assert!(view.devices_for_user("nobody").is_empty());
    }

    #[test]
    fn effective_state_folds_expiry_and_staleness_taking_the_worse() {
        // A roster valid far into the future (never expiry-degraded).
        let r = mint_signed(&root(), body(5));
        let view = validate_for_install(&r, &root().verifying_key(), 0, NOW).unwrap();
        let grace = 72 * 3600;
        let max_staleness = 24 * 3600;

        // Freshly confirmed (last_confirmed == now) → Approved.
        assert_eq!(
            view.effective_state(NOW, grace, Some(NOW), max_staleness),
            RosterState::Approved
        );
        // Stale past max_staleness but within grace → DegradedGrace (warn, keep serving).
        let lc = NOW - max_staleness - 10;
        assert_eq!(
            view.effective_state(NOW, grace, Some(lc), max_staleness),
            RosterState::DegradedGrace
        );
        // Stale past max_staleness + grace → DegradedStopped (serving stops, spec §4.3 bound).
        let lc = NOW - max_staleness - grace - 10;
        assert_eq!(
            view.effective_state(NOW, grace, Some(lc), max_staleness),
            RosterState::DegradedStopped
        );
        // last_confirmed None → no freshness constraint (back-compat) → expiry-state only (Approved).
        assert_eq!(
            view.effective_state(NOW, grace, None, max_staleness),
            RosterState::Approved
        );

        // Worse-of: an EXPIRED roster that was freshly confirmed still degrades via EXPIRY.
        let mut b = body(6);
        b.issued_at = "2000-01-01T00:00:00Z".into();
        b.expires_at = "2020-01-01T00:00:00Z".into(); // long expired vs NOW
        let expired = load_installed(&mint_signed(&root(), b), &root().verifying_key()).unwrap();
        assert_eq!(
            expired.effective_state(NOW, grace, Some(NOW), max_staleness),
            RosterState::DegradedStopped
        );
    }

    /// #93 ask c: a node pinned to the PREDECESSOR installs a roster signed by the SUCCESSOR, on
    /// the strength of a cross-signature — and is told to re-anchor.
    ///
    /// This is the property that makes rotation survivable rather than merely possible: the bridge
    /// rides EVERY roster after a rotation, so a node that was offline for the announcing
    /// publication still catches up. If it appeared only on the announcing roster, a laptop closed
    /// for a week would be stranded exactly as it is today.
    #[test]
    fn a_cross_signed_successor_installs_against_the_old_anchor_and_reports_the_new_one() {
        let old = root();
        let new = SigningKey::from_bytes(&[11u8; 32]);
        let new_pk = new.verifying_key().to_bytes();
        let mut b = body(6);
        b.format = crate::roster::ROSTER_FORMAT_ROTATION.into();
        b.successor_root_pk = Some(encode_b64u(&new_pk));
        b.successor_sig = Some(encode_b64u(&crate::roster::sign::sign_org_rotation(
            &old, "acme", &new_pk,
        )));
        // Signed by the NEW root — the node has never seen this key.
        let r = mint_signed(&new, b);

        let (view, anchor) =
            validate_for_install_with_anchor(&r, &old.verifying_key(), 5, 1_000_000_000)
                .expect("a cross-signed successor must install against the old anchor");
        assert!(view.devices().count() > 0, "the view still builds");
        assert_eq!(
            anchor.adopted_root_pk.as_deref(),
            Some(encode_b64u(&new_pk).as_str()),
            "the caller must be TOLD to re-anchor — a rotation that lived only in memory would \
             revert on restart and re-strand the node"
        );
    }

    /// A successor is only ever adopted on a statement signed by the key it replaces.
    #[test]
    fn a_successor_not_cross_signed_by_the_pinned_root_is_refused() {
        let old = root();
        let attacker = SigningKey::from_bytes(&[13u8; 32]);
        let new = SigningKey::from_bytes(&[11u8; 32]);
        let new_pk = new.verifying_key().to_bytes();

        // (a) The ATTACKER cross-signs their own successor. Valid crypto, wrong signer.
        let mut b = body(6);
        b.format = crate::roster::ROSTER_FORMAT_ROTATION.into();
        b.successor_root_pk = Some(encode_b64u(&new_pk));
        b.successor_sig = Some(encode_b64u(&crate::roster::sign::sign_org_rotation(
            &attacker, "acme", &new_pk,
        )));
        let r = mint_signed(&new, b);
        assert!(
            validate_for_install_with_anchor(&r, &old.verifying_key(), 5, 1_000_000_000).is_err(),
            "an attacker who can serve a roster must not be able to introduce their own root"
        );

        // (b) The cross-signature is for a DIFFERENT org — the replay `org_id` is signed to stop.
        let mut b = body(6);
        b.format = crate::roster::ROSTER_FORMAT_ROTATION.into();
        b.successor_root_pk = Some(encode_b64u(&new_pk));
        b.successor_sig = Some(encode_b64u(&crate::roster::sign::sign_org_rotation(
            &old,
            "other-org",
            &new_pk,
        )));
        let r = mint_signed(&new, b);
        assert!(
            validate_for_install_with_anchor(&r, &old.verifying_key(), 5, 1_000_000_000).is_err(),
            "a rotation statement for one org must not be replayable into another that shares an \
             operator"
        );

        // (c) A valid cross-signature, but the BODY is signed by someone else. The statement says
        // WHO MAY sign, not that this document is signed.
        let mut b = body(6);
        b.format = crate::roster::ROSTER_FORMAT_ROTATION.into();
        b.successor_root_pk = Some(encode_b64u(&new_pk));
        b.successor_sig = Some(encode_b64u(&crate::roster::sign::sign_org_rotation(
            &old, "acme", &new_pk,
        )));
        let r = mint_signed(&attacker, b);
        assert!(
            validate_for_install_with_anchor(&r, &old.verifying_key(), 5, 1_000_000_000).is_err(),
            "adopting a successor must not admit a body signed by a third key"
        );
    }

    /// A roster that verifies DIRECTLY is never re-anchored, even carrying a successor pair.
    ///
    /// Otherwise an ordinary roster could silently move a node's trust anchor, which is precisely
    /// the mechanism an attacker would want out of this feature.
    #[test]
    fn a_directly_verifiable_roster_never_moves_the_anchor() {
        let old = root();
        let new = SigningKey::from_bytes(&[11u8; 32]);
        let new_pk = new.verifying_key().to_bytes();
        let mut b = body(6);
        b.format = crate::roster::ROSTER_FORMAT_ROTATION.into();
        b.successor_root_pk = Some(encode_b64u(&new_pk));
        b.successor_sig = Some(encode_b64u(&crate::roster::sign::sign_org_rotation(
            &old, "acme", &new_pk,
        )));
        // Signed by the CURRENT root: the announcing roster.
        let r = mint_signed(&old, b);
        let (_view, anchor) =
            validate_for_install_with_anchor(&r, &old.verifying_key(), 5, 1_000_000_000)
                .expect("the announcing roster installs");
        assert_eq!(
            anchor.adopted_root_pk, None,
            "a roster the pinned root signs must not re-anchor the node — the successor is adopted \
             only when the pinned key no longer signs directly"
        );
    }

    /// Rollback protection survives rotation: adopting a successor does not reset the serial.
    ///
    /// Resetting it would turn a rotation into a way to replay an old membership list — including
    /// one that predates a revocation.
    #[test]
    fn adopting_a_successor_does_not_reset_rollback_protection() {
        let old = root();
        let new = SigningKey::from_bytes(&[11u8; 32]);
        let new_pk = new.verifying_key().to_bytes();
        let cross = encode_b64u(&crate::roster::sign::sign_org_rotation(
            &old, "acme", &new_pk,
        ));
        let mut b = body(3);
        b.format = crate::roster::ROSTER_FORMAT_ROTATION.into(); // OLDER than the installed serial 5
        b.successor_root_pk = Some(encode_b64u(&new_pk));
        b.successor_sig = Some(cross);
        let r = mint_signed(&new, b);
        assert!(
            matches!(
                validate_for_install_with_anchor(&r, &old.verifying_key(), 5, 1_000_000_000),
                Err(RosterError::StaleSerial { .. })
            ),
            "a cross-signed roster with a stale serial must still be refused"
        );
    }

    /// #93 ask c: `load_installed` honours the bridge too.
    ///
    /// A node adopts a successor and writes the new pin — but if that write failed, or the process
    /// died between them, the next BOOT re-reads a roster its pinned key no longer signs. Refusing
    /// there would strand a node that was working seconds earlier, on a machine whose only symptom
    /// is that roster mode stopped.
    ///
    /// Untested until the gate deleted the line and the whole workspace stayed green.
    #[test]
    fn load_installed_accepts_a_cross_signed_successor_after_a_failed_re_pin() {
        let old = root();
        let new = SigningKey::from_bytes(&[11u8; 32]);
        let new_pk = new.verifying_key().to_bytes();
        let mut b = body(6);
        b.format = crate::roster::ROSTER_FORMAT_ROTATION.into();
        b.successor_root_pk = Some(encode_b64u(&new_pk));
        b.successor_sig = Some(encode_b64u(&crate::roster::sign::sign_org_rotation(
            &old, "acme", &new_pk,
        )));
        let r = mint_signed(&new, b);
        load_installed(&r, &old.verifying_key())
            .expect("a boot on the OLD pin must still load a roster the bridge covers");

        // …and a roster with no bridge still fails against the wrong key, so this is not a blanket
        // relaxation of the signature check.
        let plain = mint_signed(&new, body(6));
        assert!(load_installed(&plain, &old.verifying_key()).is_err());
    }

    /// The rotation FORMAT and the successor fields must agree, in both directions.
    ///
    /// The format is a promise about the field set. A `/2` with no successor, or a `/1` carrying
    /// one, would make the version meaningless — and the version is the only thing that turns a
    /// pre-0.47.0 member's failure from "unknown field" into "upgrade me".
    #[test]
    fn the_rotation_format_and_the_successor_fields_must_agree() {
        let old = root();
        let new = SigningKey::from_bytes(&[11u8; 32]);
        let new_pk = new.verifying_key().to_bytes();
        let cross = encode_b64u(&crate::roster::sign::sign_org_rotation(
            &old, "acme", &new_pk,
        ));

        // /2 with NO successor pair.
        let mut b = body(6);
        b.format = crate::roster::ROSTER_FORMAT_ROTATION.into();
        let r = mint_signed(&old, b);
        assert!(matches!(
            validate_for_install_with_anchor(&r, &old.verifying_key(), 5, 1_000_000_000),
            Err(RosterError::BadFormat(_))
        ));

        // /1 CARRYING a successor pair.
        let mut b = body(6);
        b.successor_root_pk = Some(encode_b64u(&new_pk));
        b.successor_sig = Some(cross.clone());
        let r = mint_signed(&old, b);
        assert!(matches!(
            validate_for_install_with_anchor(&r, &old.verifying_key(), 5, 1_000_000_000),
            Err(RosterError::BadFormat(_))
        ));

        // An unknown format is still refused.
        let mut b = body(6);
        b.format = "mcpmesh-roster/99".into();
        let r = mint_signed(&old, b);
        assert!(matches!(
            validate_for_install_with_anchor(&r, &old.verifying_key(), 5, 1_000_000_000),
            Err(RosterError::BadFormat(_))
        ));
    }
}
