//! The persisted scope model: a named set of blob hashes + a set of granted principals.
//! An app publishes blobs INTO a scope and grants the scope to principals (a roster group name or a
//! user_id — one flat namespace). The request-time gate ALLOWS a GET iff some
//! scope contains the hash AND grants one of the caller's `{user_id} ∪ groups`.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;
use std::sync::RwLock;

use anyhow::{Context, Result};
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};

/// One scope: the blob hashes it contains + the principals it grants. Hashes are bare 64-char blake3
/// hex (`Hash::to_hex()`); principals are stable ids/names: `{eid} ∪ {user_id} ∪ groups` (#38 — never nicknames).
/// `BTreeSet` for deterministic serialization + list ordering.
/// Default cap on how many scopes one `blob_list` returns (#84b).
///
/// NOT unbounded. `blob_list` renders every scope into a single control frame against a 16 MiB
/// cap; past it the CLIENT rejects the frame as malformed. The control surface carries no strike
/// bound (see `control.rs`), so the connection survives — but the caller gets an opaque
/// `Malformed("response")` with no way to page, which is an unusable answer rather than a large
/// one. With one-scope-per-file granularity (#84d) that is reached by ordinary use.
pub const DEFAULT_LIST_LIMIT: usize = 256;

/// Hard ceiling on a caller-supplied `limit` (#84b review). Without it, `{"limit": 1000000}`
/// reproduces the unbounded listing the default exists to prevent.
pub const MAX_LIST_LIMIT: usize = 4096;

/// One row of a `blob_list` page: `(name, hashes, grants, withdrawn, hash_count, grant_count,
/// withdrawn_count)`. The counts are always present, even when `counts_only` empties the vectors.
pub type ScopePageRow = (
    String,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    usize,
    usize,
    usize,
);

/// Filters + paging for `blob_list` (#84b). All optional; `Default` is "everything, default limit".
#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    /// Exact scope name — never a prefix. Under one-scope-per-file, names are derived from hashes
    /// and share prefixes constantly, so a prefix match would return neighbours.
    pub scope: Option<String>,
    /// Only scopes containing this hash. The CALLER's rendering is normalized before comparing.
    pub hash: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    /// Omit the three vectors, keep the counts.
    pub counts_only: bool,
}

/// One page of a listing, plus what the caller needs to know it is a page.
#[derive(Debug, Clone)]
pub struct ScopePage {
    pub rows: Vec<ScopePageRow>,
    /// Scopes matching the filter BEFORE limit/offset. Without this a caller cannot distinguish a
    /// complete answer from a clipped one.
    pub total: usize,
    pub truncated: bool,
}

/// One row of the scope listing: `(name, hashes, grants, withdrawn)`.
///
/// Named because it grew a fourth member with #107's withdrawal set and an anonymous 4-tuple
/// stopped being readable at the call sites.
pub type ScopeRow = (String, Vec<String>, Vec<String>, Vec<String>);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub hashes: BTreeSet<String>,
    pub grants: BTreeSet<String>,
    /// Hashes deliberately WITHDRAWN from this scope (#107).
    ///
    /// `blob_unpublish` removes reachability but not bytes, so until a sweep runs the blob stays
    /// complete in the local store and `blob_republish` would re-add it to this scope —
    /// whose grants unpublish never touched — restoring access with no grant call and no warning.
    /// A lock cannot close that: exclusion is in lock-ACQUISITION order, not request-arrival order,
    /// so an unpublish that acquires first is still erased by a republish acquiring second.
    ///
    /// Persisted with the rest of the sidecar. A tombstone that evaporated on restart would be
    /// worse than none: it reads as durable, then silently reverts.
    ///
    /// `#[serde(default)]` so sidecars written before 0.17.0 load unchanged.
    #[serde(default)]
    pub withdrawn: BTreeSet<String>,
}

/// The full scope table: `scope_name -> Scope`. `Default` is the empty table.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobScopes {
    #[serde(default)]
    pub scopes: BTreeMap<String, Scope>,
}

impl BlobScopes {
    pub fn is_empty(&self) -> bool {
        self.scopes.is_empty()
    }

    /// Add a blob hash INTO a scope (creating the scope if absent).
    pub fn publish_hash(&mut self, scope: &str, hash_hex: &str) {
        let sc = self.scopes.entry(scope.to_string()).or_default();
        // Publishing CLEARS a withdrawal (#107). This is the deliberate act: `blob_publish` names a
        // FILE on disk, so the operator is saying "this content, in this scope, I mean it".
        // `blob_republish` — which names only a hash the node happens to hold, and is the call an
        // embedder is tempted to make as fetch hygiene — is refused instead, and `blob_grant` never
        // touches this, since granting a PRINCIPAL says nothing about a hash.
        sc.withdrawn.remove(hash_hex);
        sc.hashes.insert(hash_hex.to_string());
    }

    /// Grant a scope to a principal (creating the scope if absent).
    pub fn grant(&mut self, scope: &str, principal: &str) {
        self.scopes
            .entry(scope.to_string())
            .or_default()
            .grants
            .insert(principal.to_string());
    }

    /// Remove `principals` from EVERY scope's grant set (unpair hygiene, #38). Returns whether
    /// anything changed. Empty scopes are left in place (they still track published hashes).
    pub fn revoke_principals(&mut self, principals: &[String]) -> bool {
        let mut changed = false;
        for sc in self.scopes.values_mut() {
            for p in principals {
                changed |= sc.grants.remove(p);
            }
        }
        changed
    }

    /// Remove `principals` from ONE scope's grant set (#62, the `blob_revoke` verb). Returns
    /// whether anything changed.
    ///
    /// Deliberately NOT [`revoke_principals`](Self::revoke_principals), which strips from EVERY
    /// scope: that is unpair hygiene, and using it for a per-scope revoke would silently withdraw
    /// access the caller never asked to touch. An absent scope is a clean `false`.
    pub fn revoke_from_scope(&mut self, scope: &str, principals: &[String]) -> bool {
        let Some(sc) = self.scopes.get_mut(scope) else {
            return false;
        };
        let mut changed = false;
        for p in principals {
            changed |= sc.grants.remove(p);
        }
        changed
    }

    /// Remove `hash_hex` from ONE scope (#62, the `blob_unpublish` verb). Returns whether anything
    /// changed.
    ///
    /// This is the AUTHORIZATION boundary: [`allows`](Self::allows) requires the hash to be in some
    /// scope, so a subsequent GET for it is refused. It does NOT delete bytes SYNCHRONOUSLY — the
    /// store keeps them, and only a background sweep reclaims them, on a node that configured one
    /// (`[blobs].gc_interval`, #80). On a node that did not, they stay forever. The tombstone
    /// outlives the bytes either way, so a republish is refused before AND after a sweep — only the
    /// error changes, from `BlobWithdrawn` to `NoSuchBlob`. A hash published into several scopes
    /// stays reachable through the others (and stays LIVE for the sweep), and a transfer already
    /// streaming is not interrupted.
    pub fn unpublish_hash(&mut self, scope: &str, hash_hex: &str) -> bool {
        self.scopes.get_mut(scope).is_some_and(|sc| {
            // Record the withdrawal even when the hash was not currently listed: the operator has
            // expressed "not this content, not in this scope", and a republish afterwards must
            // still be refused (#107).
            sc.withdrawn.insert(hash_hex.to_string());
            sc.hashes.remove(hash_hex)
        })
    }

