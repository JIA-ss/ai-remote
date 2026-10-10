//! Initialize-time service selection and the reserved-namespace rule:
//! caller-supplied reserved `_meta` keys (both spellings) are deleted before anything
//! acts on the frame; refusal wording never distinguishes unknown from unauthorized.
use serde_json::Value;

/// The authorization outcome. `#[must_use]`: dropping this value would leave the
/// frame cleaned but the caller unauthorized-yet-served — the compiler must object.
#[must_use]
#[derive(Debug, PartialEq)]
pub enum ServiceDecision {
    Selected(String),
    Refuse, // caller sends errors::synthesized(id, ERR_SERVICE, MSG_SERVICE)
}

/// Delete every caller-supplied reserved key (`mcpmesh/*` and `tech.counterpunch.mcpmesh/*`) from a frame's `params._meta`.
///
/// **The single definition of "reserved".** Called by [`select_service`] on the session's first
/// frame AND by the backend pump on every later one — #164 was exactly these two drifting apart:
/// the rule held on frame 1 only, so a caller sent a `ping` first and its real `initialize` second,
/// where nothing stripped and nothing injected. A shared server reading `_meta["mcpmesh/peer"]` out
/// of `initialize` saw whatever the caller wrote.
///
/// A non-object `_meta` (array/string) has no keys in the reserved namespace and passes through
/// untouched — deliberate asymmetry with `select_service`'s non-string-request refusal (D6: parse
/// no further than the rule requires); the peer injector must therefore REPLACE a non-object
/// `_meta`, never merge (seam note).
///
/// **A JSON-RPC batch is descended into.** A top-level array root resolves `params/_meta` to
/// nothing, so a caller could wrap the forged frame in `[ ... ]` and the strip saw an array with no
/// pointer to follow. Whether that reaches a request handler depends on the backend server — rmcp
/// 3.1.0 does not unwrap batches (MCP removed them in 2025-06-18), but an older SDK or a custom
/// NDJSON server does, and this daemon pumps rather than interprets. Each element is sanitized as a
/// frame in its own right.
///
/// **Scope, stated rather than implied:** this covers `params._meta`, the seam MCP defines and the
/// only one a backend reads. A top-level `_meta` sibling of `params` is not touched, and neither is
/// `result._meta` on a client→server RESPONSE (to a `sampling/createMessage` or `roots/list`) —
/// that is a reply, not a request, and no backend reads identity out of one.
pub fn strip_reserved_meta(frame: &mut Value) {
    strip_reserved_meta_to_depth(frame, 0);
}

/// Batch nesting is bounded so a pathological input cannot recurse without limit. `serde_json`
/// already caps parse depth, and a valid JSON-RPC batch is exactly one level; anything past this is
/// not a request any server would unwrap.
const MAX_BATCH_DEPTH: usize = 8;

/// The MCP 2026-07-28 key a client writes its own software name/version into.
const CLIENT_INFO_KEY: &str = "io.modelcontextprotocol/clientInfo";

/// mcpmesh's ORIGINAL single-label `_meta` prefix. **Deprecated as of 0.51.0, removed at 1.0.**
///
/// Still emitted and still accepted — see [`RESERVED_META_PREFIXES`] for why both exist.
pub const LEGACY_META_PREFIX: &str = "mcpmesh/";

/// The reverse-DNS `_meta` prefix (#49), per SEP-1788's SHOULD.
///
/// Nothing was ever broken by the single-label form. The final `2026-07-28` grammar reserves
/// `progressToken`, the `io.modelcontextprotocol/*` keys, and bare `traceparent`/`tracestate`/
/// `baggage`; a prefixed key is reserved only by its SECOND label being `mcp` or
/// `modelcontextprotocol` (the spec's own counter-example is that `com.example.mcp/` is NOT
/// reserved). `mcpmesh/` has no second label and `tech.counterpunch.mcpmesh/`'s is `counterpunch`,
/// so neither collides. This is forward-alignment, which is exactly why it is not worth breaking
/// anyone over.
pub const META_PREFIX: &str = "tech.counterpunch.mcpmesh/";

