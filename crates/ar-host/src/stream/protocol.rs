//! `app/ai-remote/screen/1`: live video of the host's screen for a person at a paired device.
//!
//! The controller opens a bi-stream and writes one JSON line:
//! `{"monitor":null,"max_fps":30,"max_long_edge":1920,"bitrate_kbps":6000}` (every field optional).
//! The host answers with one JSON line (`{"ok":true,"monitors":[...],"screen":{x,y,width,height}}`,
//! the streamed monitor's place in screen coordinates, or `{"ok":false,"error":..}`)
//! and then binary frames until either side closes:
//!
//! ```text
//! u8  kind     1 = video frame
//! u8  flags    bit 0: keyframe
//! u16 width    big-endian, the stream size of this frame
//! u16 height
//! u64 ts_us    encoder emission time, microseconds since the stream started
//! u32 length
//! ..  H.264 Annex B, `length` bytes
//! ```
//!
//! The controller may write more lines on its half of the stream: `{"keyframe":true}` asks for a
//! keyframe (after a decoder error, or a viewer that joined late). Any device with a level may watch;
//! watching is recorded in the audit log (`view_start`, `view_end`).

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
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

use super::{capture, encode::FrameEncoder};
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
}

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
    let capture = match tokio::task::spawn_blocking(move || {
        capture::start(req.monitor, req.max_fps, req.max_long_edge.clamp(320, 3840))
    })
    .await
    {
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
    if !reply(&mut send, json!({"ok": true, "monitors": list, "screen": screen})).await {
        return;
    }
    let audit_line =
        |kind: &str| json!({"kind": kind, "peer": peer.name, "principal": peer.principal, "monitor": req.monitor});
    host.audit().event(audit_line("view_start"));

    // Encoding runs on a blocking thread; frames come back over a small channel, so a slow network
    // makes the encoder wait, and the capture slot skips frames meanwhile.
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(1);
    let keyframe = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let encoder = {
        let (slot, keyframe, stop) = (capture.slot(), keyframe.clone(), stop.clone());
        let (fps, kbps, edge) = (req.max_fps.clamp(1, 60), req.bitrate_kbps.clamp(300, 50_000), req.max_long_edge);
        tokio::task::spawn_blocking(move || {
            let mut enc = match FrameEncoder::new(fps, kbps * 1000, edge.clamp(320, 3840)) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "live view encoder");
                    return;
                }
            };
            let started = Instant::now();
            let mut latest = None;
            while !stop.load(Ordering::Relaxed) {
                let Some(next) = slot.take(Duration::from_millis(50)) else { break };
                if let Some(raw) = next {
                    latest = Some(raw);
                } else if !keyframe.load(Ordering::Relaxed) {
                    continue;
                }
                // Event-driven capture emits no new pixels for a static desktop. A decoder
                // recovering from loss must still get an IDR of the last complete frame.
                let Some(raw) = latest.as_ref() else { continue };
                if crate::platform::prompt_blocker().is_some() {
                    slot.close();
                    break;
                }
                if keyframe.swap(false, Ordering::Relaxed) {
                    enc.force_keyframe();
                }
                let frame = match enc.encode_raw(raw) {
                    Ok(Some(f)) => f,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::warn!(error = %format!("{e:#}"), "live view frame");
                        continue;
                    }
                };
                let ts = started.elapsed().as_micros() as u64;
                if tx.blocking_send(frame_bytes(&frame.data, frame.keyframe, frame.width, frame.height, ts)).is_err() {
                    break;
                }
            }
        })
    };

    let mut line = Vec::new();
    loop {
        tokio::select! {
            frame = rx.recv() => match frame {
                Some(bytes) => {
                    if !write_frame(&mut send, &mut reader, &bytes, &mut line, &keyframe, host.stopping()).await { break; }
                },
                None => break,
            },
            read = reader.read_until(b'\n', &mut line) => match read {
                Ok(n) if n > 0 => {
                    if serde_json::from_slice::<Control>(&line).is_ok_and(|c| c.keyframe) {
                        keyframe.store(true, Ordering::Relaxed);
                    }
                    line.clear();
                }
                // The controller finished its half: it stopped watching.
                _ => break,
            },
            _ = host.stopping().cancelled() => break,
            _ = send.stopped() => break,
        }
    }
    stop.store(true, Ordering::Relaxed);
    drop(rx);
    let _ = tokio::task::spawn_blocking(move || drop(capture)).await;
    let _ = encoder.await;
    let _ = send.finish();
    host.audit().event(audit_line("view_end"));
}

