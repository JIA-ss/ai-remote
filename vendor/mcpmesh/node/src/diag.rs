//! Node-level conditions that are observable but have no API to observe them through (#134).
//!
//! **The duplicate-identity condition.** Two nodes booted from COPIES of one mesh root present the
//! same endpoint id. The relay notices — it can only serve one — and tells the displaced client
//! `Status::SameEndpointIdConnected`. From the node's own point of view its peers simply stop being
//! reachable, with nothing saying why; diagnosing it cost a downstream real time.
//!
//! The local guards cannot see it: a second node on the SAME root is stopped by the redb lock, but
//! a node on a COPY is a different file, at a different path, possibly on another machine. The only
//! party that observes the collision is the network layer.
//!
//! **iroh 1.0.3 offers no API for it.** The signal arrives, and
//! `socket/transports/relay/actor.rs` handles it with `warn!("Relay server reports problem: {status}")`
//! — no event, no `Endpoint` state, no watcher. A `tracing` event is the only channel there is.
//!
//! **So this is a `Layer`, not a subscriber.** A `tracing` subscriber is global and an embedded
//! `mcpmesh-node` does not own it — the host application does. Installing our own would either fail
//! or fight theirs. [`IdentityConflictLayer`] is composed into whatever subscriber the host already
//! runs; the standalone daemon installs it itself at boot.
//!
//! **On matching a log message.** It is brittle by nature, so the needle is not written down here:
//! it is built from [`Status::SameEndpointIdConnected`]'s own `Display`, the same value iroh
//! formats into that `warn!`. An upstream rewording changes both sides together, and the test that
//! pins the two against each other fails if the shape of the coupling changes rather than the
//! wording.
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use iroh_relay::protos::relay::Status;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// The observed duplicate-identity condition (#134): when the relay last told us another endpoint
/// is presenting our identity.
///
/// Sticky by design. The condition is not a state the relay keeps reporting — it is announced once,
/// as the displaced connection is dropped — so a flag that cleared itself would be blank by the
/// time anyone read `status`. It records the LAST observation and lets the reader judge staleness
/// from the epoch, exactly as `last_change_epoch` does.
///
/// **ADVISORY, and relay-attested rather than authenticated.** The claim originates at a relay, and
/// iroh's older `Health { problem }` frame carries arbitrary text through the same log line, so any
/// relay in this node's set can synthesize this condition. It is a DIAGNOSTIC — it must never gate
/// authorization, refuse a peer, or stop a node. The worst a hostile relay achieves is a misleading
/// status field, which is the same class of trust already extended to a relay for reachability.
#[derive(Debug, Default)]
pub struct IdentityConflict {
    /// Epoch seconds of the last observation, or 0 for "never seen".
    last_seen: AtomicI64,
}

impl IdentityConflict {
    /// When the condition was last observed, or `None` if never.
    pub fn last_seen_epoch(&self) -> Option<i64> {
        match self.last_seen.load(Ordering::Relaxed) {
            0 => None,
            t => Some(t),
        }
    }

    /// Record an observation at `epoch`. Idempotent-ish: a later observation replaces an earlier
    /// one, so the reported time is the most recent, not the first.
    pub fn observe(&self, epoch: i64) {
        self.last_seen.store(epoch, Ordering::Relaxed);
    }
}

/// The message iroh's relay actor logs for the duplicate-identity condition, as a needle.
///
/// Built from iroh's own type rather than transcribed: `Status` derives `Display`, and the relay
/// actor formats exactly that value into its `warn!`. So this needle and iroh's message cannot
/// drift apart through a rewording — only through a change in HOW iroh logs it, which is what the
/// test pins.
fn needle() -> String {
    Status::SameEndpointIdConnected.to_string()
}

/// A [`tracing`] layer that watches for the relay's duplicate-identity report and records it.
///
/// Install it into the subscriber your application already owns:
///
/// ```ignore
/// use tracing_subscriber::prelude::*;
/// let conflict = std::sync::Arc::new(mcpmesh_node::diag::IdentityConflict::default());
/// tracing_subscriber::registry()
///     .with(your_fmt_layer)
///     .with(mcpmesh_node::diag::IdentityConflictLayer::new(conflict.clone()))
///     .init();
/// ```
///
/// It never filters another layer's events and never suppresses anyone's output. It does re-emit
/// one `error!` under mcpmesh's own target when it fires, because iroh's line says only that "a
/// relay reported a problem" — it does not say that THIS node's identity is in use elsewhere,
/// which is the fact an operator needs. (Under a scoped subscriber that nested event is dropped by
/// tracing's re-entrancy guard; under a global one it is emitted. The recording happens either
/// way, and the recording is what `status` reads.)
pub struct IdentityConflictLayer {
    state: Arc<IdentityConflict>,
    needle: String,
}

impl IdentityConflictLayer {
    pub fn new(state: Arc<IdentityConflict>) -> Self {
        Self {
            state,
            needle: needle(),
        }
    }
}

/// Pulls the `message` field out of an event without allocating for the other fields.
struct MessageVisitor<'a> {
    needle: &'a str,
    hit: bool,
}

