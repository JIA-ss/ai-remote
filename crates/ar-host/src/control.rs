//! Local control channel of a running host, used by `ai-remote pair-code`, `peers`, `level`,
//! `revoke`, `approvers` and friends on the same machine.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use ar_core::{Level, VERSION, approval::KeyAlg};
use ar_net::{
    Peer,
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
    /// Approver keys this host accepted.
    Approvers,
    /// Accept an approver key for a paired device without a prompt (`ai-remote approvers add`, run
    /// by someone at this computer).
    AddApprover {
        peer: String,
        alg: KeyAlg,
        /// Base64 DER SubjectPublicKeyInfo.
        key: String,
        #[serde(default)]
        label: Option<String>,
    },
    /// Remove a device's approver keys: all of them, or the one whose fingerprint starts with
    /// `fingerprint`.
    RemoveApprover {
        peer: String,
        #[serde(default)]
        fingerprint: Option<String>,
    },
    /// Run the graded preflight from inside the host (its session, token and network).
    Doctor,
    /// The paired controllers and whether the owner bypasses each (see [`crate::bypass`]).
    Bypass,
    /// Turn bypass on (until `for_secs` from now, when given) or off for a paired controller. Only
    /// here, at this computer: no controller can ask for it.
    SetBypass {
        peer: String,
        on: bool,
        #[serde(default)]
        for_secs: Option<u64>,
    },
    /// Turn the bypass default on or off: on, every paired controller (those paired later included)
    /// is bypassed unless the owner turns it off for that one. Only here, at this computer.
    SetBypassDefault {
        on: bool,
    },
    /// Whether coding agents may start here, and the sessions (see [`crate::chat`]).
    Chat,
    /// Allow or forbid starting coding agents here; forbidding ends every session. Only here, at
    /// this computer: it holds for bypassed controllers too.
    SetChatEnabled {
        on: bool,
    },
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

/// Turns off the bypass of the device `principal` (named `name` here), saying why in the audit log;
/// whether it was on. While the default is on, the device is excepted from it (`except`), unless it
/// is being forgotten: a device paired again starts from the default.
pub(crate) fn end_bypass(host: &HostState, name: &str, principal: &str, why: &str, except: bool) -> bool {
    match host.bypass().clear(principal, name, except) {
        Ok(true) => {
            tracing::info!(peer = %name, why, "bypass off");
            host.audit().event(
                json!({"kind": "bypass", "peer": name, "principal": principal, "on": false, "why": why, "by": "host"}),
            );
            true
        }
        Ok(false) => false,
        Err(e) => {
            tracing::warn!(peer = %name, "could not turn bypass off: {e:#}");
            false
        }
    }
}

/// Removes `p`'s pairing (every entry with its name), its live connections and its approver keys.
/// Used by `ai-remote forget` here and by a controller that unpairs itself.
pub(crate) async fn forget(host: &HostState, p: &Peer) -> Result<Value> {
    let mesh = host.mesh();
    // The transport removes every entry with this name and cuts their connections with its
    // generic "access revoked". Close them first, so the controller hears "not paired: pair
    // again" instead of being told to wait for an unrevoke that cannot happen.
    let devices: Vec<String> =
        mesh.peers().await?.into_iter().filter(|x| x.name == p.name).filter_map(|x| x.principal).collect();
    let closed = host.live().close(&devices, CLOSE_NO_ACCESS, REASON_NOT_PAIRED);
    mesh.forget(p).await?;
    // A forgotten device's approver keys and bypass go with it; pairing again starts from nothing.
    let mut approver_keys = 0;
    for device in &devices {
        approver_keys += host.approver().keys().remove(device, None)?;
        end_bypass(host, &p.name, device, "forgotten", false);
    }
    // Forgetting removes the pairing, not a refusal: a revoked device stays refused.
    Ok(json!({
        "peer": p.name,
        "device": p.principal,
        "forgotten": true,
        "closed_connections": closed,
        "still_revoked": p.revoked,
        "approver_keys_removed": approver_keys,
    }))
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
        HostRequest::Peers => {
            let bypassed = host.bypass().list();
            let peers: Vec<Value> = mesh
                .peers()
                .await?
                .into_iter()
                .map(|p| {
                    let entry = bypassed.iter().find(|e| p.principal.as_deref() == Some(e.principal.as_str()));
                    let mut v = serde_json::to_value(&p).unwrap_or_default();
                    v["bypass"] = host.bypass().covers(&p).into();
                    v["bypass_until_ms"] = json!(entry.and_then(|e| e.until_ms));
                    v
                })
                .collect();
            Value::Array(peers)
        }
        HostRequest::SetLevel { peer, level } => {
            let p = mesh.find_peer(&peer).await?;
            mesh.set_level(&p, level).await?;
            if level.is_none()
                && let Some(device) = &p.principal
            {
                end_bypass(host, &p.name, device, "level none", true);
            }
            json!({"peer": p.name, "level": level})
        }
        HostRequest::Revoke { peer } => {
            let p = mesh.find_peer(&peer).await?;
            mesh.revoke(&p, Some("revoked by the host owner".into())).await?;
            if let Some(device) = &p.principal {
                end_bypass(host, &p.name, device, "revoked", true);
            }
            json!({"peer": p.name, "device": p.principal, "revoked": true})
        }
        HostRequest::Bypass => {
            let bypass = host.bypass();
            let bypassed = bypass.list();
            let devices: Vec<Value> = mesh
                .peers()
                .await?
                .into_iter()
                .map(|p| {
                    let entry = bypassed.iter().find(|e| p.principal.as_deref() == Some(e.principal.as_str()));
                    let coverage = bypass.coverage(&p);
                    json!({
                        "name": p.name,
                        "device": p.principal,
                        "level": p.level,
                        "revoked": p.revoked,
                        // Effective: its own entry or the default.
                        "bypass": coverage.covered(),
                        "how": coverage,
                        "since_ms": entry.map(|e| e.since_ms),
                        "until_ms": entry.and_then(|e| e.until_ms),
                    })
                })
                .collect();
            json!({"default": bypass.default_on(), "devices": devices})
        }
        HostRequest::SetBypassDefault { on } => {
            let changed = host.bypass().set_default(on)?;
            if changed {
                if on {
                    tracing::warn!(
                        "bypass default on: every paired controller's calls skip levels, policy and approval"
                    );
                } else {
                    tracing::info!("bypass default off");
                }
                host.audit().event(json!({"kind": "bypass", "default": on, "on": on, "by": "host"}));
            }
            json!({"default": on, "changed": changed})
        }
        HostRequest::Chat => json!({
            "enabled": host.chat().enabled(),
            "sessions": host.chat().list(),
            "live": host.chat().live_count(),
            "can_start": host.chat().blocked().is_none(),
            "reason": host.chat().blocked(),
        }),
        HostRequest::SetChatEnabled { on } => {
            let changed = host.chat().enabled() != on;
            host.chat().set_enabled(on)?;
            if changed {
                tracing::info!(on, "coding agents switched {}", if on { "on" } else { "off" });
            }
            json!({"enabled": on, "changed": changed})
        }
        HostRequest::SetBypass { peer, on, for_secs } => {
            let p = mesh.find_peer(&peer).await?;
            let device = p.principal.clone().ok_or_else(|| anyhow!("{} has no device id", p.name))?;
            if !on {
                let was = end_bypass(host, &p.name, &device, "turned off", true);
                return Ok(json!({"peer": p.name, "bypass": false, "changed": was}));
            }
            if p.revoked {
                return Err(anyhow!("{} is revoked here; unrevoke it first (ai-remote unrevoke {})", p.name, p.name));
            }
            if p.level.is_none() {
                return Err(anyhow!(
                    "{} has no level here, so it cannot connect at all; give it one first (ai-remote level {} observe)",
                    p.name,
                    p.name
                ));
            }
            let until_ms = for_secs.map(|s| crate::audit::now_ms() as u64 + s.saturating_mul(1000));
            let entry = host.bypass().set(&p, until_ms)?;
            tracing::warn!(peer = %p.name, ?until_ms, "bypass on: this controller's calls skip levels, policy and approval");
            host.audit().event(json!({
                "kind": "bypass",
                "peer": p.name,
                "principal": device,
                "on": true,
                "until_ms": entry.until_ms,
                "by": "host",
            }));
            json!({"peer": p.name, "bypass": true, "until_ms": entry.until_ms})
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
        HostRequest::Forget { peer } => forget(host, &mesh.find_peer(&peer).await?).await?,
        HostRequest::Approvers => {
            let keys = host.approver().keys().list();
            let keys: Vec<Value> = keys
                .iter()
                .map(|k| {
                    json!({
                        "device": k.device,
                        "principal": k.principal,
                        "alg": k.alg,
                        "label": k.label,
                        "fingerprint": k.fingerprint(),
                        "added": k.added,
                    })
                })
                .collect();
            json!({"approvers": keys, "approval_prompt": host.config().host.approval_prompt})
        }
        HostRequest::AddApprover { peer, alg, key, label } => {
            let p = mesh.find_peer(&peer).await?;
            let principal = p.principal.as_deref().ok_or_else(|| anyhow!("that peer has no device id"))?;
            let der = data_encoding::BASE64
                .decode(key.trim().as_bytes())
                .map_err(|_| anyhow!("the key is not base64 (copy it from `ai-remote approver` on that device)"))?;
            let k = crate::approvers::ApproverKey::new(principal, &p.name, alg, &der, label.as_deref().unwrap_or(""))?;
            let fingerprint = k.fingerprint();
            let added = host.approver().keys().add(k)?;
            host.audit().event(json!({
                "kind": "approver_added",
                "peer": p.name,
                "principal": principal,
                "alg": alg,
                "fingerprint": fingerprint,
                "via": "host command",
            }));
            json!({"peer": p.name, "fingerprint": fingerprint, "added": added})
        }
        HostRequest::RemoveApprover { peer, fingerprint } => {
            // By pairing name or device id; a device whose pairing is gone is matched by its id or
            // the name it had when its key was added.
            let principal = match mesh.find_peer(&peer).await {
                Ok(p) => p.principal.ok_or_else(|| anyhow!("that peer has no device id"))?,
                Err(e) => host
                    .approver()
                    .keys()
                    .list()
                    .into_iter()
                    .find(|k| k.device == peer || k.principal == peer)
                    .map(|k| k.principal)
                    .ok_or_else(|| anyhow!("{e:#}; no approver key matches '{peer}' either"))?,
            };
            let removed = host.approver().keys().remove(&principal, fingerprint.as_deref())?;
            if removed > 0 {
                host.audit().event(json!({"kind": "approver_removed", "principal": principal, "removed": removed}));
            }
            json!({"peer": peer, "removed": removed})
        }
        HostRequest::Shutdown => {
            shutdown.cancel();
            json!({"stopping": true})
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypass_requests_as_the_desktop_app_and_the_cli_send_them() {
        let app: HostRequest =
            serde_json::from_value(json!({"op": "set_bypass", "peer": "laptop", "on": true})).unwrap();
        assert!(matches!(app, HostRequest::SetBypass { on: true, for_secs: None, .. }));
        let cli = serde_json::to_value(HostRequest::SetBypass { peer: "laptop".into(), on: false, for_secs: Some(60) })
            .unwrap();
        assert_eq!(cli, json!({"op": "set_bypass", "peer": "laptop", "on": false, "for_secs": 60}));
        assert!(matches!(serde_json::from_value(json!({"op": "bypass"})).unwrap(), HostRequest::Bypass));
    }
}
