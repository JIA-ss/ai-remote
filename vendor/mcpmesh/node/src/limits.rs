//! Rate-limiting primitives: a monotonic token bucket + a bounded,
//! idle-evicting per-endpoint bucket map. PURE and FAIL-SAFE by construction — an over-limit check
//! DENIES (returns a retry hint), never serves-more; the bucket map self-prunes so a churn of distinct
//! AUTHENTICATED endpoints cannot grow memory without bound. Keyed ONLY on the authenticated
//! `EndpointId` (never a self-asserted name — the core attribution invariant).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mcpmesh_net::EndpointId;

/// A monotonic token bucket: at most `capacity` tokens (the burst), refilling at `refill_per_sec`.
/// One request costs one token. `try_take` refills lazily from elapsed wall time via a monotonic
/// `Instant`, so it needs no background timer and cannot go backwards.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// A FULL bucket. `capacity` = the burst allowance; `refill_per_sec` = the sustained rate.
    pub fn new(capacity: f64, refill_per_sec: f64, now: Instant) -> Self {
        Self {
            capacity,
            refill_per_sec,
            tokens: capacity,
            last_refill: now,
        }
    }

    /// Refill lazily, then take one token if available. `Ok(())` = a token was spent; `Err(ms)` =
    /// empty, where `ms` is the ceil-milliseconds until the NEXT token (FAIL-SAFE deny — never a
    /// spend on empty, never negative). A zero refill rate reports a long, bounded wait.
    pub fn try_take(&mut self, now: Instant) -> Result<(), u64> {
        self.try_take_cost(now, 1.0)
    }

    /// [`try_take`](Self::try_take) for a variable cost (#84a).
    ///
    /// A byte budget meters CHUNKS, not calls: iroh-blobs' `Throttle` event carries the chunk
    /// `size` (usually 16 KiB), so a fixed cost of 1 would count events and let one peer move
    /// unbounded bytes at a bounded event rate — the exact shape #84a reports.
    ///
    /// A cost above `capacity` can never be satisfied by waiting, so it is refused with the
    /// full-refill wait rather than a deficit that under-reports. Costs are `f64` to match the
    /// bucket's existing arithmetic; a chunk size is far inside the exact-integer range.
    pub fn try_take_cost(&mut self, now: Instant, cost: f64) -> Result<(), u64> {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        if self.tokens >= cost {
            self.tokens -= cost;
            Ok(())
        } else {
            let deficit = cost - self.tokens;
            let secs = if self.refill_per_sec > 0.0 {
                deficit / self.refill_per_sec
            } else {
                f64::from(u32::MAX)
            };
            Err((secs * 1000.0).ceil() as u64)
        }
    }
}

/// A bucket unused for this long is evictable — the map self-prunes so a churn of distinct
/// authenticated endpoints cannot grow memory without bound (the AC's core property).
const IDLE_TTL: Duration = Duration::from_secs(600);
/// Hard cap on tracked buckets (defense-in-depth). Only gate-resolved endpoints ever reach the
/// limiter — strangers are refused pre-gate — so the live set is already roster/allowlist-bounded;
/// at the cap, a newcomer LRU-evicts the least-recently-seen bucket. The map NEVER exceeds this.
const MAX_BUCKETS: usize = 4096;

struct Tracked {
    bucket: TokenBucket,
    last_seen: Instant,
}

/// A bounded, idle-evicting map of per-identity token buckets (no unbounded memory).
/// Keyed ONLY on the authenticated `EndpointId`.
pub struct RateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    buckets: Mutex<HashMap<EndpointId, Tracked>>,
}

