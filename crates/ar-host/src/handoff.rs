//! Which instance of the macOS system agent serves the profile.
//!
//! `ai-remote host install --service` on macOS registers one LaunchAgent for the LoginWindow and
//! the Aqua sessions. launchd starts one instance per GUI session, and with fast user switching
//! the user's session keeps running in the background while the login window shows: two
//! instances of the same agent, both (after [`adopt_owner`](crate::platform::macos::adopt_owner))
//! running as the same user against the same profile. Only one can hold the profile (its control
//! endpoint's lock, see [`Claim`](crate::Claim)), and it must be the one whose session has the
//! screen.
//!
//! The rule, the same for both instances: **only an instance whose session is on the console
//! holds the profile; one whose session leaves the console steps aside, exiting while it still
//! holds the lock.** Exiting (rather than stopping inside the process) frees the control lock, the
//! transport's database lock and its UDP socket in the same instant, so the next holder never
//! finds the transport busy and two copies of this device never run at once.
//!
//! `ai-remote stop-host` must stay a stop and not become "hand over to the other instance": the
//! stopping host leaves a [`StopMarker`] that waiting instances honour (see [`honored`]).
//!
//! The decisions are pure functions here ([`decide`], [`WaitLog`], [`ConsoleWatch`], [`honored`]);
//! `ai-remote-hostw` does the waiting and the process exits. Nothing here runs for the Windows
//! service, a per-user LaunchAgent or a host in a terminal.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result};
use ar_core::{Profile, Role};
use serde::{Deserialize, Serialize};

/// Exit code of a system-agent host that stepped aside because its session left the console.
/// Not 0, so launchd starts the instance again; that one waits quietly until its session has the
/// console again.
pub const EXIT_STEPPED_ASIDE: u8 = 4;

/// Exit code after launchd (or anyone) sent SIGTERM: 128 + SIGTERM, as a shell reports it.
pub const EXIT_TERMINATED: u8 = 143;

/// What an instance of the system agent knows when it decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Situation {
    /// This process is not the profile owner's (another user's Aqua session): never touch the
    /// owner's profile.
    pub foreign_user: bool,
    /// A stop marker this instance honours (see [`honored`]).
    pub stopped: bool,
    /// What this instance reads about the console (see [`ConsoleRead`]).
    pub console: ConsoleRead,
    /// This is the LoginWindow-session instance (launchd started it as root).
    pub login_window: bool,
    pub claim: ClaimState,
}

/// Where this instance stands with the profile's slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimState {
    /// Not tried in this round.
    NotTried,
    /// Another host holds it (the other session's instance, or a host in a terminal).
    Taken,
    /// This instance holds it.
    Held,
}

/// Why an instance waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Its session does not have the screen.
    OffConsole,
    /// Its session has the screen, but another host holds the profile.
    Taken,
}

impl Wait {
    /// How long to wait before looking again.
    pub fn interval(self) -> Duration {
        match self {
            // Nothing to do until the user comes back; a switch is seen within two seconds.
            Wait::OffConsole => Duration::from_secs(2),
            // The holder is about to leave (it noticed the switch): take over quickly.
            Wait::Taken => Duration::from_secs(1),
        }
    }
}

/// What to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Another user's session: exit 0 (launchd does not start it again).
    NotOurs,
    /// Stopped by `stop-host`: release the slot if held and exit 0.
    Stopped,
    /// Release the slot if held, then look again after [`Wait::interval`].
    Wait(Wait),
    /// Try to claim the slot, then decide again with the outcome.
    TryClaim,
    /// Run the host with the slot held.
    Serve,
}

/// One look at the console from an instance of the system agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConsoleRead {
    /// `kCGSSessionOnConsoleKey` of this process's session (`None`: not readable).
    pub session: Option<bool>,
    /// Owner of `/dev/console` (`None`: not readable): root while the login window has the
    /// screen, the user whose session has it otherwise.
    pub device_owner: Option<u32>,
}

