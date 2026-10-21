//! Video for the live stream. Hardware encoders are the normal path: VideoToolbox on macOS (fed
//! capture's retained native pixel buffer) and the GPU vendor's Media Foundation encoder on Windows
//! (fed capture's NV12 textures). Both avoid any CPU copy of the picture. OpenH264 is the portable
//! fallback. Output is Annex B (H.264/HEVC, parameter sets on every keyframe) or AV1 OBUs (sequence
//! header on every keyframe).

use anyhow::{Context, Result, anyhow};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images::Image};
use openh264::{
    OpenH264API,
    encoder::{
        BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Profile, RateControlMode, UsageType,
    },
    formats::YUVSlices,
};
use yuvutils_rs::{
    YuvChromaSubsampling, YuvConversionMode, YuvPlanarImageMut, YuvRange, YuvStandardMatrix, bgra_to_yuv420,
};

#[cfg(all(test, target_os = "macos"))]
#[path = "encode/hardware_bench.rs"]
mod hardware_bench;
#[cfg(target_os = "macos")]
#[path = "encode/mac.rs"]
mod mac;
#[cfg(windows)]
#[path = "encode/win.rs"]
mod win;
#[cfg(all(test, windows))]
pub(crate) use win::{LOW_DELAY_VBR_FOR_TESTS, tuning};

/// A codec of the live stream. A viewer lists what it can decode, best first; the host picks the
/// first it can encode in hardware, and H.264 always works (in software if need be).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    Hevc,
    Av1,
}

impl Codec {
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "h264" | "avc" => Some(Self::H264),
            "hevc" | "h265" => Some(Self::Hevc),
            "av1" => Some(Self::Av1),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::Av1 => "av1",
        }
    }

    /// The codec's id in bits 1-2 of a frame header's flags.
    pub fn wire_id(self) -> u8 {
        match self {
            Self::H264 => 0,
            Self::Hevc => 1,
            Self::Av1 => 2,
        }
    }

    /// The largest long edge to give this codec's encoders: H.264 hardware stops at 4096 (and its
    /// highest level at 4096x2304), HEVC and AV1 hardware reach 8192.
    pub fn max_long_edge(self) -> u32 {
        match self {
            Self::H264 => 4096,
            Self::Hevc | Self::Av1 => 8192,
        }
    }
}

/// Whether this machine can encode `codec` in hardware.
pub fn hardware_available(codec: Codec) -> bool {
    #[cfg(windows)]
    {
        win::available(codec)
    }
    #[cfg(target_os = "macos")]
    {
        mac::available(codec)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = codec;
        false
    }
}

/// The software encoder's size limit: beyond it OpenH264 manages a few frames a second at most.
const SOFTWARE_MAX_LONG_EDGE: u32 = 3840;

/// The codecs to try, in order, for a viewer that decodes `wanted` (best first): those this host
/// has hardware for, then H.264 (software if need be). Also the long edge to capture at.
pub fn plan(wanted: &[Codec], max_long_edge: u32) -> (Vec<Codec>, u32) {
    let mut codecs: Vec<Codec> = Vec::new();
    for &codec in wanted {
        if !codecs.contains(&codec) && hardware_available(codec) {
            codecs.push(codec);
        }
    }
    if !codecs.contains(&Codec::H264) {
        codecs.push(Codec::H264);
    }
    let first = codecs[0];
    let limit = if hardware_available(first) { first.max_long_edge() } else { SOFTWARE_MAX_LONG_EDGE };
    (codecs, max_long_edge.min(limit))
}

/// One encoded frame.
pub struct Encoded {
    /// Annex B NAL units (a keyframe starts with its parameter sets), or AV1 OBUs.
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
    pub codec: Codec,
    /// When the picture was captured, once known (the stream's timestamps).
    pub captured: Option<std::time::Instant>,
}

/// The stream size for a source size: the long edge at most `max_long_edge`, both sides even (4:2:0
/// needs that), never upscaled.
pub fn stream_size(width: u32, height: u32, max_long_edge: u32) -> (u32, u32) {
    let long = width.max(height).max(1);
    let scale = (max_long_edge as f64 / long as f64).min(1.0);
    let even = |v: f64| ((v.round() as u32) & !1).max(2);
    (even(width as f64 * scale), even(height as f64 * scale))
}

enum Backend {
    #[cfg(any(target_os = "macos", windows))]
    PendingHardware,
    #[cfg(target_os = "macos")]
    Hardware(Box<mac::HardwareEncoder>),
    #[cfg(windows)]
    Hardware(Box<win::HardwareEncoder>),
    Software(Box<SoftwareEncoder>),
}

pub struct FrameEncoder {
    backend: Backend,
    /// Codecs to try in hardware, best first.
    #[cfg_attr(not(windows), allow(dead_code))]
    codecs: Vec<Codec>,
    max_fps: u32,
    bitrate_bps: u32,
    max_long_edge: u32,
    /// Capture's scaling target, lowered when the codec in use cannot take the size.
    #[cfg_attr(not(windows), allow(dead_code))]
    capture_edge: Option<std::sync::Arc<std::sync::atomic::AtomicU32>>,
    #[cfg_attr(not(windows), allow(dead_code))]
    guard: RateGuard,
    force_next: bool,
}