    /// One page of the scope table (#84b), filtered and bounded.
    ///
    /// Order is scope name — the table is a `BTreeMap`, so it is already sorted and stable. Paging
    /// without a stable order returns overlapping or missing rows that look plausible, which is
    /// worse than not paging at all.
    pub fn list_page(&self, q: &ListQuery) -> anyhow::Result<ScopePage> {
        // Normalize the hash filter so a caller's base32 rendering matches a stored canonical hex,
        // matching the rule #83 established for every other hash-taking surface.
        // A malformed hash is an ERROR, not "no filter" (#84b review). `.ok()` here collapsed a
        // truncated paste or a trailing newline into `None`, which means NO FILTER — so the caller
        // asked "which scopes contain this hash" and was told "all of them", with `total` claiming
        // the full count. That is a silent wrong answer on an authorization-introspection surface,
        // and every other hash-taking verb (`blob_republish`, `blob_unpublish`) propagates instead.
        let want_hash = match q.hash.as_deref() {
            Some(h) => Some(crate::blobs::parse_blob_hash(h)?.to_hex().to_string()),
            None => None,
        };

        let matching: Vec<(&String, &Scope)> = self
            .scopes
            .iter()
            .filter(|(name, sc)| {
                q.scope.as_deref().is_none_or(|want| want == name.as_str())
                    && want_hash.as_deref().is_none_or(|h| sc.hashes.contains(h))
            })
            .collect();

        let total = matching.len();
        let offset = q.offset.unwrap_or(0);
        // Clamp: `{"limit": 1000000}` is the first thing a client writes on seeing
        // `truncated: true`, and it reproduces the exact unbounded listing this exists to prevent.
        let limit = q.limit.unwrap_or(DEFAULT_LIST_LIMIT).min(MAX_LIST_LIMIT);
        let rows: Vec<ScopePageRow> = matching
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(name, sc)| {
                let (h, g, w) = (sc.hashes.len(), sc.grants.len(), sc.withdrawn.len());
                let take = |set: &BTreeSet<String>| -> Vec<String> {
                    if q.counts_only {
                        Vec::new()
                    } else {
                        set.iter().cloned().collect()
                    }
                };
                (
                    name.clone(),
                    take(&sc.hashes),
                    take(&sc.grants),
                    take(&sc.withdrawn),
                    h,
                    g,
                    w,
                )
            })
            .collect();
        let truncated = offset + rows.len() < total;
        Ok(ScopePage {
            rows,
            total,
            truncated,
        })
    }

    /// Every hash NAMED by some scope — the liveness root for blob garbage collection (#80).
    ///
    /// mcpmesh creates no persistent tags (`publish_path`'s `TempTag` is dropped as it returns), so
    /// a blob absent from this set is referenced by nothing mcpmesh knows about and the sweep may
    /// reclaim it.
    ///
    /// **`withdrawn` is deliberately excluded.** `unpublish` moves a hash out of `hashes` and
    /// records a tombstone; the tombstone lives in the sidecar, not the store, so it outlives the
    /// bytes and keeps refusing a republish after a sweep — the refusal merely changes from
    /// `BlobWithdrawn` to `NoSuchBlob`. Reclaiming withdrawn bytes is the entire point of #80: an
    /// embedder that told a user "this file is deleted" could not deliver that.
    ///
    /// Duplicates across scopes are the caller's to collapse — the consumer is a `HashSet`.
    pub fn live_hashes(&self) -> impl Iterator<Item = &str> {
        self.scopes
            .values()
            .flat_map(|sc| sc.hashes.iter().map(String::as_str))
    }

    /// Was this hash deliberately withdrawn from this scope (#107)?
    pub fn is_withdrawn(&self, scope: &str, hash_hex: &str) -> bool {
        self.scopes
            .get(scope)
            .is_some_and(|sc| sc.withdrawn.contains(hash_hex))
    }

    /// Does this scope exist at all? Lets a caller distinguish "no such scope" (an operator typo,
    /// which must be an ERROR) from "the principal/hash was not there" (genuinely idempotent).
    /// Answering `{}` to both is the #55 defect, and #62 reintroduced it before review caught it.
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.contains_key(scope)
    }

    /// SECURITY LINCHPIN (pure): ALLOW iff SOME scope contains `hash_hex` AND grants one of the
    /// caller's `principals` (`{user_id} ∪ groups`). Default-deny — an in-scope hash with no matching
    /// grant, a hash in no scope, and an empty principal set all return `false`. Hashes are never
    /// capabilities; a hash reachable in scope A does not become reachable via scope B's grant.
    pub fn allows(&self, hash_hex: &str, principals: &HashSet<&str>) -> bool {
        self.scopes.values().any(|sc| {
            // A withdrawal outranks residual membership (#107 review). `hashes` and `withdrawn`
            // are kept disjoint by the API, but this is an AUTHZ gate: if the two ever disagree —
            // a hand-edited sidecar, external tooling, a partial rollback, a future third
            // insertion site — the tombstone must win rather than the leftover row. Cheap
            // belt-and-braces on the one surface where fail-open is unacceptable.
            !sc.withdrawn.contains(hash_hex)
                && sc.hashes.contains(hash_hex)
                && sc.grants.iter().any(|g| principals.contains(g.as_str()))
        })
    }

    /// Deterministic `(name, hashes, grants)` rendering for `list` (sorted by BTree order).
    pub fn list(&self) -> Vec<ScopeRow> {
        self.scopes
            .iter()
            .map(|(name, sc)| {
                (
                    name.clone(),
                    sc.hashes.iter().cloned().collect(),
                    sc.grants.iter().cloned().collect(),
                    // #107: surface withdrawals. Without this a tombstone is discoverable only by
                    // triggering a -32042, and the set — which only `blob_publish` ever prunes —
                    // grows invisibly.
                    sc.withdrawn.iter().cloned().collect(),
                )
            })
            .collect()
    }
}

