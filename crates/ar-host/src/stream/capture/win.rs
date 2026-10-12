//! Windows Graphics Capture on a D3D11 device of our own (video support, multithread protected),
//! so the encoder can use the same device. Every frame is scaled and converted BGRA -> NV12 in one
//! pass by the GPU's video processor into a small texture pool; the hardware encoder reads those
//! textures directly. Nothing is read back to the CPU on that path.
//!
//! Why not `windows-capture`'s handler: it creates its device without video support, and its
//! `Frame::buffer()` allocates a full-screen staging texture and maps it for every frame — at
//! 5120x2160 that was most of the 113-135 ms per frame measured on 2026-10-12 (see AGENTS.md).

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};

use anyhow::{Context, Result, anyhow, bail};
use windows::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Foundation::{HMODULE, LUID, RECT},
        Graphics::{
            Direct3D::{
                D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
            },
            Direct3D11::*,
            Dxgi::{
                Common::{
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                    DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
                },
                CreateDXGIFactory1, IDXGIAdapter, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1,
            },
            Gdi::HMONITOR,
        },
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
            RO_INIT_MULTITHREADED, RoInitialize,
        },
    },
    core::{IInspectable, Interface},
};

use super::{Raw, Slot};

/// Textures in flight at most: one being encoded, one waiting in the slot, one being written, one
/// spare for an encoder that holds its input until the output arrives.
const POOL: usize = 4;

/// The device capture, conversion and encoding share.
pub struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub luid: LUID,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext1,
}

// SAFETY: D3D11 devices are free-threaded; the immediate context is shared only after
// `SetMultithreadProtected(true)`, which serializes every context call (including video context
// calls) on the device's lock. Media Foundation requires the same protection for its own use.
unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

impl Gpu {
    /// A hardware device on the adapter that scans out `monitor` (no cross-adapter copy), or the
    /// default adapter when that cannot be found (e.g. an indirect display driver).
    fn for_monitor(monitor: HMONITOR) -> Result<Arc<Self>> {
        let adapter = adapter_for(monitor);
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let (mut device, mut context) = (None, None);
        // SAFETY: plain out-parameters; the adapter (if any) outlives the call.
        unsafe {
            D3D11CreateDevice(
                adapter.as_ref(),
                if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE },
                HMODULE::default(),
                flags,
                Some(&levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        }
        .context("create a D3D11 video device")?;
        let device = device.context("D3D11 returned no device")?;
        let context = context.context("D3D11 returned no context")?;
        // SAFETY: COM casts and a property set on the freshly created device.
        unsafe {
            let _ = device.cast::<ID3D11Multithread>()?.SetMultithreadProtected(true);
        }
        let luid = unsafe { device.cast::<IDXGIDevice>()?.GetAdapter()?.GetDesc()?.AdapterLuid };
        let video_device = device.cast::<ID3D11VideoDevice>().context("the GPU has no D3D11 video device")?;
        let video_context = context.cast::<ID3D11VideoContext1>().context("the GPU has no D3D11.1 video context")?;
        Ok(Arc::new(Self { device, context, luid, video_device, video_context }))
    }
}

fn adapter_for(monitor: HMONITOR) -> Option<IDXGIAdapter> {
    // SAFETY: DXGI enumeration with owned COM results only.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut i = 0;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            let adapter: IDXGIAdapter1 = adapter;
            let mut j = 0;
            while let Ok(output) = adapter.EnumOutputs(j) {
                if output.GetDesc().is_ok_and(|d| d.Monitor == monitor) {
                    return adapter.cast().ok();
                }
                j += 1;
            }
            i += 1;
        }
        None
    }
}

/// One NV12 frame at the stream size, owned by the pool; the encoder holds an `Arc` while it reads.
pub struct GpuTexture {
    pub texture: ID3D11Texture2D,
    pub gpu: Arc<Gpu>,
    pub width: u32,
    pub height: u32,
    view: ID3D11VideoProcessorOutputView,
}

