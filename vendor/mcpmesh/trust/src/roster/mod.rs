//! The `mcpmesh-roster/1` document: pure schema types + the `b64u:` codec + the
//! typed [`RosterError`]. Crypto (JCS + Ed25519) and validation + the resolvable `RosterView`
//! live in the sibling `sign`/`validate` modules. Net-free and redb-free by design —
//! this is the trust DOMAIN; the daemon PLUMBING (gates, persistence, hot-swap) lives with the
//! daemon.

use serde::{Deserialize, Serialize};

pub mod mutate;
pub mod sign;
pub mod validate;

/// The only `format` value this version accepts.
pub const ROSTER_FORMAT: &str = "mcpmesh-roster/1";
/// The format a roster carrying a ROTATION bridge declares (#93 ask c).
///
/// The struct doc below is explicit that additive fields on this document are a format bump, never
/// a silent `#[serde(default)]` — and 0.47.0's first cut added the successor pair without one. The
/// gate proved the cost: `deny_unknown_fields` means a rotated roster does not PARSE on an older
/// binary, and because the bridge rides every subsequent roster, that member is partitioned
/// permanently — its poll loop retrying forever against a document claiming a format it does
/// support. That is verbatim the "90 days later, nobody connects cause to effect" failure #93 opens
/// with, reintroduced by the fix for it.
///
/// Declaring `/2` does not make an old binary able to verify a rotated roster — nothing could, it
/// has never seen the new key. What it buys is a LEGIBLE refusal: `unexpected roster format
/// "mcpmesh-roster/2"` instead of `unknown field successor_root_pk`, which tells an operator to
/// upgrade rather than to go looking for corruption.
///
/// Emitted ONLY by a rotation. An org that never rotates keeps producing `/1` byte-identically.
pub const ROSTER_FORMAT_ROTATION: &str = "mcpmesh-roster/2";
/// The scheme prefix on every key/id/signature string in the schema (`"b64u:…"`).
pub const B64U_PREFIX: &str = "b64u:";
/// Clock-skew tolerance for the validity window (rule 3, ±10 min).
pub const SKEW_SECS: i64 = 10 * 60;