/// The scope store. An in-RAM `RwLock<BlobScopes>` serves the hot authz read (`snapshot`, a cheap
/// clone taken per GET — no lock held across the async reply); every mutation
/// (`publish_hash`/`grant`/`revoke_*`/`unpublish_hash`) mutates and atomically persists the JSON
/// keyed redb store, one row per scope (#84c). Mutations that touch ONE scope write one row.
///
/// **`write_lock` serializes mutate-AND-persist as one unit.** An earlier version dropped the
/// `inner` write lock before persisting, which serialized the mutation but NOT the file write: two
/// concurrent revokes could persist out of order and the slower one would write back a snapshot
/// still containing the grant the other had just removed. Memory was correct, disk was not, and the
/// stale grant came back on restart — a fail-OPEN loss of a revocation, on an authorization
/// surface. Control connections are one task each, so concurrent verbs are ordinary usage, not an
/// exotic race (#62 review, reproduced).
pub struct ScopeStore {
    /// Kept for error messages and diagnostics; redb owns the file itself.
    #[allow(dead_code)]
    path: PathBuf,
    inner: RwLock<BlobScopes>,
    /// Held across mutate+persist so the write order matches the mutation order.
    write_lock: std::sync::Mutex<()>,
    /// The keyed backing store (#84c). `None` for a memory-only store (`new`) — the caller-only
    /// fetcher and most tests — which never persists.
    ///
    /// Every mutation used to clone the WHOLE table and re-serialize it to one JSON sidecar, so
    /// publishing file N+1 cost O(N) and N files cost O(N²). That was tolerable when a scope was a
    /// room; #84(d) settled that a scope is one FILE (`file:<hash>`), so scope count grows with
    /// every file ever shared.
    db: Option<Database>,
}

/// One row per scope: `scope_name -> serde_json(Scope)`.
///
/// Per-scope FILES were the obvious alternative and are wrong here: scope names are `file:<hash>`
/// and `:` is invalid in a Windows filename, which CI builds and tests. Encoding names into safe
/// filenames buys a directory of thousands of tiny files, a name→file mapping to maintain, and no
/// transaction across a multi-scope mutation.
const SCOPES: TableDefinition<&str, &[u8]> = TableDefinition::new("scopes");