// SAFETY: see `Gpu`; the texture is only written by the converter while no other `Arc` exists.
unsafe impl Send for GpuTexture {}
unsafe impl Sync for GpuTexture {}

struct Converter {
    gpu: Arc<Gpu>,
    input: (u32, u32),
    output: (u32, u32),
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    pool: Vec<Arc<GpuTexture>>,
}

// SAFETY: as for `Gpu`; the converter is only used under its mutex from the capture callback.
unsafe impl Send for Converter {}

impl Converter {
    fn new(gpu: Arc<Gpu>, input: (u32, u32), output: (u32, u32), fps: u32) -> Result<Self> {
        let rate = DXGI_RATIONAL { Numerator: fps, Denominator: 1 };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input.0,
            InputHeight: input.1,
            OutputFrameRate: rate,
            OutputWidth: output.0,
            OutputHeight: output.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        // SAFETY: video device calls with valid descriptors; results are owned COM objects.
        let (enumerator, processor) = unsafe {
            let enumerator =
                gpu.video_device.CreateVideoProcessorEnumerator(&desc).context("video processor enumerator")?;
            let support = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12).unwrap_or(0);
            if support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
                bail!("the GPU video processor cannot write NV12");
            }
            let processor = gpu.video_device.CreateVideoProcessor(&enumerator, 0).context("video processor")?;
            let vc = &gpu.video_context;
            vc.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            vc.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            vc.VideoProcessorSetStreamColorSpace1(&processor, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
            vc.VideoProcessorSetOutputColorSpace1(&processor, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            let src = RECT { left: 0, top: 0, right: input.0 as i32, bottom: input.1 as i32 };
            let dst = RECT { left: 0, top: 0, right: output.0 as i32, bottom: output.1 as i32 };
            vc.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src));
            vc.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst));
            vc.VideoProcessorSetOutputTargetRect(&processor, true, Some(&dst));
            (enumerator, processor)
        };
        let mut pool = Vec::with_capacity(POOL);
        for _ in 0..POOL {
            pool.push(Arc::new(texture(&gpu, &enumerator, output)?));
        }
        Ok(Self { gpu, input, output, enumerator, processor, pool })
    }

    /// Converts `source` into a free pool texture; `None` when every texture is still in use (the
    /// frame is skipped, as the newest-frame slot would have dropped it anyway).
    fn convert(&mut self, source: &ID3D11Texture2D) -> Result<Option<Arc<GpuTexture>>> {
        let Some(target) = self.pool.iter().find(|t| Arc::strong_count(t) == 1).cloned() else {
            return Ok(None);
        };
        let view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
            },
        };
        // SAFETY: the input view and stream reference live until `VideoProcessorBlt` returns; the
        // target texture has no other user (strong count was 1) while the blit is recorded.
        unsafe {
            let mut input = None;
            self.gpu
                .video_device
                .CreateVideoProcessorInputView(source, &self.enumerator, &view_desc, Some(&mut input))
                .context("video processor input view")?;
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(input),
                ..Default::default()
            };
            let mut streams = [stream];
            let result = self
                .gpu
                .video_context
                .VideoProcessorBlt(&self.processor, &target.view, 0, &streams)
                .context("convert the frame on the GPU");
            std::mem::ManuallyDrop::drop(&mut streams[0].pInputSurface);
            result?;
            // Start the GPU now; the encoder submits right behind it on the same device.
            self.gpu.context.Flush();
        }
        Ok(Some(target))
    }
}

