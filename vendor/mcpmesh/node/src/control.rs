//! Server-side mcpmesh-local/1 dispatch. On each accepted connection the SERVER
//! writes a `Hello` frame FIRST ("the first exchange ... identifies the api"), then reads
//! request frames, dispatches on the `method` string, and writes JSON-RPC-shaped response
//! frames back. Same-uid peers only (the seam's platform gate — peer-euid on unix, owner-only
//! pipe DACL on windows) — the gate runs before the hello.
//!
//! Dispatch discipline: the method is extracted with
//! [`mcpmesh_local_api::method_of`] and params are deserialized PER-METHOD into the typed
//! param structs local-api defines (`protocol.rs` — the one wire truth, so daemon/client
//! shape drift is a compile error) — never by deserializing the whole message into
//! `Request` (adjacent tagging rejects `params:{}` for parameterless methods, which a
//! conforming third-party client may send). Most verbs are plain request/response;
//! `open_session` and `subscribe` are special: after those requests the connection stops
//! being JSON-RPC and becomes a raw MCP byte pipe / a one-way event stream.
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use mcpmesh_local_api::transport::{LocalListener, LocalStream};
use mcpmesh_local_api::{
    API_NAME, API_VERSION, AuditListParams, AuditPruneParams, BlobFetchCancelParams,
    BlobFetchParams, BlobGrantParams, BlobPublishParams, BlobRepublishParams, BlobRevokeParams,
    BlobUnpublishParams, Hello, InviteParams, OpenSessionParams, OrgApproveParams, OrgCreateParams,
    OrgJoinCodeParams, OrgJoinParams, OrgRevokeParams, PairParams, PeerServicesParams,
    RosterInstallParams, ServiceAllowParams, SetAppMetadataParams, SetNicknameParams,
    SetRelaysParams, SetRosterUrlParams, StatusResult, UnregisterServiceParams,
    UserKeyImportParams, method_of,
};
use mcpmesh_net::framing::{FrameReader, Inbound, write_frame};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::daemon::MeshState;
use crate::ipc::{self, MAX_FRAME_BYTES};

/// Live daemon state behind the control API. `mesh` is the endpoint + gate + serve handle
/// the real daemon owns; it is `None` in control-only construction (unit tests),
/// in which case `register_service`/`peer_add` fail gracefully. The `status` service/peer
/// lists are read LIVE from the mesh's config + store on each call — there is no cached
/// snapshot here. `shutdown` is the internal signal a `shutdown` request raises so the
/// accept loop can stop cleanly.
pub struct DaemonState {
    pub stack_version: String,
    pub(crate) mesh: Option<Arc<MeshState>>,
    shutdown: Notify,
    /// Every live control-connection serving task — one per accepted socket connection
    /// ([`serve_control`]'s per-connection spawn) AND one per in-process pipe
    /// ([`Node::control`](crate::node::Node::control)'s duplex spawn). Tracked so a shutdown
    /// can ABORT them, exactly like `MeshState::accept_task`/`poll_loop`/the boot background
    /// loops: without this, a `subscribe` stream's task only notices its client is gone via a
    /// subsequent failed WRITE — which, with no audit traffic, may never come — so it (and the
    /// `Arc<DaemonState>`/mesh/redb lock it holds) would outlive the node itself. A std `Mutex`
    /// (never held across an await; push/drain are sync + tiny, like `ephemeral_services`).
    control_tasks: std::sync::Mutex<Vec<JoinHandle<()>>>,
}

