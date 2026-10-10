//! Small crate-wide utilities: the wall-clock epoch pair, the atomic file replace, the
//! blocking-work join wrapper, and the unique-temp-path scheme. One home for helpers that were
//! previously copy-pasted per module (the daemon, the roster store, the gate, doctor, and the
//! porcelain all need "now as epoch seconds"; the daemon's config writers and the roster store
//! share one atomic-replace discipline; every fs/redb call site shares one join discipline).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

/// Run `f` on a blocking worker thread and join it, folding a join failure (a panicked or
/// cancelled task) into an anyhow error under `ctx` — the house fs/redb discipline in one place:
/// blocking work never runs on a runtime worker, and every site reads
/// `blocking("join …", move || …).await?` (with a second `?` when `f` itself returns a `Result`).
pub async fn blocking<T, F>(ctx: &'static str, f: F) -> Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.context(ctx)
}

/// A per-call-unique temp path inside `dir`: `<prefix>.<pid>.<seq>`.
///
/// The name is pid + a process-global counter, so two writers (even concurrent ones within ONE
/// daemon process) can NEVER collide on the same temp file. A pid-only name would let two
/// in-process writers interleave `create(O_TRUNC)` + `write_all` on the same temp → byte-mixed
/// content → a torn file published by whichever rename runs last. This mirrors the device-key
/// mint path ("no fixed temp name").
pub fn unique_temp_path(dir: &Path, prefix: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    dir.join(format!("{prefix}.{}.{}", std::process::id(), seq))
}

/// RAII removal of a temp file: best-effort `remove_file` on drop — success, error, and
/// panic-unwind alike — so a temp file never orphans next to its target. A path already renamed
/// into place (the atomic-replace pattern) or never created makes the drop a harmless no-op.
pub struct TempPathGuard(PathBuf);

impl TempPathGuard {
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempPathGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Wall-clock now as epoch seconds — the invite/`paired_at`/roster time source (no date crate).
/// A clock before the Unix epoch (impossible on a sane host) collapses to 0 rather than
/// panicking. `pub` (not `pub(crate)`) so the `main.rs` bin crate shares the same clock.
pub fn epoch_now_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// [`epoch_now_u64`] as `i64` — the roster/freshness arithmetic works in signed seconds
/// (staleness windows subtract). Same pre-epoch-collapses-to-0 discipline.
pub fn epoch_now_i64() -> i64 {
    epoch_now_u64() as i64
}

/// Atomically replace `path`: write a same-dir temp file, fsync, rename over the target
/// (a torn file is never published). The rename is atomic on the same filesystem.
///
/// The temp name comes from [`unique_temp_path`] (pid + a process-global counter), whose doc
/// carries the two-in-process-writers collision argument; [`TempPathGuard`] cleans up on any
/// failure, so a failed write/sync/rename never orphans a `*.tmp.*` next to the target (after a
/// successful rename the guard's drop is a no-op — the temp name no longer exists). (mcpmesh
/// writes only its OWN files this way — it never edits a third-party AI client's config;
/// `proxy::client_instruction_lines` PRINTS what to paste instead.) `kb-core/src/fsutil.rs`
/// mirrors this discipline on the plugin side (layering forbids sharing the code) — keep the two
/// in step.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let parent = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("create dir {}", parent.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = TempPathGuard::new(unique_temp_path(parent, &format!("{name}.tmp")));
    {
        let mut f = std::fs::File::create(tmp.path())
            .with_context(|| format!("create temp {}", tmp.path().display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(tmp.path(), path)
        .with_context(|| format!("rename {} -> {}", tmp.path().display(), path.display()))?;
    Ok(())
}

/// Total bytes of regular files under `dir`, recursively (#88's `status.storage`). Best-effort
/// by design: a missing dir is 0 (the subsystem has written nothing yet), and an entry that
/// errors mid-walk is skipped rather than failing the read — this feeds an advisory display
/// number, not an invariant. Blocking (fs walk): call from a blocking context or accept the
/// stall like the status path's other inline redb reads.
pub fn dir_size_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total += dir_size_bytes(&entry.path());
        } else {
            total += meta.len();
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("f.toml");
        atomic_write(&path, b"one").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        atomic_write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        // No orphaned temp files next to the target.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temp orphans: {leftovers:?}");
    }

    #[test]
    fn unique_temp_paths_never_collide_and_the_guard_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let a = unique_temp_path(dir.path(), "x.tmp");
        let b = unique_temp_path(dir.path(), "x.tmp");
        assert_ne!(a, b, "the process-global counter makes every call unique");
        std::fs::write(&a, b"partial").unwrap();
        drop(TempPathGuard::new(a.clone()));
        assert!(!a.exists(), "drop removes the temp file");
        // A guard over a never-created (or already-renamed) path drops harmlessly.
        drop(TempPathGuard::new(b));
    }

    #[test]
    fn epoch_pair_agrees() {
        let u = epoch_now_u64();
        let i = epoch_now_i64();
        assert!(
            i >= u as i64 && i - u as i64 <= 1,
            "same clock, same second"
        );
    }
}
