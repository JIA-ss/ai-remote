//! Claude Code, driven as `claude -p --input-format stream-json --output-format stream-json`: one
//! JSON line per user message in, one per event out. The process stays up between turns (each
//! user line starts a turn that ends with a `result` line), so a conversation keeps its context
//! without re-reading the transcript; `--resume <session>` continues it in a new process.
//!
//! - Bypass: `--permission-mode bypassPermissions`. Should the agent still ask
//!   (`control_request` `can_use_tool`), the answer is allow, with `run_in_background` turned off:
//!   a background shell outlives the turn that started it, and nothing here would report it.
//! - `AskUserQuestion` is disallowed: its questions arrive as a permission request this client has
//!   no way to answer in the protocol, and the turn would hang.
//! - Text streams with `--include-partial-messages`; the complete `assistant` message that follows
//!   only contributes what did not stream (its tool calls).
//! - Interrupt: a `control_request` of subtype `interrupt`; the turn then ends with a `result`.
//! - A turn ends with its `result` line only. A `result` that reports an error (401, a prompt too
//!   long) ends the turn as failed with its text.

use std::collections::HashSet;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::{LiveCmd, content_text, exit_reason, report};
use crate::chat::{
    event::{
        DIFF_SIDE, Ev, Out, PREVIEW, PlanEntry, ToolContent, ToolKind, ToolStatus, TurnStatus, cut, preview_value,
    },
    proc::{AgentProc, Line},
};

pub fn args(l: &super::Launch<'_>) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-mode",
        "bypassPermissions",
        "--disallowedTools",
        "AskUserQuestion",
    ]
    .map(String::from)
    .to_vec();
    if l.hardened {
        a.extend(["--setting-sources", "user", "--strict-mcp-config"].map(String::from));
    }
    if let Some(m) = l.model {
        a.extend(["--model".into(), m.into()]);
    }
    if let Some(r) = l.resume {
        a.extend(["--resume".into(), r.into()]);
    }
    a
}

#[derive(Default)]
struct State {
    session: Option<String>,
    /// The message being streamed and the ids of messages whose text already streamed.
    current: Option<String>,
    streamed: HashSet<String>,
    /// Tool calls reported, so results of unknown ones (a plan update) are not shown.
    calls: HashSet<String>,
    interrupting: bool,
    retried: bool,
    next_req: u64,
}

pub async fn run(
    proc: AgentProc,
    mut lines: mpsc::Receiver<Line>,
    mut cmds: mpsc::Receiver<LiveCmd>,
    out: mpsc::Sender<Out>,
    name: String,
) {
    let mut st = State::default();
    loop {
        tokio::select! {
            line = lines.recv() => match line {
                Some(Ok(line)) => {
                    let (writes, outs) = handle(&mut st, &line);
                    for w in writes {
                        let _ = proc.write_line(w).await;
                    }
                    for o in outs {
                        if !report(&out, o).await {
                            proc.shutdown(std::time::Duration::from_secs(2)).await;
                            return;
                        }
                    }
                }
                Some(Err(why)) => {
                    let reason = exit_reason(&name, &proc, Some(why));
                    proc.shutdown(std::time::Duration::from_secs(1)).await;
                    report(&out, Out::Exited(reason)).await;
                    return;
                }
                None => {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    let reason = exit_reason(&name, &proc, None);
                    proc.shutdown(std::time::Duration::from_secs(1)).await;
                    report(&out, Out::Exited(reason)).await;
                    return;
                }
            },
            cmd = cmds.recv() => match cmd {
                Some(LiveCmd::Prompt { text }) => {
                    st.retried = false;
                    let line = json!({"type": "user", "message": {"role": "user", "content": [{"type": "text", "text": text}]}});
                    if let Err(e) = proc.write_line(line.to_string()).await {
                        report(&out, Out::TurnEnd { status: TurnStatus::Failed, final_text: None, reason: Some(format!("{e:#}")) }).await;
                    }
                }
                Some(LiveCmd::Interrupt) => {
                    st.interrupting = true;
                    st.next_req += 1;
                    let line = json!({"type": "control_request", "request_id": format!("ar-int-{}", st.next_req), "request": {"subtype": "interrupt"}});
                    let _ = proc.write_line(line.to_string()).await;
                }
                Some(LiveCmd::Answer { .. }) => {
                    report(&out, Out::Ev(Ev::Notice { level: "warn", code: "no_question", text: "Claude Code is not waiting for an answer".into() })).await;
                }
                Some(LiveCmd::Shutdown { done }) => {
                    proc.shutdown(std::time::Duration::from_secs(3)).await;
                    let _ = done.send(());
                    return;
                }
                None => {
                    proc.shutdown(std::time::Duration::from_secs(3)).await;
                    return;
                }
            },
        }
    }
}

