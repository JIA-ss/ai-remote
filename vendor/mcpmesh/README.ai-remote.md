# Vendored mcpmesh dependency

Upstream: https://github.com/counterpunchtech/mcpmesh (0.58.0, upstream revision
`555dc1d`, full revision in `PROVENANCE.json`). This copy is taken from patch revision
`e0bc46c5b2ec165bd16f69cfc49bcf67e8186b28`. The only upstream code change is three dependency
pins: iroh 1.3.0, rmcp 3.5.1 and iroh-blobs 0.103.1 (0.103.0 was yanked).

Only the five library crates and their original tests are included. CLI/release tooling is
excluded and the nested workspace member list is shortened accordingly. Sources are unchanged.
The root workspace patches crates.io to these paths, so anonymous builds require no private
repository access. Upstream MIT OR Apache-2.0 licensing is preserved in this directory.

To update: audit a new upstream revision, retain license notices, record source hashes and pin
changes, regenerate Cargo.lock, then run controller tests and the iOS/desktop CI builds.
