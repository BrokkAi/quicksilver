use crate::baseline::{preflight_recompile_count, prepare_baseline};
use crate::config::{merge_config, Config, ConfigExtras, FileConfig};
use crate::discover::{discover, DiscoverFilter};
use crate::estimate::{self, EstimateReport};
use crate::index::{self, CoverageIndex};
use crate::plan::{self, Plan};
use crate::scratch::{self, CloneBackend};
use crate::types::{BaselineReport, Mutant, Outcome, Paths};
use anyhow::Result;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub fn repo_root_from_cli(manifest: &Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = manifest {
        if p.ends_with("Cargo.toml") {
            return Ok(p.parent().unwrap_or(p).canonicalize()?);
        }
        return Ok(p.canonicalize()?);
    }
    Ok(std::env::current_dir()?)
}

pub fn config_from_cli(
    common: &crate::cli::CommonOpts,
    extras: ConfigExtras,
) -> Result<(Config, FileConfig)> {
    let repo = repo_root_from_cli(&common.manifest_path)?;
    let file = Config::load_file(&repo)?;
    let mut extras = extras;
    extras.packages = common.packages.clone();
    extras.files = common.files.clone();
    extras.in_diff = common.in_diff.clone();
    extras.index_scope = common.index_scope.clone();
    extras.reindex = common.reindex;
    extras.json = common.json.clone();
    extras.resume = common.resume;
    extras.shard = common
        .shard
        .as_deref()
        .map(crate::config::parse_shard)
        .transpose()?;
    extras.baseline_runs = common.baseline_runs;
    extras.timeout_floor_ms = common.timeout_floor_ms;
    extras.strict_source_isolation = common.strict_source_isolation;
    let cfg = merge_config(repo, file.clone(), common, extras)?;
    Ok((cfg, file))
}

pub fn install_ctrlc() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let f = Arc::clone(&flag);
    let _ = ctrlc::set_handler(move || {
        f.store(true, Ordering::SeqCst);
    });
    flag
}

pub fn discover_filter(cfg: &Config) -> DiscoverFilter {
    DiscoverFilter {
        packages: cfg.packages.clone(),
        files: cfg.files.clone(),
        in_diff: cfg.in_diff.clone(),
    }
}

pub fn default_index_scope(explicit: Option<&str>, mutants: &[Mutant]) -> Option<String> {
    if let Some(s) = explicit {
        return Some(s.to_string());
    }
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for m in mutants {
        *counts.entry(&m.package).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(p, _)| p.to_string())
}

pub struct Prepared {
    pub cfg: Config,
    pub paths: Paths,
    pub backend: Arc<dyn CloneBackend>,
    pub probe: scratch::ProbeReport,
    pub baseline: BaselineReport,
    pub mutants: Vec<Mutant>,
    pub index: CoverageIndex,
    pub plan: Plan,
}

pub fn prepare(mut cfg: Config, skip_baseline: bool, for_estimate: bool) -> Result<Prepared> {
    let paths = cfg.paths();
    paths.ensure()?;
    let (backend, probe) = scratch::probe_and_select(&cfg)?;
    eprintln!("{}", scratch::format_probe(&probe));
    scratch::check_free_space(&cfg.scratch_root, cfg.max_scratch)?;

    let baseline = prepare_baseline(&cfg, &paths, skip_baseline)?;

    let mut mutants = discover(&paths.baseline, &discover_filter(&cfg))?;
    eprintln!("discovered {} mutants", mutants.len());

    if for_estimate && cfg.index_scope.is_none() {
        cfg.index_scope = default_index_scope(cfg.index_scope.as_deref(), &mutants);
        if let Some(s) = &cfg.index_scope {
            eprintln!("index-scope defaulted to {s}");
        }
    }

    // Coverage is only valid for the indexed crate. Planning the rest of the
    // workspace against that index files every out-of-scope mutant as
    // NotCovered and inflates f_nc (and therefore the headline speedup).
    if let Some(scope) = cfg.index_scope.clone() {
        let before = mutants.len();
        mutants.retain(|m| m.package == scope);
        eprintln!(
            "planning {} mutants in index-scope {scope} ({} excluded, not NotCovered)",
            mutants.len(),
            before.saturating_sub(mutants.len())
        );
    }

    let mut index = index::build_or_load(&cfg, &paths, &baseline)?;
    let timing = index::attach_timings(&mut index, &baseline.tests);
    if !baseline.tests.is_empty() && !index.tests.is_empty() {
        if timing.matched == 0 {
            anyhow::bail!(
                "failed to attach baseline timings to any of {} indexed tests ({} junit cases). t_sel would collapse to ~0. Match on (binary-id, name), not bare test name.",
                timing.total,
                baseline.tests.len()
            );
        }
        if timing.matched * 5 < timing.total {
            tracing::warn!(
                "attached timings to only {}/{} indexed tests; t_sel may be biased low",
                timing.matched,
                timing.total
            );
        }
        if timing.matched > 0 {
            if let Err(err) = index.save(&paths) {
                tracing::warn!("failed to persist attached timings: {err:#}");
            }
        }
    }
    // Estimate pays Stage B only for the sample (cmd_estimate). Run/index
    // attribute every in-scope mutant now.
    if !for_estimate {
        let covered: Vec<_> = mutants
            .iter()
            .filter(|m| index.union_covers(&m.path, m.start_line, m.end_line))
            .cloned()
            .collect();
        index::stage_b_for_mutants(&cfg, &paths, &mut index, &covered)?;
        let _ = index::attach_timings(&mut index, &baseline.tests);
    }
    let plan = plan::plan(&mutants, &index, &baseline, cfg.shard, cfg.build_jobs);
    eprintln!(
        "plan: {} to build, {} not covered",
        plan.batches.iter().map(|b| b.mutants.len()).sum::<usize>(),
        plan.not_covered.len()
    );

    Ok(Prepared {
        cfg,
        paths,
        backend,
        probe,
        baseline,
        mutants,
        index,
        plan,
    })
}

