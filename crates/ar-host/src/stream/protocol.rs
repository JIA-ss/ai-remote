//! `app/ai-remote/screen/1`: live video of the host's screen for a person at a paired device.
//!
//! The controller opens a bi-stream and writes one JSON line:
//! `{"monitor":null,"max_fps":30,"max_long_edge":1920,"bitrate_kbps":6000,"codecs":["hevc","h264"]}`
//! (every field optional). `codecs` lists what the viewer decodes, best first; without it the
//! stream is H.264, which every viewer decodes. The host answers with one JSON line
//! (`{"ok":true,"monitors":[...],"screen":{x,y,width,height},"codec":"hevc"}`: the streamed
//! monitor's place in screen coordinates and the codec it starts with; or `{"ok":false,"error":..}`)
//! and then binary frames until either side closes:
//!
//! ```text
//! u8  kind     1 = video frame
//! u8  flags    bit 0: keyframe; bits 1-2: codec (0 H.264, 1 HEVC, 2 AV1)
//! u16 width    big-endian, the stream size of this frame
//! u16 height
//! u64 ts_us    when the picture was captured, microseconds on the stream's clock
//! u32 length
//! ..  H.264/HEVC Annex B or AV1 OBUs, `length` bytes
//! ```
//!
//! The codec can change at a keyframe (a hardware encoder failed and the host fell back to software
//! H.264); a viewer reconfigures its decoder when a keyframe's codec differs.
//!
//! The controller may write more lines on its half of the stream: `{"keyframe":true}` asks for a
//! keyframe (after a decoder error, or a viewer that joined late); `{"bitrate_kbps":9000}` changes
//! the bitrate of the running stream (a viewer adapting to its network; at most the request, at
//! least 300); `{"ping":7}` is answered, after
//! the frame being written, with a kind-3 message: the same 18-byte header (width and height 0,
//! `ts_us` the stream clock as it answers) and `{"pong":7}`. A viewer maps the stream clock onto its
//! own with these (the reply with the shortest round trip is the best) to tell how long ago a frame
//! was captured. Only a viewer that pings receives kind 3; older hosts ignore pings.
//!
//! `"cursor":"client"` in the request asks for the pointer separately instead of drawn into the
//! picture (so moving it does not re-encode the frame under it). A host that can do that leaves it
//! out of the picture, answers `"cursor":"client"`, and sends, whenever the pointer changes (at most
//! at the stream's frame rate), a kind-4 message (same header, width and height 0) with
//! `{"x":..,"y":..,"visible":true,"shape":n}`: the pointer's hot spot relative to the streamed
//! monitor, in the units of the reply's `screen` rect. Before the first position with a new shape
//! `n` comes a kind-5 message: width and height are the picture's, the body a big-endian u32 `n`,
//! u16 hot spot x and y, then width * height straight-alpha BGRA pixels, top row first. Shape 0
//! means unknown (draw a default arrow). Without the answer the pointer is in the picture as before.
//! Any device with a level may watch; watching is recorded in the audit log (`view_start`, `view_end`).

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use ar_net::{
    iroh::{
        endpoint::{Connection, SendStream},
        protocol::{AcceptError, ProtocolHandler},
    },
    refusal::{CLOSE_NO_ACCESS, REASON_NO_LEVEL, REASON_NOT_PAIRED},
};
use serde::Deserialize;
use serde_json::json;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    sync::mpsc,
};

use super::{
    capture,
    encode::{self, Codec, FrameEncoder},
};
use crate::HostState;

/// Viewers served at once; each one has its own capture and encoder.
const MAX_VIEWERS: usize = 3;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    #[serde(default)]
    monitor: Option<usize>,
    #[serde(default = "default_fps")]
    max_fps: u32,
    #[serde(default = "default_long_edge")]
    max_long_edge: u32,
    #[serde(default = "default_kbps")]
    bitrate_kbps: u32,
    /// What the viewer decodes, best first; unknown names are ignored.
    #[serde(default)]
    codecs: Vec<String>,
    /// `"client"`: the viewer draws the pointer itself (see the module docs).
    #[serde(default)]
    cursor: Option<String>,
}

/// Request limits: high-refresh displays, 8K, and a LAN's worth of bitrate.
const MAX_FPS: u32 = 240;
const MAX_LONG_EDGE: u32 = 8192;
const MAX_KBPS: u32 = 500_000;

