//! Local control channel of a running host, used by `ai-remote pair-code`, `peers`, `level`,
//! `revoke` and friends on the same machine.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use ar_core::{Level, VERSION};
use ar_net::ipc::Conn;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::HostState;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostRequest {
    Status,
    /// Mint a one-time pairing ticket.
    PairCode { level: Level, name: Option<String> },
    Peers,
    /// Set or clear (`level: null`) a peer's level.
    SetLevel { peer: String, level: Option<Level> },
    Revoke { peer: String },
    Unrevoke { peer: String },
    Forget { peer: String },
    Shutdown,
}

pub async fn serve(host: Arc<HostState>, mut conn: Conn, shutdown: CancellationToken) {
    while let Ok(Some(line)) = conn.read_json().await {
        let result = match serde_json::from_value::<HostRequest>(line) {
            Ok(req) => handle(&host, req, &shutdown).await,
            Err(e) => Err(anyhow!("bad request: {e}")),
        };
        if conn.reply(result).await.is_err() {
            break;
        }
    }
}

async fn handle(host: &HostState, req: HostRequest, shutdown: &CancellationToken) -> Result<Value> {
    let mesh = host.mesh();
    Ok(match req {
        HostRequest::Status => json!({
            "name": host.name(),
            "device": mesh.principal(),
            "version": VERSION,
            "peers": mesh.peers().await?.len(),
            "tools": host.registry().iter().map(|t| t.spec().name.clone()).collect::<Vec<_>>(),
            "profile": host.profile().root(),
        }),
        HostRequest::PairCode { level, name } => serde_json::to_value(mesh.invite(level, name).await?)?,
        HostRequest::Peers => serde_json::to_value(mesh.peers().await?)?,
        HostRequest::SetLevel { peer, level } => {
            let p = mesh.find_peer(&peer).await?;
            mesh.set_level(&p, level).await?;
            json!({"peer": p.name, "level": level})
        }
        HostRequest::Revoke { peer } => {
            let p = mesh.find_peer(&peer).await?;
            mesh.revoke(&p, Some("revoked by the host owner".into())).await?;
            json!({"peer": p.name, "revoked": true})
        }
        HostRequest::Unrevoke { peer } => {
            let p = mesh.find_peer(&peer).await?;
            mesh.unrevoke(&p).await?;
            json!({"peer": p.name, "revoked": false})
        }
        HostRequest::Forget { peer } => {
            let p = mesh.find_peer(&peer).await?;
            mesh.revoke(&p, Some("forgotten by the host owner".into())).await?;
            mesh.forget(&p).await?;
            json!({"peer": p.name, "forgotten": true})
        }
        HostRequest::Shutdown => {
            shutdown.cancel();
            json!({"stopping": true})
        }
    })
}
