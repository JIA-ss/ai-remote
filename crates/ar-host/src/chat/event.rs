//! The events of a chat, the same for every agent: what controllers render and what the transcript
//! stores. The vocabulary follows the Agent Client Protocol's session updates (text and thought
//! chunks, tool calls with a kind and status, plans), so ACP agents map almost one to one and the
//! native adapters translate into it.
//!
//! On the wire and on disk every event is one JSON line with an envelope:
//! `{"seq":N,"ts":<ms>,"turn":K,"t":"<type>",...}`. `seq` grows by one per event and is assigned
//! only by the session (see [`super::Chat`]), so a viewer that reconnects asks for what came after
//! the last `seq` it saw.

use serde::Serialize;
use serde_json::Value;

/// Most text kept of a tool's input or output in an event; the rest is cut (`truncated: true`).
pub const PREVIEW: usize = 8 * 1024;
/// Most text kept of either side of a diff.
pub const DIFF_SIDE: usize = 64 * 1024;

/// What a tool does, for the icon and the summary line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    Text { text: String },
    Diff { path: String, old: Option<String>, new: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanEntry {
    pub content: String,
    /// pending | in_progress | completed
    pub status: String,
}

/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Completed,
    Failed,
    Interrupted,
}

/// Who sent a message: a device and, when a person typed it, that it was a person.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct By {
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

/// One event of a chat.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Ev {
    /// A message for the agent, as accepted.
    User {
        input_id: String,
        by: By,
        text: String,
    },
    TurnStart {},
    /// Text from the agent. With `delta` it continues the message `id`.
    Message {
        id: String,
        text: String,
        delta: bool,
    },
    /// The agent's reasoning, shown folded.
    Thought {
        id: String,
        text: String,
        delta: bool,
    },
    ToolCall {
        call_id: String,
        title: String,
        kind: ToolKind,
        status: ToolStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        input: Option<Value>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        locations: Vec<String>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        content: Vec<ToolContent>,
    },
    ToolUpdate {
        call_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<ToolStatus>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        content: Vec<ToolContent>,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        truncated: bool,
    },
    Plan {
        entries: Vec<PlanEntry>,
    },
    /// The agent asks the user; answered with an `answer` command.
    Question {
        qid: String,
        prompt: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
    },
    Usage {
        #[serde(skip_serializing_if = "Option::is_none")]
        input_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
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
    /// Something the user should know that is not part of the conversation.
    Notice {
        level: &'static str,
        code: &'static str,
        text: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Queued {
    pub id: String,
    pub text: String,
}

/// What an adapter reports while it drives its agent.
#[derive(Debug)]
pub enum Out {
    Ev(Ev),
    /// The agent's own conversation id, for resuming in a new process.
    Session(String),
    /// The current turn ended.
    TurnEnd {
        status: TurnStatus,
        final_text: Option<String>,
        reason: Option<String>,
    },
    /// The agent process ended or its protocol broke; the adapter is done.
    Exited(String),
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
        let ev = Ev::Message { id: "m1".into(), text: "hi".into(), delta: true };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            serde_json::json!({"t":"message","id":"m1","text":"hi","delta":true})
        );
        let ev =
            Ev::ToolUpdate { call_id: "k".into(), status: Some(ToolStatus::Failed), content: vec![], truncated: false };
        assert_eq!(
            serde_json::to_value(&ev).unwrap(),
            serde_json::json!({"t":"tool_update","call_id":"k","status":"failed"})
        );
    }

    #[test]
    fn cut_keeps_characters_whole() {
        assert_eq!(cut("héllo", 2), ("h…".to_string(), true));
        assert_eq!(cut("abc", 3), ("abc".to_string(), false));
    }
}
