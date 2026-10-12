//! Media Foundation hardware encoders (the GPU vendor's MFT: NVENC, AMF or Quick Sync) fed straight
//! from capture's NV12 textures on the same D3D11 device: no CPU copy, conversion or scaling.
//!
//! Hardware MFTs are asynchronous: the transform asks for input and announces output through
//! events. A small thread blocks on `GetEvent` and forwards them; the encoder thread submits one
//! frame, then waits (briefly) for its output, so a frame normally leaves in the same call.
//! Low-latency mode, no B-frames and CBR keep each frame's output prompt and bounded.

use std::{
    collections::VecDeque,
    sync::{Arc, OnceLock, mpsc},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use windows::{
    Win32::{
        Foundation::{LUID, VARIANT_TRUE},
        Graphics::Direct3D11::ID3D11Texture2D,
        Media::MediaFoundation::*,
        System::{
            Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree},
            Variant::{VARIANT, VT_BOOL, VT_UI4},
        },
    },
    core::{GUID, HRESULT, Interface, PWSTR},
};

use super::{Codec, Encoded};
use crate::stream::capture::win::GpuTexture;

/// How long one frame may wait for the encoder to accept it or return its output before the
/// call gives up (the output, if it comes, is returned by a later `encode`/`poll`).
const INPUT_TIMEOUT: Duration = Duration::from_secs(2);
const OUTPUT_WAIT: Duration = Duration::from_millis(100);
/// Frames between keyframes the encoder may insert on its own; viewers ask for one when needed.
const KEYFRAME_SPACING: u32 = 1 << 30;

fn startup() -> Result<()> {
    static STARTED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    com();
    // SAFETY: process-wide reference-counted startup, never shut down (the host is long-lived).
    STARTED
        .get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_LITE) }.map_err(|e| e.to_string()))
        .clone()
        .map_err(|e| anyhow!("Media Foundation: {e}"))
}

/// Joins the multithreaded apartment; a thread already in one (or in an STA) is left as it is.
fn com() {
    // SAFETY: plain COM initialization; the result is deliberately ignored (S_FALSE/RPC_E_CHANGED_MODE).
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
}

fn subtype(codec: Codec) -> GUID {
    match codec {
        Codec::H264 => MFVideoFormat_H264,
        Codec::Hevc => MFVideoFormat_HEVC,
        Codec::Av1 => MFVideoFormat_AV1,
    }
}

/// Hardware encoder MFTs taking NV12 and producing `codec`, best first; `luid` limits them to the
/// adapter the textures live on.
fn activates(codec: Codec, luid: Option<LUID>) -> Result<Vec<IMFActivate>> {
    startup()?;
    let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
    let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: subtype(codec) };
    // SAFETY: MFTEnum2 returns a CoTaskMem array of owned activation objects; each element is read
    // out exactly once (taking its reference) before the array itself is freed.
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 1)?;
        let attributes = attributes.context("no attribute store")?;
        if let Some(luid) = luid {
            let bytes = std::slice::from_raw_parts((&luid as *const LUID).cast::<u8>(), size_of::<LUID>());
            attributes.SetBlob(&MFT_ENUM_ADAPTER_LUID, bytes)?;
        }
        let mut array: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFTEnum2(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &attributes,
            &mut array,
            &mut count,
        )?;
        let mut list = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            if let Some(activate) = std::ptr::read(array.add(i)) {
                list.push(activate);
            }
        }
        if !array.is_null() {
            CoTaskMemFree(Some(array.cast()));
        }
        Ok(list)
    }
}

/// Whether this machine has a hardware encoder for `codec` (any adapter).
pub(super) fn available(codec: Codec) -> bool {
    activates(codec, None).is_ok_and(|list| !list.is_empty())
}

fn variant_u32(value: u32) -> VARIANT {
    let mut v = VARIANT::default();
    // SAFETY: writes the tag and the matching union member of a fresh, empty VARIANT.
    unsafe {
        let inner = &mut *v.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = value;
    }
    v
}

