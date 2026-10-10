//! A generic, trust-agnostic registry of live mesh connections, for revocation-severing.
//! `run_mesh_connection` CHECK-REGISTERS each accepted connection and holds the returned RAII
//! [`Registration`] for the connection's lifetime; on close (handler return) the guard
//! DEREGISTERS. The roster install path (cli) calls [`ConnRegistry::sever_matching`] with a
//! predicate computed from the new roster — net owns the MECHANISM (handles + close), the cli owns
//! the POLICY (which to sever).
//!
//! **The TOCTOU close (why a to-be-severed peer can never keep a live session).** A connection's
//! `gate.resolve()` may run BEFORE a roster install swaps the view, but its registration lands
//! AFTER the install's `sever_matching` pass — a naive `register` would let that connection escape
//! severing. We close the race with a CHECKED register serialized on the SAME mutex `sever_matching`
//! takes: [`ConnRegistry::register_checked`] re-evaluates, UNDER the registry lock, the caller's
//! recheck predicate against the live gate (`should_sever_now(eid, roster_user)` — the FULL sever
//! predicate, both halves), and if it fires does NOT insert (the caller self-closes). Combined with
//! the installer's **swap-before-sever** ordering (swap the roster view, THEN sever), every
//! interleaving is safe:
//!  - (i) checked-register wins the mutex, reads the OLD view (swap not yet done) → inserts; the
//!    installer's later `sever_matching` (same mutex, AFTER its swap completed) finds and closes it;
//!  - (ii) swap done, checked-register acquires the mutex, reads the NEW view → should-sever →
//!    self-close, no insert;
//!  - (iii) swap + sever both done, checked-register acquires the mutex, reads the NEW view →
//!    should-sever → self-close, no insert.
//!
//! There is NO interleaving where a to-be-severed endpoint both inserts and survives. (`sever_matching`'s
//! predicate is computed into plain sets BEFORE it takes the mutex, so it holds only the registry
//! lock — no lock cycle with the gate's own lock that the recheck predicate reads.)
//!
//! **Scope — this closes BOTH halves of the sever rule.** The recheck is the full `should_sever`
//! predicate (via the gate's `should_sever_now(eid, roster_user)`), so the three-case argument above
//! extends unchanged to both severing causes. (1) The REVOKED half — a compromised/lost device
//! handled via `org revoke` → `revoked_endpoints` → `is_revoked` — can never keep a live session
//! (revocation is honored even when degraded, fail-closed). (2) The DROPPED-from-roster half — a
//! previously roster-resolved endpoint (`roster_user.is_some()`) now ABSENT from the installed
//! roster but NOT revoked (a benign DEPARTURE: a user removed from the roster) — is ALSO closed by
//! the recheck: `should_sever_now` returns `true` for it under the NEW view, so the
//! register-after-sever race can never leave such a connection live. Both halves are rechecked under
//! the registry lock, serialized against `sever_matching`; a pairing-only endpoint
//! (`roster_user == None`, not revoked) is never severed by a roster install.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::identity::EndpointId;

/// One tracked live connection.
struct Tracked {
    endpoint_id: EndpointId,
    /// `Some(user_id)` when resolved via the ROSTER (the "previously resolved via the roster"
    /// discriminator); `None` for a pairing-resolved connection (never severed by a roster
    /// install unless revoked).
    roster_user: Option<String>,
    /// The principals this connection's LIVE sessions were admitted as, counted per session
    /// (#222 review). A principal is added when a session is admitted and removed when that session
    /// ends, so the map is bounded by live sessions. A revoke matches on it IN ADDITION to the
    /// endpoints the current store/roster maps the principal to: once the principal is re-resolved
    /// per session, the store can say a device is `b64u:NEW` while a session admitted as `b64u:OLD`
    /// is still running — and a revoke of `b64u:OLD` must cut that session.
    admitted: HashMap<String, usize>,
    conn: iroh::endpoint::Connection, // Clone = another handle to the SAME connection (iroh 1.0.1)
}

