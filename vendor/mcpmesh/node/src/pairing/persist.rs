//! On-disk persistence for outstanding invites (#87b).
//!
//! `LiveInvites` was RAM-only, so every daemon restart dropped every outstanding invite — while the
//! invite advertises a 24h TTL. An embedder whose product force-applies unattended updates every
//! couple of hours found that an invite emailed to a colleague was reliably dead long before its
//! stated expiry, through no action by either person.
//!
//! **This file holds BEARER SECRETS.** Anyone who can read it can redeem those invites until they
//! expire or are burned. That is a deliberate, recorded decision rather than an oversight: the
//! device key already lives on disk at `0600` and grants strictly more — it *is* the node's
//! identity, permanently — whereas an invite secret is TTL-bounded and grants only the right to
//! pair. Declining to persist the lesser credential while persisting the greater one protects
//! nothing.
//!
//! **The bound widened with #87 and the argument still holds, but state it honestly**: an invite is
//! no longer necessarily single-use. It admits up to `max_uses` (capped at 64) redemptions inside
//! its TTL, so a leaked file is worth up to that many pairings rather than one. It remains strictly
//! less than the device key, which is unbounded and permanent — and the count is written here
//! precisely so it cannot be reset by a restart.
//!
//! Deleting this file is a safe operator action: it invalidates every outstanding invite and
//! nothing else.
//!
//! It is deliberately NOT the redb trust store. That file is not `0600` today, and changing the
//! permissions of the trust store as a side effect of an invite feature is the wrong way to make
//! that decision.
use std::io;
use std::path::{Path, PathBuf};

use super::Invite;

/// The invite file, and the whole-file read/replace discipline around it.
///
/// Small by construction — outstanding invites are few and expire — so rewriting the whole document
/// on every mutation costs nothing and removes every partial-update failure mode.
#[derive(Debug, Clone)]
pub struct InviteFile {
    path: PathBuf,
}

impl InviteFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Every invite on disk that has not yet expired at `now_epoch`.
    ///
    /// Expiry is applied HERE, not only by the reaper, so preserving a stale file buys an attacker
    /// nothing. An absent file is an empty set, not an error — a node that has never minted has no
    /// file.
    ///
    /// A corrupt or truncated file logs and yields empty rather than failing the boot. The tradeoff
    /// is deliberate and worth stating: this drops outstanding invites, which is the very failure
    /// #87 is about, but a daemon that will not start is strictly worse, and the write path is
    /// atomic specifically so this branch stays unreachable.
    pub fn load(&self, now_epoch: u64) -> Vec<Invite> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(e) => {
                tracing::warn!(%e, path = %self.path.display(), "could not read the invite file; outstanding invites are lost");
                return Vec::new();
            }
        };
        match serde_json::from_slice::<Vec<Invite>>(&bytes) {
            Ok(v) => v
                .into_iter()
                .filter(|i| i.expires_at_epoch >= now_epoch)
                .collect(),
            Err(e) => {
                tracing::warn!(%e, path = %self.path.display(), "the invite file did not parse; outstanding invites are lost");
                Vec::new()
            }
        }
    }

    /// Replace the file with exactly `invites`.
    ///
    /// Atomic: a per-call-unique temp in the SAME directory (so the rename cannot cross a
    /// filesystem), `sync_all`, then `rename` over the target. A torn file would fail the whole
    /// load and silently drop every outstanding invite — the bug being fixed — so partial writes
    /// must be unrepresentable rather than merely unlikely.
    ///
    /// `0600` is set at CREATE time, not chmod-ed afterwards: a chmod-after-write leaves a window
    /// in which the secrets are world-readable. Same discipline as `device.key`.
    pub fn store(&self, invites: &[&Invite]) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = self
            .path
            .with_extension(format!("tmp.{}.{}", std::process::id(), seq));

        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }

        let result = (|| -> io::Result<()> {
            let bytes = serde_json::to_vec(invites)?;
            let mut f = opts.open(&tmp)?;
            io::Write::write_all(&mut f, &bytes)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        })();
        if result.is_err() {
            // Never leave a temp holding secrets behind on a failed write.
            let _ = std::fs::remove_file(&tmp);
        }
        result
    }
}

