//! Running a planned sequence of input events so that it fails safe.
//!
//! The tools first turn a request into a [`Plan`] (pure, so it is unit-tested), then [`execute`]
//! it. A check runs right before every event that could change something (a key or button press, a
//! click, a scroll, text): the session is alive, no approval prompt is open, the desktop is usable,
//! the target window is acceptable. Conditions change while a long sequence runs (a prompt opens,
//! the screen locks, focus moves to another app), so one check up front is not enough. Whatever
//! happens, every key and button the plan pressed is released before [`execute`] returns: a stuck
//! Ctrl or mouse button would wreck the owner's next minutes at the machine (F15).

use std::time::Duration;

use anyhow::{Result, anyhow};
use enigo::{Axis, Button, Direction, Key};

use crate::platform::InputTarget;

/// One input operation.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Sends nothing; only the step's check runs (for example on the far end of a drag before the
    /// button goes down).
    Verify,
    /// Puts the pointer on a screen pixel.
    MoveTo(i32, i32),
    Pause(Duration),
    PressKey(Key),
    ReleaseKey(Key),
    TapKey(Key),
    PressButton(Button),
    ReleaseButton(Button),
    Click(Button),
    /// Wheel notches; positive is down (vertical axis) or right (horizontal axis).
    Scroll(i32, Axis),
    /// Literal text without control characters.
    Text(String),
    /// Ctrl+Alt+Del, sent by the Windows service (the named event to signal); pressing the keys
    /// would be ignored by Windows.
    SecureAttention(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub op: Op,
    /// Checked right before the op runs; `None` for ops that cannot act on a window by themselves
    /// (moves, pauses, releases).
    pub check: Option<InputTarget>,
    /// Units of work complete once this step has run.
    pub done: usize,
}

/// Ordered steps plus what to call a unit of work when reporting progress ("stopped after 3 of 10
/// chords").
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub steps: Vec<Step>,
    pub unit: &'static str,
    pub total: usize,
}

impl Plan {
    pub fn new(unit: &'static str) -> Self {
        Self { steps: Vec::new(), unit, total: 0 }
    }

    /// Adds an op that runs only if the check on `target` passes.
    pub fn checked(&mut self, op: Op, target: InputTarget) {
        self.steps.push(Step { op, check: Some(target), done: self.total });
    }

    /// Adds an op that runs without a check.
    pub fn plain(&mut self, op: Op) {
        self.steps.push(Step { op, check: None, done: self.total });
    }

    /// Counts `n` more units as complete once the last step so far has run.
    pub fn complete(&mut self, n: usize) {
        self.total += n;
        if let Some(step) = self.steps.last_mut() {
            step.done = self.total;
        }
    }
}

/// What actually sends input. The real one is [`EnigoBackend`]; tests record calls instead.
pub trait Backend {
    fn key(&mut self, key: Key, direction: Direction) -> Result<()>;
    fn button(&mut self, button: Button, direction: Direction) -> Result<()>;
    fn move_to(&mut self, x: i32, y: i32) -> Result<()>;
    fn scroll(&mut self, notches: i32, axis: Axis) -> Result<()>;
    fn text(&mut self, text: &str) -> Result<()>;
    fn pause(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
    /// Asks the service to send Ctrl+Alt+Del (see [`Op::SecureAttention`]).
    fn secure_attention(&mut self, event: &str) -> Result<()> {
        let _ = event;
        Err(anyhow!("Ctrl+Alt+Del cannot be sent on this platform"))
    }
}

/// Runs `plan` on `backend`, calling `active` before every step and `check` before target-sensitive steps. Stops at the first failure
/// with an error that says how far it got; keys and buttons still held are released either way.
pub fn execute<B: Backend>(
    plan: &Plan,
    backend: &mut B,
    check: &mut dyn FnMut(InputTarget) -> Result<()>,
    active: &mut dyn FnMut() -> Result<()>,
) -> Result<()> {
    let mut held = Held { backend, keys: Vec::new(), buttons: Vec::new() };
    let mut done = 0;
    for step in &plan.steps {
        let result = active()
            .and_then(|()| match step.check {
                Some(target) => check(target),
                None => Ok(()),
            })
            .and_then(|()| held.apply(&step.op));
        if let Err(e) = result {
            let released =
                if held.is_empty() { "" } else { " The keys and buttons it was holding have been released." };
            return Err(anyhow!("stopped after {done} of {} {}: {e:#}.{released}", plan.total, plan.unit));
        }
        done = step.done;
    }
    Ok(())
}

/// Tracks what is pressed and releases it on drop, whether the plan finished, failed or panicked.
struct Held<'a, B: Backend> {
    backend: &'a mut B,
    keys: Vec<Key>,
    buttons: Vec<Button>,
}

