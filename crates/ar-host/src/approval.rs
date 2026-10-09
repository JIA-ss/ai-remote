//! Owner approval: a native dialog on the host's interactive desktop.
//!
//! A host on an unattended machine has nobody to ask, so "cannot ask" always means "denied": when
//! the desktop is locked or on the secure desktop, when no dialog can be shown, or when nobody
//! answers before the timeout.
//!
//! While a prompt is waiting for its answer, [`OpenPrompts`] counts it, and the input tools refuse to
//! inject anything: a controller must never be able to answer its own prompt, not even with a key
//! sequence that started before the prompt appeared.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

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

/// Number of approval prompts whose answer is still awaited. Cheap to clone; all clones share one
/// count.
#[derive(Clone, Default)]
pub struct OpenPrompts(Arc<AtomicUsize>);

impl OpenPrompts {
    pub fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }

    /// Counts one prompt until the returned guard is dropped.
    fn open(&self) -> PromptOpen {
        self.0.fetch_add(1, Ordering::SeqCst);
        PromptOpen(self.0.clone())
    }
}

/// One counted prompt. Dropped when its answer arrives, when it times out (the dialog may stay on
/// screen, but its answer is ignored, so input landing on it changes nothing) and when the call that
/// asked is cancelled.
struct PromptOpen(Arc<AtomicUsize>);

impl Drop for PromptOpen {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Approver {
    timeout: Duration,
    open: OpenPrompts,
}

impl Approver {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            open: OpenPrompts::default(),
        }
    }

    pub fn open_prompts(&self) -> OpenPrompts {
        self.open.clone()
    }

    pub async fn ask(&self, req: ApprovalRequest) -> Verdict {
        if let Some(reason) = platform::prompt_blocker() {
            tracing::info!(%reason, tool = %req.tool, "approval unavailable");
            return Verdict::Unavailable;
        }
        // Counted before the dialog can exist, so input checked after this point never reaches it.
        let _open = self.open.open();
        let secs = self.timeout.as_secs();
        let mut body = format!("\"{}\" wants to run: {}\n", req.peer, req.tool);
        if let Some(intent) = &req.intent {
            body.push_str(&format!("\nWhy: {}\n", escape(intent)));
        }
        body.push_str(&format!("\nDetails:\n{}\n", escape(&req.args_preview)));
        body.push_str(&format!(
            "\nAllow this once? (No answer within {secs} s counts as No.)"
        ));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_prompts_until_their_guard_drops() {
        let open = OpenPrompts::default();
        let shared = open.clone();
        assert_eq!(open.count(), 0);
        let a = open.open();
        let b = shared.open();
        assert_eq!(shared.count(), 2);
        drop(a);
        assert_eq!(open.count(), 1);
        drop(b);
        assert_eq!(open.count(), 0);
    }
}
