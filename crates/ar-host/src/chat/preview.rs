//! Which files of a session a controller may preview (`chat_file`): HTML, Markdown and images the
//! agent made or changed, read whole over the files channel like `files_pull`, but without asking
//! the owner each time.
//!
//! Why that is no new reach: the device asking already has level admin and can talk to the
//! session, so it can ask the agent (which runs as the signed-in user, without asking) to print any
//! file it likes. The preview only saves the round trip through the agent. It is still kept narrow,
//! so a default-allowed tool reads no more than the conversation is about:
//!
//! - the path lies inside the session's working folder (after links are followed), or it appears in
//!   the agent's own events of the session (a tool call's location, a diff, the agent's text);
//! - the file (after links are followed) has a previewable extension ([`KINDS`]);
//! - it is at most [`MAX_PREVIEW`] bytes.

use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{Chat, detect};

/// Largest file a preview reads.
pub const MAX_PREVIEW: u64 = 16 * 1024 * 1024;

/// Previewable extensions and what the panel shows them as.
pub const KINDS: [(&str, &str); 11] = [
    ("html", "html"),
    ("htm", "html"),
    ("md", "markdown"),
    ("markdown", "markdown"),
    ("png", "image"),
    ("jpg", "image"),
    ("jpeg", "image"),
    ("gif", "image"),
    ("webp", "image"),
    ("svg", "image"),
    ("bmp", "image"),
];

/// What `path` (lowercased extension) is shown as, if it can be previewed.
pub fn kind_of(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    KINDS.iter().find(|(e, _)| *e == ext).map(|(_, k)| *k)
}

/// A file a preview may read: its resolved path, size and kind.
#[derive(Debug)]
pub struct Previewable {
    pub path: PathBuf,
    pub size: u64,
    pub kind: &'static str,
}

impl Chat {
    /// Resolves `path` as the session sees it (relative to its working folder, `~` the user's
    /// home) and checks that a preview may read it.
    pub fn previewable(&self, path: &str) -> Result<Previewable> {
        let raw = path.trim();
        if raw.is_empty() {
            bail!("give a path");
        }
        let cwd = PathBuf::from(self.meta().cwd);
        let given = PathBuf::from(detect::expand_home(raw));
        let joined = if given.is_absolute() { given } else { cwd.join(given) };
        let resolved = dunce::canonicalize(&joined).with_context(|| format!("cannot read {}", joined.display()))?;
        let md = std::fs::metadata(&resolved).with_context(|| format!("cannot read {}", resolved.display()))?;
        if !md.is_file() {
            bail!("{} is not a file", resolved.display());
        }
        let Some(kind) = kind_of(&resolved) else {
            bail!(
                "{} cannot be previewed: only HTML, Markdown and images can (fetch other files with `ai-remote pull`)",
                resolved.display()
            );
        };
        let inside = dunce::canonicalize(&cwd).is_ok_and(|c| resolved.starts_with(c));
        if !inside && !self.mentions(raw, &resolved) {
            bail!(
                "{} is outside this session's folder and the agent has not named it in this session; \
                 previews only read files the conversation is about (fetch others with `ai-remote pull`)",
                resolved.display()
            );
        }
        if md.len() > MAX_PREVIEW {
            bail!(
                "{} is {:.1} MiB; previews read at most {} MiB (fetch it with `ai-remote pull`)",
                resolved.display(),
                md.len() as f64 / 1048576.0,
                MAX_PREVIEW / 1048576
            );
        }
        Ok(Previewable { path: resolved, size: md.len(), kind })
    }

    /// Whether the agent's events of this session name `raw` (as written) or `resolved`: a tool
    /// call's location, a diff's path, or the agent's text.
    fn mentions(&self, raw: &str, resolved: &Path) -> bool {
        let resolved = resolved.display().to_string();
        let needles: Vec<String> =
            [raw, resolved.as_str()].iter().map(|s| escaped(s)).filter(|s| !s.is_empty()).collect();
        let Ok(f) = File::open(self.dir.join("events.jsonl")) else { return false };
        for line in BufReader::new(f).lines().map_while(Result::ok) {
            if !needles.iter().any(|n| line.contains(n.as_str())) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if names(&v, raw) || names(&v, &resolved) {
                return true;
            }
        }
        false
    }
}

/// `s` as it appears inside a JSON string (backslashes doubled, quotes escaped).
fn escaped(s: &str) -> String {
    let json = serde_json::to_string(s).unwrap_or_default();
    json.trim_matches('"').to_string()
}

/// Whether the event `v` (one of the agent's) names `path`.
fn names(v: &Value, path: &str) -> bool {
    match v["t"].as_str() {
        Some("tool_call") | Some("tool_update") => {
            let loc = v["locations"].as_array().into_iter().flatten().any(|l| l.as_str() == Some(path));
            let diff = v["content"].as_array().into_iter().flatten().any(|c| c["path"].as_str() == Some(path));
            loc || diff
        }
        Some("message") => v["text"].as_str().is_some_and(|t| t.contains(path)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn kinds_by_extension() {
        assert_eq!(kind_of(Path::new("/a/Report.HTML")), Some("html"));
        assert_eq!(kind_of(Path::new("notes.md")), Some("markdown"));
        assert_eq!(kind_of(Path::new("x.svg")), Some("image"));
        assert_eq!(kind_of(Path::new("id_rsa")), None);
        assert_eq!(kind_of(Path::new("a.txt")), None);
    }

    #[test]
    fn only_the_agents_events_name_a_path() {
        let p = "/tmp/out/report.html";
        assert!(names(&json!({"t":"tool_call","locations":[p]}), p));
        assert!(names(&json!({"t":"tool_update","content":[{"type":"diff","path":p,"new":""}]}), p));
        assert!(names(&json!({"t":"message","text":format!("saved to {p}")}), p));
        assert!(!names(&json!({"t":"user","text":p}), p));
        assert!(!names(&json!({"t":"tool_call","locations":["/tmp/out/other.html"]}), p));
        assert_eq!(escaped(r"C:\a\b.md"), r"C:\\a\\b.md");
    }
}
