//! QS's trees are git worktrees of the repository under test, checked out at
//! the commit being tested and built through mbx.
//!
//! They live under `<git common dir>/quicksilver/`: `main` is the baseline
//! and the first worker, `w01`, `w02`, ... are extra workers for `run`. QS's
//! own state sits in `<main>/.quicksilver/`, a directory that ignores itself,
//! never under `target/`, which mbx owns.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The name of the main worktree under [`root`].
pub const MAIN: &str = "main";
/// QS's state directory inside the main worktree.
pub const STATE_DIR: &str = ".quicksilver";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The checkout's top-level directory.
    pub toplevel: PathBuf,
    /// The repository's common git directory, shared by all its worktrees.
    pub common_dir: PathBuf,
    /// The workspace's path below the top level, with a trailing `/`, or
    /// empty when the workspace is the top level.
    pub prefix: String,
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("git {}", args.join(" ")))?;
    if !out.status.success() {
        bail!(
            "git {} in {} failed: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// The repository that `dir` (a workspace root) belongs to.
pub fn repo_of(dir: &Path) -> Result<Repo> {
    let toplevel = PathBuf::from(
        git(dir, &["rev-parse", "--show-toplevel"])
            .with_context(|| format!("{} is not in a git repository", dir.display()))?,
    );
    let common = PathBuf::from(git(dir, &["rev-parse", "--git-common-dir"])?);
    let common_dir = if common.is_absolute() {
        common
    } else {
        dir.join(common)
    };
    let common_dir = common_dir.canonicalize().unwrap_or(common_dir);
    let prefix = git(dir, &["rev-parse", "--show-prefix"])?;
    Ok(Repo {
        toplevel,
        common_dir,
        prefix,
    })
}

pub fn head(dir: &Path) -> Result<String> {
    git(dir, &["rev-parse", "HEAD"])
}

/// HEAD of the checkout at `dir`, after checking that no tracked file is
/// modified: QS tests commits, so uncommitted edits would silently not be
/// tested.
pub fn require_committed(dir: &Path) -> Result<String> {
    let status = git(dir, &["status", "--porcelain", "--untracked-files=no"])?;
    if !status.is_empty() {
        let files: Vec<&str> = status.lines().take(5).collect();
        bail!(
            "{} has uncommitted changes; QS tests a commit, so commit them first:\n  {}",
            dir.display(),
            files.join("\n  ")
        );
    }
    head(dir)
}

/// Where QS's worktrees of this repository live.
pub fn root(repo: &Repo) -> PathBuf {
    repo.common_dir.join("quicksilver")
}

/// The worktree `name` under [`root`], checked out (detached) at `commit`.
/// An existing one is moved to `commit` with `--force`, which also undoes
/// any schema source a crashed run left behind; ignored files such as the
/// `target` link and the state directory stay.
pub fn ensure(repo: &Repo, name: &str, commit: &str) -> Result<PathBuf> {
    let path = root(repo).join(name);
    let registered = list_ours(repo)?.contains(&path);
    if registered && path.join(".git").exists() {
        git(
            &path,
            &["checkout", "--quiet", "--detach", "--force", commit],
        )?;
        return Ok(path);
    }
    if registered || path.exists() {
        // Registered but deleted, or a stray directory: start over.
        let _ = git(
            &repo.toplevel,
            &["worktree", "remove", "--force", path_str(&path)?],
        );
        let _ = git(&repo.toplevel, &["worktree", "prune"]);
        if path.exists() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("remove stale {}", path.display()))?;
        }
    }
    std::fs::create_dir_all(root(repo))?;
    git(
        &repo.toplevel,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            path_str(&path)?,
            commit,
        ],
    )?;
    Ok(path)
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("{} is not UTF-8", path.display()))
}