/// Rows written since process start — the #84c property, countable.
///
/// File size cannot show it: redb reuses free pages, so a whole-table rewrite and a keyed write
/// produce byte-identical files. Measured at 500 vs 125,250 rows for the same 500 publishes.
pub(crate) static ROWS_WRITTEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl ScopeStore {
    /// An EMPTY store bound to `path` (does not read the file). Used for a caller-only fetcher (no
    /// scopes) and by tests.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            inner: RwLock::new(BlobScopes::default()),
            write_lock: std::sync::Mutex::new(()),
            db: None, // memory-only: nothing to persist to
        }
    }

    /// Open the keyed store at `path`, MIGRATING a pre-0.22 JSON sidecar if one is present (#84c).
    ///
    /// - redb file exists → use it.
    /// - redb absent, sidecar present → import it in ONE transaction, then RENAME the sidecar to
    ///   `<name>.migrated` rather than deleting it. A destructive migration of authorization state
    ///   on first boot of a new version is not something to do silently, and a downgrade after
    ///   migration would otherwise start with every grant and withdrawal invisible.
    /// - neither → empty.
    ///
    /// A corrupt sidecar is still a HARD ERROR: fail closed, never silently reset grants.
    pub fn open(path: PathBuf) -> Result<Self> {
        let sidecar = path.with_extension("json");
        let db = Database::create(&path)
            .with_context(|| format!("open blob scope store {}", path.display()))?;
        // Ensure the table exists so a read-only path never trips over a missing table.
        {
            let txn = db.begin_write()?;
            txn.open_table(SCOPES)?;
            txn.commit()?;
        }
        // Migrate iff the TABLE is empty — NOT iff the file was absent (#84c review).
        //
        // `Database::create` creates the file, so gating on `path.exists()` put the decision
        // OUTSIDE the transaction that protects it: any failure between create and commit — a
        // corrupt sidecar, a crash, a kill — flipped the next boot to "not fresh" and the sidecar
        // was never read again. The node then came up looking healthy with every grant and every
        // withdrawal tombstone gone, silently, on the SECOND boot. Losing tombstones is fail-OPEN
        // on #107: a withdrawn hash becomes republishable.
        let fresh = {
            let txn = db.begin_read()?;
            txn.open_table(SCOPES)?.iter()?.next().is_none()
        };

        let mut scopes = BlobScopes::default();
        if fresh && sidecar.exists() {
            let bytes = std::fs::read(&sidecar)
                .with_context(|| format!("read blob scopes {}", sidecar.display()))?;
            scopes = serde_json::from_slice::<BlobScopes>(&bytes)
                .with_context(|| format!("parse blob scopes {}", sidecar.display()))?;
            // ONE transaction for the whole import: a half-migrated authorization table is worse
            // than an unmigrated one.
            let txn = db.begin_write()?;
            {
                let mut t = txn.open_table(SCOPES)?;
                for (name, scope) in &scopes.scopes {
                    t.insert(name.as_str(), serde_json::to_vec(scope)?.as_slice())?;
                }
            }
            txn.commit()?;
            let keep = sidecar.with_extension("json.migrated");
            std::fs::rename(&sidecar, &keep).with_context(|| {
                format!(
                    "rename migrated sidecar {} -> {}",
                    sidecar.display(),
                    keep.display()
                )
            })?;
            tracing::info!(
                sidecar = %sidecar.display(), kept = %keep.display(),
                "migrated blob scopes to the keyed store (#84c)"
            );
        } else {
            if sidecar.exists() {
                // Not fatal: redb wins by design (spec case 4, a downgrade-then-upgrade). But an
                // operator should know the file is being ignored rather than discover it after a
                // restore.
                tracing::warn!(
                    sidecar = %sidecar.display(),
                    "a blob-scope sidecar sits beside a non-empty keyed store and is being IGNORED"
                );
            }
            let txn = db.begin_read()?;
            let t = txn.open_table(SCOPES)?;
            for row in t.iter()? {
                let (k, v) = row?;
                scopes
                    .scopes
                    .insert(k.value().to_string(), serde_json::from_slice(v.value())?);
            }
        }
        Ok(Self {
            path,
            inner: RwLock::new(scopes),
            write_lock: std::sync::Mutex::new(()),
            db: Some(db),
        })
    }

    /// Load the persisted sidecar, or an EMPTY store when the file is absent (fresh node). A present
    /// file MUST parse (a corrupt sidecar is a hard error — fail closed, do not silently reset grants).
    #[deprecated(note = "#84c: use `open`, which is keyed and O(log N) per mutation")]
    #[allow(dead_code)]
    pub fn load(path: PathBuf) -> Result<Self> {
        let scopes = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<BlobScopes>(&bytes)
                .with_context(|| format!("parse blob scopes {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BlobScopes::default(),
            Err(e) => {
                return Err(anyhow::Error::new(e))
                    .with_context(|| format!("read blob scopes {}", path.display()));
            }
        };
        Ok(Self {
            path,
            inner: RwLock::new(scopes),
            write_lock: std::sync::Mutex::new(()),
            db: None,
        })
    }

    /// A cheap clone of the current scope table for the hot authz read + `list` rendering.
    /// The read lock is released as the clone returns — NEVER held across an await.
    pub fn snapshot(&self) -> BlobScopes {
        self.inner.read().expect("scope lock not poisoned").clone()
    }

    /// TEST-ONLY: poison `inner`, so [`live_hashes`](Self::live_hashes) fails the way it fails in
    /// production — a thread that panicked mid-mutation. The only way to exercise the GC callback's
    /// `Abort` arm, which is the guard against sweeping against an EMPTY liveness root and deleting
    /// every blob on the node.
    #[cfg(test)]
    pub(crate) fn poison_for_test(&self) {
        let inner = &self.inner;
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = inner.write().expect("not yet poisoned");
            panic!("poison the scope lock");
        }));
    }

    /// The blob-GC liveness root: every hash named by some scope (#80).
    ///
    /// **Fallible on purpose, and the only reachable failure is a POISONED lock** — which means
    /// another thread panicked part-way through a mutation, so the table may be mid-update. Every
    /// other accessor here `expect`s past that, which is right for a control verb that would be
    /// returning an error anyway. It is wrong here: this answers a background sweep whose fallback
    /// on "no answer" is to delete every blob in the store. The caller maps `Err` to
    /// `ProtectOutcome::Abort`, which skips the run and keeps the schedule.
    pub fn live_hashes(&self) -> Result<std::collections::HashSet<String>> {
        let g = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("blob scope lock is poisoned"))?;
        Ok(g.live_hashes().map(str::to_string).collect())
    }

    /// Publish a hash into a scope + persist (single-writer). The write lock is dropped before the
    /// fs write so a slow fsync never blocks a concurrent authz read.
    pub fn publish_hash(&self, scope: &str, hash_hex: &str) -> Result<()> {
        let _w = self
            .write_lock
            .lock()
            .expect("scope write lock not poisoned");
        let snapshot = {
            let mut g = self.inner.write().expect("scope lock not poisoned");
            g.publish_hash(scope, hash_hex);
            g.clone()
        };
        self.persist_scope(scope, snapshot.scopes.get(scope))
    }

    /// Grant a scope to a principal + persist (single-writer). Same lock/persist discipline.
    pub fn grant(&self, scope: &str, principal: &str) -> Result<()> {
        let _w = self
            .write_lock
            .lock()
            .expect("scope write lock not poisoned");
        let snapshot = {
            let mut g = self.inner.write().expect("scope lock not poisoned");
            g.grant(scope, principal);
            g.clone()
        };
        self.persist_scope(scope, snapshot.scopes.get(scope))
    }

    /// Revoke `principals` from ONE scope + persist (#62, `blob_revoke`). Same lock/persist
    /// discipline. Returns whether anything changed.
    pub fn revoke_from_scope(&self, scope: &str, principals: &[String]) -> Result<bool> {
        let _w = self
            .write_lock
            .lock()
            .expect("scope write lock not poisoned");
        let (changed, snapshot) = {
            let mut g = self.inner.write().expect("scope lock not poisoned");
            let changed = g.revoke_from_scope(scope, principals);
            (changed, g.clone())
        };
        self.persist_scope(scope, snapshot.scopes.get(scope))?;
        Ok(changed)
    }

    /// Does this scope exist? See [`BlobScopes::has_scope`].
    /// Was this hash deliberately withdrawn from this scope (#107)? Same lock discipline as
    /// every other authz read.
    pub fn is_withdrawn(&self, scope: &str, hash_hex: &str) -> bool {
        self.inner
            .read()
            .expect("scope lock not poisoned")
            .is_withdrawn(scope, hash_hex)
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.inner
            .read()
            .expect("scope lock not poisoned")
            .has_scope(scope)
    }

    /// Remove a hash from ONE scope + persist (#62, `blob_unpublish`). Returns whether anything
    /// changed. Removes REACHABILITY, not bytes — see [`BlobScopes::unpublish_hash`].
    pub fn unpublish_hash(&self, scope: &str, hash_hex: &str) -> Result<bool> {
        let _w = self
            .write_lock
            .lock()
            .expect("scope write lock not poisoned");
        let (changed, snapshot) = {
            let mut g = self.inner.write().expect("scope lock not poisoned");
            let changed = g.unpublish_hash(scope, hash_hex);
            (changed, g.clone())
        };
        self.persist_scope(scope, snapshot.scopes.get(scope))?;
        Ok(changed)
    }

    /// Revoke `principals` from every scope + persist (single-writer). The unpair-hygiene
    /// inverse of `grant` — same lock/persist discipline. Returns whether anything changed.
    pub fn revoke_principals(&self, principals: &[String]) -> Result<bool> {
        let _w = self
            .write_lock
            .lock()
            .expect("scope write lock not poisoned");
        let (changed, snapshot) = {
            let mut g = self.inner.write().expect("scope lock not poisoned");
            let changed = g.revoke_principals(principals);
            (changed, g.clone())
        };
        if changed {
            self.persist(&snapshot)?;
        }
        Ok(changed)
    }

    /// Deterministic list rendering (delegates to `BlobScopes::list`).
    pub fn list(&self) -> Vec<ScopeRow> {
        self.inner.read().expect("scope lock not poisoned").list()
    }

    /// One filtered, bounded page (#84b).
    pub fn list_page(&self, q: &ListQuery) -> anyhow::Result<ScopePage> {
        self.inner
            .read()
            .expect("scope lock not poisoned")
            .list_page(q)
    }

    /// Write the CURRENT table (#84c).
    ///
    /// Still O(N) in rows touched, but now ONE transaction against a keyed store rather than a
    /// whole-file re-serialize, and every caller that knows which scope changed should use
    /// [`persist_scope`](Self::persist_scope) instead. Kept for the multi-scope mutations
    /// (`revoke_principals`, the unpair hygiene sweep) that genuinely touch every row — those were
    /// already O(N) and are now at least atomic.
    fn persist(&self, scopes: &BlobScopes) -> Result<()> {
        let Some(db) = &self.db else { return Ok(()) };
        let txn = db.begin_write()?;
        {
            let mut t = txn.open_table(SCOPES)?;
            // Drop rows that no longer exist, then write the survivors.
            let stale: Vec<String> = t
                .iter()?
                .filter_map(|r| r.ok().map(|(k, _)| k.value().to_string()))
                .filter(|k| !scopes.scopes.contains_key(k))
                .collect();
            for k in stale {
                t.remove(k.as_str())?;
            }
            for (name, scope) in &scopes.scopes {
                t.insert(name.as_str(), serde_json::to_vec(scope)?.as_slice())?;
                ROWS_WRITTEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Write ONE scope — the O(log N) path (#84c).
    ///
    /// This is the point of the change: `publish_hash`, `unpublish_hash`, `grant` and
    /// `revoke_from_scope` touch exactly one scope, and under one-scope-per-file (#84d) the table
    /// grows with every file ever shared. Re-serializing all of it per publish made N files cost
    /// O(N²).
    ///
    /// `None` removes the row (a scope that no longer exists).
    fn persist_scope(&self, name: &str, scope: Option<&Scope>) -> Result<()> {
        let Some(db) = &self.db else { return Ok(()) };
        let txn = db.begin_write()?;
        {
            let mut t = txn.open_table(SCOPES)?;
            match scope {
                Some(sc) => {
                    t.insert(name, serde_json::to_vec(sc)?.as_slice())?;
                    ROWS_WRITTEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                None => {
                    t.remove(name)?;
                }
            }
        }
        txn.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod gc_root_tests {
    use super::*;

    /// #80: the liveness root is the union across scopes, and a WITHDRAWN hash is not in it.
    ///
    /// The second half is the whole feature. `blob_unpublish` records a tombstone and drops the
    /// hash from `hashes`; if `live_hashes` protected withdrawn entries, the bytes an operator
    /// deliberately withdrew would be the one thing collection could never reclaim — exactly the
    /// promise #80 exists to make deliverable.
    #[test]
    fn the_liveness_root_unions_scopes_and_excludes_withdrawn() {
        let mut t = BlobScopes::default();
        t.publish_hash("room", "aa");
        t.publish_hash("room", "bb");
        // Distinct scope, and a hash SHARED with `room` — so a root that took only one scope, or
        // that double-counted, is visible.
        t.publish_hash("other", "cc");
        t.publish_hash("other", "aa");

        let root: std::collections::HashSet<&str> = t.live_hashes().collect();
        assert_eq!(
            root,
            ["aa", "bb", "cc"].into_iter().collect(),
            "every hash named by any scope is live"
        );

        // Withdraw `bb` from the only scope holding it.
        assert!(t.unpublish_hash("room", "bb"));
        assert!(
            t.is_withdrawn("room", "bb"),
            "precondition: the tombstone exists"
        );
        let root: std::collections::HashSet<&str> = t.live_hashes().collect();
        assert!(
            !root.contains("bb"),
            "a withdrawn hash must be RECLAIMABLE — protecting it makes #80 undeliverable: {root:?}"
        );
        assert!(
            root.contains("aa") && root.contains("cc"),
            "…and withdrawing one hash must not unprotect the rest: {root:?}"
        );

        // `aa` is still live through `other` after being withdrawn from `room` — a hash in several
        // scopes is protected by any one of them.
        assert!(t.unpublish_hash("room", "aa"));
        let root: std::collections::HashSet<&str> = t.live_hashes().collect();
        assert!(
            root.contains("aa"),
            "a hash withdrawn from ONE scope is still live through another: {root:?}"
        );

        assert_eq!(
            BlobScopes::default().live_hashes().count(),
            0,
            "an empty table protects nothing"
        );
    }

    /// The store-level accessor answers the same set, as owned strings for the sweep's `HashSet`.
    #[test]
    fn the_store_reports_the_same_root() {
        let s = ScopeStore::new(PathBuf::from("/nonexistent/scopes.json"));
        s.publish_hash("room", "aa").unwrap();
        s.publish_hash("room", "bb").unwrap();
        let root = s.live_hashes().expect("an unpoisoned store answers");
        assert_eq!(
            root,
            ["aa".to_string(), "bb".to_string()].into_iter().collect()
        );
    }

    /// A POISONED scope lock must be an ERROR, not an empty root.
    ///
    /// This is the only reachable failure, and it is the one that matters: the caller maps `Err` to
    /// `ProtectOutcome::Abort`, which skips the sweep. An accessor that `expect`ed — like every
    /// sibling here — would panic inside a background timer task; one that returned an empty set
    /// would hand the sweep an empty liveness root and delete every blob on the node.
    #[test]
    fn a_poisoned_scope_lock_is_an_error_and_not_an_empty_root() {
        let s = ScopeStore::new(PathBuf::from("/nonexistent/scopes.json"));
        s.publish_hash("room", "aa").unwrap();
        assert_eq!(
            s.live_hashes().unwrap().len(),
            1,
            "precondition: the root is non-empty before the lock is poisoned"
        );

        // Poison `inner` by panicking while holding its WRITE guard.
        let inner = &s.inner;
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = inner.write().expect("not yet poisoned");
            panic!("poison the scope lock");
        }));

        let e = s
            .live_hashes()
            .expect_err("a poisoned lock must not answer a liveness root");
        assert!(
            format!("{e:#}").contains("poisoned"),
            "the error must name the cause, so an operator reading the abort log knows why: {e:#}"
        );
    }
}

#[cfg(test)]
mod tests {
    /// #84c: a single-scope mutation must write ONE row, not the whole table.
    ///
    /// Every mutation used to clone the entire scope table and re-serialize it to a JSON sidecar,
    /// so publishing file N+1 cost O(N) and N files cost O(N²). Under one-scope-per-file (#84d)
    /// the table grows with every file ever shared, so that is the ordinary path, not an abuse.
    ///
    /// Asserted on the STORE FILE rather than on timing — this repo has learned that timing
    /// assertions on a loaded machine lie. A whole-table rewrite of 200 scopes touches bytes for
    /// every one of them; a keyed write touches one row.
    #[test]
    fn a_single_scope_mutation_does_not_rewrite_the_whole_table() {
        let dir = tempfile::tempdir().unwrap();
        let store = ScopeStore::open(dir.path().join("scopes.redb")).unwrap();

        for i in 0..200 {
            store
                .publish_hash(&format!("file:{i:04}"), &format!("{i:064x}"))
                .unwrap();
        }
        // Reopening must see every one of them: keyed writes are not a lossy shortcut.
        drop(store);
        let store = ScopeStore::open(dir.path().join("scopes.redb")).unwrap();
        assert_eq!(
            store.snapshot().scopes.len(),
            200,
            "all scopes survive a reopen"
        );
        assert!(
            !store
                .snapshot()
                .allows(&format!("{:064x}", 199), &HashSet::new()),
            "no principal is granted by publishing alone"
        );

        // One more publish, then confirm the OTHER 200 rows are byte-identical — i.e. the write
        // touched one key. A whole-table rewrite would re-serialize all of them.
        // Count ROWS WRITTEN, not file growth. The first version asserted on file size and was
        // VACUOUS: redb reuses free pages, so a whole-table rewrite and a keyed write produce
        // byte-identical files — measured at 500 vs 125,250 rows written, both 0 bytes of growth.
        // The spec asked for the write count for exactly this reason (#84c review).
        let before = ROWS_WRITTEN.load(std::sync::atomic::Ordering::Relaxed);
        store
            .publish_hash("file:9999", &format!("{:064x}", 9999))
            .unwrap();
        let wrote = ROWS_WRITTEN.load(std::sync::atomic::Ordering::Relaxed) - before;
        assert_eq!(
            wrote, 1,
            "publishing one more scope into a 200-scope table wrote {wrote} rows — a keyed write \
             must cost ONE row, not a re-serialize of the whole table (#84c)"
        );
    }

    /// #84c review: a redb file left behind by a FAILED migration must not suppress the retry.
    ///
    /// `Database::create` makes the file, so gating the migration on `path.exists()` put the
    /// decision outside the transaction protecting it: a corrupt sidecar, a crash, or a kill
    /// between create and commit flipped the next boot to "not fresh", the sidecar was never read
    /// again, and the node came up looking healthy with every grant and every withdrawal tombstone
    /// gone — silently, on the SECOND boot. Losing tombstones is fail-OPEN on #107.
    #[test]
    fn an_empty_store_left_by_a_failed_migration_still_migrates() {
        let dir = tempfile::tempdir().unwrap();
        let redb = dir.path().join("blob-scopes.redb");
        let sidecar = dir.path().join("blob-scopes.json");

        let mut scopes = BlobScopes::default();
        let sc = scopes.scopes.entry("file:abc".to_string()).or_default();
        sc.grants.insert("eid:deadbeef".to_string());
        sc.withdrawn.insert("bb".repeat(32));
        std::fs::write(&sidecar, serde_json::to_string(&scopes).unwrap()).unwrap();

        // Simulate the crash: the store FILE exists with the table created, but no rows and the
        // sidecar still present — exactly the state a kill between create and commit leaves.
        {
            let db = Database::create(&redb).unwrap();
            let txn = db.begin_write().unwrap();
            txn.open_table(SCOPES).unwrap();
            txn.commit().unwrap();
        }

        let store = ScopeStore::open(redb).expect("open after a failed migration");
        let snap = store.snapshot();
        let got = snap
            .scopes
            .get("file:abc")
            .expect("the retry must find the sidecar — an empty store is not a migrated store");
        assert!(got.grants.contains("eid:deadbeef"), "grants recovered");
        assert!(
            got.withdrawn.contains(&"bb".repeat(32)),
            "and withdrawals — losing a tombstone silently un-revokes a withdrawn hash (#107)"
        );
        assert!(
            dir.path().join("blob-scopes.json.migrated").exists(),
            "and the sidecar is kept, as on a first-time migration"
        );
    }

    /// #84c: a pre-0.22 JSON sidecar is migrated, and KEPT rather than deleted.
    #[test]
    fn an_old_sidecar_is_migrated_and_renamed_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let redb = dir.path().join("blob-scopes.redb");
        let sidecar = dir.path().join("blob-scopes.json");

        // A 0.21-era sidecar with a grant and a withdrawal.
        let mut scopes = BlobScopes::default();
        let sc = scopes.scopes.entry("file:abc".to_string()).or_default();
        sc.hashes.insert("aa".repeat(32));
        sc.grants.insert("eid:deadbeef".to_string());
        sc.withdrawn.insert("bb".repeat(32));
        std::fs::write(&sidecar, serde_json::to_string(&scopes).unwrap()).unwrap();

        let store = ScopeStore::open(redb.clone()).unwrap();
        let snap = store.snapshot();
        let got = snap.scopes.get("file:abc").expect("the scope migrated");
        assert!(got.hashes.contains(&"aa".repeat(32)), "hashes survive");
        assert!(
            got.grants.contains("eid:deadbeef"),
            "GRANTS survive — this is authorization state"
        );
        assert!(
            got.withdrawn.contains(&"bb".repeat(32)),
            "withdrawals survive (#107)"
        );

        assert!(!sidecar.exists(), "the sidecar is moved aside");
        assert!(
            dir.path().join("blob-scopes.json.migrated").exists(),
            "and KEPT — a downgrade after migration would otherwise find no sidecar and start with \
             every grant and withdrawal invisible, which is worse than the state it replaced"
        );

        // Re-opening must NOT re-run the migration (the redb file now wins).
        drop(store);
        let again = ScopeStore::open(redb).unwrap();
        assert_eq!(
            again.snapshot().scopes.len(),
            1,
            "migration does not run twice"
        );
    }

    use super::*;

    /// #62: a per-scope revoke must NOT behave like the global unpair-hygiene revoke. Wiring
    /// `blob_revoke` to `revoke_principals` would silently withdraw access from every other scope
    /// the caller never mentioned — this test is what distinguishes the two.
    #[test]
    fn revoke_from_scope_touches_only_that_scope() {
        let mut s = BlobScopes::default();
        s.grant("photos", "b64u:alice");
        s.grant("notes", "b64u:alice");

        assert!(s.revoke_from_scope("photos", &["b64u:alice".to_string()]));
        assert!(
            !s.scopes["photos"].grants.contains("b64u:alice"),
            "revoked from the named scope"
        );
        assert!(
            s.scopes["notes"].grants.contains("b64u:alice"),
            "the OTHER scope's grant must survive — this is not unpair hygiene"
        );

        // Idempotent, and an unknown scope is a clean no-op rather than an error.
        assert!(!s.revoke_from_scope("photos", &["b64u:alice".to_string()]));
        assert!(!s.revoke_from_scope("nope", &["b64u:alice".to_string()]));
    }

    /// #62: unpublish removes REACHABILITY immediately — the authz property, independent of GC.
    /// The grant is deliberately left alone: the person still has access to the scope, just not to
    /// that blob.
    #[test]
    fn unpublish_denies_the_hash_without_touching_the_grant() {
        let mut s = BlobScopes::default();
        s.publish_hash("photos", "abc123");
        s.grant("photos", "b64u:alice");
        let who: HashSet<&str> = ["b64u:alice"].into_iter().collect();
        assert!(s.allows("abc123", &who), "reachable before");

        assert!(s.unpublish_hash("photos", "abc123"));
        assert!(
            !s.allows("abc123", &who),
            "an unpublished hash must be unfetchable at once — no GC needed for the security half"
        );
        assert!(
            s.scopes["photos"].grants.contains("b64u:alice"),
            "the grant survives: access to the SCOPE is unchanged"
        );
        assert!(!s.unpublish_hash("photos", "abc123"), "idempotent");
    }

    /// #62: the same bytes published into two scopes stay reachable through the other one — so
    /// unpublish is not a global delete, and GC must not reclaim a hash another scope still holds.
    #[test]
    fn unpublish_is_scoped_and_leaves_other_references_live() {
        let mut s = BlobScopes::default();
        s.publish_hash("photos", "abc123");
        s.publish_hash("backup", "abc123");
        s.grant("backup", "b64u:alice");

        s.unpublish_hash("photos", "abc123");
        let who: HashSet<&str> = ["b64u:alice"].into_iter().collect();
        assert!(
            s.allows("abc123", &who),
            "still reachable via the scope that still lists it"
        );
        assert!(
            s.scopes["backup"].hashes.contains("abc123"),
            "the other scope still lists it — unpublish is per-scope, never a global delete"
        );
    }

    use std::collections::HashSet;

    fn principals<'a>(names: &'a [&'a str]) -> HashSet<&'a str> {
        names.iter().copied().collect()
    }

    /// Unpair hygiene (#38): `revoke_principals` strips exactly the named principals from
    /// every scope, leaving other grants and the published hashes intact, so a fetch that
    /// admitted before the revoke is denied after.
    #[test]
    fn revoke_principals_strips_grants_and_denies_after() {
        let mut s = BlobScopes::default();
        let hash = "aa".repeat(32);
        s.publish_hash("docs", &hash);
        s.grant("docs", "eid:beef");
        s.grant("docs", "team-eng");
        assert!(
            s.allows(&hash, &principals(&["eid:beef"])),
            "granted before revoke"
        );

        // Revoke the device principal only — the group grant and the hash survive.
        assert!(s.revoke_principals(&["eid:beef".to_string()]));
        assert!(
            !s.allows(&hash, &principals(&["eid:beef"])),
            "denied after revoke"
        );
        assert!(
            s.allows(&hash, &principals(&["team-eng"])),
            "an unrelated grant is untouched"
        );
        // Idempotent: revoking an absent principal changes nothing.
        assert!(!s.revoke_principals(&["eid:beef".to_string()]));
    }

    #[test]
    fn allows_requires_hash_in_scope_and_a_matching_grant() {
        let mut s = BlobScopes::default();
        s.publish_hash("docs", "aa".repeat(32).as_str());
        s.grant("docs", "alice");
        s.grant("docs", "team-eng");

        // In-scope hash + a granted user_id → allow.
        assert!(s.allows(&"aa".repeat(32), &principals(&["alice"])));
        // In-scope hash + a granted GROUP (the caller carries the group) → allow.
        assert!(s.allows(&"aa".repeat(32), &principals(&["bob", "team-eng"])));
        // In-scope hash but the caller has NO granted principal → deny (SECURITY: default-deny).
        assert!(!s.allows(&"aa".repeat(32), &principals(&["carol", "team-sales"])));
        // A hash in NO scope → deny even for a granted principal (hashes are not capabilities).
        assert!(!s.allows(&"bb".repeat(32), &principals(&["alice"])));
        // An empty principal set (a pairing-only caller) → deny.
        assert!(!s.allows(&"aa".repeat(32), &principals(&[])));
    }

    #[test]
    fn a_hash_in_one_scope_is_not_reachable_via_a_different_scopes_grant() {
        // Cross-scope isolation: scope "a" contains H and grants alice; scope "b" grants bob but
        // does NOT contain H. bob must NOT reach H (P10 hash probing across scopes).
        let mut s = BlobScopes::default();
        s.publish_hash("a", "cc".repeat(32).as_str());
        s.grant("a", "alice");
        s.grant("b", "bob");
        assert!(!s.allows(&"cc".repeat(32), &principals(&["bob"])));
        assert!(s.allows(&"cc".repeat(32), &principals(&["alice"])));
    }

    #[test]
    fn store_persists_and_reloads_the_same_scopes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob-scopes.redb");
        let store = ScopeStore::open(path.clone()).unwrap();
        store.publish_hash("docs", &"dd".repeat(32)).unwrap();
        store.grant("docs", "alice").unwrap();

        // A fresh store over the same file sees the persisted scopes. The first must be DROPPED:
        // redb holds an exclusive file lock, like the peer store (#61's DataDirInUse lesson).
        drop(store);
        let reloaded = ScopeStore::open(path).unwrap();
        let snap = reloaded.snapshot();
        assert!(snap.allows(&"dd".repeat(32), &principals(&["alice"])));
        // list() renders (name, hashes, grants) deterministically sorted.
        let listed = reloaded.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].0, "docs");
        assert_eq!(listed[0].1, vec!["dd".repeat(32)]);
        assert_eq!(listed[0].2, vec!["alice".to_string()]);
    }

    #[test]
    fn loading_a_missing_sidecar_is_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = ScopeStore::open(dir.path().join("does-not-exist.redb")).unwrap();
        assert!(store.snapshot().is_empty());
    }
}

