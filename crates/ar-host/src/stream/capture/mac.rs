//! ScreenCaptureKit owns capture, scaling and cursor compositing. The callback only retains an
//! immutable CoreVideo buffer into the newest-frame slot; no screenshot, CPU mapping or pixel copy.
//! Based on screencapturekit 11's basic-capture example, SCStream teardown and CMSampleBufferSCExt,
//! and Apple's "Capturing screen content in macOS" complete-frame/IOSurface contract.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use screencapturekit::{cm::SCFrameStatus, prelude::*};

use super::{Raw, Slot};
use crate::platform::SessionState;

type Geometry = (i32, i32, u32, u32);
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const BGRA: u32 = u32::from_be_bytes(*b"BGRA");
/// ProMotion displays refresh at 120 Hz; leave room for external 240 Hz panels.
const MAX_FPS: u32 = 240;
/// HEVC hardware encodes 8K; `encode::plan` lowers this for H.264 (4096).
const MAX_LONG_EDGE: u32 = 8192;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayCopyDisplayMode(display: u32) -> *mut std::ffi::c_void;
    fn CGDisplayModeGetRefreshRate(mode: *const std::ffi::c_void) -> f64;
    fn CGDisplayModeRelease(mode: *mut std::ffi::c_void);
}

/// The monitor to stream: `index` into the public monitor list, or the primary one.
fn monitor(index: Option<usize>) -> Result<xcap::Monitor> {
    // Preserve the public system_info/monitor index order. xcap reads only display metadata here.
    let monitors = xcap::Monitor::all().context("list display metadata")?;
    match index {
        Some(index) => monitors.get(index),
        None => monitors.iter().find(|m| m.is_primary().unwrap_or(false)).or(monitors.first()),
    }
    .cloned()
    .ok_or_else(|| anyhow!("monitor not found"))
}

/// The display's refresh rate from its current CGDisplayMode. Apple reports 0 for displays whose
/// rate it does not know (some external and virtual displays): `None` then.
pub(super) fn refresh_hz(index: Option<usize>) -> Option<u32> {
    let id = monitor(index).ok()?.id().ok()?;
    // SAFETY: a plain query; the copied mode object is released below, once, on every path.
    let hz = unsafe {
        let mode = CGDisplayCopyDisplayMode(id);
        if mode.is_null() {
            return None;
        }
        let hz = CGDisplayModeGetRefreshRate(mode);
        CGDisplayModeRelease(mode);
        hz
    };
    (hz.is_finite() && hz > 1.0).then(|| hz.round() as u32)
}

pub(super) struct NativeCapture {
    stream: SCStream,
    geometry: Geometry,
    slot: Arc<Slot>,
}

impl NativeCapture {
    pub(super) fn geometry(&self) -> Geometry {
        self.geometry
    }
}

impl Drop for NativeCapture {
    fn drop(&mut self) {
        // ScreenCaptureKit's bounded native completion runs outside its output callback queue.
        // Clear the slot even when startup failed before the parent Capture was constructed.
        self.slot.close();
        if let Err(error) = self.stream.stop_capture() {
            tracing::warn!(%error, "ScreenCaptureKit stop was not confirmed");
        }
    }
}

fn check_access(session: &SessionState, recording_allowed: bool) -> Result<()> {
    if !session.desktop_usable {
        bail!(
            "the host's desktop cannot be shown: {}",
            session.problem.as_deref().unwrap_or("no unlocked console session")
        );
    }
    if !recording_allowed {
        bail!(
            "live view needs macOS Screen Recording permission for the host; enable it in System Settings > Privacy & Security > Screen Recording, restart the host, then run doctor"
        );
    }
    Ok(())
}

fn preflight() -> Result<()> {
    check_access(&crate::platform::session_state(), crate::platform::macos::screen_recording_allowed())
}

fn supported_version(version: &str) -> bool {
    version.split('.').next().and_then(|s| s.parse::<u32>().ok()).is_some_and(|major| major >= 13)
}

fn point_geometry(monitor: &xcap::Monitor) -> Result<Geometry> {
    Ok((monitor.x()?, monitor.y()?, monitor.width()?, monitor.height()?))
}

