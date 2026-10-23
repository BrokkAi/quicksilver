use crate::outcome::OutcomeCounts;
use crate::plan::{mean_t_sel, scoped_len, unique_packages, Plan};
use crate::types::{BaselineReport, Mutant};
use crate::util::format_duration;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EstimateReport {
    pub sampled_scope: String,
    pub sample_size: usize,
    pub n_exact: usize,
    pub f_nc: f64,
    pub f_nc_low: f64,
    pub f_nc_high: f64,
    pub t_suite_secs: f64,
    pub t_build_mean_secs: f64,
    pub t_sel_mean_secs: f64,
    pub j_build: usize,
    pub j_test: usize,
    pub j_cm: usize,
    pub t_mutants_secs: f64,
    pub t_qs_secs: f64,
    pub t_v2_secs: f64,
    pub schema_batches: usize,
    pub sample_counts: SampleCounts,
    #[serde(default)]
    pub by_return_kind: Vec<KindBreakdown>,
    pub assumptions: Vec<String>,
}

/// Per-return-kind counts from the executed sample. This is the input that
/// prunes v2's operator set (structurally unviable kinds at ~100%).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KindBreakdown {
    pub return_kind: String,
    pub n: usize,
    pub unviable: usize,
    pub unviable_rate: f64,
    pub caught: usize,
    pub missed: usize,
    pub not_covered: usize,
    pub errored: usize,
    pub timeout: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SampleCounts {
    pub caught: usize,
    pub missed: usize,
    pub not_covered: usize,
    pub unviable: usize,
    pub errored: usize,
    pub timeout: usize,
    pub builds: usize,
    #[serde(default)]
    pub cargo_invocations: usize,
}

#[allow(clippy::too_many_arguments)]
pub fn compute(
    n: usize,
    census: &Plan,
    t_sel_plan: &Plan,
    baseline: &BaselineReport,
    sample_outcomes: &[crate::types::Outcome],
    j_build: usize,
    j_test: usize,
    j_cm: usize,
    sampled_scope: String,
    sample_size: usize,
) -> EstimateReport {
    let scoped_n = scoped_len(census);
    let f_nc = if scoped_n == 0 {
        0.0
    } else {
        census.not_covered.len() as f64 / scoped_n as f64
    };
    let t_build = if baseline.package_costs.is_empty() {
        2.0
    } else {
        baseline
            .package_costs
            .iter()
            .map(|p| p.rebuild_secs)
            .sum::<f64>()
            / baseline.package_costs.len() as f64
    };
    let t_sel = mean_t_sel(t_sel_plan);
    let t_suite = baseline.suite_secs;
    let j_cm = j_cm.max(1);
    let covered_frac = 1.0 - f_nc;
    let t_mutants = n as f64 * (t_build + t_suite) / j_cm as f64;
    let compile_side = n as f64 * covered_frac * t_build / j_build.max(1) as f64;
    let test_side = n as f64 * covered_frac * t_sel / j_test.max(1) as f64;
    let t_qs = compile_side.max(test_side);
    // Schemata batches per crate, not per function. unique_packages already
    // counts only crates with covered mutants — do not multiply by covered_frac.
    let projected_batches = unique_packages(census);
    let t_v2 = (projected_batches as f64 * t_build) / j_build.max(1) as f64
        + n as f64 * covered_frac * t_sel / j_test.max(1) as f64;

    let counts = OutcomeCounts::from_outcomes(sample_outcomes);
    let by_return_kind = kind_breakdown(sample_outcomes);
    EstimateReport {
        sampled_scope: sampled_scope.clone(),
        sample_size,
        n_exact: n,
        f_nc,
        f_nc_low: f_nc,
        f_nc_high: f_nc,
        t_suite_secs: t_suite,
        t_build_mean_secs: t_build,
        t_sel_mean_secs: t_sel,
        j_build,
        j_test,
        j_cm,
        t_mutants_secs: t_mutants,
        t_qs_secs: t_qs,
        t_v2_secs: t_v2,
        schema_batches: projected_batches,
        sample_counts: SampleCounts {
            caught: counts.caught,
            missed: counts.missed,
            not_covered: counts.not_covered,
            unviable: counts.unviable,
            errored: counts.errored,
            timeout: counts.timeout,
            builds: counts.builds,
            cargo_invocations: 0,
        },
        by_return_kind,
        assumptions: vec![
            format!("N = {n} is exact from discovery inside scope `{sampled_scope}` (out-of-scope mutants are excluded, not filed as NotCovered)."),
            format!("t_suite = {:.3}s is the workspace nextest wall clock (cargo-mutants runs the whole suite, not only -p scope).", t_suite),
            format!(
                "f_nc = {:.3} is a census of the {scoped_n} mutants against the map: a mutant is not covered when no test entered its function in the map's recording run.",
                f_nc
            ),
            format!(
                "t_sel mean = {:.4}s is over every covered mutant: the tests that entered its function, at the durations the map's recording run measured (the sample is n={}).",
                t_sel, sample_size
            ),
            format!("t_build mean = {:.3}s from one `cargo build --tests -p P` per package.", t_build),
            "T_qs is a max of compile-side and test-side (decoupled pools), not a sum.".into(),
            format!(
                "T_mutants uses a coupled worker model: N · (t_build + t_suite) / J_cm. J_cm defaults to the same RAM/CPU formula as J_build (here {j_cm}); override with --cm-jobs. cargo-mutants workers each run a full rustc and will not actually use all cores."
            ),
            format!(
                "T_v2 treats one schema compile per crate that has covered mutants ({projected_batches} batches), not one per function. That is a proxy, not a measured schemata run."
            ),
            "Unviable rate by return kind is from the executed sample. Kinds at ~100% (ref, impl_trait, dyn) are the v2 pre-filter.".into(),
        ],
    }
}

