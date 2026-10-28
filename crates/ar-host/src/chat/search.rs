//! Searching what was said in this host's sessions (`chat_search`): the people's messages and the
//! agents' answers, not tool output or reasoning. A query is split into words (letters and digits;
//! anything else separates them) and a passage matches when it contains every word, ignoring case.
//! A passage is one message of a person, or one message of the agent (its streamed parts joined).
//!
//! Transcripts are read one line at a time, newest session first, within a time and a size budget:
//! what the budget leaves unread makes the answer `truncated`.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::Value;

/// How long one search may read.
pub const TIME_BUDGET: Duration = Duration::from_millis(1500);
/// How many bytes of transcripts one search may read.
pub const READ_BUDGET: u64 = 256 * 1024 * 1024;
/// Excerpts kept for a session (its latest matches).
pub const EXCERPTS: usize = 3;
/// Characters of an excerpt, with its ellipses.
pub const EXCERPT_CHARS: usize = 160;
/// Characters of one passage that are searched (a longer message is searched at its start).
const PASSAGE_CHARS: usize = 32 * 1024;
/// Characters shown before the first matching word in an excerpt.
const LEAD: usize = 50;

/// The words of `query`, lowercased: split at anything that is not a letter or a digit; a query
/// with none (only punctuation) is one word.
pub fn terms(query: &str) -> Vec<String> {
    let q = query.trim().to_lowercase();
    let words: Vec<String> =
        q.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect();
    if words.is_empty() && !q.is_empty() { vec![q] } else { words }
}

/// What is left to read.
pub struct Budget {
    pub deadline: Instant,
    pub bytes: u64,
}

impl Budget {
    pub fn new(time: Duration, bytes: u64) -> Self {
        Self { deadline: Instant::now() + time, bytes }
    }

    fn spent(&self) -> bool {
        self.bytes == 0 || Instant::now() >= self.deadline
    }
}

/// A passage that matched.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Excerpt {
    /// `user` (a person) or `agent`.
    pub role: &'static str,
    /// The event it starts at.
    pub seq: u64,
    /// At most [`EXCERPT_CHARS`] characters around the first matching word, on one line.
    pub text: String,
}

/// What one transcript holds of the query.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// Matching passages.
    pub hits: usize,
    /// The last [`EXCERPTS`] of them, in transcript order.
    pub excerpts: Vec<Excerpt>,
    /// The budget ran out before the end of the transcript.
    pub truncated: bool,
}

