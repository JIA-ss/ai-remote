//! Synthetic buffers only: no display/capture/input permission is needed.
//! Run in release with one test thread; hardware-unavailable results remain explicit.

use std::time::Instant;

use apple_cf::{cv::CVPixelBuffer, iosurface::IOSurface};
use openh264::{decoder::Decoder, formats::YUVSource};
use serde_json::json;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

use super::{Backend, Encoded, FrameEncoder, mac};

fn synthetic(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    for (i, pixel) in pixels.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i as u32 % width, i as u32 / width);
        pixel.copy_from_slice(&[((x / 7) % 256) as u8, ((y / 5) % 256) as u8, ((x ^ y) % 256) as u8, 255]);
    }
    pixels
}

fn native_buffer(pixels: &[u8], width: u32, height: u32) -> CVPixelBuffer {
    let surface = IOSurface::create(width as usize, height as usize, u32::from_be_bytes(*b"BGRA"), 4).unwrap();
    {
        let mut guard = surface.lock_read_write().unwrap();
        let stride = guard.bytes_per_row();
        // SAFETY: Fresh allocation, not wrapped or shared. Initialize all mapped bytes before
        // exposing this immutable synthetic buffer to VideoToolbox.
        let bytes = unsafe { guard.as_slice_mut() }.unwrap();
        bytes.fill(0);
        for y in 0..height as usize {
            let row = width as usize * 4;
            bytes[y * stride..y * stride + row].copy_from_slice(&pixels[y * row..(y + 1) * row]);
        }
    }
    CVPixelBuffer::create_with_io_surface(&surface).unwrap()
}

fn require_hardware(width: u32, height: u32, edge: u32, bitrate: u32) -> anyhow::Result<FrameEncoder> {
    let (w, h) = super::stream_size(width, height, edge);
    let mut encoder = FrameEncoder::new(60, bitrate, edge)?;
    encoder.backend = Backend::Hardware(Box::new(mac::HardwareEncoder::new(w, h, 60, bitrate)?));
    Ok(encoder)
}

#[test]
fn native_without_iosurface_keeps_a_decodable_software_fallback() {
    let (width, height) = (160u32, 90u32);
    let buffer = CVPixelBuffer::create(width as usize, height as usize, u32::from_be_bytes(*b"BGRA")).unwrap();
    {
        let mut guard = buffer.lock_read_write().unwrap();
        // SAFETY: Fresh standalone buffer; it has no shared aliases or native submissions.
        unsafe { guard.as_slice_mut() }.unwrap().fill(127);
    }
    // Apple may choose an IOSurface even without the requested attribute on a future OS.
    if buffer.is_backed_by_io_surface() {
        eprintln!("non-IOSurface fallback probe unavailable: OS allocated an IOSurface");
        return;
    }
    let raw = super::super::capture::Raw { bgra: Vec::new(), width, height, pixel_buffer: Some(buffer) };
    let mut encoder = FrameEncoder::new(30, 2_000_000, 160).unwrap();
    let first = encoder.encode_raw(&raw).unwrap().expect("software fallback frame");
    assert_eq!(encoder.backend_name(), "openh264-software");
    assert!(first.keyframe);
    let mut decoder = Decoder::new().unwrap();
    assert_eq!(decoder.decode(&first.data).unwrap().unwrap().dimensions(), (160, 90));
    encoder.force_keyframe();
    let fresh = encoder.encode_raw(&raw).unwrap().expect("forced fallback keyframe");
    let mut late = Decoder::new().unwrap();
    assert!(fresh.keyframe && late.decode(&fresh.data).unwrap().is_some());
}

#[test]
#[ignore = "requires a real hardware encoder, uses only synthetic pixels"]
fn hardware_native_annex_b_roundtrip() {
    let (width, height) = (640, 360);
    let pixels = synthetic(width, height);
    let buffer = native_buffer(&pixels, width, height);
    let raw = super::super::capture::Raw { bgra: Vec::new(), width, height, pixel_buffer: Some(buffer) };
    // 640->320 checks native transfer scaling rather than CPU BGRA resizing.
    let mut encoder = require_hardware(width, height, 320, 2_000_000).expect("actual hardware encoder");
    let frame = encoder.encode_raw(&raw).unwrap().expect("hardware output");
    assert_eq!(encoder.backend_name(), "videotoolbox-hardware");
    assert_eq!((frame.width, frame.height), (320, 180));
    assert!(frame.keyframe);
    let mut decoder = Decoder::new().unwrap();
    let decoded = decoder.decode(&frame.data).unwrap().expect("terminal decoded frame");
    assert_eq!(decoded.dimensions(), (320, 180));
    let delta = encoder.encode_raw(&raw).unwrap();
    if let Some(delta) = delta {
        assert!(!delta.keyframe);
    }
    encoder.force_keyframe();
    let forced = encoder.encode_raw(&raw).unwrap().expect("forced hardware keyframe");
    assert!(forced.keyframe);
    let mut late = Decoder::new().unwrap();
    assert!(late.decode(&forced.data).unwrap().is_some());
}

