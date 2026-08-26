pub mod baseline;
pub mod cargo_cmd;
pub mod cli;
pub mod config;
pub mod discover;
pub mod engine;
pub mod estimate;
pub mod exec;
pub mod index;
pub mod outcome;
pub mod plan;
pub mod report;
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
    }
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
    let (cfg, _) = engine::config_from_cli(common, ConfigExtras::default())?;
    let prepared = engine::prepare(cfg, args.skip_baseline, false)?;
    let _ = engine::maybe_preflight(&prepared)?;
    let (outcomes, _builds) = engine::run_prepared(&prepared, args.limit, None)?;
    engine::write_outputs(&prepared.cfg, &outcomes, None)?;
    let missed = outcomes.iter().any(|o| o.kind == VerdictKind::Missed);
    Ok(if missed { 1 } else { 0 })
}

fn cmd_estimate(common: &cli::CommonOpts, args: cli::EstimateArgs) -> Result<i32> {
    let extras = ConfigExtras {
        sample: args.sample,
        sample_seed: args.sample_seed,
        ..ConfigExtras::default()
    };
    let (cfg, _) = engine::config_from_cli(common, extras)?;
    let mut prepared = engine::prepare(cfg, args.skip_baseline, true)?;
    let _ = engine::maybe_preflight(&prepared)?;
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
    eprintln!("running sample of {} mutants through the pipeline", sample.len());
    let (outcomes, builds) = engine::run_prepared(&prepared, 0, Some(&sample))?;
    let mut report = engine::estimate_from(&prepared, &sample, &outcomes);
    report.sample_counts.builds = builds as usize;
    print!("{}", estimate::render(&report));
    engine::write_outputs(&prepared.cfg, &outcomes, Some(&report))?;
    Ok(0)
}

pub fn counts(outcomes: &[types::Outcome]) -> OutcomeCounts {
    OutcomeCounts::from_outcomes(outcomes)
}
