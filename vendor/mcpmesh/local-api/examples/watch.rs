//! Follow the daemon's live event stream: one line per mesh event, as it happens.
//! Open a session from another terminal (or run `docs/loopback.sh`) to see frames arrive.
//!
//! ```sh
//! cargo run -p mcpmesh-local-api --features client --example watch
//! ```

use mcpmesh_local_api::{ReachabilitySource, StreamFrame, connect_control_default};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mesh = connect_control_default().await?;

    // `subscribe` upgrades the connection: no more requests — just frames to read until we drop
    // the subscription (or the daemon goes away).
    let mut stream = mesh.subscribe().await?;
    while let Some(frame) = stream.next().await? {
        match frame {
            // Always the first frame: a point-in-time picture, so a UI renders immediately.
            StreamFrame::Snapshot {
                active_sessions,
                reachability,
                ..
            } => println!(
                "snapshot: {} active session(s), {} peer(s) probed",
                active_sessions.len(),
                reachability.len()
            ),
            // Then one frame per audit record — sessions opening/closing, proxied requests
            // (names, digests, and counts only — never content), trust changes.
            StreamFrame::Event { record } => println!(
                "{} {:?} peer={} service={}",
                record.ts,
                record.kind,
                record.peer.as_deref().unwrap_or("-"),
                record.service.as_deref().unwrap_or("-"),
            ),
            // A peer went online or offline, or its PATH changed (#58, #92). Pushed on TRANSITION
            // only, so this is the signal to flip a liveness indicator — or to flush whatever you
            // queued for a peer that was unreachable — without polling `status`.
            //
            // `source` (#150) says which producer observed it, and that decides what you may tell
            // a user. `Session` is a claim about the link this peer's traffic is on: a call that
            // was direct and is now relayed is worth surfacing. `Probe` describes a throwaway
            // dial and says nothing about anyone's live connection. `Unknown` is a daemon older
            // than api_minor 30 (or a producer this client predates) — it could be either, so
            // hedge to the weaker claim. Do NOT infer this from `rtt_ms`: a session-sourced frame
            // for an already-probed peer carries that probe's rtt.
            StreamFrame::Reachability { peer, source } => println!(
                "reachability: {} is now {} [{}]",
                peer.name,
                if peer.reachable { "online" } else { "offline" },
                match source {
                    ReachabilitySource::Session => "live link",
                    ReachabilitySource::Probe => "probe dial",
                    _ => "producer unknown",
                }
            ),
            // We read too slowly and the daemon skipped `dropped` records for us; a reconnect
            // would deliver a fresh snapshot to resync from.
            StreamFrame::Lagged { dropped } => println!("(lagged: {dropped} events skipped)"),
            // `StreamFrame` is `#[non_exhaustive]`, so this arm keeps the match compiling against a
            // newer local-api. It does NOT rescue a newer DAEMON's frame: an unrecognised `type`
            // fails to deserialize inside `next()` and the `?` above exits this loop, so nothing
            // ever reaches here for an unknown tag. This comment claimed the opposite until 1.55.
            // A consumer that must tolerate a newer daemon reads raw frames via `open_stream`.
            other => println!("(unrecognized frame: {other:?})"),
        }
    }
    println!("stream closed");
    Ok(())
}
