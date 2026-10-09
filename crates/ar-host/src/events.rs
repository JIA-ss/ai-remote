//! The live activity feed: paired controllers subscribe on `app/ai-remote/events/1` and receive the
//! host's recent audit lines, then every new line as it is written (tool decisions and outcomes,
//! session start and end). A controller app turns this into the agent's step timeline.
//!
//! Wire format: the controller opens one bi-stream and writes one JSON line (`{"subscribe":true}`;
//! a QUIC stream only reaches the host once something is written). The host answers with a
//! `{"kind":"hello",...}` line, the backlog, then live lines, one JSON object per line, until either
//! side closes. Any device with a level may subscribe; the feed carries what the audit log carries
//! (argument previews, never secret values, which the gate already reduces to sizes).

use std::sync::Arc;

use ar_core::VERSION;
use ar_net::{
    iroh::{
        endpoint::Connection,
        protocol::{AcceptError, ProtocolHandler},
    },
    refusal::{CLOSE_NO_ACCESS, REASON_NO_LEVEL, REASON_NOT_PAIRED},
};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    sync::broadcast::error::RecvError,
};

use crate::HostState;

/// Longest subscribe line accepted.
const MAX_REQUEST: u64 = 4096;

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
            tokio::spawn(async move {
                // The subscribe line only announces the stream; its content is not needed yet.
                let mut first = String::new();
                let mut reader = BufReader::new(recv.take(MAX_REQUEST));
                if reader.read_line(&mut first).await.unwrap_or(0) == 0 {
                    return;
                }
                let (backlog, mut live) = host.audit().subscribe();
                let hello = json!({"kind": "hello", "host": host.name(), "version": VERSION}).to_string();
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
            });
        }
        Ok(())
    }
}

/// One JSON line with its newline.
fn framed(line: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(line.len() + 1);
    bytes.extend_from_slice(line.as_bytes());
    bytes.push(b'\n');
    bytes
}
