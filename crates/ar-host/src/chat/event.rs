//! The events of a chat, the same for every agent: what controllers render and what the transcript
//! stores. The vocabulary follows the Agent Client Protocol's session updates (text and thought
//! chunks, tool calls with a kind and status, plans), so ACP agents map almost one to one and the
//! native adapters translate into it.
//!
//! On the wire and on disk every event is one JSON line with an envelope:
//! `{"seq":N,"ts":<ms>,"turn":K,"t":"<type>",...}`. `seq` grows by one per event and is assigned
//! only by the session (see [`super::Chat`]), so a viewer that reconnects asks for what came after
//! the last `seq` it saw.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Most text kept of a tool's input or output in an event; the rest is cut (`truncated: true`).
pub const PREVIEW: usize = 8 * 1024;
/// Most text kept of either side of a diff.
pub const DIFF_SIDE: usize = 64 * 1024;
/// Most text kept of a turn's whole diff.
pub const TURN_DIFF: usize = 512 * 1024;
/// Most text all the diffs of one tool call may take together, so one event stays well under what a
/// controller reads as one line (4 MiB).
pub const DIFF_TOTAL: usize = 2 * 1024 * 1024;

/// What a tool does, for the icon and the summary line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    Text { text: String },
    Diff { path: String, old: Option<String>, new: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    /// pending | in_progress | completed
    pub status: String,
}

/// A slash command or skill the agent offers, for the composer's `/` menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// `command` or `skill`.
    pub kind: String,
    /// What choosing it puts in the message: `/name` for commands, how the agent names a skill
    /// otherwise (`$name` for Codex). The message goes to the agent as typed.
    pub insert: String,
    /// What the command takes, when the agent says (`<file>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Most characters of a command's description kept.
pub const COMMAND_DESCRIPTION: usize = 240;
/// Most commands kept in a list.
pub const MAX_COMMANDS: usize = 400;

impl SlashCommand {
    pub fn new(kind: &str, name: &str, insert: String, description: &str, hint: Option<&str>) -> Self {
        let description = description.trim();
        let description = match description.char_indices().nth(COMMAND_DESCRIPTION) {
            Some((at, _)) => format!("{}…", &description[..at]),
            None => description.to_string(),
        };
        Self {
            name: name.to_string(),
            description,
            kind: kind.to_string(),
            insert,
            hint: hint.map(str::trim).filter(|h| !h.is_empty()).map(str::to_string),
        }
    }
}

/// What a transcript keeps of an image sent with a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub bytes: usize,
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Completed,
    Failed,
    Interrupted,
}

/// Who sent a message: a device and, when a person typed it, that it was a person.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct By {
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

