//! The three roster-enrollment codes — the opaque one-line strings a human copies:
//! `mcpmesh-org:` (from `org create`, to a joiner), `mcpmesh-join:` (from `join`, to the operator),
//! `mcpmesh-device:` (from a new machine, for `devices add` on an enrolled device). Each is
//! `scheme + base32-nopad(JSON)`, EXACTLY the pairing `mcpmesh-invite:` codec () —
//! copy/paste-safe `[A-Z2-7]`, opaque to humans. Key/id/sig FIELDS inside stay `b64u:` so they copy
//! verbatim into a roster. Pure types + codec; the device-binding CRYPTO is `mcpmesh_trust::roster::sign`.
use anyhow::Context;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

const ORG_SCHEME: &str = "mcpmesh-org:";
const JOIN_SCHEME: &str = "mcpmesh-join:";
const DEVICE_SCHEME: &str = "mcpmesh-device:";

/// From `org create`, handed to a joiner: the org id, the org-root PUBLIC key
/// the joiner pins, and an optional roster URL (`None` when the org serves none).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgInviteCode {
    pub org_id: String,
    pub org_root_pk: String, // b64u:
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster_url: Option<String>,
}

/// From `join`, handed to the operator: the display name, a requested stable
/// user_id (), the joiner's user PUBLIC key, this device's endpoint id + label, and the
/// user-key device-binding signature the operator verifies (). The `user_pk` +
/// `device_endpoint_id` are additionally cross-checked out-of-band via a **join-code fingerprint**
/// read back on BOTH sides — the enrollment analog of the pairing SAS, closing the join-code
/// substitution MITM (nothing else binds person→user_pk).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinCode {
    pub display_name: String,
    pub requested_user_id: String,
    pub user_pk: String,            // b64u: (ed25519 user public key)
    pub device_endpoint_id: String, // b64u: (this device's endpoint id)
    pub device_label: String,
    pub binding_sig: String, // b64u: Ed25519_userkey(domain ∥ user_pk ∥ endpoint_id)
}

/// From a NEW machine, for `devices add` on an already-enrolled device: only the new
/// device's endpoint id + a label — NO keys (the enrolled device signs the binding with the shared
/// user key it already holds).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCode {
    pub device_endpoint_id: String, // b64u:
    pub device_label: String,
}

/// `scheme + base32-nopad(JSON)` — the pairing-invite codec ().
fn encode_code<T: Serialize>(scheme: &str, value: &T) -> String {
    let json = serde_json::to_vec(value).expect("enrollment code serializes");
    format!("{scheme}{}", data_encoding::BASE32_NOPAD.encode(&json))
}

/// Strip the scheme, base32-decode, JSON-deserialize. Errs (never panics) on a wrong scheme, an
/// undecodable payload, or JSON that is not `T`.
fn decode_code<T: DeserializeOwned>(scheme: &str, line: &str) -> anyhow::Result<T> {
    let payload = line
        .strip_prefix(scheme)
        .with_context(|| format!("not an {scheme} code (missing scheme)"))?;
    let json = data_encoding::BASE32_NOPAD
        .decode(payload.as_bytes())
        .context("enrollment code payload is not valid base32")?;
    serde_json::from_slice(&json).context("enrollment code payload is not the expected shape")
}

impl OrgInviteCode {
    pub fn encode(&self) -> String {
        encode_code(ORG_SCHEME, self)
    }
    pub fn decode(line: &str) -> anyhow::Result<Self> {
        decode_code(ORG_SCHEME, line)
    }
}
impl JoinCode {
    pub fn encode(&self) -> String {
        encode_code(JOIN_SCHEME, self)
    }
    pub fn decode(line: &str) -> anyhow::Result<Self> {
        decode_code(JOIN_SCHEME, line)
    }
}
impl DeviceCode {
    pub fn encode(&self) -> String {
        encode_code(DEVICE_SCHEME, self)
    }
    pub fn decode(line: &str) -> anyhow::Result<Self> {
        decode_code(DEVICE_SCHEME, line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn org_invite_code_round_trips_and_rejects_bad_input() {
        let code = OrgInviteCode {
            org_id: "acme".into(),
            org_root_pk: "b64u:AAAA".into(),
            roster_url: None,
        };
        let line = code.encode();
        assert!(line.starts_with("mcpmesh-org:"));
        assert_eq!(OrgInviteCode::decode(&line).unwrap(), code);
        // Wrong scheme / hostile payload → Err (never panic).
        assert!(OrgInviteCode::decode("mcpmesh-join:AAAA").is_err());
        assert!(OrgInviteCode::decode("mcpmesh-org:!!!").is_err());
        assert!(OrgInviteCode::decode("nope").is_err());
    }

    #[test]
    fn join_code_round_trips_carrying_the_binding() {
        let code = JoinCode {
            display_name: "Alice Nguyen".into(),
            requested_user_id: "alice".into(),
            user_pk: "b64u:UUUU".into(),
            device_endpoint_id: "b64u:DDDD".into(),
            device_label: "laptop".into(),
            binding_sig: "b64u:SSSS".into(),
        };
        let line = code.encode();
        assert!(line.starts_with("mcpmesh-join:"));
        assert_eq!(JoinCode::decode(&line).unwrap(), code);
        // A valid base32 payload that is not a JoinCode → Err (distinct from the base32 branch).
        let not_join = data_encoding::BASE32_NOPAD.encode(b"{\"nope\":1}");
        assert!(JoinCode::decode(&format!("mcpmesh-join:{not_join}")).is_err());
    }

    #[test]
    fn device_code_round_trips() {
        let code = DeviceCode {
            device_endpoint_id: "b64u:DDDD".into(),
            device_label: "desktop".into(),
        };
        let line = code.encode();
        assert!(line.starts_with("mcpmesh-device:"));
        assert_eq!(DeviceCode::decode(&line).unwrap(), code);
        assert!(DeviceCode::decode("mcpmesh-org:AAAA").is_err());
    }
}