fn default_fps() -> u32 {
    30
}
fn default_long_edge() -> u32 {
    1920
}
fn default_kbps() -> u32 {
    6000
}

#[derive(Debug, Deserialize)]
struct Control {
    #[serde(default)]
    keyframe: bool,
    #[serde(default)]
    ping: Option<u64>,
    #[serde(default)]
    bitrate_kbps: Option<u32>,
}

/// What the viewer asked for on its half of the stream, also read while a frame is being written.
struct Controls<'a> {
    keyframe: &'a AtomicBool,
    /// The bitrate the viewer asked for, in kbit/s, and the most it may ask for (its request).
    bitrate: &'a AtomicU32,
    max_kbps: u32,
    /// Pings to answer once the frame being written is out (a few at most).
    pings: Vec<u64>,
}

impl Controls<'_> {
    fn apply(&mut self, line: &[u8]) {
        let Ok(control) = serde_json::from_slice::<Control>(line) else { return };
        if control.keyframe {
            self.keyframe.store(true, Ordering::Relaxed);
        }
        if let Some(kbps) = control.bitrate_kbps {
            self.bitrate.store(kbps.clamp(300, self.max_kbps.max(300)), Ordering::Relaxed);
        }
        if let Some(n) = control.ping
            && self.pings.len() < 8
        {
            self.pings.push(n);
        }
    }
}

pub struct ScreenProtocol {
    host: Arc<HostState>,
    viewers: Arc<AtomicUsize>,
}

impl std::fmt::Debug for ScreenProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScreenProtocol")
    }
}

impl ScreenProtocol {
    pub fn new(host: Arc<HostState>) -> Self {
        Self { host, viewers: Arc::default() }
    }
}

impl ProtocolHandler for ScreenProtocol {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        let peer = match self.host.mesh().peer_for(&conn.remote_id()).await {
            Ok(Some(peer)) => peer,
            _ => {
                conn.close(CLOSE_NO_ACCESS.into(), REASON_NOT_PAIRED);
                return Ok(());
            }
        };
        if peer.level.is_none() {
            conn.close(CLOSE_NO_ACCESS.into(), REASON_NO_LEVEL);
            return Ok(());
        }
        let _present =
            self.host.presence().connect(format!("eid:{}", conn.remote_id()), crate::presence::Channel::Screen);
        while let Ok((send, recv)) = conn.accept_bi().await {
            let host = self.host.clone();
            let viewers = self.viewers.clone();
            let peer = peer.clone();
            tokio::spawn(async move { serve(host, viewers, peer, send, recv).await });
        }
        Ok(())
    }
}

/// Counts a viewer while it is served.
struct Viewer(Arc<AtomicUsize>);

