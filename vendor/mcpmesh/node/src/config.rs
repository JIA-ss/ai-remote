//! The `config.toml` model. Every table and key here is real, implemented surface —
//! docs/config.md is the operator-facing reference for all of it.
use figment::{
    Figment,
    providers::{Format, Toml},
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub identity: IdentityCfg,
    pub network: NetworkCfg,
    pub limits: LimitsCfg,
    /// Roster-mode `[roster]` tunables: the degraded-expiry grace window, the roster URL +
    /// poll interval, and the freshness bound — one `RosterState` machine consumes them all.
    pub roster: RosterCfg,
    /// `[blobs]` tunables. Today: the app-blob garbage-collection interval (#80).
    pub blobs: BlobsCfg,
    /// `[services.<name>]` registry — each entry is a served MCP server plus its allow
    /// list. Peers do NOT live in config; they live in the daemon's state store, so
    /// there is no `[peers]` table here.
    pub services: std::collections::BTreeMap<String, ServiceCfg>,
}

/// A `[services.<name>]` entry: exactly one backend kind (`run` xor `socket`) plus the
/// nicknames/groups admitted to it. The xor is validated at access time via
/// [`ServiceCfg::backend_result`] rather than at parse time, so a malformed entry is a
/// per-service error, not a whole-config load failure.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ServiceCfg {
    /// `run`: spawn this command per session (a stdio MCP server).
    pub run: Option<Vec<String>>,
    /// `socket`: dial this local UDS (an already-running MCP server).
    pub socket: Option<String>,
    /// STABLE principals admitted to this service (b64u:/eid:/roster names, #38 — never display nicknames).
    pub allow: Vec<String>,
    /// Per-service env vars for a `run` backend (#51). The `MCPMESH_PEER_*` identity vars win
    /// over these. Ignored for a `socket` backend. Default empty.
    pub env: BTreeMap<String, String>,
    /// Working directory for a `run` backend (#51). Default: inherit the daemon's cwd.
    pub cwd: Option<String>,
    /// Per-service proxied-request rate (#63), falling back to `[limits].rate_limit_per_min`.
    ///
    /// Before #63 every service a peer could reach drew from ONE shared bucket, so an agent
    /// hammering a browser or filesystem service starved the embedder's own low-rate control
    /// traffic to a different service on the same node. Buckets are now per `(service, endpoint)`.
    ///
    /// **This can only LOWER the rate.** `[limits].rate_limit_per_min` is a hard ceiling; a larger
    /// value here is clamped, not honoured. That is what keeps the limit from being raised by a
    /// config edit or a `register_service` call.
    pub rate_limit_per_min: Option<u32>,
}

