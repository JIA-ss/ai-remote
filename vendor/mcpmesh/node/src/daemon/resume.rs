//! Suspend/resume detection (#167 ask 2): the watcher that pushes a [`StreamFrame::Resumed`] when
//! this machine wakes from a sleep, so an embedder holding long-lived sessions can re-dial
//! deliberately instead of discovering the loss on its next send.
//!
//! While a machine is suspended it sends nothing, so the PEER's idle timer runs out and tears the
//! connection down before the lid is even reopened. `[network].keep_alive_secs` cannot help — a
//! suspended process emits no PINGs — and `idle_timeout_secs` is negotiated to the minimum of both
//! peers, so surviving a lid close would need every peer in the mesh raised in lockstep to a value
//! covering the longest expected sleep. #167 says that plainly: this needs reconnection semantics,
//! not transport tuning. The frame is the signal half of those semantics.
//!
//! [`StreamFrame::Resumed`]: mcpmesh_local_api::StreamFrame::Resumed

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::util::epoch_now_i64;

use super::MeshState;

/// How often the watcher compares its two clocks.
///
/// This is now the daemon's most frequent periodic timer, on a feature whose entire audience is
/// battery-powered laptops — so the cost that matters is not the tick's work (two clock reads and a
/// subtraction) but the WAKEUP, which defeats deep idle and OS timer coalescing. Five seconds keeps
/// that rare while still landing the frame far inside the 30s default idle timeout.
///
/// The frame arrives up to one full TICK after the machine is back, not immediately: tokio's timer
/// deadline is monotonic, so a sleep outstanding across a suspend has its remaining time frozen and
/// still has to elapse once the machine wakes.
const TICK: Duration = Duration::from_secs(5);

/// How far the wall clock must outrun the monotonic clock in ONE tick before we call it a suspend.
///
/// Above [`TICK`] by enough that scheduling jitter, a slow fsync or a busy runtime cannot reach it,
/// and below the 30s default QUIC idle timeout, so the signal arrives for the suspends that
/// actually kill connections. A three-second lid close does not emit — and nothing was lost either.
pub(crate) const RESUME_THRESHOLD_SECS: i64 = 10;

/// The loop's clock arithmetic, split out so the PAIRING of the two deltas is pinned by a test.
///
/// The struct below makes transposing them a deliberate mislabel rather than an invisible argument
/// swap, which is most of the protection — but "deliberate" is not "caught", and the loop that
/// builds it is the one part of this module no test drives. Reducing that gap to the two `now()`
/// reads (which cannot be swapped: they are different types) is the difference between a rule and
/// an enforced rule.
///
/// Both arguments are `(monotonic, wall)` pairs, in the same order as the fields they feed.
pub(crate) fn deltas(last: (Instant, i64), now: (Instant, i64)) -> ClockDeltas {
    ClockDeltas {
        // Truncating to whole seconds costs at most 1s against a 10s threshold.
        mono_secs: now.0.duration_since(last.0).as_secs() as i64,
        wall_secs: now.1.saturating_sub(last.1),
    }
}

/// One tick's two clock deltas.
///
/// A struct rather than two `i64` parameters because the whole rule is a SUBTRACTION of one from
/// the other, so transposing them at the call site inverts it — and the only production call site
/// is a loop no test can drive. This gate proved that exactly: with the arguments swapped, the
/// feature is 100% dead in production (the skew goes strongly negative on a real suspend, so
/// nothing ever emits) and the entire workspace stays green. Named fields make the wrong pairing
/// something you have to write out deliberately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockDeltas {
    /// How much MONOTONIC time this process experienced. Frozen across a suspend.
    pub mono_secs: i64,
    /// How much WALL-CLOCK time passed. Runs across a suspend.
    pub wall_secs: i64,
}

