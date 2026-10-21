//! The menu bar (macOS) / notification area (Windows) icon: this computer's host at a glance, and
//! the owner's controls over it, without opening the app or a terminal.
//!
//! The icon is the brand mark, faded while no host runs here. A badge in its corner says who is
//! here: a ring while a controller is connected (on any channel: tools, activity, live view,
//! files), a solid dot while someone operates this computer (a tool call running or just ended, or
//! a person who took over), two bars while paused. macOS draws it as a template image, tinted by the
//! system for the menu bar; Windows colours the badge (blue: the agent; amber: a person; orange:
//! paused). The menu's first line says the same in words, then offers the settings page, the pairing
//! code, the recent activity, pause / resume, stop / start and quit. Everything comes from the host's
//! local control channel (see `host_local`), polled every few seconds; nothing is computed here that
//! the host does not report.

use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use ar_core::Profile;
use serde_json::json;
use tauri::{
    AppHandle, Emitter, Manager, Runtime,
    image::Image,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
};

use crate::host_local::{self, DriverState, Summary};

/// The page tells the tray which language it shows, so both agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Zh,
}

impl Lang {
    pub fn parse(s: &str) -> Self {
        if s.to_ascii_lowercase().starts_with("zh") { Lang::Zh } else { Lang::En }
    }

    /// The language before the page reported one: the process locale, if it says so.
    fn from_env() -> Self {
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|k| std::env::var(k).ok())
            .find(|v| !v.is_empty())
            .map(|v| Lang::parse(&v))
            .unwrap_or(Lang::En)
    }
}

struct Items<R: Runtime> {
    status: MenuItem<R>,
    open: MenuItem<R>,
    pair: MenuItem<R>,
    recent: MenuItem<R>,
    pause: MenuItem<R>,
    stop: MenuItem<R>,
    quit: MenuItem<R>,
}

pub struct Tray<R: Runtime> {
    icon: TrayIcon<R>,
    items: Items<R>,
    lang: Mutex<Lang>,
    last: Mutex<Option<Summary>>,
    /// Wakes the poller early (after a menu action, so the icon follows at once).
    wake: mpsc::Sender<()>,
}

/// Menu item ids, also the `tray-navigate` actions the page understands.
const OPEN: &str = "tray:open";
const PAIR: &str = "tray:pair";
const RECENT: &str = "tray:recent";
const PAUSE: &str = "tray:pause";
const STOP: &str = "tray:stop";
const QUIT: &str = "tray:quit";

/// "laptop", "laptop, phone", "laptop, phone and 2 more".
fn names(lang: Lang, names: &[String]) -> String {
    let shown = names.iter().take(2).cloned().collect::<Vec<_>>();
    let more = names.len().saturating_sub(2);
    match (lang, more) {
        (_, 0) => shown.join(if lang == Lang::Zh { "、" } else { ", " }),
        (Lang::Zh, n) => format!("{} 等 {} 个", shown.join("、"), n + 2),
        (Lang::En, n) => format!("{} and {n} more", shown.join(", ")),
    }
}

/// The space Chinese text puts between a Latin name and the next Chinese word ("laptop 已连接"),
/// none after a Chinese one ("等 3 个已连接").
fn gap(s: &str) -> &'static str {
    if s.chars().last().is_some_and(|c| c.is_ascii()) { " " } else { "" }
}

