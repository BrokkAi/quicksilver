use std::path::{Path, PathBuf};
use std::process::Command;

pub fn fixture_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny")
}

pub fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in walkdir_lite(src) {
        let rel = entry.strip_prefix(src).unwrap();
        if rel.as_os_str().is_empty() {
            continue;
        }
        let dest = dst.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
        } else if entry.is_file() {
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p).unwrap();
            }
            std::fs::copy(&entry, dest).unwrap();
        }
    }
}

fn walkdir_lite(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let mut i = 0;
    while i < out.len() {
        let p = out[i].clone();
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(&p) {
                for e in rd.flatten() {
                    out.push(e.path());
                }
            }
        }
        i += 1;
    }
    out
}

pub fn setup_tiny() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    copy_tree(&fixture_src(), tmp.path());
    assert!(Command::new("git")
        .args(["init"])
        .current_dir(tmp.path())
        .status()
        .unwrap()
        .success());
    for (k, v) in [
        ("user.email", "qs@example.com"),
        ("user.name", "quicksilver"),
    ] {
        let _ = Command::new("git")
            .args(["config", k, v])
            .current_dir(tmp.path())
            .status();
    }
    assert!(Command::new("git")
        .args(["add", "-A"])
        .current_dir(tmp.path())
        .status()
        .unwrap()
        .success());
    assert!(Command::new("git")
        .args(["-c", "commit.gpgsign=false", "commit", "-m", "fixture"])
        .current_dir(tmp.path())
        .status()
        .unwrap()
        .success());
    tmp
}

pub fn scratch_cfg(repo: &Path, scratch: &Path) -> cargo_quicksilver::config::Config {
    use cargo_quicksilver::config::{CloneBackendKind, Config};
    use std::collections::BTreeMap;
    Config {
        repo_root: repo.to_path_buf(),
        scratch_root: scratch.to_path_buf(),
        clone_backend: CloneBackendKind::Copy,
        build_jobs: 2,
        test_jobs: 4,
        index_jobs: 4,
        cm_jobs: 8,
        allow_copy: true,
        max_build_memory_frac: 0.7,
        max_scratch: None,
        packages: Vec::new(),
        files: Vec::new(),
        in_diff: None,
        index_scope: None,
        reindex: false,
        json: None,
        resume: false,
        shard: None,
        sample: 0,
        sample_seed: 1,
        baseline_runs: 1,
        timeout_floor: std::time::Duration::from_millis(800),
        strict_source_isolation: false,
        build_env: BTreeMap::new(),
        zfs_remote: None,
        zfs_dataset: None,
        quiet: true,
        schema_max_rounds: 5,
        schema_batch_size: 0,
        diff_against: None,
        identity_sample: 0,
        triage: false,
        triage_neighbors: 1,
    }
}
