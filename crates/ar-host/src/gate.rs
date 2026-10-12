//! The gate every tool call passes through, in this order:
//!
//! 1. the caller's current level (re-read on every call, so a downgrade applies immediately; admin
//!    for a device the owner bypasses, see [`crate::bypass`]), and whose turn it is (an agent's
//!    calls wait while a person drives or the agent is paused, see [`crate::turns`]);
//! 2. policy (`allow` / `ask` / `deny`), skipped for a bypassed device;
//! 3. owner approval when the policy says `ask`, at the host or on an approver device (see
//!    [`crate::approval`]);
//! 4. audit record of the decision;
//! 5. the call itself, cancelled when its control grant ends, the session/connection ends or the
//!    controller cancels the request; pending approvals use the same cancellation checks;
//! 6. result budget (large text cut to head and tail, oversized structured content dropped, images
//!    kept within what model APIs accept; see [`crate::result_budget`]);
//! 7. audit record of the outcome (its size is the size after the budget).
//!
//! Messages a person left for the agent are added to the next result an agent's session receives.

use std::time::Instant;

use ar_core::{Level, budget, config::Action};
use rmcp::{
    ErrorData,
    model::{CallToolResult, ContentBlock, JsonObject},
};
use tokio_util::sync::CancellationToken;

use crate::{
    HostState,
    approval::{ApprovalRequest, Resolution, Verdict},
    audit::{AuditRecord, now_ms},
    policy, result_budget,
    session::{Session, ToolCtx},
    tools,
};

/// What the agent is told when an approval did not end in "approved".
fn declined(tool: &str, r: &Resolution) -> String {
    let who = match r.by.as_deref() {
        Some("host") => "The person at the host".to_string(),
        Some(device) => format!("The owner (on {device})"),
        None => "The owner".to_string(),
    };
    let first = match r.verdict {
        Verdict::Denied => format!("{who} declined '{tool}'."),
        Verdict::Timeout => format!("Nobody approved '{tool}' in time, so it was declined."),
        _ => format!(
            "Nobody can approve '{tool}' right now (the host's desktop is locked or unattended and no approver \
             device is set up), so it was declined."
        ),
    };
    let note = r.note.as_deref().map(|n| format!(" They said: \"{n}\"")).unwrap_or_default();
    format!("{first}{note} Do not retry automatically; tell the user what you wanted to do.")
}

/// `cancel` is the request's own token (cancelled by the controller's `notifications/cancelled`).
pub async fn call(
    host: &HostState,
    session: &Session,
    name: &str,
    args: JsonObject,
    cancel: &CancellationToken,
) -> Result<CallToolResult, ErrorData> {
    let mut result = call_gated(host, session, name, args, cancel).await?;
    if !session.person() {
        let notes = host.turns().take_notes();
        if !notes.is_empty() {
            result.content.push(ContentBlock::text(crate::turns::notes_text(&notes)));
        }
    }
    Ok(result)
}