fn texture(
    gpu: &Arc<Gpu>,
    enumerator: &ID3D11VideoProcessorEnumerator,
    (width, height): (u32, u32),
) -> Result<GpuTexture> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
    };
    // SAFETY: descriptor-driven creation with owned out-parameters.
    unsafe {
        let mut texture = None;
        gpu.device.CreateTexture2D(&desc, None, Some(&mut texture)).context("NV12 texture")?;
        let texture = texture.context("D3D11 returned no texture")?;
        let mut view = None;
        gpu.video_device
            .CreateVideoProcessorOutputView(&texture, enumerator, &view_desc, Some(&mut view))
            .context("video processor output view")?;
        Ok(GpuTexture { texture, gpu: gpu.clone(), width, height, view: view.context("no output view")? })
    }
}

/// Copies an NV12 texture to the CPU as tightly packed Y and interleaved UV planes; only the
/// software-encoder fallback uses this.
pub fn read_nv12(frame: &GpuTexture) -> Result<(Vec<u8>, Vec<u8>)> {
    let gpu = &frame.gpu;
    let desc = D3D11_TEXTURE2D_DESC {
        Width: frame.width,
        Height: frame.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let (w, h) = (frame.width as usize, frame.height as usize);
    // SAFETY: a private staging texture, mapped for reading only while the copies run.
    unsafe {
        let mut staging = None;
        gpu.device.CreateTexture2D(&desc, None, Some(&mut staging)).context("staging texture")?;
        let staging = staging.context("no staging texture")?;
        gpu.context.CopyResource(&staging, &frame.texture);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        gpu.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).context("map NV12")?;
        let pitch = mapped.RowPitch as usize;
        let base = mapped.pData as *const u8;
        let (mut y, mut uv) = (Vec::with_capacity(w * h), Vec::with_capacity(w * h / 2));
        for row in 0..h {
            y.extend_from_slice(std::slice::from_raw_parts(base.add(row * pitch), w));
        }
        let chroma = base.add(pitch * h);
        for row in 0..h / 2 {
            uv.extend_from_slice(std::slice::from_raw_parts(chroma.add(row * pitch), w));
        }
        gpu.context.Unmap(&staging, 0);
        Ok((y, uv))
    }
}

/// A running capture session; stops when dropped.
pub(super) struct NativeCapture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: i64,
    slot: Arc<Slot>,
}

