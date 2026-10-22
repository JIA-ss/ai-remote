//! `app/ai-remote/chat/1`: a conversation with a coding agent running on this host (see
//! [`crate::chat`]).
//!
//! The controller opens a bi-stream and writes one JSON line, `{"open":{"ticket":"..","since":N}}`,
//! with a ticket from `chat_start` or `chat_attach`. The host answers with one line,
//! `{"ok":true,"session":"..","role":"rw"|"ro",...}` or `{"ok":false,"error":".."}`, then sends the
//! session's events after `since` (from its transcript) and every new one, one JSON line each.
//!
//! The controller sends one JSON line per command, any time:
//!
//! - `{"input":{"id":"..","text":"..","mode":"queue"|"interrupt"|"steer"}}`: a message for the
//!   agent; with `interrupt` the running turn stops and this message goes first; with `steer` it
//!   joins the running turn (a `user` event with `"steer":true`, numbered with that turn) when the
//!   agent can take it (`caps.steer`), and is queued with a `steer_queued` notice otherwise;
//! - `{"interrupt":{}}`, `{"answer":{"qid":"..","text":".."}}`, `{"dequeue":{"id":".."}}`;
//! - `{"end":{}}`: end the session (its agent process stops);
//! - `{"bye":{}}`: stop watching (the session goes on); `{"ping":N}` is answered with `{"pong":N}`.
//!
//! Neither side relies on the other closing its half of the stream (Windows named pipes on the
//! controller's side cannot): `bye` says it. A ticket is redeemed only by the device it was issued
//! to, and every command checks again that the device still has level admin; `ro` tickets (a
//! session another device started) may only watch.

use std::{
    sync::{Arc, Weak},
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use ar_core::Level;
use ar_net::{
    Peer,
    iroh::{
        endpoint::{Connection, RecvStream, SendStream},
        protocol::{AcceptError, ProtocolHandler},
    },
    refusal::{CLOSE_NO_ACCESS, REASON_NO_LEVEL, REASON_NOT_PAIRED},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    sync::{broadcast::error::RecvError, mpsc},
};

use crate::{
    HostState,
    chat::{Chat, Cmd, Input, MAX_INPUT, Role, event::By, line_seq, random_hex},
};

/// Longest command line accepted from a controller.
const MAX_LINE: usize = 1024 * 1024;
/// The opening line must arrive within this.
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
struct Open {
    open: OpenArgs,
}

#[derive(Debug, Deserialize)]
struct OpenArgs {
    ticket: String,
    #[serde(default)]
    since: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Command {
    Input {
        id: Option<String>,
        text: String,
        #[serde(default)]
        mode: Option<String>,
    },
    Interrupt {},
    Answer {
        qid: String,
        text: String,
    },
    Dequeue {
        id: String,
    },
    End {},
    Bye {},
    Ping(Value),
}

pub struct ChatProtocol {
    host: Arc<HostState>,
}

impl std::fmt::Debug for ChatProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChatProtocol")
    }
}

impl ChatProtocol {
    pub fn new(host: Arc<HostState>) -> Self {
        Self { host }
    }
}

impl ProtocolHandler for ChatProtocol {
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
        while let Ok((send, recv)) = conn.accept_bi().await {
            let host = self.host.clone();
            let peer = peer.clone();
            tokio::spawn(async move { serve(host, peer, send, recv).await });
        }
        Ok(())
    }
}

async fn write_line(send: &mut SendStream, line: &str) -> bool {
    send.write_all(line.as_bytes()).await.is_ok() && send.write_all(b"\n").await.is_ok()
}

