//! Local control channel of a running host, used by `ai-remote pair-code`, `peers`, `level`,
//! `revoke` and friends on the same machine.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use ar_core::{Level, VERSION};
use ar_net::{
    ipc::Conn,
    refusal::{CLOSE_NO_ACCESS, REASON_NOT_PAIRED},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::HostState;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostRequest {
    Status,
    /// Mint a one-time pairing ticket.
    PairCode {
        level: Level,
        name: Option<String>,
    },
    Peers,
    /// Set or clear (`level: null`) a peer's level.
    SetLevel {
        peer: String,
        level: Option<Level>,
    },
    Revoke {
        peer: String,
    },
    Unrevoke {
        peer: String,
    },
    Forget {
        peer: String,
    },
    /// Run the graded preflight from inside the host (its session, token and network).
    Doctor,
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
            // A restart is verified by a changed pid, not by "it answers".
            "pid": std::process::id(),
            "exe": std::env::current_exe().ok().map(|p| dunce::simplified(&p).to_path_buf()),
            "background": host.background(),
        }),
        HostRequest::Doctor => serde_json::to_value(
            crate::doctor::run(crate::doctor::Inputs { profile: host.profile(), mesh: Some(mesh) }).await,
        )?,
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
            json!({"peer": p.name, "device": p.principal, "revoked": true})
        }
        HostRequest::Unrevoke { peer } => {
            // A device revoked and then forgotten has no pairing left: find it by its device id.
            let (name, device) = match mesh.find_peer(&peer).await {
                Ok(p) => (Some(p.name), p.principal.ok_or_else(|| anyhow!("that peer has no device id"))?),
                Err(e) => match mesh.find_revoked(&peer).await? {
                    Some(d) => (d.name, d.principal),
                    None => return Err(anyhow!("{e:#}; no revoked device matches '{peer}' either")),
                },
            };
            mesh.unrevoke(&device).await?;
            json!({"peer": name.as_deref().unwrap_or(&device), "device": device, "revoked": false, "paired": name.is_some()})
        }
        HostRequest::Forget { peer } => {
            let p = mesh.find_peer(&peer).await?;
            // The transport removes every entry with this name and cuts their connections with its
            // generic "access revoked". Close them first, so the controller hears "not paired: pair
            // again" instead of being told to wait for an unrevoke that cannot happen.
            let devices: Vec<String> =
                mesh.peers().await?.into_iter().filter(|x| x.name == p.name).filter_map(|x| x.principal).collect();
            let closed = host.live().close(&devices, CLOSE_NO_ACCESS, REASON_NOT_PAIRED);
            mesh.forget(&p).await?;
            // Forgetting removes the pairing, not a refusal: a revoked device stays refused.
            json!({
                "peer": p.name,
                "device": p.principal,
                "forgotten": true,
                "closed_connections": closed,
                "still_revoked": p.revoked,
            })
        }
        HostRequest::Shutdown => {
            shutdown.cancel();
            json!({"stopping": true})
        }
    })
}
