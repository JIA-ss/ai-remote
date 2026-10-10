//! mcpmesh-local/1 protocol types. Shared vocabulary between the daemon
//! and its clients (porcelain, connect proxy, later the host shell). Wire framing
//! is the family NDJSON codec — carried by the caller, not defined here.
//!
//! Request/response asymmetry: requests are one typed, closed enum (`Request`);
//! responses are per-method typed structs deserialized from the JSON-RPC `result`
//! Value — `Status` → [`StatusResult`], `RegisterService` → an ack, `OpenSession` →
//! no JSON-RPC result at all: the socket STOPS being JSON-RPC and becomes a raw
//! byte pipe.
//!
//! Additive-only: new fields (capabilities on `Hello`, groups/user_id on
//! `PeerInfo`, device on `OpenSession`) MUST land as
//! `#[serde(default, skip_serializing_if = ...)]` so older payloads still deserialize.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The first exchange on any `*-local/N` socket (the family's hello convention).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub api: String,         // "mcpmesh-local/1"
    pub api_version: String, // "MAJOR.MINOR" of the protocol surface (see API_MINOR)
    /// The protocol-compatibility MINOR as an integer, for a trivial machine comparison
    /// (`api_minor >= N`) without string parsing. Distinct from `stack_version` (the crate
    /// release train). Additive: an older daemon omits it and it defaults to 0.
    #[serde(default)]
    pub api_minor: u32,
    pub stack_version: String,
}

/// The kind of backend answering a service — the two valid values, enforced at the
/// type level and kept in lockstep with `BackendSpec`'s variants. Status reports the
/// kind only, never the command/path (no transport vocabulary).
///
/// **`Default` is a construction convenience, not a claim (#148).** Neither value means "no
/// backend" — a service has one or the other — so `Run` is chosen because it is the common config
/// shape, and for no deeper reason. It exists so [`ServiceInfo`] can derive `Default` and a
/// downstream test fixture stops breaking on every additive field we add.
///
/// It cannot mislead a reader of live data: the daemon sets `backend` explicitly on every
/// `ServiceInfo` it builds, so a defaulted value only ever exists in a fixture whose author
/// wrote it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    #[default]
    Run,
    Socket,
}

/// A registered service as reported by `status` (no transport vocabulary).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub allow: Vec<String>, // STABLE principals (b64u:/eid:) or roster names (#38) — never nicknames
    /// The HUMAN rendering of `allow`, index-aligned: each principal resolved to its peer's
    /// display nickname by the daemon (which owns the store); an unresolvable stable
    /// principal renders as a neutral placeholder — porcelain must show THESE, never raw
    /// ids (surface discipline). Additive: default + skip-if-empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_display: Vec<String>,
    pub backend: BackendKind, // "run" | "socket" (kind only, never the command/path)
    /// True if this registration is ephemeral (#36): in-memory only, tied to the registering
    /// control connection's lifetime, absent from config, gone on restart. Additive — an older
    /// daemon omits it and it reads as `false` (the persistent default).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ephemeral: bool,
}

/// A known peer as reported by `status` (nickname only — never the EndpointId).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerInfo {
    pub name: String,
    pub services: Vec<String>,
    /// The peer's PROVEN self-sovereign `user_id` (`b64u:<user_pk>`) if it presented a verified
    /// device->user binding at pairing (roster peers carry it too), else `None` (nickname-only). This
    /// is a surface-clean identity (an opaque user id, NOT an EndpointId). Additive:
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]` so older payloads round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// The peer's stable DEVICE principal `eid:<hex>` (#41) — the SAME rendering the socket
    /// backend injects into `_meta["mcpmesh/peer"]` and that appears in `[services.*].allow`.
    /// Always present for a real peer (`Option` only for additive round-trip). Distinct from
    /// `user_id` (the person-level `b64u:`, present only when the peer proved a binding): a
    /// nickname is not unique, so an embedder keys caller-scoped decisions (dial the caller
    /// back, "the requester's own data") on THIS, the authenticated endpoint. Machine-surface
    /// authz vocabulary (like the allow lists) — human porcelain still shows the nickname.
    /// Additive: `#[serde(default, skip_serializing_if = "Option::is_none")]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

/// HOW a peer is reached (#64): a direct/hole-punched QUIC path, or through a relay.
///
/// `rtt_ms` is NOT a proxy for this — a fast relay beats a slow direct path — and iroh's own
/// distinction was being dropped at the mcpmesh boundary. Three things depend on it: a truthful
/// locality claim ("this traffic never left the building"), honest disclosure that a relayed path
/// depends on third-party infrastructure, and diagnostics, since "slow" has a different cause and
/// fix in each case.
///
/// **Only `Direct` supports a locality claim.** `Unknown` means "we do not know", NOT "private" —
/// rendering it as private is the one misuse that turns this field into a false privacy statement.
/// The daemon errs the same way: when a relay path is active it reports [`Relay`](Self::Relay) even
/// if a direct path is live too, because overstating privacy is worse than understating it.
///
/// `#[non_exhaustive]`: iroh already has a third address kind (a custom transport) that could
/// warrant a variant, and adding one to a public enum later breaks every downstream exhaustive
/// `match` — the lesson #58 paid for.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PeerPath {
    /// A direct or hole-punched QUIC path: the bytes did not transit a relay.
    Direct,
    /// Through a relay server. `url` is the relay in use when known.
    Relay {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    /// Not known: never probed, several open paths with none selected, a closed connection, or a
    /// transport mcpmesh does not model. (A single open path is reported by its kind whether or
    /// not iroh calls it selected — #213.)
    ///
    /// `#[serde(other)]` makes this the landing spot for a `kind` a client has never heard of. That
    /// is what actually buys wire-additivity: `#[non_exhaustive]` only protects the Rust `match`,
    /// and without this an older client hits `unknown variant` and fails to deserialize the WHOLE
    /// `PeerReachability` — one new path kind would break every `status` response it reads.
    #[default]
    #[serde(other)]
    Unknown,
}

/// Advisory reachability of a paired peer (pairing-mode liveness). Surface-clean: a nickname, a
/// bool, latency/age NUMBERS, the stable `eid:` principal (#42), and since #64 the PATH KIND —
/// direct vs relay, plus the relay URL when relayed. Never a socket address, an IP, or a key: the
/// path field says WHICH KIND of route is in use, never where the peer is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerReachability {
    pub name: String,    // the peer's nickname
    pub reachable: bool, // result of the last probe (false if never probed)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Last measured round-trip, if reachable: dial + ping/pong, stamped AT THE PONG.
    ///
    /// It EXCLUDES the window the daemon spends afterwards determining which path the connection
    /// settled on. Before 0.20.1 it included that window, so a relayed peer could never report
    /// under 600ms and most of the figure was a deliberate wait rather than time on the wire —
    /// an embedder read ~820ms across one LAN hop and reported it as a 66x latency regression
    /// (#123). It is a wire-latency measurement now, so "relayed AND low rtt_ms" is a reachable
    /// state and a usable diagnostic.
    pub rtt_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_secs: Option<u64>, // None = never probed (consumer shows "checking…")
    /// The peer's OPTIONAL app metadata (#40) — the same opaque ≤256B blob #39 exposes via
    /// presence, here carried on the pairing-mode `mcpmesh/ping/1` probe pong so PAIRED peers
    /// (which have no presence gossip) see it too. Empty when the peer set none. Advisory
    /// display data; never an authz input. Near-real-time when `status` is read (the probe
    /// cache has a ~20s TTL), not a steady push. Additive: default + skip-if-empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub meta: String,
    /// The peer's stable DEVICE principal `eid:<hex>` (#42) — the SAME rendering as
    /// [`PeerInfo::principal`], so an embedder joins probe result + `meta` (app version) to a
    /// peer by the AUTHENTICATED endpoint rather than the non-unique nickname. Always present
    /// for a real row (`Option` only for additive round-trip). Machine-surface authz
    /// vocabulary — the human `status` reachability line is unchanged. Additive:
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
    /// HOW this peer is reached (#64) — see [`PeerPath`]. Captured by the same probe that sets
    /// `reachable`/`rtt_ms`, so it shares their freshness: one TTL, one `age_secs`. `Unknown` for a
    /// peer never probed. Additive (`#[serde(default)]`), so older rows and clients are unaffected.
    #[serde(default)]
    pub path: PeerPath,
}

/// WHICH producer emitted a [`StreamFrame::Reachability`] (#150). The two say different things
/// about the world and license different user-facing statements, and until API 1.30 the frame
/// carried no way to tell them apart.
///
/// Advisory attribution, never an authz input: it says where an observation CAME FROM, never who a
/// peer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReachabilitySource {
    /// A **probe** completed — a fresh throwaway dial (`status`/`subscribe` refreshing a stale
    /// entry). It describes that dial and nothing else: a `Probe` frame saying `Relay` does NOT
    /// mean any live connection is relayed.
    Probe,
    /// A **live session**'s selected path changed under it (#92 item 2). This is a claim about the
    /// link a peer's traffic is actually on — the frame an embedder wants when warning that a call
    /// which WAS direct silently is not any more.
    Session,
    /// The daemon did not say (`api_minor < 30`), or it named a producer this client predates.
    ///
    /// The DEFAULT, deliberately — see [`StreamFrame::Reachability`]. Like [`PeerPath::Unknown`] it
    /// means "we do not know" and must never be collapsed into either confident case.
    #[default]
    Unknown,
}

/// Hand-written so an unrecognized producer lands on [`ReachabilitySource::Unknown`] instead of
/// failing the whole frame. [`PeerPath`] gets this from `#[serde(other)]`, which serde allows only
/// on an internally/adjacently tagged enum; this one is a plain string, so it is spelled out. The
/// stakes are the same as there: without it, adding a third producer later would break every
/// `Reachability` frame an older pinned client reads, not just the new field.
///
/// It accepts ANY input, not just an unrecognized string — `null`, a number, an object all read as
/// `Unknown`. `#[serde(default)]` covers an ABSENT key and nothing else, so without this a proxy or
/// non-Rust daemon that normalizes optional fields to `null` would fail every reachability frame
/// while this module's doc promised the field could not break a parse. A degraded attribution is
/// the fail-safe: `Unknown` already means "we do not know", which is exactly true of a value we
/// could not read.
impl<'de> Deserialize<'de> for ReachabilitySource {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct AnySource;

        /// Every hook answers `Unknown` except `visit_str`, so a shape we do not model degrades
        /// instead of erroring. `visit_map`/`visit_seq` must DRAIN their input — leaving it
        /// unconsumed desynchronizes the parser and fails the enclosing frame, which is the
        /// failure this impl exists to avoid.
        impl<'de> serde::de::Visitor<'de> for AnySource {
            type Value = ReachabilitySource;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a reachability producer name")
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Self::Value, E> {
                Ok(match s {
                    "probe" => ReachabilitySource::Probe,
                    "session" => ReachabilitySource::Session,
                    _ => ReachabilitySource::Unknown,
                })
            }

            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(ReachabilitySource::Unknown)
            }

            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(ReachabilitySource::Unknown)
            }

            fn visit_some<D: serde::Deserializer<'de>>(
                self,
                d: D,
            ) -> Result<Self::Value, D::Error> {
                d.deserialize_any(AnySource)
            }

            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(ReachabilitySource::Unknown)
            }

            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(ReachabilitySource::Unknown)
            }

            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(ReachabilitySource::Unknown)
            }

            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(ReachabilitySource::Unknown)
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut m: A,
            ) -> Result<Self::Value, A::Error> {
                while m
                    .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
                    .is_some()
                {}
                Ok(ReachabilitySource::Unknown)
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut s: A,
            ) -> Result<Self::Value, A::Error> {
                while s.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(ReachabilitySource::Unknown)
            }
        }

        d.deserialize_any(AnySource)
    }
}

/// Roster-mode status. Surface-clean roster VOCABULARY only: org_id, serial, a plain
/// state word, and the pinned org-root FINGERPRINT in short words — never raw keys/EndpointIds/serials-
/// as-transport-vocab. Absent in a pure-pairing daemon.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterStatus {
    pub org_id: String,
    pub serial: u64,
    pub state: String, // "pending" | "approved" | "degraded" | "stopped"
    pub org_root_fingerprint: String, // short-word form
    /// The org's DECLARED group namespace, in roster document order (#93). `api_minor >= 46`.
    ///
    /// The set an `allow` entry may name — a roster is refused if any user carries a group outside
    /// it — so this is what a UI offers when assigning membership. Without it an embedder in roster
    /// mode had managed group membership it could not enumerate, and the only way to learn the
    /// groups was to hand-parse the daemon-owned `roster.json`.
    ///
    /// Display/authoring input, never an authorization answer: naming a group grants nothing.
    /// Additive — a payload from an older daemon reads as an empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
}

/// One reachable roster peer device as reported by `status` (the advisory presence read).
/// ADVISORY — this is a display convenience, never an authorization surface. Surface-clean:
/// FLAT vocabulary ONLY — a `user_id`, a human `device_label`, its `role` word, and an `online`
/// boolean. It carries NO EndpointId / pubkey / hash / ALPN or any transport vocabulary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresencePeer {
    pub user_id: String,
    /// The person's human display name from the roster (#93). `api_minor >= 46`.
    ///
    /// `user_id` is an authorization handle; this is what a UI puts next to a face. The roster
    /// carried it all along and the control seam dropped it, so an embedder had a presence list it
    /// could only label with an opaque id. Display-only — never an authz input.
    ///
    /// Additive: absent from an older daemon's payload, and empty when the roster's own field is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub display_name: String,
    /// The groups this person belongs to (#93). `api_minor >= 46`.
    ///
    /// The same strings an `allow` entry names, so a UI can show why someone is admitted without
    /// re-deriving it. Advisory display data: the gate reads the roster, never this. Additive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    pub device_label: String,
    pub role: String, // "primary" | "mirror" (roster vocabulary)
    /// Whether the device has a live presence heartbeat (advisory — absence never blocks a dial).
    pub online: bool,
    /// The device's OPTIONAL embedder-set app metadata (#39) — an opaque ≤256B blob carried
    /// (signed) on its presence heartbeat, empty when the device set none. Advisory display
    /// data; never an authz input. Additive: default + skip-if-empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub meta: String,
}

/// One recently completed INVITER-side pairing, surfaced by `status` so the inviter's human can
/// read the short authentication code (SAS) and compare it with the redeemer's out-of-band —
/// the pairing ceremony is "both humans compare the code": the redeemer sees it in its
/// [`PairResult`]; this is the inviter's porcelain surface for the same words. DISPLAY-ONLY
/// ceremony state: held in-memory by the daemon (a small ring), lost on restart, NEVER an
/// authorization input or trust data. Surface-clean: a nickname + the SAS wordlist words +
/// an epoch — never an EndpointId.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentPairing {
    /// The peer's nickname as stored by the inviter (its local name for the redeemer).
    pub peer_nickname: String,
    /// The display-only SAS words (e.g. `"tango-fig-cabbage"`) — the same code the redeemer's
    /// `PairResult.sas_code` carried. Never checked programmatically.
    pub sas_code: String,
    /// When the pairing completed (epoch seconds) — the porcelain renders a friendly age.
    pub paired_at_epoch: u64,
    /// `true` when the ceremony was a SELF-ENROLLMENT (`invite { as_self: true }`, #86): the
    /// redeemer became another device of THIS person rather than a peer, so there is no peer row
    /// behind `peer_nickname` and a SAS mismatch means an impostor now presents your identity —
    /// the remedy is `device_revoke` of that endpoint, not `peer_remove` (#214). The endpoint is
    /// on the `self_enroll` audit event, not here — this row stays surface-clean. Set structurally
    /// by the ceremony that ran, never inferred from the nickname text. Additive
    /// (`api_minor >= 62`): `#[serde(default, skip_serializing_if = ...)]`, absent = an ordinary
    /// pairing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub self_enroll: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusResult {
    pub stack_version: String,
    pub services: Vec<ServiceInfo>,
    pub peers: Vec<PeerInfo>,
    /// Roster-mode status, absent in a pure-pairing daemon. Additive:
    /// `#[serde(default, skip_serializing_if = ...)]` so a daemon/client without it round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster: Option<RosterStatus>,
    /// The reachable roster peer devices (the advisory presence read), each with an `online`
    /// flag. Empty in a pure-pairing daemon / when no roster is installed. Additive:
    /// `#[serde(default, skip_serializing_if = "Vec::is_empty")]` so an older payload round-trips.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presence: Vec<PresencePeer>,
    /// THIS daemon's own self-sovereign `user_id` (`b64u:<user_pk>`), if it has a user key (auto-
    /// minted at boot; shared by pairing AND roster mode). Lets the operator see + share their stable
    /// identity that multiple devices resolve to. `None` only when no user key exists. Additive:
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]` so an older payload round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_user_id: Option<String>,
    /// `true` when THIS device holds the private user key behind `self_user_id` (#214,
    /// `api_minor >= 62`). `false` when it presents that identity on the strength of an ADOPTED
    /// enrollment binding (#86) — the key lives on the device that enrolled it — and when it has
    /// no user key at all (`self_user_id` absent). An enrolled device and a key-holding one
    /// otherwise report the same `self_user_id`, and three refusals hinge on the difference:
    /// `peer_endorse`, `device_revoke` and `invite { as_self }` all refuse on an enrolled device,
    /// and [`Request::SelfEnrollDetach`] applies only there. ADVISORY display data for placing
    /// those affordances — never an authorization input; the daemon re-checks on every call.
    /// Additive: `#[serde(default, skip_serializing_if = ...)]`, so it reads `false` from a daemon
    /// below 62 — guard on `api_minor` before treating that as "enrolled".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub self_user_key_held: bool,
    /// Recent INVITER-side pairing completions, newest first (display-only pairing-ceremony aids —
    /// see [`RecentPairing`]; in-memory on the daemon, cleared by a restart). Empty on a daemon
    /// that has accepted no pairing since it started. Additive:
    /// `#[serde(default, skip_serializing_if = "Vec::is_empty")]` so an older payload round-trips.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_pairings: Vec<RecentPairing>,
    /// Advisory reachability of paired peers, from the on-demand probe cache. Empty until the
    /// first probe completes. Additive: default + skip-if-empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reachability: Vec<PeerReachability>,
    /// This node's EFFECTIVE self-nickname — what a freshly minted invite would present
    /// (config `[identity].nickname`, else the hostname, else a fingerprint; live-updated by
    /// `set_nickname`, #37). Empty only in mesh-less control-only mode. Additive: default +
    /// skip-if-empty so an older payload round-trips.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub self_nickname: String,
    /// On-disk footprint of this node's own state (#88), so an embedder can warn a user before
    /// ENOSPC rather than after — the audit log's write rate is driven by inbound peer traffic,
    /// and it shares a filesystem with `state.redb` and the device key. A LIVE read (computed
    /// per `status` call), not a boot-time snapshot. `None` only in mesh-less control-only mode.
    /// Additive: default + skip-if-none so an older payload round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageInfo>,
    /// Endpoints this node currently REFUSES (#85 ask 4, `api_minor >= 51`), local or signed.
    ///
    /// Present because a revocation is otherwise invisible: a peer that has been cut off simply
    /// stops working, and neither side can tell that from a network fault. Empty on the
    /// overwhelming majority of nodes, so it is elided rather than serialized as `[]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revoked: Vec<RevokedEndpoint>,
    /// THIS node's own reachability posture (#90) — see [`SelfNetwork`]. Computed live per
    /// call; `None` in mesh-less control-only mode. Additive: default + skip-if-none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_network: Option<SelfNetwork>,
}

/// The `status.self_network` block (#90): THIS node's own reachability posture — the first
/// question in every "my message never arrived" investigation, previously unanswerable from
/// either side of the API. Self-facing only: everything here is the node's own information
/// (relay URLs come from its own config, sanitized; direct addresses already ride its invites).
///
/// `online` is iroh's own semantics — a home-relay connection is established. In
/// `relay_mode = "disabled"` it is ALWAYS `false` with an empty `relays` list: that is a
/// configuration, not an outage — render it as "LAN-only", never as a health warning.
///
/// Additive-only.
///
/// **`Default` is `{online: false, relays: []}` — which is exactly the shape above meaning
/// "deliberately LAN-only" (#148).** The porcelain reads it that way and SUPPRESSES the "no relay
/// connection" line for it. So a fixture built with `..Default::default()` claims a healthy
/// LAN-only posture, not an unknown one. There is no third value for a `bool`; the honest way to
/// say "nobody looked" is `StatusResult.self_network: None`, which is what a defaulted
/// `StatusResult` gives you.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfNetwork {
    /// A home-relay connection is established (iroh's `online` definition). The signal #53's
    /// `set_relays` never had: when this goes false on a relay-enabled node, the relay set is
    /// the thing to look at.
    pub online: bool,
    /// The CONNECTED home relay's URL, sanitized to scheme + host + port (operator-supplied
    /// relay URLs can carry userinfo tokens; `status` output gets screenshotted). `None` when
    /// no relay is connected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_relay: Option<String>,
    /// Every known home relay and its current connection state. Empty when no relays are
    /// configured, or before the endpoint has selected any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relays: Vec<RelayInfo>,
    /// This endpoint's direct (non-relay) socket addresses — its own dialable coordinates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub direct_addrs: Vec<String>,
    /// When the daemon's watcher last observed a TRANSITION (epoch seconds) — a change of
    /// `online`, `home_relay`, or a relay's connection state; `direct_addrs` drift alone does
    /// not stamp (nor emit a frame). OMITTED (not `null`) until the first observed transition
    /// after boot, and from a point-in-time computation with no watcher running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_change_epoch: Option<i64>,
    /// This node's `[network].presence_mode` (#89): `"paired"` | `"granted"` | `"off"` — who
    /// currently gets an answer to the `mcpmesh/ping/1` reachability probe.
    ///
    /// Reported because the setting was otherwise **unobservable**: an operator who set it had no
    /// way to confirm it took effect, and a product backing a privacy switch with it could not show
    /// the user its real state. Always present from `api_minor >= 38`.
    ///
    /// **It is not "appear offline".** It withholds the pong payload and makes our own probe report
    /// this node unreachable; it does not hide that the node is running (a QUIC application close
    /// implies a completed handshake, and `mcpmesh/pair/1` answers any stranger by design). Do not
    /// render it to users as invisibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_mode: Option<String>,
    /// This node's `[network].local_discovery` (#68): `"off"` | `"on"` | `"resolve"` — whether it
    /// finds peers on the local link with no internet, and whether it announces itself there.
    ///
    /// Reported for the same reason `presence_mode` is: the setting is otherwise unobservable, and
    /// this one has two questions behind it. "Why can these two machines on one LAN not find each
    /// other" is answered by `"off"`; and **`"on"` means this node is multicasting its endpoint id
    /// and addresses to every device on the link**, which a product backing a privacy switch has to
    /// be able to show the user. `"resolve"` never publishes this node's identity or addresses,
    /// but still emits a service query roughly once a second — quieter than `"on"`, not silent.
    ///
    /// Always present from `api_minor >= 50`; absent below it, and absent without a mesh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_discovery: Option<String>,
    /// When the relay last reported that ANOTHER endpoint is presenting this node's identity
    /// (#134, epoch seconds), or absent if never — the overwhelmingly common case.
    ///
    /// Two nodes booted from COPIES of one mesh root share an endpoint id. The relay can serve only
    /// one, so the displaced node's peers simply go unreachable with nothing saying why; diagnosing
    /// that cost a downstream real time. This is that missing "why".
    ///
    /// **Sticky, and a timestamp rather than a flag.** The condition is announced once, as the
    /// displaced connection is dropped — it is not a state the relay keeps reporting — so a
    /// self-clearing flag would read false by the time anyone called `status`. Judge staleness from
    /// the epoch, exactly as with `last_change_epoch`.
    ///
    /// **Absence is not proof of uniqueness.** Detection needs an
    /// `IdentityConflictLayer` in the process's `tracing` subscriber: the standalone daemon
    /// installs one at boot, but an EMBEDDED node cannot (a subscriber is global and the host owns
    /// it) and reports `None` until the host installs it. Never render absence as "identity
    /// verified unique".
    ///
    /// Additive: `#[serde(default, skip_serializing_if = "Option::is_none")]`. `api_minor >= 32`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_conflict_epoch: Option<i64>,
}

/// One home relay's connection state (#90). No latency — per-relay RTT needs iroh's
/// `net_report`, which is unstable-feature-gated as of 1.0.3; `connected` is the stable truth.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayInfo {
    /// Sanitized (scheme + host + port), like `home_relay`.
    pub url: String,
    pub connected: bool,
}

/// One entry of `status.revoked` (#85 ask 4).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RevokedEndpoint {
    /// The `eid:` device principal that is refused.
    pub principal: String,
    /// When THIS node applied it, epoch seconds.
    pub revoked_at_epoch: u64,
    /// `"local"` — this operator's own decision about someone else's device — or `"signed"`, a
    /// statement the device's OWNER issued about their own. Two different claims, and an operator
    /// reading this list needs to tell them apart: only the second is evidence that the person
    /// themselves declared the device dead. From `api_minor >= 64` (#223) also `"roster"` — the
    /// installed roster's `revoked_endpoints`, with `revoked_at_epoch` 0 — and `"roster_identity"`,
    /// a roster device whose roster `user_id` is a revoked `b64u:` identity (named in `reason`).
    /// Treat an unknown value as a revocation.
    pub source: String,
    /// For `"signed"`: the verified `b64u:` that signed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_user_id: Option<String>,
    /// Free-text operator note, never interpreted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The local nickname this endpoint still has a pair row under, if any. Revocation does not
    /// delete the row, so this is usually present — and it is what makes the list readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
}

/// The `status.storage` block (#88): bytes actually on disk, by subsystem. Counts, never
/// content. Additive-only.
///
/// **`Default` is all zeros, which reads as "measured, and found empty" (#148).** It is here so a
/// fixture can build one field and elide the rest; it is not a way to say "unmeasured". For that,
/// leave `StatusResult.storage` as `None` — a defaulted `StatusResult` does exactly that.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageInfo {
    /// Summed sizes of the monthly audit files (`<state>/audit/*.jsonl`).
    pub audit_bytes: u64,
    /// Size of the peer/trust state store (`state.redb`).
    pub redb_bytes: u64,
    /// Total size under the app-blob store directory; 0 when no blob store exists.
    pub blobs_bytes: u64,
    /// Blob garbage collection (#80), or `None` when it is not configured — which is the default
    /// and the behavior of every release up to 0.42.0.
    ///
    /// `None` means "not collecting", NOT "collecting and idle": a configured collector reports
    /// `Some` with `runs: 0` until its first sweep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blobs_gc: Option<BlobsGcInfo>,
}

/// The `status.storage.blobs_gc` block (#80): what the background app-blob collector has done.
///
/// **There is deliberately no `bytes_reclaimed`.** iroh-blobs calls back only BEFORE a sweep, never
/// after, so any byte count here would be a guess. `blobs_bytes` is measured by walking the store
/// directory; an operator reads reclaim off that, over time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobsGcInfo {
    /// The interval the store is actually running on, in seconds.
    pub interval_secs: u64,
    /// Runs STARTED. iroh-blobs offers no completion callback, so this counts sweeps begun.
    ///
    /// **Watch this number.** Upstream's collector `break`s its loop on the first sweep error
    /// rather than continuing, so one failure silently ends collection until the daemon restarts. A
    /// `runs` that stops advancing across several intervals is the only signal that happened.
    ///
    /// Also: the collector SLEEPS before its first run, so a node with a 24h interval reports
    /// `runs: 0` for its first 24 hours. That is not a fault.
    pub runs: u64,
    /// Unix seconds at the start of the most recent run; `None` before the first.
    pub last_run_epoch: Option<i64>,
    /// Hashes protected on the most recent run — the size of the liveness root the scope table
    /// produced.
    pub last_protected: u64,
    /// Runs ABORTED because the liveness root could not be read. Each one swept nothing, which is
    /// the intended fail-safe; a number that climbs means collection is not happening.
    pub aborted: u64,
}

/// Params of [`Request::RegisterService`]: the `[services.*]` entry to write/update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterServiceParams {
    pub name: String,
    pub backend: BackendSpec,
    pub allow: Vec<String>,
    /// When true (#36), the registration is EPHEMERAL: kept in daemon memory only, never written
    /// to the on-disk config, and automatically unregistered when the control connection that
    /// registered it closes (and gone on daemon restart). For an embedder that serves a
    /// `socket` backend from a fresh path each run, this removes the need to derive a stable
    /// socket path solely to keep a persisted registration valid, and the stale-registration
    /// accumulation that comes with no unregister. Default false = the persistent behavior.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ephemeral: bool,
    /// Per-service proxied-request rate (#63), falling back to `[limits].rate_limit_per_min`.
    ///
    /// **CLAMPED, never honoured upward.** `[limits].rate_limit_per_min` is a hard ceiling: a
    /// larger value here is reduced to it, so a control call cannot uncap a service. Before #63
    /// every service a peer could reach drew from one shared bucket, so a noisy service starved a
    /// quiet one; buckets are now per `(service, endpoint)`.
    ///
    /// `0` is rejected rather than silently blocking every request. `api_minor >= 40`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_min: Option<u32>,
}