impl ConsoleRead {
    /// A definite session read, as the Aqua instance (and normally the LoginWindow one) sees it.
    pub fn session(on: bool) -> Self {
        Self { session: Some(on), device_owner: None }
    }
}

/// Whether this instance's session counts as having the screen. A definite session read always
/// wins. When the session dictionary is unreadable the Aqua instance counts as off the console, so
/// it never keeps a backgrounded session's slot on a guess; the LoginWindow instance (whose read
/// after its identity switch is not verified) falls back to `/dev/console`: it counts as on the
/// console only while root owns it (the login window has the screen), so it gives the profile back
/// as soon as a user's session has the screen again, even if launchd keeps it running. Only when
/// neither can be read does it keep serving on a guess, rather than leave the login window
/// unserved.
pub fn on_console(read: ConsoleRead, login_window: bool) -> bool {
    match (read.session, login_window, read.device_owner) {
        (Some(on), _, _) => on,
        (None, false, _) => false,
        (None, true, Some(owner)) => owner == 0,
        (None, true, None) => true,
    }
}

/// The waiting loop's decision (see the module docs for the rule).
pub fn decide(s: &Situation) -> Decision {
    if s.foreign_user {
        Decision::NotOurs
    } else if s.stopped {
        Decision::Stopped
    } else if !on_console(s.console, s.login_window) {
        Decision::Wait(Wait::OffConsole)
    } else {
        match s.claim {
            ClaimState::NotTried => Decision::TryClaim,
            ClaimState::Taken => Decision::Wait(Wait::Taken),
            ClaimState::Held => Decision::Serve,
        }
    }
}

/// Turns the waiting loop's decisions into log lines: one line per change, never one per poll,
/// so an instance that waits all day leaves one line.
#[derive(Debug, Default)]
pub struct WaitLog {
    last: Option<Decision>,
}

impl WaitLog {
    /// The line to log for `d`, if it is news. Claim attempts are not news.
    pub fn note(&mut self, d: Decision) -> Option<&'static str> {
        if d == Decision::TryClaim || self.last == Some(d) {
            return None;
        }
        self.last = Some(d);
        Some(match d {
            Decision::NotOurs => "not starting: this is another user's session, and the host belongs to its owner",
            Decision::Stopped => "not starting: the host was stopped with `ai-remote stop-host` (see `host status`)",
            Decision::Wait(Wait::OffConsole) => {
                "waiting: this session is not on the console (the screen shows another session or the login window)"
            }
            Decision::Wait(Wait::Taken) => {
                "waiting: another host holds this profile (the other session's instance, or a host in a terminal)"
            }
            Decision::Serve => "this session has the console: taking over the profile",
            Decision::TryClaim => unreachable!("filtered above"),
        })
    }
}

/// Watches a serving instance's session: it steps aside once its session has been off the console
/// for [`NEEDED`](Self::NEEDED) reads in a row, so one odd read does not drop every connection.
/// The lock screen keeps the session on the console and never triggers it.
#[derive(Debug, Clone)]
pub struct ConsoleWatch {
    login_window: bool,
    off: u8,
}

impl ConsoleWatch {
    pub const NEEDED: u8 = 2;
    /// Time between reads.
    pub const PERIOD: Duration = Duration::from_secs(1);

    pub fn new(login_window: bool) -> Self {
        Self { login_window, off: 0 }
    }

    /// Records one read; true when the instance must step aside.
    pub fn observe(&mut self, console: ConsoleRead) -> bool {
        if on_console(console, self.login_window) {
            self.off = 0;
        } else {
            self.off = self.off.saturating_add(1);
        }
        self.off >= Self::NEEDED
    }
}

/// Reads the session's console flag every [`ConsoleWatch::PERIOD`] and returns once the instance
/// must step aside.
pub async fn watch_console(login_window: bool, mut read: impl FnMut() -> ConsoleRead) {
    let mut watch = ConsoleWatch::new(login_window);
    let mut tick = tokio::time::interval(ConsoleWatch::PERIOD);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if watch.observe(read()) {
            return;
        }
    }
}