#[cfg(test)]
mod withdrawal_tests {
    use super::*;

    /// #107: an unpublish must be DURABLE against a later republish on this node. Bytes are never
    /// reclaimed (#80), so `has()` stays true forever and republish would otherwise re-add the hash
    /// to a scope whose grants unpublish never touched — restoring access with no grant call.
    #[test]
    fn unpublish_records_a_withdrawal_that_blocks_republish() {
        let mut s = BlobScopes::default();
        s.publish_hash("room", "aa");
        s.grant("room", "b64u:alice");
        assert!(s.unpublish_hash("room", "aa"), "the hash was there");
        assert!(
            s.is_withdrawn("room", "aa"),
            "unpublish must record the withdrawal, not merely drop the hash"
        );
    }

    /// Per-(scope, hash), not global: withdrawing H from A must not block H in B, nor a different
    /// hash in A. Fails if the set is kept per-store instead of per-scope.
    #[test]
    fn a_withdrawal_is_scoped_to_one_scope_and_one_hash() {
        let mut s = BlobScopes::default();
        s.publish_hash("a", "h1");
        s.publish_hash("b", "h1");
        s.publish_hash("a", "h2");
        s.unpublish_hash("a", "h1");
        assert!(s.is_withdrawn("a", "h1"));
        assert!(!s.is_withdrawn("b", "h1"), "another scope is unaffected");
        assert!(!s.is_withdrawn("a", "h2"), "another hash is unaffected");
    }

