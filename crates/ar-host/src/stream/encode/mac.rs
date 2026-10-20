//! The maintained doom-fish bindings own all VideoToolbox callbacks and native retain/release.
//! Capture's immutable IOSurface stays native; the BGRA API and software fallback explicitly copy.
//!
//! HEVC (every Apple Silicon Mac and T2 Intel Macs encode it in hardware) and H.264 High profile;
//! AV1 has no Apple hardware encoder. The low-latency rate controller
//! (`EnableLowLatencyRateControl`) is asked for first and the regular encoder taken when the
//! machine has none.

use anyhow::{Context, Result, anyhow, bail};
use apple_cf::{
    cf::{CFDictionary, CFNumber, CFString},
    cm::CMTime,
    cv::CVPixelBuffer,
    iosurface::IOSurface,
};
use videotoolbox::{
    CompressionSession, FrameProperties, HardwareAcceleration, PixelTransferSession, ProfileLevel, ScalingMode,
};

use super::{Codec, Encoded, avcc_to_annex_b};

/// Whether VideoToolbox can encode `codec` in hardware on some Mac: AV1 never; HEVC and H.264 on
/// every Mac this host supports (macOS 13+), although `HardwareEncoder::new` is the real test.
pub(super) fn available(codec: Codec) -> bool {
    matches!(codec, Codec::H264 | Codec::Hevc)
}

/// Keyframes only when the stream asks (a new viewer, a decoder error): a periodic IDR costs a
/// large frame for nothing on a desktop picture. VideoToolbox wants a positive bound.
const KEYFRAME_INTERVAL_FRAMES: i32 = 1 << 30;

const BGRA: u32 = u32::from_be_bytes(*b"BGRA");

struct SurfaceBuffer {
    surface: IOSurface,
    pixel_buffer: CVPixelBuffer,
}

impl SurfaceBuffer {
    fn new(width: u32, height: u32) -> Result<Self> {
        let surface =
            IOSurface::create(width as usize, height as usize, BGRA, 4).context("allocate encoder BGRA IOSurface")?;
        // Initialize padding as well as visible bytes before sharing this fresh allocation.
        {
            let mut guard = surface.lock_read_write().map_err(|status| anyhow!("lock IOSurface: {status}"))?;
            // SAFETY: This fresh surface is not yet wrapped, shared with any other thread/native
            // API, or submitted to an encoder. The lock and surface outlive the mutable slice.
            unsafe { guard.as_slice_mut() }.context("map IOSurface")?.fill(0);
        }
        let pixel_buffer = CVPixelBuffer::create_with_io_surface(&surface)
            .map_err(|status| anyhow!("wrap encoder IOSurface: {status}"))?;
        Ok(Self { surface, pixel_buffer })
    }

    fn size(&self) -> (u32, u32) {
        (self.pixel_buffer.width() as u32, self.pixel_buffer.height() as u32)
    }

    fn write(&mut self, bgra: &[u8], width: u32, height: u32, stride: u32) -> Result<()> {
        anyhow::ensure!(self.size() == (width, height), "IOSurface source dimensions changed");
        let mut guard = self.surface.lock_read_write().map_err(|status| anyhow!("lock source IOSurface: {status}"))?;
        let destination_stride = guard.bytes_per_row();
        // SAFETY: This surface is privately owned by one encoder. Each previous synchronous
        // encode/transfer completed before this write; no frame is concurrently submitted.
        // The retained pixel-buffer alias is not accessed while the surface is mapped.
        let destination = unsafe { guard.as_slice_mut() }.context("map source IOSurface")?;
        let row_size = width as usize * 4;
        for y in 0..height as usize {
            destination[y * destination_stride..y * destination_stride + row_size]
                .copy_from_slice(&bgra[y * stride as usize..y * stride as usize + row_size]);
        }
        Ok(())
    }
}

