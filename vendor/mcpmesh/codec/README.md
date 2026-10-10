# mcpmesh-codec

The mcpmesh family's one wire codec: newline-delimited compact JSON (UTF-8, one
frame per `\n`, 16 MiB cap) over async byte streams, plus typed framing
violations so callers can log or strike misbehaving peers.

This is a lockstep-versioned kernel crate of the [mcpmesh](https://github.com/counterpunchtech/mcpmesh)
workspace: both ends of every mcpmesh wire link this same implementation (the
daemon side via `mcpmesh-net`, the no-network client side via
`mcpmesh-local-api`), so the two ends can never drift apart. It is deliberately
tiny — serde_json and tokio's io traits only.

Most integrators want [`mcpmesh-local-api`](https://crates.io/crates/mcpmesh-local-api),
the typed client for talking to a locally-running mesh; depend on this crate
directly only if you are implementing a wire end yourself.