pub fn render(report: &EstimateReport) -> String {
    let mut s = String::new();
    s.push_str("=== cargo-quicksilver estimate ===\n\n");
    s.push_str(&format!("Sampled scope: {}\n", report.sampled_scope));
    s.push_str(&format!("Sample size: {} mutants\n", report.sample_size));
    s.push_str(&format!("N (mutants): {}  [exact]\n\n", report.n_exact));
    s.push_str("Measured inputs:\n");
    s.push_str(&format!(
        "  f_nc (not covered): {:.3}  [census of {}]\n",
        report.f_nc, report.sampled_scope
    ));
    s.push_str(&format!(
        "  t_suite: {}  [exact]\n",
        format_duration(report.t_suite_secs)
    ));
    s.push_str(&format!(
        "  t_build (mean): {}\n",
        format_duration(report.t_build_mean_secs)
    ));
    s.push_str(&format!(
        "  t_sel (mean, covered): {}\n",
        format_duration(report.t_sel_mean_secs)
    ));
    s.push_str(&format!(
        "  J_build={}, J_test={}, J_cm={}\n\n",
        report.j_build, report.j_test, report.j_cm
    ));
    s.push_str("Projected exhaustive wall clock:\n");
    s.push_str(&format!(
        "  cargo-mutants:  {}   [N · (t_build + t_suite) / J_cm]\n",
        format_duration(report.t_mutants_secs)
    ));
    s.push_str(&format!(
        "  per-mutant builds (removed engine): {}   [max(compile, test)]\n",
        format_duration(report.t_qs_secs)
    ));
    s.push_str(&format!(
        "  schemata v2:    {}   [{} schema builds]\n\n",
        format_duration(report.t_v2_secs),
        report.schema_batches
    ));
    s.push_str(&format!(
        "Outcome breakdown (sample of {}):\n",
        report.sample_size
    ));
    s.push_str(&format!("  Caught:     {}\n", report.sample_counts.caught));
    s.push_str(&format!("  Missed:     {}\n", report.sample_counts.missed));
    s.push_str(&format!(
        "  NotCovered: {}\n",
        report.sample_counts.not_covered
    ));
    s.push_str(&format!(
        "  Unviable:   {}\n",
        report.sample_counts.unviable
    ));
    s.push_str(&format!("  Errored:    {}\n", report.sample_counts.errored));
    s.push_str(&format!("  Timeout:    {}\n", report.sample_counts.timeout));
    s.push_str(&format!(
        "  Mutant builds: {}\n",
        report.sample_counts.builds
    ));
    s.push_str(&format!(
        "  Cargo invocations: {}\n\n",
        report.sample_counts.cargo_invocations
    ));
    if !report.by_return_kind.is_empty() {
        s.push_str("Unviable rate by return kind (sample):\n");
        s.push_str(&format!(
            "  {:<12} {:>6} {:>9} {:>8} {:>7} {:>7}\n",
            "kind", "n", "unviable", "rate", "caught", "missed"
        ));
        for k in &report.by_return_kind {
            s.push_str(&format!(
                "  {:<12} {:>6} {:>9} {:>8.3} {:>7} {:>7}\n",
                k.return_kind, k.n, k.unviable, k.unviable_rate, k.caught, k.missed
            ));
        }
        s.push('\n');
    }
    s.push_str("Assumptions:\n");
    for a in &report.assumptions {
        s.push_str(&format!("  - {a}\n"));
    }
    s
}