pub(super) struct HardwareEncoder {
    session: CompressionSession,
    codec: Codec,
    low_latency: bool,
    width: u32,
    height: u32,
    fps: u32,
    sequence: i64,
    source: Option<SurfaceBuffer>,
    scaled: Option<SurfaceBuffer>,
    transfer: Option<PixelTransferSession>,
}

impl HardwareEncoder {
    pub(super) fn new(width: u32, height: u32, fps: u32, bitrate_bps: u32, codec: Codec) -> Result<Self> {
        let (vt_codec, profile) = match codec {
            Codec::H264 => (videotoolbox::Codec::H264, ProfileLevel::H264HighAutoLevel),
            Codec::Hevc => (videotoolbox::Codec::HEVC, ProfileLevel::HEVCMainAutoLevel),
            Codec::Av1 => bail!("VideoToolbox has no AV1 hardware encoder"),
        };
        let build = |low_latency: bool| {
            CompressionSession::builder(i32::try_from(width)?, i32::try_from(height)?, vt_codec)
                .with_hardware_acceleration(HardwareAcceleration::Required)
                .with_low_latency_rate_control(low_latency)
                .with_real_time(true)
                .with_allow_frame_reordering(false)
                .with_average_bit_rate(i32::try_from(bitrate_bps)?)
                .with_expected_frame_rate(f64::from(fps))
                .with_max_keyframe_interval(KEYFRAME_INTERVAL_FRAMES)
                .with_profile_level(profile)
                .build()
                .with_context(|| format!("create required hardware {} session", codec.name()))
        };
        let (session, low_latency) = match build(true) {
            Ok(session) => (session, true),
            Err(error) => {
                tracing::debug!(codec = codec.name(), "no low-latency VideoToolbox encoder: {error:#}");
                (build(false)?, false)
            }
        };
        // `Required` already fails the session without a hardware encoder; the regular encoder
        // also answers the question, the low-latency one does not (property not supported).
        if !low_latency {
            // SAFETY: This key is the system's valid, process-lifetime compression-property
            // CFString. No raw session/callback ownership crosses our code; copy_property retains
            // its result.
            let hardware = unsafe {
                session.copy_property(videotoolbox::ffi::kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder)
            }
            .context("query actual hardware encoder")?
            .context("hardware-encoder property was unavailable")?;
            // CFBoolean's two values are process-lifetime singleton objects; query must return true.
            let is_hardware = unsafe { hardware.as_ptr().cast_const() == videotoolbox::ffi::kCFBooleanTrue.cast() };
            anyhow::ensure!(is_hardware, "VideoToolbox did not confirm hardware acceleration");
        }
        Ok(Self {
            session,
            codec,
            low_latency,
            width,
            height,
            fps,
            sequence: 0,
            source: None,
            scaled: None,
            transfer: None,
        })
    }

    pub(super) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub(super) fn codec(&self) -> Codec {
        self.codec
    }

    /// Whether the low-latency rate controller took the session.
    pub(super) fn low_latency(&self) -> bool {
        self.low_latency
    }

    /// Changes the target bitrate of the running session; the next frames follow it.
    pub(super) fn set_bitrate(&self, bitrate_bps: u32) -> Result<()> {
        // SAFETY: the key is the system's process-lifetime constant; the borrowed wrapper retains it.
        let key = unsafe {
            CFString::from_raw_borrowed(videotoolbox::ffi::kVTCompressionPropertyKey_AverageBitRate.cast_mut().cast())
        }
        .context("AverageBitRate property key")?;
        let value = CFNumber::from_i64(i64::from(bitrate_bps));
        let properties = CFDictionary::from_pairs(&[(&key, &value)]);
        self.session.set_properties(&properties).context("set VideoToolbox AverageBitRate")
    }

    pub(super) fn encode_bgra(
        &mut self,
        bgra: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        force: bool,
    ) -> Result<Option<Encoded>> {
        if self.source.as_ref().is_none_or(|source| source.size() != (width, height)) {
            self.source = Some(SurfaceBuffer::new(width, height)?);
        }
        let source = self.source.as_mut().context("source IOSurface missing")?;
        source.write(bgra, width, height, stride)?;
        // The clone only retains the native buffer; it copies no pixels.
        let buffer = source.pixel_buffer.clone();
        self.encode_pixel_buffer(&buffer, force)
    }

