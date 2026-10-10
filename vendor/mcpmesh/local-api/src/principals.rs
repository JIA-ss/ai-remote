//! THE principal-set expansion: the ONE definition of a resolved caller's flat
//! authorization identity — `groups ∪ {eid} ∪ {user_id}`, empty components skipped,
//! default-deny (no identity ⇒ empty set ⇒ nothing matches).
//!
//! **Nicknames are NOT principals** (#38): display names are self-asserted and rewritable
//! (`set_nickname`, rename-by-fresh-invite), so nothing authz-bearing may key on them.
//! The device arm is the pre-rendered stable principal `eid:<hex>` — rendered by the ONE
//! sanctioned encoder, `mcpmesh_net::EndpointId::principal()` (callers pass the string;
//! this crate stays iroh-free).
//!
//! WHY THIS LIVES HERE: three enforcement sites consume this
//! expansion — the mesh service allow check (`mcpmesh-net::endpoint::caller_admits`), the
//! plugin-seam audience expansion (`service::peer_audiences`), and the blob-scope gate
//! (`node/src/blobs/provider.rs`). They live in crates with no other shared home:
//! `mcpmesh-net` must not pull the seam's tokio/rustix surface, and `mcpmesh-codec` is
//! wire-codec-only by charter. `mcpmesh-local-api`'s DEFAULT (feature-less) surface is the
//! family's dependency-free vocabulary crate — protocol types, serde only — so a pure
//! `principal_set` fn belongs on it, and `mcpmesh-net` takes a default-features-only dep
//! (types-only, no iroh/tokio cycle; local-api depends on nothing in the platform DAG).
//! One implementation, drift impossible.
use std::collections::BTreeSet;

/// Expand a resolved peer identity into its flat principal set:
/// `groups ∪ {eid} ∪ {user_id}`. Borrowed — callers match `allow`/grant entries against it.
///
/// - `eid` is the PRE-RENDERED device principal (`eid:<hex>` of the AUTHENTICATED
///   endpoint id) — always present for a gate-resolved caller.
/// - `user_id` is the person's verified id (roster, or a pairing device→user binding);
///   `None` for an unbound pairing peer. Roster user_ids are bare handles; pairing ones
///   carry the `b64u:` prefix — both land verbatim.
/// - `groups` are bare roster group names.
/// - Empty strings are never principals (an absent value must not become a matchable "").
/// - An absent/empty identity yields the EMPTY set — default deny.
pub fn principal_set<'a>(
    eid: Option<&'a str>,
    user_id: Option<&'a str>,
    groups: &'a [String],
) -> BTreeSet<&'a str> {
    let mut set: BTreeSet<&'a str> = groups
        .iter()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    for v in [eid, user_id].into_iter().flatten() {
        if !v.is_empty() {
            set.insert(v);
        }
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn principal_set_is_groups_union_eid_union_user_id() {
        let groups = vec!["eng".to_string(), "ops".to_string()];
        let set = principal_set(Some("eid:0707"), Some("b64u:BOB"), &groups);
        let want: BTreeSet<&str> = ["eng", "ops", "eid:0707", "b64u:BOB"].into();
        assert_eq!(set, want);
    }

    #[test]
    fn empty_components_are_skipped_and_absent_identity_is_default_deny() {
        // Empty strings never become principals.
        let groups = vec![String::new(), "eng".to_string()];
        let set = principal_set(Some(""), None, &groups);
        assert_eq!(set, BTreeSet::from(["eng"]));
        // No identity at all ⇒ empty set ⇒ default deny.
        assert!(principal_set(None, None, &[]).is_empty());
    }

    /// The #38 invariant: an unbound pairing-mode peer is a principal by its STABLE
    /// device id ONLY — its display nickname is never in the set, so no rename
    /// (self-rename, rename-by-fresh-invite) can ever change what it is granted.
    #[test]
    fn pairing_mode_endpoint_id_is_the_only_principal() {
        let set = principal_set(Some("eid:0707"), None, &[]);
        assert_eq!(set, BTreeSet::from(["eid:0707"]));
    }
}