/// Write `bytes` to `path` privately and atomically: 0600 at CREATE (never a widen-after-write
/// race), fsync, rename, and remove the temp on every failure branch (#86).
///
/// Extracted from the invite writer above rather than duplicated — both files hold material that
/// must not be world-readable, and a second hand-rolled copy is how one of them ends up 0644.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
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
    let result = (|| -> io::Result<()> {
        let mut f = opts.open(&tmp)?;
        io::Write::write_all(&mut f, bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite(secret: u8, expires_at_epoch: u64) -> Invite {
        Invite {
            secret: [secret; 32],
            inviter_id: [9u8; 32],
            inviter_addr_json: "{}".into(),
            nickname: "alice".into(),
            services: vec!["notes".into()],
            expires_at_epoch,
            app_label: None,
            uses_remaining: 1,
            // #87: seeded so the persistence round-trip actually carries it — the inviter's local
            // alias must survive a restart with the invite, since it is stripped from the LINE and
            // so cannot be recovered from anywhere else.
            peer_nickname: Some("their-laptop".into()),
            as_self: false,
        }
    }

    /// #86 gate: `write_private` must create at 0600. It exists so a second hand-rolled copy of
    /// this pattern cannot end up 0644 — and nothing pinned the mode, so widening it passed.
    #[test]
    #[cfg(unix)]
    fn write_private_creates_at_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("secret.json");
        write_private(&path, b"{}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "an adopted binding is identity material; 0600 AT CREATE, never a widen-after-write \
             race. Got {mode:o}"
        );
        // Overwriting an existing file must keep it private too — `create_new` on a temp then
        // rename, so the mode comes from the temp rather than the pre-existing file.
        write_private(&path, b"{\"a\":1}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "…including on rewrite. Got {mode:o}");
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"a\":1}");
    }

    /// #87b: an invite written to disk is still there after the process that wrote it is gone —
    /// which is the entire point.
    #[test]
    fn an_invite_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = InviteFile::new(dir.path().join("invites.json"));
        assert!(
            f.load(100).is_empty(),
            "a node that has never minted has no file, and that is not an error"
        );

        let a = invite(1, 1_000);
        f.store(&[&a]).unwrap();
        assert_eq!(f.load(100), vec![a], "survives the registry that wrote it");
    }

    /// #87b: expiry is enforced on LOAD, not only by the reaper — so preserving a stale file buys
    /// nothing, and the file does not accumulate dead invites across restarts.
    #[test]
    fn an_expired_invite_does_not_survive_a_load() {
        let dir = tempfile::tempdir().unwrap();
        let f = InviteFile::new(dir.path().join("invites.json"));
        let live = invite(1, 1_000);
        let dead = invite(2, 500);
        f.store(&[&live, &dead]).unwrap();

        let loaded = f.load(600);
        assert_eq!(loaded, vec![live], "the expired one is dropped: {loaded:?}");
    }

    /// #87b: the file holds bearer secrets. `0600`, set at create time — a chmod after the write
    /// would leave a window where they are world-readable.
    #[cfg(unix)]
    #[test]
    fn the_invite_file_is_not_readable_by_anyone_else() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let f = InviteFile::new(dir.path().join("invites.json"));
        f.store(&[&invite(1, 1_000)]).unwrap();
        let mode = std::fs::metadata(f.path()).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "invite secrets must not be group/world readable: {:o}",
            mode & 0o777
        );
    }

    /// #87b: a corrupt file must not take the daemon down. It costs the outstanding invites — the
    /// failure this issue is about — which is why the write path is atomic; but refusing to boot
    /// would be strictly worse.
    #[test]
    fn a_corrupt_file_degrades_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invites.json");
        std::fs::write(&path, b"{ this is not an invite array").unwrap();
        assert!(InviteFile::new(&path).load(0).is_empty());

        // A truncated write of a previously-valid document behaves the same way.
        std::fs::write(&path, br#"[{"secret":[1,1,1"#).unwrap();
        assert!(InviteFile::new(&path).load(0).is_empty());
    }

    /// The replace really replaces: a burned invite is gone from disk, not merely from RAM.
    #[test]
    fn storing_replaces_rather_than_appends() {
        let dir = tempfile::tempdir().unwrap();
        let f = InviteFile::new(dir.path().join("invites.json"));
        let a = invite(1, 1_000);
        let b = invite(2, 1_000);
        f.store(&[&a, &b]).unwrap();
        assert_eq!(f.load(0).len(), 2);

        f.store(&[&b]).unwrap();
        assert_eq!(
            f.load(0),
            vec![b],
            "a redeemed invite must not be resurrectable by a restart"
        );
    }

    /// A failed write leaves no temp file holding secrets behind.
    #[test]
    fn a_failed_write_leaves_no_secret_bearing_temp() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the target path is itself a directory → rename fails.
        let path = dir.path().join("invites.json");
        std::fs::create_dir(&path).unwrap();
        let f = InviteFile::new(&path);
        assert!(f.store(&[&invite(1, 1_000)]).is_err());

        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed write must not litter a world-readable temp full of bearer secrets: \
             {leftovers:?}"
        );
    }
}