fn variant_bool(value: bool) -> VARIANT {
    let mut v = VARIANT::default();
    // SAFETY: as above.
    unsafe {
        let inner = &mut *v.Anonymous.Anonymous;
        inner.vt = VT_BOOL;
        inner.Anonymous.boolVal = if value { VARIANT_TRUE } else { Default::default() };
    }
    v
}

/// Sets a codec property; vendors differ in what they accept, so a refusal is only logged.
fn set(api: &ICodecAPI, key: &GUID, value: VARIANT, what: &str) {
    // SAFETY: both pointers are valid for the call; the VARIANT holds no owned resources.
    if let Err(e) = unsafe { api.SetValue(key, &value) } {
        tracing::debug!(property = what, error = %e, "hardware encoder ignored a property");
    }
}

enum Event {
    NeedInput,
    HaveOutput,
    Failed(HRESULT),
}

struct Generator(IMFMediaEventGenerator);
// SAFETY: hardware MFTs are free-threaded; their event generator is meant to be read from a thread
// other than the one feeding the transform.
unsafe impl Send for Generator {}

impl Generator {
    fn next(&self) -> windows::core::Result<IMFMediaEvent> {
        // SAFETY: a blocking read on a live generator; it fails with MF_E_SHUTDOWN once the
        // transform is shut down, which ends the thread.
        unsafe { self.0.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)) }
    }
}

fn pump(generator: Generator, tx: mpsc::Sender<Event>) {
    com();
    while let Ok(event) = generator.next() {
        // SAFETY: reads two plain values from a live event.
        let (kind, status) = unsafe { (event.GetType().unwrap_or(0), event.GetStatus().unwrap_or(HRESULT(0))) };
        let event = if status.is_err() {
            Event::Failed(status)
        } else if kind == METransformNeedInput.0 as u32 {
            Event::NeedInput
        } else if kind == METransformHaveOutput.0 as u32 {
            Event::HaveOutput
        } else {
            continue;
        };
        if tx.send(event).is_err() {
            break;
        }
    }
}

pub(super) struct HardwareEncoder {
    /// Kept to shut the transform down with it: releasing an activation object without
    /// `ShutdownObject` leaves the transform (and its GPU encoder session) alive. Consumer GPUs
    /// allow only a few sessions, and a new encoder waited seconds for the leaked ones.
    activate: IMFActivate,
    transform: IMFTransform,
    api: Option<ICodecAPI>,
    events: mpsc::Receiver<Event>,
    _manager: IMFDXGIDeviceManager,
    codec: Codec,
    width: u32,
    height: u32,
    fps: u32,
    input_id: u32,
    output_id: u32,
    provides_samples: bool,
    output_size: u32,
    need_input: u32,
    /// Frames submitted whose output has not arrived; each keeps its texture out of the pool.
    in_flight: VecDeque<Arc<GpuTexture>>,
    ready: VecDeque<Encoded>,
    started: Instant,
    parameter_sets: Vec<u8>,
    name: String,
}

// SAFETY: see `Generator`; the encoder is used by one thread at a time (the live-view loop).
unsafe impl Send for HardwareEncoder {}

impl Drop for HardwareEncoder {
    fn drop(&mut self) {
        // SAFETY: shutting down unblocks the event thread (GetEvent returns MF_E_SHUTDOWN) and
        // releases the GPU session; nothing uses the transform afterwards.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Ok(shutdown) = self.transform.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
            let _ = self.activate.ShutdownObject();
        }
    }
}

