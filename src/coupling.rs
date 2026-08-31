//! Persistent mutant-by-test observations and derived kill-vector coupling.

use crate::types::{Paths, TestObservation, TestVerdict};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

const COUPLING_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouplingRow {
    pub activation_id: String,
    pub mutant_ids: Vec<String>,
    pub selected_test_ids: Vec<String>,
    pub observations: Vec<TestObservation>,
    pub complete: bool,
}

impl CouplingRow {
    pub fn new(
        activation_id: String,
        mut mutant_ids: Vec<String>,
        selected_test_ids: Vec<String>,
        observations: Vec<TestObservation>,
        complete: bool,
    ) -> Self {
        mutant_ids.sort();
        mutant_ids.dedup();
        let mut row = Self {
            activation_id,
            mutant_ids,
            selected_test_ids,
            observations,
            complete,
        };
        row.normalize();
        row
    }

    fn normalize(&mut self) {
        self.selected_test_ids.sort();
        self.selected_test_ids.dedup();
        self.observations.sort_by(|a, b| a.test_id.cmp(&b.test_id));
        self.observations.dedup_by(|a, b| a.test_id == b.test_id);
        self.complete = self.complete
            && self.selected_test_ids.iter().all(|id| {
                self.observations
                    .iter()
                    .any(|observation| &observation.test_id == id)
            });
    }

    fn comparable(&self) -> bool {
        self.complete
            && self.observations.iter().all(|observation| {
                matches!(
                    observation.verdict,
                    TestVerdict::Passed | TestVerdict::Killed
                )
            })
    }