/// The whole detection rule, PURE — given one tick's clock deltas, how long the machine was away,
/// or `None` if this was an ordinary tick.
///
/// `Instant` is `CLOCK_MONOTONIC` on Linux and `CLOCK_UPTIME_RAW` on Apple targets, and **neither
/// advances while the machine is suspended**; the wall clock does. So the three cases separate
/// cleanly:
///
/// | | monotonic Δ | wall Δ | skew |
/// |---|---|---|---|
/// | ordinary tick | ≈ TICK | ≈ TICK | ≈ 0 |
/// | suspend/resume | ≈ TICK | ≫ TICK | **the sleep** |
/// | starved runtime | ≫ TICK | ≫ TICK | ≈ 0 |
///
/// Subtracting the monotonic delta rather than comparing the wall delta to [`TICK`] is what
/// distinguishes the last row from the middle one. A watcher that looked only at the wall clock
/// would emit a false resume every time the machine got busy, and a liveness signal that fires
/// under load is one consumers learn to ignore.
///
/// A BACKWARDS wall-clock step (an NTP correction) yields a negative skew and emits nothing: it is
/// not evidence of a suspend, and reporting `0` seconds away would be a frame asserting a wake that
/// did not happen. A FORWARD step is a different matter — it is indistinguishable from a suspend by
/// this method and DOES emit; see `a_forward_clock_step_is_reported_as_a_suspend_and_that_is_documented`
/// for why that is stated rather than papered over.
pub(crate) fn detect_suspend(d: ClockDeltas) -> Option<u64> {
    // Saturating: `resume_tick_for_test` is `pub` (only `#[doc(hidden)]`), so a caller can reach
    // this with `i64::MIN` and a plain `-` would panic the daemon in a debug build.
    let skew = d.wall_secs.saturating_sub(d.mono_secs);
    (skew >= RESUME_THRESHOLD_SECS).then_some(skew as u64)
}

/// What the watcher broadcasts. Carries the detection stamp rather than letting the subscriber
/// stamp it, so every subscriber reports the same `at_epoch` for one wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeEvent {
    pub suspended_secs: u64,
    pub at_epoch: i64,
}

/// ONE tick's decision and its effect — split out of the loop so it is reachable from a test.
///
/// The loop itself cannot be: staging a real suspend would mean suspending the machine running the
/// suite. What a test CAN drive is everything downstream of the two clock reads, which is where the
/// mistakes live — a detected suspend that is never broadcast, or one broadcast with the tick's
/// duration in place of the sleep's. Leaving that inside the loop would have meant a module whose
/// only test was [`detect_suspend`], and a `send` no test would miss if it were deleted.
pub(crate) fn tick(mesh: &MeshState, deltas: ClockDeltas, now_wall: i64) -> Option<ResumeEvent> {
    let suspended_secs = detect_suspend(deltas)?;
    let event = ResumeEvent {
        suspended_secs,
        at_epoch: now_wall,
    };
    // Best-effort, like `reach_bcast`: `send` errors only with no subscribers.
    let _ = mesh.resume_bcast.send(event);
    Some(event)
}

