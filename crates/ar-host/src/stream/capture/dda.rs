//! DXGI Desktop Duplication on the same D3D11 device and GPU conversion as `win.rs`: frames come out
//! as the same NV12 `Raw { gpu }` textures. Windows Graphics Capture cannot see the secure desktop
//! (lock screen, UAC prompts); Desktop Duplication can, from a thread attached to that desktop in a
//! process allowed to open it. This thread follows the input desktop and rebuilds the duplication
//! whenever DXGI reports it lost (desktop switch, mode change, a full-screen app taking the output).
//!
//! Not the default yet: `AI_REMOTE_CAPTURE=dda` selects it (see `capture::start`). The pointer is not
//! in the picture (Desktop Duplication reports it separately).
//! TODO: draw the pointer from `DXGI_OUTDUPL_FRAME_INFO` / `GetFramePointerShape`.
//! TODO: rotated outputs arrive unrotated; rotate in the video processor.
//!
//! Follows RustDesk's `libs/scrap/src/dxgi/mod.rs` (acquire / release, access lost) and
//! `src/server/video_service.rs` with `platform/windows.cc` (`inputDesktopSelected` /
//! `selectInputDesktop`) for the desktop switch.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use windows::{
    Win32::{
        Foundation::{GENERIC_WRITE, HANDLE},
        Graphics::{
            Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Texture2D},
            Dxgi::{
                Common::{DXGI_MODE_ROTATION_IDENTITY, DXGI_MODE_ROTATION_UNSPECIFIED},
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
    core::Interface,
};

use super::{
    Raw, Slot,
    win::{Converter, Gpu},
};

/// How often a still screen checks for a switch of the input desktop.
const DESKTOP_CHECK: Duration = Duration::from_millis(100);
/// Pause between attempts while the output cannot be duplicated (e.g. the secure desktop is up and
/// this process may not open it).
const RETRY: Duration = Duration::from_millis(500);

/// A running Desktop Duplication capture; stops when dropped.
pub(super) struct DdaCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for DdaCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts duplicating `monitor`. Fails when the first duplication fails; later losses (desktop
/// switches, mode changes) are retried on the capture thread until the capture is dropped.
/// Every frame Desktop Duplication delivers is converted (at most one per refresh); the encode
/// loop's pacer keeps the requested rate. Dropping frames here would lose the last change before
/// the screen goes still, since a still screen delivers nothing more.
pub(super) fn start(
    monitor: HMONITOR,
    max_fps: u32,
    max_long_edge: Arc<AtomicU32>,
    slot: Arc<Slot>,
) -> Result<DdaCapture> {
    let gpu = Gpu::for_monitor(monitor)?;
    let fps = max_fps.clamp(1, 240);
    let stop = Arc::new(AtomicBool::new(false));
    let (first_tx, first_rx) = mpsc::channel();
    // HMONITOR is a plain handle value; it crosses the thread boundary as an integer.
    let raw_monitor = monitor.0 as isize;
    let thread = {
        let stop = stop.clone();
        std::thread::Builder::new().name("ai-remote-dda".into()).spawn(move || {
            let monitor = HMONITOR(raw_monitor as *mut _);
            run(&gpu, monitor, fps, &max_long_edge, &slot, &stop, first_tx);
        })?
    };
    let capture = DdaCapture { stop, thread: Some(thread) };
    match first_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(capture),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(anyhow!("Desktop Duplication did not start")),
    }
}

/// A converter and the (input, output) sizes it was built for.
type SizedConverter = (Converter, (u32, u32), (u32, u32));

fn run(
    gpu: &Arc<Gpu>,
    monitor: HMONITOR,
    fps: u32,
    max_long_edge: &AtomicU32,
    slot: &Slot,
    stop: &AtomicBool,
    first: mpsc::Sender<Result<()>>,
) {
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
    let mut converter: Option<SizedConverter> = None;
    let mut failing = false;
    let mut desktop_checked = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        let Some((output, dup)) = duplication.as_ref() else {
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
                    std::thread::sleep(RETRY);
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
        let converted = if info.LastPresentTime == 0 {
            Ok(None)
        } else {
            resource.as_ref().context("no desktop image").and_then(|r| {
                let texture: ID3D11Texture2D = r.cast()?;
                let mut desc = D3D11_TEXTURE2D_DESC::default();
                // SAFETY: a descriptor query on a live texture.
                unsafe { texture.GetDesc(&mut desc) };
                let input = (desc.Width, desc.Height);
                let output =
                    crate::stream::encode::stream_size(input.0, input.1, max_long_edge.load(Ordering::Relaxed));
                if converter.as_ref().is_none_or(|(_, i, o)| (*i, *o) != (input, output)) {
                    // Cleared first: a converter that cannot be built for this size stops the capture.
                    converter = None;
                    converter = Some((Converter::new(gpu.clone(), input, output, fps)?, input, output));
                }
                converter.as_mut().map_or(Ok(None), |(c, _, _)| c.convert(&texture))
            })
        };
        drop(resource);
        // SAFETY: the frame acquired above; the conversion is already queued on the same device, and
        // releasing is ordered after it.
        if unsafe { dup.ReleaseFrame() }.is_err() {
            duplication = None;
        }
        match converted {
            Ok(Some(frame)) => {
                slot.put(Raw { bgra: Vec::new(), width: frame.width, height: frame.height, captured, gpu: Some(frame) })
            }
            Ok(None) => {}
            Err(e) if converter.is_none() => {
                tracing::warn!(error = %format!("{e:#}"), "GPU frame conversion unavailable");
                slot.close();
                return;
            }
            Err(e) => tracing::warn!(error = %format!("{e:#}"), "live view frame conversion"),
        }
    }
}

/// Duplicates the output that shows `monitor` on the capture device's adapter.
fn duplicate(gpu: &Gpu, monitor: HMONITOR) -> Result<(IDXGIOutput1, IDXGIOutputDuplication)> {
    // SAFETY: DXGI enumeration and duplication with owned COM results only.
    unsafe {
        let adapter = gpu.device.cast::<IDXGIDevice>()?.GetAdapter()?;
        for i in 0.. {
            let output = adapter.EnumOutputs(i).context("the monitor is not driven by the capture GPU")?;
            if output.GetDesc().is_ok_and(|d| d.Monitor == monitor) {
                let output = output.cast::<IDXGIOutput1>()?;
                let duplication = output.DuplicateOutput(&gpu.device).context("duplicate the output")?;
                let rotation = duplication.GetDesc().Rotation;
                if rotation != DXGI_MODE_ROTATION_IDENTITY && rotation != DXGI_MODE_ROTATION_UNSPECIFIED {
                    tracing::warn!(?rotation, "rotated display: Desktop Duplication frames are not rotated yet");
                }
                return Ok((output, duplication));
            }
        }
        unreachable!()
    }
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

    use super::*;
    use crate::stream::encode::{Codec, FrameEncoder};

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
}
