//! DXGI Desktop Duplication on the same D3D11 device and GPU conversion as `win.rs`: frames come out
//! as the same NV12 `Raw { gpu }` textures. Windows Graphics Capture cannot see the secure desktop
//! (lock screen, UAC prompts); Desktop Duplication can, from a thread attached to that desktop in a
//! process allowed to open it. This thread follows the input desktop and rebuilds the duplication
//! whenever DXGI reports it lost (desktop switch, mode change, a full-screen app taking the output).
//!
//! Chosen in service mode and on a secure desktop, or with `AI_REMOTE_CAPTURE=dda` (see
//! `capture::start`). The pointer is not in the picture (Desktop Duplication reports it separately).
//! TODO: draw the pointer from `DXGI_OUTDUPL_FRAME_INFO` / `GetFramePointerShape`.
//! A rotated output (a portrait monitor) arrives in the panel's own orientation; the video processor
//! turns it upright (see [`upright`]), as Windows Graphics Capture does by itself.
//!
//! A process may duplicate an output only once, so every viewer of a monitor shares one duplication
//! (`capture/shared.rs`): its thread converts each frame once per viewer, into that viewer's own size
//! and texture pool, and puts it in that viewer's slot.
//!
//! Follows RustDesk's `libs/scrap/src/dxgi/mod.rs` (acquire / release, access lost) and
//! `src/server/video_service.rs` with `platform/windows.cc` (`inputDesktopSelected` /
//! `selectInputDesktop`) for the desktop switch.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use windows::{
    Win32::{
        Foundation::{E_ACCESSDENIED, E_INVALIDARG, GENERIC_WRITE, HANDLE},
        Graphics::{
            Direct3D11::{
                D3D11_TEXTURE2D_DESC, D3D11_VIDEO_PROCESSOR_ROTATION, D3D11_VIDEO_PROCESSOR_ROTATION_90,
                D3D11_VIDEO_PROCESSOR_ROTATION_180, D3D11_VIDEO_PROCESSOR_ROTATION_270,
                D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY, ID3D11Texture2D,
            },
            Dxgi::{
                Common::{
                    DXGI_MODE_ROTATION, DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE180,
                    DXGI_MODE_ROTATION_ROTATE270,
                },
                DXGI_ERROR_NOT_CURRENTLY_AVAILABLE, DXGI_ERROR_SESSION_DISCONNECTED, DXGI_ERROR_UNSUPPORTED,
                DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIDevice, IDXGIOutput1, IDXGIOutputDuplication,
                IDXGIResource,
            },
            Gdi::HMONITOR,
        },
        System::{
            StationsAndDesktops::{
                CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CREATEMENU, DESKTOP_CREATEWINDOW, DESKTOP_ENUMERATE,
                DESKTOP_HOOKCONTROL, DESKTOP_READOBJECTS, DESKTOP_SWITCHDESKTOP, DESKTOP_WRITEOBJECTS,
                GetThreadDesktop, GetUserObjectInformationW, HDESK, OpenInputDesktop, SetThreadDesktop, UOI_NAME,
            },
            Threading::GetCurrentThreadId,
        },
    },
    core::{HRESULT, Interface},
};

use super::{
    Raw, Slot,
    shared::{Member, Source, Sources},
    win::{Converter, Gpu},
};

/// How often a still screen checks for a switch of the input desktop.
const DESKTOP_CHECK: Duration = Duration::from_millis(100);
/// Pause between attempts while the output cannot be duplicated (e.g. the secure desktop is up and
/// this process may not open it).
const RETRY: Duration = Duration::from_millis(500);

/// Every Desktop Duplication of this process: one per monitor, keyed by its `HMONITOR`.
static DUPLICATIONS: Sources<isize, Viewer> = Sources::new();

/// One viewer's share of a monitor's Desktop Duplication; leaves when dropped, and the last one out
/// stops the duplication.
pub(super) struct DdaCapture {
    _member: Member<isize, Viewer>,
}

/// What one viewer of a duplicated monitor gets: frames at its own size limit, through its own
/// converter (and texture pool), into its own slot.
struct Viewer {
    fps: u32,
    max_long_edge: Arc<AtomicU32>,
    slot: Arc<Slot>,
    converter: Option<SizedConverter>,
    /// No converter could be built for this viewer: its slot is closed and nothing more is converted.
    failed: bool,
}

