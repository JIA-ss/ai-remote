//! The files of a session's folder, for the composer's `@` menu (the `files` command of
//! [`crate::chat_channel`]): names only, relative to the folder, read-only.
//!
//! In a git work tree the list is what git knows there (`git ls-files`: tracked files and untracked
//! ones that are not ignored); elsewhere the folder is walked to a few levels, skipping hidden
//! folders and the usual build output. Either way the list is capped, kept for a few seconds per
//! session (each keystroke asks again), and filtered here, so only matches go over the wire.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::Result;

/// Files listed at most per folder.
const MAX_LISTED: usize = 100_000;
/// How deep the walk goes outside a git work tree.
const MAX_DEPTH: usize = 6;
/// How long a listing is reused.
const KEEP: Duration = Duration::from_secs(10);
/// Most matches sent back.
pub const MAX_MATCHES: usize = 200;
/// How long `git ls-files` may take.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Folders the walk does not enter (besides hidden ones).
const SKIP: &[&str] = &["node_modules", "target", "dist", "build", "__pycache__", "venv", "Library", "Temp", "obj"];

/// One session's last listing, and whether `git` may be asked for it.
pub struct FileCache(Mutex<Option<Listing>>, bool);

struct Listing {
    at: Instant,
    dir: PathBuf,
    files: Vec<String>,
    source: &'static str,
}

/// What a `files` command answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Matches {
    pub files: Vec<String>,
    /// More files matched than were sent, or the folder had more than could be listed.
    pub truncated: bool,
    /// `git` or `walk`.
    pub source: &'static str,
}

impl FileCache {
    /// `git`: whether this host may run `git` in the folder. A host running as SYSTEM may not: the
    /// folder's repository is the user's, and its config can make git run programs (an fsmonitor
    /// hook, for one), which would then run as SYSTEM; it walks the folder instead.
    pub fn new(git: bool) -> Self {
        Self(Mutex::new(None), git)
    }

    /// The files under `cwd` whose relative path contains every word of `query` (case-insensitive),
    /// shortest paths first, at most `limit`.
    pub async fn matches(&self, cwd: &Path, query: &str, limit: usize) -> Result<Matches> {
        let (files, source, capped) = self.listing(cwd).await?;
        let words: Vec<String> = query.to_lowercase().split_whitespace().map(str::to_string).collect();
        let mut hits: Vec<&String> = files
            .iter()
            .filter(|f| {
                let f = f.to_lowercase();
                words.iter().all(|w| f.contains(w.as_str()))
            })
            .collect();
        let limit = limit.clamp(1, MAX_MATCHES);
        // The file's own name matching ranks first, then shorter paths.
        let base = |f: &str| f.rsplit('/').next().unwrap_or(f).to_lowercase();
        let last = words.last().cloned().unwrap_or_default();
        hits.sort_by_key(|f| (!base(f).contains(last.as_str()), f.len()));
        let truncated = capped || hits.len() > limit;
        Ok(Matches { files: hits.into_iter().take(limit).cloned().collect(), truncated, source })
    }

    async fn listing(&self, cwd: &Path) -> Result<(Vec<String>, &'static str, bool)> {
        if let Some(l) = &*self.0.lock().unwrap()
            && l.at.elapsed() < KEEP
            && l.dir == cwd
        {
            return Ok((l.files.clone(), l.source, l.files.len() >= MAX_LISTED));
        }
        let git = if self.1 { git_files(cwd).await } else { None };
        let (files, source) = match git {
            Some(f) => (f, "git"),
            None => {
                let dir = cwd.to_path_buf();
                (tokio::task::spawn_blocking(move || walk(&dir)).await?, "walk")
            }
        };
        let capped = files.len() >= MAX_LISTED;
        *self.0.lock().unwrap() =
            Some(Listing { at: Instant::now(), dir: cwd.to_path_buf(), files: files.clone(), source });
        Ok((files, source, capped))
    }
}

/// `git ls-files` in `cwd`, or `None` when it is not a work tree or git is missing or slow.
async fn git_files(cwd: &Path) -> Option<Vec<String>> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["-c", "core.quotepath=off", "ls-files", "--cached", "--others", "--exclude-standard", "-z"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    crate::platform::no_console_window(cmd.as_std_mut());
    let out = tokio::time::timeout(GIT_TIMEOUT, cmd.output()).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    let mut files: Vec<String> = out
        .stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .take(MAX_LISTED)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    files.dedup();
    Some(files)
}

/// The files under `root` to [`MAX_DEPTH`] levels, as `/`-separated relative paths.
fn walk(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            // Symbolic links are not followed (they may point anywhere).
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_dir() {
                if depth + 1 < MAX_DEPTH && !name.starts_with('.') && !SKIP.contains(&name.as_str()) {
                    stack.push((e.path(), depth + 1));
                }
            } else if kind.is_file() {
                let rel = e.path().strip_prefix(root).map(|p| p.to_path_buf()).unwrap_or_default();
                files.push(rel.to_string_lossy().replace('\\', "/"));
                if files.len() >= MAX_LISTED {
                    return files;
                }
            }
        }
    }
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_plain_folder_is_walked_and_filtered() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for f in
            ["src/main.rs", "src/lib/domain.rs", "README.md", ".hidden/x.rs", "node_modules/m/index.js", "a b/c d.txt"]
        {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "").unwrap();
        }
        let cache = FileCache::new(true);
        let m = cache.matches(root, "", 50).await.unwrap();
        assert_eq!(m.source, "walk");
        assert_eq!(m.files.len(), 4, "{m:?}");
        assert!(!m.files.iter().any(|f| f.contains("hidden") || f.contains("node_modules")));
        let m = cache.matches(root, "main", 50).await.unwrap();
        assert_eq!(m.files, ["src/main.rs", "src/lib/domain.rs"]);
        let m = cache.matches(root, "src rs", 1).await.unwrap();
        assert_eq!((m.files.len(), m.truncated), (1, true));
        assert_eq!(cache.matches(root, "b/c", 5).await.unwrap().files, ["a b/c d.txt"]);
    }

    #[tokio::test]
    async fn a_git_work_tree_lists_what_git_knows() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| std::process::Command::new("git").args(args).current_dir(root).output();
        if git(&["init", "-q"]).is_err() {
            return; // no git here
        }
        std::fs::write(root.join(".gitignore"), "out/\n").unwrap();
        std::fs::create_dir_all(root.join("out")).unwrap();
        std::fs::write(root.join("out/big.bin"), "").unwrap();
        std::fs::write(root.join("kept.rs"), "").unwrap();
        let m = FileCache::new(true).matches(root, "", 50).await.unwrap();
        assert_eq!(m.source, "git");
        assert_eq!(m.files, ["kept.rs", ".gitignore"]);
        // A host that may not run git (one running as SYSTEM) walks the same work tree.
        let m = FileCache::new(false).matches(root, "", 50).await.unwrap();
        assert_eq!(m.source, "walk");
        assert!(m.files.iter().any(|f| f == "kept.rs"), "{m:?}");
    }
}
