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

use super::pattern::{desktop_like, scroll_top, still_after};
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
    pub(super) fn for_monitor(monitor: HMONITOR) -> Result<Arc<Self>> {
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

pub(super) struct Converter {
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
    pub(super) fn new(gpu: Arc<Gpu>, input: (u32, u32), output: (u32, u32), fps: u32) -> Result<Self> {
        Self::rotated(gpu, input, output, fps, D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY)
    }

    /// Like [`Converter::new`], and turns every frame clockwise by `rotation` on the way: `input` is
    /// the source texture's size, `output` the size after turning and scaling (a quarter turn swaps
    /// width and height). Desktop Duplication delivers a rotated display's image unrotated.
    pub(super) fn rotated(
        gpu: Arc<Gpu>,
        input: (u32, u32),
        output: (u32, u32),
        fps: u32,
        rotation: D3D11_VIDEO_PROCESSOR_ROTATION,
    ) -> Result<Self> {
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
            if rotation != D3D11_VIDEO_PROCESSOR_ROTATION_IDENTITY {
                let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
                enumerator.GetVideoProcessorCaps(&mut caps).context("video processor caps")?;
                if caps.FeatureCaps & D3D11_VIDEO_PROCESSOR_FEATURE_CAPS_ROTATION.0 as u32 == 0 {
                    bail!("the GPU video processor cannot rotate frames");
                }
                vc.VideoProcessorSetStreamRotation(&processor, 0, true, rotation);
            }
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
    pub(super) fn convert(&mut self, source: &ID3D11Texture2D) -> Result<Option<Arc<GpuTexture>>> {
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
    draw_pointer: bool,
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
            let captured = std::time::Instant::now();
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
                    captured,
                    gpu: Some(gpu_frame),
                }),
                Some(Ok(None)) | None => {}
                Some(Err(e)) => tracing::warn!(error = %format!("{e:#}"), "live view frame conversion"),
            }
            Ok(())
        })
    };
    let token = pool.FrameArrived(&handler)?;
    let _ = session.SetIsCursorCaptureEnabled(draw_pointer);
    // No yellow border on the person's screen (Windows 11; older builds ignore it).
    let _ = session.SetIsBorderRequired(false);
    // Without this Windows paces capture at 60 Hz, and a minimum interval of exactly 1/fps rounds
    // up to whole refreshes (60 fps on a 100 Hz display gave 50). Half the interval lets every
    // refresh that could be needed through; the encode loop's pacer keeps `fps` exactly.
    let _ = session.SetMinUpdateInterval(windows::Foundation::TimeSpan { Duration: 10_000_000 / (2 * fps as i64) });
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

fn bgra_texture(gpu: &Gpu, width: u32, height: u32, pixels: Option<&[u8]>) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
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
    // SAFETY: the initial data (if any) outlives the call and matches the descriptor's size.
    unsafe { gpu.device.CreateTexture2D(&desc, data.as_ref().map(|d| d as *const _), Some(&mut texture)) }
        .context("BGRA texture")?;
    texture.context("D3D11 returned no texture")
}

