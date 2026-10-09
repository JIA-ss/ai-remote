//! Local control channel between ai-remote processes of the same user: the CLI talks to a running
//! host or controller daemon, and the stdio MCP shim talks to the controller daemon.
//!
//! Transport: an owner-only named pipe (Windows) or a 0600 Unix socket, from mcpmesh's hardened
//! local transport. Framing: one JSON object per line. The client sends a request
//! `{"op": "...", ...}`; the server answers `{"ok": true, "result": ...}` or
//! `{"ok": false, "error": "..."}`. A request may upgrade the stream (the MCP shim does): after the
//! `ok` line both sides switch to raw bytes.
//!
//! Never rely on half-close here: on Windows a named pipe's shutdown is only a flush, so the other
//! side would not see end-of-stream. A session ends when one side drops the whole stream.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use mcpmesh_local_api::transport as local;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

pub use local::LocalStream;

/// Longest control line accepted (requests and responses are small; MCP traffic is not framed here).
const MAX_LINE: u64 = 4 * 1024 * 1024;

/// A bound control endpoint.
pub struct Listener(local::LocalListener);

impl Listener {
    /// Binds `path`. Fails with `AddrInUse` when another daemon of the same role already runs.
    pub fn bind(path: &Path) -> std::io::Result<Self> {
        local::bind_local(path).map(Self)
    }

    pub async fn accept(&mut self) -> std::io::Result<Conn> {
        let stream = self.0.accept().await?;
        if !local::authorize_local_peer(&stream) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "peer is another user",
            ));
        }
        Ok(Conn::new(stream))
    }
}

/// One control connection (either side).
pub struct Conn {
    io: BufReader<LocalStream>,
}

impl Conn {
    fn new(stream: LocalStream) -> Self {
        Self {
            io: BufReader::new(stream),
        }
    }

    /// Connects to a daemon's endpoint. `NotFound` means no daemon is running.
    pub async fn connect(path: &Path) -> std::io::Result<Self> {
        local::connect_local(path).await.map(Self::new)
    }

    /// Reads one JSON line; `None` at end of stream.
    pub async fn read_json(&mut self) -> Result<Option<Value>> {
        read_json_line(&mut self.io).await
    }

    pub async fn write_json(&mut self, value: &Value) -> Result<()> {
        write_json_line(self.io.get_mut(), value).await
    }

    /// Server side: answers a request.
    pub async fn reply(&mut self, result: Result<Value>) -> Result<()> {
        let line = match result {
            Ok(v) => json!({"ok": true, "result": v}),
            Err(e) => json!({"ok": false, "error": format!("{e:#}")}),
        };
        self.write_json(&line).await
    }

    /// Client side: sends a request and waits for its answer.
    pub async fn request(&mut self, req: &Value) -> Result<Value> {
        self.write_json(req).await?;
        let resp = self
            .read_json()
            .await?
            .context("daemon closed the connection without answering")?;
        if resp.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(resp.get("result").cloned().unwrap_or(Value::Null))
        } else {
            let msg = resp
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            bail!("{msg}")
        }
    }

    /// Typed [`request`](Self::request).
    pub async fn request_as<T: DeserializeOwned>(&mut self, req: &impl Serialize) -> Result<T> {
        let v = self.request(&serde_json::to_value(req)?).await?;
        serde_json::from_value(v).map_err(|e| anyhow!("unexpected answer from daemon: {e}"))
    }

    /// Gives up line framing and returns the raw stream, plus any bytes already buffered.
    pub fn into_raw(self) -> (LocalStream, Vec<u8>) {
        let buffered = self.io.buffer().to_vec();
        (self.io.into_inner(), buffered)
    }
}

async fn read_json_line<R: AsyncBufRead + Unpin>(r: &mut R) -> Result<Option<Value>> {
    let mut line = String::new();
    let n = tokio::io::AsyncReadExt::take(&mut *r, MAX_LINE)
        .read_line(&mut line)
        .await?;
    if n == 0 {
        return Ok(None);
    }
    if !line.ends_with('\n') && n as u64 >= MAX_LINE {
        bail!("control line too long");
    }
    Ok(Some(
        serde_json::from_str(line.trim()).context("malformed control line")?,
    ))
}

async fn write_json_line<W: AsyncWrite + Unpin>(w: &mut W, value: &Value) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    w.write_all(&bytes).await?;
    w.flush().await?;
    Ok(())
}
