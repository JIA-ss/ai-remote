# mcpmesh-trust

The mcpmesh trust kernel: Ed25519 key files with an atomic, owner-only storage
discipline (`DeviceKey`, `OrgRootKey`, `UserKey`), the signed `mcpmesh-roster/1`
document (schema, JCS signing, the six validation rules, operator mutations),
and the self-sovereign device→user binding used in pairing mode.

This is a lockstep-versioned kernel crate of the [mcpmesh](https://github.com/counterpunchtech/mcpmesh)
workspace, shared by the daemon and the operator porcelain. It is the trust
*domain* only — no network, no database; gates over live state and roster
persistence live with the daemon.

Its API takes and returns `ed25519_dalek` types and re-exports the crate: use
`mcpmesh_trust::ed25519_dalek::…`, never your own `ed25519-dalek` dependency, so
versions can never mismatch.

Most integrators want [`mcpmesh-local-api`](https://crates.io/crates/mcpmesh-local-api),
the typed client for talking to a locally-running mesh.
