//! Who is here right now, for the owner's view at this computer (`status` on the control channel,
//! which the desktop app's tray shows): the controller connections open on every channel (tools,
//! activity feed, live view, files) and the tool calls running. Only reported, never used to decide
//! anything; revocation and forgetting close connections through [`crate::protocol::LiveConns`].

use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use serde::Serialize;

use crate::audit::now_ms;

/// The channel a controller connection was opened for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// MCP tool calls (agents, the CLI, the app's own calls).
    Tools,
    /// The activity feed (the app's computer page and session view).
    Events,
    /// Live video.
    Screen,
    /// File transfers.
    Files,
}

/// A tool call running now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunningCall {
    pub peer: String,
    pub tool: String,
    /// A person's call (someone took over and works from the desktop app), not an agent's.
    pub person: bool,
    pub since_ms: u64,
}

/// One connected controller and the channels it has open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Connected {
    /// The device principal (`eid:…`).
    pub device: String,
    pub channels: Vec<Channel>,
}

#[derive(Default)]
pub struct Presence {
    next: AtomicU64,
    conns: Mutex<HashMap<u64, (String, Channel)>>,
    calls: Mutex<HashMap<u64, RunningCall>>,
    /// The last tool call that ended, and when (milliseconds since the Unix epoch).
    last: Mutex<Option<EndedCall>>,
}

/// A tool call that ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EndedCall {
    #[serde(flatten)]
    pub call: RunningCall,
    pub ended_ms: u64,
}

impl Presence {
    /// Records an open connection of `device` (`eid:…`) on `channel` until the guard drops.
    pub fn connect(&self, device: String, channel: Channel) -> ConnGuard<'_> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.conns.lock().unwrap().insert(id, (device, channel));
        ConnGuard { presence: self, id }
    }

    /// Records a running tool call until the guard drops.
    pub fn call(&self, peer: &str, tool: &str, person: bool) -> CallGuard<'_> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let call = RunningCall { peer: peer.to_string(), tool: tool.to_string(), person, since_ms: now_ms() as u64 };
        self.calls.lock().unwrap().insert(id, call);
        CallGuard { presence: self, id }
    }

    /// Connected devices, each once, with their open channels (sorted, deduplicated).
    pub fn connected(&self) -> Vec<Connected> {
        let mut by_device: HashMap<String, Vec<Channel>> = HashMap::new();
        for (device, channel) in self.conns.lock().unwrap().values() {
            by_device.entry(device.clone()).or_default().push(*channel);
        }
        let mut out: Vec<Connected> = by_device
            .into_iter()
            .map(|(device, mut channels)| {
                channels.sort();
                channels.dedup();
                Connected { device, channels }
            })
            .collect();
        out.sort_by(|a, b| a.device.cmp(&b.device));
        out
    }

    /// Tool calls running now, oldest first.
    pub fn calls(&self) -> Vec<RunningCall> {
        let mut calls: Vec<RunningCall> = self.calls.lock().unwrap().values().cloned().collect();
        calls.sort_by_key(|c| c.since_ms);
        calls
    }

    /// The last tool call that ended, if one has.
    pub fn last_call(&self) -> Option<EndedCall> {
        self.last.lock().unwrap().clone()
    }
}

/// Removes a connection from [`Presence`] when dropped.
pub struct ConnGuard<'a> {
    presence: &'a Presence,
    id: u64,
}

impl Drop for ConnGuard<'_> {
    fn drop(&mut self) {
        self.presence.conns.lock().unwrap().remove(&self.id);
    }
}

/// Removes a running call from [`Presence`] when dropped, noting when it ended.
pub struct CallGuard<'a> {
    presence: &'a Presence,
    id: u64,
}

impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        if let Some(call) = self.presence.calls.lock().unwrap().remove(&self.id) {
            *self.presence.last.lock().unwrap() = Some(EndedCall { call, ended_ms: now_ms() as u64 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_and_calls_last_as_long_as_their_guards() {
        let p = Presence::default();
        assert!(p.connected().is_empty());
        assert_eq!(p.last_call(), None);
        let a = p.connect("eid:aa".into(), Channel::Screen);
        let b = p.connect("eid:aa".into(), Channel::Events);
        let c = p.connect("eid:aa".into(), Channel::Events);
        let d = p.connect("eid:bb".into(), Channel::Tools);
        assert_eq!(
            p.connected(),
            vec![
                Connected { device: "eid:aa".into(), channels: vec![Channel::Events, Channel::Screen] },
                Connected { device: "eid:bb".into(), channels: vec![Channel::Tools] },
            ]
        );
        drop((a, b, d));
        assert_eq!(p.connected(), vec![Connected { device: "eid:aa".into(), channels: vec![Channel::Events] }]);
        drop(c);
        assert!(p.connected().is_empty());

        let call = p.call("laptop", "screenshot", false);
        assert_eq!(p.calls().len(), 1);
        assert_eq!(p.calls()[0].tool, "screenshot");
        drop(call);
        assert!(p.calls().is_empty());
        let last = p.last_call().unwrap();
        assert_eq!(
            (last.call.peer.as_str(), last.call.tool.as_str(), last.call.person),
            ("laptop", "screenshot", false)
        );
        assert!(last.ended_ms >= last.call.since_ms);
    }
}
