//! Who is driving this computer: the agent (the default), a person who took over from a paired
//! device, or nobody (paused). Take over, hand back, pause and resume arrive on the events channel
//! from a paired device; messages for the agent wait here and ride along with its next call result.
//!
//! This coordinates turns between people and agents; it is not a security boundary between them (an
//! agent and the person's app may share one device identity). What limits an agent is the gate:
//! levels, policy and approvals.

use std::sync::Mutex;

use anyhow::{Result, bail};
use ar_core::Level;
use serde::Serialize;
use serde_json::{Value, json};

use crate::audit::now_ms;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Driver {
    Agent,
    /// A person drives from `device`; the agent's calls are refused, except looking at the screen
    /// when `agent_may_watch`.
    Person {
        device: String,
        since_ms: u64,
        agent_may_watch: bool,
    },
    /// Nobody drives; the agent's calls are refused until someone resumes.
    Paused {
        by: String,
        since_ms: u64,
    },
}

/// A message for the agent from a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub from: String,
    pub text: String,
}

/// Longest message kept; longer ones are cut.
const MAX_NOTE: usize = 2000;
/// Messages kept for the agent before the oldest is dropped.
const MAX_NOTES: usize = 8;

pub struct Turns {
    driver: Mutex<Driver>,
    notes: Mutex<Vec<Note>>,
}

impl Default for Turns {
    fn default() -> Self {
        Self { driver: Mutex::new(Driver::Agent), notes: Mutex::default() }
    }
}

impl Turns {
    pub fn driver(&self) -> Driver {
        self.driver.lock().unwrap().clone()
    }

    /// `device` takes the controls; the agent stops at its next call.
    pub fn take_over(&self, device: &str, agent_may_watch: bool) -> Driver {
        let next = Driver::Person { device: device.to_string(), since_ms: now(), agent_may_watch };
        *self.driver.lock().unwrap() = next.clone();
        next
    }

    /// Gives the controls back to the agent, with an optional message telling it what changed.
    pub fn hand_back(&self, device: &str, note: Option<&str>) -> Result<Driver> {
        let mut driver = self.driver.lock().unwrap();
        if !matches!(*driver, Driver::Person { .. }) {
            bail!("nobody has taken over this computer");
        }
        *driver = Driver::Agent;
        drop(driver);
        let text = match note.map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => format!("They took over and handed back. They said: \"{}\"", clean(n)),
            None => "They took over and handed back.".to_string(),
        };
        self.push(Note { from: device.to_string(), text: format!("{text} The screen may have changed.") });
        Ok(Driver::Agent)
    }

    pub fn pause(&self, by: &str) -> Driver {
        let next = Driver::Paused { by: by.to_string(), since_ms: now() };
        *self.driver.lock().unwrap() = next.clone();
        next
    }

    pub fn resume(&self) -> Result<Driver> {
        let mut driver = self.driver.lock().unwrap();
        if !matches!(*driver, Driver::Paused { .. }) {
            bail!("the agent is not paused");
        }
        *driver = Driver::Agent;
        Ok(Driver::Agent)
    }

    /// Queues a message for the agent; it arrives with its next call result.
    pub fn tell(&self, from: &str, text: &str) -> Result<()> {
        let text = clean(text.trim());
        if text.is_empty() {
            bail!("the message is empty");
        }
        self.push(Note { from: from.to_string(), text: format!("\"{text}\"") });
        Ok(())
    }

    fn push(&self, note: Note) {
        let mut notes = self.notes.lock().unwrap();
        if notes.len() == MAX_NOTES {
            notes.remove(0);
        }
        notes.push(note);
    }

    /// The waiting messages, oldest first; they are delivered once.
    pub fn take_notes(&self) -> Vec<Note> {
        std::mem::take(&mut *self.notes.lock().unwrap())
    }

    /// Whether a call may run now. `person` says the call comes from a person's session (the app
    /// while someone drives), `level` is the tool's level. `Err` holds the sentence the agent gets.
    pub fn admits(&self, person: bool, level: Level, host: &str) -> Result<(), String> {
        match &*self.driver.lock().unwrap() {
            Driver::Agent => Ok(()),
            Driver::Person { .. } | Driver::Paused { .. } if person => Ok(()),
            Driver::Person { agent_may_watch: true, .. } if level == Level::Observe => Ok(()),
            Driver::Person { device, .. } => Err(format!(
                "A person took over {host} from {device}, so this call did not run. Do not retry right away: \
                 calls work again once they hand back. Tell the user what you were about to do."
            )),
            Driver::Paused { by, .. } => Err(format!(
                "{host} is paused (by {by}), so this call did not run. Do not retry right away: calls work \
                 again once someone resumes. Tell the user what you were about to do."
            )),
        }
    }
}

