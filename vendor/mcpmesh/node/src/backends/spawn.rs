//! The `run` backend: one child MCP server process per session.
//! Its stdio is pumped Value/semantic-faithfully to/from the QUIC-framed transport,
//! the resolved caller identity is injected as environment variables, the child is
//! killed when the session closes, and concurrent spawns are bounded per service.
//!
//! The platform PUMPS, it never INTERPRETS: frames move across as JSON `Value`s
//! (one codec everywhere; the child's stdout MCP-stdio framing IS our wire framing —
//! newline-delimited compact JSON), and nothing here inspects the child's MCP
//! method/result semantics. Fidelity is Value/semantic, not byte-for-byte: each frame
//! re-serializes through `serde_json::Value` (keys re-sorted, no arbitrary_precision),
//! the shared property with the mesh transport — see [`super`].
//!
//! Identity reaches a `run` child as ENV, not `_meta`: `MCPMESH_PEER_EID` (the STABLE `eid:<hex>`
//! device principal — ALWAYS present for a resolved caller, #60), `MCPMESH_PEER_NAME` (the display
//! nickname), `MCPMESH_PEER_USER` (the self-sovereign user_id — set in roster mode, and in pairing
//! mode once a device->user binding is verified), and `MCPMESH_PEER_GROUPS` (comma-joined).
//!
//! Scope per caller on `MCPMESH_PEER_EID` (or `MCPMESH_PEER_USER` when you want person-level rather
//! than device-level), NEVER on `MCPMESH_PEER_NAME`: a nickname collides and changes under a rename,
//! which is the whole argument of #38/#41. It arrives PER-CALLER through `run`
//! (`Option<PeerIdentity>`), not as a construction field: `serve` builds each
//! backend once per service and reuses it across all callers, so the injected
//! identity cannot be baked in. `select_service` strips the caller's reserved
//! `mcpmesh/*` `_meta` keys from the first frame and the pump's sanitizer strips them
//! from every later one (#164), so no frame reaching the child carries a forged one.
//! This backend is given NO `_meta` identity to inject: it carries identity in env
//! vars, and inventing an `_meta` seam would make a `run` server see a key that
//! appears on no other release.
use std::collections::BTreeMap;
use std::process::Stdio;

use anyhow::{Context, Result};
use mcpmesh_net::errors::synthesized_limited;
use mcpmesh_net::transport::NdjsonTransport;
use mcpmesh_net::{PeerIdentity, SessionBackend, SessionTransport};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::audit::RequestAuditor;

/// The `retry_after_ms` hint on a concurrency-cap refusal. Unlike a token bucket there is
/// no exact refill instant — a held permit frees when a peer session ends — so a fixed ~1s nudge.
const CONCURRENCY_RETRY_MS: u64 = 1000;

/// The `run` backend for one registered service. `cmd` is an argv vector executed
/// directly — NEVER through a shell. `concurrency` is the per-service
/// spawn cap (default 4), shared across the sessions of one service. The
/// caller identity is NOT a field — it is threaded per-session through
/// [`SessionBackend::run`] (this backend is shared across callers).
pub struct SpawnBackend {
    pub cmd: Vec<String>,
    /// Per-service env vars (#51), overlaid on the inherited env; `MCPMESH_PEER_*` wins.
    pub env: BTreeMap<String, String>,
    /// Working directory to spawn in (#51); `None` inherits the daemon's cwd.
    pub cwd: Option<String>,
    pub concurrency: std::sync::Arc<Semaphore>,
    /// This service's name (the registry key) — recorded as `service` in audit records.
    pub service: String,
    /// The audit sink. `AuditSink::disabled()` in tests / a non-audited build.
    pub audit: crate::audit::AuditSink,
    /// The request limiter for THIS service (#63): per `(service, endpoint)`, not shared across backends.
    /// Consulted per proxied request line in `super::pump`. Keyed on `identity.endpoint`.
    pub limiter: std::sync::Arc<crate::limits::RateLimiter>,
}

#[async_trait::async_trait]
impl SessionBackend for SpawnBackend {
    /// Drive one mesh session against a freshly spawned child. The concrete
    /// `SessionTransport` is iroh-typed; the spawn + pump body lives in the
    /// transport-generic [`SpawnBackend::run_over`] so it is exercisable over an
    /// in-memory pipe in tests. Both paths run identical logic.
    async fn run(
        &self,
        identity: Option<PeerIdentity>,
        initialize: Value,
        transport: SessionTransport,
    ) -> anyhow::Result<()> {
        self.run_over(identity, initialize, transport).await
    }
}