pub fn kind_breakdown(outcomes: &[crate::types::Outcome]) -> Vec<KindBreakdown> {
    use crate::types::VerdictKind;
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Acc {
        n: usize,
        unviable: usize,
        caught: usize,
        missed: usize,
        not_covered: usize,
        errored: usize,
        timeout: usize,
    }
    let mut map: BTreeMap<String, Acc> = BTreeMap::new();
    for o in outcomes {
        let e = map.entry(o.mutant.return_kind.clone()).or_default();
        e.n += 1;
        match o.kind {
            VerdictKind::Unviable => e.unviable += 1,
            VerdictKind::Caught => e.caught += 1,
            VerdictKind::Missed => e.missed += 1,
            VerdictKind::NotCovered => e.not_covered += 1,
            VerdictKind::Errored => e.errored += 1,
            VerdictKind::Timeout => e.timeout += 1,
        }
    }
    let mut rows: Vec<KindBreakdown> = map
        .into_iter()
        .map(|(return_kind, a)| KindBreakdown {
            unviable_rate: if a.n == 0 {
                0.0
            } else {
                a.unviable as f64 / a.n as f64
            },
            return_kind,
            n: a.n,
            unviable: a.unviable,
            caught: a.caught,
            missed: a.missed,
            not_covered: a.not_covered,
            errored: a.errored,
            timeout: a.timeout,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.n.cmp(&a.n)
            .then_with(|| a.return_kind.cmp(&b.return_kind))
    });
    rows
}

