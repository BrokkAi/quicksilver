pub mod baseline;
pub mod bifrost;
pub mod cargo_cmd;
pub mod cli;
pub mod config;
pub mod coupling;
pub mod discover;
pub mod engine;
pub mod estimate;
pub mod exec;
pub mod higher_order;
pub mod index;
pub mod outcome;
pub mod plan;
pub mod reach;
pub mod report;
pub mod schema;
pub mod scratch;
pub mod types;
pub mod util;

use anyhow::Result;
use config::ConfigExtras;
use discover::discover;
use estimate::sample_mutants;
use outcome::OutcomeCounts;
use types::VerdictKind;

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();
}

pub fn run_cli(cli: cli::Cli) -> Result<i32> {
    init_tracing();
    match cli.command {
        cli::Command::Probe(args) => cmd_probe(&cli.common, args),
        cli::Command::List(_) => cmd_list(&cli.common),
        cli::Command::Index(_) => cmd_index(&cli.common),
        cli::Command::Run(args) => cmd_run(&cli.common, args),
        cli::Command::Estimate(args) => cmd_estimate(&cli.common, args),
        cli::Command::AuditSelection(args) => cmd_audit_selection(&cli.common, args),
        cli::Command::GraphProbe(args) => cmd_graph_probe(&cli.common, args),
    }
}

fn cmd_graph_probe(common: &cli::CommonOpts, args: cli::GraphProbeArgs) -> Result<i32> {
    let (cfg, _) = engine::config_from_cli(common, ConfigExtras::default())?;
    let root = args.root.clone().unwrap_or_else(|| cfg.repo_root.clone());
    let opened = crate::bifrost::open_workspace(&root)?;
    eprintln!(
        "bifrost open: {:.2}s (store {:?})",
        opened.open_secs, opened.store_path
    );
    let start = std::time::Instant::now();
    let mutants =
        crate::discover::discover_in(&opened.workspace, &root, &engine::discover_filter(&cfg))?;
    eprintln!(
        "discovered {} mutants in {:.2}s",
        mutants.len(),
        start.elapsed().as_secs_f64()
    );
    let meta = crate::cargo_cmd::load_metadata(&root)?;
    let tests = match &args.nextest_list {
        Some(p) => {
            let bytes = std::fs::read(p)?;
            crate::index::parse_nextest_list(&bytes, &meta, &root.join("target"))?
        }
        None => Vec::new(),
    };
    let reach = crate::reach::ReachIndex::build(&opened.workspace, &meta, &tests)?;
    println!("{}", serde_json::to_string_pretty(&reach.stats)?);
    let start = std::time::Instant::now();
    let sample = sample_mutants(&mutants, args.sample, args.sample_seed);
    let mut complete = 0usize;
    let mut total_tests = 0usize;
    for m in &sample {
        let sel = reach.select(m);
        complete += usize::from(sel.complete);
        total_tests += sel.tests.len();
        println!(
            "{:<70} complete={} reached_fns={} tests={}{}",
            format!("{}:{}", m.path, m.fqn),
            sel.complete,
            sel.reached_functions,
            sel.tests.len(),
            sel.fallback_reason
                .map(|r| format!(" ({r})"))
                .unwrap_or_default()
        );
        for t in sel.tests.iter().take(5) {
            println!("    {}", t.id);
        }
    }
    eprintln!(
        "selected for {} mutants in {:.2}s: {} complete, {} tests total",
        sample.len(),
        start.elapsed().as_secs_f64(),
        complete,
        total_tests
    );
    Ok(0)
}

fn cmd_probe(common: &cli::CommonOpts, args: cli::ProbeArgs) -> Result<i32> {
    let (cfg, _) = engine::config_from_cli(common, ConfigExtras::default())?;
    std::fs::create_dir_all(&cfg.scratch_root)?;
    let report = scratch::probe(&cfg.scratch_root, cfg.clone_backend, cfg.allow_copy)?;
    println!("{}", scratch::format_probe(&report));
    if args.with_rebuild {
        let prepared = engine::prepare(cfg, false, false)?;
        let _ = engine::maybe_preflight(&prepared)?;
    }
    Ok(0)
}

fn cmd_list(common: &cli::CommonOpts) -> Result<i32> {
    let (cfg, _) = engine::config_from_cli(common, ConfigExtras::default())?;
    let mutants = discover(&cfg.repo_root, &engine::discover_filter(&cfg))?;
    crate::report::print_list(&mutants, std::io::stdout())?;
    if let Some(path) = &cfg.json {
        crate::util::write_atomic(path, serde_json::to_vec_pretty(&mutants)?)?;
    }
    Ok(0)
}

