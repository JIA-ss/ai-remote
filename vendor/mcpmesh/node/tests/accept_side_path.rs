//! #213: the ACCEPTING side of an app-protocol connection loses its selected path while traffic
//! flows, so `Path::is_selected()` is false on the only open IP path — and `Node::connection_path`
//! must answer `Direct` there anyway.
//!
//! Two full nodes on loopback, relay disabled, paired through the real control API; app-protocol
//! connections carrying datagrams both ways.
//!
//! The `*_stay_selected` tests are the iroh REPRODUCTION (measured on 1.0.3 and 1.2.0): they read
//! `is_selected()` raw and are `#[ignore]`d because they FAIL there by design (run with
//! `--ignored` to re-measure; see `docs/superpowers/specs/2026-09-14-213-accept-side-path-design.md`).
//! Whether a given run hits the defect depends on which of the host's addresses each connection
//! settles on, so they are also inherently timing-based. The `connection_path_*` tests are the
//! regression tests for the fix and run in the suite.

use std::sync::Arc;
use std::time::Duration;

use mcpmesh_node::{Config, Node, NodeBuilder, iroh};

const ALPN: &[u8] = b"app/213-repro/1";

/// Hands every accepted connection to the test and echoes datagrams for the connection's life.
#[derive(Debug)]
struct Echo {
    tx: tokio::sync::mpsc::UnboundedSender<iroh::endpoint::Connection>,
}

impl iroh::protocol::ProtocolHandler for Echo {
    async fn accept(
        &self,
        conn: iroh::endpoint::Connection,
    ) -> Result<(), iroh::protocol::AcceptError> {
        let _ = self.tx.send(conn.clone());
        while let Ok(d) = conn.read_datagram().await {
            let _ = conn.send_datagram(d);
        }
        Ok(())
    }
}

fn render(conn: &iroh::endpoint::Connection) -> Vec<String> {
    conn.paths()
        .iter()
        .map(|p| {
            format!(
                "{}:{:?}<-{:?}:selected={}",
                p.id(),
                p.remote_addr(),
                p.local_addr(),
                p.is_selected()
            )
        })
        .collect()
}

struct Peer {
    _root: tempfile::TempDir,
    node: Node,
    /// The OTHER peer's nickname as this node knows it.
    other: String,
    accepted: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<iroh::endpoint::Connection>>,
}

impl Peer {
    async fn dial(&self) -> iroh::endpoint::Connection {
        self.node
            .connect_protocol(&self.other, ALPN)
            .await
            .expect("connect_protocol")
    }
    async fn accepted(&self) -> iroh::endpoint::Connection {
        tokio::time::timeout(Duration::from_secs(5), async {
            self.accepted.lock().await.recv().await
        })
        .await
        .expect("accept within 5s")
        .expect("handler ran")
    }
}

async fn boot(name: &str) -> (tempfile::TempDir, Node) {
    let root = tempfile::tempdir().unwrap();
    let cfg = Config::from_toml_str(&format!(
        "[identity]\nnickname = \"{name}\"\n[network]\nrelay_mode = \"disabled\"\n"
    ))
    .unwrap();
    let node = NodeBuilder::new(root.path())
        .config(cfg)
        .start()
        .await
        .expect("start");
    (root, node)
}