/// Params of [`Request::Invite`]: the services the minted invite grants. Rejects unknown
/// fields (so `{service: "kb"}` — a singular typo — is a loud error, not a silently
/// grants-nothing invite), and the daemon additionally rejects an empty/absent `services`
/// list (an invite that grants nothing is useless — #34).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InviteParams {
    #[serde(default)]
    pub services: Vec<String>,
    /// An OPAQUE, caller-chosen label carried through to the redeemer in the `pair` result (#31).
    /// mcpmesh never interprets it (not a nickname, never resolved or authorized) — a per-pairing
    /// metadata slot for the embedder (e.g. its own URN). Capped at the daemon; omit for none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_label: Option<String>,
    /// How many times this invite may be redeemed (#87). Absent = **1**, the single-use behaviour
    /// every existing caller already gets.
    ///
    /// Each redemption runs its OWN SAS ceremony and writes its own mutual peer rows — this is not
    /// a shared or group identity, it is N independent pairings that happen to share one secret.
    /// Onboarding a team stops being N mint-and-send rounds.
    ///
    /// Clamped to [`MAX_INVITE_USES`]; `0` is rejected rather than silently meaning "unusable". A
    /// bearer credential's blast radius is `max_uses` × TTL, so it is opt-in and capped on purpose.
    /// The value actually applied comes back in [`InviteResult::uses_remaining`] — read that rather
    /// than assuming you got what you asked for.
    ///
    /// **`api_minor >= 35`, and sending it to an older daemon FAILS rather than degrading.**
    /// `InviteParams` is `deny_unknown_fields`, so an `api_minor < 35` daemon answers `-32602
    /// unknown field 'max_uses'` — it does not quietly mint a single-use invite. Loud is the right
    /// behaviour; omit the field entirely when talking to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,
    /// YOUR local name for whoever redeems this invite (#87), overriding the nickname they claim
    /// for themselves in the ceremony.
    ///
    /// The redeemer's self-claimed name is usually its hostname, so two same-model laptops collide
    /// and the pairing is refused with [`ERR_NICKNAME_TAKEN`]. Before this field the only fixes
    /// were to ask the other person to rename their machine, or to unpair whoever holds the name.
    /// This lets you just call them something else.
    ///
    /// Local only: it is never sent to the peer and never affects what they call themselves or
    /// you. It does **not** bypass the collision check — an alias that itself collides is refused
    /// identically, because a duplicate display name makes your own `<peer>/<service>` routing
    /// ambiguous whoever chose it.
    ///
    /// **Rejected with `max_uses > 1`:** one alias applied to every redeemer of a multi-use invite
    /// would collide on the second redemption, so it is refused at MINT rather than producing an
    /// invite that works exactly once. `api_minor >= 39`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_nickname: Option<String>,
    /// Mint a SELF-ENROLLMENT invite (#86): the redeemer becomes another device of **you**, not a
    /// peer.
    ///
    /// The ceremony is the ordinary one — same secret, same SAS. What differs is the outcome:
    /// neither side writes a peer row and nothing is granted, and the inviter signs a device→user
    /// binding for the redeemer's authenticated endpoint. Both devices then present the same
    /// `user_pk`, so every peer resolves them to ONE `user_id`.
    ///
    /// **The private key never moves.** The enrolling device signs a binding for the new device's
    /// endpoint and hands over only that signature, so a second copy of the identity never exists.
    /// The consequence: an enrolled device cannot enroll a third — enroll every device from the one
    /// that holds the key.
    ///
    /// **The SAS matters more here than anywhere else.** The inviter signs a binding for whichever
    /// endpoint redeems, so a redemption by an impostor mints *that impostor* a binding for your
    /// identity. Requires `max_uses = 1` and an empty `services`, both refused otherwise.
    /// `api_minor >= 43`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub as_self: bool,
}

/// The ceiling on [`InviteParams::max_uses`] (#87). Comfortably above "a team", far below "a
/// fleet": one leaked invite line must not be able to enroll an unbounded number of devices for the
/// whole 24h TTL.
pub const MAX_INVITE_USES: u32 = 64;

/// Params of [`Request::Pair`]: the copyable `mcpmesh-invite:` line. Defaultable — an
/// absent field reads as an empty line, which simply fails to decode (a clean pair error).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairParams {
    #[serde(default)]
    pub invite_line: String,
    /// YOUR local name for the inviter (#87), overriding the nickname their invite suggests.
    ///
    /// An invite carries the inviter's suggestion for what you should call them — usually their
    /// hostname. If you already use that name for a different peer, the pairing is refused with
    /// [`ERR_INVITE_NAME_CONFLICT`] and the message tells you to go ask them for a new invite.
    /// This lets you resolve it yourself, without `set_nickname` (which rewrites your own GLOBAL
    /// self-name — not what anyone wants in order to add one colleague).
    ///
    /// Local only: never sent to the inviter. It does **not** bypass the collision check — an alias
    /// that itself collides is refused identically, because a duplicate display name makes your own
    /// `<peer>/<service>` routing ambiguous whoever chose it. `api_minor >= 39`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_nickname: Option<String>,
    /// Consent to complete a SELF-ENROLLMENT (#178): a `mcpmesh-enroll:` line is refused with
    /// [`ERR_SELF_ENROLL_NOT_OFFERED`] unless this is set.
    ///
    /// Defaults to `false`, which is the whole point. #86 gave self-enrollment its own scheme so a
    /// version-skewed redeemer refuses rather than pairing wrongly — but a CURRENT caller that only
    /// ever meant to pair still ran the ceremony to completion, and learned which one it had run
    /// from [`PairResult::enrolled_as_self`] only AFTER the device→user binding was written. That
    /// binding admits this device to everyone who trusts the inviter's `user_id`, and it is
    /// irrevocable short of rotating that user key — so "observe it afterwards" is not a place a
    /// caller can refuse from.
    ///
    /// Set it when the ceremony is one your UI actually OFFERED ("add another of my devices"). Leave
    /// it unset on an ordinary "join / add a contact" field: the refusal costs nothing, the invite is
    /// untouched (nothing is dialled and nothing is burned), and the same line still works if the
    /// person is then offered the real choice.
    ///
    /// To decide BEFORE calling — to show the right prompt rather than recover from a refusal — use
    /// `mcpmesh_node::pairing::is_enrollment_line`. `api_minor >= 45`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_self_enroll: bool,
}

/// Params of [`Request::AttestTo`] (#85 ask 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttestToParams {
    /// A `mcpmesh-attest:` line from the peer that will admit this device.
    pub offer: String,
}

/// Result of [`Request::AttestOffer`] (#85 ask 3).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AttestOfferResult {
    /// The `mcpmesh-attest:` line. Hand it to your other device.
    pub offer: String,
}

/// Params of [`Request::PeerRevoke`] (#85 ask 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRevokeParams {
    /// A nickname, an `eid:` device principal, or a `b64u:` user_id. A `b64u:` revokes EVERY
    /// endpoint this node associates with that person.
    pub peer: String,
    /// Free-text operator note, stored and shown in `status`. Never interpreted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Result of [`Request::PeerRevoke`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PeerRevokeResult {
    /// The `eid:` principals now revoked. Empty means the name resolved to no endpoint — an ERROR
    /// is returned instead in that case, so an empty list here never reads as success.
    pub revoked: Vec<String>,
    /// Live connections severed by this call. `0` is normal (the peer may be offline) and is NOT a
    /// failure — but a non-zero count is the evidence that revocation was immediate rather than
    /// deferred to the peer's next disconnect.
    pub severed: usize,
}

/// Params of [`Request::PeerUnrevoke`] (#85 ask 4).
///
/// A nickname or `eid:` lifts that device's ENDPOINT revocation and reports it in `unrevoked` —
/// but under a standing IDENTITY revocation (`peer_revoke b64u:`) on the `user_id` its row carries,
/// that re-admits nothing (#218, `api_minor >= 61`): the gate still refuses the device on the
/// identity. Unrevoke the `b64u:` to restore every device of the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerUnrevokeParams {
    /// Nickname, `eid:`, or `b64u:` — the same vocabulary as `peer_revoke`.
    pub peer: String,
}

/// Result of [`Request::PeerUnrevoke`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PeerUnrevokeResult {
    /// The `eid:` principals whose revocation was lifted. Empty = none were revoked (idempotent).
    pub unrevoked: Vec<String>,
}

/// Params of [`Request::DeviceRevoke`] (#85 ask 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRevokeParams {
    /// The `eid:` principal of the device being revoked — one of THIS person's own.
    ///
    /// Explicit rather than "this node", because the device you are revoking is usually the one you
    /// no longer have: you issue the token from your replacement machine, naming the lost one.
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Result of [`Request::DeviceRevoke`]: the portable token, and what it says.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DeviceRevokeResult {
    /// The signed statement, `mcpmesh-revoke:<base64url>`. Hand this to your peers.
    ///
    /// Not a secret — it authorizes nothing and grants nothing; it only asks that an endpoint be
    /// treated as dead, and only peers who already trust the signer will act on it.
    pub token: String,
    /// The `eid:` it revokes, echoed so a caller can confirm they named the device they meant.
    pub endpoint: String,
    /// The signing `b64u:` user_id — the identity your peers already pinned.
    pub user_id: String,
}

/// Params of [`Request::DeviceRevocationImport`] (#85 ask 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRevocationImportParams {
    /// A `mcpmesh-revoke:` token from a peer.
    pub token: String,
}

/// Result of [`Request::DeviceRevocationImport`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DeviceRevocationImportResult {
    /// The `eid:` now revoked.
    pub endpoint: String,
    /// The `b64u:` that signed it — verified, not claimed.
    pub user_id: String,
    /// `false` when an equal-or-newer revocation for this endpoint was already held: the import is
    /// idempotent, and a replayed OLDER token must not overwrite a newer one.
    pub applied: bool,
    /// Live connections severed. See `PeerRevokeResult::severed`.
    pub severed: usize,
}

/// Params of [`Request::PeerRemove`]: the nickname to unpair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRemoveParams {
    pub nickname: String,
}

/// Params of [`Request::PeerRename`]: the contact to rename — every device sharing `user_id`
/// when given, else the single provisional `nickname` entry — and the new nickname `to`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRenameParams {
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub nickname: Option<String>,
    pub to: String,
}

/// Params of [`Request::PeerAdd`] (reserved/internal — see the variant): a raw `endpoint_id`
/// (iroh base32) plus the nickname and service allow list to install it under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerAddParams {
    pub nickname: String,
    pub endpoint_id: String,
    #[serde(default)]
    pub allow: Vec<String>,
}

/// Params of [`Request::PeerEndorse`] (#65): vouch for a peer so a third party can install them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerEndorseParams {
    /// The subject's endpoint id, `eid:<hex>` — usually a peer you are paired with, though the
    /// daemon does not require that: an endorsement is YOUR statement, and the recipient decides
    /// what it is worth.
    pub subject: String,
    /// The subject's user key, `b64u:`, when you are also vouching for that. The recipient will
    /// additionally require the SUBJECT's own device binding before trusting it — see
    /// [`PeerIntroduceParams::subject_binding`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_user_id: Option<String>,
}

/// Result of [`Request::PeerEndorse`] (#65) — hand both fields to the recipient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerEndorseResult {
    /// YOUR user id, `b64u:` — what the recipient passes as `endorsed_by`. They must already be
    /// paired with you for it to resolve.
    pub endorsed_by: String,
    /// The signature, `b64u:` — what the recipient passes as `evidence`.
    pub evidence: String,
}

/// Params of [`Request::PeerIntroduce`] (#65): install a peer vouched for by someone you are
/// already paired with.
///
/// The endorsement replaces pairing's SAS with the endorser's signature, so you are trusting that
/// endorser's judgment and key hygiene as well as their identity. It buys identity resolution only
/// — see [`Request::PeerIntroduce`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerIntroduceParams {
    /// The subject's endpoint id, `eid:<hex>` — who is being introduced.
    pub subject: String,
    /// The endorser's user public key, `b64u:`. MUST be the `user_id` of a CURRENTLY paired peer:
    /// the chain has to terminate at someone you paired with yourself, so an endorsement from a
    /// stranger — or from someone you have since unpaired — is refused.
    pub endorsed_by: String,
    /// The endorser's signature over the domain-separated preimage, `b64u:`.
    pub evidence: String,
    /// The subject's OWN user key, `b64u:`, so several of the subject's devices resolve to one
    /// person. Part of the endorser's signed statement, so it cannot be added or removed after
    /// the fact.
    ///
    /// **Requires `subject_binding` too, and is REFUSED without it.** A `user_id` is
    /// authorization-bearing — service `allow` lists match on it — so the endorser vouching for it
    /// is not enough: an endorser could otherwise name a *victim's* `user_id` (which is public, on
    /// `status` and every audit record) for an attacker's endpoint, and the attacker would inherit
    /// that victim's grants. The subject must prove the key is theirs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_user_id: Option<String>,
    /// The SUBJECT's own device→user binding for `subject_user_id`, `b64u:` — the same signature a
    /// peer presents at pairing (`mcpmesh/join/device-binding/1`), proving *it* controls that user
    /// key and that the key is bound to *this* endpoint.
    ///
    /// Two independent signatures are required for a `user_id`, and they say different things: the
    /// endorser's says "I vouch for this endpoint", the subject's says "this user key is mine".
    /// Neither alone is sufficient.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_binding: Option<String>,
    /// YOUR local name for the subject. Same rules and the same collision guard as pairing (#87).
    pub nickname: String,
}

/// Params of [`Request::OpenSession`]: the `peer/service` target to dial. Both fields are
/// defaultable — an empty target simply fails the dial (a clean `-32055` error).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenSessionParams {
    #[serde(default)]
    pub peer: String,
    #[serde(default)]
    pub service: String,
    /// A QUIC idle timeout for THIS session only, in seconds (#166, `api_minor >= 54`).
    ///
    /// Absent inherits `[network].idle_timeout_secs`, i.e. today's behaviour. `0` means "no idle
    /// timeout from this side" — the same meaning the node-wide knob gives it, so the two cannot be
    /// read differently.
    ///
    /// **Only LOWERING is unilateral.** QUIC negotiates `max_idle_timeout` to the MINIMUM of the
    /// two peers' values (RFC 9000 §10.1), so this can always make a session die sooner when it
    /// goes quiet and can never make it outlive what the peer allows. Raising needs both peers
    /// configured — which was already true of the node-wide knob, and which no per-connection seam
    /// changes.
    ///
    /// **Outbound only, and that is not half a feature.** An accepted session uses the node-wide
    /// value, because iroh gives the server config no per-connection seam — but the direction that
    /// CAN work one-sidedly is available here, and the direction that cannot never worked anywhere.
    /// A node wanting a shorter timeout on a session it did not initiate can dial instead.
    ///
    /// **Ignored on a RACING dial**, because that opens connections it then abandons. The precise
    /// rule, since the first version of this sentence was wrong in both directions:
    ///
    /// - A **roster person** races whenever the roster lists them with ANY device — including
    ///   exactly one — so a `user_id` naming a rostered person never gets it.
    /// - A pairing-mode **`b64u:`** races only with TWO OR MORE stored devices; with exactly one it
    ///   falls through to the single-entry path and DOES get it.
    ///
    /// The node logs at `warn!` when it drops the value, naming the peer — a caller cannot know how
    /// many devices a peer has, so a silent drop would be unattributable. Name one device with
    /// `eid:` to be certain.
    ///
    /// No per-connection KEEPALIVE: iroh caps the per-path keepalive at 5s and discards larger
    /// values, so one could only make pings more frequent — the node-wide knob already refuses that
    /// direction (#56).
    ///
    /// **Its own connection (#215, `api_minor >= 66`).** Sessions to one peer normally share a single
    /// QUIC connection; a session that sets this cannot, because the timeout is a property of the
    /// connection. It dials its own, and plain sessions never join it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_secs: Option<u64>,
}

/// Params of [`Request::RosterInstall`]: the LOCAL roster file `path`, plus the org-root pin
/// on FIRST install (`b64u:`; omit once pinned — config carries it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RosterInstallParams {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_root_pk: Option<String>,
}

/// Params of [`Request::OrgJoin`]: the `[identity]` pin. `user_key` is a LOCAL path — the key
/// never crosses the API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgJoinParams {
    pub org_id: String,
    pub org_root_pk: String,
    pub user_id: String,
    pub user_key: String,
}

/// Params of [`Request::SetAppMetadata`]: this node's opaque app-metadata blob (#39). The
/// daemon NEVER interprets it — the embedder structures its own bytes (a version string,
/// small JSON, …). Capped at 256 bytes; `""` clears it. Roster-mode only (it rides the
/// signed presence heartbeat); a pure-pairing daemon accepts + stores it but never gossips it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetAppMetadataParams {
    pub metadata: String,
}

/// Params of [`Request::PeerServices`] (#52): the peer to query — a nickname, an `eid:` device
/// principal, or a `b64u:` user_id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerServicesParams {
    pub peer: String,
}

/// Result of [`Request::PeerServices`] (#52): the services the queried peer CURRENTLY grants the
/// caller — computed authoritatively on the peer (which owns the truth), always current, only
/// the caller's own admitted services (never the peer's full registry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerServicesResult {
    pub services: Vec<String>,
}

/// Params of [`Request::PeerDiagnostics`] (#140): the peer to dump — a nickname or an `eid:`
/// device principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerDiagnosticsParams {
    pub peer: String,
}

/// Result of [`Request::PeerDiagnostics`] (#140): the DURABLE per-peer state this node carries,
/// for diagnosing why a specific long-lived pairing behaves differently from a fresh one.
///
/// **This surface carries a PEER's transport coordinates on purpose.** The rendered porcelain is
/// address-free everywhere — nicknames and path KINDS — because that discipline keeps a peer's
/// coordinates out of screenshots. (`SelfNetwork.direct_addrs` already returns this node's OWN
/// addresses on `status`; what is new here is another endpoint's.) The question this answers is
/// "what address is this node about to dial, and where did it come from", which has no answer
/// without the address. It is your own store's record of your own paired peers. Do not render it
/// in ordinary porcelain, and read it before pasting it anywhere public.
///
/// The intended use is a paired capture: run it on BOTH ends of a stuck pairing and compare the
/// stored hint against the live path each side reports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerDiagnosticsResult {
    /// The peer's nickname as this node stores it.
    pub nickname: String,
    /// The peer's stable `eid:` device principal.
    pub principal: String,
    /// The peer's `b64u:` user_id if it proved a device→user binding at pairing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// When the pairing was written (epoch seconds as a string), if recorded. A LONG-LIVED pairing
    /// is exactly what #140 is about, so the age is part of the evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paired_at: Option<String>,
    /// The persisted dial HINT, verbatim as stored — the durable state a freshly paired identity
    /// does not have. `None` for a peer added without one.
    ///
    /// It is MERGED with discovery rather than replacing it — iroh inserts it as one more
    /// candidate path (`Source::App`) and then triggers address lookup.
    ///
    /// **But that lookup is skipped when a path is already selected.** iroh's
    /// `trigger_address_lookup` returns early if `selected_path.is_some()`, and a selected path is
    /// cleared only when the last connection to that peer closes. So on a pair that already holds
    /// an open RELAYED connection — live sessions, dial-backs, a working relay — discovery does
    /// NOT re-run, and this hint is the only addressing the dial contributes. Do not read "merged,
    /// so a stale hint is harmless" as unconditional; it is least true in exactly the state a
    /// stuck pairing is in.
    ///
    /// It is the only durable per-peer state ON THIS NODE'S DISK that the dial path reads, which
    /// is what makes it the first thing to compare between two ends. It is not the only durable
    /// state a long-lived identity carries — a published discovery record under the same key, and
    /// [`SelfNetwork::identity_conflict_epoch`], live elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_addr: Option<String>,
    /// The addresses parsed out of `last_addr`, for reading without a JSON round trip: IP
    /// addresses verbatim, relay URLs as `relay <url>` and SANITIZED to scheme+host+port (an
    /// operator's relay URL can carry a userinfo token, and this output is meant to be pasted into
    /// an issue). Empty when the hint is absent, unparseable, or for a different endpoint — all of
    /// which degrade to an id-only dial.
    ///
    /// A `relay …` entry with no IP alongside it is worth noticing: that hint can never punch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hint_addrs: Vec<String>,
    /// Whether the stored hint actually contributes anything to a dial. `false` with a present
    /// `last_addr` means it is being silently discarded — because it does not parse, because its
    /// embedded id is a different peer, or (since 0.52.1, #203) because every address in it is one
    /// that can never be a QUIC peer and was filtered out. `last_addr` is reported verbatim above,
    /// so comparing it against `hint_addrs` distinguishes the three.
    pub hint_usable: bool,
    /// This node's LIVE view of the peer, read straight from the reachability cache — the same
    /// values `status` reports, repeated here so one capture holds both the durable and the live
    /// side. `None` when this peer has **never been probed**, which is the honest answer on a
    /// freshly restarted daemon; it is not the same as unreachable.
    ///
    /// Read from the cache rather than through `status`'s projection deliberately: that projection
    /// spawns a background probe for every stale peer, which would make this diagnostic a
    /// participant in the reproduction it is meant to observe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reachability: Option<PeerReachability>,
    /// What **iroh** currently holds for this endpoint, as opposed to what this node stored
    /// (#140, `api_minor >= 56`). The other half of the capture, and the half the standing
    /// hypothesis lives in.
    ///
    /// Everything above describes our own disk. Until this field there was no way to see what iroh
    /// made of it — whether the hint's addresses are in its remote map at all, whether discovery
    /// contributed anything alongside them, or which address is carrying traffic. Read straight off
    /// `Endpoint::remote_info`, a point read of the remote map: no dial, no probe, no address
    /// lookup, so this stays safe to run ON a live reproduction.
    ///
    /// **`None` means iroh currently holds NO ENTRY — which is not the same as an empty list, and
    /// is not the same as "never heard of".** iroh reaps a remote's state about 60 seconds after
    /// the last connection to it closes (`ACTOR_MAX_IDLE_TIMEOUT`), so `None` is the normal answer
    /// both for a peer never dialled AND for one talked to a few minutes ago. An empty list means
    /// iroh has an entry and holds no address in it.
    ///
    /// This distinction matters most to the question #140 is asking, which is about durability over
    /// time: reading `None` as "iroh never knew this peer" would be wrong on any daemon that has
    /// been idle, which is most of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_addrs: Option<Vec<KnownAddr>>,
    /// Addresses in [`hint_addrs`](Self::hint_addrs) that iroh does **not** currently hold.
    ///
    /// The hint is written whole: `set_last_addr` REPLACES the stored value with one built from the
    /// live connection's open IP paths, so these are not accumulated cruft — they are addresses
    /// that were real at the last successful connection and are absent from iroh's view now. (An
    /// earlier version of this doc said #124 "amends but never removes"; that is false, and
    /// `dial_hint.rs` says so in as many words.)
    ///
    /// **Empty whenever [`known_addrs`](Self::known_addrs) is `None`**, because there is no view to
    /// difference against. Reporting the whole hint as unknown when iroh simply has no entry would
    /// call a current, correct hint stale on every idle daemon.
    ///
    /// INFERRED by set difference, not read: iroh 1.0.3's `TransportAddrInfo` carries no
    /// provenance, so "did this come from our hint or from discovery?" cannot be answered directly.
    /// Naming it an inference is the honest version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hint_addrs_unknown_to_iroh: Vec<String>,
    /// The converse: what iroh holds that our stored hint does not name — inferred the same way,
    /// and empty for the same reason when [`known_addrs`](Self::known_addrs) is `None`.
    ///
    /// **A non-empty list here is the NORMAL state of a healthy peer, and a growing one is not
    /// evidence that the stored hint has drifted.** The two lists are built from different things
    /// and are expected to differ in size:
    ///
    /// - [`hint_addrs`](Self::hint_addrs) comes from the open **IP** paths of one live connection —
    ///   typically one to three addresses.
    /// - [`known_addrs`](Self::known_addrs) is iroh's remote map: every address it has learned for
    ///   the peer, from discovery and from paths, active or not, **relay URLs included**.
    ///
    /// So this list grows when **iroh learns more candidates**, which is discovery working. It says
    /// nothing about whether the hint is current; [`hint_addrs_unknown_to_iroh`] is the field that
    /// speaks to that, in the other direction.
    ///
    /// A relay URL iroh holds will sit here for as long as a hint whose addresses came from
    /// `dial_hint::observed_for` stands: that function filters to IP paths, so a value it produced
    /// cannot name a relay. Verified end to end against a **real** relay by
    /// `a_live_session_refreshes_the_dial_hint_and_never_stores_a_relay`
    /// (`cli/tests/dial_hint_refresh.rs`), which starts a relayed session and asserts the healed
    /// hint carries the peer's direct address and no relay.
    ///
    /// **Sourced-from, not written-when — the distinction is load-bearing.** A stored hint can
    /// still name a relay three ways, and each is reported rather than hidden: a legacy row written
    /// before 0.52.2; a legacy value *carried forward* by a later write, since `merge_hint` and the
    /// attestation admit path preserve a stored value rather than originating one; and a direct
    /// `PeerStore::add` by an embedder, `allowlist` being a public module.
    ///
    /// This warning exists because the reading was gotten wrong in the field, on #140 itself: a
    /// reporter read this going 5 -> 8 across an upgrade as the hint drifting from iroh's view.
    ///
    /// [`hint_addrs_unknown_to_iroh`]: Self::hint_addrs_unknown_to_iroh
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iroh_addrs_not_in_hint: Vec<String>,
}

/// One address iroh holds for a peer (#140, `api_minor >= 56`).
///
/// Rendered by the SAME labelling [`PeerDiagnosticsResult::hint_addrs`] uses — IPs verbatim, relays
/// as `relay <url>` sanitized to scheme+host+port — because the two lists exist to be read side by
/// side, and differently-formatted addresses would make a formatting difference look like a real
/// one. The sanitization is not optional here either: this output is meant to be pasted into a
/// public issue, and an operator's relay URL can carry a userinfo token.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownAddr {
    /// The address, labelled as above.
    pub addr: String,
    /// Whether iroh reports this address as in ACTIVE use, as opposed to merely known.
    ///
    /// **One bit, and it collapses three states.** iroh maps only `PathStatus::Open` to active;
    /// `Inactive`, `Unusable` ("we attempted holepunching and it didn't work") and `Unknown` ("we
    /// have not yet attempted holepunching") all render `false`. So an idle direct address beside an
    /// active relay is *consistent with* #140's selected-path hypothesis and equally consistent with
    /// an ordinary failed hole-punch — which is a NAT diagnosis, the opposite conclusion. This field
    /// cannot discriminate them; it says what is carrying traffic and no more.
    pub active: bool,
}

/// Params of [`Request::PeerHintClear`] (#140): whose stored dial hint to forget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerHintClearParams {
    /// Nickname / `eid:` / `b64u:` — the same vocabulary every other peer verb takes.
    pub peer: String,
}

/// Result of [`Request::PeerHintClear`] (#140).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerHintClearResult {
    /// How many hints were removed. A `b64u:` user_id names EVERY device of that person and the
    /// racing dial path attaches a hint for each, so clearing "the" hint would leave the pairing
    /// still addressing from stored state while reporting success. Every device is cleared, and the
    /// count says how many actually had one.
    ///
    /// `0` means nothing was stored — already the state this verb produces, so a no-op, not an error.
    pub cleared: usize,
    /// The raw hints removed, verbatim, in the order the devices resolved.
    ///
    /// **This is the undo, and since 0.52.2 it is the ONLY one.** There is no `peer_hint_set`, and
    /// on the pairing this verb targets the hint may never be rewritten on its own: every writer
    /// now stores only what a connection OBSERVED, and all of them decline to store anything for a
    /// relay-only connection — which is the defining property of a stuck pair. Re-pairing no longer
    /// reliably restores it either: the ceremony writes a hint only if it observes a direct path,
    /// and on a pair that cannot punch it does not. Keep this value.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forgotten: Vec<String>,
}

/// Params of [`Request::UnregisterService`] (#50): the persistent (or ephemeral) service name
/// to remove — the deregistration mirror of `register_service`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnregisterServiceParams {
    pub name: String,
}

/// Params of [`Request::ServiceAllowGrant`] / [`Request::ServiceAllowRevoke`] (#44): toggle a
/// single stable `principal` (`b64u:`/`eid:`) on a single `service`'s allow list, WITHOUT
/// unpairing. The per-peer "sharing" switch primitive the embedder drives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceAllowParams {
    pub service: String,
    pub principal: String,
}

/// Params of [`Request::SetNickname`]: this node's new self-nickname (#37). Display-only
/// semantics: it names this node in FUTURE invites/presentations; peers keep the nickname
/// they stored at pairing time until a re-invite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetNicknameParams {
    pub nickname: String,
}

/// Params of [`Request::SetRosterUrl`]: the HTTPS roster URL to pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetRosterUrlParams {
    pub url: String,
}

/// Params of [`Request::SetRelays`] (#53): the node's desired CUSTOM relay set. Declarative —
/// "make the custom relay set exactly this" — applied as a live insert/remove diff against the
/// running endpoint (iroh 1.0.3 `Endpoint::insert_relay`/`remove_relay`) when the node is already
/// in `relay_mode = "custom"`, then persisted to `[network]`. Each entry must parse as an iroh
/// `RelayUrl`; an empty list is rejected (custom mode requires ≥1 relay — fully disabling relays
/// is a `relay_mode = "disabled"` restart, not this verb). Switching a node that is currently
/// `default`/`disabled` onto custom persists the config but needs a restart to take effect (iroh
/// cannot live-transition the relay MODE) — signalled by [`SetRelaysResult::restart_required`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetRelaysParams {
    pub relay_urls: Vec<String>,
}

/// Result of [`Request::SetRelays`] (#53).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetRelaysResult {
    /// The persisted `relay_urls` differed from the prior config (a no-op edit → `false`).
    pub changed: bool,
    /// `true` iff the node's current `relay_mode` is not `custom`, so the new set was persisted
    /// but NOT applied live — a node restart is required for it to take effect. `false` on the
    /// live custom→custom path (already applied to the running endpoint).
    pub restart_required: bool,
}

/// Params of [`Request::BlobPublish`]: the scope to publish into and the LOCAL file to add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobPublishParams {
    pub scope: String,
    pub path: String,
}

/// Params of [`Request::BlobGrant`]: the scope and the flat-namespace principal to grant it to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobGrantParams {
    pub scope: String,
    pub principal: String,
}

/// Params of [`Request::BlobRevoke`] (#62): the scope and the principals to withdraw from it.
///
/// SCOPED, unlike unpair hygiene: only the named scope's grants change. A principal that also holds
/// grants on other scopes keeps them — withdrawing access to one thing must not silently withdraw
/// access to everything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobRevokeParams {
    pub scope: String,
    pub principals: Vec<String>,
}

