//! `app/ai-remote/file/1`: the bytes of a file move approved with `files_push` or `files_pull`
//! (see [`crate::tools::files`]).
//!
//! The controller opens a bi-stream and writes one JSON line, `{"ticket":"<id>"}`. The host answers
//! with one line, `{"ok":true,"op":"push","size":N}` or `{"ok":true,"op":"pull","size":N}`, or
//! `{"ok":false,"error":".."}`, and then:
//!
//! - push: the controller sends exactly N bytes. The host writes them to a temporary file next to
//!   the target, checks size and SHA-256 against what was approved, moves the file into place
//!   (replacing an existing one only when that was approved) and answers
//!   `{"ok":true,"bytes":N,"sha256":".."}` or `{"ok":false,"error":".."}`.
//! - pull: N bytes follow the reply, then one line `{"sha256":".."}` for the controller to check.
//!   Fewer bytes than announced (the file shrank, a read failed) end the stream early.
//!
//! Neither side relies on the other closing its half of the stream: sizes are known up front.
//!
//! A ticket is redeemed only by the device it was issued to, whose level must still be admin; an
//! agent's transfer also needs the agent to have the controls, and a takeover or pause stops it
//! midway, like any call. Each transfer is recorded in the audit log (`transfer`).

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
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
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};

use crate::{
    HostState,
    tools::files::{Direction, Ticket},
};

/// A transfer whose peer sends or reads nothing for this long is abandoned.
const IDLE: Duration = Duration::from_secs(120);
/// Bytes per read and write.
const CHUNK: usize = 256 * 1024;

#[derive(Debug, Deserialize)]
struct Request {
    ticket: String,
}

pub struct FilesProtocol {
    host: Arc<HostState>,
}

impl std::fmt::Debug for FilesProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FilesProtocol")
    }
}

impl FilesProtocol {
    pub fn new(host: Arc<HostState>) -> Self {
        Self { host }
    }
}

