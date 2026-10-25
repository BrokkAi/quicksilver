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
pub mod linediff;
pub mod outcome;
pub mod pending;
pub mod plan;
pub mod progress;
pub mod proptest;
pub mod report;
pub mod sandbox;
pub mod scan;
pub mod schema;
pub mod scratch;
pub mod sitemap;
pub mod survivors;
pub mod target;
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
    let result = match cli.command {
        cli::Command::List(_) => cmd_list(&cli.common),
        cli::Command::Run(args) => cmd_run(&cli.common, args),
        cli::Command::Estimate(args) => cmd_estimate(&cli.common, args),
        cli::Command::AuditSelection(args) => cmd_audit_selection(&cli.common, args),
        cli::Command::Map(args) => cmd_map(&cli.common, args),
        cli::Command::Build(args) => cmd_build(&cli.common, args),
        cli::Command::Test(args) => cmd_test(&cli.common, args),
        cli::Command::Uncovered(args) => cmd_uncovered(&cli.common, args),
        cli::Command::Pending(args) => pending::run(&cli.common, args),
        cli::Command::Survivors(args) => survivors::run(&cli.common, args),
        cli::Command::Clean => cmd_clean(&cli.common),
    };
    if let Some(line) = sandbox::summary() {
        eprintln!("{line}");
    }
    result
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
    if !map.skipped_tests.is_empty() {
        eprintln!(
            "{} test(s) left out by [map] skip-tests: not run, not counted as not green",
            map.skipped_tests.len()
        );
    }
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
    let sources = exec::edited::CaptureSources {
        checkout: (!args.no_capture && args.patches.is_empty()).then(|| cfg.repo_root.clone()),
        patches: args.patches.clone(),
        options: capture::CaptureOptions {
            skip_outside_bodies: args.skip_outside_bodies,
        },
        intents: match &args.intents {
            Some(path) => exec::edited::read_intents(path)?,
            None => Default::default(),
        },
    };
    let report = exec::edited::build(&cfg, &paths, &sources)?;
    // The capture result and any rejection were printed by `build` before
    // it compiled anything.
    if !report.rejected.is_empty() {
        return Ok(2);
    }
    if report.results.is_empty() {
        eprintln!("nothing to build");
        return Ok(0);
    }
    let mut unviable = 0usize;
    let mut new = 0usize;
    let mut untested = 0usize;
    for (m, status) in &report.results {
        match status {
            exec::edited::Status::Built { generation, .. } => {
                eprintln!("  COMPILED {} {}", m.id, m.fqns());
                if *generation == report.generation {
                    untested += 1;
                    if report.captured.iter().any(|captured| captured.id == m.id) {
                        new += 1;
                    }
                }
            }
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
        "build: {} new, {} untested in total, {} unviable (generation {}, {} cargo invocation(s), {:.1}s)",
        new,
        untested,
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

fn cmd_uncovered(common: &cli::CommonOpts, args: cli::UncoveredArgs) -> Result<i32> {
    let (cfg, _) = engine::config_from_cli(common, ConfigExtras::default())?;
    let head = worktree::head(&cfg.repo_root)?;
    let map = exec::edited::map_at(&cfg.paths(), &head)?;
    let files = crate::discover::file_globset(&cfg.files)?;
    let in_scope = |s: &sitemap::MapSite| {
        (cfg.packages.is_empty() || cfg.packages.contains(&s.package))
            && files.as_ref().is_none_or(|g| g.is_match(&s.path))
    };
    let total = map.sites.iter().filter(|s| in_scope(s)).count();
    let unentered: Vec<&sitemap::MapSite> = map
        .unentered()
        .into_iter()
        .filter(|s| in_scope(s))
        .collect();
    let hidden = unentered
        .iter()
        .filter(|s| s.scaffolding.is_some() && !args.scaffolding)
        .count();
    let shown: Vec<&sitemap::MapSite> = unentered
        .iter()
        .copied()
        .filter(|s| args.scaffolding || s.scaffolding.is_none())
        .collect();
    let no_callers = sitemap::without_callers(&map, &cfg.repo_root, &shown).unwrap_or_else(|err| {
        tracing::warn!("cannot look for callers: {err:#}");
        Default::default()
    });
    // The list is the command's output, on stdout; the summary is on stderr.
    for s in &shown {
        let mut marks = Vec::new();
        if no_callers.contains(&s.site_id) {
            marks.push("no callers".to_string());
        }
        if let Some(why) = &s.scaffolding {
            marks.push(format!("scaffolding: {why}"));
        }
        let marks = if marks.is_empty() {
            String::new()
        } else {
            format!("  [{}]", marks.join("; "))
        };
        println!("{}:{} {}{marks}", s.path, s.start_line, s.fqn);
    }
    eprintln!(
        "{} of {} functions are entered by no test (map at {}, target {})",
        unentered.len(),
        total,
        &map.commit[..12.min(map.commit.len())],
        if map.target.is_empty() {
            "unknown"
        } else {
            map.target.as_str()
        }
    );
    if !no_callers.is_empty() {
        eprintln!(
            "{} of them are named nowhere in the workspace except their own definition and their file's tests: check whether they are dead (public API can have callers elsewhere) before writing tests for them",
            no_callers.len()
        );
    }
    if hidden > 0 {
        eprintln!("{hidden} test-scaffolding function(s) not listed; --scaffolding lists them");
    }
    if !map.not_green.is_empty() {
        eprintln!(
            "{} test(s) failed without any mutant and count for nothing; a function only they reach is listed. `map` printed why; the map's JSON (`map --json FILE`) keeps the end of each one's output under `not_green_output`",
            map.not_green.len()
        );
    }
    let mapped: Vec<String> = map
        .sites
        .iter()
        .filter(|s| in_scope(s))
        .map(|s| s.package.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let omitted = sitemap::omitted_test_dependents(&cfg.repo_root, &mapped, &map.test_packages);
    if !omitted.is_empty() {
        eprintln!(
            "{}",
            sitemap::dependent_hint(&cfg.repo_root, &map, &shown, &omitted)
        );
    }
    if let Some(path) = &cfg.json {
        let rows: Vec<serde_json::Value> = shown
            .iter()
            .map(|s| {
                serde_json::json!({
                    "package": s.package, "path": s.path, "line": s.start_line, "fqn": s.fqn,
                    "no_callers": no_callers.contains(&s.site_id),
                    "scaffolding": s.scaffolding,
                })
            })
            .collect();
        crate::util::write_atomic(path, serde_json::to_vec_pretty(&rows)?)?;
    }
    Ok(0)
}

fn cmd_test(common: &cli::CommonOpts, args: cli::TestArgs) -> Result<i32> {
    let (cfg, _) = engine::config_from_cli(common, ConfigExtras::default())?;
    let paths = cfg.paths();
    paths.ensure()?;
    let results = exec::edited::test(
        &cfg,
        &paths,
        &args.mutants,
        args.proptest_cases,
        &args.again,
    )?;
    // Every verdict of this map, not only this run's: a run that continues
    // a stopped one still writes the whole set.
    let state = exec::edited::State::load(&paths)?;
    let mut tested: Vec<(&capture::EditedMutant, &exec::edited::Verdict)> = state
        .records
        .iter()
        .filter_map(|r| match &r.status {
            exec::edited::Status::Tested { verdict, .. } => Some((&r.mutant, verdict)),
            _ => None,
        })
        .collect();
    tested.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    if results.is_empty() {
        if tested.is_empty() {
            eprintln!("no compiled mutants to test; run `cargo quicksilver build` first");
        } else {
            eprintln!(
                "every compiled mutant already has a verdict ({} in all)",
                tested.len()
            );
        }
    }
    if let Some(path) = &cfg.json {
        let map = sitemap::SiteMap::load(&paths)?;
        let rows: Vec<serde_json::Value> = tested
            .iter()
            .map(|(m, v)| {
                serde_json::json!({
                    "id": m.id,
                    "intent": m.intent,
                    "edits": m.edits.iter().map(|e| serde_json::json!({
                        "fqn": e.fqn, "path": e.path, "line": e.line,
                        "original": e.original, "body": e.body,
                    })).collect::<Vec<_>>(),
                    "verdict": v,
                })
            })
            .collect();
        let report = serde_json::json!({
            "commit": map.as_ref().map(|m| m.commit.clone()),
            "target": map.as_ref().map(|m| m.target.clone()),
            "mutants": rows,
        });
        crate::util::write_atomic(path, serde_json::to_vec_pretty(&report)?)?;
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
    let mut prepared = engine::prepare(cfg, args.skip_baseline, false)?;
    if !args.skip_baseline {
        let _ = engine::maybe_preflight(&prepared)?;
    }
    let sample = sample_mutants(&prepared.mutants, args.sample, args.sample_seed);
    eprintln!(
        "audit-selection: {} mutants, each run with the selected tests and with every candidate test",
        sample.len()
    );
    prepared.cfg.resume = false;
    let (selected, _) = engine::run_schema(&prepared, 0, Some(&sample))?;
    let jsonl = prepared.paths.state.join("outcomes.jsonl");
    if jsonl.is_file() {
        let _ = std::fs::rename(&jsonl, prepared.paths.state.join("audit-selected.jsonl"));
    }
    let full_plan = crate::plan::plan_full_suite(
        &sample,
        prepared.selector(),
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
    Ok(if report.lost_kills > 0 { 1 } else { 0 })
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
    // Which functions any test enters, for every mutant, comes from the map
    // of this commit. Check for it before the long preparation.
    let map = exec::edited::map_at(&cfg.paths(), &worktree::head(&cfg.repo_root)?)?;
    let prepared = engine::prepare(cfg, args.skip_baseline, true)?;
    if !args.skip_baseline {
        let _ = engine::maybe_preflight(&prepared)?;
    }
    let sample = sample_mutants(&prepared.mutants, args.sample, args.sample_seed);
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
    let mut report = engine::estimate_from(&prepared, &map, sample.len(), outcomes);
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
