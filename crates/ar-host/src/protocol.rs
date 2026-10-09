//! Accepts controller connections on the MCP ALPN and serves one MCP session per bi-stream.
//!
//! Only paired, non-revoked devices reach this handler (the transport gate refuses everyone else);
//! a paired device without a level is closed here.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use ar_net::{
    iroh::{
        endpoint::Connection,
        protocol::{AcceptError, ProtocolHandler},
    },
    // Shared with the controller, which turns them into plain language.
    refusal::{CLOSE_NO_ACCESS, REASON_NO_LEVEL, REASON_NOT_PAIRED, REASON_STOPPING, REASON_UNRESOLVED},
};
use rmcp::ServiceExt;
use tokio_util::sync::CancellationToken;

use crate::{HostState, server::HostServer, session::Session};

/// The controller connections being served, by device. The transport can cut a device's
/// connections too, but only with its own generic reason; this lets the host say why first.
#[derive(Default)]
pub(crate) struct LiveConns {
    next: AtomicU64,
    conns: Mutex<HashMap<u64, (String, Connection)>>,
}

impl LiveConns {
    fn register(&self, device: String, conn: Connection) -> Registration<'_> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.conns.lock().unwrap().insert(id, (device, conn));
        Registration { conns: self, id }
    }

    /// Closes every connection of the given devices (`eid:` principals) with `code` and `reason`;
    /// returns how many were closed. The first close of a connection is the one its peer sees.
    pub(crate) fn close(&self, devices: &[String], code: u32, reason: &[u8]) -> usize {
        let doomed: Vec<Connection> = self
            .conns
            .lock()
            .unwrap()
            .values()
            .filter(|(device, _)| devices.contains(device))
            .map(|(_, conn)| conn.clone())
            .collect();
        for conn in &doomed {
            conn.close(code.into(), reason);
        }
        doomed.len()
    }
}

/// Removes a connection from [`LiveConns`] when its handler ends.
struct Registration<'a> {
    conns: &'a LiveConns,
    id: u64,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.conns.conns.lock().unwrap().remove(&self.id);
    }
}

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
                conn.close(CLOSE_NO_ACCESS.into(), REASON_NOT_PAIRED);
                return Ok(());
            }
            Err(e) => {
                tracing::warn!(%e, "cannot resolve caller");
                conn.close(CLOSE_NO_ACCESS.into(), REASON_UNRESOLVED);
                return Ok(());
            }
        };
        let Some(level) = peer.level else {
            conn.close(CLOSE_NO_ACCESS.into(), REASON_NO_LEVEL);
            return Ok(());
        };
        tracing::info!(peer = %peer.name, %level, "controller connected");
        let _registered = self.host.live().register(format!("eid:{remote}"), conn.clone());

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
                // Tell the controller now instead of letting it wait for an idle timeout.
                _ = self.host.stopping().cancelled() => {
                    conn.close(0u32.into(), REASON_STOPPING);
                    break;
                }
            };
            let session = Arc::new(Session::new(peer.clone(), level, conn_cancel.child_token()));
            let server = HostServer::new(self.host.clone(), session.clone());
            let host = self.host.clone();
            tokio::spawn(async move {
                let (id, name, principal) = (&session.id, &session.peer.name, session.peer.principal.as_deref());
                match server.serve_with_ct((recv, send), session.cancel.clone()).await {
                    Ok(running) => {
                        let person = running
                            .peer()
                            .peer_info()
                            .is_some_and(|i| i.client_info.name.starts_with(ar_core::PERSON_CLIENT));
                        session.set_person(person);
                        let client = if person { "person" } else { "agent" };
                        host.audit().session("session_start", id, name, principal, client);
                        let _ = running.waiting().await;
                        host.audit().session("session_end", id, name, principal, client);
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