impl Drop for Viewer {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn serve(
    host: Arc<HostState>,
    viewers: Arc<AtomicUsize>,
    peer: ar_net::Peer,
    mut send: SendStream,
    recv: ar_net::iroh::endpoint::RecvStream,
) {
    let mut reader = BufReader::new(recv.take(1 << 20));
    let mut first = String::new();
    if reader.read_line(&mut first).await.unwrap_or(0) == 0 {
        return;
    }
    let req: Request = match serde_json::from_str(&first) {
        Ok(r) => r,
        Err(e) => {
            reply(&mut send, json!({"ok": false, "error": format!("bad request: {e}")})).await;
            return;
        }
    };
    if viewers.fetch_add(1, Ordering::SeqCst) >= MAX_VIEWERS {
        viewers.fetch_sub(1, Ordering::SeqCst);
        reply(&mut send, json!({"ok": false, "error": "too many people are watching this computer right now"})).await;
        return;
    }
    let _viewer = Viewer(viewers);
    if let Some(reason) = crate::platform::desktop_blocker() {
        reply(&mut send, json!({"ok": false, "error": format!("the host's desktop cannot be shown: {reason}")})).await;
        return;
    }
    let monitors = capture::monitors().unwrap_or_default();
    let wanted: Vec<Codec> = req.codecs.iter().filter_map(|c| Codec::parse(c)).collect();
    // No faster than the display refreshes: the encoder budgets bits per frame from this rate.
    let refresh = capture::refresh_hz(req.monitor).unwrap_or(MAX_FPS);
    let (fps, kbps) = (req.max_fps.clamp(1, MAX_FPS).min(refresh), req.bitrate_kbps.clamp(300, MAX_KBPS));
    let requested_edge = req.max_long_edge.clamp(320, MAX_LONG_EDGE);
    let draw_pointer = !(req.cursor.as_deref() == Some("client") && super::pointer::supported());
    // The stream's clock: frame timestamps and pongs both count from here.
    let epoch = Instant::now();
    let started = tokio::task::spawn_blocking(move || {
        // Hardware probing and capture setup both block; the codec decides the capture size.
        let (codecs, edge) = encode::plan(&wanted, requested_edge);
        capture::start(req.monitor, fps, edge, draw_pointer).map(|capture| (capture, codecs, edge))
    })
    .await;
    let (capture, codecs, edge) = match started {
        Ok(Ok(c)) => c,
        result => {
            let error = match result {
                Ok(Err(e)) => format!("{e:#}"),
                Err(e) => format!("capture worker failed: {e}"),
                Ok(Ok(_)) => unreachable!(),
            };
            reply(&mut send, json!({"ok": false, "error": error})).await;
            return;
        }
    };
    let list: Vec<_> =
        monitors.iter().map(|(i, w, h, p)| json!({"index": i, "width": w, "height": h, "primary": p})).collect();
    // Where the streamed monitor is on the screen, so a viewer can turn a point of the picture into
    // screen coordinates for the mouse tool.
    let screen = capture.geometry().map(|(x, y, w, h)| json!({"x": x, "y": y, "width": w, "height": h}));
    let mut answer = json!({"ok": true, "monitors": list, "screen": screen, "codec": codecs[0].name()});
    // The pointer goes separately only when it really is out of the picture.
    let mut pointer = (!draw_pointer && !capture.pointer_drawn())
        .then(|| PointerFeed::new(capture.geometry().map_or((0, 0), |(x, y, _, _)| (x, y))));
    if pointer.is_some() {
        answer["cursor"] = "client".into();
    }
    if !reply(&mut send, answer).await {
        return;
    }
    let audit_line =
        |kind: &str| json!({"kind": kind, "peer": peer.name, "principal": peer.principal, "monitor": req.monitor});
    host.audit().event(audit_line("view_start"));

    // Encoding runs on a blocking thread; frames come back over a small channel, so a slow network
    // makes the encoder wait, and the capture slot skips frames meanwhile.
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(1);
    let keyframe = Arc::new(AtomicBool::new(false));
    let bitrate = Arc::new(AtomicU32::new(kbps));
    let stop = Arc::new(AtomicBool::new(false));
    let encoder = {
        let (slot, keyframe, stop, capture_edge) =
            (capture.slot(), keyframe.clone(), stop.clone(), capture.max_long_edge());
        let bitrate = bitrate.clone();
        tokio::task::spawn_blocking(move || {
            let mut enc = match FrameEncoder::new(fps, kbps * 1000, edge) {
                Ok(e) => e.with_codecs(&codecs).with_capture_edge(capture_edge),
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "live view encoder");
                    return;
                }
            };
            let send = |frame: encode::Encoded| {
                let ts = frame.captured.map_or_else(|| epoch.elapsed(), |c| c.saturating_duration_since(epoch));
                let ts = ts.as_micros() as u64;
                tx.blocking_send(frame_bytes(&frame.data, frame.keyframe, frame.codec, frame.width, frame.height, ts))
                    .is_ok()
            };
            let mut latest = None;
            let mut backend = String::new();
            let mut pacer = Pacer::new(fps, Instant::now());
            let mut refine = Refine::new(Instant::now());
            while !stop.load(Ordering::Relaxed) {
                // An asynchronous hardware encoder may finish a frame after its call returned.
                match enc.poll() {
                    Ok(Some(frame)) => {
                        if frame.keyframe {
                            refine.observe(true, false, frame.data.len(), 0, fps, Instant::now());
                        }
                        if !send(frame) {
                            break;
                        }
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(error = %format!("{e:#}"), "live view frame"),
                }
                // Paced here, not in capture: the slot keeps the newest frame, so the last change
                // before a still screen is always sent once its turn comes.
                let wait = pacer.wait(Instant::now());
                if !wait.is_zero() {
                    std::thread::sleep(wait);
                    continue;
                }
                let Some(next) = slot.take(Duration::from_millis(5)) else { break };
                let fresh = next.is_some();
                if let Some(raw) = next {
                    latest = Some(raw);
                } else if !keyframe.load(Ordering::Relaxed) && !refine.wanted(Instant::now()) {
                    continue;
                }
                // Event-driven capture emits no new pixels for a static desktop. A decoder
                // recovering from loss must still get an IDR of the last complete frame, and a
                // still picture is re-encoded until it is sharp (see `Refine`).
                let Some(raw) = latest.as_mut() else { continue };
                if !fresh {
                    // Re-sent pixels: how long the screen stood still is not a delay of the stream.
                    raw.captured = Instant::now();
                }
                if crate::platform::desktop_blocker().is_some() {
                    slot.close();
                    break;
                }
                if keyframe.swap(false, Ordering::Relaxed) {
                    enc.force_keyframe();
                }
                let want = bitrate.load(Ordering::Relaxed).saturating_mul(1000);
                if want != enc.bitrate() {
                    tracing::info!(from = enc.bitrate(), to = want, "live view bitrate changed by the viewer");
                    enc.set_bitrate(want);
                }
                pacer.spend();
                let frame = enc.encode_raw(&*raw);
                if enc.backend_name() != backend {
                    backend = enc.backend_name();
                    tracing::info!(%backend, "live view encoder");
                }
                let frame = match frame {
                    Ok(Some(f)) => f,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "live view frame");
                        continue;
                    }
                };
                let share = (enc.bitrate() / 8 / fps.max(1)) as usize;
                refine.observe(frame.keyframe, fresh, frame.data.len(), share, fps, Instant::now());
                if !send(frame) {
                    break;
                }
            }
        })
    };

    let mut line = Vec::new();
    let mut controls = Controls { keyframe: &keyframe, bitrate: &bitrate, max_kbps: kbps, pings: Vec::new() };
    // The pointer is looked at once a frame (a still pointer sends nothing).
    let mut pointer_tick = tokio::time::interval(Duration::from_secs_f64(1.0 / f64::from(fps.clamp(30, 120))));
    pointer_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = pointer_tick.tick(), if pointer.is_some() => {
                let messages = pointer.as_mut().map(|p| p.poll(epoch)).unwrap_or_default();
                if !write_messages(&mut send, &messages).await {
                    break;
                }
            },
            frame = rx.recv() => match frame {
                Some(bytes) => {
                    if !write_frame(&mut send, &mut reader, &bytes, &mut line, &mut controls, host.stopping()).await { break; }
                },
                None => break,
            },
            read = reader.read_until(b'\n', &mut line) => match read {
                Ok(n) if n > 0 => {
                    controls.apply(&line);
                    line.clear();
                }
                // The controller finished its half: it stopped watching.
                _ => break,
            },
            _ = host.stopping().cancelled() => break,
            _ = send.stopped() => break,
        }
        if !answer_pings(&mut send, &mut controls.pings, epoch).await {
            break;
        }
    }
    stop.store(true, Ordering::Relaxed);
    drop(rx);
    let _ = tokio::task::spawn_blocking(move || drop(capture)).await;
    let _ = encoder.await;
    let _ = send.finish();
    host.audit().event(audit_line("view_end"));
}

