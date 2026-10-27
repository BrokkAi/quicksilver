use crate::baseline::{preflight_recompile_count, prepare_baseline};
use crate::cargo_cmd::load_metadata;
use crate::config::{merge_config, Config, ConfigExtras, FileConfig};
use crate::discover::DiscoverFilter;
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
    crate::exec::test::set_test_budgets(&cfg.test_budget);
    crate::sandbox::configure(&cfg, &cfg.paths())?;
    Ok((cfg, file))
}

/// A flag that Ctrl-C, SIGTERM or SIGHUP sets, for stopping after the work
/// in progress. A second signal ends the process at once. Every call
/// returns the same flag.
pub fn install_ctrlc() -> Arc<AtomicBool> {
    static FLAG: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
    FLAG.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(false));
        let f = Arc::clone(&flag);
        let _ = ctrlc::set_handler(move || {
            if f.swap(true, Ordering::SeqCst) {
                std::process::exit(130);
            }
        });
        flag
    })
    .clone()
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
    /// Candidate tests per package. The executor narrows each mutant to the
    /// tests its recording run saw enter the mutated function.
    pub selector: plan::PackageSelector,
    /// Ids of the tests that are proptest properties.
    pub property_tests: std::collections::BTreeSet<String>,
    pub plan: Plan,
}

impl Prepared {
    pub fn selector(&self) -> &dyn plan::TestSelector {
        &self.selector
    }
}

pub fn prepare(cfg: Config, skip_baseline: bool) -> Result<Prepared> {
    let paths = cfg.paths();
    let baseline = if !skip_baseline {
        crate::baseline::prepare_baseline_reusing_timings(&cfg, &paths)?
    } else {
        prepare_baseline(&cfg, &paths, skip_baseline)?
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
    let skipped = index::drop_skipped(&cfg, &mut tests)?;
    if skipped.is_empty() {
        eprintln!("{} tests catalogued", tests.len());
    } else {
        eprintln!(
            "{} tests catalogued, {} more left out by [map] skip-tests",
            tests.len(),
            skipped.len()
        );
    }
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
    let property_tests = crate::proptest::property_tests(&paths.baseline, &meta, &tests)
        .unwrap_or_else(|err| {
            tracing::warn!("cannot look for proptest properties: {err:#}");
            Default::default()
        });
    let plan = plan::plan(&mutants, &selector, &baseline, cfg.shard, cfg.build_jobs);
    eprintln!(
        "plan: {} to build, {} not covered",
        plan.batches.iter().map(|b| b.mutants.len()).sum::<usize>(),
        plan.not_covered.len()
    );

    Ok(Prepared {
        cfg,
        paths,
        baseline,
        mutants,
        tests,
        selector,
        property_tests,
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
    // A survivor that only proptest properties judged may be caught on
    // another run, with other random cases.
    if !prepared.property_tests.is_empty() {
        for o in &mut exec.outcomes {
            if o.kind != crate::types::VerdictKind::Missed {
                continue;
            }
            if exec.selected_test_ids.get(&o.mutant.id).is_some_and(|ids| {
                !ids.is_empty() && ids.iter().all(|i| prepared.property_tests.contains(i))
            }) {
                o.message = Some(crate::proptest::NOTE.to_string());
            }
        }
    }
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
                    "warning: {n} crates rebuilt for one function-body edit. A source-hashing build script is likely at work. Pin identities with [build-env] in quicksilver.toml."
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

pub fn write_outputs(cfg: &Config, outcomes: &[Outcome], cargo_invocations: u64) -> Result<()> {
    crate::report::print_outcomes(outcomes, cargo_invocations);
    if let Some(path) = &cfg.json {
        crate::report::write_json(path, outcomes, cargo_invocations)?;
    }
    Ok(())
}
