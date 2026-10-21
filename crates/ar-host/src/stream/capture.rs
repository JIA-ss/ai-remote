//! Screen capture for the live view: Windows Graphics Capture on our own D3D11 device with GPU
//! scaling and NV12 conversion (`capture/win.rs`; the older `windows-capture` BGRA path remains as
//! a fallback), and event-driven ScreenCaptureKit on macOS. Frames arrive only when the screen
//! changes; the newest one waits in a slot for the encoder, so a slow encoder or network skips
//! frames before they are encoded instead of queueing them.

use std::{
    sync::{Arc, Condvar, Mutex, atomic::AtomicU32},
    time::Duration,
};

use anyhow::{Result, anyhow};

/// A captured frame. Windows keeps an NV12 texture at the stream size on the GPU (`bgra` empty), or
/// tightly packed BGRA on the fallback path; macOS retains the native, immutable IOSurface-backed
/// pixel buffer for VideoToolbox instead of mapping/copying pixels on the CPU.
pub struct Raw {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// When the picture was captured: the start of the latency a viewer sees.
    pub captured: std::time::Instant,
    #[cfg(target_os = "macos")]
    pub pixel_buffer: Option<apple_cf::cv::CVPixelBuffer>,
    #[cfg(windows)]
    pub gpu: Option<Arc<win::GpuTexture>>,
}

/// The newest frame not yet taken, shared by the capture thread and the encoder.
#[derive(Default)]
pub struct Slot {
    frame: Mutex<SlotState>,
    ready: Condvar,
}

#[derive(Default)]
struct SlotState {
    frame: Option<Raw>,
    closed: bool,
    #[cfg(target_os = "macos")]
    error: Option<String>,
}

impl Slot {
    fn put(&self, frame: Raw) {
        let mut state = self.frame.lock().unwrap();
        if state.closed {
            return;
        }
        let previous = state.frame.replace(frame);
        self.ready.notify_one();
        // Release the superseded native surface outside the lock; there is never a frame queue.
        drop(state);
        drop(previous);
    }

    pub fn close(&self) {
        let mut state = self.frame.lock().unwrap();
        state.closed = true;
        let previous = state.frame.take();
        self.ready.notify_all();
        drop(state);
        drop(previous);
    }

    #[cfg(target_os = "macos")]
    fn fail(&self, error: String) {
        let mut state = self.frame.lock().unwrap();
        if state.closed {
            return;
        }
        tracing::warn!(%error, "macOS live capture stopped");
        state.error = Some(error);
        state.closed = true;
        let previous = state.frame.take();
        self.ready.notify_all();
        drop(state);
        drop(previous);
    }

    #[cfg(target_os = "macos")]
    fn is_closed(&self) -> bool {
        self.frame.lock().unwrap().closed
    }

    /// A successful native start is not sufficient: retain a complete first frame before replying.
    #[cfg(target_os = "macos")]
    fn wait_ready(&self, timeout: Duration) -> Result<()> {
        let state = self.frame.lock().unwrap();
        let (state, _) = self.ready.wait_timeout_while(state, timeout, |s| s.frame.is_none() && !s.closed).unwrap();
        if state.closed {
            return Err(anyhow!(
                "{}",
                state.error.as_deref().unwrap_or("screen capture stopped before its first frame")
            ));
        }
        if state.frame.is_none() {
            return Err(anyhow!(
                "ScreenCaptureKit produced no complete frame; check the display and Screen Recording permission, then reconnect"
            ));
        }
        Ok(())
    }

    /// Waits for the next frame; `None` once closed. Times out to let the caller check for a stop.
    pub fn take(&self, timeout: Duration) -> Option<Option<Raw>> {
        let guard = self.frame.lock().unwrap();
        let (mut guard, _) = self.ready.wait_timeout_while(guard, timeout, |s| s.frame.is_none() && !s.closed).unwrap();
        if guard.closed {
            return None;
        }
        Some(guard.frame.take())
    }
}

/// A running capture; stops when dropped.
pub struct Capture {
    #[cfg(windows)]
    control: Option<windows_capture::capture::CaptureControl<legacy::Handler, legacy::Error>>,
    #[cfg(windows)]
    native: Option<win::NativeCapture>,
    #[cfg(windows)]
    pattern: Option<win::TestPattern>,
    #[cfg(windows)]
    dda: Option<dda::DdaCapture>,
    #[cfg(target_os = "macos")]
    native: Option<mac::NativeCapture>,
    #[cfg(target_os = "macos")]
    pattern: Option<mac::TestPattern>,
    slot: Arc<Slot>,
    geometry: Option<(i32, i32, u32, u32)>,
    max_long_edge: Arc<AtomicU32>,
    /// Whether the pointer is drawn into the picture.
    pointer_drawn: bool,
}

