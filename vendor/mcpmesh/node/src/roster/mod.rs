//! Daemon-owned installed-roster PERSISTENCE: read/parse/validate a roster file, atomically
//! persist the accepted document (a field-set-preserving re-serialization of the parsed
//! `Roster`; the sig re-verifies over the JCS canonical form, NOT the byte layout), and
//! load + re-verify at startup. The gate PLUMBING (RosterGate/ComposedGate) is `gate`;
//! the roster DOMAIN (schema, JCS, validation) is `mcpmesh_trust::roster`. `installed_serial` =
//! the currently-persisted roster's serial (0 if none) — the rollback high-water mark.
//!
//! Sync by design: the daemon drives these fs ops from `spawn_blocking` on a runtime
//! worker (the house rule for every redb/fs op), so this module stays runtime-agnostic.

pub mod distribute;
pub mod enroll;
pub mod freshness;
pub mod gate;
pub mod presence;
pub mod transport;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ed25519_dalek::VerifyingKey;
use mcpmesh_trust::roster::validate::{
    AnchorChange, RosterView, load_installed, validate_for_install_with_anchor,
};
use mcpmesh_trust::roster::{Roster, decode_b64u};

/// Persists the installed `roster.json` and re-loads it. Path-agnostic (the daemon supplies
/// `default_roster_path()`); tests point it at a temp file.
pub struct RosterStore {
    path: PathBuf,
}

impl RosterStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Load + re-verify the installed roster, or `None` when none is installed. Startup path
    /// (load): verifies the signature (rule 1) but not expiry/serial (degraded mode
    /// is computed later at resolve-time).
    pub fn load(&self, root_pk: &VerifyingKey) -> Result<Option<RosterView>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(e).with_context(|| format!("read roster {}", self.path.display()));
            }
        };
        let roster: Roster = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse installed roster {}", self.path.display()))?;
        let view = load_installed(&roster, root_pk).context("re-verify installed roster")?;
        Ok(Some(view))
    }

    /// The installed serial (rollback high-water), or 0 when none is installed. Reads the persisted
    /// doc's own serial WITHOUT signature checks (it was verified at install; this is only for the
    /// `serial >` comparison, itself re-guarded by full validation in `install_from_file`).
    pub fn installed_serial(&self) -> Result<u64> {
        match std::fs::read(&self.path) {
            Ok(b) => Ok(serde_json::from_slice::<Roster>(&b)
                .map(|r| r.serial)
                .unwrap_or(0)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e).with_context(|| format!("read roster {}", self.path.display())),
        }
    }

    /// Install a new roster from a local file (the manual `internal roster install` path). Runs
    /// FULL validation (rules 1–6) against `root_pk` + the installed serial + `now`, then persists
    /// the accepted document via a field-set-preserving re-serialization (atomic temp+fsync+rename).
    /// The sig re-verifies on reload because it is over the JCS canonical form, not the byte layout
    /// (so pretty-printing is safe). Returns the resolvable view for the gate to hot-swap in. A
    /// FAILED validation returns Err BEFORE the write, so the on-disk roster is left UNTOUCHED.
    ///
    /// Also returns an [`AnchorChange`] (#93 ask c): when the roster was accepted on the strength
    /// of a cross-signed SUCCESSOR root, the caller must persist the new anchor to
    /// `[identity].org_root_pk`. A rotation that lived only in memory would revert on restart and
    /// re-strand the node.
    pub fn install_from_file(
        &self,
        file: &Path,
        root_pk: &VerifyingKey,
        now_epoch: i64,
    ) -> Result<(RosterView, AnchorChange)> {
        let bytes =
            std::fs::read(file).with_context(|| format!("read roster file {}", file.display()))?;
        let roster: Roster = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse roster file {}", file.display()))?;
        let installed = self.installed_serial()?;
        let (view, anchor) =
            validate_for_install_with_anchor(&roster, root_pk, installed, now_epoch)
                .context("roster failed validation")?;
        // Persist a field-set-preserving re-serialization of the parsed Roster (the sig is a schema
        // field, so it survives; it re-verifies over the JCS canonical form on load).
        let out = serde_json::to_vec_pretty(&roster).context("serialize roster for persist")?;
        crate::util::atomic_write(&self.path, &out)?;
        Ok((view, anchor))
    }
}

