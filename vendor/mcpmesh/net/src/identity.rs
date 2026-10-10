//! Identity resolution at accept time: [`EndpointId`], [`PeerIdentity`], and the
//! [`TrustGate`] trait every trust policy implements.
use std::collections::HashMap;

/// The public identity of a device on the mesh: its verified ed25519 public key,
/// as 32 raw bytes.
///
/// A dedicated newtype — not a bare `[u8; 32]` — so a public endpoint id can never
/// be cross-wired with the other 32-byte arrays in the family (secret key bytes,
/// hashes). Construct one from raw bytes with [`EndpointId::from_bytes`] (or
/// `From<[u8; 32]>`), or directly from an authenticated `iroh::EndpointId`
/// (`From<iroh::EndpointId>`); read the bytes back with [`EndpointId::as_bytes`].
///
/// There is deliberately no `Display` impl: endpoint ids are never rendered in
/// user-facing output (see SECURITY.md's surface discipline). Where a raw id must
/// be shown (diagnostics), render via `iroh::EndpointId`'s base32 form explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EndpointId([u8; 32]);

impl EndpointId {
    /// Wrap raw public-key bytes as an endpoint id.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw public-key bytes.
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The raw public-key bytes, by value.
    pub const fn to_bytes(self) -> [u8; 32] {
        self.0
    }

    /// THE sanctioned authz rendering of a device identity: `eid:<hex>`, byte-identical to
    /// `iroh::EndpointId`'s `Display` (HEXLOWER of the raw key bytes — what `peer_add` parses
    /// and `mcpmesh internal id` prints).
    /// This is deliberately a NAMED method and not a `Display` impl — the surface-leak
    /// discipline stands: HUMAN porcelain never prints raw endpoint ids (status render
    /// maps principals back to store-resolved display names); this rendering exists for
    /// the MACHINE authz namespace (`allow` entries, blob grants, `principal_set`).
    /// The encoding is byte-identical to `iroh::EndpointId`'s `Display` (HEXLOWER of the
    /// raw key bytes) but total over ANY bytes — a zeroed test identity renders a
    /// matchable `eid:` of zeros rather than failing point validation; production
    /// identities are always gate-stamped from the authenticated TLS key.
    pub fn principal(&self) -> String {
        format!("eid:{}", data_encoding::HEXLOWER.encode(&self.0))
    }
}