fn cmd_index(common: &cli::CommonOpts) -> Result<i32> {
    let (cfg, _) = engine::config_from_cli(common, ConfigExtras::default())?;
    // Stage A only; per-test attribution is the sample/run path.
    let prepared = engine::prepare(cfg, true, true)?;
    println!(
        "coverage index: {} tests catalogued, {} files in union, {} files attributed",
        prepared.index.tests.len(),
        prepared.index.union.len(),
        prepared.index.lines.len()
    );
    Ok(0)
}

fn cmd_run(common: &cli::CommonOpts, args: cli::RunArgs) -> Result<i32> {
    let extras = ConfigExtras {
        schema_max_rounds: args.schema_max_rounds,
        schema_batch_size: args.schema_batch_size,
        diff_against: args.diff_against.clone(),
        identity_sample: args.identity_sample,
        triage: args.triage,
        triage_neighbors: args.triage_neighbors,
        ..ConfigExtras::default()
    };
    let (cfg, _) = engine::config_from_cli(common, extras)?;
    let prepared = engine::prepare(cfg, args.skip_baseline, false)?;
    let _ = engine::maybe_preflight(&prepared)?;
    let exec = engine::run_schema_detailed(&prepared, args.limit, None)?;
    let outcomes = &exec.outcomes;
    let cargo_invocations = exec.cargo_invocations;
    crate::report::print_exhaustive(outcomes);
    if let Some(triage) = &exec.triage {
        crate::report::print_triage(triage);
    }
    if let Some(second_order) = &exec.second_order {
        crate::report::print_second_order(second_order);
    }
    if let Some(coupling) = &exec.coupling {
        crate::report::print_coupling(coupling);
    }
    if let Some(path) = &prepared.cfg.diff_against {
        let diffs = crate::schema::gate_differential(path, outcomes)?;
        if !diffs.is_empty() {
            eprintln!("differential gate failed: {} disagreements", diffs.len());
            for d in &diffs {
                eprintln!(
                    "  {}  {}  reference={:?} this run={:?}",
                    d.mutant_id, d.fqn, d.v1, d.v2
                );
            }
            engine::write_outputs(
                &prepared.cfg,
                outcomes,
                None,
                cargo_invocations,
                exec.triage.as_ref(),
                exec.second_order.as_ref(),
                exec.coupling.as_ref(),
            )?;
            return Ok(2);
        }
        eprintln!(
            "differential gate: {} reference verdicts matched",
            outcomes.len()
        );
    }
    engine::write_outputs(
        &prepared.cfg,
        outcomes,
        None,
        cargo_invocations,
        exec.triage.as_ref(),
        exec.second_order.as_ref(),
        exec.coupling.as_ref(),
    )?;
    let missed = outcomes.iter().any(|o| o.kind == VerdictKind::Missed);
    Ok(if missed { 1 } else { 0 })
}

fn cmd_audit_selection(common: &cli::CommonOpts, args: cli::AuditSelectionArgs) -> Result<i32> {
    let extras = ConfigExtras {
        sample: args.sample,
        sample_seed: args.sample_seed,
        resume: false,
        ..ConfigExtras::default()
    };
    let (cfg, _) = engine::config_from_cli(common, extras)?;
    let mut prepared = engine::prepare(cfg, args.skip_baseline, true)?;
    if !args.skip_baseline {
        let _ = engine::maybe_preflight(&prepared)?;
    }
    let sample = sample_mutants(&prepared.mutants, args.sample, args.sample_seed);
    let covered: Vec<_> = sample
        .iter()
        .filter(|m| {
            prepared
                .index
                .union_covers(&m.path, m.start_line, m.end_line)
        })
        .cloned()
        .collect();
    crate::index::stage_b_for_mutants(
        &prepared.cfg,
        &prepared.paths,
        &mut prepared.index,
        &covered,
    )?;
    eprintln!(
        "audit-selection: {} covered mutants (of {} sampled)",
        covered.len(),
        sample.len()
    );
    prepared.cfg.resume = false;
    let (selected, _) = engine::run_schema(&prepared, 0, Some(&covered))?;
    let jsonl = prepared.paths.state.join("outcomes.jsonl");
    if jsonl.is_file() {
        let _ = std::fs::rename(&jsonl, prepared.paths.state.join("audit-selected.jsonl"));
    }
    let full_plan = crate::plan::plan_full_suite(
        &covered,
        &prepared.index,
        &prepared.baseline,
        prepared.cfg.shard,
        prepared.cfg.build_jobs,
    );
    let cancel = engine::install_ctrlc();
    let full_exec = crate::exec::schema::execute(
        &prepared.cfg,
        &prepared.paths,
        std::sync::Arc::clone(&prepared.backend),
        full_plan,
        cancel,
        0,
    )?;
    let report = crate::report::audit_compare(&selected, &full_exec.outcomes);
    crate::report::print_audit(&report);
    if let Some(path) = &prepared.cfg.json {
        crate::util::write_atomic(path, serde_json::to_vec_pretty(&report)?)?;
    }
    Ok(if report.selected_missed_full_caught > 0 {
        1
    } else {
        0
    })
}

