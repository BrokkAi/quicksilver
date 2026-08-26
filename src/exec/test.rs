use crate::exec::process::{RunStatus, run_exact};
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
}

pub fn run_mutant_tests(
    planned: &PlannedMutant,
    run_dir: &Path,
    baseline_root: &Path,
    timeout_floor: Duration,
) -> Result<Outcome> {
    let manifest_path = run_dir.join("manifest.json");
    let text = fs::read_to_string(&manifest_path)?;
    let manifest: BinariesManifest = serde_json::from_str(&text)?;
    let selected_sum: f64 = planned.tests.iter().map(|t| t.duration_secs).sum();
    let timeout = duration_from_secs(timeout_floor.as_secs_f64().max(3.0 * selected_sum));

    for t in &manifest.tests {
        let binary = run_dir.join(&t.binary);
        let cwd = if t.cwd_rel.is_empty() {
            baseline_root.to_path_buf()
        } else {
            baseline_root.join(&t.cwd_rel)
        };
        match run_exact(&binary, &t.name, &cwd, timeout)? {
            RunStatus::Passed => continue,
            RunStatus::Failed => {
                return Ok(Outcome::caught(planned.mutant.clone(), t.name.clone()));
            }
            RunStatus::Error { code } => {
                return Ok(Outcome::unviable(
                    planned.mutant.clone(),
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