/// Two paired nodes, each serving `ALPN`. `a` invited, `b` redeemed.
async fn paired() -> (Peer, Peer) {
    if std::env::var_os("MCPMESH_213_TRACE").is_some() {
        use tracing_subscriber::prelude::*;
        let filter = tracing_subscriber::filter::filter_fn(|m| {
            m.target().starts_with("iroh::_events::path")
                || m.target()
                    .starts_with("iroh::socket::remote_map::remote_state")
        });
        let _ = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_filter(filter),
            )
            .try_init();
    }
    let (ra, a) = boot("acceptor").await;
    let (rb, b) = boot("dialer").await;
    eprintln!(
        "acceptor endpoint={} dialer endpoint={}",
        a.endpoint_id(),
        b.endpoint_id()
    );
    let mut ca = a.control().await.unwrap();
    ca.register_service_with(
        "notes",
        mcpmesh_local_api::BackendSpec::Socket {
            path: ra.path().join("notes.sock").display().to_string(),
        },
        vec![],
        true,
    )
    .await
    .unwrap();
    let invite = ca.invite(vec!["notes".into()]).await.unwrap();
    let mut cb = b.control().await.unwrap();
    let paired = cb.pair(&invite.invite_line).await.unwrap();
    let b_name = ca.status().await.unwrap().peers[0].name.clone();

    let (tx_a, rx_a) = tokio::sync::mpsc::unbounded_channel();
    a.accept_protocol(ALPN, Arc::new(Echo { tx: tx_a }))
        .unwrap();
    let (tx_b, rx_b) = tokio::sync::mpsc::unbounded_channel();
    b.accept_protocol(ALPN, Arc::new(Echo { tx: tx_b }))
        .unwrap();
    (
        Peer {
            _root: ra,
            node: a,
            other: b_name,
            accepted: tokio::sync::Mutex::new(rx_a),
        },
        Peer {
            _root: rb,
            node: b,
            other: paired.peer_nickname,
            accepted: tokio::sync::Mutex::new(rx_b),
        },
    )
}