/// Searches the transcript at `path` for passages containing all `terms` (see [`terms`]).
pub fn transcript(path: &Path, terms: &[String], budget: &mut Budget) -> Found {
    let mut found = Found::default();
    let Ok(f) = File::open(path) else { return found };
    let mut reader = BufReader::new(f);
    // Agent messages arrive in parts: the text so far of each, by id, until the turn ends.
    let mut open: BTreeMap<String, (u64, String)> = BTreeMap::new();
    let mut line = String::new();
    loop {
        if budget.spent() {
            found.truncated = true;
            break;
        }
        line.clear();
        let n = match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        budget.bytes = budget.bytes.saturating_sub(n as u64);
        // Only the lines that hold what was said are parsed.
        let user = line.contains("\"t\":\"user\"");
        let message = !user && line.contains("\"t\":\"message\"");
        if !user && !message {
            if line.contains("\"t\":\"turn_end\"") {
                for (_, (seq, text)) in std::mem::take(&mut open) {
                    check(&mut found, "agent", seq, &text, terms);
                }
            }
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
        let seq = v["seq"].as_u64().unwrap_or(0);
        let text = v["text"].as_str().unwrap_or_default();
        if user {
            check(&mut found, "user", seq, text, terms);
            continue;
        }
        let id = v["id"].as_str().unwrap_or_default().to_string();
        let entry = open.entry(id).or_insert_with(|| (seq, String::new()));
        if v["delta"].as_bool() != Some(true) {
            entry.1.clear();
        }
        if entry.1.len() < PASSAGE_CHARS * 4 {
            entry.1.push_str(text);
        }
    }
    for (_, (seq, text)) in open {
        check(&mut found, "agent", seq, &text, terms);
    }
    found
}

fn check(found: &mut Found, role: &'static str, seq: u64, text: &str, terms: &[String]) {
    if terms.is_empty() {
        return;
    }
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = text.chars().take(PASSAGE_CHARS).collect();
    // The lowercased text, and where in it each character starts (lowercasing may lengthen it).
    let mut lower = String::with_capacity(text.len());
    let mut starts = Vec::with_capacity(chars.len());
    for c in &chars {
        starts.push(lower.len());
        lower.extend(c.to_lowercase());
    }
    let mut first = usize::MAX;
    for t in terms {
        match lower.find(t.as_str()) {
            Some(at) => first = first.min(at),
            None => return,
        }
    }
    found.hits += 1;
    let at = starts.partition_point(|&s| s <= first).saturating_sub(1);
    found.excerpts.push(Excerpt { role, seq, text: excerpt(&chars, at) });
    if found.excerpts.len() > EXCERPTS {
        found.excerpts.remove(0);
    }
}

/// At most [`EXCERPT_CHARS`] characters of `chars` showing the one at `at`, with an ellipsis where
/// the text goes on.
fn excerpt(chars: &[char], at: usize) -> String {
    let room = EXCERPT_CHARS - 2;
    if chars.len() <= EXCERPT_CHARS {
        return chars.iter().collect();
    }
    let mut start = at.saturating_sub(LEAD);
    if start + room > chars.len() {
        start = chars.len() - room;
    }
    let end = (start + room).min(chars.len());
    let mut s = String::new();
    if start > 0 {
        s.push('…');
    }
    s.extend(&chars[start..end]);
    if end < chars.len() {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn write(lines: &[String]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        f
    }

    fn user(seq: u64, text: &str) -> String {
        serde_json::json!({"seq": seq, "ts": 1, "turn": 1, "t": "user", "input_id": "u", "by": {"device": "d"}, "text": text})
            .to_string()
    }

    fn part(seq: u64, id: &str, text: &str, delta: bool) -> String {
        serde_json::json!({"seq": seq, "ts": 1, "turn": 1, "t": "message", "id": id, "text": text, "delta": delta})
            .to_string()
    }

    fn budget() -> Budget {
        Budget::new(TIME_BUDGET, READ_BUDGET)
    }

    #[test]
    fn queries_split_into_lowercase_words() {
        assert_eq!(terms("  Build ERROR, ar-host "), ["build", "error", "ar", "host"]);
        assert_eq!(terms("修复报错"), ["修复报错"]);
        assert_eq!(terms("--"), ["--"]);
        assert!(terms("   ").is_empty());
    }

    #[test]
    fn every_word_must_be_in_one_passage_and_tool_output_is_not_searched() {
        let f = write(&[
            user(1, "Remember the word KIWI please"),
            part(2, "m1", "I will remem", true),
            part(3, "m1", "ber kiwi.", true),
            serde_json::json!({"seq": 4, "t": "tool_update", "call_id": "c", "content": [{"type": "text", "text": "kiwi apple"}]})
                .to_string(),
            r#"{"seq":5,"ts":1,"turn":1,"t":"turn_end","status":"completed"}"#.into(),
            user(6, "apple pie"),
        ]);
        let found = transcript(f.path(), &terms("kiwi remember"), &mut budget());
        assert_eq!(found.hits, 2, "the user's message and the agent's (joined parts): {found:?}");
        assert_eq!(found.excerpts[0], Excerpt { role: "user", seq: 1, text: "Remember the word KIWI please".into() });
        assert_eq!(found.excerpts[1].text, "I will remember kiwi.");
        assert_eq!(found.excerpts[1].seq, 2);
        // "apple" is only in tool output and a passage without "kiwi".
        assert_eq!(transcript(f.path(), &terms("kiwi apple"), &mut budget()).hits, 0);
        assert_eq!(transcript(f.path(), &terms("APPLE"), &mut budget()).hits, 1);
    }

    #[test]
    fn excerpts_are_short_lines_around_the_match_and_the_last_three_are_kept() {
        let long = format!("{} needle {}", "a ".repeat(200), "b ".repeat(200));
        let mut lines: Vec<String> = (1..=5).map(|i| user(i, &format!("needle {i}\nnext line"))).collect();
        lines.push(user(6, &long));
        let f = write(&lines);
        let found = transcript(f.path(), &terms("needle"), &mut budget());
        assert_eq!(found.hits, 6);
        assert_eq!(found.excerpts.iter().map(|e| e.seq).collect::<Vec<_>>(), [4, 5, 6]);
        assert_eq!(found.excerpts[0].text, "needle 4 next line");
        let e = &found.excerpts[2].text;
        assert!(e.chars().count() <= EXCERPT_CHARS, "{}", e.chars().count());
        assert!(e.starts_with('…') && e.ends_with('…') && e.contains("needle"), "{e}");
    }

    #[test]
    fn a_spent_budget_stops_the_read_and_says_so() {
        let f = write(&(1..=100).map(|i| user(i, "x")).collect::<Vec<_>>());
        let mut b = Budget::new(TIME_BUDGET, 200);
        let found = transcript(f.path(), &terms("x"), &mut b);
        assert!(found.truncated && found.hits < 100, "{found:?}");
        let mut b = Budget::new(Duration::ZERO, READ_BUDGET);
        let found = transcript(f.path(), &terms("x"), &mut b);
        assert!(found.truncated && found.hits == 0);
    }

    #[test]
    fn a_large_transcript_is_read_through() {
        // About 10 MiB of tool output before one message: read a line at a time, within budget.
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let noise = format!(
            "{}\n",
            serde_json::json!({"seq": 1, "t": "tool_update", "call_id": "c", "content": [{"type": "text", "text": "z".repeat(4096)}]})
        );
        for _ in 0..2_500 {
            f.write_all(noise.as_bytes()).unwrap();
        }
        writeln!(f, "{}", user(2, "the needle at the end")).unwrap();
        let started = Instant::now();
        let found = transcript(f.path(), &terms("needle"), &mut budget());
        assert_eq!((found.hits, found.truncated), (1, false), "{found:?}");
        assert!(started.elapsed() < TIME_BUDGET, "{:?}", started.elapsed());
    }
}
