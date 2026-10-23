//! The git branch of a session's folder, read from the repository's `HEAD` file. Git itself is
//! never run: in service mode this host runs as SYSTEM, and a repository's configuration can make
//! git run programs (hooks, `core.fsmonitor`, filters).
//!
//! The repository is the nearest folder from the session's folder up with a `.git` entry: a
//! folder, or (a worktree, a submodule) a file `gitdir: <path>` naming one. Symbolic links are not
//! followed (a `.git` or `HEAD` that is a link counts as none), and only small files are read.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

/// Folders looked at from the session's folder up.
const MAX_DEPTH: usize = 64;
/// Largest `.git` file or `HEAD` read.
const MAX_FILE: u64 = 1024;

/// The branch checked out in the repository holding `dir`; `None` outside a repository, on a
/// detached `HEAD`, or when something is not as expected.
pub fn branch(dir: &Path) -> Option<String> {
    let gitdir = find_gitdir(dir)?;
    let head = read_small(&gitdir.join("HEAD"))?;
    let name = head.trim().strip_prefix("ref:")?.trim();
    let name = name.strip_prefix("refs/heads/").unwrap_or(name);
    (!name.is_empty() && name.len() <= 255 && !name.chars().any(char::is_control)).then(|| name.to_string())
}

/// The git directory of the repository holding `dir`.
fn find_gitdir(dir: &Path) -> Option<PathBuf> {
    for folder in dir.ancestors().take(MAX_DEPTH) {
        let dot = folder.join(".git");
        let Ok(meta) = fs::symlink_metadata(&dot) else { continue };
        if meta.file_type().is_symlink() {
            return None;
        }
        if meta.is_dir() {
            return Some(dot);
        }
        // `gitdir: <path>`, relative to the folder holding the file.
        let text = read_small(&dot)?;
        let target = Path::new(text.lines().next()?.strip_prefix("gitdir:")?.trim());
        let target = if target.is_absolute() { target.to_path_buf() } else { folder.join(target) };
        let meta = fs::symlink_metadata(&target).ok()?;
        return (meta.is_dir() && !meta.file_type().is_symlink()).then_some(target);
    }
    None
}

/// A regular file of at most [`MAX_FILE`] bytes, as text.
fn read_small(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return None;
    }
    let mut text = String::new();
    fs::File::open(path).ok()?.take(MAX_FILE).read_to_string(&mut text).ok()?;
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ar-git-{name}-{}", crate::chat::random_hex()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_repository_names_its_branch_from_a_subfolder() {
        let repo = temp("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::create_dir_all(repo.join("src/deep")).unwrap();
        fs::write(repo.join(".git/HEAD"), "ref: refs/heads/feat/x\n").unwrap();
        assert_eq!(branch(&repo.join("src/deep")).as_deref(), Some("feat/x"));
        // Detached: no branch.
        fs::write(repo.join(".git/HEAD"), "0123456789abcdef0123456789abcdef01234567\n").unwrap();
        assert_eq!(branch(&repo), None);
        let _ = fs::remove_dir_all(repo);
    }

    #[test]
    fn a_worktree_follows_its_gitdir_file() {
        let root = temp("wt");
        let gitdir = root.join("main/.git/worktrees/wt");
        fs::create_dir_all(&gitdir).unwrap();
        fs::write(gitdir.join("HEAD"), "ref: refs/heads/ws/host\n").unwrap();
        fs::create_dir_all(root.join("wt")).unwrap();
        fs::write(root.join("wt/.git"), "gitdir: ../main/.git/worktrees/wt\n").unwrap();
        assert_eq!(branch(&root.join("wt")).as_deref(), Some("ws/host"));
        // A gitdir that is not there: nothing.
        fs::write(root.join("wt/.git"), "gitdir: /nonexistent/x\n").unwrap();
        assert_eq!(branch(&root.join("wt")), None);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn links_are_not_followed_and_big_files_not_read() {
        let root = temp("link");
        let real = root.join("real");
        fs::create_dir_all(real.join(".git")).unwrap();
        fs::write(real.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        // `.git` is a link to another repository's: no branch.
        fs::create_dir_all(root.join("linked")).unwrap();
        std::os::unix::fs::symlink(real.join(".git"), root.join("linked/.git")).unwrap();
        assert_eq!(branch(&root.join("linked")), None);
        // `HEAD` is a link: no branch.
        let other = root.join("other");
        fs::create_dir_all(other.join(".git")).unwrap();
        std::os::unix::fs::symlink(real.join(".git/HEAD"), other.join(".git/HEAD")).unwrap();
        assert_eq!(branch(&other), None);
        // A `HEAD` too big to be one.
        fs::write(real.join(".git/HEAD"), format!("ref: refs/heads/{}\n", "x".repeat(2000))).unwrap();
        assert_eq!(branch(&real), None);
        let _ = fs::remove_dir_all(root);
    }
}