/// The menu's first line: whether the host runs, who is connected, who operates.
fn status_line(lang: Lang, s: &Summary) -> String {
    let zh = lang == Lang::Zh;
    if !s.running {
        return if zh { "被控端：未运行" } else { "Host: not running" }.into();
    }
    if s.unknown.is_some() {
        return if zh {
            "被控端：在运行，但读不到它的状态"
        } else {
            "Host: running, but its state cannot be read"
        }
        .into();
    }
    let who = names(lang, &s.connected);
    match (&s.driver, &s.busy, s.connected.is_empty()) {
        (DriverState::Paused, _, true) => if zh { "被控端：已暂停" } else { "Host: paused" }.into(),
        (DriverState::Paused, _, false) => {
            if zh {
                format!("被控端：已暂停 · {who}{}已连接", gap(&who))
            } else {
                format!("Host: paused · {who} connected")
            }
        }
        (DriverState::Person(d), _, _) => {
            if zh {
                format!("被控端：{d}{}已接管", gap(d))
            } else {
                format!("Host: {d} took over")
            }
        }
        (DriverState::Agent, Some(b), _) if b.person => {
            if zh {
                format!("被控端：{}{}正在操作（{}）", b.peer, gap(&b.peer), b.tool)
            } else {
                format!("Host: {} is operating ({})", b.peer, b.tool)
            }
        }
        (DriverState::Agent, Some(b), _) => {
            if zh {
                format!("被控端：{} 的智能体正在操作（{}）", b.peer, b.tool)
            } else {
                format!("Host: an agent on {} is operating ({})", b.peer, b.tool)
            }
        }
        (DriverState::Agent, None, false) => {
            if zh {
                format!("被控端：{who}{}已连接，空闲", gap(&who))
            } else {
                format!("Host: {who} connected, idle")
            }
        }
        (DriverState::Agent, None, true) => {
            if zh { "被控端：运行中，没有控制端连接" } else { "Host: running, nobody connected" }.into()
        }
    }
}

/// Text of every menu item in `lang` for `s`.
fn texts(lang: Lang, s: &Summary) -> [String; 7] {
    let status = status_line(lang, s);
    let paused = s.driver == DriverState::Paused;
    match lang {
        Lang::Zh => [
            status,
            "打开设置…".into(),
            "显示配对码…".into(),
            "最近活动…".into(),
            if paused { "恢复被控端" } else { "暂停被控端" }.into(),
            if s.running { "停止被控端" } else { "启动被控端" }.into(),
            if s.running { "退出 ai-remote（被控端继续运行）" } else { "退出 ai-remote" }.into(),
        ],
        Lang::En => [
            status,
            "Open Settings…".into(),
            "Show Pairing Code…".into(),
            "Recent Activity…".into(),
            if paused { "Resume Host" } else { "Pause Host" }.into(),
            if s.running { "Stop Host" } else { "Start Host" }.into(),
            if s.running { "Quit ai-remote (the host keeps running)" } else { "Quit ai-remote" }.into(),
        ],
    }
}

/// Installs the tray icon and starts following the host. The returned handle is kept in the app's
/// state so the page can set the language.
pub fn install<R: Runtime>(app: &AppHandle<R>, profile: Profile, cli: std::path::PathBuf) -> Result<Tray<R>> {
    let lang = Lang::from_env();
    let initial = Summary::STOPPED;
    let t = texts(lang, &initial);
    let items = Items {
        status: MenuItem::with_id(app, "tray:status", &t[0], false, None::<&str>)?,
        open: MenuItem::with_id(app, OPEN, &t[1], true, None::<&str>)?,
        pair: MenuItem::with_id(app, PAIR, &t[2], false, None::<&str>)?,
        recent: MenuItem::with_id(app, RECENT, &t[3], false, None::<&str>)?,
        pause: MenuItem::with_id(app, PAUSE, &t[4], false, None::<&str>)?,
        stop: MenuItem::with_id(app, STOP, &t[5], true, None::<&str>)?,
        quit: MenuItem::with_id(app, QUIT, &t[6], true, None::<&str>)?,
    };
    let menu = Menu::with_items(
        app,
        &[
            &items.status,
            &PredefinedMenuItem::separator(app)?,
            &items.open,
            &items.pair,
            &items.recent,
            &PredefinedMenuItem::separator(app)?,
            &items.pause,
            &items.stop,
            &PredefinedMenuItem::separator(app)?,
            &items.quit,
        ],
    )?;
    let icon = TrayIconBuilder::with_id("ai-remote")
        .icon(render(&initial))
        .icon_as_template(TEMPLATE)
        .tooltip("ai-remote")
        .menu(&menu)
        // macOS: the menu is what a left click shows (menu bar convention). Windows: a left click
        // opens the app; the menu is on the right button.
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event
                && !cfg!(target_os = "macos")
            {
                // Only brings the window back; the menu (right button) has the settings.
                show_main(tray.app_handle(), json!({}));
            }
        })
        .build(app)
        .context("create the tray icon")?;
    let (wake, woken) = mpsc::channel();
    let tray = Tray { icon, items, lang: Mutex::new(lang), last: Mutex::new(None), wake };
    poll(app.clone(), profile.clone(), woken);
    let handle = app.clone();
    app.on_menu_event(move |_, event| on_menu(&handle, &profile, &cli, event));
    Ok(tray)
}

