//! The `subscribe` live event stream (pairing liveness & health telemetry). The frame vocabulary
//! ([`StreamFrame`] with snapshot/event/lagged, [`ActiveSession`], the audit record) is PUBLISHED
//! wire surface, defined in [`mcpmesh_local_api::protocol`] so embedders deserialize the stream
//! instead of re-modeling it; the daemon (`control::run_subscription`) is its producer. One tagged
//! envelope per frame; `Event.record` is the [`AuditRecord`](crate::audit::AuditRecord) verbatim,
//! so the stream and the on-disk log carry ONE schema.
pub use mcpmesh_local_api::{ActiveSession, StreamFrame};
