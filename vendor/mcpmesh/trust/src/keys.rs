//! Ed25519 operator-local keys at rest as 32 raw secret bytes, 0600.
//!
//! One file-io discipline (`load_or_generate_signing_key` / `mint_signing_key_at`) backs three
//! semantically DISTINCT key types: [`DeviceKey`] (per-device), [`OrgRootKey`] (the
//! operator's roster-signing anchor), and [`UserKey`] (a person's device-binding key). The
//! newtypes keep the io DRY while making the type system forbid signing a roster with a device key
//! (a real security surface — a type confusion here would be a genuine bug).
use ed25519_dalek::{SigningKey, VerifyingKey};
use std::path::Path;

/// Why loading or minting a key file failed. `#[non_exhaustive]` so a future
/// failure kind is not a breaking change — match with a wildcard arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KeyError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed key file: {0}")]
    Malformed(String),
}

/// Shared Ed25519 key-file discipline: 0600, atomic, EEXIST-race-safe. The
/// single implementation behind [`DeviceKey`], [`OrgRootKey`], and [`UserKey`] — one
/// io path, three semantic types. Returns (key, created); created=true iff this call minted it.
fn load_or_generate_signing_key(path: &Path) -> Result<(SigningKey, bool), KeyError> {
    // Bounded: an EEXIST publish race loops back to load the winner's key; anything
    // that keeps EEXISTing past the budget is surfaced rather than spun on.
    for _ in 0..4 {
        // Reload trusts the stored mode; a loosened-permissions lint belongs to `mcpmesh doctor`.
        match std::fs::read(path) {
            Ok(bytes) => {
                let arr: [u8; 32] = bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| KeyError::Malformed(format!("{} bytes, want 32", bytes.len())))?;
                return Ok((SigningKey::from_bytes(&arr), false));
            }
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            Err(_) => {}
        }
        match mint_signing_key_at(path) {
            Ok(key) => return Ok((key, true)),
            // Another same-uid process published first: loop back and load theirs.
            Err(KeyError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(KeyError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "key mint retry budget exhausted (racing writers?)",
    )))
}

/// Mint via same-directory temp file (0600 at create), fsync, then publish with hard_link — the key
/// file either exists complete or not at all, and an existing key is never overwritten.
/// On Windows the key file inherits the user-profile ACL of %APPDATA% (owner-only by default);
/// there is no mode bit to set.
fn mint_signing_key_at(path: &Path) -> Result<SigningKey, KeyError> {
    // Parent dir is umask-default (typically 0755); a 0700-dir lint also belongs to `mcpmesh doctor`.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Per-call-unique temp name: create_new on it can never EEXIST (no stale-litter
    // collisions, and remove_file below can only ever touch our own temp).
    static MINT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = MINT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), seq));
    let key = SigningKey::generate(&mut rand::rngs::OsRng);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| -> Result<(), KeyError> {
        let mut f = opts.open(&tmp)?;
        use std::io::Write;
        f.write_all(&key.to_bytes())?;
        f.sync_all()?;
        std::fs::hard_link(&tmp, path)?;
        Ok(())
    })();
    // The temp file is removed on every path — success, publish-race loss, or write failure.
    let _ = std::fs::remove_file(&tmp);
    result.map(|_| key)
}

