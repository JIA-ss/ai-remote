//! Screen capture for the live view: Windows Graphics Capture through `windows-capture`. Frames
//! arrive only when the screen changes; the newest one waits in a slot for the encoder, so a slow
//! encoder or network skips frames before they are encoded instead of queueing them.

use std::{
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use anyhow::{Result, anyhow};

/// A captured frame, tightly packed BGRA.
pub struct Raw {
    pub bgra: Vec<u8>,
    pub width: u32,
    pub height: u32,
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
}

impl Slot {
    fn put(&self, frame: Raw) {
        self.frame.lock().unwrap().frame = Some(frame);
        self.ready.notify_one();
    }

    pub fn close(&self) {
        self.frame.lock().unwrap().closed = true;
        self.ready.notify_all();
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
    control: Option<windows_capture::capture::CaptureControl<win::Handler, win::Error>>,
    slot: Arc<Slot>,
}

impl Capture {
    pub fn slot(&self) -> Arc<Slot> {
        self.slot.clone()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.slot.close();
        #[cfg(windows)]
        if let Some(control) = self.control.take() {
            let _ = control.stop();
        }
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
    #[cfg(not(windows))]
    Err(anyhow!("live view is not available on this platform yet"))
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
pub fn start(index: Option<usize>, max_fps: u32) -> Result<Capture> {
    let slot = Arc::new(Slot::default());
    #[cfg(windows)]
    {
        let control = win::start(index, max_fps, slot.clone())?;
        Ok(Capture { control: Some(control), slot })
    }
    #[cfg(not(windows))]
    {
        let _ = (index, max_fps, slot);
        Err(anyhow!("live view is not available on this platform yet"))
    }
}

#[cfg(windows)]
mod win {
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
            self.slot.put(Raw { bgra, width, height });
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
            MinimumUpdateIntervalSettings::Custom(Duration::from_millis(1000 / max_fps.clamp(1, 60) as u64)),
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
        let capture = start(None, 30).unwrap();
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
