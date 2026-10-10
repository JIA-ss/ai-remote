//! Adapts a framed byte stream (QUIC bi-stream, UDS, or in-memory pipe) to rmcp's
//! [`Transport`](rmcp::transport::Transport) so both ends of a session speak MCP
//! over our one codec.

use std::sync::Arc;

use rmcp::ErrorData;
use rmcp::service::{RxJsonRpcMessage, ServiceRole, TxJsonRpcMessage};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Mutex;

use crate::framing::{FrameReader, Inbound, Violation, write_frame};

/// One MCP endpoint's byte stream, framed by the family codec.
///
/// The writer sits behind `Arc<Mutex<Option<W>>>` because rmcp's
/// `Transport::send` must return a `Send + 'static` future (sends may run
/// concurrently), so it cannot borrow `&mut self`. `None` marks a closed
/// transport. Same shape as rmcp's own `AsyncRwTransport`.
pub struct NdjsonTransport<R, W> {
    reader: FrameReader<R>, // FrameReader buffers internally (mcpmesh-codec)
    writer: Arc<Mutex<Option<W>>>,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> NdjsonTransport<R, W> {
    pub fn new(reader: R, writer: W, max_frame: usize) -> Self {
        Self {
            reader: FrameReader::new(reader, max_frame),
            writer: Arc::new(Mutex::new(Some(writer))),
        }
    }

    pub async fn send_value(&mut self, v: Value) -> std::io::Result<()> {
        send_locked(&self.writer, &v).await
    }

    /// A cloneable handle to this transport's write half. The writer already lives
    /// behind `Arc<Mutex<Option<W>>>` (sends are `'static`), so this handle can send
    /// frames CONCURRENTLY with a [`recv_value`](Self::recv_value) on the same
    /// transport — the two directions run as independent loops. That concurrency is
    /// what lets a full-duplex pump avoid deadlock: a blocked write in one direction
    /// never stalls the other direction's reads (used by the mcpmesh backends' shared
    /// pump). The handle sends through the SAME single codec path as `send_value`.
    pub fn writer(&self) -> TransportWriter<W> {
        TransportWriter {
            writer: Arc::clone(&self.writer),
        }
    }

    /// Gracefully close the write half: shut the stream down so an iroh
    /// `SendStream` sends its FIN — flushing any buffered frame (e.g. a final
    /// refusal error) to the peer — instead of being dropped abruptly. For
    /// iroh's `SendStream`, tokio's `AsyncWrite::poll_shutdown` calls `finish()`;
    /// for in-memory writers it is a graceful no-op. A no-op once the transport
    /// is already closed. This is the orderly-teardown primitive the session
    /// loop uses (a bare drop abandons buffered data).
    pub async fn shutdown(&mut self) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        if let Some(w) = self.writer.lock().await.as_mut() {
            w.shutdown().await?;
        }
        Ok(())
    }

    /// Ok(None) = clean EOF. Violations surface as a TYPED error carrying the
    /// [`Violation`]: the session loop needs the discriminant to choose -32051
    /// vs -32700.
    pub async fn recv_value(&mut self) -> Result<Option<Value>, RecvError> {
        match self.reader.next().await? {
            None => Ok(None),
            Some(Inbound::Frame(v)) => Ok(Some(v)),
            Some(Inbound::Violation(v)) => Err(RecvError::Violation(v)),
        }
    }
}

/// A cloneable send-half of an [`NdjsonTransport`], obtained via
/// [`NdjsonTransport::writer`]. Holds an `Arc` clone of the transport's writer, so a
/// bidirectional pump can send through this while `recv_value` reads on the transport
/// — the two directions run concurrently without a shared `&mut self` (the deadlock
/// fix). Once the transport is closed (its writer taken), `send_value` errors
/// `NotConnected` rather than hanging.
pub struct TransportWriter<W> {
    writer: Arc<Mutex<Option<W>>>,
}

impl<W: AsyncWrite + Unpin> TransportWriter<W> {
    /// Send one frame through the shared write half (the SAME single codec path as
    /// [`NdjsonTransport::send_value`]).
    pub async fn send_value(&self, v: Value) -> std::io::Result<()> {
        send_locked(&self.writer, &v).await
    }

    /// Half-close: finish the shared write half — a clean end-of-input toward the peer —
    /// while the transport's read half stays open to drain responses still in flight. This
    /// is the teardown discipline for a consumer with no more requests to send: closing the
    /// whole transport instead would race away the reply to the final request.
    pub async fn shutdown(&self) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        if let Some(w) = self.writer.lock().await.as_mut() {
            w.shutdown().await?;
        }
        Ok(())
    }
}

