//! `mouse` and `keyboard`: pixel-level input, the fallback for what the accessibility tree cannot do.
//!
//! Coordinates are pixels of a screenshot the caller received; the session remembers each
//! screenshot's monitor and scale, so a model never does DPI or multi-monitor math. A call is
//! validated completely before its first event is sent (key names, refused chords, points outside
//! the screenshot or on no monitor, length limits), then run by [`inject`] with a check before every
//! event and a guarantee that nothing stays pressed. Calls run one at a time, so two calls cannot
//! interleave their events (a ctrl held by one would turn another's click into ctrl+click).

mod inject;
mod keys;

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::Level;
use async_trait::async_trait;
use enigo::{Axis, Button, Key};
use rmcp::model::{CallToolResult, JsonObject};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, MutexGuard, Semaphore, SemaphorePermit};

use self::{
    inject::{Op, Plan},
    keys::{Modifier, Os, Pressed},
};
use super::{Tool, ToolSpec, json, parse};
use crate::{
    approval::OpenPrompts,
    platform::{self, InputTarget, InputTargetCheck},
    session::{ShotGeometry, ToolCtx},
};

pub fn tools() -> Vec<Arc<dyn Tool>> {
    vec![Arc::new(Mouse::new()), Arc::new(Keyboard::new())]
}

/// Most characters one `keyboard` call types; longer text should be split (or pasted).
const MAX_TEXT_CHARS: usize = 10_000;
/// Most times one `keyboard` call repeats its keys.
const MAX_REPEAT: u32 = 100;
/// Most chords one `keyboard` call presses in total (keys x repeat).
const MAX_KEY_PRESSES: usize = 1_000;
/// Most wheel notches one `mouse` scroll sends.
const MAX_SCROLL: u32 = 50;

/// Characters sent in one go; checks run between runs, and the pause lets slow apps keep up.
const TEXT_RUN: usize = 50;
const TEXT_PAUSE: Duration = Duration::from_millis(10);
/// Between chords, so apps that react to a shortcut (a menu opening) are ready for the next one.
const CHORD_PAUSE: Duration = Duration::from_millis(10);
/// After moving onto a target and before acting, so hover-driven UI sees the pointer arrive.
const HOVER_PAUSE: Duration = Duration::from_millis(30);
/// Between scroll notches, so smooth-scrolling apps count each one.
const SCROLL_PAUSE: Duration = Duration::from_millis(15);
/// A drag moves in this many steps with this pause each, so apps notice the drag threshold.
const DRAG_STEPS: i32 = 10;
const DRAG_PAUSE: Duration = Duration::from_millis(15);

#[cfg(target_os = "macos")]
struct ManualBackend {
    peer: String,
    point: Option<(i32, i32)>,
    lifetime: tokio_util::sync::CancellationToken,
    backend: inject::EnigoBackend,
}

// Enigo on macOS keeps native multi-click state in its instance. Reuse it only
// for the same explicit human and driver epoch; every event remains checked.
#[cfg(target_os = "macos")]
static MANUAL_BACKEND: std::sync::Mutex<Option<ManualBackend>> = std::sync::Mutex::new(None);

/// Serialises input calls (see the module comment).
static INPUT: Mutex<()> = Mutex::const_new(());
/// At most one executing call and one waiter. Waiting must not occupy blocking
/// threads needed by screenshots, native prompts and unrelated host work.
static INPUT_CAPACITY: Semaphore = Semaphore::const_new(2);

const PROMPT_OPEN: &str = "an ai-remote approval prompt is open on the host, so mouse and keyboard input is \
     paused until the owner answers it (input must never be able to answer a prompt). Retry in a little while.";

