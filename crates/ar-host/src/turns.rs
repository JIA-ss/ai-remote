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
use tokio_util::sync::CancellationToken;

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
        /// Paused by the owner at this computer (the desktop app's tray or page): a person's calls
        /// are refused too, and only the owner, here, can resume.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        owner: bool,
    },
}

/// A message for the agent from a person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub from: String,
    pub text: String,
}

/// Why a controller cannot take over or resume during the owner's pause.
const OWNER_PAUSED: &str = "the owner paused this computer at the computer itself; only they can resume it there";

/// Longest message kept; longer ones are cut.
const MAX_NOTE: usize = 2000;
/// Messages kept for the agent before the oldest is dropped.
const MAX_NOTES: usize = 8;

pub struct Turns {
    state: Mutex<State>,
    notes: Mutex<Vec<Note>>,
}

struct State {
    driver: Driver,
    control: CancellationToken,
    observe: CancellationToken,
    /// While the owner's pause holds: the driver to return to when they resume.
    before_owner_pause: Option<Driver>,
}

impl State {
    fn owner_paused(&self) -> bool {
        matches!(self.driver, Driver::Paused { owner: true, .. })
    }
}

impl State {
    /// Old control grants remain cancelled after handback/resume. Checking only
    /// the current driver would let queued work survive a quick takeover cycle.
    fn change(&mut self, next: Driver) -> Driver {
        self.control.cancel();
        self.control = CancellationToken::new();
        if !matches!(next, Driver::Agent | Driver::Person { agent_may_watch: true, .. }) {
            self.observe.cancel();
            self.observe = CancellationToken::new();
        }
        self.driver = next.clone();
        next
    }
}

impl Default for Turns {
    fn default() -> Self {
        Self {
            state: Mutex::new(State {
                driver: Driver::Agent,
                control: CancellationToken::new(),
                observe: CancellationToken::new(),
                before_owner_pause: None,
            }),
            notes: Mutex::default(),
        }
    }
}

impl Turns {
    pub fn driver(&self) -> Driver {
        self.state.lock().unwrap().driver.clone()
    }

    /// A cache lifetime, never an admission grant. Explicit human calls keep
    /// their existing semantics; cached native state must not cross driver changes.
    #[cfg(target_os = "macos")]
    pub(crate) fn input_cache_lifetime(&self) -> CancellationToken {
        self.state.lock().unwrap().control.clone()
    }

    /// `device` takes the controls; active and queued agent calls lose their grant. Refused while
    /// the owner's pause holds.
    pub fn take_over(&self, device: &str, agent_may_watch: bool) -> Result<Driver> {
        let mut state = self.state.lock().unwrap();
        if state.owner_paused() {
            bail!("{OWNER_PAUSED}");
        }
        let next = Driver::Person { device: device.to_string(), since_ms: now(), agent_may_watch };
        Ok(state.change(next))
    }