/// Decode the pinned org-root pubkey from its `b64u:` config string to a `VerifyingKey`. Typed
/// errors on a missing prefix / bad base64url / wrong length / invalid key — never a panic.
pub fn parse_org_root_pk(b64u: &str) -> Result<VerifyingKey> {
    let bytes = decode_b64u(b64u).map_err(|e| anyhow::anyhow!("bad org_root_pk: {e}"))?;
    let arr: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("org_root_pk is not 32 bytes"))?;
    VerifyingKey::from_bytes(&arr)
        .map_err(|e| anyhow::anyhow!("org_root_pk is not a valid key: {e}"))
}

/// Atomic string write — the freshness sidecar's persist path ([`freshness::FreshnessStore::store`]).
/// A thin `&str` shim over the crate-shared [`atomic_write`](crate::util::atomic_write) (same
/// temp+fsync+rename torn-state-never discipline), so the one-epoch-integer sidecar shares the ONE
/// atomic-replace implementation.
pub(crate) fn atomic_write_str(path: &Path, s: &str) -> Result<()> {
    crate::util::atomic_write(path, s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use mcpmesh_trust::roster::encode_b64u;
    use mcpmesh_trust::roster::sign::mint_signed;
    use mcpmesh_trust::roster::{Roster, RosterDevice, RosterUser};

    fn root() -> SigningKey {
        SigningKey::from_bytes(&[9u8; 32])
    }

    fn body(serial: u64) -> Roster {
        Roster {
            format: "mcpmesh-roster/1".into(),
            org_id: "acme".into(),
            serial,
            issued_at: "2000-01-01T00:00:00Z".into(),
            expires_at: "2999-01-01T00:00:00Z".into(),
            groups: vec!["team-eng".into()],
            users: vec![RosterUser {
                user_id: "alice".into(),
                display_name: "Alice".into(),
                user_pk: encode_b64u(&[1u8; 32]),
                groups: vec!["team-eng".into()],
                devices: vec![RosterDevice {
                    endpoint_id: encode_b64u(&[2u8; 32]),
                    label: "laptop".into(),
                    role: "primary".into(),
                }],
            }],
            revoked_endpoints: vec![],
            successor_root_pk: None,
            successor_sig: None,
            sig: String::new(),
        }
    }

    #[test]
    fn install_persists_validates_and_rejects_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let store = RosterStore::new(dir.path().join("roster.json"));
        let root_pk = root().verifying_key();
        let now = 1_760_000_000;

        // First install (serial 5): no installed roster → installed_serial 0 → accepted + persisted.
        let f1 = dir.path().join("r5.json");
        std::fs::write(
            &f1,
            serde_json::to_vec(&mint_signed(&root(), body(5))).unwrap(),
        )
        .unwrap();
        let (view, _anchor) = store
            .install_from_file(&f1, &root_pk, now)
            .expect("install serial 5");
        assert_eq!(view.serial(), 5);
        assert!(dir.path().join("roster.json").exists(), "roster persisted");

        // A load re-reads the persisted file and re-verifies → same serial.
        let loaded = store
            .load(&root_pk)
            .unwrap()
            .expect("a roster is installed");
        assert_eq!(loaded.serial(), 5);

        // Rollback (serial 4) is rejected against installed serial 5.
        let f2 = dir.path().join("r4.json");
        std::fs::write(
            &f2,
            serde_json::to_vec(&mint_signed(&root(), body(4))).unwrap(),
        )
        .unwrap();
        assert!(store.install_from_file(&f2, &root_pk, now).is_err());
        // The on-disk installed roster is untouched (still serial 5).
        assert_eq!(store.load(&root_pk).unwrap().unwrap().serial(), 5);

        // A newer serial (6) installs and replaces.
        let f3 = dir.path().join("r6.json");
        std::fs::write(
            &f3,
            serde_json::to_vec(&mint_signed(&root(), body(6))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            store
                .install_from_file(&f3, &root_pk, now)
                .unwrap()
                .0
                .serial(),
            6
        );
    }

    #[test]
    fn load_is_none_when_no_roster_installed() {
        let dir = tempfile::tempdir().unwrap();
        let store = RosterStore::new(dir.path().join("roster.json"));
        assert!(store.load(&root().verifying_key()).unwrap().is_none());
    }
}
