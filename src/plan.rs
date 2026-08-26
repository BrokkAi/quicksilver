use crate::index::CoverageIndex;
use crate::types::{BaselineReport, LocalityBatch, Mutant, PlannedMutant};
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub batches: Vec<LocalityBatch>,
    pub not_covered: Vec<Mutant>,
}

pub fn plan(
    mutants: &[Mutant],
    index: &CoverageIndex,
    baseline: &BaselineReport,
    shard: Option<(u32, u32)>,
    build_jobs: usize,
) -> Plan {
    let cost_by_pkg: HashMap<&str, f64> = baseline
        .package_costs
        .iter()
        .map(|p| (p.package.as_str(), p.rebuild_secs))
        .collect();

    let mut not_covered = Vec::new();
    let mut covered = Vec::new();
    for m in mutants {
        let mut tests = index.tests_covering(&m.path, m.start_line, m.end_line);
        tests.sort_by(|a, b| {
            a.duration_secs
                .partial_cmp(&b.duration_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.name.cmp(&b.name))
        });
        if tests.is_empty() {
            not_covered.push(m.clone());
            continue;
        }
        let t_build = cost_by_pkg.get(m.package.as_str()).copied().unwrap_or(1.0);
        let t_sel: f64 = tests.iter().map(|t| t.duration_secs).sum();
        covered.push(PlannedMutant {
            mutant: m.clone(),
            tests,
            estimated_cost_secs: t_build + t_sel,
        });
    }

    // Locality batches keyed (package, file), ordered by start_byte.
    let mut groups: HashMap<(String, String), Vec<PlannedMutant>> = HashMap::new();
    for p in covered {
        groups
            .entry((p.mutant.package.clone(), p.mutant.path.clone()))
            .or_default()
            .push(p);
    }
    let mut batches: Vec<LocalityBatch> = groups
        .into_iter()
        .map(|((package, file), mut mutants)| {
            mutants.sort_by_key(|m| m.mutant.start_byte);
            LocalityBatch {
                package,
                file,
                mutants,
            }
        })
        .collect();
    batches.sort_by(|a, b| (&a.package, &a.file).cmp(&(&b.package, &b.file)));

    if let Some((k, n)) = shard {
        batches = batches
            .into_iter()
            .enumerate()
            .filter(|(i, _)| (*i as u32) % n == k - 1)
            .map(|(_, b)| b)
            .collect();
    }

    // Balance total estimated cost across build workers by assigning whole batches.
    batches = balance_batches(batches, build_jobs.max(1));

    Plan {
        batches,
        not_covered,
    }
}

fn balance_batches(batches: Vec<LocalityBatch>, jobs: usize) -> Vec<LocalityBatch> {
    // Keep locality order; the assignment is "whole batch to one worker" at
    // execution time via a shared queue. We only reorder here so costly
    // batches go first (better packing).
    let mut batches = batches;
    batches.sort_by(|a, b| {
        let ca: f64 = a.mutants.iter().map(|m| m.estimated_cost_secs).sum();
        let cb: f64 = b.mutants.iter().map(|m| m.estimated_cost_secs).sum();
        cb.partial_cmp(&ca)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| (&a.package, &a.file).cmp(&(&b.package, &b.file)))
    });
    let _ = jobs;
    batches
}

pub fn mean_t_sel(plan: &Plan) -> f64 {
    let mut sum = 0.0;
    let mut n = 0.0;
    for b in &plan.batches {
        for m in &b.mutants {
            sum += m.tests.iter().map(|t| t.duration_secs).sum::<f64>();
            n += 1.0;
        }
    }
    if n == 0.0 { 0.0 } else { sum / n }
}

pub fn unique_functions(plan: &Plan) -> usize {
    let mut set = std::collections::BTreeSet::new();
    for b in &plan.batches {
        for m in &b.mutants {
            set.insert((m.mutant.package.as_str(), m.mutant.fqn.as_str()));
        }
    }
    set.len()
}