impl DaemonState {
    /// Control-only state (no mesh) — used by unit tests. `register_service`/`peer_add`
    /// return an error against this construction.
    pub fn new(stack_version: impl Into<String>) -> Self {
        Self {
            stack_version: stack_version.into(),
            mesh: None,
            shutdown: Notify::new(),
            control_tasks: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The full daemon state: the control server over the mesh half.
    ///
    /// `pub` (like [`MeshState::new`](crate::daemon::MeshState::new)) so integration tests can
    /// assemble a serving `DaemonState` around a HERMETIC `MeshState` (temp config + store +
    /// endpoint) and drive the real control handlers — e.g. the `pair --remove` test calls
    /// `daemon::remove_peer` over a state built this way, asserting on the store + config truth.
    pub fn with_mesh(stack_version: impl Into<String>, mesh: Arc<MeshState>) -> Self {
        Self {
            stack_version: stack_version.into(),
            mesh: Some(mesh),
            shutdown: Notify::new(),
            control_tasks: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Wait until a shutdown has been requested — the control `shutdown` verb, or an
    /// embedder's `Node::shutdown`. (`notify_one` stores a permit, so a request that
    /// landed before this call still resolves it.)
    pub(crate) async fn shutdown_requested(&self) {
        self.shutdown.notified().await;
    }

    /// Raise the shutdown signal — the programmatic form of the control `shutdown` verb.
    pub(crate) fn request_shutdown(&self) {
        self.shutdown.notify_one();
    }

    /// The mesh half, if this daemon owns an endpoint (always, except control-only tests).
    /// Returns `&Arc<MeshState>` so callers that must reload the accept loop (the pairing
    /// grant, `register_service`) can cheaply clone the shared handle.
    pub(crate) fn mesh(&self) -> Option<&Arc<MeshState>> {
        self.mesh.as_ref()
    }

    /// The mesh half, or the one control-only-mode refusal every mesh-requiring control verb
    /// answers — the single home of the "daemon has no mesh (control-only mode)" guard.
    pub(crate) fn mesh_required(&self) -> Result<&Arc<MeshState>> {
        self.mesh()
            .context("daemon has no mesh (control-only mode)")
    }

    /// Track a just-spawned control-connection serving task so a later
    /// [`abort_control_tasks`](Self::abort_control_tasks) can end it. Opportunistically drops
    /// already-finished handles first (most connections close long before shutdown) so a
    /// long-lived daemon's list stays bounded to roughly the CURRENTLY live connections rather
    /// than growing with every connection ever served.
    pub(crate) fn track_control_task(&self, handle: JoinHandle<()>) {
        let mut tasks = self
            .control_tasks
            .lock()
            .expect("control_tasks lock not poisoned");
        tasks.retain(|h| !h.is_finished());
        tasks.push(handle);
    }

    /// Abort every tracked live control-connection serving task — subscription streams end
    /// immediately, in-flight requests get a dropped connection. Called from
    /// [`Node::shutdown`](crate::node::Node::shutdown) AND from the wire-level `shutdown` verb
    /// handler (after that verb's own ack is written), so BOTH the programmatic and the
    /// control-protocol shutdown path get the same guarantee: shutdown means shutdown, even for
    /// a connection with no other reason to ever notice.
    pub(crate) fn abort_control_tasks(&self) {
        let tasks = std::mem::take(
            &mut *self
                .control_tasks
                .lock()
                .expect("control_tasks lock not poisoned"),
        );
        for task in tasks {
            task.abort();
        }
    }
}

/// Accept control connections until a `shutdown` request stops the loop. Each connection is
/// handled in its own task so independent clients never head-of-line-block one another.
pub async fn serve_control(mut listener: LocalListener, state: Arc<DaemonState>) -> Result<()> {
    loop {
        tokio::select! {
            // `notify_one` stores a permit if the loop is momentarily between iterations, so
            // a fresh `notified()` here still resolves — the shutdown signal is never lost.
            () = state.shutdown.notified() => {
                tracing::info!("shutdown requested; control server stopping");
                return Ok(());
            }
            accepted = listener.accept() => {
                let stream = match accepted {
                    Ok(s) => s,
                    Err(e) => {
                        // Back off before retrying: a persistent accept error (e.g. EMFILE
                        // under fd exhaustion) would otherwise busy-spin the loop at 100% CPU.
                        tracing::warn!(%e, "control accept failed; backing off");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let conn_state = state.clone();
                let handle = tokio::spawn(async move {
                    if let Err(e) = handle_conn(stream, conn_state).await {
                        tracing::debug!(%e, "control connection ended");
                    }
                });
                state.track_control_task(handle);
            }
        }
    }
}

async fn handle_conn(stream: LocalStream, state: Arc<DaemonState>) -> Result<()> {
    // Same-user gate BEFORE any frame: refuse other users pre-hello (peer-euid
    // on unix; on windows the pipe DACL already enforced this at connect). A cross-user
    // connection attempt is security-relevant, so it is logged at `warn!` here (returning Ok
    // keeps the normal clean-close path at `debug!` in `serve_control`).
    if let Err(e) = ipc::check_peer(&stream) {
        tracing::warn!(%e, "refused unauthorized control connection");
        return Ok(());
    }
    let (read_half, write_half) = mcpmesh_local_api::transport::split_local(stream);
    serve_control_io(read_half, write_half, state).await
}

/// Serve one mcpmesh-local/1 connection over ALREADY-AUTHORIZED byte halves — the
/// transport-agnostic body of `handle_conn`, and what an embedded node's in-memory
/// control connection runs (`Node::control` — a tokio duplex needs no peer gate: it
/// never leaves the process).
pub async fn serve_control_io<W>(
    read_half: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    write_half: W,
    state: Arc<DaemonState>,
) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    // The server speaks first: a `Hello` frame identifies the api.
    let hello = Hello {
        api: API_NAME.into(),
        api_version: API_VERSION.into(),
        api_minor: mcpmesh_local_api::API_MINOR,
        stack_version: state.stack_version.clone(),
    };
    // ONE writer, shared by the read loop and every spawned request task (#172). A
    // `tokio::sync::Mutex` rather than a `std` one because it is taken for a WHOLE frame across
    // awaits: locking per `poll_write` would let two tasks interleave fragments of two frames,
    // which is worse than the head-of-line blocking this change removes.
    let writer = Arc::new(tokio::sync::Mutex::new(write_half));
    write_frame(&mut *writer.lock().await, &serde_json::to_value(&hello)?).await?;

    let reader = FrameReader::new(tokio::io::BufReader::new(read_half), MAX_FRAME_BYTES);
    // NOTE: control connections carry no framing-violation strike bound (unlike the
    // mesh path in net::endpoint). Acceptable — the peer is same-uid, already inside
    // the trust boundary; a strike/close budget lands if/when this surface widens.

    // #36: names this connection registered EPHEMERALLY, torn down when it closes. Shared with
    // the request loop via an Arc so the loop (which owns `reader`/`write_half`) can record into
    // it while this frame keeps a handle to drain after the loop ends, on ANY exit path.
    let ephemeral_registered = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let loop_state = state.clone();
    let eph = ephemeral_registered.clone();
    let outcome: Result<()> = async move {
        let mut reader = reader;
        // In-flight request tasks (#172). Dropping this JoinSet ABORTS them, which is what makes
        // closing a control connection genuinely stop a `blob_fetch` — before this, the transfer
        // ran to completion and only then discovered nobody was listening.
        let mut tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();
        // Per-CONNECTION in-flight bound. Never `acquire().await` on it in this loop: waiting for a
        // permit here would reintroduce exactly the head-of-line blocking concurrency exists to
        // remove. Over the cap we refuse immediately instead — see `ERR_TOO_MANY_INFLIGHT`.
        let inflight = Arc::new(tokio::sync::Semaphore::new(
            mcpmesh_local_api::MAX_INFLIGHT,
        ));
        loop {
            // Reap finished tasks so the set does not grow for the connection's whole life.
            // Nothing to inspect: a handler panic is caught INSIDE the task and answered there
            // (see the `catch_unwind` below), so a join error here cannot be a swallowed panic.
            while tasks.try_join_next().is_some() {}
            match reader.next().await? {
                None => return Ok(()), // client closed the connection
                Some(Inbound::Violation(v)) => {
                    // A malformed/oversized request frame carries no recoverable id: answer a
                    // JSON-RPC parse error and keep the connection open for the next frame.
                    let resp = error(Value::Null, -32700, format!("invalid request frame: {v:?}"));
                    write_frame(&mut *writer.lock().await, &resp).await?;
                }
                Some(Inbound::Frame(req)) => {
                    // NOTE: the "shutdown" method string is matched here and in `dispatch`;
                    // the small duplication is deliberate (the ack shape stays in `dispatch`).
                    if method_of(&req) == Some("shutdown") {
                        // An explicit stop must ALWAYS stop: raise the shutdown signal FIRST
                        // (unconditionally), THEN best-effort ack. A client that sends `shutdown`
                        // and closes without reading the ack must still stop the daemon.
                        loop_state.shutdown.notify_one();
                        let resp = dispatch(&req, &loop_state);
                        let _ = write_frame(&mut *writer.lock().await, &resp).await;
                        // Abort every OTHER live control connection (this one's own task is
                        // included and about to return anyway — no correctness issue, the ack
                        // above already landed). Mirrors `Node::shutdown`'s programmatic path:
                        // the wire-level `shutdown` verb gets the same guarantee that an
                        // attached `subscribe` stream (or any other live connection) ends
                        // immediately rather than lingering until it next fails a write.
                        loop_state.abort_control_tasks();
                        return Ok(());
                    }
                    if method_of(&req) == Some("open_session") {
                        // After this request the connection STOPS being JSON-RPC and becomes a raw
                        // MCP byte pipe (protocol.rs `OpenSession`): hand the framed halves to the
                        // daemon's dial + pipe, which consumes the connection for the session's
                        // lifetime. The loop cannot continue — `reader`/`write_half` move away.
                        // (A malformed params SHAPE — not merely absent fields, which default —
                        // answers an error frame and keeps the connection JSON-RPC.)
                        let params = req.get("params").cloned().unwrap_or(Value::Null);
                        let p: OpenSessionParams = match params_of(&params) {
                            Ok(p) => p,
                            Err(e) => {
                                let id = req.get("id").cloned().unwrap_or(Value::Null);
                                // Params shape error → -32602 (invalid params), matching `respond`.
                                let resp = error(id, -32602, format!("open_session failed: {e}"));
                                write_frame(&mut *writer.lock().await, &resp).await?;
                                continue;
                            }
                        };
                        let write_half = reclaim_writer(&mut tasks, writer).await?;
                        return crate::daemon::open_session(
                            &loop_state,
                            &p.peer,
                            &p.service,
                            p.idle_timeout_secs,
                            reader,
                            write_half,
                        )
                        .await;
                    }
                    if method_of(&req) == Some("subscribe") {
                        // Like `open_session`, this upgrades the connection: after `subscribe` it
                        // STOPS being request/response and becomes a one-way push stream of
                        // `StreamFrame`s (`crate::stream`). The loop cannot continue — `write_half`
                        // moves into the stream driver for the subscription's lifetime.
                        let write_half = reclaim_writer(&mut tasks, writer).await?;
                        return run_subscription(&loop_state, write_half).await;
                    }
                    // Everything else runs CONCURRENTLY (#172): the loop goes straight back to the
                    // reader, so a minutes-long `blob_fetch` no longer stalls every other verb on
                    // this connection. Responses consequently arrive in COMPLETION order — see
                    // `API_MINOR` 44.
                    let permit = match inflight.clone().try_acquire_owned() {
                        Ok(p) => p,
                        Err(_) => {
                            let id = req.get("id").cloned().unwrap_or(Value::Null);
                            let resp = error(
                                id,
                                mcpmesh_local_api::ERR_TOO_MANY_INFLIGHT,
                                format!(
                                    "connection already has {} requests in flight; retry after one completes, or use a second control connection",
                                    mcpmesh_local_api::MAX_INFLIGHT
                                ),
                            );
                            write_frame(&mut *writer.lock().await, &resp).await?;
                            continue;
                        }
                    };
                    // #36 teardown, recorded BEFORE the handler runs — the ordering is
                    // load-bearing now that the handler can be ABORTED. `register_service` inserts
                    // into `mesh.ephemeral_services` and then awaits a config reload; a client that
                    // closes the socket in that window used to be impossible (the handler was
                    // awaited inline) and now aborts the task mid-flight. Recording afterwards left
                    // the registration live with an empty teardown list — an orphan service
                    // pointing at a dead backend, and a name `register_service` then refused
                    // forever. Recorded up front it is torn down whatever happens; the task removes
                    // it again if the register turns out to have FAILED, so this connection never
                    // tears down a name that belongs to another one.
                    let pending_ephemeral = ephemeral_name(&req).map(|name| {
                        eph.lock()
                            .expect("ephemeral_registered lock not poisoned")
                            .push(name.to_string());
                        name.to_string()
                    });
                    let task_state = loop_state.clone();
                    let task_writer = writer.clone();
                    let task_eph = eph.clone();
                    tasks.spawn(async move {
                        // Held for the request's lifetime; released when this task ends.
                        let _permit = permit;
                        // A panic must ANSWER, not hang. Before dispatch was concurrent a panicking
                        // handler unwound the connection task and the client saw EOF immediately;
                        // in a `JoinSet` it would instead be swallowed, leaving a client that sent
                        // one request waiting on a response frame that can never arrive. Caught
                        // here so the caller gets `-32603` — strictly better than the EOF it used
                        // to get, and the connection stays usable for the requests behind it.
                        let resp = match n0_future::FutureExt::catch_unwind(
                            std::panic::AssertUnwindSafe(handle_request(&req, &task_state)),
                        )
                        .await
                        {
                            Ok(resp) => resp,
                            Err(_) => {
                                let id = req.get("id").cloned().unwrap_or(Value::Null);
                                error(
                                    id,
                                    -32603,
                                    "internal error: the request handler panicked (see the daemon log)",
                                )
                            }
                        };
                        // #36: a register that FAILED never took the name, so drop the teardown
                        // entry this connection optimistically recorded — otherwise a refused
                        // register (a name another connection holds) would tear down that other
                        // connection's service when this one closes.
                        if let Some(name) = pending_ephemeral
                            && resp.get("result").is_none()
                        {
                            let mut held = task_eph
                                .lock()
                                .expect("ephemeral_registered lock not poisoned");
                            if let Some(i) = held.iter().rposition(|n| *n == name) {
                                held.remove(i);
                            }
                        }
                        // A write failure cannot be propagated from here. It does not need to be:
                        // the only reason it fails is that the client is gone, and the read loop
                        // learns that from its very next `reader.next()`.
                        let _ = write_frame(&mut *task_writer.lock().await, &resp).await;
                    });
                }
            }
        }
    }
    .await;

    // Teardown on ANY exit path (clean close, IO error, or the open_session/subscribe upgrades
    // returning): unregister every service this connection registered ephemerally (#36). A no-op
    // when it registered none.
    if let Some(mesh) = state.mesh() {
        let names = ephemeral_registered
            .lock()
            .expect("ephemeral_registered lock not poisoned")
            .clone();
        crate::daemon::unregister_ephemeral(mesh, &names).await;
    }
    outcome
}

/// The service name a request would register EPHEMERALLY (#36), or `None` for anything else.
///
/// Peeked from the REQUEST alone, because the teardown entry has to be recorded before the handler
/// runs — see the call site.
fn ephemeral_name(req: &Value) -> Option<&str> {
    if method_of(req) != Some("register_service") {
        return None;
    }
    let params = req.get("params")?;
    params
        .get("ephemeral")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        .then(|| params.get("name").and_then(|v| v.as_str()))
        .flatten()
}

/// Take the write half back, by value, for an upgrade path that must OWN it (#172).
///
/// `open_session` and `subscribe` consume the connection for their lifetime, which is why
/// concurrent dispatch could not be added alongside the rest of #82: once responses are written
/// from spawned tasks the writer has to be shared, and a shared writer cannot be moved.
///
/// The resolution is to drain first. Every in-flight request runs to completion and writes its
/// response, its `Arc` clone drops, and the sole remaining reference unwraps back into the plain
/// writer — so the upgrade path keeps its existing by-value signature and no frame can interleave
/// with the raw bytes that follow.
///
/// **An upgrade therefore WAITS for this connection's own in-flight requests.** A client that
/// pipelines a `blob_fetch` and then a `subscribe` down one socket waits for the fetch. Upgrade on
/// a fresh connection — which is what `ControlClient` does anyway, since `open_stream`/`open_session`
/// consume `self`.
///
/// Taking the guard WITHOUT draining would be safe on the wire and wrong everywhere else: in-flight
/// tasks would block on a mutex nobody ever releases, holding their responses forever.
async fn reclaim_writer<W>(
    tasks: &mut tokio::task::JoinSet<()>,
    writer: Arc<tokio::sync::Mutex<W>>,
) -> Result<W> {
    while tasks.join_next().await.is_some() {}
    // Unreachable given the drain above — every clone lived in a task that has now ended. Loud
    // rather than silent: a leaked clone would mean an upgrade sharing a writer with something
    // still writing frames into it.
    Arc::try_unwrap(writer)
        .map(tokio::sync::Mutex::into_inner)
        .map_err(|_| {
            anyhow::anyhow!("control writer still shared after draining in-flight requests")
        })
}

/// Drive a live event stream over a subscribed control connection. Mirrors
/// [`open_session`](crate::daemon::open_session)'s upgrade: it consumes the write half for the
/// subscription's lifetime. Sends the initial [`Snapshot`](crate::stream::StreamFrame::Snapshot)
/// FIRST, then forwards TWO independent taps until the client disconnects:
/// every broadcast [`AuditRecord`](crate::audit::AuditRecord) as an
/// [`Event`](crate::stream::StreamFrame::Event), and every reachability transition as a
/// [`Reachability`](crate::stream::StreamFrame::Reachability) frame (#58).
///
/// They are merged HERE rather than at the source: the audit broadcast is the same call that
/// appends to the on-disk log, so routing probe results through it would either write them into the
/// audit file or force splitting record-from-broadcast.
///
/// Backpressure (spec): a subscriber that falls behind EITHER ring surfaces as
/// `RecvError::Lagged(n)` → one [`Lagged`](crate::stream::StreamFrame::Lagged) frame, then the loop
/// CONTINUES (the subscriber is never dropped on lag). NOTE the consequence for liveness: a
/// dropped reachability transition is never re-asserted, so a consumer that ignores `Lagged` can
/// hold a stale online/offline indicator indefinitely — the documented advice is to reconnect for a
/// fresh snapshot. A failed `write_frame` (the client is gone) → clean return. No lock is held
/// across either `recv().await`.
///
/// The stream lives as long as EITHER tap does. A closed tap is dropped and the other continues;
/// only when both are gone does the loop return. So a mesh daemon with auditing DISABLED still
/// pushes reachability — auditing and liveness are independent signals (#58). A control-only
/// daemon has neither tap, and gets the snapshot alone, exactly as before.
async fn run_subscription(
    state: &Arc<DaemonState>,
    mut w: impl tokio::io::AsyncWrite + Unpin,
) -> Result<()> {
    use crate::stream::StreamFrame;
    // The audit sink is the telemetry hub; the mesh (if any) feeds the reachability snapshot.
    let (audit, mesh) = match state.mesh() {
        Some(mesh) => (mesh.audit(), Some(mesh)),
        None => (crate::audit::AuditSink::disabled(), None),
    };
    // Register the live receiver BEFORE snapshotting. If we snapshotted first, any record
    // broadcast in the gap between `active_sessions()` and `subscribe()` would be LOST — absent from
    // the snapshot (captured earlier) AND from the stream (receiver not yet registered), so a
    // consumer could see a `session_close` for a session it never saw open. Subscribing first turns
    // that race into an at-most-idempotent DOUBLE (a session may appear both in `active_sessions`
    // and as a live `session_open`), which a state-projecting consumer absorbs harmlessly.
    let rx = audit.subscribe();
    // The reachability ring (#58) is registered BEFORE the snapshot for the same reason the audit
    // one is: a transition landing in the gap would otherwise be absent from both.
    let mut reach_rx = mesh.map(|m| m.reach_bcast.subscribe());
    // The self-network ring (#90), registered before the snapshot for the same gap-loss reason.
    let mut self_rx = mesh.map(|m| m.self_net_bcast.subscribe());
    // The app-blob transfer ring (#82 ask 2), registered before the snapshot for the same
    // gap-loss reason as the other two.
    let mut blob_rx = mesh.map(|m| m.blob_bcast.subscribe());
    // The suspend/resume ring (#167 ask 2), registered before the snapshot for the same
    // gap-loss reason as the other three.
    let mut resume_rx = mesh.map(|m| m.resume_bcast.subscribe());
    let snapshot = StreamFrame::Snapshot {
        active_sessions: audit.active_sessions(),
        reachability: mesh.map(crate::daemon::reachability_of).unwrap_or_default(),
        // #90: live-computed, so a fresh subscriber renders posture without a status poll —
        // and without waiting for the watcher to observe a change.
        self_network: mesh.map(|m| {
            let stamp = *m
                .self_net_change
                .lock()
                .expect("self_net_change lock not poisoned");
            crate::daemon::self_network_now(m, stamp)
        }),
    };
    write_frame(&mut w, &serde_json::to_value(&snapshot)?).await?;

    // The stream lives as long as EITHER tap does. A disabled audit sink used to end the stream
    // outright; since #58 a mesh still pushes reachability transitions, so auditing being off must
    // not also switch liveness off — they are independent signals. Only when NEITHER tap exists
    // (a control-only daemon with no mesh) does the snapshot stand alone.
    let mut rx = rx;
    use tokio::sync::broadcast::error::RecvError;

    /// Map an audit-ring result to a frame; `None` (with `closed` set) means this tap is finished.
    fn audit_frame(
        r: Result<crate::audit::AuditRecord, RecvError>,
        closed: &mut bool,
    ) -> Option<StreamFrame> {
        match r {
            Ok(record) => Some(StreamFrame::Event {
                record: Box::new(record),
            }),
            Err(RecvError::Lagged(n)) => Some(StreamFrame::Lagged { dropped: n }),
            Err(RecvError::Closed) => {
                *closed = true;
                None
            }
        }
    }

    /// The app-blob transfer-ring equivalent (#82 ask 2). The producer coalesces, so a `Lagged`
    /// here means a genuinely slow consumer rather than a large transfer.
    fn blob_frame(
        r: Result<crate::daemon::BlobTransfer, RecvError>,
        closed: &mut bool,
    ) -> Option<StreamFrame> {
        match r {
            Ok(t) => Some(StreamFrame::BlobTransfer {
                direction: t.direction,
                hash: t.hash,
                bytes_done: t.bytes_done,
                bytes_total: t.bytes_total,
                state: t.state,
                peer: t.peer,
            }),
            Err(RecvError::Lagged(n)) => Some(StreamFrame::Lagged { dropped: n }),
            Err(RecvError::Closed) => {
                *closed = true;
                None
            }
        }
    }

    /// The reachability-ring equivalent (#58).
    fn reach_frame(
        r: Result<crate::daemon::ReachTransition, RecvError>,
        closed: &mut bool,
    ) -> Option<StreamFrame> {
        match r {
            // #150: the producer rides the ring, stamped by whichever sender observed the
            // transition — this mapping never has to guess.
            Ok(t) => Some(StreamFrame::Reachability {
                peer: t.peer,
                source: t.source,
            }),
            Err(RecvError::Lagged(n)) => Some(StreamFrame::Lagged { dropped: n }),
            Err(RecvError::Closed) => {
                *closed = true;
                None
            }
        }
    }

    /// The self-network-ring equivalent (#90).
    fn self_net_frame(
        r: Result<mcpmesh_local_api::SelfNetwork, RecvError>,
        closed: &mut bool,
    ) -> Option<StreamFrame> {
        match r {
            Ok(self_network) => Some(StreamFrame::SelfNetwork { self_network }),
            Err(RecvError::Lagged(n)) => Some(StreamFrame::Lagged { dropped: n }),
            Err(RecvError::Closed) => {
                *closed = true;
                None
            }
        }
    }

    /// The suspend/resume-ring equivalent (#167 ask 2).
    fn resume_frame(
        r: Result<crate::daemon::ResumeEvent, RecvError>,
        closed: &mut bool,
    ) -> Option<StreamFrame> {
        match r {
            Ok(e) => Some(StreamFrame::Resumed {
                suspended_secs: e.suspended_secs,
                at_epoch: e.at_epoch,
            }),
            Err(RecvError::Lagged(n)) => Some(StreamFrame::Lagged { dropped: n }),
            Err(RecvError::Closed) => {
                *closed = true;
                None
            }
        }
    }

    /// Await the next value from an optional tap; an absent tap pends forever, so `select!`
    /// simply never picks it. Replaced the per-combination match when the third ring arrived
    /// (#90) — eight arms was where that shape stopped scaling.
    async fn tap<T: Clone>(
        rx: &mut Option<tokio::sync::broadcast::Receiver<T>>,
    ) -> Result<T, RecvError> {
        match rx {
            Some(rx) => rx.recv().await,
            None => std::future::pending().await,
        }
    }

    let (mut closed_audit, mut closed_reach, mut closed_self) = (false, false, false);
    let (mut closed_blob, mut closed_resume) = (false, false);
    loop {
        // Five independent rings — audit records, peer-reachability transitions (#58),
        // self-network transitions (#90), blob transfers (#82) and suspend/resume (#167) — merged
        // here rather than at the source, so the audit broadcast (which is the same call that
        // appends to the on-disk log) keeps its schema untouched. Lag on ANY ring reports the same
        // `Lagged` frame and never drops the subscriber.
        if rx.is_none()
            && reach_rx.is_none()
            && self_rx.is_none()
            && blob_rx.is_none()
            && resume_rx.is_none()
        {
            return Ok(());
        }
        let frame = tokio::select! {
            r = tap(&mut rx) => audit_frame(r, &mut closed_audit),
            r = tap(&mut reach_rx) => reach_frame(r, &mut closed_reach),
            r = tap(&mut self_rx) => self_net_frame(r, &mut closed_self),
            r = tap(&mut blob_rx) => blob_frame(r, &mut closed_blob),
            r = tap(&mut resume_rx) => resume_frame(r, &mut closed_resume),
        };
        // A tap whose sender is gone is dropped rather than ending the stream — the OTHERS may
        // still be healthy. When all are gone the check at the loop top returns.
        if closed_audit {
            rx = None;
            closed_audit = false;
        }
        if closed_reach {
            reach_rx = None;
            closed_reach = false;
        }
        if closed_blob {
            blob_rx = None;
            closed_blob = false;
        }
        if closed_self {
            self_rx = None;
            closed_self = false;
        }
        if closed_resume {
            resume_rx = None;
            closed_resume = false;
        }
        let Some(frame) = frame else { continue };
        if write_frame(&mut w, &serde_json::to_value(&frame)?)
            .await
            .is_err()
        {
            return Ok(()); // client gone
        }
    }
}

/// Dispatch one request, handling the async control methods (`register_service`, `peer_add`)
/// that touch the config file / redb store, and delegating the parameterless synchronous
/// methods (`status`, `shutdown`) to [`dispatch`]. Params are deserialized per-method into
/// the local-api param structs via [`with_params`] (never `from_value::<Request>` on the
/// whole message).
pub(crate) async fn handle_request(req: &Value, state: &DaemonState) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    match method_of(req) {
        Some("register_service") => respond(
            id,
            "register_service",
            with_params(&params, |p| crate::daemon::register_service(state, p))
                .await
                .map(unit),
        ),
        Some("peer_add") => respond(
            id,
            "peer_add",
            with_params(&params, |p| crate::daemon::add_peer(state, p))
                .await
                .map(unit),
        ),
        // #65: install a peer from a SIGNED endorsement by someone already paired. Identity only —
        // it grants nothing, which is what bounds a reduced-ceremony trust path.
        Some("peer_introduce") => respond(
            id,
            "peer_introduce",
            with_params(&params, |p| crate::daemon::introduce_peer(state, p))
                .await
                .map(unit),
        ),
        // #65: PRODUCE an endorsement for someone else to redeem. The other half of an
        // introduction — without it nothing can generate `evidence`.
        Some("peer_endorse") => respond(
            id,
            "peer_endorse",
            with_params(&params, |p| crate::daemon::endorse_peer(state, p)).await,
        ),
        // Unpair a peer: the nickname to drop.
        // `remove_peer` revokes the peer's service authorization AND drops its identity row
        // (the inverse of the pairing grant) — see its fail-safe ordering.
        Some("peer_remove") => respond(
            id,
            "peer_remove",
            with_params(&params, |p| crate::daemon::remove_peer(state, p))
                .await
                .map(unit),
        ),
        // Rename a contact's nickname (Contacts rename): the person's `user_id` (or a
        // provisional `nickname`) + the new nickname `to`. `rename_peer` guards the collision
        // (no inheriting another identity's grants), rewrites allow lists, and reloads.
        // #85 ask 3 — device attestation. `attest_offer` says where another of this person's
        // devices should dial; `attest_to` runs the ceremony from the device being admitted.
        // #93 ask c — rotate the org root, publishing a bridge members adopt as they receive it.
        Some("org_rotate") => respond(
            id,
            "org_rotate",
            with_params(&params, |p: mcpmesh_local_api::OrgRotateParams| {
                crate::daemon::org_rotate(state, p.new_key_path)
            })
            .await,
        ),
        Some("attest_offer") => {
            respond(id, "attest_offer", crate::daemon::attest_offer(state).await)
        }
        Some("attest_to") => respond(
            id,
            "attest_to",
            with_params(&params, |p| crate::daemon::attest_to(state, p)).await,
        ),
        // #85 ask 4 — pairing-mode revocation. `peer_revoke` is an operator's LOCAL decision about
        // someone else's device; `device_revoke` signs a portable statement about one of THIS
        // person's own. Deliberately distinct from `peer_remove`: removal is routine and
        // re-pairable, revocation is a compromise claim that outlives the pair row.
        Some("peer_revoke") => respond(
            id,
            "peer_revoke",
            with_params(&params, |p| crate::daemon::peer_revoke(state, p)).await,
        ),
        Some("peer_unrevoke") => respond(
            id,
            "peer_unrevoke",
            with_params(&params, |p| crate::daemon::peer_unrevoke(state, p)).await,
        ),
        Some("device_revoke") => respond(
            id,
            "device_revoke",
            with_params(&params, |p| crate::daemon::device_revoke(state, p)).await,
        ),
        Some("device_revocation_import") => respond(
            id,
            "device_revocation_import",
            with_params(&params, |p| {
                crate::daemon::device_revocation_import(state, p)
            })
            .await,
        ),
        Some("peer_rename") => respond(
            id,
            "peer_rename",
            with_params(&params, |p| crate::daemon::rename_peer(state, p))
                .await
                .map(unit),
        ),
        Some("invite") => {
            // Mint a pairing invite granting a service list ([`InviteParams`]
            // tolerates an absent list).
            let mesh = match state.mesh_required() {
                Ok(mesh) => mesh,
                Err(e) => return error(id, -32000, e.to_string()),
            };
            respond(
                id,
                "invite",
                with_params(&params, |p: InviteParams| {
                    crate::daemon::mint_invite(
                        p.services,
                        p.app_label,
                        p.max_uses,
                        p.peer_nickname,
                        p.as_self,
                        mesh,
                    )
                })
                .await,
            )
        }
        // Redeem a pairing invite: the copyable `mcpmesh-invite:` line
        // ([`PairParams`] tolerates an absent field — an empty line simply fails to decode
        // → a clean pair error).
        Some("pair") => respond(
            id,
            "pair",
            with_params(&params, |p: PairParams| {
                // #178: the flag is translated into the ceremony vocabulary HERE, once. The
                // handler and the redeemer take `SelfEnroll`, not a bool, so no site downstream
                // can pass the wrong polarity of a nameless `true`.
                let self_enroll = if p.allow_self_enroll {
                    crate::pairing::SelfEnroll::Allow
                } else {
                    crate::pairing::SelfEnroll::Refuse
                };
                crate::daemon::redeem(state, p.invite_line, p.as_nickname, self_enroll)
            })
            .await,
        ),
        // Install a signed roster from a local file: the file `path`
        // (a local file the same-uid daemon reads) and an OPTIONAL `org_root_pk`
        // that pins the org root on first install. `install_roster` validates (rules 1–6),
        // persists, hot-swaps the gate, and severs revoked sessions.
        // #93: the READ half of roster mode — the declared groups plus every person the installed
        // roster carries, online or not. Parameterless and read-only; `status` answers who is
        // REACHABLE, which is a different question and omits anyone whose devices are all down.
        Some("roster_members") => respond(
            id,
            "roster_members",
            async {
                let mesh = state.mesh_required()?;
                Ok(crate::daemon::roster_members(mesh))
            }
            .await,
        ),
        // #66: the AUTHORING verbs. Roster mode's operator side used to be CLI-only, so an
        // embedded node could consume a roster and never author one — no "approve this person"
        // button without shelling out to a second binary. The porcelain now calls these too, so
        // both front doors run ONE implementation.
        Some("org_create") => respond(
            id,
            "org_create",
            with_params(&params, |p: OrgCreateParams| {
                crate::daemon::org_create(state, p.name, p.expires_secs, p.roster_url)
            })
            .await,
        ),
        Some("org_approve") => respond(
            id,
            "org_approve",
            with_params(&params, |p: OrgApproveParams| {
                crate::daemon::org_approve(state, p.join_code, p.groups, p.user_id)
            })
            .await,
        ),
        // #85 ask 2: the recovery phrase. The response carries a PRIVATE KEY — never audited,
        // never logged, and it exists nowhere else on this surface.
        Some("user_key_export") => respond(
            id,
            "user_key_export",
            crate::daemon::user_key_export(state).await,
        ),
        Some("user_key_import") => respond(
            id,
            "user_key_import",
            with_params(&params, |p: UserKeyImportParams| {
                crate::daemon::user_key_import(state, p.recovery_phrase, p.replace)
            })
            .await,
        ),
        // #214: leave an identity this device was ENROLLED into — the exit `adopt_hook` lacked.
        Some("self_enroll_detach") => respond(
            id,
            "self_enroll_detach",
            crate::daemon::self_enroll_detach(state).await,
        ),
        // Read-only: decode + verify a join code and return the fingerprint the operator confirms
        // OUT-OF-BAND before approving. Nothing is signed, installed, or persisted.
        Some("org_join_code") => respond(
            id,
            "org_join_code",
            with_params(&params, |p: OrgJoinCodeParams| {
                crate::daemon::org_join_code(state, p.join_code)
            })
            .await,
        ),
        Some("org_revoke") => respond(
            id,
            "org_revoke",
            with_params(&params, |p: OrgRevokeParams| {
                crate::daemon::org_revoke(state, p.target, p.user_key)
            })
            .await,
        ),
        Some("roster_install") => respond(
            id,
            "roster_install",
            with_params(&params, |p: RosterInstallParams| {
                crate::daemon::install_roster(state, p.path, p.org_root_pk)
            })
            .await,
        ),
        // Pin the org root on a JOINER without a roster. `user_key` is
        // a LOCAL path (the key never crosses the API). `org_join` validates the pk BEFORE
        // writing, then surgically pins the four `[identity]` keys under `reload_lock`. No
        // roster is installed.
        Some("org_join") => respond(
            id,
            "org_join",
            with_params(&params, |p: OrgJoinParams| {
                crate::daemon::org_join(state, p.org_id, p.org_root_pk, p.user_id, p.user_key)
            })
            .await,
        ),
        // Pin the HTTPS roster URL (`[roster].url`) in config. Written by
        // `org create --roster-url` (operator) and by `join` when the org invite carries one
        // (the joiner's FIRST-roster bootstrap). `set_roster_url` writes it atomically
        // under `reload_lock` (single-writer).
        Some("set_roster_url") => respond(
            id,
            "set_roster_url",
            with_params(&params, |p: SetRosterUrlParams| {
                crate::daemon::set_roster_url(state, p.url)
            })
            .await
            .map(unit),
        ),
        // Rename this node LIVE (#37): validated + persisted under `reload_lock`, then the
        // in-memory name updates — future invites present it immediately, no restart.
        Some("set_nickname") => respond(
            id,
            "set_nickname",
            with_params(&params, |p: SetNicknameParams| {
                crate::daemon::set_nickname(state, p.nickname)
            })
            .await
            .map(unit),
        ),
        // Set this node's CUSTOM relay set LIVE (#53): validated + diffed against the running
        // endpoint and applied via iroh insert_relay/remove_relay (custom→custom), then persisted
        // under `reload_lock`. Answers a SetRelaysResult { changed, restart_required }.
        Some("set_relays") => respond(
            id,
            "set_relays",
            with_params(&params, |p: SetRelaysParams| {
                crate::daemon::set_relays(state, p.relay_urls)
            })
            .await,
        ),
        // Peer service discovery (#52): dial the peer, return the services it grants us.
        Some("peer_services") => {
            let p: PeerServicesParams = match params_of(&params) {
                Ok(p) => p,
                Err(e) => return error(id, -32602, format!("peer_services: {e}")),
            };
            respond(
                id,
                "peer_services",
                crate::daemon::peer_services(state, p.peer).await,
            )
        }
        // #140: dump the durable per-peer state — a DIAGNOSTIC surface that deliberately carries
        // transport vocabulary, because "what address is this node about to dial" cannot be
        // answered without the address.
        Some("peer_diagnostics") => {
            let p: mcpmesh_local_api::PeerDiagnosticsParams = match params_of(&params) {
                Ok(p) => p,
                Err(e) => return error(id, -32602, format!("peer_diagnostics: {e}")),
            };
            respond(
                id,
                "peer_diagnostics",
                crate::daemon::peer_diagnostics(state, &p.peer).await,
            )
        }
        // #140: forget a peer's stored dial hint. An experiment tool — see the verb's rustdoc.
        Some("peer_hint_clear") => {
            let p: mcpmesh_local_api::PeerHintClearParams = match params_of(&params) {
                Ok(p) => p,
                Err(e) => return error(id, -32602, format!("peer_hint_clear: {e}")),
            };
            respond(
                id,
                "peer_hint_clear",
                crate::daemon::peer_hint_clear(state, &p.peer).await,
            )
        }
        // Deregistration (#50): remove a service registration, mirror of register_service.
        Some("unregister_service") => respond(
            id,
            "unregister_service",
            with_params(&params, |p: UnregisterServiceParams| {
                crate::daemon::unregister_service(state, p.name)
            })
            .await
            .map(unit),
        ),
        // Per-peer access toggle (#44): grant/revoke a single principal on a single service's
        // allow WITHOUT unpairing, under the SAME reload_lock as the pairing grant.
        Some("service_allow_grant") => respond(
            id,
            "service_allow_grant",
            with_params(&params, |p: ServiceAllowParams| {
                crate::daemon::service_allow_grant(state, p.service, p.principal)
            })
            .await
            .map(unit),
        ),
        Some("service_allow_revoke") => respond(
            id,
            "service_allow_revoke",
            with_params(&params, |p: ServiceAllowParams| {
                crate::daemon::service_allow_revoke(state, p.service, p.principal)
            })
            .await
            .map(unit),
        ),
        // Set this node's app-metadata blob (#39): stored in memory, folded signed into
        // future presence heartbeats — no config write, no reload.
        Some("set_app_metadata") => respond(
            id,
            "set_app_metadata",
            with_params(&params, |p: SetAppMetadataParams| {
                crate::daemon::set_app_metadata(state, p.metadata)
            })
            .await
            .map(unit),
        ),
        Some("blob_publish") => respond(
            id,
            "blob_publish",
            with_params(&params, |p: BlobPublishParams| {
                crate::daemon::blob_publish(state, p.scope, p.path)
            })
            .await,
        ),
        Some("blob_grant") => respond(
            id,
            "blob_grant",
            with_params(&params, |p: BlobGrantParams| {
                crate::daemon::blob_grant(state, p.scope, p.principal)
            })
            .await
            .map(unit),
        ),
        // #62: per-scope withdrawal — un-sharing a file without unpairing the person.
        Some("blob_revoke") => respond(
            id,
            "blob_revoke",
            with_params(&params, |p: BlobRevokeParams| {
                crate::daemon::blob_revoke(state, p.scope, p.principals)
            })
            .await
            .map(unit),
        ),
        Some("blob_unpublish") => respond(
            id,
            "blob_unpublish",
            with_params(&params, |p: BlobUnpublishParams| {
                crate::daemon::blob_unpublish(state, p.scope, p.hash)
            })
            .await
            .map(unit),
        ),
        Some("blob_republish") => respond(
            id,
            "blob_republish",
            with_params(&params, |p: BlobRepublishParams| {
                crate::daemon::blob_republish(state, p.scope, p.hash)
            })
            .await,
        ),
        Some("blob_list") => respond(
            id,
            "blob_list",
            with_params(&params, |p: mcpmesh_local_api::BlobListParams| {
                crate::daemon::blob_list(state, p)
            })
            .await,
        ),
        Some("blob_fetch") => respond(
            id,
            "blob_fetch",
            with_params(&params, |p: BlobFetchParams| {
                crate::daemon::blob_fetch(state, p.ticket, p.dest_path, p.from)
            })
            .await,
        ),
        Some("blob_fetch_cancel") => respond(
            id,
            "blob_fetch_cancel",
            with_params(&params, |p: BlobFetchCancelParams| async move {
                crate::daemon::blob_fetch_cancel(state, &p.hash)
            })
            .await,
        ),
        Some("audit_summary") => {
            // Summarize THIS node's LOCAL audit log: read the daemon's OWN
            // audit dir off the runtime (spawn_blocking — the fs house rule) and aggregate to
            // per-peer / per-service session counts. Never touches the network; params are ignored
            // (parameterless, like `status`). Works in control-only mode (an empty/absent dir → an
            // empty summary). The dir is THE one this node's audit writer was spawned over
            // (per-node — an embedded node roots it under its own root dir); the env default
            // remains only for the mesh-less control-only mode, which has no writer to ask.
            let sink_dir = state
                .mesh
                .as_ref()
                .and_then(|m| m.audit().dir().map(std::path::Path::to_path_buf));
            match tokio::task::spawn_blocking(move || {
                let dir = match sink_dir {
                    Some(d) => d,
                    None => mcpmesh_trust::paths::default_audit_dir()?,
                };
                crate::audit::read_all_records(&dir)
                    .map(|recs| crate::audit::summarize_sessions(&recs))
            })
            .await
            {
                Ok(r) => respond(id, "audit_summary", r.map_err(anyhow::Error::from)),
                Err(e) => error(id, -32000, format!("audit_summary task failed: {e}")),
            }
        }
        Some("audit_prune") => {
            // #88: delete audit months strictly older than `before`. The month shape is
            // validated FIRST — `prune_before` string-compares, so a malformed key would
            // otherwise report a clean empty prune instead of the loud error a typo deserves.
            // Destructive but owner-only: the control socket is the daemon owner's.
            //
            // FAIL-CLOSED dir resolution, unlike audit_summary's env-default fallback (#88
            // gate): this verb DELETES. A control-only state, or a mesh whose sink was never
            // installed, has no writer-owned dir — falling back to the env default there would
            // let a hermetic test or embedder silently delete the real user's audit history.
            // A read verb answering the env default is a quirk; a delete doing it is a footgun.
            let sink_dir = state
                .mesh
                .as_ref()
                .and_then(|m| m.audit().dir().map(std::path::Path::to_path_buf));
            let r = with_params(&params, move |p: AuditPruneParams| async move {
                anyhow::ensure!(
                    crate::audit::valid_month_key(&p.before),
                    "before must be a zero-padded YYYY-MM month key, got '{}'",
                    p.before
                );
                let dir = sink_dir.context(
                    "audit_prune requires a daemon with a live audit writer — refusing to \
                     guess a directory for a destructive operation",
                )?;
                tokio::task::spawn_blocking(move || crate::audit::prune_before(&dir, &p.before))
                    .await
                    .map_err(|e| anyhow::anyhow!("audit_prune task failed: {e}"))?
                    .map(|deleted_months| mcpmesh_local_api::AuditPruneResult { deleted_months })
                    .map_err(anyhow::Error::from)
            })
            .await;
            respond(id, "audit_prune", r)
        }
        Some("audit_list") => {
            // #88: the filtered, paged read — "show me everything you hold about me". The kind
            // string is parsed BEFORE the fs work: an unknown kind errors rather than silently
            // matching everything, which would make that answer overclaim. The limit clamp is
            // load-bearing (one JSON response frame; blob_list's minor-20 lesson).
            let sink_dir = state
                .mesh
                .as_ref()
                .and_then(|m| m.audit().dir().map(std::path::Path::to_path_buf));
            let r = with_params(&params, move |p: AuditListParams| async move {
                // Month bounds validated like `audit_prune.before` (#88 gate): a non-padded
                // typo (`"2026-7"`) lexicographically excludes EVERY month, so the verb would
                // answer "nothing is held about X" on a typo — the silent-underclaim this
                // dispatch already makes a loud error for `kind`.
                for (name, bound) in [("since", &p.since), ("until", &p.until)] {
                    if let Some(m) = bound {
                        anyhow::ensure!(
                            crate::audit::valid_month_key(m),
                            "{name} must be a zero-padded YYYY-MM month key, got '{m}'"
                        );
                    }
                }
                let kind = match &p.kind {
                    Some(s) => Some(crate::audit::parse_kind(s).ok_or_else(|| {
                        anyhow::anyhow!(
                            "unknown kind '{s}' — one of session_open, session_close, request, \
                             blob_fetch, trust"
                        )
                    })?),
                    None => None,
                };
                let limit = p.limit.unwrap_or(500).min(1000) as usize;
                let offset = p.offset.unwrap_or(0) as usize;
                tokio::task::spawn_blocking(move || {
                    let dir = match sink_dir {
                        Some(d) => d,
                        None => mcpmesh_trust::paths::default_audit_dir()?,
                    };
                    crate::audit::list_page(
                        &dir,
                        p.since.as_deref(),
                        p.until.as_deref(),
                        kind,
                        p.peer.as_deref(),
                        limit,
                        offset,
                    )
                })
                .await
                .map_err(|e| anyhow::anyhow!("audit_list task failed: {e}"))?
                .map_err(anyhow::Error::from)
            })
            .await;
            respond(id, "audit_list", r)
        }
        // TEST-ONLY (#172). Concurrency, the in-flight cap, and abort-on-close can only be asserted
        // against a verb whose latency the test CONTROLS; the real one is `blob_fetch`, which a
        // control-only fixture cannot perform. Compiled out of every non-test build, so it is
        // unreachable on any shipped daemon.
        // TEST-ONLY (#172): the only way to reach the panic path without an actual bug.
        #[cfg(test)]
        Some("__test_panic") => panic!("deliberate test panic"),
        #[cfg(test)]
        Some("__test_block") => {
            let gate = params.get("gate").and_then(|v| v.as_u64()).unwrap_or(0);
            tests::test_block(gate).await;
            ok(id, json!({}))
        }
        _ => dispatch(req, state),
    }
}

/// A params-deserialization failure, distinguished from a handler failure so [`respond`] can map
/// it to the JSON-RPC standard `-32602` "invalid params" (a param typo / unknown field / bad shape
/// is the caller's error), while a handler failure stays `-32000`. Carried through the shared
/// `anyhow::Result` surface and recovered by downcast.
#[derive(Debug)]
pub(crate) struct InvalidParams(pub(crate) String);
impl std::fmt::Display for InvalidParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for InvalidParams {}

/// Fold one control call's `Result` into the JSON-RPC response frame — the boilerplate every
/// dispatch arm shared: `Ok(v)` → `{"result": v}` (a `()`-returning verb maps itself to `json!({})`
/// via [`unit`] first), a params error → `-32602`, any other `Err(e)` → `-32000`, both with the
/// `"{method} failed: {e}"` message shape every arm used.
fn respond<T: serde::Serialize>(id: Value, method: &str, r: anyhow::Result<T>) -> Value {
    match r {
        Ok(v) => ok(
            id,
            serde_json::to_value(v).expect("control result serializes"),
        ),
        Err(e) if e.downcast_ref::<InvalidParams>().is_some() => {
            error(id, -32602, format!("{method} failed: {e}"))
        }
        // #55: "no such service" is BRANCHABLE, not a generic failure — a caller distinguishing
        // "register it first" from "the daemon broke" cannot parse `-32000` messages reliably.
        // #83: a missing BLOB gets its own code — "fetch it first" is a different remedy from
        // "that scope does not exist", and a client should not have to parse messages to tell them
        // apart. Checked BEFORE the shared arm below.
        Err(e) if e.downcast_ref::<crate::daemon::BlobWithdrawn>().is_some() => error(
            id,
            mcpmesh_local_api::ERR_BLOB_WITHDRAWN,
            format!("{method} failed: {e}"),
        ),
        Err(e) if e.downcast_ref::<crate::daemon::NoSuchBlob>().is_some() => error(
            id,
            mcpmesh_local_api::ERR_NO_SUCH_BLOB,
            format!("{method} failed: {e}"),
        ),
        // #172: a request the CALLER stopped is not a failure. Its own code so a UI can close a
        // progress bar quietly instead of raising an error for something its user asked for.
        Err(e) if e.downcast_ref::<crate::daemon::Cancelled>().is_some() => error(
            id,
            mcpmesh_local_api::ERR_CANCELLED,
            format!("{method} failed: {e}"),
        ),
        // #159: an onboarding refusal that carries its own code. ONE arm for the whole family —
        // expired line, no live invite, inviter unreachable, id mismatch, name conflict, and the
        // deliberately-opaque refusal — so adding the next condition is a constant plus a call
        // site rather than another arm here. Checked BEFORE the generic fallthrough.
        Err(e)
            if e.downcast_ref::<crate::pairing::rendezvous::PairRefusal>()
                .is_some() =>
        {
            let refusal = e
                .downcast_ref::<crate::pairing::rendezvous::PairRefusal>()
                .expect("checked by the guard");
            error(id, refusal.code(), format!("{method} failed: {e}"))
        }
        // #147: the ONE `pair` refusal with a self-service remedy — rename and redeem the same
        // invite again. It gets a code so a GUI embedder writes its own recovery copy naming its
        // own rename affordance, instead of substring-matching prose that is generated on the
        // INVITER's side and cannot be rewritten downstream. Every other refusal stays `-32000`.
        Err(e)
            if e.downcast_ref::<crate::pairing::rendezvous::NicknameTaken>()
                .is_some() =>
        {
            error(
                id,
                mcpmesh_local_api::ERR_NICKNAME_TAKEN,
                format!("{method} failed: {e}"),
            )
        }
        Err(e)
            if e.downcast_ref::<crate::daemon::NoSuchService>().is_some()
                || e.downcast_ref::<crate::daemon::NoSuchBlobScope>().is_some() =>
        {
            error(
                id,
                mcpmesh_local_api::ERR_NO_SUCH_SERVICE,
                format!("{method} failed: {e}"),
            )
        }
        Err(e)
            if e.downcast_ref::<crate::daemon::PrincipalRevoked>()
                .is_some() =>
        {
            error(
                id,
                mcpmesh_local_api::ERR_PRINCIPAL_REVOKED,
                format!("{method} failed: {e}"),
            )
        }
        Err(e) => error(id, -32000, format!("{method} failed: {e}")),
    }
}

/// The `status.revoked` rows the installed ROSTER contributes (#223): its `revoked_endpoints`
/// (`source: "roster"`), and each active roster device whose roster `user_id` is a revoked `b64u:`
/// identity (`source: "roster_identity"`, the identity in `reason`). The same two roster clauses
/// `dial_refused` applies, read from the same view. Empty without a roster.
fn roster_revocations(
    mesh: &crate::daemon::MeshState,
    rows: &[crate::allowlist::PeerEntry],
) -> Vec<mcpmesh_local_api::RevokedEndpoint> {
    let Some(view) = mesh.roster.view() else {
        return Vec::new();
    };
    let nickname = |id: &[u8; 32]| {
        rows.iter()
            .find(|e| &e.endpoint_id == id)
            .map(|e| e.nickname.clone())
    };
    let mut out: Vec<mcpmesh_local_api::RevokedEndpoint> = view
        .revoked_endpoints()
        .map(|id| mcpmesh_local_api::RevokedEndpoint {
            principal: mcpmesh_net::EndpointId::from_bytes(*id).principal(),
            revoked_at_epoch: 0,
            source: "roster".into(),
            signer_user_id: None,
            reason: None,
            nickname: nickname(id),
        })
        .collect();
    for id in view.device_endpoints() {
        let Some(d) = view.resolve(id) else { continue };
        if !mesh.store.is_roster_user_revoked(&d.user_id) {
            continue;
        }
        let revoked_at_epoch = mesh
            .store
            .list_revoked_users()
            .unwrap_or_default()
            .into_iter()
            .find(|(uid, _)| uid == &d.user_id)
            .map(|(_, r)| r.revoked_at)
            .unwrap_or(0);
        out.push(mcpmesh_local_api::RevokedEndpoint {
            principal: mcpmesh_net::EndpointId::from_bytes(*id).principal(),
            revoked_at_epoch,
            source: "roster_identity".into(),
            signer_user_id: None,
            reason: Some(format!("device of revoked identity {}", d.user_id)),
            nickname: nickname(id),
        });
    }
    out
}

/// Map a `()`-returning control verb's success to the empty-object result the wire always carried
/// (`serde_json::to_value(())` would yield `null`, not `{}`).
fn unit((): ()) -> Value {
    json!({})
}

/// Dispatch one control request on its `method` string (never `from_value::<Request>` on
/// the whole message — that rejects `params:{}` for parameterless methods). Returns a
/// JSON-RPC-shaped response frame. Params are read per-method; the methods dispatched here
/// are parameterless, so `status` ignores whatever `params` shape the client sent (omitted
/// / null / `{}` all answered) — the tolerance a third-party client depends on.
fn dispatch(req: &Value, state: &DaemonState) -> Value {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    match method_of(req) {
        Some("status") => respond(id, "status", status_result(state)),
        Some("shutdown") => ok(id, json!({})),
        Some(other) => error(id, -32601, format!("unknown method: {other}")),
        None => error(id, -32600, "request is missing a `method`"),
    }
}

pub(crate) fn status_result(state: &DaemonState) -> Result<StatusResult> {
    // Services + peers are read LIVE from the mesh's config + store (like `roster`/`presence`
    // below) — there is no cached snapshot: the pairing grant (grant_service_access) and the
    // rendezvous PeerEntry write land durably without touching `DaemonState`, so only a live
    // read shows a just-paired peer / its grant immediately. A live read is always-current and
    // cheap (status is a human-invoked command). An unreadable config is an explicit ERROR —
    // never silently-stale data. A control-only daemon (no mesh → no config/store to read)
    // answers empty lists.
    // The config is loaded ONCE per status call and shared by the live service list AND
    // `roster_status` (which reads only the pinned org anchor from it) — the host polls status,
    // so a load-twice would be a real per-poll cost.
    let (services, peers, roster) = match state.mesh() {
        Some(mesh) => {
            let cfg = crate::config::Config::load(&mesh.config_path).map_err(|e| {
                anyhow::anyhow!("config unreadable at {}: {e}", mesh.config_path.display())
            })?;
            // Roster status is computed LIVE from `mesh.roster.view()` (never a cached snapshot —
            // the gate view is already hot-swapped on install; a live read is cheap + always-
            // current). A pure-pairing daemon (no mesh, or an empty roster gate) yields None → no
            // roster block.
            let roster = crate::daemon::roster_status(mesh, Some(&cfg));
            // #100: `status` no longer needs the ephemeral map — the live registry carries the
            // `ephemeral` flag per entry, so this whole-map clone on every status call is gone.
            {
                // One store read serves both the peer list and the allow-display
                // annotation (fails open on corrupt rows, like `peer_infos`). The #212
                // revocation filter inside `service_infos` is the exception: it fails CLOSED
                // per entry, hiding the entry when the store cannot be read — the gate would
                // refuse that principal too.
                let entries = mesh.store.list().unwrap_or_default();
                (
                    crate::daemon::service_infos(mesh, &entries),
                    crate::daemon::peer_infos(&mesh.store),
                    roster,
                )
            }
        }
        None => (Vec::new(), Vec::new(), None),
    };
    // Advisory presence read: the installed roster's devices joined with the live
    // presence table (online = a non-expired heartbeat). ADVISORY — a display convenience; a device
    // with no heartbeat is `online: false` yet still a dial candidate. Empty (→ omitted) without a
    // roster. Surface-clean: flat vocabulary only (user_id/device_label/role/online).
    let presence = state
        .mesh()
        .map(crate::daemon::presence_peers)
        .unwrap_or_default();
    // This daemon's own self-sovereign user_id (`b64u:<user_pk>`), read from its precomputed
    // self-binding (auto-minted at boot; shared by pairing + roster mode). `None` in a control-only
    // daemon or when no user key exists.
    let self_user_id = state
        .mesh()
        .and_then(|mesh| mesh.self_binding())
        .map(|binding| binding.user_pk);
    // #214: does THIS device hold the key behind that id? An enrolled device presents an ADOPTED
    // binding (#86) and holds nothing, so `peer_endorse` / `device_revoke` / `invite {as_self}`
    // all refuse on it and `self_enroll_detach` applies only to it — none of which the id alone
    // reveals. `false` with no id at all: there is no key to hold. Advisory; every verb re-checks.
    let self_user_key_held = self_user_id.is_some()
        && state.mesh().is_some_and(|mesh| {
            mesh.adopted_binding
                .read()
                .expect("adopted_binding lock not poisoned")
                .is_none()
        });
    // Recent inviter-side pairing completions (display-only ceremony aids, newest first):
    // a snapshot of the mesh's in-memory ring. Empty in a control-only daemon and after a
    // restart (in-memory by design — not trust data).
    let recent_pairings = state
        .mesh()
        .map(|mesh| mesh.recent_pairings())
        .unwrap_or_default();
    // Advisory reachability of paired peers, from the on-demand probe cache (spec: pairing-mode
    // liveness). Mirrors the `presence` read above: cached values returned immediately, with any
    // stale/missing entry refreshed on a background probe `reachability_of` spawns — status stays a
    // non-blocking hot path. Surface-clean: nicknames + numbers only.
    let reachability = state
        .mesh()
        .map(crate::daemon::reachability_of)
        .unwrap_or_default();
    // #88: the on-disk footprint, computed LIVE per call (the test pins that a new audit month
    // moves the number on the next status). Inline fs stats/walks, matching this fn's existing
    // inline redb reads (`store.list()` above); the audit dir holds at most a handful of month
    // files and the blob walk is bounded by the store's own contents. `None` without a mesh.
    let storage = state.mesh().map(|mesh| {
        let audit_bytes = mesh
            .audit()
            .dir()
            .and_then(|d| crate::audit::list_month_files(d).ok())
            .map(|files| files.iter().map(|(_, _, size)| size).sum())
            .unwrap_or(0);
        let redb_bytes = std::fs::metadata(mesh.store.path())
            .map(|m| m.len())
            .unwrap_or(0);
        let blobs_bytes = mesh
            .blobs_dir()
            .map(crate::util::dir_size_bytes)
            .unwrap_or(0);
        // #80: `Some` only when collection is actually running — an unset cell means the operator
        // did not configure it (or configured it unparseably, which leaves it off), and that is a
        // different answer from "running, no sweep yet".
        let blobs_gc = mesh.blobs_gc.get().map(|(interval_secs, stats)| {
            use std::sync::atomic::Ordering;
            let last = stats.last_run_epoch.load(Ordering::Relaxed);
            mcpmesh_local_api::BlobsGcInfo {
                interval_secs: *interval_secs,
                runs: stats.runs.load(Ordering::Relaxed),
                // 0 is the never-ran sentinel, not a 1970 timestamp.
                last_run_epoch: (last != 0).then_some(last),
                last_protected: stats.last_protected.load(Ordering::Relaxed),
                aborted: stats.aborted.load(Ordering::Relaxed),
            }
        });
        mcpmesh_local_api::StorageInfo {
            audit_bytes,
            redb_bytes,
            blobs_bytes,
            blobs_gc,
        }
    });
    // #90: THIS node's own reachability posture — live point reads off the endpoint's stable
    // watcher surface, merged with the boot watcher's last-change stamp. None without a mesh.
    let self_network = state.mesh().map(|mesh| {
        let stamp = *mesh
            .self_net_change
            .lock()
            .expect("self_net_change lock not poisoned");
        crate::daemon::self_network_now(mesh, stamp)
    });
    // #85 ask 4: the revocation list, rendered with the local nickname where one survives —
    // revocation never deletes the pair row, so it usually does, and a list of bare `eid:` hex is
    // unreadable for the operator who has to act on it. `None` without a mesh.
    let revoked: Vec<mcpmesh_local_api::RevokedEndpoint> = state
        .mesh()
        .map(|mesh| {
            let rows = mesh.store.list().unwrap_or_default();
            mesh.store
                .list_revoked()
                .unwrap_or_default()
                .into_iter()
                .map(|r| mcpmesh_local_api::RevokedEndpoint {
                    principal: mcpmesh_net::EndpointId::from_bytes(r.endpoint_id).principal(),
                    revoked_at_epoch: r.revoked_at,
                    source: r.source,
                    signer_user_id: r.signer_user_id,
                    reason: r.reason,
                    nickname: rows
                        .iter()
                        .find(|e| e.endpoint_id == r.endpoint_id)
                        .map(|e| e.nickname.clone()),
                })
                // …plus revoked IDENTITIES (#85 ask 3 gate). These have no endpoint of their own —
                // they refuse every device of a person, including ones this node has never seen —
                // so they are rendered with the `b64u:` as the principal. Without them `status`
                // would show an operator who revoked a PERSON nothing but the devices that
                // happened to be known at the time, which is the misunderstanding that made
                // endpoint-only revocation look sufficient.
                .chain(
                    mesh.store
                        .list_revoked_users()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|(uid, r)| mcpmesh_local_api::RevokedEndpoint {
                            principal: uid,
                            revoked_at_epoch: r.revoked_at,
                            source: r.source,
                            signer_user_id: r.signer_user_id,
                            reason: r.reason,
                            nickname: None,
                        }),
                )
                // …plus what the ROSTER makes this node refuse (#223 review). Every device listed
                // here is one `dial_refused` refuses, so its reachability row is never refreshed
                // again; leaving the roster half out made that row look merely stale.
                .chain(roster_revocations(mesh, &rows))
                .fold(
                    Vec::new(),
                    |mut acc: Vec<mcpmesh_local_api::RevokedEndpoint>, r| {
                        // A device revoked by this node AND by the roster is one row: the local one,
                        // which came first and carries the operator's own reason.
                        if !acc.iter().any(|x| x.principal == r.principal) {
                            acc.push(r);
                        }
                        acc
                    },
                )
        })
        .unwrap_or_default();
    Ok(StatusResult {
        revoked,
        stack_version: state.stack_version.clone(),
        services,
        peers,
        roster,
        presence,
        self_user_id,
        self_user_key_held,
        recent_pairings,
        reachability,
        // The EFFECTIVE self-nickname (live — reflects a `set_nickname` immediately);
        // empty in mesh-less control-only mode, which the additive field skips.
        self_nickname: state
            .mesh()
            .map(|mesh| mesh.self_nickname())
            .unwrap_or_default(),
        storage,
        self_network,
    })
}

/// Deserialize a request's `params` into a method's typed param struct — the local-api wire
/// truth (`protocol.rs`), so param-shape drift between the daemon and its clients is a compile
/// error, not silent divergence. Omitted/`null` params read as `{}`, preserving the leniency
/// for methods whose params are all defaultable (`invite`, `pair`, `open_session`).
fn params_of<T: serde::de::DeserializeOwned>(params: &Value) -> anyhow::Result<T> {
    let v = match params {
        Value::Null => json!({}),
        p => p.clone(),
    };
    serde_json::from_value(v)
        .map_err(|e| anyhow::Error::new(InvalidParams(format!("invalid params: {e}"))))
}

/// The shared parse-then-handle shape of every param-carrying dispatch arm: deserialize
/// `params` into the method's typed struct ([`params_of`]) and run the handler on it. A parse
/// failure folds into the same anyhow error surface as a handler failure (→ `-32000` via
/// [`respond`]).
async fn with_params<P, R, F>(params: &Value, f: impl FnOnce(P) -> F) -> anyhow::Result<R>
where
    P: serde::de::DeserializeOwned,
    F: Future<Output = anyhow::Result<R>>,
{
    f(params_of(params)?).await
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control_only() -> Arc<DaemonState> {
        Arc::new(DaemonState::new("0.1.0-test"))
    }
    fn req(method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
    }

    /// #159: each onboarding refusal reaches the embedder as its OWN code.
    ///
    /// The point of the issue: `ERR_NICKNAME_TAKEN` was the only coded pairing failure, so an
    /// embedder either forwarded our prose verbatim to end users or substring-matched it. These
    /// codes let it decide per case.
    #[test]
    fn each_onboarding_refusal_carries_its_own_code() {
        use crate::pairing::rendezvous::PairRefusal;
        let coded = |code: i64| -> Value {
            let e: anyhow::Result<()> = Err(anyhow::Error::new(PairRefusal::new(code, "why")));
            respond(json!(1), "pair", e)
        };
        for code in [
            mcpmesh_local_api::ERR_INVITE_EXPIRED,
            mcpmesh_local_api::ERR_INVITE_NOT_LIVE,
            mcpmesh_local_api::ERR_INVITER_UNREACHABLE,
            mcpmesh_local_api::ERR_INVITER_MISMATCH,
            mcpmesh_local_api::ERR_INVITE_NAME_CONFLICT,
            mcpmesh_local_api::ERR_INVITE_REFUSED,
        ] {
            let v = coded(code);
            assert_eq!(
                v["error"]["code"], code,
                "each condition keeps its own code: {v}"
            );
        }

        // They are all DISTINCT — collapsing any two would silently merge two different remedies.
        let all = [
            mcpmesh_local_api::ERR_INVITE_EXPIRED,
            mcpmesh_local_api::ERR_INVITE_NOT_LIVE,
            mcpmesh_local_api::ERR_INVITER_UNREACHABLE,
            mcpmesh_local_api::ERR_INVITER_MISMATCH,
            mcpmesh_local_api::ERR_INVITE_NAME_CONFLICT,
            mcpmesh_local_api::ERR_INVITE_REFUSED,
            mcpmesh_local_api::ERR_NICKNAME_TAKEN,
        ];
        let unique: std::collections::BTreeSet<i64> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len(), "codes must not collide: {all:?}");

        // `ERR_INVITER_MISMATCH` must never read as "retry me", which means it must not equal any
        // of the recoverable codes. The first version of this assertion filtered the mismatch code
        // out and then asked whether anything left equalled it — always false, so it passed for
        // any input including all-codes-identical (#159 gate). This compares it against the
        // recoverable set directly.
        let recoverable = [
            mcpmesh_local_api::ERR_INVITE_EXPIRED,
            mcpmesh_local_api::ERR_INVITE_NOT_LIVE,
            mcpmesh_local_api::ERR_INVITER_UNREACHABLE,
            mcpmesh_local_api::ERR_INVITE_NAME_CONFLICT,
            mcpmesh_local_api::ERR_INVITE_REFUSED,
            mcpmesh_local_api::ERR_NICKNAME_TAKEN,
        ];
        assert!(
            !recoverable.contains(&mcpmesh_local_api::ERR_INVITER_MISMATCH),
            "the address-swap refusal must not share a code with any recoverable one — an app \
             that renders every pairing failure as a friendly retry would paper over exactly the \
             attack that check exists to catch"
        );

        // And an unrelated failure still answers -32000, so the codes stay meaningful.
        let plain: anyhow::Result<()> = Err(anyhow::anyhow!("something else"));
        assert_eq!(respond(json!(1), "pair", plain)["error"]["code"], -32000);
    }

    /// #147: a nickname-collision refusal reaches the embedder as `ERR_NICKNAME_TAKEN`, and
    /// nothing else does.
    ///
    /// This pins the DOWNCAST ARM, which is the whole embedder-visible contract — the point of the
    /// issue is that a consumer branches on this number instead of substring-matching prose it
    /// cannot rewrite. The gate caught that deleting the arm outright broke no test: the unit
    /// coverage exercised the `refusal_error` helper, never the call site that turns its output
    /// into a code. A tested helper nobody calls is not coverage.
    #[test]
    fn a_nickname_collision_answers_err_nickname_taken() {
        let reason = "pairing refused: nickname 'studio-mac' is already taken by another paired \
                      peer; the invite was NOT consumed — rename this node and redeem the same \
                      invite again";
        let typed: anyhow::Result<()> = Err(anyhow::Error::new(
            crate::pairing::rendezvous::NicknameTaken(reason.into()),
        ));
        let v = respond(json!(1), "pair", typed);
        assert_eq!(
            v["error"]["code"],
            mcpmesh_local_api::ERR_NICKNAME_TAKEN,
            "the collision must be branchable, not -32000: {v}"
        );
        assert_eq!(
            v["error"]["code"], -32043,
            "the value is the wire contract: {v}"
        );
        let msg = v["error"]["message"].as_str().unwrap_or_default();
        assert!(
            msg.contains("rename this node") && !msg.contains("set_nickname"),
            "the message carries the inviter's reworded prose verbatim: {v}"
        );

        // A NON-pairing failure stays generic. (Since #159 the other pairing refusals carry their
        // own codes rather than -32000 — see `each_onboarding_refusal_carries_its_own_code`; what
        // this pins is that an untyped error is not silently absorbed into one of them.)
        let generic: anyhow::Result<()> = Err(anyhow::anyhow!("pairing refused: pairing refused"));
        let v = respond(json!(1), "pair", generic);
        assert_eq!(v["error"]["code"], -32000, "got {v}");
    }

    /// The transport-agnostic serve body speaks full mcpmesh-local/1 over a plain duplex —
    /// what an embedded node's `Node::control` runs (no socket, no peer gate: the pipe
    /// never leaves the process). Proves hello + a typed request round-trip end to end
    /// against the REAL `connect_control_io` client.
    #[tokio::test]
    async fn serve_control_io_speaks_the_protocol_over_a_duplex() {
        let state = control_only();
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let (sr, sw) = tokio::io::split(server_io);
        tokio::spawn(serve_control_io(sr, sw, state));
        let (cr, cw) = tokio::io::split(client_io);
        let mut client = mcpmesh_local_api::connect_control_io(cr, cw)
            .await
            .expect("hello handshake");
        assert_eq!(client.hello().stack_version, "0.1.0-test");
        let status = client.status().await.expect("status");
        assert_eq!(status.stack_version, "0.1.0-test");
        assert!(status.services.is_empty());
    }

    /// `status` on a control-only daemon answers version + empty service/peer lists and no
    /// roster/presence block. Exercises `status_result`'s no-mesh branch.
    #[test]
    fn dispatch_status_answers_empty_lists_without_a_mesh() {
        let st = control_only();
        let r = dispatch(&req("status", json!({})), &st);
        assert_eq!(r["result"]["stack_version"], "0.1.0-test");
        assert!(r["result"]["peers"].as_array().unwrap().is_empty());
        assert!(r["result"]["services"].as_array().unwrap().is_empty());
        assert!(r["result"]["roster"].is_null());
    }

    /// `status` tolerates whatever `params` shape a third-party client sends (omitted / null / {}) —
    /// the parameterless-method leniency the spec guarantees.
    #[test]
    fn dispatch_status_tolerates_any_params_shape() {
        let st = control_only();
        for p in [json!({}), Value::Null, json!({ "junk": true })] {
            assert!(dispatch(&req("status", p), &st).get("result").is_some());
        }
        // Params omitted entirely.
        let omitted = json!({ "jsonrpc": "2.0", "id": 1, "method": "status" });
        assert!(dispatch(&omitted, &st).get("result").is_some());
    }

    #[test]
    fn dispatch_shutdown_acks_and_unknown_methods_error() {
        let st = control_only();
        assert_eq!(
            dispatch(&req("shutdown", json!({})), &st)["result"],
            json!({})
        );
        // An unimplemented method → -32601.
        assert_eq!(
            dispatch(&req("frobnicate", json!({})), &st)["error"]["code"],
            -32601
        );
        // A request missing a `method` → -32600.
        let no_method = json!({ "jsonrpc": "2.0", "id": 1 });
        assert_eq!(dispatch(&no_method, &st)["error"]["code"], -32600);
    }

    /// Every mesh-requiring control method fails GRACEFULLY (a -32000 error, never a panic) in
    /// control-only mode — the per-method error arms of `handle_request`.
    #[tokio::test]
    async fn mesh_methods_error_gracefully_without_a_mesh() {
        let st = control_only();
        for method in [
            "register_service",
            "peer_add",
            // #65: both halves of an introduction. Without these the dispatch arms could be
            // DELETED with a green suite — the verbs would answer -32601 over the wire.
            "peer_introduce",
            "peer_endorse",
            "peer_remove",
            "peer_rename",
            "invite",
            "pair",
            "roster_install",
            "org_join",
            "set_roster_url",
            "blob_publish",
            "blob_grant",
            "blob_list",
            "blob_fetch",
            // #85 asks 3/4 + #93 ask c. Every one of these dispatch arms could be DELETED with a
            // green suite until they were listed here — the 0.45.0 gate proved it for the
            // revocation four, and the 0.47.0 gate proved it again for `org_rotate`. The verbs
            // would answer -32601 over the wire and every embedder's client call would fail.
            "peer_revoke",
            "peer_unrevoke",
            "device_revoke",
            "device_revocation_import",
            "attest_offer",
            "attest_to",
            "org_rotate",
            // #214: the self-enrollment exit. Parameterless, so it reaches the handler and must
            // report the missing mesh rather than answer -32601.
            "self_enroll_detach",
        ] {
            let r = handle_request(&req(method, json!({})), &st).await;
            // Graceful error, never a panic or a success. With empty params, a method whose
            // params carry required fields is rejected at parse (-32602, #34); a method whose
            // params are all defaultable reaches the handler and reports the missing mesh
            // (-32000). Both are clean errors — assert it's one of the two and never a result.
            let code = r["error"]["code"].as_i64();
            assert!(
                matches!(code, Some(-32000) | Some(-32602)),
                "method {method} should error gracefully in control-only mode, got {r}"
            );
            assert!(
                r.get("result").is_none(),
                "method {method} must not succeed: {r}"
            );
        }
    }

    /// A param-carrying method with a malformed `params` shape answers a `-32000` error whose
    /// message carries the invalid-params detail — the typed per-method deserialization into the
    /// local-api param structs (never a panic, and the connection-level envelope stays lenient).
    #[tokio::test]
    async fn malformed_params_answer_an_invalid_params_error() {
        let st = control_only();
        // Wrong field type (nickname must be a string) → the JSON-RPC standard -32602
        // "invalid params" (#34: params shape errors are the caller's error, distinct from a
        // handler failure's -32000).
        let r = handle_request(&req("peer_remove", json!({ "nickname": 42 })), &st).await;
        assert_eq!(r["error"]["code"], -32602);
        assert!(
            r["error"]["message"]
                .as_str()
                .unwrap()
                .contains("invalid params"),
            "message names the params problem: {r}"
        );
        // Missing required field → also -32602.
        let r = handle_request(&req("peer_rename", json!({ "user_id": "u" })), &st).await;
        assert_eq!(r["error"]["code"], -32602);
        // An unknown field is now rejected too (deny_unknown_fields), not silently ignored.
        let r = handle_request(
            &req("peer_remove", json!({ "nickname": "a", "extra": true })),
            &st,
        )
        .await;
        assert_eq!(
            r["error"]["code"], -32602,
            "unknown params field is rejected: {r}"
        );
    }

    /// `audit_summary` works WITHOUT a mesh (a local-only read; an empty/absent audit dir yields an
    /// empty summary) — the one non-parameterless method answerable in control-only mode.
    #[tokio::test]
    async fn audit_summary_works_in_control_only_mode() {
        let st = control_only();
        let r = handle_request(&req("audit_summary", json!({})), &st).await;
        assert!(
            r.get("result").is_some(),
            "audit_summary should succeed: {r}"
        );
    }

    /// `handle_request` delegates the parameterless synchronous methods to `dispatch`.
    #[tokio::test]
    async fn handle_request_delegates_status_to_dispatch() {
        let st = control_only();
        let r = handle_request(&req("status", json!({})), &st).await;
        assert_eq!(r["result"]["stack_version"], "0.1.0-test");
    }

    // ---- #172: concurrent dispatch ----------------------------------------------------------

    /// A verb whose latency the TEST controls, standing in for `blob_fetch`. Registered per test so
    /// tests sharing this process never gate on each other's token.
    pub(super) struct Gate {
        released: crate::cancel::CancelToken,
        entered: std::sync::atomic::AtomicUsize,
        completed: std::sync::atomic::AtomicUsize,
    }

    static GATES: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<u64, std::sync::Arc<Gate>>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    static NEXT_GATE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    fn new_gate() -> (u64, std::sync::Arc<Gate>) {
        let id = NEXT_GATE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let gate = std::sync::Arc::new(Gate {
            released: crate::cancel::CancelToken::new(),
            entered: std::sync::atomic::AtomicUsize::new(0),
            completed: std::sync::atomic::AtomicUsize::new(0),
        });
        GATES.lock().unwrap().insert(id, gate.clone());
        (id, gate)
    }

    /// The body of the `__test_block` verb: park until the owning test releases the gate.
    ///
    /// `completed` is incremented AFTER the wait, so it distinguishes "ran to completion" from
    /// "was aborted mid-flight" — which is exactly what the close-aborts-in-flight test asserts.
    pub(super) async fn test_block(id: u64) {
        let gate = GATES
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .expect("__test_block names a registered gate");
        gate.entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        gate.released.cancelled().await;
        gate.completed
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn load(c: &std::sync::atomic::AtomicUsize) -> usize {
        c.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Wait for a condition, BOUNDED, sleeping rather than spinning.
    ///
    /// Both properties are load-bearing under a loaded CI box. A `yield_now` spin busy-waits a
    /// whole worker thread, which on a small runner running the suite in parallel starves the very
    /// task it is waiting for; and an unbounded wait turns a broken assumption into a job that
    /// hangs until the runner's own timeout kills it with no failing test named. One did.
    async fn until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !cond() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for: {what}"
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    type ClientReader =
        FrameReader<tokio::io::BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>;
    type ClientWriter = tokio::io::WriteHalf<tokio::io::DuplexStream>;

    /// A raw framed control connection — NOT `ControlClient`, which is one request at a time by
    /// construction (`&mut self`) and therefore cannot express pipelining at all.
    async fn raw_conn(
        state: Arc<DaemonState>,
    ) -> (
        ClientReader,
        ClientWriter,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        let (client_io, server_io) = tokio::io::duplex(256 * 1024);
        let (sr, sw) = tokio::io::split(server_io);
        let server = tokio::spawn(serve_control_io(sr, sw, state));
        let (cr, cw) = tokio::io::split(client_io);
        let mut reader = FrameReader::new(tokio::io::BufReader::new(cr), MAX_FRAME_BYTES);
        let hello = next_frame(&mut reader).await;
        assert_eq!(hello["api"], API_NAME, "server speaks first with a Hello");
        (reader, cw, server)
    }

    /// Read one frame, BOUNDED. The bound is load-bearing for the mutation checks: several of the
    /// mutations these tests exist to catch (awaiting a permit instead of refusing over the cap,
    /// skipping the upgrade drain) manifest as a frame that never arrives, and an unbounded read
    /// would hang the suite instead of failing it.
    async fn next_frame(r: &mut ClientReader) -> Value {
        let read = tokio::time::timeout(Duration::from_secs(10), r.next())
            .await
            .expect("a frame should arrive within 10s");
        match read.expect("read a frame") {
            Some(Inbound::Frame(v)) => v,
            other => panic!("expected a frame, got {other:?}"),
        }
    }

    async fn send(w: &mut ClientWriter, v: &Value) {
        write_frame(w, v).await.expect("write a request frame");
    }

    fn blocking_req(id: u64, gate: u64) -> Value {
        json!({ "id": id, "method": "__test_block", "params": { "gate": gate } })
    }

    /// THE point of #172: a long request no longer stalls the ones behind it. A `status` issued
    /// AFTER a blocked request answers FIRST, and the blocked one answers once released.
    ///
    /// Mutation anchor: awaiting a permit (or awaiting the handler inline) makes the first frame
    /// read here carry id 1 and the assertion fail.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_slow_request_does_not_stall_the_ones_behind_it() {
        let (gate_id, gate) = new_gate();
        let (mut r, mut w, _server) = raw_conn(control_only()).await;
        send(&mut w, &blocking_req(1, gate_id)).await;
        send(&mut w, &json!({ "id": 2, "method": "status" })).await;

        let first = next_frame(&mut r).await;
        assert_eq!(
            first["id"], 2,
            "the fast request must answer first: {first}"
        );
        assert!(
            first.get("result").is_some(),
            "status should succeed: {first}"
        );
        assert_eq!(
            load(&gate.completed),
            0,
            "the slow request is still running"
        );

        gate.released.cancel();
        let second = next_frame(&mut r).await;
        assert_eq!(
            second["id"], 1,
            "the released request answers second: {second}"
        );
    }

    /// Over the per-connection cap the daemon refuses IMMEDIATELY with a branchable, retryable
    /// code — it does not queue, and it does not stop reading. Every accepted request still
    /// answers once released.
    ///
    /// Mutation anchor: `acquire().await` in place of `try_acquire_owned` never produces the
    /// refusal and this test hangs.
    #[tokio::test(flavor = "multi_thread")]
    async fn over_the_inflight_cap_a_request_is_refused_not_queued() {
        let (gate_id, gate) = new_gate();
        let (mut r, mut w, _server) = raw_conn(control_only()).await;
        let cap = mcpmesh_local_api::MAX_INFLIGHT as u64;
        for id in 1..=cap {
            send(&mut w, &blocking_req(id, gate_id)).await;
        }
        // One past the cap.
        send(&mut w, &blocking_req(cap + 1, gate_id)).await;

        let refusal = next_frame(&mut r).await;
        assert_eq!(
            refusal["id"],
            cap + 1,
            "the overflow request is the one refused"
        );
        assert_eq!(
            refusal["error"]["code"],
            mcpmesh_local_api::ERR_TOO_MANY_INFLIGHT,
            "over the cap must be branchable, not -32000: {refusal}"
        );

        gate.released.cancel();
        let mut answered = std::collections::HashSet::new();
        for _ in 0..cap {
            let f = next_frame(&mut r).await;
            answered.insert(f["id"].as_u64().expect("id is a number"));
        }
        assert_eq!(
            answered,
            (1..=cap).collect::<std::collections::HashSet<_>>(),
            "every accepted request answers"
        );

        // The connection is usable again once permits free.
        send(&mut w, &json!({ "id": 999, "method": "status" })).await;
        let after = next_frame(&mut r).await;
        assert_eq!(after["id"], 999);
        assert!(
            after.get("result").is_some(),
            "usable after the cap clears: {after}"
        );
    }

    /// An upgrade DRAINS first: the pending response lands before the subscription's snapshot, so
    /// no response frame can interleave with what the upgraded connection writes.
    ///
    /// Mutation anchor: skipping the drain in `reclaim_writer` (taking the writer straight back)
    /// makes the snapshot arrive first — or, with a guard instead of `try_unwrap`, deadlocks.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_upgrade_waits_for_in_flight_responses_before_it_takes_the_writer() {
        let (gate_id, gate) = new_gate();
        let (mut r, mut w, _server) = raw_conn(control_only()).await;
        send(&mut w, &blocking_req(1, gate_id)).await;
        send(&mut w, &json!({ "method": "subscribe" })).await;
        // Give the read loop time to reach the upgrade and start draining, so the ordering this
        // asserts is the drain's and not merely the release's.
        tokio::time::sleep(Duration::from_millis(50)).await;
        gate.released.cancel();

        let first = next_frame(&mut r).await;
        assert_eq!(
            first["id"], 1,
            "the in-flight response lands first: {first}"
        );
        let snapshot = next_frame(&mut r).await;
        assert!(
            snapshot.get("id").is_none() && snapshot["type"] == "snapshot",
            "the subscription snapshot follows it: {snapshot}"
        );
    }

    /// `open_session` still reclaims the writer by value and answers over it — the OTHER upgrade
    /// path through `reclaim_writer` (here on the control-only branch, which synthesizes an
    /// unreachable answer). Pipelined behind an in-flight request, so it exercises the drain and
    /// not merely the reclaim.
    #[tokio::test(flavor = "multi_thread")]
    async fn open_session_drains_in_flight_requests_before_it_takes_the_writer() {
        let (gate_id, gate) = new_gate();
        let (mut r, mut w, _server) = raw_conn(control_only()).await;
        send(&mut w, &blocking_req(7, gate_id)).await;
        send(
            &mut w,
            &json!({ "id": 8, "method": "open_session", "params": { "peer": "bob", "service": "kb" } }),
        )
        .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        gate.released.cancel();

        let first = next_frame(&mut r).await;
        assert_eq!(
            first["id"], 7,
            "the in-flight response lands first: {first}"
        );
        let second = next_frame(&mut r).await;
        assert!(
            second.get("error").is_some(),
            "a mesh-less daemon answers open_session with an error frame: {second}"
        );
    }

    fn ephemeral_register(id: u64, name: &str) -> Value {
        json!({
            "id": id,
            "method": "register_service",
            "params": {
                "name": name,
                "backend": { "socket": { "path": "/run/nowhere.sock" } },
                "allow": [],
                "ephemeral": true,
            }
        })
    }

    /// `bulk` pads `config.toml` with N persistent services. `register_service` inserts into
    /// `mesh.ephemeral_services` and THEN awaits a config reload; padding widens that reload so a
    /// client closing the socket reliably lands INSIDE the window, instead of the test passing by
    /// racing past it.
    async fn mesh_state(
        bulk: usize,
    ) -> (
        tempfile::TempDir,
        Arc<crate::daemon::MeshState>,
        Arc<DaemonState>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let mut cfg = String::new();
        for i in 0..bulk {
            cfg.push_str(&format!(
                "[services.pad{i}]\nsocket = \"/run/pad{i}.sock\"\nallow = []\n"
            ));
        }
        std::fs::write(&config_path, cfg).unwrap();
        let mesh = crate::daemon::testutil::hermetic_mesh(config_path).await;
        let state = Arc::new(DaemonState::with_mesh("test", mesh.clone()));
        (dir, mesh, state)
    }

    fn ephemeral_names(mesh: &Arc<crate::daemon::MeshState>) -> Vec<String> {
        let mut v: Vec<String> = mesh
            .ephemeral_services
            .lock()
            .expect("ephemeral_services lock not poisoned")
            .keys()
            .cloned()
            .collect();
        v.sort();
        v
    }

    /// #36's invariant under #172's abort: an ephemeral registration is torn down even when the
    /// connection closes MID-REGISTER.
    ///
    /// `register_service` inserts into `mesh.ephemeral_services` and then awaits a config reload.
    /// That window did not exist while the handler was awaited inline; once it runs in an abortable
    /// task, recording the teardown entry AFTER the handler returned left the registration live
    /// with an empty teardown list — an orphan service pointing at a dead backend, and a name
    /// `register_service` then refused forever.
    ///
    /// Mutation anchor: recording `pending_ephemeral` inside the spawned task (after
    /// `handle_request`) instead of before the spawn fails this.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_ephemeral_registration_is_torn_down_even_if_the_connection_closes_mid_register() {
        let (_dir, mesh, state) = mesh_state(300).await;
        let (r, mut w, server) = raw_conn(state).await;
        send(&mut w, &ephemeral_register(1, "leaky")).await;
        // Close once the registration EXISTS but before the reload behind it has finished — the
        // window itself, not merely "soon after sending". Closing earlier would abort the handler
        // before it registered anything, which leaks nothing and proves nothing.
        until("the ephemeral registration to appear", || {
            !ephemeral_names(&mesh).is_empty()
        })
        .await;
        // Close WITHOUT reading the ack — the whole point.
        drop(w);
        drop(r);
        server
            .await
            .expect("connection task not panicked")
            .expect("connection ends cleanly");
        assert!(
            ephemeral_names(&mesh).is_empty(),
            "a registration must not outlive the connection that made it, however it ended: {:?}",
            ephemeral_names(&mesh)
        );
    }

    /// The other half of recording the teardown up front: a connection whose register was REFUSED
    /// must not tear down that name on its way out — it belongs to whoever actually holds it.
    ///
    /// Mutation anchor: dropping the "remove it again if the register failed" branch makes the
    /// second connection's close unregister the FIRST connection's live service.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_register_does_not_tear_down_a_name_another_connection_holds() {
        let (_dir, mesh, state) = mesh_state(0).await;
        let (mut r1, mut w1, _holder) = raw_conn(state.clone()).await;
        send(&mut w1, &ephemeral_register(1, "contested")).await;
        let ack = next_frame(&mut r1).await;
        assert!(
            ack.get("result").is_some(),
            "the first register wins: {ack}"
        );
        assert_eq!(ephemeral_names(&mesh), vec!["contested".to_string()]);

        // A register that names the same service but is REFUSED — `rate_limit_per_min: 0` is
        // rejected rather than silently blocking every request (#63).
        let (mut r2, mut w2, loser) = raw_conn(state).await;
        let mut bad = ephemeral_register(2, "contested");
        bad["params"]["rate_limit_per_min"] = json!(0);
        send(&mut w2, &bad).await;
        let refused = next_frame(&mut r2).await;
        assert!(
            refused.get("error").is_some(),
            "the second register loses: {refused}"
        );
        drop(w2);
        drop(r2);
        loser
            .await
            .expect("connection task not panicked")
            .expect("connection ends cleanly");

        assert_eq!(
            ephemeral_names(&mesh),
            vec!["contested".to_string()],
            "the holder's service must survive the loser closing"
        );
    }