/// A partial frame is terminal on failure. Keep cancellation and keyframe controls responsive
/// during network backpressure, and retain partial control lines across select cancellations.
async fn write_frame<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin>(
    send: &mut W,
    reader: &mut R,
    bytes: &[u8],
    line: &mut Vec<u8>,
    keyframe: &AtomicBool,
    stop: &tokio_util::sync::CancellationToken,
) -> bool {
    let write = tokio::time::timeout(Duration::from_secs(2), send.write_all(bytes));
    tokio::pin!(write);
    loop {
        tokio::select! {
            result = &mut write => return matches!(result, Ok(Ok(()))),
            read = reader.read_until(b'\n', line) => match read {
                Ok(n) if n > 0 => {
                    if serde_json::from_slice::<Control>(line).is_ok_and(|c| c.keyframe) { keyframe.store(true, Ordering::Relaxed); }
                    line.clear();
                },
                _ => return false,
            },
            _ = stop.cancelled() => return false,
        }
    }
}

/// Writes one JSON line; `false` when the controller is gone.
async fn reply(send: &mut SendStream, v: serde_json::Value) -> bool {
    let mut line = v.to_string();
    line.push('\n');
    send.write_all(line.as_bytes()).await.is_ok()
}

/// One frame in the wire format of this module.
pub fn frame_bytes(data: &[u8], keyframe: bool, width: u32, height: u32, ts_us: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(18 + data.len());
    out.push(1);
    out.push(keyframe as u8);
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
        let mut line = Vec::new();
        let started = Instant::now();
        let controls = async {
            peer.write_all(b"{\"keyframe\":true}\n").await.unwrap();
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
            tokio::join!(write_frame(&mut send, &mut reader, &[0; 4096], &mut line, &keyframe, &stop), controls);
        assert!(!sent, "cancelled partial frames must terminate instead of continuing");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(keyframe.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn partial_control_json_survives_an_interleaved_frame_write() {
        let (mut send, mut receiver) = tokio::io::duplex(1);
        let (control, mut peer) = tokio::io::duplex(128);
        let mut reader = BufReader::new(control);
        let stop = tokio_util::sync::CancellationToken::new();
        let keyframe = AtomicBool::new(false);
        let mut line = Vec::new();
        peer.write_all(b"{\"key").await.unwrap();
        let receive = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            let mut bytes = [0; 4];
            receiver.read_exact(&mut bytes).await.unwrap();
            assert_eq!(bytes, [1, 2, 3, 4]);
        };
        let (sent, _) =
            tokio::join!(write_frame(&mut send, &mut reader, &[1, 2, 3, 4], &mut line, &keyframe, &stop), receive);
        assert!(sent);
        assert_eq!(line, b"{\"key");
        peer.write_all(b"frame\":true}\n").await.unwrap();
        reader.read_until(b'\n', &mut line).await.unwrap();
        assert!(serde_json::from_slice::<Control>(&line).unwrap().keyframe);
    }

    #[test]
    fn frames_have_an_18_byte_header() {
        let b = super::frame_bytes(&[0, 0, 0, 1, 0x67], true, 1920, 1080, 42);
        assert_eq!(b.len(), 18 + 5);
        assert_eq!(&b[..2], &[1, 1]);
        assert_eq!(u16::from_be_bytes([b[2], b[3]]), 1920);
        assert_eq!(u16::from_be_bytes([b[4], b[5]]), 1080);
        assert_eq!(u64::from_be_bytes(b[6..14].try_into().unwrap()), 42);
        assert_eq!(u32::from_be_bytes(b[14..18].try_into().unwrap()), 5);
    }
}