impl From<[u8; 32]> for EndpointId {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl From<EndpointId> for [u8; 32] {
    fn from(id: EndpointId) -> Self {
        id.0
    }
}

/// The authenticated id of an iroh connection peer (`Connection::remote_id`)
/// converts directly — the one production source of an `EndpointId`.
impl From<iroh::EndpointId> for EndpointId {
    fn from(id: iroh::EndpointId) -> Self {
        Self(*id.as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    /// The AUTHENTICATED endpoint id (the peer's verified ed25519 pubkey). Stamped by every
    /// `TrustGate::resolve` from the queried key — the rate-limit key (SECURITY invariant 1);
    /// never a self-asserted value.
    pub endpoint: EndpointId,
    pub name: String,
    // The self-sovereign user_id: the org roster value in roster mode, else the one proven by a
    // verified device->user binding at pairing (`None` only for a pairing peer that presented no
    // binding). Accepter-derived from the trust gate — never self-asserted.
    pub user_id: Option<String>,
    pub groups: Vec<String>,
}

impl PeerIdentity {
    /// A pairing-mode identity by nickname. `endpoint` is zeroed here and STAMPED by the gate's
    /// `resolve` from the queried key (test/bootstrap constructor — production always resolves).
    pub fn nickname(name: &str) -> Self {
        Self {
            endpoint: EndpointId::from_bytes([0u8; 32]),
            name: name.into(),
            user_id: None,
            groups: vec![],
        }
    }
}

/// A trust policy consulted at accept time: resolve an authenticated endpoint to
/// an identity (or refuse it), and answer the revocation/severing questions the
/// connection registry asks.
pub trait TrustGate: Send + Sync + 'static {
    fn resolve(&self, endpoint: &EndpointId) -> Option<PeerIdentity>;

    /// Is this endpoint revoked by a roster this gate holds? Consulted FIRST by the composed
    /// gate (revocation wins over pairing, across all ALPNs) and by the check-register recheck.
    /// Default `false`: pairing/static gates have no revocation concept. A roster-backed gate
    /// overrides it, honoring its installed roster's revoked set even when degraded (fail-closed).
    fn is_revoked(&self, _endpoint: &EndpointId) -> bool {
        false
    }

    /// The register-time sever gate: should a connection from `endpoint` (resolved with
    /// `roster_user`) be severed AS OF the live roster? The generalization of [`Self::is_revoked`] —
    /// it also catches a previously-roster-resolved endpoint now ABSENT from the roster (a benign
    /// departure), closing the dropped-from-roster register-after-sever race symmetrically with the
    /// revoked half. Default `false` (pairing-only gates never sever on a roster). Consulted by
    /// `ConnRegistry::register_checked`'s caller-supplied closure UNDER the registry lock (the
    /// TOCTOU close).
    fn should_sever_now(&self, _endpoint: &EndpointId, _roster_user: Option<&str>) -> bool {
        false
    }

    /// The "roster-resolved" discriminator for a connection from `endpoint`: `Some(user_id)` IFF
    /// this endpoint is a member of the gate's CURRENT roster view (else `None`). This — NOT the
    /// resolved `PeerIdentity::user_id` — is what `register_checked` stores and `should_sever`/
    /// `should_sever_now` key on, because a PAIRING peer can also carry a `user_id` (the verified
    /// self-sovereign device→user binding), so `identity.user_id.is_some()` does not mean
    /// "roster-resolved". Default `None`: pairing/static gates have no roster, so their peers are
    /// never severed by a roster install. A composed gate overrides it to consult the roster view.
    fn roster_user(&self, _endpoint: &EndpointId) -> Option<String> {
        None
    }
}

/// Test/bootstrap gate: a fixed allowlist.
pub struct StaticGate(HashMap<EndpointId, PeerIdentity>);

impl StaticGate {
    pub fn new(entries: impl IntoIterator<Item = (EndpointId, PeerIdentity)>) -> Self {
        Self(entries.into_iter().collect())
    }
}

impl TrustGate for StaticGate {
    fn resolve(&self, endpoint: &EndpointId) -> Option<PeerIdentity> {
        self.0.get(endpoint).cloned().map(|mut id| {
            id.endpoint = *endpoint; // stamp the authenticated key (SECURITY invariant 1)
            id
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(b: u8) -> EndpointId {
        EndpointId::from_bytes([b; 32])
    }

    #[test]
    fn static_gate_resolves_known_and_refuses_unknown() {
        let gate = StaticGate::new([(id(1), PeerIdentity::nickname("bob"))]);
        let got = gate.resolve(&id(1)).expect("known endpoint resolves");
        assert_eq!(got.name, "bob");
        assert_eq!(
            got.endpoint,
            id(1),
            "resolve stamps the AUTHENTICATED endpoint"
        );
        assert!(gate.resolve(&id(2)).is_none());
    }

    #[test]
    fn endpoint_id_round_trips_its_bytes() {
        let raw = [7u8; 32];
        let eid = EndpointId::from_bytes(raw);
        assert_eq!(*eid.as_bytes(), raw);
        assert_eq!(eid.to_bytes(), raw);
        assert_eq!(EndpointId::from(raw), eid);
        assert_eq!(<[u8; 32]>::from(eid), raw);
    }
}

#[cfg(test)]
mod principal_tests {
    use super::*;
    /// `principal()` must stay byte-identical to iroh's own EndpointId rendering — the
    /// encoding `peer_add` parses and diagnostics print. Guards an iroh re-pin changing
    /// the wire format out from under persisted `eid:` grants.
    #[test]
    fn eid_principal_tracks_irohs_endpoint_rendering() {
        let key = iroh::SecretKey::from_bytes(&[7u8; 32]).public();
        let ours = EndpointId::from(key).principal();
        assert_eq!(ours, format!("eid:{key}"));
        assert!(ours.starts_with("eid:"));
    }
}