/// The resolved backend kind of a [`ServiceCfg`], borrowing the config as slices (no
/// clone). `&[String]`/`&str` rather than `&Vec`/`&String` — idiomatic and gives the
/// daemon's backend builders the most flexible borrow.
#[derive(Debug)]
pub enum Backend<'a> {
    Run(&'a [String]),
    Socket(&'a str),
}

impl ServiceCfg {
    /// Resolve the backend, enforcing exactly-one-of `run`/`socket`. Both or neither is an
    /// error — surfaced to the operator, never a silent default.
    #[allow(dead_code)] // consumed by the daemon service wiring
    pub fn backend_result(&self) -> Result<Backend<'_>, String> {
        match (&self.run, &self.socket) {
            (Some(cmd), None) => Ok(Backend::Run(cmd.as_slice())),
            (None, Some(p)) => Ok(Backend::Socket(p.as_str())),
            (Some(_), Some(_)) => Err("service has both run and socket".into()),
            (None, None) => Err("service has neither run nor socket".into()),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct IdentityCfg {
    pub device_key: Option<PathBuf>, // None → paths::default_device_key_path()
    /// This device's suggested name for itself, carried in a minted pairing invite.
    /// `None` → the daemon defaults to a short fingerprint of the endpoint id.
    /// Additive (`#[serde(default)]` at the struct level).
    pub nickname: Option<String>,
    /// Roster mode: the org id this node joined (pinned at install/join).
    pub org_id: Option<String>,
    /// Roster mode: the pinned org-root public key, `b64u:`. The single trust anchor
    /// roster signatures verify against. Pinned on first roster install / `join`.
    pub org_root_pk: Option<String>,
    /// Roster mode: this node's stable user_id in the org. Pinned at `join` (proposed)
    /// and reconciled to the roster's authoritative value once installed.
    pub user_id: Option<String>,
    /// #85 ask 3: admit another DEVICE of a person this node already pairs with, when it presents a
    /// binding signed by that person's user key — no fresh SAS ceremony.
    ///
    /// **OFF by default**, and #85 asks for it to be seamless. It changes what a pairing MEANS:
    /// today a pairing admits a device, and with this on it admits a person and their future
    /// devices. #38 arguably made that true of grants already — they are keyed on stable
    /// principals, so a person's devices share authorization — but "arguably implied" is not a
    /// reason to widen admission on somebody's node during an upgrade.
    ///
    /// **It does not resurrect a device you removed.** `peer_remove` deletes the row; it does not
    /// stop the PERSON from attesting a device afterwards, because you still pair with them. If you
    /// removed a device because it was compromised, REVOKE it — attestation checks the revocation
    /// list first, which is why #85 ask 4 shipped before this.
    ///
    /// **It also keeps the pair ALPN's front door open.** That ALPN fast-closes when no invite is
    /// live; an attestation needs no invite, so with this on the (rate-limited, binding-verified)
    /// ceremony is reachable continuously. That is the cost of the feature, not a side effect.
    #[serde(default)]
    pub admit_attested_devices: bool,
    /// Roster mode: path to this person's user key. Minted by `join`; binds this
    /// person's devices. `None` → paths::default_user_key_path() when needed.
    pub user_key: Option<PathBuf>,
}

/// `[network]`. The knobs are exactly what `daemon::net_plan` implements —
/// no aspirational surface:
/// - `relay_mode = "default" | "custom" | "disabled"`. `"custom"` requires `relay_urls`
///   (self-hosted iroh relays); `"disabled"` is the HERMETIC mode — no relay AND no
///   discovery (localhost/tests).
/// - `discovery_mode = "default" | "custom"`. `"custom"` requires `discovery_urls` —
///   self-hosted pkarr relay URLs (e.g. an iroh-dns-server), used for BOTH publishing and
///   resolving peer addresses in place of n0's DNS/pkarr. Ignored (off) when
///   `relay_mode = "disabled"`.
///
/// Unknown modes or a `custom` without URLs are startup ERRORS (`net_plan`), never a silent
/// fallback — a metadata-privacy knob must not quietly revert to public infrastructure.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct NetworkCfg {
    pub relay_mode: String,
    /// Self-hosted relay URLs, required when `relay_mode = "custom"`.
    pub relay_urls: Vec<String>,
    pub discovery_mode: String,
    /// Self-hosted pkarr relay URLs, required when `discovery_mode = "custom"`.
    pub discovery_urls: Vec<String>,
    /// TESTING ONLY (#116): force application data over the RELAY even when a direct path exists.
    ///
    /// Requires the `unstable-relay-only` cargo feature. Without it this field still PARSES — a
    /// config must stay portable between a test build and a production one — but is ignored with a
    /// `warn!`. It is never a startup error: a testing switch must not brick a node, and it must
    /// never be ignored SILENTLY, because believing you tested the relay when you did not is the
    /// exact failure #116 reports.
    ///
    /// Selects the relay path; it does NOT prevent hole-punching (that is socket-level behaviour a
    /// `PathSelector` cannot reach). A direct path may still form — it simply never carries data,
    /// and `status` reports `relay` because #64 derives the path from `is_selected()`.
    pub relay_only: bool,
    /// `[network].presence_mode` (#89) — who gets a reachability pong on `mcpmesh/ping/1`.
    ///
    /// - `"paired"` (default): any paired peer, today's behaviour.
    /// - `"granted"`: only a caller currently holding at least one service grant. This is what
    ///   makes an embedder's per-peer sharing switch control presence too — revoking the last
    ///   service takes presence with it, live, with no restart and no new verb.
    /// - `"off"`: never pong.
    ///
    /// The arm is gated by PAIRING alone otherwise, so `service_allow_revoke` has no effect on it:
    /// a peer whose every service was revoked still learns you are online right now, your RTT, your
    /// `stack_version` and your app metadata, on demand and forever. The only lever was a full
    /// unpair — a relationship-destroying action to express a privacy preference (#89).
    ///
    /// A refusal under `"off"`/`"granted"` matches the trust gate's, so this arm does not
    /// distinguish "not paired" from "hidden" from "no grants".
    ///
    /// **This is NOT "appear offline".** It withholds the pong payload (`stack_version`, app
    /// metadata, the caller's services) and makes our own probe report you unreachable. It does not
    /// hide that the node is running: a QUIC application close implies a completed handshake,
    /// `mcpmesh/pair/1` answers any stranger by design, and a paired peer still gets a served
    /// `mcpmesh/mcp/1` session. Do not describe it to users as invisibility (#89 gate).
    ///
    /// Read at BOOT — changing the mode needs a restart. The per-peer effect under `"granted"` is
    /// live, because grants are.
    pub presence_mode: String,
    /// QUIC idle timeout in seconds (#56) — how long a connection survives with NO traffic and no
    /// keepalive before the transport closes it. `None` = iroh's default, **30s** on iroh 1.0.3.
    ///
    /// This is not "how long an idle session lives". iroh keepalives every 5s by default, so a held
    /// session survives indefinitely while the process runs; this is what detects a peer that
    /// VANISHED.
    ///
    /// **It is NEGOTIATED, not imposed.** QUIC takes the MINIMUM of the two peers' advertised
    /// values (RFC 9000 §10.1), so raising this on one node achieves nothing against a peer still
    /// on the default — the connection still times out at 30s. Raising it is only meaningful when
    /// every node is configured together; lowering it works one-sidedly.
    ///
    /// `0` means "no timeout" from THIS side, which likewise yields the peer's value; against a
    /// default peer that is still 30s. Only if both sides say `0` does a vanished peer go
    /// undetected at the transport layer.
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
    /// QUIC keepalive interval in seconds (#56) — how often the transport PINGs an otherwise idle
    /// connection. `None` = iroh's default, **5s** on iroh 1.0.3.
    ///
    /// Sets BOTH the connection-level and the per-path keepalive — setting only the former would
    /// leave every path pinging at iroh's 5s regardless.
    ///
    /// A transport keepalive carries no method-bearing frame, so it does NOT consume a
    /// `[limits].rate_limit_per_min` token — unlike an application-level heartbeat, which does.
    ///
    /// Must be less than the EFFECTIVE idle timeout — `idle_timeout_secs` if set, otherwise iroh's
    /// 30s — or boot fails. Note that effective timeout is the negotiated minimum, so a value that
    /// passes this check locally can still be too slow for a peer with a shorter one.
    ///
    /// **This can only LOWER the ping rate.** iroh caps the per-path keepalive at 5s and silently
    /// discards anything larger, so a value above 5 would leave every path pinging at 5s anyway —
    /// boot refuses it rather than pretend it took effect. There is no supported way to reduce
    /// keepalive traffic on a metered link with iroh 1.0.3.
    #[serde(default)]
    pub keep_alive_secs: Option<u64>,
    /// `[network].local_discovery` (#68) — find peers on the LAN with **no internet at all**.
    ///
    /// - `"off"` (default): no multicast sent, none listened for.
    /// - `"on"`: resolve peers on the link AND announce this node to it.
    /// - `"resolve"`: learn where peers are without publishing this node's identity or addresses.
    ///   NOT silent: resolving over mDNS means asking, and the library asks on a fixed cadence, so
    ///   this mode multicasts a `_mcpmesh._udp.local` query roughly once a second for as long as
    ///   the node runs. Every device on the link can see that something here runs mcpmesh and is
    ///   up. If that matters, the mode is `"off"`.
    ///
    /// Peer resolution otherwise needs external infrastructure — the pkarr publisher a relay
    /// provides, or an address someone already handed over — so two machines on the same LAN with
    /// no uplink cannot find each other though the path between them is fine. That is the scenario
    /// where "peer to peer" earns its keep, and the commoner weak version too: a LAN where the
    /// internet is merely flaky, so peers that could talk directly fail to resolve because
    /// resolution goes out first.
    ///
    /// **OFF by default, and #68 asked for on.** The two disclosures are not comparable. pkarr
    /// publishes a signed record someone must already know your endpoint id to look up. mDNS
    /// announces your endpoint id and addresses to EVERY device on the link, unprompted and
    /// repeatedly, to machines that had no idea you existed — and that id is the one peers pin, so
    /// it correlates you across networks. On a home LAN that is the point; on a café, hotel or
    /// conference network it is a broadcast to strangers. A node cannot un-send a multicast packet:
    /// turning this on is one line and reversible, turning it on for someone silently is not.
    /// `"resolve"` is there for whoever wants the benefit without publishing their identity — with
    /// the query caveat above, which is the honest limit of that mode.
    ///
    /// What `"on"` puts on the link is broader than "addresses" suggests: the LAN address, the
    /// PUBLIC WAN IPv4, and global IPv6 addresses. A café LAN learns your home/ISP address.
    ///
    /// Read at BOOT. Like `relay_mode`/`presence_mode`, an unknown value is a startup ERROR.
    #[serde(default = "default_local_discovery")]
    pub local_discovery: String,
}

fn default_local_discovery() -> String {
    "off".into()
}
impl Default for NetworkCfg {
    fn default() -> Self {
        Self {
            relay_mode: "default".into(),
            relay_urls: Vec::new(),
            discovery_mode: "default".into(),
            discovery_urls: Vec::new(),
            relay_only: false,
            presence_mode: "paired".into(),
            idle_timeout_secs: None,
            keep_alive_secs: None,
            local_discovery: default_local_discovery(),
        }
    }
}

/// `[limits]`. NOTE — the frame cap is deliberately NOT here: the 16 MiB `max_frame`
/// default is a fixed CONSTANT at each wire (`mcpmesh_net::endpoint` for the mesh,
/// `ipc::MAX_FRAME_BYTES` for the control socket, `backends::MAX_FRAME_BYTES` for local MCP
/// servers), not a config tunable. A `max_frame` config field existed historically but was never
/// threaded into any `FrameReader` (dead surface); threading it into the mesh path would widen
/// `mcpmesh-net`'s public API for no demonstrated need, so the field was removed instead (serde
/// ignores an unknown `max_frame` key in existing configs).
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct LimitsCfg {
    pub rate_limit_per_min: u32,
    pub max_inflight: u32,
    pub max_sessions: u32,
    /// Per-authenticated-endpoint app-blob BYTE budget, bytes per minute (#84a).
    ///
    /// **0 = unlimited, and that is the default**, so an existing deployment is unchanged on
    /// upgrade. The pre-existing blob limiter counts CONNECTIONS, which cannot see one granted
    /// peer re-pulling a 4 GB blob on each of 60 connections a minute; this bounds the bytes.
    ///
    /// A peer that exceeds it gets its transfer ABORTED (retryable), not paced — pacing holds the
    /// request open and turns a bandwidth problem into an unbounded-concurrency one.
    ///
    /// **Use 0 or at least 32768** (two chunks); a value in `1..32768` is FLOORED to 32768.
    ///
    /// Admission reserves one chunk before any bytes and the transfer then meters its own chunks,
    /// so a sub-floor budget does not fail closed — it silently caps every servable blob at
    /// roughly `budget - 16384` bytes and truncates anything larger. Measured: 20480 serves a
    /// 4 KiB blob and nothing bigger. Two earlier drafts of this comment got that wrong, first
    /// recommending the bricking value and then claiming it failed closed.
    ///
    /// Requires a restart: the limiter and the provider's event mask are both built once at boot.
    pub blob_bytes_per_min: u64,
    /// Audit-log retention window in calendar months (#88). **0 = keep forever, and that is the
    /// default** — flipping today's keep-everything behavior to auto-deletion is a product call,
    /// deliberately not made here. When N > 0, boot deletes monthly audit files older than the
    /// last N months (the current month counts as month 1). Boot-time only: a long-running
    /// daemon prunes on its next start; the `audit_prune` verb covers live needs.
    pub audit_retain_months: u32,
}
impl Default for LimitsCfg {
    fn default() -> Self {
        Self {
            rate_limit_per_min: 120,
            max_inflight: 16,
            max_sessions: 4,
            blob_bytes_per_min: 0, // unlimited: opt-in, no behaviour change on upgrade
            audit_retain_months: 0, // keep forever: opt-in, no behaviour change on upgrade
        }
    }
}

/// The default degraded-expiry grace window (`[roster].grace_period` default "72h").
/// A stale roster keeps serving for this window past `expires_at` (with a warning) before it
/// stops granting roster identity. Kept here so [`RosterCfg::default`] and the parse fallback
/// share one source; the gate mirrors it as `roster::gate::DEFAULT_GRACE_SECS`.
const DEFAULT_GRACE_SECS: i64 = 72 * 3600;

/// The default freshness bound (`[roster].max_staleness`, default "24h" = 86400s). A roster
/// this node has not re-confirmed current within this window degrades on the SAME `RosterState`
/// machine as expiry (warnings within `grace`, then serving stops) — bounding adversarial staleness at
/// `max_staleness + grace` independent of `expires_at`. Shared by [`RosterCfg::default`] + the parse
/// fallback.
const DEFAULT_MAX_STALENESS_SECS: i64 = 24 * 3600;

/// The `[roster]` config table. `grace_period` is the degraded-expiry grace window — how
/// long a roster past `expires_at` keeps serving (degraded, warning) before it stops. Additive
/// (`#[serde(default)]`): a config with no `[roster]` table gets the 72h default.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct RosterCfg {
    /// Degraded-expiry grace window: `"72h"` / `"24h"` / plain seconds (default "72h").
    pub grace_period: String,
    /// The pinned roster URL for the HTTPS poll. Operator-managed static hosting; also how a
    /// joiner bootstraps its FIRST roster. `None` → no URL poll (manual installs only).
    /// Additive (`#[serde(default)]`): a config with no `url` key gets `None`.
    pub url: Option<String>,
    /// How often to poll `url` (default "1h"). Total-parse like `grace_period` — an
    /// unparseable value falls back to the hourly default rather than disabling the poll.
    pub poll_interval: String,
    /// The freshness bound (default "24h"): how long this node may go without re-confirming
    /// the installed roster current (via a TLS URL poll ≥ installed, a gossip install, or a
    /// manual install) before it degrades on the SAME `RosterState` machine as expiry. Total-parse
    /// like `grace_period` (an unparseable value falls back to the 24h default — a typo never disables
    /// the bound). Additive (`#[serde(default)]`): a config with no `max_staleness` key gets 24h.
    pub max_staleness: String,
}
impl Default for RosterCfg {
    fn default() -> Self {
        Self {
            grace_period: "72h".into(),
            url: None,
            poll_interval: "1h".into(),
            max_staleness: "24h".into(),
        }
    }
}

impl RosterCfg {
    /// The grace window in SECONDS. An absent or unparseable `grace_period` falls back to the 72h
    /// default rather than erroring — an operator typo must never disable degraded serving, and a
    /// grace window is advisory, not a security bound (revocation is enforced regardless of
    /// degraded state).
    ///
    /// Two paths degrade on the ONE `RosterState` machine (`RosterView::state`, Approved →
    /// DegradedGrace → DegradedStopped): expiry (`expires_at` + THIS grace window) and freshness
    /// (`last_confirmed` + `max_staleness`). Once DegradedStopped, the gate stops granting roster
    /// identity (fail-closed — revocation is still enforced); within grace, serving continues
    /// with a warning (`daemon::warn_if_degraded_grace`).
    pub fn grace_seconds(&self) -> i64 {
        parse_duration(&self.grace_period).unwrap_or(DEFAULT_GRACE_SECS)
    }

    /// The URL poll interval in SECONDS (default 3600). Like [`grace_seconds`](Self::grace_seconds)
    /// it is TOTAL — an absent/unparseable value falls back to the hourly default rather than
    /// erroring, so an operator typo slows the poll to hourly instead of disabling freshness.
    pub fn poll_interval_seconds(&self) -> i64 {
        parse_duration(&self.poll_interval).unwrap_or(3600)
    }

    /// The freshness bound in SECONDS (default 86400 = 24h). Like [`grace_seconds`](Self::grace_seconds)
    /// it is TOTAL — an absent/unparseable value falls back to the 24h default rather than erroring, so
    /// an operator typo tightens/loosens to 24h instead of disabling the freshness bound.
    pub fn max_staleness_seconds(&self) -> i64 {
        parse_duration(&self.max_staleness).unwrap_or(DEFAULT_MAX_STALENESS_SECS)
    }
}

/// The shortest `[blobs].gc_interval` that is honoured. Below this, collection stays OFF.
///
/// A sweep walks every blob in the store and deletes what the scope table does not name; running
/// it every few seconds is all cost and no benefit, and `"1s"` is far more likely to be a mistake
/// than an intent.
pub const MIN_GC_INTERVAL_SECS: i64 = 60;

/// The `[blobs]` table.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct BlobsCfg {
    /// How often to garbage-collect the app-blob store (#80), e.g. `"1h"`. **Absent means no
    /// collection at all** — `<data_dir>/blobs/` grows monotonically, which is the behavior of
    /// every release up to 0.42.0.
    ///
    /// Opt-in because a sweep deletes bytes the node holds but no scope names, which includes
    /// every blob this node has FETCHED and not republished. Those are reclaimable — the fetch
    /// already wrote the caller's `dest_path` and the store copy is a cache — but it means a
    /// `blob_republish` of a hash fetched more than one interval ago fails. Silent background
    /// deletion is the wrong default for a local-first tool, and that interaction makes it wrong
    /// twice.
    pub gc_interval: Option<String>,
}