fn cmd_estimate(common: &cli::CommonOpts, args: cli::EstimateArgs) -> Result<i32> {
    let extras = ConfigExtras {
        sample: args.sample,
        sample_seed: args.sample_seed,
        schema_max_rounds: args.schema_max_rounds,
        schema_batch_size: args.schema_batch_size,
        diff_against: args.diff_against.clone(),
        identity_sample: args.identity_sample,
        second_order_pairs: args.second_order_pairs,
        ..ConfigExtras::default()
    };
    let (cfg, _) = engine::config_from_cli(common, extras)?;
    let mut prepared = engine::prepare(cfg, args.skip_baseline, true)?;
    // Preflight mutates `baseline/` in place. Parallel --skip-baseline shards
    // hashing that tree then see a different cache key and rebuild Stage A.
    if !args.skip_baseline {
        let _ = engine::maybe_preflight(&prepared)?;
    }
    let sample = sample_mutants(&prepared.mutants, args.sample, args.sample_seed);
    let covered_sample: Vec<_> = sample
        .iter()
        .filter(|m| {
            prepared
                .index
                .union_covers(&m.path, m.start_line, m.end_line)
        })
        .cloned()
        .collect();
    crate::index::stage_b_for_mutants(
        &prepared.cfg,
        &prepared.paths,
        &mut prepared.index,
        &covered_sample,
    )?;
    // Stage B reloads coverage.json, which used to wipe attached durations
    // back to the 0.001s index default and 5s-timeout every slow test.
    let _ = crate::index::attach_timings(&mut prepared.index, &prepared.baseline.tests);
    eprintln!(
        "running sample of {} mutants through the pipeline",
        sample.len()
    );
    let exec = engine::run_schema_detailed(&prepared, 0, Some(&sample))?;
    let outcomes = &exec.outcomes;
    let cargo_invocations = exec.cargo_invocations;
    if let Some(path) = &prepared.cfg.diff_against {
        let diffs = crate::schema::gate_differential(path, outcomes)?;
        if !diffs.is_empty() {
            eprintln!("differential gate failed: {} disagreements", diffs.len());
            for d in &diffs {
                eprintln!(
                    "  {}  {}  reference={:?} this run={:?}",
                    d.mutant_id, d.fqn, d.v1, d.v2
                );
            }
            engine::write_outputs(
                &prepared.cfg,
                outcomes,
                None,
                cargo_invocations,
                None,
                exec.second_order.as_ref(),
                exec.coupling.as_ref(),
            )?;
            return Ok(2);
        }
        eprintln!(
            "differential gate: {} reference verdicts matched",
            outcomes.len()
        );
    }
    let mut report = engine::estimate_from(&prepared, &sample, outcomes);
    report.sample_counts.cargo_invocations = cargo_invocations as usize;
    print!("{}", estimate::render(&report));
    if let Some(second_order) = &exec.second_order {
        crate::report::print_second_order(second_order);
    }
    if let Some(coupling) = &exec.coupling {
        crate::report::print_coupling(coupling);
    }
    engine::write_outputs(
        &prepared.cfg,
        outcomes,
        Some(&report),
        cargo_invocations,
        None,
        exec.second_order.as_ref(),
        exec.coupling.as_ref(),
    )?;
    Ok(0)
}

pub fn counts(outcomes: &[types::Outcome]) -> OutcomeCounts {
    OutcomeCounts::from_outcomes(outcomes)
}