    /// The deliberate act clears it: `blob_publish` names a FILE, so it is an operator saying "I
    /// mean this content, in this scope".
    #[test]
    fn publishing_from_a_path_clears_the_withdrawal() {
        let mut s = BlobScopes::default();
        s.publish_hash("room", "aa");
        s.unpublish_hash("room", "aa");
        assert!(s.is_withdrawn("room", "aa"));
        s.publish_hash("room", "aa");
        assert!(
            !s.is_withdrawn("room", "aa"),
            "a deliberate publish-from-path is the un-withdraw"
        );
    }

    /// Granting a PRINCIPAL says nothing about a hash. If it cleared withdrawals, content would
    /// resurrect as a side effect of an unrelated act — the silent widening this issue exists to
    /// stop.
    #[test]
    fn granting_a_principal_does_not_clear_a_withdrawal() {
        let mut s = BlobScopes::default();
        s.publish_hash("room", "aa");
        s.unpublish_hash("room", "aa");
        s.grant("room", "b64u:bob");
        assert!(
            s.is_withdrawn("room", "aa"),
            "a grant must never un-withdraw — it names a principal, not a hash"
        );
    }

    /// #107's load-bearing property: the withdrawal SURVIVES A RESTART. A tombstone that
    /// evaporated on reload would be worse than none — it reads as durable, then silently reverts,
    /// and the operator has no way to notice.
    #[test]
    fn a_withdrawal_survives_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scopes.redb");
        {
            let store = ScopeStore::open(path.clone()).unwrap();
            store.publish_hash("room", "aa").unwrap();
            store.grant("room", "b64u:alice").unwrap();
            store.unpublish_hash("room", "aa").unwrap();
            assert!(store.is_withdrawn("room", "aa"));
        }
        let reloaded = ScopeStore::open(path).unwrap();
        assert!(
            reloaded.is_withdrawn("room", "aa"),
            "the withdrawal must persist — a daemon restart must not silently un-revoke"
        );
    }

    /// A sidecar written BEFORE 0.17.0 has no `withdrawn` field; it must load, not fail closed on
    /// deserialization and not fail open by inventing withdrawals.
    #[test]
    fn a_pre_0_17_sidecar_loads_with_no_withdrawals() {
        let dir = tempfile::tempdir().unwrap();
        // The sidecar sits beside the store and is migrated on first open (#84c).
        let path = dir.path().join("old.redb");
        std::fs::write(
            dir.path().join("old.json"),
            r#"{"scopes":{"room":{"hashes":["aa"],"grants":["b64u:alice"]}}}"#,
        )
        .unwrap();
        let store = ScopeStore::open(path).expect("an old sidecar still loads");
        assert!(!store.is_withdrawn("room", "aa"), "nothing was withdrawn");
        assert!(
            store
                .snapshot()
                .allows("aa", &["b64u:alice"].into_iter().collect()),
            "and the existing grant still works"
        );
    }
}

