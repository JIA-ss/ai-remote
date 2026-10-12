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
//! was captured. Only a viewer that pings receives kind 3; older hosts ignore pings. Any device with a level may watch;
//! watching is recorded in the audit log (`view_start`, `view_end`).

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
    if let Some(reason) = crate::platform::prompt_blocker() {
        reply(&mut send, json!({"ok": false, "error": format!("the host's desktop cannot be shown: {reason}")})).await;
        return;
    }
    let monitors = capture::monitors().unwrap_or_default();
    let wanted: Vec<Codec> = req.codecs.iter().filter_map(|c| Codec::parse(c)).collect();
    // No faster than the display refreshes: the encoder budgets bits per frame from this rate.
    let refresh = capture::refresh_hz(req.monitor).unwrap_or(MAX_FPS);
    let (fps, kbps) = (req.max_fps.clamp(1, MAX_FPS).min(refresh), req.bitrate_kbps.clamp(300, MAX_KBPS));
    let requested_edge = req.max_long_edge.clamp(320, MAX_LONG_EDGE);
    // The stream's clock: frame timestamps and pongs both count from here.
    let epoch = Instant::now();
    let started = tokio::task::spawn_blocking(move || {
        // Hardware probing and capture setup both block; the codec decides the capture size.
        let (codecs, edge) = encode::plan(&wanted, requested_edge);
        capture::start(req.monitor, fps, edge).map(|capture| (capture, codecs, edge))
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
    if !reply(&mut send, json!({"ok": true, "monitors": list, "screen": screen, "codec": codecs[0].name()})).await {
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
            while !stop.load(Ordering::Relaxed) {
                // An asynchronous hardware encoder may finish a frame after its call returned.
                match enc.poll() {
                    Ok(Some(frame)) => {
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
                } else if !keyframe.load(Ordering::Relaxed) {
                    continue;
                }
                // Event-driven capture emits no new pixels for a static desktop. A decoder
                // recovering from loss must still get an IDR of the last complete frame.
                let Some(raw) = latest.as_mut() else { continue };
                if !fresh {
                    // Re-sent pixels: how long the screen stood still is not a delay of the stream.
                    raw.captured = Instant::now();
                }
                if crate::platform::prompt_blocker().is_some() {
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
                if !send(frame) {
                    break;
                }
            }
        })
    };

    let mut line = Vec::new();
    let mut controls = Controls { keyframe: &keyframe, bitrate: &bitrate, max_kbps: kbps, pings: Vec::new() };
    loop {
        tokio::select! {
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
    let payload = format!("{{\"pong\":{n}}}");
    let mut out = Vec::with_capacity(18 + payload.len());
    out.extend_from_slice(&[3, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&ts_us.to_be_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload.as_bytes());
    out
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