impl<R: Runtime> Tray<R> {
    /// The page's language; the menu follows it.
    pub fn set_lang(&self, lang: Lang) {
        *self.lang.lock().unwrap() = lang;
        let last = self.last.lock().unwrap().clone();
        if let Some(s) = last {
            self.apply(&s);
        }
    }

    /// Whether a host runs on this computer, as of the last poll.
    pub fn host_running(&self) -> bool {
        self.last.lock().unwrap().as_ref().is_some_and(|s| s.running)
    }

    /// Whether closing the window keeps the app in the tray: a host runs here, or the person made
    /// the app open at login (they want the icon around).
    pub fn keeps_running(&self) -> bool {
        self.host_running() || RESIDENT.load(Ordering::Relaxed)
    }

    /// Polls again now (after a menu action).
    pub fn refresh(&self) {
        let _ = self.wake.send(());
    }

    fn apply(&self, s: &Summary) {
        let lang = *self.lang.lock().unwrap();
        let t = texts(lang, s);
        let items = &self.items;
        for (item, text) in [
            (&items.status, &t[0]),
            (&items.open, &t[1]),
            (&items.pair, &t[2]),
            (&items.recent, &t[3]),
            (&items.pause, &t[4]),
            (&items.stop, &t[5]),
            (&items.quit, &t[6]),
        ] {
            let _ = item.set_text(text);
        }
        let _ = items.pair.set_enabled(s.running);
        let _ = items.recent.set_enabled(s.running);
        let _ = items.pause.set_enabled(s.running);
        // `set_icon` alone would drop the template flag on macOS.
        let _ = self.icon.set_icon_with_as_template(Some(render(s)), TEMPLATE);
        let _ = self.icon.set_tooltip(Some(&t[0]));
    }

    fn update(&self, s: Summary) {
        let changed = {
            let mut last = self.last.lock().unwrap();
            let changed = last.as_ref() != Some(&s);
            *last = Some(s.clone());
            changed
        };
        if changed {
            self.apply(&s);
        }
    }
}

/// Asks the host for its status every few seconds (sooner when woken) and redraws on change.
fn poll<R: Runtime>(app: AppHandle<R>, profile: Profile, woken: mpsc::Receiver<()>) {
    std::thread::Builder::new()
        .name("tray-poll".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tray runtime");
            loop {
                let probe = rt.block_on(host_local::probe(&profile));
                if let Some(tray) = app.try_state::<Tray<R>>() {
                    tray.update(Summary::from_probe(&probe, host_local::now_ms()));
                }
                // Every 3 s, or 0.4 s after a menu action (the host needs a moment to change).
                match woken.recv_timeout(Duration::from_secs(3)) {
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Ok(()) => std::thread::sleep(Duration::from_millis(400)),
                }
            }
        })
        .expect("spawn the tray poller");
}