impl RateLimiter {
    /// [`per_minute`](Self::per_minute) with `f64` capacity, for a budget that exceeds `u32`
    /// (`[limits].blob_bytes_per_min` is a byte count, #84a).
    pub fn per_minute_f64(per_min: f64, burst: f64) -> Self {
        Self {
            capacity: burst.max(1.0),
            refill_per_sec: per_min.max(1.0) / 60.0,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Build from a per-minute rate (config `[limits].rate_limit_per_min`). `burst` = bucket
    /// capacity (the instantaneous allowance); sustained rate = `per_min / 60` tokens·s⁻¹.
    pub fn per_minute(per_min: u32, burst: u32) -> Self {
        Self {
            capacity: f64::from(burst.max(1)),
            refill_per_sec: f64::from(per_min.max(1)) / 60.0,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// An effectively-unlimited limiter (control-only test daemon / the `None`-identity path).
    pub fn unlimited_shared() -> Arc<Self> {
        Arc::new(Self::per_minute(u32::MAX, u32::MAX))
    }

    /// Check-and-consume one token for `endpoint` at `now`. `Ok(())` = admit; `Err(ms)` = over limit
    /// (FAIL-SAFE deny). Lazily creates the endpoint's bucket, records `last_seen`, and prunes idle
    /// buckets so the map stays bounded (idle-TTL retain + a hard LRU cap).
    pub fn check(&self, endpoint: &EndpointId, now: Instant) -> Result<(), u64> {
        let mut map = self.buckets.lock().expect("rate limiter mutex");
        if !map.contains_key(endpoint) {
            make_room(&mut map, now);
            map.insert(
                *endpoint,
                Tracked {
                    bucket: TokenBucket::new(self.capacity, self.refill_per_sec, now),
                    last_seen: now,
                },
            );
        }
        let t = map
            .get_mut(endpoint)
            .expect("present after the insert above");
        t.last_seen = now;
        t.bucket.try_take(now)
    }

    /// [`check`](Self::check) for a variable cost — the byte-budget path (#84a).
    ///
    /// Same per-endpoint map, same bounded-map `make_room` discipline; only the cost differs. A
    /// second metering primitive would have meant a second unbounded map to get wrong.
    pub fn check_cost(&self, endpoint: &EndpointId, now: Instant, cost: f64) -> Result<(), u64> {
        let mut map = self.buckets.lock().expect("rate limiter mutex");
        if !map.contains_key(endpoint) {
            make_room(&mut map, now);
            map.insert(
                *endpoint,
                Tracked {
                    bucket: TokenBucket::new(self.capacity, self.refill_per_sec, now),
                    last_seen: now,
                },
            );
        }
        let t = map
            .get_mut(endpoint)
            .expect("present after the insert above");
        t.last_seen = now;
        t.bucket.try_take_cost(now, cost)
    }

    /// Number of tracked buckets (the AC's bounded-memory assertion reads this).
    pub fn tracked(&self) -> usize {
        self.buckets.lock().expect("rate limiter mutex").len()
    }
}

/// Prune idle buckets (`last_seen` older than IDLE_TTL); if the map is STILL at the hard cap, evict
/// the single least-recently-seen entry so a newcomer fits. O(n) under the lock; `n ≤ MAX_BUCKETS`.
fn make_room(map: &mut HashMap<EndpointId, Tracked>, now: Instant) {
    map.retain(|_, t| now.saturating_duration_since(t.last_seen) < IDLE_TTL);
    if map.len() >= MAX_BUCKETS
        && let Some(oldest) = map.iter().min_by_key(|(_, t)| t.last_seen).map(|(k, _)| *k)
    {
        map.remove(&oldest);
    }
}

/// Per-session rate-limit handle for the pump: the shared per-endpoint limiter + THIS session's
/// authenticated endpoint. A `None` endpoint (the reserved no-identity path) is never limited.
/// Consulted once per inbound proxied request line.
pub struct RateGate {
    limiter: Arc<RateLimiter>,
    endpoint: Option<EndpointId>,
}

impl RateGate {
    pub fn new(limiter: Arc<RateLimiter>, endpoint: Option<EndpointId>) -> Self {
        Self { limiter, endpoint }
    }

    /// Try to admit one request now. `Ok(())` = forward it; `Err(retry_after_ms)` = throttle (DENY).
    pub fn admit(&self) -> Result<(), u64> {
        self.admit_at(Instant::now())
    }

    /// `admit` at an explicit instant (deterministic tests).
    pub fn admit_at(&self, now: Instant) -> Result<(), u64> {
        match self.endpoint {
            Some(eid) => self.limiter.check(&eid, now),
            None => Ok(()),
        }
    }
}

/// Global pairing-accept rate. The pairing listener accepts strangers by design, who pick
/// fresh ids — so a SINGLE global bucket bounds a
/// distinct-id flood (a per-endpoint map would be defeated by fresh ids). NOT the removed per-invite
/// attempt cap; the 32-byte secret is the security.
const PAIR_ACCEPT_PER_MIN: u32 = 30;
/// Per-authenticated-endpoint app-blob CONNECTION rate: a valid
/// roster member with no scope grant can open blob connections whose GETs are denied — this bounds
/// that churn per endpoint.
const BLOB_CONN_PER_MIN: u32 = 60;

/// Per-authenticated-endpoint reachability-probe (`mcpmesh/ping/1`) rate (#89).
///
/// The probe arm was trust-gated but UNMETERED: a paired peer could pong-flood at will, and the
/// only bound was the peer's own politeness. Generous — a healthy peer probes on a 20s TTL, so a
/// handful per minute is normal and this only bites a peer probing orders of magnitude harder.
/// Per-endpoint rather than global: one noisy peer must not deny liveness for everyone else, which
/// is the mistake the pair-accept bucket makes deliberately (there, ids are attacker-chosen).
const PING_PER_MIN: u32 = 60;

/// The smallest USEFUL app-blob byte budget: two chunks (#84a). One chunk is reserved at request
/// admission before any bytes, so a budget below this admits a request and then starves the
/// transfer. A configured value in `1..MIN` is floored to this rather than honoured.
pub const MIN_BLOB_BYTES_PER_MIN: u64 = 2 * 16 * 1024;

/// The daemon's rate/concurrency limiter bundle, built ONCE from config and carried
/// on `MeshState`. Bundled so `MeshState` gains ONE handle. Every map is bounded.
pub struct MeshLimiters {
    /// Per-authenticated-endpoint proxied-request buckets (`[limits].rate_limit_per_min`).
    ///
    /// Retained as the DEFAULT and the CEILING. Since #63 the bucket a session actually consults is
    /// per (service, endpoint) — see [`for_service`](Self::for_service) — and this one is what a
    /// service without an override gets.
    pub requests: Arc<RateLimiter>,
    /// The global `[limits].rate_limit_per_min`, or `None` for an unlimited bundle (#63).
    ///
    /// The **ceiling**: a per-service override may only LOWER the rate. Before #63 this value was a
    /// bound on a peer's aggregate rate across every mount; now it bounds a peer's rate PER SERVICE,
    /// and no config entry or control call can raise it. `None` propagates, so
    /// [`unlimited`](Self::unlimited) stays unlimited whatever a service configures.
    global_rpm: Option<u32>,
    /// Per-service `(service, endpoint)` request buckets (#63).
    ///
    /// Lives HERE, not in `build_services`, because `MeshLimiters` is built once and survives
    /// hot-reloads while `build_services` runs on every one (grant, revoke, register, roster
    /// install). Creating these there would reset every peer's bucket on each reload, so a local
    /// caller could spam grants to clear its own rate limit.
    services: Mutex<HashMap<String, ServiceBucket>>,
    /// A GLOBAL pair-ALPN accept bucket (bounds a distinct-id stranger flood).
    pair_accept: Mutex<TokenBucket>,
    /// Per-authenticated-endpoint app-blob BYTE budget (`[limits].blob_bytes_per_min`, #84a).
    /// `None` when the budget is 0 = unlimited, so the default deployment allocates nothing and
    /// consults nothing — the feature is opt-in and changes no existing behaviour on upgrade.
    blob_bytes: Option<Arc<RateLimiter>>,
    /// Per-authenticated-endpoint app-blob connection buckets.
    blob_conn: Arc<RateLimiter>,
    /// Per-authenticated-endpoint reachability-probe buckets (#89).
    ping: Arc<RateLimiter>,
    /// Probes REFUSED by the ping bucket (#89 gate): a probe is not a session, so a refusal
    /// leaves no audit row — this count and the accept arm's debug line are its only footprint.
    /// RESPONDER-side by nature (the refuser is the only party that knows), and not yet surfaced
    /// by any verb; wiring it into `status`/diagnostics is #89 follow-up work.
    ping_refused: std::sync::atomic::AtomicU64,
}

/// One service's request limiter plus the effective rate it was built for (#63). The rate is kept
/// so a reload can tell "unchanged" from "changed" — recreating on every reload would reset buckets.
struct ServiceBucket {
    effective_rpm: Option<u32>,
    limiter: Arc<RateLimiter>,
}

/// Cap on distinct service names tracked (#63). Beyond it an entry is evicted,
/// exactly as the endpoint map does — NOT a fall back to the global limiter, which would RAISE the
/// rate of a service configured lower and undo the only-lower rule.
///
/// Eviction resets that service's buckets. Reaching it needs more than this many distinct names
/// **ever seen** — entries are not removed when a service is unregistered or drops out of config —
/// which is a LOCAL, owner-only control-socket operation (the socket is 0600); a caller who can do
/// it already owns the node. Note a REMOTE peer can trigger the reload that re-creates entries (a
/// first-time pairing grant reaches `reload_services_from_disk`), so on a node whose map is already
/// full that chains into remote-triggered bucket resets.
///
/// **Aggregate memory.** Each entry carries its own endpoint map at `MAX_BUCKETS`, so the bundle's
/// worst case is this many × `MAX_BUCKETS` buckets — 256× the pre-#63 ceiling. Per-map bounds and
/// `IDLE_TTL` pruning are unchanged; what moved is the total, and a quiet service retains its map
/// until something calls `check` on it again.
const MAX_TRACKED_SERVICES: usize = 256;

impl MeshLimiters {
    /// The request limiter for one service (#63): its own `(service, endpoint)` buckets.
    ///
    /// `configured` is `[services.<name>].rate_limit_per_min`. It may only LOWER the rate —
    /// `[limits].rate_limit_per_min` is a hard ceiling no config entry and no control call can
    /// exceed, which is what keeps an unclamped `register_service` from uncapping a service.
    ///
    /// Returns the EXISTING limiter when the effective rate is unchanged, so a reload preserves
    /// every bucket; a changed rate replaces it (and necessarily resets it, which is the point).
    pub fn for_service(&self, name: &str, configured: Option<u32>) -> Arc<RateLimiter> {
        // `effective_rpm` is the single place the ceiling is applied — the config path and the
        // control path must not be able to enforce it differently. `None` = unlimited bundle, which
        // stays unlimited whatever a service asks for.
        let effective = self.effective_rpm(configured);
        let mut map = self
            .services
            .lock()
            .expect("service limiter map not poisoned");
        if let Some(existing) = map.get(name)
            && existing.effective_rpm == effective
        {
            return existing.limiter.clone();
        }
        if map.len() >= MAX_TRACKED_SERVICES && !map.contains_key(name) {
            // No LRU stamp on this map (it is touched once per session build, not per request), so
            // evict an arbitrary entry. Bounded-ness is the property; WHICH one goes is not
            // security-bearing, and reaching the cap is owner-only (see the const).
            if let Some(victim) = map.keys().next().cloned() {
                map.remove(&victim);
            }
        }
        let limiter = match effective {
            None => RateLimiter::unlimited_shared(),
            Some(rpm) => Arc::new(RateLimiter::per_minute(rpm, rpm)),
        };
        map.insert(
            name.to_string(),
            ServiceBucket {
                effective_rpm: effective,
                limiter: limiter.clone(),
            },
        );
        limiter
    }

    /// The effective rate CURRENTLY TRACKED for `name`, or `None` if this service has no bucket
    /// yet (#63). `Some(None)` = tracked and unlimited.
    ///
    /// Exists so a test can pin that `build_services` actually routes each backend through
    /// [`for_service`](Self::for_service). Asserting on `for_service` directly proves only that the
    /// helper works — reverting the call site to the one shared limiter passed that test.
    pub fn tracked_rpm(&self, name: &str) -> Option<Option<u32>> {
        self.services
            .lock()
            .expect("service limiter map not poisoned")
            .get(name)
            .map(|b| b.effective_rpm)
    }

    /// The effective per-minute rate a service would get (#63) — the clamp, without building a
    /// limiter. THE single place the ceiling is applied, so the config and control paths cannot
    /// enforce it differently.
    ///
    /// Not reported anywhere yet. #63's second ask — "a way to observe remaining budget so an
    /// embedder can pace rather than retry" — is NOT implemented; a consumer still learns the limit
    /// only by hitting `-32053`.
    pub fn effective_rpm(&self, configured: Option<u32>) -> Option<u32> {
        self.global_rpm
            .map(|global| configured.map_or(global, |c| c.min(global)))
    }

    /// Build from `[limits]`. Burst == the per-minute rate (a full minute of instantaneous allowance,
    /// then the sustained rate caps at `per_min`).
    pub fn from_config(limits: &crate::config::LimitsCfg) -> Arc<Self> {
        let now = Instant::now();
        Arc::new(Self {
            requests: Arc::new(RateLimiter::per_minute(
                limits.rate_limit_per_min,
                limits.rate_limit_per_min,
            )),
            global_rpm: Some(limits.rate_limit_per_min),
            services: Mutex::new(HashMap::new()),
            pair_accept: Mutex::new(TokenBucket::new(
                f64::from(PAIR_ACCEPT_PER_MIN),
                f64::from(PAIR_ACCEPT_PER_MIN) / 60.0,
                now,
            )),
            blob_conn: Arc::new(RateLimiter::per_minute(
                BLOB_CONN_PER_MIN,
                BLOB_CONN_PER_MIN,
            )),
            ping: Arc::new(RateLimiter::per_minute(PING_PER_MIN, PING_PER_MIN)),
            ping_refused: std::sync::atomic::AtomicU64::new(0),
            // 0 = unlimited (the default): no bucket, no map, nothing consulted (#84a).
            blob_bytes: (limits.blob_bytes_per_min > 0).then(|| {
                // FLOOR at two chunks, the repo idiom (`max_sessions.max(1)`, daemon.rs). A budget
                // between 1 and 32767 admits a request (reserving one chunk) and then silently
                // caps every servable blob at `budget - 16384` bytes — measured. Documenting a
                // floor and not enforcing it leaves an operator with a daemon that truncates
                // large blobs and says nothing (#84a fourth review).
                let per_min = limits.blob_bytes_per_min.max(MIN_BLOB_BYTES_PER_MIN);
                // Capacity == the per-minute rate: the burst a peer may take instantly is one
                // minute's worth, matching how `requests`/`blob_conn` are sized.
                Arc::new(RateLimiter::per_minute_f64(per_min as f64, per_min as f64))
            }),
        })
    }

    /// An effectively-unlimited bundle (control-only test daemon / `build_services` default).
    pub fn unlimited() -> Arc<Self> {
        let now = Instant::now();
        Arc::new(Self {
            requests: RateLimiter::unlimited_shared(),
            // `None`, not `Some(u32::MAX)`: it must PROPAGATE, so a service configuring 10/min in a
            // test daemon still gets unlimited rather than silently starting to throttle (#63).
            global_rpm: None,
            services: Mutex::new(HashMap::new()),
            pair_accept: Mutex::new(TokenBucket::new(
                f64::from(u32::MAX),
                f64::from(u32::MAX),
                now,
            )),
            blob_conn: RateLimiter::unlimited_shared(),
            ping: RateLimiter::unlimited_shared(),
            ping_refused: std::sync::atomic::AtomicU64::new(0),
            blob_bytes: None,
        })
    }

    /// Admit one pair-ALPN accept (FAIL-SAFE: `false` = over-limit → close the connection).
    pub fn admit_pair_accept(&self) -> bool {
        self.admit_pair_accept_at(Instant::now())
    }
    pub fn admit_pair_accept_at(&self, now: Instant) -> bool {
        self.pair_accept
            .lock()
            .expect("pair-accept bucket")
            .try_take(now)
            .is_ok()
    }

    /// Admit `bytes` of app-blob payload for `endpoint` (#84a).
    ///
    /// **FAIL-CLOSED on an unknown endpoint is the caller's job**, not this one: this answers only
    /// "is there budget". A `Throttle` event names a CONNECTION, and a connection with no recorded
    /// endpoint must be refused rather than metered against nobody — see `provider.rs`.
    ///
    /// `true` when no budget is configured (0 = unlimited), so the default path allocates nothing.
    pub fn admit_blob_bytes(&self, endpoint: &EndpointId, bytes: u64) -> bool {
        self.admit_blob_bytes_at(endpoint, bytes, Instant::now())
    }
    pub fn admit_blob_bytes_at(&self, endpoint: &EndpointId, bytes: u64, now: Instant) -> bool {
        match &self.blob_bytes {
            Some(l) => l.check_cost(endpoint, now, bytes as f64).is_ok(),
            None => true,
        }
    }

    /// Is a byte budget configured at all? Lets the provider skip arming the throttle intercept.
    pub fn blob_bytes_enabled(&self) -> bool {
        self.blob_bytes.is_some()
    }

    /// Admit one reachability probe from `endpoint` (#89). FAIL-SAFE: `false` = over-limit → close
    /// with no pong, which is the same answer an unpaired scanner gets, so a flooding peer learns
    /// nothing new from being refused.
    pub fn admit_ping(&self, endpoint: &EndpointId) -> bool {
        self.admit_ping_at(endpoint, Instant::now())
    }
    pub fn admit_ping_at(&self, endpoint: &EndpointId, now: Instant) -> bool {
        let admitted = self.ping.check(endpoint, now).is_ok();
        if !admitted {
            self.ping_refused
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        admitted
    }

    /// How many probes the ping bucket has refused since boot (#89 gate). Read by the flood
    /// test today; not yet surfaced to operators (see the field doc).
    pub fn pings_refused(&self) -> u64 {
        self.ping_refused.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Admit one app-blob connection from `endpoint` (FAIL-SAFE: `false` = over-limit → close).
    pub fn admit_blob_conn(&self, endpoint: &EndpointId) -> bool {
        self.admit_blob_conn_at(endpoint, Instant::now())
    }
    pub fn admit_blob_conn_at(&self, endpoint: &EndpointId, now: Instant) -> bool {
        self.blob_conn.check(endpoint, now).is_ok()
    }
}

#[cfg(test)]
mod tests {
    /// #89: the reachability probe is metered PER ENDPOINT.
    ///
    /// The arm was trust-gated but unmetered, so a paired peer could pong-flood with no bound but
    /// its own politeness. Per-endpoint, not global: one noisy peer must not deny liveness for
    /// every other peer, which is the opposite trade from the pair-accept bucket (where ids are
    /// attacker-chosen, so a global bound is the only one that works).
    #[test]
    fn the_reachability_probe_is_metered_per_endpoint() {
        let lim = MeshLimiters::from_config(&crate::config::LimitsCfg::default());
        let a = EndpointId::from_bytes([1u8; 32]);
        let b = EndpointId::from_bytes([2u8; 32]);
        let t0 = Instant::now();

        let mut admitted = 0;
        for _ in 0..500 {
            if lim.admit_ping_at(&a, t0) {
                admitted += 1;
            }
        }
        assert!(
            admitted <= 60,
            "a flooding peer is bounded at the per-minute rate: {admitted}"
        );
        // LOWER bound too, not just upper. `admitted > 0` is satisfied by a cap of ONE, because
        // `per_minute` floors capacity at `burst.max(1)` — so mutating PING_PER_MIN to 1 left this
        // whole suite green while making every paired peer report offline within REACH_TTL_SECS.
        // An honest peer probes on a 20s TTL (~3/min) and a client polling `status` at 1/s adds
        // ~3/min more; the cap has to leave real headroom above that, not merely be non-zero.
        assert!(
            admitted >= 30,
            "the cap must leave headroom for honest probing, not just be non-zero: {admitted} \
             admitted from a 60/min bucket — a cap this low reports healthy peers as offline"
        );

        // A DIFFERENT peer is unaffected — one noisy peer must not starve liveness for others.
        assert!(
            lim.admit_ping_at(&b, t0),
            "a second endpoint has its own budget; a global bucket would let one peer deny \
             reachability for the whole mesh"
        );
    }

    /// #84a: the byte budget is PER ENDPOINT, and 0 means unlimited.
    ///
    /// Per-endpoint is the whole design. A `Throttle` event names a CONNECTION, so a budget keyed
    /// on `connection_id` would give a peer a fresh allowance per connection — 60 connections a
    /// minute, 60 budgets, which is exactly the bypass #84a reports.
    #[test]
    fn the_byte_budget_is_per_endpoint_and_zero_is_unlimited() {
        use crate::config::LimitsCfg;
        let a = EndpointId::from_bytes([1u8; 32]);
        let b = EndpointId::from_bytes([2u8; 32]);
        let t0 = Instant::now();

        let cfg = LimitsCfg {
            blob_bytes_per_min: 32_768, // == the enforced floor (two chunks)
            ..Default::default()
        };
        let lim = MeshLimiters::from_config(&cfg);

        // 32768 == two chunks, so two fit and the third does not.
        assert!(lim.admit_blob_bytes_at(&a, 16_384, t0), "first chunk fits");
        assert!(lim.admit_blob_bytes_at(&a, 16_384, t0), "second chunk fits");
        assert!(
            !lim.admit_blob_bytes_at(&a, 16_384, t0),
            "the same endpoint must be refused once its own budget is spent"
        );
        assert!(
            lim.admit_blob_bytes_at(&b, 16_384, t0),
            "a DIFFERENT endpoint has its own budget — one peer must not starve another"
        );

        // 0 = unlimited is the default: nothing is consulted, however much is asked for.
        let lim = MeshLimiters::from_config(&LimitsCfg::default());
        assert!(!lim.blob_bytes_enabled(), "default must be off");
        for _ in 0..100 {
            assert!(
                lim.admit_blob_bytes_at(&a, u64::MAX, t0),
                "with no budget configured nothing is metered — upgrading must not start refusing"
            );
        }
    }

    /// #84a: a byte budget must meter BYTES, not calls.
    ///
    /// The existing blob limiter counts CONNECTIONS, so one granted peer can re-pull a 4 GB blob on
    /// each of 60 connections a minute. iroh-blobs' `Throttle` event carries the chunk `size`
    /// (usually 16 KiB), so a fixed cost of 1 per event would bound the event rate and leave the
    /// byte rate unbounded — which is the bug, not the fix.
    #[test]
    fn a_bucket_can_meter_a_variable_cost() {
        let t0 = Instant::now();
        // 20 KiB of capacity, refilling slowly enough that the window does not matter here.
        let mut b = TokenBucket::new(20_480.0, 1.0, t0);

        assert!(
            b.try_take_cost(t0, 16_384.0).is_ok(),
            "the first 16 KiB chunk fits inside a 20 KiB budget"
        );
        assert!(
            b.try_take_cost(t0, 16_384.0).is_err(),
            "the SECOND must be refused — 32 KiB does not fit in 20 KiB. A limiter that counted \
             calls would admit it, which is exactly #84a: bounded events, unbounded bytes"
        );

        // A cost larger than the whole bucket is unsatisfiable, not merely delayed-a-little.
        let mut b = TokenBucket::new(1_024.0, 1.0, t0);
        let wait = b
            .try_take_cost(t0, 4_096.0)
            .expect_err("a chunk larger than capacity cannot be admitted");
        assert!(wait > 0, "a refusal must report a wait, not zero");

        // And the cost-1 path is unchanged, so every existing caller keeps its semantics.
        let mut b = TokenBucket::new(2.0, 1.0, t0);
        assert!(b.try_take(t0).is_ok());
        assert!(b.try_take(t0).is_ok());
        assert!(b.try_take(t0).is_err(), "capacity 2 admits exactly two");
    }

    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn bucket_bursts_then_throttles_then_refills() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(3.0, 1.0, t0); // burst 3, 1 token/s
        assert!(b.try_take(t0).is_ok());
        assert!(b.try_take(t0).is_ok());
        assert!(b.try_take(t0).is_ok());
        let retry = b.try_take(t0).unwrap_err(); // empty → deny with a retry hint
        assert!(
            (900..=1000).contains(&retry),
            "≈1s until the next token, got {retry}"
        );
        let t1 = t0 + Duration::from_secs(1); // exactly one token refilled
        assert!(b.try_take(t1).is_ok());
        assert!(b.try_take(t1).is_err());
    }

    #[test]
    fn buckets_are_per_endpoint() {
        let t0 = Instant::now();
        let rl = RateLimiter::per_minute(60, 2); // burst 2, 60/min
        let (a, b) = (EndpointId::from([1u8; 32]), EndpointId::from([2u8; 32]));
        assert!(rl.check(&a, t0).is_ok());
        assert!(rl.check(&a, t0).is_ok());
        assert!(rl.check(&a, t0).is_err(), "a exhausted its own bucket");
        assert!(rl.check(&b, t0).is_ok(), "b has an independent bucket");
    }

    #[test]
    fn map_self_prunes_idle_buckets() {
        let t0 = Instant::now();
        let rl = RateLimiter::per_minute(60, 60);
        assert!(rl.check(&[1u8; 32].into(), t0).is_ok());
        assert!(rl.check(&[2u8; 32].into(), t0).is_ok());
        assert_eq!(rl.tracked(), 2);
        // A check far past IDLE_TTL prunes the two idle buckets before inserting the third.
        let later = t0 + IDLE_TTL + Duration::from_secs(1);
        assert!(rl.check(&[3u8; 32].into(), later).is_ok());
        assert_eq!(
            rl.tracked(),
            1,
            "idle buckets evicted; only the fresh one remains"
        );
    }

    #[test]
    fn unlimited_never_throttles() {
        let t0 = Instant::now();
        let rl = RateLimiter::unlimited_shared();
        for _ in 0..10_000 {
            assert!(rl.check(&[9u8; 32].into(), t0).is_ok());
        }
    }

    #[test]
    fn rate_gate_admits_then_throttles_and_none_endpoint_is_unlimited() {
        let t = Instant::now();
        let limiter = Arc::new(RateLimiter::per_minute(60, 2));
        let gate = RateGate::new(limiter, Some([5u8; 32].into()));
        assert!(gate.admit_at(t).is_ok());
        assert!(gate.admit_at(t).is_ok());
        assert!(
            gate.admit_at(t).is_err(),
            "third over the burst is throttled"
        );
        // A None-endpoint session (reserved no-identity path) is never rate-limited.
        let open = RateGate::new(RateLimiter::unlimited_shared(), None);
        for _ in 0..1000 {
            assert!(open.admit_at(t).is_ok());
        }
    }

    /// #63, THE ISSUE: a noisy service must not starve a quiet one. Before this, every service a
    /// peer could reach drew from one shared bucket, so an agent hammering a filesystem service
    /// exhausted the embedder's own low-rate control traffic to a *different* service.
    #[test]
    fn exhausting_one_service_leaves_another_admitting() {
        let ml = MeshLimiters::from_config(&crate::config::LimitsCfg {
            rate_limit_per_min: 2,
            ..Default::default()
        });
        let eid = EndpointId::from_bytes([7u8; 32]);
        let t = Instant::now();
        let noisy = ml.for_service("browser", None);
        let quiet = ml.for_service("control", None);

        // Drain the noisy service for this peer.
        assert!(noisy.check(&eid, t).is_ok());
        assert!(noisy.check(&eid, t).is_ok());
        assert!(
            noisy.check(&eid, t).is_err(),
            "the noisy service's own bucket must be exhausted"
        );

        // The quiet one is untouched. THIS is the property #63 asks for.
        assert!(
            quiet.check(&eid, t).is_ok(),
            "a different service must have its OWN budget — a shared bucket is what lets one \
             mount starve another"
        );
    }

    /// #63: a per-service value may only LOWER the rate. `[limits].rate_limit_per_min` stays a hard
    /// ceiling, which is what stops an unclamped `register_service` from uncapping a service — the
    /// vector that got the first attempt at this parked.
    #[test]
    fn a_per_service_rate_can_lower_but_never_raise() {
        let ml = MeshLimiters::from_config(&crate::config::LimitsCfg {
            rate_limit_per_min: 10,
            ..Default::default()
        });
        assert_eq!(ml.effective_rpm(None), Some(10), "absent = the global");
        assert_eq!(ml.effective_rpm(Some(3)), Some(3), "lower is honoured");
        assert_eq!(
            ml.effective_rpm(Some(1_000_000)),
            Some(10),
            "HIGHER IS CLAMPED — the global is a ceiling no config entry or control call can raise"
        );

        // …and observably so, through the real limiter rather than the arithmetic.
        let eid = EndpointId::from_bytes([8u8; 32]);
        let t = Instant::now();
        let greedy = ml.for_service("greedy", Some(1_000_000));
        for _ in 0..10 {
            assert!(greedy.check(&eid, t).is_ok());
        }
        assert!(
            greedy.check(&eid, t).is_err(),
            "an over-ceiling request must be clamped to 10, not honoured"
        );
    }

    /// #63: a reload with an UNCHANGED rate must preserve bucket state. `build_services` runs on
    /// every grant/revoke/register/roster-install, so re-creating limiters there would let a local
    /// caller spam grants to clear its own rate limit — a cheaper version of the hole this closes.
    #[test]
    fn a_reload_preserves_buckets_unless_the_rate_actually_changes() {
        // Global 10, service 1: high enough that raising the service rate to 5 later is a REAL
        // change rather than another clamp back to the same effective value. With global == 1 the
        // "changed" half of this test was vacuous — Some(5) clamps to 1 and nothing moves.
        let ml = MeshLimiters::from_config(&crate::config::LimitsCfg {
            rate_limit_per_min: 10,
            ..Default::default()
        });
        let eid = EndpointId::from_bytes([9u8; 32]);
        let t = Instant::now();

        let first = ml.for_service("svc", Some(1));
        assert!(first.check(&eid, t).is_ok());
        assert!(first.check(&eid, t).is_err(), "budget of 1 is spent");

        // A reload: same name, same rate. The bucket must still be empty.
        let again = ml.for_service("svc", Some(1));
        assert!(
            again.check(&eid, t).is_err(),
            "a reload must NOT mint a fresh limiter — that resets every peer's bucket, so a local \
             caller could spam grants to clear its own rate limit"
        );

        // A reload that CHANGES the rate applies the new one (and necessarily resets — the point).
        let changed = ml.for_service("svc", Some(5));
        assert!(
            changed.check(&eid, t).is_ok(),
            "a changed rate must take effect"
        );
    }

    /// #63: `unlimited()` must STAY unlimited per-service. The first attempt at this had
    /// `build_services` enforce 120/min on a bundle documented as unlimited.
    #[test]
    fn an_unlimited_bundle_stays_unlimited_per_service() {
        let ml = MeshLimiters::unlimited();
        assert_eq!(ml.effective_rpm(Some(1)), None, "unlimited PROPAGATES");
        let eid = EndpointId::from_bytes([10u8; 32]);
        let t = Instant::now();
        let l = ml.for_service("svc", Some(1));
        for _ in 0..1000 {
            assert!(
                l.check(&eid, t).is_ok(),
                "an unlimited bundle must not start throttling because a service configured a rate"
            );
        }
    }

    /// #63: the map is bounded, and over the cap it must NOT fall back to the global limiter —
    /// that would RAISE the rate of a service configured lower and undo the only-lower rule.
    #[test]
    fn the_service_map_is_bounded_and_never_falls_back_upward() {
        let ml = MeshLimiters::from_config(&crate::config::LimitsCfg {
            rate_limit_per_min: 100,
            ..Default::default()
        });
        for i in 0..(MAX_TRACKED_SERVICES + 20) {
            let l = ml.for_service(&format!("svc-{i}"), Some(1));
            let eid = EndpointId::from_bytes([11u8; 32]);
            let t = Instant::now();
            assert!(l.check(&eid, t).is_ok());
            assert!(
                l.check(&eid, t).is_err(),
                "service {i} past the cap must still enforce its OWN 1/min, not the global 100"
            );
        }
        // Bounded AND bounded-in-damage. `<= cap` alone passed a `map.clear()` on every cap hit,
        // which wipes every service's buckets for every peer — a far bigger reset than the single
        // eviction this is meant to be.
        let len = ml.services.lock().expect("not poisoned").len();
        assert_eq!(
            len, MAX_TRACKED_SERVICES,
            "the map must sit AT the cap after overflowing it — a smaller size means eviction \
             removed more than the one entry it needed to, resetting buckets it had no reason to"
        );
    }

    #[test]
    fn mesh_limiters_from_config_uses_the_request_rate() {
        let cfg = crate::config::LimitsCfg {
            rate_limit_per_min: 5,
            max_inflight: 16,
            max_sessions: 4,
            blob_bytes_per_min: 0,
            audit_retain_months: 0,
        };
        let ml = MeshLimiters::from_config(&cfg);
        let t = Instant::now();
        let eid = EndpointId::from([7u8; 32]);
        // burst == rate == 5 → five admits, then throttle.
        for _ in 0..5 {
            assert!(ml.requests.check(&eid, t).is_ok());
        }
        assert!(
            ml.requests.check(&eid, t).is_err(),
            "the request limiter engages at the config rate"
        );
    }

    #[test]
    fn pair_accept_and_blob_conn_limiters_engage() {
        let t = Instant::now();
        let ml = MeshLimiters::from_config(&crate::config::LimitsCfg {
            rate_limit_per_min: 120,
            max_inflight: 16,
            max_sessions: 4,
            blob_bytes_per_min: 0,
            audit_retain_months: 0,
        });
        // The GLOBAL pair-accept bucket engages after its burst (bounds a distinct-id stranger flood).
        let mut admitted = 0;
        for _ in 0..1000 {
            if ml.admit_pair_accept_at(t) {
                admitted += 1;
            }
        }
        assert!(
            admitted > 0 && admitted < 1000,
            "pair-accept limiter engages: admitted {admitted}"
        );
        // The per-endpoint blob-conn limiter engages per endpoint.
        let eid = EndpointId::from([4u8; 32]);
        let mut blob_ok = 0;
        for _ in 0..1000 {
            if ml.admit_blob_conn_at(&eid, t) {
                blob_ok += 1;
            }
        }
        assert!(
            blob_ok > 0 && blob_ok < 1000,
            "blob-conn limiter engages: ok {blob_ok}"
        );
    }
}
