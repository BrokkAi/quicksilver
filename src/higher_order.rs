//! Higher-order experiment planning and evidence classification.

use crate::types::{
    Mutant, Outcome, PlannedMutant, TestInfo, TestObservation, TestVerdict, VerdictKind,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub(crate) struct RunnableMutant {
    pub planned: PlannedMutant,
    pub run_dir: PathBuf,
    pub site_id: u32,
    pub variant: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct TriagePair {
    pub survivor: RunnableMutant,
    pub control: RunnableMutant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageClassification {
    /// The survivor is killed by a test selected only through the control.
    SelectionGap,
    /// The survivor adds no observable effect to the caught control over the
    /// complete union test set. This is bounded evidence, not an equivalence proof.
    LikelyEquivalent,
    /// The pair adds kills neither constituent produces alone.
    InteractionExposesGap,
    /// The survivor suppresses at least one kill produced by the control.
    MasksControl,
    /// The pair both adds and suppresses kills.
    MixedInteraction,
    /// At least one vector was incomplete or contained non-comparable results.
    Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorSummary {
    pub tests: usize,
    pub killed_test_ids: Vec<String>,
    pub complete: bool,
    pub comparable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageFinding {
    pub survivor_id: String,
    pub survivor_fqn: String,
    pub control_id: String,
    pub control_fqn: String,
    pub classification: TriageClassification,
    pub survivor: VectorSummary,
    pub control: VectorSummary,
    pub pair: VectorSummary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TriageReport {
    pub survivors: usize,
    pub experiments: Vec<TriageFinding>,
}

/// Choose nearby caught controls for each survivor. Only mutants in the same
/// staged schema directory can be combined; different sites are required.
pub(crate) fn choose_triage_pairs(
    runnable: &[RunnableMutant],
    outcomes: &[Outcome],
    neighbors: usize,
) -> Vec<TriagePair> {
    let verdicts: HashMap<&str, VerdictKind> = outcomes
        .iter()
        .map(|outcome| (outcome.mutant.id.as_str(), outcome.kind))
        .collect();
    let mut survivors: Vec<&RunnableMutant> = runnable
        .iter()
        .filter(|r| verdicts.get(r.planned.mutant.id.as_str()) == Some(&VerdictKind::Missed))
        .collect();
    survivors.sort_by(|a, b| a.planned.mutant.id.cmp(&b.planned.mutant.id));

    let mut pairs = Vec::new();
    for survivor in survivors {
        let mut controls: Vec<&RunnableMutant> = runnable
            .iter()
            .filter(|control| {
                verdicts.get(control.planned.mutant.id.as_str()) == Some(&VerdictKind::Caught)
                    && control.run_dir == survivor.run_dir
                    && control.site_id != survivor.site_id
            })
            .collect();
        controls
            .sort_by_key(|control| neighbor_key(&survivor.planned.mutant, &control.planned.mutant));
        for control in controls.into_iter().take(neighbors) {
            pairs.push(TriagePair {
                survivor: survivor.clone(),
                control: control.clone(),
            });
        }
    }
    pairs
}

fn neighbor_key(survivor: &Mutant, control: &Mutant) -> (u8, usize, String) {
    let locality = if survivor.path == control.path { 0 } else { 1 };
    (
        locality,
        survivor.start_byte.abs_diff(control.start_byte),
        control.id.clone(),
    )
}

pub(crate) fn union_tests(a: &PlannedMutant, b: &PlannedMutant) -> Vec<TestInfo> {
    let mut by_id: BTreeMap<String, TestInfo> = BTreeMap::new();
    for test in a.tests.iter().chain(&b.tests) {
        by_id.entry(test.id.clone()).or_insert_with(|| test.clone());
    }
    let mut tests: Vec<TestInfo> = by_id.into_values().collect();
    tests.sort_by(|a, b| {
        a.duration_secs
            .partial_cmp(&b.duration_secs)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });
    tests
}

pub(crate) fn classify_triage(
    survivor: &[TestObservation],
    survivor_complete: bool,
    control: &[TestObservation],
    control_complete: bool,
    pair: &[TestObservation],
    pair_complete: bool,
) -> (
    TriageClassification,
    VectorSummary,
    VectorSummary,
    VectorSummary,
) {
    let survivor = summarize(survivor, survivor_complete);
    let control = summarize(control, control_complete);
    let pair = summarize(pair, pair_complete);
    let comparable = survivor.complete
        && control.complete
        && pair.complete
        && survivor.comparable
        && control.comparable
        && pair.comparable;
    let class = if !comparable {
        TriageClassification::Inconclusive
    } else {
        let s: BTreeSet<&str> = survivor
            .killed_test_ids
            .iter()
            .map(String::as_str)
            .collect();
        let c: BTreeSet<&str> = control.killed_test_ids.iter().map(String::as_str).collect();
        let p: BTreeSet<&str> = pair.killed_test_ids.iter().map(String::as_str).collect();
        if !s.is_empty() {
            TriageClassification::SelectionGap
        } else {
            let added = p.difference(&c).next().is_some();
            let removed = c.difference(&p).next().is_some();
            match (added, removed) {
                (false, false) => TriageClassification::LikelyEquivalent,
                (true, false) => TriageClassification::InteractionExposesGap,
                (false, true) => TriageClassification::MasksControl,
                (true, true) => TriageClassification::MixedInteraction,
            }
        }
    };
    (class, survivor, control, pair)
}

fn summarize(observations: &[TestObservation], complete: bool) -> VectorSummary {
    let comparable = observations
        .iter()
        .all(|o| matches!(o.verdict, TestVerdict::Passed | TestVerdict::Killed));
    let mut killed_test_ids: Vec<String> = observations
        .iter()
        .filter(|o| o.verdict == TestVerdict::Killed)
        .map(|o| o.test_id.clone())
        .collect();
    killed_test_ids.sort();
    killed_test_ids.dedup();
    VectorSummary {
        tests: observations.len(),
        killed_test_ids,
        complete,
        comparable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(id: &str, verdict: TestVerdict) -> TestObservation {
        TestObservation {
            test_id: id.into(),
            test_name: id.into(),
            verdict,
            wall_secs: 0.1,
        }
    }

    #[test]
    fn triage_distinguishes_equivalent_gap_and_masking_evidence() {
        let pass = vec![obs("a", TestVerdict::Passed), obs("b", TestVerdict::Passed)];
        let control = vec![obs("a", TestVerdict::Killed), obs("b", TestVerdict::Passed)];
        assert_eq!(
            classify_triage(&pass, true, &control, true, &control, true).0,
            TriageClassification::LikelyEquivalent
        );
        let survivor_killed = vec![obs("a", TestVerdict::Passed), obs("b", TestVerdict::Killed)];
        assert_eq!(
            classify_triage(&survivor_killed, true, &control, true, &control, true).0,
            TriageClassification::SelectionGap
        );
        assert_eq!(
            classify_triage(&pass, true, &control, true, &pass, true).0,
            TriageClassification::MasksControl
        );
    }

    #[test]
    fn incomplete_or_infrastructure_vectors_are_inconclusive() {
        let pass = vec![obs("a", TestVerdict::Passed)];
        let infra = vec![obs("a", TestVerdict::Infrastructure)];
        assert_eq!(
            classify_triage(&pass, false, &pass, true, &pass, true).0,
            TriageClassification::Inconclusive
        );
        assert_eq!(
            classify_triage(&pass, true, &infra, true, &pass, true).0,
            TriageClassification::Inconclusive
        );
    }
}