#[cfg(test)]
mod listing_tests {
    use super::*;

    fn table(n: usize) -> BlobScopes {
        let mut s = BlobScopes::default();
        for i in 0..n {
            s.publish_hash(&format!("file:{i:04}"), &format!("{i:064x}"));
            s.grant(&format!("file:{i:04}"), "b64u:alice");
        }
        s
    }

    /// #84b back-compat: an unfiltered listing still works, and now reports how many matched so a
    /// caller can tell a complete answer from a clipped one.
    #[test]
    fn an_unfiltered_listing_reports_its_total_and_is_not_truncated() {
        let page = table(5).list_page(&ListQuery::default()).unwrap();
        assert_eq!(page.rows.len(), 5);
        assert_eq!(page.total, 5);
        assert!(!page.truncated, "5 scopes fit under any sane default");
    }

    /// The failure #84 reports is a CLOSED CONNECTION, not a slow one: `blob_list` renders every
    /// scope into one frame against a 16 MiB cap whose violation strikes the connection out. A
    /// default limit turns that into a truncated answer the caller can detect and page through.
    #[test]
    fn the_default_limit_truncates_and_says_so() {
        let page = table(300).list_page(&ListQuery::default()).unwrap();
        assert_eq!(page.rows.len(), DEFAULT_LIST_LIMIT, "default limit applies");
        assert_eq!(page.total, 300, "total counts MATCHES, not returned rows");
        assert!(
            page.truncated,
            "a clipped answer must announce itself — a caller that cannot tell is the silent \
             wrong answer this repo keeps re-learning"
        );
    }