// ---------------------------------------------------------------------------------------------
// mouse

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum MouseAction {
    Move,
    Click,
    DoubleClick,
    TripleClick,
    RightClick,
    MiddleClick,
    Drag,
    Scroll,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Space {
    /// Pixels of a screenshot image, origin top-left.
    #[default]
    Screenshot,
    /// Screen coordinates (physical pixels on Windows, points on macOS), as in `system_info`'s monitors.
    Screen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ScrollDirection {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MouseParams {
    /// move, click, double_click, triple_click, right_click, middle_click, drag or scroll.
    action: MouseAction,
    /// Target x (for drag: where it starts). Omit x and y to click or scroll where the pointer is.
    #[serde(default)]
    x: Option<f64>,
    /// Target y (for drag: where it starts).
    #[serde(default)]
    y: Option<f64>,
    /// Where a drag ends (x), in the same space as x/y.
    #[serde(default)]
    to_x: Option<f64>,
    /// Where a drag ends (y).
    #[serde(default)]
    to_y: Option<f64>,
    /// "screenshot" (default): pixels of a screenshot you received. "screen": screen coordinates as
    /// listed in system_info's monitors.
    #[serde(default)]
    space: Space,
    /// The screenshot the coordinates refer to (its id from the screenshot tool). Default: the
    /// latest screenshot of this session.
    #[serde(default)]
    screenshot_id: Option<String>,
    /// For scroll: up, down, left or right.
    #[serde(default)]
    direction: Option<ScrollDirection>,
    /// For scroll: wheel notches, 1-50 (default 3).
    #[serde(default)]
    amount: Option<u32>,
    /// Keys held during a click, drag or scroll: ctrl, shift, alt, win (cmd on macOS).
    #[serde(default)]
    modifiers: Vec<String>,
    /// Why you are doing this; shown to the host owner in approval prompts and the audit log.
    #[serde(default)]
    #[allow(dead_code)] // read by the gate from the raw arguments
    intent: Option<String>,
}

/// A mouse request whose shape (which fields go with which action) has been checked.
#[derive(Debug, Clone, PartialEq)]
struct MouseRequest {
    action: MouseAction,
    /// The target point, as given (screenshot or screen space).
    at: Option<(f64, f64)>,
    /// The end of a drag, as given.
    to: Option<(f64, f64)>,
    /// Signed notches (negative = up / left) and axis.
    scroll: Option<(i32, Axis)>,
    modifiers: Vec<Modifier>,
}

fn pair(a: Option<f64>, b: Option<f64>, names: &str) -> Result<Option<(f64, f64)>> {
    match (a, b) {
        (Some(a), Some(b)) => Ok(Some((a, b))),
        (None, None) => Ok(None),
        _ => bail!("give both {names} or neither"),
    }
}

impl MouseRequest {
    fn from_params(p: &MouseParams) -> Result<Self> {
        use MouseAction as A;
        let at = pair(p.x, p.y, "x and y")?;
        let to = pair(p.to_x, p.to_y, "to_x and to_y")?;
        let modifiers = keys::parse_modifiers(&p.modifiers)?;
        match p.action {
            A::Move if at.is_none() => bail!("move needs x and y"),
            A::Drag if at.is_none() || to.is_none() => {
                bail!("drag needs x and y (where it starts) and to_x and to_y (where it ends)")
            }
            A::Scroll if p.direction.is_none() => bail!("scroll needs a direction: up, down, left or right"),
            _ => {}
        }
        if p.action != A::Drag && to.is_some() {
            bail!("to_x and to_y only apply to drag");
        }
        if p.action != A::Scroll && (p.direction.is_some() || p.amount.is_some()) {
            bail!("direction and amount only apply to scroll");
        }
        if p.action == A::Move && !modifiers.is_empty() {
            bail!("modifiers apply to clicks, drags and scrolls, not to move");
        }
        if p.space == Space::Screen && p.screenshot_id.is_some() {
            bail!("screenshot_id only applies to space \"screenshot\"");
        }
        let scroll = match p.direction {
            None => None,
            Some(direction) => {
                let amount = p.amount.unwrap_or(3);
                if !(1..=MAX_SCROLL).contains(&amount) {
                    bail!("amount must be 1-{MAX_SCROLL} wheel notches");
                }
                let n = amount as i32;
                Some(match direction {
                    ScrollDirection::Up => (-n, Axis::Vertical),
                    ScrollDirection::Down => (n, Axis::Vertical),
                    ScrollDirection::Left => (-n, Axis::Horizontal),
                    ScrollDirection::Right => (n, Axis::Horizontal),
                })
            }
        };
        Ok(Self { action: p.action, at, to, scroll, modifiers })
    }
}

/// A monitor's rectangle in screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl Rect {
    fn contains(&self, (x, y): (i32, i32)) -> bool {
        let (x, y) = (i64::from(x), i64::from(y));
        let (left, top) = (i64::from(self.x), i64::from(self.y));
        x >= left && y >= top && x < left + i64::from(self.width) && y < top + i64::from(self.height)
    }
}

fn monitors() -> Result<Vec<Rect>> {
    let all = xcap::Monitor::all().map_err(|e| anyhow!("cannot list the monitors: {e}"))?;
    all.iter()
        .map(|m| Ok(Rect { x: m.x()?, y: m.y()?, width: m.width()?, height: m.height()? }))
        .collect::<Result<Vec<_>, xcap::XCapError>>()
        .map_err(|e| anyhow!("cannot read the monitor layout: {e}"))
}

fn monitor_list(monitors: &[Rect]) -> String {
    monitors.iter().map(|m| format!("({}, {}) {}x{}", m.x, m.y, m.width, m.height)).collect::<Vec<_>>().join(", ")
}

/// Fails when the monitor a screenshot was taken of is no longer where it was: its pixels would
/// map to the wrong place.
fn check_shot_current(id: &str, g: &ShotGeometry, monitors: &[Rect]) -> Result<()> {
    let shot = Rect { x: g.monitor_x, y: g.monitor_y, width: g.monitor_width, height: g.monitor_height };
    if monitors.contains(&shot) {
        Ok(())
    } else {
        bail!(
            "the display layout changed since screenshot {id} was taken (its monitor at ({}, {}) {}x{} is gone or \
             moved); take a new screenshot",
            shot.x,
            shot.y,
            shot.width,
            shot.height
        )
    }
}

fn check_on_monitor(p: (i32, i32), monitors: &[Rect]) -> Result<()> {
    if monitors.iter().any(|m| m.contains(p)) {
        Ok(())
    } else {
        bail!(
            "screen point ({}, {}) is on no monitor (monitors: {}). Use a point on one of them, or take a screenshot \
             and use its pixel coordinates.",
            p.0,
            p.1,
            monitor_list(monitors)
        )
    }
}

/// Maps a point given in screenshot pixels to the screen.
fn image_to_screen((x, y): (f64, f64), id: &str, g: &ShotGeometry) -> Result<(i32, i32)> {
    if !x.is_finite() || !y.is_finite() {
        bail!("coordinates must be finite numbers");
    }
    if x < 0.0 || y < 0.0 || x >= f64::from(g.image_width) || y >= f64::from(g.image_height) {
        bail!(
            "({x}, {y}) is outside screenshot {id}, which is {}x{} pixels; coordinates are pixels of that image, \
             origin top-left",
            g.image_width,
            g.image_height
        );
    }
    Ok(g.to_screen(x, y))
}

/// A point given in screen coordinates.
fn screen_point((x, y): (f64, f64)) -> Result<(i32, i32)> {
    let coord = |v: f64| {
        if v.is_finite() && v >= f64::from(i32::MIN) && v <= f64::from(i32::MAX) {
            Ok(v.floor() as i32)
        } else {
            Err(anyhow!("screen coordinate {v} is out of range"))
        }
    };
    Ok((coord(x)?, coord(y)?))
}

fn button_and_count(action: MouseAction) -> Option<(Button, usize)> {
    Some(match action {
        MouseAction::Click => (Button::Left, 1),
        MouseAction::DoubleClick => (Button::Left, 2),
        MouseAction::TripleClick => (Button::Left, 3),
        MouseAction::RightClick => (Button::Right, 1),
        MouseAction::MiddleClick => (Button::Middle, 1),
        _ => return None,
    })
}

/// Turns a validated request with screen points into input steps.
fn plan_mouse(req: &MouseRequest, at: Option<(i32, i32)>, to: Option<(i32, i32)>) -> Plan {
    let target = at.map_or(InputTarget::Pointer, |(x, y)| InputTarget::At(x, y));
    let approach = |plan: &mut Plan| {
        if let Some((x, y)) = at {
            plan.plain(Op::MoveTo(x, y));
            plan.plain(Op::Pause(HOVER_PAUSE));
        }
    };
    let press_modifiers = |plan: &mut Plan, target| {
        for m in &req.modifiers {
            plan.checked(Op::PressKey(m.key()), target);
        }
    };
    let release_modifiers = |plan: &mut Plan| {
        for m in req.modifiers.iter().rev() {
            plan.plain(Op::ReleaseKey(m.key()));
        }
    };

    if let Some((button, count)) = button_and_count(req.action) {
        let mut plan = Plan::new("clicks");
        approach(&mut plan);
        press_modifiers(&mut plan, target);
        for _ in 0..count {
            plan.checked(Op::Click(button), target);
            plan.complete(1);
        }
        release_modifiers(&mut plan);
        return plan;
    }
    match req.action {
        MouseAction::Move => {
            let mut plan = Plan::new("moves");
            approach(&mut plan);
            plan.complete(1);
            plan
        }
        MouseAction::Scroll => {
            let mut plan = Plan::new("notches");
            let (notches, axis) = req.scroll.unwrap_or((3, Axis::Vertical));
            approach(&mut plan);
            press_modifiers(&mut plan, target);
            for i in 0..notches.unsigned_abs() {
                if i > 0 {
                    plan.plain(Op::Pause(SCROLL_PAUSE));
                }
                plan.checked(Op::Scroll(notches.signum(), axis), target);
                plan.complete(1);
            }
            release_modifiers(&mut plan);
            plan
        }
        _ => {
            // Drag. Both ends are checked before the button goes down, and the end again before the
            // drop, in case a prompt opened on the way.
            let mut plan = Plan::new("drags");
            let (Some(from), Some(to)) = (at, to) else { return plan };
            let (start, end) = (InputTarget::At(from.0, from.1), InputTarget::At(to.0, to.1));
            plan.checked(Op::Verify, end);
            approach(&mut plan);
            press_modifiers(&mut plan, start);
            plan.checked(Op::PressButton(Button::Left), start);
            for i in 1..=DRAG_STEPS {
                let lerp = |a: i32, b: i32| {
                    (i64::from(a) + (i64::from(b) - i64::from(a)) * i64::from(i) / i64::from(DRAG_STEPS)) as i32
                };
                plan.plain(Op::Pause(DRAG_PAUSE));
                plan.plain(Op::MoveTo(lerp(from.0, to.0), lerp(from.1, to.1)));
            }
            plan.plain(Op::Pause(HOVER_PAUSE));
            plan.checked(Op::ReleaseButton(Button::Left), end);
            release_modifiers(&mut plan);
            plan.complete(1);
            plan
        }
    }
}

struct Mouse {
    spec: ToolSpec,
}

impl Mouse {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<MouseParams>(
                "mouse",
                Level::Control,
                "Move, click, drag or scroll the mouse on the host. x/y are pixels of your latest screenshot \
                 (origin top-left), so take a screenshot first; pass screenshot_id for an earlier one, or \
                 space=\"screen\" for screen coordinates. Returns once the input is sent; take a screenshot to see \
                 the effect.",
            ),
        }
    }
}

