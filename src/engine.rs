use crate::baseline::{preflight_recompile_count, prepare_baseline};
use crate::cargo_cmd::load_metadata;
use crate::config::{merge_config, Config, ConfigExtras, FileConfig};
use crate::discover::DiscoverFilter;
use crate::estimate::{self, EstimateReport};
use crate::index;
use crate::plan::{self, Plan};
use crate::scratch;
use crate::types::{BaselineReport, Mutant, Outcome, Paths, TestInfo};
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

pub struct Prepared {
    pub cfg: Config,
    pub paths: Paths,
    pub baseline: BaselineReport,
    pub mutants: Vec<Mutant>,
    /// Every listed test, with baseline durations attached.
    pub tests: Vec<TestInfo>,
    /// The commit under test; keys evidence that depends on the source.
    pub source_key: String,
    /// Candidate tests per package. The executor narrows each mutant to the
    /// tests its recording run saw enter the mutated function.
    pub selector: plan::PackageSelector,
    pub plan: Plan,
}

impl Prepared {
    pub fn selector(&self) -> &dyn plan::TestSelector {
        &self.selector
    }
}

pub fn prepare(cfg: Config, skip_baseline: bool, for_estimate: bool) -> Result<Prepared> {
    let paths = cfg.paths();
    let baseline = if !skip_baseline && !for_estimate {
        crate::baseline::prepare_baseline_reusing_timings(&cfg, &paths)?
    } else {
        prepare_baseline(&cfg, &paths, skip_baseline, for_estimate)?
    };
    scratch::check_free_space(&paths.session, cfg.max_scratch)?;

    // Listing tests runs cargo (building the tests when the baseline build
    // was skipped); discovery is analysis. Neither needs the other.
    let listing = {
        let (cfg, paths) = (cfg.clone(), paths.clone());
        std::thread::spawn(move || index::list_tests_plain(&cfg, &paths))
    };
    let mutants = if cfg.in_diff.is_some() {
        crate::discover::discover_in_diff(&paths.baseline, &discover_filter(&cfg))?
    } else {
        crate::discover::discover(&paths.baseline, &discover_filter(&cfg))?
    };
    eprintln!("discovered {} mutants", mutants.len());

    let mut tests = listing
        .join()
        .map_err(|_| anyhow::anyhow!("the test listing thread panicked"))??;
    eprintln!("{} tests catalogued", tests.len());
    let timing = index::attach_timings(&mut tests, &baseline.tests);
    if !baseline.tests.is_empty() && !tests.is_empty() {
        if timing.matched == 0 {
            anyhow::bail!(
                "failed to attach baseline timings to any of {} listed tests ({} junit cases). Match on (binary-id, name), not bare test name.",
                timing.total,
                baseline.tests.len()
            );
        }
        if timing.matched * 5 < timing.total {
            tracing::warn!(
                "attached timings to only {}/{} listed tests",
                timing.matched,
                timing.total
            );
        }
    }
    let meta = load_metadata(&paths.baseline)?;
    let selector = plan::PackageSelector::new(&meta, &tests);
    let plan = plan::plan(&mutants, &selector, &baseline, cfg.shard, cfg.build_jobs);
    eprintln!(
        "plan: {} to build, {} not covered",
        plan.batches.iter().map(|b| b.mutants.len()).sum::<usize>(),
        plan.not_covered.len()
    );
    let source_key = crate::worktree::head(&paths.main_tree())?;

    Ok(Prepared {
        cfg,
        paths,
        baseline,
        mutants,
        tests,
        source_key,
        selector,
        plan,
    })
}

pub fn run_schema(
    prepared: &Prepared,
    limit: usize,
    sample: Option<&[Mutant]>,
) -> Result<(Vec<Outcome>, u64)> {
    let exec = run_schema_detailed(prepared, limit, sample)?;
    Ok((exec.outcomes, exec.cargo_invocations))
}

pub fn run_schema_detailed(
    prepared: &Prepared,
    limit: usize,
    sample: Option<&[Mutant]>,
) -> Result<crate::exec::ExecResult> {
    let cancel = install_ctrlc();
    let plan = if let Some(sample) = sample {
        plan::plan(
            sample,
            prepared.selector(),
            &prepared.baseline,
            prepared.cfg.shard,
            prepared.cfg.build_jobs,
        )
    } else {
        prepared.plan.clone()
    };
    let mut exec =
        crate::exec::schema::execute(&prepared.cfg, &prepared.paths, plan, cancel, limit)?;
    let rows = std::mem::take(&mut exec.coupling_rows);
    exec.coupling = Some(crate::coupling::merge_and_save(
        &prepared.paths,
        &prepared.source_key,
        rows,
    )?);
    Ok(exec)
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
    // Preflight costs a whole workspace rebuild, and its answer depends only
    // on manifests and build scripts. Measure once per scratch session and
    // again when one of those changes, not on every per-commit run.
    let key = match crate::baseline::preflight_key(&prepared.paths.baseline) {
        Ok(key) => Some(key),
        Err(err) => {
            tracing::warn!("preflight key: {err:#}");
            None
        }
    };
    if let Some(n) = key
        .as_deref()
        .and_then(|k| crate::baseline::cached_preflight(&prepared.paths, k))
    {
        eprintln!("preflight recompile count after one mutant: {n} units (measured earlier in this scratch session)");
        return Ok(Some(n));
    }
    match preflight_recompile_count(&prepared.cfg, &prepared.paths, m) {
        Ok(n) => {
            if let Some(k) = &key {
                if let Err(err) = crate::baseline::save_preflight(&prepared.paths, k, n) {
                    tracing::warn!("saving the preflight count: {err:#}");
                }
            }
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

/// The estimate for a full run, from the executed sample and a census of
/// every discovered mutant against `map`: a mutant whose function no test
/// entered needs neither a build nor a test run, and the others run the tests
/// that entered their function, with the durations the map recorded.
pub fn estimate_from(
    prepared: &Prepared,
    map: &crate::sitemap::SiteMap,
    sample_size: usize,
    outcomes: &[Outcome],
) -> EstimateReport {
    let scope = if prepared.cfg.packages.is_empty() {
        "workspace".to_string()
    } else {
        prepared.cfg.packages.join(", ")
    };
    let selector = crate::sitemap::MapSelector::new(map);
    let census = plan::plan(
        &prepared.mutants,
        &selector,
        &prepared.baseline,
        prepared.cfg.shard,
        prepared.cfg.build_jobs,
    );
    estimate::compute(
        prepared.mutants.len(),
        &census,
        &census,
        &prepared.baseline,
        outcomes,
        prepared.cfg.build_jobs,
        prepared.cfg.test_jobs,
        prepared.cfg.cm_jobs,
        scope,
        sample_size,
    )
}

pub fn write_outputs(
    cfg: &Config,
    outcomes: &[Outcome],
    estimate: Option<&EstimateReport>,
    cargo_invocations: u64,
    triage: Option<&crate::higher_order::TriageReport>,
    second_order: Option<&crate::higher_order::SecondOrderReport>,
    coupling: Option<&crate::coupling::CouplingSummary>,
) -> Result<()> {
    crate::report::print_outcomes(outcomes, cargo_invocations);
    if let Some(path) = &cfg.json {
        crate::report::write_json_with_evidence(
            path,
            outcomes,
            estimate,
            cargo_invocations,
            triage,
            second_order,
            coupling,
        )?;
    }
    Ok(())
}