fn on_menu<R: Runtime>(app: &AppHandle<R>, profile: &Profile, cli: &std::path::Path, event: MenuEvent) {
    let id = event.id().as_ref();
    match id {
        OPEN => show_main(app, json!({"view": "settings", "sec": "device"})),
        PAIR => show_main(app, json!({"view": "settings", "sec": "device", "action": "pair_code"})),
        RECENT => show_main(app, json!({"view": "settings", "sec": "device", "anchor": "recent"})),
        PAUSE | STOP => {
            let app = app.clone();
            let profile = profile.clone();
            let cli = cli.to_path_buf();
            let id = id.to_string();
            tauri::async_runtime::spawn(async move {
                let tray = app.try_state::<Tray<R>>();
                let last = tray.as_ref().and_then(|t| t.last.lock().unwrap().clone());
                // A Windows service needs an administrator to start again: the page asks first.
                if id == STOP && last.as_ref().is_some_and(|s| s.running && s.system) {
                    show_main(&app, json!({"view": "settings", "sec": "device", "action": "confirm_stop"}));
                    return;
                }
                let result = if id == PAUSE {
                    let paused = last.as_ref().is_some_and(|s| s.driver == DriverState::Paused);
                    host_local::require(&profile, json!({"op": "pause", "on": !paused})).await.map(|_| ())
                } else if last.as_ref().is_some_and(|s| s.running) {
                    host_local::require(&profile, json!({"op": "shutdown"})).await.map(|_| ())
                } else {
                    host_local::cli_json(&cli, &["host", "start"]).await.map(|_| ())
                };
                if let Err(e) = result {
                    tracing_warn(&format!("tray {id}: {e:#}"));
                    // The page shows the reason (the tray cannot).
                    show_main(&app, json!({"view": "settings", "sec": "device", "error": format!("{e:#}")}));
                }
                if let Some(t) = tray {
                    t.refresh();
                }
            });
        }
        QUIT => app.exit(0),
        _ => {}
    }
}

fn tracing_warn(msg: &str) {
    eprintln!("[tray] {msg}");
}

/// The app opens at login (see `login_item`): closing the window then keeps it in the tray.
static RESIDENT: AtomicBool = AtomicBool::new(false);

pub fn set_resident(on: bool) {
    RESIDENT.store(on, Ordering::Relaxed);
}

/// macOS: the Dock icon shows while the window does; with the window in the tray the app is a menu
/// bar app. Elsewhere nothing to do (the taskbar button goes with the window).
pub fn set_dock<R: Runtime>(app: &AppHandle<R>, visible: bool) {
    #[cfg(target_os = "macos")]
    let _ = app.set_dock_visibility(visible);
    #[cfg(not(target_os = "macos"))]
    let _ = (app, visible);
}

/// Shows the main window and tells the page where to go (`tray-navigate`).
pub fn show_main<R: Runtime>(app: &AppHandle<R>, to: serde_json::Value) {
    set_dock(app, true);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        let _ = w.emit("tray-navigate", to);
    }
}

/// The brand mark (two chevron-like polygons, see `glyphs.tsx`) in a square of 256 × 184 units.
const MARK_A: [(f32, f32); 7] =
    [(120.8, 43.3), (120.8, 190.1), (83.2, 156.2), (57.8, 212.7), (28.6, 199.5), (54.0, 144.0), (7.9, 144.0)];
const MARK_Y: [(f32, f32); 7] =
    [(135.2, 43.3), (135.2, 190.1), (172.8, 156.2), (198.2, 212.7), (227.4, 199.5), (202.0, 144.0), (248.1, 144.0)];

fn inside(poly: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut hit = false;
    let n = poly.len();
    for i in 0..n {
        let (x1, y1) = poly[i];
        let (x2, y2) = poly[(i + 1) % n];
        if (y1 > y) != (y2 > y) && x < (x2 - x1) * (y - y1) / (y2 - y1) + x1 {
            hit = !hit;
        }
    }
    hit
}

/// macOS: the icon is a template image (only its alpha counts; the system tints it for the menu
/// bar, light or dark, like every other menu bar extra). Windows: drawn in colour as is.
const TEMPLATE: bool = cfg!(target_os = "macos");

/// The corner badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Badge {
    None,
    /// A controller is connected.
    Ring([u8; 3]),
    /// Someone operates this computer.
    Dot([u8; 3]),
    Pause([u8; 3]),
}

const AGENT: [u8; 3] = [76, 141, 255];
const PERSON: [u8; 3] = [240, 179, 58];
const PAUSED: [u8; 3] = [255, 159, 10];

fn badge(s: &Summary) -> Badge {
    if !s.running {
        return Badge::None;
    }
    match (&s.driver, &s.busy) {
        (DriverState::Paused, _) => Badge::Pause(PAUSED),
        (DriverState::Person(_), _) => Badge::Dot(PERSON),
        (DriverState::Agent, Some(b)) => Badge::Dot(if b.person { PERSON } else { AGENT }),
        (DriverState::Agent, None) if !s.connected.is_empty() => Badge::Ring(AGENT),
        _ => Badge::None,
    }
}