/// Starts duplicating `monitor`, or joins the duplication of it another viewer started. A new
/// duplication fails when its first `DuplicateOutput` fails; later losses (desktop switches, mode
/// changes) are retried on the capture thread until the last viewer leaves. Every frame Desktop
/// Duplication delivers is converted for every viewer (at most one per refresh); each viewer's
/// encode loop paces its own rate. Dropping frames here would lose the last change before the screen
/// goes still, since a still screen delivers nothing more.
pub(super) fn start(
    monitor: HMONITOR,
    max_fps: u32,
    max_long_edge: Arc<AtomicU32>,
    slot: Arc<Slot>,
) -> Result<DdaCapture> {
    let viewer = Viewer { fps: max_fps.clamp(1, 240), max_long_edge, slot, converter: None, failed: false };
    // HMONITOR is a plain handle value; it crosses the thread boundary as an integer.
    let raw_monitor = monitor.0 as isize;
    let member = DUPLICATIONS.join(raw_monitor, viewer, |source| {
        let gpu = Gpu::for_monitor(monitor)?;
        let (first_tx, first_rx) = mpsc::channel();
        let thread = {
            let source = source.clone();
            std::thread::Builder::new().name("ai-remote-dda".into()).spawn(move || {
                run(&gpu, HMONITOR(raw_monitor as *mut _), &source, first_tx);
            })?
        };
        match first_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(thread),
            result => {
                source.stop();
                let _ = thread.join();
                Err(match result {
                    Ok(Err(e)) => e,
                    _ => anyhow!("Desktop Duplication did not start"),
                })
            }
        }
    })?;
    let viewers = member.viewers();
    if viewers > 1 {
        tracing::info!(viewers, "another viewer joined this monitor's Desktop Duplication");
    }
    Ok(DdaCapture { _member: member })
}

/// A converter and the (input, output) sizes and rotation it was built for.
type SizedConverter = (Converter, (u32, u32), (u32, u32), D3D11_VIDEO_PROCESSOR_ROTATION);

/// The clockwise turn that brings a duplicated frame of `size` upright on an output whose rotation is
/// `rotation`, and the upright size. Desktop Duplication hands over the image as the panel scans it
/// out: on a monitor turned to portrait it lies on its side. Measured on 2026-10-22 on a portrait
/// monitor reporting `ROTATE270`: the desktop's top-left corner was the frame's top-right corner and
/// its rows ran down the frame, so it takes a 270-degree turn; the names match for every rotation
/// (Microsoft's Desktop Duplication sample maps them the same way).
fn upright(rotation: DXGI_MODE_ROTATION, (width, height): (u32, u32)) -> (D3D11_VIDEO_PROCESSOR_ROTATION, (u32, u32)) {
    match rotation {
        DXGI_MODE_ROTATION_ROTATE90 => (D3D11_VIDEO_PROCESSOR_ROTATION_90, (height, width)),
        DXGI_MODE_ROTATION_ROTATE180 => (D3D11_VIDEO_PROCESSOR_ROTATION_180, (width, height)),
        DXGI_MODE_ROTATION_ROTATE270 => (D3D11_VIDEO_PROCESSOR_ROTATION_270, (height, width)),
        _ => (D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY, (width, height)),
    }
}

impl Viewer {
    /// Converts the desktop image `texture` (of size `input`, from an output rotated by `rotation`)
    /// upright for this viewer and puts the result in its slot.
    fn deliver(
        &mut self,
        gpu: &Arc<Gpu>,
        texture: &ID3D11Texture2D,
        input: (u32, u32),
        rotation: DXGI_MODE_ROTATION,
        captured: Instant,
    ) {
        if self.failed {
            return;
        }
        let (turn, (width, height)) = upright(rotation, input);
        let output = crate::stream::encode::stream_size(width, height, self.max_long_edge.load(Ordering::Relaxed));
        if self.converter.as_ref().is_none_or(|(_, i, o, r)| (*i, *o, *r) != (input, output, turn)) {
            // Cleared first: a converter that cannot be built for this size ends this viewer's stream.
            self.converter = None;
            match Converter::rotated(gpu.clone(), input, output, self.fps, turn) {
                Ok(c) => self.converter = Some((c, input, output, turn)),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "GPU frame conversion unavailable");
                    self.slot.close();
                    self.failed = true;
                    return;
                }
            }
        }
        let Some((converter, _, _, _)) = self.converter.as_mut() else { return };
        match converter.convert(texture) {
            Ok(Some(frame)) => self.slot.put(Raw {
                bgra: Vec::new(),
                width: frame.width,
                height: frame.height,
                captured,
                gpu: Some(frame),
            }),
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "live view frame conversion"),
        }
    }
}

/// Sleeps for `total`, or less when the last viewer leaves meanwhile (it waits for this thread).
fn pause(source: &Source<Viewer>, total: Duration) {
    let until = Instant::now() + total;
    while !source.stopping() {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(Duration::from_millis(20)));
    }
}