/// Live-connection registry. Keyed by a registry-issued monotonic id (NOT iroh's `stable_id`, to
/// avoid any reuse-after-close ambiguity); the RAII guard removes exactly its own entry.
#[derive(Default)]
pub struct ConnRegistry {
    inner: Mutex<HashMap<u64, Tracked>>,
    seq: AtomicU64,
}

/// RAII deregistration guard — held by the connection handler for the connection's lifetime.
pub struct Registration {
    registry: Arc<ConnRegistry>,
    id: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        // Recover from poison rather than `expect`: a Drop that panics while the thread is already
        // unwinding (e.g. from a panic inside a closure run under this lock) aborts the process.
        self.registry
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}

impl Registration {
    /// A non-owning handle the connection's per-stream tasks use to PROMOTE this entry's
    /// `roster_user` (#222). Dropping it never deregisters — only the [`Registration`] does.
    /// `roster_resolved` seeds the tracker with whether the entry was registered roster-resolved.
    pub(crate) fn tracker(&self, roster_resolved: bool) -> ConnTracker {
        ConnTracker {
            registry: self.registry.clone(),
            id: self.id,
            promoted: Arc::new(std::sync::atomic::AtomicBool::new(roster_resolved)),
        }
    }
}

/// Keeps a live connection's sever discriminator in step with PER-STREAM resolution (#222).
///
/// The discriminator (`roster_user`) is captured at register time. Once each stream re-resolves its
/// principal, a connection registered PAIRING-only (`None`) can go on to carry a ROSTER-authorized
/// session (the device joined the roster mid-connection) — and `should_sever` never cuts a `None`
/// connection on a roster drop, so that session would outlive the roster that authorized it.
/// [`admit_roster_user`](Self::admit_roster_user) promotes the entry to `Some` before such a stream
/// is served.
///
/// Promotion is ONE-WAY (`None → Some`). Demoting would let a connection that already carries a
/// roster-authorized session escape a later roster sever; keeping `Some` costs at most a sever of a
/// connection whose later streams ran as the pairing identity, which the client redials.
#[derive(Clone)]
pub(crate) struct ConnTracker {
    registry: Arc<ConnRegistry>,
    id: u64,
    /// Lock-free fast path: `true` once the entry holds `Some`, so a roster-resolved stream on an
    /// already-promoted connection never takes the registry mutex.
    promoted: Arc<std::sync::atomic::AtomicBool>,
}

impl ConnTracker {
    /// Record that a stream on this connection resolved via the roster as `roster_user`, and say
    /// whether that stream may be served.
    ///
    /// `None` (a pairing-resolved stream) and an already-promoted entry return `true` without
    /// locking. Otherwise, UNDER the registry lock `sever_matching` takes, `should_sever_now` is
    /// re-evaluated for the promoted discriminator — the same TOCTOU close
    /// [`ConnRegistry::register_checked`] makes. A roster install that swapped the view after this
    /// stream resolved either runs its sever after the promotion (and finds the entry) or has
    /// already swapped (and the recheck refuses). `false` means refuse the stream: the recheck
    /// fired, or the entry is gone because the connection is closing.
    pub(crate) fn admit_roster_user(
        &self,
        roster_user: Option<&str>,
        should_sever_now: impl FnOnce(&EndpointId) -> bool,
    ) -> bool {
        let Some(user) = roster_user else {
            return true;
        };
        if self.promoted.load(Ordering::Acquire) {
            return true;
        }
        let mut map = self.registry.inner.lock().expect("conn registry mutex");
        let Some(tracked) = map.get_mut(&self.id) else {
            return false;
        };
        if should_sever_now(&tracked.endpoint_id) {
            return false;
        }
        tracked.roster_user = Some(user.to_string());
        self.promoted.store(true, Ordering::Release);
        true
    }