/// Params of [`Request::BlobUnpublish`] (#62): the scope and the blake3 hex to remove from it.
///
/// Removes REACHABILITY, not bytes. The scope gate requires a hash to be listed in some scope, so
/// this takes effect immediately for authorization — but the bytes stay in the local store until a
/// GARBAGE-COLLECTION sweep reclaims them, and only a node that set `[blobs].gc_interval` runs one
/// (#80, `api_minor >= 49`). There is no reclaim VERB — collection is periodic and configured at
/// store construction, so "deleted" means "deleted within an interval" at best. On a node with no
/// interval configured — the default — the bytes stay forever. Do not surface this to a user as
/// deletion unless you know the node collects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobUnpublishParams {
    pub scope: String,
    pub hash: String,
}

/// Params of [`Request::BlobRepublish`] (#83): the scope and the blake3 hex to add to it.
///
/// The blob must already be held COMPLETE by this daemon — republish makes a fetched blob servable
/// FROM this node, it does not fetch. A hash that is absent, or only partially present from an
/// interrupted fetch, is refused with [`ERR_NO_SUCH_BLOB`]: advertising bytes we cannot serve would
/// turn the original publisher going offline into a hang at every fetcher.
///
/// It grants NOBODY. The republisher names a scope they already control; inheriting the original
/// publisher's grants would be a silent authorization transfer. Share with `blob_grant`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobRepublishParams {
    pub scope: String,
    pub hash: String,
}

/// Params of [`Request::BlobFetch`]: the `mcpmesh/blob/1` ticket and the LOCAL export path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobFetchParams {
    pub ticket: String,
    pub dest_path: String,
    /// ADDITIONAL sources to try if the ticket's publisher does not answer (#83).
    /// `api_minor >= 47`.
    ///
    /// Content addressing makes every recipient a potential source, and without this the control
    /// API made that unusable: a ticket names ONE address, so a file shared with a room became
    /// unfetchable the moment the sender closed their laptop — even though other people in the room
    /// already held the identical, verified bytes.
    ///
    /// Each entry is a stable principal (`eid:` device, `b64u:` user_id) or a paired nickname —
    /// the same vocabulary `open_session` takes. They are preferred **in order, after** the ticket's
    /// own address, so the publisher stays the first choice and a live one costs nothing —
    /// alternates are not dialled at all when it answers.
    ///
    /// Since 0.49.1 the dials are **hedged**: an alternate starts about a second after a source has
    /// failed to answer, rather than after that source's full dial timeout. A source unreachable at
    /// the head of the list therefore costs about a second, not twenty, and a long `from` list is no
    /// longer a long wait. Each source is still dialled at most once.
    ///
    /// **An alternate only works if it can serve you.** The bytes are BLAKE3-verified against the
    /// ticket's hash whoever supplies them, so a hostile alternate cannot substitute content — but
    /// it must have republished the hash into a scope that grants you, or it answers with a
    /// permission refusal and the fetch moves on. Every failure mode falls through, not only an
    /// unreachable dial: a refusal, a missing hash, a mid-stream reset, and a stalled transfer all
    /// move to the next source.
    ///
    /// Additive: absent from an older caller's payload and read as empty, which is the
    /// single-source behaviour.
    /// Capped at [`MAX_BLOB_SOURCES`]; more is an error rather than a silent truncation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from: Vec<String>,
}

/// The ceiling on [`BlobFetchParams::from`] (#83).
///
/// Naming a PERSON expands to every device of theirs, so the list a caller writes is not the number
/// of dials it buys. Comfortably above "everyone in a room", far below anything that turns a fetch
/// into an hour.
///
/// **The justification changed in 0.49.1 and the number did not.** Until then sources were tried
/// strictly in turn, each costing up to a dial timeout, so the cap bounded a WAIT. Dials are now
/// hedged, which bounds the wait by roughly a second per unresponsive source instead — but the cap
/// still bounds the WORK: a fetch holds one of the connection's [`MAX_INFLIGHT`] slots, and a long
/// list is a long list of other people's machines being asked to answer.
///
/// Exceeding it is an ERROR, not a truncation: silently dropping the tail would make a fetch fail
/// while the source that had the blob sat unused, which is exactly what this feature exists to
/// prevent.
pub const MAX_BLOB_SOURCES: usize = 32;

/// Params of [`Request::BlobFetchCancel`] (#172): stop every in-flight [`Request::BlobFetch`] of
/// this blob.
///
/// Keyed by HASH, not by JSON-RPC id, and the reason is not aesthetic: [`crate::ControlClient`]
/// borrows `&mut self` for a request's whole duration, so a client physically cannot send an
/// id-keyed cancel down the connection whose request it would name. A hash is reachable from
/// anywhere — including a fresh connection — and it is already the key a consumer holds, since
/// every [`crate::StreamFrame::BlobTransfer`] carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobFetchCancelParams {
    /// The blob's BLAKE3 hash, hex — as it appears on `BlobTransfer` frames and `BlobFetchResult`.
    pub hash: String,
}

/// Control-API requests. Serialized as `{ "method": "...", "params": {...} }`
/// (JSON-RPC-shaped; the id/jsonrpc envelope is added by the transport layer).
///
/// Each param-carrying variant wraps its named `*Params` struct — the ONE wire truth for that
/// method's params, shared by clients (which serialize whole `Request`s) and the daemon (which
/// deserializes `params` into the same struct after its method-string dispatch). Adjacent
/// tagging serializes a newtype variant's content as the struct's fields, so the wire shape is
/// identical to inline variant bodies.
///
/// **Servers dispatch on the `method` string and deserialize `params` per-method** — tolerating
/// omitted / null / empty-object params for parameterless methods — rather than deserializing a
/// whole message into `Request` (adjacent tagging rejects `params:{}` for unit variants).
/// This keeps the wire tolerant for third-party clients (the versioned, additive-only surface).
/// Use [`method_of`] to extract the tag, then match + deserialize `params` per-method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    /// Register/update a `[services.*]` entry idempotently.
    RegisterService(RegisterServiceParams),
    Status,
    /// Mint a pairing invite granting `services` — single-use unless `max_uses` says otherwise
    /// (#87). The daemon
    /// answers an [`InviteResult`] carrying the copyable `mcpmesh-invite:` line. Tag
    /// `"invite"` (snake_case). `method_of` needs no per-variant arm — it reads the
    /// `method` string generically; the tag comes from `rename_all`.
    Invite(InviteParams),
    /// Redeem a pairing invite. The daemon dials the inviter named by
    /// `invite_line` on `mcpmesh/pair/1`, proves the secret, writes the mutual
    /// (dial-back) `PeerEntry`, and answers a [`PairResult`]. Tag `"pair"`
    /// (snake_case); `method_of` reads the `method` string generically.
    ///
    /// `PeerEntry` — the durable allowlist row — lives in the daemon crate.
    Pair(PairParams),
    /// Remove a paired peer by nickname (`mcpmesh pair --remove`). The daemon drops the
    /// peer's `PeerEntry` (identity) AND revokes its access by stripping its stable principals from every
    /// `[services.*].allow` (authorization) — the inverse of the pairing grant. Idempotent: a
    /// nickname with no entry / no allow membership is a clean no-op. Live in-flight sessions are
    /// NOT severed here: existing sessions run to completion; the peer only loses the
    /// ability to establish NEW authorized sessions. Tag `"peer_remove"` (snake_case);
    /// `method_of` reads the `method` string generically (no per-variant arm).
    ///
    /// `PeerEntry` — the durable allowlist row — lives in the daemon crate.
    PeerRemove(PeerRemoveParams),
    /// Rename a contact's nickname (nickname) authoritatively. Renames the
    /// PERSON — every `PeerEntry` sharing `user_id` when given (one op for all their devices), else the
    /// single `nickname` entry (a provisional, no-`user_id` contact) — to `to`, AND rewrites the old
    /// nickname → `to` in every `[services.*].allow` so grants follow the rename. Refuses (error frame)
    /// when `to` is empty or already names/grants a DIFFERENT identity — the same collision guard the
    /// pairing rendezvous uses, so a rename can't inherit another peer's access. Tag `"peer_rename"`;
    /// host-privileged like the other pair ops.
    PeerRename(PeerRenameParams),
    /// Mint an attestation OFFER (#85 ask 3): a `mcpmesh-attest:` line telling another device of a
    /// person you already pair with where to dial. Tag `"attest_offer"`.
    ///
    /// **Carries nothing secret** — the offering node's id and address, both of which an invite
    /// line already carries in the clear. It exists because a device freshly restored from a
    /// recovery phrase holds no rows and so has no way to find anyone.
    ///
    /// Refused unless this node has `[identity].admit_attested_devices` on: an offer it would not
    /// honour is worse than no offer.
    AttestOffer,
    /// Present THIS device's identity to a peer, using their `mcpmesh-attest:` line (#85 ask 3).
    /// Tag `"attest_to"`.
    ///
    /// The recovery path's second half: `user_key_import` restores the `b64u:` your peers pinned,
    /// and this is what gets the machine holding it ADMITTED. Requires a user key — with none there
    /// is nothing to attest.
    ///
    /// The peer admits this device only if it already holds a row for this person, has opted in,
    /// and has revoked neither this endpoint nor the IDENTITY. It cannot admit a stranger.
    AttestTo(AttestToParams),
    /// REVOKE an endpoint locally: "I no longer trust this device" (#85 ask 4). Tag
    /// `"peer_revoke"`.
    ///
    /// **Not the same act as [`PeerRemove`](Self::PeerRemove), deliberately.** Removal is routine —
    /// we are not working together any more — and re-pairing afterwards is normal. Revocation is a
    /// claim about COMPROMISE, and it outlives the pair row: a revoked endpoint is refused whether
    /// or not it is in the allowlist, so it cannot be undone by a fresh pairing. Making them one
    /// verb would either make removal irreversible or make revocation weak.
    ///
    /// Takes effect IMMEDIATELY, like `service_allow_revoke` (#54): live sessions are severed
    /// rather than left to end on their own, which for long-lived MCP sessions is unbounded.
    ///
    /// A `b64u:` principal revokes EVERY endpoint this node associates with that person — the "not
    /// on any of their devices" case, which an operator will otherwise get wrong doing it device by
    /// device.
    ///
    /// Reversible with [`PeerUnrevoke`](Self::PeerUnrevoke): this list is LOCAL and not
    /// authoritative for anyone else, so an operator mistake has to be fixable. Both directions are
    /// audited.
    PeerRevoke(PeerRevokeParams),
    /// Lift a local revocation (#85 ask 4). Tag `"peer_unrevoke"`. Idempotent.
    ///
    /// Restores the peer only if its pair row still exists — revocation never deleted it. A
    /// revocation applied from a SIGNED statement can be lifted too: the signature proved who asked,
    /// not that this node must obey forever.
    PeerUnrevoke(PeerUnrevokeParams),
    /// Sign a revocation of one of THIS node's own endpoints with THIS node's user key (#85 ask 4),
    /// producing a portable token. Tag `"device_revoke"`.
    ///
    /// The direction that matters, and the one local revocation cannot express: my laptop was
    /// stolen, and my peers cannot discover that by themselves. I have to tell them, and they have
    /// to be able to verify it was me.
    ///
    /// Requires a user key — a node with none has no authority to speak for its person.
    /// **Distribution is out of band**: pairing mode has no gossip, so getting the token to your
    /// peers is the same problem as getting them an invite line, and has the same answer.
    DeviceRevoke(DeviceRevokeParams),
    /// Apply a signed device revocation from a peer (#85 ask 4). Tag `"device_revocation_import"`.
    ///
    /// Refused unless this node ALREADY trusts the signing `user_id` (holds a paired entry carrying
    /// it) **and** the revoked endpoint is one this node associates with that same person, or is
    /// entirely unknown to it. That bound is what keeps this from being an "mark any endpoint dead"
    /// primitive: a peer may kill their OWN devices in your node, and nothing else.
    DeviceRevocationImport(DeviceRevocationImportParams),
    /// RESERVED / INTERNAL (`docs/local-protocol.md` "Reserved / internal methods"): install a
    /// peer directly from a raw `endpoint_id` — the trust-population stand-in for pairing behind
    /// `mcpmesh internal peer add`. A deliberate, documented exception to the surface discipline
    /// (raw endpoint identifiers otherwise never cross this socket); NOT part of the stable
    /// vocabulary — do not build on it. Tag `"peer_add"`.
    PeerAdd(PeerAddParams),
    /// Install a peer from a SIGNED endorsement by someone you are already paired with (#65) —
    /// O(N) onboarding for a small group, without a fresh two-human SAS ceremony per pair.
    ///
    /// **It installs IDENTITY, never AUTHORIZATION.** The subject becomes resolvable; it is granted
    /// nothing. Service access stays principal-keyed in config (#38) and an explicit, separate act.
    /// That is what bounds the feature: a compromised endorser can make you KNOW about an attacker,
    /// it cannot make you SERVE one.
    ///
    /// Unlike [`PeerAdd`](Self::PeerAdd) — which is reserved precisely because the caller merely
    /// ASSERTS an id — this is verifiable: the endorsement is checked against a user key you
    /// already hold from pairing with the endorser. Tag `"peer_introduce"`.
    PeerIntroduce(PeerIntroduceParams),
    /// PRODUCE an endorsement of a peer, for someone else to redeem with
    /// [`PeerIntroduce`](Self::PeerIntroduce) (#65). The other half of an introduction: without it
    /// nothing can generate `evidence`, and the install half is unusable.
    ///
    /// Signs with THIS node's user key, so the result is only meaningful to someone who has paired
    /// with you. Endorsing does not change your own trust in the subject. Tag `"peer_endorse"`.
    PeerEndorse(PeerEndorseParams),
    /// Open a mesh session to `peer/service`; the daemon dials and pipes.
    /// Distinct from the proxy's job: this returns a session the client streams.
    /// Named `open_session` rather than `connect` to avoid colliding
    /// with the `connect` porcelain.
    OpenSession(OpenSessionParams),
    /// Install a signed roster from a local file (the manual `internal roster install` path).
    /// `path` is a LOCAL file the same-uid daemon reads (the daemon runs as the caller's own
    /// uid, so passing a path rather than the bytes crosses no trust boundary). `org_root_pk`
    /// pins the org root on FIRST install (`b64u:`); omit it
    /// once pinned (config carries it). Tag `"roster_install"`.
    RosterInstall(RosterInstallParams),
    /// Read the installed roster's MEMBERSHIP: the declared groups, and every person with their
    /// display name, groups, and devices (#93). Parameterless. Tag `"roster_members"`.
    ///
    /// The read half of roster mode. `status` reports that a roster exists (`RosterStatus`) and who
    /// is currently online (`PresencePeer`); neither answered "who is in this org" — a person with
    /// no live device appeared nowhere at all, so an embedder could not draw a member list, and
    /// the only route to one was hand-parsing the daemon-owned `roster.json`.
    ///
    /// ADVISORY and display-oriented, like `status`: the gate reads the roster document, never
    /// this. Empty in a pure-pairing daemon or before the first roster is installed.
    RosterMembers,
    /// AUTHOR an org: mint this node's org root key, sign an empty roster (serial 1), install it
    /// (which pins the root), and return the copyable org invite (#66). Tag `"org_create"`.
    ///
    /// One-time per node — a second call is refused rather than replacing the key, because
    /// replacing it would orphan every roster it has signed.
    OrgCreate(OrgCreateParams),
    /// APPROVE a join code into the roster: verify its device→user-key binding, upsert the member
    /// with the given groups, bump the serial, re-sign, install (#66). Tag `"org_approve"`.
    ///
    /// The cryptographic half of the ceremony. Verifying that the code came from the PERSON you
    /// think it did stays an out-of-band human step, and `join_code_fingerprint` in the result is
    /// what the two humans compare.
    OrgApprove(OrgApproveParams),
    /// ROTATE the org root (#93 ask c). Tag `"org_rotate"`.
    ///
    /// The org's trust anchor is one pinned key, and nothing could move it: an operator laptop that
    /// died took the org with it 90 days later, when the roster expired — and the delay is what
    /// made that hard to diagnose. Recovery was O(N) fresh ceremonies with every member.
    ///
    /// Publishes a roster signed by the SUCCESSOR carrying a cross-signature by the CURRENT root, so
    /// a member still pinned to the current key adopts the successor with the key it already has.
    /// The bridge rides every subsequent roster, so a member offline for one publication catches up
    /// — but one two rotations behind needs a fresh `org_join`.
    ///
    /// **This is not escrow.** If the current root key is LOST there is nothing to sign the bridge
    /// with. Copying `org-root.key` to a second operator machine already works and remains the
    /// answer for that.
    OrgRotate(OrgRotateParams),
    /// INSPECT a join code without approving it (#66): who it claims to be, and the fingerprint the
    /// two humans compare. Read-only — nothing is signed, installed, or persisted.
    /// Tag `"org_join_code"`.
    ///
    /// This is what makes an "approve this person" button correct rather than merely possible. The
    /// fingerprint has to be shown and confirmed out-of-band BEFORE the approval, because a
    /// substituted code is caught there or not at all — and reading it off `OrgApprove`'s result
    /// is too late, the member is already in the signed roster. The CLI always had this (it
    /// decoded the code locally); an embedder could not, since the join-code format lives in
    /// `mcpmesh-node` and not on this seam.
    OrgJoinCode(OrgJoinCodeParams),
    /// REVOKE from the roster: remove a person, one device, or a person's user key, then bump,
    /// re-sign, and install — which severs the cut devices' live sessions (#66).
    /// Tag `"org_revoke"`.
    OrgRevoke(OrgRevokeParams),
    /// EXPORT this node's user key as a recovery phrase (#85 ask 2). Parameterless.
    /// Tag `"user_key_export"`.
    ///
    /// **The phrase IS the private key**, in a form a human can write down. Anyone who reads it can
    /// present this identity. It is deliberately not logged, not audited, and not echoed anywhere
    /// but this response.
    UserKeyExport,
    /// IMPORT a user key from a recovery phrase (#85 ask 2), so a person's `b64u:` survives the
    /// hardware. Tag `"user_key_import"`.
    UserKeyImport(UserKeyImportParams),
    /// DETACH this device from an identity it was ENROLLED into (#214) — the inverse of the
    /// adoption a `pair { allow_self_enroll }` performs (#86). Parameterless. Tag
    /// `"self_enroll_detach"`.
    ///
    /// Drops the adopted device→user binding, live and on disk, so this node goes back to
    /// presenting its own identity (imported, else boot-derived). The exit that self-enrollment
    /// otherwise lacks: an
    /// enrolled device holds no user key, so `user_key_import` — the only other verb that clears
    /// the slot — is not available to it, and a person handed a substituted `mcpmesh-enroll:` line
    /// was permanently a device of a stranger's identity.
    ///
    /// **Local only.** It does not reach any peer: whoever already learned this endpoint as that
    /// person's device still believes it. Telling THEM is `device_revoke`, issued from the device
    /// that holds the key. Refused with [`ERR_NOT_ENROLLED`] when nothing is adopted.
    SelfEnrollDetach,
    /// Pin the org root on a JOINER — WITHOUT a roster (the joiner has none yet; its poll loop
    /// fetches the first one). Records `[identity]` org_id / org_root_pk / user_id / user_key.
    /// `user_key` is a LOCAL path
    /// (the key never crosses the API). Tag `"org_join"`.
    OrgJoin(OrgJoinParams),
    /// Pin the HTTPS roster URL (`[roster].url`) in config. Written by `org create
    /// --roster-url` (the operator keeps it current) AND by `join` when the org invite carries one —
    /// so the joiner's poll loop bootstraps its FIRST roster. The daemon writes it under
    /// `reload_lock` (single-writer), then the poll loop picks it up on the next daemon start. Tag
    /// `"set_roster_url"`.
    SetRosterUrl(SetRosterUrlParams),
    /// Rename this node LIVE (#37): validate + upsert `[identity].nickname` through the
    /// daemon's own serialized config-write path (no lost-update window against a
    /// concurrent grant/registration) and update the in-memory name future invites
    /// present — no restart. Ack result. Tag `"set_nickname"` (snake_case).
    SetNickname(SetNicknameParams),
    /// Set this node's opaque app-metadata blob (#39): validated (≤256B) and folded, signed,
    /// into each outgoing presence heartbeat, so paired roster peers see it in their `status`
    /// presence — no per-peer session. Ack result. Tag `"set_app_metadata"`. In-memory (lost
    /// on restart; the embedder re-sets on startup).
    SetAppMetadata(SetAppMetadataParams),
    /// Set this node's CUSTOM relay set LIVE (#53): validate each URL as an iroh `RelayUrl`, diff
    /// against the running endpoint's current custom relays and apply the delta via iroh 1.0.3
    /// `Endpoint::insert_relay`/`remove_relay` (no endpoint rebuild, no dropped sessions), then
    /// persist `[network] relay_mode="custom" relay_urls=[…]` under `reload_lock`. When the node
    /// is currently `default`/`disabled`, the config is persisted but the live mode transition
    /// isn't possible — [`SetRelaysResult::restart_required`] is `true`. Answers a
    /// [`SetRelaysResult`]. Tag `"set_relays"`.
    SetRelays(SetRelaysParams),
    /// Grant a single stable principal access to a single service's allow (#44) — the per-peer
    /// "sharing on" toggle, idempotent + serialized under the config lock. Ack result.
    /// Remove a service registration (#50) — the deregistration mirror of `register_service`.
    /// Removes the whole `[services.<name>]` entry (allow included) + any ephemeral one, then
    /// hot-reloads. Idempotent. Ack result.
    UnregisterService(UnregisterServiceParams),
    /// Discover which services a paired peer CURRENTLY grants the caller (#52) — dials the peer
    /// and returns the service names whose allow admits the caller's principal. Answers
    /// [`PeerServicesResult`].
    PeerServices(PeerServicesParams),
    /// Dump the DURABLE per-peer state this node carries for one peer (#140) — the persisted dial
    /// hint, the pairing stamp, and the live reachability row, in one capture. A DIAGNOSTIC verb:
    /// unlike every other surface it carries transport vocabulary on purpose. Answers with
    /// [`PeerDiagnosticsResult`]. `api_minor >= 33`.
    PeerDiagnostics(PeerDiagnosticsParams),
    /// FORGET the persisted dial hint for one peer (#140) — `api_minor >= 59`.
    ///
    /// An experiment tool, and a workaround. `PeerEntry.last_addr` is the only durable per-peer
    /// state on this node's disk that the dial path reads, and it is the ONLY thing a long-lived
    /// pairing carries that a freshly paired identity does not. Nothing invalidates it: a
    /// relay-only connection deliberately declines to overwrite it (persisting a relay URL over a
    /// direct candidate was #124's own bug), and "learned nothing" means leave alone. So a hint
    /// written before a network change can persist indefinitely on a pair whose every connection
    /// since has been relayed.
    ///
    /// Clearing it makes an existing pairing **addressing-equivalent to a fresh identity**, which
    /// is exactly the difference #140 is about — a pair that cannot punch as a long-lived pairing
    /// while punching in 23ms with fresh identities on the same hardware.
    ///
    /// **Advisory, never authorization.** A hint is addressing; its absence is a supported state
    /// (the dial degrades to id-only, which is what every peer does before its first refresh). The
    /// worst case is one slower dial while discovery runs. The peer row, its `user_id`, its
    /// services and its pairing stamp are untouched.
    ///
    /// Nothing clears a hint automatically — choosing an invalidation policy before the data exists
    /// is what produced a fix that helped one peer and not this one. Answers
    /// [`PeerHintClearResult`].
    PeerHintClear(PeerHintClearParams),
    ServiceAllowGrant(ServiceAllowParams),
    /// Revoke a single stable principal from a single service's allow (#44) — "sharing off"
    /// WITHOUT unpairing (the peer's identity row is untouched; only NEW sessions are refused).
    /// Idempotent. Ack result.
    ServiceAllowRevoke(ServiceAllowParams),
    /// Publish a LOCAL file INTO a scope: the daemon adds the bytes to its gated
    /// app-blob store and records the hash in `scope`. `path` is a local file the same-uid daemon
    /// reads. Answers a [`BlobPublishResult`] carrying the `mcpmesh/blob/1` ticket + hash.
    /// Tag `"blob_publish"`.
    BlobPublish(BlobPublishParams),
    /// Grant a scope to a principal — any flat-namespace entry: a group name, a user_id, or a
    /// nickname (the shared `principal_set` expansion). Tag
    /// `"blob_grant"`.
    BlobGrant(BlobGrantParams),
    /// Tag `"blob_revoke"`: withdraw principals from ONE scope's grants (#62).
    BlobRevoke(BlobRevokeParams),
    /// Tag `"blob_unpublish"`: remove a hash from ONE scope (#62). Withdraws reachability, not
    /// bytes.
    BlobUnpublish(BlobUnpublishParams),
    /// #83: make a blob this daemon already holds servable from HERE, in a scope it controls.
    /// Answers a [`BlobPublishResult`] — same shape as `blob_publish`, so a client can treat the
    /// two interchangeably after a fetch.
    BlobRepublish(BlobRepublishParams),
    /// List the daemon's blob scopes (name → hashes + grants). Tag `"blob_list"`.
    BlobList(BlobListParams),
    /// Fetch a `mcpmesh/blob/1` ticket THROUGH the daemon (BLAKE3-verified streaming) and export the
    /// verified blob to `dest_path` (a local file the same-uid daemon writes). Answers a
    /// [`BlobFetchResult`] with the verified hash + byte length. Tag `"blob_fetch"`.
    BlobFetch(BlobFetchParams),
    /// Cancel every in-flight [`BlobFetch`](Self::BlobFetch) of one hash (#172). Answers a
    /// [`BlobFetchCancelResult`]; the cancelled fetches themselves answer [`ERR_CANCELLED`].
    /// Tag `"blob_fetch_cancel"`.
    BlobFetchCancel(BlobFetchCancelParams),
    /// Summarize this node's LOCAL audit log into per-peer / per-service SESSION counts
    /// (local-only — the daemon reads its OWN audit dir, nothing is transmitted). The host Mesh surface
    /// renders these as "who serves me / whom I serve / session counts". Parameterless (like `Status`);
    /// the server dispatches on the `method` string. Tag `"audit_summary"` (snake_case);
    /// `method_of` reads the `method` string generically (no per-variant arm).
    AuditSummary,
    /// Delete audit months strictly older than `before` (#88) — the retention lever the log
    /// never had. Local-only and owner-only (the control socket is the daemon owner's). Answers
    /// [`AuditPruneResult`]. Tag `"audit_prune"`.
    AuditPrune(AuditPruneParams),
    /// Read this node's LOCAL audit records, filtered and paged (#88) — the "show me everything
    /// you hold about me" verb. Local-only; nothing is transmitted. Answers
    /// [`AuditListResult`]. Tag `"audit_list"`.
    AuditList(AuditListParams),
    /// Open a live event stream (pairing liveness & health telemetry). Like `open_session`, the
    /// connection STOPS being request/response after this call and becomes a one-way push stream
    /// of `StreamFrame`s. Parameterless. Tag `"subscribe"`.
    Subscribe,
}

/// Result of [`Request::OrgJoin`] — the pinned org id echoed back (surface-clean; the fingerprint is
/// computed porcelain-side from the invite's org_root_pk). Additive-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgJoinResult {
    pub org_id: String,
    /// `true` when the org root was pinned but this node's ROSTER TRANSPORT is not running, so the
    /// join is only half in effect until the daemon restarts (#93). `api_minor >= 46`.
    ///
    /// Roster mode is decided at BOOT: it fixes the ALPN set bound on the endpoint and whether
    /// gossip, presence and app-blobs are constructed at all. The roster GATE hot-swaps live. So a
    /// node that booted in pairing mode and then joins an org reaches a state where MCP sessions to
    /// org members work as soon as a roster arrives, while `status.presence` stays permanently
    /// empty and every blob verb hard-closes with `blobs not enabled` — succeeding partially, with
    /// no error anywhere, which a caller previously had no way to detect.
    ///
    /// **What to do with it:** if `true`, tell the user the join succeeded and the node must
    /// restart before presence and file sharing work. Do not treat it as a failure — nothing was
    /// left half-written; the pin is durable and the restart is sufficient.
    ///
    /// `false` when the transport is already composed (the node booted with an org root pinned), or
    /// on a re-join of an org this node is already in.
    ///
    /// Same shape as [`SetRelaysResult::restart_required`] (#53), for the same reason. Additive:
    /// absent from an older daemon's payload and reads as `false` — which was that daemon's
    /// implicit, and wrong, answer.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restart_required: bool,
}

/// Result of a [`Request::RosterInstall`] request (the manual install path): the installed roster's
/// org id + serial (roster-status vocabulary the confirmation line is permitted to render) plus how
/// many live sessions the install severed. Surface-clean: NO keys / EndpointIds / paths.
///
/// Additive-only: any future field MUST land as
/// `#[serde(default, skip_serializing_if = ...)]` so older payloads still deserialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterInstallResult {
    pub org_id: String,
    pub serial: u64,
    /// How many live sessions were severed, for the porcelain's confirmation line.
    #[serde(default)]
    pub severed: u32,
}

/// Params of [`Request::OrgCreate`] (#66).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgCreateParams {
    /// The org's name — its `org_id`, the value every member's roster carries.
    pub name: String,
    /// How long the signed roster stays valid, in seconds. Omit for the 90-day default.
    ///
    /// This is an operator-grade default and a sharp edge at small scale: past it the roster
    /// degrades and the group stops working, which for a handful of laptops can arrive days after
    /// one of them was closed for a long weekend. A small team should pass a long value here
    /// deliberately rather than discover the default later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_secs: Option<i64>,
    /// An HTTPS URL where the signed roster will be published. Carried in the org invite (so a
    /// joiner bootstraps its first roster) AND pinned in this operator's `[roster].url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster_url: Option<String>,
}