/// Every prefix mcpmesh reserves. **The strip must cover ALL of them**, not just the one currently
/// emitted: a prefix that is written but not stripped is a key a caller can forge, and a backend
/// reading it would be reading caller-controlled data under an authoritative-looking name.
pub const RESERVED_META_PREFIXES: [&str; 2] = [LEGACY_META_PREFIX, META_PREFIX];

/// The service-routing key, in both spellings, reverse-DNS first.
///
/// The order is a tie-break that never fires: both present and EQUAL gives the same answer either
/// way, both present and DIFFERENT is refused, and one non-string is refused. What is real is the
/// FALLBACK — a frame carrying only the legacy spelling still routes, which is how a newer daemon
/// serves an older caller.
pub const SERVICE_KEYS: [&str; 2] = ["tech.counterpunch.mcpmesh/service", "mcpmesh/service"];

/// The caller-identity key, in both spellings. Both are WRITTEN, with the identical value: a backend
/// is a third-party process reading `mcpmesh/peer`, not something version-locked to this daemon, so
/// dropping the legacy spelling would make every existing backend silently stop seeing an identity —
/// and one that reads "no identity" as "local caller" would fail OPEN.
pub const PEER_KEYS: [&str; 2] = ["tech.counterpunch.mcpmesh/peer", "mcpmesh/peer"];

/// Remove a `clientInfo` that names itself in mcpmesh's PRINCIPAL grammar (#189).
///
/// With the `initialize` handshake gone in MCP 2026-07-28, client identity moves into per-request
/// `_meta` as `io.modelcontextprotocol/clientInfo`. mcpmesh injects the transport-authenticated
/// caller into the SAME object as `mcpmesh/peer`. So a backend now sees two identity-shaped keys
/// side by side with opposite trust properties:
///
/// | key | written by | trustworthy |
/// |---|---|---|
/// | `mcpmesh/peer` | mcpmesh, from the authenticated endpoint | yes |
/// | `io.modelcontextprotocol/clientInfo` | the caller | **no** |
///
/// **mcpmesh does not populate, filter or validate `clientInfo` in general.** It is legitimate
/// protocol data describing the client SOFTWARE, not the principal; overwriting it would destroy
/// information a backend wants and make mcpmesh the only transport that lies about which MCP client
/// is connected. A backend that reads `clientInfo.name` as an identity has an authorization bug no
/// transport filtering can fix, because that field is caller-controlled BY DESIGN. That is a
/// contract to document, and `docs/local-protocol.md` documents it.
///
/// The one exception is here. A `name` written in **our** grammar — `eid:…` or `b64u:…` — is never
/// client software naming itself; it is only ever an attempt to look authoritative beside the key
/// that is. That is #164's shape exactly (a caller writing in the reserved grammar) and it gets
/// #164's treatment: removal, on every frame, descending batches.
///
/// **Called from BOTH places `strip_reserved_meta` is**, and for the same reason: [`select_service`]
/// on the session's first frame, the backend pump on every later one. The pump alone would miss
/// frame 1 — `serve` hands the selected `initialize` straight to the backend — which is #164 in
/// mirror image, and worse here, because under 2026-07-28 `clientInfo` rides EVERY request rather
/// than only the handshake.
///
/// Deliberately narrow. Nicknames are NOT checked: "bob" is a plausible name for client software,
/// the nickname space is unbounded and per-node, and a check there would be leaky and
/// false-positive-prone at once. A non-string `name`, an absent `name`, a non-object `clientInfo`,
/// and every other name pass through verbatim.
///
/// The WHOLE entry goes, not just `name`: an object whose name is a forgery has nothing worth
/// preserving, and a `clientInfo` missing its required field is worse for a strict backend than no
/// `clientInfo` at all.
///
/// Returns whether anything was removed, so the caller can log ONCE per session. A `warn!` per
/// offending frame is an unbounded, caller-driven log-growth vector — the same class as the audit
/// DoS this project has already fixed once.
#[must_use]
pub fn strip_impersonating_client_info(frame: &mut Value) -> bool {
    strip_impersonating_client_info_to_depth(frame, 0)
}

