//! The live activity feed and the approval channel, on `app/ai-remote/events/1`.
//!
//! A controller opens one bi-stream per request and writes one JSON line; the line decides what the
//! stream is for (a QUIC stream only reaches the host once something is written):
//!
//! - `{"subscribe":true}`: the feed. The host answers with a `{"kind":"hello",...}` line (its device
//!   id, this device's level and the approvals waiting right now), the backlog of recent audit lines, then every new line
//!   as it is written (tool decisions and outcomes, session start and end, approval requests and
//!   answers), one JSON object per line, until either side closes. A controller app turns this into
//!   the agent's step timeline and its approval cards.
//! - `{"pending":true}`: the approvals waiting right now.
//! - `{"answer":{...}}`: allow or deny a waiting call ([`crate::approval::RemoteAnswer`]).
//! - `{"enroll":{"alg":..,"key":..,"label":..}}`: offer an approver key; someone at this host must
//!   confirm it.
//! - `{"approver_status":{"alg":..,"key":..}}`: whether this host accepted that key for this device.
//! - `{"turn":{"action":"take_over"|"hand_back"|"pause"|"resume",...}}`: change who drives (see
//!   [`crate::turns`]); `take_over` takes `agent_may_watch`, `hand_back` an optional `note`.
//! - `{"tell":{"text":".."}}`: a message for the agent, delivered with its next call result.
//! - `{"access":true}`: what this device may do here: its level, the standing rules that concern it,
//!   the devices paired with this computer (all of them for an admin, otherwise only itself) and
//!   the tools on offer, built in and mounted, with the level each needs and whether it asks first;
//!   `bypass: true` when the owner trusts this device with everything (every tool allowed then).
//! - `{"lower":{"level":".."}}`: lower this device's own level. Raising it takes someone at this
//!   computer (`ai-remote level`), so a stolen controller cannot climb back.
//! - `{"leave":true}`: remove this device's pairing here (unpairing from the controller's side);
//!   the reply comes first, then the connections close.
//!
//! Requests other than `subscribe` get one `{"ok":true,...}` or `{"ok":false,"error":"..."}` line.
//! Any device with a level may use the stream; the feed carries what the audit log carries (argument
//! previews, never secret values, which the gate already reduces to sizes).

use std::sync::Arc;

use ar_core::{Level, VERSION, approval::KeyAlg, config::MountStart};
use ar_net::{
    Peer,
    iroh::{
        endpoint::{Connection, SendStream},
        protocol::{AcceptError, ProtocolHandler},
    },
    refusal::{CLOSE_NO_ACCESS, REASON_NO_LEVEL, REASON_NOT_PAIRED},
};
use data_encoding::BASE64;
use serde::{Deserialize, de::IgnoredAny};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    sync::broadcast::error::RecvError,
};

use crate::{HostState, approval::RemoteAnswer, approvers::ApproverKey};