/// Result of [`Request::OrgCreate`] (#66).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgCreateResult {
    pub org_id: String,
    pub serial: u64,
    /// The copyable `mcpmesh-org:` invite to hand a joiner — one of the two permitted opaque
    /// artifacts on this surface, the same carve-out the pairing invite line takes.
    pub org_invite: String,
    /// The org root's fingerprint in short words, for the out-of-band read-back that anchors every
    /// joiner's trust. NOT the key.
    pub org_root_fingerprint: String,
}

/// Params of [`Request::OrgRotate`] (#93 ask c).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgRotateParams {
    /// Where to read or write the successor key. Omit for `<config>/org_root_next.key`.
    ///
    /// Reused if it already exists, so a rotation can be prepared on a machine that is not the one
    /// publishing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_key_path: Option<String>,
}

/// Result of [`Request::OrgRotate`] (#93 ask c).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OrgRotateResult {
    pub org_id: String,
    pub serial: u64,
    /// The new anchor, `b64u:`. Members adopt it as they receive the roster.
    pub new_root_pk: String,
    /// Short fingerprints, for an operator to read out when telling members what changed.
    pub old_root_fingerprint: String,
    pub new_root_fingerprint: String,
}

/// Params of [`Request::OrgApprove`] (#66).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgApproveParams {
    /// The `mcpmesh-join:` code the joiner sent.
    pub join_code: String,
    /// The groups to grant. Each must be DECLARED in the roster (see
    /// [`RosterMembersResult::groups`]) — an undeclared one is refused, because it would make an
    /// `allow` entry naming it ambiguous.
    #[serde(default)]
    pub groups: Vec<String>,
    /// Override the `user_id` the joiner requested. Omit to accept theirs.
    ///
    /// Worth using: the requested id is chosen by the person being approved, and it is the string
    /// every `allow` entry will name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

/// Result of [`Request::OrgApprove`] (#66).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgApproveResult {
    pub user_id: String,
    pub groups: Vec<String>,
    pub org_id: String,
    pub serial: u64,
    /// The join code's fingerprint in short words — the enrollment analogue of the pairing SAS.
    ///
    /// **Show this to the operator and have them confirm it out-of-band before trusting the
    /// approval.** Nothing else binds the person to the `user_pk` in the code, so a substituted
    /// code is caught here or not at all. Returned rather than checked, because only the human can
    /// check it.
    pub join_code_fingerprint: String,
}

/// Params of [`Request::UserKeyImport`] (#85 ask 2).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserKeyImportParams {
    /// The recovery phrase, as written down. Whitespace and case are forgiven; a wrong word, the
    /// wrong count, or a failed checksum are refused by position rather than guessed at.
    pub recovery_phrase: String,
    /// Replace an EXISTING user key. Defaults to `false`, and the refusal is the point: importing
    /// over a live key discards the identity this node currently presents, which is irreversible
    /// without that key's own phrase.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
}

/// Result of [`Request::UserKeyExport`] (#85 ask 2).
///
/// **`recovery_phrase` is the private key.** Show it to the person who owns it, once, and do not
/// persist it anywhere your application would not persist the key file itself.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserKeyExportResult {
    /// 33 words. Write them down in order.
    pub recovery_phrase: String,
    /// The `b64u:` identity this phrase restores — safe to display and to record, unlike the
    /// phrase. Compare it after an import to confirm the right identity came back.
    pub user_id: String,
}

/// REDACTING `Debug` — the phrase is a private key, and a derived one would put it in any
/// `tracing::debug!(?params)` a future change adds to the dispatch, or in an embedder's `dbg!`.
/// Three lines to make that leak unrepresentable rather than merely absent today.
impl std::fmt::Debug for UserKeyImportParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserKeyImportParams")
            .field("recovery_phrase", &"<redacted>")
            .field("replace", &self.replace)
            .finish()
    }
}

/// REDACTING `Debug` — see [`UserKeyImportParams`]. The `user_id` is safe and is kept, because a
/// `{:?}` with nothing in it is worse for debugging than one with the non-secret half.
impl std::fmt::Debug for UserKeyExportResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserKeyExportResult")
            .field("recovery_phrase", &"<redacted>")
            .field("user_id", &self.user_id)
            .finish()
    }
}

/// Result of [`Request::SelfEnrollDetach`] (#214).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfEnrollDetachResult {
    /// The `b64u:` identity now in effect — this node's own (imported, else boot-derived). What a
    /// UI shows to confirm the exit took. `None` only when this node has no user key of its own: boot mints
    /// one, so that is a boot that could NOT load or mint it (logged as a warning there) — the
    /// same condition under which `status.self_user_id` is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// The `b64u:` identity this device has STOPPED presenting — the one it was enrolled into.
    pub detached_from: String,
}

/// Result of [`Request::UserKeyImport`] (#85 ask 2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserKeyImportResult {
    /// The `b64u:` identity now in effect. **Compare it against the one you are recovering** — the
    /// phrase's checksum catches most transcription errors, but a `user_id` that does not match is
    /// the definitive answer, and the only one that distinguishes "restored the wrong key" from
    /// "peers have not seen me yet".
    pub user_id: String,
    /// `true` when this discarded a REAL identity — a user key the node had loaded from disk, or
    /// one an earlier import in this daemon's lifetime wrote (#221), rather than one it minted at
    /// this boot and had never presented to anyone.
    ///
    /// The distinction is the useful one: a fresh node always has a key on disk before an import
    /// can run (its own boot mints one), so "a file existed" would be `true` for every recovery on
    /// new hardware and would have a UI warn that something was destroyed when nothing was.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replaced: bool,
}

/// Params of [`Request::OrgJoinCode`] (#66).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgJoinCodeParams {
    /// The `mcpmesh-join:` code to inspect.
    pub join_code: String,
}

/// Result of [`Request::OrgJoinCode`] (#66): what a join code claims, plus the fingerprint that
/// decides whether to believe it.
///
/// The claims are ATTACKER-CONTROLLED — they come out of a code someone handed you. `display_name`
/// and `requested_user_id` are what the sender asked for, not facts. What is verified is the
/// device→user-key binding (a bad one is refused rather than reported), and what is *checkable* is
/// `join_code_fingerprint`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgJoinCodeResult {
    /// The human name the code carries. Sender-chosen — render it, do not trust it.
    pub display_name: String,
    /// The `user_id` the sender is asking for. Sender-chosen, and it is what every `allow` entry
    /// would name, so it is worth an operator's attention before approving.
    pub requested_user_id: String,
    /// The label of the device being enrolled. Sender-chosen.
    pub device_label: String,
    /// The fingerprint in short words — the enrollment analogue of the pairing SAS.
    ///
    /// **Show this and have the operator confirm it out-of-band before calling
    /// [`Request::OrgApprove`].** Nothing in a join code binds it to a person; a substituted code
    /// carries a different `user_pk` and so diverges here. This is the entire check.
    pub join_code_fingerprint: String,
}

/// Params of [`Request::OrgRevoke`] (#66).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgRevokeParams {
    /// Who or what to revoke: a `user_id` (the person and every device), or `"<user_id>/<label>"`
    /// (one device).
    pub target: String,
    /// Treat this as a USER-KEY rotation instead: remove the person but leave their devices
    /// un-revoked, so the same hardware re-enrolls under a fresh user key and is re-approved with
    /// the same `user_id`.
    ///
    /// The distinction is the point — a departure must revoke the devices, a rotation must not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub user_key: bool,
}

/// Result of [`Request::OrgRevoke`] (#66).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgRevokeResult {
    pub target: String,
    /// `"person"` | `"device"` | `"user-key-rotation"` — which grammar the target resolved to, so a
    /// caller can confirm the destructive reading it got was the one it meant.
    pub mode: String,
    pub org_id: String,
    pub serial: u64,
    /// Live sessions severed by the install. Revocation is IMMEDIATE (#54): a cut device's existing
    /// connections are closed, not left to drain.
    #[serde(default)]
    pub severed: u32,
}

/// Result of [`Request::RosterMembers`]: the org's membership as an embedder renders it (#93).
///
/// Distinct from `status.presence` in what it enumerates: that lists reachable DEVICES and omits a
/// person entirely when none of their devices is up. This lists every person the roster carries,
/// online or not — a member list, not a presence list — with `online` per device so a UI can draw
/// both from one read.
///
/// ADVISORY. Every field here is display or authoring input; nothing in it is an authorization
/// answer. The gate reads the signed roster.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterMembersResult {
    /// The org's declared group namespace, in document order — the set an `allow` entry may name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// Every person in the roster, ordered by `user_id` for a stable display.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub users: Vec<RosterMember>,
}

/// One person in a [`RosterMembersResult`] (#93).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterMember {
    /// The stable authorization handle — what an `allow` entry names.
    pub user_id: String,
    /// The human name, for display. Empty if the roster's own field is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub display_name: String,
    /// The groups this person belongs to, each declared in [`RosterMembersResult::groups`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// Their ACTIVE devices — revoked ones are absent, exactly as the gate sees it. Ordered
    /// primary-before-mirror, then by label.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<RosterMemberDevice>,
}

/// One device of a [`RosterMember`] (#93).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterMemberDevice {
    /// The device's human label.
    pub label: String,
    /// `"primary"` | `"mirror"` — the advisory dial-ordering hint, never a security property.
    pub role: String,
    /// The device's stable `eid:` principal — the SAME vocabulary [`PeerInfo::principal`] carries,
    /// and what a per-device `allow` entry names.
    ///
    /// Included where `PresencePeer` deliberately omits it, because this surface exists to be
    /// ACTED on: an embedder granting or revoking one device of a person needs the handle to name
    /// it, and the alternative is a nickname that does not exist in roster mode.
    pub principal: String,
    /// Whether the device has a live presence heartbeat. Advisory — absence never blocks a dial,
    /// and never removes a dial candidate.
    pub online: bool,
}

/// Result of [`Request::BlobPublish`]: the copyable `mcpmesh/blob/1` ticket + the blob's blake3 hash.
/// A ticket/hash here is blob-reference vocabulary (NOT a transport-vocab leak — the same
/// carve-out as the pairing invite line). Additive-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobPublishResult {
    pub ticket: String,
    pub hash: String, // bare blake3 hex
}

/// One scope in a [`BlobScopeList`]: its name + the hashes it contains + the principals it
/// grants. Flat vocabulary ONLY — no EndpointId/pubkey/ALPN. Additive-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeInfo {
    pub name: String,
    pub hashes: Vec<String>,
    pub grants: Vec<String>,
    /// Hashes deliberately WITHDRAWN from this scope (#107): `blob_unpublish` was called, and
    /// `blob_republish` of these into THIS scope is refused with [`ERR_BLOB_WITHDRAWN`]. Cleared
    /// only by a deliberate `blob_publish {scope, path}`. Additive — omitted when empty, so a
    /// pre-`api_minor` 19 client sees exactly what it saw before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub withdrawn: Vec<String>,
    /// Size of `hashes` — always present, even when `counts_only` empties the vector (#84b).
    #[serde(default)]
    pub hash_count: usize,
    /// Size of `grants`.
    #[serde(default)]
    pub grant_count: usize,
    /// Size of `withdrawn`.
    #[serde(default)]
    pub withdrawn_count: usize,
}

/// Params of [`Request::BlobList`] (#84b). ALL optional — `blob_list {}` still works, which
/// matters because the verb took no params before `api_minor` 20.
///
/// A DEFAULT LIMIT applies when `limit` is absent. Deliberate: unpaged, `blob_list` renders every
/// scope into one frame against the 16 MiB cap; past it the CLIENT rejects the frame as malformed.
/// The control surface carries no strike bound, so the connection survives — but the caller gets an
/// opaque failure with no way to page, which is unusable rather than merely large.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BlobListParams {
    /// EXACT scope name, never a prefix.
    pub scope: Option<String>,
    /// Only scopes containing this hash; the rendering you send is normalized first.
    pub hash: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    /// Omit `hashes`/`grants`/`withdrawn`, keep the counts.
    pub counts_only: bool,
}

/// Result of [`Request::BlobList`]: the daemon's scopes. Additive-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobScopeList {
    pub scopes: Vec<ScopeInfo>,
    /// Scopes matching the filter BEFORE `limit`/`offset` (#84b). Without this you cannot tell a
    /// complete answer from a clipped one.
    #[serde(default)]
    pub total: usize,
    /// True when more scopes matched than were returned. Page with `offset` to see the rest.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Result of [`Request::BlobFetch`]: the verified hash + byte length written to `dest_path`.
/// Additive-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobFetchResult {
    pub hash: String,
    pub bytes_len: u64,
}

/// Result of [`Request::BlobFetchCancel`] (#172).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobFetchCancelResult {
    /// True when a fetch of that hash was in flight and has been told to stop. False is NOT an
    /// error — it means nothing was fetching that blob here, which is also what a caller sees when
    /// it races a fetch that just finished.
    pub cancelled: bool,
}

/// Params of [`Request::AuditPrune`] (#88): delete monthly audit files STRICTLY older than
/// `before` (that month itself is kept — delete-before, not delete-including). Rejects unknown
/// fields, and the daemon validates the `YYYY-MM` shape up front: a malformed month errors
/// loudly instead of string-comparing to nothing and reporting a clean no-op.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditPruneParams {
    /// A zero-padded `YYYY-MM` month key.
    pub before: String,
}

/// Result of [`Request::AuditPrune`]: the month keys actually deleted, ascending. Empty when
/// nothing was older than `before` (idempotent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditPruneResult {
    pub deleted_months: Vec<String>,
}

/// Params of [`Request::AuditList`] (#88). All filters optional and AND-combined; every field
/// absent lists everything (paged). Rejects unknown fields — a typo'd filter that silently
/// matched everything would let a "what do you hold about X" answer overclaim.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditListParams {
    /// Inclusive `YYYY-MM` lower bound — month-file granularity (the rotation unit), so an
    /// out-of-range month is skipped without parsing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// Inclusive `YYYY-MM` upper bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    /// One of the wire kind strings (`session_open` / `session_close` / `request` /
    /// `blob_fetch` / `trust`). An UNKNOWN string is an error, never silently-all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The record's attributed peer nickname.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    /// Page size, default 500, clamped to 1000 — a month file can be arbitrarily large and the
    /// response is ONE JSON frame under the transport's frame cap, so the clamp is load-bearing
    /// (the same lesson as `blob_list`'s, minor 20).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Records to skip (after filtering), for paging.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

/// Result of [`Request::AuditList`]: one page of matching records in chronological order
/// (oldest month first, in-file order within a month), plus the TOTAL match count so a caller
/// can page without a second counting call. `total` counts ALL matches, not the page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditListResult {
    pub records: Vec<AuditRecord>,
    pub total: u64,
}

/// Result of [`Request::AuditSummary`]: LOCAL per-peer / per-service session counts
/// aggregated from this node's OWN audit log — NEVER transmitted (local-only). Surface-clean:
/// peer names are nicknames / user_ids (NEVER EndpointIds), service names are the registered
/// service names (NEVER transport vocabulary). A "session" is one `SessionOpen` record. `per_peer` /
/// `per_service` are sorted ascending by name (deterministic). Tuples mirror kb's
/// `InsightResponse::per_peer_contribution` — `["bob", 2]` on the wire.
///
/// Additive-only: any future field MUST land as
/// `#[serde(default, skip_serializing_if = ...)]` so older payloads still deserialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditSummaryResult {
    /// Sessions opened per peer (nickname). A session with no attributed peer is NOT counted here (no
    /// peer to attribute) but IS in `total_sessions`.
    pub per_peer: Vec<(String, u64)>,
    /// Sessions opened per registered service name.
    pub per_service: Vec<(String, u64)>,
    /// Total sessions opened (every `SessionOpen` record, including peer-less ones).
    #[serde(default)]
    pub total_sessions: u64,
}

/// Result of an [`Request::Invite`] request: the copyable `mcpmesh-invite:` artifact
/// (the ONE pairing artifact deliberately carved out of the
/// transport-vocabulary blocklist, so this is NOT a transport-vocab leak) plus its
/// absolute expiry in epoch seconds (≤ now + 24h).
///
/// `invite` returns BEFORE any redemption, so the SAS — which is derived from the redeemer's
/// endpoint id, unknown until they redeem — cannot appear here. The inviter reads its side of
/// the SAS from [`StatusResult::recent_pairings`] once a redemption completes (a `trust`/`pair`
/// frame on the live [`StreamFrame`] stream signals that moment). See the "embedding the pairing
/// ceremony" note in `docs/local-protocol.md` (#35).
///
/// Additive-only: any future field MUST land as `#[serde(default, skip_serializing_if = ...)]`
/// so older payloads still deserialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteResult {
    /// The `mcpmesh-invite:<base32>` line, copied out-of-band to the redeemer.
    pub invite_line: String,
    /// When the invite expires (epoch seconds); the daemon burns it at redemption or expiry.
    pub expires_at_epoch: u64,
    /// How many redemptions this invite has left (#87) — **the value actually applied**, after the
    /// [`MAX_INVITE_USES`] clamp. `1` for an ordinary single-use invite.
    ///
    /// Reported so a caller that asked for more than the cap is told what it got rather than
    /// discovering it when the fourth colleague fails. Additive: `#[serde(default = "one")]`, so a
    /// response from an older daemon reads as single-use. `api_minor >= 35`.
    #[serde(default = "one_use")]
    pub uses_remaining: u32,
}

/// The serde default for a `uses_remaining` field absent from an older payload or invite line: one
/// redemption, which is what every pre-#87 invite is.
pub fn one_use() -> u32 {
    1
}

/// Result of a [`Request::Pair`] request: the inviter's suggested nickname (the
/// redeemer's local name for the new peer) plus the display-only short authentication
/// code (SAS) — a few words the human reads aloud to a second channel to
/// catch a whole-invite forgery / address-swap MITM. The SAS is a pairing-ceremony
/// artifact (like the invite line), NOT a transport-vocabulary leak.
///
/// Additive-only: any future field MUST land as
/// `#[serde(default, skip_serializing_if = ...)]` so older payloads still deserialize.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairResult {
    /// The inviter's suggested nickname (from the invite) — the redeemer's local name for it.
    pub peer_nickname: String,
    /// The display-only short authentication code (e.g. `"tango-fig-42"`), shown on both
    /// sides for the out-of-band human check. Never sent on the wire, never checked
    /// programmatically.
    pub sas_code: String,
    /// TRUE when this redemption was a SELF-ENROLLMENT (#86): you are now another device of the
    /// inviter's person, not their peer. No peer row was written and nothing was granted.
    ///
    /// Reported so a caller can tell the two outcomes apart without inspecting its own store.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enrolled_as_self: bool,
    /// The services this pairing granted the redeemer — each mountable as `<peer>/<service>`.
    /// Populated from the invite (`invite.services`) by the redeemer-side `redeem_invite`, so
    /// the porcelain can print the "You can mount: alice/notes" line without re-decoding the
    /// invite. Additive: `#[serde(default, skip_serializing_if = ...)]` so a `PairResult`
    /// minted by an older daemon (which omits `services`) still deserializes — to an empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<String>,
    /// The opaque `app_label` the inviter attached at `invite` time (#31), echoed verbatim — or
    /// absent if none was set. mcpmesh never interprets it; the embedder does. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_label: Option<String>,
    /// The inviter's proven self-sovereign `user_id` (`b64u:<user_pk>`), when it presented a
    /// device→user binding at pairing (#30). This is the STABLE, portable identity the redeemer
    /// can align with its own — and the same value it may later pass to `open_session` to dial
    /// this peer by identity rather than by local nickname. `None` if the inviter presented no
    /// binding (a legacy/keyless peer). Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_user_id: Option<String>,
}

/// The event class of an [`AuditRecord`] (the four audit event classes). An additive discriminant on
/// top of the base record schema: it removes no field and makes the JSONL self-describing so
/// a consumer can filter by class without guessing from which optional fields are present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    /// A mesh session opened (a backend was selected for an authenticated peer).
    /// (A `session_open` with `status:"error"` is a synthesized FAILED-dial marker — no backend
    /// was reached; it records an attempted-and-failed reach for the telemetry stream.)
    SessionOpen,
    /// A mesh session closed (the backend returned / the session tore down).
    SessionClose,
    /// One proxied MCP request line (method + tool NAME + args_hash). NEVER carries raw arguments.
    Request,
    /// A peer fetched a blob from this node's gated provider (peer + hash + allow/deny).
    BlobFetch,
    /// A trust mutation (pair, unpair, roster install/swap, revoke).
    Trust,
}

/// One audit record — the union of the event classes, and the `record` payload of a
/// [`StreamFrame::Event`]. ONE schema for the on-disk JSONL log and the live stream. Every field
/// beyond `ts`/`kind` is optional and elided when absent (`skip_serializing_if`), so each class
/// serializes to just its relevant keys (a session record has no `method`; a trust record has no
/// `bytes_out`).
///
/// PRIVACY: the proxied-request record carries `method` + `tool` (NAME only) +
/// `args_hash` (`"blake3:<hex>"`), and NEVER the raw arguments, the request/response content, or
/// any tool-output bytes — only a `bytes_out` COUNT and a `status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// RFC3339 UTC with millisecond precision, e.g. `"2026-07-03T14:02:11.480Z"`. The `YYYY-MM`
    /// prefix also selects the monthly file (the rotation boundary), so it is always present.
    pub ts: String,
    pub kind: AuditKind,
    /// The gate-resolved authenticated peer (attributed by the endpoint_id-keyed trust gate). Absent on
    /// local-only events with no remote peer (a manual roster install).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// The tool NAME only (never its arguments or output) — e.g. `"read_file"` for a `tools/call`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// `"blake3:<hex>"` of the request arguments. The raw arguments are NEVER stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args_hash: Option<String>,
    /// Byte COUNT of the response sent back to the peer — a count, never the content.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_out: Option<u64>,
    /// `"ok"` / `"error"` (proxied request) or `"ok"` / `"denied"` (blob fetch).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Trust-event verb: `"pair"` / `"unpair"` / `"roster_install"` / `"revoke"` (kind == Trust).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// A reference, NEVER content: a blob hash (`BlobFetch`) or a trust-event target such as a
    /// nickname or `org/serial` (`Trust`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The subject's STABLE principal, from the same gate resolution that produced `peer`
    /// (#57, `api_minor >= 29`). `peer` is a display name and collides — two devices under one
    /// nickname were indistinguishable in the stream and the on-disk log. Same argument and
    /// shape as `PeerInfo` (#41), `PeerReachability` (#42), and `ActiveSession` (#73).
    ///
    /// TWO NAMESPACES, deliberately: session/request/blob records attribute the DEVICE
    /// (`eid:<hex>`, like `ActiveSession` — the exact authenticated endpoint), while the trust
    /// `pair` record carries the value the grant appended to the allow (`b64u:<pk>` when the
    /// device presented a user binding, else `eid:`, #38). Joining a bound peer's sessions to
    /// its allow entry therefore goes through the `status` peers list (which carries BOTH the
    /// device principal and the `user_id`), not string equality on this field alone.
    ///
    /// **`peer_introduce` (#65) is the one exception, deliberately:** it carries the ENDORSER, not
    /// the subject. An introduction's whole security question is *who vouched for this peer*, and
    /// the subject is already in `target`. So `audit_list --peer <endorser>` finds the
    /// introductions that endorser caused, which is the query an operator actually runs.
    ///
    /// Deliberately absent on: `unpair` (may tear down several devices — no single subject),
    /// `roster_install` (purely local), and the failed-outbound-dial session record (our own
    /// dial, not a gate-resolved caller). Absent on every record written before 0.24.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

impl AuditRecord {
    fn base(ts: String, kind: AuditKind) -> Self {
        Self {
            ts,
            kind,
            peer: None,
            service: None,
            method: None,
            tool: None,
            args_hash: None,
            bytes_out: None,
            status: None,
            latency_ms: None,
            event: None,
            target: None,
            principal: None,
        }
    }

    /// `principal` is an EXPLICIT parameter on every constructor (#57, kept from the original
    /// #72 design): a builder would let a call site silently omit it and reintroduce the
    /// collapsed-identity bug for that one event class. Pass `None` only for the documented
    /// no-single-subject records (see the field doc).
    pub fn session_open(
        ts: String,
        peer: Option<String>,
        service: String,
        principal: Option<String>,
    ) -> Self {
        let mut r = Self::base(ts, AuditKind::SessionOpen);
        r.peer = peer;
        r.service = Some(service);
        r.principal = principal;
        r
    }

    /// Set the record's `status` (`"ok"`/`"error"`/`"denied"`), returning `self` for chaining.
    /// Marks a synthesized failure record — e.g. the `session_open` for a FAILED dial, which
    /// reaches no backend and so is never audited by the far side's session guard — without a
    /// dedicated constructor. DRY: reuses the existing optional `status` field.
    pub fn with_status(mut self, status: &str) -> Self {
        self.status = Some(status.into());
        self
    }

    pub fn session_close(
        ts: String,
        peer: Option<String>,
        service: String,
        principal: Option<String>,
    ) -> Self {
        let mut r = Self::base(ts, AuditKind::SessionClose);
        r.peer = peer;
        r.service = Some(service);
        r.principal = principal;
        r
    }

    /// A completed (request→response correlated) proxied line: method + tool NAME + args_hash, plus
    /// the response's `bytes_out` COUNT, `status`, and `latency_ms`. PRIVACY: `args_hash` is a digest;
    /// no raw arguments, request/response content, or tool-output bytes are ever passed in.
    #[allow(clippy::too_many_arguments)]
    pub fn proxied_request(
        ts: String,
        peer: Option<String>,
        service: String,
        method: String,
        tool: Option<String>,
        args_hash: String,
        bytes_out: u64,
        status: String,
        latency_ms: u64,
        principal: Option<String>,
    ) -> Self {
        let mut r = Self::base(ts, AuditKind::Request);
        r.peer = peer;
        r.service = Some(service);
        r.method = Some(method);
        r.tool = tool;
        r.args_hash = Some(args_hash);
        r.bytes_out = Some(bytes_out);
        r.status = Some(status);
        r.latency_ms = Some(latency_ms);
        r.principal = principal;
        r
    }

    /// A proxied NOTIFICATION line (no `id`, so no response correlates): method + tool + args_hash,
    /// no `bytes_out`/`status`/`latency_ms`. The line is still recorded — every proxied request is audited.
    pub fn proxied_notification(
        ts: String,
        peer: Option<String>,
        service: String,
        method: String,
        tool: Option<String>,
        args_hash: String,
        principal: Option<String>,
    ) -> Self {
        let mut r = Self::base(ts, AuditKind::Request);
        r.peer = peer;
        r.service = Some(service);
        r.method = Some(method);
        r.tool = tool;
        r.args_hash = Some(args_hash);
        r.principal = principal;
        r
    }

    pub fn blob_fetch(
        ts: String,
        peer: Option<String>,
        hash: String,
        status: String,
        principal: Option<String>,
    ) -> Self {
        let mut r = Self::base(ts, AuditKind::BlobFetch);
        r.peer = peer;
        r.target = Some(hash);
        r.status = Some(status);
        r.principal = principal;
        r
    }

    pub fn trust(
        ts: String,
        event: String,
        target: Option<String>,
        principal: Option<String>,
    ) -> Self {
        let mut r = Self::base(ts, AuditKind::Trust);
        r.event = Some(event);
        r.target = target;
        r.principal = principal;
        r
    }
}

/// One live mesh session, in a [`StreamFrame::Snapshot`]. Surface-clean: `peer` is the
/// user_id-or-nickname the audit records carry, never an endpoint-id. `opened_at` is epoch seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveSession {
    pub peer: String,
    pub service: String,
    pub opened_at: i64,
    /// The caller's STABLE device principal, `eid:<hex>` (#73).
    ///
    /// `peer` is a display nickname and collides: two devices under one nickname, or two contacts
    /// sharing a display name, are indistinguishable in the live-session view. So "who is using my
    /// service right now", per-peer session counts, and any UI that lets a user act on a live
    /// session (revoke, disconnect, inspect) were all keyed on a collidable string.
    ///
    /// Same argument and same shape as [`PeerInfo`] (#41) and [`PeerReachability`] (#42).
    /// Nicknames NEVER authorize; this is the value to key on.
    ///
    /// **Snapshot only, for now.** `ActiveSession` appears in [`StreamFrame::Snapshot`] — there is
    /// no `active_sessions` on `StatusResult`. A client that keeps its view current by applying
    /// subsequent `session_open`/`session_close` events still has a collision problem: those are
    /// [`AuditRecord`]s and carry no principal (#57, unmerged). So the snapshot distinguishes two
    /// same-nickname devices and the next `session_close` for that nickname does not say which row
    /// to drop. Re-subscribe for an authoritative view until #57 lands.
    ///
    /// Always present for a real row — `Option` only so an older client round-trips. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

/// Which side of an app-blob transfer a [`StreamFrame::BlobTransfer`] describes (#82).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobDirection {
    /// We are SERVING bytes to a peer that dialed our app-blob ALPN.
    Serve,
    /// We are FETCHING bytes from a peer, via `blob_fetch`.
    Fetch,
}

/// Where an app-blob transfer is in its life (#82).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobTransferState {
    /// The transfer began; `bytes_total` is known from here on.
    Started,
    /// Bytes advanced. COALESCED — see [`StreamFrame::BlobTransfer`].
    Progress,
    /// Finished successfully. Carries the FINAL byte count.
    Completed,
    /// Ended without completing (peer went away, refused, or the store errored).
    Aborted,
}

