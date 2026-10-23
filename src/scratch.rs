//! Worker trees and disk checks.
//!
//! Every tree QS builds in is a git worktree of the repository under test
//! (see [`crate::worktree`]). Worker 0 is the main worktree, which is also the
//! baseline; extra workers for parallel crate builds are temporary worktrees
//! checked out at the same commit. mbx gives each its own managed `target/`
//! and fills it from its cache, so no worker copies another's build output.

use crate::types::Paths;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{info, warn};

fn worker_name(slot: usize) -> String {
    format!("w{slot:02}")
}

/// The workspace root inside worker `slot`'s tree, checked out at the main
/// worktree's commit. Slot 0 is the main worktree itself.
pub fn ensure_worker(paths: &Paths, slot: usize) -> Result<PathBuf> {
    if slot == 0 {
        return Ok(paths.baseline.clone());
    }
    let main = paths.main_tree();
    let repo = crate::worktree::repo_of(&main)?;
    let commit = crate::worktree::head(&main)?;
    let start = Instant::now();
    let tree = crate::worktree::ensure(&repo, &worker_name(slot), &commit)
        .with_context(|| format!("worker {slot}"))?;
    let root = crate::worktree::under(&tree, &repo.prefix);
    info!(
        "worker {slot}: worktree {} at {} in {:.1}s",
        root.display(),
        &commit[..12.min(commit.len())],
        start.elapsed().as_secs_f64()
    );
    Ok(root)
}

/// Remove every extra worker worktree (slot 1 and up), telling mbx first.
pub fn remove_extra_workers(paths: &Paths) {
    let main = paths.main_tree();
    let Ok(repo) = crate::worktree::repo_of(&main) else {
        return;
    };
    let Ok(ours) = crate::worktree::list_ours(&repo) else {
        return;
    };
    for tree in ours.into_iter().filter(|t| *t != main) {
        crate::worktree::remove(&repo, &tree);
    }
}

pub fn check_free_space(path: &Path, min: Option<u64>) -> Result<()> {
    let avail = crate::util::statvfs_avail(path)?;
    if let Some(min) = min {
        if avail < min {
            bail!(
                "{} has {avail} bytes free; --max-scratch wants at least {min}",
                path.display()
            );
        }
    }
    if avail < 5 * (1 << 30) {
        warn!("{} has only {} bytes free", path.display(), avail);
    }
    Ok(())
}