impl BlobsCfg {
    /// The GC interval in SECONDS, or `None` for "do not collect".
    ///
    /// **Deliberately NOT total, unlike every other duration accessor here.**
    /// [`RosterCfg::grace_seconds`] and friends fall back to their default on a typo because a typo
    /// must never disable a safety property. This one runs the other way: a value that fell back to
    /// *some* interval would let `gc_interval = "1hh"` start deleting data the operator never
    /// authorized. So an unparseable value — or one below [`MIN_GC_INTERVAL_SECS`] — leaves
    /// collection OFF and warns.
    ///
    /// A below-floor value is refused rather than clamped UP: a clamped value reads back through
    /// `status.storage.blobs_gc.interval_secs` as honoured, and is not.
    pub fn gc_interval_seconds(&self) -> Option<u64> {
        let raw = self.gc_interval.as_deref()?;
        match parse_duration(raw) {
            Ok(secs) if secs >= MIN_GC_INTERVAL_SECS => Some(secs as u64),
            Ok(secs) => {
                tracing::warn!(
                    value = raw,
                    secs,
                    min = MIN_GC_INTERVAL_SECS,
                    "[blobs].gc_interval is below the minimum; blob garbage collection is OFF"
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    value = raw,
                    %e,
                    "[blobs].gc_interval is unparseable; blob garbage collection is OFF"
                );
                None
            }
        }
    }
}

