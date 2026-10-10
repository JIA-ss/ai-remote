//! mcpmesh-codec: the family's ONE wire codec — compact JSON, UTF-8,
//! one frame per `\n`, 16 MiB cap.
//!
//! Both ends of every wire share THIS implementation: the daemon side re-exports it as
//! `mcpmesh_net::framing`, and the no-iroh client side (kb, loc, the host shell) as
//! `mcpmesh_local_api::codec` — so the two ends can never drift apart. Deliberately tiny:
//! serde_json + tokio io traits only, NO iroh. Session POLICY (violation strikes,
//! synthesized error frames) stays with the session owners (`mcpmesh-net`); only the pure
//! codec lives here.
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// 16 MiB — the family frame cap. App payloads are ≤1 MiB by policy.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// One inbound frame, or a framing violation (kept distinct so callers can log/strike).
///
/// Deliberately EXHAUSTIVE (no `#[non_exhaustive]`): a reader yields exactly these
/// two outcomes, and every caller must decide both — a silently-ignorable third
/// outcome would be a codec bug, not an extension point.
#[derive(Debug)]
pub enum Inbound {
    Frame(Value),
    Violation(Violation),
}

/// How an inbound frame violated the codec. `#[non_exhaustive]` so a future
/// violation kind is not a breaking change — match with a wildcard arm.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Violation {
    TooLarge,
    InvalidJson,
}

/// NDJSON frame reader over any `AsyncRead`. Reads byte-at-a-time from an INTERNAL
/// `BufReader` (misuse-proof: callers cannot forget the buffering and pay a
/// poll-per-byte penalty).
///
/// `next()` is cancellation-safe: all parse state lives in `self`; dropping the
/// future loses no bytes.
#[derive(Debug)]
pub struct FrameReader<R> {
    reader: BufReader<R>,
    max_frame: usize,
    buf: Vec<u8>,
    /// true while skipping the remainder of an oversized line
    discarding: bool,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    pub fn new(reader: R, max_frame: usize) -> Self {
        debug_assert!(max_frame > 0);
        Self {
            reader: BufReader::new(reader),
            max_frame,
            buf: Vec::new(),
            discarding: false,
        }
    }

    /// Unwrap to the BUFFERED reader half. Returns the internal `BufReader` — NOT the
    /// raw stream — so bytes already read ahead (e.g. a frame the peer pipelined behind
    /// the one just parsed) travel WITH the reader instead of being silently dropped.
    /// Call this only BETWEEN frames: any partially-accumulated frame bytes in the
    /// codec's own line buffer are discarded (immediately after a successful `next()`
    /// that buffer is empty, so the hand-off is lossless).
    pub fn into_inner(self) -> BufReader<R> {
        self.reader
    }

    /// `Ok(None)` = clean EOF. Violations do not consume the following frames.
    ///
    /// EOF while discarding an oversized line still reports `TooLarge` (the next
    /// call returns `Ok(None)`). A truncated ORDINARY frame at EOF is silently
    /// dropped (matches MCP stdio convention — deliberate).
    pub async fn next(&mut self) -> std::io::Result<Option<Inbound>> {
        let mut byte = [0u8; 1];
        loop {
            let n = self.reader.read(&mut byte).await?;
            if n == 0 {
                if std::mem::take(&mut self.discarding) {
                    return Ok(Some(Inbound::Violation(Violation::TooLarge)));
                }
                return Ok(None);
            }
            if byte[0] == b'\n' {
                if std::mem::take(&mut self.discarding) {
                    return Ok(Some(Inbound::Violation(Violation::TooLarge)));
                }
                // Re-grows from zero each frame by design: no 16MiB pinned per idle
                // session; do not "optimize" into buffer retention.
                let line = std::mem::take(&mut self.buf);
                // An empty line is InvalidJson (-32700) and strikes: recorded decision.
                return Ok(Some(match serde_json::from_slice::<Value>(&line) {
                    Ok(v) => Inbound::Frame(v),
                    Err(_) => Inbound::Violation(Violation::InvalidJson),
                }));
            }
            if self.discarding {
                continue;
            }
            if self.buf.len() >= self.max_frame {
                self.discarding = true;
                self.buf.clear();
                continue;
            }
            self.buf.push(byte[0]);
        }
    }
}

