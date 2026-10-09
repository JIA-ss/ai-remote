//! The MCP clients (agents) connected to this controller right now: what the app's Agents settings
//! list, and a way to cut one off. A person's own calls (the app while someone drives) and one-off
//! `ai-remote call`s are not agent sessions and are not listed.

use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::Serialize;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize)]
pub struct AgentSession {
    pub id: u64,
    /// The MCP client's own name (`claude-code`), once it sent its first request.
    pub client: Option<String>,
    /// Unix milliseconds.
    pub since_ms: u64,
    /// When it last called a tool (or connected).
    pub last_ms: u64,
    pub calls: u64,
    #[serde(skip)]
    stop: CancellationToken,
}

#[derive(Default)]
pub struct AgentSessions {
    next: AtomicU64,
    list: Mutex<BTreeMap<u64, AgentSession>>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

impl AgentSessions {
    /// Registers a new session; the token is cancelled when someone disconnects it.
    pub fn open(&self) -> (u64, CancellationToken) {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let stop = CancellationToken::new();
        let now = now_ms();
        let session = AgentSession { id, client: None, since_ms: now, last_ms: now, calls: 0, stop: stop.clone() };
        self.list.lock().unwrap().insert(id, session);
        (id, stop)
    }

    pub fn close(&self, id: u64) {
        self.list.lock().unwrap().remove(&id);
    }

    pub fn named(&self, id: u64, client: &str) {
        if let Some(s) = self.list.lock().unwrap().get_mut(&id) {
            s.client = Some(client.to_string());
        }
    }

    pub fn called(&self, id: u64) {
        if let Some(s) = self.list.lock().unwrap().get_mut(&id) {
            s.calls += 1;
            s.last_ms = now_ms();
        }
    }

    pub fn list(&self) -> Vec<AgentSession> {
        self.list.lock().unwrap().values().cloned().collect()
    }

    /// Ends a session: its MCP client sees the connection close. False when there is no such session.
    pub fn stop(&self, id: u64) -> bool {
        match self.list.lock().unwrap().get(&id) {
            Some(s) => {
                s.stop.cancel();
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_are_listed_named_counted_and_stopped() {
        let sessions = AgentSessions::default();
        let (a, stop_a) = sessions.open();
        let (b, _) = sessions.open();
        assert_ne!(a, b);
        sessions.named(a, "claude-code");
        sessions.called(a);
        sessions.called(a);
        let list = sessions.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].client.as_deref(), Some("claude-code"));
        assert_eq!(list[0].calls, 2);
        assert!(list[1].client.is_none());

        assert!(sessions.stop(a));
        assert!(stop_a.is_cancelled());
        sessions.close(a);
        assert!(!sessions.stop(a));
        assert_eq!(sessions.list().len(), 1);
    }
}
