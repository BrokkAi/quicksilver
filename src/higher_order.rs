//! Higher-order experiment planning and evidence classification.

use crate::types::{
    Mutant, Outcome, PlannedMutant, TestInfo, TestObservation, TestVerdict, VerdictKind,
};
use rand::seq::SliceRandom;
use rand::SeedableRng;
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

#[derive(Debug, Clone)]
pub(crate) struct SecondOrderPair {
    pub left: RunnableMutant,
    pub right: RunnableMutant,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecondOrderExperiment {
    pub pair_id: String,
    pub mutant_ids: [String; 2],
    pub vector: VectorSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecondOrderReport {
    pub requested_pairs: usize,
    pub eligible_mutants: usize,
    pub completed_pairs: usize,
    pub caught_pairs: usize,
    pub higher_order_kill_rate: f64,
    pub implied_first_order_kill_probability: f64,
    pub experiments: Vec<SecondOrderExperiment>,
    pub assumption: String,
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

/// Select disjoint, reproducible pairs. A pair must share a staged schema
/// binary and activate distinct sites. Only decided first-order Caught/Missed
/// mutants are eligible.
pub(crate) fn choose_second_order_pairs(
    runnable: &[RunnableMutant],
    outcomes: &[Outcome],
    requested: usize,
    seed: u64,
) -> (usize, Vec<SecondOrderPair>) {
    let verdicts: HashMap<&str, VerdictKind> = outcomes
        .iter()
        .map(|outcome| (outcome.mutant.id.as_str(), outcome.kind))
        .collect();
    let mut eligible: Vec<RunnableMutant> = runnable
        .iter()
        .filter(|r| {
            matches!(
                verdicts.get(r.planned.mutant.id.as_str()),
                Some(VerdictKind::Caught | VerdictKind::Missed)
            )
        })
        .cloned()
        .collect();
    eligible.sort_by(|a, b| a.planned.mutant.id.cmp(&b.planned.mutant.id));
    let eligible_count = eligible.len();
    eligible.shuffle(&mut rand::rngs::StdRng::seed_from_u64(seed));

    let mut used = vec![false; eligible.len()];
    let mut pairs = Vec::new();
    for left_index in 0..eligible.len() {
        if used[left_index] || pairs.len() >= requested {
            continue;
        }
        let Some(right_index) = (left_index + 1..eligible.len()).find(|&right_index| {
            !used[right_index]
                && eligible[left_index].run_dir == eligible[right_index].run_dir
                && eligible[left_index].site_id != eligible[right_index].site_id
        }) else {
            continue;
        };
        used[left_index] = true;
        used[right_index] = true;
        pairs.push(SecondOrderPair {
            left: eligible[left_index].clone(),
            right: eligible[right_index].clone(),
        });
    }
    (eligible_count, pairs)
}

pub fn pair_id(left: &str, right: &str) -> String {
    if left <= right {
        format!("{left}+{right}")
    } else {
        format!("{right}+{left}")
    }
}

pub fn implied_first_order_probability(higher_order_kill_rate: f64) -> f64 {
    let h = higher_order_kill_rate.clamp(0.0, 1.0);
    1.0 - (1.0 - h).sqrt()
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
    let survivor = summarize_vector(survivor, survivor_complete);
    let control = summarize_vector(control, control_complete);
    let pair = summarize_vector(pair, pair_complete);
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

pub(crate) fn summarize_vector(observations: &[TestObservation], complete: bool) -> VectorSummary {
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

    #[test]
    fn second_order_inversion_has_expected_boundaries_and_midpoint() {
        assert_eq!(implied_first_order_probability(0.0), 0.0);
        assert_eq!(implied_first_order_probability(1.0), 1.0);
        let p = implied_first_order_probability(0.75);
        assert!((p - 0.5).abs() < 1e-12, "p={p}");
    }

    #[test]
    fn pair_id_is_order_independent() {
        assert_eq!(pair_id("a", "b"), pair_id("b", "a"));
    }
}