    /// Gives the controls back to the agent, with an optional message telling it what changed.
    pub fn hand_back(&self, device: &str, note: Option<&str>) -> Result<Driver> {
        let mut state = self.state.lock().unwrap();
        // Handing back during the owner's pause: their resume then goes to the agent.
        if state.owner_paused()
            && matches!(&state.before_owner_pause, Some(Driver::Person { device: d, .. }) if d == device)
        {
            state.before_owner_pause = Some(Driver::Agent);
            return Ok(state.driver.clone());
        }
        if !matches!(state.driver, Driver::Person { .. }) {
            bail!("nobody has taken over this computer");
        }
        state.change(Driver::Agent);
        drop(state);
        let text = match note.map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => format!("They took over and handed back. They said: \"{}\"", clean(n)),
            None => "They took over and handed back.".to_string(),
        };
        self.push(Note { from: device.to_string(), text: format!("{text} The screen may have changed.") });
        Ok(Driver::Agent)
    }

    /// A paired controller pauses the agent. The owner's pause, if on, stays as it is.
    pub fn pause(&self, by: &str) -> Driver {
        let mut state = self.state.lock().unwrap();
        if state.owner_paused() {
            return state.driver.clone();
        }
        state.change(Driver::Paused { by: by.to_string(), since_ms: now(), owner: false })
    }

    /// A paired controller lets the agent go on; not past the owner's pause.
    pub fn resume(&self) -> Result<Driver> {
        let mut state = self.state.lock().unwrap();
        if state.owner_paused() {
            bail!("{OWNER_PAUSED}");
        }
        if !matches!(state.driver, Driver::Paused { .. }) {
            bail!("the agent is not paused");
        }
        Ok(state.change(Driver::Agent))
    }

    /// The owner, at this computer, stops everyone: the agent and a person who took over. Kept
    /// until the owner resumes here; whoever drove before gets the controls back then.
    pub fn owner_pause(&self) -> Driver {
        let mut state = self.state.lock().unwrap();
        if state.owner_paused() {
            return state.driver.clone();
        }
        let before = match &state.driver {
            Driver::Person { .. } => state.driver.clone(),
            _ => Driver::Agent,
        };
        state.before_owner_pause = Some(before);
        state.change(Driver::Paused { by: "owner".into(), since_ms: now(), owner: true })
    }

    /// The owner lifts any pause (theirs or a controller's); nothing to do when none holds.
    pub fn owner_resume(&self) -> Driver {
        let mut state = self.state.lock().unwrap();
        if !matches!(state.driver, Driver::Paused { .. }) {
            return state.driver.clone();
        }
        let next = state.before_owner_pause.take().unwrap_or(Driver::Agent);
        state.change(next)
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

    /// Admits a call and snapshots its control grant under the same lock. A grant
    /// is cancelled when that access ends; it never revives after handback.
    pub fn admits(&self, person: bool, level: Level, host: &str) -> Result<CancellationToken, String> {
        let state = self.state.lock().unwrap();
        if person && !state.owner_paused() {
            return Ok(CancellationToken::new());
        }
        let grant = || if level == Level::Observe { state.observe.clone() } else { state.control.clone() };
        match &state.driver {
            Driver::Agent => Ok(grant()),
            Driver::Person { agent_may_watch: true, .. } if level == Level::Observe => Ok(grant()),
            Driver::Person { device, .. } => Err(format!(
                "A person took over {host} from {device}, so this call did not run. Do not retry right away: \
                 calls work again once they hand back. Tell the user what you were about to do."
            )),
            Driver::Paused { owner: true, .. } => Err(format!(
                "The owner of {host} paused it at that computer, so this call did not run. Only they can resume it \
                 there; do not retry. Tell the user what you were about to do."
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
        t.take_over("laptop", false).unwrap();
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

    #[cfg(target_os = "macos")]
    #[test]
    fn native_cache_lifetimes_never_cross_driver_changes_or_change_human_admission() {
        let t = Turns::default();
        let before = t.input_cache_lifetime();
        let human = t.admits(true, Level::Control, "pc").unwrap();
        t.take_over("laptop", false).unwrap();
        assert!(before.is_cancelled());
        let taken = t.input_cache_lifetime();
        t.hand_back("laptop", None).unwrap();
        assert!(taken.is_cancelled());
        t.take_over("laptop", false).unwrap();
        let fresh = t.input_cache_lifetime();
        assert!(!fresh.is_cancelled());
        t.pause("laptop");
        assert!(fresh.is_cancelled());
        assert!(!human.is_cancelled(), "cache invalidation must not redefine explicit human input grants");
    }

    #[test]
    fn a_watching_agent_may_only_look() {
        let t = Turns::default();
        t.take_over("laptop", true).unwrap();
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
    fn takeover_and_handback_do_not_revive_old_or_queued_grants() {
        let t = Turns::default();
        let active = t.admits(false, Level::Control, "pc").unwrap();
        let queued = t.admits(false, Level::Admin, "pc").unwrap();
        let watching = t.admits(false, Level::Observe, "pc").unwrap();
        let human = t.admits(true, Level::Control, "pc").unwrap();
        t.take_over("phone", true).unwrap();
        t.hand_back("phone", None).unwrap();
        assert!(active.is_cancelled() && queued.is_cancelled());
        assert!(!watching.is_cancelled(), "watching was allowed throughout");
        assert!(!human.is_cancelled(), "human calls keep their existing semantics");
        assert!(!t.admits(false, Level::Control, "pc").unwrap().is_cancelled());
    }

    #[test]
    fn pause_invalidates_observation_and_control_even_after_resume() {
        let t = Turns::default();
        let read = t.admits(false, Level::Observe, "pc").unwrap();
        let write = t.admits(false, Level::Control, "pc").unwrap();
        t.pause("phone");
        t.resume().unwrap();
        assert!(read.is_cancelled() && write.is_cancelled());
        assert!(!t.admits(false, Level::Observe, "pc").unwrap().is_cancelled());
        let read = t.admits(false, Level::Observe, "pc").unwrap();
        t.take_over("phone", false).unwrap();
        assert!(read.is_cancelled());
    }

    #[test]
    fn the_owners_pause_stops_everyone_and_only_the_owner_lifts_it() {
        let t = Turns::default();
        t.take_over("laptop", false).unwrap();
        let paused = t.owner_pause();
        assert!(matches!(paused, Driver::Paused { owner: true, .. }));
        assert!(t.admits(true, Level::Control, "pc").is_err(), "a person's calls stop too");
        assert!(t.admits(false, Level::Observe, "pc").unwrap_err().contains("owner of pc"));
        assert!(t.resume().is_err() && t.take_over("phone", false).is_err(), "controllers cannot lift it");
        assert_eq!(t.pause("phone"), paused, "a controller's pause does not replace it");
        // The person who drove gets the controls back.
        assert!(matches!(t.owner_resume(), Driver::Person { device, .. } if device == "laptop"));
        assert!(matches!(t.owner_resume(), Driver::Person { .. }), "resuming again changes nothing");
        // Handing back during the pause makes the resume go to the agent.
        t.owner_pause();
        t.hand_back("laptop", None).unwrap();
        assert_eq!(t.owner_resume(), Driver::Agent);
        // The owner lifts a controller's pause as well.
        t.pause("phone");
        assert_eq!(t.owner_resume(), Driver::Agent);
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