impl<B: Backend> Held<'_, B> {
    fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }

    fn apply(&mut self, op: &Op) -> Result<()> {
        match op {
            Op::Verify => Ok(()),
            Op::MoveTo(x, y) => self.backend.move_to(*x, *y),
            Op::Pause(d) => {
                self.backend.pause(*d);
                Ok(())
            }
            // Recorded before sending, so a press that failed halfway is still released.
            Op::PressKey(k) => {
                self.keys.push(*k);
                self.backend.key(*k, Direction::Press)
            }
            Op::ReleaseKey(k) => {
                self.backend.key(*k, Direction::Release)?;
                forget(&mut self.keys, k);
                Ok(())
            }
            Op::TapKey(k) => self.backend.key(*k, Direction::Click),
            Op::PressButton(b) => {
                self.buttons.push(*b);
                self.backend.button(*b, Direction::Press)
            }
            Op::ReleaseButton(b) => {
                self.backend.button(*b, Direction::Release)?;
                forget(&mut self.buttons, b);
                Ok(())
            }
            Op::Click(b) => self.backend.button(*b, Direction::Click),
            Op::Scroll(n, axis) => self.backend.scroll(*n, *axis),
            Op::Text(t) => self.backend.text(t),
            Op::SecureAttention(event) => self.backend.secure_attention(event),
        }
    }
}

impl<B: Backend> Drop for Held<'_, B> {
    fn drop(&mut self) {
        // Buttons first, then keys, each in reverse press order: the way a person lets go.
        for b in self.buttons.drain(..).rev() {
            if let Err(e) = self.backend.button(b, Direction::Release) {
                tracing::warn!(?b, %e, "releasing a mouse button failed");
            }
        }
        for k in self.keys.drain(..).rev() {
            if let Err(e) = self.backend.key(k, Direction::Release) {
                tracing::warn!(?k, %e, "releasing a key failed");
            }
        }
    }
}

fn forget<T: PartialEq>(held: &mut Vec<T>, item: &T) {
    if let Some(i) = held.iter().rposition(|h| h == item) {
        held.remove(i);
    }
}

/// Input through enigo, except pointer moves on Windows (see [`crate::platform::move_pointer`]).
pub struct EnigoBackend {
    enigo: enigo::Enigo,
}

impl EnigoBackend {
    pub fn new() -> Result<Self> {
        // Attach this thread to the desktop that now receives input, so `SendInput` and pointer
        // moves below reach it rather than a stale desktop left over from a switch. Best-effort:
        // see `platform::follow_input_desktop`.
        #[cfg(windows)]
        crate::platform::follow_input_desktop();
        #[cfg(target_os = "macos")]
        if !crate::platform::macos::accessibility_allowed() {
            anyhow::bail!(
                "cannot inject input: macOS Accessibility permission is missing for this host. Enable it in System Settings > Privacy & Security > Accessibility, restart the host, then run doctor"
            );
        }
        // Defaults also release keys enigo saw pressed when it is dropped: a second safety net.
        let settings = enigo::Settings::default();
        #[cfg(target_os = "macos")]
        let settings = enigo::Settings { open_prompt_to_get_permissions: false, ..settings };
        let enigo = enigo::Enigo::new(&settings).map_err(|e| {
            let hint = if cfg!(target_os = "macos") {
                " Grant ai-remote Accessibility access (System Settings > Privacy & Security > Accessibility)."
            } else {
                ""
            };
            anyhow!("cannot start input injection: {e}.{hint}")
        })?;
        Ok(Self { enigo })
    }
}

