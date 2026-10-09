//! The gate every tool call passes through, in this order:
//!
//! 1. the caller's current level (re-read on every call, so a downgrade applies immediately);
//! 2. policy (`allow` / `ask` / `deny`);
//! 3. owner approval when the policy says `ask`;
//! 4. audit record of the decision;
//! 5. the call itself, cancelled when the session or connection ends;
//! 6. audit record of the outcome;
//! 7. result budget (large text cut to head and tail).

use std::time::Instant;

use ar_core::{budget, config::Action};
use rmcp::{
    ErrorData,
    model::{CallToolResult, ContentBlock, JsonObject},
};

use crate::{
    HostState,
    approval::{ApprovalRequest, Verdict},
    audit::{AuditRecord, now_ms},
    policy,
    session::{Session, ToolCtx},
};

pub async fn call(
    host: &HostState,
    session: &Session,
    name: &str,
    args: JsonObject,
) -> Result<CallToolResult, ErrorData> {
    let Some(tool) = host.registry().get(name) else {
        return Err(ErrorData::invalid_params(format!("unknown tool '{name}'"), None));
    };
    let spec = tool.spec().clone();
    let canonical = serde_json::to_string(&args).unwrap_or_default();
    let args_hash = blake3::hash(canonical.as_bytes()).to_hex().to_string();
    let preview = budget::head_tail(&canonical, 400, 190);
    let intent = args.get("intent").and_then(|v| v.as_str()).map(str::to_string);

    let record = |kind: &'static str| AuditRecord {
        ts: now_ms(),
        kind,
        session: &session.id,
        peer: &session.peer.name,
        principal: session.peer.principal.as_deref(),
        tool: name,
        decision: None,
        intent: intent.as_deref(),
        args_hash: &args_hash,
        args_preview: None,
        outcome: None,
        duration_ms: None,
        result_bytes: None,
        error: None,
    };

    // 1. Current level.
    let level = match host.current_level(&session.peer).await {
        Some(level) => level,
        None => {
            host.audit().write(&AuditRecord { decision: Some("level"), args_preview: Some(&preview), ..record("decision") });
            return Ok(refusal("This device no longer has access to this host."));
        }
    };
    if level < spec.level {
        host.audit().write(&AuditRecord { decision: Some("level"), args_preview: Some(&preview), ..record("decision") });
        return Ok(refusal(&format!(
            "'{name}' requires level '{}', but this device has '{level}'. Ask the host owner to raise it \
             (on the host: ai-remote level <this-device> {}).",
            spec.level, spec.level
        )));
    }

    // 2-3. Policy and approval.
    let action = policy::decide(&host.config().rules, name, &session.peer, spec.default_action);
    let decision = match action {
        Action::Allow => "allow",
        Action::Deny => {
            host.audit().write(&AuditRecord { decision: Some("policy"), args_preview: Some(&preview), ..record("decision") });
            return Ok(refusal(&format!("The host's policy does not allow '{name}'.")));
        }
        Action::Ask => {
            let verdict = host
                .approver()
                .ask(ApprovalRequest {
                    peer: session.peer.name.clone(),
                    tool: name.to_string(),
                    intent: intent.clone(),
                    args_preview: preview.clone(),
                })
                .await;
            if verdict != Verdict::Approved {
                host.audit().write(&AuditRecord {
                    decision: Some(verdict.as_str()),
                    args_preview: Some(&preview),
                    ..record("decision")
                });
                let why = match verdict {
                    Verdict::Denied => "The host owner declined",
                    Verdict::Timeout => "Nobody approved it on the host in time for",
                    _ => "No one can approve actions on the host right now (desktop locked or unattended), so it refused",
                };
                return Ok(refusal(&format!(
                    "{why} '{name}'. Do not retry automatically; tell the user what you wanted to do."
                )));
            }
            "approved"
        }
    };

    // 4. Decision record.
    host.audit().write(&AuditRecord { decision: Some(decision), args_preview: Some(&preview), ..record("decision") });

    // 5. The call, bounded by the session's lifetime.
    let started = Instant::now();
    let ctx = ToolCtx { host, session };
    let result = tokio::select! {
        r = tool.call(&ctx, args) => Some(r),
        _ = session.cancel.cancelled() => None,
    };
    let duration_ms = Some(started.elapsed().as_millis());

    // 6-7. Outcome and budget.
    let (outcome, result) = match result {
        None => ("cancelled", CallToolResult::error(vec![ContentBlock::text("cancelled: the session ended")])),
        Some(Ok(r)) => ("ok", apply_budget(r)),
        Some(Err(e)) => ("error", CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))])),
    };
    let outcome = if outcome == "ok" && result.is_error == Some(true) { "error" } else { outcome };
    let bytes = serde_json::to_vec(&result.content).map(|v| v.len()).unwrap_or(0);
    let error_text = (outcome == "error")
        .then(|| result.content.first().and_then(|c| c.as_text()).map(|t| budget::head_tail(&t.text, 300, 140)))
        .flatten();
    host.audit().write(&AuditRecord {
        outcome: Some(outcome),
        duration_ms,
        result_bytes: Some(bytes),
        error: error_text.as_deref(),
        ..record("outcome")
    });
    Ok(result)
}

fn refusal(msg: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(msg.to_string())])
}

fn apply_budget(mut r: CallToolResult) -> CallToolResult {
    for block in &mut r.content {
        if let ContentBlock::Text(t) = block {
            if t.text.len() > budget::TEXT_LIMIT {
                t.text = budget::clip(&t.text);
            }
        }
    }
    r
}