/// Longest request line accepted.
const MAX_REQUEST: u64 = 8192;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    /// The flag's value does not matter.
    Subscribe(IgnoredAny),
    Pending(IgnoredAny),
    Answer(RemoteAnswer),
    Enroll(Enroll),
    ApproverStatus(KeyRef),
    Turn(Turn),
    Tell(Tell),
    Access(IgnoredAny),
    Lower(Lower),
    Leave(IgnoredAny),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Lower {
    level: Level,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Turn {
    action: TurnAction,
    #[serde(default)]
    agent_may_watch: bool,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TurnAction {
    TakeOver,
    HandBack,
    Pause,
    Resume,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Tell {
    text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Enroll {
    alg: KeyAlg,
    /// Base64 DER SubjectPublicKeyInfo.
    key: String,
    #[serde(default)]
    label: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyRef {
    alg: KeyAlg,
    key: String,
}

pub struct EventsProtocol {
    host: Arc<HostState>,
}

impl std::fmt::Debug for EventsProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EventsProtocol")
    }
}

impl EventsProtocol {
    pub fn new(host: Arc<HostState>) -> Self {
        Self { host }
    }
}

impl ProtocolHandler for EventsProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let peer = match self.host.mesh().peer_for(&conn.remote_id()).await {
            Ok(Some(peer)) => peer,
            _ => {
                conn.close(CLOSE_NO_ACCESS.into(), REASON_NOT_PAIRED);
                return Ok(());
            }
        };
        if peer.level.is_none() {
            conn.close(CLOSE_NO_ACCESS.into(), REASON_NO_LEVEL);
            return Ok(());
        }
        while let Ok((mut send, recv)) = conn.accept_bi().await {
            let host = self.host.clone();
            let peer = peer.clone();
            tokio::spawn(async move {
                let mut first = String::new();
                let mut reader = BufReader::new(recv.take(MAX_REQUEST));
                if reader.read_line(&mut first).await.unwrap_or(0) == 0 {
                    return;
                }
                let request = serde_json::from_str::<Request>(&first);
                // Changing this device's own access cuts its connections, so it happens after the reply.
                let after = match &request {
                    Ok(Request::Lower(l)) => Some(After::Lower(l.level)),
                    Ok(Request::Leave(_)) => Some(After::Leave),
                    _ => None,
                };
                let reply = match request {
                    Ok(Request::Subscribe(_)) => return feed(&host, &peer, send).await,
                    Ok(req) => handle(&host, &peer, req).await,
                    Err(e) => Err(anyhow::anyhow!("bad request: {e}")),
                };
                let after = after.filter(|_| reply.is_ok());
                let line = match reply {
                    Ok(mut v) => {
                        v["ok"] = true.into();
                        v
                    }
                    Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
                };
                if send.write_all(&framed(&line.to_string())).await.is_ok() {
                    let _ = send.finish();
                    // Let the reply reach the controller before the stream is dropped.
                    let _ = send.stopped().await;
                }
                if let Some(after) = after {
                    apply(&host, &peer, after).await;
                }
            });
        }
        Ok(())
    }
}

async fn handle(host: &HostState, peer: &Peer, req: Request) -> anyhow::Result<Value> {
    let approver = host.approver();
    // The level now, not when this connection opened: it may have been lowered since. Checks use
    // the effective one (admin while the owner bypasses this device); lowering uses its own.
    let own = host.current_level(peer).await;
    let level = host.effective_level(peer).await;
    Ok(match req {
        Request::Subscribe(_) => unreachable!("served as a feed"),
        Request::Pending(_) => json!({"device": host.mesh().principal(), "pending": approver.pending()}),
        Request::Answer(answer) => {
            approver.answer(peer, answer)?;
            json!({})
        }
        Request::Enroll(e) => {
            let principal = peer.principal.as_deref().ok_or_else(|| anyhow::anyhow!("this device has no device id"))?;
            let der = BASE64.decode(e.key.as_bytes()).map_err(|_| anyhow::anyhow!("the key is not base64"))?;
            let key = ApproverKey::new(principal, &peer.name, e.alg, &der, &e.label)?;
            let fingerprint = key.fingerprint();
            let added = approver.enroll(peer, key, host.audit()).await?;
            json!({"added": added, "fingerprint": fingerprint})
        }
        Request::ApproverStatus(k) => {
            let principal = peer.principal.as_deref().unwrap_or_default();
            json!({"accepted": approver.keys().find(principal, k.alg, &k.key).is_some()})
        }
        Request::Turn(t) => {
            // Anyone paired may stop the agent; taking the controls or letting it go on needs control.
            if !matches!(t.action, TurnAction::Pause) && level < Some(Level::Control) {
                anyhow::bail!("this device's level does not allow that (it needs control)");
            }
            let turns = host.turns();
            let driver = match t.action {
                TurnAction::TakeOver => turns.take_over(&peer.name, t.agent_may_watch),
                TurnAction::HandBack => turns.hand_back(&peer.name, t.note.as_deref())?,
                TurnAction::Pause => turns.pause(&peer.name),
                TurnAction::Resume => turns.resume()?,
            };
            let mut line = driver.event(&peer.name);
            if let Some(note) = t.note.as_deref().filter(|_| matches!(t.action, TurnAction::HandBack)) {
                line["note"] = note.into();
            }
            host.audit().event(line);
            json!({"driver": driver})
        }
        Request::Tell(t) => {
            if level < Some(Level::Control) {
                anyhow::bail!("this device's level does not allow messages to the agent (it needs control)");
            }
            host.turns().tell(&peer.name, &t.text)?;
            host.audit().event(json!({"kind": "note", "from": peer.name, "text": t.text}));
            json!({})
        }
        Request::Access(_) => access(host, peer, own).await?,
        Request::Lower(l) => {
            let Some(current) = own else { anyhow::bail!("this device has no level here") };
            if l.level > current {
                anyhow::bail!(
                    "raising a level takes someone at this computer (ai-remote level {} {})",
                    peer.name,
                    l.level.as_str()
                );
            }
            // Applied after the reply: see `After`.
            json!({"level": l.level})
        }
        Request::Leave(_) => json!({"leaving": true}),
    })
}

/// A change to the requesting device's own access, made once its reply is on the way (the
/// transport cuts the device's connections when its grant changes).
enum After {
    Lower(Level),
    Leave,
}

async fn apply(host: &HostState, peer: &Peer, after: After) {
    match after {
        After::Lower(level) => match host.mesh().set_level(peer, Some(level)).await {
            Ok(()) => {
                host.audit().event(json!({"kind": "level", "peer": peer.name, "level": level, "by": peer.name}));
                // Asking for less access ends the bypass too, or lowering would change nothing.
                if let Some(device) = &peer.principal {
                    crate::control::end_bypass(host, &peer.name, device, "lowered its level");
                }
            }
            Err(e) => {
                tracing::warn!("{} asked to lower its level to {}, but it failed: {e:#}", peer.name, level.as_str())
            }
        },
        After::Leave => match crate::control::forget(host, peer).await {
            Ok(_) => host.audit().event(json!({"kind": "unpaired", "peer": peer.name, "by": peer.name})),
            Err(e) => tracing::warn!("{} asked to unpair, but it failed: {e:#}", peer.name),
        },
    }
}

/// What `peer` may do on this computer; see `{"access":true}` in the module docs.
async fn access(host: &HostState, peer: &Peer, level: Option<Level>) -> anyhow::Result<Value> {
    let rules = &host.config().rules;
    let standing: Vec<Value> = rules
        .iter()
        .filter(|r| crate::policy::applies(r, peer))
        .map(|r| json!({"tool": r.tool, "action": r.action, "scope": if r.principal.is_some() { "you" } else { "everyone" }}))
        .collect();
    let bypass = host.bypass().covers(peer);
    let admin = bypass || level == Some(Level::Admin);
    let devices: Vec<Value> = host
        .mesh()
        .peers()
        .await?
        .into_iter()
        .filter(|p| admin || (p.principal.is_some() && p.principal == peer.principal))
        .map(|p| {
            let you = p.principal.is_some() && p.principal == peer.principal;
            json!({"name": p.name, "level": p.level, "revoked": p.revoked, "you": you})
        })
        .collect();
    let tool = |name: &str| {
        host.registry().get(name).map(|t| {
            let spec = t.spec();
            let action = if bypass {
                ar_core::config::Action::Allow
            } else {
                crate::policy::decide(rules, &spec.name, peer, spec.default_action)
            };
            json!({"name": spec.name, "description": spec.description, "level": spec.level, "action": action})
        })
    };
    let mut mounted = std::collections::HashSet::new();
    let mounts: Vec<Value> = host
        .config()
        .mounts
        .iter()
        .map(|m| {
            let names: Vec<String> = m.tools.keys().map(|t| crate::mounts::tool_name(&m.name, t)).collect();
            mounted.extend(names.iter().cloned());
            let tools: Vec<Value> = names.iter().filter_map(|n| tool(n)).collect();
            json!({"name": m.name, "tools": tools, "needs_desktop": m.needs_desktop, "lazy": m.start == MountStart::Lazy})
        })
        .collect();
    let builtin: Vec<Value> = host
        .registry()
        .iter()
        .map(|t| t.spec().name.clone())
        .filter(|n| !mounted.contains(n))
        .filter_map(|n| tool(&n))
        .collect();
    Ok(json!({
        "host": host.name(),
        "you": peer.name,
        "level": level,
        "bypass": bypass,
        "rules": standing,
        "devices": devices,
        "builtin": builtin,
        "mounts": mounts,
    }))
}

/// Serves a feed subscription until either side closes or the host stops.
async fn feed(host: &HostState, peer: &Peer, mut send: SendStream) {
    let (backlog, mut live) = host.audit().subscribe();
    let hello = json!({
        // This device's level now (a change cuts the connection, so a new feed brings the new one).
        "level": host.current_level(peer).await,
        "bypass": host.bypass().covers(peer),
        "kind": "hello",
        "host": host.name(),
        "device": host.mesh().principal(),
        "version": VERSION,
        "os": std::env::consts::OS,
        "pending": host.approver().pending(),
        "driver": crate::turns::hello(&host.turns().driver()),
    })
    .to_string();
    if send.write_all(&framed(&hello)).await.is_err() {
        return;
    }
    for line in backlog {
        if send.write_all(&framed(&line)).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            next = live.recv() => match next {
                Ok(line) => {
                    if send.write_all(&framed(&line)).await.is_err() {
                        return;
                    }
                }
                // A slow subscriber missed lines: say so instead of silently skipping.
                Err(RecvError::Lagged(n)) => {
                    let note = json!({"kind": "lagged", "missed": n}).to_string();
                    if send.write_all(&framed(&note)).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Closed) => return,
            },
            _ = host.stopping().cancelled() => {
                let _ = send.finish();
                return;
            }
            // The controller went away.
            _ = send.stopped() => return,
        }
    }
}

/// One JSON line with its newline.
fn framed(line: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    bytes
}