/// One event of a chat.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Ev {
    /// A message for the agent, as accepted. With `steer` it joined the running turn (its
    /// envelope carries that turn's number) instead of starting one.
    User {
        input_id: String,
        by: By,
        text: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        steer: bool,
        /// The images sent with it (their bytes are not kept).
        #[serde(skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Attachment>,
    },
    TurnStart {},
    /// Text from the agent. With `delta` it continues the message `id`.
    Message {
        id: String,
        text: String,
        delta: bool,
        /// Where the agent says the message belongs, when it says: `commentary` (said between
        /// steps) or `final_answer` (its answer to the turn). Codex only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    /// The agent's reasoning, shown folded.
    Thought {
        id: String,
        text: String,
        delta: bool,
    },
    ToolCall {
        call_id: String,
        /// The tool call of the sub-agent that made this one (Claude Code's `Agent` / `Task`
        /// tool): shown inside that sub-agent's row.
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        title: String,
        kind: ToolKind,
        status: ToolStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<Value>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        locations: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content: Vec<ToolContent>,
    },
    ToolUpdate {
        call_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<ToolStatus>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        content: Vec<ToolContent>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
        /// A finished command's exit code, when the agent reports it (Codex).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
        /// How long the tool ran, as the agent measured it (Codex).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    Plan {
        entries: Vec<PlanEntry>,
    },
    /// The agent asks the user; answered with an `answer` command (one per question).
    Question {
        qid: String,
        prompt: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
        /// Codex: the request the question belongs to (its questions are answered together; an
        /// empty answer skips one), the item that asked, the question's short header, a description
        /// per option (aligned with `options`, empty where it has none), whether a free answer is
        /// welcome besides the options, whether it is a secret (typed but not shown), and whether
        /// the turn waits on it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        header: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        descriptions: Vec<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        other: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        secret: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        blocking: Option<bool>,
    },
    /// A question request is over (Codex): `how` is `answered`, `skipped` (every answer empty),
    /// `resolved` (Codex closed it, or its turn ended unanswered) or `dismissed` (its turn was
    /// stopped); `answers` are the non-empty answers by question id.
    QuestionDone {
        request: String,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        answers: BTreeMap<String, Vec<String>>,
        how: String,
    },
    Usage {
        #[serde(skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        /// The model's context window, in tokens.
        #[serde(skip_serializing_if = "Option::is_none")]
        context_window: Option<u64>,
        /// Tokens the conversation takes of it now (the last request's input and output).
        #[serde(skip_serializing_if = "Option::is_none")]
        context_used: Option<u64>,
        /// Of `input_tokens`: read from the prompt cache, and written to it.
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_read_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cache_write_tokens: Option<u64>,
        /// Of `output_tokens`: the model's reasoning.
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_tokens: Option<u64>,
    },
    TurnEnd {
        status: TurnStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        final_text: Option<String>,
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The session's state changed (see [`super::State`]), with the messages waiting their turn.
    State {
        state: super::State,
        queued: Vec<Queued>,
    },
    /// What the running agent says it can do, where that differs from what its kind was assumed to
    /// (an ACP agent that takes no images).
    Agent {
        images: bool,
    },
    /// Everything the turn changed, as one unified diff over all files, where the agent reports it
    /// (Codex): repeated edits of a file show as their net result. Cut to [`TURN_DIFF`].
    TurnDiff {
        diff: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    /// The commands and skills the agent offers now (all of them; the list replaces the last one).
    Commands {
        commands: Vec<SlashCommand>,
    },
    /// Something the user should know that is not part of the conversation. `input_id` names the
    /// message it is about when it is about one (a message dropped because the queue was full).
    Notice {
        #[serde(skip_serializing_if = "Option::is_none")]
        input_id: Option<String>,
        #[serde(deserialize_with = "static_str")]
        level: Word,
        #[serde(deserialize_with = "static_str")]
        code: Word,
        text: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queued {
    pub id: String,
    pub text: String,
}

/// What an adapter reports while it drives its agent. Serialized when the agent runs in a separate
/// runner process (see [`super::runner`]).
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "o", content = "v", rename_all = "snake_case")]
pub enum Out {
    Ev(Ev),
    /// The agent's own conversation id, for resuming in a new process.
    Session(String),
    /// A message sent with [`super::adapters::LiveCmd::Steer`] joined the current turn.
    Steered {
        id: String,
    },
    /// The current turn ended before the agent read that message; the agent runs it as its next
    /// turn on its own (nothing is to be sent again).
    SteerLate {
        id: String,
    },
    /// The agent did not take that message; it goes to the queue.
    SteerRejected {
        id: String,
        reason: String,
    },
    /// The current turn ended.
    TurnEnd {
        status: TurnStatus,
        final_text: Option<String>,
        reason: Option<String>,
    },
    /// The agent process ended or its protocol broke; the adapter is done.
    Exited(String),
    /// The runner's number of the last output passed on before this one: the session records it
    /// and acknowledges it, so a host that restarts asks the runner only for what came after.
    /// Made by [`super::runner`], never by an adapter.
    Mark(u64),
}

/// A notice's level or code. An alias, so that deriving `Deserialize` does not tie the
/// deserializer's lifetime to `'static` (the words are mapped by [`static_str`] instead).
pub type Word = &'static str;

/// The levels and codes notices use. A notice that arrives from a runner process is mapped back to
/// one of these (the field is a `&'static str`); an unknown one becomes "other".
const NOTICE_WORDS: &[&str] = &[
    "info",
    "warn",
    "error",
    "api_retry",
    "context_lost",
    "killed",
    "no_question",
    "paused",
    "queue_full",
    "quiet",
    "resumed",
    "rpc_error",
    "start_failed",
    "stopped",
    "too_many",
    "reattached",
    "runner",
    "compacted",
    "model",
    "no_images",
    "other",
    "steer_queued",
];

fn static_str<'de, D: Deserializer<'de>>(d: D) -> Result<&'static str, D::Error> {
    let s = String::deserialize(d)?;
    Ok(NOTICE_WORDS.iter().find(|w| **w == s).copied().unwrap_or("other"))
}

/// `s` cut to at most `max` bytes on a character boundary, and whether it was cut.
pub fn cut(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}…", &s[..end]), true)
}

/// A JSON value cut to a preview: long strings inside are shortened.
pub fn preview_value(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::String(cut(s, PREVIEW).0),
        Value::Array(a) => Value::Array(a.iter().take(64).map(preview_value).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), preview_value(v))).collect()),
        v => v.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_serialize_with_their_type() {
        let ev = Ev::Message { id: "m1".into(), text: "hi".into(), delta: true, phase: None };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            serde_json::json!({"t":"message","id":"m1","text":"hi","delta":true})
        );
        let ev = Ev::ToolUpdate {
            call_id: "k".into(),
            status: Some(ToolStatus::Failed),
            content: vec![],
            truncated: false,
            exit_code: None,
            duration_ms: None,
        };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            serde_json::json!({"t":"tool_update","call_id":"k","status":"failed"})
        );
    }

    #[test]
    fn outputs_round_trip_through_json() {
        let outs = [
            Out::Ev(Ev::Notice { input_id: None, level: "warn", code: "api_retry", text: "x".into() }),
            Out::Ev(Ev::ToolCall {
                call_id: "c".into(),
                parent: None,
                title: "ls".into(),
                kind: ToolKind::Execute,
                status: ToolStatus::InProgress,
                input: None,
                locations: vec![],
                content: vec![ToolContent::Diff { path: "a".into(), old: None, new: "b".into() }],
            }),
            Out::Session("s".into()),
            Out::TurnEnd { status: TurnStatus::Completed, final_text: Some("ok".into()), reason: None },
            Out::Exited("gone".into()),
        ];
        for o in outs {
            let text = serde_json::to_string(&o).unwrap();
            let back: Out = serde_json::from_str(&text).unwrap();
            assert_eq!(serde_json::to_string(&back).unwrap(), text);
        }
        let odd: Out =
            serde_json::from_str(r#"{"o":"ev","v":{"t":"notice","level":"loud","code":"new_code","text":"t"}}"#)
                .unwrap();
        assert!(matches!(odd, Out::Ev(Ev::Notice { level: "other", code: "other", .. })));
    }

    #[test]
    fn cut_keeps_characters_whole() {
        assert_eq!(cut("héllo", 2), ("h…".to_string(), true));
        assert_eq!(cut("abc", 3), ("abc".to_string(), false));
    }
}