/// Translates one line of Claude Code's output; returns lines to write back and messages for the
/// session.
fn handle(st: &mut State, line: &str) -> (Vec<String>, Vec<Out>) {
    let mut writes = Vec::new();
    let mut outs = Vec::new();
    let Ok(v) = serde_json::from_str::<Value>(line) else { return (writes, outs) };
    // Events of sub-agents (the Task tool) carry their parent's tool id; only the summary the
    // parent receives is shown.
    let nested = v.get("parent_tool_use_id").is_some_and(|p| !p.is_null());
    match v["type"].as_str().unwrap_or("") {
        "system" => match v["subtype"].as_str().unwrap_or("") {
            "init" => {
                if let Some(id) = v["session_id"].as_str()
                    && st.session.as_deref() != Some(id)
                {
                    st.session = Some(id.to_string());
                    outs.push(Out::Session(id.to_string()));
                }
            }
            "api_retry" if !st.retried => {
                st.retried = true;
                let err = v["error"].as_str().unwrap_or("error");
                let status = v["error_status"].as_u64().map(|s| format!(" ({s})")).unwrap_or_default();
                outs.push(Out::Ev(Ev::Notice {
                    level: "warn",
                    code: "api_retry",
                    text: format!("the model API failed{status}: {err}; retrying"),
                }));
            }
            _ => {}
        },
        "stream_event" if !nested => {
            let e = &v["event"];
            match e["type"].as_str().unwrap_or("") {
                "message_start" => st.current = e["message"]["id"].as_str().map(str::to_string),
                "content_block_delta" => {
                    let Some(msg) = st.current.clone() else { return (writes, outs) };
                    let id = format!("{msg}:{}", e["index"].as_u64().unwrap_or(0));
                    let d = &e["delta"];
                    match d["type"].as_str().unwrap_or("") {
                        "text_delta" => {
                            st.streamed.insert(msg);
                            outs.push(Out::Ev(Ev::Message {
                                id,
                                text: d["text"].as_str().unwrap_or("").into(),
                                delta: true,
                            }));
                        }
                        "thinking_delta" => {
                            st.streamed.insert(msg);
                            outs.push(Out::Ev(Ev::Thought {
                                id,
                                text: d["thinking"].as_str().unwrap_or("").into(),
                                delta: true,
                            }));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        "assistant" if !nested => {
            let msg = v["message"]["id"].as_str().unwrap_or("").to_string();
            let streamed = st.streamed.contains(&msg);
            for (i, block) in v["message"]["content"].as_array().into_iter().flatten().enumerate() {
                let id = format!("{msg}:{i}");
                match block["type"].as_str().unwrap_or("") {
                    "text" if !streamed => outs.push(Out::Ev(Ev::Message {
                        id,
                        text: block["text"].as_str().unwrap_or("").into(),
                        delta: false,
                    })),
                    "thinking" if !streamed => outs.push(Out::Ev(Ev::Thought {
                        id,
                        text: block["thinking"].as_str().unwrap_or("").into(),
                        delta: false,
                    })),
                    "tool_use" => {
                        if let Some(o) = tool_call(st, block) {
                            outs.push(o);
                        }
                    }
                    _ => {}
                }
            }
        }
        "user" if !nested => {
            for block in v["message"]["content"].as_array().into_iter().flatten() {
                if block["type"] != "tool_result" {
                    continue;
                }
                let call_id = block["tool_use_id"].as_str().unwrap_or("").to_string();
                if !st.calls.contains(&call_id) {
                    continue;
                }
                let (text, truncated) = cut(&content_text(&block["content"]), PREVIEW);
                let failed = block["is_error"].as_bool() == Some(true);
                outs.push(Out::Ev(Ev::ToolUpdate {
                    call_id,
                    status: Some(if failed { ToolStatus::Failed } else { ToolStatus::Completed }),
                    content: if text.is_empty() { vec![] } else { vec![ToolContent::Text { text }] },
                    truncated,
                }));
            }
        }
        "result" => {
            let usage = &v["usage"];
            outs.push(Out::Ev(Ev::Usage {
                input_tokens: usage["input_tokens"].as_u64().map(|i| {
                    i + usage["cache_read_input_tokens"].as_u64().unwrap_or(0)
                        + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                }),
                output_tokens: usage["output_tokens"].as_u64(),
                cost_usd: v["total_cost_usd"].as_f64(),
            }));
            let text = v["result"].as_str().map(str::to_string);
            let failed = v["is_error"].as_bool() == Some(true) || v["subtype"].as_str().is_some_and(|s| s != "success");
            let status = if st.interrupting {
                TurnStatus::Interrupted
            } else if failed {
                TurnStatus::Failed
            } else {
                TurnStatus::Completed
            };
            let reason = (failed && !st.interrupting)
                .then(|| text.clone().unwrap_or_else(|| v["subtype"].as_str().unwrap_or("error").replace('_', " ")));
            st.interrupting = false;
            st.streamed.clear();
            outs.push(Out::TurnEnd { status, final_text: if failed { None } else { text }, reason });
        }
        "control_request" => {
            let id = v["request_id"].clone();
            let req = &v["request"];
            let reply = if req["subtype"] == "can_use_tool" {
                let mut input = req["input"].clone();
                if input.get("run_in_background").is_some() {
                    input["run_in_background"] = json!(false);
                }
                json!({"type": "control_response", "response": {"subtype": "success", "request_id": id, "response": {"behavior": "allow", "updatedInput": input}}})
            } else {
                json!({"type": "control_response", "response": {"subtype": "error", "request_id": id, "error": "not supported by this client"}})
            };
            writes.push(reply.to_string());
        }
        _ => {}
    }
    (writes, outs)
}

/// A `tool_use` block as a tool call (or, for the to-do list, a plan).
fn tool_call(st: &mut State, block: &Value) -> Option<Out> {
    let call_id = block["id"].as_str()?.to_string();
    let name = block["name"].as_str().unwrap_or("tool");
    let input = &block["input"];
    if name == "TodoWrite" {
        let entries = input["todos"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| PlanEntry {
                content: t["content"].as_str().unwrap_or("").to_string(),
                status: t["status"].as_str().unwrap_or("pending").to_string(),
            })
            .collect();
        return Some(Out::Ev(Ev::Plan { entries }));
    }
    st.calls.insert(call_id.clone());
    let s = |k: &str| input[k].as_str().unwrap_or("").to_string();
    let (kind, title) = match name {
        "Bash" => (ToolKind::Execute, s("command")),
        "Read" | "NotebookRead" => (ToolKind::Read, s("file_path")),
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => (ToolKind::Edit, s("file_path")),
        "Grep" => (ToolKind::Search, s("pattern")),
        "Glob" => (ToolKind::Search, s("pattern")),
        "LS" => (ToolKind::Search, s("path")),
        "WebFetch" => (ToolKind::Fetch, s("url")),
        "WebSearch" => (ToolKind::Fetch, s("query")),
        "Task" | "Agent" => (ToolKind::Think, s("description")),
        _ => (ToolKind::Other, String::new()),
    };
    let title = if title.is_empty() { name.to_string() } else { format!("{name} {}", cut(&title, 300).0) };
    let path = input["file_path"].as_str().map(str::to_string);
    let mut content = Vec::new();
    match name {
        "Edit" => content.push(ToolContent::Diff {
            path: s("file_path"),
            old: Some(cut(&s("old_string"), DIFF_SIDE).0),
            new: cut(&s("new_string"), DIFF_SIDE).0,
        }),
        "Write" => {
            content.push(ToolContent::Diff { path: s("file_path"), old: None, new: cut(&s("content"), DIFF_SIDE).0 })
        }
        "MultiEdit" => {
            for e in input["edits"].as_array().into_iter().flatten().take(20) {
                content.push(ToolContent::Diff {
                    path: s("file_path"),
                    old: Some(cut(e["old_string"].as_str().unwrap_or(""), DIFF_SIDE).0),
                    new: cut(e["new_string"].as_str().unwrap_or(""), DIFF_SIDE).0,
                });
            }
        }
        _ => {}
    }
    let shown_input = if content.is_empty() { Some(preview_value(input)) } else { None };
    Some(Out::Ev(Ev::ToolCall {
        call_id,
        title,
        kind,
        status: ToolStatus::InProgress,
        input: shown_input,
        locations: path.into_iter().collect(),
        content,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_lines(st: &mut State, lines: &[Value]) -> (Vec<String>, Vec<Out>) {
        let (mut w, mut o) = (Vec::new(), Vec::new());
        for l in lines {
            let (a, b) = handle(st, &l.to_string());
            w.extend(a);
            o.extend(b);
        }
        (w, o)
    }

    #[test]
    fn a_turn_streams_text_tools_and_ends_with_its_result() {
        let mut st = State::default();
        let (writes, outs) = run_lines(
            &mut st,
            &[
                json!({"type": "system", "subtype": "init", "session_id": "s1"}),
                json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "m1"}}}),
                json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hel"}}}),
                json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "lo"}}}),
                json!({"type": "assistant", "message": {"id": "m1", "content": [
                    {"type": "text", "text": "Hello"},
                    {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls -la", "run_in_background": true}},
                ]}}),
                json!({"type": "control_request", "request_id": "r1", "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls", "run_in_background": true}}}),
                json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "a\nb"}]}]}}),
                json!({"type": "stream_event", "parent_tool_use_id": "x", "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "nested"}}}),
                json!({"type": "result", "subtype": "success", "is_error": false, "result": "Hello", "usage": {"input_tokens": 3, "output_tokens": 2}, "total_cost_usd": 0.01}),
            ],
        );
        assert_eq!(writes.len(), 1);
        let reply: Value = serde_json::from_str(&writes[0]).unwrap();
        assert_eq!(reply["response"]["response"]["behavior"], "allow");
        assert_eq!(reply["response"]["response"]["updatedInput"]["run_in_background"], false);
        let shown: Vec<String> = outs.iter().map(|o| format!("{o:?}")).collect();
        assert!(matches!(&outs[0], Out::Session(s) if s == "s1"));
        assert!(matches!(&outs[1], Out::Ev(Ev::Message { text, delta: true, .. }) if text == "Hel"));
        assert!(matches!(&outs[2], Out::Ev(Ev::Message { text, delta: true, .. }) if text == "lo"));
        assert!(
            matches!(&outs[3], Out::Ev(Ev::ToolCall { kind: ToolKind::Execute, title, .. }) if title == "Bash ls -la"),
            "{shown:?}"
        );
        assert!(matches!(&outs[4], Out::Ev(Ev::ToolUpdate { status: Some(ToolStatus::Completed), .. })));
        assert!(matches!(&outs[5], Out::Ev(Ev::Usage { output_tokens: Some(2), .. })));
        assert!(
            matches!(&outs[6], Out::TurnEnd { status: TurnStatus::Completed, final_text: Some(t), .. } if t == "Hello")
        );
        assert_eq!(outs.len(), 7, "{shown:?}");
    }

    #[test]
    fn errors_and_interrupts_end_the_turn_accordingly() {
        let mut st = State::default();
        let (_, outs) = run_lines(
            &mut st,
            &[
                json!({"type": "result", "subtype": "success", "is_error": true, "result": "API Error: 401 token expired"}),
            ],
        );
        assert!(
            matches!(&outs[1], Out::TurnEnd { status: TurnStatus::Failed, reason: Some(r), .. } if r.contains("401"))
        );
        st.interrupting = true;
        let (_, outs) =
            run_lines(&mut st, &[json!({"type": "result", "subtype": "error_during_execution", "is_error": true})]);
        assert!(matches!(&outs[1], Out::TurnEnd { status: TurnStatus::Interrupted, reason: None, .. }));
        assert!(!st.interrupting);
    }

    #[test]
    fn edits_carry_their_diff_and_todos_become_a_plan() {
        let mut st = State::default();
        let (_, outs) = run_lines(
            &mut st,
            &[json!({"type": "assistant", "message": {"id": "m", "content": [
                {"type": "tool_use", "id": "e", "name": "Edit", "input": {"file_path": "/p/a.rs", "old_string": "x", "new_string": "y"}},
                {"type": "tool_use", "id": "p", "name": "TodoWrite", "input": {"todos": [{"content": "build", "status": "in_progress"}]}},
            ]}})],
        );
        assert!(
            matches!(&outs[0], Out::Ev(Ev::ToolCall { kind: ToolKind::Edit, content, input: None, .. }) if matches!(&content[0], ToolContent::Diff { new, .. } if new == "y"))
        );
        assert!(matches!(&outs[1], Out::Ev(Ev::Plan { entries }) if entries[0].status == "in_progress"));
        assert!(!st.calls.contains("p"));
    }

    #[test]
    fn arguments_put_bypass_and_resume_in_place() {
        let r = crate::chat::detect::Resolved {
            id: "claude".into(),
            name: "Claude Code".into(),
            runtime: crate::chat::runtimes::builtin("claude").unwrap(),
            program: "claude".into(),
            prefix: vec![],
            env: Default::default(),
            resolved_by: "path",
        };
        let l = super::super::Launch {
            resolved: &r,
            cwd: std::path::Path::new("/"),
            model: Some("haiku"),
            hardened: true,
            resume: Some("s1"),
        };
        let a = args(&l).join(" ");
        assert!(a.contains("--permission-mode bypassPermissions"), "{a}");
        assert!(a.contains("--setting-sources user --strict-mcp-config"), "{a}");
        assert!(a.ends_with("--model haiku --resume s1"), "{a}");
    }
}
