# Vendored mcpmesh dependency

Upstream: https://github.com/counterpunchtech/mcpmesh (0.58.0, upstream revision
`555dc1d`, full revision in `PROVENANCE.json`). This copy is taken from patch revision
`e0bc46c5b2ec165bd16f69cfc49bcf67e8186b28`. The only upstream code change is three dependency
pins: iroh 1.3.0, rmcp 3.5.1 and iroh-blobs 0.103.1 (0.103.0 was yanked).

ai-remote patch (listed under `ai_remote_patches` in `PROVENANCE.json`, hashes updated):
`[network].proxy_url` in `node/src/config.rs`, applied in `node/src/daemon/boot.rs` (`apply_proxy`) so
relay connections can go through an HTTP(S) proxy on networks that only allow traffic out that way.
Test: `cargo test -p mcpmesh-node --lib proxy_url` from this directory.
Second patch: `node/Cargo.toml` enables iroh's `unstable-net-report` and `node/src/node.rs` adds
`Node::net_report()` (UDP v4/v6, symmetric NAT, captive portal) for `ai-remote doctor` and the relay
notice. The feature is iroh's unstable API: recheck it on every iroh bump.
Third patch: `[network].bind_port` (`node/src/config.rs`, `build_endpoint` in `node/src/daemon/boot.rs`)
binds the IPv4 socket on a fixed port for router port forwarding, falling back to a random port when it
is taken. Test: `cargo test -p mcpmesh-node --lib bind_port`.
Fourth patch: `[network].port_mapping` and `[network].dns_fallback` (`node/src/config.rs`, `apply_port_mapping`
and `apply_dns_fallback` in `node/src/daemon/boot.rs`) let ai-remote's quiet mode turn off router port mapping
(UPnP, NAT-PMP, PCP) and keep DNS lookups on the system's resolvers. Both default to iroh's behaviour. Test:
`cargo test -p mcpmesh-node --lib -- port_mapping dns_fallback`.

Only the five library crates and their original tests are included. CLI/release tooling is
excluded and the nested workspace member list is shortened accordingly. Sources are unchanged.
The root workspace patches crates.io to these paths, so anonymous builds require no private
repository access. Upstream MIT OR Apache-2.0 licensing is preserved in this directory.

To update: audit a new upstream revision, retain license notices, record source hashes and pin
changes, regenerate Cargo.lock, then run controller tests and the iOS/desktop CI builds.
