//! Endpoint smoke: two in-process endpoints, dial by key, echo over one bi-stream.
//! Relay disabled; direct localhost addresses only — no external infra in CI.
use anyhow::Result;
use std::time::Duration;

const ALPN: &[u8] = b"mcpmesh/spike/0";

#[tokio::test]
async fn dial_by_key_and_echo() -> Result<()> {
    // Test-level timeout: an iroh regression into a hang fails cleanly in 30s
    // instead of burning the CI job's 30-minute budget as an opaque kill.
    tokio::time::timeout(Duration::from_secs(30), async {
        let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await?;
        let server_addr = server.addr(); // includes direct socket addrs

        let accept_loop = tokio::spawn(async move {
            let incoming = server.accept().await.expect("incoming");
            let conn = incoming.await.expect("handshake");
            let (mut send, mut recv) = conn.accept_bi().await.expect("bi");
            let msg = recv.read_to_end(1024).await.expect("read");
            send.write_all(&msg).await.expect("write");
            send.finish().expect("finish");
            conn.closed().await;
        });

        let client = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await?;
        let conn = client.connect(server_addr, ALPN).await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        send.write_all(b"ping over the mesh").await?;
        send.finish()?;
        let echoed = recv.read_to_end(1024).await?;
        assert_eq!(&echoed, b"ping over the mesh");
        conn.close(0u32.into(), b"done");
        accept_loop.await?;
        Ok(())
    })
    .await?
}
