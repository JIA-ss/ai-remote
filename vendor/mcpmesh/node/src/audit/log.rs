//! The append-only JSONL writer: a bounded-channel single-writer task whose `record()`
//! never blocks the caller, plus the sync append core. Best-effort by construction — an audit-write
//! failure is a logged warning, never a blocked or failed session (audit must not block or
//! fail the hot path). Local-only: nothing here is transmitted; the file is written and read only
//! on this machine.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tokio::sync::{broadcast, mpsc};

use crate::audit::record::AuditRecord;

/// Bounded queue depth between the hot path and the single writer task. Deep enough that a bursty
/// session never drops under normal load; bounded so a stuck disk cannot grow memory without limit.
/// On overflow, `record()` DROPS the record with a warning rather than
/// awaiting — the hot path never blocks on the audit channel.
const AUDIT_CHANNEL_DEPTH: usize = 1024;

/// Ring-buffer depth of the live broadcast fan-out (the telemetry stream tap). A slow subscriber
/// that falls this far behind gets a `Lagged` from `recv()` rather than blocking `record()` — the
/// hot path never waits on a live consumer. Bounded so a stuck subscriber cannot grow memory.
const STREAM_BROADCAST_DEPTH: usize = 256;

/// Append one record as a single JSONL line to `<dir>/<YYYY-MM>.jsonl` (the monthly file — the
/// rotation boundary is the calendar month, derived from the record's own `ts`). The directory is
/// created lazily. `O_APPEND` + a single `write_all` of `line + "\n"` is atomic-enough for JSONL:
/// concurrent appenders never interleave a partial line. This is the SYNC core the writer task runs
/// on the blocking pool; it is also unit-tested directly.
pub(crate) fn append_record(dir: &Path, rec: &AuditRecord) -> std::io::Result<()> {
    use std::io::Write as _;
    // The month key is the `YYYY-MM` prefix of the RFC3339 timestamp (always ≥ 7 chars from
    // `now_ts`); guard a malformed short ts by falling back to a fixed bucket rather than panicking.
    let month = rec.ts.get(0..7).unwrap_or("0000-00");
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{month}.jsonl"));
    let mut line = serde_json::to_vec(rec)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push(b'\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(&line)
}

/// One live mesh session, for the telemetry snapshot — published wire vocabulary
/// ([`mcpmesh_local_api::protocol`]), re-exported where the live table that mints it lives.
pub use mcpmesh_local_api::ActiveSession;

/// A running audit log: a handle over the sender half of the writer channel. Cheap to clone (an
/// `Arc` over the sender). The writer task drains the channel and appends each record on the
/// blocking pool for the daemon's lifetime.
///
/// It is also the telemetry hub: alongside the unchanged file-writer path, `record` fans every
/// record out to a `broadcast` channel (live subscribers) and the RAII [`SessionGuard`] maintains a
/// live-session table so "which sessions are open right now" is queryable.
pub struct AuditLog {
    tx: mpsc::Sender<AuditRecord>,
    /// The directory the writer task appends under — retained so readers (`audit_summary`)
    /// summarize exactly what THIS node wrote (per-node under an embedded node's root, the
    /// standard state dir under the daemon), never a re-derived location.
    dir: PathBuf,
    /// Live fan-out of every record to `subscribe()`rs. Independent of the file path — a full ring
    /// buffer or zero subscribers never affects the writer channel or blocks `record`.
    bcast: broadcast::Sender<AuditRecord>,
    /// Currently-open sessions keyed by a monotonic id (so a guard removes exactly its own row).
    /// Behind a `Mutex`; the lock is never held across an `.await`.
    live: Mutex<HashMap<u64, ActiveSession>>,
    /// Monotonic session-id source for the live table.
    seq: AtomicU64,
}

impl AuditLog {
    /// Spawn the single writer task over `dir` and return the handle. The task drains a bounded
    /// channel and appends each record via `append_record` on `spawn_blocking` (keeping fs off the
    /// runtime workers, the repo's fs house rule). An append error is logged and the record dropped —
    /// the task never exits on an IO error, so a transient full disk does not disable auditing.
    pub fn spawn(dir: PathBuf) -> Arc<Self> {
        let writer_dir = dir.clone();
        let (tx, mut rx) = mpsc::channel::<AuditRecord>(AUDIT_CHANNEL_DEPTH);
        let (bcast, _) = broadcast::channel(STREAM_BROADCAST_DEPTH);
        tokio::spawn(async move {
            while let Some(rec) = rx.recv().await {
                let dir = writer_dir.clone();
                let res = tokio::task::spawn_blocking(move || append_record(&dir, &rec)).await;
                match res {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(%e, "audit append failed (record dropped)"),
                    Err(e) => tracing::warn!(%e, "audit writer join failed (record dropped)"),
                }
            }
        });
        Arc::new(Self {
            tx,
            dir,
            bcast,
            live: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
        })
    }

    /// Record one event — NON-BLOCKING and infallible from the caller's view. Uses
    /// `try_send`: a full channel (writer wedged on a slow disk) or a closed channel (writer gone)
    /// DROPS the record with a debug log and returns immediately. The hot path NEVER awaits the disk
    /// and an audit failure NEVER propagates into a session.
    pub fn record(&self, rec: AuditRecord) {
        // Live fan-out FIRST (a cheap clone): an `Err` means zero subscribers — fine, ignore it.
        // This never blocks and never affects the file path below.
        let _ = self.bcast.send(rec.clone());
        if let Err(e) = self.tx.try_send(rec) {
            tracing::debug!(%e, "audit channel full or closed; dropping record (best-effort)");
        }
    }

    /// A live receiver of every subsequent `AuditRecord` (the telemetry stream tap). Lagging past the
    /// ring-buffer depth surfaces as `RecvError::Lagged`, never as back-pressure on `record`.
    pub fn subscribe(&self) -> broadcast::Receiver<AuditRecord> {
        self.bcast.subscribe()
    }

    /// Snapshot of currently-open sessions, sorted by open time. Sessions sharing an epoch-second
    /// have an unspecified relative order (the sort key is `opened_at` only).
    pub fn active_sessions(&self) -> Vec<ActiveSession> {
        let mut v: Vec<ActiveSession> = self
            .live
            .lock()
            .expect("audit live lock")
            .values()
            .cloned()
            .collect();
        v.sort_by_key(|s| s.opened_at);
        v
    }

    /// Emit `session_open`, insert the session into the live table, and return its id (the RAII
    /// [`SessionGuard`] holds it to remove exactly this row on drop). The lock is released before
    /// return and never held across an `.await`.
    fn open_tracked(&self, peer: String, service: String, principal: Option<String>) -> u64 {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        self.record(AuditRecord::session_open(
            now_ts(),
            Some(peer.clone()),
            service.clone(),
            // #57: the same #73 principal the live-session row keys on — record and row agree.
            principal.clone(),
        ));
        self.live.lock().expect("audit live lock").insert(
            id,
            ActiveSession {
                peer,
                service,
                opened_at: crate::util::epoch_now_i64(),
                principal,
            },
        );
        id
    }

    /// Remove the session `id` from the live table and emit `session_close` (only if the row was
    /// present, so a double-drop or disabled path is a no-op).
    fn close_tracked(&self, id: u64) {
        let removed = self.live.lock().expect("audit live lock").remove(&id);
        if let Some(s) = removed {
            self.record(AuditRecord::session_close(
                now_ts(),
                Some(s.peer),
                s.service,
                // #57: read back from the stored row, so open and close CANNOT disagree.
                s.principal,
            ));
        }
    }
}

/// A cheap, cloneable audit handle threaded into the backends / provider / trust hooks. `None` means
/// auditing is DISABLED (unit tests, or a daemon that failed to spawn the log) — `record` is a
/// no-op, so every hook is written unconditionally and the disabled path is zero-cost.
#[derive(Clone, Default)]
pub struct AuditSink(Option<Arc<AuditLog>>);

impl AuditSink {
    pub fn new(log: Arc<AuditLog>) -> Self {
        Self(Some(log))
    }
    /// The no-op sink (auditing disabled).
    pub fn disabled() -> Self {
        Self(None)
    }
    /// The directory the log writes under, or `None` when auditing is disabled.
    pub fn dir(&self) -> Option<&Path> {
        self.0.as_ref().map(|log| log.dir.as_path())
    }
    /// Record an event if enabled; a no-op otherwise. Never blocks, never errors.
    pub fn record(&self, rec: AuditRecord) {
        if let Some(log) = &self.0 {
            log.record(rec);
        }
    }

    /// Begin a tracked session: emits `session_open` and tracks it in the live table. Drop the
    /// returned [`SessionGuard`] to close it (emits `session_close` + table removal). A disabled sink
    /// returns a no-op guard that does nothing on drop.
    pub fn session(
        &self,
        peer: String,
        service: String,
        principal: Option<String>,
    ) -> SessionGuard {
        match &self.0 {
            Some(log) => SessionGuard {
                id: log.open_tracked(peer, service, principal),
                log: Some(log.clone()),
            },
            None => SessionGuard { log: None, id: 0 },
        }
    }

    /// A live receiver of subsequent records, or `None` when auditing is disabled.
    pub fn subscribe(&self) -> Option<broadcast::Receiver<AuditRecord>> {
        self.0.as_ref().map(|log| log.subscribe())
    }

    /// Currently-open sessions (empty when disabled).
    pub fn active_sessions(&self) -> Vec<ActiveSession> {
        self.0
            .as_ref()
            .map(|log| log.active_sessions())
            .unwrap_or_default()
    }
}

/// RAII live-session guard: emits `session_open` on creation and `session_close` + table-removal on
/// drop, so a session is tracked for exactly its lifetime regardless of how it ends (normal return,
/// early return, or panic unwind). A guard built over a disabled sink does nothing.
pub struct SessionGuard {
    log: Option<Arc<AuditLog>>,
    id: u64,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(log) = &self.log {
            log.close_tracked(self.id);
        }
    }
}

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use serde_json::Value;