/// Parse a duration string to SECONDS: a `d`/`h`/`m`/`s` suffix (days/hours/minutes/seconds) or a
/// bare number (seconds). Trim + suffix-strip + checked multiply; rejects a
/// negative/overflowing/garbage value as `Err` (the caller supplies the
/// default). `u64` parse then a checked `i64` conversion: a negative grace is meaningless, so `-1`
/// fails the `u64` parse and falls back to the default rather than becoming a negative window.
// Reached only by the accessors above and the `org create --expires` porcelain
// (`enrollcmd`, the operator-managed validity window — now across the crate seam, hence
// `pub`; still `#[doc(hidden)]` at the module level). Pure parser — no state.
pub fn parse_duration(s: &str) -> Result<i64, String> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix('d') {
        (n, 24 * 3600)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1)
    } else {
        (s, 1)
    };
    num.trim()
        .parse::<u64>()
        .ok()
        .and_then(|v| v.checked_mul(mult))
        .and_then(|v| i64::try_from(v).ok())
        .ok_or_else(|| format!("unparseable duration: {s}"))
}

// figment::Error is ~208 bytes; boxing it would churn the API for a cold path.
#[allow(clippy::result_large_err)]
impl Config {
    #[allow(dead_code)] // exercised by unit tests; config-string entry point for later tooling
    pub fn from_toml_str(s: &str) -> Result<Self, figment::Error> {
        Figment::new().merge(Toml::string(s)).extract()
    }

