//! The maintained doom-fish bindings own all VideoToolbox callbacks and native retain/release.
//! Capture's immutable IOSurface stays native; the BGRA API and software fallback explicitly copy.

use anyhow::{Context, Result, anyhow};
use apple_cf::{cm::CMTime, cv::CVPixelBuffer, iosurface::IOSurface};
use videotoolbox::{
    Codec, CompressionSession, FrameProperties, HardwareAcceleration, PixelTransferSession, ProfileLevel, ScalingMode,
};

use super::{Encoded, avcc_to_annex_b};

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
    width: u32,
    height: u32,
    fps: u32,
    sequence: i64,
    source: Option<SurfaceBuffer>,
    scaled: Option<SurfaceBuffer>,
    transfer: Option<PixelTransferSession>,
}

impl HardwareEncoder {
    pub(super) fn new(width: u32, height: u32, fps: u32, bitrate_bps: u32) -> Result<Self> {
        let session = CompressionSession::builder(i32::try_from(width)?, i32::try_from(height)?, Codec::H264)
            .with_hardware_acceleration(HardwareAcceleration::Required)
            .with_real_time(true)
            .with_allow_frame_reordering(false)
            .with_average_bit_rate(i32::try_from(bitrate_bps)?)
            .with_expected_frame_rate(f64::from(fps))
            .with_max_keyframe_interval(i32::try_from(fps.saturating_mul(10))?)
            .with_profile_level(ProfileLevel::H264BaselineAutoLevel)
            .build()
            .context("create required hardware H.264 session")?;
        // SAFETY: This key is the system's valid, process-lifetime compression-property CFString.
        // No raw session/callback ownership crosses our code; copy_property retains its result.
        let hardware = unsafe {
            session.copy_property(videotoolbox::ffi::kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder)
        }
        .context("query actual hardware encoder")?
        .context("hardware-encoder property was unavailable")?;
        // CFBoolean's two values are process-lifetime singleton objects; query must return true.
        let is_hardware = unsafe { hardware.as_ptr().cast_const() == videotoolbox::ffi::kCFBooleanTrue.cast() };
        anyhow::ensure!(is_hardware, "VideoToolbox did not confirm hardware acceleration");
        Ok(Self { session, width, height, fps, sequence: 0, source: None, scaled: None, transfer: None })
    }

    pub(super) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
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
            .context("encode native H.264 frame")?;
        self.sequence = self.sequence.checked_add(1).context("encoder timestamp overflow")?;
        if encoded.data.is_empty() {
            return Ok(None);
        }
        let sample = encoded.cm_sample_buffer().context("encoded frame has no sample buffer")?;
        let keyframe = sample.is_sync_sample();
        let format = sample.format_description().context("encoded frame has no H.264 format")?;
        anyhow::ensure!(
            format.video_dimensions() == Some((self.width as i32, self.height as i32)),
            "hardware encoder output dimensions changed"
        );
        let parameter_sets =
            format.video_parameter_sets().map_err(|status| anyhow!("read H.264 parameter sets: {status}"))?;
        let data = avcc_to_annex_b(
            &encoded.data,
            usize::try_from(parameter_sets.nal_unit_header_length)?,
            &parameter_sets.parameter_sets,
            keyframe,
        )?;
        Ok(Some(Encoded {
            data,
            keyframe,
            width: self.width,
            height: self.height,
            codec: super::Codec::H264,
            captured: None,
        }))
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