    /// Record that a session on this connection is being admitted as `principals`, returning the
    /// guard that un-records them when the session ends — or `None` to refuse the session.
    ///
    /// **Not lock-free (a deliberate deviation from #222's "lock-free accept path").** Every admitted
    /// session takes the registry mutex twice — here, and in [`AdmittedSession`]'s drop — each a
    /// short synchronous section, never held across an `.await`. Lock order is registry mutex →
    /// `LiveServices` read lock (the closure calls `LiveServices::get` under the mutex); nothing
    /// takes them in the other order (`LiveServices::store` holds its write lock alone, and a sever
    /// computes its predicate sets before locking). The mutex is what makes the admit atomic with
    /// the sever; a lock-free record could not close the race below.
    ///
    /// UNDER the registry lock every sever takes, `still_admitted` is evaluated (the caller re-reads
    /// the live service registry) and the principals are recorded — atomically. A revoke is
    /// swap-before-sever (#99), so either its sever runs after this record and finds the principal,
    /// or its swap already happened and `still_admitted` sees the new allow list and refuses. `None`
    /// also when the entry is gone (the connection is closing).
    pub(crate) fn admit_session(
        &self,
        principals: Vec<String>,
        still_admitted: impl FnOnce() -> bool,
    ) -> Option<AdmittedSession> {
        let mut map = self.registry.inner.lock().expect("conn registry mutex");
        let tracked = map.get_mut(&self.id)?;
        if !still_admitted() {
            return None;
        }
        for p in &principals {
            *tracked.admitted.entry(p.clone()).or_insert(0) += 1;
        }
        Some(AdmittedSession {
            registry: self.registry.clone(),
            id: self.id,
            principals,
        })
    }
}

/// RAII guard for one admitted session's principals (#222 review): dropping it — the session
/// ended — removes exactly the counts [`ConnTracker::admit_session`] added. Holds no lock.
pub(crate) struct AdmittedSession {
    registry: Arc<ConnRegistry>,
    id: u64,
    principals: Vec<String>,
}

impl Drop for AdmittedSession {
    fn drop(&mut self) {
        // Poison-tolerant for the same reason as `Registration::drop`: `admit_session` runs the
        // caller's closure under this lock, so a panic there must not turn a later drop into an
        // abort.
        let mut map = self
            .registry
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(tracked) = map.get_mut(&self.id) else {
            return; // the connection already deregistered
        };
        for p in &self.principals {
            if let Some(n) = tracked.admitted.get_mut(p) {
                *n -= 1;
                if *n == 0 {
                    tracked.admitted.remove(p);
                }
            }
        }
    }
}

/// The sever decision, pure + unit-testable: sever iff the endpoint is `revoked` by the new
/// roster, OR it was roster-resolved (`roster_user.is_some()`) AND is absent from the new roster's
/// `active_devices` set. A pairing-only endpoint (`roster_user == None`, not revoked) is NEVER
/// severed by a roster install. The cli wraps this with the concrete sets from the just-installed
/// view (net owns the generic rule; the roster MEANING of the inputs is cli's).
pub fn should_sever(
    endpoint_id: &EndpointId,
    roster_user: Option<&str>,
    revoked: &std::collections::HashSet<EndpointId>,
    active_devices: &std::collections::HashSet<EndpointId>,
) -> bool {
    revoked.contains(endpoint_id)
        || (roster_user.is_some() && !active_devices.contains(endpoint_id))
}