impl Capture {
    pub fn slot(&self) -> Arc<Slot> {
        self.slot.clone()
    }

    /// The long edge frames are scaled to. Windows' GPU path reads it for every frame, so the
    /// encoder can lower it when the codec it ends up with has a smaller size limit.
    pub fn max_long_edge(&self) -> Arc<AtomicU32> {
        self.max_long_edge.clone()
    }

    /// Whether the pointer is in the picture: always unless the caller asked otherwise and the
    /// capture path can leave it out (Windows Graphics Capture, Desktop Duplication).
    pub fn pointer_drawn(&self) -> bool {
        self.pointer_drawn
    }

    /// Point coordinates for the exact monitor chosen when capture started, not Retina pixels.
    pub fn geometry(&self) -> Option<(i32, i32, u32, u32)> {
        self.geometry
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.slot.close();
        #[cfg(windows)]
        if let Some(control) = self.control.take() {
            let _ = control.stop();
        }
        #[cfg(any(windows, target_os = "macos"))]
        if let Some(native) = self.native.take() {
            drop(native);
        }
        #[cfg(any(windows, target_os = "macos"))]
        drop(self.pattern.take());
        #[cfg(windows)]
        drop(self.dda.take());
    }
}

/// Monitors as (index, width, height, is primary), in the capture API's order.
pub fn monitors() -> Result<Vec<(usize, u32, u32, bool)>> {
    #[cfg(windows)]
    {
        use windows_capture::monitor::Monitor;
        let primary = Monitor::primary().ok().and_then(|m| m.name().ok());
        let list = Monitor::enumerate().map_err(|e| anyhow!("list monitors: {e}"))?;
        Ok(list
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let is_primary = primary.is_some() && m.name().ok() == primary;
                (i, m.width().unwrap_or(0), m.height().unwrap_or(0), is_primary)
            })
            .collect())
    }
    #[cfg(target_os = "macos")]
    {
        xcap::Monitor::all()?
            .iter()
            .enumerate()
            .map(|(i, m)| Ok((i, m.width()?, m.height()?, m.is_primary()?)))
            .collect()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    Err(anyhow!("live view is not available on this platform yet"))
}

/// Monitor `index`'s refresh rate in Hz (`None`: the primary one), when the platform reports it.
/// Frames cannot come faster, so it bounds the stream's frame rate and the encoder's budget per frame.
pub fn refresh_hz(index: Option<usize>) -> Option<u32> {
    #[cfg(windows)]
    {
        use windows_capture::monitor::Monitor;
        let monitor = match index {
            Some(i) => Monitor::from_index(i + 1),
            None => Monitor::primary(),
        };
        // 0 and 1 mean "the hardware default" rather than a rate.
        monitor.ok()?.refresh_rate().ok().filter(|&hz| hz > 1)
    }
    #[cfg(target_os = "macos")]
    {
        mac::refresh_hz(index)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = index;
        None
    }
}

/// Where monitor `index` (`None`: the primary one) sits on the screen, as (x, y, width, height) in
/// screen coordinates, the space the mouse tool takes with `space: "screen"`.
pub fn geometry(index: Option<usize>) -> Option<(i32, i32, u32, u32)> {
    let all = xcap::Monitor::all().ok()?;
    let m = match index {
        Some(i) => all.get(i)?,
        None => all.iter().find(|m| m.is_primary().unwrap_or(false)).or(all.first())?,
    };
    Some((m.x().ok()?, m.y().ok()?, m.width().ok()?, m.height().ok()?))
}