/// Write SPECIFIC key bytes to `path` with the same discipline `load_or_generate` mints under:
/// 0600, a per-call-unique temp, `create_new` so nothing is clobbered by a race (#85 ask 2).
///
/// Used by the recovery import. Separate from the mint path because the caller supplies the key —
/// and separate from a plain `atomic_write` because a key file's MODE is load-bearing: a
/// world-readable user key is an identity anyone on the box can present, and `doctor` reports
/// exactly that.
///
/// `replace` decides what happens when `path` already exists: `false` refuses, `true` overwrites
/// ATOMICALLY.
///
/// The overwrite is a `rename` over the target, not an unlink-then-link. The first version of the
/// caller did the latter and left a window where a failed or interrupted write — ENOSPC, EPERM, a
/// read-only mount — destroyed the key outright. That is not a small window to leave open, because
/// the next boot does not come up keyless: `load_or_generate` mints a FRESH random identity, so the
/// node returns as a third stranger and pairs as one if nobody notices. `rename` has no such
/// window: the old key is in place until the instant the new one replaces it.
pub fn write_signing_key(path: &Path, key: &SigningKey, replace: bool) -> Result<(), KeyError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), seq));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| -> Result<(), KeyError> {
        let mut f = opts.open(&tmp)?;
        use std::io::Write;
        f.write_all(&key.to_bytes())?;
        f.sync_all()?;
        if replace {
            // ATOMIC overwrite: the old key is readable until the instant the new one replaces it,
            // so no failure mode leaves the node with no key at all.
            std::fs::rename(&tmp, path)?;
        } else {
            // `hard_link` FAILS if the target exists, so a concurrent writer cannot be clobbered —
            // the same choice `load_or_generate` makes, for the same reason.
            std::fs::hard_link(&tmp, path)?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

pub struct DeviceKey(SigningKey);

impl DeviceKey {
    /// Returns (key, created): created=true iff this call minted the key.
    pub fn load_or_generate(path: &Path) -> Result<(Self, bool), KeyError> {
        let (key, created) = load_or_generate_signing_key(path)?;
        Ok((Self(key), created))
    }

    /// Wrap a key the EMBEDDER already holds (#85), instead of loading one from disk.
    ///
    /// The point is what it does NOT do: no file is read, minted, or written. An application
    /// keeping the key in the OS keychain (or a passphrase-wrapped blob, or hardware) never has to
    /// materialise 32 raw secret bytes at a path the node owns — which was the entire at-rest
    /// posture, and something an embedder could not fix from outside, because the file lives inside
    /// the mesh root it is told not to hand-write.
    ///
    /// Custody moves with it: nothing here can recover the identity if the caller loses the key.
    pub fn from_signing_key(key: SigningKey) -> Self {
        Self(key)
    }

    pub fn public_bytes(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    // Hardening note: this copy is not zeroized; dalek scrubs SigningKey on drop (default feature), the copy is the residual.
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// Short human fingerprint for status output (never the raw key — surface discipline).
    pub fn fingerprint(&self) -> String {
        let b = self.public_bytes();
        format!("{:02x}{:02x}{:02x}{:02x}", b[0], b[1], b[2], b[3])
    }
}

/// The operator's roster-signing key (the org root key). Only on the operator's node. Same
/// 0600/atomic/race-safe discipline as [`DeviceKey`]; a DISTINCT type so it can never be confused
/// with a device or user key at a signing call site.
///
/// **Highest-value secret in the system.** Compromise = the ability to forge ANY roster —
/// the sole trust anchor every joiner pins — so it is catastrophic by design. Posture:
/// stored 0600, minted ONLY on the operator's node (never on a joiner), read ONLY by the local
/// porcelain to sign in-process, and NEVER crossing the control API or any wire — the daemon is
/// never an online signing oracle; only the PUBLIC half + finished signatures ever leave. Future
/// hardening (offline/HSM storage, threshold signing) is deliberately out of scope for now: a
/// single offline key + an operator runbook is the accepted posture.
pub struct OrgRootKey(SigningKey);

impl OrgRootKey {
    pub fn load_or_generate(path: &Path) -> Result<(Self, bool), KeyError> {
        let (key, created) = load_or_generate_signing_key(path)?;
        Ok((Self(key), created))
    }
    /// The signing key for `roster::sign::sign` (operator signs rosters with this).
    pub fn signing_key(&self) -> &SigningKey {
        &self.0
    }
    pub fn verifying_key(&self) -> VerifyingKey {
        self.0.verifying_key()
    }
    /// The org-root PUBLIC key bytes — pinned by joiners (`org_root_pk` b64u), never the secret.
    pub fn public_bytes(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }
}

/// A person's user key: binds their devices, proves device additions. One per person,
/// on their first device; never moves between machines. Same discipline; a DISTINCT type. Second-tier
/// secret (compromise = bind devices as that person until the operator rotates the user key).
pub struct UserKey(SigningKey);

impl UserKey {
    pub fn load_or_generate(path: &Path) -> Result<(Self, bool), KeyError> {
        let (key, created) = load_or_generate_signing_key(path)?;
        Ok((Self(key), created))
    }
    /// The signing key for `roster::sign::sign_device_binding` (a device→user-key binding).
    pub fn signing_key(&self) -> &SigningKey {
        &self.0
    }
    pub fn verifying_key(&self) -> VerifyingKey {
        self.0.verifying_key()
    }
    /// The user PUBLIC key bytes — carried in the join code (`user_pk` b64u) + the roster.
    pub fn public_bytes(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    /// Wrap a key the caller already holds — the recovery import (#85 ask 2), which decodes it
    /// from a phrase rather than reading a file.
    pub fn from_signing_key(key: SigningKey) -> Self {
        Self(key)
    }
}

#[cfg(test)]
mod write_key_tests {
    use super::*;

    /// #85 ask 2: `write_signing_key` had NO tests. Deleting its `mode(0o600)` left the whole
    /// `mcpmesh-trust` suite green — and without it the file is created `0666 & ~umask`, typically
    /// 0644: a WORLD-READABLE user key, which is an identity anyone on the box can present. The
    /// existing 0600 assertions all go through `mint_signing_key_at`, a separate copy of this code.
    #[test]
    #[cfg(unix)]
    fn a_written_key_is_0600_and_holds_the_supplied_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // A path whose PARENT does not exist, so `create_dir_all` is exercised too.
        let path = dir.path().join("nested").join("user.key");
        let key = SigningKey::from_bytes(&[77u8; 32]);

        write_signing_key(&path, &key, false).expect("writes");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "a key file's mode is load-bearing — 0644 is an identity anyone on the box can present"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            key.to_bytes(),
            "and the bytes written must be the bytes supplied"
        );
    }

    /// Without `replace` an existing key is REFUSED, not clobbered — the caller must decide to
    /// discard an identity explicitly.
    #[test]
    fn writing_over_an_existing_key_needs_replace() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user.key");
        let first = SigningKey::from_bytes(&[1u8; 32]);
        let second = SigningKey::from_bytes(&[2u8; 32]);

        write_signing_key(&path, &first, false).expect("first write");
        assert!(
            write_signing_key(&path, &second, false).is_err(),
            "an existing key must not be clobbered without an explicit replace"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            first.to_bytes(),
            "…and the refusal must leave the original INTACT, not half-replaced"
        );

        write_signing_key(&path, &second, true).expect("replace writes");
        assert_eq!(std::fs::read(&path).unwrap(), second.to_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600,
                "the REPLACE path must set the mode too — it renames a temp over the target, and a \
                 temp created without the mode would silently widen it"
            );
        }
    }

    /// No temp file is left behind on either path.
    #[test]
    fn no_temp_survives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("user.key");
        let key = SigningKey::from_bytes(&[3u8; 32]);
        write_signing_key(&path, &key, false).unwrap();
        write_signing_key(&path, &key, true).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files leaked: {leftovers:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_call_mints_and_second_call_reloads_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.key");
        let (k1, created1) = DeviceKey::load_or_generate(&path).unwrap();
        let (k2, created2) = DeviceKey::load_or_generate(&path).unwrap();
        assert!(created1);
        assert!(!created2);
        assert_eq!(k1.public_bytes(), k2.public_bytes());
    }

    // Unix-only: asserts the 0600 mode bits, which windows key files carry via
    // user-profile ACLs instead of a POSIX mode.
    #[cfg(unix)]
    #[test]
    fn key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.key");
        DeviceKey::load_or_generate(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn corrupt_key_file_is_an_error_not_a_regenerate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.key");
        std::fs::write(&path, b"short").unwrap();
        assert!(matches!(
            DeviceKey::load_or_generate(&path),
            Err(KeyError::Malformed(_))
        ));
    }

    #[test]
    fn org_root_and_user_keys_mint_0600_reload_and_expose_signing_keys() {
        use ed25519_dalek::Signer;
        let dir = tempfile::tempdir().unwrap();

        // OrgRootKey: mint → 0600 → reload same public half.
        let op = dir.path().join("org-root.key");
        let (root1, created1) = OrgRootKey::load_or_generate(&op).unwrap();
        assert!(created1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&op).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let (root2, created2) = OrgRootKey::load_or_generate(&op).unwrap();
        assert!(!created2);
        assert_eq!(root1.public_bytes(), root2.public_bytes());
        // signing_key() is usable (roster signing reuses ed25519_dalek::Signer).
        let sig = root1.signing_key().sign(b"hello");
        assert!(root1.verifying_key().verify_strict(b"hello", &sig).is_ok());

        // UserKey: same discipline, a DISTINCT type + key.
        let up = dir.path().join("user.key");
        let (user, _) = UserKey::load_or_generate(&up).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&up).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_ne!(user.public_bytes(), root1.public_bytes());
    }
}