fn run(gpu: &Arc<Gpu>, monitor: HMONITOR, source: &Source<Viewer>, first: mpsc::Sender<Result<()>>) {
    let mut desktop = InputDesktop::default();
    desktop.follow();
    let mut duplication = match duplicate(gpu, monitor) {
        Ok(d) => {
            let _ = first.send(Ok(()));
            Some(d)
        }
        Err(e) => {
            let _ = first.send(Err(e));
            return;
        }
    };
    let mut failing = false;
    let mut desktop_checked = Instant::now();
    while !source.stopping() {
        let Some((output, dup, rotation)) = duplication.as_ref() else {
            desktop.follow();
            match duplicate(gpu, monitor) {
                Ok(d) => {
                    tracing::info!(desktop = desktop.name.as_deref().unwrap_or("?"), "Desktop Duplication restarted");
                    duplication = Some(d);
                    failing = false;
                }
                Err(e) => {
                    if !failing {
                        tracing::warn!(error = %format!("{e:#}"), "Desktop Duplication unavailable; retrying");
                        failing = true;
                    }
                    pause(source, RETRY);
                }
            }
            continue;
        };
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // Wait for the next refresh outside `AcquireNextFrame`, then take whatever is there without
        // waiting. A waiting `AcquireNextFrame` stalls the hardware encoder on the same device
        // (multithread protected; it presumably holds the device lock while it waits): measured on
        // 2026-10-12 at 5120x2160, HEVC 20-30 ms per frame and 44 fps instead of 5 ms and 99 fps.
        // SAFETY: a wait on an output of this adapter.
        if unsafe { output.WaitForVBlank() }.is_err() {
            std::thread::sleep(Duration::from_millis(1));
        }
        // SAFETY: out-parameters owned here; the frame is released below before the next acquire.
        match unsafe { dup.AcquireNextFrame(0, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                // A still screen: a switch to the secure desktop usually also loses access, but
                // check here too so a quiet switch is not missed.
                if desktop_checked.elapsed() >= DESKTOP_CHECK {
                    desktop_checked = Instant::now();
                    if desktop.changed() {
                        duplication = None;
                    }
                }
                continue;
            }
            Err(e) => {
                // DXGI_ERROR_ACCESS_LOST: desktop switch, mode change or a full-screen exclusive app.
                tracing::debug!(error = %e, "Desktop Duplication lost; recreating");
                duplication = None;
                continue;
            }
        }
        let captured = Instant::now();
        // LastPresentTime 0: only the pointer moved, the desktop image is unchanged.
        if info.LastPresentTime != 0 {
            match resource.as_ref().context("no desktop image").and_then(|r| Ok(r.cast::<ID3D11Texture2D>()?)) {
                Ok(texture) => {
                    let mut desc = D3D11_TEXTURE2D_DESC::default();
                    // SAFETY: a descriptor query on a live texture.
                    unsafe { texture.GetDesc(&mut desc) };
                    let input = (desc.Width, desc.Height);
                    source.each(|viewer| viewer.deliver(gpu, &texture, input, *rotation, captured));
                }
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "live view frame conversion"),
            }
        }
        drop(resource);
        // SAFETY: the frame acquired above; every viewer's conversion is already queued on the same
        // device, and releasing is ordered after it.
        if unsafe { dup.ReleaseFrame() }.is_err() {
            duplication = None;
        }
    }
}

/// Duplicates the output that shows `monitor` on the capture device's adapter; with the output's rotation.
fn duplicate(gpu: &Gpu, monitor: HMONITOR) -> Result<(IDXGIOutput1, IDXGIOutputDuplication, DXGI_MODE_ROTATION)> {
    // SAFETY: DXGI enumeration and duplication with owned COM results only.
    unsafe {
        let adapter = gpu.device.cast::<IDXGIDevice>()?.GetAdapter()?;
        for i in 0.. {
            let output = adapter.EnumOutputs(i).context("the monitor is not driven by the capture GPU")?;
            if output.GetDesc().is_ok_and(|d| d.Monitor == monitor) {
                let output = output.cast::<IDXGIOutput1>()?;
                let duplication = output.DuplicateOutput(&gpu.device).map_err(refused)?;
                let rotation = duplication.GetDesc().Rotation;
                if upright(rotation, (0, 0)).0 != D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY {
                    tracing::info!(?rotation, "rotated display: frames are turned upright");
                }
                return Ok((output, duplication, rotation));
            }
        }
        unreachable!()
    }
}

/// A failed `DuplicateOutput`, with the reason its code documents, in words a viewer can act on.
fn refused(error: windows::core::Error) -> anyhow::Error {
    let why = why_refused(error.code()).unwrap_or("duplicate the output");
    anyhow::Error::new(error).context(why)
}

