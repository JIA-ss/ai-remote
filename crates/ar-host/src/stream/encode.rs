//! BGRA frames to H.264: scale to the stream size, convert to I420, encode with OpenH264 tuned for
//! screen content. Output is Annex B (start codes), which WebCodecs and every decoder take as is.

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

/// One encoded frame.
pub struct Encoded {
    /// Annex B NAL units; a keyframe starts with SPS and PPS.
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
}

/// The stream size for a source size: the long edge at most `max_long_edge`, both sides even (4:2:0
/// needs that), never upscaled.
pub fn stream_size(width: u32, height: u32, max_long_edge: u32) -> (u32, u32) {
    let long = width.max(height).max(1);
    let scale = (max_long_edge as f64 / long as f64).min(1.0);
    let even = |v: f64| ((v.round() as u32) & !1).max(2);
    (even(width as f64 * scale), even(height as f64 * scale))
}

pub struct FrameEncoder {
    encoder: Encoder,
    resizer: Resizer,
    max_long_edge: u32,
    scaled: Vec<u8>,
    yuv: Option<YuvPlanarImageMut<'static, u8>>,
    out: Vec<u8>,
}

impl FrameEncoder {
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
        Ok(Some(Encoded { data: std::mem::take(&mut self.out), keyframe, width: w, height: h }))
    }
}

#[cfg(test)]
mod tests {
    use openh264::decoder::Decoder;

    use super::*;

    #[test]
    fn stream_sizes_are_even_and_never_upscaled() {
        assert_eq!(stream_size(3840, 2160, 1920), (1920, 1080));
        assert_eq!(stream_size(1366, 768, 1920), (1366, 768));
        assert_eq!(stream_size(1365, 767, 1920), (1364, 766));
        assert_eq!(stream_size(2560, 1600, 1600), (1600, 1000));
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
        let mut enc = FrameEncoder::new(30, 4_000_000, 800).unwrap();
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