/// Whether a still picture should be encoded again. After a keyframe, or a large frame (motion: a
/// scroll, a window drawn), the picture on the viewer's screen is blurrier than the encoder can make
/// it: a keyframe is held to the rate-control buffer, and moving frames get fewer bits each. Capture
/// sends nothing for a still screen, so without this it would stay blurry until something changed.
/// The still picture is re-encoded at the frame rate until the encoder has nothing left to add: its
/// output stays under a tenth of a frame's share of the bitrate for a quarter second (not counted in
/// the first [`Refine::GRACE`] after a keyframe, when the rate control pays the keyframe back with
/// empty frames), or for [`Refine::CAP`] at most. Small fresh frames (a blinking caret) start nothing.
///
/// Measured 2026-10-13 (`encoded_quality`, `scroll=0`, the desktop-like test picture, one run each):
/// a keyframe alone is 14-15 dB PSNR-Y; re-encoding the still picture reaches 46-47 dB in 1.5-1.8 s
/// at 1080p 12 Mbit/s and 4K 40 Mbit/s (where this rule stops at the plateau), still rising after
/// 2.5 s at the lower bitrates. Larger keyframes do not help: 4 times the bitrate for the keyframe
/// made it 15.7 dB instead of 14.5.
struct Refine {
    settled: bool,
    /// When the last keyframe came out, and since when the picture has been still.
    keyframe_at: Instant,
    since: Instant,
    /// Re-encoded frames in a row that added almost nothing.
    small: u32,
}