/// Mouse tools use CGDisplayBounds points; CoreVideo output dimensions use backing pixels.
fn native_size(points: (u32, u32), scale: f32, max_long_edge: u32) -> Result<(u32, u32)> {
    if points.0 == 0 || points.1 == 0 || !scale.is_finite() || scale <= 0.0 {
        bail!("the display has invalid bounds or Retina backing scale; reconnect after checking its configuration");
    }
    let pixels = (f64::from(points.0) * f64::from(scale), f64::from(points.1) * f64::from(scale));
    if pixels.0 > f64::from(u32::MAX) || pixels.1 > f64::from(u32::MAX) {
        bail!("the display's pixel dimensions are out of range");
    }
    Ok(crate::stream::encode::stream_size(
        pixels.0.round() as u32,
        pixels.1.round() as u32,
        max_long_edge.clamp(320, MAX_LONG_EDGE),
    ))
}

struct FrameHandler {
    slot: Arc<Slot>,
    monitor: xcap::Monitor,
    geometry: Geometry,
    output_size: (u32, u32),
}

impl SCStreamOutputTrait for FrameHandler {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, output_type: SCStreamOutputType) {
        if output_type != SCStreamOutputType::Screen || self.slot.is_closed() {
            return;
        }
        // Check the actual console/TCC on every native event, including suspended/blank events.
        // Closing drops any queued surface, instead of exposing lock-screen or stale frames.
        if let Err(error) = preflight() {
            self.slot.fail(format!("{error:#}"));
            return;
        }
        if point_geometry(&self.monitor).ok() != Some(self.geometry) {
            self.slot.fail("the display bounds changed; reconnect to refresh the mouse coordinate mapping".into());
            return;
        }
        match sample.frame_status() {
            Some(SCFrameStatus::Complete) if sample.is_valid() => {}
            Some(SCFrameStatus::Blank | SCFrameStatus::Suspended | SCFrameStatus::Stopped) => {
                self.slot.fail("ScreenCaptureKit stopped providing visible desktop content; reconnect when the display is available".into());
                return;
            }
            // Idle, started, unknown and missing status do not prove a complete usable frame.
            _ => return,
        }
        let Some(pixel_buffer) = sample.pixel_buffer() else { return };
        let (Ok(width), Ok(height)) = (u32::try_from(pixel_buffer.width()), u32::try_from(pixel_buffer.height()))
        else {
            self.slot.fail("ScreenCaptureKit returned pixel dimensions out of range".into());
            return;
        };
        if (width, height) != self.output_size
            || pixel_buffer.pixel_format() != BGRA
            || pixel_buffer.io_surface().is_none()
        {
            self.slot.fail(
                "ScreenCaptureKit returned an unexpected frame size, format or missing IOSurface; reconnect".into(),
            );
            return;
        }
        // pixel_buffer() retains the CF object; releasing CMSampleBuffer is safe. The native
        // surface stays immutable and owned through encoding, then returns to the capture pool.
        self.slot.put(Raw {
            bgra: Vec::new(),
            width,
            height,
            captured: std::time::Instant::now(),
            pixel_buffer: Some(pixel_buffer),
        });
    }
}

struct StreamDelegate(Arc<Slot>);

impl SCStreamDelegateTrait for StreamDelegate {
    fn did_stop_with_error(&self, error: SCError) {
        self.0.fail(format!("ScreenCaptureKit stopped: {error}"));
    }
}

