# mcpmesh-net

The mcpmesh session kernel: endpoint identity (`EndpointId`, `PeerIdentity`),
framed MCP transports over iroh/QUIC (`NdjsonTransport`), trust-gated serving
(`serve` / `run_mesh_connection` behind a `TrustGate`), dialing (`connect`), and
live-connection severing (`ConnRegistry`).

This is a lockstep-versioned kernel crate of the [mcpmesh](https://github.com/counterpunchtech/mcpmesh)
workspace, shared by the daemon and anything that serves or dials mesh services
with full identity control. Trust *policy* (rosters, pairing, key storage) lives
in `mcpmesh-trust` and the daemon; this crate only defines the gate trait.

It exact-pins iroh and re-exports it: use `mcpmesh_net::iroh::…`, never your own
iroh dependency — any other version is a different crate to the type system.

Most integrators want [`mcpmesh-local-api`](https://crates.io/crates/mcpmesh-local-api),
the typed client for talking to a locally-running mesh — it needs no iroh at all.
