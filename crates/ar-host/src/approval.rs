//! Owner approval for calls the policy marks `ask`.
//!
//! Two kinds of people can answer, and the first answer wins:
//!
//! - someone at this computer, in a native dialog on its interactive desktop (unless
//!   `approval_prompt = false`);
//! - a person at a paired device whose approver key this host accepted ([`crate::approvers`]). The
//!   request appears in the activity feed; allowing needs a signature from that key, which the
//!   device's OS produces only after Windows Hello, Touch ID or Face ID. Denying needs none.
//!
//! No answer before the timeout means denied, and so does having nobody who could answer (no
//! usable desktop here and no approver key).
//!
//! While a dialog is waiting for its answer, [`OpenPrompts`] counts it, and the input tools refuse to
//! inject anything: a controller must never be able to answer its own prompt, not even with a key
//! sequence that started before the prompt appeared.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use ar_core::approval::{self, AllowTarget, KeyAlg};
use ar_net::Peer;
use data_encoding::BASE64;
use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;

use crate::{
    approvers::{ApproverKey, ApproverKeys},
    audit::{AuditLog, now_ms},
    platform,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Approved,
    Denied,
    /// Nobody answered in time.
    Timeout,
    /// Nobody could be asked (no usable desktop here, no approver key).
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
    pub session: String,
    pub peer: String,
    pub principal: Option<String>,
    pub tool: String,
    pub intent: Option<String>,
    pub args_preview: String,
    pub args_hash: String,
}

/// How an approval ended.
#[derive(Debug, Clone)]
pub struct Resolution {
    pub verdict: Verdict,
    /// The approval's id; `None` when nobody could be asked.
    pub id: Option<String>,
    /// Who answered: `host` (someone at this computer) or a device name.
    pub by: Option<String>,
    /// What the person told the agent with a denial.
    pub note: Option<String>,
}

impl Resolution {
    fn unanswered(verdict: Verdict, id: Option<String>) -> Self {
        Self { verdict, id, by: None, note: None }
    }
}

/// Number of approval dialogs whose answer is still awaited. Cheap to clone; all clones share one
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

/// One counted prompt. Dropped when its answer arrives, when it times out or is answered elsewhere
/// (the dialog may stay on screen, but its answer is ignored, so input landing on it changes
/// nothing) and when the call that asked is cancelled.
struct PromptOpen(Arc<AtomicUsize>);

impl Drop for PromptOpen {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A call waiting for a person.
struct Pending {
    target: AllowTarget,
    session: String,
    peer: String,
    intent: Option<String>,
    args_preview: String,
    host_prompt: bool,
    done: oneshot::Sender<Answer>,
}

impl Pending {
    fn describe(&self) -> Value {
        json!({
            "id": self.target.id,
            "session": self.session,
            "peer": self.peer,
            "tool": self.target.tool,
            "intent": self.intent,
            "args_hash": self.target.args_hash,
            "args_preview": self.args_preview,
            "expires_ms": self.target.expires_ms,
            "host_prompt": self.host_prompt,
        })
    }
}

struct Answer {
    verdict: Verdict,
    by: String,
    note: Option<String>,
}

/// An answer sent by a paired device (the `answer` line of the events protocol).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteAnswer {
    pub id: String,
    pub decision: Decision,
    /// Told to the agent with a denial ("Tell the agent instead").
    #[serde(default)]
    pub note: Option<String>,
    /// For `allow`: the approver key (base64 DER SubjectPublicKeyInfo), its scheme and the base64
    /// signature over [`approval::allow_message`].
    #[serde(default)]
    pub alg: Option<KeyAlg>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Deny,
}

pub struct Approver {
    timeout: Duration,
    host_prompt: bool,
    open: OpenPrompts,
    pending: Mutex<HashMap<String, Pending>>,
    keys: ApproverKeys,
}

impl Approver {
    pub fn new(timeout: Duration, host_prompt: bool, keys: ApproverKeys) -> Self {
        Self { timeout, host_prompt, open: OpenPrompts::default(), pending: Mutex::default(), keys }
    }

    pub fn open_prompts(&self) -> OpenPrompts {
        self.open.clone()
    }