/// Watches a hardware encoder's output against the requested bitrate, because some encoders do not
/// keep to it at all. NVENC's AV1 through Media Foundation (driver 581.57, 2026-10-12) produced
/// 11-122 Mbit/s for 12 requested depending on the size, and opening its QP ceiling only moved the
/// failure from native sizes (50-122) to scaled ones (99-321); it ignored the rate-control mode
/// entirely; at 5K with 150 Mbit/s requested it ran at 191. HEVC and H.264 kept within 1.13x of the
/// request in the same runs. When the output (keyframes aside, they are expected bursts) runs over
/// 1.2x the request for two seconds, the stream moves to the next codec the viewer accepts.
#[derive(Default)]
struct RateGuard {
    since: Option<std::time::Instant>,
    bits: u64,
}

const RATE_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);
const RATE_TOLERANCE: f64 = 1.2;

impl RateGuard {
    /// Adds a frame; `true` when the window just ended over the tolerance.
    #[cfg_attr(not(windows), allow(dead_code))]
    fn over(&mut self, frame: &Encoded, bitrate_bps: u32, now: std::time::Instant) -> bool {
        if frame.keyframe {
            return false;
        }
        let since = *self.since.get_or_insert(now);
        self.bits += frame.data.len() as u64 * 8;
        let elapsed = now.saturating_duration_since(since);
        if elapsed < RATE_WINDOW {
            return false;
        }
        let rate = self.bits as f64 / elapsed.as_secs_f64();
        *self = Self { since: Some(now), bits: 0 };
        rate > f64::from(bitrate_bps) * RATE_TOLERANCE
    }
}

impl FrameEncoder {
    pub fn new(max_fps: u32, bitrate_bps: u32, max_long_edge: u32) -> Result<Self> {
        anyhow::ensure!(max_fps > 0 && bitrate_bps > 0 && max_long_edge >= 2, "invalid stream settings");
        #[cfg(any(target_os = "macos", windows))]
        let backend = Backend::PendingHardware;
        #[cfg(not(any(target_os = "macos", windows)))]
        let backend = Backend::Software(Box::new(SoftwareEncoder::new(max_fps, bitrate_bps, max_long_edge)?));
        Ok(Self {
            backend,
            codecs: vec![Codec::H264],
            max_fps,
            bitrate_bps,
            max_long_edge,
            capture_edge: None,
            guard: RateGuard::default(),
            force_next: true,
        })
    }

    /// Codecs to try in hardware, best first (see `plan`).
    pub fn with_codecs(mut self, codecs: &[Codec]) -> Self {
        if !codecs.is_empty() {
            self.codecs = codecs.to_vec();
        }
        self
    }

    /// Lets the encoder lower capture's long edge when its codec has a smaller size limit.
    pub fn with_capture_edge(mut self, edge: std::sync::Arc<std::sync::atomic::AtomicU32>) -> Self {
        self.capture_edge = Some(edge);
        self
    }

    /// The backend actually selected, rather than the requested hardware preference.
    pub fn backend_name(&self) -> String {
        match &self.backend {
            #[cfg(any(target_os = "macos", windows))]
            Backend::PendingHardware => "pending".into(),
            #[cfg(target_os = "macos")]
            Backend::Hardware(encoder) => format!(
                "videotoolbox ({}, hardware{})",
                encoder.codec().name(),
                if encoder.low_latency() { ", low latency" } else { "" }
            ),
            #[cfg(windows)]
            Backend::Hardware(encoder) => format!("{} ({}, hardware)", encoder.name(), encoder.codec().name()),
            Backend::Software(_) => "openh264-software".into(),
        }
    }

    pub fn force_keyframe(&mut self) {
        self.force_next = true;
    }

    /// The bitrate in use: the request, or what the viewer lowered it to.
    pub fn bitrate(&self) -> u32 {
        self.bitrate_bps
    }

    /// Changes the bitrate of the running stream (the viewer adapting to its network). Hardware
    /// encoders and OpenH264 take it on the next frame; should VideoToolbox refuse the live change,
    /// the encoder is set up again with a keyframe.
    pub fn set_bitrate(&mut self, bitrate_bps: u32) {
        if bitrate_bps == self.bitrate_bps || bitrate_bps == 0 {
            return;
        }
        self.bitrate_bps = bitrate_bps;
        self.guard = RateGuard::default();
        #[cfg(target_os = "macos")]
        let mut rebuild = false;
        match &mut self.backend {
            #[cfg(windows)]
            Backend::Hardware(encoder) => encoder.set_bitrate(bitrate_bps),
            #[cfg(target_os = "macos")]
            Backend::Hardware(encoder) => {
                if let Err(error) = encoder.set_bitrate(bitrate_bps) {
                    tracing::warn!("VideoToolbox refused the new bitrate; restarting the encoder: {error:#}");
                    rebuild = true;
                }
            }
            Backend::Software(encoder) => encoder.set_bitrate(bitrate_bps),
            _ => {}
        }
        #[cfg(target_os = "macos")]
        if rebuild {
            self.backend = Backend::PendingHardware;
        }
    }