/// Tell mbx the worktree's build state is no longer needed, then remove the
/// worktree. Failures are warnings: cleanup must not fail a run.
pub fn remove(repo: &Repo, path: &Path) {
    match Command::new("mbx").arg("clean").arg(path).output() {
        Ok(out) if out.status.success() => {}
        Ok(out) => tracing::warn!(
            "mbx clean {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(err) => tracing::warn!("mbx clean {}: {err}", path.display()),
    }
    let Ok(p) = path_str(path) else { return };
    if let Err(err) = git(&repo.toplevel, &["worktree", "remove", "--force", p]) {
        tracing::warn!("{err:#}");
        let _ = std::fs::remove_dir_all(path);
        let _ = git(&repo.toplevel, &["worktree", "prune"]);
    }
}

/// QS's registered worktrees of this repository.
pub fn list_ours(repo: &Repo) -> Result<Vec<PathBuf>> {
    let ours = root(repo);
    let listing = git(&repo.toplevel, &["worktree", "list", "--porcelain"])?;
    Ok(listing
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(PathBuf::from)
        .filter(|p| p.starts_with(&ours))
        .collect())
}

/// `<tree>/.quicksilver`, created with a `.gitignore` that ignores the
/// directory itself, so the worktree stays clean in `git status` and
/// `git worktree remove` does not refuse it.
pub fn state_dir(tree: &Path) -> Result<PathBuf> {
    let dir = tree.join(STATE_DIR);
    std::fs::create_dir_all(&dir)?;
    let ignore = dir.join(".gitignore");
    if !ignore.is_file() {
        std::fs::write(&ignore, "*\n")?;
    }
    Ok(dir)
}

/// The committed bytes of `path` (relative to the repository top level).
pub fn show(dir: &Path, commit: &str, path: &str) -> Result<Vec<u8>> {
    let out = Command::new("git")
        .args(["cat-file", "blob", &format!("{commit}:{path}")])
        .current_dir(dir)
        .output()
        .context("git cat-file")?;
    if !out.status.success() {
        bail!(
            "git cat-file {commit}:{path}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

/// QS builds through mbx; stop early with the fix when it is missing.
pub fn require_mbx() -> Result<()> {
    match Command::new("mbx").arg("--version").output() {
        Ok(out) if out.status.success() => Ok(()),
        _ => bail!(
            "cargo-quicksilver builds through mbx, which is not on PATH. Install it from https://mr-boxington.jdx.dev/installation"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args([
                "-c",
                "user.email=qs@example.com",
                "-c",
                "user.name=qs",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn repo_with_commit() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q"]);
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        run(dir.path(), &["add", "a.txt"]);
        run(dir.path(), &["commit", "-qm", "one"]);
        dir
    }

    #[test]
    fn a_worktree_follows_the_commit_and_keeps_ignored_state() {
        let dir = repo_with_commit();
        let repo = repo_of(dir.path()).unwrap();
        assert_eq!(repo.prefix, "");
        let first = head(dir.path()).unwrap();
        let tree = ensure(&repo, MAIN, &first).unwrap();
        assert_eq!(tree, root(&repo).join(MAIN));
        assert_eq!(
            std::fs::read_to_string(tree.join("a.txt")).unwrap(),
            "one\n"
        );
        let state = state_dir(&tree).unwrap();
        std::fs::write(state.join("map.json"), "{}").unwrap();
        assert_eq!(git(&tree, &["status", "--porcelain"]).unwrap(), "");

        std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
        run(dir.path(), &["commit", "-qam", "two"]);
        let second = head(dir.path()).unwrap();
        // A crash left schema source behind; ensure restores the commit.
        std::fs::write(tree.join("a.txt"), "schema\n").unwrap();
        let again = ensure(&repo, MAIN, &second).unwrap();
        assert_eq!(again, tree);
        assert_eq!(
            std::fs::read_to_string(tree.join("a.txt")).unwrap(),
            "two\n"
        );
        assert!(state.join("map.json").is_file(), "state survives");
        assert_eq!(show(dir.path(), &first, "a.txt").unwrap(), b"one\n");

        assert_eq!(list_ours(&repo).unwrap(), vec![tree.clone()]);
        remove(&repo, &tree);
        assert!(!tree.exists());
        assert!(list_ours(&repo).unwrap().is_empty());
    }

    #[test]
    fn uncommitted_tracked_changes_are_refused() {
        let dir = repo_with_commit();
        assert!(require_committed(dir.path()).is_ok());
        std::fs::write(dir.path().join("untracked.txt"), "x").unwrap();
        assert!(
            require_committed(dir.path()).is_ok(),
            "untracked files are fine"
        );
        std::fs::write(dir.path().join("a.txt"), "edited\n").unwrap();
        let err = require_committed(dir.path()).unwrap_err().to_string();
        assert!(
            err.contains("commit them first") && err.contains("a.txt"),
            "{err}"
        );
    }

    #[test]
    fn a_stale_registration_is_replaced() {
        let dir = repo_with_commit();
        let repo = repo_of(dir.path()).unwrap();
        let commit = head(dir.path()).unwrap();
        let tree = ensure(&repo, "w01", &commit).unwrap();
        std::fs::remove_dir_all(&tree).unwrap();
        let again = ensure(&repo, "w01", &commit).unwrap();
        assert!(again.join("a.txt").is_file());
    }
}