/// One frame of the [`Request::Subscribe`] stream (pairing liveness & health telemetry). Tagged on
/// `type` (snake_case), so a frame is `{"type":"snapshot",...}` / `{"type":"event",...}` /
/// `{"type":"lagged",...}`. `Event.record` is the [`AuditRecord`] verbatim, so the stream and the
/// on-disk log carry ONE schema. The daemon serializes these; an embedding consumer deserializes
/// them (see `docs/local-protocol.md` "Live event stream").
///
/// **`#[non_exhaustive]`**: a future frame kind must not break a downstream `match`. Adding
/// `Reachability` in 0.13.0 DID break exhaustive matches — which is why that release is a MINOR,
/// per `RELEASING.md`'s pre-1.0 rule that breaking changes bump the minor. Consumers now write a
/// `_ =>` arm and later additions are additive for Rust.
///
/// **They are NOT additive for JSON, and this line claimed they were until 1.55.** The enum is
/// `#[serde(tag = "type")]` with no catch-all variant, so an unrecognised tag is a hard
/// deserialization error: [`StreamSubscription::next`](crate::StreamSubscription::next) surfaces a
/// newer daemon's frame as [`ClientError::Malformed`](crate::ClientError::Malformed) and a consumer
/// that propagates it loses the whole stream. `#[non_exhaustive]` is a Rust-`match` property and
/// does nothing for serde. A consumer that must survive a daemon newer than its `mcpmesh-local-api`
/// reads raw frames via [`ControlClient::open_stream`](crate::ControlClient::open_stream) and
/// ignores tags it does not know — that is the only forward-compatible path, and the crates ship in
/// lockstep precisely because of this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum StreamFrame {
    /// The FIRST frame: a point-in-time picture of the mesh (open sessions + paired-peer
    /// reachability) so a fresh subscriber renders immediately without replaying history.
    Snapshot {
        active_sessions: Vec<ActiveSession>,
        reachability: Vec<PeerReachability>,
        /// THIS node's own reachability posture (#90), so a fresh subscriber renders it without
        /// a `status` poll. `None` in mesh-less control-only mode. Additive: default +
        /// skip-if-none so an older payload round-trips.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        self_network: Option<SelfNetwork>,
    },
    /// A live audit event (session open/close, request, blob fetch, trust) — the tap on the hub.
    /// Boxed so this (much larger) variant does not bloat every frame; serde delegates through the
    /// `Box`, so the wire shape is the record's fields verbatim.
    Event { record: Box<AuditRecord> },
    /// A peer's reachability TRANSITIONED (#58): it became reachable, became unreachable, or was
    /// probed for the first time. Pushed so an embedder does not have to poll `status` for a live
    /// online/offline indicator — and so work queued for an unreachable peer can flush the moment
    /// it returns, rather than on the next poll tick.
    ///
    /// Emitted on a change of `reachable` **or of `path`**. A refresh with the same verdict AND the
    /// same path emits nothing, so a peer that stays up does not produce a frame per TTL refresh;
    /// `rtt_ms`/`meta`/`services` drift is advisory detail and is not a transition. `age_secs` is
    /// `0` — the observation just completed.
    ///
    /// **Do not treat this as an up/down toggle.** It carried that meaning through 0.18, and this
    /// doc said "on a CHANGE of `reachable` only" until 1.22 — which stopped being true in 0.19.0
    /// (#92 item 1), when `path` joined the transition rule. A consumer that assumed same-verdict
    /// frames were impossible was reading a stale guarantee.
    ///
    /// Two producers, as of API 1.22 — and since 1.30 `source` says WHICH ONE, so the distinction
    /// is readable rather than inferred:
    ///
    /// - [`ReachabilitySource::Probe`] — a probe completing (`status`/`subscribe` refreshing a
    ///   stale entry). It describes a throwaway dial, not anyone's live connection.
    /// - [`ReachabilitySource::Session`] — a live session whose selected path changed under it
    ///   (#92 item 2). A claim about the link in use.
    ///
    /// The second producer is why `path` is trustworthy for a long-lived session: a session that
    /// degrades Direct→Relay mid-call now says so when it happens, rather than staying silently
    /// mislabelled until something probes. `path` is a truth claim about where user data went, so
    /// `Unknown` means "we do not know" and must never be rendered as private.
    ///
    /// **`rtt_ms` is not a discriminator, and never was** (#150). Until 1.30 this doc said a
    /// session-sourced frame carries `rtt_ms: None` — true only of a FIRST observation, where no
    /// round trip was measured and none is invented. A session-sourced frame for an
    /// already-probed peer carries that probe's `rtt_ms: Some(..)`, because the path watcher
    /// deliberately leaves `rtt_ms`/`meta`/`probed_at` alone (refreshing them would stamp a stale
    /// RTT as fresh and suppress the corrective probe — #92 review). That is the common case for a
    /// peer probed at pairing time and then watched through a long call. Read `source`.
    Reachability {
        peer: PeerReachability,
        /// Which producer emitted this frame (#150). `api_minor >= 30`.
        ///
        /// Additive: `#[serde(default)]`, landing on [`ReachabilitySource::Unknown`] — NOT on
        /// `Probe`. A daemon at `api_minor` 22–29 already has both producers, so an absent field
        /// genuinely does not say which one ran; defaulting to `Probe` would assert the wrong
        /// producer for every session-sourced frame such a daemon emits, which is the exact
        /// ambiguity this field exists to remove.
        #[serde(default)]
        source: ReachabilitySource,
    },
    /// THIS node's own network posture CHANGED (#90): `online` flipped, the home relay moved,
    /// or a relay's connection state changed — pushed so an embedder learns "you just went
    /// unreachable" the moment it happens instead of on a poll tick, and so #53's `set_relays`
    /// finally has a signal telling someone to use it. `direct_addrs` drift alone does not
    /// emit (address churn is chatty and not a decision point; it rides the next frame).
    /// `api_minor >= 28`.
    SelfNetwork { self_network: SelfNetwork },
    /// This machine was SUSPENDED and has just resumed (#167 ask 2) — a closed laptop lid, a sleep,
    /// a hibernate. `api_minor >= 55`.
    ///
    /// The signal an embedder holding long-lived sessions actually needs. While a machine is
    /// suspended it sends nothing, so the *peer's* idle timer runs out and tears the connection down
    /// before the lid is even reopened; `keep_alive_secs` cannot help, because a suspended process
    /// emits no PINGs. Without this frame the first thing an app learns is a failed send, which is
    /// the worst possible moment to find out. With it, the app can tear down and re-dial
    /// deliberately.
    ///
    /// **Detected as clock skew, not guessed.** Rust's `Instant` is `CLOCK_MONOTONIC` on Linux and
    /// `CLOCK_UPTIME_RAW` on Apple targets, and neither advances across a suspend while the wall
    /// clock does. A tick where the wall clock outran the monotonic clock by more than the threshold is a
    /// suspend; a tick where BOTH ran long is a starved runtime and emits nothing. That distinction
    /// is deliberate — a signal that fired under load would train consumers to ignore it.
    ///
    /// **It names no session.** The daemon cannot know which of an embedder's sessions survived —
    /// a short suspend may well leave one intact. It means "this machine was away for N seconds;
    /// re-check what you hold".
    ///
    /// **A forward wall-clock STEP also emits, and cannot be told apart from a sleep.** A board with
    /// no RTC that boots with a bogus epoch and is then stepped months forward by NTP produces a
    /// frame claiming a multi-year suspend. Read `suspended_secs` as "wall time ran this much
    /// further than this process did", which is what is actually measured; the frame's advice
    /// survives either cause, but the number is not a measurement of sleep alone. Distinguishing
    /// them needs a continuous clock (`CLOCK_BOOTTIME` / `mach_continuous_time`), which is a
    /// per-platform dependency this does not take.
    ///
    /// Not retroactive, and — unlike the other transitions — with **nothing in `Snapshot` to
    /// recover it from.** [`Reachability`](StreamFrame::Reachability) and
    /// [`SelfNetwork`](StreamFrame::SelfNetwork) missed frames are recoverable because `Snapshot`
    /// carries `reachability` and `self_network`; there is no resume field, because "was this
    /// machine recently asleep" is not a state the daemon holds. A subscriber that attaches after a
    /// wake cannot learn one happened. If you reconnect, assume you missed events and re-check what
    /// you hold — the same advice this frame carries.
    Resumed {
        /// How long the machine was away, in seconds — the number that decides whether to re-dial
        /// everything or nothing. It is the wall-clock/monotonic skew, so it measures the suspend
        /// itself and not the time since the last frame.
        suspended_secs: u64,
        /// Wall-clock epoch seconds at which the resume was DETECTED (up to one tick after the
        /// machine actually woke).
        at_epoch: i64,
    },
    /// The subscriber fell `dropped` records behind the broadcast ring; the stream continues (a
    /// fresh reconnect would re-`Snapshot`). Never drops the subscriber — lag is reported, never fatal.
    Lagged { dropped: u64 },
    /// An app-blob transfer advanced (#82). Emitted on BOTH sides: `Serve` while we send bytes to
    /// a peer, `Fetch` while `blob_fetch` pulls them.
    ///
    /// **`Progress` is COALESCED, deliberately.** iroh-blobs reports progress per ~16 KiB chunk, so
    /// a 4 GiB transfer would push ~262k frames through a bounded ring and every subscriber would
    /// see `Lagged` — losing the audit events that share it. A frame is emitted on `Started`, on
    /// `Completed`/`Aborted`, and on `Progress` only after at least `max(1 MiB, total/100)` more
    /// bytes, so a transfer costs at most ~102 frames whatever its size.
    ///
    /// **Do not treat the last `Progress` as the total** — the final stride is usually skipped.
    /// `Completed` carries the final `bytes_done`.
    BlobTransfer {
        direction: BlobDirection,
        /// The blob's hash, hex.
        hash: String,
        bytes_done: u64,
        /// Known from `Started` onward; `None` only if the size was never reported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bytes_total: Option<u64>,
        state: BlobTransferState,
        /// SERVING side only: the STABLE `eid:` device principal we are serving (#38 — never a
        /// display nickname). Always `eid:<hex>`: this comes from the authenticated endpoint, so it
        /// is NOT the same namespace as a grant written as a user_id or roster name. Absent when fetching, where the counterparty is named
        /// by the ticket rather than by a resolved identity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer: Option<String>,
    },
}

/// Extract the `method` tag from a raw request value without deserializing the whole
/// message. The daemon's dispatcher uses this: match on the method string, then deserialize
/// `params` per-method — which tolerates omitted / null / `{}` params for parameterless
/// methods (adjacent tagging rejects `params:{}` on unit variants).
pub fn method_of(v: &serde_json::Value) -> Option<&str> {
    v.get("method").and_then(serde_json::Value::as_str)
}

/// How a service is answered. Mirrors the config `[services.*]` *kinds*;
/// Config→BackendSpec is a hand-written match, not a serde passthrough.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendSpec {
    Run {
        cmd: Vec<String>,
        /// Per-service environment variables (#51) for the spawned child. Overlaid on the
        /// daemon's inherited env; the injected `MCPMESH_PEER_*` identity vars ALWAYS win over
        /// these (identity is not spoofable by a service definition). Default empty.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        /// Working directory to spawn the child in (#51). Default: inherit the daemon's cwd.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    Socket {
        path: String,
    },
}

/// Control-API error code: the named service exists in neither `config.toml` nor the ephemeral
/// registry (#55). Distinct from the generic `-32000` so a caller can BRANCH on "no such service"
/// instead of parsing a message — `service_allow_grant`/`service_allow_revoke` previously answered
/// `{}` (success) for an unknown name, which silently included every ephemeral service.
pub const ERR_NO_SUCH_SERVICE: i64 = -32040;
/// The named blob is not held COMPLETE by this daemon (#83, `blob_republish`). Distinct from
/// [`ERR_NO_SUCH_SERVICE`] because the remedy differs: fetch the blob first.
pub const ERR_NO_SUCH_BLOB: i64 = -32041;
/// The blob was deliberately withdrawn from this scope (#107). Distinct from
/// [`ERR_NO_SUCH_BLOB`]: that means "fetch it first", this means "someone un-shared this on
/// purpose — `blob_publish` from the file if the re-share is intended".
pub const ERR_BLOB_WITHDRAWN: i64 = -32042;
/// `pair` was refused because the redeemer's nickname is already held by a DIFFERENT paired peer
/// (#87), so an embedder can branch on the one refusal that has a self-service remedy — rename and
/// redeem the same invite again — without reading the prose (#147).
///
/// Reading the prose was the only option before this code, and it does not survive translation: the
/// message is generated on the INVITER's side and travels to the redeemer, so the embedder that
/// DISPLAYS it cannot rewrite it into its own vocabulary except by substring-matching our copy.
/// Branch on this and write your own sentence naming your own rename affordance.
///
/// Deliberately narrow. It rides ONLY this refusal, which is sent exclusively to a caller that
/// proved possession of a live invite secret. The generic refusal keeps `-32000` and its opaque
/// reason: distinguishing unknown-vs-expired-vs-wrong-secret would be a redemption oracle.
pub const ERR_NICKNAME_TAKEN: i64 = -32043;

/// The invite line's own `expires_at_epoch` has passed (#159). Decided LOCALLY, before dialing —
/// this says nothing about the inviter's state. Remedy: ask for a fresh invite.
pub const ERR_INVITE_EXPIRED: i64 = -32044;

/// The inviter has **no outstanding invite at all** — its accept gate fast-closed the dial (#159).
///
/// This is as close to "expired or already used" as we can safely get, and the distinction matters:
/// it is a fact about the INVITER, not about the secret presented. Answering per-secret would be a
/// redemption oracle — a prober would learn which guessed secrets were ever real — which is why
/// [`ERR_INVITE_REFUSED`] stays deliberately undifferentiated. Remedy: ask for a fresh invite.
pub const ERR_INVITE_NOT_LIVE: i64 = -32045;

/// The inviter's address could not be dialed at all (#159) — offline, asleep, or unroutable.
/// Remedy: check they are running, then retry the same invite; it is untouched.
pub const ERR_INVITER_UNREACHABLE: i64 = -32046;

/// **The address-swap defense fired**: the TLS-authenticated peer is not the endpoint the invite
/// names (#159) — or, from `api_minor >= 64` (#223), the invite's embedded address names a
/// different endpoint than its `inviter_id`, refused BEFORE dialling so the named endpoint never
/// sees a handshake.
///
/// The one refusal here that must NOT be rendered as "try again". Something answered in place of
/// the machine the invite identifies — a substituted address, or a forged invite. An embedder that
/// treats every pairing failure as a friendly retry papers over exactly the attack this check
/// exists to catch. Remedy: do not retry; get the invite again through a channel you trust.
pub const ERR_INVITER_MISMATCH: i64 = -32047;

/// The invite asks to be called a name this node already uses for a DIFFERENT peer (#159).
///
/// The redeemer-side mirror of [`ERR_NICKNAME_TAKEN`], and a distinct condition: that one is the
/// inviter refusing the redeemer's name, this is the redeemer refusing the inviter's suggestion.
/// Nothing is granted either way — a name confers no access (#38) — so this protects this node's
/// own display and routing clarity. Remedy: ask for an invite suggesting a different name.
pub const ERR_INVITE_NAME_CONFLICT: i64 = -32048;

/// The inviter refused, and the cause is **deliberately withheld** (#159).
///
/// Unknown secret, expired secret, and wrong secret are one answer on purpose: telling them apart
/// is a redemption oracle. The code carries exactly as much as the prose already did — "that invite
/// did not work" — so a consumer can branch without parsing, and without learning anything a
/// prober could use. Remedy: ask for a fresh invite.
pub const ERR_INVITE_REFUSED: i64 = -32049;

/// The request was stopped on purpose before it finished (#172) — today, a `blob_fetch` that
/// [`Request::BlobFetchCancel`] tripped.
///
/// A cancelled request still ANSWERS. Cancellation is cooperative rather than a task abort
/// precisely so this code can be delivered: an aborted task returns nothing, and the caller waits
/// forever on work that already stopped. Distinct from `-32000` because it is not a failure — the
/// caller (or its user) asked for it. Remedy: none; retry the fetch if the cancel was a mistake.
///
/// **What it does not promise:** partial chunks already streamed into the blob store stay there,
/// unlisted and unreclaimable, exactly as they do when a fetch fails. That is #80's reclaim gap,
/// unchanged by cancellation.
pub const ERR_CANCELLED: i64 = -32050;

/// This control connection already has [`MAX_INFLIGHT`] requests running, so this one was refused
/// without being started (#172).
///
/// **Retryable, and cheap to retry** — retry after any response lands, or spread the load over a
/// second control connection. It is refused rather than queued deliberately: a queue is invisible
/// backpressure that a caller cannot tell apart from a slow daemon, and waiting for a permit inside
/// the read loop would reintroduce the head-of-line blocking concurrent dispatch exists to remove.
///
/// Not a security boundary — the control socket is the daemon owner's. It bounds the work one
/// connection can have outstanding so a buggy client cannot spawn unboundedly.
pub const ERR_TOO_MANY_INFLIGHT: i64 = -32051;

/// The invite line is a SELF-ENROLLMENT (`mcpmesh-enroll:`) and the caller did not offer that
/// ceremony — [`PairParams::allow_self_enroll`] was unset (#178).
///
/// Decided from the line in hand, BEFORE any dial: nothing was contacted, no secret was revealed,
/// and the invite is untouched. Like [`ERR_INVITE_EXPIRED`] it therefore reveals nothing about the
/// inviter and is safe to name precisely.
///
/// Distinct from [`ERR_INVITE_REFUSED`] in the direction it points: that one is the inviter turning
/// US down, this one is US declining a ceremony we were never asked to run. Remedy: if the person
/// meant to add another of their OWN devices, offer that explicitly and retry the SAME line with
/// `allow_self_enroll`; otherwise they pasted the wrong link and want an ordinary
/// `mcpmesh-invite:` one.
pub const ERR_SELF_ENROLL_NOT_OFFERED: i64 = -32052;

/// `service_allow_grant` — the principal is REVOKED on this node, so the grant was refused before
/// anything was written (#212, `api_minor >= 60`). Also `peer_introduce` (#218, `api_minor >= 61`)
/// when the SUBJECT proves a `user_id` this node has revoked: the row would be refused on sight.
///
/// Admission runs two gates: the revocation table first (`peer_revoke`), then the service's
/// `allow`. A grant to a revoked principal used to succeed and write a real `allow` entry that the
/// first gate would never let a session reach — the caller was told "granted" and `status`
/// confirmed it, while every session from that peer was refused. Now the write path consults the
/// same table admission does, and answers this instead of `{}`.
///
/// "Revoked" here means what admission means by it, read live: an `eid:` that `peer_revoke` marked
/// dead (locally or by a signed import, or by the installed roster), or a `b64u:` identity that
/// `peer_revoke` revoked — every row carrying that `user_id` is refused, whatever the endpoint
/// table says about the device (#218). Remedy: `peer_unrevoke` first if the revocation was a
/// mistake; otherwise there is nothing to grant.
///
/// **Numbering:** `-32053`..`-32055` are skipped. They are the SESSION-plane codes
/// (`mcpmesh_net::errors`: rate-limited / service refused / unreachable), and `-32053` in particular
/// is named by number throughout this file and the docs with its rate-limit meaning. Control codes
/// `-32051` and `-32052` already collide with session codes, which is survivable because the two
/// planes never share a frame — but a control `-32053` would make "guard on `-32053`" ambiguous on
/// the page that documents both. The control family continues from `-32056`.
pub const ERR_PRINCIPAL_REVOKED: i64 = -32056;

/// `pair` — the INVITER refused because it has revoked YOUR identity (`peer_revoke b64u:` on its
/// side) (#218, `api_minor >= 61`).
///
/// The inviter-side twin of [`ERR_PRINCIPAL_REVOKED`], and a different code because it names a
/// different node's table: that one is "this node revoked X, run `peer_unrevoke` here"; this one is
/// "the peer revoked me, and nothing on this side lifts it". Before #218 the inviter made no such
/// check — the row was written with the revoked `user_id`, the caller was told "paired", and every
/// session it opened was refused.
///
/// The one `pair` refusal besides [`ERR_INVITER_MISMATCH`] that must NOT be rendered as "ask for a
/// fresh invite": the invite was consumed, and any invite redeemed by a device presenting that
/// identity is refused the same way until the inviter lifts the revocation. What it does NOT do:
/// identity revocation refuses a device presenting the identity, and cannot stop a person who stops
/// presenting the key — redeeming with no binding (or under a new user key) lands as an ordinary
/// `eid:` pairing of an endpoint the inviter never revoked. Coded rather than folded into [`ERR_INVITE_REFUSED`] for the reason #147
/// codes anything: a caller that has PROVEN a live secret may be told the truth (the redemption
/// oracle that keeps `-32049` opaque is about unproven secrets), and the remedy differs from every
/// other refusal's — it is on the other side of the wire.
pub const ERR_PAIR_IDENTITY_REVOKED: i64 = -32057;

/// `self_enroll_detach` on a device with NO adopted binding in effect (#214): there is no live
/// enrollment to exit, and nothing live was changed. A STALE enrollment file left on disk (by a
/// boot that declined a binding that did not verify for this endpoint) is removed first, so the
/// next boot will not re-adopt it, and the message says when that happened. A removal that fails
/// is not this code: it is an uncoded `-32000` naming the file.
///
/// A UI that offers the detach only when `self_user_id` is present and `self_user_key_held` is
/// `false` does not send it except in a race — a concurrent detach, or an import (which also clears
/// the slot) — and can treat it as "already done". (It also never cleans a boot-declined file,
/// which is harmless: boot declines it again on every start.) Offering it on `!self_user_key_held`
/// alone also offers it to a node with no user key at all, which gets this code.
pub const ERR_NOT_ENROLLED: i64 = -32058;

/// `invite { as_self: true }` on a device that was itself ENROLLED into another identity (#214):
/// it holds no user key, so there is nothing to sign a binding with.
///
/// Refused AT MINT, on the device that can act on it. Before this the mint succeeded and the
/// refusal surfaced one round trip later on the OTHER machine as the deliberately opaque
/// [`ERR_INVITE_REFUSED`] — "that invite didn't work, ask for a new one", which is dead-end advice
/// for a permanent condition. Remedy: enroll from the device that holds the key, or
/// [`Request::SelfEnrollDetach`] this one first if it should not be enrolled at all.
/// `StatusResult::self_user_key_held` is the same fact, readable before anyone presses the button.
pub const ERR_SELF_ENROLL_NO_KEY: i64 = -32059;

/// How many requests one control connection may have in flight at once (#172), after which it
/// answers [`ERR_TOO_MANY_INFLIGHT`]. Per connection, not per daemon.
pub const MAX_INFLIGHT: usize = 32;