    /// A PANICKING handler answers `-32603` instead of hanging its caller (#172 gate).
    ///
    /// Before dispatch was concurrent a panic unwound the connection task and the client saw EOF at
    /// once. In a `JoinSet` it would be swallowed: the ordinary one-request-at-a-time client would
    /// wait forever on a response frame that can never arrive, with the connection still open.
    ///
    /// Mutation anchor: removing the `catch_unwind` makes this test time out on its first read.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_panicking_handler_answers_instead_of_hanging_the_caller() {
        let (mut r, mut w, _server) = raw_conn(control_only()).await;
        send(&mut w, &json!({ "id": 5, "method": "__test_panic" })).await;
        let f = next_frame(&mut r).await;
        assert_eq!(f["id"], 5, "the panicking request still answers: {f}");
        assert_eq!(f["error"]["code"], -32603, "as an internal error: {f}");

        // And the connection survives it — the requests behind a panicking one are not collateral.
        send(&mut w, &json!({ "id": 6, "method": "status" })).await;
        let after = next_frame(&mut r).await;
        assert_eq!(after["id"], 6);
        assert!(after.get("result").is_some(), "still usable: {after}");
    }

    /// `respond` maps a [`Cancelled`](crate::daemon::Cancelled) handler error to `ERR_CANCELLED`,
    /// not the generic `-32000` — the whole reason it is a distinct error type.
    ///
    /// Mutation anchor: deleting that arm in `respond` passed the entire suite before this test.
    #[test]
    fn a_cancelled_request_answers_err_cancelled() {
        let r = respond::<()>(
            json!(1),
            "blob_fetch",
            Err(crate::daemon::Cancelled("blake3:beef".into()).into()),
        );
        assert_eq!(
            r["error"]["code"],
            mcpmesh_local_api::ERR_CANCELLED,
            "a cancel is branchable, not a generic failure: {r}"
        );
        assert!(
            r["error"]["message"]
                .as_str()
                .expect("a message")
                .contains("blake3:beef"),
            "and it names what was cancelled: {r}"
        );
    }

    /// Closing the control connection ABORTS its in-flight work (#172). Before this, a `blob_fetch`
    /// ran to completion and only then discovered nobody was listening — which is why an embedder's
    /// Cancel button could not stop the bytes.
    ///
    /// Mutation anchor: detaching the request with `tokio::spawn` instead of the connection-owned
    /// `JoinSet` leaves it running, and `completed` reaches 1.
    #[tokio::test(flavor = "multi_thread")]
    async fn closing_the_connection_aborts_its_in_flight_requests() {
        let (gate_id, gate) = new_gate();
        let (r, mut w, server) = raw_conn(control_only()).await;
        send(&mut w, &blocking_req(1, gate_id)).await;
        // Wait for the handler to actually be running before closing — otherwise this could pass
        // by aborting something that had not started.
        until("the handler to start", || load(&gate.entered) > 0).await;

        drop(w);
        drop(r);
        server
            .await
            .expect("connection task not panicked")
            .expect("connection ends cleanly on client close");

        // Release AFTER the connection is gone: an aborted task can never observe it.
        gate.released.cancel();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(load(&gate.entered), 1, "the handler did start");
        assert_eq!(
            load(&gate.completed),
            0,
            "an in-flight request must be aborted when its connection closes"
        );
    }
}