impl Drop for NativeCapture {
    fn drop(&mut self) {
        self.slot.close();
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

struct Shared<T>(T);
// SAFETY: WinRT capture objects are agile; the D3D objects inside follow `Gpu`'s rule.
unsafe impl<T> Send for Shared<T> {}
unsafe impl<T> Sync for Shared<T> {}

impl<T> Shared<T> {
    /// Through a method, so a closure captures the whole wrapper rather than the inner field.
    fn get(&self) -> &T {
        &self.0
    }
}

/// Starts capturing `monitor`. `max_long_edge` is read for every frame, so the encoder can lower
/// it (a codec with a smaller size limit) without restarting the capture.
pub(super) fn start(
    monitor: HMONITOR,
    max_fps: u32,
    max_long_edge: Arc<AtomicU32>,
    slot: Arc<Slot>,
) -> Result<NativeCapture> {
    // SAFETY: initializes WinRT for this thread (an already-initialized thread is fine).
    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    anyhow::ensure!(
        GraphicsCaptureSession::IsSupported().unwrap_or(false),
        "Windows Graphics Capture is not supported here"
    );
    let gpu = Gpu::for_monitor(monitor)?;
    // SAFETY: COM interop factory and casts on owned objects.
    let (item, direct3d): (GraphicsCaptureItem, IDirect3DDevice) = unsafe {
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let item = interop.CreateForMonitor(monitor).context("capture item for the monitor")?;
        let dxgi: IDXGIDevice = gpu.device.cast()?;
        (item, CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?)
    };
    let size = item.Size()?;
    let format = DirectXPixelFormat::B8G8R8A8UIntNormalized;
    // Two buffers: one being converted while the compositor writes the next.
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(&direct3d, format, 2, size)?;
    let session = pool.CreateCaptureSession(&item)?;
    let fps = max_fps.clamp(1, 240);
    let converter: Arc<Mutex<Option<Converter>>> = Arc::default();
    let last_size = Arc::new(Mutex::new(size));
    let handler = {
        let (slot, gpu, direct3d) = (slot.clone(), Shared(gpu.clone()), Shared(direct3d.clone()));
        TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |pool, _| {
            let Some(pool) = pool.as_ref() else { return Ok(()) };
            let Ok(frame) = pool.TryGetNextFrame() else { return Ok(()) };
            let content = frame.ContentSize()?;
            {
                let mut last = last_size.lock().unwrap();
                if (content.Width, content.Height) != (last.Width, last.Height) {
                    // The display changed resolution: this frame still has the old surface size.
                    *last = content;
                    drop(frame);
                    pool.Recreate(direct3d.get(), format, 2, content)?;
                    return Ok(());
                }
            }
            let texture: ID3D11Texture2D =
                unsafe { frame.Surface()?.cast::<IDirect3DDxgiInterfaceAccess>()?.GetInterface()? };
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { texture.GetDesc(&mut desc) };
            let input = (desc.Width.min(content.Width as u32), desc.Height.min(content.Height as u32));
            let output = crate::stream::encode::stream_size(input.0, input.1, max_long_edge.load(Ordering::Relaxed));
            let mut converter = converter.lock().unwrap();
            if converter.as_ref().is_none_or(|c| c.input != input || c.output != output) {
                match Converter::new(gpu.get().clone(), input, output, fps) {
                    Ok(c) => *converter = Some(c),
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "GPU frame conversion unavailable");
                        slot.close();
                        return Ok(());
                    }
                }
            }
            let converted = converter.as_mut().map(|c| c.convert(&texture));
            drop(texture);
            drop(frame);
            match converted {
                Some(Ok(Some(gpu_frame))) => slot.put(Raw {
                    bgra: Vec::new(),
                    width: gpu_frame.width,
                    height: gpu_frame.height,
                    gpu: Some(gpu_frame),
                }),
                Some(Ok(None)) | None => {}
                Some(Err(e)) => tracing::warn!(error = %format!("{e:#}"), "live view frame conversion"),
            }
            Ok(())
        })
    };
    let token = pool.FrameArrived(&handler)?;
    let _ = session.SetIsCursorCaptureEnabled(true);
    // No yellow border on the person's screen (Windows 11; older builds ignore it).
    let _ = session.SetIsBorderRequired(false);
    // Without this Windows paces capture at 60 Hz; ask for every refresh up to `fps`.
    let _ = session.SetMinUpdateInterval(windows::Foundation::TimeSpan { Duration: 10_000_000 / fps as i64 });
    session.StartCapture().context("start Windows Graphics Capture")?;
    Ok(NativeCapture { pool, session, token, slot })
}

/// The monitor handle for `index` in `monitors()` order (`None`: the primary one).
pub(super) fn monitor(index: Option<usize>) -> Result<HMONITOR> {
    use windows_capture::monitor::Monitor;
    let monitor = match index {
        Some(i) => Monitor::from_index(i + 1),
        None => Monitor::primary(),
    }
    .map_err(|e| anyhow!("no such monitor: {e}"))?;
    Ok(HMONITOR(monitor.as_raw_hmonitor()))
}

/// GPU conversion + hardware encoding throughput on a synthetic, desktop-like picture that scrolls
/// by a few rows every frame, so every frame changes everywhere (the hardest common case). No
/// screen is read. Run by hand, in release:
///
/// `cargo test -p ar-host --release --lib gpu_encode_throughput -- --ignored --nocapture`
///
/// `AR_SIZES` (default `5120x2160,3840x2160,1920x1080`), `AR_CODECS` (`h264,hevc,av1`), `AR_FPS`
/// (120), `AR_KBPS` (80000), `AR_FRAMES` (240). Streams go to `target/stream-bench/` for decoding
/// elsewhere (the desktop decoder check); H.264 is also decoded here.
#[cfg(test)]
mod bench {
    use std::time::{Duration, Instant};