/// The -32600 reply the rmcp `receive` path sends when a frame is valid JSON but
/// not a JSON-RPC message for `Role`. This is platform-synthesized (the transport
/// answers before any backend sees the frame), so it MUST carry the
/// `data.source: "mcpmesh"` marker — parity with [`crate::errors::synthesized`],
/// the producer of the -32051/-32054/-32700 errors.
fn invalid_request_reply<Role: ServiceRole>() -> TxJsonRpcMessage<Role> {
    TxJsonRpcMessage::<Role>::error(
        ErrorData::invalid_request(
            "Invalid request",
            Some(serde_json::json!({ "source": "mcpmesh" })),
        ),
        None,
    )
}

/// The one write path: `send_value` and the rmcp `Transport::send` future both
/// serialize through here (one codec — no second framing path).
async fn send_locked<W: AsyncWrite + Unpin>(
    writer: &Mutex<Option<W>>,
    frame: &Value,
) -> std::io::Result<()> {
    match writer.lock().await.as_mut() {
        Some(w) => write_frame(w, frame).await,
        None => Err(std::io::Error::new(
            std::io::ErrorKind::NotConnected,
            "transport closed",
        )),
    }
}

/// Why a [`NdjsonTransport::recv_value`] read failed: the underlying stream
/// errored, or the peer sent a frame that violates the codec. `#[non_exhaustive]`
/// so a future failure kind is not a breaking change — match with a wildcard arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RecvError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("framing violation: {0:?}")]
    Violation(Violation),
}

