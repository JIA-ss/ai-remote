//! Cooperative cancellation for long-running control requests (#172).
//!
//! Deliberately NOT `JoinHandle::abort`. An aborted task returns nothing, so the caller waits
//! forever on a request that has already stopped; a cooperative token lets the cancelled work
//! return [`ERR_CANCELLED`](mcpmesh_local_api::ERR_CANCELLED) like any other answer. Every verb
//! that can be cancelled must therefore poll [`CancelToken::cancelled`] in a `select!` rather than
//! being killed from outside.
//!
//! A closed `Semaphore` is the whole mechanism: `acquire()` on a semaphore with permits available
//! never returns while the token is live (we hold the only permit), and returns `Err` the instant
//! `close()` is called — which is exactly a cancellation signal, level-triggered, observable by any
//! number of waiters. This is `tokio_util::sync::CancellationToken` in a dozen lines, and avoids
//! taking a whole dependency for one type.

use std::sync::Arc;

use tokio::sync::Semaphore;

/// A one-way flag: live until [`cancel`](Self::cancel), cancelled forever after.
///
/// Cheap to clone; every clone observes the same trip.
#[derive(Clone, Debug)]
pub struct CancelToken {
    sem: Arc<Semaphore>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    pub fn new() -> Self {
        // ZERO permits, not one: `acquire()` on an open semaphore with no permits parks forever,
        // which is the "not cancelled yet" state. A permit here would let the first waiter take it
        // and return Ok immediately — reporting a cancellation that never happened.
        Self {
            sem: Arc::new(Semaphore::new(0)),
        }
    }

    /// Trip the token. Idempotent — a second call is a no-op, so racing cancels are harmless.
    pub fn cancel(&self) {
        self.sem.close();
    }

    /// True once [`cancel`](Self::cancel) has been called. For a caller that wants to check rather
    /// than wait (a loop between chunks, say).
    pub fn is_cancelled(&self) -> bool {
        self.sem.is_closed()
    }

    /// Resolve when — and only when — this token is cancelled. Safe to use as a `select!` arm:
    /// it never completes on its own.
    pub async fn cancelled(&self) {
        // `acquire()` can only return here by erroring on close; the semaphore has no permits to
        // hand out. `Ok` is unreachable, and treating it as cancellation anyway is the safe read.
        let _ = self.sem.acquire().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_live_token_never_resolves_and_a_cancelled_one_resolves_for_every_waiter() {
        let t = CancelToken::new();
        assert!(!t.is_cancelled());
        // The load-bearing half: `cancelled()` must NOT complete while the token is live. A
        // semaphore built with a permit available would let this through instantly.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), t.cancelled())
                .await
                .is_err(),
            "a live token resolved `cancelled()`"
        );

        // Two independent waiters, both parked, both released by one trip — the property that lets
        // several fetches of one hash share a token.
        let (a, b) = (t.clone(), t.clone());
        let ha = tokio::spawn(async move { a.cancelled().await });
        let hb = tokio::spawn(async move { b.cancelled().await });
        t.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), ha)
            .await
            .expect("waiter a released")
            .expect("waiter a not panicked");
        tokio::time::timeout(std::time::Duration::from_secs(5), hb)
            .await
            .expect("waiter b released")
            .expect("waiter b not panicked");
        assert!(t.is_cancelled());

        // Idempotent, and a token cancelled BEFORE anyone waits still resolves immediately —
        // otherwise a cancel that beats the fetch's first poll would be lost.
        t.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(5), t.cancelled())
            .await
            .expect("an already-cancelled token resolves immediately");
    }
}