    fn use_software(&mut self) -> Result<()> {
        self.backend =
            Backend::Software(Box::new(SoftwareEncoder::new(self.max_fps, self.bitrate_bps, self.max_long_edge)?));
        // A fallback is a new H.264 stream: its first output must be independently decodable.
        self.force_next = true;
        Ok(())
    }

    /// Sets up (or re-sets up, after a size change) the VideoToolbox encoder for a native frame:
    /// the first codec in preference order the machine encodes in hardware, each at the largest
    /// size its codec allows (VideoToolbox scales the frame on the GPU).
    #[cfg(target_os = "macos")]
    fn prepare_hardware(&mut self, width: u32, height: u32) -> Result<()> {
        let size_for =
            |codec: Codec, max_long_edge: u32| stream_size(width, height, max_long_edge.min(codec.max_long_edge()));
        let initialize = match &self.backend {
            Backend::PendingHardware => true,
            Backend::Hardware(encoder) => encoder.size() != size_for(encoder.codec(), self.max_long_edge),
            Backend::Software(_) => false,
        };
        if !initialize {
            return Ok(());
        }
        for &codec in &self.codecs {
            if !hardware_available(codec) {
                continue;
            }
            let size = size_for(codec, self.max_long_edge);
            match mac::HardwareEncoder::new(size.0, size.1, self.max_fps, self.bitrate_bps, codec) {
                Ok(encoder) => {
                    tracing::info!(
                        width = size.0,
                        height = size.1,
                        codec = codec.name(),
                        low_latency = encoder.low_latency(),
                        "using VideoToolbox hardware encoding"
                    );
                    self.backend = Backend::Hardware(Box::new(encoder));
                    self.force_next = true;
                    return Ok(());
                }
                Err(error) => tracing::warn!(codec = codec.name(), "VideoToolbox encoder unavailable: {error:#}"),
            }
        }
        tracing::warn!("no VideoToolbox hardware encoder; using OpenH264");
        self.use_software()
    }

    /// Sets up (or re-sets up, after a size change) the hardware encoder for a GPU frame: the
    /// first codec in preference order that takes this size. `false` when this frame must be
    /// skipped because capture was asked for a smaller size.
    #[cfg(windows)]
    fn prepare_windows_hardware(&mut self, frame: &super::capture::win::GpuTexture) -> Result<bool> {
        let size = (frame.width, frame.height);
        let initialize = match &self.backend {
            Backend::PendingHardware => true,
            Backend::Hardware(encoder) => encoder.size() != size,
            Backend::Software(_) => false,
        };
        if !initialize {
            return Ok(true);
        }
        let long = size.0.max(size.1);
        for &codec in &self.codecs {
            if long > codec.max_long_edge() {
                continue;
            }
            match win::HardwareEncoder::new(frame, codec, self.max_fps, self.bitrate_bps) {
                Ok(encoder) => {
                    self.backend = Backend::Hardware(Box::new(encoder));
                    self.force_next = true;
                    return Ok(true);
                }
                Err(error) => tracing::warn!(codec = codec.name(), "hardware encoder unavailable: {error:#}"),
            }
        }
        // Nothing took this size. A codec with a smaller limit might, or software at most 4K.
        let any_hardware = self.codecs.iter().any(|c| hardware_available(*c));
        let smaller = if any_hardware {
            self.codecs.iter().map(|c| c.max_long_edge()).filter(|&edge| edge < long).max()
        } else {
            (SOFTWARE_MAX_LONG_EDGE < long).then_some(SOFTWARE_MAX_LONG_EDGE)
        };
        if let (Some(smaller), Some(edge)) = (smaller, &self.capture_edge) {
            tracing::warn!(long, smaller, "lowering the live view size for the encoder");
            edge.store(smaller, std::sync::atomic::Ordering::Relaxed);
            self.max_long_edge = self.max_long_edge.min(smaller);
            return Ok(false);
        }
        tracing::warn!("no hardware encoder takes this stream; using OpenH264");
        self.use_software()?;
        Ok(true)
    }

    /// Encodes a captured frame on its fastest path: the native buffer (macOS) or GPU texture
    /// (Windows) straight into the hardware encoder. An output from a synchronous encoder is this
    /// frame; an asynchronous one's output carries its own frame's capture instant.
    pub fn encode_raw(&mut self, raw: &super::capture::Raw) -> Result<Option<Encoded>> {
        let out = self.encode_raw_inner(raw)?;
        Ok(out.map(|mut frame| {
            frame.captured.get_or_insert(raw.captured);
            frame
        }))
    }