/// Keep datagrams flowing both ways on a client-side connection.
fn pump(client: &iroh::endpoint::Connection) -> tokio::task::JoinHandle<()> {
    let client = client.clone();
    tokio::spawn(async move {
        loop {
            let _ = client.send_datagram(bytes::Bytes::from_static(b"tick"));
            let _ = tokio::time::timeout(Duration::from_millis(50), client.read_datagram()).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
}

/// Sample every labelled connection for `secs`; return, per label, how many samples had NO
/// selected path.
async fn observe(conns: &[(&str, &iroh::endpoint::Connection)], secs: u64) -> Vec<(String, usize)> {
    let start = tokio::time::Instant::now();
    let mut last: Vec<Vec<String>> = vec![Vec::new(); conns.len()];
    let mut unselected = vec![0usize; conns.len()];
    let mut samples = 0usize;
    while start.elapsed() < Duration::from_secs(secs) {
        for (i, (label, conn)) in conns.iter().enumerate() {
            let now = render(conn);
            if now != last[i] {
                println!("t+{:.3}s {label} {now:?}", start.elapsed().as_secs_f64());
                last[i] = now;
            }
            if !conn.paths().iter().any(|p| p.is_selected()) {
                unselected[i] += 1;
            }
        }
        samples += 1;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let out: Vec<_> = conns
        .iter()
        .zip(unselected)
        .map(|((l, _), n)| (l.to_string(), n))
        .collect();
    println!("samples={samples} unselected={out:?}");
    out
}

/// One connection: the accept side keeps a selected path (control case).
#[ignore = "iroh reproduction (1.0.3, 1.2.0): fails by design (#213)"]
#[tokio::test(flavor = "multi_thread")]
async fn single_connection_accept_side_stays_selected() {
    let (a, b) = paired().await;
    let client = b.dial().await;
    let server = a.accepted().await;
    let p = pump(&client);
    let r = observe(&[("server", &server), ("client", &client)], 8).await;
    p.abort();
    a.node.shutdown().await;
    b.node.shutdown().await;
    assert!(r.iter().all(|(_, n)| *n == 0), "{r:?}");
}

/// Two connections dialed by the SAME side (b -> a twice): both accept sides must stay selected.
#[ignore = "iroh reproduction (1.0.3, 1.2.0): fails by design (#213)"]
#[tokio::test(flavor = "multi_thread")]
async fn two_connections_same_direction_accept_sides_stay_selected() {
    let (a, b) = paired().await;
    let c1 = b.dial().await;
    let s1 = a.accepted().await;
    let c2 = b.dial().await;
    let s2 = a.accepted().await;
    let p1 = pump(&c1);
    let p2 = pump(&c2);
    let r = observe(
        &[
            ("server1", &s1),
            ("client1", &c1),
            ("server2", &s2),
            ("client2", &c2),
        ],
        8,
    )
    .await;
    p1.abort();
    p2.abort();
    a.node.shutdown().await;
    b.node.shutdown().await;
    assert!(r.iter().all(|(_, n)| *n == 0), "{r:?}");
}

/// Two connections in OPPOSITE directions (b -> a, then a -> b): each node holds one client-side
/// and one server-side connection to the same remote — the shape a daemon is in when it holds a
/// mesh session one way and an app-protocol connection the other.
#[ignore = "iroh reproduction (1.0.3, 1.2.0): fails by design (#213)"]
#[tokio::test(flavor = "multi_thread")]
async fn two_connections_opposite_directions_accept_sides_stay_selected() {
    let (a, b) = paired().await;
    let c1 = b.dial().await;
    let s1 = a.accepted().await;
    let c2 = a.dial().await;
    let s2 = b.accepted().await;
    let p1 = pump(&c1);
    let p2 = pump(&c2);
    let r = observe(
        &[
            ("a-server1", &s1),
            ("b-client1", &c1),
            ("b-server2", &s2),
            ("a-client2", &c2),
        ],
        8,
    )
    .await;
    p1.abort();
    p2.abort();
    a.node.shutdown().await;
    b.node.shutdown().await;
    assert!(r.iter().all(|(_, n)| *n == 0), "{r:?}");
}

/// One `Node::connection_path` reading, with the connection's path list at both edges.
struct Reading {
    /// Seconds since sampling began, at the start and end of the reading.
    start: f64,
    end: f64,
    path: mcpmesh_local_api::PeerPath,
    /// `id:kind:selected` for every listed path, just before and just after the reading.
    before: Vec<String>,
    after: Vec<String>,
}

fn describe_paths(conn: &iroh::endpoint::Connection) -> Vec<String> {
    conn.paths()
        .iter()
        .map(|p| {
            let kind = if p.is_ip() {
                "ip"
            } else if p.is_relay() {
                "relay"
            } else {
                "other"
            };
            format!("{}:{kind}:{}", p.id(), p.is_selected())
        })
        .collect()
}

/// Sample `Node::connection_path` on every labelled connection, round-robin, for `secs`.
async fn readings(
    conns: &[(&str, &iroh::endpoint::Connection)],
    secs: u64,
) -> Vec<(String, Vec<Reading>)> {
    let start = tokio::time::Instant::now();
    let mut all: Vec<Vec<Reading>> = conns.iter().map(|_| Vec::new()).collect();
    while start.elapsed() < Duration::from_secs(secs) {
        for (i, (_, conn)) in conns.iter().enumerate() {
            let t0 = start.elapsed().as_secs_f64();
            let before = describe_paths(conn);
            let path = Node::connection_path(conn).await;
            all[i].push(Reading {
                start: t0,
                end: start.elapsed().as_secs_f64(),
                path,
                before,
                after: describe_paths(conn),
            });
        }
    }
    conns
        .iter()
        .zip(all)
        .map(|((l, _), v)| (l.to_string(), v))
        .collect()
}

/// The longest run of consecutive non-`Direct` readings may span.
///
/// One hole-punch round plus settle. A reading is at most three 250ms windows (750ms); a single
/// `Unknown` while iroh opens and abandons probe paths is inside the documented contract, two in a
/// row is not.
const MAX_UNKNOWN_RUN_SECS: f64 = 1.5;

/// The property #213 is about — not "every sample is Direct", which a single `Unknown` during
/// iroh's periodic hole-punch round legitimately breaks, but:
///
/// - (a) never `Relay`: relays are disabled, so a Relay reading is a false claim;
/// - (b) `Direct` for at least 80% of readings, AND the last reading is `Direct`;
/// - (c) no run of non-`Direct` readings spanning more than [`MAX_UNKNOWN_RUN_SECS`] (from the
///   first such reading's start to the last one's end).
///
/// The #213 defect was an accept side reading `Unknown` for the LIFE of a live connection, which
/// fails all of (b) and (c). Returns every violation, with the path lists around each non-`Direct`
/// reading, so a failure names what the connection looked like.
fn violations(label: &str, rs: &[Reading]) -> Vec<String> {
    use mcpmesh_local_api::PeerPath;
    let mut out = Vec::new();
    let off: Vec<String> = rs
        .iter()
        .filter(|r| r.path != PeerPath::Direct)
        .map(|r| {
            format!(
                "t+{:.2}..{:.2}s {:?} paths {:?} -> {:?}",
                r.start, r.end, r.path, r.before, r.after
            )
        })
        .collect();
    let direct = rs.iter().filter(|r| r.path == PeerPath::Direct).count();
    println!(
        "{label}: {direct}/{} readings Direct; non-Direct: {off:#?}",
        rs.len()
    );
    if rs.is_empty() {
        out.push(format!("{label}: no readings taken"));
        return out;
    }
    if rs.iter().any(|r| matches!(r.path, PeerPath::Relay { .. })) {
        out.push(format!("{label}: a Relay reading with relays disabled"));
    }
    if direct * 5 < rs.len() * 4 {
        out.push(format!(
            "{label}: only {direct}/{} readings Direct (< 80%)",
            rs.len()
        ));
    }
    if rs.last().map(|r| &r.path) != Some(&PeerPath::Direct) {
        out.push(format!("{label}: the last reading is not Direct"));
    }
    let mut longest = 0.0f64;
    let mut run: Option<(f64, f64)> = None;
    for r in rs {
        if r.path == PeerPath::Direct {
            if let Some((s, e)) = run.take() {
                longest = longest.max(e - s);
            }
        } else {
            let first = run.map_or(r.start, |(s, _)| s);
            run = Some((first, r.end));
        }
    }
    if let Some((s, e)) = run {
        longest = longest.max(e - s);
    }
    if longest > MAX_UNKNOWN_RUN_SECS {
        out.push(format!(
            "{label}: a non-Direct run spanned {longest:.2}s (> {MAX_UNKNOWN_RUN_SECS}s)"
        ));
    }
    if !out.is_empty() {
        out.push(format!("{label} non-Direct readings: {off:#?}"));
    }
    out
}

fn assert_the_accept_side_reads_direct(results: &[(String, Vec<Reading>)]) {
    let problems: Vec<String> = results
        .iter()
        .flat_map(|(label, rs)| violations(label, rs))
        .collect();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Two paired nodes with one app-protocol connection each way: `c1`/`s1` is b→a, `c2`/`s2` is a→b,
/// so `s1` and `s2` are both accept sides of a SECOND connection to a peer each node already holds.
struct OppositePair {
    a: Peer,
    b: Peer,
    c1: iroh::endpoint::Connection,
    s1: iroh::endpoint::Connection,
    c2: iroh::endpoint::Connection,
    s2: iroh::endpoint::Connection,
}

impl OppositePair {
    async fn new() -> Self {
        let (a, b) = paired().await;
        let c1 = b.dial().await;
        let s1 = a.accepted().await;
        let c2 = a.dial().await;
        let s2 = b.accepted().await;
        Self {
            a,
            b,
            c1,
            s1,
            c2,
            s2,
        }
    }

    fn accept_sides_unselected(&self) -> bool {
        [&self.s1, &self.s2]
            .iter()
            .all(|c| !c.paths().iter().any(|p| p.is_selected()))
    }

    async fn shutdown(self) {
        drop((self.c1, self.s1, self.c2, self.s2));
        self.a.node.shutdown().await;
        self.b.node.shutdown().await;
    }
}

/// Find a pair in the #213 shape: BOTH accept sides with no selected path.
///
/// Whether a pair lands there depends on which of the host's addresses each connection settles
/// on. Measured on iroh 1.2.0, a fresh pair reached it in 2 of 6 trials — and redialing on the
/// same two nodes reaches it only on the first dial, because iroh then settles the address — so
/// this boots fresh pairs, three at a time, for up to two batches, and keeps the first pair whose
/// accept sides stay unselected across three checks 100ms apart.
///
/// Returns the pair and whether it is in the shape. A host where no pair gets there (a
/// single-address CI runner) still gets a pair, and the caller says the run did not exercise the
/// single-path rule rather than pretending it did.
async fn seek_the_213_shape() -> (OppositePair, bool) {
    const BATCHES: usize = 2;
    const PER_BATCH: usize = 3;
    for batch in 0..BATCHES {
        let spawned: Vec<_> = (0..PER_BATCH)
            .map(|_| tokio::spawn(OppositePair::new()))
            .collect();
        let mut pairs = Vec::new();
        for handle in spawned {
            pairs.push(handle.await.expect("pair setup"));
        }
        let mut hits = vec![true; pairs.len()];
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            for (hit, pair) in hits.iter_mut().zip(&pairs) {
                *hit &= pair.accept_sides_unselected();
            }
        }
        let chosen = hits.iter().position(|h| *h);
        let last = batch + 1 == BATCHES;
        if chosen.is_some() || last {
            let keep = chosen.unwrap_or(0);
            let mut kept = None;
            for (i, pair) in pairs.into_iter().enumerate() {
                if i == keep {
                    kept = Some(pair);
                } else {
                    pair.shutdown().await;
                }
            }
            let in_shape = chosen.is_some();
            println!(
                "FIXTURE: {} the #213 shape (both accept sides unselected) in batch {}",
                if in_shape { "reached" } else { "did NOT reach" },
                batch + 1
            );
            return (kept.expect("a kept pair"), in_shape);
        }
        for pair in pairs {
            pair.shutdown().await;
        }
    }
    unreachable!("the last batch always returns")
}

/// #213, the fix: in the #213 shape (both accept sides unselected), `Node::connection_path` reads
/// `Direct` on both accept sides and both dial sides, with traffic flowing, for longer than iroh's
/// 5s hole-punch interval — because it measures where the datagrams went. See [`violations`] for
/// exactly what is asserted.
#[tokio::test(flavor = "multi_thread")]
async fn connection_path_reads_direct_on_a_busy_accept_side() {
    let (pair, in_shape) = seek_the_213_shape().await;
    let p1 = pump(&pair.c1);
    let p2 = pump(&pair.c2);
    let r = readings(
        &[
            ("a-server1", &pair.s1),
            ("b-client1", &pair.c1),
            ("b-server2", &pair.s2),
            ("a-client2", &pair.c2),
        ],
        7,
    )
    .await;
    p1.abort();
    p2.abort();
    pair.shutdown().await;
    println!("in the #213 shape: {in_shape}");
    assert_the_accept_side_reads_direct(&r);
}

/// #213, the structural half: the same shape with NO traffic in flight. Nothing moves in the
/// measurement window, so the answer comes from the path list alone — and a single open IP path is
/// `Direct` whether or not iroh calls it selected.
///
/// This is the integration pin for the single-path rule, and it only pins it when the fixture
/// reached the #213 shape: with a selected path the rule is never consulted. The unit test
/// `a_single_unselected_path_is_where_the_bytes_go` pins the rule on every host.
#[tokio::test(flavor = "multi_thread")]
async fn connection_path_reads_direct_on_an_idle_accept_side() {
    let (pair, in_shape) = seek_the_213_shape().await;
    let r = readings(&[("a-server1", &pair.s1), ("b-server2", &pair.s2)], 7).await;
    pair.shutdown().await;
    if !in_shape {
        println!(
            "NOTE: this run never reached the #213 shape, so it did not exercise the single-path rule"
        );
    }
    assert_the_accept_side_reads_direct(&r);
}

/// #213 review: a CLOSED connection answers `Unknown`. iroh keeps a closed connection's path list
/// populated (`PathStateSender::close` marks the state closed and leaves the list alone) and its
/// counters readable, so without an explicit check a teardown snapshot reads `Direct` — on both
/// the side that closed and the side that was closed on.
#[tokio::test(flavor = "multi_thread")]
async fn connection_path_reads_unknown_on_a_closed_connection() {
    let (a, b) = paired().await;
    let client = b.dial().await;
    let server = a.accepted().await;
    // Live first, so the assertion below is about closing and not about a connection that never
    // read Direct.
    let p = pump(&client);
    let live = Node::connection_path(&server).await;
    p.abort();
    assert_eq!(
        live,
        mcpmesh_local_api::PeerPath::Direct,
        "precondition: live reads Direct"
    );

    client.close(0u32.into(), b"done");
    // Bounded, sleeping wait for the close to reach the accept side.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while server.close_reason().is_none() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        server.close_reason().is_some(),
        "the close must reach the accept side"
    );
    let paths = server.paths().iter().count();
    println!("closed accept side still lists {paths} path(s)");

    assert_eq!(
        Node::connection_path(&server).await,
        mcpmesh_local_api::PeerPath::Unknown,
        "a closed connection must not read as Direct"
    );
    assert_eq!(
        Node::connection_path(&client).await,
        mcpmesh_local_api::PeerPath::Unknown,
        "the side that closed must not read as Direct either"
    );
    a.node.shutdown().await;
    b.node.shutdown().await;
}