impl Refine {
    const GRACE: Duration = Duration::from_millis(400);
    const CAP: Duration = Duration::from_secs(5);

    fn new(now: Instant) -> Self {
        Self { settled: false, keyframe_at: now, since: now, small: 0 }
    }

    fn wanted(&self, now: Instant) -> bool {
        !self.settled && now.duration_since(self.since) < Self::CAP
    }

    /// An encoded frame of `bytes`, of new pixels (`fresh`) or of the still picture again; `share` is
    /// a frame's share of the bitrate in bytes.
    fn observe(&mut self, keyframe: bool, fresh: bool, bytes: usize, share: usize, fps: u32, now: Instant) {
        if keyframe {
            self.keyframe_at = now;
        }
        if keyframe || (fresh && bytes * 10 >= share) {
            *self = Self { settled: false, keyframe_at: self.keyframe_at, since: now, small: 0 };
            return;
        }
        if fresh || now.duration_since(self.keyframe_at) < Self::GRACE {
            return;
        }
        if bytes * 10 < share {
            self.small += 1;
            if self.small >= (fps / 4).max(8) {
                self.settled = true;
            }
        } else {
            self.small = 0;
        }
    }
}

/// Holds the stream to `fps` frames a second on average. A display only delivers frames on its
/// refreshes, so a fixed minimum interval rounds up to whole refreshes: 60 fps on a 100 Hz display
/// became 50 and 30 became 25 (measured 2026-10-12), and on 144 Hz 60 would become 48. Credit
/// accrues continuously instead, and a frame may go once nine tenths of a frame's credit is there
/// (refresh jitter), so the average is exact; gaps alternate between neighbouring refreshes.
struct Pacer {
    fps: f64,
    credit: f64,
    last: Instant,
}

impl Pacer {
    fn new(fps: u32, now: Instant) -> Self {
        Self { fps: f64::from(fps.max(1)), credit: 1.0, last: now }
    }

    /// How long until the next frame may go; zero when it may go now.
    fn wait(&mut self, now: Instant) -> Duration {
        let earned = now.saturating_duration_since(self.last).as_secs_f64() * self.fps;
        self.last = now;
        // At most one frame saved up: after a still screen the next change goes at once, no burst.
        self.credit = (self.credit + earned).min(1.0);
        if self.credit >= 0.9 { Duration::ZERO } else { Duration::from_secs_f64((0.9 - self.credit) / self.fps) }
    }

    fn spend(&mut self) {
        self.credit -= 1.0;
    }
}

/// A partial frame is terminal on failure. Keep cancellation and keyframe controls responsive
/// during network backpressure, and retain partial control lines across select cancellations.
async fn write_frame<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
    send: &mut W,
    reader: &mut R,
    bytes: &[u8],
    line: &mut Vec<u8>,
    controls: &mut Controls<'_>,
    stop: &tokio_util::sync::CancellationToken,
) -> bool {
    let write = tokio::time::timeout(Duration::from_secs(2), send.write_all(bytes));
    tokio::pin!(write);
    loop {
        tokio::select! {
            result = &mut write => return matches!(result, Ok(Ok(()))),
            read = reader.read_until(b'\n', line) => match read {
                Ok(n) if n > 0 => {
                    controls.apply(line);
                    line.clear();
                },
                _ => return false,
            },
            _ = stop.cancelled() => return false,
        }
    }
}

/// The pointer for a viewer that draws it: the messages to send when it moved or changed shape.
struct PointerFeed {
    origin: (i32, i32),
    last: Option<super::pointer::Position>,
    /// Shapes sent, by the system's handle, with the id the viewer knows them by.
    shapes: std::collections::HashMap<u64, u32>,
}

impl PointerFeed {
    /// Shapes kept: a session cycles through a handful; a leaking app could make new ones forever.
    const MAX_SHAPES: usize = 256;

    fn new(origin: (i32, i32)) -> Self {
        Self { origin, last: None, shapes: std::collections::HashMap::new() }
    }

