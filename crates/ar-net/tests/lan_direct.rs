//! LAN mode end to end on one machine: two nodes with no relay and no address lookup pair and
//! dial each other at the stored address. When the host comes back on another port the controller
//! cannot find it (there is nothing to look it up in) until its address is set by hand.

use std::{path::Path, sync::Arc, time::Duration};

use ar_core::{Level, Role, config::NetworkSection};
use ar_net::{
    Mesh, MeshOptions, NetworkOptions,
    iroh::{
        endpoint::Connection,
        protocol::{AcceptError, ProtocolHandler},
    },
};

const ALPN: &[u8] = b"test/ai-remote/echo/1";

#[derive(Debug)]
struct Echo;

impl ProtocolHandler for Echo {
    async fn accept(&self, conn: Connection) -> Result<(), AcceptError> {
        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
            if let Ok(bytes) = recv.read_to_end(64).await {
                let _ = send.write_all(&bytes).await;
                let _ = send.finish();
            }
        }
        Ok(())
    }
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn start(dir: &Path, key: u8, name: &str, role: Role, port: Option<u16>) -> Mesh {
    let section = NetworkSection { mode: Some("lan".into()), port, ..Default::default() };
    Mesh::start(MeshOptions {
        root: dir.into(),
        device_key: [key; 32],
        nickname: name.into(),
        network: NetworkOptions::from_config(&section, role).unwrap(),
    })
    .await
    .unwrap()
}

async fn start_host(dir: &Path, port: u16) -> Mesh {
    let host = start(dir, 1, "host", Role::Host, Some(port)).await;
    host.ensure_level_services().await.unwrap();
    host.accept(ALPN, Arc::new(Echo)).unwrap();
    host
}

/// One round trip, bounded: a host that cannot be found costs a few seconds, not the dial timeout.
async fn echo(controller: &Mesh) -> anyhow::Result<()> {
    let conn = tokio::time::timeout(Duration::from_secs(5), controller.dial("host", ALPN)).await??;
    let (mut send, mut recv) = conn.open_bi().await?;
    send.write_all(b"ping").await?;
    send.finish()?;
    let got = tokio::time::timeout(Duration::from_secs(5), recv.read_to_end(64)).await??;
    anyhow::ensure!(got == b"ping", "echo returned {got:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn lan_mode_pairs_dials_and_is_found_again_at_an_address_set_by_hand() {
    let host_dir = tempfile::tempdir().unwrap();
    let ctl_dir = tempfile::tempdir().unwrap();
    let first_port = free_udp_port();

    let host = start_host(host_dir.path(), first_port).await;
    assert!(host.lan_direct());
    let controller = start(ctl_dir.path(), 2, "ctl", Role::Controller, None).await;
    let invite = host.invite(Level::Observe, Some("ctl".into())).await.unwrap();
    controller.pair(&invite.line, Some("host")).await.unwrap();
    echo(&controller).await.expect("paired over the ticket's direct addresses");
    let stored = controller.peer_addrs("host").await.unwrap();
    assert!(stored.iter().any(|a| a.port() == first_port), "the hint carries the host's port: {stored:?}");

    // The host comes back on another port; a fresh controller only has the old address.
    host.shutdown().await;
    controller.shutdown().await;
    let second_port = free_udp_port();
    let host = start_host(host_dir.path(), second_port).await;
    let controller = start(ctl_dir.path(), 2, "ctl", Role::Controller, None).await;
    assert!(echo(&controller).await.is_err(), "no relay and no lookup: the old address is a dead end");

    let set = controller.set_peer_addrs("host", &[format!("127.0.0.1:{second_port}").parse().unwrap()]).await.unwrap();
    assert_eq!(set.len(), 1);
    echo(&controller).await.expect("reached again at the address set by hand");

    controller.shutdown().await;
    host.shutdown().await;
}