#[async_trait]
impl Tool for Mouse {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: MouseParams = parse(args)?;
        let req = MouseRequest::from_params(&p)?;
        ready(ctx)?;

        let shot = match (req.at, p.space) {
            (Some(_), Space::Screenshot) => Some(find_shot(ctx, p.screenshot_id.as_deref())?),
            _ => None,
        };
        let monitors = tokio::task::spawn_blocking(monitors).await.context("listing the monitors")??;
        let map = |point: Option<(f64, f64)>| -> Result<Option<(i32, i32)>> {
            let Some(point) = point else { return Ok(None) };
            let screen = match &shot {
                Some((id, g)) => image_to_screen(point, id, g)?,
                None => screen_point(point)?,
            };
            check_on_monitor(screen, &monitors)?;
            Ok(Some(screen))
        };
        if let Some((id, g)) = &shot {
            check_shot_current(id, g, &monitors)?;
        }
        let at = map(req.at)?;
        let to = map(req.to)?;

        let plan = plan_mouse(&req, at, to);
        let warnings = run(ctx, plan).await?;
        let point = |p: Option<(i32, i32)>| p.map(|(x, y)| json!({ "x": x, "y": y }));
        let mut out = json!({
            "ok": true,
            "action": p.action,
            "screen": point(at),
        });
        if let Some(to) = to {
            out["to_screen"] = json!({ "x": to.0, "y": to.1 });
        }
        if let Some((id, _)) = &shot {
            out["screenshot_id"] = json!(id);
        }
        if at.is_none() && req.action != MouseAction::Move {
            out["screen"] = json!("pointer position");
        }
        if !warnings.is_empty() {
            out["warnings"] = json!(warnings);
        }
        Ok(json(out))
    }
}