    /// Missing file → defaults (first run); malformed file → Err.
    /// Callers must surface the Err — swallowing it silently reverts user choices.
    pub fn load(path: &std::path::Path) -> Result<Self, figment::Error> {
        Figment::new().merge(Toml::file(path)).extract()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_yields_spec_defaults() {
        let c = Config::from_toml_str("").unwrap();
        assert_eq!(c.network.relay_mode, "default");
        assert_eq!(c.network.discovery_mode, "default");
        assert_eq!(c.limits.rate_limit_per_min, 120);
        assert_eq!(c.limits.max_inflight, 16);
        assert_eq!(c.limits.max_sessions, 4);
    }

    #[test]
    fn values_override_defaults() {
        let c = Config::from_toml_str(
            "[network]\nrelay_mode = \"disabled\"\n[limits]\nrate_limit_per_min = 60\n",
        )
        .unwrap();
        assert_eq!(c.network.relay_mode, "disabled");
        assert_eq!(c.limits.rate_limit_per_min, 60);
        assert_eq!(c.limits.max_inflight, 16);
    }

    /// A legacy config carrying the removed `max_frame` key still loads (serde ignores unknown
    /// fields) — the frame cap is a fixed constant now, not a tunable (see the `LimitsCfg` doc).
    #[test]
    fn legacy_max_frame_key_is_ignored_not_an_error() {
        let c =
            Config::from_toml_str("[limits]\nmax_frame = \"1MiB\"\nmax_sessions = 2\n").unwrap();
        assert_eq!(c.limits.max_sessions, 2);
    }

    /// The self-hosting knobs parse: `custom` modes with their URL lists. (Validation —
    /// custom-without-urls, unknown modes — lives in `daemon::net_plan`, tested there.)
    #[test]
    fn network_relay_and_discovery_urls_parse() {
        let c = Config::from_toml_str(
            "[network]\nrelay_mode = \"custom\"\nrelay_urls = [\"https://relay.acme.com\"]\n\
             discovery_mode = \"custom\"\ndiscovery_urls = [\"https://dns.acme.com/pkarr\"]\n",
        )
        .unwrap();
        assert_eq!(c.network.relay_mode, "custom");
        assert_eq!(
            c.network.relay_urls,
            vec!["https://relay.acme.com".to_string()]
        );
        assert_eq!(c.network.discovery_mode, "custom");
        assert_eq!(
            c.network.discovery_urls,
            vec!["https://dns.acme.com/pkarr".to_string()]
        );
        // Absent → empty lists (the defaults need no URLs).
        let c = Config::from_toml_str("").unwrap();
        assert!(c.network.relay_urls.is_empty() && c.network.discovery_urls.is_empty());
    }

    #[test]
    fn missing_file_loads_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c = Config::load(&dir.path().join("nope.toml")).unwrap();
        assert_eq!(c.network.relay_mode, "default");
    }

