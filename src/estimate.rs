use crate::outcome::OutcomeCounts;
use crate::plan::{Plan, mean_t_sel, unique_functions};
use crate::types::{BaselineReport, Mutant};
use crate::util::format_duration;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
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
    pub assumptions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct SampleCounts {
    pub caught: usize,
    pub missed: usize,
    pub not_covered: usize,
    pub unviable: usize,
    pub timeout: usize,
    pub builds: usize,
}

pub fn compute(
    n: usize,
    plan: &Plan,
    baseline: &BaselineReport,
    sample_outcomes: &[crate::types::Outcome],
    j_build: usize,
    j_test: usize,
    sampled_scope: String,
    sample_size: usize,
) -> EstimateReport {
    let scoped_n = plan.batches.iter().map(|b| b.mutants.len()).sum::<usize>() + plan.not_covered.len();
    let f_nc = if scoped_n == 0 {
        0.0
    } else {
        plan.not_covered.len() as f64 / scoped_n as f64
    };
    let (low, high) = wilson_interval(plan.not_covered.len(), scoped_n.max(1));
    let t_build = if baseline.package_costs.is_empty() {
        2.0
    } else {
        baseline.package_costs.iter().map(|p| p.rebuild_secs).sum::<f64>()
            / baseline.package_costs.len() as f64
    };
    let t_sel = mean_t_sel(plan);
    let t_suite = baseline.suite_secs;
    let j_cm = j_build.max(1);
    let covered_frac = 1.0 - f_nc;
    let t_mutants = n as f64 * (t_build + t_suite) / j_cm as f64;
    let compile_side = n as f64 * covered_frac * t_build / j_build.max(1) as f64;
    let test_side = n as f64 * covered_frac * t_sel / j_test.max(1) as f64;
    let t_qs = compile_side.max(test_side);
    // Scale unique functions from the sampled plan up to N.
    let functions_per_mutant = if scoped_n == 0 {
        1.0
    } else {
        unique_functions(plan) as f64 / scoped_n.max(1) as f64
    };
    let projected_batches = ((n as f64 * covered_frac * functions_per_mutant).max(1.0)) as usize;
    let t_v2 = (projected_batches as f64 * t_build) / j_build.max(1) as f64
        + n as f64 * covered_frac * t_sel / j_test.max(1) as f64;

    let counts = OutcomeCounts::from_outcomes(sample_outcomes);
    EstimateReport {
        sampled_scope: sampled_scope.clone(),
        sample_size,
        n_exact: n,
        f_nc,
        f_nc_low: low,
        f_nc_high: high,
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
            timeout: counts.timeout,
            builds: counts.builds,
        },
        assumptions: vec![
            format!("N = {n} is exact from discovery."),
            format!("t_suite = {:.3}s is exact from the baseline nextest run.", t_suite),
            format!(
                "f_nc = {:.3} (95% Wilson interval {:.3}–{:.3}) is sampled from scope `{sampled_scope}` and may not generalize. A crate with unusual coverage skews f_nc hard, and f_nc is the largest lever in the model.",
                f_nc, low, high
            ),
            format!(
                "t_sel mean = {:.4}s is the sum of baseline durations of covering tests, sampled from the same scope (n={}).",
                t_sel, sample_size
            ),
            format!("t_build mean = {:.3}s from per-package touch-and-rebuild.", t_build),
            "T_qs is a max of compile-side and test-side (decoupled pools), not a sum.".into(),
            "T_mutants uses a coupled worker model: N · (t_build + t_suite) / J_cm with J_cm = build-jobs.".into(),
            format!(
                "T_v2 treats one schema compile per unique covered function (projected {projected_batches} batches); that is a ceiling-ish proxy, not a measured schemata run."
            ),
            "Coverage index can under-report tests that spawn other processes. LLVM_PROFILE_FILE uses %p.".into(),
        ],
    }
}

fn wilson_interval(successes: usize, n: usize) -> (f64, f64) {
    if n == 0 {
        return (0.0, 1.0);
    }
    let z = 1.96_f64;
    let n = n as f64;
    let p = successes as f64 / n;
    let z2 = z * z;
    let denom = 1.0 + z2 / n;
    let center = p + z2 / (2.0 * n);
    let margin = z * ((p * (1.0 - p) + z2 / (4.0 * n)) / n).sqrt();
    let low = ((center - margin) / denom).clamp(0.0, 1.0);
    let high = ((center + margin) / denom).clamp(0.0, 1.0);
    (low, high)
}

pub fn render(report: &EstimateReport) -> String {
    let mut s = String::new();
    s.push_str("=== cargo-quicksilver estimate ===\n\n");
    s.push_str(&format!("Sampled scope: {}\n", report.sampled_scope));
    s.push_str(&format!("Sample size: {} mutants\n", report.sample_size));
    s.push_str(&format!(
        "N (mutants): {}  [exact]\n\n",
        report.n_exact
    ));
    s.push_str("Measured inputs:\n");
    s.push_str(&format!(
        "  f_nc (not covered): {:.3}  [range {:.3}–{:.3}; sampled from {}]\n",
        report.f_nc, report.f_nc_low, report.f_nc_high, report.sampled_scope
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
        "  quicksilver v1: {}   [max(compile, test)]\n",
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
    s.push_str(&format!("  Unviable:   {}\n", report.sample_counts.unviable));
    s.push_str(&format!("  Timeout:    {}\n", report.sample_counts.timeout));
    s.push_str(&format!("  Builds:     {}\n\n", report.sample_counts.builds));
    s.push_str("Assumptions:\n");
    for a in &report.assumptions {
        s.push_str(&format!("  - {a}\n"));
    }
    s
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
