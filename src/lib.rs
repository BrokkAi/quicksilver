pub mod baseline;
pub mod capture;
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
pub mod report;
pub mod scan;
pub mod schema;
pub mod scratch;
pub mod sitemap;
pub mod types;
pub mod util;
pub mod worktree;

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
        cli::Command::List(_) => cmd_list(&cli.common),
        cli::Command::Index(_) => cmd_index(&cli.common),
        cli::Command::Run(args) => cmd_run(&cli.common, args),
        cli::Command::Estimate(args) => cmd_estimate(&cli.common, args),
        cli::Command::AuditSelection(args) => cmd_audit_selection(&cli.common, args),
        cli::Command::Map(args) => cmd_map(&cli.common, args),
        cli::Command::Build(args) => cmd_build(&cli.common, args),
        cli::Command::Test(args) => cmd_test(&cli.common, args),
        cli::Command::Clean => cmd_clean(&cli.common),
    }
}

fn cmd_map(common: &cli::CommonOpts, args: cli::MapArgs) -> Result<i32> {
    let extras = ConfigExtras {
        schema_max_rounds: args.schema_max_rounds,
        identity_sample: args.identity_sample,
        ..ConfigExtras::default()
    };
    let (cfg, _) = engine::config_from_cli(common, extras)?;
    let paths = cfg.paths();
    let map = exec::edited::build_map(&cfg, &paths)?;
    let entered: std::collections::BTreeSet<u32> = map.hits.values().flatten().copied().collect();
    let green = map.hits.len();
    eprintln!(
        "map: {} sites ({} functions without a site), {} tests ({} green), {} sites entered by at least one test, {:.1}s",
        map.sites.len(),
        map.dropped.len(),
        map.tests.len(),
        green,
        entered.len(),
        map.map_secs
    );
    eprintln!(
        "map written to {}",
        sitemap::SiteMap::path(&paths).display()
    );
    eprintln!(
        "next: edit function bodies in {}, then run `cargo quicksilver build`",
        cfg.repo_root.display()
    );
    if let Some(path) = &cfg.json {
        crate::util::write_atomic(path, serde_json::to_vec_pretty(&map)?)?;
    }
    Ok(0)
}

fn cmd_build(common: &cli::CommonOpts, args: cli::BuildArgs) -> Result<i32> {
    let extras = ConfigExtras {
        schema_max_rounds: args.schema_max_rounds,
        ..ConfigExtras::default()
    };
    let (cfg, _) = engine::config_from_cli(common, extras)?;
    let paths = cfg.paths();
    paths.ensure()?;
    let checkout = cfg.repo_root.clone();
    let report = exec::edited::build(
        &cfg,
        &paths,
        (!args.no_capture).then_some(checkout.as_path()),
        args.restore,
    )?;
    if report.discarded_records > 0 {
        eprintln!(
            "discarded {} mutant(s) captured against an earlier map",
            report.discarded_records
        );
    }
    if !report.rejected.is_empty() {
        eprintln!("the edits in {} were not captured:", checkout.display());
        for p in &report.rejected {
            eprintln!("  {p}");
        }
        eprintln!("a mutant may only change function bodies outside test code; reset the checkout and edit again");
        return Ok(2);
    }
    match &report.captured {
        Some(m) if report.duplicate => eprintln!(
            "the edits in {} are mutant {} (already captured; edits {})",
            checkout.display(),
            m.id,
            m.fqns()
        ),
        Some(m) => eprintln!(
            "captured mutant {} from {}: edits {}",
            m.id,
            checkout.display(),
            m.fqns()
        ),
        None if !args.no_capture => eprintln!("no edits in {}", checkout.display()),
        None => {}
    }
    if report.results.is_empty() {
        eprintln!("nothing to build");
        return Ok(0);
    }
    let mut unviable = 0usize;
    for (m, status) in &report.results {
        match status {
            exec::edited::Status::Built { .. } => eprintln!("  COMPILED {} {}", m.id, m.fqns()),
            exec::edited::Status::Unviable { message } => {
                unviable += 1;
                eprintln!("  UNVIABLE {} {}", m.id, m.fqns());
                for line in message.lines().take(30) {
                    eprintln!("      {line}");
                }
            }
            other => eprintln!("  {:?} {} {}", other, m.id, m.fqns()),
        }
    }
    eprintln!(
        "build: {} mutant(s) compiled, {} unviable (generation {}, {} cargo invocation(s), {:.1}s)",
        report.results.len() - unviable,
        unviable,
        report.generation,
        report.builds,
        report.secs
    );
    eprintln!("next: run `cargo quicksilver test`");
    if let Some(path) = &cfg.json {
        let state = exec::edited::State::load(&paths)?;
        crate::util::write_atomic(path, serde_json::to_vec_pretty(&state)?)?;
    }
    Ok(if unviable > 0 { 1 } else { 0 })
}