/// Principal prefixes reserved to mcpmesh. A `clientInfo.name` in either is a forgery attempt.
const PRINCIPAL_PREFIXES: [&str; 2] = ["eid:", "b64u:"];

fn strip_impersonating_client_info_to_depth(frame: &mut Value, depth: usize) -> bool {
    if let Some(batch) = frame.as_array_mut() {
        if depth >= MAX_BATCH_DEPTH {
            return false;
        }
        // NOT short-circuiting: every element must be sanitized even once one has already matched.
        let mut hit = false;
        for element in batch {
            hit |= strip_impersonating_client_info_to_depth(element, depth + 1);
        }
        return hit;
    }
    let Some(meta) = frame
        .pointer_mut("/params/_meta")
        .and_then(Value::as_object_mut)
    else {
        return false;
    };
    let impersonating = meta
        .get(CLIENT_INFO_KEY)
        .and_then(|ci| ci.get("name"))
        .and_then(Value::as_str)
        .is_some_and(|n| PRINCIPAL_PREFIXES.iter().any(|p| n.starts_with(p)));
    if impersonating {
        meta.remove(CLIENT_INFO_KEY);
    }
    impersonating
}

fn strip_reserved_meta_to_depth(frame: &mut Value, depth: usize) {
    if let Some(batch) = frame.as_array_mut() {
        if depth < MAX_BATCH_DEPTH {
            for element in batch {
                strip_reserved_meta_to_depth(element, depth + 1);
            }
        }
        return;
    }
    if let Some(meta) = frame
        .pointer_mut("/params/_meta")
        .and_then(Value::as_object_mut)
    {
        meta.retain(|k, _| !RESERVED_META_PREFIXES.iter().any(|p| k.starts_with(p)));
    }
}