/// Left by a host stopped with `ai-remote stop-host` (macOS), in `<profile>/host/stopped`, so the
/// system agent's other instance does not take over what the owner stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopMarker {
    /// `kern.bootsessionuuid` when it was written: the marker means nothing after a restart.
    pub boot: String,
    /// Audit session id of the host that stopped: the login session it was stopped in.
    pub asid: i32,
    /// Unix seconds, for `host status`.
    pub at: u64,
    /// The stopped host was the LoginWindow instance: the owner stopped it from the login window,
    /// so the stop also holds for the user sessions waiting in the background.
    #[serde(default)]
    pub login_window: bool,
}

/// Whether an instance honours `marker`: written in this boot, and either in this instance's own
/// login session (the lock screen, a switch back to it), or this instance serves the login window,
/// or the stop was made at the login window. So a stop made in a user session holds until the
/// next real login (a new session) or restart, one made at the login window until the next
/// restart; `host install` clears either.
pub fn honored(marker: Option<&StopMarker>, boot: Option<&str>, asid: Option<i32>, login_window: bool) -> bool {
    let (Some(marker), Some(boot)) = (marker, boot) else { return false };
    marker.boot == boot && (login_window || marker.login_window || asid == Some(marker.asid))
}

/// Whether a marker no longer means anything to anyone and may be removed: unreadable (`corrupt`)
/// or from an earlier boot. A marker of this boot is never removed by an instance that does not
/// honour it: it may be meant for the other instance.
pub fn stale(marker: Option<&StopMarker>, corrupt: bool, boot: Option<&str>) -> bool {
    corrupt || matches!((marker, boot), (Some(m), Some(boot)) if m.boot != boot)
}

/// `<profile>/host/stopped`.
pub fn marker_path(profile: &Profile) -> Result<PathBuf> {
    Ok(profile.role_dir(Role::Host)?.join("stopped"))
}

/// Reads the marker: `Ok(None)` when there is none, `Err` when it is unreadable or corrupt.
pub fn read_marker(path: &Path) -> Result<Option<StopMarker>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(parse_marker(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

fn parse_marker(bytes: &[u8]) -> Result<StopMarker> {
    serde_json::from_slice(bytes).context("parse the stop marker")
}

/// Writes the marker atomically (a temporary file renamed over it).
pub fn write_marker(path: &Path, marker: &StopMarker) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(marker)?).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Removes the marker (absent is fine).
pub fn clear_marker(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove {}", path.display())),
    }
}

/// The marker for a host stopping now in this process's session (macOS); `None` when the boot or
/// session id cannot be read.
#[cfg(target_os = "macos")]
pub fn marker_here() -> Option<StopMarker> {
    use crate::platform::macos;
    Some(StopMarker {
        boot: macos::boot_session()?,
        asid: macos::audit_session_id()?,
        at: ar_core::unix_now(),
        login_window: macos::login_window_instance(),
    })
}

/// What the system agent reads about the console now (macOS).
#[cfg(target_os = "macos")]
pub fn read_console() -> ConsoleRead {
    use crate::platform::macos;
    ConsoleRead { session: macos::on_console(), device_owner: macos::console_device_owner() }
}