impl ProtocolHandler for FilesProtocol {
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

async fn reply(send: &mut SendStream, value: Value) -> bool {
    let mut line = value.to_string();
    line.push('\n');
    send.write_all(line.as_bytes()).await.is_ok()
}

async fn serve(host: Arc<HostState>, peer: Peer, mut send: SendStream, recv: RecvStream) {
    let mut reader = BufReader::with_capacity(CHUNK, recv);
    let ticket = match admit(&host, &peer, &mut reader).await {
        Ok(t) => t,
        Err(e) => {
            reply(&mut send, json!({"ok": false, "error": format!("{e:#}")})).await;
            let _ = send.finish();
            return;
        }
    };
    let grant = match host.turns().admits(ticket.person, Level::Admin, host.name()) {
        Ok(grant) => grant,
        Err(why) => {
            reply(&mut send, json!({"ok": false, "error": why})).await;
            let _ = send.finish();
            return;
        }
    };
    let started = Instant::now();
    let moved = tokio::select! {
        biased;
        _ = grant.cancelled() => Err(anyhow!(
            "stopped: control changed during the transfer (a person took over or paused the agent)"
        )),
        _ = host.stopping().cancelled() => Err(anyhow!("stopped: the host is stopping")),
        r = transfer(&ticket, &mut send, &mut reader) => r,
    };
    let (outcome, bytes, error) = match &moved {
        Ok(Moved { bytes, .. }) => ("ok", Some(*bytes), None),
        Err(e) => ("error", None, Some(format!("{e:#}"))),
    };
    match moved {
        // A push answers with its result; a pull's bytes and digest are already sent.
        Ok(Moved { reply: Some(v), .. }) => {
            reply(&mut send, v).await;
        }
        Err(e) if matches!(ticket.direction, Direction::Push { .. }) => {
            reply(&mut send, json!({"ok": false, "error": format!("{e:#}")})).await;
        }
        Err(_) => {
            // Ends a pull short: the controller sees fewer bytes than announced.
            let _ = send.reset(0u32.into());
        }
        Ok(_) => {}
    }
    let _ = send.finish();
    host.audit().event(json!({
        "kind": "transfer",
        "tool": ticket.tool,
        "session": ticket.session,
        "peer": ticket.peer,
        "principal": ticket.principal,
        "path": display(&ticket.path),
        "outcome": outcome,
        "bytes": bytes,
        "duration_ms": started.elapsed().as_millis() as u64,
        "error": error,
    }));
}

/// Reads the request and redeems its ticket for this device, whose level must still be admin.
async fn admit<R: AsyncBufRead + Unpin>(host: &HostState, peer: &Peer, reader: &mut R) -> Result<Ticket> {
    let mut line = Vec::new();
    let read = tokio::time::timeout(IDLE, (&mut *reader).take(4096).read_until(b'\n', &mut line)).await;
    match read {
        Ok(Ok(n)) if n > 0 && line.ends_with(b"\n") => {}
        Ok(Ok(_)) => bail!("bad request: expected one JSON line with a ticket"),
        Ok(Err(e)) => bail!("bad request: {e}"),
        Err(_) => bail!("no request within {} s", IDLE.as_secs()),
    }
    let req: Request = serde_json::from_slice(&line).map_err(|e| anyhow!("bad request: {e}"))?;
    let ticket = host.transfers().redeem(&req.ticket, peer.principal.as_deref())?;
    match host.effective_level(peer).await {
        Some(Level::Admin) => Ok(ticket),
        Some(level) => bail!("moving files requires level 'admin', but this device has '{level}' now"),
        None => bail!("This device no longer has access to this host."),
    }
}

struct Moved {
    bytes: u64,
    /// The push's final answer.
    reply: Option<Value>,
}

async fn transfer<R: AsyncRead + Unpin>(ticket: &Ticket, send: &mut SendStream, reader: &mut R) -> Result<Moved> {
    match &ticket.direction {
        Direction::Push { size, sha256, overwrite } => receive(ticket, *size, sha256, *overwrite, send, reader).await,
        Direction::Pull => deliver(&ticket.path, send).await,
    }
}

fn display(p: &Path) -> String {
    dunce::simplified(p).display().to_string()
}

/// Deletes a temporary file unless it was moved into place.
struct PartFile(Option<PathBuf>);

impl Drop for PartFile {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

async fn receive<R: AsyncRead + Unpin>(
    ticket: &Ticket,
    size: u64,
    sha256: &str,
    overwrite: bool,
    send: &mut SendStream,
    reader: &mut R,
) -> Result<Moved> {
    let target = &ticket.path;
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        bail!("{} names no file", display(target));
    };
    if !overwrite && tokio::fs::try_exists(target).await.unwrap_or(false) {
        bail!("{} appeared since the push was approved; it was not replaced", display(target));
    }
    let part = dir.join(format!(".{}.ai-remote-{}.part", name.to_string_lossy(), &ticket.id[..8]));
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)
        .await
        .with_context(|| format!("cannot write in {}", display(dir)))?;
    let mut guard = PartFile(Some(part.clone()));
    if !reply(send, json!({"ok": true, "op": "push", "size": size})).await {
        bail!("the controller went away");
    }
    let mut out = BufWriter::with_capacity(CHUNK * 4, file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut got = 0u64;
    while got < size {
        let want = (size - got).min(CHUNK as u64) as usize;
        let n = match tokio::time::timeout(IDLE, reader.read(&mut buf[..want])).await {
            Ok(Ok(0)) => bail!("the upload ended after {got} of {size} bytes"),
            Ok(Ok(n)) => n,
            Ok(Err(e)) => bail!("the upload broke off after {got} of {size} bytes: {e}"),
            Err(_) => bail!("no data for {} s after {got} of {size} bytes", IDLE.as_secs()),
        };
        hasher.update(&buf[..n]);
        out.write_all(&buf[..n]).await.with_context(|| format!("cannot write {}", display(&part)))?;
        got += n as u64;
    }
    out.flush().await?;
    let file = out.into_inner();
    file.sync_all().await.with_context(|| format!("cannot write {}", display(&part)))?;
    drop(file);
    let digest = data_encoding::HEXLOWER.encode(&hasher.finalize());
    if digest != sha256 {
        bail!("the received file's SHA-256 is {digest}, not the approved {sha256}; it was not kept");
    }
    if !overwrite && tokio::fs::try_exists(target).await.unwrap_or(false) {
        bail!("{} appeared during the upload; it was not replaced", display(target));
    }
    tokio::fs::rename(&part, target).await.map_err(|e| {
        anyhow!("cannot move the file into place at {}: {e} (is it open in another program?)", display(target))
    })?;
    guard.0 = None;
    Ok(Moved { bytes: got, reply: Some(json!({"ok": true, "bytes": got, "sha256": digest, "path": display(target)})) })
}

async fn deliver(path: &Path, send: &mut SendStream) -> Result<Moved> {
    let file = tokio::fs::File::open(path).await.with_context(|| format!("cannot read {}", display(path)))?;
    let md = file.metadata().await?;
    if !md.is_file() {
        bail!("{} is not a file", display(path));
    }
    let size = md.len();
    if !reply(send, json!({"ok": true, "op": "pull", "size": size})).await {
        bail!("the controller went away");
    }
    let mut file = file.take(size);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    let mut sent = 0u64;
    loop {
        let n = file.read(&mut buf).await.with_context(|| format!("cannot read {}", display(path)))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        match tokio::time::timeout(IDLE, send.write_all(&buf[..n])).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => bail!("the download broke off after {sent} of {size} bytes: {e}"),
            Err(_) => bail!("the controller took nothing for {} s", IDLE.as_secs()),
        }
        sent += n as u64;
    }
    if sent != size {
        bail!("{} shrank to {sent} bytes while it was read", display(path));
    }
    let digest = data_encoding::HEXLOWER.encode(&hasher.finalize());
    if !reply(send, json!({"sha256": digest})).await {
        bail!("the controller went away");
    }
    Ok(Moved { bytes: sent, reply: None })
}