    pub(super) fn encode_pixel_buffer(&mut self, buffer: &CVPixelBuffer, force: bool) -> Result<Option<Encoded>> {
        // GPU-scaled ScreenCaptureKit frames normally take this branch without mapping or copying.
        let surface = if (buffer.width(), buffer.height()) == (self.width as usize, self.height as usize) {
            buffer.io_surface().context("native capture buffer is not IOSurface-backed")?
        } else {
            if self.scaled.is_none() {
                self.scaled = Some(SurfaceBuffer::new(self.width, self.height)?);
            }
            if self.transfer.is_none() {
                let transfer = PixelTransferSession::new().context("create pixel-transfer session")?;
                transfer.set_real_time(true).context("set real-time pixel transfer")?;
                transfer.set_scaling_mode(ScalingMode::Normal).context("set pixel-transfer scaling")?;
                self.transfer = Some(transfer);
            }
            let destination = self.scaled.as_ref().context("scaled IOSurface missing")?;
            self.transfer
                .as_ref()
                .context("pixel-transfer session missing")?
                .transfer(buffer, &destination.pixel_buffer)
                .context("scale native pixel buffer")?;
            destination.surface.clone()
        };
        let encoded = self
            .session
            .encode_with_properties(
                &surface,
                CMTime::new(self.sequence, i32::try_from(self.fps)?),
                FrameProperties::new().with_force_key_frame(force),
            )
            .with_context(|| format!("encode native {} frame", self.codec.name()))?;
        self.sequence = self.sequence.checked_add(1).context("encoder timestamp overflow")?;
        if encoded.data.is_empty() {
            return Ok(None);
        }
        let sample = encoded.cm_sample_buffer().context("encoded frame has no sample buffer")?;
        let keyframe = sample.is_sync_sample();
        let format = sample.format_description().context("encoded frame has no video format")?;
        anyhow::ensure!(
            format.video_dimensions() == Some((self.width as i32, self.height as i32)),
            "hardware encoder output dimensions changed"
        );
        let parameter_sets = format
            .video_parameter_sets()
            .map_err(|status| anyhow!("read {} parameter sets: {status}", self.codec.name()))?;
        let data = avcc_to_annex_b(
            &encoded.data,
            usize::try_from(parameter_sets.nal_unit_header_length)?,
            &parameter_sets.parameter_sets,
            keyframe,
            self.codec,
        )?;
        Ok(Some(Encoded { data, keyframe, width: self.width, height: self.height, codec: self.codec, captured: None }))
    }
}

/// Software fallback for capture's BGRA buffer. This path is deliberately absent from the normal
/// hardware/native flow. Capture retains an immutable sample buffer until encode_raw returns.
pub(super) fn copy_bgra(buffer: &CVPixelBuffer) -> Result<(Vec<u8>, u32)> {
    anyhow::ensure!(
        buffer.pixel_format() == BGRA && !buffer.is_planar(),
        "software fallback needs a BGRA capture buffer"
    );
    let row_size = buffer.width().checked_mul(4).context("native row size overflow")?;
    let length = row_size.checked_mul(buffer.height()).context("native frame size overflow")?;
    let stride = u32::try_from(row_size)?;
    let guard = buffer.lock_read_only().map_err(|status| anyhow!("lock native fallback frame: {status}"))?;
    let mut packed = Vec::with_capacity(length);
    for y in 0..buffer.height() {
        // SAFETY: Capture owns a retained, immutable completed sample. No Rust/native alias
        // mutates it while this read-only lock and row borrow exist; each row is copied at once.
        let row = unsafe { guard.row(y) }.context("native BGRA row unavailable")?;
        let visible = row.get(..row_size).context("native BGRA row is shorter than width")?;
        packed.extend_from_slice(visible);
    }
    Ok((packed, stride))
}
