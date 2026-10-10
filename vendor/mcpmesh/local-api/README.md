# mcpmesh-local-api

Typed Rust bindings for `mcpmesh-local/1` — the protocol that [mcpmesh](https://github.com/counterpunchtech/mcpmesh)'s
daemon speaks with programs on the **same machine** over a same-user local endpoint (a Unix domain
socket on macOS/Linux, a named pipe on Windows). This is the crate a GUI, TUI, or any other program
that drives or embeds the mesh builds on.

## Do you need this crate?

| You want to… | You need |
|---|---|
| **Share a tool** with a peer | nothing mcpmesh-specific — write an ordinary stdio [MCP](https://modelcontextprotocol.io) server and `mcpmesh serve <name> -- <your command>` |
| **Drive or embed the mesh** (a GUI, a TUI, another launcher) | this crate with the `client` feature — or speak the [wire protocol](https://github.com/counterpunchtech/mcpmesh/blob/main/docs/local-protocol.md) directly from any language |
| **Reimplement the peer transport** (iroh/QUIC, pairing crypto) | out of scope — that layer is exposed only through the CLI/daemon |

## Quickstart

```toml
[dependencies]
mcpmesh-local-api = { version = "0.4", features = ["client"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

```rust
use mcpmesh_local_api::connect_control_default;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Resolves the platform endpoint, connects, and verifies the Hello handshake.
    let mut mesh = connect_control_default().await?;
    let status = mesh.status().await?;
    println!("mcpmesh {}", status.stack_version);
    for peer in &status.peers {
        println!("  {} shares {:?} with you", peer.name, peer.services);
    }
    Ok(())
}
```

This expects a running daemon — any porcelain verb (e.g. `mcpmesh status`) starts one. Every
control method has a typed helper on `ControlClient` (`invite`, `pair`, `register_service`, …);
`request()` remains as a raw-`serde_json::Value` escape hatch.

Runnable versions live in [`examples/`](examples): `status` (the picture above, rendered) and
`watch` (the typed live event stream) — run them from the repo with
`cargo run -p mcpmesh-local-api --features client --example status` — plus `status.py`, the same
`status` call in dependency-free Python, because the wire protocol needs no SDK.

## Features

| Feature | Adds |
|---|---|
| *(default)* | the serde vocabulary only — `Request`, the result types, the stream/audit frames, plus `paths` endpoint resolution; std-only, no async runtime |
| `client` | `ControlClient`, `connect_control_default()`, and the shared NDJSON codec (tokio) |
| `service` | the plugin-daemon seam: local-endpoint bind with the same-user gate, audience authorization, and control-socket self-registration |

## Protocol

The full wire spec — framing, every method, the identity contract, the security model — is
[`docs/local-protocol.md`](https://github.com/counterpunchtech/mcpmesh/blob/main/docs/local-protocol.md).
It is a small newline-delimited JSON protocol: anything that can open the endpoint can speak it in
any language. This crate is the Rust binding, not the only door.

## Stability

Pre-release. The wire protocol is versioned (`mcpmesh-local/1`) and evolves additively, but until a
stable release this crate's Rust API may break between minor versions — pin the version you build
against.

## License

MIT OR Apache-2.0, at your option.