/// What `DuplicateOutput`'s reference documents for `code`.
fn why_refused(code: HRESULT) -> Option<&'static str> {
    Some(match code {
        // Of the documented causes, the one that applies: this process already duplicates the
        // output. Viewers of a monitor share one duplication, so an earlier one is still letting go.
        E_INVALIDARG => "this monitor's Desktop Duplication is already in use in this host (another viewer of it)",
        DXGI_ERROR_NOT_CURRENTLY_AVAILABLE => {
            "too many programs are capturing this monitor with Desktop Duplication (Windows allows four at a time)"
        }
        E_ACCESSDENIED => "this process may not read the desktop now on this monitor (a secure desktop)",
        DXGI_ERROR_UNSUPPORTED => "the display driver offers no Desktop Duplication in this monitor's current mode",
        DXGI_ERROR_SESSION_DISCONNECTED => "the session is disconnected, so there is no screen to duplicate",
        _ => return None,
    })
}

/// Keeps the capture thread on the desktop that receives input (`Default`, or `Winlogon` for the
/// lock screen and UAC prompts): duplicating a desktop the thread is not attached to fails.
#[derive(Default)]
struct InputDesktop {
    /// The desktop this thread switched to, closed when it switches again.
    opened: Option<isize>,
    name: Option<String>,
}

const DESKTOP_ACCESS: DESKTOP_ACCESS_FLAGS = DESKTOP_ACCESS_FLAGS(
    DESKTOP_CREATEMENU.0
        | DESKTOP_CREATEWINDOW.0
        | DESKTOP_ENUMERATE.0
        | DESKTOP_HOOKCONTROL.0
        | DESKTOP_WRITEOBJECTS.0
        | DESKTOP_READOBJECTS.0
        | DESKTOP_SWITCHDESKTOP.0
        | GENERIC_WRITE.0,
);

impl InputDesktop {
    /// Whether input now goes to a desktop other than this thread's. `false` when the input
    /// desktop cannot be opened (this process may not open the secure desktop).
    fn changed(&self) -> bool {
        // SAFETY: the opened handle is closed before returning; the thread's own is not ours.
        unsafe {
            let Ok(input) = OpenInputDesktop(Default::default(), false, DESKTOP_ACCESS_FLAGS(0)) else {
                return false;
            };
            let input_name = desktop_name(input);
            let _ = CloseDesktop(input);
            let current = GetThreadDesktop(GetCurrentThreadId()).ok().and_then(desktop_name);
            input_name.is_some() && input_name != current
        }
    }

    /// Attaches this thread to the input desktop if it is not already on it.
    fn follow(&mut self) {
        // SAFETY: desktop handles are closed once this thread no longer uses them; this thread
        // owns no windows or hooks, which `SetThreadDesktop` requires.
        unsafe {
            let Ok(input) = OpenInputDesktop(Default::default(), false, DESKTOP_ACCESS) else {
                return;
            };
            let name = desktop_name(input);
            let current = GetThreadDesktop(GetCurrentThreadId()).ok().and_then(desktop_name);
            if name == current || SetThreadDesktop(input).is_err() {
                let _ = CloseDesktop(input);
                return;
            }
            if let Some(previous) = self.opened.replace(input.0 as isize) {
                let _ = CloseDesktop(HDESK(previous as *mut _));
            }
            tracing::info!(desktop = name.as_deref().unwrap_or("?"), "capture thread switched desktop");
            self.name = name;
        }
    }
}

fn desktop_name(desktop: HDESK) -> Option<String> {
    let mut buf = [0u16; 256];
    let mut needed = 0u32;
    // SAFETY: buffer and length describe `buf`.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(buf.as_mut_ptr().cast()),
            (buf.len() * 2) as u32,
            Some(&mut needed),
        )
        .ok()?;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

