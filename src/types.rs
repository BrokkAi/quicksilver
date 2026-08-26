use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Stable mutant identity: hash of (package, path, fqn, replacement).
/// Deliberately not line-numbered so ids survive unrelated edits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Mutant {
    pub id: String,
    pub package: String,
    /// Repository-relative path using forward slashes.
    pub path: String,
    pub fqn: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
    /// Absolute byte range of the function body, including braces.
    pub body_start_byte: usize,
    pub body_end_byte: usize,
    /// Replacement body, including braces. A span, not a diff.
    pub replacement: String,
    pub return_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestInfo {
    pub id: String,
    pub name: String,
    pub package: String,
    pub binary_id: String,
    pub binary_name: String,
    /// Path of the test binary relative to `CARGO_TARGET_DIR`.
    pub binary_rel: String,
    /// Package directory relative to the workspace root (test cwd).
    pub cwd_rel: String,
    pub duration_secs: f64,
}

impl TestInfo {
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.duration_secs.max(0.0))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedMutant {
    pub mutant: Mutant,
    /// Ordered cheapest-first. `exec` short-circuits on first failure.
    pub tests: Vec<TestInfo>,
    pub estimated_cost_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalityBatch {
    pub package: String,
    pub file: String,
    pub mutants: Vec<PlannedMutant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictKind {
    Caught,
    Missed,
    NotCovered,
    Unviable,
    /// Test binary ran but did not fail as libtest (exit 101). Dynamic-link
    /// failures, abort, etc. Not a compile failure and not Caught.
    Errored,
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub mutant: Mutant,
    pub kind: VerdictKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub build: bool,
}

impl Outcome {
    pub fn not_covered(mutant: Mutant) -> Self {
        Self {
            mutant,
            kind: VerdictKind::NotCovered,
            test: None,
            message: None,
            build: false,
        }
    }

    pub fn caught(mutant: Mutant, test: String) -> Self {
        Self {
            mutant,
            kind: VerdictKind::Caught,
            test: Some(test),
            message: None,
            build: true,
        }
    }

    pub fn missed(mutant: Mutant) -> Self {
        Self {
            mutant,
            kind: VerdictKind::Missed,
            test: None,
            message: None,
            build: true,
        }
    }

    pub fn unviable(mutant: Mutant, message: String) -> Self {
        Self {
            mutant,
            kind: VerdictKind::Unviable,
            test: None,
            message: Some(message),
            build: true,
        }
    }

    pub fn timeout(mutant: Mutant, test: String) -> Self {
        Self {
            mutant,
            kind: VerdictKind::Timeout,
            test: Some(test),
            message: None,
            build: true,
        }
    }

    pub fn errored(mutant: Mutant, test: String, message: String) -> Self {
        Self {
            mutant,
            kind: VerdictKind::Errored,
            test: Some(test),
            message: Some(message),
            build: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub repo_root: PathBuf,
    pub session: PathBuf,
    pub baseline: PathBuf,
    pub baseline_cov: PathBuf,
    pub index: PathBuf,
    pub workers: PathBuf,
    pub runs: PathBuf,
    pub state: PathBuf,
}

impl Paths {
    pub fn for_session(scratch_root: &std::path::Path, repo_root: &std::path::Path) -> Self {
        let hash = crate::util::repo_hash(repo_root);
        let session = scratch_root.join(hash);
        Self {
            repo_root: repo_root.to_path_buf(),
            baseline: session.join("baseline"),
            baseline_cov: session.join("baseline-cov"),
            index: session.join("index"),
            workers: session.join("workers"),
            runs: session.join("runs"),
            state: session.join("state"),
            session,
        }
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for p in [
            &self.session,
            &self.index,
            &self.workers,
            &self.runs,
            &self.state,
        ] {
            std::fs::create_dir_all(p)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageCost {
    pub package: String,
    pub rebuild_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineReport {
    pub suite_secs: f64,
    pub tests: Vec<TestInfo>,
    pub package_costs: Vec<PackageCost>,
    pub preflight_recompiled: Option<u64>,
    /// Test ids that failed in the uninstrumented suite (non-seed SLO flakes, etc.).
    #[serde(default)]
    pub failed_test_ids: Vec<String>,
}