fn send_failed(e: enigo::InputError) -> anyhow::Error {
    anyhow!("sending input failed ({e}); the desktop may have locked, or the target window belongs to an elevated app")
}

impl Backend for EnigoBackend {
    fn key(&mut self, key: Key, direction: Direction) -> Result<()> {
        enigo::Keyboard::key(&mut self.enigo, key, direction).map_err(send_failed)
    }

    fn button(&mut self, button: Button, direction: Direction) -> Result<()> {
        enigo::Mouse::button(&mut self.enigo, button, direction).map_err(send_failed)
    }

    fn move_to(&mut self, x: i32, y: i32) -> Result<()> {
        #[cfg(windows)]
        {
            crate::platform::move_pointer(x, y)
        }
        #[cfg(not(windows))]
        {
            enigo::Mouse::move_mouse(&mut self.enigo, x, y, enigo::Coordinate::Abs).map_err(send_failed)
        }
    }

    fn scroll(&mut self, notches: i32, axis: Axis) -> Result<()> {
        enigo::Mouse::scroll(&mut self.enigo, notches, axis).map_err(send_failed)
    }

    fn text(&mut self, text: &str) -> Result<()> {
        enigo::Keyboard::text(&mut self.enigo, text).map_err(send_failed)
    }

    #[cfg(windows)]
    fn secure_attention(&mut self, event: &str) -> Result<()> {
        crate::service::request_secure_attention(event)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Records calls; fails the call with index `fail_at` (counting every backend call).
    #[derive(Default)]
    pub struct Recorder {
        pub calls: Vec<String>,
        pub fail_at: Option<usize>,
    }

    impl Recorder {
        fn record(&mut self, call: String) -> Result<()> {
            let index = self.calls.len();
            self.calls.push(call);
            if self.fail_at == Some(index) { Err(anyhow!("injected failure")) } else { Ok(()) }
        }
    }

    impl Backend for Recorder {
        fn key(&mut self, key: Key, direction: Direction) -> Result<()> {
            self.record(format!("key {key:?} {direction:?}"))
        }
        fn button(&mut self, button: Button, direction: Direction) -> Result<()> {
            self.record(format!("button {button:?} {direction:?}"))
        }
        fn move_to(&mut self, x: i32, y: i32) -> Result<()> {
            self.record(format!("move {x},{y}"))
        }
        fn scroll(&mut self, notches: i32, axis: Axis) -> Result<()> {
            self.record(format!("scroll {notches} {axis:?}"))
        }
        fn text(&mut self, text: &str) -> Result<()> {
            self.record(format!("text {text}"))
        }
        fn pause(&mut self, _duration: Duration) {}
        fn secure_attention(&mut self, event: &str) -> Result<()> {
            self.record(format!("sas {event}"))
        }
    }

    /// Runs `plan` with every check passing.
    pub fn run(plan: &Plan) -> (Result<()>, Vec<String>) {
        let mut rec = Recorder::default();
        let r = execute(plan, &mut rec, &mut |_| Ok(()), &mut || Ok(()));
        (r, rec.calls)
    }

    fn ctrl_drag() -> Plan {
        let mut plan = Plan::new("drags");
        plan.plain(Op::MoveTo(1, 1));
        plan.checked(Op::PressKey(Key::Control), InputTarget::At(1, 1));
        plan.checked(Op::PressButton(Button::Left), InputTarget::At(1, 1));
        plan.plain(Op::MoveTo(5, 5));
        plan.checked(Op::ReleaseButton(Button::Left), InputTarget::At(5, 5));
        plan.plain(Op::ReleaseKey(Key::Control));
        plan.complete(1);
        plan
    }

    #[test]
    fn runs_a_plan_in_order() {
        let (r, calls) = run(&ctrl_drag());
        r.unwrap();
        assert_eq!(
            calls,
            [
                "move 1,1",
                "key Control Press",
                "button Left Press",
                "move 5,5",
                "button Left Release",
                "key Control Release"
            ]
        );
    }

    #[test]
    fn releases_everything_held_when_a_step_fails() {
        let mut rec = Recorder { fail_at: Some(3), ..Default::default() };
        let err = execute(&ctrl_drag(), &mut rec, &mut |_| Ok(()), &mut || Ok(())).unwrap_err().to_string();
        assert!(err.contains("stopped after 0 of 1 drags: injected failure"), "{err}");
        assert!(err.contains("have been released"), "{err}");
        // The failing move, then the button and the key, in that order.
        assert_eq!(&rec.calls[3..], ["move 5,5", "button Left Release", "key Control Release"]);
    }

    #[test]
    fn releases_a_press_that_itself_failed() {
        let mut rec = Recorder { fail_at: Some(1), ..Default::default() };
        execute(&ctrl_drag(), &mut rec, &mut |_| Ok(()), &mut || Ok(())).unwrap_err();
        assert_eq!(rec.calls, ["move 1,1", "key Control Press", "key Control Release"]);
    }

    #[test]
    fn a_failed_check_stops_before_its_step_and_reports_progress() {
        let mut plan = Plan::new("chords");
        for _ in 0..3 {
            plan.checked(Op::PressKey(Key::Shift), InputTarget::Focused);
            plan.checked(Op::TapKey(Key::Tab), InputTarget::Focused);
            plan.plain(Op::ReleaseKey(Key::Shift));
            plan.complete(1);
        }
        let mut rec = Recorder::default();
        let mut checks = 0;
        let err = execute(
            &plan,
            &mut rec,
            &mut |_| {
                checks += 1;
                if checks == 4 { Err(anyhow!("a prompt opened")) } else { Ok(()) }
            },
            &mut || Ok(()),
        )
        .unwrap_err()
        .to_string();
        assert!(err.starts_with("stopped after 1 of 3 chords: a prompt opened"), "{err}");
        // Chord 2 pressed shift, then its tap was refused: shift is released, nothing else is sent.
        assert_eq!(rec.calls.len(), 5);
        assert_eq!(rec.calls[3..], ["key Shift Press", "key Shift Release"]);
    }

    #[test]
    fn cancellation_stops_unchecked_drag_moves_and_releases_held_input() {
        let mut rec = Recorder::default();
        let mut steps = 0;
        let err = execute(&ctrl_drag(), &mut rec, &mut |_| Ok(()), &mut || {
            steps += 1;
            if steps == 4 { Err(anyhow!("control changed")) } else { Ok(()) }
        })
        .unwrap_err()
        .to_string();
        assert!(err.contains("control changed"), "{err}");
        assert_eq!(
            rec.calls,
            ["move 1,1", "key Control Press", "button Left Press", "button Left Release", "key Control Release"]
        );
    }

    #[test]
    fn cancelled_queued_input_sends_nothing_even_for_a_plain_move() {
        let mut plan = Plan::new("moves");
        plan.plain(Op::MoveTo(10, 20));
        let mut rec = Recorder::default();
        execute(&plan, &mut rec, &mut |_| Ok(()), &mut || Err(anyhow!("cancelled"))).unwrap_err();
        assert!(rec.calls.is_empty());
    }

    #[test]
    fn failed_releases_are_retried_by_cleanup() {
        for index in [4, 5] {
            let mut rec = Recorder { fail_at: Some(index), ..Default::default() };
            execute(&ctrl_drag(), &mut rec, &mut |_| Ok(()), &mut || Ok(())).unwrap_err();
            let release = if index == 4 { "button Left Release" } else { "key Control Release" };
            assert_eq!(rec.calls.iter().filter(|call| *call == release).count(), 2);
        }
    }

    #[test]
    fn counts_units_without_steps() {
        let mut plan = Plan::new("characters");
        plan.complete(1);
        plan.checked(Op::Text("ab".into()), InputTarget::Focused);
        plan.complete(2);
        assert_eq!(plan.total, 3);
        assert_eq!(plan.steps[0].done, 3);
    }
}
