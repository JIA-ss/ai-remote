//! Accepts controller connections on the MCP ALPN and serves one MCP session per bi-stream.
//!
//! Only paired, non-revoked devices reach this handler (the transport gate refuses everyone else);
//! a paired device without a level is closed here.

use std::sync::Arc;

use ar_net::iroh::{
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler},
};
use rmcp::ServiceExt;
use tokio_util::sync::CancellationToken;

use crate::{HostState, server::HostServer, session::Session};

const CLOSE_NO_ACCESS: u32 = 403;

pub struct McpProtocol {
    host: Arc<HostState>,
}

impl std::fmt::Debug for McpProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("McpProtocol")
    }
}

impl McpProtocol {
    pub fn new(host: Arc<HostState>) -> Self {
        Self { host }
    }
}

impl ProtocolHandler for McpProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let remote = conn.remote_id();
        let peer = match self.host.mesh().peer_for(&remote).await {
            Ok(Some(peer)) => peer,
            Ok(None) => {
                conn.close(CLOSE_NO_ACCESS.into(), b"not paired");
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(%e, "cannot resolve caller");
                conn.close(CLOSE_NO_ACCESS.into(), b"cannot resolve caller");
                return Ok(());
            }
        };
        let Some(level) = peer.level else {
            conn.close(CLOSE_NO_ACCESS.into(), b"no access level granted");
            return Ok(());
        };
        tracing::info!(peer = %peer.name, %level, "controller connected");

        // Everything this connection started ends with it (including revocation, which closes it).
        let conn_cancel = CancellationToken::new();
        {
            let conn = conn.clone();
            let cancel = conn_cancel.clone();
            tokio::spawn(async move {
                conn.closed().await;
                cancel.cancel();
            });
        }

        loop {
            let (send, recv) = tokio::select! {
                s = conn.accept_bi() => match s {
                    Ok(s) => s,
                    Err(_) => break,
                },
                _ = conn_cancel.cancelled() => break,
            };
            let session = Arc::new(Session::new(peer.clone(), level, conn_cancel.child_token()));
            let server = HostServer::new(self.host.clone(), session.clone());
            tokio::spawn(async move {
                match server.serve_with_ct((recv, send), session.cancel.clone()).await {
                    Ok(running) => {
                        let _ = running.waiting().await;
                    }
                    Err(e) => tracing::debug!(%e, "MCP session did not start"),
                }
                // Stop anything still running for this session.
                session.cancel.cancel();
            });
        }
        tracing::info!(peer = %peer.name, "controller disconnected");
        Ok(())
    }
}