/// A running test pattern; stops when dropped.
pub(super) struct TestPattern {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for TestPattern {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(super) fn start_test_pattern(
    (width, height): (u32, u32),
    max_fps: u32,
    max_long_edge: Arc<AtomicU32>,
    slot: Arc<Slot>,
) -> Result<TestPattern> {
    use windows::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint},
    };
    // SAFETY: a plain query for the primary monitor's handle.
    let gpu = Gpu::for_monitor(unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) })?;
    let tall = bgra_texture(&gpu, width, height * 2, Some(&desktop_like(width, height * 2)))?;
    let screen = bgra_texture(&gpu, width, height, None)?;
    let fps = max_fps.clamp(1, 240);
    // Frames to show before the picture stands still, if it ever does.
    let still_frames = still_after(fps);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread = {
        let (stop, gpu, tall, screen) = (stop.clone(), Shared(gpu), Shared(tall), Shared(screen));
        std::thread::Builder::new().name("ai-remote-test-pattern".into()).spawn(move || {
            let interval = std::time::Duration::from_secs_f64(1.0 / f64::from(fps));
            let mut next = std::time::Instant::now();
            let mut converter: Option<Converter> = None;
            let mut frame = 0u32;
            while !stop.load(Ordering::Relaxed) {
                // A still desktop: no new frames at all.
                if still_frames.is_some_and(|n| frame >= n) {
                    std::thread::sleep(interval);
                    continue;
                }
                let top = scroll_top(frame.wrapping_mul(7), height);
                frame = frame.wrapping_add(1);
                let region = D3D11_BOX { left: 0, top, front: 0, right: width, bottom: top + height, back: 1 };
                // SAFETY: copies between two private textures of the same device.
                unsafe {
                    gpu.get().context.CopySubresourceRegion(screen.get(), 0, 0, 0, 0, tall.get(), 0, Some(&region))
                };
                let output = crate::stream::encode::stream_size(width, height, max_long_edge.load(Ordering::Relaxed));
                if converter.as_ref().is_none_or(|c| c.output != output) {
                    match Converter::new(gpu.get().clone(), (width, height), output, fps) {
                        Ok(c) => converter = Some(c),
                        Err(e) => {
                            tracing::warn!(error = %format!("{e:#}"), "test pattern conversion");
                            slot.close();
                            return;
                        }
                    }
                }
                if let Some(Ok(Some(converted))) = converter.as_mut().map(|c| c.convert(screen.get())) {
                    slot.put(Raw {
                        bgra: Vec::new(),
                        width: converted.width,
                        height: converted.height,
                        captured: std::time::Instant::now(),
                        gpu: Some(converted),
                    });
                }
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
        Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint},
    };

    use super::*;
    use crate::stream::encode::{Codec, Encoded, FrameEncoder};

    fn env(key: &str, default: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| default.into())
    }

    /// Appends a frame to the saved stream; returns its codec and its size unless it is a keyframe.
    fn keep(frame: Encoded, stream: &mut Vec<u8>) -> (Codec, usize) {
        let delta = if frame.keyframe { 0 } else { frame.data.len() };
        stream.push(frame.keyframe as u8);
        stream.extend_from_slice(&(frame.data.len() as u32).to_le_bytes());
        stream.extend_from_slice(&frame.data);
        (frame.codec, delta)
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

    /// The stream keeps to the requested bitrate on full-screen motion, fed at its real frame rate,
    /// at the display's size and scaled down from 5K (the usual presets). H.264 and HEVC keep to it
    /// themselves and must stay in use; NVENC's AV1 does not (11-321 Mbit/s for 12 requested,
    /// whatever the QP ceiling or rate-control mode), and the rate guard must move such a stream to
    /// the next codec within its window. Measured over the last 1.5 s of 4 s. `AR_CODECS` /
    /// `AR_KBPS` narrow the run.
    #[test]
    #[ignore]
    fn streams_keep_to_the_requested_bitrate() {
        let _ = tracing_subscriber::fmt().with_env_filter(env("RUST_LOG", "info")).with_test_writer().try_init();
        let gpu =
            Gpu::for_monitor(unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }).unwrap();
        let codecs: Vec<Codec> = env("AR_CODECS", "h264,hevc,av1").split(',').filter_map(Codec::parse).collect();
        let rates: Vec<u32> = env("AR_KBPS", "12000,40000").split(',').map(|k| k.parse().unwrap()).collect();
        let mut failures = Vec::new();
        // (source, stream size, fps): native 4K and 5K, and 5K scaled to the 1080p and 4K presets.
        let cases = [
            ((3840u32, 2160u32), (3840u32, 2160u32), 60u32),
            ((5120, 2160), (5120, 2160), 100),
            ((5120, 2160), (1920, 810), 60),
            ((5120, 2160), (3840, 1620), 60),
        ];
        for ((w, h), (sw, sh), fps) in cases {
            let tall = bgra_texture(&gpu, w, h * 2, Some(&desktop_like(w, h * 2))).unwrap();
            let screen = bgra_texture(&gpu, w, h, None).unwrap();
            for &codec in &codecs {
                if sw.max(sh) > codec.max_long_edge() {
                    continue;
                }
                // What a viewer that decodes everything offers, led by the codec under test.
                let mut offered = vec![codec];
                offered.extend([Codec::Hevc, Codec::H264].into_iter().filter(|c| *c != codec));
                for &kbps in &rates {
                    let mut converter = Converter::new(gpu.clone(), (w, h), (sw, sh), fps).unwrap();
                    let mut encoder = FrameEncoder::new(fps, kbps * 1000, sw.max(sh)).unwrap().with_codecs(&offered);
                    let (mut bytes, mut frames, mut last) = (0usize, 0usize, codec);
                    let total = fps * 4;
                    let start = Instant::now();
                    for f in 0..total {
                        let top = scroll_top(f * 7, h);
                        let region = D3D11_BOX { left: 0, top, front: 0, right: w, bottom: top + h, back: 1 };
                        unsafe { gpu.context.CopySubresourceRegion(&screen, 0, 0, 0, 0, &tall, 0, Some(&region)) };
                        let frame = converter.convert(&screen).unwrap().unwrap();
                        let raw = super::super::Raw {
                            bgra: Vec::new(),
                            width: sw,
                            height: sh,
                            captured: Instant::now(),
                            gpu: Some(frame),
                        };
                        let mut ready = encoder.encode_raw(&raw).unwrap();
                        while let Some(out) = ready.take().or_else(|| encoder.poll().unwrap()) {
                            last = out.codec;
                            if f >= total - fps * 3 / 2 && !out.keyframe {
                                bytes += out.data.len();
                                frames += 1;
                            }
                        }
                        let due = start + Duration::from_secs_f64(f64::from(f + 1) / f64::from(fps));
                        std::thread::sleep(due.saturating_duration_since(Instant::now()));
                    }
                    let mbps = bytes as f64 * 8.0 * f64::from(fps) / frames.max(1) as f64 / 1e6;
                    let target = f64::from(kbps) / 1000.0;
                    let case = format!("{w}x{h} -> {sw}x{sh} @{fps} {}", codec.name());
                    println!("{case}: {target:.0} Mbit/s requested, {mbps:.1} produced, ending on {}", last.name());
                    if mbps > target * 1.15 {
                        failures.push(format!("{case}: {mbps:.1}/{target:.0} Mbit/s"));
                    }
                    if codec != Codec::Av1 && last != codec {
                        failures.push(format!("{case}: moved off {} to {}", codec.name(), last.name()));
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{failures:?}");
    }

    /// A bitrate change from the viewer takes effect on the running encoder, down and back up, for
    /// the codecs that keep to a bitrate at all (H.264, HEVC), without a new keyframe. Each phase
    /// runs 4 s: its last 2 s must settle within 1.15x of the new rate, and no 2 s of it may run
    /// over 1.2x (the rate guard would take that for a runaway encoder and change codec).
    #[test]
    #[ignore]
    fn bitrate_changes_take_effect_mid_stream() {
        let _ = tracing_subscriber::fmt().with_env_filter(env("RUST_LOG", "info")).with_test_writer().try_init();
        let gpu =
            Gpu::for_monitor(unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }).unwrap();
        let (w, h, fps) = (3840u32, 2160u32, 60u32);
        let tall = bgra_texture(&gpu, w, h * 2, Some(&desktop_like(w, h * 2))).unwrap();
        let screen = bgra_texture(&gpu, w, h, None).unwrap();
        let mut failures = Vec::new();
        for codec in [Codec::H264, Codec::Hevc] {
            let mut converter = Converter::new(gpu.clone(), (w, h), (w, h), fps).unwrap();
            let mut encoder = FrameEncoder::new(fps, 40_000_000, w).unwrap().with_codecs(&[codec]);
            // A cut, the controller's 15% steps back up, and a jump.
            let phases = [40_000_000u32, 10_000_000, 11_500_000, 25_000_000];
            let (mut f, mut keyframes) = (0u32, 0usize);
            for &bps in &phases {
                encoder.set_bitrate(bps);
                let target = f64::from(bps) / 1e6;
                // Non-keyframe bytes per half second.
                let mut bins = [0usize; 8];
                let start = Instant::now();
                for i in 0..fps * 4 {
                    let top = scroll_top(f * 7, h);
                    f += 1;
                    let region = D3D11_BOX { left: 0, top, front: 0, right: w, bottom: top + h, back: 1 };
                    unsafe { gpu.context.CopySubresourceRegion(&screen, 0, 0, 0, 0, &tall, 0, Some(&region)) };
                    let frame = converter.convert(&screen).unwrap().unwrap();
                    let raw = super::super::Raw {
                        bgra: Vec::new(),
                        width: w,
                        height: h,
                        captured: Instant::now(),
                        gpu: Some(frame),
                    };
                    let mut ready = encoder.encode_raw(&raw).unwrap();
                    while let Some(out) = ready.take().or_else(|| encoder.poll().unwrap()) {
                        assert_eq!(out.codec, codec);
                        keyframes += usize::from(out.keyframe);
                        if !out.keyframe {
                            bins[(i / (fps / 2)) as usize] += out.data.len();
                        }
                    }
                    let due = start + Duration::from_secs_f64(f64::from(i + 1) / f64::from(fps));
                    std::thread::sleep(due.saturating_duration_since(Instant::now()));
                }
                let mbps: Vec<f64> = bins.iter().map(|b| *b as f64 * 8.0 * 2.0 / 1e6).collect();
                let settled = mbps[4..].iter().sum::<f64>() / 4.0;
                let worst_2s = mbps.windows(4).map(|w| w.iter().sum::<f64>() / 4.0).fold(0.0, f64::max);
                println!(
                    "{} set {target} Mbit/s: per 0.5 s {mbps:.1?}, last 2 s {settled:.1}, worst 2 s {worst_2s:.1}",
                    codec.name()
                );
                if settled > target * 1.15 || settled < target * 0.4 {
                    failures.push(format!("{} at {target}: settled at {settled:.1}", codec.name()));
                }
                if worst_2s > target * 1.2 {
                    failures.push(format!("{} at {target}: {worst_2s:.1} over 2 s", codec.name()));
                }
            }
            if keyframes != 1 {
                failures.push(format!("{}: {keyframes} keyframes (a change must not add any)", codec.name()));
            }
        }
        assert!(failures.is_empty(), "{failures:?}");
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
        let screen = bgra_texture(&gpu, w, h, Some(&desktop_like(w, h))).unwrap();
        let mut converter = Converter::new(gpu.clone(), (w, h), (w, h), 60).unwrap();
        let mut slowest = Duration::ZERO;
        for round in 0..12 {
            let codec = [Codec::H264, Codec::Hevc, Codec::Av1][round % 3];
            let mut encoder = FrameEncoder::new(60, 20_000_000, w).unwrap().with_codecs(&[codec]);
            let t = Instant::now();
            let frame = converter.convert(&screen).unwrap().unwrap();
            let raw =
                super::super::Raw { bgra: Vec::new(), width: w, height: h, captured: Instant::now(), gpu: Some(frame) };
            let out = encoder.encode_raw(&raw).unwrap().or_else(|| encoder.poll().unwrap());
            let took = t.elapsed();
            slowest = slowest.max(took);
            println!("round {round}: {} first frame in {:.0} ms", encoder.backend_name(), took.as_secs_f64() * 1000.0);
            assert!(out.is_some_and(|f| f.keyframe && f.codec == codec), "round {round}: no hardware keyframe");
        }
        assert!(slowest < Duration::from_secs(1), "an encoder took {slowest:?} to start");
    }

    /// An encoder's output, kept for scoring.
    #[derive(Default)]
    struct Sink {
        stream: Vec<u8>,
        outputs: u32,
        keyframes: u32,
        actual: Option<Codec>,
        delta_bytes: usize,
        deltas: usize,
        /// Sizes of the non-key frames.
        sizes: Vec<usize>,
    }

    impl Sink {
        fn take(&mut self, out: Encoded) {
            self.actual = Some(out.codec);
            self.outputs += 1;
            self.keyframes += u32::from(out.keyframe);
            if !out.keyframe {
                self.delta_bytes += out.data.len();
                self.deltas += 1;
                self.sizes.push(out.data.len());
            }
            self.stream.extend_from_slice(&out.data);
        }
    }

    /// Streams for the objective quality comparison (`tools/stream-quality.py` scores them with
    /// ffmpeg: PSNR, SSIM, VMAF). The scrolling desktop-like picture at its native size, fed at its
    /// real frame rate (rate control works from timestamps), for every codec and bitrate; with
    /// `AR_RC=both` also in low-delay VBR, to see what the CBR that adaptive bitrate needs costs.
    /// Writes `target/stream-quality/`: the tall source picture once per size (`ref-WxH.bgra`, the
    /// reference: frame n shows its rows from `scroll_top(7 n)`), one elementary stream per case
    /// and `cases.json`. `AR_SIZES` (`1920x1080,3840x2160`), `AR_CODECS`, `AR_FPS` (60),
    /// `AR_FRAMES` (300), `AR_KBPS` (per size: 1080p 6000,12000; larger 20000,40000).
    /// `AR_VARIANTS` tunes the encoder, `;`-separated: `default`, `qvs=N` (quality vs speed,
    /// 0..100), `vbvms=N` (rate-control buffer in ms of the bitrate), `early=0` (the buffer size only
    /// after the media types, where NVENC ignores it), `edge=N` (stream scaled to this long edge),
    /// `scroll=N` (rows a frame, 0 a still screen), `boost=K` (keyframes at K times the bitrate),
    /// `rekey=N` (a keyframe asked for at frame N), combinable as `qvs=100,vbvms=125`. Each case
    /// also records the time `encode_raw` took and how bursty the frame sizes are.
    #[test]
    #[ignore]
    fn encoded_quality() {
        use std::sync::atomic::Ordering;

        use crate::stream::encode::LOW_DELAY_VBR_FOR_TESTS;
        let _ = tracing_subscriber::fmt().with_env_filter(env("RUST_LOG", "warn")).with_test_writer().try_init();
        let gpu =
            Gpu::for_monitor(unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }).unwrap();
        let fps: u32 = env("AR_FPS", "60").parse().unwrap();
        let frames: u32 = env("AR_FRAMES", "300").parse().unwrap();
        let modes: &[bool] = match env("AR_RC", "cbr").as_str() {
            "both" => &[false, true],
            "vbr" => &[true],
            _ => &[false],
        };
        let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/stream-quality");
        std::fs::create_dir_all(&out_dir).unwrap();
        let mut cases = Vec::new();
        for size in env("AR_SIZES", "1920x1080,3840x2160").split(',') {
            let (w, h) =
                size.split_once('x').map(|(a, b)| (a.parse::<u32>().unwrap(), b.parse::<u32>().unwrap())).unwrap();
            let source = desktop_like(w, h * 2);
            std::fs::write(out_dir.join(format!("ref-{w}x{h}.bgra")), &source).unwrap();
            let tall = bgra_texture(&gpu, w, h * 2, Some(&source)).unwrap();
            let screen = bgra_texture(&gpu, w, h, None).unwrap();
            let rates = std::env::var("AR_KBPS")
                .unwrap_or_else(|_| if w.max(h) <= 1920 { "6000,12000" } else { "20000,40000" }.into());
            let variants = env("AR_VARIANTS", "default");
            for (&vbr, variant) in modes.iter().flat_map(|m| variants.split(';').map(move |v| (m, v))) {
                LOW_DELAY_VBR_FOR_TESTS.store(vbr, Ordering::Relaxed);
                let knob = |name: &str| {
                    variant.split(',').find_map(|kv| kv.strip_prefix(name)?.strip_prefix('=')?.parse::<u32>().ok())
                };
                crate::stream::encode::tuning::set(knob("qvs"), knob("vbvms"), knob("early").map(|v| v != 0));
                for codec in env("AR_CODECS", "h264,hevc,av1").split(',').filter_map(Codec::parse) {
                    for kbps in rates.split(',').map(|k| k.parse::<u32>().unwrap()) {
                        // `edge=N` streams a scaled-down picture (scored against the full-size source).
                        let edge = knob("edge").unwrap_or(w.max(h)).min(w.max(h));
                        let (sw, sh) = crate::stream::encode::stream_size(w, h, edge);
                        let mut converter = Converter::new(gpu.clone(), (w, h), (sw, sh), fps).unwrap();
                        // `scroll=N` rows a frame (0: a still screen); `boost=K` runs keyframes at K times
                        // the bitrate, back to it right after; `rekey=N` asks for a keyframe at frame N.
                        let rows = knob("scroll").unwrap_or(7);
                        let boost = knob("boost").unwrap_or(1).max(1);
                        let rekey = knob("rekey");
                        let mut encoder =
                            FrameEncoder::new(fps, kbps * 1000 * boost, edge).unwrap().with_codecs(&[codec]);
                        let mut sink = Sink::default();
                        let mut times = Vec::with_capacity(frames as usize);
                        let start = Instant::now();
                        for f in 0..frames {
                            if Some(f) == rekey {
                                encoder.set_bitrate(kbps * 1000 * boost);
                                encoder.force_keyframe();
                            }
                            let top = scroll_top(f * rows, h);
                            let region = D3D11_BOX { left: 0, top, front: 0, right: w, bottom: top + h, back: 1 };
                            unsafe { gpu.context.CopySubresourceRegion(&screen, 0, 0, 0, 0, &tall, 0, Some(&region)) };
                            let frame = converter.convert(&screen).unwrap().unwrap();
                            let raw = super::super::Raw {
                                bgra: Vec::new(),
                                width: sw,
                                height: sh,
                                captured: Instant::now(),
                                gpu: Some(frame),
                            };
                            let t = Instant::now();
                            let mut ready = encoder.encode_raw(&raw).unwrap();
                            if f > 0 {
                                times.push(t.elapsed().as_secs_f64() * 1000.0);
                            }
                            while let Some(out) = ready.take().or_else(|| encoder.poll().unwrap()) {
                                if out.keyframe && boost > 1 {
                                    encoder.set_bitrate(kbps * 1000);
                                }
                                sink.take(out);
                            }
                            let due = start + Duration::from_secs_f64(f64::from(f + 1) / f64::from(fps));
                            std::thread::sleep(due.saturating_duration_since(Instant::now()));
                        }
                        let drain = Instant::now();
                        while sink.outputs < frames && drain.elapsed() < Duration::from_millis(500) {
                            match encoder.poll().unwrap() {
                                Some(out) => sink.take(out),
                                None => std::thread::sleep(Duration::from_millis(2)),
                            }
                        }
                        let Sink { stream, outputs, keyframes, actual, delta_bytes, deltas, mut sizes } = sink;
                        times.sort_by(f64::total_cmp);
                        sizes.sort_unstable();
                        let pct = |v: &[f64], p: f64| v[((v.len() - 1) as f64 * p) as usize];
                        let mean_size = delta_bytes as f64 / deltas.max(1) as f64;
                        let size_pct = |p: f64| sizes[((sizes.len() - 1) as f64 * p) as usize] as f64 / mean_size;
                        let tag = if variant == "default" {
                            String::new()
                        } else {
                            format!("-{}", variant.replace([',', '='], ""))
                        };
                        let actual = actual.map(|c| c.name()).unwrap_or("none");
                        let rc = if vbr { "vbr" } else { "cbr" };
                        let ext = match actual {
                            "hevc" => "hevc",
                            "av1" => "obu",
                            _ => "h264",
                        };
                        let file = format!("{actual}-{w}x{h}-{kbps}-{rc}{tag}.{ext}");
                        std::fs::write(out_dir.join(&file), &stream).unwrap();
                        let mbps = delta_bytes as f64 * 8.0 * f64::from(fps) / deltas.max(1) as f64 / 1e6;
                        println!(
                            "{w}x{h} {actual} {rc} {variant} {kbps} kbit/s: {outputs}/{frames} frames, {keyframes} keyframes, \
                             non-key {mbps:.1} Mbit/s, encode p50 {:.2} p95 {:.2} ms, frame size p95 {:.2}x max {:.2}x \
                             mean -> {file}",
                            pct(&times, 0.5),
                            pct(&times, 0.95),
                            size_pct(0.95),
                            size_pct(1.0)
                        );
                        cases.push(serde_json::json!({
                            "file": file, "codec": actual, "width": sw, "height": sh, "source": [w, h], "fps": fps, "kbps": kbps,
                            "rate_control": rc, "variant": variant, "frames": frames, "outputs": outputs, "keyframes": keyframes,
                            "encode_ms": {"p50": pct(&times, 0.5), "p95": pct(&times, 0.95)},
                            "frame_size_vs_mean": {"p95": size_pct(0.95), "max": size_pct(1.0)},
                            "bytes": stream.len(), "delta_mbps": mbps, "backend": encoder.backend_name(),
                            "reference": format!("ref-{w}x{h}.bgra"), "scroll_rows": rows,
                        }));
                    }
                }
            }
        }
        LOW_DELAY_VBR_FOR_TESTS.store(false, Ordering::Relaxed);
        crate::stream::encode::tuning::set(None, None, None);
        std::fs::write(out_dir.join("cases.json"), serde_json::to_string_pretty(&cases).unwrap()).unwrap();
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
        let scroll: u32 = env("AR_SCROLL", "7").parse().unwrap();
        let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/stream-bench");
        std::fs::create_dir_all(&out_dir).unwrap();
        for size in env("AR_SIZES", "5120x2160,3840x2160,1920x1080").split(',') {
            let (w, h) =
                size.split_once('x').map(|(a, b)| (a.parse::<u32>().unwrap(), b.parse::<u32>().unwrap())).unwrap();
            let tall = bgra_texture(&gpu, w, h * 2, Some(&desktop_like(w, h * 2))).unwrap();
            let screen = bgra_texture(&gpu, w, h, None).unwrap();
            for codec in env("AR_CODECS", "h264,hevc,av1").split(',').filter_map(Codec::parse) {
                let mut converter = Converter::new(gpu.clone(), (w, h), (w, h), fps).unwrap();
                let mut encoder = FrameEncoder::new(fps, kbps * 1000, w.max(h)).unwrap().with_codecs(&[codec]);
                let (mut times, mut stream, mut outputs, mut bytes, mut actual) =
                    (Vec::new(), Vec::new(), 0usize, 0usize, None);
                let mut deltas = 0usize;
                let started = Instant::now();
                for f in 0..frames {
                    // Scroll `AR_SCROLL` rows a frame (7; 0 is a still screen) through the tall picture.
                    let top = scroll_top(f * scroll, h);
                    let region = D3D11_BOX { left: 0, top, front: 0, right: w, bottom: top + h, back: 1 };
                    unsafe { gpu.context.CopySubresourceRegion(&screen, 0, 0, 0, 0, &tall, 0, Some(&region)) };
                    // A viewer joining halfway asks for a keyframe.
                    if f == frames / 2 {
                        encoder.force_keyframe();
                    }
                    let t = Instant::now();
                    let Some(frame) = converter.convert(&screen).unwrap() else { continue };
                    let raw = super::super::Raw {
                        bgra: Vec::new(),
                        width: w,
                        height: h,
                        captured: Instant::now(),
                        gpu: Some(frame),
                    };
                    let mut ready = encoder.encode_raw(&raw).unwrap();
                    times.push(t.elapsed());
                    while let Some(out) = ready.take().or_else(|| encoder.poll().unwrap()) {
                        let (c, n) = keep(out, &mut stream);
                        (actual, outputs, bytes, deltas) =
                            (Some(c), outputs + 1, bytes + n, deltas + usize::from(n > 0));
                    }
                }
                let elapsed = started.elapsed();
                let drain = Instant::now();
                while drain.elapsed() < Duration::from_millis(300) {
                    if let Some(out) = encoder.poll().unwrap() {
                        let (c, n) = keep(out, &mut stream);
                        (actual, outputs, bytes, deltas) =
                            (Some(c), outputs + 1, bytes + n, deltas + usize::from(n > 0));
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
                    "{w}x{h} want {}: {} | {} in, {outputs} out | p50 {:.2} ms p95 {:.2} ms max {:.2} ms | {:.0} fps back-to-back | non-key frames {:.1} Mbit/s at {fps} fps | {check}",
                    codec.name(),
                    encoder.backend_name(),
                    times.len(),
                    pct(0.5),
                    pct(0.95),
                    pct(1.0),
                    times.len() as f64 / elapsed.as_secs_f64(),
                    bytes as f64 * 8.0 * fps as f64 / deltas.max(1) as f64 / 1e6,
                );
            }
        }
    }
}