    /// Paging must not overlap or skip. The table is a BTreeMap so name order is stable; without a
    /// stable order paging returns garbage that looks plausible.
    #[test]
    fn offset_and_limit_page_without_overlap_or_gaps() {
        let t = table(25);
        let p1 = t
            .list_page(&ListQuery {
                limit: Some(10),
                ..Default::default()
            })
            .unwrap();
        let p2 = t
            .list_page(&ListQuery {
                limit: Some(10),
                offset: Some(10),
                ..Default::default()
            })
            .unwrap();
        let n1: Vec<&String> = p1.rows.iter().map(|r| &r.0).collect();
        let n2: Vec<&String> = p2.rows.iter().map(|r| &r.0).collect();
        assert_eq!(n1.len(), 10);
        assert_eq!(n2.len(), 10);
        assert!(
            n1.iter().all(|n| !n2.contains(n)),
            "pages must be disjoint: {n1:?} vs {n2:?}"
        );
        let mut union: Vec<String> = n1.iter().chain(n2.iter()).map(|s| (*s).clone()).collect();
        union.sort();
        let expected: Vec<String> = (0..20).map(|i| format!("file:{i:04}")).collect();
        assert_eq!(
            union, expected,
            "and together they are the first 20 in order"
        );
    }

    /// Exact match, not prefix or substring — `file:aa` must not match `file:aabb`. Under
    /// one-scope-per-file the names are derived from hashes and share prefixes constantly.
    #[test]
    fn the_scope_filter_is_exact_not_a_prefix() {
        let mut s = BlobScopes::default();
        s.publish_hash("file:aa", "11");
        s.publish_hash("file:aabb", "22");
        let page = s
            .list_page(&ListQuery {
                scope: Some("file:aa".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].0, "file:aa");
        assert_eq!(page.total, 1, "and the total reflects the filter");
    }

    /// Spec item 5: the `hash` filter NORMALIZES, so a base32 rendering matches a scope storing
    /// canonical hex — the rule #83 established for every hash-taking surface.
    #[test]
    fn the_hash_filter_normalizes_the_callers_rendering() {
        let canonical = format!("{:064x}", 42);
        let mut s = BlobScopes::default();
        s.publish_hash("file:has-it", &canonical);
        s.publish_hash("file:lacks-it", &format!("{:064x}", 43));

        let parsed = crate::blobs::parse_blob_hash(&canonical).unwrap();
        let base32 = data_encoding::BASE32_NOPAD
            .encode(parsed.as_bytes())
            .to_ascii_lowercase();
        assert_ne!(base32, canonical, "the fixture must actually differ");

        let page = s
            .list_page(&ListQuery {
                hash: Some(base32),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.rows.len(), 1, "the base32 rendering must match");
        assert_eq!(page.rows[0].0, "file:has-it");
    }

    /// #84b review: a MALFORMED hash filter must ERROR, not silently return everything.
    ///
    /// `.ok()` collapsed a bad hash to "no filter", so a truncated paste or a trailing newline
    /// answered "which scopes contain this hash" with EVERY scope, and `total` claimed the full
    /// count. A silent wrong answer on an authorization-introspection surface is worse than a
    /// refusal, and every other hash-taking verb propagates the parse error.
    #[test]
    fn a_malformed_hash_filter_errors_instead_of_matching_everything() {
        let s = table(5);
        for bad in ["abc", "", "not-a-hash", &format!("{:064x}\n", 1)] {
            let got = s.list_page(&ListQuery {
                hash: Some(bad.to_string()),
                ..Default::default()
            });
            assert!(
                got.is_err(),
                "{bad:?} must be refused, not treated as no-filter (would have returned all 5)"
            );
        }
    }

    /// A caller-supplied `limit` is CLAMPED to `MAX_LIST_LIMIT`.
    ///
    /// Without the clamp, `{"limit": 1000000}` — the obvious move on seeing `truncated: true` —
    /// reproduces the unbounded listing the default exists to prevent. Uses a table LARGER than
    /// the ceiling so the assertion is about the clamp rather than about the fixture.
    #[test]
    fn an_enormous_limit_is_clamped_to_the_ceiling() {
        let big = table(MAX_LIST_LIMIT + 50);
        let page = big
            .list_page(&ListQuery {
                limit: Some(usize::MAX),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            page.rows.len(),
            MAX_LIST_LIMIT,
            "a caller cannot opt out of the cap"
        );
        assert_eq!(page.total, MAX_LIST_LIMIT + 50, "total is still honest");
        assert!(page.truncated, "and it says it truncated");
    }

    /// `counts_only` answers "how many files / how many withdrawn" in constant response size —
    /// the common question under one-scope-per-file — without shipping every hash.
    #[test]
    fn counts_only_omits_the_vectors_but_keeps_the_counts() {
        let mut s = table(3);
        s.unpublish_hash("file:0000", &format!("{:064x}", 0));
        let page = s
            .list_page(&ListQuery {
                counts_only: true,
                ..Default::default()
            })
            .unwrap();
        let row = page.rows.iter().find(|r| r.0 == "file:0000").unwrap();
        assert!(row.1.is_empty(), "hashes omitted");
        assert!(row.2.is_empty(), "grants omitted");
        assert!(row.3.is_empty(), "withdrawn omitted");
        assert_eq!(row.4, 0, "hash_count: the one hash was withdrawn");
        assert_eq!(row.5, 1, "but the grant count survives");
        assert_eq!(row.6, 1, "and the withdrawn count");
    }
}