impl HardwareEncoder {
    /// The first hardware encoder for `codec` on the frame's adapter that accepts the size.
    pub(super) fn new(frame: &GpuTexture, codec: Codec, fps: u32, bitrate_bps: u32) -> Result<Self> {
        let list = activates(codec, Some(frame.gpu.luid))?;
        anyhow::ensure!(!list.is_empty(), "no {} hardware encoder on this GPU", codec.name());
        let mut last = None;
        for activate in list {
            match Self::open(activate.clone(), frame, codec, fps, bitrate_bps) {
                Ok(encoder) => return Ok(encoder),
                Err(e) => {
                    tracing::debug!(error = %format!("{e:#}"), codec = codec.name(), "hardware encoder refused");
                    // SAFETY: releases the activation's transform after a failed setup.
                    let _ = unsafe { activate.ShutdownObject() };
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("no usable {} hardware encoder", codec.name())))
    }

    fn open(activate: IMFActivate, frame: &GpuTexture, codec: Codec, fps: u32, bitrate_bps: u32) -> Result<Self> {
        let (width, height) = (frame.width, frame.height);
        let name = friendly_name(&activate);
        let opening = Instant::now();
        // SAFETY: Media Foundation setup on owned objects; every pointer passed lives through its call.
        unsafe {
            let transform: IMFTransform = activate.ActivateObject().context("activate the encoder")?;
            let attributes = transform.GetAttributes()?;
            anyhow::ensure!(attributes.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 1, "not an asynchronous MFT");
            attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
            anyhow::ensure!(attributes.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 1, "not Direct3D 11 aware");

            let mut token = 0;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
            let manager = manager.context("no DXGI device manager")?;
            manager.ResetDevice(&frame.gpu.device, token)?;
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .context("give the encoder our GPU")?;

            let (mut inputs, mut outputs) = ([0u32], [0u32]);
            let (input_id, output_id) = if transform.GetStreamIDs(&mut inputs, &mut outputs).is_ok() {
                (inputs[0], outputs[0])
            } else {
                (0, 0)
            };

            let api = transform.cast::<ICodecAPI>().ok();
            if let Some(api) = &api {
                // Rate control before the media types: several vendors only read it there.
                set(
                    api,
                    &CODECAPI_AVEncCommonRateControlMode,
                    variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32),
                    "rate control",
                );
                set(api, &CODECAPI_AVEncCommonMeanBitRate, variant_u32(bitrate_bps), "bitrate");
                set(api, &CODECAPI_AVLowLatencyMode, variant_bool(true), "low latency");
                set(api, &CODECAPI_AVEncMPVGOPSize, variant_u32(KEYFRAME_SPACING), "GOP");
            }

            let output = MFCreateMediaType()?;
            output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            output.SetGUID(&MF_MT_SUBTYPE, &subtype(codec))?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_bps)?;
            // NVENC ignores the GOP property and sends an IDR every second otherwise: a 2 MB
            // burst at 5K that stalls the stream for a frame or two. The format attribute holds.
            output.SetUINT32(&MF_MT_MAX_KEYFRAME_SPACING, KEYFRAME_SPACING)?;
            video_type(&output, width, height, fps)?;
            match codec {
                Codec::H264 => output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?,
                Codec::Hevc => output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH265VProfile_Main_420_8.0 as u32)?,
                Codec::Av1 => output.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncAV1VProfile_Main_420_8.0 as u32)?,
            }
            transform.SetOutputType(output_id, &output, 0).context("set the encoded format")?;

            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            video_type(&input, width, height, fps)?;
            transform.SetInputType(input_id, &input, 0).context("set the NV12 input format")?;

            if let Some(api) = &api {
                set(
                    api,
                    &CODECAPI_AVEncCommonRateControlMode,
                    variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32),
                    "rate control",
                );
                set(api, &CODECAPI_AVEncCommonMeanBitRate, variant_u32(bitrate_bps), "bitrate");
                // A couple of frames of buffer: a scene change is not starved, latency stays bounded.
                set(
                    api,
                    &CODECAPI_AVEncCommonBufferSize,
                    variant_u32((bitrate_bps / fps.max(1)).saturating_mul(2)),
                    "VBV",
                );
                set(api, &CODECAPI_AVLowLatencyMode, variant_bool(true), "low latency");
                set(api, &CODECAPI_AVEncMPVDefaultBPictureCount, variant_u32(0), "B-frames");
                // Keyframes come on request: a viewer joining or recovering. The stream is reliable.
                set(api, &CODECAPI_AVEncMPVGOPSize, variant_u32(KEYFRAME_SPACING), "GOP");
                if codec == Codec::H264 {
                    set(api, &CODECAPI_AVEncH264CABACEnable, variant_bool(true), "CABAC");
                }
            }

            let info = transform.GetOutputStreamInfo(output_id)?;
            let provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
            let generator = Generator(transform.cast::<IMFMediaEventGenerator>()?);
            let (tx, events) = mpsc::channel();
            std::thread::Builder::new()
                .name("ai-remote-encoder-events".into())
                .spawn(move || pump(generator, tx))
                .context("start the encoder event thread")?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let parameter_sets = sequence_header(&transform, output_id);
            let setup_ms = opening.elapsed().as_millis() as u64;
            tracing::info!(encoder = %name, codec = codec.name(), width, height, fps, bitrate_bps, setup_ms, "using a hardware encoder");
            Ok(Self {
                activate,
                transform,
                api,
                events,
                _manager: manager,
                codec,
                width,
                height,
                fps,
                input_id,
                output_id,
                provides_samples,
                output_size: info.cbSize.max(1 << 20),
                need_input: 0,
                in_flight: VecDeque::new(),
                ready: VecDeque::new(),
                started: Instant::now(),
                parameter_sets,
                name,
            })
        }
    }

    pub(super) fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub(super) fn codec(&self) -> Codec {
        self.codec
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// Submits `frame` and returns an encoded frame when one is ready (normally this one's).
    pub(super) fn encode(&mut self, frame: Arc<GpuTexture>, keyframe: bool) -> Result<Option<Encoded>> {
        anyhow::ensure!((frame.width, frame.height) == (self.width, self.height), "frame size changed");
        self.drain()?;
        let deadline = Instant::now() + INPUT_TIMEOUT;
        while self.need_input == 0 {
            anyhow::ensure!(self.wait(deadline)?, "the hardware encoder stopped accepting frames");
        }
        // SAFETY: the sample wraps our texture (a COM reference, released with the sample); the
        // texture's `Arc` stays in `in_flight` until the encoder has produced this frame's output.
        unsafe {
            if keyframe && let Some(api) = &self.api {
                set(api, &CODECAPI_AVEncVideoForceKeyFrame, variant_u32(1), "force keyframe");
            }
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &frame.texture, 0, false)?;
            if let Ok(plane) = buffer.cast::<IMF2DBuffer>() {
                buffer.SetCurrentLength(plane.GetContiguousLength()?)?;
            }
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            let now = self.started.elapsed().as_nanos() as i64 / 100;
            sample.SetSampleTime(now)?;
            sample.SetSampleDuration(10_000_000 / self.fps.max(1) as i64)?;
            self.transform.ProcessInput(self.input_id, &sample, 0).context("submit a frame")?;
        }
        self.need_input -= 1;
        self.in_flight.push_back(frame);
        let deadline = Instant::now() + OUTPUT_WAIT;
        while self.ready.is_empty() && !self.in_flight.is_empty() && self.wait(deadline)? {}
        Ok(self.ready.pop_front())
    }

    /// An encoded frame that became ready since the last call, without submitting anything.
    pub(super) fn poll(&mut self) -> Result<Option<Encoded>> {
        self.drain()?;
        Ok(self.ready.pop_front())
    }

    fn drain(&mut self) -> Result<()> {
        loop {
            match self.events.try_recv() {
                Ok(event) => self.handle(event)?,
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
                Err(mpsc::TryRecvError::Disconnected) => bail!("the hardware encoder shut down"),
            }
        }
    }

    /// Handles one event; `false` when `deadline` passed first.
    fn wait(&mut self, deadline: Instant) -> Result<bool> {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.events.recv_timeout(left) {
            Ok(event) => self.handle(event).map(|_| true),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(false),
            Err(mpsc::RecvTimeoutError::Disconnected) => bail!("the hardware encoder shut down"),
        }
    }

    fn handle(&mut self, event: Event) -> Result<()> {
        match event {
            Event::NeedInput => self.need_input += 1,
            Event::HaveOutput => self.output()?,
            Event::Failed(status) => bail!("hardware encoder error {status}"),
        }
        Ok(())
    }

    fn output(&mut self) -> Result<()> {
        // SAFETY: ProcessOutput fills `buffer`; both ManuallyDrop fields are taken back exactly once.
        let (result, sample) = unsafe {
            let sample = if self.provides_samples {
                None
            } else {
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(self.output_size)?)?;
                Some(sample)
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.output_id,
                pSample: std::mem::ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: std::mem::ManuallyDrop::new(None),
            }];
            let mut status = 0;
            let result = self.transform.ProcessOutput(0, &mut buffers, &mut status);
            std::mem::ManuallyDrop::drop(&mut buffers[0].pEvents);
            (result, std::mem::ManuallyDrop::take(&mut buffers[0].pSample))
        };
        if let Err(e) = result {
            if e.code() == MF_E_TRANSFORM_STREAM_CHANGE {
                // SAFETY: renegotiates the output type the encoder now proposes.
                unsafe {
                    let proposed = self.transform.GetOutputAvailableType(self.output_id, 0)?;
                    self.transform.SetOutputType(self.output_id, &proposed, 0)?;
                }
                self.parameter_sets = sequence_header(&self.transform, self.output_id);
                return Ok(());
            }
            if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                return Ok(());
            }
            return Err(anyhow!("read encoded output: {e}"));
        }
        let sample = sample.context("the encoder returned no sample")?;
        self.in_flight.pop_front();
        // SAFETY: reads a locked contiguous buffer of the sample we now own.
        let (mut data, clean) = unsafe {
            let clean = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
            let buffer = sample.ConvertToContiguousBuffer()?;
            let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock()?;
            (data, clean)
        };
        if data.is_empty() {
            return Ok(());
        }
        let keyframe = clean || super::is_keyframe(self.codec, &data);
        if keyframe && self.codec != Codec::Av1 && !super::has_parameter_sets(self.codec, &data) {
            anyhow::ensure!(!self.parameter_sets.is_empty(), "keyframe without parameter sets");
            let mut with_sets = self.parameter_sets.clone();
            with_sets.append(&mut data);
            data = with_sets;
        }
        self.ready.push_back(Encoded { data, keyframe, width: self.width, height: self.height, codec: self.codec });
        Ok(())
    }
}