pub const API_NAME: &str = "mcpmesh-local/1";
/// The protocol-compatibility version as `"MAJOR.MINOR"`, distinct from the crate/stack version.
///
/// - **MAJOR** matches the `/N` in [`API_NAME`] and changes only on a breaking wire change (the
///   transport already rejects a mismatched `api`, so an equality check on that is redundant).
/// - **MINOR** ([`API_MINOR`]) increments on a surface change within a major — additive fields, new
///   methods, or a strictness change like params validation — bumped in the same change that makes
///   it. A client can guard with `api_minor >= N` for a feature it needs, or refuse a daemon older
///   than a minor it requires. It never resets except on a MAJOR bump.
///
///   It also bumps for a change to what a field MEANS with no change to its shape — six of the
///   thirty have, see [`API_MINOR`]'s history. "Every surface change" is what this line used
///   to claim, and it was wrong in both directions: minor 9's entry records surface changes that
///   shipped WITHOUT a bump, and six bumps changed no type at all. Read the history, not the rule.
pub const API_VERSION: &str = "1.66";
/// The integer MINOR of [`API_VERSION`] — see there. Bumped from 0 to 1 when params validation
/// became strict (#34); to 2 with the `set_nickname` verb + `StatusResult.self_nickname` (#37);
/// to 3 when `allow`/grant strings became STABLE principals — `b64u:`/`eid:`/roster names,
/// never nicknames (#38); to 4 with the `set_app_metadata` verb + `PresencePeer.meta` (#39);
/// to 5 with `PeerReachability.meta` — pairing-mode app metadata on the probe pong (#40);
/// to 6 with `PeerInfo.principal` — the peer's eid: device principal on `status` (#41);
/// to 7 with `PeerReachability.principal` — the same on reachability rows (#42); to 8 with the
/// `service_allow_grant`/`service_allow_revoke` per-peer access verbs (#44); to 9 covering the
/// `unregister_service` (#50) / `peer_services` (#52) / Run `env`+`cwd` (#51) surface that shipped
/// in 0.10.1 without a bump, PLUS the `set_relays` live relay-set verb (#53); to 10 when
/// `service_allow_revoke`/`peer_remove` became IMMEDIATE — no verb shape changed, but their
/// observable contract did: a revoked principal's next session is refused even on a connection it
/// already holds, and its live connections are severed. Previously both waited for the peer to
/// disconnect on its own, which is unbounded (#54). A consumer can guard on
/// `api_minor >= 10` before telling a user that revocation has taken effect; to 11 when
/// `service_allow_grant`/`service_allow_revoke` gained EPHEMERAL-service support and became strict
/// about an unknown service name — a name in neither the config nor the ephemeral registry now
/// answers [`ERR_NO_SUCH_SERVICE`] instead of a silent `{}` (#55, #69); to 12 with the pushed
/// [`StreamFrame::Reachability`] liveness transition frame (#58); to 13 with
/// [`PeerReachability::path`] — direct-vs-relay attribution on every reachability row (#64); to 14
/// with the `run`-backend `MCPMESH_PEER_EID` identity var — the caller's stable device principal,
/// unconditionally present, so a `run` server can scope per caller without keying on a nickname
/// (#60); to 15 with the `blob_revoke` / `blob_unpublish` verbs — per-scope withdrawal of a grant
/// and of a published hash, so un-sharing a file no longer requires unpairing the person (#62); to
/// 16 when the app-blob provider became available in PAIRING mode — the blob verbs previously
/// errored on any daemon without an org root key, though their scope gate never needed one (#61);
/// to 17 when the service answer began coming from the LIVE registry rather than config + overlay,
/// so a grant the accept path would refuse is no longer advertised. Three surfaces share that
/// resolver and all changed together: `status`'s `services[].allow`, `peer_services`' name list,
/// and the `mcpmesh/ping/1` probe's `services`. No wire shape changed, only the source of truth —
/// exactly the class of change a downstream cannot see in a type diff (#100); to 18 with `blob_republish`, so a fetched blob can
/// be re-served and every recipient becomes a source (#83); to 19 with durable blob revocation — an
/// unpublish now survives a later republish via a per-scope withdrawal set, and
/// [`ERR_BLOB_WITHDRAWN`] distinguishes "deliberately withdrawn" from "never had it" (#107); to 20
/// with `blob_list` filters + paging AND a DEFAULT limit of 256 scopes (the clamp is 4096) — a
/// daemon with more scopes than that previously answered with
/// everything, and past the 16 MiB frame cap the CLIENT rejected the response as malformed, leaving
/// the caller an opaque failure with no way to page. The connection survived: the control surface
/// carries no strike bound. This is a behaviour change for existing callers, detectable via the new
/// `total`/`truncated` (#84b); to 21 when a
/// PATH change became a reachability transition — [`StreamFrame::Reachability`] stopped being an
/// up/down toggle and same-verdict frames became possible (#92); to 22 with a SECOND producer for
/// that frame: a live per-session watcher that pushes when a session's selected path changes,
/// rather than waiting for a probe, at a cadence probes never had (#92); to 23 when
/// [`PeerReachability::rtt_ms`] stopped including the path-settle window — a relayed peer could
/// previously never report under 600ms, so "relayed AND fast" was unreachable by construction
/// (#123); to 24 when `reachable` stopped sharing a deadline with path classification — a relayed
/// peer whose pong arrived after ~2.4s was reported OFFLINE while it was answering (#128); to 25
/// with [`ActiveSession::principal`] — the live-session view was keyed on a display nickname, so
/// two devices under one nickname were indistinguishable and any UI acting on a session (revoke,
/// disconnect, inspect) keyed on a collidable string (#73); to 26 when a
/// rate-limited inbound NOTIFICATION stopped being silently dropped and became a recorded audit
/// event — no type changed; the observable audit stream did (#76, #139); to 27 with the `audit_prune` /
/// `audit_list` verbs, `StatusResult::storage`, and the opt-in `[limits].audit_retain_months`
/// boot retention — the audit log stopped being a permanent, unbounded, unreadable record (#88);
/// to 28 with `StatusResult::self_network` / `StreamFrame::SelfNetwork` / the snapshot's copy —
/// the node's OWN reachability posture, previously unanswerable from either side of the API
/// (#90); to 29 with [`AuditRecord::principal`] — stable identity on the event stream and the
/// on-disk log, resolving #57's parked docs conflict in favour of the #41/#42/#73 line (the
/// audit surface bans secrets and raw hex, not the prefixed principal rendering); to 30 with
/// [`StreamFrame::Reachability`]'s `source` — the frame has had TWO producers since 22 with no way
/// to tell them apart, so an embedder could not distinguish "a throwaway dial went via a relay"
/// from "the link this call is on just degraded", and had to hedge every message down to the
/// weaker claim. `rtt_ms: None` was never the discriminator the doc implied (#150); to 31 with
/// [`ERR_NICKNAME_TAKEN`] — the nickname-collision `pair` refusal is branchable instead of
/// `-32000`, so an embedder writes its own recovery copy rather than substring-matching ours. The
/// prose changed with it: it named the `set_nickname` CONTROL VERB as the remedy, which a GUI user
/// cannot type, and the refusal is generated inviter-side so the embedder displaying it could not
/// rewrite it (#147); to 32 with [`SelfNetwork::identity_conflict_epoch`] — two nodes booted from
/// COPIES of one mesh root share an endpoint id, and the displaced one's peers went unreachable
/// with nothing saying why. The relay reports it and iroh only `warn!`s it, so the fact existed
/// and was unreadable (#134); to 33 with the `peer_diagnostics` verb — a long-lived pairing that
/// cannot hole-punch while a fresh identity on the same hardware can differs only in DURABLE
/// per-peer state, and none of it was readable from outside the daemon (#140); to 34 when
/// outstanding invites became DURABLE — `invite.expires_at_epoch` changed meaning from an upper
/// bound on the daemon's process lifetime to the real lifetime, and `invite` gained an error where
/// it previously always succeeded. No shape changed, which is exactly the class minor 10 records:
/// guard on `api_minor >= 34` before telling a user their invite will still be good tomorrow
/// (#87b); to 35 with `InviteParams.max_uses` + `InviteResult.uses_remaining` — a bounded
/// multi-use invite, so onboarding a team is one link rather than one ceremony per person. Each
/// redemption still runs its own SAS and writes its own peer rows; it is N pairings sharing a
/// secret, never a group identity (#87); to 36 with branchable codes for the rest of the ONBOARDING
/// refusals — expired line, no live invite, inviter unreachable, id mismatch, name conflict, and
/// the deliberately-opaque refusal. `ERR_NICKNAME_TAKEN` had been the only coded pairing failure,
/// so every other one arrived as `-32000` and an embedder could either forward our prose to end
/// users or substring-match it (#159); to 63 when the identity gates began to hold under the
/// user-key lock (#221, #219) — no type changed: `user_key_export` REFUSES (`-32602`) on an
/// enrolled device, where through 62 it returned the recovery phrase of the local key boot minted,
/// an identity no peer has paired with; `user_key_import`'s `replace` guard now protects a key an
/// earlier import wrote within the same daemon lifetime, where through 62 a second import without
/// `replace` silently discarded the first; `peer_endorse` refuses when no user key is on disk
/// instead of minting one; and `peer_endorse`, `device_revoke`, `user_key_export` and the
/// self-enrollment signature check the enrollment gate under the lock an adoption or import writes
/// under, so neither can land between the check and the signature. Guard on `>= 63` before
/// offering `user_key_export` on a device whose `self_user_key_held` is false. Crate-level a MINOR
/// release of `mcpmesh-node` (0.54 → 0.55): `pairing::rendezvous::SignBindingFn` became async; to
/// 64 when revocation began to hold on the OUTBOUND dial paths (#223) — no type changed:
/// `peer_services` refuses (`-32000`, "is REVOKED on this node") a device the installed roster
/// revoked and a roster device whose roster `user_id` is a revoked `b64u:` identity, where through
/// 63 it checked only this node's own revocation tables; the reachability probe no longer dials a
/// revoked peer and COMMITS NOTHING for it — no `reachability` frame with `source: "probe"`, and
/// its `status.reachability` row is not refreshed (never probed reads `age_secs` absent), where
/// through 63 every stale read dialled it and committed whatever came back — including
/// `reachable: true` from a revoked device that still pairs this node, since that device answers
/// the ping — and a live session's path watcher does not write that row either (no `source:
/// "session"` frame for a revoked peer); `status.revoked` also lists roster refusals (`source: "roster"` for the roster's
/// `revoked_endpoints`, `"roster_identity"` for a roster device under a revoked `b64u:` user), so a
/// row that probe no longer refreshes can be matched to its revocation; `blob_fetch` never dials
/// a revoked ticket publisher or named `from` source, and when the fetch then fails its message
/// says how many named sources were skipped; `pair` refuses with `-32047` BEFORE dialling an invite
/// whose embedded address names a different endpoint than its `inviter_id`, and `attest_to` refuses
/// an offer shaped the same way and one naming a node this node revoked. `peer_diagnostics` and
/// `peer_hint_clear` are unchanged — they dial nothing. Guard on `>= 64` before reading a missing
/// probe frame for a revoked peer as "no change". Crate-level a MINOR release of `mcpmesh-node`
/// (0.55 → 0.56): `roster::distribute::DistributionHost` gained the required `dial_refused`; to
/// 65 when revocation began to hold on EVERY outbound connection (#229) — no type changed: the
/// node's endpoint refuses to dial a revoked device on every ALPN but pairing, so iroh-gossip no
/// longer dials a revoked device it learned from the swarm and an embedder protocol dial to one
/// fails "rejected locally"; and `peer_revoke`, `device_revoke`, `device_revocation_import`,
/// `org_revoke` and `roster_install` close connections this node OPENED to a device they revoke —
/// an `open_session` pipe ends and this node closes a held `connect_protocol` connection with code
/// 401, where through 64 both ran until the peer hung up. `peer_remove` and a roster drop that
/// revokes nothing refuse no dial and close nothing outbound. `severed` counts the ACCEPTED
/// connections a revoke cut, exactly as through 64; the dialled connections it closes are not in it.
/// Guard on `>= 65` before relying on an outbound session ending when its peer is revoked; to 66
/// when sessions to one peer began SHARING one QUIC connection (#215) — no type in this file
/// changed; two meanings did. A connection-level event (the peer closing, an idle timeout, a
/// revoke's close pass) now ends EVERY `open_session` pipe to that device at once, not one. And
/// `peer_services` answers only from a pong: a live session's path watcher writes a reachable row
/// with no services in it, and through 65 this verb answered `[]` from that row for up to a TTL,
/// indistinguishable from "offers you nothing". From 66 an empty list means the peer's pong named no
/// services, and a probe that fetched no pong fails retryably ("could not be fetched just now")
/// rather than answering `[]`. Guard on `>= 66` before treating an empty `peer_services` answer as
/// "offers nothing". A peer that dies without closing its connection changes `open_session`'s
/// failure shape too: a session to it opens at once on the still-open shared connection and ends
/// (EOF) at the idle timeout, where through 65 it failed at open with `-32055`. Crate-level, a
/// MINOR release of `mcpmesh-node` (0.57 → 0.58): `daemon::ReachEntry` gained the pub field
/// `pong_at` and became `#[non_exhaustive]`; to
/// 62 with the self-enrollment EXIT and the surface an
/// embedder needs to ship the ceremony at all (#214): [`Request::SelfEnrollDetach`] drops an
/// adopted binding (the inverse of `pair { allow_self_enroll }`, and the only exit an enrolled
/// device has — `user_key_import` needs the phrase of a key it does not hold);
/// [`RecentPairing::self_enroll`] marks which ceremony produced a SAS row, so the inviter's
/// mismatch branch can route to `device_revoke` instead of a `peer_remove` of a peer that does not
/// exist; [`StatusResult::self_user_key_held`] says whether this device holds its key, which is
/// what `peer_endorse`, `device_revoke` and `invite { as_self }` all refuse without; `device_revoke`
/// now REFUSES on an enrolled device (a bug: it signed with the local key, self-applied the
/// revocation and severed sessions, then returned a token no peer would ever accept — a silent
/// partial success on the stolen-laptop path); and `invite { as_self }` on an enrolled device is
/// refused at mint with [`ERR_SELF_ENROLL_NO_KEY`] rather than one round trip later, on the other
/// machine, as the opaque `-32049`. Guard on `>= 62` before offering the detach; the two fields
/// read `false` from an older daemon, which is wrong for exactly the case each exists to name
/// (`self_user_key_held` for a key-holder, `self_enroll` for an enrollment row), so guard before
/// rendering them. Crate-level, this one is a MINOR release of `mcpmesh-node` (0.53 → 0.54), not a
/// patch: `InviterCtx::record_pairing`'s closure signature changed (`RecordPairingFn` now takes the
/// whole `RecentPairing`) and `MeshState::recent_pairings` became `pub`, both on that crate's
/// public surface; to 61 when an IDENTITY revocation (`peer_revoke b64u:`)
/// began to hold at every site (#218): `pair` answers [`ERR_PAIR_IDENTITY_REVOKED`] when the
/// inviter has revoked the redeemer's proven `user_id`, `peer_introduce` answers
/// [`ERR_PRINCIPAL_REVOKED`] for a subject proving one this node revoked, and admission refuses
/// any stored row carrying a revoked `user_id` — as does every OUTBOUND dial: `open_session`,
/// `peer_services` and `peer_diagnostics` answer the same "REVOKED" refusal an endpoint-revoked
/// device gets, and `pair` REDEEMING an invite from an inviter this node revoked (its endpoint, the
/// identity its row carries, or the identity it proves in its reply) answers
/// [`ERR_PRINCIPAL_REVOKED`] before writing a row or running the grant-back. Below 61 only device attestation consulted the
/// identity table: a fresh invite (or an introduction) landed a row for the person's next device,
/// the caller was told "paired", and the row's `services[].allow` grant was honoured — which is
/// also why minor 60's "a `b64u:` hides only while EVERY device is refused" no longer holds: a
/// per-device `peer_unrevoke` under a standing identity revocation re-admits nothing, and the
/// entry stays hidden until the identity itself is unrevoked. Guard on `>= 61` before branching
/// on the new code; to 60 when `services[].allow` on `status` became
/// REVOCATION-AWARE and `service_allow_grant` began refusing a revoked principal with
/// [`ERR_PRINCIPAL_REVOKED`] (#212). No shape changed — minor 17's class again. #100 made
/// `services[].allow` answer from the live registry so it could not report a grant the accept path
/// refuses, and that claim was one gate short: `peer_revoke` writes only the revocation table, the
/// registry's `allow` kept the entry, and `status` copied it out unfiltered. An entry whose
/// principal is revoked — as admission defines it — is now omitted from `allow` (and, index-aligned,
/// from `allow_display`); for a `peer_revoke` revocation `status.revoked` still lists the principal,
/// so the fact is not lost, only moved to the surface that means it (a ROSTER-revoked device is
/// omitted too, and its record is the roster's own `revoked_endpoints` — it is absent from
/// `roster_members` as well). The write path closed with it: the grant used to succeed and
/// write an entry admission would never honour. Guard on `>= 60` before treating an `allow` entry's
/// absence as "not granted" rather than "granted but refused", and before branching on the new
/// code; below it, join `status.revoked` against `allow` yourself; to 59 with [`Request::PeerHintClear`] — FORGET one peer's
/// persisted dial hint (#140). An experiment tool and a workaround, not a policy change: nothing
/// clears a hint automatically. `PeerEntry.last_addr` is the only durable per-peer state on a node's
/// disk that the dial path reads and the only thing a long-lived pairing carries that a freshly
/// paired identity does not, so clearing it makes the pairing ADDRESSING-EQUIVALENT to a fresh one —
/// which is exactly the difference #140 is about. Advisory, never authorization: the row, its
/// `user_id`, its services and its pairing stamp are untouched, and an absent hint is a supported
/// state. Guard on `>= 59`; to 58 with the REVERSE-DNS `_meta` key spellings
/// `tech.counterpunch.mcpmesh/{service,peer}` alongside the legacy `mcpmesh/{service,peer}` (#49,
/// SEP-1788's SHOULD). No shape changed, and **no peer or backend has to change what it READS**:
/// both spellings are WRITTEN with identical values and EITHER is accepted, which is why this did
/// not need the coordinated wire change #49 assumed. A backend reading `mcpmesh/peer` keeps working.
///
/// **It is not a no-op for every backend, though.** A handler that rejects unknown `_meta` keys —
/// `deny_unknown_fields`, `additionalProperties: false` — now sees a second one and will refuse
/// requests it accepted at 57, the same class of break that made 0.50.0 a MINOR. Both prefixes are stripped from caller frames, because a
/// prefix mcpmesh writes but does not strip is one a caller can forge. A caller sending the two
/// service spellings with DIFFERENT values is REFUSED rather than reconciled. The reserved-key
/// enumeration in the 2026-07-28 grammar is `progressToken`, `io.modelcontextprotocol/*`, and bare
/// `traceparent`/`tracestate`/`baggage`; a prefixed key is reserved only by its SECOND label, so
/// neither of our spellings ever collided. **The legacy
/// `mcpmesh/*` spellings are deprecated as of 0.51.0 and will be removed at 1.0** — migrate reads to
/// the reverse-DNS form. Guard on `>= 58` only if you need the new spelling to be present; to 57
/// when `_meta["mcpmesh/peer"]` began being injected on
/// EVERY proxied request rather than only the handshake (#45 ask 2). **No shape changed** — this is
/// the same class as 37, on the same field: what changed is WHEN a backend can rely on the value
/// being there. Before 57 a served backend learned its caller on the session's first frame and on
/// any later `initialize`, and saw nothing on frames 2..N; from 57 every request carrying a `method`
/// is attributed. A backend that authorizes per-request rather than binding at session start must
/// guard on `>= 57`, exactly as 37's note says to guard before trusting the value at all. Positional
/// (array) params are the one exception and are deliberately left un-attributed — see
/// `docs/local-protocol.md`. Guard on `>= 57`; to 56 with [`PeerDiagnosticsResult::known_addrs`] and its two
/// set-difference companions — IROH's own view of a peer's addresses alongside the hint we stored
/// (#140). The verb dumped this node's disk and nothing about what iroh made of it, which is the
/// half the standing hypothesis lives in: iroh skips address lookup while a path is selected, so a
/// pair holding a relayed connection never re-discovers and the stale hint is the only addressing
/// the dial contributes. Read-only (a point read of the remote map), so it stays safe to run on a
/// live reproduction — though it does reset iroh's ~60s per-remote idle timer, so polling it keeps
/// state alive that would otherwise be reaped. `known_addrs` ABSENT means iroh holds no entry right
/// now, which is neither an empty list nor "never heard of": iroh reaps a remote ~60s after its last
/// connection closes. Guard on `>= 56`; to 55 with [`StreamFrame::Resumed`] — a SUSPEND/RESUME
/// signal on `subscribe` (#167 ask 2). A suspended machine sends nothing, so the peer's idle timer
/// tears the connection down before the lid reopens and `keep_alive_secs` cannot help; the frame is
/// what lets an embedder re-dial deliberately instead of discovering it on the next send. Detected
/// as wall-clock/monotonic skew, so a starved runtime (both clocks run long) does NOT emit. It names
/// no session — the daemon cannot know which survived. Guard on `>= 55`; to 54 with
/// [`OpenSessionParams::idle_timeout_secs`] — a
/// PER-CONNECTION QUIC idle timeout on the dial path (#166). The node-wide `[network]` knob made a
/// chat session, a bulk blob transfer and a media flow share one compromise. Only LOWERING is
/// unilateral (QUIC negotiates the minimum of both peers), which is also why the absence of an
/// accept-side twin is a missing symmetry rather than a missing half: the direction that can work
/// one-sidedly is available, and the direction that cannot never worked anywhere. Ignored on a
/// RACING dial, which abandons connections (logged at `warn!`, since a caller cannot know how many
/// devices a peer has). No per-connection keepalive — iroh caps the per-path
/// interval at 5s, so one could only make pings more frequent. Guard on `>= 54`; to 53 with `org_rotate` and the roster's
/// `successor_root_pk`/`successor_sig` — ORG ROOT ROTATION (#93 ask c). The schema pinned exactly
/// one signature slot against one key, so an operator laptop that died took the org with it 90 days
/// later when the roster expired, and the delay is what made it undiagnosable; recovery was O(N)
/// fresh ceremonies with every member. A roster now carries a successor root cross-signed by its
/// predecessor, and the bridge rides EVERY subsequent roster — so a member offline for the
/// announcing publication still catches up, which the obvious one-shot design does not give you. A
/// successor is adopted only when the pinned key no longer signs directly, only on a statement that
/// key signed, and never resets rollback protection. **A rotated roster declares
/// `mcpmesh-roster/2`**, so EVERY member must be on 0.47.0+ before an org rotates — an older binary
/// refuses the closed-schema document and stops receiving membership changes entirely; an org that
/// never rotates keeps producing `/1` byte-identically. NOT escrow: a LOST root cannot sign a bridge —
/// copy `org-root.key` to a second operator machine for that. Guard on `>= 53`; to 52 with `attest_offer` and the DEVICE ATTESTATION
/// ceremony (#85 ask 3) — a peer that already holds your `b64u:` admits a replacement device on the
/// strength of a user-key binding, with no fresh SAS ceremony with everyone you ever paired with.
/// `PeerEntry.user_id` was written once at pairing and never refreshed, so a machine restored from
/// a recovery phrase (ask 2) was still a complete stranger. **OFF by default**
/// (`[identity].admit_attested_devices`): it changes what a pairing MEANS, from admitting a device
/// to admitting a person and their future devices, and that should be chosen rather than inherited
/// on upgrade. An attestation cannot admit a stranger — the receiver must already hold a row for
/// that `user_id` — cannot resurrect a REVOKED endpoint OR a revoked IDENTITY (ask 4, which is why
/// it shipped first; `peer_revoke` on a `b64u:` now revokes the person, because a thief holding the
/// disk holds the user key and can mint a fresh endpoint id at will), and
/// grants the INTERSECTION of that person's existing services, never the union. Guard on `>= 52`;
/// to 51 with the PAIRING-MODE REVOCATION verbs —
/// `peer_revoke` / `peer_unrevoke` / `device_revoke` / `device_revocation_import`, plus
/// [`StatusResult::revoked`] (#85 ask 4). `revoked_endpoints` was roster-only, so in pairing mode
/// the only remedy for a stolen device was every peer independently running `peer_remove`, with
/// nothing telling them they should — until the last one did, whoever held the disk authenticated
/// as its owner and every message they sent was cryptographically indistinguishable. Two
/// directions, deliberately not one verb: `peer_revoke` is MY local decision about YOUR device;
/// `device_revoke` signs a portable statement about MY OWN, which is the half my peers cannot
/// discover for themselves. An import is honoured only from a `user_id` this node already pairs
/// with, and only for that person's OWN devices — a signature proves who asked, not that the
/// endpoint was ever theirs. Revocation is IMMEDIATE (live sessions severed, #54) and outlives the
/// pair row, so it cannot be undone by re-pairing. Guard on `>= 51`; to 50 with [`SelfNetwork::local_discovery`] — LOCAL (mDNS)
/// peer discovery, off unless `[network].local_discovery` asks for it (#68). Peer resolution
/// otherwise needs external infrastructure, so two machines on one LAN with no uplink could not
/// find each other though the path between them was fine. Three modes: `"off"` (default), `"on"`
/// (resolve AND announce), `"resolve"` (resolve without publishing this node's identity — it still
/// QUERIES about once a second, so it is quieter, not silent). Reported on `status` because the
/// setting was
/// otherwise unobservable and because **`"on"` means this node multicasts its endpoint id and
/// addresses to every device on the link** — a product backing a privacy switch has to be able to
/// show that. Deliberately NOT on by default, against what #68 asked for: pkarr publishes a signed
/// record you must already know the endpoint id to look up, while mDNS announces to strangers on a
/// café or hotel network, and a multicast packet cannot be un-sent. Guard on `>= 50`; to 49 with [`StorageInfo::blobs_gc`] — app-blob GARBAGE
/// COLLECTION, off unless `[blobs].gc_interval` is set (#80). `blob_unpublish` and `blob_revoke`
/// closed the AUTHORIZATION half at 15; neither reclaimed a byte, so `<data_dir>/blobs/` grew
/// monotonically for the life of the node and an embedder that had told a user "this file is
/// deleted" could not deliver that. Opt-in, because a sweep also reclaims blobs this node FETCHED
/// and never republished — reclaimable in themselves (the fetch already wrote the caller's
/// `dest_path`) but it means `blob_republish` of a hash fetched more than one interval ago fails.
/// `blobs_gc` is `None` when collection is not configured, `Some` with `runs: 0` when it is
/// configured and has not swept yet — a distinction worth reading, because the collector sleeps a
/// full interval before its first run. WATCH `runs`: iroh-blobs ends collection for the process on
/// its first sweep error, so a counter that stops advancing is the only signal. Guard on `>= 49`;
/// to 48 with `user_key_export` / `user_key_import` — a
/// RECOVERY PHRASE for the user key, so a person's `b64u:` survives the hardware (#85 ask 2). It
/// lived in one file on one machine with no export, import or escrow verb anywhere, so replacing a
/// laptop destroyed the identity peers pin, kb audiences key on, and a roster names — recovery was
/// an in-person SAS ceremony with everyone you had ever paired with. **The export response carries
/// a PRIVATE KEY**: it is deliberately absent from the audit log, from `status`, and from every
/// other surface. Import refuses to overwrite an existing key unless asked, because doing so
/// discards a live identity irreversibly. What it does NOT do: get a device admitted. Peers
/// authorize per DEVICE, and a restored user key puts this endpoint in nobody's allowlist — that is
/// #85 ask 3, unshipped, and the reason a recovered person still pairs. Guard on `>= 48`; to 47 with `BlobFetchParams::from` — ADDITIONAL sources a
/// fetch falls back to when the ticket's publisher does not answer (#83). Content addressing makes
/// every recipient a potential source, and a one-address ticket made that unusable: a file shared
/// with a room became unfetchable the moment the sender closed their laptop, though others in the
/// room already held the identical verified bytes. Additive and absent-tolerant — an older caller's
/// payload reads as empty, which is the single-source behaviour — so guard on `>= 47` only before
/// SENDING the field (`deny_unknown_fields` rejects the whole request below it). The bytes stay
/// BLAKE3-verified against the ticket's hash whoever serves them, so an alternate can refuse but
/// never substitute; it must have republished the hash into a scope granting the caller
/// (`blob_republish`, `api_minor >= 18`). What did NOT land: multi-source PARALLEL fetch — sources
/// are tried in order, so an offline publisher costs one dial timeout; to 46 with the roster-mode embedding surface (#66, #93):
/// the `org_create` / `org_approve` / `org_revoke` AUTHORING verbs, the `roster_members` read,
/// `PresencePeer::display_name` + `groups`, `RosterStatus::groups`, and
/// `OrgJoinResult::restart_required`. Two gaps close together. Authoring existed only as CLI
/// porcelain, so an embedded node could CONSUME a roster and never author one — no "approve this
/// person" button without shelling out to a second binary. And the roster's own contents never
/// crossed the seam: an embedder had managed group membership it could not display, and the only
/// route to a member list was hand-parsing the daemon-owned `roster.json`. `roster_members` is a
/// different question from `status.presence` — that lists reachable DEVICES and omits a person
/// whose devices are all down. `restart_required` closes a silent partial success: `roster_mode` is
/// a BOOT decision fixing the bound ALPNs and whether gossip/presence/blobs exist at all, so a
/// pairing-mode node that ran `org_join` got working MCP sessions with permanently empty presence
/// and no way to detect it. Guard on `>= 46` before offering org authoring in a UI; the read fields
/// are additive and degrade to empty. What did NOT land: org root ROTATION (#93c) — an operator
/// laptop that dies still takes the org with it once the roster expires; to 45 with [`PairParams::allow_self_enroll`] +
/// [`ERR_SELF_ENROLL_NOT_OFFERED`] — `pair` now REFUSES a `mcpmesh-enroll:` line unless the caller
/// asked for that ceremony. A behaviour change for existing callers, deliberately: at 43-44 a caller
/// whose UI only ever offered "add a contact" completed a self-enrollment and learned which
/// ceremony it had run from `enrolled_as_self` afterwards — by which point the device→user binding
/// was written and irrevocable short of rotating the user key (#178). The refusal is decided from
/// the line before any dial, so the invite survives it and the same line works once the ceremony is
/// actually offered. Guard on `>= 45` before sending the field — below it `deny_unknown_fields`
/// rejects the whole request. Note what the guard means: a daemon BELOW 45 gives a caller no way to
/// decline, so a UI that does not offer device enrollment should require `>= 45` rather than pair
/// without it; to 44 when control responses stopped arriving in REQUEST
/// order and the `blob_fetch_cancel` verb landed (#172). The daemon now dispatches each request
/// CONCURRENTLY on its connection, so a `blob_fetch` no longer stalls every other verb behind it —
/// and responses arrive in COMPLETION order. JSON-RPC ids make that legal and the in-tree
/// `ControlClient` cannot observe it (one request at a time, by construction), but a hand-rolled
/// client that pipelines and matches responses POSITIONALLY breaks. A connection also caps
/// in-flight requests and refuses over it with [`ERR_TOO_MANY_INFLIGHT`], and closing a control
/// connection now genuinely ABORTS its in-flight work rather than letting it run to completion
/// unread. Guard on `>= 44` before pipelining, before sending `blob_fetch_cancel`, and before
/// treating [`ERR_CANCELLED`] as unexpected; to 43 with `InviteParams::as_self` — SELF-ENROLLMENT, so one
/// person's devices share a `user_id` instead of appearing as unrelated strangers (#86). The
/// ceremony is ordinary pairing; the outcome is a device→user binding rather than a peer row, and
/// the private key never moves. Guard on `>= 43`. What this entry did NOT say, and 45 fixed: the
/// distinct scheme closes the version-SKEW hazard (a pre-43 redeemer silently over-granting) and
/// closes nothing for a CURRENT redeemer, which had no way to decline a ceremony it never offered
/// (#178); to 42 with the `peer_introduce` + `peer_endorse`
/// verbs — install a peer from a
/// SIGNED endorsement by someone you are already paired with, so a small group onboards in O(N)
/// instead of O(N²) two-human ceremonies (#65). It installs IDENTITY only and grants nothing, which
/// is what bounds it. Guard on `>= 42`; to 41 with `StreamFrame::BlobTransfer` — live app-blob
/// transfer progress on both the serving and fetching side (#82 ask 2), so an embedder can draw a
/// real progress bar instead of an indeterminate spinner. Guard on `>= 41` before expecting the
/// frame. NOTE what it did NOT bring, and 44 did: at 41 `blob_fetch` still blocked its whole
/// control connection for the transfer and nothing could cancel it (#172) — progress arrived on the
/// SUBSCRIBE connection, which is a different one; to 40 with `[services.<name>].rate_limit_per_min` +
/// `RegisterServiceParams::rate_limit_per_min` — proxied-request buckets became per
/// `(service, endpoint)` instead of one shared per-endpoint bucket, so a noisy service can no
/// longer starve a quiet one (#63). `-32053` changes meaning with it: it is now per-service, so a
/// consumer that backs off globally on one is backing off further than it needs to. Guard on
/// `>= 40` before sending the field or narrowing a back-off; to 39 with `PairParams::as_nickname` +
/// `InviteParams::peer_nickname` — LOCAL aliases for the other party, so a nickname collision is
/// resolvable by the person who hit it instead of requiring the other human to rename a machine or
/// re-mint. #147 made the collision diagnosable; this makes it fixable. Guard on `>= 39` before
/// offering an alias field in a UI: below it `deny_unknown_fields` rejects the whole request
/// (#87); to 38 with `[network].presence_mode` + `SelfNetwork.
/// presence_mode` — `reachable: false` gained a new meaning ("up, paired, and deliberately not
/// answering"), and `peer_services` flips from "reachable, empty list" to "unreachable" for a
/// caller holding no grant. A consumer must guard on `api_minor >= 38` before telling a user their
/// peer is offline, since below it that verdict could not mean this (#89); to 37 when the reserved
/// `mcpmesh/*` `_meta` namespace began
/// being enforced on EVERY proxied frame rather than the session's first. `run_session` treats
/// frame 1 as the `initialize` whatever its method is, so a caller could send any other method
/// first and put its real `initialize` — with a forged `mcpmesh/peer` naming another principal,
/// forged `groups` and all — in frame 2, where nothing stripped or injected. No shape changed;
/// what changed is whether `_meta["mcpmesh/peer"]` can be trusted, which is the entire reason a
/// backend reads it. Guard on `api_minor >= 37` before keying authorization on that value (#164).
///
/// **Not every semantic change gets a minor, and that is the gap to watch (#122).** A minor marks a
/// change to this *surface*. A change to behaviour BEHIND the surface — same fields, same shapes,
/// different meaning — may not bump it, and is invisible to a type diff. 17 and 24 above happen to
/// be that class and did bump; do not infer from them that every such change will. When bumping
/// several minors at once, read this block end to end AND the release notes, not the diff.
///
/// That class is bigger than it looks: **10, 17, 21, 22, 23, 24 and 37 all shipped with no change
/// to any type in this file** — they moved meaning, not shape. Seven of the forty, and 37 is
/// a SECURITY fix, which is the case where a consumer most needs the guard. 38 adds a field, but
/// its REAL content is a meaning change to `reachable` — the field exists so the new meaning is
/// observable at all. A downstream
/// that diffs types across a multi-minor bump sees nothing for any of them.
pub const API_MINOR: u32 = 66;

#[cfg(test)]
mod tests {
    use super::*;

    /// #64: the path field's wire shape, and its ADDITIVE default. A row from an older daemon has
    /// no `path` key at all and must land on `Unknown` — never on `Direct`, which would invent a
    /// privacy guarantee that daemon never made.
    /// #223 review: `API_VERSION` is the string form of `API_MINOR`, and a bump that edited only one
    /// of them shipped once. Pinned so it cannot again.
    #[test]
    fn api_version_is_the_string_form_of_api_minor() {
        assert_eq!(API_VERSION, format!("1.{API_MINOR}"));
    }

    #[test]
    fn peer_path_tags_and_defaults_to_unknown() {
        let tagged = |p: PeerPath| serde_json::to_value(p).unwrap();
        assert_eq!(tagged(PeerPath::Direct)["kind"], "direct");
        assert_eq!(tagged(PeerPath::Unknown)["kind"], "unknown");
        let relay = tagged(PeerPath::Relay {
            url: Some("https://relay.example/".into()),
        });
        assert_eq!(relay["kind"], "relay");
        assert_eq!(relay["url"], "https://relay.example/");
        // A relay whose URL we do not know still tags as relay, with the key elided.
        let bare = tagged(PeerPath::Relay { url: None });
        assert_eq!(bare["kind"], "relay");
        assert!(bare.get("url").is_none(), "elided, not null: {bare}");

        // #64 review: a path kind from a NEWER daemon must degrade to Unknown, not fail the whole
        // row. Without `#[serde(other)]` an unknown `kind` errors out of
        // `PeerReachability` entirely, so one new variant would break every `status` read an
        // older pinned client does.
        let future: PeerPath =
            serde_json::from_value(serde_json::json!({"kind": "quantum", "id": "x"})).unwrap();
        assert_eq!(future, PeerPath::Unknown);
        let row: PeerReachability = serde_json::from_value(serde_json::json!({
            "name": "bob", "reachable": true, "path": {"kind": "quantum"}
        }))
        .expect("an unknown path kind must not fail the whole row");
        assert_eq!(row.path, PeerPath::Unknown);
        assert!(row.reachable, "the rest of the row survives");

        // A pre-#64 row: no `path` key.
        let old = serde_json::json!({"name": "bob", "reachable": true});
        let parsed: PeerReachability = serde_json::from_value(old).unwrap();
        assert_eq!(
            parsed.path,
            PeerPath::Unknown,
            "an older daemon's row must never imply a direct path"
        );
    }