/// `draw_pointer: false` asks ScreenCaptureKit to leave the pointer out (`pointer` sends it).
pub(super) fn start(
    index: Option<usize>,
    max_fps: u32,
    max_long_edge: u32,
    slot: Arc<Slot>,
    draw_pointer: bool,
) -> Result<NativeCapture> {
    // Do not enumerate shareable windows or create/start a stream before these read-only gates.
    preflight()?;
    let version = sysinfo::System::os_version().unwrap_or_default();
    if !supported_version(&version) {
        bail!("native live view requires macOS 13 or newer; no screenshot-polling fallback is enabled");
    }
    let monitor = monitor(index)?;
    let geometry = point_geometry(&monitor)?;
    let size = native_size((geometry.2, geometry.3), monitor.scale_factor()?, max_long_edge)?;
    let display_id = monitor.id()?;
    let content = SCShareableContent::get().context("get ScreenCaptureKit displays")?;
    let display = content
        .displays()
        .into_iter()
        .find(|display| display.display_id() == display_id)
        .ok_or_else(|| anyhow!("the selected display is not available to ScreenCaptureKit"))?;
    let filter = SCContentFilter::create().with_display(&display).with_excluding_windows(&[]).build()?;
    let configuration = SCStreamConfiguration::new()
        .with_width(size.0)
        .with_height(size.1)
        .with_pixel_format(PixelFormat::BGRA)
        .with_shows_cursor(draw_pointer)
        .with_captures_audio(false)
        .with_minimum_frame_interval(&CMTime::new(1, max_fps.clamp(1, MAX_FPS) as i32))
        // Three is Apple's minimum: one encoding, one newest, one available to the producer.
        .with_queue_depth(3);
    let mut stream = SCStream::new_with_delegate(&filter, &configuration, StreamDelegate(slot.clone()))?;
    let queue = DispatchQueue::new("ai-remote.screen-capture", DispatchQoS::UserInteractive);
    stream.add_output_handler_with_queue(
        FrameHandler { slot: slot.clone(), monitor, geometry, output_size: size },
        SCStreamOutputType::Screen,
        Some(&queue),
    )?;
    let native = NativeCapture { stream, geometry, slot: slot.clone() };
    // Recheck after synchronous shareable-content discovery, which can take time.
    preflight()?;
    native.stream.start_capture().context("start ScreenCaptureKit")?;
    slot.wait_ready(FIRST_FRAME_TIMEOUT)?;
    Ok(native)
}

/// A running test pattern (see `capture::pattern`); stops when dropped. The picture is written
/// into IOSurface-backed pixel buffers, so the encoder takes the same native path as a captured
/// frame (VideoToolbox scales it to the stream size on the GPU).
pub(super) struct TestPattern {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for TestPattern {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A BGRA IOSurface with the pixel buffer that wraps it.
struct PatternSurface {
    surface: apple_cf::iosurface::IOSurface,
    pixel_buffer: apple_cf::cv::CVPixelBuffer,
}

impl PatternSurface {
    fn new(width: u32, height: u32) -> Result<Self> {
        let surface = apple_cf::iosurface::IOSurface::create(width as usize, height as usize, BGRA, 4)
            .context("allocate test-pattern IOSurface")?;
        let pixel_buffer = apple_cf::cv::CVPixelBuffer::create_with_io_surface(&surface)
            .map_err(|status| anyhow!("wrap test-pattern IOSurface: {status}"))?;
        Ok(Self { surface, pixel_buffer })
    }