    pub fn keys(&self) -> &ApproverKeys {
        &self.keys
    }

    /// The calls waiting for a person right now.
    pub fn pending(&self) -> Vec<Value> {
        let pending = self.pending.lock().unwrap();
        let mut out: Vec<_> = pending.values().map(Pending::describe).collect();
        out.sort_by_key(|p| p["expires_ms"].as_u64());
        out
    }

    /// Asks for approval and waits for the first answer, the timeout or the caller's cancellation
    /// (dropping the future withdraws the request). `host_id` is this host's device principal.
    pub async fn ask(&self, host_id: &str, audit: &AuditLog, req: ApprovalRequest) -> Resolution {
        let blocker =
            if self.host_prompt { platform::prompt_blocker() } else { Some("approval_prompt = false".into()) };
        let remote = self.keys.any();
        if let (Some(reason), false) = (&blocker, remote) {
            tracing::info!(%reason, tool = %req.tool, "approval unavailable: no prompt here and no approver key");
            return Resolution::unanswered(Verdict::Unavailable, None);
        }
        let id = new_id();
        let expires_ms = (now_ms() + self.timeout.as_millis()) as u64;
        let (tx, rx) = oneshot::channel();
        let pending = Pending {
            target: AllowTarget {
                host: host_id.to_string(),
                id: id.clone(),
                tool: req.tool.clone(),
                args_hash: req.args_hash.clone(),
                expires_ms,
            },
            session: req.session.clone(),
            peer: req.peer.clone(),
            intent: req.intent.clone(),
            args_preview: req.args_preview.clone(),
            host_prompt: blocker.is_none(),
            done: tx,
        };
        let mut line = pending.describe();
        line["kind"] = "approval".into();
        line["principal"] = req.principal.clone().into();
        line["remote"] = remote.into();
        self.pending.lock().unwrap().insert(id.clone(), pending);
        audit.event(line);
        // Withdraws the request however this future ends (answer, timeout, cancellation).
        let mut guard = Withdraw { approver: self, audit, id: &id, started: now_ms(), resolved: false };

        // Counted before the dialog can exist, so input checked after this point never reaches it.
        let dialog = blocker.is_none().then(|| show_dialog(&req, self.timeout, self.open.open()));
        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut rx = rx;
        let local = async {
            match dialog {
                Some(d) => d.await.ok(),
                None => std::future::pending().await,
            }
        };
        let host = |verdict| Some(Answer { verdict, by: "host".into(), note: None });
        let mut verdict_if_unanswered = Verdict::Timeout;
        let answer = tokio::select! {
            shown = local => match shown {
                Some(MessageDialogResult::Yes) => host(Verdict::Approved),
                Some(_) => host(Verdict::Denied),
                // No dialog after all: an approver device may still answer.
                None if remote => tokio::select! {
                    a = &mut rx => a.ok(),
                    _ = tokio::time::sleep_until(deadline) => None,
                },
                None => {
                    verdict_if_unanswered = Verdict::Unavailable;
                    None
                }
            },
            a = &mut rx => a.ok(),
            _ = tokio::time::sleep_until(deadline) => None,
        };
        let resolution = match answer {
            Some(a) => Resolution { verdict: a.verdict, id: Some(id.clone()), by: Some(a.by), note: a.note },
            None => Resolution::unanswered(verdict_if_unanswered, Some(id.clone())),
        };
        guard.finish(&resolution);
        resolution
    }