/// A signed org roster. Field order + names are the wire contract — do NOT rename;
/// additive fields are a format bump (`mcpmesh-roster/2`, a distinct `format` value + a distinct
/// parse — validation checks the value), never a silent `#[serde(default)]` on THIS security
/// document (unlike the local-api additive convention). `#[serde(deny_unknown_fields)]` makes the
/// parse strict: `mcpmesh-roster/1` is a CLOSED schema, so an unknown field is REJECTED at
/// deserialize rather than silently dropped — the verified canonical form (signing canonicalizes
/// the re-serialized struct) is therefore exactly this closed field set, with no ambiguity about
/// dropped input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roster {
    pub format: String,
    pub org_id: String,
    pub serial: u64,
    pub issued_at: String,  // RFC3339 (parsed in validate::rule 3)
    pub expires_at: String, // RFC3339
    pub groups: Vec<String>,
    pub users: Vec<RosterUser>,
    pub revoked_endpoints: Vec<String>, // each `b64u:<endpoint_id>`
    /// ORG ROOT ROTATION (#93 ask c): the root that supersedes the one this roster is signed by,
    /// `b64u:<ed25519 pk>`. Absent on every roster of an org that has never rotated.
    ///
    /// Carried on EVERY roster after a rotation, not only the one that introduces it — that is the
    /// whole point. A node validates against one pinned key, so if the successor appeared only in
    /// the announcing roster, a node offline for that single publication would find every later
    /// roster unverifiable and be stranded exactly as it is today. Being offline for one
    /// publication is a laptop closed for a week.
    ///
    /// `#[serde(default)]` + `skip_serializing_if` so a pre-0.47.0 roster round-trips unchanged and
    /// an org that never rotates carries no new bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub successor_root_pk: Option<String>,
    /// The PREDECESSOR root's signature over `domain ∥ org_id ∥ successor_pk` (#93 ask c) — the
    /// bridge that lets a node still pinned to the old anchor adopt the new one.
    ///
    /// Verified with the PINNED key, never the successor. That is the entire security of rotation:
    /// an attacker who can serve a roster still cannot introduce their own root without the current
    /// root's signature over it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub successor_sig: Option<String>,
    pub sig: String, // `b64u:<ed25519 signature>` over JCS(doc \ sig)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RosterUser {
    pub user_id: String,
    pub display_name: String,
    pub user_pk: String, // `b64u:<ed25519 user public key>`
    pub groups: Vec<String>,
    pub devices: Vec<RosterDevice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RosterDevice {
    pub endpoint_id: String, // `b64u:<endpoint_id>`
    pub label: String,
    /// Advisory dial-ordering hint (`primary`|`mirror`), NEVER a security property.
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_role() -> String {
    "primary".to_string()
}

/// Typed roster failure — callers (and tests) match on the specific rule violated.
/// `#[non_exhaustive]` so a future rule is not a breaking change — match with a
/// wildcard arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RosterError {
    #[error("roster json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("jcs canonicalization: {0}")]
    Jcs(String),
    #[error("bad b64u encoding: {0}")]
    Encoding(String),
    #[error("bad timestamp: {0}")]
    BadTimestamp(String),
    #[error(
        "unexpected roster format {0:?} (want {ROSTER_FORMAT:?} or {ROSTER_FORMAT_ROTATION:?} — \
         a {ROSTER_FORMAT_ROTATION:?} roster carries an org-root rotation and needs mcpmesh 0.47.0 \
         or newer on every member)"
    )]
    BadFormat(String),
    /// Rule 1: the org-root signature does not verify.
    #[error("signature does not verify against the pinned org root")]
    BadSignature,
    /// Rule 2: rollback protection.
    #[error("serial {got} is not strictly greater than the installed serial {installed}")]
    StaleSerial { got: u64, installed: u64 },
    /// Rule 3: validity window (with ±10 min skew).
    #[error("roster is not currently valid (issued_at ≤ now ≤ expires_at ± skew failed)")]
    OutOfValidity,
    /// Rule 4a: an endpoint appears under more than one user.
    #[error("endpoint appears more than once across users")]
    DuplicateEndpoint,
    /// Defensive completeness (beyond rules 1–6, parallel to rule 4's endpoint uniqueness): a
    /// `user_id` appears on more than one user entry, which would make `allow = ["<user_id>"]`
    /// ambiguous. The roster is org-root-signed, so this is an integrity footgun, not an attack.
    #[error("user_id {0:?} appears on more than one user entry")]
    DuplicateUser(String),
    /// Rule 5a: the flat namespace (user_ids ∪ groups) is not disjoint.
    #[error("name {0:?} is both a user_id and a group (flat namespace must be disjoint)")]
    NamespaceCollision(String),
    /// Rule 5b: a user references a group not declared in the top-level `groups`.
    #[error("group {0:?} is used by a user but not declared in the roster's top-level groups")]
    UndeclaredGroup(String),
}

/// `b64u:<base64url-nopad>` of `bytes` (the schema's key/id/sig encoding).
pub fn encode_b64u(bytes: &[u8]) -> String {
    format!(
        "{B64U_PREFIX}{}",
        data_encoding::BASE64URL_NOPAD.encode(bytes)
    )
}

/// Decode a `b64u:` string to bytes. Errors (never panics) on a missing prefix or bad base64url.
pub fn decode_b64u(s: &str) -> Result<Vec<u8>, RosterError> {
    let payload = s
        .strip_prefix(B64U_PREFIX)
        .ok_or_else(|| RosterError::Encoding(format!("missing {B64U_PREFIX} prefix")))?;
    data_encoding::BASE64URL_NOPAD
        .decode(payload.as_bytes())
        .map_err(|e| RosterError::Encoding(e.to_string()))
}