    fn poll(&mut self, epoch: Instant) -> Vec<Vec<u8>> {
        let Some(now) = super::pointer::position() else { return Vec::new() };
        if self.last == Some(now) {
            return Vec::new();
        }
        self.last = Some(now);
        let ts = epoch.elapsed().as_micros() as u64;
        let mut out = Vec::new();
        let id = match self.shapes.get(&now.shape) {
            Some(&id) => id,
            None if now.visible => match super::pointer::shape(now.shape) {
                Some(shape) => {
                    if self.shapes.len() >= Self::MAX_SHAPES {
                        self.shapes.clear();
                    }
                    let id = self.shapes.len() as u32 + 1;
                    self.shapes.insert(now.shape, id);
                    out.push(pointer_shape_bytes(id, &shape, ts));
                    id
                }
                None => 0,
            },
            None => 0,
        };
        let body = json!({"x": now.x - self.origin.0, "y": now.y - self.origin.1, "visible": now.visible, "shape": id});
        out.push(message_bytes(4, 0, 0, ts, body.to_string().as_bytes()));
        out
    }
}

/// A kind-5 pointer picture (see the module docs).
fn pointer_shape_bytes(id: u32, shape: &super::pointer::Shape, ts_us: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + shape.bgra.len());
    body.extend_from_slice(&id.to_be_bytes());
    body.extend_from_slice(&(shape.hotspot.0 as u16).to_be_bytes());
    body.extend_from_slice(&(shape.hotspot.1 as u16).to_be_bytes());
    body.extend_from_slice(&shape.bgra);
    message_bytes(5, shape.width as u16, shape.height as u16, ts_us, &body)
}

/// A non-video message: the frame header with `kind`, no flags, and `body`.
fn message_bytes(kind: u8, width: u16, height: u16, ts_us: u64, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(18 + body.len());
    out.push(kind);
    out.push(0);
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&ts_us.to_be_bytes());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// Writes small messages between frames; `false` when the controller is gone.
async fn write_messages<W: AsyncWrite + Unpin>(send: &mut W, messages: &[Vec<u8>]) -> bool {
    for m in messages {
        if !matches!(tokio::time::timeout(Duration::from_secs(2), send.write_all(m)).await, Ok(Ok(()))) {
            return false;
        }
    }
    true
}

/// Answers the pings read so far with the stream clock; `false` when the controller is gone.
async fn answer_pings<W: AsyncWrite + Unpin>(send: &mut W, pings: &mut Vec<u64>, epoch: Instant) -> bool {
    for n in pings.drain(..) {
        let pong = pong_bytes(n, epoch.elapsed().as_micros() as u64);
        if !matches!(tokio::time::timeout(Duration::from_secs(2), send.write_all(&pong)).await, Ok(Ok(()))) {
            return false;
        }
    }
    true
}

/// A kind-3 reply to `{"ping":n}`, stamped with the stream clock.
pub fn pong_bytes(n: u64, ts_us: u64) -> Vec<u8> {
    message_bytes(3, 0, 0, ts_us, format!("{{\"pong\":{n}}}").as_bytes())
}

/// Writes one JSON line; `false` when the controller is gone.
async fn reply(send: &mut SendStream, v: serde_json::Value) -> bool {
    let mut line = v.to_string();
    line.push('\n');
    send.write_all(line.as_bytes()).await.is_ok()
}