    /// #58: the pushed liveness frame tags as `{"type":"reachability","peer":{…}}` and carries a
    /// whole `PeerReachability` row — the SAME shape the opening snapshot's list holds, so a
    /// consumer projects both through one code path.
    #[test]
    fn reachability_frame_tags_and_round_trips() {
        let frame = StreamFrame::Reachability {
            peer: PeerReachability {
                name: "bob".into(),
                reachable: true,
                rtt_ms: Some(12),
                age_secs: Some(0),
                meta: String::new(),
                principal: Some("eid:beef".into()),
                path: Default::default(),
            },
            source: ReachabilitySource::Probe,
        };
        let v = serde_json::to_value(&frame).unwrap();
        assert_eq!(v["type"], "reachability");
        assert_eq!(v["peer"]["name"], "bob");
        assert_eq!(v["peer"]["reachable"], true);
        assert_eq!(
            v["peer"]["age_secs"], 0,
            "a transition frame is fresh by construction: {v}"
        );
        assert_eq!(v["source"], "probe", "#150: the producer is named: {v}");
        let back: StreamFrame = serde_json::from_value(v).unwrap();
        assert_eq!(back, frame);
    }

    /// #150: the frame's `source` wire shape, and the two ways it must degrade.
    ///
    /// The default is the load-bearing part. An absent key comes from a daemon at `api_minor`
    /// 22–29, which ALREADY has both producers — so it must land on `Unknown`, never on `Probe`.
    /// Defaulting to `Probe` would tell a consumer "a throwaway dial saw this" about frames that
    /// were a live session degrading, which is the ambiguity the field exists to remove.
    /// #140: `known_addrs` absent, empty, and populated are three DIFFERENT wire shapes.
    ///
    /// The distinction is the field's whole reason for existing, and it is carried by
    /// `skip_serializing_if` — so it lives on the wire, not in Rust. An embedder checking
    /// `known_addrs === null` gets `undefined`, which is why the absent case is asserted as a
    /// MISSING KEY rather than a null.
    #[test]
    fn known_addrs_distinguishes_absent_from_empty_on_the_wire() {
        let base = PeerDiagnosticsResult {
            nickname: "jetson".into(),
            principal: "eid:beef".into(),
            hint_usable: true,
            ..Default::default()
        };

        // Absent: the key is OMITTED, never serialized as null.
        let v = serde_json::to_value(&base).unwrap();
        assert!(
            v.get("known_addrs").is_none(),
            "an absent iroh entry elides the key — a consumer must test presence, not null: {v}"
        );

        // Empty: iroh HAS an entry and holds no address. A real, different state.
        let empty = PeerDiagnosticsResult {
            known_addrs: Some(vec![]),
            ..base.clone()
        };
        let v = serde_json::to_value(&empty).unwrap();
        assert_eq!(
            v["known_addrs"],
            serde_json::json!([]),
            "an empty list must survive as `[]` rather than collapsing back to absent: {v}"
        );
        assert_eq!(
            serde_json::from_value::<PeerDiagnosticsResult>(v).unwrap(),
            empty,
            "and round-trip back to Some(vec![]), not None"
        );

        // Populated, with the `active` flag per address.
        let full = PeerDiagnosticsResult {
            known_addrs: Some(vec![
                KnownAddr {
                    addr: "192.168.1.77:4433".into(),
                    active: true,
                },
                KnownAddr {
                    addr: "relay https://r.example:8443".into(),
                    active: false,
                },
            ]),
            hint_addrs_unknown_to_iroh: vec!["10.0.0.9:4433".into()],
            ..base
        };
        let v = serde_json::to_value(&full).unwrap();
        assert_eq!(v["known_addrs"][0]["addr"], "192.168.1.77:4433");
        assert_eq!(v["known_addrs"][0]["active"], true);
        assert_eq!(v["known_addrs"][1]["active"], false);
        assert_eq!(
            v["hint_addrs_unknown_to_iroh"],
            serde_json::json!(["10.0.0.9:4433"])
        );
        assert!(
            v.get("iroh_addrs_not_in_hint").is_none(),
            "an empty difference elides rather than shipping `[]` noise in every capture: {v}"
        );
        assert_eq!(
            serde_json::from_value::<PeerDiagnosticsResult>(v).unwrap(),
            full
        );
    }

    #[test]
    fn reachability_source_tags_and_defaults_to_unknown() {
        let tagged = |s: ReachabilitySource| serde_json::to_value(s).unwrap();
        assert_eq!(tagged(ReachabilitySource::Probe), "probe");
        assert_eq!(tagged(ReachabilitySource::Session), "session");
        assert_eq!(tagged(ReachabilitySource::Unknown), "unknown");
        for s in [
            ReachabilitySource::Probe,
            ReachabilitySource::Session,
            ReachabilitySource::Unknown,
        ] {
            let back: ReachabilitySource = serde_json::from_value(tagged(s)).unwrap();
            assert_eq!(back, s, "round trip");
        }

        let peer = serde_json::json!({"name": "bob", "reachable": true});

        // A pre-#150 frame: no `source` key at all.
        let old: StreamFrame =
            serde_json::from_value(serde_json::json!({"type": "reachability", "peer": peer}))
                .expect("an older daemon's frame must still parse");
        let StreamFrame::Reachability { source, .. } = old else {
            panic!("expected a reachability frame");
        };
        assert_eq!(
            source,
            ReachabilitySource::Unknown,
            "an api_minor 22-29 daemon has BOTH producers, so an absent key must not claim Probe"
        );

        // A producer from a NEWER daemon must degrade to Unknown, not fail the whole frame — the
        // same stake `PeerPath` buys with `#[serde(other)]`. Without the hand-written Deserialize
        // a third producer would break every Reachability frame an older pinned client reads.
        let future: StreamFrame = serde_json::from_value(
            serde_json::json!({"type": "reachability", "peer": peer, "source": "telemetry"}),
        )
        .expect("an unknown producer must not fail the whole frame");
        let StreamFrame::Reachability { source, peer } = future else {
            panic!("expected a reachability frame");
        };
        assert_eq!(source, ReachabilitySource::Unknown);
        assert!(peer.reachable, "the rest of the frame survives");
    }

    /// #148: a defaulted status is EMPTY and honest — the fixture ergonomic an embedder gets in
    /// exchange for us adding fields.
    ///
    /// Its content is the load-bearing part. A downstream test that omits a field must not thereby
    /// assert something: no phantom peers or services, and the optional blocks absent rather than
    /// zeroed. `storage: Some(StorageInfo::default())` would read as "0 bytes on disk", which is a
    /// measurement nobody took.
    #[test]
    fn a_defaulted_status_is_empty_and_claims_nothing() {
        let d = StatusResult::default();
        assert!(d.peers.is_empty() && d.services.is_empty(), "{d:?}");
        assert!(d.reachability.is_empty() && d.presence.is_empty(), "{d:?}");
        assert!(d.recent_pairings.is_empty(), "{d:?}");
        assert_eq!(d.roster, None, "no roster is not an empty roster");
        assert_eq!(d.storage, None, "absent, not 0 bytes — nobody measured");
        assert_eq!(d.self_network, None, "absent, not offline — nobody looked");
        assert_eq!(d.self_user_id, None);
        assert!(
            d.stack_version.is_empty() && d.self_nickname.is_empty(),
            "{d:?}"
        );

        // The pattern the issue actually asks for: additive growth stops breaking fixtures.
        let fixture = StatusResult {
            peers: vec![PeerInfo {
                name: "bob".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(fixture.peers[0].name, "bob");
        assert!(fixture.services.is_empty());

        // A default round-trips, so the elide-vs-null discipline holds for one too.
        let v = serde_json::to_value(&d).unwrap();
        assert!(v.get("roster").is_none(), "elided, not null: {v}");
        assert!(v.get("storage").is_none(), "elided, not null: {v}");
        let back: StatusResult = serde_json::from_value(v).unwrap();
        assert_eq!(back, d);
    }

    /// #148: a defaulted reachability row is NOT reachable and makes NO path claim.
    ///
    /// This is the one default where a wrong choice would be a false guarantee rather than a
    /// harmless placeholder — the same trap `PeerPath`'s `#[default] Unknown` exists to avoid
    /// (#64), now reachable through a second door. A fixture that forgot to set `path` must not
    /// thereby assert the peer was reached directly, and one that forgot `reachable` must not
    /// claim it was up.
    #[test]
    fn a_defaulted_reachability_row_asserts_nothing_about_the_peer() {
        let d = PeerReachability::default();
        assert!(!d.reachable, "an unset row must not claim the peer is up");
        assert_eq!(
            d.path,
            PeerPath::Unknown,
            "an unset path must never read as Direct — that is a privacy claim no one made"
        );
        assert_eq!(d.rtt_ms, None, "no measurement was taken");
        assert_eq!(d.age_secs, None, "never probed");
        assert_eq!(d.principal, None);
        assert!(d.name.is_empty() && d.meta.is_empty());
    }

    /// #148 gate: the REST of the new defaults, which the first pass left entirely unasserted —
    /// moving `BackendKind`'s `#[default]` to `Socket` failed nothing across the whole workspace.
    ///
    /// Each assertion below is the conservative reading of a field that could otherwise let a
    /// fixture assert something by omission.
    #[test]
    fn the_remaining_defaults_are_conservative() {
        let s = ServiceInfo::default();
        assert!(
            s.allow.is_empty(),
            "an unset allow must admit NOBODY — empty is deny (the gate's `any()` is false on an \
             empty list), and a permissive default here would be an authz hole reachable from a \
             fixture"
        );
        assert!(s.allow_display.is_empty() && s.name.is_empty());
        assert!(
            !s.ephemeral,
            "persistent is the conservative reading, and matches the wire default"
        );
        assert_eq!(
            s.backend,
            BackendKind::Run,
            "the documented choice — a convenience, not a claim; pinned so it cannot drift \
             silently out of step with its own rustdoc"
        );
        assert_eq!(BackendKind::default(), BackendKind::Run);

        let p = PeerInfo::default();
        assert!(p.name.is_empty() && p.services.is_empty());
        assert_eq!(p.user_id, None, "no identity was proven");
        assert_eq!(p.principal, None);

        // The gate's finding: this default is the documented "deliberately LAN-only" posture,
        // which the porcelain renders as healthy and NOT as a warning. It is unavoidable (a bool
        // has no third state) but it must stay deliberate, so it is pinned rather than left to
        // be rediscovered by whoever writes the next fixture.
        let n = SelfNetwork::default();
        assert!(!n.online, "no relay connection is established");
        assert!(
            n.relays.is_empty() && n.home_relay.is_none(),
            "and none are known — which the renderer reads as LAN-BY-CONFIGURATION, not as an \
             outage; say 'nobody looked' with StatusResult.self_network: None instead"
        );
        assert_eq!(n.last_change_epoch, None, "no transition was observed");

        let r = RelayInfo::default();
        assert!(
            !r.connected,
            "an unset relay must not claim a live connection"
        );

        let st = StorageInfo::default();
        assert_eq!(
            (st.audit_bytes, st.redb_bytes, st.blobs_bytes),
            (0, 0, 0),
            "zeros read as MEASURED-and-empty; `StatusResult.storage: None` is 'unmeasured'"
        );

        let ro = RosterStatus::default();
        assert!(
            ro.state.is_empty(),
            "not a valid state word, deliberately — `doctor` warns on an unknown state rather \
             than reporting a healthy roster"
        );
        assert_eq!(ro.serial, 0);

        let pp = PresencePeer::default();
        assert!(
            !pp.online,
            "an unset presence row must not claim the device is up"
        );
        assert!(pp.role.is_empty() && pp.user_id.is_empty());

        let rp = RecentPairing::default();
        assert_eq!(rp.paired_at_epoch, 0);
        assert!(rp.sas_code.is_empty(), "no ceremony produced a code");
        assert!(
            !rp.self_enroll,
            "an unset row is an ordinary pairing (#214)"
        );
    }

    /// #214: `RecentPairing.self_enroll` is elided when false (an older client's payload
    /// round-trips) and rides the wire when true — the field an inviter-side SAS panel routes on,
    /// instead of the `"(this person's device)"` prose.
    #[test]
    fn recent_pairing_self_enroll_is_additive_and_defaults_to_false() {
        let legacy: RecentPairing = serde_json::from_value(serde_json::json!({
            "peer_nickname": "bob", "sas_code": "tango-fig-cabbage", "paired_at_epoch": 1
        }))
        .unwrap();
        assert!(
            !legacy.self_enroll,
            "an absent self_enroll is an ordinary pairing"
        );

        let ordinary = RecentPairing {
            peer_nickname: "bob".into(),
            sas_code: "tango-fig-cabbage".into(),
            paired_at_epoch: 1,
            self_enroll: false,
        };
        let v = serde_json::to_value(&ordinary).unwrap();
        assert!(
            v.get("self_enroll").is_none(),
            "false is elided, like every additive bool on this surface: {v}"
        );

        let enrolled = RecentPairing {
            self_enroll: true,
            ..ordinary
        };
        let v = serde_json::to_value(&enrolled).unwrap();
        assert_eq!(v["self_enroll"], true);
        let back: RecentPairing = serde_json::from_value(v).unwrap();
        assert_eq!(back, enrolled);
    }

    /// #214: `StatusResult.self_user_key_held` is additive — absent reads `false` — and the detach
    /// verb has a parameterless tag plus a result whose `user_id` is optional on the wire.
    #[test]
    fn self_user_key_held_and_self_enroll_detach_serialize_additively() {
        let legacy: StatusResult = serde_json::from_value(serde_json::json!({
            "stack_version": "0", "services": [], "peers": []
        }))
        .unwrap();
        assert!(
            !legacy.self_user_key_held,
            "an older daemon omits the field; it must read false rather than fail the parse"
        );
        let v = serde_json::to_value(&legacy).unwrap();
        assert!(
            v.get("self_user_key_held").is_none(),
            "false is elided: {v}"
        );

        let v = serde_json::to_value(&Request::SelfEnrollDetach).unwrap();
        assert_eq!(v["method"], "self_enroll_detach");
        assert!(v.get("params").is_none(), "parameterless: {v}");

        let res = SelfEnrollDetachResult {
            user_id: Some("b64u:mine".into()),
            detached_from: "b64u:theirs".into(),
        };
        let v = serde_json::to_value(&res).unwrap();
        assert_eq!(v["user_id"], "b64u:mine");
        assert_eq!(v["detached_from"], "b64u:theirs");
        assert_eq!(
            serde_json::from_value::<SelfEnrollDetachResult>(v).unwrap(),
            res
        );
        let keyless: SelfEnrollDetachResult =
            serde_json::from_value(serde_json::json!({"detached_from": "b64u:theirs"})).unwrap();
        assert_eq!(keyless.user_id, None);

        assert_eq!(ERR_NOT_ENROLLED, -32058);
        assert_eq!(ERR_SELF_ENROLL_NO_KEY, -32059);
    }

    /// #150 gate: "an unrecognized value reads as `unknown`" must hold for any VALUE, not just an
    /// unrecognized string.
    ///
    /// `#[serde(default)]` covers an absent key and nothing else, so `"source": null` — what a
    /// proxy or non-Rust daemon that normalizes optional fields produces — went through the
    /// deserializer and failed the WHOLE frame, silently dropping a liveness transition while the
    /// protocol doc promised the field could not break a parse. The container shapes matter
    /// separately: a visitor that answers without draining a map/seq desynchronizes the parser and
    /// fails the frame anyway, which looks identical from outside.
    #[test]
    fn a_malformed_source_degrades_instead_of_failing_the_frame() {
        let peer = serde_json::json!({"name": "bob", "reachable": true});
        for bad in [
            serde_json::Value::Null,
            serde_json::json!(7),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!(true),
            serde_json::json!({"kind": "probe", "nested": {"deep": [1, 2]}}),
            serde_json::json!(["probe", "session"]),
        ] {
            let frame: StreamFrame = serde_json::from_value(
                serde_json::json!({"type": "reachability", "peer": peer, "source": bad}),
            )
            .unwrap_or_else(|e| panic!("`source: {bad}` must not fail the whole frame: {e}"));
            let StreamFrame::Reachability { source, peer } = frame else {
                panic!("expected a reachability frame");
            };
            assert_eq!(source, ReachabilitySource::Unknown, "for source: {bad}");
            assert!(peer.reachable, "the rest of the frame survives: {bad}");
        }
    }

    /// #90: the self-network frame tags as `{"type":"self_network","self_network":{…}}` — the
    /// SAME block `status` and the snapshot carry. Pinned explicitly (like the reachability
    /// tag) so a variant rename cannot slip past a suite whose two ends share the type while
    /// breaking every doc-following third-party client.
    #[test]
    fn self_network_frame_tags_and_round_trips() {
        let frame = StreamFrame::SelfNetwork {
            self_network: SelfNetwork {
                online: true,
                home_relay: Some("https://relay.example:443".into()),
                relays: vec![RelayInfo {
                    url: "https://relay.example:443".into(),
                    connected: true,
                }],
                direct_addrs: vec!["192.168.1.2:4444".into()],
                last_change_epoch: Some(1_753_842_000),
                identity_conflict_epoch: None,
                // #89: seeded NON-default so the round-trip actually carries it — an empty value
                // here would round-trip through a `skip_serializing_if` and prove nothing.
                presence_mode: Some("granted".into()),
                // #68: seeded `"resolve"`, deliberately NOT the `"off"` default — a fixture equal
                // to the default would round-trip identically whether or not the field was carried
                // at all, and the assertion below would measure nothing.
                local_discovery: Some("resolve".into()),
            },
        };
        let v = serde_json::to_value(&frame).unwrap();
        assert_eq!(v["type"], "self_network");
        assert_eq!(v["self_network"]["online"], true);
        assert_eq!(v["self_network"]["home_relay"], "https://relay.example:443");
        assert_eq!(v["self_network"]["relays"][0]["connected"], true);
        assert_eq!(
            v["self_network"]["presence_mode"], "granted",
            "#89: the live presence mode must reach the wire — it is the only way an operator can \
             confirm the knob took effect, and a product's privacy switch has nothing to render \
             without it"
        );
        assert_eq!(
            v["self_network"]["local_discovery"], "resolve",
            "#68: the live local-discovery mode must reach the wire — `\"on\"` means this node is \
             multicasting its endpoint id to every device on the link, and a privacy switch has \
             nothing to render without it"
        );
        let back: StreamFrame = serde_json::from_value(v).unwrap();
        assert_eq!(back, frame);
    }

    #[test]
    fn peer_reachability_serde_is_additive() {
        let r = PeerReachability {
            name: "bob".into(),
            reachable: true,
            rtt_ms: Some(42),
            age_secs: Some(3),
            meta: String::new(),
            principal: None,
            path: Default::default(),
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["name"], "bob");
        assert_eq!(v["reachable"], true);
        assert_eq!(v["rtt_ms"], 42);
        assert_eq!(v["age_secs"], 3);
        // Never-probed peer: optionals elided, not null.
        let unknown = PeerReachability {
            name: "carol".into(),
            reachable: false,
            rtt_ms: None,
            age_secs: None,
            meta: String::new(),
            principal: None,
            path: Default::default(),
        };
        let uv = serde_json::to_value(&unknown).unwrap();
        assert!(uv.get("rtt_ms").is_none() && uv.get("age_secs").is_none());
        // An older StatusResult (no reachability field) still deserializes.
        let old = serde_json::json!({"stack_version":"0.1.0","services":[],"peers":[]});
        let s: StatusResult = serde_json::from_value(old).unwrap();
        assert!(s.reachability.is_empty());
    }

    #[test]
    fn subscribe_method_tag_resolves() {
        let req = serde_json::to_value(Request::Subscribe).unwrap();
        assert_eq!(method_of(&req), Some("subscribe"));
    }

    // --- #34: params structs reject unknown fields (the `{service: "kb"}` silent-accept bug) ---

    #[test]
    fn invite_params_reject_singular_service_typo() {
        // The reported bug: `{"service":"kb"}` (singular) used to deserialize to
        // `InviteParams { services: [] }` and mint a grants-nothing invite that looked
        // successful. With deny_unknown_fields the typo is a loud parse error instead.
        let err = serde_json::from_value::<InviteParams>(serde_json::json!({"service": "kb"}));
        assert!(
            err.is_err(),
            "an unknown `service` key must be rejected, not silently ignored"
        );
        // The correct plural shape still parses.
        let ok: InviteParams =
            serde_json::from_value(serde_json::json!({"services": ["kb"]})).unwrap();
        assert_eq!(ok.services, vec!["kb".to_string()]);
    }

    #[test]
    fn open_session_params_reject_unknown_field() {
        let err = serde_json::from_value::<OpenSessionParams>(
            serde_json::json!({"peer": "a", "service": "b", "nonsense": 1}),
        );
        assert!(err.is_err(), "unknown params keys must be rejected");
    }

    #[test]
    fn set_app_metadata_request_carries_the_method_tag() {
        let r = Request::SetAppMetadata(SetAppMetadataParams {
            metadata: "v=1.2.3".into(),
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "set_app_metadata");
        assert_eq!(v["params"]["metadata"], "v=1.2.3");
        assert_eq!(method_of(&v), Some("set_app_metadata"));
    }

    #[test]
    fn set_app_metadata_params_reject_unknown_field() {
        let err = serde_json::from_value::<SetAppMetadataParams>(
            serde_json::json!({"metadata": "x", "nonsense": 1}),
        );
        assert!(err.is_err(), "unknown params keys must be rejected");
    }

    /// `PresencePeer.meta` is additive — an older payload (no meta) still deserializes, and an
    /// empty meta does not serialize.
    #[test]
    fn peer_info_principal_is_additive() {
        // An older payload (no principal) still deserializes; empty does not serialize.
        let old = serde_json::json!({"name": "bob", "services": ["notes"]});
        let p: PeerInfo = serde_json::from_value(old).unwrap();
        assert_eq!(p.principal, None);
        assert!(serde_json::to_value(&p).unwrap().get("principal").is_none());
        // A bound peer carries BOTH the person user_id AND the device principal (#41).
        let full = PeerInfo {
            name: "bob".into(),
            services: vec!["notes".into()],
            user_id: Some("b64u:BOB".into()),
            principal: Some("eid:0707".into()),
        };
        let back: PeerInfo = serde_json::from_value(serde_json::to_value(&full).unwrap()).unwrap();
        assert_eq!(back.user_id.as_deref(), Some("b64u:BOB"));
        assert_eq!(back.principal.as_deref(), Some("eid:0707"));
    }

    #[test]
    fn active_session_principal_is_additive() {
        // An OLD payload (no `principal`) must still deserialize — #73 is additive.
        let old: ActiveSession =
            serde_json::from_str(r#"{"peer":"bob","service":"notes","opened_at":7}"#).unwrap();
        assert_eq!(old.principal, None, "serde(default) supplies it");

        // And a `None` must not serialize, so an old client sees the shape it expects.
        let json = serde_json::to_string(&old).unwrap();
        assert!(
            !json.contains("principal"),
            "skip_serializing_if must omit it: {json}"
        );

        // A real row round-trips the principal.
        let new = ActiveSession {
            peer: "bob".into(),
            service: "notes".into(),
            opened_at: 7,
            principal: Some("eid:1f0a".into()),
        };
        let back: ActiveSession =
            serde_json::from_str(&serde_json::to_string(&new).unwrap()).unwrap();
        assert_eq!(back.principal.as_deref(), Some("eid:1f0a"));
    }

    #[test]
    fn peer_reachability_principal_is_additive() {
        // Older payload (no principal) still deserializes; empty does not serialize; a set
        // value round-trips alongside the #40 meta so an embedder joins on the principal.
        let old = serde_json::json!({"name": "bob", "reachable": true});
        let r: PeerReachability = serde_json::from_value(old).unwrap();
        assert_eq!(r.principal, None);
        assert!(serde_json::to_value(&r).unwrap().get("principal").is_none());
        let full = PeerReachability {
            name: "bob".into(),
            reachable: true,
            rtt_ms: Some(12),
            age_secs: Some(3),
            meta: "v=1.2.3".into(),
            principal: Some("eid:0707".into()),
            path: Default::default(),
        };
        let back: PeerReachability =
            serde_json::from_value(serde_json::to_value(&full).unwrap()).unwrap();
        assert_eq!(back.principal.as_deref(), Some("eid:0707"));
        assert_eq!(back.meta, "v=1.2.3");
    }

    #[test]
    fn peer_reachability_meta_is_additive() {
        // An older payload (no meta) still deserializes; an empty meta does not serialize.
        let old = serde_json::json!({"name": "bob", "reachable": true});
        let r: PeerReachability = serde_json::from_value(old).unwrap();
        assert_eq!(r.meta, "");
        assert!(serde_json::to_value(&r).unwrap().get("meta").is_none());
        // A set value round-trips.
        let with = PeerReachability {
            name: "bob".into(),
            reachable: true,
            rtt_ms: Some(12),
            age_secs: Some(3),
            meta: "v=1.2.3".into(),
            principal: None,
            path: Default::default(),
        };
        let back: PeerReachability =
            serde_json::from_value(serde_json::to_value(&with).unwrap()).unwrap();
        assert_eq!(back.meta, "v=1.2.3");
    }

    #[test]
    fn presence_peer_meta_is_additive() {
        let old = serde_json::json!({
            "user_id": "b64u:A", "device_label": "laptop", "role": "primary", "online": true
        });
        let p: PresencePeer = serde_json::from_value(old).unwrap();
        assert_eq!(p.meta, "");
        assert!(serde_json::to_value(&p).unwrap().get("meta").is_none());
    }

    #[test]
    fn set_nickname_request_carries_the_method_tag() {
        let r = Request::SetNickname(SetNicknameParams {
            nickname: "workbench".into(),
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "set_nickname");
        assert_eq!(v["params"]["nickname"], "workbench");
        assert_eq!(method_of(&v), Some("set_nickname"));
    }

    #[test]
    fn set_nickname_params_reject_unknown_field() {
        let err = serde_json::from_value::<SetNicknameParams>(
            serde_json::json!({"nickname": "x", "nonsense": 1}),
        );
        assert!(err.is_err(), "unknown params keys must be rejected");
    }

    /// An OLDER daemon's status payload (no `self_nickname`) must still deserialize —
    /// the additive-only contract — and an empty name must not serialize at all.
    #[test]
    fn status_self_nickname_is_additive() {
        let old = serde_json::json!({
            "stack_version": "0.7.0", "services": [], "peers": []
        });
        let s: StatusResult = serde_json::from_value(old).unwrap();
        assert_eq!(s.self_nickname, "");
        let v = serde_json::to_value(&s).unwrap();
        assert!(v.get("self_nickname").is_none(), "empty name is skipped");
    }

    #[test]
    fn api_minor_is_present_and_monotonic_from_hello() {
        // #34 part 2: a machine-comparable protocol-compat minor, distinct from the
        // crate/stack version, additive on the Hello frame.
        let h = Hello {
            api: API_NAME.into(),
            api_version: API_VERSION.into(),
            api_minor: API_MINOR,
            stack_version: "9.9.9".into(),
        };
        let v = serde_json::to_value(&h).unwrap();
        assert_eq!(v["api_minor"], API_MINOR);
        // An OLD Hello without api_minor still deserializes (additive contract).
        let old = serde_json::json!({
            "api": API_NAME, "api_version": "1.0", "stack_version": "0.4.0"
        });
        let back: Hello = serde_json::from_value(old).unwrap();
        assert_eq!(back.api_minor, 0, "absent api_minor defaults to 0");
    }

    #[test]
    fn hello_result_roundtrips() {
        let h = Hello {
            api: "mcpmesh-local/1".into(),
            api_version: "1.0".into(),
            api_minor: 0,
            stack_version: "0.1.0".into(),
        };
        let v = serde_json::to_value(&h).unwrap();
        assert_eq!(v["api"], "mcpmesh-local/1");
        let back: Hello = serde_json::from_value(v).unwrap();
        assert_eq!(back, h);
    }

    #[test]
    fn request_tagged_by_method() {
        let r = Request::Status;
        assert_eq!(serde_json::to_value(&r).unwrap()["method"], "status");
        let r = Request::OpenSession(OpenSessionParams {
            peer: "alice".into(),
            service: "notes".into(),
            // #166: seeded NON-default so the round-trip carries it. An absent value would be
            // elided by `skip_serializing_if` and the assertion below would prove nothing.
            idle_timeout_secs: Some(7),
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "open_session");
        assert_eq!(v["params"]["peer"], "alice");
        assert_eq!(v["params"]["idle_timeout_secs"], 7);
        // …and an absent one really is absent, so an older daemon sees the payload it always saw.
        let bare = serde_json::to_value(Request::OpenSession(OpenSessionParams {
            peer: "alice".into(),
            service: "notes".into(),
            idle_timeout_secs: None,
        }))
        .unwrap();
        assert!(bare["params"].get("idle_timeout_secs").is_none());
    }

    #[test]
    fn parameterless_method_tolerates_params_forms() {
        // Omitted and null params deserialize straight into the unit variant.
        let omitted: Request =
            serde_json::from_value(serde_json::json!({"method": "status"})).unwrap();
        assert_eq!(omitted, Request::Status);
        let null: Request =
            serde_json::from_value(serde_json::json!({"method": "status", "params": null}))
                .unwrap();
        assert_eq!(null, Request::Status);

        // Known limitation: adjacent tagging rejects `params:{}` for a unit variant, so
        // the server MUST dispatch on the method string rather than deserialize the whole
        // message into `Request`. This is the pattern the daemon's dispatcher uses.
        let empty = serde_json::json!({"method": "status", "params": {}});
        assert!(serde_json::from_value::<Request>(empty.clone()).is_err());
        match method_of(&empty) {
            Some("status") => {} // dispatcher resolves Status via the method string
            other => panic!("method_of failed to resolve status: {other:?}"),
        }
    }

    #[test]
    fn backend_spec_roundtrips() {
        let run = BackendSpec::Run {
            cmd: vec!["notes-mcp".into(), "--stdio".into()],
            env: Default::default(),
            cwd: None,
        };
        let v = serde_json::to_value(&run).unwrap();
        assert_eq!(v["run"]["cmd"][0], "notes-mcp");
        assert_eq!(serde_json::from_value::<BackendSpec>(v).unwrap(), run);

        let sock = BackendSpec::Socket {
            path: "/run/notes.sock".into(),
        };
        let v = serde_json::to_value(&sock).unwrap();
        assert_eq!(v["socket"]["path"], "/run/notes.sock");
        assert_eq!(serde_json::from_value::<BackendSpec>(v).unwrap(), sock);
    }

    #[test]
    fn register_service_wire_shape() {
        let r = Request::RegisterService(RegisterServiceParams {
            name: "notes".into(),
            backend: BackendSpec::Run {
                cmd: vec!["notes-mcp".into()],
                env: Default::default(),
                cwd: None,
            },
            allow: vec!["alice".into()],
            ephemeral: false,
            rate_limit_per_min: None,
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "method": "register_service",
                "params": {
                    "name": "notes",
                    "backend": {"run": {"cmd": ["notes-mcp"]}},
                    "allow": ["alice"],
                }
            })
        );
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), r);
    }

    #[test]
    fn invite_request_and_result_roundtrip() {
        // Request::Invite → `{ "method": "invite", "params": { "services": [...] } }`.
        let r = Request::Invite(InviteParams {
            services: vec!["notes".into(), "kb".into()],
            app_label: None,
            max_uses: None,
            // #87: seeded NON-None so the round-trip actually carries it — `None` rides
            // `skip_serializing_if` straight past the assertion and proves nothing.
            peer_nickname: Some("laptop-of-alice".into()),
            as_self: false,
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "invite");
        assert_eq!(
            v["params"]["peer_nickname"], "laptop-of-alice",
            "#87: the inviter's local alias for the redeemer must reach the wire"
        );
        assert_eq!(v["params"]["services"][0], "notes");
        assert_eq!(v["params"]["services"][1], "kb");
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), r);
        // method_of resolves the tag generically (no per-variant arm).
        assert_eq!(
            method_of(&serde_json::json!({"method": "invite", "params": {"services": []}})),
            Some("invite")
        );

        // InviteResult carries the copyable line + expiry (surface #2 pairing artifact).
        let res = InviteResult {
            invite_line: "mcpmesh-invite:ABCDEF".into(),
            expires_at_epoch: 1_800_000_000,
            uses_remaining: 1,
        };
        let v = serde_json::to_value(&res).unwrap();
        assert_eq!(v["invite_line"], "mcpmesh-invite:ABCDEF");
        assert_eq!(v["expires_at_epoch"], 1_800_000_000u64);
        assert_eq!(serde_json::from_value::<InviteResult>(v).unwrap(), res);
    }

    #[test]
    fn pair_request_and_result_roundtrip() {
        // Request::Pair → `{ "method": "pair", "params": { "invite_line": "..." } }`.
        let r = Request::Pair(PairParams {
            invite_line: "mcpmesh-invite:ABCDEF".into(),
            as_nickname: Some("alice-mbp".into()),
            allow_self_enroll: true,
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "pair");
        assert_eq!(v["params"]["invite_line"], "mcpmesh-invite:ABCDEF");
        assert_eq!(
            v["params"]["as_nickname"], "alice-mbp",
            "#87: the redeemer's local alias for the inviter must reach the wire"
        );
        assert_eq!(
            v["params"]["allow_self_enroll"], true,
            "#178: the caller's consent to a self-enrollment must reach the wire — the daemon \
             refuses the ceremony without it"
        );
        // An OLD caller's payload — no alias — must still decode. The field is additive.
        let legacy: PairParams =
            serde_json::from_value(serde_json::json!({"invite_line": "x"})).unwrap();
        assert_eq!(legacy.as_nickname, None);
        // #178: and the consent defaults to REFUSING. A caller that predates the field, or one that
        // simply never set it, must not be read as having offered a device enrollment — that is the
        // whole guard, and a `#[serde(default)]` flipping to `true` would silently remove it.
        assert!(
            !legacy.allow_self_enroll,
            "an absent allow_self_enroll must default to false (refuse), never to true"
        );
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), r);
        // method_of resolves the tag generically (no per-variant arm).
        assert_eq!(
            method_of(&serde_json::json!({"method": "pair", "params": {"invite_line": "x"}})),
            Some("pair")
        );

        // PairResult carries the inviter's suggested nickname + the display-only SAS words +
        // the granted services (the porcelain renders each as `<peer>/<service>`).
        let res = PairResult {
            peer_nickname: "alice".into(),
            sas_code: "tango-fig-cabbage".into(),
            services: vec!["notes".into(), "kb".into()],
            app_label: None,
            peer_user_id: None,
            enrolled_as_self: false,
        };
        let v = serde_json::to_value(&res).unwrap();
        assert_eq!(v["peer_nickname"], "alice");
        assert_eq!(v["sas_code"], "tango-fig-cabbage");
        assert_eq!(v["services"][0], "notes");
        assert_eq!(v["services"][1], "kb");
        assert_eq!(serde_json::from_value::<PairResult>(v).unwrap(), res);

        // Additive-only: a PairResult minted by an older daemon (no `services` key) still
        // deserializes — the `#[serde(default)]` fills it with an empty list.
        let old_shape = serde_json::json!({
            "peer_nickname": "alice",
            "sas_code": "tango-fig-cabbage",
        });
        let back: PairResult = serde_json::from_value(old_shape).unwrap();
        assert_eq!(back.peer_nickname, "alice");
        assert!(back.services.is_empty());
    }

    #[test]
    fn roster_install_request_and_result_roundtrip() {
        // Request::RosterInstall → `{ "method": "roster_install", "params": { "path": ...,
        // "org_root_pk": ... } }`. The optional pk is present on the first-install shape.
        let r = Request::RosterInstall(RosterInstallParams {
            path: "/tmp/roster.json".into(),
            org_root_pk: Some("b64u:AAAA".into()),
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "roster_install");
        assert_eq!(v["params"]["path"], "/tmp/roster.json");
        assert_eq!(v["params"]["org_root_pk"], "b64u:AAAA");
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), r);
        // method_of resolves the tag generically (no per-variant arm).
        assert_eq!(
            method_of(&serde_json::json!({"method": "roster_install", "params": {"path": "/x"}})),
            Some("roster_install")
        );

        // When the pk is omitted (a subsequent install using the pinned value), it is
        // `skip_serializing_if`-dropped from the wire and deserializes back to `None`.
        let omit = Request::RosterInstall(RosterInstallParams {
            path: "/tmp/roster.json".into(),
            org_root_pk: None,
        });
        let v = serde_json::to_value(&omit).unwrap();
        assert!(
            v["params"].get("org_root_pk").is_none(),
            "an omitted org_root_pk must not appear on the wire: {v}"
        );
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), omit);

