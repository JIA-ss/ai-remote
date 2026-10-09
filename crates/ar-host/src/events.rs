//! The live activity feed and the approval channel, on `app/ai-remote/events/1`.
//!
//! A controller opens one bi-stream per request and writes one JSON line; the line decides what the
//! stream is for (a QUIC stream only reaches the host once something is written):
//!
//! - `{"subscribe":true}`: the feed. The host answers with a `{"kind":"hello",...}` line (its device
//!   id and the approvals waiting right now), the backlog of recent audit lines, then every new line
//!   as it is written (tool decisions and outcomes, session start and end, approval requests and
//!   answers), one JSON object per line, until either side closes. A controller app turns this into
//!   the agent's step timeline and its approval cards.
//! - `{"pending":true}`: the approvals waiting right now.
//! - `{"answer":{...}}`: allow or deny a waiting call ([`crate::approval::RemoteAnswer`]).
//! - `{"enroll":{"alg":..,"key":..,"label":..}}`: offer an approver key; someone at this host must
//!   confirm it.
//! - `{"approver_status":{"alg":..,"key":..}}`: whether this host accepted that key for this device.
//!
//! Requests other than `subscribe` get one `{"ok":true,...}` or `{"ok":false,"error":"..."}` line.
//! Any device with a level may use the stream; the feed carries what the audit log carries (argument
//! previews, never secret values, which the gate already reduces to sizes).

use std::sync::Arc;

use ar_core::{VERSION, approval::KeyAlg};
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
                let reply = match serde_json::from_str::<Request>(&first) {
                    Ok(Request::Subscribe(_)) => return feed(&host, send).await,
                    Ok(req) => handle(&host, &peer, req).await,
                    Err(e) => Err(anyhow::anyhow!("bad request: {e}")),
                };
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
            });
        }
        Ok(())
    }
}

async fn handle(host: &HostState, peer: &Peer, req: Request) -> anyhow::Result<Value> {
    let approver = host.approver();
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
    })
}

/// Serves a feed subscription until either side closes or the host stops.
async fn feed(host: &HostState, mut send: SendStream) {
    let (backlog, mut live) = host.audit().subscribe();
    let hello = json!({
        "kind": "hello",
        "host": host.name(),
        "device": host.mesh().principal(),
        "version": VERSION,
        "pending": host.approver().pending(),
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
