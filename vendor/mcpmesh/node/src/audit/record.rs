//! The audit record's DAEMON side. The record TYPE itself ([`AuditRecord`] /
//! [`AuditKind`]) is published wire vocabulary — it rides the `subscribe` stream verbatim — so it
//! lives in [`mcpmesh_local_api::protocol`] and is re-exported here; this module keeps the two
//! pure PRODUCER helpers: the blake3 args hash (the PRIVACY core — arguments are hashed, never
//! stored) and a zero-dependency RFC3339-millis timestamp formatter (matching the codebase's
//! "no date crate" idiom at `daemon::epoch_now`).
use serde_json::Value;

pub use mcpmesh_local_api::{AuditKind, AuditRecord};

/// Hash a request's arguments to `"blake3:<hex>"`. The raw argument bytes are NEVER
/// stored — only this digest — because callers' inputs can be sensitive. Deterministic within a
/// process for a given `params` value (serde_json serialization). `params` may be `Value::Null`
/// (a parameterless method) — that hashes the four bytes `null`, a stable non-empty digest.
///
/// **Every `_meta` key mcpmesh itself writes is excluded** (#45 gate, widened #49). The daemon
/// writes the authenticated caller into `_meta` before this runs, so hashing it verbatim would fold
/// the caller's identity into a field documented as "a digest of the arguments": two peers making an
/// identical call would no longer collide, and renaming a peer or changing its groups would change
/// the hash of arguments nobody touched.
///
/// **Excluded BY PREFIX, not by an enumerated key.** The first version listed `mcpmesh/peer`
/// literally; 0.51.0 then added a second spelling of the same annotation and silently re-broke the
/// property, because the test's fixture only carried the legacy key. Any key mcpmesh writes is
/// covered by one of [`RESERVED_META_PREFIXES`], so matching on those cannot fall behind a new
/// spelling.
///
/// Caller-supplied `_meta` still hashes — an `app/trace-id` a caller sent IS caller input. And a
/// caller-FORGED reserved key never reaches here: the strip removes it first.
///
/// [`RESERVED_META_PREFIXES`]: mcpmesh_net::service::RESERVED_META_PREFIXES
pub fn args_hash(params: &Value) -> String {
    let ours = |k: &str| {
        mcpmesh_net::service::RESERVED_META_PREFIXES
            .iter()
            .any(|p| k.starts_with(p))
    };
    let bytes = match params.get("_meta").and_then(Value::as_object) {
        // The common case allocates nothing: no `_meta`, or one we did not write into.
        Some(meta) if meta.keys().any(|k| ours(k)) => {
            let mut owned = params.clone();
            if let Some(m) = owned.get_mut("_meta").and_then(Value::as_object_mut) {
                m.retain(|k, _| !ours(k));
                // An `_meta` that held ONLY our annotations is dropped entirely, so the digest
                // matches a request that arrived with no `_meta` at all — which is what the caller
                // actually sent.
                if m.is_empty() {
                    owned.as_object_mut().map(|o| o.remove("_meta"));
                }
            }
            serde_json::to_vec(&owned).unwrap_or_default()
        }
        _ => serde_json::to_vec(params).unwrap_or_default(),
    };
    format!("blake3:{}", blake3::hash(&bytes).to_hex())
}

#[cfg(test)]
mod args_hash_tests {
    use super::args_hash;
    use serde_json::json;

    /// The `params` a backend actually receives for `read_file`, with `peer` written into `_meta`
    /// under EVERY spelling the daemon emits — read off the constant, so a future spelling is
    /// covered the moment it is added rather than the next time someone notices.
    fn injected(peer: serde_json::Value) -> serde_json::Value {
        let mut meta = serde_json::Map::new();
        for key in mcpmesh_net::service::PEER_KEYS {
            meta.insert(key.to_string(), peer.clone());
        }
        json!({"name": "read_file", "arguments": {"path": "/x"}, "_meta": meta})
    }

    /// #45 gate: the digest is of the CALLER'S arguments, and the daemon's own identity annotation
    /// must not move it.
    ///
    /// `AuditRecord` rides the `subscribe` stream and `docs/local-protocol.md` calls `args_hash`
    /// "a digest of the arguments", so folding the caller in makes two peers issuing an identical
    /// call produce different digests and moves the digest of untouched arguments whenever a peer
    /// is renamed.
    ///
    /// **This test was written for 0.50.0 and 0.51.0 broke the property anyway**, because the
    /// fixture hand-built `_meta` with only `mcpmesh/peer` while the code had started writing a
    /// second spelling. It builds the real shape now — the empty/stale-fixture failure, caught by
    /// review rather than by this test, which is the whole reason the fixture reads the constant.
    #[test]
    fn the_injected_caller_identity_does_not_change_the_args_hash() {
        let bare = json!({"name": "read_file", "arguments": {"path": "/x"}});
        let expected = args_hash(&bare);

        assert_eq!(
            args_hash(&injected(
                json!({"eid": "eid:aa", "name": "alice", "groups": ["eng"]})
            )),
            expected,
            "the same call from any peer must hash identically, under every spelling written"
        );
        // Two DIFFERENT callers, same arguments, same digest — the property that makes the field
        // usable for correlating calls at all.
        assert_eq!(
            args_hash(&injected(
                json!({"eid": "eid:bb", "name": "bob", "groups": []})
            )),
            expected
        );
    }