        // RosterInstallResult carries org_id + serial + severed count (roster-status vocabulary).
        let res = RosterInstallResult {
            org_id: "acme".into(),
            serial: 42,
            severed: 1,
        };
        let v = serde_json::to_value(&res).unwrap();
        assert_eq!(v["org_id"], "acme");
        assert_eq!(v["serial"], 42u64);
        assert_eq!(v["severed"], 1u32);
        assert_eq!(
            serde_json::from_value::<RosterInstallResult>(v).unwrap(),
            res
        );

        // Additive-only: a result minted by an older daemon (no `severed` key) still
        // deserializes — the `#[serde(default)]` fills it with 0.
        let old_shape = serde_json::json!({ "org_id": "acme", "serial": 7 });
        let back: RosterInstallResult = serde_json::from_value(old_shape).unwrap();
        assert_eq!(back.serial, 7);
        assert_eq!(back.severed, 0);
    }

    #[test]
    fn org_join_request_and_result_roundtrip() {
        // Request::OrgJoin → `{ "method": "org_join", "params": { org_id, org_root_pk, user_id,
        // user_key } }`. `user_key` is a LOCAL path string (the key never crosses the API).
        let r = Request::OrgJoin(OrgJoinParams {
            org_id: "acme".into(),
            org_root_pk: "b64u:AAAA".into(),
            user_id: "alice".into(),
            user_key: "/home/alice/.config/mcpmesh/user.key".into(),
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "org_join");
        assert_eq!(v["params"]["org_id"], "acme");
        assert_eq!(v["params"]["org_root_pk"], "b64u:AAAA");
        assert_eq!(v["params"]["user_id"], "alice");
        assert_eq!(
            v["params"]["user_key"],
            "/home/alice/.config/mcpmesh/user.key"
        );
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), r);
        // method_of resolves the tag generically (no per-variant arm).
        assert_eq!(
            method_of(&serde_json::json!({"method": "org_join", "params": {"org_id": "x"}})),
            Some("org_join")
        );

        // OrgJoinResult echoes the pinned org id (surface-clean; the fingerprint is porcelain-side).
        let res = OrgJoinResult {
            org_id: "acme".into(),
            restart_required: true,
        };
        let v = serde_json::to_value(&res).unwrap();
        assert_eq!(v["org_id"], "acme");
        assert_eq!(
            v["restart_required"], true,
            "#93: a half-live join must reach the wire — the caller cannot detect it any other way"
        );
        // Additive: an older daemon omits it, and absent reads as `false` — which was that
        // daemon's implicit answer. Pinned so the default cannot silently flip to `true` and start
        // telling every caller to restart.
        let legacy: OrgJoinResult =
            serde_json::from_value(serde_json::json!({"org_id": "acme"})).unwrap();
        assert!(!legacy.restart_required);
        // …and the false case must not bloat the payload.
        let quiet = serde_json::to_value(OrgJoinResult {
            org_id: "acme".into(),
            restart_required: false,
        })
        .unwrap();
        assert!(quiet.get("restart_required").is_none());
        assert_eq!(serde_json::from_value::<OrgJoinResult>(v).unwrap(), res);
    }

    #[test]
    fn set_roster_url_request_roundtrip() {
        // Request::SetRosterUrl → `{ "method": "set_roster_url", "params": { "url": "..." } }`.
        let r = Request::SetRosterUrl(SetRosterUrlParams {
            url: "https://intranet.acme.com/roster.json".into(),
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "set_roster_url");
        assert_eq!(v["params"]["url"], "https://intranet.acme.com/roster.json");
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), r);
        assert_eq!(
            method_of(&serde_json::json!({"method": "set_roster_url", "params": {"url": "x"}})),
            Some("set_roster_url")
        );
    }

    #[test]
    fn peer_remove_request_roundtrip() {
        // Request::PeerRemove → `{ "method": "peer_remove", "params": { "nickname": "..." } }`.
        let r = Request::PeerRemove(PeerRemoveParams {
            nickname: "bob".into(),
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "peer_remove");
        assert_eq!(v["params"]["nickname"], "bob");
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), r);
        // method_of resolves the tag generically (no per-variant arm).
        assert_eq!(
            method_of(&serde_json::json!({"method": "peer_remove", "params": {"nickname": "bob"}})),
            Some("peer_remove")
        );
    }

    /// The reserved/internal `peer_add` rides the SAME typed vocabulary as every other method —
    /// `{ "method": "peer_add", "params": { nickname, endpoint_id, allow } }` — with `allow`
    /// defaulting to empty when absent.
    /// #65: the wire tags for the introduction pair. The serde tag must equal the dispatch string
    /// the daemon matches on — nothing else checks that they agree.
    #[test]
    fn peer_introduce_and_endorse_roundtrip() {
        let r = Request::PeerIntroduce(PeerIntroduceParams {
            subject: "eid:aa".into(),
            endorsed_by: "b64u:carol".into(),
            evidence: "b64u:sig".into(),
            subject_user_id: Some("b64u:bob".into()),
            subject_binding: Some("b64u:bind".into()),
            nickname: "bob".into(),
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(
            v["method"], "peer_introduce",
            "the tag must match the daemon's dispatch string exactly"
        );
        assert_eq!(v["params"]["subject"], "eid:aa");
        assert_eq!(v["params"]["subject_binding"], "b64u:bind");
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), r);

        // The two proof fields are OPTIONAL on the wire and omitted when absent.
        let minimal = Request::PeerIntroduce(PeerIntroduceParams {
            subject: "eid:aa".into(),
            endorsed_by: "b64u:carol".into(),
            evidence: "b64u:sig".into(),
            subject_user_id: None,
            subject_binding: None,
            nickname: "bob".into(),
        });
        let v = serde_json::to_value(&minimal).unwrap();
        assert!(v["params"].get("subject_user_id").is_none());
        assert!(v["params"].get("subject_binding").is_none());
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), minimal);

        let e = Request::PeerEndorse(PeerEndorseParams {
            subject: "eid:aa".into(),
            subject_user_id: None,
        });
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["method"], "peer_endorse");
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), e);

        let res = PeerEndorseResult {
            endorsed_by: "b64u:me".into(),
            evidence: "b64u:sig".into(),
        };
        let v = serde_json::to_value(&res).unwrap();
        assert_eq!(v["endorsed_by"], "b64u:me");
        assert_eq!(serde_json::from_value::<PeerEndorseResult>(v).unwrap(), res);
    }

    #[test]
    fn peer_add_request_roundtrip() {
        let r = Request::PeerAdd(PeerAddParams {
            nickname: "bob".into(),
            endpoint_id: "96246d3f".into(),
            allow: vec!["notes".into()],
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "peer_add");
        assert_eq!(v["params"]["nickname"], "bob");
        assert_eq!(v["params"]["endpoint_id"], "96246d3f");
        assert_eq!(v["params"]["allow"][0], "notes");
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), r);
        // An absent allow list deserializes to empty (the server-side tolerance).
        let p: PeerAddParams =
            serde_json::from_value(serde_json::json!({"nickname": "bob", "endpoint_id": "x"}))
                .unwrap();
        assert!(p.allow.is_empty());
    }

    #[test]
    fn peer_rename_request_roundtrip() {
        // By user_id (renames all of a person's devices in one op).
        let r = Request::PeerRename(PeerRenameParams {
            user_id: Some("b64u:BOB".into()),
            nickname: None,
            to: "Bobby".into(),
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "peer_rename");
        assert_eq!(v["params"]["user_id"], "b64u:BOB");
        assert_eq!(v["params"]["to"], "Bobby");
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), r);
        // A provisional contact is renamed by nickname; omitted user_id defaults to None.
        assert_eq!(
            method_of(
                &serde_json::json!({"method": "peer_rename", "params": {"nickname": "carol", "to": "Carol"}})
            ),
            Some("peer_rename")
        );
    }

    #[test]
    fn status_result_roundtrips() {
        // Pure-pairing daemon: `roster` is None — absent from the wire (skip_serializing_if) and an
        // older payload with no `roster` key still deserializes to None (serde default).
        let s = StatusResult {
            stack_version: "0.1.0".into(),
            services: vec![ServiceInfo {
                name: "notes".into(),
                allow: vec!["alice".into()],
                allow_display: vec![],
                backend: BackendKind::Run,
                ephemeral: false,
            }],
            peers: vec![PeerInfo {
                name: "alice".into(),
                services: vec!["notes".into()],
                // A paired peer that proved a self-sovereign user_id at pairing (surface-clean id).
                user_id: Some("b64u:alicepk".into()),
                principal: None,
            }],
            roster: None,
            presence: vec![],
            self_user_id: Some("b64u:selfpk".into()),
            self_user_key_held: true,
            recent_pairings: vec![],
            reachability: vec![],
            self_nickname: String::new(),
            storage: None,
            revoked: Vec::new(),
            self_network: None,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["services"][0]["backend"], "run");
        // The additive identity fields ride the wire when present.
        assert_eq!(v["peers"][0]["user_id"], "b64u:alicepk");
        assert_eq!(v["self_user_id"], "b64u:selfpk");
        assert_eq!(
            v["self_user_key_held"], true,
            "#214: rides the wire when set"
        );
        assert!(
            v.get("roster").is_none(),
            "an absent roster must not appear on the wire: {v}"
        );
        assert!(
            v.get("presence").is_none(),
            "an empty presence must not appear on the wire: {v}"
        );
        assert!(
            v.get("recent_pairings").is_none(),
            "an empty recent_pairings must not appear on the wire: {v}"
        );
        assert_eq!(serde_json::from_value::<StatusResult>(v).unwrap(), s);

        // A payload minted by an older daemon (no `roster`/`presence`/identity keys) still
        // deserializes — the identity fields default to None / a nickname-only peer.
        let old_shape = serde_json::json!({
            "stack_version": "0.1.0",
            "services": [],
            "peers": [{ "name": "bob", "services": [] }],
        });
        let back: StatusResult = serde_json::from_value(old_shape).unwrap();
        assert!(back.roster.is_none());
        assert!(back.presence.is_empty());
        assert!(back.self_user_id.is_none());
        assert!(back.peers[0].user_id.is_none());
        assert!(back.recent_pairings.is_empty());

        // Roster daemon: a Some(RosterStatus) + an advisory presence list round-trip. `presence`
        // carries FLAT vocabulary only (user_id/device_label/role/online) — no EndpointId/key.
        let s = StatusResult {
            stack_version: "0.1.0".into(),
            services: vec![],
            peers: vec![],
            roster: Some(RosterStatus {
                org_id: "acme".into(),
                serial: 42,
                state: "approved".into(),
                org_root_fingerprint: "tango-fig-cabbage-anchor".into(),
                groups: vec!["eng".into(), "ops".into()],
            }),
            presence: vec![
                PresencePeer {
                    user_id: "alice".into(),
                    display_name: "Alice Example".into(),
                    groups: vec!["eng".into()],
                    device_label: "laptop".into(),
                    role: "primary".into(),
                    online: true,
                    meta: String::new(),
                },
                PresencePeer {
                    user_id: "alice".into(),
                    display_name: "Alice Example".into(),
                    groups: vec!["eng".into()],
                    device_label: "desktop".into(),
                    role: "mirror".into(),
                    online: false,
                    meta: String::new(),
                },
            ],
            self_user_id: None,
            self_user_key_held: false,
            recent_pairings: vec![],
            reachability: vec![],
            self_nickname: String::new(),
            storage: None,
            revoked: Vec::new(),
            self_network: None,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["roster"]["org_id"], "acme");
        assert_eq!(v["roster"]["serial"], 42u64);
        assert_eq!(v["roster"]["state"], "approved");
        assert_eq!(
            v["roster"]["org_root_fingerprint"],
            "tango-fig-cabbage-anchor"
        );
        assert_eq!(v["presence"][0]["user_id"], "alice");
        assert_eq!(v["presence"][0]["device_label"], "laptop");
        assert_eq!(v["presence"][0]["role"], "primary");
        assert_eq!(v["presence"][0]["online"], true);
        assert_eq!(v["presence"][1]["online"], false);
        assert_eq!(serde_json::from_value::<StatusResult>(v).unwrap(), s);
    }

    /// The `recent_pairings` status field is ADDITIVE: a populated list round-trips with
    /// the flat `{peer_nickname, sas_code, paired_at_epoch}` shape (nickname + SAS words + epoch —
    /// never an EndpointId), an empty list is dropped from the wire, and a payload minted by an
    /// older daemon (no key at all) still deserializes to empty.
    #[test]
    fn recent_pairings_are_additive_on_status() {
        let s = StatusResult {
            stack_version: "0.1.0".into(),
            services: vec![],
            peers: vec![],
            roster: None,
            presence: vec![],
            self_user_id: None,
            self_user_key_held: false,
            recent_pairings: vec![RecentPairing {
                peer_nickname: "bob".into(),
                sas_code: "tango-fig-cabbage".into(),
                paired_at_epoch: 1_800_000_000,
                self_enroll: false,
            }],
            reachability: vec![],
            self_nickname: String::new(),
            storage: None,
            revoked: Vec::new(),
            self_network: None,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["recent_pairings"][0]["peer_nickname"], "bob");
        assert_eq!(v["recent_pairings"][0]["sas_code"], "tango-fig-cabbage");
        assert_eq!(v["recent_pairings"][0]["paired_at_epoch"], 1_800_000_000u64);
        assert_eq!(serde_json::from_value::<StatusResult>(v).unwrap(), s);

        // A payload minted by an OLDER daemon (no `recent_pairings` key) still deserializes —
        // the `#[serde(default)]` fills it with an empty list.
        let old_shape = serde_json::json!({
            "stack_version": "0.1.0",
            "services": [],
            "peers": [],
        });
        let back: StatusResult = serde_json::from_value(old_shape).unwrap();
        assert!(back.recent_pairings.is_empty());
    }

    #[test]
    fn blob_requests_and_results_roundtrip() {
        // BlobPublish → { method, params: { scope, path } }.
        let r = Request::BlobPublish(BlobPublishParams {
            scope: "docs".into(),
            path: "/tmp/a.bin".into(),
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "blob_publish");
        assert_eq!(v["params"]["scope"], "docs");
        assert_eq!(v["params"]["path"], "/tmp/a.bin");
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), r);

        // BlobGrant → { method, params: { scope, principal } }.
        // #62: the two withdrawal verbs' wire tags. A wrong dispatch string or a swapped param
        // would otherwise ship undetected — the e2e test calls the provider directly and never
        // crosses JSON-RPC.
        let rev = Request::BlobRevoke(BlobRevokeParams {
            scope: "photos".into(),
            principals: vec!["alice".into()],
        });
        let v = serde_json::to_value(&rev).unwrap();
        assert_eq!(v["method"], "blob_revoke");
        assert_eq!(v["params"]["principals"][0], "alice");
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), rev);

        let unp = Request::BlobUnpublish(BlobUnpublishParams {
            scope: "photos".into(),
            hash: "abc123".into(),
        });
        let v = serde_json::to_value(&unp).unwrap();
        assert_eq!(v["method"], "blob_unpublish");
        assert_eq!(v["params"]["hash"], "abc123");
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), unp);

        let r = Request::BlobGrant(BlobGrantParams {
            scope: "docs".into(),
            principal: "alice".into(),
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "blob_grant");
        assert_eq!(v["params"]["principal"], "alice");
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), r);

        // BlobList is parameterless (method_of resolves it).
        assert_eq!(
            method_of(&serde_json::json!({"method": "blob_list"})),
            Some("blob_list")
        );

        // BlobFetch → { method, params: { ticket, dest_path, from? } }.
        let r = Request::BlobFetch(BlobFetchParams {
            ticket: "blobAAA".into(),
            dest_path: "/tmp/out.bin".into(),
            from: vec!["eid:aa".into(), "b64u:bb".into()],
        });
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["method"], "blob_fetch");
        assert_eq!(v["params"]["ticket"], "blobAAA");
        assert_eq!(v["params"]["dest_path"], "/tmp/out.bin");
        assert_eq!(
            v["params"]["from"][0], "eid:aa",
            "#83: the alternate sources must reach the wire IN ORDER — the publisher is tried \
             first and these follow, so a reordering changes which source answers"
        );
        assert_eq!(v["params"]["from"][1], "b64u:bb");
        assert_eq!(serde_json::from_value::<Request>(v).unwrap(), r);
        // Additive: an older caller omits it, and absent reads as EMPTY — the single-source
        // behaviour. A default that invented sources would dial peers the caller never named.
        let legacy: BlobFetchParams =
            serde_json::from_value(serde_json::json!({"ticket": "x", "dest_path": "/tmp/y"}))
                .unwrap();
        assert!(legacy.from.is_empty());
        // …and an empty list must not bloat the payload.
        let quiet = serde_json::to_value(Request::BlobFetch(BlobFetchParams {
            ticket: "x".into(),
            dest_path: "/tmp/y".into(),
            from: vec![],
        }))
        .unwrap();
        assert!(quiet["params"].get("from").is_none());

        // BlobPublishResult carries the ticket + hash (blob-reference vocabulary).
        let res = BlobPublishResult {
            ticket: "blobAAA".into(),
            hash: "ab".repeat(32),
        };
        let v = serde_json::to_value(&res).unwrap();
        assert_eq!(v["ticket"], "blobAAA");
        assert_eq!(serde_json::from_value::<BlobPublishResult>(v).unwrap(), res);

        // BlobScopeList carries flat (name, hashes, grants) — no EndpointId/key leakage.
        let res = BlobScopeList {
            scopes: vec![ScopeInfo {
                name: "docs".into(),
                hashes: vec!["ab".repeat(32)],
                grants: vec!["alice".into()],
                withdrawn: vec![],
                hash_count: 1,
                grant_count: 1,
                withdrawn_count: 0,
            }],
            total: 1,
            truncated: false,
        };
        let v = serde_json::to_value(&res).unwrap();
        assert_eq!(v["scopes"][0]["name"], "docs");
        assert_eq!(v["scopes"][0]["grants"][0], "alice");
        assert_eq!(serde_json::from_value::<BlobScopeList>(v).unwrap(), res);

        // BlobFetchResult carries the verified hash + byte length.
        let res = BlobFetchResult {
            hash: "ab".repeat(32),
            bytes_len: 4194304,
        };
        let v = serde_json::to_value(&res).unwrap();
        assert_eq!(v["bytes_len"], 4194304u64);
        assert_eq!(serde_json::from_value::<BlobFetchResult>(v).unwrap(), res);
    }

    /// The three `subscribe` frame shapes round-trip with the documented `type`-tagged wire form
    /// (docs/local-protocol.md "Live event stream"): `snapshot` carries the flat session/reachability
    /// lists, `event` delegates through the `Box` so the record's fields sit VERBATIM under
    /// `record` (one schema with the JSONL log), and `lagged` carries the dropped count.
    #[test]
    fn stream_frames_roundtrip_with_the_documented_tags() {
        let snap = StreamFrame::Snapshot {
            self_network: None,
            active_sessions: vec![ActiveSession {
                peer: "bob".into(),
                service: "notes".into(),
                opened_at: 1_751_760_000,
                principal: None,
            }],
            reachability: vec![PeerReachability {
                name: "bob".into(),
                reachable: true,
                rtt_ms: Some(42),
                age_secs: Some(3),
                meta: String::new(),
                principal: None,
                path: Default::default(),
            }],
        };
        let v = serde_json::to_value(&snap).unwrap();
        assert_eq!(v["type"], "snapshot");
        assert_eq!(v["active_sessions"][0]["peer"], "bob");
        assert_eq!(v["active_sessions"][0]["opened_at"], 1_751_760_000i64);
        assert_eq!(v["reachability"][0]["name"], "bob");
        assert_eq!(serde_json::from_value::<StreamFrame>(v).unwrap(), snap);

        let event = StreamFrame::Event {
            record: Box::new(AuditRecord::session_open(
                "2026-07-03T14:02:11.480Z".into(),
                Some("bob".into()),
                "notes".into(),
                None,
            )),
        };
        let v = serde_json::to_value(&event).unwrap();
        assert_eq!(v["type"], "event");
        // The record's fields ride verbatim under `record` — no Box indirection on the wire.
        assert_eq!(v["record"]["kind"], "session_open");
        assert_eq!(v["record"]["peer"], "bob");
        assert_eq!(v["record"]["service"], "notes");
        assert_eq!(serde_json::from_value::<StreamFrame>(v).unwrap(), event);

        // #167 ask 2: the resume frame's wire shape. A consumer dispatches on `type`, so the tag
        // is part of the contract, and `at_epoch` is signed on purpose — the wire carries epoch
        // seconds as i64 everywhere else in this file.
        let resumed = StreamFrame::Resumed {
            suspended_secs: 7200,
            at_epoch: 1_700_007_202,
        };
        let v = serde_json::to_value(&resumed).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "type": "resumed",
                "suspended_secs": 7200,
                "at_epoch": 1_700_007_202i64,
            })
        );
        assert_eq!(serde_json::from_value::<StreamFrame>(v).unwrap(), resumed);

        let lagged = StreamFrame::Lagged { dropped: 12 };
        let v = serde_json::to_value(&lagged).unwrap();
        assert_eq!(v, serde_json::json!({ "type": "lagged", "dropped": 12 }));
        assert_eq!(serde_json::from_value::<StreamFrame>(v).unwrap(), lagged);
    }

    /// A frame minted by a NEWER daemon (an unknown `type`) fails to deserialize rather than
    /// mis-parsing — the typed stream surface is closed; a forward-compatible consumer reads the
    /// raw `Value` stream instead (`ControlClient::open_stream`).
    #[test]
    fn unknown_stream_frame_type_is_rejected() {
        let future = serde_json::json!({ "type": "future_kind", "x": 1 });
        assert!(serde_json::from_value::<StreamFrame>(future).is_err());
    }

    #[test]
    fn audit_summary_request_and_result_roundtrip() {
        // Request::AuditSummary is parameterless → `{ "method": "audit_summary" }`. Like Status, it
        // tolerates omitted/null params; the server dispatches on the method string (method_of).
        let r = Request::AuditSummary;
        assert_eq!(serde_json::to_value(&r).unwrap()["method"], "audit_summary");
        assert_eq!(
            method_of(&serde_json::json!({"method": "audit_summary"})),
            Some("audit_summary")
        );

        // AuditSummaryResult carries LOCAL per-peer / per-service session counts (nicknames + service
        // names only — never endpoints/transport terms) + a total. Tuples mirror kb's
        // InsightResponse.per_peer_contribution: `["bob", 2]` on the wire.
        let res = AuditSummaryResult {
            per_peer: vec![("alice".into(), 1), ("bob".into(), 2)],
            per_service: vec![("kb".into(), 1), ("notes".into(), 3)],
            total_sessions: 4,
        };
        let v = serde_json::to_value(&res).unwrap();
        assert_eq!(v["per_peer"][1][0], "bob");
        assert_eq!(v["per_peer"][1][1], 2u64);
        assert_eq!(v["per_service"][1][0], "notes");
        assert_eq!(v["total_sessions"], 4u64);
        assert_eq!(
            serde_json::from_value::<AuditSummaryResult>(v).unwrap(),
            res
        );

        // Additive-only: a result minted by an older daemon (no `total_sessions` key) still
        // deserializes — the `#[serde(default)]` fills it with 0.
        let old_shape = serde_json::json!({ "per_peer": [], "per_service": [] });
        let back: AuditSummaryResult = serde_json::from_value(old_shape).unwrap();
        assert_eq!(back.total_sessions, 0);
        assert!(back.per_peer.is_empty());
    }
}