use crate::audit::record::{args_hash, now_ts};

/// One pending request awaiting its response (keyed by JSON-RPC id). Holds only the DIGEST of the
/// args (never the raw args) plus the metadata the completed record needs.
struct Pending {
    method: String,
    tool: Option<String>,
    args_hash: String,
    started: Instant,
}

/// Per-session correlation for the proxied-request-line hook. The pump drives it from
/// its two directions: `on_request` (caller → server) hashes the args and remembers the request by
/// id; `on_response` (server → caller) matches the id, computes latency + status, and emits ONE
/// completed record with the response's `bytes_out` COUNT. A NOTIFICATION (no id) is recorded at
/// request time (no response correlates). PRIVACY — the raw arguments are hashed and then dropped;
/// they are never stored in `Pending` or written to the log.
///
/// The two directions run as concurrent async blocks in one pump future, so the pending map lives
/// behind a `Mutex` with only non-await critical sections (`Send`-safe, no lock held across `.await`).
/// The disabled path (a `RequestAuditor` built over `AuditSink::disabled()`) records nothing.
#[derive(Clone)]
pub struct RequestAuditor {
    inner: Option<Arc<RequestAuditorInner>>,
}

struct RequestAuditorInner {
    sink: AuditSink,
    peer: Option<String>,
    service: String,
    /// The caller's stable principal (#57) — stamped on every proxied request/notification
    /// record, same source as the session guard's.
    principal: Option<String>,
    pending: Mutex<HashMap<String, Pending>>,
    /// Has a dropped notification already been reported for this session (#76 review)? Cleared by
    /// the next delivered line, so a peer that recovers and is throttled again is reported again.
    /// Per-auditor, i.e. per session, which is also per authenticated peer.
    drop_reported: AtomicBool,
}