/// Spawn the suspend watcher: tick, compare both clocks, and broadcast a [`ResumeEvent`] on a skew
/// past [`RESUME_THRESHOLD_SECS`].
///
/// `pub` for symmetry with [`spawn_self_net_watch`](super::self_net::spawn_self_net_watch) and so
/// an embedder driving `boot_node`'s pieces itself can install it. It is NOT pub for a test: the
/// loop reads two real clocks and no test drives it — [`tick`] is the seam, reached through
/// [`MeshState::resume_tick_for_test`].
pub fn spawn_resume_watch(mesh: Arc<MeshState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_mono = Instant::now();
        let mut last_wall = epoch_now_i64();
        loop {
            tokio::time::sleep(TICK).await;
            let (now_mono, now_wall) = (Instant::now(), epoch_now_i64());
            tick(
                &mesh,
                deltas((last_mono, last_wall), (now_mono, now_wall)),
                now_wall,
            );
            (last_mono, last_wall) = (now_mono, now_wall);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(monotonic, wall)` — the order the struct's fields are declared in, so a transposed test is
    /// a transposed literal rather than a silently different meaning.
    fn d(mono_secs: i64, wall_secs: i64) -> ClockDeltas {
        ClockDeltas {
            mono_secs,
            wall_secs,
        }
    }

    /// An ordinary tick — both clocks advanced by the period — is not a resume. The case that
    /// matters most: this runs every 2 seconds forever, and a false positive here is an infinite
    /// stream of frames telling an embedder to re-dial its whole mesh.
    #[test]
    fn an_ordinary_tick_is_not_a_resume() {
        assert_eq!(detect_suspend(d(2, 2)), None);
        // Jitter in either direction, still not a resume.
        assert_eq!(detect_suspend(d(2, 3)), None);
        assert_eq!(detect_suspend(d(3, 2)), None);
    }

    /// The case the frame exists for: the monotonic clock froze across the sleep while the wall
    /// clock ran, and the reported number is the SLEEP — not the time since the last tick.
    #[test]
    fn a_frozen_monotonic_clock_reports_the_sleep_not_the_tick() {
        // A two-hour lid close: the tick's own 2s still elapsed monotonically.
        assert_eq!(detect_suspend(d(2, 7202)), Some(7200));
    }

    /// A starved runtime — a loaded machine, a blocked executor — advances BOTH clocks together.
    /// This is the mutation guard on subtracting the monotonic delta: a watcher comparing the wall
    /// delta against the tick period alone would call every one of these a suspend.
    #[test]
    fn a_starved_runtime_is_not_a_resume() {
        // The tick was 5 minutes late, but the machine was awake for all of it.
        assert_eq!(detect_suspend(d(300, 300)), None);
        assert_eq!(detect_suspend(d(300, 302)), None);
        // Even an extreme stall stays silent while the two clocks agree.
        assert_eq!(detect_suspend(d(86_400, 86_400)), None);
    }

    /// The threshold, pinned from BOTH sides — a boundary asserted from one side passes whether the
    /// comparison is `>=`, `>`, or off by a second.
    #[test]
    fn the_threshold_is_pinned_from_both_sides() {
        assert_eq!(detect_suspend(d(2, 2 + RESUME_THRESHOLD_SECS - 1)), None);
        assert_eq!(
            detect_suspend(d(2, 2 + RESUME_THRESHOLD_SECS)),
            Some(RESUME_THRESHOLD_SECS as u64)
        );
    }

    /// The loop's own arithmetic assigns each clock to the field that means it. This is the
    /// mutation the gate landed: with the two swapped, the skew becomes `mono - wall`, which is ≈0
    /// on an ordinary tick and strongly NEGATIVE on a real suspend — so the feature is 100% dead in
    /// production while every other test in the workspace stays green.
    ///
    /// The two deltas are deliberately DIFFERENT here (3 vs 100): equal values would pass with the
    /// fields swapped, which is how a call-site test ends up proving nothing.
    #[test]
    fn the_loop_assigns_each_clock_to_the_field_that_means_it() {
        let last_mono = Instant::now();
        let now_mono = last_mono + Duration::from_secs(3);
        let got = deltas((last_mono, 1_700_000_000), (now_mono, 1_700_000_100));
        assert_eq!(got.mono_secs, 3, "monotonic advanced 3s: {got:?}");
        assert_eq!(got.wall_secs, 100, "the wall clock advanced 100s: {got:?}");
        // And end to end through the rule: 97s of skew is a suspend.
        assert_eq!(detect_suspend(got), Some(97));
    }

    /// A forward wall-clock STEP is indistinguishable from a suspend by this method, and DOES
    /// emit. Pinned as a test because it is a real false positive on real hardware, not a
    /// hypothetical: a board with no RTC (this project deploys to a Jetson) boots with a bogus
    /// epoch and `systemd-timesyncd` steps the clock forward by months on first sync. Every
    /// subscriber then gets a frame claiming a multi-year sleep.
    ///
    /// It is not "fixed" here because the fix is a different clock, not a bigger threshold: a
    /// continuous clock that DOES advance across suspend (`CLOCK_BOOTTIME` on Linux,
    /// `mach_continuous_time` on Apple) would measure the sleep directly and be immune to wall-clock
    /// steps entirely. That is a per-platform `libc` dependency and its own change. Until then the
    /// docs say so rather than letting an embedder discover it.
    ///
    /// The advice the frame carries — "re-check what you hold" — happens to stay sound: after a
    /// months-long clock step, anything held really is stale.
    #[test]
    fn a_forward_clock_step_is_reported_as_a_suspend_and_that_is_documented() {
        // ~6 months of NTP correction on a first boot, with the tick itself taking its 5s.
        assert_eq!(detect_suspend(d(5, 15_552_005)), Some(15_552_000));
    }

    /// The subtraction saturates. `resume_tick_for_test` is `pub` (merely `#[doc(hidden)]`), so
    /// these values are reachable from outside the crate, and a plain `-` panics in a debug build.
    #[test]
    fn pathological_deltas_do_not_panic() {
        assert_eq!(detect_suspend(d(i64::MIN, i64::MAX)), Some(i64::MAX as u64));
        assert_eq!(detect_suspend(d(i64::MAX, i64::MIN)), None);
    }

    /// A wall clock stepped BACKWARDS (an NTP correction) is not a wake. Emitting here would be a
    /// frame asserting a suspend that never happened — and `skew as u64` on a negative value would
    /// report one about 18 quintillion seconds long.
    #[test]
    fn a_backwards_clock_step_is_not_a_resume() {
        assert_eq!(detect_suspend(d(2, -3600)), None);
    }
}