    /// Copies `height` rows of the `width`-wide picture starting at row `top`.
    fn write(&mut self, picture: &[u8], width: u32, height: u32, top: u32) -> Result<()> {
        let mut guard = self.surface.lock_read_write().map_err(|status| anyhow!("lock IOSurface: {status}"))?;
        let stride = guard.bytes_per_row();
        // SAFETY: the surface is private to the pattern thread; the encoder reads a surface only
        // while encoding the frame it was handed, and the pool is deeper than the frames in flight.
        let destination = unsafe { guard.as_slice_mut() }.context("map test-pattern IOSurface")?;
        let row = width as usize * 4;
        for y in 0..height as usize {
            let source = (top as usize + y) * row;
            destination[y * stride..y * stride + row].copy_from_slice(&picture[source..source + row]);
        }
        Ok(())
    }
}

pub(super) fn start_test_pattern((width, height): (u32, u32), max_fps: u32, slot: Arc<Slot>) -> Result<TestPattern> {
    let fps = max_fps.clamp(1, MAX_FPS);
    let still_frames = super::pattern::still_after(fps);
    let tall = super::pattern::desktop_like(width, height * 2);
    // One being encoded, one waiting in the slot, two being written: never the same one twice in
    // a row while a frame is in flight.
    let mut pool = (0..4).map(|_| PatternSurface::new(width, height)).collect::<Result<Vec<_>>>()?;
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread = {
        let stop = stop.clone();
        std::thread::Builder::new().name("ai-remote-test-pattern".into()).spawn(move || {
            let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
            let mut next = std::time::Instant::now();
            let mut frame = 0u32;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                // A still desktop: no new frames at all.
                if still_frames.is_some_and(|n| frame >= n) {
                    std::thread::sleep(interval);
                    continue;
                }
                let top = super::pattern::scroll_top(frame.wrapping_mul(7), height);
                let surface = &mut pool[frame as usize % 4];
                frame = frame.wrapping_add(1);
                if let Err(error) = surface.write(&tall, width, height, top) {
                    tracing::warn!(error = %format!("{error:#}"), "test pattern frame");
                    slot.close();
                    return;
                }
                slot.put(Raw {
                    bgra: Vec::new(),
                    width,
                    height,
                    captured: std::time::Instant::now(),
                    pixel_buffer: Some(surface.pixel_buffer.clone()),
                });
                next += interval;
                let now = std::time::Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now;
                }
            }
        })?
    };
    tracing::warn!(width, height, fps, "AI_REMOTE_TEST_PATTERN: streaming a synthetic picture instead of the screen");
    Ok(TestPattern { stop, thread: Some(thread) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_pattern_delivers_native_frames_without_a_screen() {
        let slot = Arc::new(Slot::default());
        let pattern = start_test_pattern((128, 96), 60, slot.clone()).unwrap();
        let raw = slot.take(Duration::from_secs(5)).flatten().expect("a pattern frame");
        assert_eq!((raw.width, raw.height), (128, 96));
        let buffer = raw.pixel_buffer.as_ref().expect("an IOSurface-backed pixel buffer");
        assert_eq!((buffer.width(), buffer.height()), (128, 96));
        assert!(buffer.io_surface().is_some());
        drop(pattern);
    }

    #[test]
    fn the_primary_display_reports_a_refresh_rate_or_none() {
        // Headless CI has no display: None is fine; a rate must be plausible.
        if let Some(hz) = refresh_hz(None) {
            assert!((24..=500).contains(&hz), "{hz}");
        }
    }

    #[test]
    fn retina_pixels_scale_without_changing_point_geometry() {
        let geometry = (-1440, 0, 1440, 900);
        assert_eq!(native_size((geometry.2, geometry.3), 2.0, 3840).unwrap(), (2880, 1800));
        assert_eq!(native_size((geometry.2, geometry.3), 2.0, 1920).unwrap(), (1920, 1200));
        assert_eq!(geometry, (-1440, 0, 1440, 900));
        assert_eq!(native_size((1365, 767), 1.0, 1920).unwrap(), (1364, 766));
        assert_eq!(native_size((1440, 2560), 1.0, 1920).unwrap(), (1080, 1920));
        // 5K and 6K displays are no longer cut to 4K: the encoder's codec limit applies instead.
        assert_eq!(native_size((2560, 1440), 2.0, 8192).unwrap(), (5120, 2880));
        assert_eq!(native_size((3008, 1692), 2.0, 9000).unwrap(), (6016, 3384));
    }

    #[test]
    fn invalid_bounds_and_scale_are_refused() {
        for (points, scale) in [
            ((0, 900), 2.0),
            ((1440, 0), 1.0),
            ((1440, 900), 0.0),
            ((1440, 900), f32::NAN),
            ((1440, 900), f32::INFINITY),
        ] {
            assert!(native_size(points, scale, 1920).is_err());
        }
    }

    #[test]
    fn locked_or_unpermitted_capture_is_refused() {
        let locked = SessionState {
            desktop_usable: false,
            problem: Some("the macOS screen is locked".into()),
            ..Default::default()
        };
        assert!(check_access(&locked, true).unwrap_err().to_string().contains("locked"));
        let unlocked = SessionState { desktop_usable: true, ..Default::default() };
        assert!(check_access(&unlocked, false).unwrap_err().to_string().contains("Screen Recording"));
        assert!(check_access(&unlocked, true).is_ok());
    }

    #[test]
    fn native_platform_floor_is_explicit() {
        assert!(!supported_version("12.7.6"));
        assert!(!supported_version(""));
        assert!(supported_version("13.0"));
        assert!(supported_version("27.0"));
    }
}