fn find_shot(ctx: &ToolCtx<'_>, id: Option<&str>) -> Result<(String, ShotGeometry)> {
    ctx.session.shot(id).ok_or_else(|| match id {
        Some(id) => anyhow!(
            "unknown screenshot id '{id}' in this session (the host keeps the latest 64); take a new screenshot"
        ),
        None => anyhow!(
            "no screenshot has been taken in this session yet. Take a screenshot first, then use pixel coordinates \
             of that image (or pass space=\"screen\" with screen coordinates)"
        ),
    })
}

// ---------------------------------------------------------------------------------------------
// keyboard

#[derive(Debug, Deserialize, JsonSchema)]
struct KeyboardParams {
    /// Literal text to type into the focused window, in any language. A newline presses Enter and a
    /// tab presses Tab. At most 10000 characters.
    #[serde(default)]
    text: Option<String>,
    /// Keys to press: a chord such as "ctrl+l", "ctrl+shift+t" or "cmd+space", or a space-separated
    /// sequence such as "Down Down Return". Key names: letters, digits, punctuation, enter, tab,
    /// esc, backspace, delete, home, end, pageup, pagedown, up, down, left, right, space, f1-f24, ...
    #[serde(default)]
    keys: Option<String>,
    /// How many times to press keys (1-100, default 1).
    #[serde(default)]
    repeat: Option<u32>,
    /// Why you are doing this; shown to the host owner in approval prompts and the audit log.
    #[serde(default)]
    #[allow(dead_code)] // read by the gate from the raw arguments
    intent: Option<String>,
}

/// Plans typing `text`: runs of characters as Unicode text (layout-independent), newlines and tabs
/// as the real keys (enigo would send both the key and the character).
fn plan_text(text: &str) -> Result<Plan> {
    let count = text.chars().count();
    if count == 0 {
        bail!("text is empty");
    }
    if count > MAX_TEXT_CHARS {
        bail!("text has {count} characters; at most {MAX_TEXT_CHARS} per call, so split it into several calls");
    }
    if let Some((i, c)) = text.chars().enumerate().find(|(_, c)| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
        bail!("text contains the control character U+{:04X} at position {i}; use keys for special keys", u32::from(c));
    }
    let mut plan = Plan::new("characters");
    let mut run = String::new();
    let mut run_len = 0;
    let flush = |plan: &mut Plan, run: &mut String, run_len: &mut usize| {
        if run.is_empty() {
            return;
        }
        if !plan.steps.is_empty() {
            plan.plain(Op::Pause(TEXT_PAUSE));
        }
        plan.checked(Op::Text(std::mem::take(run)), InputTarget::Focused);
        plan.complete(std::mem::take(run_len));
    };
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let key = match c {
            // "\r\n" is one line break.
            '\r' if chars.peek() == Some(&'\n') => {
                plan.complete(1);
                continue;
            }
            '\n' | '\r' => Key::Return,
            '\t' => Key::Tab,
            c => {
                run.push(c);
                run_len += 1;
                if run_len == TEXT_RUN {
                    flush(&mut plan, &mut run, &mut run_len);
                }
                continue;
            }
        };
        flush(&mut plan, &mut run, &mut run_len);
        plan.checked(Op::TapKey(key), InputTarget::Focused);
        plan.complete(1);
    }
    flush(&mut plan, &mut run, &mut run_len);
    Ok(plan)
}