/// Records that the host serving `profile` is being stopped on request (macOS): written before
/// the slot is released, so an instance waiting for it finds the marker first. Only the system
/// agent records a stop (`Ok(false)` elsewhere): stopping a host in a terminal, or the per-user
/// LaunchAgent, leaves the system agent free to take the profile back.
#[cfg(target_os = "macos")]
pub fn record_stop(profile: &Profile) -> Result<bool> {
    if !crate::platform::macos::login_window_agent() {
        return Ok(false);
    }
    let marker = marker_here().context("the boot or login session id cannot be read")?;
    write_marker(&marker_path(profile)?, &marker)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(console: Option<bool>, login_window: bool, claim: ClaimState) -> Situation {
        Situation { foreign_user: false, stopped: false, console: r(console), login_window, claim }
    }

    /// A read with only the session flag (no `/dev/console` owner).
    fn r(session: Option<bool>) -> ConsoleRead {
        ConsoleRead { session, device_owner: None }
    }

    /// A read with an unreadable session and `/dev/console` owned by `owner`.
    fn dev(owner: u32) -> ConsoleRead {
        ConsoleRead { session: None, device_owner: Some(owner) }
    }

    /// One round of the waiting loop, the way `ai-remote-hostw` runs it: decide, and when told to
    /// claim, decide again with the claim's outcome (and a fresh look at the marker).
    fn round(s: Situation, slot_free: bool, stopped_after_claim: bool) -> Decision {
        match decide(&s) {
            Decision::TryClaim if slot_free => {
                decide(&Situation { claim: ClaimState::Held, stopped: stopped_after_claim, ..s })
            }
            Decision::TryClaim => decide(&Situation { claim: ClaimState::Taken, ..s }),
            d => d,
        }
    }

    use ClaimState::*;
    const A: bool = false; // the Aqua (user session) instance
    const L: bool = true; // the LoginWindow instance

    #[test]
    fn fast_user_switching_to_the_login_window_hands_the_profile_over() {
        // A serves; L waits while A holds the profile.
        assert_eq!(round(at(Some(true), L, NotTried), false, false), Decision::Wait(Wait::Taken));
        // The switch: A's session leaves the console; two reads in a row and it steps aside.
        let mut watch = ConsoleWatch::new(A);
        assert!(!watch.observe(r(Some(false))));
        assert!(watch.observe(r(Some(false))));
        // A exited holding the lock; L's next round claims and serves.
        assert_eq!(round(at(Some(true), L, NotTried), true, false), Decision::Serve);
        // launchd starts A again (exit 4): it waits off the console without claiming.
        assert_eq!(round(at(Some(false), A, NotTried), true, false), Decision::Wait(Wait::OffConsole));
    }

    #[test]
    fn switching_back_returns_the_profile_to_the_user_session() {
        // Whether or not launchd ends L, L's session leaves the console and L steps aside.
        let mut watch = ConsoleWatch::new(L);
        assert!(!watch.observe(r(Some(false))));
        assert!(watch.observe(r(Some(false))));
        // A was waiting off the console; now on it, it claims the freed slot.
        assert_eq!(round(at(Some(true), A, NotTried), true, false), Decision::Serve);
        // Had L not left yet, A waits for it instead of failing.
        assert_eq!(round(at(Some(true), A, NotTried), false, false), Decision::Wait(Wait::Taken));
    }

    #[test]
    fn logout_and_restart_leave_one_fresh_instance_that_claims() {
        // Logout: A died with its session; the new login window's L finds the slot free.
        assert_eq!(round(at(Some(true), L, NotTried), true, false), Decision::Serve);
        // Restart, then login: A starts while L may still hold the slot, and waits briefly.
        assert_eq!(round(at(Some(true), A, NotTried), false, false), Decision::Wait(Wait::Taken));
        assert_eq!(round(at(Some(true), A, NotTried), true, false), Decision::Serve);
    }

    #[test]
    fn the_lock_screen_keeps_the_session_on_the_console() {
        // Locked: same Aqua session, still on the console; the watch never fires.
        let mut watch = ConsoleWatch::new(A);
        for _ in 0..10 {
            assert!(!watch.observe(r(Some(true))));
        }
    }

    #[test]
    fn a_stopped_host_stays_stopped_even_when_the_slot_frees() {
        let mut s = at(Some(true), L, NotTried);
        s.stopped = true;
        assert_eq!(round(s, true, true), Decision::Stopped);
        // The marker appears between the holder's stop and the claim: the claim is given back.
        assert_eq!(round(at(Some(true), L, NotTried), true, true), Decision::Stopped);
        // Off the console and stopped: stopped wins (exit 0, launchd does not restart it).
        let mut off = at(Some(false), A, NotTried);
        off.stopped = true;
        assert_eq!(decide(&off), Decision::Stopped);
    }

    #[test]
    fn after_a_crash_the_waiting_instance_takes_over() {
        // The holder crashed; the kernel freed the lock; whoever has the console claims.
        assert_eq!(round(at(Some(true), A, NotTried), true, false), Decision::Serve);
        // The crashed instance, restarted by launchd, finds it taken and waits.
        assert_eq!(round(at(Some(true), A, NotTried), false, false), Decision::Wait(Wait::Taken));
    }

    #[test]
    fn a_host_in_a_terminal_keeps_the_profile_and_the_agent_logs_one_line() {
        let mut log = WaitLog::default();
        let mut lines = Vec::new();
        for _ in 0..30 {
            let d = round(at(Some(true), A, NotTried), false, false);
            lines.extend(log.note(d));
        }
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("another host"));
        // It quits with Ctrl+C: the agent takes over and says so once.
        let d = round(at(Some(true), A, NotTried), true, false);
        assert_eq!(d, Decision::Serve);
        assert_eq!(log.note(d).map(|l| l.contains("taking over")), Some(true));
    }

    #[test]
    fn repeated_waits_log_once_per_change() {
        let mut log = WaitLog::default();
        let seq = [
            Decision::Wait(Wait::OffConsole),
            Decision::TryClaim,
            Decision::Wait(Wait::OffConsole),
            Decision::Wait(Wait::Taken),
            Decision::TryClaim,
            Decision::Wait(Wait::Taken),
            Decision::Wait(Wait::OffConsole),
        ];
        let logged = seq.iter().filter(|d| log.note(**d).is_some()).count();
        assert_eq!(logged, 3);
    }

    #[test]
    fn another_users_session_never_touches_the_profile() {
        let mut s = at(Some(true), A, NotTried);
        s.foreign_user = true;
        assert_eq!(decide(&s), Decision::NotOurs);
    }

    #[test]
    fn an_unreadable_session_counts_as_on_the_console_only_at_the_login_window() {
        assert!(on_console(r(None), L));
        assert!(!on_console(r(None), A));
        assert_eq!(round(at(None, L, NotTried), true, false), Decision::Serve);
        assert_eq!(round(at(None, A, NotTried), true, false), Decision::Wait(Wait::OffConsole));
        let mut l = ConsoleWatch::new(L);
        assert!(!l.observe(r(None)) && !l.observe(r(None)));
        let mut a = ConsoleWatch::new(A);
        assert!(!a.observe(r(None)) && a.observe(r(None)));
    }

    #[test]
    fn one_odd_read_does_not_step_aside() {
        let mut w = ConsoleWatch::new(A);
        assert!(!w.observe(r(Some(false))));
        assert!(!w.observe(r(Some(true))));
        assert!(!w.observe(r(Some(false))));
        assert!(w.observe(r(Some(false))));
    }

    #[test]
    fn waits_poll_at_the_documented_pace() {
        assert_eq!(Wait::OffConsole.interval(), Duration::from_secs(2));
        assert_eq!(Wait::Taken.interval(), Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn the_console_watch_reads_once_a_second_and_needs_two_off_reads() {
        let start = tokio::time::Instant::now();
        let reads = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seq = [Some(true), Some(false), Some(true), Some(false), Some(false), Some(true)];
        let log = reads.clone();
        let mut i = 0;
        watch_console(A, move || {
            log.lock().unwrap().push(tokio::time::Instant::now() - start);
            let v = r(seq[i]);
            i += 1;
            v
        })
        .await;
        let reads = reads.lock().unwrap();
        assert_eq!(reads.len(), 5);
        for pair in reads.windows(2) {
            assert!(pair[1] - pair[0] >= ConsoleWatch::PERIOD);
        }
    }

    #[test]
    fn stop_markers_hold_until_the_next_login_or_restart() {
        let m = StopMarker { boot: "B1".into(), asid: 100, at: 0, login_window: false };
        // Same session (lock screen, switching back): stopped.
        assert!(honored(Some(&m), Some("B1"), Some(100), A));
        // The login window in this boot: stopped.
        assert!(honored(Some(&m), Some("B1"), Some(7), L));
        // A new login session: no longer.
        assert!(!honored(Some(&m), Some("B1"), Some(101), A));
        // A restart: no longer, anywhere.
        assert!(!honored(Some(&m), Some("B2"), Some(100), A));
        assert!(!honored(Some(&m), Some("B2"), Some(7), L));
        // Unknown boot or no marker: not stopped.
        assert!(!honored(Some(&m), None, Some(100), A));
        assert!(!honored(None, Some("B1"), Some(100), L));
        assert!(!honored(Some(&m), Some("B1"), None, A));
    }

    #[test]
    fn the_login_window_instance_falls_back_to_the_console_device_and_gives_the_profile_back() {
        // Unreadable session, root owns /dev/console: the login window has the screen.
        assert!(on_console(dev(0), L));
        // A user's session has the screen (a switch back, or another user): L is off the console,
        // even if launchd keeps it running, and steps aside after two reads.
        assert!(!on_console(dev(501), L));
        let mut l = ConsoleWatch::new(L);
        assert!(!l.observe(dev(0)));
        assert!(!l.observe(dev(501)));
        assert!(l.observe(dev(501)));
        // A redundant L while user B has the screen never claims the profile A freed.
        let s = Situation { console: dev(502), ..at(None, L, NotTried) };
        assert_eq!(decide(&s), Decision::Wait(Wait::OffConsole));
        // A definite session read wins over the device owner.
        assert!(!on_console(ConsoleRead { session: Some(false), device_owner: Some(0) }, L));
        assert!(on_console(ConsoleRead { session: Some(true), device_owner: Some(501) }, L));
        // The Aqua instance never serves on a guess.
        assert!(!on_console(dev(501), A));
    }

    #[test]
    fn a_stop_at_the_login_window_holds_for_the_waiting_user_session() {
        let m = StopMarker { boot: "B1".into(), asid: 7, at: 0, login_window: true };
        // A, waiting in the background with another session id, honours it.
        assert!(honored(Some(&m), Some("B1"), Some(100), A));
        // Until a restart.
        assert!(!honored(Some(&m), Some("B2"), Some(100), A));
    }

    #[test]
    fn only_markers_stale_for_everyone_are_removed() {
        let m = StopMarker { boot: "B1".into(), asid: 7, at: 0, login_window: false };
        // Same boot, another session's stop: not ours to honour, and not ours to remove either.
        assert!(!honored(Some(&m), Some("B1"), Some(100), A));
        assert!(!stale(Some(&m), false, Some("B1")));
        // Unknown boot: keep it.
        assert!(!stale(Some(&m), false, None));
        // An earlier boot, or unreadable: removed.
        assert!(stale(Some(&m), false, Some("B2")));
        assert!(stale(None, true, Some("B1")));
        assert!(!stale(None, false, Some("B1")));
    }

    #[test]
    fn markers_round_trip_and_corrupt_ones_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host").join("stopped");
        assert_eq!(read_marker(&path).unwrap(), None);
        let m = StopMarker { boot: "B".into(), asid: 3, at: 9, login_window: false };
        write_marker(&path, &m).unwrap();
        assert_eq!(read_marker(&path).unwrap(), Some(m));
        std::fs::write(&path, b"{not json").unwrap();
        assert!(read_marker(&path).is_err());
        clear_marker(&path).unwrap();
        clear_marker(&path).unwrap();
        assert_eq!(read_marker(&path).unwrap(), None);
    }
}
