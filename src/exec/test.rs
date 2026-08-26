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
    let selected_sum: f64 = planned.tests.iter().map(|t| t.duration_secs).sum();
    let timeout = duration_from_secs(timeout_floor.as_secs_f64().max(3.0 * selected_sum));

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