impl ConnRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// CHECK-register a live connection (the TOCTOU close — see the module doc). UNDER the
    /// registry lock: if `should_sever_now(endpoint_id)` (the FULL sever predicate against
    /// the live gate, possibly just swapped by a concurrent install — revoked OR roster-resolved-
    /// but-dropped) → return `None` WITHOUT inserting (the caller self-closes); else insert
    /// and return the RAII guard. Serializing the recheck+insert on the SAME lock `sever_matching`
    /// takes is what makes the race provably closed. The guard deregisters on drop (connection close).
    pub fn register_checked(
        self: &Arc<Self>,
        conn: &iroh::endpoint::Connection,
        roster_user: Option<String>,
        should_sever_now: impl FnOnce(&EndpointId) -> bool,
    ) -> Option<Registration> {
        let endpoint_id: EndpointId = conn.remote_id().into();
        let mut map = self.inner.lock().expect("conn registry mutex");
        if should_sever_now(&endpoint_id) {
            return None; // to-be-severed as of the live gate → refuse; caller self-closes (no insert).
        }
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        map.insert(
            id,
            Tracked {
                endpoint_id,
                roster_user,
                admitted: HashMap::new(),
                conn: conn.clone(),
            },
        );
        Some(Registration {
            registry: self.clone(),
            id,
        })
    }

    /// Close every tracked connection for which `should_sever(endpoint_id, roster_user)` is true.
    /// Returns the count severed. The QUIC close (`code`, `reason`) tears the connection down
    /// across every stream/session; each handler task's guard then deregisters it as it unwinds.
    /// Holds ONLY the registry lock (the predicate is precomputed by the cli into plain sets);
    /// `close` is non-blocking.
    pub fn sever_matching(
        &self,
        code: u32,
        reason: &[u8],
        should_sever: impl Fn(&EndpointId, Option<&str>) -> bool,
    ) -> usize {
        let map = self.inner.lock().expect("conn registry mutex");
        let mut n = 0;
        for t in map.values() {
            if should_sever(&t.endpoint_id, t.roster_user.as_deref()) {
                t.conn.close(code.into(), reason);
                n += 1;
            }
        }
        n
    }

    /// Close every tracked connection whose endpoint matches `endpoints`, OR one of whose LIVE
    /// sessions was admitted as a principal matching `principals` (#222 review). Returns the count
    /// severed (each connection once).
    ///
    /// The revoke paths use this rather than [`sever_matching`](Self::sever_matching): the
    /// endpoints a principal maps to come from the CURRENT store/roster, but sessions are
    /// authorized per session, so a session admitted as a principal the device no longer carries
    /// is reachable only through what it was admitted as. Same lock discipline: only the registry
    /// lock, predicates precomputed by the caller, `close` non-blocking.
    pub fn sever_matching_admitted(
        &self,
        code: u32,
        reason: &[u8],
        endpoints: impl Fn(&EndpointId) -> bool,
        principals: impl Fn(&str) -> bool,
    ) -> usize {
        let map = self.inner.lock().expect("conn registry mutex");
        let mut n = 0;
        for t in map.values() {
            if endpoints(&t.endpoint_id) || t.admitted.keys().any(|p| principals(p)) {
                t.conn.close(code.into(), reason);
                n += 1;
            }
        }
        n
    }

    /// Number of tracked live connections (for integration-test accounting assertions).
    pub fn len(&self) -> usize {
        self.inner.lock().expect("conn registry mutex").len()
    }

    /// Whether no live connection is tracked (clippy's `len_without_is_empty` companion).
    pub fn is_empty(&self) -> bool {
        self.inner.lock().expect("conn registry mutex").is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The sever rule, exhaustively: revoked ∪ {roster-resolved AND absent from the new device
    /// set}; never a pairing-only peer.
    #[test]
    fn should_sever_selects_revoked_and_dropped_roster_but_not_pairing() {
        let eid = |b: u8| EndpointId::from_bytes([b; 32]);
        let revoked: HashSet<EndpointId> = [eid(3)].into_iter().collect(); // endpoint revoked
        let devices: HashSet<EndpointId> = [eid(2)].into_iter().collect(); // still-active device
        // (a) roster-resolved + still a device → KEEP.
        assert!(!should_sever(&eid(2), Some("alice"), &revoked, &devices));
        // (b) revoked (regardless of source — even a pairing peer that got revoked) → SEVER.
        assert!(should_sever(&eid(3), Some("alice"), &revoked, &devices));
        assert!(
            should_sever(&eid(3), None, &revoked, &devices),
            "revocation wins over pairing"
        );
        // (c) roster-resolved but DROPPED from the new roster (absent from devices, not revoked) → SEVER.
        assert!(should_sever(&eid(9), Some("bob"), &revoked, &devices));
        // (d) pairing-only (roster_user None), not revoked → KEEP (never severed by a roster install).
        assert!(!should_sever(&eid(9), None, &revoked, &devices));
    }

    /// The tracked `roster_user` as `sever_matching` sees it, observed WITHOUT severing.
    fn tracked_roster_users(registry: &ConnRegistry) -> Vec<Option<String>> {
        let seen = Mutex::new(Vec::new());
        registry.sever_matching(0, b"", |_, ru| {
            seen.lock().unwrap().push(ru.map(str::to_string));
            false
        });
        seen.into_inner().unwrap()
    }

    /// #222: `ConnTracker::admit_roster_user` — the promotion that keeps a pairing-registered
    /// connection severable once one of its streams is roster-authorized.
    #[tokio::test]
    async fn tracker_promotes_under_the_recheck_and_never_after_close() {
        let (_keep, conn) = live_conn().await;

        let registry = Arc::new(ConnRegistry::new());
        let registration = registry
            .register_checked(&conn, None, |_| false)
            .expect("registers");
        let tracker = registration.tracker(false);

        // A pairing-resolved stream records nothing and never consults the recheck.
        assert!(tracker.admit_roster_user(None, |_| panic!("no recheck for a pairing stream")));
        assert_eq!(tracked_roster_users(&registry), vec![None]);

        // The recheck fires (the roster already dropped the device): refuse, do NOT promote.
        let mut asked = false;
        assert!(!tracker.admit_roster_user(Some("carol"), |_| {
            asked = true;
            assert!(
                registry.inner.try_lock().is_err(),
                "the promotion recheck must run UNDER the registry lock sever_matching takes"
            );
            true
        }));
        assert!(
            asked,
            "the promotion must re-evaluate the live sever predicate"
        );
        assert_eq!(tracked_roster_users(&registry), vec![None]);

        // The recheck passes: admit and promote, so a later roster drop severs this connection.
        assert!(tracker.admit_roster_user(Some("carol"), |_| {
            assert!(
                registry.inner.try_lock().is_err(),
                "recheck and write must be ONE lock acquisition"
            );
            false
        }));
        assert_eq!(
            tracked_roster_users(&registry),
            vec![Some("carol".to_string())]
        );

        // Already promoted: lock-free, and the discriminator is never demoted.
        assert!(
            tracker
                .clone()
                .admit_roster_user(Some("dave"), |_| panic!("fast path"))
        );
        assert!(tracker.admit_roster_user(None, |_| panic!("fast path")));
        assert_eq!(
            tracked_roster_users(&registry),
            vec![Some("carol".to_string())]
        );

        // The connection deregistered (closing): a fresh tracker refuses rather than promoting a
        // ghost entry.
        let unpromoted = registration.tracker(false);
        drop(registration);
        assert!(registry.is_empty());
        assert!(!unpromoted.admit_roster_user(Some("carol"), |_| false));
    }

    type Keep = (iroh::Endpoint, iroh::Endpoint, iroh::endpoint::Connection);

    /// A real localhost connection (server side returned; the rest kept alive by the caller).
    async fn live_conn() -> (Keep, iroh::endpoint::Connection) {
        let bind = || async {
            iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(iroh::RelayMode::Disabled)
                .alpns(vec![b"t".to_vec()])
                .bind()
                .await
                .expect("bind")
        };
        let (server, client) = (bind().await, bind().await);
        let addr = server.addr();
        let accept = tokio::spawn(async move {
            let conn = server.accept().await.unwrap().await.unwrap();
            (server, conn)
        });
        let client_conn = client.connect(addr, b"t").await.expect("dial");
        let (server, conn) = accept.await.unwrap();
        ((server, client, client_conn), conn)
    }

    /// #222 review: `admit_session` records what a session was admitted AS, under the lock, counted
    /// per session and removed when the session ends; `sever_matching_admitted` severs on it.
    #[tokio::test]
    async fn admitted_principals_are_recorded_per_live_session_and_sever_on_them() {
        let (_keep, conn) = live_conn().await;
        let registry = Arc::new(ConnRegistry::new());
        let registration = registry
            .register_checked(&conn, None, |_| false)
            .expect("registers");
        let tracker = registration.tracker(false);
        let never = |_: &EndpointId| false;
        // Count (without closing) the connections matching `principal` via the admitted set.
        let matches = |principal: &str| {
            let map = registry.inner.lock().unwrap();
            map.values()
                .filter(|t| t.admitted.contains_key(principal))
                .count()
        };

        // A refused admit records nothing — and the recheck runs under the lock.
        assert!(
            tracker
                .admit_session(vec!["b64u:OLD".into()], || {
                    assert!(
                        registry.inner.try_lock().is_err(),
                        "the admit recheck must run UNDER the registry lock"
                    );
                    false
                })
                .is_none()
        );
        assert_eq!(matches("b64u:OLD"), 0);

        // Two sessions admitted as b64u:OLD, one of them also as a group.
        let a = tracker
            .admit_session(vec!["b64u:OLD".into(), "eng".into()], || true)
            .expect("admitted");
        let b = tracker
            .admit_session(vec!["b64u:OLD".into()], || true)
            .expect("admitted");
        assert_eq!((matches("b64u:OLD"), matches("eng")), (1, 1));

        // One ends: the other still holds b64u:OLD; its group goes with it.
        drop(a);
        assert_eq!((matches("b64u:OLD"), matches("eng")), (1, 0));
        // Both ended: nothing remains (bounded by live sessions).
        drop(b);
        assert_eq!(matches("b64u:OLD"), 0);
        assert_eq!(
            registry.sever_matching_admitted(0, b"", never, |p| p == "b64u:OLD"),
            0,
            "a principal no live session was admitted as must sever nothing"
        );

        // Live again: the sever finds the connection by the ADMITTED principal alone.
        let _c = tracker
            .admit_session(vec!["b64u:OLD".into()], || true)
            .expect("admitted");
        assert_eq!(
            registry.sever_matching_admitted(0, b"", never, |p| p == "b64u:NEW"),
            0
        );
        assert_eq!(
            registry.sever_matching_admitted(0, b"", never, |p| p == "b64u:OLD"),
            1
        );

        // The connection deregistered: an admit refuses rather than recording on a ghost.
        drop(registration);
        assert!(tracker.admit_session(vec!["x".into()], || true).is_none());
    }

    /// #222 review: a panic inside a closure run UNDER the registry lock (the admit recheck)
    /// poisons the mutex; the RAII drops must still clean up rather than panic — a panicking Drop
    /// during unwinding aborts the process.
    #[tokio::test]
    async fn guards_drop_cleanly_after_the_registry_mutex_is_poisoned() {
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let (_keep, conn) = live_conn().await;
        let registry = Arc::new(ConnRegistry::new());
        let registration = registry
            .register_checked(&conn, None, |_| false)
            .expect("registers");
        let tracker = registration.tracker(false);
        let session = tracker
            .admit_session(vec!["b64u:OLD".into()], || true)
            .expect("admitted");

        let poisoned = catch_unwind(AssertUnwindSafe(|| {
            tracker.admit_session(vec!["x".into()], || {
                panic!("recheck panicked under the lock")
            })
        }));
        assert!(poisoned.is_err(), "setup: the closure panicked");
        assert!(registry.inner.is_poisoned(), "setup: the mutex is poisoned");

        assert!(
            catch_unwind(AssertUnwindSafe(|| drop(session))).is_ok(),
            "AdmittedSession::drop must tolerate a poisoned registry mutex"
        );
        assert!(
            catch_unwind(AssertUnwindSafe(|| drop(registration))).is_ok(),
            "Registration::drop must tolerate a poisoned registry mutex"
        );
        let map = registry
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(map.is_empty(), "the poisoned drops still deregistered");
    }
}
