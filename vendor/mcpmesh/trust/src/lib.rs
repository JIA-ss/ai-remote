//! mcpmesh-trust: the trust kernel of the mcpmesh workspace — Ed25519 key
//! generation/storage ([`DeviceKey`]/[`OrgRootKey`]/[`UserKey`]), the signed
//! `mcpmesh-roster/1` document (schema, signing, validation, mutation), and the
//! self-sovereign device→user binding for pairing mode.
//!
//! This crate is the trust DOMAIN, shared by the mcpmesh daemon and the operator
//! porcelain. It deliberately excludes the daemon PLUMBING that acts on trust —
//! gates over live state, persistence, hot-swap, and severing all live with the
//! daemon — and anything network-facing: no iroh, no sockets, no database. Most
//! integrators talking to a locally-running mesh want `mcpmesh-local-api` instead.
//!
//! # The ed25519-dalek re-export
//!
//! This crate's API takes and returns `ed25519_dalek` types (`SigningKey`,
//! `VerifyingKey`). Use the re-export — `mcpmesh_trust::ed25519_dalek::…` — and
//! never add your own `ed25519-dalek` dependency: a version mismatch is a
//! different crate to the type system and breaks the build.
pub use ed25519_dalek;

pub mod app;
pub mod binding;
pub mod keys;
pub mod roster;
pub use app::{APP_SIG_DOMAIN, sign_app, verify_app};
pub use keys::{DeviceKey, KeyError, OrgRootKey, UserKey};
/// The family paths rule lives on `mcpmesh-local-api` (the featureless vocabulary crate —
/// plugins are barred from depending on trust, and the endpoint formula must have ONE
/// home); re-exported here so `mcpmesh_trust::paths::…` stays a valid spelling.
pub use mcpmesh_local_api::paths;
pub use roster::mutate::{empty_roster, remove_user, revoke_device, upsert_member};
pub use roster::validate::{ResolvedDevice, RosterState, RosterView};
pub use roster::{Roster, RosterDevice, RosterError, RosterUser};