    /// Applies an answer from the paired device `peer`. Allowing needs a signature from one of that
    /// device's accepted approver keys over exactly the waiting call.
    pub fn answer(&self, peer: &Peer, a: RemoteAnswer) -> Result<()> {
        let principal = peer.principal.as_deref().ok_or_else(|| anyhow!("this device has no device id"))?;
        let mut pending = self.pending.lock().unwrap();
        let Some(waiting) = pending.get(&a.id) else {
            bail!("No approval {} is waiting on this host (it was answered, timed out or never existed).", a.id);
        };
        if now_ms() as u64 >= waiting.target.expires_ms {
            bail!("Approval {} timed out.", a.id);
        }
        let answer = match a.decision {
            Decision::Deny => {
                Answer { verdict: Verdict::Denied, by: peer.name.clone(), note: a.note.map(|n| clean(&n, 500)) }
            }
            Decision::Allow => {
                let (Some(alg), Some(key), Some(sig)) = (a.alg, a.key.as_deref(), a.signature.as_deref()) else {
                    bail!("Allowing needs an approver key and its signature.");
                };
                let Some(accepted) = self.keys.find(principal, alg, key) else {
                    bail!(
                        "This host has not accepted that approver key for this device. Add it at the host first \
                         (it asks the person there to confirm)."
                    );
                };
                let der = BASE64.decode(accepted.key.as_bytes())?;
                let sig = BASE64.decode(sig.as_bytes()).map_err(|_| anyhow!("the signature is not base64"))?;
                approval::verify(alg, &der, &approval::allow_message(&waiting.target)?, &sig)?;
                Answer { verdict: Verdict::Approved, by: peer.name.clone(), note: None }
            }
        };
        let waiting = pending.remove(&a.id).expect("present above");
        // The asking call may have just gone away; then there is nothing left to answer.
        let _ = waiting.done.send(answer);
        Ok(())
    }

    /// Asks the person at this computer to accept `key` as an approver key of device `peer`.
    pub async fn enroll(&self, peer: &Peer, key: ApproverKey, audit: &AuditLog) -> Result<bool> {
        if self.keys.find(&key.principal, key.alg, &key.key).is_some() {
            return Ok(false);
        }
        if !self.host_prompt {
            bail!("This host does not show prompts (approval_prompt = false). At the host, run: {}", add_hint(&key));
        }
        if let Some(reason) = platform::prompt_blocker() {
            bail!(
                "Someone at the host has to confirm a new approver key, but no prompt can be shown there ({reason}). \
                 At the host, run: {}",
                add_hint(&key)
            );
        }
        let _open = self.open.open();
        let body = format!(
            "\"{}\" asks to approve actions on this computer from afar.\n\nKey: {} ({})\n\nAllow only if you are \
             setting this up yourself right now. You can remove it later with: ai-remote approvers remove \"{}\"",
            clean(&peer.name, 80),
            clean(&key.label, 80),
            key.fingerprint(),
            clean(&peer.name, 80),
        );
        let dialog = tokio::task::spawn_blocking(move || {
            MessageDialog::new()
                .set_level(MessageLevel::Warning)
                .set_title("ai-remote: new approver")
                .set_description(body)
                .set_buttons(MessageButtons::YesNo)
                .show()
        });
        match tokio::time::timeout(self.timeout, dialog).await {
            Ok(Ok(MessageDialogResult::Yes)) => {}
            Ok(Ok(_)) => bail!("The person at the host declined the approver key."),
            Ok(Err(_)) => bail!("The confirmation prompt could not be shown at the host."),
            Err(_) => bail!("Nobody confirmed the approver key at the host in time."),
        }
        let added = self.keys.add(key.clone())?;
        audit.event(json!({
            "kind": "approver_added",
            "peer": peer.name,
            "principal": key.principal,
            "alg": key.alg,
            "fingerprint": key.fingerprint(),
            "label": key.label,
        }));
        Ok(added)
    }
}

/// The host command that adds a key without a prompt.
fn add_hint(key: &ApproverKey) -> String {
    format!("ai-remote approvers add \"{}\" {}:{}", key.device, key.alg.as_str(), key.key)
}

/// Removes a pending request when its `ask` ends for any reason, and records how it ended.
struct Withdraw<'a> {
    approver: &'a Approver,
    audit: &'a AuditLog,
    id: &'a str,
    started: u128,
    resolved: bool,
}

impl Withdraw<'_> {
    fn finish(&mut self, r: &Resolution) {
        self.approver.pending.lock().unwrap().remove(self.id);
        self.record(r.verdict.as_str(), r.by.as_deref(), r.note.as_deref());
        self.resolved = true;
    }

    fn record(&self, verdict: &str, by: Option<&str>, note: Option<&str>) {
        self.audit.event(json!({
            "kind": "approval_resolved",
            "id": self.id,
            "verdict": verdict,
            "by": by,
            "note": note,
            "waited_ms": now_ms().saturating_sub(self.started),
        }));
    }
}