fn video_type(t: &IMFMediaType, width: u32, height: u32, fps: u32) -> Result<()> {
    // SAFETY: attribute writes on an owned media type.
    unsafe {
        t.SetUINT64(&MF_MT_FRAME_SIZE, (u64::from(width) << 32) | u64::from(height))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        // What capture's GPU conversion produces: BT.709, studio range.
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
    }
    Ok(())
}

/// The parameter sets (SPS/PPS, VPS for HEVC) the encoder reports for its output, Annex B.
fn sequence_header(transform: &IMFTransform, output_id: u32) -> Vec<u8> {
    // SAFETY: reads a blob attribute of the current output type into an owned buffer.
    unsafe {
        let Ok(t) = transform.GetOutputCurrentType(output_id) else { return Vec::new() };
        let Ok(size) = t.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) else { return Vec::new() };
        let mut blob = vec![0u8; size as usize];
        if t.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, None).is_err() {
            return Vec::new();
        }
        blob
    }
}

fn friendly_name(activate: &IMFActivate) -> String {
    // SAFETY: GetAllocatedString hands back a CoTaskMem string we free after copying.
    unsafe {
        let (mut text, mut len) = (PWSTR::null(), 0u32);
        if activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut text, &mut len).is_err() {
            return "hardware encoder".into();
        }
        let name = text.to_string().unwrap_or_default();
        CoTaskMemFree(Some(text.0 as *const _));
        name
    }
}