/// Serialize `frame` and write it terminated by `\n`, flushing (a BufWriter-safe no-op
/// for unbuffered writers — makes BufWriter wrapping misuse-proof).
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(frame)?;
    line.push(b'\n');
    w.write_all(&line).await?;
    w.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reader_over(bytes: &[u8], cap: usize) -> FrameReader<&[u8]> {
        FrameReader::new(bytes, cap)
    }

    #[tokio::test]
    async fn valid_frames_roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
            .await
            .unwrap();
        write_frame(&mut buf, &json!({"jsonrpc":"2.0","id":2,"method":"pong"}))
            .await
            .unwrap();
        let mut r = reader_over(&buf, 1024);
        assert!(matches!(r.next().await.unwrap().unwrap(), Inbound::Frame(v) if v["id"] == 1));
        assert!(matches!(r.next().await.unwrap().unwrap(), Inbound::Frame(v) if v["id"] == 2));
        assert!(r.next().await.unwrap().is_none()); // clean EOF
    }

    #[tokio::test]
    async fn oversized_frame_is_discarded_and_reported_and_stream_continues() {
        let big = format!("{{\"pad\":\"{}\"}}\n", "x".repeat(100));
        let mut bytes = big.into_bytes();
        bytes.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ok\"}\n");
        let mut r = reader_over(&bytes, 64);
        assert!(matches!(
            r.next().await.unwrap().unwrap(),
            Inbound::Violation(Violation::TooLarge)
        ));
        assert!(matches!(r.next().await.unwrap().unwrap(), Inbound::Frame(v) if v["id"] == 7));
    }

    #[tokio::test]
    async fn invalid_json_is_a_violation() {
        let mut r = reader_over(b"not json at all\n", 1024);
        assert!(matches!(
            r.next().await.unwrap().unwrap(),
            Inbound::Violation(Violation::InvalidJson)
        ));
    }

    #[tokio::test]
    async fn oversized_frame_truncated_by_eof_is_still_reported() {
        let bytes = format!("{{\"pad\":\"{}\"}}", "x".repeat(100)).into_bytes(); // no newline
        let mut r = reader_over(&bytes, 64);
        assert!(matches!(
            r.next().await.unwrap().unwrap(),
            Inbound::Violation(Violation::TooLarge)
        ));
        assert!(r.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn back_to_back_oversized_frames_then_valid_frame() {
        let big = format!("{{\"pad\":\"{}\"}}\n", "x".repeat(100));
        let mut bytes = big.clone().into_bytes();
        bytes.extend_from_slice(big.as_bytes());
        bytes.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ok\"}\n");
        let mut r = reader_over(&bytes, 64);
        assert!(matches!(
            r.next().await.unwrap().unwrap(),
            Inbound::Violation(Violation::TooLarge)
        ));
        assert!(matches!(
            r.next().await.unwrap().unwrap(),
            Inbound::Violation(Violation::TooLarge)
        ));
        assert!(matches!(r.next().await.unwrap().unwrap(), Inbound::Frame(v) if v["id"] == 9));
    }

    #[tokio::test]
    async fn frame_of_exactly_max_frame_bytes_passes_one_more_is_too_large() {
        // A bare JSON string is a valid frame; 62 x's + 2 quotes = exactly 64 bytes
        // before the newline terminator (the newline is not counted against the cap).
        let payload = "x".repeat(62);
        let exact = format!("\"{payload}\"\n");
        let mut r = reader_over(exact.as_bytes(), 64);
        assert!(matches!(r.next().await.unwrap().unwrap(), Inbound::Frame(v) if v == payload));

        let over = format!("\"{}\"\n", "x".repeat(63)); // 65 bytes before the newline
        let mut r = reader_over(over.as_bytes(), 64);
        assert!(matches!(
            r.next().await.unwrap().unwrap(),
            Inbound::Violation(Violation::TooLarge)
        ));
    }

    /// `into_inner` hands back the internal `BufReader`, so read-ahead the codec already
    /// pulled off the stream (a pipelined second frame) survives the hand-off and is
    /// readable through a NEW FrameReader over the returned reader. This is the
    /// lossless-rebox contract kb's mesh opener relies on (mcpmesh-local/1 OpenSession).
    #[tokio::test]
    async fn into_inner_preserves_pipelined_read_ahead() {
        let bytes = b"{\"id\":1}\n{\"id\":2}\n";
        let mut r = reader_over(bytes, 1024);
        // Frame 1 consumed; the internal BufReader has (with an 8 KiB buffer over a
        // ready slice) already read frame 2 ahead.
        assert!(matches!(r.next().await.unwrap().unwrap(), Inbound::Frame(v) if v["id"] == 1));
        // Re-box exactly like kb does: into_inner -> Box<dyn AsyncRead> -> new reader.
        let boxed: Box<dyn AsyncRead + Unpin + Send> = Box::new(r.into_inner());
        let mut r2 = FrameReader::new(boxed, 1024);
        assert!(matches!(r2.next().await.unwrap().unwrap(), Inbound::Frame(v) if v["id"] == 2));
        assert!(r2.next().await.unwrap().is_none());
    }
}