impl Drop for Withdraw<'_> {
    fn drop(&mut self) {
        if !self.resolved {
            self.approver.pending.lock().unwrap().remove(self.id);
            self.record("cancelled", None, None);
        }
    }
}

fn show_dialog(
    req: &ApprovalRequest,
    timeout: Duration,
    open: PromptOpen,
) -> tokio::task::JoinHandle<MessageDialogResult> {
    let secs = timeout.as_secs();
    let mut body = format!("\"{}\" wants to run: {}\n", clean(&req.peer, 80), req.tool);
    if let Some(intent) = &req.intent {
        body.push_str(&format!("\nWhy: {}\n", escape(intent)));
    }
    body.push_str(&format!("\nDetails:\n{}\n", escape(&req.args_preview)));
    body.push_str(&format!("\nAllow this once? (No answer within {secs} s counts as No.)"));
    tokio::task::spawn_blocking(move || {
        // Cancelling the asking future cannot close a native blocking dialog.
        // Keep input blocked until the actual dialog is dismissed.
        let _open = open;
        MessageDialog::new()
            .set_level(MessageLevel::Warning)
            .set_title("ai-remote: approve an action")
            .set_description(body)
            .set_buttons(MessageButtons::YesNo)
            .show()
    })
}

/// 128 random bits in hex: unguessable, so only subscribers of this host's feed can name a request.
fn new_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS random number generator");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One line of untrusted text: control characters dropped, length bounded.
fn clean(s: &str, max: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(max).collect()
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
    use ring::{
        rand::SystemRandom,
        signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair},
    };

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

    struct Fixture {
        _dir: tempfile::TempDir,
        approver: Arc<Approver>,
        audit: Arc<AuditLog>,
        pair: EcdsaKeyPair,
        key_b64: String,
    }

    fn peer(name: &str, principal: &str) -> Peer {
        Peer {
            name: name.into(),
            principal: Some(principal.into()),
            user_id: None,
            level: Some(ar_core::Level::Control),
            revoked: false,
        }
    }

    /// An approver with prompts off (tests never show dialogs) and one accepted key for `eid:phone`.
    fn fixture(timeout: Duration) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let keys = ApproverKeys::open(&dir.path().join("approvers.json")).unwrap();
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let der = approval::p256_spki(pair.public_key().as_ref()).unwrap();
        keys.add(ApproverKey::new("eid:phone", "phone", KeyAlg::EcdsaP256Sha256, &der, "test").unwrap()).unwrap();
        let audit = Arc::new(AuditLog::open(&dir.path().join("audit.jsonl")).unwrap());
        Fixture {
            approver: Arc::new(Approver::new(timeout, false, keys)),
            audit,
            pair,
            key_b64: BASE64.encode(&der),
            _dir: dir,
        }
    }

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            session: "s1".into(),
            peer: "laptop".into(),
            principal: Some("eid:laptop".into()),
            tool: "shell".into(),
            intent: Some("clean up".into()),
            args_preview: "{\"command\":\"del *.tmp\"}".into(),
            args_hash: "abcd".into(),
        }
    }

    /// Starts an `ask` and returns it with the waiting request's description.
    async fn ask(f: &Fixture) -> (tokio::task::JoinHandle<Resolution>, Value) {
        let (approver, audit) = (f.approver.clone(), f.audit.clone());
        let task = tokio::spawn(async move { approver.ask("eid:host", &audit, request()).await });
        for _ in 0..100 {
            if let Some(p) = f.approver.pending().pop() {
                return (task, p);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no pending approval");
    }

    fn signed(f: &Fixture, p: &Value, tool: &str) -> RemoteAnswer {
        let target = AllowTarget {
            host: "eid:host".into(),
            id: p["id"].as_str().unwrap().into(),
            tool: tool.into(),
            args_hash: p["args_hash"].as_str().unwrap().into(),
            expires_ms: p["expires_ms"].as_u64().unwrap(),
        };
        let sig = f.pair.sign(&SystemRandom::new(), &approval::allow_message(&target).unwrap()).unwrap();
        RemoteAnswer {
            id: target.id,
            decision: Decision::Allow,
            note: None,
            alg: Some(KeyAlg::EcdsaP256Sha256),
            key: Some(f.key_b64.clone()),
            signature: Some(BASE64.encode(sig.as_ref())),
        }
    }

    #[tokio::test]
    async fn a_signed_allow_from_an_approver_device_approves() {
        let f = fixture(Duration::from_secs(30));
        let (task, p) = ask(&f).await;
        f.approver.answer(&peer("phone", "eid:phone"), signed(&f, &p, "shell")).unwrap();
        let r = task.await.unwrap();
        assert_eq!(r.verdict, Verdict::Approved);
        assert_eq!(r.by.as_deref(), Some("phone"));
        assert!(f.approver.pending().is_empty());
        let (lines, _) = f.audit.subscribe();
        assert!(lines.iter().any(|l| l.contains("\"kind\":\"approval\"") && l.contains("\"remote\":true")));
        assert!(lines.iter().any(|l| l.contains("approval_resolved") && l.contains("\"verdict\":\"approved\"")));
    }

    #[tokio::test]
    async fn allowing_needs_the_right_device_key_and_call() {
        let f = fixture(Duration::from_secs(30));
        let (task, p) = ask(&f).await;
        // The asking device itself has no accepted key.
        let e = f.approver.answer(&peer("laptop", "eid:laptop"), signed(&f, &p, "shell")).unwrap_err();
        assert!(e.to_string().contains("not accepted"), "{e}");
        // A signature over another call.
        assert!(f.approver.answer(&peer("phone", "eid:phone"), signed(&f, &p, "jobs_kill")).is_err());
        // No signature.
        let unsigned = RemoteAnswer { signature: None, ..signed(&f, &p, "shell") };
        assert!(f.approver.answer(&peer("phone", "eid:phone"), unsigned).is_err());
        assert_eq!(f.approver.pending().len(), 1, "still waiting after refused answers");
        task.abort();
    }

    #[tokio::test]
    async fn any_device_can_deny_and_its_note_reaches_the_agent() {
        let f = fixture(Duration::from_secs(30));
        let (task, p) = ask(&f).await;
        let deny = RemoteAnswer {
            id: p["id"].as_str().unwrap().into(),
            decision: Decision::Deny,
            note: Some("not now\u{7}".into()),
            alg: None,
            key: None,
            signature: None,
        };
        f.approver.answer(&peer("laptop", "eid:laptop"), deny).unwrap();
        let r = task.await.unwrap();
        assert_eq!(r.verdict, Verdict::Denied);
        assert_eq!(r.note.as_deref(), Some("not now"));
        // A second answer finds nothing.
        assert!(f.approver.answer(&peer("phone", "eid:phone"), signed(&f, &p, "shell")).is_err());
    }

    #[tokio::test]
    async fn unanswered_requests_time_out_and_cancelled_ones_are_withdrawn() {
        let f = fixture(Duration::from_millis(200));
        let (task, _) = ask(&f).await;
        assert_eq!(task.await.unwrap().verdict, Verdict::Timeout);
        assert!(f.approver.pending().is_empty());

        let f = fixture(Duration::from_secs(30));
        let (task, _) = ask(&f).await;
        task.abort();
        let _ = task.await;
        assert!(f.approver.pending().is_empty());
        let (lines, _) = f.audit.subscribe();
        assert!(lines.iter().any(|l| l.contains("\"verdict\":\"cancelled\"")));
    }

    #[tokio::test]
    async fn without_prompt_or_approver_nobody_can_be_asked() {
        let dir = tempfile::tempdir().unwrap();
        let keys = ApproverKeys::open(&dir.path().join("approvers.json")).unwrap();
        let audit = AuditLog::open(&dir.path().join("audit.jsonl")).unwrap();
        let approver = Approver::new(Duration::from_secs(30), false, keys);
        let r = approver.ask("eid:host", &audit, request()).await;
        assert_eq!(r.verdict, Verdict::Unavailable);
        assert!(r.id.is_none());
    }
}