/// One frame in the wire format of this module.
pub fn frame_bytes(data: &[u8], keyframe: bool, codec: Codec, width: u32, height: u32, ts_us: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(18 + data.len());
    out.push(1);
    out.push(keyframe as u8 | codec.wire_id() << 1);
    out.extend_from_slice(&(width as u16).to_be_bytes());
    out.extend_from_slice(&(height as u16).to_be_bytes());
    out.extend_from_slice(&ts_us.to_be_bytes());
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn slow_write_still_accepts_keyframe_control_and_stop() {
        let (mut send, _slow_peer) = tokio::io::duplex(1);
        let (control, mut peer) = tokio::io::duplex(128);
        let mut reader = BufReader::new(control);
        let stop = tokio_util::sync::CancellationToken::new();
        let keyframe = AtomicBool::new(false);
        let bitrate = AtomicU32::new(8000);
        let mut controls = Controls { keyframe: &keyframe, bitrate: &bitrate, max_kbps: 8000, pings: Vec::new() };
        let mut line = Vec::new();
        let started = Instant::now();
        let viewer = async {
            peer.write_all(b"{\"keyframe\":true}\n{\"ping\":5}\n").await.unwrap();
            tokio::time::timeout(Duration::from_millis(500), async {
                while !keyframe.load(Ordering::Relaxed) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            stop.cancel();
        };
        let (sent, _) =
            tokio::join!(write_frame(&mut send, &mut reader, &[0; 4096], &mut line, &mut controls, &stop), viewer);
        assert!(!sent, "cancelled partial frames must terminate instead of continuing");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(keyframe.load(Ordering::Relaxed));
        assert_eq!(controls.pings, [5], "a ping read during a write is answered after it");
    }

    #[tokio::test]
    async fn partial_control_json_survives_an_interleaved_frame_write() {
        let (mut send, mut receiver) = tokio::io::duplex(1);
        let (control, mut peer) = tokio::io::duplex(128);
        let mut reader = BufReader::new(control);
        let stop = tokio_util::sync::CancellationToken::new();
        let keyframe = AtomicBool::new(false);
        let bitrate = AtomicU32::new(8000);
        let mut controls = Controls { keyframe: &keyframe, bitrate: &bitrate, max_kbps: 8000, pings: Vec::new() };
        let mut line = Vec::new();
        peer.write_all(b"{\"key").await.unwrap();
        let receive = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let mut bytes = [0; 4];
            receiver.read_exact(&mut bytes).await.unwrap();
            assert_eq!(bytes, [1, 2, 3, 4]);
        };
        let (sent, _) =
            tokio::join!(write_frame(&mut send, &mut reader, &[1, 2, 3, 4], &mut line, &mut controls, &stop), receive);
        assert!(sent);
        assert_eq!(line, b"{\"key");
        peer.write_all(b"frame\":true}\n").await.unwrap();
        reader.read_until(b'\n', &mut line).await.unwrap();
        assert!(serde_json::from_slice::<Control>(&line).unwrap().keyframe);
    }

    /// Frames taken by an encode loop when a display with `refresh_hz` delivers every refresh.
    fn paced(fps: u32, refresh_hz: f64, seconds: f64) -> usize {
        let start = Instant::now();
        let mut pacer = Pacer::new(fps, start);
        let (mut now, mut next_refresh, mut sent) = (0.0f64, 0.0f64, 0);
        while now < seconds {
            let wait = pacer.wait(start + Duration::from_secs_f64(now)).as_secs_f64();
            if wait > 0.0 {
                now += wait;
                continue;
            }
            // Take the newest frame; wait for the next refresh when none is pending.
            if next_refresh > now {
                now = next_refresh;
            }
            pacer.spend();
            sent += 1;
            next_refresh = ((now * refresh_hz + 1e-6).floor() + 1.0) / refresh_hz;
        }
        sent
    }

    #[test]
    fn pacing_keeps_the_requested_rate_on_any_refresh_rate() {
        for (fps, hz, expected) in [(60, 100.0, 600), (30, 100.0, 300), (60, 144.0, 600), (120, 144.0, 1200)] {
            let sent = paced(fps, hz, 10.0);
            assert!(sent.abs_diff(expected) <= 2, "{fps} fps on {hz} Hz sent {sent}");
        }
        // Never more than the display delivers.
        assert!(paced(240, 100.0, 10.0) <= 1001);
        assert!(paced(100, 100.0, 10.0) >= 998);
    }

    #[test]
    fn a_change_after_a_still_screen_goes_at_once_without_a_burst() {
        let start = Instant::now();
        let mut pacer = Pacer::new(30, start);
        assert!(pacer.wait(start).is_zero());
        pacer.spend();
        let later = start + Duration::from_secs(5);
        assert!(pacer.wait(later).is_zero());
        pacer.spend();
        assert!(!pacer.wait(later + Duration::from_millis(10)).is_zero());
    }

    #[test]
    fn a_still_picture_is_refined_until_the_encoder_has_nothing_to_add() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut r = Refine::new(t0);
        r.observe(true, true, 150_000, 12_500, 60, at(0));
        assert!(r.wanted(at(10)), "a keyframe is refined");
        // Empty frames while the keyframe is paid back do not settle it.
        for i in 1..20 {
            r.observe(false, false, 0, 12_500, 60, at(i * 16));
        }
        assert!(r.wanted(at(330)));
        // Real refinement, then a quarter second of almost nothing: settled.
        for i in 25..60 {
            r.observe(false, false, 12_000, 12_500, 60, at(i * 16));
        }
        assert!(r.wanted(at(960)));
        for i in 60..75 {
            r.observe(false, false, 500, 12_500, 60, at(i * 16));
        }
        assert!(!r.wanted(at(1200)), "settled after 15 small frames");
        // A blinking caret (a small fresh frame) starts nothing; a scroll (a large one) does.
        r.observe(false, true, 300, 12_500, 60, at(1300));
        assert!(!r.wanted(at(1310)));
        r.observe(false, true, 20_000, 12_500, 60, at(1400));
        assert!(r.wanted(at(1410)));
        // Never longer than the cap after the picture went still.
        assert!(!r.wanted(at(1400) + Refine::CAP));
    }

    #[test]
    fn pointer_shapes_carry_their_id_hot_spot_and_pixels() {
        let shape =
            crate::stream::pointer::Shape { width: 2, height: 1, hotspot: (1, 0), bgra: vec![1, 2, 3, 4, 5, 6, 7, 8] };
        let bytes = pointer_shape_bytes(7, &shape, 42);
        assert_eq!(&bytes[..6], &[5, 0, 0, 2, 0, 1]);
        assert_eq!(u64::from_be_bytes(bytes[6..14].try_into().unwrap()), 42);
        assert_eq!(u32::from_be_bytes(bytes[14..18].try_into().unwrap()), 16);
        assert_eq!(&bytes[18..26], &[0, 0, 0, 7, 0, 1, 0, 0]);
        assert_eq!(&bytes[26..], &shape.bgra[..]);
    }

    #[tokio::test]
    async fn pings_are_answered_with_the_stream_clock() {
        let epoch = Instant::now() - Duration::from_millis(1500);
        let (mut send, mut receiver) = tokio::io::duplex(1024);
        let mut pings = vec![7, 8];
        assert!(answer_pings(&mut send, &mut pings, epoch).await);
        assert!(pings.is_empty());
        for n in [7u64, 8] {
            let mut header = [0u8; 18];
            receiver.read_exact(&mut header).await.unwrap();
            assert_eq!(header[0], 3);
            let ts = u64::from_be_bytes(header[6..14].try_into().unwrap());
            assert!((1_500_000..2_500_000).contains(&ts), "{ts}");
            let mut payload = vec![0; u32::from_be_bytes(header[14..18].try_into().unwrap()) as usize];
            receiver.read_exact(&mut payload).await.unwrap();
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&payload).unwrap(), serde_json::json!({"pong": n}));
        }
        // Old controls still parse: a keyframe line, and unknown fields are ignored.
        let keyframe = AtomicBool::new(false);
        let bitrate = AtomicU32::new(8000);
        let mut controls = Controls { keyframe: &keyframe, bitrate: &bitrate, max_kbps: 8000, pings: Vec::new() };
        controls.apply(br#"{"keyframe":true,"later":1}"#);
        controls.apply(br#"{"ping":3}"#);
        assert!(keyframe.load(Ordering::Relaxed));
        assert_eq!(controls.pings, [3]);
        // A viewer lowers the bitrate; it cannot raise it past its request, nor below 300.
        controls.apply(br#"{"bitrate_kbps":2500}"#);
        assert_eq!(bitrate.load(Ordering::Relaxed), 2500);
        controls.apply(br#"{"bitrate_kbps":90000}"#);
        assert_eq!(bitrate.load(Ordering::Relaxed), 8000);
        controls.apply(br#"{"bitrate_kbps":1}"#);
        assert_eq!(bitrate.load(Ordering::Relaxed), 300);
    }

    #[test]
    fn frames_have_an_18_byte_header() {
        let b = super::frame_bytes(&[0, 0, 0, 1, 0x67], true, Codec::H264, 1920, 1080, 42);
        assert_eq!(b.len(), 18 + 5);
        assert_eq!(&b[..2], &[1, 1]);
        // Codec ids sit above the keyframe bit; H.264 keeps the header older viewers read.
        assert_eq!(super::frame_bytes(&[0], false, Codec::Hevc, 2, 2, 0)[1], 0b010);
        assert_eq!(super::frame_bytes(&[0], true, Codec::Av1, 2, 2, 0)[1], 0b101);
        assert_eq!(u16::from_be_bytes([b[2], b[3]]), 1920);
        assert_eq!(u16::from_be_bytes([b[4], b[5]]), 1080);
        assert_eq!(u64::from_be_bytes(b[6..14].try_into().unwrap()), 42);
        assert_eq!(u32::from_be_bytes(b[14..18].try_into().unwrap()), 5);
    }
}
