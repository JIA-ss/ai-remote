//! Read the local daemon's whole picture — services, peers, reachability — over the typed client.
//!
//! Run it with a daemon up (any `mcpmesh` verb starts one):
//!
//! ```sh
//! cargo run -p mcpmesh-local-api --features client --example status
//! ```

use mcpmesh_local_api::connect_control_default;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // One call: resolve the platform endpoint, connect, and verify the Hello handshake.
    let mut mesh = connect_control_default().await?;

    // `status` is the daemon's self-description, as one typed struct.
    let status = mesh.status().await?;
    println!("mcpmesh {}", status.stack_version);

    println!("serving:");
    for svc in &status.services {
        let allow = if svc.allow.is_empty() {
            "nobody yet".into()
        } else {
            svc.allow.join(", ")
        };
        println!("  {} -> {allow}", svc.name);
    }

    println!("peers:");
    for peer in &status.peers {
        println!("  {} shares: {}", peer.name, peer.services.join(", "));
    }

    // Reachability is advisory: `age_secs == None` means "never probed yet" — render it as
    // still checking, not as offline.
    for r in &status.reachability {
        let state = match (r.age_secs, r.reachable, r.rtt_ms) {
            (None, ..) => "checking…".into(),
            (Some(_), true, Some(ms)) => format!("online · {ms} ms"),
            (Some(_), true, None) => "online".into(),
            (Some(_), false, _) => "offline".into(),
        };
        println!("  {} is {state}", r.name);
    }
    Ok(())
}