pub fn sample_mutants(mutants: &[Mutant], n: usize, seed: u64) -> Vec<Mutant> {
    use rand::seq::SliceRandom;
    use rand::{Rng, SeedableRng};
    let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
    let mut v = mutants.to_vec();
    v.shuffle(&mut rng);
    if n == 0 || n >= v.len() {
        return v;
    }
    v.truncate(n);
    let _ = rng.gen::<u64>();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::Plan;
    use crate::types::{
        BaselineReport, LocalityBatch, Mutant, Outcome, PackageCost, PlannedMutant, TestInfo,
    };

    fn mutant(pkg: &str, fqn: &str) -> Mutant {
        Mutant {
            id: format!("{pkg}::{fqn}"),
            package: pkg.into(),
            path: format!("{pkg}/src/lib.rs"),
            fqn: fqn.into(),
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            end_line: 3,
            body_start_byte: 0,
            body_end_byte: 10,
            replacement: "{ 0 }".into(),
            return_kind: "number".into(),
        }
    }

    fn covered(pkg: &str, fqn: &str) -> PlannedMutant {
        PlannedMutant {
            mutant: mutant(pkg, fqn),
            tests: vec![TestInfo {
                id: "pkg::t".into(),
                name: "t".into(),
                package: pkg.into(),
                binary_id: pkg.into(),
                binary_name: pkg.into(),
                binary_rel: "deps/t".into(),
                cwd_rel: pkg.into(),
                duration_secs: 0.5,
            }],
            estimated_cost_secs: 1.5,
        }
    }

    #[test]
    fn t_v2_batches_per_crate_not_per_function() {
        let plan = Plan {
            batches: vec![
                LocalityBatch {
                    package: "a".into(),
                    file: "a/src/lib.rs".into(),
                    mutants: (0..20).map(|i| covered("a", &format!("f{i}"))).collect(),
                },
                LocalityBatch {
                    package: "b".into(),
                    file: "b/src/lib.rs".into(),
                    mutants: (0..15).map(|i| covered("b", &format!("g{i}"))).collect(),
                },
            ],
            not_covered: vec![mutant("a", "uncovered")],
            full_suite: false,
        };
        let baseline = BaselineReport {
            suite_secs: 10.0,
            tests: Vec::new(),
            package_costs: vec![PackageCost {
                package: "a".into(),
                rebuild_secs: 4.0,
            }],
            preflight_recompiled: None,
            failed_test_ids: Vec::new(),
        };
        let n = crate::plan::scoped_len(&plan);
        let report = compute(
            n,
            &plan,
            &plan,
            &baseline,
            &[] as &[Outcome],
            2,
            4,
            16,
            "a+b".into(),
            0,
        );
        assert_eq!(n, 36);
        // 1 not covered / 36 in-scope — not 0 because we did not file other crates as NC.
        assert!((report.f_nc - 1.0 / 36.0).abs() < 1e-9);
        assert_eq!(report.schema_batches, 2, "one schema compile per crate");
        assert_eq!(report.j_cm, 16);
        assert_ne!(report.j_cm, report.j_build);
        // Compile side of v2 is 2 * t_build / J_build, not 35 function-builds.
        let expected_v2_compile = 2.0 * 4.0 / 2.0;
        let covered_frac = 1.0 - report.f_nc;
        let expected_v2_test = n as f64 * covered_frac * 0.5 / 4.0;
        assert!((report.t_v2_secs - (expected_v2_compile + expected_v2_test)).abs() < 1e-9);
    }

    #[test]
    fn j_cm_independent_of_j_build() {
        let plan = Plan::default();
        let baseline = BaselineReport {
            suite_secs: 100.0,
            tests: Vec::new(),
            package_costs: vec![PackageCost {
                package: "a".into(),
                rebuild_secs: 10.0,
            }],
            preflight_recompiled: None,
            failed_test_ids: Vec::new(),
        };
        let a = compute(100, &plan, &plan, &baseline, &[], 5, 5, 5, "a".into(), 0);
        let b = compute(100, &plan, &plan, &baseline, &[], 5, 5, 50, "a".into(), 0);
        assert!(b.t_mutants_secs < a.t_mutants_secs);
        assert!((a.t_mutants_secs / b.t_mutants_secs - 10.0).abs() < 1e-9);
    }

    #[test]
    fn unviable_rate_by_return_kind() {
        let mut outcomes = Vec::new();
        let mut m = mutant("a", "f");
        m.return_kind = "ref".into();
        outcomes.push(Outcome::unviable(m.clone(), "no".into()));
        outcomes.push(Outcome::unviable(m, "no".into()));
        let mut n = mutant("a", "g");
        n.return_kind = "number".into();
        outcomes.push(Outcome::caught(n, "t".into()));
        let rows = kind_breakdown(&outcomes);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].return_kind, "ref");
        assert_eq!(rows[0].n, 2);
        assert_eq!(rows[0].unviable, 2);
        assert!((rows[0].unviable_rate - 1.0).abs() < 1e-9);
        assert_eq!(rows[1].return_kind, "number");
        assert_eq!(rows[1].unviable, 0);
        assert_eq!(rows[1].caught, 1);
    }
}