impl RequestAuditor {
    pub fn new(
        sink: AuditSink,
        peer: Option<String>,
        service: String,
        principal: Option<String>,
    ) -> Self {
        Self {
            inner: Some(Arc::new(RequestAuditorInner {
                sink,
                peer,
                service,
                principal,
                pending: Mutex::new(HashMap::new()),
                drop_reported: AtomicBool::new(false),
            })),
        }
    }

    /// Direction A (caller → local server): a request line is about to be forwarded. Hash its args
    /// (NEVER stored raw), extract method + tool NAME, and either remember it by id (a request, to be
    /// completed by its response) or — for a notification (no id) — record it immediately.
    /// Record a method-bearing frame that was DROPPED by the rate limiter and never forwarded
    /// (#76).
    ///
    /// A dropped REQUEST gets a synthesized `-32053` on the wire, so its sender learns of it. A
    /// dropped NOTIFICATION gets nothing — JSON-RPC gives it no reply channel — which made the
    /// loss silent and undetectable, so an embedder relying on server-initiated pushes could never
    /// make its reconciliation path optional.
    ///
    /// "No reply channel" is not "the sender cannot be told": the loss is now visible in the audit
    /// log and the `subscribe` stream, with `status: "rate_limited"`, distinct from a delivered
    /// notification (which carries no status). Not correlated to a specific frame — that is what
    /// the missing reply channel really costs — but observable after the fact.
    pub fn on_dropped(&self, frame: &Value) {
        let Some(inner) = &self.inner else { return };
        // Report ONCE until this auditor sees a delivery again (#76 review). Without this it is
        // the #84a audit-log DoS by another door: measured 274,310 records/s through the drop
        // path, and because `record` try_sends into a bounded channel and a 256-deep broadcast
        // ring, the flood EVICTS unrelated records — 18,382 of 20,000 lost by a live subscriber.
        // A peer that is already throttled would become the cheapest way to clear trust events
        // and session open/close out of every operator's live view.
        if inner.drop_reported.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            return;
        };
        let tool = if method == "tools/call" {
            frame
                .pointer("/params/name")
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        };
        let mut rec = AuditRecord::proxied_notification(
            now_ts(),
            inner.peer.clone(),
            inner.service.clone(),
            method.to_string(),
            tool,
            args_hash(frame.get("params").unwrap_or(&Value::Null)),
            inner.principal.clone(),
        );
        rec.status = Some("rate_limited".into());
        inner.sink.record(rec);
    }

    pub fn on_request(&self, frame: &Value) {
        let Some(inner) = &self.inner else { return };
        // A line got through: re-arm the drop report, so a later throttle is news again (#76).
        inner.drop_reported.store(false, Ordering::Relaxed);
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            return; // not a request/notification line (e.g. a client-side response); nothing to log
        };
        // Tool NAME only for tools/call — never the tool arguments or output.
        let tool = if method == "tools/call" {
            frame
                .pointer("/params/name")
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        };
        // PRIVACY: hash the params; the raw args are never retained past this line.
        let ah = args_hash(frame.get("params").unwrap_or(&Value::Null));

        match frame.get("id") {
            Some(id) if !id.is_null() => {
                // A request: remember it until its response correlates (bytes_out/latency/status).
                let key = id.to_string();
                let mut pending = inner
                    .pending
                    .lock()
                    .expect("audit pending map not poisoned");
                pending.insert(
                    key,
                    Pending {
                        method: method.to_string(),
                        tool,
                        args_hash: ah,
                        started: Instant::now(),
                    },
                );
            }
            _ => {
                // A notification (no id / null id): record now — no response will correlate.
                inner.sink.record(AuditRecord::proxied_notification(
                    now_ts(),
                    inner.peer.clone(),
                    inner.service.clone(),
                    method.to_string(),
                    tool,
                    ah,
                    inner.principal.clone(),
                ));
            }
        }
    }

    /// Direction B (local server → caller): a response line is about to go back. If it correlates to
    /// a pending request (by id), emit ONE completed record with the response's `bytes_out` COUNT,
    /// `status` (ok/error), and `latency_ms`. A response with no matching request (server-initiated)
    /// is ignored.
    ///
    /// SERVER-INITIATED REQUESTS (correctness, not just tidiness): MCP servers send REQUESTS to the
    /// client too (sampling/`createMessage`, `roots/list`, elicitation), and those flow through pump
    /// Direction B into here. Both peers number JSON-RPC ids from 1, so a server request id=1 would
    /// otherwise EVICT the client's still-pending id=1 and emit a bogus record (wrong bytes/latency),
    /// leaving the client's real response uncorrelated. A JSON-RPC *response* never carries `method`,
    /// so the `method`-present guard below cleanly excludes every server-initiated request/notification
    /// from response correlation (symmetric with Direction A's `on_request`, which REQUIRES `method`).
    pub fn on_response(&self, frame: &Value, bytes_out: u64) {
        let Some(inner) = &self.inner else { return };
        // Only a real response correlates: a frame carrying `method` is a server-initiated request or
        // notification (see the doc above), NOT a response to a client request — skip it.
        if frame.get("method").is_some() {
            return;
        }
        let Some(id) = frame.get("id").filter(|v| !v.is_null()) else {
            return; // a server notification, not a response — no correlation
        };
        let key = id.to_string();
        let pending = {
            let mut map = inner
                .pending
                .lock()
                .expect("audit pending map not poisoned");
            map.remove(&key)
        };
        let Some(p) = pending else { return };
        let status = if frame.get("error").is_some() {
            "error"
        } else {
            "ok"
        };
        let latency_ms = p.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        inner.sink.record(AuditRecord::proxied_request(
            now_ts(),
            inner.peer.clone(),
            inner.service.clone(),
            p.method,
            p.tool,
            p.args_hash,
            bytes_out,
            status.to_string(),
            latency_ms,
            inner.principal.clone(),
        ));
    }
}