impl SpawnBackend {
    /// The transport-generic core of [`SessionBackend::run`]: acquire a spawn
    /// permit, launch the child with the identity env, then pump frames both ways
    /// until either side ends. Generic over the transport's byte substrate so the
    /// real path (iroh streams) and the test path (`tokio::io::duplex`) share one
    /// implementation — the concrete `SessionTransport` alias would otherwise
    /// forbid a hermetic in-memory test.
    ///
    /// The concurrency permit is held for the whole session (`_permit` lives until
    /// this future returns). On acquisition failure the backend fully HANDLES the
    /// session: it synthesizes a `-32053` refusal frame on the transport (id echoed
    /// from the `initialize`), flushes it via `shutdown()`, and returns `Ok(())` — a
    /// clean, load-triggered refusal, NOT a session error (mirrors the -32054
    /// `ServiceDecision::Refuse` path, which also returns `Ok` so `serve`'s session
    /// task does not `warn!("session ended with error")` for a normal refusal). The
    /// caller sees a well-formed rate-limit answer, not a hang. `retry_after_ms` is
    /// a token-bucket concept, omitted here.
    pub async fn run_over<R, W>(
        &self,
        identity: Option<PeerIdentity>,
        initialize: Value,
        mut transport: NdjsonTransport<R, W>,
    ) -> Result<()>
    where
        R: AsyncRead + Send + Unpin,
        W: AsyncWrite + Send + Unpin,
    {
        let _permit = match self.concurrency.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                // Cap reached: answer the caller with a well-formed -32053{retry_after_ms} and close
                // cleanly (FAIL-SAFE deny — bounds a spawn-bomb). Best-effort writes; returning Ok
                // keeps this off `serve`'s session-error log.
                let id = initialize.get("id").cloned().unwrap_or(Value::Null);
                let _ = transport
                    .send_value(synthesized_limited(id, CONCURRENCY_RETRY_MS))
                    .await;
                let _ = transport.shutdown().await;
                return Ok(());
            }
        };

        let (program, args) = self.cmd.split_first().context("run backend cmd is empty")?;
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // child diagnostics flow to the daemon's stderr
            .kill_on_drop(true); // session close (this future returning) kills the child
        // #51: per-service env + cwd. The service's env is applied FIRST, but with every
        // `MCPMESH_*` key STRIPPED — those are ours, and a service definition must never be able
        // to set (spoof) the injected identity, including `MCPMESH_PEER_USER` for an UNBOUND
        // caller (which the injection below would otherwise leave untouched). Identity is then
        // injected AUTHORITATIVELY: all three vars are set for a resolved caller, and REMOVED
        // when there is no identity — a service can supply none of them by any path.
        // Case-INSENSITIVE: Windows environment keys compare case-insensitively (std's `EnvKey`
        // uses `CompareStringOrdinal(bIgnoreCase)`), so an exact-case filter would let
        // `Mcpmesh_Home` through to arrive as `MCPMESH_HOME`. The identity vars below are immune
        // either way (the authoritative `env`/`env_remove` wins on the same case-folded key), but
        // `MCPMESH_HOME` — which sandboxes ALL daemon state — is protected by this filter ALONE.
        command.envs(
            self.env
                .iter()
                .filter(|(k, _)| !k.to_ascii_uppercase().starts_with("MCPMESH_")),
        );
        if let Some(cwd) = &self.cwd {
            command.current_dir(cwd);
        }
        match &identity {
            Some(id) => {
                command.env("MCPMESH_PEER_NAME", &id.name);
                match &id.user_id {
                    Some(user) => command.env("MCPMESH_PEER_USER", user),
                    None => command.env_remove("MCPMESH_PEER_USER"),
                };
                command.env("MCPMESH_PEER_GROUPS", id.groups.join(","));
                // #60: the caller's STABLE device principal, `eid:<hex>` — the authenticated
                // endpoint id. UNCONDITIONAL, unlike `MCPMESH_PEER_USER`: it is the one identifier
                // that is always present and can never collide or change under a rename.
                //
                // Without it a `run` server had only the display nickname for an unbound pairing
                // peer, so any per-caller scoping (a filesystem server rooted per person, per-caller
                // state) was keyed on exactly what #38/#41 say must never authorize. The socket
                // backend already carries the full principal set in `_meta["mcpmesh/peer"]`; this
                // closes the same hole for `run`.
                command.env("MCPMESH_PEER_EID", id.endpoint.principal());
            }
            None => {
                command.env_remove("MCPMESH_PEER_NAME");
                command.env_remove("MCPMESH_PEER_USER");
                command.env_remove("MCPMESH_PEER_GROUPS");
                command.env_remove("MCPMESH_PEER_EID");
            }
        }

        let mut child = command
            .spawn()
            .with_context(|| format!("spawn run backend child `{program}`"))?;
        let child_stdin = child.stdin.take().expect("stdin piped above");
        let child_stdout = child.stdout.take().expect("stdout piped above");

        // Session lifecycle audit: attribute to the gate-resolved identity (roster
        // user_id if present, else nickname). The cap-refusal path above returns before this, so a
        // refused session is not recorded as opened.
        let peer = identity
            .as_ref()
            .map(|id| id.user_id.clone().unwrap_or_else(|| id.name.clone()));
        // Session lifecycle via the RAII guard: it emits `session_open` now and, on drop (every exit
        // path — EOF, error, panic), emits `session_close` and removes the live-table row. Held for
        // the whole session scope, so it MUST outlive the pump below.
        let _session = self.audit.session(
            peer.clone().unwrap_or_default(),
            self.service.clone(),
            // #73: the STABLE device principal, so a live-session row is keyed on something
            // that cannot collide. `peer` above is a display name and two devices can share it.
            super::session_principal(identity.as_ref()),
        );
        let auditor = RequestAuditor::new(
            self.audit.clone(),
            peer.clone(),
            self.service.clone(),
            // #57: the same stable principal the session guard stamps — every proxied line of
            // this session joins to policy without nickname matching.
            super::session_principal(identity.as_ref()),
        );

        // Pump the two directions concurrently until either side EOFs/closes (shared
        // with the socket backend — one codec, one pump, no full-duplex deadlock); the
        // child then drops here (kill_on_drop fires) and the permit is released on
        // EVERY exit path.
        let outcome = super::pump(
            initialize,
            &mut transport,
            child_stdout,
            child_stdin,
            auditor,
            crate::limits::RateGate::new(
                self.limiter.clone(),
                identity.as_ref().map(|i| i.endpoint),
            ),
            // #164: the `run` backend conveys identity through `MCPMESH_PEER_*` env vars, set once
            // per spawned process. It has no `_meta` identity seam, so injecting one here would
            // invent a surface this backend does not have — the STRIP still applies to every frame.
            None,
        )
        .await;
        // `_session` drops here (or on any early return above), emitting `session_close`.
        outcome
    }
}
