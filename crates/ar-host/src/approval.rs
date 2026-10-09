//! Owner approval: a native dialog on the host's interactive desktop.
//!
//! A host on an unattended machine has nobody to ask, so "cannot ask" always means "denied": when
//! the desktop is locked or on the secure desktop, when no dialog can be shown, or when nobody
//! answers before the timeout.

use std::time::Duration;

use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};

use crate::platform;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Approved,
    Denied,
    /// Nobody answered in time.
    Timeout,
    /// No prompt could be shown (locked desktop, no interactive session).
    Unavailable,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Approved => "approved",
            Verdict::Denied => "denied",
            Verdict::Timeout => "timeout",
            Verdict::Unavailable => "unavailable",
        }
    }
}

pub struct ApprovalRequest {
    pub peer: String,
    pub tool: String,
    pub intent: Option<String>,
    pub args_preview: String,
}

pub struct Approver {
    timeout: Duration,
}

impl Approver {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    pub async fn ask(&self, req: ApprovalRequest) -> Verdict {
        if let Some(reason) = platform::prompt_blocker() {
            tracing::info!(%reason, tool = %req.tool, "approval unavailable");
            return Verdict::Unavailable;
        }
        let secs = self.timeout.as_secs();
        let mut body = format!("\"{}\" wants to run: {}\n", req.peer, req.tool);
        if let Some(intent) = &req.intent {
            body.push_str(&format!("\nWhy: {}\n", escape(intent)));
        }
        body.push_str(&format!("\nDetails:\n{}\n", escape(&req.args_preview)));
        body.push_str(&format!("\nAllow this once? (No answer within {secs} s counts as No.)"));
        let dialog = tokio::task::spawn_blocking(move || {
            MessageDialog::new()
                .set_level(MessageLevel::Warning)
                .set_title("ai-remote: approve an action")
                .set_description(body)
                .set_buttons(MessageButtons::YesNo)
                .show()
        });
        match tokio::time::timeout(self.timeout, dialog).await {
            Ok(Ok(MessageDialogResult::Yes)) => Verdict::Approved,
            Ok(Ok(_)) => Verdict::Denied,
            Ok(Err(_)) => Verdict::Unavailable,
            // The dialog stays open but its answer is ignored from now on.
            Err(_) => Verdict::Timeout,
        }
    }
}

/// Shows untrusted text safely: control characters (other than newlines) become `\u{..}` escapes
/// and the length is bounded, so a caller cannot hide or spoof parts of the prompt.
fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().take(600) {
        if c == '\n' || !c.is_control() {
            out.push(c);
        } else {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        }
    }
    if s.chars().count() > 600 {
        out.push_str(" ...");
    }
    out
}
