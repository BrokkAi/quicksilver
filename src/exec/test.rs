use crate::exec::process::{run_exact_with_env, RunStatus};
use crate::types::{Outcome, PlannedMutant};
use crate::util::duration_from_secs;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinariesManifest {
    pub mutant_id: String,
    pub tests: Vec<ManifestTest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestTest {
    pub name: String,
    pub binary: String,
    pub cwd_rel: String,
    #[serde(default)]
    pub id: String,
}

pub fn run_mutant_tests(
    planned: &PlannedMutant,
    run_dir: &Path,
    baseline_root: &Path,
    timeout_floor: Duration,
) -> Result<Outcome> {
    run_mutant_tests_with_env(planned, run_dir, baseline_root, timeout_floor, &[])
}

pub fn run_mutant_tests_with_env(
    planned: &PlannedMutant,
    run_dir: &Path,
    baseline_root: &Path,
    timeout_floor: Duration,
    extra_env: &[(String, String)],
) -> Result<Outcome> {
    let manifest_path = run_dir.join("manifest.json");
    let text = fs::read_to_string(&manifest_path)?;
    let manifest: BinariesManifest = serde_json::from_str(&text)?;
    let tests: Vec<&ManifestTest> = if planned.tests.is_empty() {
        manifest.tests.iter().collect()
    } else {
        planned
            .tests
            .iter()
            .filter_map(|want| {
                manifest.tests.iter().find(|t| {
                    (!want.id.is_empty() && (t.id == want.id || t.name == want.name))
                        || (t.name == want.name && t.cwd_rel == want.cwd_rel)
                        || t.name == want.name
                })
            })
            .collect()
    };
    let tests = if tests.is_empty() {
        manifest.tests.iter().collect::<Vec<_>>()
    } else {
        tests
    };

    for t in tests {
        let binary = run_dir.join(&t.binary);
        let cwd = if t.cwd_rel.is_empty() {
            baseline_root.to_path_buf()
        } else {
            baseline_root.join(&t.cwd_rel)
        };
        let dur = planned
            .tests
            .iter()
            .find(|p| (!p.id.is_empty() && (p.id == t.id || p.name == t.name)) || p.name == t.name)
            .map(|p| p.duration_secs)
            .unwrap_or(0.001);
        let timeout = test_timeout(dur, timeout_floor);
        match run_exact_with_env(&binary, &t.name, &cwd, timeout, extra_env)? {
            RunStatus::Passed => continue,
            RunStatus::Failed => {
                return Ok(Outcome::caught(planned.mutant.clone(), t.name.clone()));
            }
            RunStatus::Error { code } => {
                return Ok(Outcome::errored(
                    planned.mutant.clone(),
                    t.name.clone(),
                    format!(
                        "test {} exited {:?} (libtest failures are 101; this is not Caught)",
                        t.name, code
                    ),
                ));
            }
            RunStatus::Timeout => {
                return Ok(Outcome::timeout(planned.mutant.clone(), t.name.clone()));
            }
        }
    }
    Ok(Outcome::missed(planned.mutant.clone()))
}

pub fn cleanup_run(run_dir: &Path) {
    let _ = fs::remove_dir_all(run_dir);
}

pub fn run_dir_for(runs_root: &Path, mutant_id: &str) -> PathBuf {
    runs_root.join(mutant_id)
}

/// Per-test budget: 3× that test's green duration, never the sum of the
/// whole selection. Missing timings (index default 0.001s) used to collapse
/// this to the 5s floor, so 17s integration tests always timed out.
pub fn test_timeout(duration_secs: f64, floor: Duration) -> Duration {
    const SENTINEL: f64 = 0.0015;
    // nextest profile.quicksilver slow-timeout period.
    const MISSING_SECS: f64 = 120.0;
    let secs = if duration_secs <= SENTINEL {
        MISSING_SECS.max(floor.as_secs_f64())
    } else {
        (duration_secs * 3.0).max(floor.as_secs_f64())
    };
    duration_from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_uses_this_test_not_the_selection_sum() {
        let floor = Duration::from_secs(5);
        assert_eq!(test_timeout(17.0, floor), Duration::from_secs_f64(51.0));
        assert_eq!(test_timeout(0.2, floor), floor);
        assert_eq!(test_timeout(0.001, floor), Duration::from_secs(120));
    }
}