/// Reads JSON lines of at most [`MAX_LINE`] bytes from `recv` into a channel.
fn read_lines(recv: RecvStream) -> mpsc::Receiver<Result<String, String>> {
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        let mut reader = BufReader::new(recv);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match (&mut reader).take(MAX_LINE as u64 + 1).read_until(b'\n', &mut buf).await {
                Ok(0) => break,
                Ok(_) if buf.len() > MAX_LINE => {
                    let _ = tx.send(Err(format!("a line over {} KiB", MAX_LINE >> 10))).await;
                    break;
                }
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf).trim().to_string();
                    if !line.is_empty() && tx.send(Ok(line)).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e.to_string())).await;
                    break;
                }
            }
        }
    });
    rx
}

async fn serve(host: Arc<HostState>, peer: Peer, mut send: SendStream, recv: RecvStream) {
    let mut lines = read_lines(recv);
    let opened = match tokio::time::timeout(OPEN_TIMEOUT, lines.recv()).await {
        Ok(Some(Ok(line))) => open(&host, &peer, &line).await,
        Ok(_) => Err(anyhow!("bad request: expected one JSON line with a ticket")),
        Err(_) => Err(anyhow!("no request within {} s", OPEN_TIMEOUT.as_secs())),
    };
    let (chat, role, since) = match opened {
        Ok(v) => v,
        Err(e) => {
            write_line(&mut send, &json!({"ok": false, "error": format!("{e:#}")}).to_string()).await;
            let _ = send.finish();
            return;
        }
    };
    let (backlog, upto, mut live) = chat.follow(since);
    let meta = chat.meta();
    let caps = host.chat().runtimes(false).await["runtimes"]
        .as_array()
        .and_then(|rs| rs.iter().find(|r| r["id"] == meta.runtime.as_str()).map(|r| r["caps"].clone()))
        .unwrap_or(Value::Null);
    let hello = json!({
        "ok": true, "session": chat.id, "role": role, "runtime": {"id": meta.runtime, "name": meta.name},
        "cwd": meta.cwd, "state": meta.state, "seq": upto, "caps": caps, "by": meta.by,
    });
    if !write_line(&mut send, &hello.to_string()).await {
        return;
    }
    host.audit().event(json!({"kind": "chat_session", "action": "attach", "chat": chat.id, "peer": peer.name, "principal": peer.principal, "role": role}));
    let mut last = upto;
    for line in backlog {
        if !write_line(&mut send, &line).await {
            return;
        }
    }
    let why = loop {
        tokio::select! {
            ev = live.recv() => match ev {
                Ok(line) => {
                    let Some(seq) = line_seq(&line) else { continue };
                    if seq <= last {
                        continue;
                    }
                    last = seq;
                    if !write_line(&mut send, &line).await {
                        break "the controller went away";
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    // Too slow for the live feed: catch up from the transcript.
                    let (lines, upto, rx) = chat.follow(last);
                    live = rx;
                    for line in lines {
                        if !write_line(&mut send, &line).await {
                            return;
                        }
                    }
                    last = upto;
                }
                Err(RecvError::Closed) => break "the session ended",
            },
            line = lines.recv() => match line {
                Some(Ok(line)) => match command(&host, &peer, &chat, role, &line).await {
                    Ok(Some(reply)) => {
                        if !write_line(&mut send, &reply.to_string()).await {
                            break "the controller went away";
                        }
                    }
                    Ok(None) => {}
                    Err(Stop) => break "bye",
                },
                Some(Err(e)) => {
                    write_line(&mut send, &json!({"t": "error", "text": e}).to_string()).await;
                    break "bad input";
                }
                None => break "the controller went away",
            },
            _ = host.stopping().cancelled() => break "the host is stopping",
        }
    };
    write_line(&mut send, &json!({"t": "bye", "reason": why}).to_string()).await;
    let _ = send.finish();
    host.audit()
        .event(json!({"kind": "chat_session", "action": "detach", "chat": chat.id, "peer": peer.name, "reason": why}));
}

/// Redeems the ticket in the opening line.
async fn open(host: &HostState, peer: &Peer, line: &str) -> Result<(Arc<Chat>, Role, u64)> {
    let req: Open = serde_json::from_str(line).map_err(|e| anyhow!("bad request: {e}"))?;
    let ticket = host.chat().redeem(&req.open.ticket, peer.principal.as_deref())?;
    require_admin(host, peer).await?;
    let chat = host.chat().get(&ticket.chat)?;
    Ok((chat, ticket.role, req.open.since))
}

async fn require_admin(host: &HostState, peer: &Peer) -> Result<()> {
    match host.effective_level(peer).await {
        Some(Level::Admin) => Ok(()),
        Some(level) => bail!("coding agents require level 'admin', but this device has '{level}' now"),
        None => bail!("This device no longer has access to this host."),
    }
}

/// The controller said bye.
struct Stop;

/// Applies one command; returns a line to send back, if any.
async fn command(host: &HostState, peer: &Peer, chat: &Chat, role: Role, line: &str) -> Result<Option<Value>, Stop> {
    let cmd: Command = match serde_json::from_str(line) {
        Ok(c) => c,
        Err(e) => return Ok(Some(json!({"t": "error", "text": format!("unknown command: {e}")}))),
    };
    match cmd {
        Command::Bye {} => return Err(Stop),
        Command::Ping(n) => return Ok(Some(json!({"pong": n}))),
        _ => {}
    }
    if role == Role::Ro {
        return Ok(Some(json!({"t": "error", "code": "readonly", "text": "this device may only watch this session"})));
    }
    if let Err(e) = require_admin(host, peer).await {
        return Ok(Some(json!({"t": "error", "text": format!("{e:#}")})));
    }
    let by = By { device: peer.name.clone(), principal: peer.principal.clone() };
    let sent = match cmd {
        Command::Input { id, text, mode } => {
            if text.trim().is_empty() {
                return Ok(Some(json!({"t": "error", "text": "empty message"})));
            }
            if text.len() > MAX_INPUT {
                return Ok(Some(
                    json!({"t": "error", "text": format!("the message is longer than {MAX_INPUT} bytes")}),
                ));
            }
            let id = id.filter(|i| crate::chat::valid_token(i)).unwrap_or_else(|| format!("u{}", &random_hex()[..8]));
            let input = Input { id, text, by };
            match mode.as_deref() {
                Some("interrupt") => chat.send(Cmd::Interrupt { then: Some(input) }),
                Some("steer") => chat.send(Cmd::Steer(input)),
                _ => chat.send(Cmd::Input(input)),
            }
        }
        Command::Interrupt {} => chat.send(Cmd::Interrupt { then: None }),
        Command::Answer { qid, text } => chat.send(Cmd::Answer { qid, text }),
        Command::Dequeue { id } => chat.send(Cmd::Dequeue { id }),
        Command::End {} => chat.send(Cmd::End { reason: format!("ended by {}", peer.name) }),
        Command::Bye {} | Command::Ping(_) => unreachable!(),
    };
    if !sent {
        return Ok(Some(json!({"t": "error", "code": "ended", "text": "this session has ended"})));
    }
    Ok(None)
}

/// Every few seconds: ends sessions whose device lost level admin, and stops running turns while
/// the computer is paused. Ends with the host.
pub async fn enforce(host: Weak<HostState>) {
    let mut tick = tokio::time::interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        let Some(host) = host.upgrade() else { return };
        if host.stopping().is_cancelled() {
            return;
        }
        let paused = match host.turns().driver() {
            crate::turns::Driver::Paused { by, .. } => Some(by),
            _ => None,
        };
        host.chat().set_paused(paused);
        let Ok(peers) = host.mesh().peers().await else { continue };
        host.chat().enforce(|p| {
            let level =
                peers.iter().find(|x| x.principal.is_some() && x.principal == p.principal).and_then(|x| x.level);
            match level {
                Some(Level::Admin) => true,
                Some(_) => host.bypass().covers(p),
                None => false,
            }
        });
    }
}