    use windows::Win32::{
        Foundation::POINT,
        Graphics::{
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint},
        },
    };

    use super::*;
    use crate::stream::encode::{Codec, Encoded, FrameEncoder};

    fn env(key: &str, default: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| default.into())
    }

    /// Text-like glyph rows, window bars and a photo-like gradient with grain, BGRA.
    fn desktop_like(width: u32, height: u32) -> Vec<u8> {
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

    fn bgra_texture(gpu: &Gpu, width: u32, height: u32, pixels: Option<&[u8]>) -> ID3D11Texture2D {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let data = pixels.map(|p| D3D11_SUBRESOURCE_DATA {
            pSysMem: p.as_ptr().cast(),
            SysMemPitch: width * 4,
            SysMemSlicePitch: 0,
        });
        let mut texture = None;
        unsafe { gpu.device.CreateTexture2D(&desc, data.as_ref().map(|d| d as *const _), Some(&mut texture)).unwrap() };
        texture.unwrap()
    }

    fn keep(frame: Encoded, stream: &mut Vec<u8>) -> (Codec, usize) {
        stream.push(frame.keyframe as u8);
        stream.extend_from_slice(&(frame.data.len() as u32).to_le_bytes());
        stream.extend_from_slice(&frame.data);
        (frame.codec, frame.data.len())
    }

    fn decode_h264(stream: &[u8]) -> usize {
        let mut decoder = openh264::decoder::Decoder::new().unwrap();
        let (mut ok, mut at) = (0, 0);
        while at < stream.len() {
            let len = u32::from_le_bytes(stream[at + 1..at + 5].try_into().unwrap()) as usize;
            if decoder.decode(&stream[at + 5..at + 5 + len]).is_ok_and(|p| p.is_some()) {
                ok += 1;
            }
            at += 5 + len;
        }
        ok
    }

    /// Opening encoders one after another must stay fast: each one has to release its GPU
    /// session when dropped (a leaked session made the next one wait seconds, then fail).
    #[test]
    #[ignore]
    fn hardware_encoders_open_and_close_repeatedly() {
        let _ = tracing_subscriber::fmt().with_env_filter(env("RUST_LOG", "info")).with_test_writer().try_init();
        let gpu =
            Gpu::for_monitor(unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }).unwrap();
        let (w, h) = (3840, 2160);
        let screen = bgra_texture(&gpu, w, h, Some(&desktop_like(w, h)));
        let mut converter = Converter::new(gpu.clone(), (w, h), (w, h), 60).unwrap();
        let mut slowest = Duration::ZERO;
        for round in 0..12 {
            let codec = [Codec::H264, Codec::Hevc, Codec::Av1][round % 3];
            let mut encoder = FrameEncoder::new(60, 20_000_000, w).unwrap().with_codecs(&[codec]);
            let t = Instant::now();
            let frame = converter.convert(&screen).unwrap().unwrap();
            let raw = super::super::Raw { bgra: Vec::new(), width: w, height: h, gpu: Some(frame) };
            let out = encoder.encode_raw(&raw).unwrap().or_else(|| encoder.poll().unwrap());
            let took = t.elapsed();
            slowest = slowest.max(took);
            println!("round {round}: {} first frame in {:.0} ms", encoder.backend_name(), took.as_secs_f64() * 1000.0);
            assert!(out.is_some_and(|f| f.keyframe && f.codec == codec), "round {round}: no hardware keyframe");
        }
        assert!(slowest < Duration::from_secs(1), "an encoder took {slowest:?} to start");
    }

    #[test]
    #[ignore]
    fn gpu_encode_throughput() {
        let _ = tracing_subscriber::fmt().with_env_filter(env("RUST_LOG", "info")).with_test_writer().try_init();
        let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
        let gpu = Gpu::for_monitor(monitor).unwrap();
        let fps: u32 = env("AR_FPS", "120").parse().unwrap();
        let kbps: u32 = env("AR_KBPS", "80000").parse().unwrap();
        let frames: u32 = env("AR_FRAMES", "240").parse().unwrap();
        let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/stream-bench");
        std::fs::create_dir_all(&out_dir).unwrap();
        for size in env("AR_SIZES", "5120x2160,3840x2160,1920x1080").split(',') {
            let (w, h) =
                size.split_once('x').map(|(a, b)| (a.parse::<u32>().unwrap(), b.parse::<u32>().unwrap())).unwrap();
            let tall = bgra_texture(&gpu, w, h * 2, Some(&desktop_like(w, h * 2)));
            let screen = bgra_texture(&gpu, w, h, None);
            for codec in env("AR_CODECS", "h264,hevc,av1").split(',').filter_map(Codec::parse) {
                let mut converter = Converter::new(gpu.clone(), (w, h), (w, h), fps).unwrap();
                let mut encoder = FrameEncoder::new(fps, kbps * 1000, w.max(h)).unwrap().with_codecs(&[codec]);
                let (mut times, mut stream, mut outputs, mut bytes, mut actual) =
                    (Vec::new(), Vec::new(), 0usize, 0usize, None);
                let started = Instant::now();
                for f in 0..frames {
                    // Scroll 7 rows a frame through the tall picture.
                    let top = (f * 7) % h;
                    let region = D3D11_BOX { left: 0, top, front: 0, right: w, bottom: top + h, back: 1 };
                    unsafe { gpu.context.CopySubresourceRegion(&screen, 0, 0, 0, 0, &tall, 0, Some(&region)) };
                    // A viewer joining halfway asks for a keyframe.
                    if f == frames / 2 {
                        encoder.force_keyframe();
                    }
                    let t = Instant::now();
                    let Some(frame) = converter.convert(&screen).unwrap() else { continue };
                    let raw = super::super::Raw { bgra: Vec::new(), width: w, height: h, gpu: Some(frame) };
                    let mut ready = encoder.encode_raw(&raw).unwrap();
                    times.push(t.elapsed());
                    while let Some(out) = ready.take().or_else(|| encoder.poll().unwrap()) {
                        let (c, n) = keep(out, &mut stream);
                        (actual, outputs, bytes) = (Some(c), outputs + 1, bytes + n);
                    }
                }
                let elapsed = started.elapsed();
                let drain = Instant::now();
                while drain.elapsed() < Duration::from_millis(300) {
                    if let Some(out) = encoder.poll().unwrap() {
                        let (c, n) = keep(out, &mut stream);
                        (actual, outputs, bytes) = (Some(c), outputs + 1, bytes + n);
                    }
                }
                times.sort();
                let pct = |p: f64| times[((times.len() - 1) as f64 * p) as usize].as_secs_f64() * 1000.0;
                let actual = actual.map(|c| c.name()).unwrap_or("none");
                let name = format!("{actual}-{w}x{h}.bin");
                std::fs::write(out_dir.join(&name), &stream).unwrap();
                let check = if actual == "h264" {
                    format!("{} decoded by OpenH264", decode_h264(&stream))
                } else {
                    format!("saved {name}")
                };
                println!(
                    "{w}x{h} want {}: {} | {} in, {outputs} out | p50 {:.2} ms p95 {:.2} ms max {:.2} ms | {:.0} fps back-to-back | {:.1} Mbit/s at {fps} fps | {check}",
                    codec.name(),
                    encoder.backend_name(),
                    times.len(),
                    pct(0.5),
                    pct(0.95),
                    pct(1.0),
                    times.len() as f64 / elapsed.as_secs_f64(),
                    bytes as f64 * 8.0 * fps as f64 / outputs.max(1) as f64 / 1e6,
                );
            }
        }
    }
}