    fn kills(&self) -> BTreeSet<&str> {
        self.observations
            .iter()
            .filter(|observation| observation.verdict == TestVerdict::Killed)
            .map(|observation| observation.test_id.as_str())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouplingArtifact {
    pub version: u32,
    pub source_cache_key: String,
    pub rows: BTreeMap<String, CouplingRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subsumption {
    /// The harder-to-kill mutant: every test that kills it also kills `subsumed_id`.
    pub dominator_id: String,
    pub subsumed_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CouplingSummary {
    pub path: PathBuf,
    pub rows: usize,
    pub complete_rows: usize,
    pub test_columns: usize,
    pub subsumptions: Vec<Subsumption>,
    pub invalidated_for_source_change: bool,
}

impl CouplingArtifact {
    pub fn empty(source_cache_key: String) -> Self {
        Self {
            version: COUPLING_VERSION,
            source_cache_key,
            rows: BTreeMap::new(),
        }
    }

    pub fn load_or_empty(path: &Path, source_cache_key: &str) -> Result<(Self, bool)> {
        if !path.is_file() {
            return Ok((Self::empty(source_cache_key.into()), false));
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read coupling artifact {}", path.display()))?;
        let artifact: Self = serde_json::from_str(&text)
            .with_context(|| format!("parse coupling artifact {}", path.display()))?;
        if artifact.version != COUPLING_VERSION || artifact.source_cache_key != source_cache_key {
            return Ok((Self::empty(source_cache_key.into()), true));
        }
        Ok((artifact, false))
    }

    pub fn merge(&mut self, rows: impl IntoIterator<Item = CouplingRow>) {
        for mut incoming in rows {
            incoming.normalize();
            match self.rows.get_mut(&incoming.activation_id) {
                None => {
                    self.rows.insert(incoming.activation_id.clone(), incoming);
                }
                Some(existing) if existing.mutant_ids == incoming.mutant_ids => {
                    let mut selected: BTreeSet<String> =
                        existing.selected_test_ids.iter().cloned().collect();
                    selected.extend(incoming.selected_test_ids);
                    let mut observations: BTreeMap<String, TestObservation> = existing
                        .observations
                        .iter()
                        .cloned()
                        .map(|observation| (observation.test_id.clone(), observation))
                        .collect();
                    for observation in incoming.observations {
                        observations.insert(observation.test_id.clone(), observation);
                    }
                    existing.selected_test_ids = selected.into_iter().collect();
                    existing.observations = observations.into_values().collect();
                    existing.complete = existing
                        .selected_test_ids
                        .iter()
                        .all(|id| observations_contains(&existing.observations, id));
                    existing.normalize();
                }
                Some(existing) => {
                    // An activation id collision must not combine unrelated rows.
                    *existing = incoming;
                }
            }
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::util::write_atomic(path, serde_json::to_vec_pretty(self)?)
    }

    pub fn subsumptions(&self) -> Vec<Subsumption> {
        let rows: Vec<&CouplingRow> = self
            .rows
            .values()
            .filter(|row| row.mutant_ids.len() == 1 && row.comparable())
            .collect();
        let mut relations = Vec::new();
        for (index, left) in rows.iter().enumerate() {
            let left_kills = left.kills();
            if left_kills.is_empty() {
                continue;
            }
            for right in rows.iter().skip(index + 1) {
                if left.selected_test_ids != right.selected_test_ids {
                    continue;
                }
                let right_kills = right.kills();
                if right_kills.is_empty() || left_kills == right_kills {
                    continue;
                }
                if left_kills.is_subset(&right_kills) {
                    relations.push(Subsumption {
                        dominator_id: left.mutant_ids[0].clone(),
                        subsumed_id: right.mutant_ids[0].clone(),
                    });
                } else if right_kills.is_subset(&left_kills) {
                    relations.push(Subsumption {
                        dominator_id: right.mutant_ids[0].clone(),
                        subsumed_id: left.mutant_ids[0].clone(),
                    });
                }
            }
        }
        relations.sort_by(|a, b| {
            (&a.dominator_id, &a.subsumed_id).cmp(&(&b.dominator_id, &b.subsumed_id))
        });
        relations.dedup();
        relations
    }

    pub fn summary(&self, path: PathBuf, invalidated: bool) -> CouplingSummary {
        let tests: BTreeSet<&str> = self
            .rows
            .values()
            .flat_map(|row| row.selected_test_ids.iter().map(String::as_str))
            .collect();
        CouplingSummary {
            path,
            rows: self.rows.len(),
            complete_rows: self.rows.values().filter(|row| row.complete).count(),
            test_columns: tests.len(),
            subsumptions: self.subsumptions(),
            invalidated_for_source_change: invalidated,
        }
    }
}

fn observations_contains(observations: &[TestObservation], id: &str) -> bool {
    observations
        .iter()
        .any(|observation| observation.test_id == id)
}

pub fn path(paths: &Paths) -> PathBuf {
    paths.index.join("coupling-v1.json")
}

pub fn merge_and_save(
    paths: &Paths,
    source_cache_key: &str,
    rows: Vec<CouplingRow>,
) -> Result<CouplingSummary> {
    let path = path(paths);
    let lock_path = paths.index.join("coupling-v1.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("open coupling lock {}", lock_path.display()))?;
    // Shards share one session. Serialize load-merge-save so two processes do
    // not both read the same old artifact and let the last writer erase rows.
    let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("lock coupling artifact {}", lock_path.display()));
    }
    let (mut artifact, invalidated) = CouplingArtifact::load_or_empty(&path, source_cache_key)?;
    artifact.merge(rows);
    artifact.save(&path)?;
    let summary = artifact.summary(path, invalidated);
    let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("unlock coupling artifact {}", lock_path.display()));
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(test: &str, verdict: TestVerdict) -> TestObservation {
        TestObservation {
            test_id: test.into(),
            test_name: test.into(),
            verdict,
            wall_secs: 0.1,
        }
    }

    fn row(id: &str, selected: &[&str], killed: &[&str], complete: bool) -> CouplingRow {
        CouplingRow::new(
            id.into(),
            vec![id.into()],
            selected.iter().map(|value| (*value).into()).collect(),
            selected
                .iter()
                .take(if complete { selected.len() } else { 1 })
                .map(|test| {
                    observation(
                        test,
                        if killed.contains(test) {
                            TestVerdict::Killed
                        } else {
                            TestVerdict::Passed
                        },
                    )
                })
                .collect(),
            complete,
        )
    }

    #[test]
    fn prefix_merge_becomes_complete_only_after_every_column_is_observed() {
        let mut artifact = CouplingArtifact::empty("source".into());
        artifact.merge([row("m", &["a", "b"], &["a"], false)]);
        assert!(!artifact.rows["m"].complete);
        artifact.merge([CouplingRow::new(
            "m".into(),
            vec!["m".into()],
            vec!["a".into(), "b".into()],
            vec![
                observation("a", TestVerdict::Killed),
                observation("b", TestVerdict::Passed),
            ],
            true,
        )]);
        assert!(artifact.rows["m"].complete);
    }

    #[test]
    fn subsumption_requires_complete_comparable_equal_column_vectors() {
        let mut artifact = CouplingArtifact::empty("source".into());
        artifact.merge([
            row("hard", &["a", "b"], &["a"], true),
            row("easy", &["a", "b"], &["a", "b"], true),
            row("prefix", &["a", "b"], &["a"], false),
        ]);
        assert_eq!(
            artifact.subsumptions(),
            vec![Subsumption {
                dominator_id: "hard".into(),
                subsumed_id: "easy".into(),
            }]
        );
    }

    #[test]
    fn source_key_change_invalidates_old_rows() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("coupling.json");
        let mut artifact = CouplingArtifact::empty("old".into());
        artifact.merge([row("m", &["a"], &["a"], true)]);
        artifact.save(&path).unwrap();
        let (fresh, invalidated) = CouplingArtifact::load_or_empty(&path, "new").unwrap();
        assert!(invalidated);
        assert!(fresh.rows.is_empty());
        assert_eq!(fresh.source_cache_key, "new");
    }
}