/// Starts capturing monitor `index` (`None`: the primary one) at most `max_fps` times a second.
/// Both platforms scale on the GPU to the requested long edge before handing frames to the encoder.
/// `draw_pointer: false` leaves the pointer out where the capture path allows it (see
/// [`Capture::pointer_drawn`]); the viewer then draws it from `pointer`.
pub fn start(index: Option<usize>, max_fps: u32, max_long_edge: u32, draw_pointer: bool) -> Result<Capture> {
    let slot = Arc::new(Slot::default());
    let edge = Arc::new(AtomicU32::new(max_long_edge));
    #[cfg(windows)]
    {
        if let Some(size) = pattern::test_pattern_size() {
            let pattern = win::start_test_pattern(size, max_fps, edge.clone(), slot.clone())?;
            let geometry = Some((0, 0, size.0, size.1));
            return Ok(Capture {
                control: None,
                native: None,
                pattern: Some(pattern),
                dda: None,
                slot,
                geometry,
                max_long_edge: edge,
                // The synthetic picture has no pointer in it.
                pointer_drawn: false,
            });
        }
        let monitor = win::monitor(index)?;
        // Desktop Duplication instead of Windows Graphics Capture where the secure desktop (lock
        // screen, UAC) must be seen: a host running as SYSTEM (service mode) can open it and WGC
        // cannot show it, and a host already looking at a secure desktop. `AI_REMOTE_CAPTURE=dda`
        // or `=wgc` forces either way (testing). Falls through to WGC on failure.
        if want_dda() {
            match dda::start(monitor, max_fps, edge.clone(), slot.clone()) {
                Ok(dda) => {
                    return Ok(Capture {
                        control: None,
                        native: None,
                        pattern: None,
                        dda: Some(dda),
                        slot,
                        geometry: geometry(index),
                        max_long_edge: edge,
                        // Desktop Duplication does not draw it (yet).
                        pointer_drawn: false,
                    });
                }
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "Desktop Duplication unavailable; using WGC");
                }
            }
        }
        // The GPU path is the normal one; the CPU path keeps a live view where the GPU has no
        // video processor (a basic display driver, some virtual adapters).
        match win::start(monitor, max_fps, edge.clone(), slot.clone(), draw_pointer) {
            Ok(native) => Ok(Capture {
                control: None,
                native: Some(native),
                pattern: None,
                dda: None,
                slot,
                geometry: geometry(index),
                max_long_edge: edge,
                pointer_drawn: draw_pointer,
            }),
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "GPU screen capture unavailable; using the CPU path");
                let control = legacy::start(index, max_fps, slot.clone())?;
                Ok(Capture {
                    control: Some(control),
                    native: None,
                    pattern: None,
                    dda: None,
                    slot,
                    geometry: geometry(index),
                    max_long_edge: edge,
                    pointer_drawn: true,
                })
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(size) = pattern::test_pattern_size() {
            let pattern = mac::start_test_pattern(size, max_fps, slot.clone())?;
            let geometry = Some((0, 0, size.0, size.1));
            // The synthetic picture has no pointer in it.
            return Ok(Capture {
                native: None,
                pattern: Some(pattern),
                slot,
                geometry,
                max_long_edge: edge,
                pointer_drawn: false,
            });
        }
        // ScreenCaptureKit composites the pointer itself unless asked not to.
        let native = mac::start(index, max_fps, max_long_edge, slot.clone(), draw_pointer)?;
        let geometry = Some(native.geometry());
        Ok(Capture {
            native: Some(native),
            pattern: None,
            slot,
            geometry,
            max_long_edge: edge,
            pointer_drawn: draw_pointer,
        })
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (index, max_fps, max_long_edge, slot, draw_pointer);
        Err(anyhow!("live view is not available on this platform yet"))
    }
}

/// Whether to capture with Desktop Duplication (see [`start`]).
#[cfg(windows)]
fn want_dda() -> bool {
    match std::env::var("AI_REMOTE_CAPTURE") {
        Ok(v) if v.eq_ignore_ascii_case("dda") => return true,
        Ok(v) if v.eq_ignore_ascii_case("wgc") => return false,
        _ => {}
    }
    crate::platform::runs_as_system() || crate::platform::session_state().secure_desktop
}

#[cfg(windows)]
mod dda;
#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
pub mod win;