/// Draws the icon for `s`. Rendered 4× and box-filtered, so edges stay smooth at menu bar size.
pub fn render(s: &Summary) -> Image<'static> {
    // macOS fits the image to the menu bar's height (22 pt), so 44 px stays sharp on Retina;
    // Windows' notification area wants 32 px.
    let size: u32 = if cfg!(target_os = "macos") { 44 } else { 32 };
    // The mark: black for a template (alpha is what counts), neutral gray in colour (visible on
    // light and dark taskbars). Faded while no host runs.
    let mark = if TEMPLATE { [0, 0, 0] } else { [142, 142, 147] };
    let alpha = if s.running { 255u8 } else { 90 };
    let b = badge(s);
    let tint = |c: [u8; 3]| if TEMPLATE { [0, 0, 0] } else { c };
    let b = match b {
        Badge::None => Badge::None,
        Badge::Ring(c) => Badge::Ring(tint(c)),
        Badge::Dot(c) => Badge::Dot(tint(c)),
        Badge::Pause(c) => Badge::Pause(tint(c)),
    };
    render_px(size, mark, alpha, b)
}

fn render_px(size: u32, mark: [u8; 3], alpha: u8, badge: Badge) -> Image<'static> {
    const SS: u32 = 4;
    let big = size * SS;
    let f = big as f32;
    // The mark fills 88 % of the width, centred vertically.
    let scale = f * 0.88 / (248.1 - 7.9);
    let ox = (f - (248.1 - 7.9) * scale) / 2.0 - 7.9 * scale;
    let oy = (f - (212.7 - 43.3) * scale) / 2.0 - 43.3 * scale;
    let (dcx, dcy, dr) = (f * 0.76, f * 0.76, f * 0.2);
    // A clear margin separates the badge from the mark.
    let gap = dr + f * 0.06;
    let mut px = vec![[0u8; 4]; (big * big) as usize];
    for y in 0..big {
        for x in 0..big {
            let (sx, sy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (mx, my) = ((sx - ox) / scale, (sy - oy) / scale);
            let d2 = (sx - dcx).powi(2) + (sy - dcy).powi(2);
            let p = &mut px[(y * big + x) as usize];
            let in_badge = match badge {
                Badge::None => None,
                Badge::Dot(c) => (d2 <= dr * dr).then_some(c),
                Badge::Ring(c) => (d2 <= dr * dr && d2 >= (dr * 0.55).powi(2)).then_some(c),
                Badge::Pause(c) => {
                    let (bw, bh) = (dr * 0.55, dr * 1.7);
                    let left = (sx - (dcx - dr * 0.45)).abs() <= bw / 2.0;
                    let right = (sx - (dcx + dr * 0.45)).abs() <= bw / 2.0;
                    ((left || right) && (sy - dcy).abs() <= bh / 2.0).then_some(c)
                }
            };
            if let Some(c) = in_badge {
                *p = [c[0], c[1], c[2], 255];
            } else if badge != Badge::None && d2 <= gap * gap {
                // Keep the badge's surroundings clear.
            } else if inside(&MARK_A, mx, my) || inside(&MARK_Y, mx, my) {
                *p = [mark[0], mark[1], mark[2], alpha];
            }
        }
    }
    // Box filter down to `size`.
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut acc = [0u32; 4];
            for dy in 0..SS {
                for dx in 0..SS {
                    let p = px[((y * SS + dy) * big + x * SS + dx) as usize];
                    // Premultiplied average, so transparent neighbours do not darken edges.
                    let a = p[3] as u32;
                    acc[0] += p[0] as u32 * a;
                    acc[1] += p[1] as u32 * a;
                    acc[2] += p[2] as u32 * a;
                    acc[3] += a;
                }
            }
            // Alpha 0 means every channel sum is 0 too, so the guard-free division is exact.
            let a = acc[3];
            let ch = |v: u32| (v / a.max(1)) as u8;
            out.extend_from_slice(&[ch(acc[0]), ch(acc[1]), ch(acc[2]), (a / (SS * SS)) as u8]);
        }
    }
    Image::new_owned(out, size, size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_local::Busy;

    fn summary(connected: &[&str], driver: DriverState, busy: Option<Busy>) -> Summary {
        let connected = connected.iter().map(|s| s.to_string()).collect();
        Summary { running: true, unknown: None, system: false, connected, driver, busy }
    }

    fn agent_call() -> Option<Busy> {
        Some(Busy { peer: "laptop".into(), tool: "screenshot".into(), person: false })
    }

    /// The pixel at the badge's centre.
    fn centre(img: &Image<'_>) -> Vec<u8> {
        let (x, y) = ((img.width() as f32 * 0.76) as usize, (img.height() as f32 * 0.76) as usize);
        let i = (y * img.width() as usize + x) * 4;
        img.rgba()[i..i + 4].to_vec()
    }

    #[test]
    fn badges_follow_who_is_here() {
        assert_eq!(badge(&Summary::STOPPED), Badge::None);
        assert_eq!(badge(&summary(&[], DriverState::Agent, None)), Badge::None);
        assert_eq!(badge(&summary(&["laptop"], DriverState::Agent, None)), Badge::Ring(AGENT));
        assert_eq!(badge(&summary(&["laptop"], DriverState::Agent, agent_call())), Badge::Dot(AGENT));
        let person = Some(Busy { person: true, ..agent_call().unwrap() });
        assert_eq!(badge(&summary(&["laptop"], DriverState::Agent, person)), Badge::Dot(PERSON));
        assert_eq!(badge(&summary(&["laptop"], DriverState::Person("laptop".into()), None)), Badge::Dot(PERSON));
        assert_eq!(badge(&summary(&["laptop"], DriverState::Paused, agent_call())), Badge::Pause(PAUSED));
    }

    #[test]
    fn the_icon_draws_the_mark_and_the_badge() {
        let idle = render(&summary(&[], DriverState::Agent, None));
        let size = idle.width() as usize;
        assert_eq!(idle.rgba().len(), size * size * 4);
        let opaque = idle.rgba().chunks(4).filter(|p| p[3] > 200).count();
        assert!(opaque > size * size / 10, "the mark covers a good part of the square: {opaque}");
        // Idle: the mark itself is under the badge's spot; a ring leaves its centre clear; a dot fills it.
        assert_eq!(centre(&idle)[3], 255);
        assert_eq!(centre(&render(&summary(&["laptop"], DriverState::Agent, None)))[3], 0);
        let busy = centre(&render(&summary(&["laptop"], DriverState::Agent, agent_call())));
        assert_eq!(busy[3], 255);
        if !TEMPLATE {
            assert_eq!(busy[..3], AGENT);
        }
        assert!(render(&Summary::STOPPED).rgba().chunks(4).all(|p| p[3] <= 90), "faded while no host runs");
    }

    #[test]
    fn menu_texts_follow_state_and_language() {
        let s = summary(&["laptop"], DriverState::Paused, None);
        let zh = texts(Lang::Zh, &s);
        assert_eq!(zh[0], "被控端：已暂停 · laptop 已连接");
        assert_eq!(zh[4], "恢复被控端");
        assert_eq!(
            texts(Lang::Zh, &summary(&["laptop"], DriverState::Agent, agent_call()))[0],
            "被控端：laptop 的智能体正在操作（screenshot）"
        );
        let three = summary(&["a", "b", "c"], DriverState::Agent, None);
        assert_eq!(status_line(Lang::En, &three), "Host: a, b and 1 more connected, idle");
        assert_eq!(status_line(Lang::Zh, &three), "被控端：a、b 等 3 个已连接，空闲");
        let en = texts(Lang::En, &Summary::STOPPED);
        assert_eq!(en[0], "Host: not running");
        assert_eq!(en[5], "Start Host");
        assert_eq!(Lang::parse("zh-CN"), Lang::Zh);
        assert_eq!(Lang::parse("en_US.UTF-8"), Lang::En);
    }
}
