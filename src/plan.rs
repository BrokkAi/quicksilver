use crate::types::{BaselineReport, LocalityBatch, Mutant, PlannedMutant, TestInfo};
use std::collections::HashMap;

/// Where the planner gets each mutant's tests.
pub trait TestSelector {
    /// False files the mutant as NotCovered at plan time.
    fn covers(&self, m: &Mutant) -> bool;
    fn tests_for(&self, m: &Mutant) -> Vec<TestInfo>;
    /// Every test the selector knows about.
    fn catalog(&self) -> &[TestInfo];
}

/// Every test in a package that depends on the mutant's package is a
/// candidate. Nothing is NotCovered at plan time; the
/// schema executor narrows each mutant to the tests that entered its site.
pub struct PackageSelector {
    tests: Vec<TestInfo>,
    candidates: HashMap<String, Vec<usize>>,
}

impl PackageSelector {
    pub fn new(meta: &cargo_metadata::Metadata, tests: &[TestInfo]) -> Self {
        let mut candidates = HashMap::new();
        for pkg in meta.workspace_packages() {
            let name = pkg.name.to_string();
            let allowed = crate::cargo_cmd::reverse_dependents(meta, std::slice::from_ref(&name));
            let picked: Vec<usize> = tests
                .iter()
                .enumerate()
                .filter(|(_, t)| allowed.contains(&t.package))
                .map(|(i, _)| i)
                .collect();
            candidates.insert(name, picked);
        }
        Self {
            tests: tests.to_vec(),
            candidates,
        }
    }
}

impl TestSelector for PackageSelector {
    fn covers(&self, _m: &Mutant) -> bool {
        true
    }
    fn tests_for(&self, m: &Mutant) -> Vec<TestInfo> {
        self.candidates
            .get(&m.package)
            .map(|ix| ix.iter().map(|&i| self.tests[i].clone()).collect())
            .unwrap_or_default()
    }
    fn catalog(&self) -> &[TestInfo] {
        &self.tests
    }
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub batches: Vec<LocalityBatch>,
    pub not_covered: Vec<Mutant>,
    /// Run every planned test. Otherwise the executor records which tests
    /// enter each mutant's function and runs only those.
    pub full_suite: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestMode {
    /// Tests the coverage index claims execute this mutant.
    Impact,
    /// Every catalogued test (the cargo-mutants full-suite ground truth).
    FullSuite,
}

pub fn plan(
    mutants: &[Mutant],
    index: &dyn TestSelector,
    baseline: &BaselineReport,
    shard: Option<(u32, u32)>,
    build_jobs: usize,
) -> Plan {
    plan_with(
        mutants,
        index,
        baseline,
        shard,
        build_jobs,
        TestMode::Impact,
    )
}

/// Same as `plan`, but every union-covered mutant runs the whole suite.
pub fn plan_full_suite(
    mutants: &[Mutant],
    index: &dyn TestSelector,
    baseline: &BaselineReport,
    shard: Option<(u32, u32)>,
    build_jobs: usize,
) -> Plan {
    plan_with(
        mutants,
        index,
        baseline,
        shard,
        build_jobs,
        TestMode::FullSuite,
    )
}

fn plan_with(
    mutants: &[Mutant],
    index: &dyn TestSelector,
    baseline: &BaselineReport,
    shard: Option<(u32, u32)>,
    build_jobs: usize,
    mode: TestMode,
) -> Plan {
    let cost_by_pkg: HashMap<&str, f64> = baseline
        .package_costs
        .iter()
        .map(|p| (p.package.as_str(), p.rebuild_secs))
        .collect();

    let mut all_suite = if !baseline.tests.is_empty() {
        baseline.tests.clone()
    } else {
        index.catalog().to_vec()
    };
    all_suite.sort_by(|a, b| {
        a.duration_secs
            .partial_cmp(&b.duration_secs)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.name.cmp(&b.name))
    });

    let mut not_covered = Vec::new();
    let mut covered = Vec::new();
    for m in mutants {
        if !index.covers(m) {
            not_covered.push(m.clone());
            continue;
        }
        let mut tests = match mode {
            TestMode::Impact => index.tests_for(m),
            TestMode::FullSuite => all_suite.clone(),
        };
        tests.sort_by(|a, b| {
            a.duration_secs
                .partial_cmp(&b.duration_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.name.cmp(&b.name))
        });
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
        full_suite: mode == TestMode::FullSuite,
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
            if m.tests.is_empty() {
                continue;
            }
            sum += m.tests.iter().map(|t| t.duration_secs).sum::<f64>();
            n += 1.0;
        }
    }
    if n == 0.0 {
        0.0
    } else {
        sum / n
    }
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

/// Schemata batches per crate that has at least one covered mutant.
pub fn unique_packages(plan: &Plan) -> usize {
    let mut set = std::collections::BTreeSet::new();
    for b in &plan.batches {
        if !b.mutants.is_empty() {
            set.insert(b.package.as_str());
        }
    }
    set.len()
}

pub fn scoped_len(plan: &Plan) -> usize {
    plan.batches.iter().map(|b| b.mutants.len()).sum::<usize>() + plan.not_covered.len()
}