/// The synthetic test picture both platforms can stream instead of the screen (see
/// [`pattern::test_pattern_size`]).
#[cfg(any(windows, target_os = "macos"))]
pub(crate) mod pattern {
    /// Text-like glyph rows, window bars and a photo-like gradient with grain, BGRA.
    pub fn desktop_like(width: u32, height: u32) -> Vec<u8> {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rand = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (w, h) = (width as usize, height as usize);
        let mut px = vec![255u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                if (y / 540) % 3 == 1 && x > w / 2 {
                    let n = (rand() & 31) as u8;
                    px[i] = ((x * 255 / w) as u8).wrapping_add(n);
                    px[i + 1] = ((y * 255 / h) as u8).wrapping_add(n);
                    px[i + 2] = (((x + y) * 127 / (w + h)) as u8).wrapping_add(n);
                } else if y % 540 < 28 {
                    px[i..i + 3].copy_from_slice(&[0x40, 0x30, 0x28]);
                } else {
                    // Glyph cells of 8x14 with a 4-row gap; every sixth cell is blank.
                    let (cx, cy, gx, gy) = (x / 8, y / 18, x % 8, y % 18);
                    let cell = (cx as u64).wrapping_mul(0x2545_f491_4f6c_dd1d) ^ (cy as u64).wrapping_mul(0x9e37_79b9);
                    if gy < 14 && gx < 7 && !cell.is_multiple_of(6) && (cell >> (gx * 7 + gy % 7)) & 1 == 1 {
                        px[i..i + 3].copy_from_slice(&[0x20, 0x20, 0x20]);
                    }
                }
                px[i + 3] = 255;
            }
        }
        px
    }

    /// Where a `height`-row view of a `2 * height`-row picture starts after scrolling `rows`: down
    /// to the end and back up, never jumping. A wrap back to the top would change the whole picture
    /// at once, like a scene cut, and its large frame every few seconds skewed bitrate measurements.
    pub fn scroll_top(rows: u32, height: u32) -> u32 {
        let phase = rows % (2 * height);
        if phase <= height { phase } else { 2 * height - phase }
    }

    /// `AI_REMOTE_TEST_PATTERN=<width>x<height>` on the host replaces the screen with a synthetic,
    /// desktop-like picture of that size that scrolls 7 rows a frame at the requested rate: every
    /// frame changes everywhere, so the stream runs at its frame rate and bitrate cap. Everything
    /// after capture (GPU conversion, hardware encoding, transport, the viewer's decoder) is the
    /// real path. For repeatable end-to-end measurements (`tools/stream-e2e.py --pattern`); no
    /// screen is read. `<width>x<height>,still` shows the first picture once and then nothing
    /// changes, as on an idle desktop (`--pattern 1920x1080,still`); `,stop=<seconds>` scrolls that
    /// long, then stands still.
    pub fn test_pattern_size() -> Option<(u32, u32)> {
        let value = std::env::var("AI_REMOTE_TEST_PATTERN").ok()?;
        let value = value.split(',').next()?;
        let (w, h) = value.split_once('x')?;
        let size = (w.trim().parse().ok()?, h.trim().parse().ok()?);
        (size.0 >= 64 && size.1 >= 64 && size.0 <= 8192 && size.1 <= 8192).then_some(size)
    }

    /// Frames to show at `fps` before the test pattern stands still, if it ever does.
    pub fn still_after(fps: u32) -> Option<u32> {
        std::env::var("AI_REMOTE_TEST_PATTERN").ok().and_then(|v| {
            v.split(',').skip(1).find_map(|p| match p.trim() {
                "still" => Some(1),
                p => p.strip_prefix("stop=")?.parse::<f64>().ok().map(|s| ((s * f64::from(fps)) as u32).max(1)),
            })
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn scrolling_bounces_instead_of_wrapping() {
            assert_eq!(scroll_top(0, 10), 0);
            assert_eq!(scroll_top(10, 10), 10);
            assert_eq!(scroll_top(15, 10), 5);
            assert_eq!(scroll_top(20, 10), 0);
        }

        #[test]
        fn the_picture_is_opaque_bgra_of_the_requested_size() {
            let px = desktop_like(64, 70);
            assert_eq!(px.len(), 64 * 70 * 4);
            assert!(px.chunks_exact(4).all(|p| p[3] == 255));
        }
    }
}

/// The CPU path through `windows-capture`: BGRA read back into memory for the software encoder.
#[cfg(windows)]
mod legacy {
    use std::{sync::Arc, time::Duration};

    use anyhow::{Result, anyhow};
    use windows_capture::{
        capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
        frame::Frame,
        graphics_capture_api::InternalCaptureControl,
        monitor::Monitor,
        settings::{
            ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, MinimumUpdateIntervalSettings,
            SecondaryWindowSettings, Settings,
        },
    };

    use super::{Raw, Slot};

    pub type Error = Box<dyn std::error::Error + Send + Sync>;

    pub struct Handler {
        slot: Arc<Slot>,
        scratch: Vec<u8>,
    }

    impl GraphicsCaptureApiHandler for Handler {
        type Flags = Arc<Slot>;
        type Error = Error;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            Ok(Self { slot: ctx.flags, scratch: Vec::new() })
        }

        fn on_frame_arrived(&mut self, frame: &mut Frame, _control: InternalCaptureControl) -> Result<(), Self::Error> {
            let buffer = frame.buffer()?;
            let (width, height) = (buffer.width(), buffer.height());
            let bgra = buffer.as_nopadding_buffer(&mut self.scratch).to_vec();
            self.slot.put(Raw { bgra, width, height, captured: std::time::Instant::now(), gpu: None });
            Ok(())
        }

        fn on_closed(&mut self) -> Result<(), Self::Error> {
            self.slot.close();
            Ok(())
        }
    }

    pub fn start(index: Option<usize>, max_fps: u32, slot: Arc<Slot>) -> Result<CaptureControl<Handler, Error>> {
        let monitor = match index {
            Some(i) => Monitor::from_index(i + 1),
            None => Monitor::primary(),
        }
        .map_err(|e| anyhow!("no such monitor: {e}"))?;
        let settings = Settings::new(
            monitor,
            CursorCaptureSettings::WithCursor,
            // The yellow capture border would be on the person's screen the whole time.
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Custom(Duration::from_millis(1000 / max_fps.clamp(1, 240) as u64)),
            DirtyRegionSettings::Default,
            ColorFormat::Bgra8,
            slot,
        );
        Handler::start_free_threaded(settings).map_err(|e| anyhow!("start screen capture: {e}"))
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::stream::encode::FrameEncoder;

    /// Captures and encodes the real screen for a few seconds and reports the rate (run by hand:
    /// `cargo test -p ar-host capture_and_encode -- --ignored --nocapture`). Nothing leaves this
    /// machine; the stream is decoded in memory only.
    #[test]
    #[ignore]
    fn capture_and_encode_the_screen() {
        let capture = start(None, 30, 1920, true).unwrap();
        let slot = capture.slot();
        let mut enc = FrameEncoder::new(30, 6_000_000, 1920).unwrap();
        let mut dec = openh264::decoder::Decoder::new().unwrap();
        let (mut frames, mut bytes, mut decoded, mut encode_time) = (0, 0usize, 0, Duration::ZERO);
        let started = Instant::now();
        let mut size = (0, 0);
        while started.elapsed() < Duration::from_secs(4) {
            let Some(Some(raw)) = slot.take(Duration::from_millis(200)) else { continue };
            let t = Instant::now();
            if let Some(e) = enc.encode(&raw.bgra, raw.width, raw.height, raw.width * 4).unwrap() {
                encode_time += t.elapsed();
                frames += 1;
                bytes += e.data.len();
                size = (e.width, e.height);
                if dec.decode(&e.data).unwrap().is_some() {
                    decoded += 1;
                }
            }
        }
        let secs = started.elapsed().as_secs_f64();
        println!(
            "{frames} frames ({decoded} decoded) at {}x{} in {secs:.1} s: {:.1} fps, {:.0} kbit/s, {:.1} ms per encode",
            size.0,
            size.1,
            frames as f64 / secs,
            bytes as f64 * 8.0 / secs / 1000.0,
            encode_time.as_secs_f64() * 1000.0 / frames.max(1) as f64
        );
        assert!(frames > 0 && decoded == frames);
    }
}

#[cfg(test)]
mod slot_tests {
    use super::*;

    fn raw(value: u8) -> Raw {
        Raw {
            bgra: vec![value; 16],
            width: 2,
            height: 2,
            captured: std::time::Instant::now(),
            #[cfg(target_os = "macos")]
            pixel_buffer: None,
            #[cfg(windows)]
            gpu: None,
        }
    }

    #[test]
    fn a_slow_consumer_gets_only_the_newest_frame() {
        let slot = Slot::default();
        for value in 0..100 {
            slot.put(raw(value));
        }
        let next = slot.take(Duration::ZERO).unwrap().unwrap();
        assert_eq!(next.bgra, vec![99; 16]);
        assert!(slot.take(Duration::ZERO).unwrap().is_none());
    }

    #[test]
    fn close_discards_pending_frames_and_refuses_late_callbacks() {
        let slot = Slot::default();
        slot.put(raw(1));
        slot.close();
        assert!(slot.frame.lock().unwrap().frame.is_none());
        slot.put(raw(2));
        assert!(slot.frame.lock().unwrap().frame.is_none());
        assert!(slot.take(Duration::ZERO).is_none());
    }

    #[test]
    fn close_wakes_a_waiting_encoder() {
        let slot = Arc::new(Slot::default());
        let waiting = slot.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            tx.send(waiting.take(Duration::from_secs(30)).is_none()).unwrap();
        });
        slot.close();
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        thread.join().unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn first_frame_handshake_preserves_frame_and_propagates_capture_errors() {
        let slot = Slot::default();
        assert!(slot.wait_ready(Duration::ZERO).is_err());
        slot.put(raw(7));
        slot.wait_ready(Duration::ZERO).unwrap();
        assert_eq!(slot.take(Duration::ZERO).unwrap().unwrap().bgra, vec![7; 16]);
        slot.put(raw(8));
        slot.fail("capture permission revoked".into());
        assert!(slot.frame.lock().unwrap().frame.is_none());
        assert_eq!(slot.wait_ready(Duration::ZERO).unwrap_err().to_string(), "capture permission revoked");
    }
}