    #[test]
    fn roster_url_and_poll_interval_parse_with_defaults() {
        // No [roster] table → url None, poll 1h default.
        let c = Config::from_toml_str("").unwrap();
        assert!(c.roster.url.is_none());
        assert_eq!(c.roster.poll_interval_seconds(), 3600);
        // A configured url + poll interval.
        let c = Config::from_toml_str(
            "[roster]\nurl = \"https://intranet.acme.com/roster.json\"\npoll_interval = \"30m\"\n",
        )
        .unwrap();
        assert_eq!(
            c.roster.url.as_deref(),
            Some("https://intranet.acme.com/roster.json")
        );
        assert_eq!(c.roster.poll_interval_seconds(), 30 * 60);
        // An unparseable poll_interval falls back to the hourly default (never disables the poll).
        let c = Config::from_toml_str("[roster]\npoll_interval = \"never\"\n").unwrap();
        assert_eq!(c.roster.poll_interval_seconds(), 3600);
        // The url is additive: setting only grace_period keeps url None + the default poll.
        let c = Config::from_toml_str("[roster]\ngrace_period = \"24h\"\n").unwrap();
        assert!(c.roster.url.is_none());
        assert_eq!(c.roster.poll_interval_seconds(), 3600);
    }

    #[test]
    fn roster_max_staleness_defaults_to_24h_and_parses() {
        // No [roster] table → the 24h freshness bound (the default).
        let c = Config::from_toml_str("").unwrap();
        assert_eq!(c.roster.max_staleness_seconds(), 24 * 3600);
        // A configured value parses (units, like grace_period).
        let c = Config::from_toml_str("[roster]\nmax_staleness = \"6h\"\n").unwrap();
        assert_eq!(c.roster.max_staleness_seconds(), 6 * 3600);
        // An unparseable value falls back to the 24h default (never disables the freshness bound).
        let c = Config::from_toml_str("[roster]\nmax_staleness = \"forever\"\n").unwrap();
        assert_eq!(c.roster.max_staleness_seconds(), 24 * 3600);
        // Additive: setting only grace_period keeps the 24h max_staleness default.
        let c = Config::from_toml_str("[roster]\ngrace_period = \"48h\"\n").unwrap();
        assert_eq!(c.roster.max_staleness_seconds(), 24 * 3600);
    }

