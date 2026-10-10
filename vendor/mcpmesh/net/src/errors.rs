//! Platform-reserved JSON-RPC codes -32050…-32069 + the data.source marker.
use serde_json::{Value, json};

pub const ERR_FRAMING: i64 = -32051;
pub const ERR_SESSION: i64 = -32052;
pub const ERR_LIMIT: i64 = -32053;
pub const ERR_SERVICE: i64 = -32054;
pub const ERR_UNREACHABLE: i64 = -32055;
pub const ERR_PARSE: i64 = -32700; // JSON-RPC standard parse error

/// The refusal wording is identical for unknown and unauthorized — deliberate:
/// a caller must not be able to probe which services exist.
pub const MSG_SERVICE: &str = "unknown or unauthorized service";

/// Every synthesized error MUST carry data.source = "mcpmesh".
pub fn synthesized(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": {
        "code": code, "message": message, "data": { "source": "mcpmesh" } } })
}

/// A rate-limit / concurrency-cap refusal (`-32053`): carries `retry_after_ms` in
/// `error.data` ALONGSIDE the mandatory `source` marker. Both the per-identity token bucket and the
/// per-service concurrency cap synthesize this, so a throttled caller receives a well-formed,
/// actionable answer rather than a hang. FAIL-SAFE: this is a DENY — the request is never served.
pub fn synthesized_limited(id: Value, retry_after_ms: u64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": {
        "code": ERR_LIMIT, "message": "rate limited",
        "data": { "source": "mcpmesh", "retry_after_ms": retry_after_ms } } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesized_errors_carry_the_marker() {
        let e = synthesized(serde_json::Value::Null, ERR_FRAMING, "frame too large");
        assert_eq!(e["error"]["data"]["source"], "mcpmesh");
        assert_eq!(e["error"]["code"], ERR_FRAMING);
        assert!(e["id"].is_null());
    }

    #[test]
    fn limited_error_carries_retry_after_and_marker() {
        let e = synthesized_limited(serde_json::json!(7), 1500);
        assert_eq!(e["error"]["code"], ERR_LIMIT);
        assert_eq!(e["error"]["data"]["source"], "mcpmesh");
        assert_eq!(e["error"]["data"]["retry_after_ms"], 1500);
        assert_eq!(e["id"], 7);
    }
}