    /// But everything the CALLER put in `_meta` still hashes — it is their input.
    #[test]
    fn caller_supplied_meta_still_affects_the_args_hash() {
        let a = json!({"arguments": {}, "_meta": {"app/trace-id": "one"}});
        let b = json!({"arguments": {}, "_meta": {"app/trace-id": "two"}});
        assert_ne!(
            args_hash(&a),
            args_hash(&b),
            "a caller-supplied `_meta` value is caller input and must be covered by the digest"
        );

        // Removing OUR keys leaves theirs in place rather than dropping `_meta` wholesale — with
        // both spellings present, which is what is really on the wire.
        let mixed = json!({"arguments": {},
                           "_meta": {"app/trace-id": "one",
                                     "mcpmesh/peer": {"eid": "eid:aa"},
                                     "tech.counterpunch.mcpmesh/peer": {"eid": "eid:aa"}}});
        assert_eq!(
            args_hash(&mixed),
            args_hash(&a),
            "stripping our annotations must leave the caller's other `_meta` keys hashed: {mixed}"
        );
    }

    /// Shapes that must not panic or silently change meaning.
    #[test]
    fn odd_params_shapes_hash_without_panicking() {
        for v in [
            json!(null),
            json!("a string"),
            json!([1, 2, 3]),
            json!({"_meta": "not an object"}),
            json!({"_meta": null}),
            json!({}),
        ] {
            let h = args_hash(&v);
            assert!(h.starts_with("blake3:"), "{v} -> {h}");
        }
    }
}

/// The current wall-clock instant as an RFC3339 UTC millisecond string (the record `ts`). Uses
/// `SystemTime` + a hand-rolled civil-date conversion — NO date crate — matching the codebase's
/// stated preference at `daemon::epoch_now` ("no date crate"). A pre-epoch clock (impossible on a
/// sane host) collapses to the epoch rather than panicking.
pub fn now_ts() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    fmt_rfc3339_millis(millis)
}

/// Format Unix-epoch milliseconds as `"YYYY-MM-DDTHH:MM:SS.mmmZ"`. Pure + unit-testable.
pub fn fmt_rfc3339_millis(unix_millis: u128) -> String {
    let secs = (unix_millis / 1000) as i64;
    let millis = (unix_millis % 1000) as u64;
    let days = secs.div_euclid(86_400);
    let secs_of_day = secs.rem_euclid(86_400);
    let (h, m, s) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (year, month [1..=12], day [1..=31]).
/// Proleptic Gregorian, exact for the whole i64 range — the standard branch-free algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn timestamp_formats_epoch_and_millis() {
        // The Unix epoch and a sub-second instant — the boundary the writer derives the month from.
        assert_eq!(fmt_rfc3339_millis(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(fmt_rfc3339_millis(1500), "1970-01-01T00:00:01.500Z");
        // A known modern instant: 1_700_000_000_000 ms == 2023-11-14T22:13:20.000Z.
        assert_eq!(
            fmt_rfc3339_millis(1_700_000_000_000),
            "2023-11-14T22:13:20.000Z"
        );
        // The month prefix (the rotation key) is the first 7 chars.
        assert_eq!(&fmt_rfc3339_millis(1_700_000_000_000)[..7], "2023-11");
    }

    #[test]
    fn args_hash_is_blake3_prefixed_and_deterministic() {
        let p = json!({"path": "/secret/passwords.txt"});
        let h = args_hash(&p);
        assert!(h.starts_with("blake3:"));
        assert_eq!(h, args_hash(&p), "same params → same hash");
        assert_ne!(h, args_hash(&json!({"path": "/other"})), "differs by input");
        // The hex is a 64-char blake3 digest.
        assert_eq!(h.len(), "blake3:".len() + 64);
    }

    /// PRIVACY: a proxied-request record NEVER carries the raw arguments — only their
    /// blake3 digest — and it carries the method + tool NAME. Serialize a record built from a
    /// sensitive argument and assert the raw secret is ABSENT from the JSON while the digest is
    /// present. This is the unit-level guard on the hard privacy invariant.
    #[test]
    fn proxied_request_serialization_never_leaks_raw_arguments() {
        let secret = "hunter2-super-secret-token";
        let params = json!({"name": "read_file", "arguments": {"token": secret}});
        let rec = AuditRecord::proxied_request(
            "2026-07-03T14:02:11.480Z".into(),
            Some("bob".into()),
            "notes".into(),
            "tools/call".into(),
            Some("read_file".into()),
            args_hash(&params),
            6210,
            "ok".into(),
            41,
            None,
        );
        let line = serde_json::to_string(&rec).unwrap();
        assert!(
            !line.contains(secret),
            "raw argument leaked into the record: {line}"
        );
        assert!(line.contains("blake3:"), "args_hash digest must be present");
        assert!(
            line.contains("\"tool\":\"read_file\""),
            "tool NAME is recorded"
        );
        assert!(line.contains("\"method\":\"tools/call\""));
        // The kind serializes snake_case; a session record elides request-only fields.
        let s = AuditRecord::session_open(
            "2026-07-03T14:02:11.480Z".into(),
            Some("bob".into()),
            "notes".into(),
            None,
        );
        let sline = serde_json::to_string(&s).unwrap();
        assert!(sline.contains("\"kind\":\"session_open\""));
        assert!(
            !sline.contains("method"),
            "session record elides request fields"
        );
    }

    #[test]
    fn record_round_trips_through_jsonl() {
        // internal audit reads records back; a round-trip proves Deserialize matches Serialize.
        let rec = AuditRecord::trust(
            "2026-07-03T14:02:11.480Z".into(),
            "pair".into(),
            Some("bob".into()),
            None,
        );
        let line = serde_json::to_string(&rec).unwrap();
        let back: AuditRecord = serde_json::from_str(&line).unwrap();
        assert_eq!(back, rec);
        assert_eq!(back.kind, AuditKind::Trust);
    }
}