    #[test]
    fn roster_grace_defaults_to_72h_and_parses_units() {
        // Absent `[roster]` → the 72h default.
        let c = Config::from_toml_str("").unwrap();
        assert_eq!(c.roster.grace_seconds(), 72 * 3600);
        // Hours / days / minutes / seconds / bare-seconds all resolve to seconds.
        for (body, want) in [
            ("[roster]\ngrace_period = \"24h\"\n", 24 * 3600),
            ("[roster]\ngrace_period = \"72h\"\n", 72 * 3600),
            ("[roster]\ngrace_period = \"1d\"\n", 24 * 3600),
            ("[roster]\ngrace_period = \"30m\"\n", 30 * 60),
            ("[roster]\ngrace_period = \"90s\"\n", 90),
            ("[roster]\ngrace_period = \"3600\"\n", 3600), // bare seconds
        ] {
            assert_eq!(
                Config::from_toml_str(body).unwrap().roster.grace_seconds(),
                want,
                "{body}"
            );
        }
    }

    #[test]
    fn roster_grace_unparseable_or_negative_falls_back_to_default() {
        // A garbage / negative / overflowing grace never disables degraded serving — it defaults.
        for body in [
            "[roster]\ngrace_period = \"seventy-two hours\"\n",
            "[roster]\ngrace_period = \"-5h\"\n",
            "[roster]\ngrace_period = \"18446744073709551615d\"\n", // overflows the checked_mul
            "[roster]\ngrace_period = \"\"\n",
        ] {
            assert_eq!(
                Config::from_toml_str(body).unwrap().roster.grace_seconds(),
                72 * 3600,
                "{body}"
            );
        }
    }

