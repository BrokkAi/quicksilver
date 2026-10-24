use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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

/// Canonical result of one selected test under one mutant activation. A
/// libtest assertion failure is `Killed`; other process failures stay
/// distinct so they cannot inflate mutation evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestVerdict {
    Passed,
    Killed,
    Errored,
    Timeout,
    Infrastructure,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestObservation {
    pub test_id: String,
    pub test_name: String,
    pub verdict: TestVerdict,
    pub wall_secs: f64,
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

    /// Caught by a test that crashed (a signal, or an exit other than 0 and
    /// libtest's 101) with the mutant active, crashed again on retry, and
    /// passed with no mutant active. The message starts with
    /// [`CRASH_PREFIX`] so reports can count crash kills separately.
    pub fn caught_by_crash(mutant: Mutant, test: String, message: String) -> Self {
        Self {
            mutant,
            kind: VerdictKind::Caught,
            test: Some(test),
            message: Some(message),
            build: true,
        }
    }

    /// Caught by a test that timed out with the mutant active, twice, while
    /// it passed without it well inside its calibration budget: the mutant
    /// made the code hang or slow down and the test's own deadline (or
    /// QS's) noticed. The message starts with [`TIMEOUT_PREFIX`].
    pub fn caught_by_timeout(mutant: Mutant, test: String, message: String) -> Self {
        Self {
            mutant,
            kind: VerdictKind::Caught,
            test: Some(test),
            message: Some(message),
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

    /// Pre-filtered or otherwise never compiled.
    pub fn unviable_skipped(mutant: Mutant, message: String) -> Self {
        Self {
            mutant,
            kind: VerdictKind::Unviable,
            test: None,
            message: Some(message),
            build: false,
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

/// Prefix of a [`Outcome::caught_by_crash`] message.
pub const CRASH_PREFIX: &str = "crashed under the mutant";

/// Prefix of a [`Outcome::caught_by_timeout`] message.
pub const TIMEOUT_PREFIX: &str = "timed out under the mutant";

/// "exit 3" or "killed by a signal" for a non-libtest test failure.
pub fn describe_exit(code: Option<i32>) -> String {
    match code {
        Some(c) => format!("exit {c}"),
        None => "killed by a signal".into(),
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
    /// The layout of QS's trees for a workspace at `prefix` below the top
    /// level of a repository whose QS worktrees live in `worktree_root`:
    /// `main/<prefix>` is the baseline and first worker, `main/.quicksilver`
    /// holds QS's state, and extra workers are siblings of `main`.
    pub fn for_tree(worktree_root: &Path, prefix: &str, repo_root: &Path) -> Self {
        let main = worktree_root.join(crate::worktree::MAIN);
        let session = main.join(crate::worktree::STATE_DIR);
        Self {
            repo_root: repo_root.to_path_buf(),
            baseline: crate::worktree::under(&main, prefix),
            baseline_cov: session.join("cov-target"),
            index: session.join("index"),
            workers: worktree_root.to_path_buf(),
            runs: session.join(std::env::var("QS_RUNS_SUBDIR").unwrap_or_else(|_| "runs".into())),
            state: session
                .join(std::env::var("QS_STATE_SUBDIR").unwrap_or_else(|_| "state".into())),
            session,
        }
    }

    /// The main worktree's top level.
    pub fn main_tree(&self) -> PathBuf {
        self.workers.join(crate::worktree::MAIN)
    }

    /// Create the state directories. The main worktree must exist: a state
    /// directory created first would make `git worktree add` refuse the path.
    pub fn ensure(&self) -> anyhow::Result<()> {
        let main = self.main_tree();
        anyhow::ensure!(
            main.join(".git").exists(),
            "QS has no worktree at {} yet; run `cargo quicksilver map` or `run` first",
            main.display()
        );
        crate::worktree::state_dir(&main)?;
        for p in [&self.index, &self.runs, &self.state] {
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