#[cfg(test)]
mod tests {
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED};

    use super::*;

    #[test]
    fn refusals_say_why_in_words() {
        // What the second viewer of a monitor got on 2026-10-22: 0x80070057.
        let text = format!("{:#}", refused(windows::core::Error::from(E_INVALIDARG)));
        assert!(text.starts_with("this monitor's Desktop Duplication is already in use in this host"), "{text}");
        assert!(text.contains("0x80070057"), "{text}");
        assert!(why_refused(DXGI_ERROR_NOT_CURRENTLY_AVAILABLE).unwrap().contains("four"));
        assert!(why_refused(E_ACCESSDENIED).is_some());
        let other = format!("{:#}", refused(windows::core::Error::from(DXGI_ERROR_WAIT_TIMEOUT)));
        assert!(other.starts_with("duplicate the output: "), "{other}");
    }

    #[test]
    fn rotated_outputs_are_turned_upright() {
        // A 1440x2560 portrait monitor arrives as the 2560x1440 panel scans it out.
        assert_eq!(
            upright(DXGI_MODE_ROTATION_ROTATE270, (2560, 1440)),
            (D3D11_VIDEO_PROCESSOR_ROTATION_270, (1440, 2560))
        );
        assert_eq!(
            upright(DXGI_MODE_ROTATION_ROTATE90, (2560, 1440)),
            (D3D11_VIDEO_PROCESSOR_ROTATION_90, (1440, 2560))
        );
        assert_eq!(
            upright(DXGI_MODE_ROTATION_ROTATE180, (5120, 2160)),
            (D3D11_VIDEO_PROCESSOR_ROTATION_180, (5120, 2160))
        );
        for plain in [DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED] {
            assert_eq!(upright(plain, (5120, 2160)), (D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY, (5120, 2160)));
        }
    }
}