impl tracing::field::Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" && format!("{value:?}").contains(self.needle) {
            self.hit = true;
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" && value.contains(self.needle) {
            self.hit = true;
        }
    }
}

impl<S: tracing::Subscriber> Layer<S> for IdentityConflictLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        // Cheap gate first: the condition is reported at WARN by iroh, and the overwhelming
        // majority of events in a busy host are not. Only then pay for the field visit.
        if *event.metadata().level() > tracing::Level::WARN {
            return;
        }
        let mut v = MessageVisitor {
            needle: &self.needle,
            hit: false,
        };
        event.record(&mut v);
        if v.hit {
            self.state.observe(crate::util::epoch_now_i64());
            // Re-emit at ERROR under OUR target, naming the condition and the remedy. iroh's own
            // line says a relay reported a problem; it does not say that this node's identity is
            // in use elsewhere, which is the fact an operator needs.
            tracing::error!(
                "another endpoint is presenting THIS node's identity — two nodes booted from \
                 copies of one mesh root cannot both use the relay, and peers will appear \
                 unreachable. Stop the duplicate, or give it its own identity."
            );
        }
    }
}

/// Install [`IdentityConflictLayer`] as the process-global subscriber. **Standalone daemon only.**
///
/// A `tracing` subscriber is process-global and can be set once. `serve_forever` owns its process
/// and installs none of its own, so taking it there is safe. It must NOT be called from any path an
/// embedded node shares: seizing the global would panic a host that calls `fmt::init()` afterwards,
/// and silently swallow its logs for the process lifetime if it uses `try_init()`.
///
/// The layer is filtered to `WARN`, which matters for more than tidiness: `tracing`'s max-level
/// hint is derived from the installed subscriber, and a bare registry sets it to `TRACE` — turning
/// every `trace!` and `trace_span!` in iroh, quinn and tokio from a compiled-out no-op into an
/// allocation on the datagram hot path, for a daemon that prints nothing anyway.
///
/// Best effort: if a subscriber already exists we leave it alone and say so at `debug!`.
pub fn install_for_daemon(state: Arc<IdentityConflict>) {
    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::layer::SubscriberExt;

    let layer = IdentityConflictLayer::new(state).with_filter(LevelFilter::WARN);
    if tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer)).is_err()
    {
        tracing::debug!(
            "a tracing subscriber is already installed, so the duplicate-identity detector was \
             NOT registered (#134)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::prelude::*;

    /// #134: the needle really is what iroh logs.
    ///
    /// The coupling, not the wording, is the thing under test — the needle is DERIVED from
    /// `Status`'s `Display`, so a rewording moves both together. What this pins is that the
    /// derivation still produces a non-trivial, identity-specific string: if iroh ever made
    /// `Status` display as something generic (or empty), the layer would start matching unrelated
    /// warnings, or nothing at all, and silently stop being a diagnostic.
    #[test]
    fn the_needle_is_derived_from_iroh_and_is_specific() {
        let n = needle();
        assert!(
            n.len() > 20,
            "too short to be a safe substring match — it would match unrelated warnings: {n:?}"
        );
        assert!(
            n.to_lowercase().contains("endpoint id"),
            "the needle must still be about endpoint identity; if iroh made this generic the \
             layer is no longer a diagnostic: {n:?}"
        );
        assert_ne!(
            n,
            Status::Healthy.to_string(),
            "the needle must not collapse onto the healthy status"
        );
    }

    /// #134: the layer records the condition, and does NOT record on unrelated warnings.
    ///
    /// The second half is the one that matters — a detector that fires on any warning would
    /// report a duplicate identity on a node that merely lost a relay, which is worse than no
    /// detection at all.
    #[test]
    fn the_layer_records_only_the_duplicate_identity_warning() {
        let state = Arc::new(IdentityConflict::default());
        assert_eq!(state.last_seen_epoch(), None, "nothing observed yet");

        let sub = tracing_subscriber::registry().with(IdentityConflictLayer::new(state.clone()));
        tracing::subscriber::with_default(sub, || {
            // Unrelated noise at the same level, including a DIFFERENT relay status.
            tracing::warn!("Relay server reports problem: {}", Status::Healthy);
            tracing::warn!("peer unreachable");
            tracing::error!("something else broke entirely");
            assert_eq!(
                state.last_seen_epoch(),
                None,
                "an unrelated warning must never read as a duplicate identity"
            );

            // The real thing, formatted exactly as iroh's relay actor formats it.
            tracing::warn!(
                "Relay server reports problem: {}",
                Status::SameEndpointIdConnected
            );
            assert!(
                state.last_seen_epoch().is_some(),
                "the duplicate-identity report must be recorded"
            );
        });
    }

    /// The stamp is the LAST observation, so a reader can judge staleness — the condition is
    /// announced once as the connection is dropped, never re-reported, so a self-clearing flag
    /// would be blank by the time anyone read `status`.
    #[test]
    fn the_stamp_is_the_most_recent_observation() {
        let s = IdentityConflict::default();
        s.observe(100);
        assert_eq!(s.last_seen_epoch(), Some(100));
        s.observe(200);
        assert_eq!(s.last_seen_epoch(), Some(200), "later replaces earlier");
    }
}