impl Driver {
    /// The audit / feed line for a change of driver.
    pub fn event(&self, by: &str) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        v["kind"] = "driver".into();
        v["by"] = by.into();
        v
    }
}

/// The text added to a call result for waiting messages.
pub fn notes_text(notes: &[Note]) -> String {
    let mut out = String::from("Messages from the owner while you worked:");
    for n in notes {
        out.push_str(&format!("\n- From {}: {}", n.from, n.text));
    }
    out.push_str("\nTake these into account before your next step.");
    out
}

fn clean(s: &str) -> String {
    let mut out: String =
        s.chars().filter(|c| *c == '\n' || !c.is_control()).take(MAX_NOTE).collect::<String>().replace('"', "'");
    if s.chars().count() > MAX_NOTE {
        out.push_str(" ...");
    }
    out
}

fn now() -> u64 {
    now_ms() as u64
}

/// The driver line for a feed's `hello`.
pub fn hello(driver: &Driver) -> Value {
    serde_json::to_value(driver).unwrap_or_else(|_| json!({"state": "agent"}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_who_takes_over_stops_the_agent_until_they_hand_back() {
        let t = Turns::default();
        assert!(t.admits(false, Level::Control, "pc").is_ok());
        t.take_over("laptop", false);
        let why = t.admits(false, Level::Control, "pc").unwrap_err();
        assert!(why.contains("took over pc from laptop"), "{why}");
        assert!(t.admits(false, Level::Observe, "pc").is_err(), "not watching");
        assert!(t.admits(true, Level::Control, "pc").is_ok(), "the person's own calls run");
        t.hand_back("laptop", Some("fixed the chart\u{7}")).unwrap();
        assert!(t.admits(false, Level::Control, "pc").is_ok());
        let notes = t.take_notes();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].text.contains("fixed the chart") && !notes[0].text.contains('\u{7}'));
        assert!(t.take_notes().is_empty(), "delivered once");
    }

    #[test]
    fn a_watching_agent_may_only_look() {
        let t = Turns::default();
        t.take_over("laptop", true);
        assert!(t.admits(false, Level::Observe, "pc").is_ok());
        assert!(t.admits(false, Level::Control, "pc").is_err());
    }

    #[test]
    fn pause_and_resume() {
        let t = Turns::default();
        assert!(t.resume().is_err());
        t.pause("phone");
        assert!(t.admits(false, Level::Observe, "pc").unwrap_err().contains("paused (by phone)"));
        t.resume().unwrap();
        assert_eq!(t.driver(), Driver::Agent);
        assert!(t.hand_back("x", None).is_err(), "nothing to hand back");
    }

    #[test]
    fn messages_are_bounded() {
        let t = Turns::default();
        assert!(t.tell("a", "   ").is_err());
        for i in 0..20 {
            t.tell("a", &format!("m{i}")).unwrap();
        }
        let notes = t.take_notes();
        assert_eq!(notes.len(), MAX_NOTES);
        assert!(notes[0].text.contains("m12"));
        assert!(notes_text(&notes).contains("From a: \"m19\""));
    }
}