/// Desktop Duplication + GPU conversion + hardware encoding of the real primary screen, 120 frames
/// per codec. A small window in the top-left corner changes colour every few milliseconds so the
/// screen keeps presenting (Desktop Duplication delivers nothing while the screen is still); the
/// rest of the screen is whatever is on it, so this is the common small-change case, not the
/// full-screen scroll of `gpu_encode_throughput`. Nothing leaves this machine. Run by hand:
///
/// `cargo test -p ar-host --release --lib dda_encode_throughput -- --ignored --nocapture`
///
/// `AR_CODECS` (`hevc,av1,h264`), `AR_FPS` (120), `AR_KBPS` (80000), `AR_FRAMES` (120), `AR_EDGE`
/// (8192: native size). "capture->encoded" runs from the moment `AcquireNextFrame` returned to the
/// moment `encode_raw` returned (GPU conversion, the hand-off through the slot, encoder submit);
/// "encode" is the `encode_raw` call alone.
#[cfg(test)]
mod bench {
    use windows::{
        Win32::{
            Foundation::{COLORREF, POINT, RECT},
            Graphics::Gdi::{
                CreateSolidBrush, DeleteObject, FillRect, GetDC, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint, ReleaseDC,
            },
            UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, WS_EX_NOACTIVATE,
                WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
            },
        },
        core::w,
    };

    use std::{sync::atomic::AtomicBool, thread::JoinHandle};

    use super::*;
    use crate::stream::encode::{Codec, FrameEncoder};

    /// How often `dda_desktop_switch_recovery` reads a frame back to check for black when the screen
    /// is not switching desktops.
    const HALF_SECOND: Duration = Duration::from_millis(500);

    fn env(key: &str, default: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| default.into())
    }

    /// A 96x96 top-most window whose colour changes every ~4 ms, until `stop`.
    fn animate(stop: Arc<AtomicBool>) -> JoinHandle<()> {
        std::thread::spawn(move || {
            // SAFETY: a window owned by this thread, drawn and destroyed on it.
            unsafe {
                let hwnd = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    w!("STATIC"),
                    w!(""),
                    WS_POPUP | WS_VISIBLE,
                    0,
                    0,
                    96,
                    96,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
                let rect = RECT { left: 0, top: 0, right: 96, bottom: 96 };
                let mut n = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    let mut msg = MSG::default();
                    while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                        DispatchMessageW(&msg);
                    }
                    let dc = GetDC(Some(hwnd));
                    let brush = CreateSolidBrush(COLORREF(n.wrapping_mul(0x0004_0812) & 0x00ff_ffff));
                    FillRect(dc, &rect, brush);
                    let _ = DeleteObject(brush.into());
                    ReleaseDC(Some(hwnd), dc);
                    n = n.wrapping_add(1);
                    std::thread::sleep(Duration::from_millis(4));
                }
                let _ = DestroyWindow(hwnd);
            }
        })
    }

    #[test]
    #[ignore]
    fn dda_encode_throughput() {
        let _ = tracing_subscriber::fmt().with_env_filter(env("RUST_LOG", "info")).with_test_writer().try_init();
        let fps: u32 = env("AR_FPS", "120").parse().unwrap();
        let kbps: u32 = env("AR_KBPS", "80000").parse().unwrap();
        let frames: usize = env("AR_FRAMES", "120").parse().unwrap();
        let edge: u32 = env("AR_EDGE", "8192").parse().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let animation = animate(stop.clone());
        // SAFETY: a plain query for the primary monitor's handle.
        let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        let slot = Arc::new(Slot::default());
        let limit = Arc::new(AtomicU32::new(edge));
        let capture = start(monitor, fps, limit.clone(), slot.clone()).unwrap();
        for codec in env("AR_CODECS", "hevc,av1,h264").split(',').filter_map(Codec::parse) {
            // As the stream does: scale down to what the codec's hardware encoder takes (H.264: 4096).
            let edge = edge.min(codec.max_long_edge());
            limit.store(edge, Ordering::Relaxed);
            let mut encoder = FrameEncoder::new(fps, kbps * 1000, edge).unwrap().with_codecs(&[codec]);
            let (mut latency, mut encode, mut outputs, mut bytes, mut actual) =
                (Vec::new(), Vec::new(), 0usize, 0usize, None);
            let mut size = (0, 0);
            // The first frame starts the encoder (60-170 ms); time the steady state after it.
            let mut started = None;
            let deadline = Instant::now() + Duration::from_secs(20);
            while latency.len() < frames && Instant::now() < deadline {
                let Some(Some(raw)) = slot.take(Duration::from_millis(200)) else { continue };
                if raw.width.max(raw.height) > edge {
                    continue; // converted before the new limit took effect
                }
                size = (raw.width, raw.height);
                let t = Instant::now();
                let mut ready = encoder.encode_raw(&raw).unwrap();
                let done = Instant::now();
                if started.is_none() {
                    started = Some(done);
                } else {
                    latency.push(done - raw.captured);
                    encode.push(done - t);
                }
                while let Some(out) = ready.take().or_else(|| encoder.poll().unwrap()) {
                    (actual, outputs, bytes) = (Some(out.codec), outputs + 1, bytes + out.data.len());
                }
            }
            let elapsed = started.map(|s| s.elapsed()).unwrap_or_default();
            assert!(!latency.is_empty(), "Desktop Duplication delivered no frames for {}", codec.name());
            latency.sort();
            encode.sort();
            let pct = |v: &[Duration], p: f64| v[((v.len() - 1) as f64 * p) as usize].as_secs_f64() * 1000.0;
            println!(
                "DDA {}x{} want {}: {} -> {} | {} frames, {outputs} out | capture->encoded p50 {:.2} ms p95 {:.2} ms max {:.2} ms | encode p50 {:.2} ms p95 {:.2} ms | {:.1} fps | {:.1} Mbit/s",
                size.0,
                size.1,
                codec.name(),
                encoder.backend_name(),
                actual.map(|c| c.name()).unwrap_or("none"),
                latency.len(),
                pct(&latency, 0.5),
                pct(&latency, 0.95),
                pct(&latency, 1.0),
                pct(&encode, 0.5),
                pct(&encode, 0.95),
                latency.len() as f64 / elapsed.as_secs_f64(),
                bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1e6,
            );
        }
        drop(capture);
        stop.store(true, Ordering::Relaxed);
        animation.join().unwrap();
    }

    /// Hands-on check that Desktop Duplication recovers from the desktop switches that cost it the
    /// output (lock -> unlock, a display mode change, an exclusive-fullscreen app entering/leaving):
    /// how long the viewer is frozen each time, and whether the first frame back is good or black.
    ///
    /// It captures the primary monitor for `AR_SECS` seconds and drains the slot. The 96x96 window
    /// keeps the screen presenting, so a long gap between delivered frames is a rebuild, not a still
    /// screen; each gap over `AR_GAP_MS` is printed with its wall-clock offset. Tracing is on, so the
    /// module's own `Desktop Duplication restarted` / `capture thread switched desktop` /
    /// `... unavailable; retrying` lines interleave and corroborate each gap. After every gap (and
    /// every ~0.5 s otherwise) the frame's Y plane is read back and its mean reported, so a frame
    /// that comes back black (mean near video-black 16, or 0) is caught even though nothing is
    /// displaying the captured image. Nothing leaves this machine. Run by hand, then operate the
    /// machine as the printed prompt says:
    ///
    /// `cargo test -p ar-host --release --lib dda_desktop_switch_recovery -- --ignored --nocapture`
    ///
    /// `AR_SECS` (90), `AR_GAP_MS` (250: report freezes at least this long), `AR_FPS` (60),
    /// `AR_EDGE` (1920), `AR_READBACK` (1; set 0 to skip the Y-plane read and only time gaps).
    #[test]
    #[ignore]
    fn dda_desktop_switch_recovery() {
        let _ = tracing_subscriber::fmt().with_env_filter(env("RUST_LOG", "info")).with_test_writer().try_init();
        let secs: u64 = env("AR_SECS", "90").parse().unwrap();
        let gap_ms: u64 = env("AR_GAP_MS", "250").parse().unwrap();
        let fps: u32 = env("AR_FPS", "60").parse().unwrap();
        let edge: u32 = env("AR_EDGE", "1920").parse().unwrap();
        let readback = env("AR_READBACK", "1") != "0";

        let stop = Arc::new(AtomicBool::new(false));
        let animation = animate(stop.clone());
        // SAFETY: a plain query for the primary monitor's handle.
        let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        let slot = Arc::new(Slot::default());
        let limit = Arc::new(AtomicU32::new(edge));
        let capture = start(monitor, fps, limit, slot.clone()).unwrap();

        println!(
            "Recording {secs}s. While it runs, do each at least twice and note the time you act:\n  \
             - lock the screen (Win+L), wait ~2 s, unlock;\n  \
             - change the display resolution and change it back;\n  \
             - enter an exclusive-fullscreen game and exit it.\n\
             Each freeze prints below as `t+<when> frozen <ms>`; a rebuild also logs `Desktop Duplication restarted`."
        );

        let begin = Instant::now();
        let deadline = begin + Duration::from_secs(secs);
        let mut last_frame: Option<Instant> = None;
        let mut frames = 0usize;
        let mut gaps: Vec<f64> = Vec::new();
        let mut black = 0usize;
        let mut sample_next = true;
        let mut last_sample = begin;
        let mut readback_failed = false;
        let mut closed = false;
        while Instant::now() < deadline {
            let Some(slotframe) = slot.take(Duration::from_millis(200)) else { continue };
            let Some(raw) = slotframe else {
                closed = true;
                println!("  t+{:6.2}s  capture closed (slot shut)", begin.elapsed().as_secs_f64());
                break;
            };
            let now = Instant::now();
            frames += 1;
            if let Some(prev) = last_frame {
                let gap = (now - prev).as_secs_f64() * 1000.0;
                if gap as u64 >= gap_ms {
                    let froze_at = (prev - begin).as_secs_f64();
                    println!("  t+{froze_at:6.2}s  frozen {gap:7.1} ms");
                    gaps.push(gap);
                    sample_next = true;
                }
            }
            last_frame = Some(now);

            if readback && !readback_failed && (sample_next || now.duration_since(last_sample) >= HALF_SECOND) {
                let after_gap = sample_next;
                sample_next = false;
                last_sample = now;
                if let Some(tex) = raw.gpu.as_ref() {
                    match crate::stream::capture::win::read_nv12(tex) {
                        Ok((y, _uv)) if !y.is_empty() => {
                            let mean = y.iter().map(|&b| u64::from(b)).sum::<u64>() / y.len() as u64;
                            if mean <= 20 {
                                black += 1;
                                println!(
                                    "  t+{:6.2}s  frame Y mean {mean} (near black){}",
                                    now.duration_since(begin).as_secs_f64(),
                                    if after_gap { ", first frame after a freeze" } else { "" }
                                );
                            } else if after_gap {
                                println!(
                                    "  t+{:6.2}s  recovered, frame Y mean {mean}",
                                    now.duration_since(begin).as_secs_f64()
                                );
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            readback_failed = true;
                            println!("  read-back unavailable ({e:#}); timing gaps only from here on");
                        }
                    }
                }
            }
        }
        drop(capture);
        stop.store(true, Ordering::Relaxed);
        animation.join().unwrap();

        let elapsed = begin.elapsed().as_secs_f64();
        let fps_avg = frames as f64 / elapsed;
        let longest = gaps.iter().copied().fold(0.0_f64, f64::max);
        println!(
            "Summary: {frames} frames in {elapsed:.1}s ({fps_avg:.1} fps avg); {} freezes >= {gap_ms} ms, longest {longest:.1} ms; {black} near-black samples{}.",
            gaps.len(),
            if closed { "; capture closed before the deadline" } else { "" }
        );
        assert!(!closed, "capture closed on its own before the recording ended");
        assert!(frames > 0, "Desktop Duplication delivered no frames");
    }

    /// Hands-on check on a machine with rotated (portrait) monitors: captures every monitor once
    /// through Desktop Duplication and once through Windows Graphics Capture (run it as the signed-in
    /// user: WGC is not available to SYSTEM), checks that each frame has its monitor's orientation,
    /// and writes the frame's luma as `AR_OUT/<dda|wgc>-<index>.pgm` to look at (an upside-down
    /// picture has the right orientation too, so look). Put something readable on each monitor first.
    ///
    /// `cargo test -p ar-host --lib every_monitor_comes_out_upright -- --ignored --nocapture`
    ///
    /// `AR_OUT` (the temp dir's `ar-upright`), `AR_EDGE` (1280).
    #[test]
    #[ignore]
    fn every_monitor_comes_out_upright() {
        let _ = tracing_subscriber::fmt().with_env_filter(env("RUST_LOG", "info")).with_test_writer().try_init();
        let out = std::path::PathBuf::from(env("AR_OUT", &std::env::temp_dir().join("ar-upright").to_string_lossy()));
        std::fs::create_dir_all(&out).unwrap();
        let edge: u32 = env("AR_EDGE", "1280").parse().unwrap();
        let mut wrong = Vec::new();
        for m in crate::stream::capture::monitors().unwrap() {
            let monitor = super::super::win::monitor(Some(m.index)).unwrap();
            for path in ["dda", "wgc"] {
                let slot = Arc::new(Slot::default());
                let limit = Arc::new(AtomicU32::new(edge));
                // Kept running until its frame is read back.
                let _capture: Box<dyn std::any::Any> = match path {
                    "dda" => Box::new(start(monitor, 30, limit, slot.clone()).unwrap()),
                    _ => Box::new(super::super::win::start(monitor, 30, limit, slot.clone(), false).unwrap()),
                };
                let raw = (0..50).find_map(|_| slot.take(Duration::from_millis(100)).flatten()).expect("no frame");
                let (y, _) = super::super::win::read_nv12(raw.gpu.as_deref().expect("a GPU frame")).unwrap();
                let file = out.join(format!("{path}-{}.pgm", m.index));
                let mut pgm = format!("P5\n{} {}\n255\n", raw.width, raw.height).into_bytes();
                pgm.extend_from_slice(&y);
                std::fs::write(&file, pgm).unwrap();
                let upright = (raw.width > raw.height) == (m.width > m.height);
                println!(
                    "monitor {} at ({}, {}) {}x{}{}: {path} frame {}x{} {} -> {}",
                    m.index,
                    m.x,
                    m.y,
                    m.width,
                    m.height,
                    if m.primary { " primary" } else { "" },
                    raw.width,
                    raw.height,
                    if upright { "upright" } else { "ON ITS SIDE" },
                    file.display()
                );
                if !upright {
                    wrong.push(format!("{path} monitor {}", m.index));
                }
            }
        }
        assert!(wrong.is_empty(), "frames on their side: {wrong:?}");
    }

    /// Two viewers of the primary monitor at once, as two connections to one host ask for it: both
    /// get frames at their own size, the one left goes on getting frames when the other leaves, and a
    /// viewer that comes right after the last one left starts a new duplication. Before viewers shared
    /// one, the second `start` failed with E_INVALIDARG (`DuplicateOutput` in a process that already
    /// duplicates the output). Nothing leaves this machine. Run by hand:
    ///
    /// `cargo test -p ar-host --lib two_viewers_of_one_monitor -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn two_viewers_of_one_monitor_both_get_frames() {
        let _ = tracing_subscriber::fmt().with_env_filter(env("RUST_LOG", "info")).with_test_writer().try_init();
        let stop = Arc::new(AtomicBool::new(false));
        let animation = animate(stop.clone());
        // SAFETY: a plain query for the primary monitor's handle.
        let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        let viewer = |edge: u32| {
            let slot = Arc::new(Slot::default());
            start(monitor, 30, Arc::new(AtomicU32::new(edge)), slot.clone()).map(|capture| (capture, slot))
        };
        // A frame captured after `since`.
        let frame = |slot: &Slot, since: Instant| {
            (0..100)
                .find_map(|_| slot.take(Duration::from_millis(100)).flatten().filter(|raw| raw.captured > since))
                .expect("no frame within 10 s")
        };
        let begin = Instant::now();
        let (first, first_slot) = viewer(1280).unwrap();
        let (second, second_slot) = viewer(640).expect("a second viewer of the same monitor");
        let (a, b) = (frame(&first_slot, begin), frame(&second_slot, begin));
        println!("first viewer {}x{}, second viewer {}x{}", a.width, a.height, b.width, b.height);
        assert!(a.width.max(a.height) <= 1280 && b.width.max(b.height) <= 640);
        assert!(a.width > b.width, "each viewer gets its own size");

        drop(first);
        let left = Instant::now();
        let c = frame(&second_slot, left);
        println!("after the first left, the second still gets {}x{}", c.width, c.height);

        drop(second);
        let again = Instant::now();
        let (third, third_slot) = viewer(640).expect("a viewer right after the last one left");
        frame(&third_slot, again);
        println!(
            "a viewer that came right after the last one left had its first frame {:.0} ms later",
            again.elapsed().as_secs_f64() * 1000.0
        );
        drop(third);
        stop.store(true, Ordering::Relaxed);
        animation.join().unwrap();
    }
}