/// Plans pressing resolved chords `repeat` times: modifiers down, key tapped, modifiers up.
fn plan_keys(chords: &[Pressed], repeat: usize) -> Plan {
    let mut plan = Plan::new("chords");
    for _ in 0..repeat {
        for chord in chords {
            if !plan.steps.is_empty() {
                plan.plain(Op::Pause(CHORD_PAUSE));
            }
            for m in &chord.modifiers {
                plan.checked(Op::PressKey(m.key()), InputTarget::Focused);
            }
            plan.checked(Op::TapKey(chord.key), InputTarget::Focused);
            for m in chord.modifiers.iter().rev() {
                plan.plain(Op::ReleaseKey(m.key()));
            }
            plan.complete(1);
        }
    }
    plan
}

/// Parses, resolves and vets a key spec; refused chords fail here, before anything is pressed.
fn plan_key_spec(spec: &str, repeat: u32) -> Result<Plan> {
    if !(1..=MAX_REPEAT).contains(&repeat) {
        bail!("repeat must be 1-{MAX_REPEAT}");
    }
    let chords = keys::parse_keys(spec)?;
    let presses = chords.len() * repeat as usize;
    if presses > MAX_KEY_PRESSES {
        bail!("keys x repeat would press {presses} chords; at most {MAX_KEY_PRESSES} per call");
    }
    let mut pressed = Vec::with_capacity(chords.len());
    for chord in &chords {
        let p = keys::resolve_chord(chord)?;
        if let Some(why) = keys::forbidden(&p, Os::CURRENT) {
            bail!(
                "'{chord}' is refused: {why}. That is the owner's decision; tell the user instead of working around it."
            );
        }
        pressed.push(p);
    }
    Ok(plan_keys(&pressed, repeat as usize))
}

struct Keyboard {
    spec: ToolSpec,
}

impl Keyboard {
    fn new() -> Self {
        Self {
            spec: ToolSpec::new::<KeyboardParams>(
                "keyboard",
                Level::Control,
                "Type text or press keys on the host; input goes to the focused window. Use text for literal text \
                 (any language) and keys for shortcuts and special keys (\"ctrl+l\", \"alt+tab\", \"Down Down \
                 Return\"). Chords that lock the machine or sign the user out (win+l, ctrl+alt+del) are refused. \
                 Returns once the input is sent; take a screenshot to see the effect.",
            ),
        }
    }
}

#[async_trait]
impl Tool for Keyboard {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    async fn call(&self, ctx: &ToolCtx<'_>, args: JsonObject) -> Result<CallToolResult> {
        let p: KeyboardParams = parse(args)?;
        let (plan, mut out) = match (&p.text, &p.keys) {
            (Some(_), Some(_)) => bail!("give either text or keys, not both (make two calls)"),
            (None, None) => bail!("give text to type, or keys to press (for example \"ctrl+l\")"),
            (Some(text), None) => {
                if p.repeat.is_some() {
                    bail!("repeat only applies to keys");
                }
                let plan = plan_text(text)?;
                let typed = plan.total;
                (plan, json!({ "ok": true, "typed_characters": typed }))
            }
            (None, Some(spec)) => {
                let plan = plan_key_spec(spec, p.repeat.unwrap_or(1))?;
                let chords = plan.total;
                (plan, json!({ "ok": true, "pressed": spec, "chords": chords }))
            }
        };
        ready(ctx)?;
        let warnings = run(ctx, plan).await?;
        if !warnings.is_empty() {
            out["warnings"] = json!(warnings);
        }
        Ok(json(out))
    }
}

// ---------------------------------------------------------------------------------------------
// shared