    fn encode_raw_inner(&mut self, raw: &super::capture::Raw) -> Result<Option<Encoded>> {
        #[cfg(windows)]
        if let Some(frame) = &raw.gpu {
            if !self.prepare_windows_hardware(frame)? {
                return Ok(None);
            }
            if let Backend::Hardware(encoder) = &mut self.backend {
                match encoder.encode(frame.clone(), raw.captured, self.force_next) {
                    Ok(out) => {
                        // The keyframe request went with this frame, whenever its output comes.
                        self.force_next = false;
                        return Ok(self.guarded(out));
                    }
                    Err(error) => {
                        tracing::warn!("hardware encoding failed; using OpenH264: {error:#}");
                        self.use_software()?;
                        if let Some(edge) = &self.capture_edge {
                            edge.fetch_min(SOFTWARE_MAX_LONG_EDGE, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
            }
            // OpenH264 refuses frames past 4K; capture sends smaller ones once it has rescaled.
            if frame.width.max(frame.height) > SOFTWARE_MAX_LONG_EDGE {
                return Ok(None);
            }
            let (y, uv) = super::capture::win::read_nv12(frame)?;
            return self.encode_nv12(&y, &uv, frame.width, frame.height);
        }
        #[cfg(target_os = "macos")]
        if let Some(buffer) = &raw.pixel_buffer {
            anyhow::ensure!(
                buffer.width() == raw.width as usize && buffer.height() == raw.height as usize,
                "native frame dimensions do not match its pixel buffer"
            );
            self.prepare_hardware(raw.width, raw.height)?;
            if let Backend::Hardware(encoder) = &mut self.backend {
                match encoder.encode_pixel_buffer(buffer, self.force_next) {
                    Ok(frame) => {
                        if frame.is_some() {
                            self.force_next = false;
                        }
                        return Ok(frame);
                    }
                    Err(error) => {
                        tracing::warn!("VideoToolbox native encoding failed; using OpenH264: {error:#}");
                        self.use_software()?;
                    }
                }
            }
            // Native capture leaves BGRA empty. Only the unavailable/failed hardware path maps
            // the retained immutable BGRA pixel buffer and copies it to the portable encoder.
            let (bgra, stride) = mac::copy_bgra(buffer)?;
            return self.encode(&bgra, raw.width, raw.height, stride);
        }
        self.encode(&raw.bgra, raw.width, raw.height, raw.width.saturating_mul(4))
    }

    /// An encoded frame an asynchronous hardware encoder finished after `encode_raw` returned.
    /// Call it while no new frame arrives, so a static screen still gets its last frame out.
    pub fn poll(&mut self) -> Result<Option<Encoded>> {
        #[cfg(windows)]
        if let Backend::Hardware(encoder) = &mut self.backend {
            let out = encoder.poll()?;
            return Ok(self.guarded(out));
        }
        Ok(None)
    }

    /// Passes a hardware encoder's output through the rate guard. When the codec does not keep to
    /// the requested bitrate, the next frame sets up the next codec (with a keyframe); this output
    /// still goes out, so the viewer is never left without a picture.
    #[cfg(windows)]
    fn guarded(&mut self, out: Option<Encoded>) -> Option<Encoded> {
        let frame = out?;
        if self.guard.over(&frame, self.bitrate_bps, std::time::Instant::now())
            && let Backend::Hardware(encoder) = &self.backend
            && self.codecs.len() > 1
        {
            let codec = encoder.codec();
            tracing::warn!(
                codec = codec.name(),
                requested_bps = self.bitrate_bps,
                "the hardware encoder does not keep to the requested bitrate; moving to the next codec"
            );
            self.codecs.retain(|c| *c != codec);
            self.backend = Backend::PendingHardware;
            self.guard = RateGuard::default();
        }
        Some(frame)
    }

    #[cfg(windows)]
    fn encode_nv12(&mut self, y: &[u8], uv: &[u8], width: u32, height: u32) -> Result<Option<Encoded>> {
        let Backend::Software(encoder) = &mut self.backend else { anyhow::bail!("no software encoder") };
        if self.force_next {
            encoder.force_keyframe();
        }
        let frame = encoder.encode_nv12(y, uv, width, height)?;
        if frame.is_some() {
            self.force_next = false;
        }
        Ok(frame)
    }

    pub fn encode(&mut self, bgra: &[u8], width: u32, height: u32, stride: u32) -> Result<Option<Encoded>> {
        validate_bgra(bgra, width, height, stride)?;
        #[cfg(target_os = "macos")]
        {
            self.prepare_hardware(width, height)?;
            if let Backend::Hardware(encoder) = &mut self.backend {
                match encoder.encode_bgra(bgra, width, height, stride, self.force_next) {
                    Ok(frame) => {
                        if frame.is_some() {
                            self.force_next = false;
                        }
                        return Ok(frame);
                    }
                    Err(error) => {
                        tracing::warn!("VideoToolbox BGRA encoding failed; using OpenH264: {error:#}");
                        self.use_software()?;
                    }
                }
            }
        }
        // Windows BGRA frames come from the CPU capture path, which only the software encoder takes.
        #[cfg(windows)]
        if !matches!(self.backend, Backend::Software(_)) {
            self.use_software()?;
        }
        let Backend::Software(encoder) = &mut self.backend else { unreachable!("encoder was initialized") };
        if self.force_next {
            encoder.force_keyframe();
        }
        let frame = encoder.encode(bgra, width, height, stride)?;
        if frame.is_some() {
            self.force_next = false;
        }
        Ok(frame)
    }

    #[cfg(test)]
    fn software(max_fps: u32, bitrate_bps: u32, max_long_edge: u32) -> Result<Self> {
        let mut encoder = Self::new(max_fps, bitrate_bps, max_long_edge)?;
        encoder.use_software()?;
        Ok(encoder)
    }
}

/// Whether `data` starts a picture that decodes on its own.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_keyframe(codec: Codec, data: &[u8]) -> bool {
    match codec {
        Codec::H264 => nal_headers(data).any(|b| b & 0x1f == 5),
        // IDR_W_RADL, IDR_N_LP, CRA.
        Codec::Hevc => nal_headers(data).any(|b| matches!((b >> 1) & 0x3f, 19..=21)),
        // A temporal unit with a sequence header (encoders send one with each keyframe).
        Codec::Av1 => av1_obu_types(data).any(|t| t == 1),
    }
}

/// Whether an access unit carries the parameter sets a decoder joining here needs.
#[cfg_attr(not(windows), allow(dead_code))]
fn has_parameter_sets(codec: Codec, data: &[u8]) -> bool {
    match codec {
        Codec::H264 => nal_headers(data).any(|b| b & 0x1f == 7),
        Codec::Hevc => nal_headers(data).any(|b| (b >> 1) & 0x3f == 33),
        Codec::Av1 => av1_obu_types(data).any(|t| t == 1),
    }
}

/// The first header byte of each Annex B NAL unit.
#[cfg_attr(not(windows), allow(dead_code))]
fn nal_headers(data: &[u8]) -> impl Iterator<Item = u8> + '_ {
    (0..data.len().saturating_sub(3))
        .filter(|&i| data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1)
        .filter_map(move |i| data.get(i + 3).copied())
}

/// OBU types of a low-overhead AV1 temporal unit.
#[cfg_attr(not(windows), allow(dead_code))]
fn av1_obu_types(data: &[u8]) -> impl Iterator<Item = u8> + '_ {
    let mut at = 0usize;
    std::iter::from_fn(move || {
        let header = *data.get(at)?;
        let kind = (header >> 3) & 0x0f;
        let mut next = at + 1 + usize::from(header & 0x04 != 0);
        if header & 0x02 == 0 {
            // No size field: the OBU runs to the end of the unit.
            at = data.len();
            return Some(kind);
        }
        let (mut size, mut shift) = (0usize, 0u32);
        loop {
            let byte = *data.get(next)?;
            next += 1;
            size |= usize::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 || shift > 56 {
                break;
            }
        }
        at = next.checked_add(size)?;
        Some(kind)
    })
}

fn validate_bgra(bgra: &[u8], width: u32, height: u32, stride: u32) -> Result<()> {
    let row = width.checked_mul(4).context("BGRA row size overflow")?;
    let length = (height as usize).checked_mul(stride as usize).context("BGRA frame size overflow")?;
    anyhow::ensure!(width >= 2 && height >= 2 && stride >= row && bgra.len() >= length, "invalid BGRA frame or stride");
    Ok(())
}

/// Convert only structurally valid AVCC access units, using the format's actual NAL length size.
#[cfg(any(target_os = "macos", test))]
/// AVCC/HVCC (length-prefixed NAL units) to Annex B, with the parameter sets (H.264: SPS and PPS;
/// HEVC: VPS, SPS and PPS) in front of a keyframe.
fn avcc_to_annex_b(
    data: &[u8],
    length_size: usize,
    parameter_sets: &[Vec<u8>],
    keyframe: bool,
    codec: Codec,
) -> Result<Vec<u8>> {
    anyhow::ensure!(matches!(length_size, 1 | 2 | 4), "invalid AVCC NAL length size");
    let mut out = Vec::with_capacity(data.len() + 128);
    if keyframe {
        let has = |nal_type: u8| {
            parameter_sets.iter().any(|s| {
                s.first().is_some_and(|b| match codec {
                    Codec::H264 => b & 0x1f == nal_type,
                    Codec::Hevc | Codec::Av1 => (b >> 1) & 0x3f == nal_type,
                })
            })
        };
        let complete = match codec {
            Codec::H264 => has(7) && has(8),
            Codec::Hevc => has(32) && has(33) && has(34),
            Codec::Av1 => false,
        };
        anyhow::ensure!(complete, "{} keyframe is missing its parameter sets", codec.name());
        for set in parameter_sets {
            anyhow::ensure!(!set.is_empty(), "empty parameter set");
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(set);
        }
    }
    let mut offset = 0usize;
    while offset < data.len() {
        let header_end = offset.checked_add(length_size).context("AVCC header overflow")?;
        let header = data.get(offset..header_end).context("truncated AVCC length header")?;
        let length = header.iter().fold(0usize, |n, byte| (n << 8) | *byte as usize);
        anyhow::ensure!(length > 0, "empty AVCC NAL unit");
        let end = header_end.checked_add(length).context("AVCC NAL length overflow")?;
        let nal = data.get(header_end..end).context("truncated AVCC NAL unit")?;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        offset = end;
    }
    anyhow::ensure!(offset > 0, "empty AVCC frame");
    Ok(out)
}

struct SoftwareEncoder {
    encoder: Encoder,
    resizer: Resizer,
    max_long_edge: u32,
    scaled: Vec<u8>,
    yuv: Option<YuvPlanarImageMut<'static, u8>>,
    out: Vec<u8>,
}

impl SoftwareEncoder {
    pub fn new(max_fps: u32, bitrate_bps: u32, max_long_edge: u32) -> Result<Self> {
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .profile(Profile::Baseline)
            .bitrate(BitRate::from_bps(bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(max_fps as f32))
            // Holds the bit rate; a skipped frame just means the next one carries the change.
            .rate_control_mode(RateControlMode::Bitrate)
            .skip_frames(true)
            // A keyframe only at the start and when a viewer asks for one (the stream is reliable).
            .intra_frame_period(IntraFramePeriod::from_num_frames(0))
            .num_threads(4);
        let encoder =
            Encoder::with_api_config(OpenH264API::from_source(), config).context("start the H.264 encoder")?;
        Ok(Self { encoder, resizer: Resizer::new(), max_long_edge, scaled: Vec::new(), yuv: None, out: Vec::new() })
    }

    /// The next frame will be a keyframe (a viewer joined or lost its place).
    pub fn force_keyframe(&mut self) {
        self.encoder.force_intra_frame();
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) {
        use openh264_sys2::{ENCODER_OPTION_BITRATE, ENCODER_OPTION_MAX_BITRATE, SBitrateInfo, SPATIAL_LAYER_ALL};
        let bitrate = i32::try_from(bitrate_bps).unwrap_or(i32::MAX);
        for option in [ENCODER_OPTION_MAX_BITRATE, ENCODER_OPTION_BITRATE] {
            let mut info = SBitrateInfo { iLayer: SPATIAL_LAYER_ALL, iBitrate: bitrate };
            // SAFETY: an initialized encoder and a live, correctly typed option struct.
            let status = unsafe { self.encoder.raw_api().set_option(option, std::ptr::addr_of_mut!(info).cast()) };
            if status != 0 {
                tracing::debug!(status, option, "OpenH264 refused a bitrate change");
            }
        }
    }

    /// Encodes one BGRA frame (`stride` bytes per row). `None` when the encoder produced nothing.
    pub fn encode(&mut self, bgra: &[u8], width: u32, height: u32, stride: u32) -> Result<Option<Encoded>> {
        let (w, h) = stream_size(width, height, self.max_long_edge);
        // Scale (or crop to even sides) into a tightly packed buffer.
        let (src, src_stride) = if (w, h) == (width, height) && stride == width * 4 {
            (bgra, stride)
        } else {
            let mut packed;
            let tight: &[u8] = if stride == width * 4 {
                bgra
            } else {
                packed = Vec::with_capacity((width * height * 4) as usize);
                for row in bgra.chunks(stride as usize).take(height as usize) {
                    packed.extend_from_slice(&row[..(width * 4) as usize]);
                }
                &packed
            };
            let mut tight = tight.to_vec();
            let src_img = Image::from_slice_u8(width, height, &mut tight, PixelType::U8x4)
                .map_err(|e| anyhow!("frame buffer: {e}"))?;
            self.scaled.resize((w * h * 4) as usize, 0);
            let mut dst_img = Image::from_slice_u8(w, h, &mut self.scaled, PixelType::U8x4)
                .map_err(|e| anyhow!("scaled buffer: {e}"))?;
            let options =
                ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)).use_alpha(false);
            self.resizer.resize(&src_img, &mut dst_img, &options).map_err(|e| anyhow!("scale the frame: {e}"))?;
            (&self.scaled[..], w * 4)
        };
        let yuv = match &mut self.yuv {
            Some(y) if y.width == w && y.height == h => y,
            slot => slot.insert(YuvPlanarImageMut::alloc(w, h, YuvChromaSubsampling::Yuv420)),
        };
        bgra_to_yuv420(yuv, src, src_stride, YuvRange::Limited, YuvStandardMatrix::Bt709, YuvConversionMode::Balanced)
            .map_err(|e| anyhow!("convert the frame: {e:?}"))?;
        let slices = YUVSlices::new(
            (yuv.y_plane.borrow(), yuv.u_plane.borrow(), yuv.v_plane.borrow()),
            (w as usize, h as usize),
            (yuv.y_stride as usize, yuv.u_stride as usize, yuv.v_stride as usize),
        );
        let stream = self.encoder.encode(&slices).context("encode a frame")?;
        let keyframe = matches!(stream.frame_type(), FrameType::IDR | FrameType::I);
        self.out.clear();
        stream.write_vec(&mut self.out);
        if self.out.is_empty() {
            return Ok(None);
        }
        Ok(Some(Encoded {
            data: std::mem::take(&mut self.out),
            keyframe,
            width: w,
            height: h,
            codec: Codec::H264,
            captured: None,
        }))
    }