pub fn select_service(init: &mut Value, caller_allowed: &[String]) -> ServiceDecision {
    // Read the request before stripping, distinguishing "key absent" (may default)
    // from "key present but not a string" (malformed → requested something
    // unresolvable → Refuse; it must never fall through to the default).
    //
    // BOTH spellings are accepted (#49): reverse-DNS preferred, single-label legacy as fallback, so
    // a newer daemon still serves an older caller and neither peer needs a version gate.
    // RFC 6901 escaping. `~` would need `~0` first; no reserved key contains one, and
    // `every_written_key_is_covered_by_a_stripped_prefix` keeps the set to what we control.
    let read = |key: &str| init.pointer(&format!("/params/_meta/{}", key.replace('/', "~1")));
    // A fixed-size array, not a Vec: the conflict check below indexes [0] and [1], and the 1.0
    // removal of the legacy spelling shrinks SERVICE_KEYS to one entry. As a Vec that becomes a
    // PANIC on the first frame of every session, on the authorization path; as an array the
    // compiler stops it.
    let entries: [Option<&Value>; SERVICE_KEYS.len()] = SERVICE_KEYS.map(read);
    let malformed = entries.iter().flatten().any(|v| !v.is_string());
    // A caller sending BOTH with different values is not making a coherent request. Silently
    // preferring one would let it present a different service to each daemon version — the whole
    // reason this transition accepts two spellings is that peers may disagree about which they read.
    let conflicting = matches!(
        (
            entries[0].and_then(Value::as_str),
            entries[1].and_then(Value::as_str)
        ),
        (Some(a), Some(b)) if a != b
    );
    let requested: Option<String> = entries
        .iter()
        .flatten()
        .find_map(|v| v.as_str())
        .map(String::from);

    // Strip ALL reserved keys, always — before any decision is acted on.
    strip_reserved_meta(init);

    // #189 on the SESSION'S FIRST FRAME. The backend pump covers every LATER frame, but the first
    // one never reaches it: `serve` hands the already-selected `initialize` to the backend, which
    // injects `mcpmesh/peer` itself. Wiring the removal only into the pump would leave frame 1
    // untouched — the exact shape of #164, in mirror image, and a worse one here: under MCP
    // 2026-07-28 `clientInfo` rides EVERY request, so frame 1 is the likeliest place to see it,
    // not the least.
    //
    // Warned inline rather than returned. `select_service` runs once per session, so this is
    // bounded by session count — the same "once per session" the pump achieves with a flag, with
    // no signal to thread through `ServiceDecision` and no second place for the rule to drift to.
    if strip_impersonating_client_info(init) {
        tracing::warn!(
            "caller's `io.modelcontextprotocol/clientInfo` named itself in mcpmesh's principal \
             grammar (eid:/b64u:) on the session's first frame; the whole entry was removed. \
             `mcpmesh/peer` is the only authenticated identity in that object."
        );
    }

    if malformed || conflicting {
        return ServiceDecision::Refuse;
    }
    match requested {
        Some(name) if caller_allowed.contains(&name) => ServiceDecision::Selected(name),
        Some(_) => ServiceDecision::Refuse,
        None if caller_allowed.len() == 1 => ServiceDecision::Selected(caller_allowed[0].clone()),
        None => ServiceDecision::Refuse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn init_with_meta(meta: serde_json::Value) -> serde_json::Value {
        json!({"jsonrpc":"2.0","id":1,"method":"initialize",
               "params":{"protocolVersion":"2025-11-25","_meta": meta,"capabilities":{}}})
    }

    const NEW_SVC: &str = "tech.counterpunch.mcpmesh/service";
    const OLD_SVC: &str = "mcpmesh/service";

    /// #49: EITHER spelling routes, so neither peer needs a version gate during the transition.
    ///
    /// The reason this release does not need the coordinated break #49 assumed: a newer daemon
    /// serves an older caller (legacy key), and an older daemon serves a newer caller (which emits
    /// both). Asserting only the new key would pass on an implementation that dropped the fallback
    /// and refused every dial from an un-upgraded peer.
    #[test]
    fn either_service_key_spelling_routes() {
        let allowed = vec!["notes".to_string(), "kb".to_string()];

        for key in [NEW_SVC, OLD_SVC] {
            let mut f = init_with_meta(json!({key: "kb"}));
            assert_eq!(
                select_service(&mut f, &allowed),
                ServiceDecision::Selected("kb".into()),
                "`{key}` must route"
            );
            // And it is stripped whichever spelling arrived — see the prefix test below.
            assert!(
                f["params"]["_meta"].get(key).is_none(),
                "the routing key must never reach the backend: {f}"
            );
        }

        // BOTH present, same value — what this release actually puts on the wire.
        let mut both = init_with_meta(json!({NEW_SVC: "kb", OLD_SVC: "kb"}));
        assert_eq!(
            select_service(&mut both, &allowed),
            ServiceDecision::Selected("kb".into()),
            "the pair this daemon emits must route: {both}"
        );
    }

    /// A caller sending the two spellings with DIFFERENT values is refused, not silently resolved.
    ///
    /// Picking one would let a caller show a different service to each daemon version — and the
    /// whole premise of accepting two spellings is that peers may disagree about which they read.
    /// This is the attack the transition creates, so it is refused rather than reconciled.
    #[test]
    fn conflicting_service_key_spellings_are_refused() {
        let allowed = vec!["notes".to_string(), "kb".to_string()];
        let mut f = init_with_meta(json!({NEW_SVC: "kb", OLD_SVC: "notes"}));
        assert_eq!(
            select_service(&mut f, &allowed),
            ServiceDecision::Refuse,
            "two spellings naming different services is not a coherent request: {f}"
        );
        // Refused even when BOTH names would individually be allowed — this is about coherence,
        // not authorization, and a test using one disallowed name would prove nothing.
    }

    /// #49: BOTH reserved prefixes are stripped.
    ///
    /// The new spelling is the new attack surface: a prefix mcpmesh WRITES but does not STRIP is a
    /// key a caller can forge, which a backend would then read under an authoritative-looking name.
    #[test]
    fn both_reserved_prefixes_are_stripped_including_the_new_one() {
        let mut f = init_with_meta(json!({
            "tech.counterpunch.mcpmesh/peer": {"name": "attacker", "groups": ["admin"]},
            "tech.counterpunch.mcpmesh/anything": "forged",
            "mcpmesh/peer": {"name": "attacker"},
            "mcpmesh/anything": "forged",
            "app/keep": "yes",
            "io.modelcontextprotocol/clientInfo": {"name": "some-client"},
        }));
        let _ = select_service(&mut f, &["notes".to_string()]);
        let meta = &f["params"]["_meta"];
        for k in [
            "tech.counterpunch.mcpmesh/peer",
            "tech.counterpunch.mcpmesh/anything",
            "mcpmesh/peer",
            "mcpmesh/anything",
        ] {
            assert!(
                meta.get(k).is_none(),
                "`{k}` is a reserved prefix and must not survive: {meta}"
            );
        }
        assert_eq!(meta["app/keep"], "yes", "non-reserved keys survive: {meta}");
        assert_eq!(
            meta["io.modelcontextprotocol/clientInfo"]["name"], "some-client",
            "a clientInfo that does NOT name a principal passes through: {meta}"
        );
    }

    /// Every prefix mcpmesh writes must also be one it strips. Written as a loop over the constants
    /// rather than a hand-listed set, so adding a third prefix without stripping it fails here.
    #[test]
    fn every_written_key_is_covered_by_a_stripped_prefix() {
        for key in SERVICE_KEYS.iter().chain(PEER_KEYS.iter()) {
            assert!(
                RESERVED_META_PREFIXES.iter().any(|p| key.starts_with(p)),
                "`{key}` is written onto the wire but no reserved prefix covers it — a caller \
                 could forge it and a backend would read it as authoritative"
            );
        }
    }

    const CI: &str = "io.modelcontextprotocol/clientInfo";

    fn frame_with_client_info(ci: serde_json::Value) -> serde_json::Value {
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
               "params":{"_meta":{CI: ci, "other/key":"kept"}}})
    }

    /// #189 on the session's FIRST frame — the one the pump never sees.
    ///
    /// `serve` hands the selected `initialize` straight to the backend, which injects
    /// `mcpmesh/peer` itself; only LATER frames go through the pump's sanitizer. A `clientInfo`
    /// check wired only into the pump would therefore miss frame 1 — #164 in mirror image, and
    /// worse here, because under MCP 2026-07-28 `clientInfo` rides every request and frame 1 is
    /// where a handshake-shaped one appears.
    #[test]
    fn the_first_frame_is_covered_too() {
        let mut init = init_with_meta(json!({
            "mcpmesh/service": "notes",
            CI: {"name": "eid:forged", "version": "1"},
        }));
        assert_eq!(
            select_service(&mut init, &["notes".into()]),
            ServiceDecision::Selected("notes".into()),
            "the selection itself is unaffected"
        );
        assert!(
            init["params"]["_meta"].get(CI).is_none(),
            "an impersonating clientInfo must not reach the backend on frame 1: {init}"
        );

        // A legitimate one survives selection untouched.
        let ci = json!({"name": "Claude Code", "version": "2.0"});
        let mut ok = init_with_meta(json!({"mcpmesh/service": "notes", CI: ci.clone()}));
        let _ = select_service(&mut ok, &["notes".into()]);
        assert_eq!(ok["params"]["_meta"][CI], ci);
    }

    /// #189: a `clientInfo` naming itself in mcpmesh's PRINCIPAL grammar is removed.
    ///
    /// Under MCP 2026-07-28 this key sits in the same `_meta` object as the authenticated
    /// `mcpmesh/peer`. A caller writing `eid:`/`b64u:` there is not naming its software; it is
    /// dressing up as the key that is trustworthy.
    #[test]
    fn a_client_info_wearing_our_principal_grammar_is_removed() {
        for name in [
            "eid:abcdef",
            "b64u:AAAA",
            // No separator required beyond the prefix — a bare prefix still reads as a principal.
            "eid:",
        ] {
            let mut f = frame_with_client_info(json!({"name": name, "version": "1.0"}));
            assert!(
                strip_impersonating_client_info(&mut f),
                "{name} must be reported as impersonating, so the pump can warn once"
            );
            assert!(
                f["params"]["_meta"].get(CI).is_none(),
                "the WHOLE entry goes — a clientInfo missing its required `name` is worse for a \
                 strict backend than none: {f}"
            );
            assert_eq!(
                f["params"]["_meta"]["other/key"], "kept",
                "nothing else in _meta is touched"
            );
        }
    }

    /// …and everything else passes through VERBATIM. The narrowness is the design: `clientInfo` is
    /// legitimate protocol data describing client SOFTWARE, and mcpmesh pumps rather than
    /// interprets.
    #[test]
    fn an_ordinary_client_info_is_never_touched() {
        for ci in [
            json!({"name": "Claude Code", "version": "2.0"}),
            // A nickname-shaped name: deliberately NOT checked. "bob" is plausible software
            // naming, and the nickname space is unbounded and per-node.
            json!({"name": "bob"}),
            // Prefix-ish but not ours.
            json!({"name": "eidetic-client"}),
            json!({"name": "identity:bob"}),
            // Non-string / absent `name`, and a non-object clientInfo.
            json!({"name": 42}),
            json!({"name": {"eid": "nested"}}),
            json!({"version": "1.0"}),
            json!("not-an-object"),
            json!(["eid:array"]),
            json!(null),
        ] {
            let mut f = frame_with_client_info(ci.clone());
            assert!(
                !strip_impersonating_client_info(&mut f),
                "{ci} must not be reported as impersonating"
            );
            assert_eq!(
                f["params"]["_meta"][CI], ci,
                "{ci} must pass through verbatim"
            );
        }

        // No `_meta`, no `params`, a non-object `_meta`, and a scalar frame: no panic, no change.
        for mut f in [
            json!({"method":"ping","params":{}}),
            json!({"method":"ping"}),
            json!({"method":"ping","params":{"_meta":["not","an","object"]}}),
            json!("scalar"),
        ] {
            let before = f.clone();
            assert!(!strip_impersonating_client_info(&mut f));
            assert_eq!(f, before);
        }
    }

    /// A JSON-RPC batch is descended into — the #164 hole, on this rule.
    ///
    /// A caller that could wrap the forged frame in `[ ... ]` and slip past would have the exact
    /// bypass #164 was, so the batch case is pinned rather than assumed.
    #[test]
    fn a_batch_wrapped_impersonation_is_removed_in_every_element() {
        // The LAST element is deliberately legitimate: with an impersonator last, `hit |= …`
        // degraded to `hit = …` still returns true and the accumulator bug goes unseen.
        let mut batch = json!([
            frame_with_client_info(json!({"name": "eid:one"})),
            frame_with_client_info(json!({"name": "Claude Code"})),
            frame_with_client_info(json!({"name": "b64u:three"})),
            frame_with_client_info(json!({"name": "Some Client"})),
        ]);
        assert!(
            strip_impersonating_client_info(&mut batch),
            "a hit anywhere in the batch must be reported, not just one in the last element"
        );
        assert!(batch[0]["params"]["_meta"].get(CI).is_none());
        assert_eq!(
            batch[1]["params"]["_meta"][CI]["name"], "Claude Code",
            "a legitimate sibling in the same batch is untouched"
        );
        assert!(
            batch[2]["params"]["_meta"].get(CI).is_none(),
            "the scan must not stop at the first hit — element 3 is after element 1"
        );
        assert_eq!(
            batch[3]["params"]["_meta"][CI]["name"], "Some Client",
            "…and a legitimate element AFTER a hit survives"
        );

        // Past the depth bound, nothing is descended — same contract as `strip_reserved_meta`.
        let mut deep = frame_with_client_info(json!({"name": "eid:deep"}));
        for _ in 0..(MAX_BATCH_DEPTH + 1) {
            deep = json!([deep]);
        }
        assert!(!strip_impersonating_client_info(&mut deep));
    }

    #[test]
    fn named_and_allowed_service_is_selected_and_meta_stripped() {
        let mut init = init_with_meta(json!({"mcpmesh/service":"notes","other/key":"kept"}));
        let d = select_service(&mut init, &["notes".into()]);
        assert_eq!(d, ServiceDecision::Selected("notes".into()));
        let meta = &init["params"]["_meta"];
        assert!(
            meta.get("mcpmesh/service").is_none(),
            "reserved keys must be stripped"
        );
        assert_eq!(meta["other/key"], "kept");
    }

    #[test]
    fn caller_forged_peer_meta_never_survives() {
        let mut init =
            init_with_meta(json!({"mcpmesh/service":"notes","mcpmesh/peer":{"name":"forged"}}));
        // This test checks only the strip side effect; discarding the decision is
        // deliberate and must be explicit — which is the #[must_use] working.
        let _ = select_service(&mut init, &["notes".into()]);
        assert!(init["params"]["_meta"].get("mcpmesh/peer").is_none());
    }

    #[test]
    fn missing_meta_with_exactly_one_allowed_defaults() {
        let mut init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}});
        let d = select_service(&mut init, &["only".into()]);
        assert_eq!(d, ServiceDecision::Selected("only".into()));
    }

    #[test]
    fn missing_meta_with_two_allowed_refuses() {
        let mut init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}});
        let d = select_service(&mut init, &["a".into(), "b".into()]);
        assert!(matches!(d, ServiceDecision::Refuse));
    }

    #[test]
    fn unknown_and_unauthorized_are_indistinguishable() {
        let mut a = init_with_meta(json!({"mcpmesh/service":"nope"}));
        let mut b = init_with_meta(json!({"mcpmesh/service":"exists-but-not-yours"}));
        assert_eq!(
            select_service(&mut a, &["notes".into()]),
            select_service(&mut b, &["notes".into()])
        );
    }

    #[test]
    fn non_object_meta_passes_through_untouched_and_defaults() {
        // Binding seam contract (plan Task 9 notes): a non-object `_meta` names no
        // service and holds no reserved keys — it survives verbatim, and the
        // key-absent default rule applies. The M2 peer injector replaces, not merges.
        let mut init = json!({"jsonrpc":"2.0","id":1,"method":"initialize",
               "params":{"_meta": ["not", "an", "object"]}});
        let d = select_service(&mut init, &["only".into()]);
        assert_eq!(d, ServiceDecision::Selected("only".into()));
        assert_eq!(init["params"]["_meta"], json!(["not", "an", "object"]));
    }

    #[test]
    fn non_string_service_request_refuses_never_defaults() {
        // A non-string `mcpmesh/service` is malformed caller input: it requested
        // something unresolvable, so it must Refuse — never fall through to the
        // single-allowed default as if nothing was requested.
        let mut init = init_with_meta(json!({"mcpmesh/service": 42}));
        let d = select_service(&mut init, &["only".into()]);
        assert!(matches!(d, ServiceDecision::Refuse));
        // Stripping is unconditional even on the refusal path.
        assert!(init["params"]["_meta"].get("mcpmesh/service").is_none());
    }
}
