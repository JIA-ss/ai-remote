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
        max_long_edge.clamp(320, 3840),
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
        self.slot.put(Raw { bgra: Vec::new(), width, height, pixel_buffer: Some(pixel_buffer) });
    }
}

struct StreamDelegate(Arc<Slot>);

impl SCStreamDelegateTrait for StreamDelegate {
    fn did_stop_with_error(&self, error: SCError) {
        self.0.fail(format!("ScreenCaptureKit stopped: {error}"));
    }
}

pub(super) fn start(index: Option<usize>, max_fps: u32, max_long_edge: u32, slot: Arc<Slot>) -> Result<NativeCapture> {
    // Do not enumerate shareable windows or create/start a stream before these read-only gates.
    preflight()?;
    let version = sysinfo::System::os_version().unwrap_or_default();
    if !supported_version(&version) {
        bail!("native live view requires macOS 13 or newer; no screenshot-polling fallback is enabled");
    }
    // Preserve the public system_info/monitor index order. xcap reads only display metadata here.
    let monitors = xcap::Monitor::all().context("list display metadata")?;
    let monitor = match index {
        Some(index) => monitors.get(index),
        None => monitors.iter().find(|m| m.is_primary().unwrap_or(false)).or(monitors.first()),
    }
    .ok_or_else(|| anyhow!("monitor not found"))?
    .clone();
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
        .with_shows_cursor(true)
        .with_captures_audio(false)
        .with_minimum_frame_interval(&CMTime::new(1, max_fps.clamp(1, 60) as i32))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retina_pixels_scale_without_changing_point_geometry() {
        let geometry = (-1440, 0, 1440, 900);
        assert_eq!(native_size((geometry.2, geometry.3), 2.0, 3840).unwrap(), (2880, 1800));
        assert_eq!(native_size((geometry.2, geometry.3), 2.0, 1920).unwrap(), (1920, 1200));
        assert_eq!(geometry, (-1440, 0, 1440, 900));
        assert_eq!(native_size((1365, 767), 1.0, 1920).unwrap(), (1364, 766));
        assert_eq!(native_size((1440, 2560), 1.0, 1920).unwrap(), (1080, 1920));
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