async fn call_gated(
    host: &HostState,
    session: &Session,
    name: &str,
    args: JsonObject,
    cancel: &CancellationToken,
) -> Result<CallToolResult, ErrorData> {
    let Some(tool) = host.registry().get(name) else {
        return Err(ErrorData::invalid_params(format!("unknown tool '{name}'"), None));
    };
    let spec = tool.spec().clone();
    let canonical = serde_json::to_string(&args).unwrap_or_default();
    let args_hash = blake3::hash(canonical.as_bytes()).to_hex().to_string();
    // What the owner sees (prompt, audit log): secret values only by size; the hash covers them.
    // What a person types while driving is theirs: the log keeps only its size.
    let secret: &[&str] = if session.person() && name == "keyboard" { &["text"] } else { tool.secret_args() };
    let shown = serde_json::to_string(&tools::shown_args(&args, secret)).unwrap_or_default();
    let preview = budget::head_tail(&shown, 400, 190);
    let intent = args.get("intent").and_then(|v| v.as_str()).map(str::to_string);
    let bypassed = host.bypass().covers(&session.peer);

    let record = |kind: &'static str| AuditRecord {
        ts: now_ms(),
        kind,
        session: &session.id,
        peer: &session.peer.name,
        principal: session.peer.principal.as_deref(),
        tool: name,
        decision: None,
        approval: None,
        by: None,
        intent: intent.as_deref(),
        args_hash: &args_hash,
        args_preview: None,
        outcome: None,
        duration_ms: None,
        result_bytes: None,
        error: None,
        bypass: bypassed.then_some(true),
    };

    // 1. Current level.
    // One answer for the whole call: the level check and the policy below agree on `bypassed`.
    let level = match host.current_level(&session.peer).await {
        Some(_) if bypassed => Level::Admin,
        Some(level) => level,
        None => {
            host.audit().write(&AuditRecord {
                decision: Some("level"),
                args_preview: Some(&preview),
                ..record("decision")
            });
            return Ok(refusal("This device no longer has access to this host."));
        }
    };
    if level < spec.level {
        host.audit().write(&AuditRecord {
            decision: Some("level"),
            args_preview: Some(&preview),
            ..record("decision")
        });
        return Ok(refusal(&format!(
            "'{name}' requires level '{}', but this device has '{level}'. Ask the host owner to raise it \
             (on the host: ai-remote level <this-device> {}).",
            spec.level, spec.level
        )));
    }

    // Whose turn it is.
    let turn_cancel = match host.turns().admits(session.person(), spec.level, host.name()) {
        Ok(grant) => grant,
        Err(why) => {
            host.audit().write(&AuditRecord {
                decision: Some("turn"),
                args_preview: Some(&preview),
                ..record("decision")
            });
            return Ok(refusal(&why));
        }
    };

    // 2-3. Policy and approval.
    let (mut approval, mut approved_by) = (None::<String>, None::<String>);
    let action = if bypassed {
        Action::Allow
    } else {
        policy::decide(&host.config().rules, name, &session.peer, spec.default_action)
    };
    let decision = match action {
        Action::Allow => "allow",
        Action::Deny => {
            host.audit().write(&AuditRecord {
                decision: Some("policy"),
                args_preview: Some(&preview),
                ..record("decision")
            });
            return Ok(refusal(&format!("The host's policy does not allow '{name}'.")));
        }
        Action::Ask => {
            let host_id = host.mesh().principal();
            let asking = host.approver().ask(
                &host_id,
                host.audit(),
                ApprovalRequest {
                    session: session.id.clone(),
                    peer: session.peer.name.clone(),
                    principal: session.peer.principal.clone(),
                    tool: name.to_string(),
                    intent: intent.clone(),
                    args_preview: preview.clone(),
                    args_hash: args_hash.clone(),
                },
            );
            let r = tokio::select! {
                biased;
                why = stopped(&session.cancel, cancel, &turn_cancel) => {
                    host.audit().write(&AuditRecord {
                        decision: Some("cancelled"), args_preview: Some(&preview), ..record("decision")
                    });
                    return Ok(refusal(why));
                }
                r = asking => r,
            };
            approval = r.id.clone();
            approved_by = r.by.clone();
            if r.verdict != Verdict::Approved {
                host.audit().write(&AuditRecord {
                    decision: Some(r.verdict.as_str()),
                    approval: approval.as_deref(),
                    by: approved_by.as_deref(),
                    args_preview: Some(&preview),
                    ..record("decision")
                });
                return Ok(refusal(&declined(name, &r)));
            }
            "approved"
        }
    };

    // 4. Decision record.
    host.audit().write(&AuditRecord {
        decision: Some(decision),
        approval: approval.as_deref(),
        by: approved_by.as_deref(),
        args_preview: Some(&preview),
        ..record("decision")
    });

    // 5. The call, bounded by the session's lifetime and the request's own cancellation.
    let started = Instant::now();
    let work = cancel.child_token();
    let _work_guard = work.clone().drop_guard();
    let ctx = ToolCtx { host, session, cancel: work.clone(), turn_cancel: turn_cancel.clone() };
    // Async tools stop when their future is dropped. Blocking input workers also
    // check the call's tokens before every event, including moves during a drag.
    let result = tokio::select! {
        biased;
        why = stopped(&session.cancel, cancel, &turn_cancel) => Err(why),
        r = tool.call(&ctx, args) => Ok(r),
    };
    work.cancel();
    let duration_ms = Some(started.elapsed().as_millis());

    // 6-7. Outcome and budget.
    let (outcome, result) = match result {
        Err(why) => ("cancelled", CallToolResult::error(vec![ContentBlock::text(why)])),
        Ok(Ok(r)) => ("ok", result_budget::apply(r).await),
        Ok(Err(e)) => ("error", CallToolResult::error(vec![ContentBlock::text(format!("{e:#}"))])),
    };
    let outcome = if outcome == "ok" && result.is_error == Some(true) { "error" } else { outcome };
    let bytes = serde_json::to_vec(&result.content).map(|v| v.len()).unwrap_or(0);
    let error_text = matches!(outcome, "error" | "cancelled")
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

async fn stopped(session: &CancellationToken, request: &CancellationToken, turn: &CancellationToken) -> &'static str {
    tokio::select! {
        biased;
        _ = session.cancelled() => "cancelled: the session ended",
        _ = request.cancelled() => "cancelled by the controller",
        _ = turn.cancelled() => "cancelled: control changed during this call. Some actions may already have run. \
            After handback or resume, inspect the host before issuing a new action; this call will not resume.",
    }
}