/// Refuses early when input cannot reach the desktop or a prompt is open.
fn ready(ctx: &ToolCtx<'_>) -> Result<()> {
    desktop_ready()?;
    #[cfg(target_os = "macos")]
    accessibility_ready()?;
    if ctx.host.open_prompts().count() > 0 {
        bail!(PROMPT_OPEN);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn accessibility_ready() -> Result<()> {
    if !crate::platform::macos::accessibility_allowed() {
        bail!(
            "input cannot reach the desktop: macOS Accessibility permission is missing; enable it for this host and restart it"
        );
    }
    Ok(())
}

fn desktop_ready() -> Result<()> {
    let state = platform::session_state();
    if state.desktop_usable {
        Ok(())
    } else {
        bail!(
            "input cannot reach the host's desktop: {}. Check system_info, and ask the owner to unlock or log in.",
            state.problem.as_deref().unwrap_or("the desktop is not usable")
        )
    }
}

/// The check before every event that could act on a window. Doubtful targets add a warning
/// (once) instead of failing.
fn check_step(
    cancel: &tokio_util::sync::CancellationToken,
    prompts: &OpenPrompts,
    target: InputTarget,
    warnings: &mut Vec<String>,
) -> Result<()> {
    if cancel.is_cancelled() {
        bail!("the session ended");
    }
    if prompts.count() > 0 {
        bail!(PROMPT_OPEN);
    }
    desktop_ready()?;
    #[cfg(target_os = "macos")]
    accessibility_ready()?;
    match platform::check_input_target(target) {
        InputTargetCheck::Allowed => Ok(()),
        InputTargetCheck::Doubtful(warning) => {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
            Ok(())
        }
        InputTargetCheck::Refused(reason) => Err(anyhow!(reason)),
    }
}

/// Runs a plan on a blocking thread, one input call at a time. Returns the warnings collected.
/// Keeps checking the session: the gate stops awaiting a cancelled call, but this thread would
/// otherwise keep typing.
async fn run(ctx: &ToolCtx<'_>, plan: Plan) -> Result<Vec<String>> {
    #[cfg(target_os = "macos")]
    let manual = ctx.session.person();
    #[cfg(target_os = "macos")]
    let cache_lifetime = ctx.host.turns().input_cache_lifetime();
    #[cfg(target_os = "macos")]
    let peer = ctx.session.peer.principal.clone().unwrap_or_else(|| ctx.session.peer.name.clone());
    #[cfg(target_os = "macos")]
    let point = plan.steps.iter().find_map(|step| match (&step.op, step.check) {
        (Op::Click(_), Some(InputTarget::At(x, y))) => Some((x, y)),
        _ => None,
    });
    let cancel = ctx.session.cancel.clone();
    let request = ctx.cancel.clone();
    let turn = ctx.turn_cancel.clone();
    let prompts = ctx.host.open_prompts();
    let (capacity, input) = input_slot().await?;
    tokio::task::spawn_blocking(move || {
        // These guards remain with the actual worker if its JoinHandle is dropped.
        let (_capacity, _one_at_a_time) = (capacity, input);
        check_active(&cancel, &request, &turn)?;
        #[cfg(target_os = "macos")]
        accessibility_ready()?;
        let execute = |backend: &mut inject::EnigoBackend| {
            let mut warnings = Vec::new();
            inject::execute(
                &plan,
                backend,
                &mut |target| check_step(&cancel, &prompts, target, &mut warnings),
                &mut || check_active(&cancel, &request, &turn),
            )?;
            Ok(warnings)
        };
        #[cfg(target_os = "macos")]
        if manual {
            let mut cache =
                MANUAL_BACKEND.lock().map_err(|_| anyhow!("manual input state failed; restart the host"))?;
            if cache_lifetime.is_cancelled() {
                // An explicitly human call keeps its admission behavior, but a
                // call queued before a driver change cannot reuse newer click state.
                *cache = None;
                return execute(&mut inject::EnigoBackend::new()?);
            }
            if cache.as_ref().is_none_or(|previous| {
                previous.peer != peer || previous.lifetime.is_cancelled() || point.is_none() || previous.point != point
            }) {
                *cache = Some(ManualBackend {
                    peer,
                    point,
                    lifetime: cache_lifetime,
                    backend: inject::EnigoBackend::new()?,
                });
            }
            let result = execute(&mut cache.as_mut().unwrap().backend);
            if result.is_err() {
                *cache = None;
            }
            return result;
        }
        execute(&mut inject::EnigoBackend::new()?)
    })
    .await
    .context("the input thread failed")?
}

pub(crate) async fn input_slot() -> Result<(SemaphorePermit<'static>, MutexGuard<'static, ()>)> {
    let capacity = INPUT_CAPACITY.try_acquire().map_err(|_| {
        anyhow!("input busy: one call is executing and one is already queued; wait for them before sending more input")
    })?;
    let input = INPUT.lock().await;
    Ok((capacity, input))
}

fn check_active(
    session: &tokio_util::sync::CancellationToken,
    request: &tokio_util::sync::CancellationToken,
    turn: &tokio_util::sync::CancellationToken,
) -> Result<()> {
    if session.is_cancelled() {
        bail!("the session ended");
    }
    if request.is_cancelled() {
        bail!("the input call was cancelled");
    }
    if turn.is_cancelled() {
        bail!("control changed; this input call will not resume after handback or resume");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use inject::tests::run as run_recorded;

    #[tokio::test]
    async fn input_queue_is_bounded_and_a_cancelled_waiter_releases_capacity() {
        let active = input_slot().await.unwrap();
        let mut queued = Box::pin(input_slot());
        assert!(futures::poll!(queued.as_mut()).is_pending());
        let err = input_slot().await.unwrap_err().to_string();
        assert!(err.contains("input busy"), "{err}");
        drop(queued);
        let mut replacement = Box::pin(input_slot());
        assert!(futures::poll!(replacement.as_mut()).is_pending());
        drop(active);
        assert!(replacement.await.is_ok());
    }

    #[test]
    fn input_checks_request_and_driver_grants_in_addition_to_the_session() {
        use tokio_util::sync::CancellationToken;
        let session = CancellationToken::new();
        let request = CancellationToken::new();
        let turn = CancellationToken::new();
        assert!(check_active(&session, &request, &turn).is_ok());
        request.cancel();
        assert!(check_active(&session, &request, &turn).unwrap_err().to_string().contains("call was cancelled"));
        let request = CancellationToken::new();
        turn.cancel();
        assert!(check_active(&session, &request, &turn).unwrap_err().to_string().contains("control changed"));
        session.cancel();
        assert!(check_active(&session, &request, &turn).unwrap_err().to_string().contains("session ended"));
    }

    fn mouse(args: serde_json::Value) -> Result<MouseRequest> {
        let p: MouseParams = parse(args.as_object().unwrap().clone())?;
        MouseRequest::from_params(&p)
    }

    #[test]
    fn validates_mouse_request_shapes() {
        let err = |v| mouse(v).unwrap_err().to_string();
        assert!(err(json!({"action": "move"})).contains("move needs x and y"));
        assert!(err(json!({"action": "click", "x": 3})).contains("both x and y"));
        assert!(err(json!({"action": "drag", "x": 1, "y": 2})).contains("to_x and to_y"));
        assert!(err(json!({"action": "click", "x": 1, "y": 2, "to_x": 3, "to_y": 4})).contains("only apply to drag"));
        assert!(err(json!({"action": "scroll"})).contains("needs a direction"));
        assert!(err(json!({"action": "click", "amount": 2})).contains("only apply to scroll"));
        assert!(err(json!({"action": "scroll", "direction": "up", "amount": 51})).contains("1-50"));
        assert!(err(json!({"action": "move", "x": 1, "y": 1, "modifiers": ["ctrl"]})).contains("not to move"));
        assert!(err(json!({"action": "click", "modifiers": ["q"]})).contains("modifier 'q'"));
        assert!(err(json!({"action": "click", "space": "screen", "screenshot_id": "s1"})).contains("screenshot_id"));
        assert!(err(json!({"action": "hover"})).contains("invalid arguments"));

        let r = mouse(json!({"action": "scroll", "direction": "left", "modifiers": ["Shift", "ctrl"]})).unwrap();
        assert_eq!(r.scroll, Some((-3, Axis::Horizontal)));
        assert_eq!(r.modifiers, vec![Modifier::Ctrl, Modifier::Shift]);
        assert_eq!(r.at, None);
    }

    fn shot() -> ShotGeometry {
        ShotGeometry {
            monitor_x: 0,
            monitor_y: 0,
            monitor_width: 5120,
            monitor_height: 2160,
            image_width: 1280,
            image_height: 540,
        }
    }

    /// One 5120x2160 primary between two portrait monitors that start 9 px higher.
    fn layout() -> Vec<Rect> {
        vec![
            Rect { x: 0, y: 0, width: 5120, height: 2160 },
            Rect { x: -1440, y: -9, width: 1440, height: 2560 },
            Rect { x: 5120, y: -9, width: 1440, height: 2560 },
        ]
    }

    #[test]
    fn maps_screenshot_pixels_and_rejects_points_outside() {
        assert_eq!(image_to_screen((640.0, 270.0), "s1", &shot()).unwrap(), (2562, 1082));
        assert_eq!(image_to_screen((1279.9, 539.9), "s1", &shot()).unwrap(), (5119, 2159));
        for bad in [(-1.0, 3.0), (1280.0, 3.0), (3.0, 540.0), (f64::NAN, 1.0), (f64::INFINITY, 1.0)] {
            let err = image_to_screen(bad, "s1", &shot()).unwrap_err().to_string();
            assert!(err.contains("outside screenshot s1") || err.contains("finite"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn rejects_screen_points_on_no_monitor() {
        let monitors = layout();
        // Inside the virtual-desktop bounding box, but below the primary and between the portraits.
        let err = check_on_monitor((100, 2300), &monitors).unwrap_err().to_string();
        assert!(err.contains("on no monitor") && err.contains("(-1440, -9) 1440x2560"), "{err}");
        check_on_monitor((-1440, -9), &monitors).unwrap();
        check_on_monitor((6559, 2550), &monitors).unwrap();
        assert!(check_on_monitor((6560, 0), &monitors).is_err());
        assert!(check_on_monitor((0, -1), &monitors).is_err());
        assert_eq!(screen_point((10.7, -3.2)).unwrap(), (10, -4));
        assert!(screen_point((1e12, 0.0)).is_err());
    }

    #[test]
    fn detects_stale_screenshots() {
        check_shot_current("s1", &shot(), &layout()).unwrap();
        let moved = vec![Rect { x: 0, y: 0, width: 3840, height: 2160 }];
        assert!(check_shot_current("s1", &shot(), &moved).unwrap_err().to_string().contains("take a new screenshot"));
    }

    fn calls(plan: &Plan) -> Vec<String> {
        let (r, calls) = run_recorded(plan);
        r.unwrap();
        calls
    }

    #[test]
    fn plans_clicks_with_modifiers() {
        let req = mouse(json!({"action": "double_click", "x": 1, "y": 1, "modifiers": ["shift"]})).unwrap();
        let plan = plan_mouse(&req, Some((10, 20)), None);
        assert_eq!((plan.unit, plan.total), ("clicks", 2));
        assert_eq!(
            calls(&plan),
            ["move 10,20", "key Shift Press", "button Left Click", "button Left Click", "key Shift Release"]
        );
        // Every click is checked against the window at the point.
        assert!(
            plan.steps
                .iter()
                .filter(|s| matches!(s.op, Op::Click(_)))
                .all(|s| s.check == Some(InputTarget::At(10, 20)))
        );
    }

    #[test]
    fn plans_a_click_at_the_pointer() {
        let req = mouse(json!({"action": "right_click"})).unwrap();
        let plan = plan_mouse(&req, None, None);
        assert_eq!(calls(&plan), ["button Right Click"]);
        assert_eq!(plan.steps[0].check, Some(InputTarget::Pointer));
    }

    #[test]
    fn plans_scrolls_notch_by_notch() {
        let req = mouse(json!({"action": "scroll", "direction": "up", "amount": 2, "x": 0, "y": 0})).unwrap();
        let plan = plan_mouse(&req, Some((5, 5)), None);
        assert_eq!((plan.unit, plan.total), ("notches", 2));
        assert_eq!(calls(&plan), ["move 5,5", "scroll -1 Vertical", "scroll -1 Vertical"]);
    }

    #[test]
    fn plans_drags_checking_both_ends_first() {
        let req = mouse(json!({"action": "drag", "x": 0, "y": 0, "to_x": 1, "to_y": 1})).unwrap();
        let plan = plan_mouse(&req, Some((0, 0)), Some((100, -50)));
        assert_eq!(plan.steps[0], inject::Step { op: Op::Verify, check: Some(InputTarget::At(100, -50)), done: 0 });
        let calls = calls(&plan);
        assert_eq!(calls.first().unwrap(), "move 0,0");
        assert_eq!(calls[1], "button Left Press");
        assert_eq!(calls[calls.len() - 2], "move 100,-50");
        assert_eq!(calls.last().unwrap(), "button Left Release");
        assert_eq!(calls.iter().filter(|c| c.starts_with("move")).count(), 1 + DRAG_STEPS as usize);
    }

    #[test]
    fn plans_text_in_runs_with_real_line_breaks() {
        let plan = plan_text("ab\r\ncd\n\tx\ry").unwrap();
        assert_eq!(plan.total, 11);
        assert_eq!(
            calls(&plan),
            [
                "text ab",
                "key Return Click",
                "text cd",
                "key Return Click",
                "key Tab Click",
                "text x",
                "key Return Click",
                "text y"
            ]
        );
        // Runs are cut by characters, so a surrogate pair never splits.
        let long: String = "\u{1F600}".repeat(TEXT_RUN + 1);
        let plan = plan_text(&long).unwrap();
        let texts: Vec<_> =
            plan.steps.iter().filter_map(|s| if let Op::Text(t) = &s.op { Some(t) } else { None }).collect();
        assert_eq!(texts.len(), 2);
        assert_eq!(texts[0].chars().count(), TEXT_RUN);
        assert_eq!(plan.total, TEXT_RUN + 1);
    }

    #[test]
    fn rejects_bad_text() {
        assert!(plan_text("").unwrap_err().to_string().contains("empty"));
        assert!(plan_text("a\u{7}b").unwrap_err().to_string().contains("U+0007 at position 1"));
        assert!(plan_text(&"x".repeat(MAX_TEXT_CHARS)).is_ok());
        assert!(plan_text(&"x".repeat(MAX_TEXT_CHARS + 1)).unwrap_err().to_string().contains("split it"));
    }

    #[test]
    fn plans_chords_and_repeats() {
        let plan = plan_key_spec("ctrl+shift+t", 1).unwrap();
        assert_eq!(
            calls(&plan),
            [
                "key Control Press",
                "key Shift Press",
                if cfg!(windows) { "key T Click" } else { "key Unicode('t') Click" },
                "key Shift Release",
                "key Control Release"
            ]
        );
        let plan = plan_key_spec("Down Return", 3).unwrap();
        assert_eq!((plan.unit, plan.total), ("chords", 6));
        // Every press and tap is checked against the focused window; releases never wait on a check.
        for step in &plan.steps {
            match step.op {
                Op::PressKey(_) | Op::TapKey(_) => assert_eq!(step.check, Some(InputTarget::Focused)),
                _ => assert_eq!(step.check, None),
            }
        }
    }

    #[test]
    fn refuses_key_specs_before_pressing_anything() {
        let err = |s: &str, r| plan_key_spec(s, r).unwrap_err().to_string();
        #[cfg(windows)]
        {
            assert!(err("win+l", 1).contains("'win+l' is refused: it locks the workstation"));
            assert!(err("a ctrl+alt+del", 1).contains("secure attention"));
        }
        #[cfg(target_os = "macos")]
        assert!(err("ctrl+cmd+q", 1).contains("locks"));
        assert!(err("a", 0).contains("repeat must be 1-100"));
        assert!(err("a", 101).contains("repeat must be 1-100"));
        assert!(err(&"a ".repeat(20), 100).contains("2000 chords"));
        assert!(err("hyper+x", 1).contains("unknown key"));
    }
}