fn cmd_test(common: &cli::CommonOpts, args: cli::TestArgs) -> Result<i32> {
    let (cfg, _) = engine::config_from_cli(common, ConfigExtras::default())?;
    let paths = cfg.paths();
    paths.ensure()?;
    let results = exec::edited::test(&cfg, &paths, &args.mutants)?;
    if results.is_empty() {
        eprintln!("no compiled mutants to test; run `cargo quicksilver build` first");
    }
    for (m, v) in &results {
        match v.kind {
            VerdictKind::Caught => eprintln!(
                "  CAUGHT      {} {} by {}{}",
                m.id,
                m.fqns(),
                v.test.as_deref().unwrap_or("?"),
                if v.message.is_some() { " (crash)" } else { "" }
            ),
            VerdictKind::Missed => eprintln!(
                "  MISSED      {} {} ({} tests passed: {}{})",
                m.id,
                m.fqns(),
                v.tests_run.len(),
                v.tests_run
                    .iter()
                    .take(4)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                if v.tests_run.len() > 4 { ", ..." } else { "" }
            ),
            VerdictKind::NotCovered => {
                eprintln!(
                    "  NOT_COVERED {} {} (no test enters these functions)",
                    m.id,
                    m.fqns()
                )
            }
            VerdictKind::Timeout => eprintln!(
                "  TIMEOUT     {} {} in {}",
                m.id,
                m.fqns(),
                v.test.as_deref().unwrap_or("?")
            ),
            VerdictKind::Errored | VerdictKind::Unviable => eprintln!(
                "  ERRORED     {} {} {}",
                m.id,
                m.fqns(),
                v.message.as_deref().unwrap_or("")
            ),
        }
    }
    if let Some(path) = &cfg.json {
        crate::util::write_atomic(path, serde_json::to_vec_pretty(&results)?)?;
    }
    Ok(0)
}

fn cmd_clean(common: &cli::CommonOpts) -> Result<i32> {
    let (cfg, _) = engine::config_from_cli(common, ConfigExtras::default())?;
    let repo = worktree::repo_of(&cfg.repo_root)?;
    let ours = worktree::list_ours(&repo)?;
    if ours.is_empty() {
        eprintln!("no QS worktrees for {}", repo.toplevel.display());
    }
    for tree in &ours {
        worktree::remove(&repo, tree);
        eprintln!("removed {}", tree.display());
    }
    let root = worktree::root(&repo);
    // Also forget trees that were deleted by hand; mbx's own collection is
    // not something to rely on.
    match std::process::Command::new("mbx")
        .arg("clean")
        .arg("--under")
        .arg(&root)
        .output()
    {
        Ok(out) if !out.status.success() => tracing::warn!(
            "mbx clean --under {}: {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(err) => tracing::warn!("mbx clean --under {}: {err}", root.display()),
        _ => {}
    }
    if root.exists() {
        let _ = std::fs::remove_dir_all(&root);
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
    if cfg.test_selection != crate::config::TestSelection::Coverage {
        anyhow::bail!(
            "audit-selection audits coverage selection; run it with --test-selection coverage"
        );
    }
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
    let full_exec =
        crate::exec::schema::execute(&prepared.cfg, &prepared.paths, full_plan, cancel, 0)?;
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
    if prepared.cfg.test_selection == crate::config::TestSelection::Coverage {
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
    }
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