    /// Encodes an NV12 frame already at the stream size (read back from the GPU path).
    #[cfg(windows)]
    pub fn encode_nv12(&mut self, y: &[u8], uv: &[u8], width: u32, height: u32) -> Result<Option<Encoded>> {
        let (w, h) = (width as usize, height as usize);
        anyhow::ensure!(y.len() >= w * h && uv.len() >= w * h / 2 && w % 2 == 0 && h % 2 == 0, "invalid NV12 frame");
        let (mut u, mut v) = (Vec::with_capacity(w * h / 4), Vec::with_capacity(w * h / 4));
        for pair in uv[..w * h / 2].chunks_exact(2) {
            u.push(pair[0]);
            v.push(pair[1]);
        }
        let slices = YUVSlices::new((&y[..w * h], &u, &v), (w, h), (w, w / 2, w / 2));
        let stream = self.encoder.encode(&slices).context("encode a frame")?;
        let keyframe = matches!(stream.frame_type(), FrameType::IDR | FrameType::I);
        self.out.clear();
        stream.write_vec(&mut self.out);
        if self.out.is_empty() {
            return Ok(None);
        }
        Ok(Some(Encoded {
            data: std::mem::take(&mut self.out),
            keyframe,
            width,
            height,
            codec: Codec::H264,
            captured: None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use openh264::decoder::Decoder;

    use super::*;

    #[test]
    fn the_rate_guard_trips_only_on_sustained_non_keyframe_overshoot() {
        let start = std::time::Instant::now();
        let frame = |bytes: usize, keyframe: bool| Encoded {
            data: vec![0; bytes],
            keyframe,
            width: 2,
            height: 2,
            codec: Codec::Av1,
            captured: None,
        };
        let at = |ms: u64| start + std::time::Duration::from_millis(ms);
        // 1 Mbit/s requested; 60 frames a second of 4 kB is 1.9 Mbit/s, of 2.7 kB 1.3 Mbit/s.
        let mut guard = RateGuard::default();
        let tripped = (0..=130u64).any(|i| guard.over(&frame(4_000, false), 1_000_000, at(i * 1000 / 60)));
        assert!(tripped, "2 s at 1.9x must trip");
        // Keyframes do not count, however big.
        let mut guard = RateGuard::default();
        assert!(!(0..=130u64).any(|i| guard.over(&frame(4_000_000, true), 1_000_000, at(i * 1000 / 60))));
        let mut guard = RateGuard::default();
        assert!((0..=130u64).any(|i| guard.over(&frame(2_700, false), 1_000_000, at(i * 1000 / 60))));
        // 1.15x (2.4 kB) for any length of time is within the tolerance.
        let mut guard = RateGuard::default();
        assert!(!(0..=600u64).any(|i| guard.over(&frame(2_400, false), 1_000_000, at(i * 1000 / 60))));
    }

    #[test]
    fn stream_sizes_are_even_and_never_upscaled() {
        assert_eq!(stream_size(3840, 2160, 1920), (1920, 1080));
        assert_eq!(stream_size(1366, 768, 1920), (1366, 768));
        assert_eq!(stream_size(1365, 767, 1920), (1364, 766));
        assert_eq!(stream_size(2560, 1600, 1600), (1600, 1000));
    }

    #[test]
    fn annex_b_rejects_truncated_units_and_preserves_parameter_sets() {
        let sets = vec![vec![0x67, 1], vec![0x68, 2]];
        let data = [0, 0, 0, 2, 0x65, 3, 0, 0, 0, 1, 0x06];
        assert_eq!(
            avcc_to_annex_b(&data, 4, &sets, true, Codec::H264).unwrap(),
            [0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 3, 0, 0, 0, 1, 0x06]
        );
        assert!(avcc_to_annex_b(&data[..data.len() - 1], 4, &sets, true, Codec::H264).is_err());
        assert!(avcc_to_annex_b(&[0, 0, 0], 4, &sets, false, Codec::H264).is_err());
        assert!(avcc_to_annex_b(&[0], 1, &sets, false, Codec::H264).is_err());
        assert!(avcc_to_annex_b(&data, 3, &sets, false, Codec::H264).is_err());
        assert!(avcc_to_annex_b(&data, 4, &[], true, Codec::H264).is_err());
        assert_eq!(avcc_to_annex_b(&[1, 0x41], 1, &[], false, Codec::H264).unwrap(), [0, 0, 0, 1, 0x41]);
        assert_eq!(avcc_to_annex_b(&[0, 1, 0x41], 2, &[], false, Codec::H264).unwrap(), [0, 0, 0, 1, 0x41]);
    }

    #[test]
    fn malformed_bgra_is_an_error_instead_of_a_panic() {
        let mut encoder = FrameEncoder::new(30, 4_000_000, 800).unwrap();
        assert!(encoder.encode(&[0; 16], 8, 8, 32).is_err());
        assert!(encoder.encode(&[0; 64], 4, 4, 8).is_err());
        assert!(encoder.encode(&[], u32::MAX, 2, u32::MAX).is_err());
    }

    /// A synthetic screen: encodes, and decodes back at the stream size.
    #[test]
    fn frames_round_trip_through_a_decoder() {
        let (w, h, stride) = (1000u32, 600u32, 1000u32 * 4 + 64);
        let mut bgra = vec![0u8; (stride * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = (y * stride + x * 4) as usize;
                bgra[i..i + 4].copy_from_slice(&[(x % 256) as u8, (y % 256) as u8, 128, 255]);
            }
        }
        let mut enc = FrameEncoder::software(30, 4_000_000, 800).unwrap();
        let first = enc.encode(&bgra, w, h, stride).unwrap().expect("a first frame");
        assert!(first.keyframe);
        assert_eq!((first.width, first.height), (800, 480));
        let mut dec = Decoder::new().unwrap();
        let yuv = dec.decode(&first.data).unwrap().expect("a decoded picture");
        assert_eq!(openh264::formats::YUVSource::dimensions(&yuv), (800, 480));
        // Later frames are not keyframes until one is asked for.
        bgra[0] = 255;
        let second = enc.encode(&bgra, w, h, stride).unwrap();
        assert!(second.is_none_or(|f| !f.keyframe));
        enc.force_keyframe();
        let third = enc.encode(&bgra, w, h, stride).unwrap().expect("a keyframe");
        assert!(third.keyframe);
        // A late-joining decoder receives no preceding frame or configuration object.
        let mut late = Decoder::new().unwrap();
        assert!(late.decode(&third.data).unwrap().is_some());
    }
}

#[cfg(test)]
mod bench {
    use std::time::Instant;

    use super::*;

    /// Per-stage cost on a moving synthetic 2560x1080 screen (run by hand with --ignored --nocapture,
    /// in release).
    #[test]
    #[ignore]
    fn stage_costs() {
        let (w, h) = (2560u32, 1080u32);
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for (i, p) in bgra.chunks_mut(4).enumerate() {
            let (x, y) = (i as u32 % w, i as u32 / w);
            p.copy_from_slice(&[((x / 7) % 256) as u8, ((y / 5) % 256) as u8, ((x ^ y) % 256) as u8, 255]);
        }
        for (label, threads, complexity) in [
            ("2 threads, medium", 2u16, openh264::encoder::Complexity::Medium),
            ("4 threads, low", 4, openh264::encoder::Complexity::Low),
        ] {
            let config = EncoderConfig::new()
                .usage_type(UsageType::ScreenContentRealTime)
                .profile(Profile::Baseline)
                .bitrate(BitRate::from_bps(6_000_000))
                .max_frame_rate(FrameRate::from_hz(30.0))
                .skip_frames(false)
                .complexity(complexity)
                .num_threads(threads);
            let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), config).unwrap();
            let (sw, sh) = stream_size(w, h, 1920);
            let mut resizer = Resizer::new();
            let mut scaled = vec![0u8; (sw * sh * 4) as usize];
            let mut yuv = YuvPlanarImageMut::alloc(sw, sh, YuvChromaSubsampling::Yuv420);
            let (mut t_scale, mut t_conv, mut t_enc) = (0.0, 0.0, 0.0);
            let n = 30;
            for f in 0..n {
                // Move a block so every frame differs.
                let off = (f * 37 % 2000) as usize * 4;
                bgra[off..off + 400].fill((f * 9) as u8);
                let t = Instant::now();
                let mut src = bgra.clone();
                let src_img = Image::from_slice_u8(w, h, &mut src, PixelType::U8x4).unwrap();
                let mut dst_img = Image::from_slice_u8(sw, sh, &mut scaled, PixelType::U8x4).unwrap();
                let opts =
                    ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)).use_alpha(false);
                resizer.resize(&src_img, &mut dst_img, &opts).unwrap();
                t_scale += t.elapsed().as_secs_f64();
                let t = Instant::now();
                bgra_to_yuv420(
                    &mut yuv,
                    &scaled,
                    sw * 4,
                    YuvRange::Limited,
                    YuvStandardMatrix::Bt709,
                    YuvConversionMode::Balanced,
                )
                .unwrap();
                t_conv += t.elapsed().as_secs_f64();
                let t = Instant::now();
                let slices = YUVSlices::new(
                    (yuv.y_plane.borrow(), yuv.u_plane.borrow(), yuv.v_plane.borrow()),
                    (sw as usize, sh as usize),
                    (yuv.y_stride as usize, yuv.u_stride as usize, yuv.v_stride as usize),
                );
                let _ = encoder.encode(&slices).unwrap().to_vec();
                t_enc += t.elapsed().as_secs_f64();
            }
            let ms = |t: f64| t * 1000.0 / n as f64;
            println!(
                "{label}: scale {:.1} ms (incl. copy), convert {:.1} ms, encode {:.1} ms",
                ms(t_scale),
                ms(t_conv),
                ms(t_enc)
            );
        }
    }
}