/// Decode a `b64u:` endpoint_id to the wire-agnostic `[u8; 32]`. Errors on a non-32-byte payload.
pub fn decode_endpoint_id(s: &str) -> Result<[u8; 32], RosterError> {
    decode_b64u(s)?
        .as_slice()
        .try_into()
        .map_err(|_| RosterError::Encoding("endpoint_id is not 32 bytes".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_json_round_trips_the_spec_example() {
        // A representative roster, verbatim shape. Deserialize → serialize → the field set survives.
        let src = serde_json::json!({
            "format": "mcpmesh-roster/1", "org_id": "acme", "serial": 42,
            "issued_at": "2026-07-03T12:00:00Z", "expires_at": "2026-10-01T00:00:00Z",
            "groups": ["team-eng", "team-research", "all"],
            "users": [{
                "user_id": "alice", "display_name": "Alice Nguyen", "user_pk": "b64u:AAAA",
                "groups": ["team-eng", "all"],
                "devices": [{ "endpoint_id": "b64u:BBBB", "label": "laptop", "role": "primary" }]
            }],
            "revoked_endpoints": ["b64u:CCCC"], "sig": "b64u:DDDD"
        });
        let roster: Roster = serde_json::from_value(src.clone()).expect("deserialize");
        assert_eq!(roster.format, "mcpmesh-roster/1");
        assert_eq!(roster.serial, 42);
        assert_eq!(roster.users[0].user_id, "alice");
        assert_eq!(roster.users[0].devices[0].label, "laptop");
        assert_eq!(roster.revoked_endpoints, vec!["b64u:CCCC".to_string()]);
        // serialize back to a Value and confirm the key set is identical (no dropped/renamed keys).
        assert_eq!(serde_json::to_value(&roster).unwrap(), src);
    }

    #[test]
    fn unknown_field_is_rejected_closed_schema() {
        // mcpmesh-roster/1 is CLOSED: an extended roster (extra top-level key) is REJECTED at
        // deserialize (deny_unknown_fields), never silently dropped. Same for a nested struct.
        let extended = serde_json::json!({
            "format": "mcpmesh-roster/1", "org_id": "acme", "serial": 1,
            "issued_at": "2026-07-03T12:00:00Z", "expires_at": "2026-10-01T00:00:00Z",
            "groups": [], "users": [], "revoked_endpoints": [], "sig": "b64u:AAAA",
            "extra": "future-field"
        });
        assert!(serde_json::from_value::<Roster>(extended).is_err());
        let extended_device = serde_json::json!({
            "endpoint_id": "b64u:AAAA", "label": "laptop", "unexpected": true
        });
        assert!(serde_json::from_value::<RosterDevice>(extended_device).is_err());
    }

    #[test]
    fn device_role_defaults_to_primary_when_absent() {
        let d: RosterDevice = serde_json::from_value(serde_json::json!({
            "endpoint_id": "b64u:AAAA", "label": "laptop"
        }))
        .unwrap();
        assert_eq!(d.role, "primary"); // #[serde(default = ...)] advisory dial-hint
    }

    #[test]
    fn b64u_round_trips_and_rejects_a_missing_prefix() {
        let bytes = [7u8; 32];
        let encoded = encode_b64u(&bytes);
        assert!(encoded.starts_with("b64u:"));
        assert_eq!(decode_b64u(&encoded).unwrap(), bytes.to_vec());
        assert_eq!(decode_endpoint_id(&encoded).unwrap(), bytes);
        // A raw (unprefixed) or wrong-length payload is a typed error, never a panic.
        assert!(matches!(decode_b64u("AAAA"), Err(RosterError::Encoding(_))));
        // A valid prefix over NON-base64url content exercises the `.decode(..)` error branch
        // (distinct from the missing-prefix and wrong-length paths) — still typed, never a panic.
        assert!(matches!(
            decode_b64u("b64u:@@@@"),
            Err(RosterError::Encoding(_))
        ));
        assert!(decode_endpoint_id(&encode_b64u(&[1u8; 8])).is_err()); // not 32 bytes
    }
}