pub fn run_schema(
    prepared: &Prepared,
    limit: usize,
    sample: Option<&[Mutant]>,
) -> Result<(Vec<Outcome>, u64)> {
    let cancel = install_ctrlc();
    let plan = if let Some(sample) = sample {
        plan::plan(
            sample,
            &prepared.index,
            &prepared.baseline,
            prepared.cfg.shard,
            prepared.cfg.build_jobs,
        )
    } else {
        prepared.plan.clone()
    };
    let exec = crate::exec::schema::execute(
        &prepared.cfg,
        &prepared.paths,
        Arc::clone(&prepared.backend),
        plan,
        cancel,
        limit,
    )?;
    Ok((exec.outcomes, exec.cargo_invocations))
}

pub fn maybe_preflight(prepared: &Prepared) -> Result<Option<u64>> {
    let Some(m) = prepared
        .plan
        .batches
        .iter()
        .flat_map(|b| b.mutants.iter())
        .map(|p| &p.mutant)
        .next()
    else {
        return Ok(None);
    };
    match preflight_recompile_count(&prepared.cfg, &prepared.paths, m) {
        Ok(n) => {
            eprintln!("preflight recompile count after one mutant: {n} units");
            if n > 8 {
                eprintln!(
                    "warning: {n} crates rebuilt for one function-body edit. A source-hashing build script is likely at work. Pin identities with [build-env] in quicksilver.toml (bifrost: BIFROST_BUILD_IDENTITY_OVERRIDE)."
                );
            }
            Ok(Some(n))
        }
        Err(err) => {
            tracing::warn!("preflight failed: {err:#}");
            Ok(None)
        }
    }
}

pub fn estimate_from(
    prepared: &Prepared,
    sample: &[Mutant],
    outcomes: &[Outcome],
) -> EstimateReport {
    let scope = prepared
        .cfg
        .index_scope
        .clone()
        .unwrap_or_else(|| "workspace".into());
    let sample_plan = plan::plan(
        sample,
        &prepared.index,
        &prepared.baseline,
        prepared.cfg.shard,
        prepared.cfg.build_jobs,
    );
    estimate::compute(
        prepared.mutants.len(),
        &prepared.plan,
        &sample_plan,
        &prepared.baseline,
        outcomes,
        prepared.cfg.build_jobs,
        prepared.cfg.test_jobs,
        prepared.cfg.cm_jobs,
        scope,
        sample.len(),
    )
}

pub fn write_outputs(
    cfg: &Config,
    outcomes: &[Outcome],
    estimate: Option<&EstimateReport>,
    cargo_invocations: u64,
) -> Result<()> {
    crate::report::print_outcomes(outcomes, cargo_invocations);
    if let Some(path) = &cfg.json {
        crate::report::write_json(path, outcomes, estimate, cargo_invocations)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Mutant;

    fn mutant(package: &str) -> Mutant {
        Mutant {
            id: format!("{package}::f"),
            package: package.into(),
            path: format!("{package}/src/lib.rs"),
            fqn: "f".into(),
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            end_line: 1,
            body_start_byte: 0,
            body_end_byte: 1,
            replacement: "{ 0 }".into(),
            return_kind: "number".into(),
        }
    }

    #[test]
    fn index_scope_picks_the_most_populous_package() {
        let mutants = vec![
            mutant("brokk-bifrost-core"),
            mutant("tiny_a"),
            mutant("tiny_a"),
            mutant("tiny_a"),
        ];
        assert_eq!(
            default_index_scope(None, &mutants).as_deref(),
            Some("tiny_a")
        );
    }

    #[test]
    fn index_scope_explicit_wins() {
        let mutants = vec![mutant("tiny_a"), mutant("tiny_a"), mutant("tiny_b")];
        assert_eq!(
            default_index_scope(Some("tiny_b"), &mutants).as_deref(),
            Some("tiny_b")
        );
    }
}