    #[test]
    fn services_parse_run_and_socket() {
        let c = Config::from_toml_str(concat!(
            "[services.notes]\nrun = [\"npx\", \"server\"]\nallow = [\"bob\"]\n",
            "[services.kb]\nsocket = \"/run/kb.sock\"\nallow = [\"team-eng\"]\n",
        ))
        .unwrap();
        let notes = c.services.get("notes").unwrap();
        assert!(
            matches!(notes.backend_result(), Ok(Backend::Run(cmd)) if cmd == &["npx".to_string(), "server".to_string()][..])
        );
        assert_eq!(notes.allow, vec!["bob".to_string()]);
        assert!(
            matches!(c.services.get("kb").unwrap().backend_result(), Ok(Backend::Socket(p)) if p == "/run/kb.sock")
        );
    }

    #[test]
    fn service_with_both_run_and_socket_is_an_error() {
        let e = Config::from_toml_str("[services.x]\nrun=[\"a\"]\nsocket=\"/s\"\nallow=[]\n");
        // exactly one backend kind is required — validate at access time.
        assert!(
            e.unwrap()
                .services
                .get("x")
                .unwrap()
                .backend_result()
                .is_err()
        );
    }

    #[test]
    fn identity_reads_user_id_and_user_key() {
        let toml = "[identity]\n\
            org_id = \"acme\"\n\
            org_root_pk = \"b64u:AAAA\"\n\
            user_id = \"alice\"\n\
            user_key = \"/home/alice/.config/mcpmesh/user.key\"\n";
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.identity.user_id.as_deref(), Some("alice"));
        assert_eq!(
            cfg.identity.user_key.as_deref(),
            Some(std::path::Path::new("/home/alice/.config/mcpmesh/user.key"))
        );
        // Absent → None (pure-pairing / operator-only node).
        let bare: Config = toml::from_str("[identity]\n").unwrap();
        assert!(bare.identity.user_id.is_none() && bare.identity.user_key.is_none());
    }

    /// #80: `[blobs].gc_interval` must FAIL SAFE. A knob that deletes bytes gets the opposite
    /// convention from every other duration here.
    ///
    /// `grace_period`/`poll_interval`/`max_staleness` fall back to their default on a typo, because
    /// a typo must never disable a safety property. Here a fallback to *some* interval would let
    /// `"1hh"` start deleting data the operator never authorized, so the fallback is OFF.
    #[test]
    fn a_bad_gc_interval_leaves_collection_off_rather_than_guessing_one() {
        let off: Config = toml::from_str("[identity]\n").unwrap();
        assert_eq!(
            off.blobs.gc_interval_seconds(),
            None,
            "absent means no collection at all — the behavior of every release up to 0.42.0"
        );

        let on: Config = toml::from_str("[blobs]\ngc_interval = \"2h\"\n").unwrap();
        assert_eq!(on.blobs.gc_interval_seconds(), Some(7200));

        for bad in ["1hh", "", "soon", "-1", "0.5h"] {
            let c: Config = toml::from_str(&format!("[blobs]\ngc_interval = \"{bad}\"\n")).unwrap();
            assert_eq!(
                c.blobs.gc_interval_seconds(),
                None,
                "an unparseable interval ({bad:?}) must leave collection OFF, never fall back to \
                 a default that deletes data"
            );
        }
    }

    /// Below the floor is REFUSED, not clamped up.
    ///
    /// A clamped value reads back through `status.storage.blobs_gc.interval_secs` as honoured and
    /// is not. The boundary is pinned from both sides so a `>` / `>=` slip is visible.
    #[test]
    fn a_sub_minimum_gc_interval_is_refused_rather_than_clamped() {
        let at: Config = toml::from_str(&format!(
            "[blobs]\ngc_interval = \"{MIN_GC_INTERVAL_SECS}s\"\n"
        ))
        .unwrap();
        assert_eq!(
            at.blobs.gc_interval_seconds(),
            Some(MIN_GC_INTERVAL_SECS as u64),
            "exactly the floor is honoured"
        );
        for under in [MIN_GC_INTERVAL_SECS - 1, 1, 0] {
            let c: Config =
                toml::from_str(&format!("[blobs]\ngc_interval = \"{under}s\"\n")).unwrap();
            assert_eq!(
                c.blobs.gc_interval_seconds(),
                None,
                "{under}s is below the floor and must leave collection OFF, not be raised to it"
            );
        }
    }
}
