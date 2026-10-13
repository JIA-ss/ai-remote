//! Restart pacing shared by the processes that keep a host running: the background supervisor
//! (`ai-remote-hostw`) and the Windows service (see [`crate::service`]).

use std::time::Duration;

/// How long to wait before restarting a host that exited abnormally: 5 s, doubling up to five
/// minutes, so a host that fails at once (a broken config) does not spin. One that ran for ten
/// minutes or more counts as healthy and starts the sequence over.
#[derive(Debug, Clone)]
pub struct Backoff {
    next: Duration,
}

impl Backoff {
    pub const FIRST: Duration = Duration::from_secs(5);
    pub const MAX: Duration = Duration::from_secs(300);
    pub const HEALTHY: Duration = Duration::from_secs(600);

    /// The delay before the next start, given how long the last run lasted.
    pub fn after(&mut self, ran: Duration) -> Duration {
        if ran >= Self::HEALTHY {
            self.next = Self::FIRST;
        }
        let delay = self.next;
        self.next = (self.next * 2).min(Self::MAX);
        delay
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self { next: Self::FIRST }
    }
}

/// Exit code of a host that found another one running for its profile: nothing to restart.
pub const EXIT_ALREADY_RUNNING: u8 = 3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarts_back_off_and_recover() {
        let mut b = Backoff::default();
        let quick = Duration::from_secs(1);
        let delays: Vec<u64> = (0..8).map(|_| b.after(quick).as_secs()).collect();
        assert_eq!(delays, vec![5, 10, 20, 40, 80, 160, 300, 300]);
        // A run that lasted counts as healthy: back to the first delay.
        assert_eq!(b.after(Duration::from_secs(3600)).as_secs(), 5);
        assert_eq!(b.after(quick).as_secs(), 10);
    }
}