#[cfg(test)]
mod tests {
    /// #76: a notification dropped by the rate limiter must be OBSERVABLE.
    ///
    /// A dropped request gets a synthesized -32053 on the wire; a notification has no reply
    /// channel and got nothing, so the loss was indistinguishable from delivery. An embedder
    /// relying on server-initiated pushes then has undetectable loss and can never make its
    /// reconciliation path optional — a permanent complexity cost for an invisible failure mode.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dropped_notification_is_recorded_distinctly_from_a_delivered_one() {
        let dir = tempfile::tempdir().unwrap();
        let sink = AuditSink::new(AuditLog::spawn(dir.path().to_path_buf()));
        let mut rx = sink.subscribe().expect("auditing enabled");
        let auditor = RequestAuditor::new(sink.clone(), Some("bob".into()), "notes".into(), None);

        let note = serde_json::json!({"method": "notifications/progress", "params": {"p": 1}});

        // Delivered: recorded with NO status.
        auditor.on_request(&note);
        let delivered = rx
            .recv()
            .await
            .expect("a delivered notification is recorded");
        assert_eq!(delivered.method.as_deref(), Some("notifications/progress"));
        assert_eq!(
            delivered.status, None,
            "a delivered notification carries no status"
        );

        // Dropped: recorded with status rate_limited, so the two are distinguishable.
        auditor.on_dropped(&note);
        let dropped = rx
            .recv()
            .await
            .expect("a DROPPED notification must be recorded too");
        assert_eq!(dropped.method.as_deref(), Some("notifications/progress"));
        assert_eq!(
            dropped.status.as_deref(),
            Some("rate_limited"),
            "the drop must be distinguishable from delivery — otherwise the loss is silent, which \
             is the whole of #76"
        );
        // And it still carries no raw args, only the hash.
        assert!(
            dropped.args_hash.is_some(),
            "args are hashed, never retained"
        );
    }

    /// #73: a live-session row carries the STABLE principal, not just a display nickname.
    ///
    /// Two devices under one nickname were indistinguishable in the subscribe snapshot's
    /// `active_sessions`, so
    /// per-peer session counts and any UI that acts on a session (revoke, disconnect, inspect)
    /// were keyed on a collidable string. Same argument as #41/#42.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_live_session_is_keyed_on_the_principal_not_the_nickname() {
        let dir = tempfile::tempdir().unwrap();
        let sink = AuditSink::new(AuditLog::spawn(dir.path().to_path_buf()));
        // TWO devices sharing one display nickname — the collision the issue is about.
        let a = "eid:aa".to_string();
        let b = "eid:bb".to_string();
        let _s1 = sink.session("laptop".into(), "notes".into(), Some(a.clone()));
        let _s2 = sink.session("laptop".into(), "notes".into(), Some(b.clone()));

        let live = sink.active_sessions();
        assert_eq!(live.len(), 2, "two sessions");
        assert_eq!(
            live[0].peer, live[1].peer,
            "the fixture must actually collide on the nickname, or this proves nothing"
        );

        let mut principals: Vec<_> = live.iter().filter_map(|s| s.principal.clone()).collect();
        principals.sort();
        assert_eq!(
            principals,
            vec![a, b],
            "each session must carry its own device principal — without it the two rows are \
             indistinguishable and a caller cannot act on one of them (#73)"
        );
    }

    use super::*;
    use crate::audit::record::{AuditRecord, args_hash};
    use serde_json::json;

    #[test]
    fn append_writes_jsonl_to_the_month_file_and_hides_raw_args() {
        let dir = tempfile::tempdir().unwrap();
        let secret = "top-secret-argument-value";
        let params = json!({"arguments": {"q": secret}});
        let rec = AuditRecord::proxied_request(
            "2026-07-03T14:02:11.480Z".into(),
            Some("bob".into()),
            "notes".into(),
            "tools/call".into(),
            Some("read_file".into()),
            args_hash(&params),
            42,
            "ok".into(),
            7,
            None,
        );
        append_record(dir.path(), &rec).unwrap();

        // The month is derived from the ts prefix → 2026-07.jsonl (the rotation boundary).
        let file = dir.path().join("2026-07.jsonl");
        let body = std::fs::read_to_string(&file).unwrap();
        assert!(body.ends_with('\n'), "one JSONL line, newline-terminated");
        assert_eq!(body.lines().count(), 1);
        // PRIVACY: the raw argument NEVER lands in the file — only the blake3 digest.
        assert!(
            !body.contains(secret),
            "raw argument leaked to disk: {body}"
        );
        assert!(body.contains("blake3:"));

        // A second record in a DIFFERENT month lands in its own file (monthly rotation).
        let rec2 = AuditRecord::session_open(
            "2026-08-01T00:00:00.000Z".into(),
            None,
            "notes".into(),
            None,
        );
        append_record(dir.path(), &rec2).unwrap();
        assert!(dir.path().join("2026-08.jsonl").exists());
        // The July file still has exactly one line (append, not overwrite).
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 1);
    }

    #[tokio::test]
    async fn record_is_non_blocking_and_writer_persists() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::spawn(dir.path().to_path_buf());
        let sink = AuditSink::new(log);
        // A burst of records — none of these calls awaits or blocks the caller.
        for i in 0..5 {
            sink.record(AuditRecord::session_open(
                format!("2026-07-03T14:02:1{i}.000Z"),
                Some("bob".into()),
                "notes".into(),
                None,
            ));
        }
        // The writer task drains asynchronously; poll the file until the records land.
        let file = dir.path().join("2026-07.jsonl");
        let mut lines = 0;
        for _ in 0..50 {
            if let Ok(body) = std::fs::read_to_string(&file) {
                lines = body.lines().count();
                if lines >= 5 {
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(lines, 5, "all five records persisted");
    }

    #[test]
    fn disabled_sink_is_a_silent_no_op() {
        // The disabled sink never touches disk and never errors — the zero-cost test path.
        let sink = AuditSink::disabled();
        sink.record(AuditRecord::trust(
            "2026-07-03T14:02:11.480Z".into(),
            "pair".into(),
            None,
            None,
        ));
        // Nothing observable to assert beyond "did not panic / no file created"; the call returns.
    }

    #[tokio::test]
    async fn request_auditor_records_correlated_line_without_raw_args() {
        let dir = tempfile::tempdir().unwrap();
        let audit_dir = dir.path().to_path_buf();
        let sink = AuditSink::new(AuditLog::spawn(audit_dir.clone()));
        let auditor = RequestAuditor::new(sink, Some("bob".into()), "notes".into(), None);

        let secret = "sensitive-search-query-xyzzy";
        // Direction A: a tools/call request with a sensitive argument. The auditor sees the raw args
        // (to hash them) but must NEVER write them.
        let req = json!({
            "jsonrpc": "2.0", "id": 7, "method": "tools/call",
            "params": {"name": "read_file", "arguments": {"query": secret}}
        });
        auditor.on_request(&req);
        // A notification (no id) is recorded immediately at request time.
        auditor.on_request(&json!({
            "jsonrpc": "2.0", "method": "notifications/progress",
            "params": {"token": "t1"}
        }));
        // Direction B: the correlated response (matched by id 7) with 6210 bytes out, status ok.
        let resp = json!({"jsonrpc": "2.0", "id": 7, "result": {"content": []}});
        auditor.on_response(&resp, 6210);

        let month = &crate::audit::now_ts()[..7];
        let file = audit_dir.join(format!("{month}.jsonl"));
        let mut body = String::new();
        for _ in 0..50 {
            if let Ok(b) = std::fs::read_to_string(&file)
                && b.matches("\"kind\":\"request\"").count() >= 2
            {
                body = b;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // PRIVACY: the raw argument value never reaches disk — only its blake3 digest.
        assert!(!body.contains(secret), "raw args leaked: {body}");
        // The correlated request record carries method + tool NAME + args_hash + bytes_out + status.
        assert!(body.contains("\"method\":\"tools/call\""));
        assert!(body.contains("\"tool\":\"read_file\""));
        assert!(body.contains("blake3:"));
        assert!(body.contains("\"bytes_out\":6210"));
        assert!(body.contains("\"status\":\"ok\""));
        assert!(body.contains("\"peer\":\"bob\""));
        // The notification recorded its method + a nil-tool (not a tools/call) with no bytes_out.
        assert!(body.contains("\"method\":\"notifications/progress\""));
    }

    /// A SERVER-INITIATED request (MCP sampling/roots/elicitation) flows through Direction B into
    /// `on_response`. It carries `method` AND an id that COLLIDES with the client's id numbering
    /// (both start at 1), so without the `method`-present guard it would evict the client's pending
    /// id=1 and emit a bogus record. Assert: the client's real id=1 response still correlates to a
    /// tools/call record, and the server request (method="sampling/createMessage") is NOT recorded.
    #[tokio::test]
    async fn server_initiated_request_does_not_corrupt_client_correlation() {
        let dir = tempfile::tempdir().unwrap();
        let sink = AuditSink::new(AuditLog::spawn(dir.path().to_path_buf()));
        let auditor = RequestAuditor::new(sink, Some("bob".into()), "notes".into(), None);
        // Client sends request id=1 (tools/call).
        auditor.on_request(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "read_file", "arguments": {"path": "/x"}}
        }));
        // Server sends its OWN request, ALSO id=1 (a collision) — flows through Direction B.
        auditor.on_response(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "sampling/createMessage", "params": {}}),
            50,
        );
        // Client's real response to id=1 arrives — must still correlate to the tools/call.
        auditor.on_response(&json!({"jsonrpc": "2.0", "id": 1, "result": {}}), 300);

        let month = &crate::audit::now_ts()[..7];
        let file = dir.path().join(format!("{month}.jsonl"));
        let mut body = String::new();
        for _ in 0..50 {
            if let Ok(b) = std::fs::read_to_string(&file)
                && b.contains("\"tool\":\"read_file\"")
            {
                body = b;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // The client's tools/call correlated (bytes_out from its REAL 300-byte response, not 50).
        assert!(body.contains("\"tool\":\"read_file\""));
        assert!(
            body.contains("\"bytes_out\":300"),
            "correlated to the client's response, not the server request"
        );
        // The server-initiated request was NOT recorded (the method-present guard skipped it).
        assert!(
            !body.contains("sampling/createMessage"),
            "server-initiated request must not be logged"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn record_fans_out_to_a_live_subscriber() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::spawn(dir.path().to_path_buf());
        let mut rx = log.subscribe();
        log.record(AuditRecord::session_open(
            now_ts(),
            Some("bob".into()),
            "notes".into(),
            None,
        ));
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.kind, crate::audit::record::AuditKind::SessionOpen);
        assert_eq!(got.peer.as_deref(), Some("bob"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_guard_tracks_active_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let sink = AuditSink::new(AuditLog::spawn(dir.path().to_path_buf()));
        assert!(sink.active_sessions().is_empty());
        {
            let _s = sink.session("bob".into(), "notes".into(), Some("eid:bob".into()));
            let live = sink.active_sessions();
            assert_eq!(live.len(), 1);
            assert_eq!(live[0].peer, "bob");
            assert_eq!(live[0].service, "notes");
        }
        // Guard dropped → session removed.
        assert!(sink.active_sessions().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_guard_removes_only_its_own_row() {
        // The core invariant of id-keying: dropping one guard removes EXACTLY its own session,
        // leaving concurrent (overlapping-lifetime) sessions untouched.
        let dir = tempfile::tempdir().unwrap();
        let sink = AuditSink::new(AuditLog::spawn(dir.path().to_path_buf()));
        let a = sink.session("alice".into(), "notes".into(), None);
        let b = sink.session("bob".into(), "notes".into(), None);
        assert_eq!(sink.active_sessions().len(), 2);
        // Drop A → only B survives.
        drop(a);
        let live = sink.active_sessions();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].peer, "bob");
        // Drop B → empty.
        drop(b);
        assert!(sink.active_sessions().is_empty());
    }

    #[test]
    fn disabled_sink_session_is_a_noop() {
        let sink = AuditSink::disabled();
        let _s = sink.session("bob".into(), "notes".into(), None);
        assert!(sink.active_sessions().is_empty()); // no panic, no tracking
    }

    #[tokio::test]
    async fn request_auditor_marks_error_responses() {
        let dir = tempfile::tempdir().unwrap();
        let sink = AuditSink::new(AuditLog::spawn(dir.path().to_path_buf()));
        let auditor = RequestAuditor::new(sink, Some("bob".into()), "notes".into(), None);
        auditor
            .on_request(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}));
        auditor.on_response(
            &json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -32000, "message": "boom"}}),
            120,
        );
        let month = &crate::audit::now_ts()[..7];
        let file = dir.path().join(format!("{month}.jsonl"));
        let mut ok = false;
        for _ in 0..50 {
            if let Ok(b) = std::fs::read_to_string(&file)
                && b.contains("\"status\":\"error\"")
            {
                ok = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(ok, "an error response records status=error");
    }
}