fn cpu_ms(system: &mut System) -> u64 {
    let pid = sysinfo::get_current_pid().unwrap();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        false,
        ProcessRefreshKind::nothing().with_cpu(),
    );
    system.process(pid).unwrap().accumulated_cpu_time()
}

fn percentile(times: &mut [f64], percentile: f64) -> f64 {
    times.sort_by(f64::total_cmp);
    times[((times.len() - 1) as f64 * percentile).ceil() as usize]
}

#[test]
#[ignore = "synthetic encoder A/B benchmark; run release, --test-threads=1, --nocapture"]
fn synthetic_encoder_ab() {
    let frames = 60usize;
    let mut results = Vec::new();
    // Native keeps pixel memory immutable and exercises the production direct-buffer path.
    // Its synthetic upload is setup, excluded from timings; no actual capture is claimed.
    for (width, height, edge, bitrate) in [(1920u32, 1080u32, 1920u32, 6_000_000u32), (3840, 2160, 3840, 20_000_000)] {
        let pixels = synthetic(width, height);
        let native = native_buffer(&pixels, width, height);
        for mode in ["openh264-bgra", "videotoolbox-bgra", "videotoolbox-native"] {
            let attempt = if mode == "openh264-bgra" {
                FrameEncoder::software(60, bitrate, edge)
            } else {
                require_hardware(width, height, edge, bitrate)
            };
            let mut encoder = match attempt {
                Ok(encoder) => encoder,
                Err(error) => {
                    results.push(
                        json!({"mode":mode,"source":[width,height],"result":"unavailable","reason":error.to_string()}),
                    );
                    continue;
                }
            };
            let raw =
                super::super::capture::Raw { bgra: Vec::new(), width, height, pixel_buffer: Some(native.clone()) };
            let encode = |encoder: &mut FrameEncoder| -> anyhow::Result<Option<Encoded>> {
                if mode == "videotoolbox-native" {
                    encoder.encode_raw(&raw)
                } else {
                    encoder.encode(&pixels, width, height, width * 4)
                }
            };
            // Warm initialization and code paths; independent decoding starts at a forced IDR.
            assert!(encode(&mut encoder).unwrap().is_some());
            encoder.force_keyframe();
            let mut process = System::new();
            let before_cpu = cpu_ms(&mut process);
            let started = Instant::now();
            let mut times = Vec::with_capacity(frames);
            let mut output = Vec::with_capacity(frames);
            for _ in 0..frames {
                let before = Instant::now();
                let frame = encode(&mut encoder).unwrap();
                times.push(before.elapsed().as_secs_f64() * 1000.0);
                if let Some(frame) = frame {
                    output.push(frame);
                }
            }
            let wall = started.elapsed().as_secs_f64();
            let cpu = cpu_ms(&mut process).saturating_sub(before_cpu);
            let mut decoder = Decoder::new().unwrap();
            let decode_started = Instant::now();
            let mut decoded = 0usize;
            let bytes: usize = output.iter().map(|frame| frame.data.len()).sum();
            for frame in &output {
                if let Some(decoded_frame) = decoder.decode(&frame.data).unwrap() {
                    assert_eq!(decoded_frame.dimensions(), (frame.width as usize, frame.height as usize));
                    decoded += 1;
                }
            }
            assert!(!output.is_empty() && decoded == output.len(), "every output must reach terminal decode");
            let p50 = percentile(&mut times, 0.50);
            let p95 = percentile(&mut times, 0.95);
            results.push(json!({"mode":mode,"source":[width,height],"encoded_size":[output[0].width,output[0].height],
                "backend":encoder.backend_name(),"submitted":frames,"output_frames":output.len(),"decoded_frames":decoded,
                "wall_seconds":wall,"output_fps":output.len() as f64/wall,"encode_p50_ms":p50,"encode_p95_ms":p95,
                "process_cpu_ms":cpu,"payload_bytes":bytes,"decode_seconds":decode_started.elapsed().as_secs_f64(),"result":"passed"}));
        }
    }
    let report = json!({"scope":"synthetic immutable buffers, warmed unpaced encoder-only timing; terminal decode outside timing; native upload excluded; no capture, network or display measurement", "results":results});
    println!("ENCODER_AB {}", serde_json::to_string(&report).unwrap());
    if let Some(path) = std::env::var_os("AI_REMOTE_BENCH_OUTPUT") {
        let path = std::path::PathBuf::from(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