/// rmcp reality (reconciled, and re-verified byte-for-byte across the 2.1.0 -> 3.0.0-beta.3
/// major — the `Transport` trait did not change): `Transport<R: ServiceRole>` with
/// `type Error`, `send() -> impl Future + Send + 'static`,
/// `receive() -> impl Future<Output = Option<RxJsonRpcMessage<R>>>` (no error
/// channel), and `close()`. One `NdjsonTransport` serves either role — unlike
/// rmcp's `AsyncRwTransport` it needs no role phantom because our wire type is
/// `Value`, role-agnostic by construction.
impl<Role, R, W> rmcp::transport::Transport<Role> for NdjsonTransport<R, W>
where
    Role: ServiceRole,
    R: AsyncRead + Send + Unpin,
    W: AsyncWrite + Send + Unpin + 'static,
{
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<Role>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let writer = Arc::clone(&self.writer);
        async move {
            let frame = serde_json::to_value(&item)?;
            send_locked(&writer, &frame).await
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<Role>> {
        loop {
            let frame = match self.recv_value().await {
                Ok(Some(v)) => v,
                Ok(None) => return None,
                Err(RecvError::Io(e)) => {
                    tracing::error!("transport read error: {e}");
                    return None;
                }
                // Framing violations have no channel through rmcp's `receive`
                // (it yields Option, not Result). Platform violation semantics
                // (-32051/-32700 + strikes) belong to the session loop, which
                // drives `recv_value` directly; here we skip the
                // frame and keep reading, matching rmcp's own AsyncRwTransport
                // treatment of unparsable input.
                Err(RecvError::Violation(v)) => {
                    tracing::debug!("ignoring framing violation: {v:?}");
                    continue;
                }
            };
            match serde_json::from_value::<RxJsonRpcMessage<Role>>(frame) {
                Ok(msg) => return Some(msg),
                // Valid JSON that isn't a JSON-RPC message for this role:
                // answer -32600 (as rmcp's AsyncRwTransport does) and keep
                // reading. This reply is platform-synthesized (before any
                // backend sees the frame), so it MUST carry the
                // `data.source: "mcpmesh"` marker — parity with
                // `errors::synthesized`, the other producer of synthesized errors.
                Err(e) => {
                    tracing::debug!("protocol-shape error on incoming message: {e}");
                    let Ok(frame) = serde_json::to_value(invalid_request_reply::<Role>()) else {
                        return None;
                    };
                    if send_locked(&self.writer, &frame).await.is_err() {
                        return None;
                    }
                }
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        drop(self.writer.lock().await.take());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rmcp::RoleServer;
    use rmcp::transport::Transport;
    use serde_json::json;
    use tokio::time::timeout;

    use super::*;

    /// Pin `Role = RoleServer` so the tests drive the REAL rmcp trait methods
    /// (the blanket `Transport<Role>` impl leaves the role ambiguous at call
    /// sites otherwise).
    async fn srv_receive<T: Transport<RoleServer>>(
        t: &mut T,
    ) -> Option<RxJsonRpcMessage<RoleServer>> {
        t.receive().await
    }

    async fn srv_send<T: Transport<RoleServer>>(
        t: &mut T,
        m: TxJsonRpcMessage<RoleServer>,
    ) -> Result<(), T::Error> {
        t.send(m).await
    }

    async fn srv_close<T: Transport<RoleServer>>(t: &mut T) -> Result<(), T::Error> {
        t.close().await
    }

    /// Family MUST: EVERY platform-synthesized JSON-RPC error carries
    /// `data.source == "mcpmesh"`. There are two producers of such errors in the
    /// net crate — `errors::synthesized` (the -32051/-32054/-32700 codes) and
    /// the transport's -32600 wrong-shape reply (`invalid_request_reply`). This
    /// table asserts the marker on a representative error from each, so a future
    /// producer that forgets the marker fails here rather than silently on the
    /// wire.
    #[test]
    fn every_synthesized_error_carries_the_marker() {
        use crate::errors::{ERR_FRAMING, ERR_PARSE, ERR_SERVICE, synthesized};

        for e in [
            synthesized(json!(1), ERR_FRAMING, "frame too large"),
            synthesized(json!(1), ERR_SERVICE, "unknown or unauthorized service"),
            synthesized(Value::Null, ERR_PARSE, "parse error"),
            // Serialized exactly as the rmcp receive() path emits it.
            serde_json::to_value(invalid_request_reply::<RoleServer>()).unwrap(),
        ] {
            assert_eq!(
                e["error"]["data"]["source"], "mcpmesh",
                "producer emitted a synthesized error without the marker: {e}"
            );
        }
    }

    #[tokio::test]
    async fn adapter_moves_messages_both_directions() {
        let (a, b) = tokio::io::duplex(4096);
        let (ar, aw) = tokio::io::split(a);
        let (br, bw) = tokio::io::split(b);
        let mut left = NdjsonTransport::new(ar, aw, 1024 * 1024);
        let mut right = NdjsonTransport::new(br, bw, 1024 * 1024);

        left.send_value(json!({"jsonrpc":"2.0","id":1,"method":"ping"}))
            .await
            .unwrap();
        let got = right.recv_value().await.unwrap().unwrap();
        assert_eq!(got["method"], "ping");

        right
            .send_value(json!({"jsonrpc":"2.0","id":1,"result":{}}))
            .await
            .unwrap();
        let back = left.recv_value().await.unwrap().unwrap();
        assert_eq!(back["id"], 1);
    }

    /// The `writer()` split sends CONCURRENTLY with `recv_value` on the SAME transport
    /// (disjoint halves: `recv_value` holds `&mut left` for the reader while the cloned
    /// writer handle drives the write half) — the primitive the backends' bidirectional
    /// pump relies on to avoid a full-duplex deadlock.
    #[tokio::test]
    async fn writer_handle_sends_concurrently_with_recv() {
        timeout(Duration::from_secs(5), async {
            let (a, b) = tokio::io::duplex(4096);
            let (ar, aw) = tokio::io::split(a);
            let (br, bw) = tokio::io::split(b);
            let mut left = NdjsonTransport::new(ar, aw, 1024 * 1024);
            let mut right = NdjsonTransport::new(br, bw, 1024 * 1024);

            // On the SAME transport `left`: a `send` via the cloned writer handle and a
            // `recv_value` run in one `join!`. The peer (`right`) reads left's ping and
            // sends a pong back, which left's concurrent recv must observe.
            let left_writer = left.writer();
            let send = left_writer.send_value(json!({"jsonrpc":"2.0","id":1,"method":"ping"}));
            let recv = left.recv_value();
            let peer = async {
                let ping = right.recv_value().await.unwrap().unwrap();
                assert_eq!(ping["method"], "ping");
                right
                    .send_value(json!({"jsonrpc":"2.0","id":1,"result":{}}))
                    .await
                    .unwrap();
            };
            let (sent, got, ()) = tokio::join!(send, recv, peer);
            sent.unwrap();
            assert_eq!(got.unwrap().unwrap()["id"], 1);
        })
        .await
        .expect("writer handle test timed out");
    }

    /// Violations surface TYPED, carrying the discriminant the session loop
    /// needs to choose -32051 vs -32700.
    #[tokio::test]
    async fn recv_value_surfaces_typed_violations() {
        let mut t = NdjsonTransport::new(&b"not json\n"[..], Vec::new(), 64);
        match t.recv_value().await {
            Err(RecvError::Violation(crate::framing::Violation::InvalidJson)) => {}
            other => panic!("expected InvalidJson violation, got {other:?}"),
        }

        let big = format!("\"{}\"\n", "x".repeat(100));
        let mut t = NdjsonTransport::new(big.as_bytes(), Vec::new(), 64);
        match t.recv_value().await {
            Err(RecvError::Violation(crate::framing::Violation::TooLarge)) => {}
            other => panic!("expected TooLarge violation, got {other:?}"),
        }
    }

    /// Seam behavior (a): a framing violation mid-stream is skipped by the rmcp
    /// `Transport::receive` path — the NEXT valid frame is still delivered and
    /// EOF is still a clean `None`. (Strike policy stays in the session loop.)
    #[tokio::test]
    async fn rmcp_receive_skips_violation_and_continues() {
        let raw = format!(
            "\"{}\"\n{}\n",
            "x".repeat(100), // oversized under the 64-byte cap -> TooLarge
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#
        );
        let mut t = NdjsonTransport::new(raw.as_bytes(), Vec::new(), 64);

        let msg = srv_receive(&mut t)
            .await
            .expect("valid frame after skipped violation");
        assert_eq!(serde_json::to_value(&msg).unwrap()["method"], "ping");
        assert!(srv_receive(&mut t).await.is_none(), "then clean EOF");
    }

    /// Seam behavior (b): valid JSON that is not a JSON-RPC message for the
    /// role draws a -32600 invalid_request reply on the wire, and the stream
    /// keeps going — the next valid frame is delivered.
    #[tokio::test]
    async fn rmcp_receive_answers_wrong_shape_with_invalid_request_and_continues() {
        timeout(Duration::from_secs(5), async {
            let (a, b) = tokio::io::duplex(4096);
            let (ar, aw) = tokio::io::split(a);
            let (br, bw) = tokio::io::split(b);
            let mut server = NdjsonTransport::new(br, bw, 1024 * 1024);
            let mut probe = NdjsonTransport::new(ar, aw, 1024 * 1024);

            probe
                .send_value(json!({"valid":"json","but":"not jsonrpc"}))
                .await
                .unwrap();
            // receive() loops past the bad frame, so drive it concurrently.
            let srv = tokio::spawn(async move { (srv_receive(&mut server).await, server) });

            let reply = probe.recv_value().await.unwrap().unwrap();
            assert_eq!(reply["error"]["code"], -32600);
            // Family MUST: this platform-synthesized error carries the marker,
            // and it survives serialize -> wire -> parse (proves ErrorData.data
            // round-trips, not just that it was set).
            assert_eq!(reply["error"]["data"]["source"], "mcpmesh");

            probe
                .send_value(json!({"jsonrpc":"2.0","id":2,"method":"ping"}))
                .await
                .unwrap();
            let (msg, _server) = srv.await.unwrap();
            let v = serde_json::to_value(msg.expect("stream continued past bad shape")).unwrap();
            assert_eq!(v["method"], "ping");
            assert_eq!(v["id"], 2);
        })
        .await
        .expect("wrong-shape roundtrip timed out");
    }

    /// Seam behavior (c): `close()` takes the writer; a later `send()` through
    /// the rmcp trait fails NotConnected instead of hanging or panicking.
    #[tokio::test]
    async fn rmcp_send_after_close_is_not_connected() {
        let mut t = NdjsonTransport::new(&b""[..], Vec::new(), 64);
        srv_close(&mut t).await.unwrap();

        let err = srv_send(
            &mut t,
            TxJsonRpcMessage::<RoleServer>::error(
                ErrorData::invalid_request("closed-path probe", None),
                None,
            ),
        )
        .await
        .expect_err("send after close must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::NotConnected);
    }

    /// The reconcile proof: a real rmcp client and server complete the MCP
    /// initialize handshake (requests + responses, both directions) with our
    /// codec as the only framing path.
    #[tokio::test]
    async fn rmcp_handshake_completes_over_ndjson_transport() {
        struct NullServer;
        impl rmcp::ServerHandler for NullServer {}

        timeout(Duration::from_secs(5), async {
            let (a, b) = tokio::io::duplex(4096);
            let (ar, aw) = tokio::io::split(a);
            let (br, bw) = tokio::io::split(b);

            let server_task = tokio::spawn(rmcp::serve_server(
                NullServer,
                NdjsonTransport::new(br, bw, 1024 * 1024),
            ));
            let client = rmcp::serve_client((), NdjsonTransport::new(ar, aw, 1024 * 1024))
                .await
                .expect("client handshake over NdjsonTransport");
            let server = server_task
                .await
                .unwrap()
                .expect("server handshake over NdjsonTransport");

            assert!(
                client.peer().peer_info().is_some(),
                "an InitializeResult must have round-tripped"
            );

            client.cancel().await.unwrap();
            server.cancel().await.unwrap();
        })
        .await
        .expect("handshake timed out");
    }
}
