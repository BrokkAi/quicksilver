//! Schema pool + test pool. Test binaries do not change between mutants.

use crate::cargo_cmd::{build_tests_captured, crate_root_source, load_metadata, CapturedBuild};
use crate::config::{Config, TestSelection};
use crate::coupling::CouplingRow;
use crate::exec::build::resolve_binary;
use crate::exec::test::{
    run_mutant_tests_observed, run_mutant_tests_with_env, BinariesManifest, ManifestTest,
    ObservedRun, TestRunPolicy,
};
use crate::exec::{append_outcome, load_resume, ExecResult};
use crate::higher_order::{
    choose_second_order_pairs, choose_triage_pairs, classify_triage,
    implied_first_order_probability, pair_id, summarize_vector, union_tests, RunnableMutant,
    SecondOrderExperiment, SecondOrderReport, TriageFinding, TriageReport,
};
use crate::plan::Plan;
use crate::schema::{
    apply_schema_to_tree, attribute_spans, build_sites, chunk_sites, disable_attributed,
    disable_sites, inject_runtime, lookup_variant, parse_compiler_error_spans, qs_mutant_env,
    qs_mutants_env, restore_files, snapshot_sources, Site,
};
use crate::scratch::{ensure_worker, remove_extra_workers};
use crate::types::{Mutant, Outcome, Paths, PlannedMutant, TestInfo, VerdictKind};
use anyhow::{bail, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};

#[derive(Clone)]
struct SchemaReady {
    planned: PlannedMutant,
    run_dir: PathBuf,
    /// The worker tree that built this schema binary. Tests start in their
    /// package directory under it, so a test that runs `cargo build` (as
    /// trycmd does for clap's examples) compiles the schema source, and the
    /// child programs it starts can see the active mutant.
    tree: PathBuf,
    env: Vec<(String, String)>,
    site_id: u32,
    variant: u32,
    /// Dropped once this mutant has a verdict. The build thread waits for
    /// every token of a batch before it restores the original source.
    _batch: Sender<()>,
}

pub fn execute(
    cfg: &Config,
    paths: &Paths,
    plan: Plan,
    cancel: Arc<AtomicBool>,
    limit: usize,
) -> Result<ExecResult> {
    let mut outcomes = Vec::new();
    let decided = if cfg.resume {
        load_resume(&paths.state.join("outcomes.jsonl"))?
    } else {
        Vec::new()
    };
    let decided_ids: std::collections::HashSet<String> =
        decided.iter().map(|o| o.mutant.id.clone()).collect();
    outcomes.extend(decided);

    let jsonl = paths.state.join("outcomes.jsonl");
    for m in plan.not_covered {
        if decided_ids.contains(&m.id) {
            continue;
        }
        let o = Outcome::not_covered(m);
        append_outcome(&jsonl, &o)?;
        outcomes.push(o);
    }

    let mut planned: Vec<PlannedMutant> = plan
        .batches
        .into_iter()
        .flat_map(|b| b.mutants)
        .filter(|m| !decided_ids.contains(&m.mutant.id))
        .collect();
    if limit > 0 && planned.len() > limit {
        planned.truncate(limit);
    }

    let mut by_pkg: HashMap<String, Vec<PlannedMutant>> = HashMap::new();
    for p in planned {
        by_pkg.entry(p.mutant.package.clone()).or_default().push(p);
    }

    let cargo_invocations = Arc::new(AtomicU64::new(0));
    let runnable = Arc::new(Mutex::new(Vec::<RunnableMutant>::new()));
    let coupling_rows = Arc::new(Mutex::new(Vec::<CouplingRow>::new()));
    let jobs: Vec<(String, Vec<PlannedMutant>)> = by_pkg.into_iter().collect();
    let n_mutants: usize = jobs.iter().map(|(_, v)| v.len()).sum();
    info!(
        "schema exec: {} mutants across {} crates ({} build / {} test jobs)",
        n_mutants,
        jobs.len(),
        cfg.build_jobs,
        cfg.test_jobs
    );

    let test_bound = (4 * cfg.test_jobs).max(4);
    let (job_tx, job_rx) = bounded::<(String, Vec<PlannedMutant>)>(jobs.len().max(1));
    let (ready_tx, ready_rx) = bounded::<SchemaReady>(test_bound);
    let (out_tx, out_rx) = bounded::<Outcome>(test_bound.max(32));
    for j in jobs {
        job_tx.send(j).ok();
    }
    drop(job_tx);

    let cfg = Arc::new(cfg.clone());
    let paths = Arc::new(paths.clone());
    let pb = progress_bar(n_mutants as u64, cfg.quiet);

    std::thread::scope(|scope| {
        for slot in 0..cfg.build_jobs.max(1) {
            let job_rx = job_rx.clone();
            let ready_tx = ready_tx.clone();
            let out_tx = out_tx.clone();
            let cfg = Arc::clone(&cfg);
            let paths = Arc::clone(&paths);
            let cancel = Arc::clone(&cancel);
            let cargo_invocations = Arc::clone(&cargo_invocations);
            let runnable = Arc::clone(&runnable);
            scope.spawn(move || {
                let mut worker: Option<PathBuf> = None;
                while let Ok((package, planned)) = job_rx.recv() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let w = match &worker {
                        Some(w) => w.clone(),
                        None => match ensure_worker(&paths, slot) {
                            Ok(w) => {
                                worker = Some(w.clone());
                                w
                            }
                            Err(err) => {
                                tracing::error!("schema worker {slot} clone failed: {err:#}");
                                return;
                            }
                        },
                    };
                    let planned_for_err = planned.clone();
                    let dispatch = |r: SchemaReady| -> bool {
                        runnable.lock().unwrap().push(RunnableMutant {
                            planned: r.planned.clone(),
                            run_dir: r.run_dir.clone(),
                            site_id: r.site_id,
                            variant: r.variant,
                        });
                        ready_tx.send(r).is_ok()
                    };
                    match run_crate(
                        &cfg,
                        &paths,
                        &w,
                        &package,
                        planned,
                        &cargo_invocations,
                        &cancel,
                        &dispatch,
                    ) {
                        Ok(CrateDone { unviable, keep }) => {
                            for o in unviable {
                                if out_tx.send(o).is_err() {
                                    return;
                                }
                            }
                            let _ = keep;
                        }
                        Err(err) => {
                            tracing::error!("schema crate {package}: {err:#}");
                            for p in planned_for_err {
                                let _ = out_tx.send(Outcome::unviable(
                                    p.mutant,
                                    format!("schema crate {package}: {err:#}"),
                                ));
                            }
                        }
                    }
                }
            });
        }

        for _ in 0..cfg.test_jobs.max(1) {
            let ready_rx: Receiver<SchemaReady> = ready_rx.clone();
            let out_tx: Sender<Outcome> = out_tx.clone();
            let cfg = Arc::clone(&cfg);
            let cancel = Arc::clone(&cancel);
            let coupling_rows = Arc::clone(&coupling_rows);
            scope.spawn(move || {
                while let Ok(ready) = ready_rx.recv() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let policy = mutant_pool_policy(&cfg);
                    let observed = run_mutant_tests_observed(
                        &ready.planned,
                        &ready.run_dir,
                        &ready.tree,
                        &policy,
                        &ready.env,
                        false,
                    );
                    match observed {
                        Ok(run) => {
                            coupling_rows.lock().unwrap().push(row_from_run(
                                ready.planned.mutant.id.clone(),
                                vec![ready.planned.mutant.id.clone()],
                                &ready.planned,
                                &run,
                            ));
                            let _ = out_tx.send(run.outcome);
                        }
                        Err(err) => {
                            let _ = out_tx.send(Outcome::unviable(
                                ready.planned.mutant.clone(),
                                err.to_string(),
                            ));
                        }
                    }
                }
            });
        }

        drop(ready_tx);
        drop(out_tx);
        drop(ready_rx);

        let collected = Arc::new(Mutex::new(Vec::<Outcome>::new()));
        while let Ok(o) = out_rx.recv() {
            let _ = append_outcome(&jsonl, &o);
            pb.inc(1);
            collected.lock().unwrap().push(o);
        }
        outcomes.extend(collected.lock().unwrap().clone());
    });

    pb.finish_and_clear();
    remove_extra_workers(&paths);
    let mut evidence_rows = coupling_rows.lock().unwrap().clone();
    let triage = if cfg.triage {
        let (report, rows) =
            run_triage(&cfg, &paths.baseline, &outcomes, &runnable.lock().unwrap())?;
        evidence_rows.extend(rows);
        Some(report)
    } else {
        None
    };
    let second_order = if cfg.second_order_pairs > 0 {
        let (report, rows) =
            run_second_order(&cfg, &paths.baseline, &outcomes, &runnable.lock().unwrap())?;
        evidence_rows.extend(rows);
        Some(report)
    } else {
        None
    };
    // QS_KEEP_RUNS=1 keeps staged schema binaries for debugging.
    if std::env::var_os("QS_KEEP_RUNS").is_none() {
        let _ = fs::remove_dir_all(paths.runs.join("schema"));
    }

    Ok(ExecResult {
        cargo_invocations: cargo_invocations.load(Ordering::SeqCst),
        outcomes,
        triage,
        second_order,
        coupling_rows: evidence_rows,
        coupling: None,
    })
}

struct CrateDone {
    unviable: Vec<Outcome>,
    keep: Vec<PathBuf>,
}

fn run_triage(
    cfg: &Config,
    baseline: &Path,
    outcomes: &[Outcome],
    runnable: &[RunnableMutant],
) -> Result<(TriageReport, Vec<CouplingRow>)> {
    let pairs = choose_triage_pairs(runnable, outcomes, cfg.triage_neighbors);
    let survivors = outcomes
        .iter()
        .filter(|outcome| outcome.kind == VerdictKind::Missed)
        .count();
    info!(
        "triage: {} survivor(s), {} survivor/control experiment(s)",
        survivors,
        pairs.len()
    );
    let policy = TestRunPolicy {
        floor: cfg.timeout_floor.max(Duration::from_secs(30)),
        slowdown: SCHEMA_SLOWDOWN,
        confirm: true,
    };
    let mut experiments = Vec::new();
    let mut rows = Vec::new();
    for pair in pairs {
        let tests = union_tests(&pair.survivor.planned, &pair.control.planned);
        let survivor_plan = PlannedMutant {
            mutant: pair.survivor.planned.mutant.clone(),
            tests: tests.clone(),
            estimated_cost_secs: 0.0,
        };
        let control_plan = PlannedMutant {
            mutant: pair.control.planned.mutant.clone(),
            tests: tests.clone(),
            estimated_cost_secs: 0.0,
        };
        let pair_plan = PlannedMutant {
            mutant: pair.survivor.planned.mutant.clone(),
            tests,
            estimated_cost_secs: 0.0,
        };
        let survivor_env = vec![qs_mutant_env(pair.survivor.site_id, pair.survivor.variant)];
        let control_env = vec![qs_mutant_env(pair.control.site_id, pair.control.variant)];
        let pair_env = vec![qs_mutants_env(&[
            (pair.survivor.site_id, pair.survivor.variant),
            (pair.control.site_id, pair.control.variant),
        ])];
        let survivor_run = run_mutant_tests_observed(
            &survivor_plan,
            &pair.survivor.run_dir,
            baseline,
            &policy,
            &survivor_env,
            true,
        )?;
        let control_run = run_mutant_tests_observed(
            &control_plan,
            &pair.control.run_dir,
            baseline,
            &policy,
            &control_env,
            true,
        )?;
        let pair_run = run_mutant_tests_observed(
            &pair_plan,
            &pair.survivor.run_dir,
            baseline,
            &policy,
            &pair_env,
            true,
        )?;
        let (classification, survivor, control, pair_summary) = classify_triage(
            &survivor_run.observations,
            survivor_run.complete,
            &control_run.observations,
            control_run.complete,
            &pair_run.observations,
            pair_run.complete,
        );
        let survivor_id = pair.survivor.planned.mutant.id.clone();
        let control_id = pair.control.planned.mutant.id.clone();
        rows.push(row_from_run(
            survivor_id.clone(),
            vec![survivor_id.clone()],
            &survivor_plan,
            &survivor_run,
        ));
        rows.push(row_from_run(
            control_id.clone(),
            vec![control_id.clone()],
            &control_plan,
            &control_run,
        ));
        rows.push(row_from_run(
            pair_id(&survivor_id, &control_id),
            vec![survivor_id.clone(), control_id.clone()],
            &pair_plan,
            &pair_run,
        ));
        experiments.push(TriageFinding {
            survivor_id,
            survivor_fqn: pair.survivor.planned.mutant.fqn.clone(),
            control_id,
            control_fqn: pair.control.planned.mutant.fqn.clone(),
            classification,
            survivor,
            control,
            pair: pair_summary,
        });
    }
    Ok((
        TriageReport {
            survivors,
            experiments,
        },
        rows,
    ))
}

fn run_second_order(
    cfg: &Config,
    baseline: &Path,
    outcomes: &[Outcome],
    runnable: &[RunnableMutant],
) -> Result<(SecondOrderReport, Vec<CouplingRow>)> {
    let (eligible_mutants, pairs) =
        choose_second_order_pairs(runnable, outcomes, cfg.second_order_pairs, cfg.sample_seed);
    info!(
        "second-order estimate: {} pair(s) from {} eligible mutant(s)",
        pairs.len(),
        eligible_mutants
    );
    let policy = TestRunPolicy {
        floor: cfg.timeout_floor.max(Duration::from_secs(30)),
        slowdown: SCHEMA_SLOWDOWN,
        confirm: true,
    };
    let mut experiments = Vec::new();
    let mut rows = Vec::new();
    for pair in pairs {
        let tests = union_tests(&pair.left.planned, &pair.right.planned);
        let plan = PlannedMutant {
            mutant: pair.left.planned.mutant.clone(),
            tests,
            estimated_cost_secs: 0.0,
        };
        let env = vec![qs_mutants_env(&[
            (pair.left.site_id, pair.left.variant),
            (pair.right.site_id, pair.right.variant),
        ])];
        let run =
            run_mutant_tests_observed(&plan, &pair.left.run_dir, baseline, &policy, &env, true)?;
        let vector = summarize_vector(&run.observations, run.complete);
        let left = pair.left.planned.mutant.id.clone();
        let right = pair.right.planned.mutant.id.clone();
        let activation_id = pair_id(&left, &right);
        rows.push(row_from_run(
            activation_id.clone(),
            vec![left.clone(), right.clone()],
            &plan,
            &run,
        ));
        experiments.push(SecondOrderExperiment {
            pair_id: activation_id,
            mutant_ids: [left, right],
            vector,
        });
    }
    let completed_pairs = experiments
        .iter()
        .filter(|experiment| experiment.vector.complete && experiment.vector.comparable)
        .count();
    let caught_pairs = experiments
        .iter()
        .filter(|experiment| {
            experiment.vector.complete
                && experiment.vector.comparable
                && !experiment.vector.killed_test_ids.is_empty()
        })
        .count();
    let higher_order_kill_rate = if completed_pairs == 0 {
        0.0
    } else {
        caught_pairs as f64 / completed_pairs as f64
    };
    Ok((SecondOrderReport {
        requested_pairs: cfg.second_order_pairs,
        eligible_mutants,
        completed_pairs,
        caught_pairs,
        higher_order_kill_rate,
        implied_first_order_kill_probability: implied_first_order_probability(
            higher_order_kill_rate,
        ),
        experiments,
        assumption: "p = 1 - sqrt(1 - h) assumes independently killable, equally distributed first-order constituents within this paired sample; it supplements rather than replaces first-order verdicts".into(),
    }, rows))
}

fn row_from_run(
    activation_id: String,
    mutant_ids: Vec<String>,
    planned: &PlannedMutant,
    run: &ObservedRun,
) -> CouplingRow {
    CouplingRow::new(
        activation_id,
        mutant_ids,
        planned
            .tests
            .iter()
            .map(|test| {
                if test.id.is_empty() {
                    test.name.clone()
                } else {
                    test.id.clone()
                }
            })
            .collect(),
        run.observations.clone(),
        run.complete,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_crate(
    cfg: &Config,
    paths: &Paths,
    worker: &Path,
    package: &str,
    planned: Vec<PlannedMutant>,
    cargo_invocations: &AtomicU64,
    cancel: &AtomicBool,
    dispatch: &dyn Fn(SchemaReady) -> bool,
) -> Result<CrateDone> {
    let mutants: Vec<Mutant> = planned.iter().map(|p| p.mutant.clone()).collect();
    let (sites, prefiltered) = build_sites(&mutants)?;
    let mut unviable = Vec::new();
    let by_id: HashMap<String, PlannedMutant> = planned
        .iter()
        .map(|p| (p.mutant.id.clone(), p.clone()))
        .collect();
    for m in prefiltered {
        unviable.push(Outcome::unviable_skipped(
            m.clone(),
            format!(
                "pre-filtered: return kind `{}` is structurally unviable under schemata",
                m.return_kind
            ),
        ));
    }
    if sites.is_empty() {
        return Ok(CrateDone {
            unviable,
            keep: Vec::new(),
        });
    }

    if cancel.load(Ordering::Relaxed) {
        return Ok(CrateDone {
            unviable,
            keep: Vec::new(),
        });
    }

    let target = worker.join("target");
    let meta = load_metadata(worker)?;
    let crate_root_abs =
        crate_root_source(&meta, package).with_context(|| format!("crate root for {package}"))?;
    let crate_root = rebase_to_worker(&crate_root_abs, &paths.baseline, worker);
    let crate_root_rel = crate::util::rel_slash(
        crate_root
            .strip_prefix(worker)
            .unwrap_or(crate_root.as_path()),
    );

    // A crate whose mutants have no candidate tests cannot be judged; build
    // nothing and say so rather than fail the null-schema gate on 0 tests.
    if planned.iter().all(|p| p.tests.is_empty()) {
        let decided: BTreeSet<String> = unviable.iter().map(|o| o.mutant.id.clone()).collect();
        for p in &planned {
            if !decided.contains(&p.mutant.id) {
                unviable.push(Outcome::not_covered(p.mutant.clone()));
            }
        }
        return Ok(CrateDone {
            unviable,
            keep: Vec::new(),
        });
    }

    let mut packages = vec![package.to_string()];
    for p in &planned {
        for t in &p.tests {
            if !packages.iter().any(|x| x == &t.package) {
                packages.push(t.package.clone());
            }
        }
    }

    let batches = chunk_sites(sites, cfg.schema_batch_size);
    let mut keep = Vec::new();

    for (bi, batch) in batches.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let snap = snapshot_sources(worker, &batch, &crate_root_rel)?;
        let isolated = isolate(
            cfg,
            worker,
            &target,
            package,
            &packages,
            &snap,
            &crate_root,
            batch,
            cargo_invocations,
        )?;
        for (id, msg) in isolated.unviable {
            if let Some(p) = by_id.get(&id) {
                unviable.push(Outcome::unviable(p.mutant.clone(), msg));
            }
        }
        if isolated.sites.is_empty() {
            restore_files(worker, &snap)?;
            continue;
        }
        let run_dir = paths.runs.join("schema").join(format!("{package}-{bi}"));
        stage_binaries(
            cfg,
            &target,
            &run_dir,
            &planned,
            &isolated.sites,
            &isolated.executables,
        )?;
        // Gates, recording and mutant runs all start tests under the worker,
        // which holds this batch's schema source until every mutant of the
        // batch has a verdict.
        null_schema_gate(cfg, &run_dir, worker, &planned)?;
        identity_gate(cfg, &run_dir, worker, &isolated.sites, &by_id)?;
        let recording = if cfg.test_selection == TestSelection::Sites {
            Some(record_sites(
                cfg,
                &run_dir,
                worker,
                &planned,
                &isolated.sites,
            )?)
        } else {
            None
        };
        let (batch_tx, batch_rx) = crossbeam_channel::unbounded::<()>();
        let mut batch_ready = Vec::new();
        for p in &planned {
            if let Some((site, variant)) = lookup_variant(&isolated.sites, &p.mutant.id) {
                let mut selected = p.clone();
                if let Some(recording) = &recording {
                    selected
                        .tests
                        .retain(|t| recording.entered(t, site.site_id));
                    for t in &mut selected.tests {
                        if let Some(secs) = recording.recorded_secs(t) {
                            t.duration_secs = secs;
                        }
                    }
                    if selected.tests.is_empty() {
                        unviable.push(Outcome::not_covered(p.mutant.clone()));
                        continue;
                    }
                    selected.estimated_cost_secs =
                        selected.tests.iter().map(|t| t.duration_secs).sum();
                }
                batch_ready.push(SchemaReady {
                    planned: selected,
                    run_dir: run_dir.clone(),
                    tree: worker.to_path_buf(),
                    env: vec![qs_mutant_env(site.site_id, variant)],
                    site_id: site.site_id,
                    variant,
                    _batch: batch_tx.clone(),
                });
            }
        }
        drop(batch_tx);
        for r in batch_ready {
            if !dispatch(r) {
                break;
            }
        }
        wait_for_batch(&batch_rx, cancel);
        keep.push(run_dir);
        restore_files(worker, &snap)?;
    }

    Ok(CrateDone { unviable, keep })
}

/// Block until every mutant of a batch has a verdict, which is when the last
/// `SchemaReady::_batch` sender is dropped. A cancelled run stops waiting:
/// test threads stop taking work, so queued mutants would never finish.
fn wait_for_batch(batch: &Receiver<()>, cancel: &AtomicBool) {
    loop {
        match batch.recv_timeout(Duration::from_millis(100)) {
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) if cancel.load(Ordering::Relaxed) => {
                return
            }
            _ => {}
        }
    }
}

struct Isolated {
    sites: Vec<Site>,
    unviable: Vec<(String, String)>,
    executables: Vec<crate::cargo_cmd::BuiltExe>,
}

fn cargo_build(
    cfg: &Config,
    worker: &Path,
    target: &Path,
    packages: &[String],
    cargo_invocations: &AtomicU64,
) -> Result<CapturedBuild> {
    cargo_invocations.fetch_add(1, Ordering::SeqCst);
    build_tests_captured(worker, target, cfg, packages, false)
}

/// Attribution rounds compile only `mutated`. The prologue never changes a
/// signature, so no dependent crate can newly fail to compile. `-> impl Trait`
/// is pre-filtered before we get here. The full `test_packages` list is built
/// once, after the mutated crate is clean, to produce test binaries.
fn finalize_test_binaries(
    cfg: &Config,
    worker: &Path,
    target: &Path,
    mutated: &str,
    test_packages: &[String],
    crate_only: CapturedBuild,
    cargo_invocations: &AtomicU64,
) -> Result<Vec<crate::cargo_cmd::BuiltExe>> {
    let only_mutated =
        test_packages.is_empty() || (test_packages.len() == 1 && test_packages[0] == mutated);
    if only_mutated {
        return Ok(crate_only.executables);
    }
    let full = cargo_build(cfg, worker, target, test_packages, cargo_invocations)?;
    if !full.success {
        bail!(
            "schema test packages failed to compile against a clean mutated crate:\n{}",
            compiler_errors(&full.stdout, &full.stderr)
        );
    }
    Ok(full.executables)
}

#[allow(clippy::too_many_arguments)]
fn isolate(
    cfg: &Config,
    worker: &Path,
    target: &Path,
    mutated: &str,
    test_packages: &[String],
    originals: &HashMap<String, Vec<u8>>,
    crate_root: &Path,
    mut sites: Vec<Site>,
    cargo_invocations: &AtomicU64,
) -> Result<Isolated> {
    let mutated_pkgs = [mutated.to_string()];
    let mut unviable = Vec::new();
    let max_rounds = cfg.schema_max_rounds.max(1);
    for round in 1..=max_rounds {
        restore_files(worker, originals)?;
        if sites.is_empty() {
            inject_runtime(crate_root)?;
            let built = cargo_build(cfg, worker, target, &mutated_pkgs, cargo_invocations)?;
            if !built.success {
                bail!(
                    "schema runtime itself does not compile:\n{}",
                    tail_utf8(&built.stderr)
                );
            }
            return Ok(Isolated {
                sites,
                unviable,
                executables: Vec::new(),
            });
        }
        let applied = apply_schema_to_tree(worker, &sites, crate_root)?;
        let built = cargo_build(cfg, worker, target, &mutated_pkgs, cargo_invocations)?;
        if built.success {
            let executables = finalize_test_binaries(
                cfg,
                worker,
                target,
                mutated,
                test_packages,
                built,
                cargo_invocations,
            )?;
            return Ok(Isolated {
                sites,
                unviable,
                executables,
            });
        }
        let spans = parse_compiler_error_spans(&built.stdout);
        let attr = attribute_spans(&applied, &spans, worker);
        if attr.is_empty() {
            warn!(
                "schema isolation round {round}: no attributable rustc span; bisecting this crate"
            );
            let offenders = bisect_offenders(
                cfg,
                worker,
                target,
                mutated,
                originals,
                crate_root,
                &sites,
                cargo_invocations,
            )?;
            let disabled = disable_sites(&mut sites, &offenders);
            if disabled.is_empty() {
                bail!(
                    "schema build failed with no attributable site and bisection found none:\n{}",
                    tail_utf8(&built.stderr)
                );
            }
            for (id, why) in disabled {
                unviable.push((id, format!("bisection (round {round}): {why}")));
            }
            continue;
        }
        let disabled = disable_attributed(&mut sites, &attr);
        if disabled.is_empty() {
            bail!(
                "schema isolation attributed diagnostics but disabled nothing:\n{}",
                tail_utf8(&built.stderr)
            );
        }
        for (id, why) in disabled {
            unviable.push((id, format!("rustc diagnostic (round {round}): {why}")));
        }
        let _ = applied;
    }

    restore_files(worker, originals)?;
    if !sites.is_empty() {
        let applied = apply_schema_to_tree(worker, &sites, crate_root)?;
        let built = cargo_build(cfg, worker, target, &mutated_pkgs, cargo_invocations)?;
        if !built.success {
            warn!("schema still failing after {max_rounds} diagnostic rounds; bisecting");
            let offenders = bisect_offenders(
                cfg,
                worker,
                target,
                mutated,
                originals,
                crate_root,
                &sites,
                cargo_invocations,
            )?;
            let disabled = disable_sites(&mut sites, &offenders);
            for (id, why) in disabled {
                unviable.push((id, format!("bisection (after max rounds): {why}")));
            }
            restore_files(worker, originals)?;
            if !sites.is_empty() {
                let _ = apply_schema_to_tree(worker, &sites, crate_root)?;
                let last = cargo_build(cfg, worker, target, &mutated_pkgs, cargo_invocations)?;
                if !last.success {
                    bail!(
                        "schema build still failing after bisection:\n{}",
                        tail_utf8(&last.stderr)
                    );
                }
                let executables = finalize_test_binaries(
                    cfg,
                    worker,
                    target,
                    mutated,
                    test_packages,
                    last,
                    cargo_invocations,
                )?;
                return Ok(Isolated {
                    sites,
                    unviable,
                    executables,
                });
            }
        } else {
            let executables = finalize_test_binaries(
                cfg,
                worker,
                target,
                mutated,
                test_packages,
                built,
                cargo_invocations,
            )?;
            return Ok(Isolated {
                sites,
                unviable,
                executables,
            });
        }
        let _ = applied;
    }
    Ok(Isolated {
        sites,
        unviable,
        executables: Vec::new(),
    })
}

#[allow(clippy::too_many_arguments)]
fn bisect_offenders(
    cfg: &Config,
    worker: &Path,
    target: &Path,
    mutated: &str,
    originals: &HashMap<String, Vec<u8>>,
    crate_root: &Path,
    sites: &[Site],
    cargo_invocations: &AtomicU64,
) -> Result<BTreeSet<u32>> {
    let mutated_pkgs = [mutated.to_string()];
    restore_files(worker, originals)?;
    inject_runtime(crate_root)?;
    let rt_only = cargo_build(cfg, worker, target, &mutated_pkgs, cargo_invocations)?;
    if !rt_only.success {
        bail!(
            "schema __qs_rt does not compile (bisection aborted):\n{}",
            tail_utf8(&rt_only.stderr)
        );
    }

    let mut remaining = sites.to_vec();
    let mut offenders = BTreeSet::new();
    while !remaining.is_empty() {
        restore_files(worker, originals)?;
        apply_schema_to_tree(worker, &remaining, crate_root)?;
        let built = cargo_build(cfg, worker, target, &mutated_pkgs, cargo_invocations)?;
        if built.success {
            break;
        }
        if remaining.len() == 1 {
            offenders.insert(remaining[0].site_id);
            remaining.clear();
            continue;
        }
        let mid = remaining.len() / 2;
        let left = remaining[..mid].to_vec();
        restore_files(worker, originals)?;
        apply_schema_to_tree(worker, &left, crate_root)?;
        let left_built = cargo_build(cfg, worker, target, &mutated_pkgs, cargo_invocations)?;
        if !left_built.success {
            remaining = left;
        } else {
            remaining = remaining[mid..].to_vec();
        }
    }
    Ok(offenders)
}

pub(crate) fn union_selected_tests(planned: &[PlannedMutant]) -> Vec<TestInfo> {
    let mut tests: Vec<TestInfo> = Vec::new();
    for p in planned {
        for t in &p.tests {
            if !tests.iter().any(|x| x.id == t.id) {
                tests.push(t.clone());
            }
        }
    }
    tests
}

/// Smoke the schema binary. The union of selected tests for a crate sample
/// on bifrost-core is ~10k reverse-dep tests; running them all serially
/// with `--exact` is a full-suite gate, not a schema check.
pub(crate) const NULL_GATE_TESTS: usize = 8;

/// Schema binaries carry a per-call dispatch check in every instrumented
/// function, so 3× the green duration is not enough budget.
pub(crate) const SCHEMA_SLOWDOWN: f64 = 3.0;

/// Budget for mutant runs. Coverage selection only has green durations from
/// the baseline binary, so it pads them for the schema binary and keeps a
/// 30 s floor. Sites selection timed every selected test under this schema
/// binary with no mutant active, so 3x that time over the ordinary floor is
/// enough; the 30 s floor made each hanging mutant cost 30 s per run on tests
/// that take milliseconds.
fn mutant_pool_policy(cfg: &Config) -> TestRunPolicy {
    match cfg.test_selection {
        TestSelection::Sites => TestRunPolicy {
            floor: cfg.timeout_floor,
            slowdown: 1.0,
            confirm: true,
        },
        TestSelection::Coverage => TestRunPolicy {
            floor: cfg.timeout_floor.max(Duration::from_secs(30)),
            slowdown: SCHEMA_SLOWDOWN,
            confirm: true,
        },
    }
}

pub(crate) fn cheapest_tests(mut tests: Vec<TestInfo>, n: usize) -> Vec<TestInfo> {
    tests.sort_by(|a, b| {
        a.duration_secs
            .partial_cmp(&b.duration_secs)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });
    tests.truncate(n.max(1));
    tests
}

pub(crate) fn null_schema_gate(
    cfg: &Config,
    run_dir: &Path,
    baseline: &Path,
    planned: &[PlannedMutant],
) -> Result<()> {
    let dummy = PlannedMutant {
        mutant: planned
            .first()
            .map(|p| p.mutant.clone())
            .unwrap_or_else(|| Mutant {
                id: "null-schema".into(),
                package: String::new(),
                path: String::new(),
                fqn: String::new(),
                start_byte: 0,
                end_byte: 0,
                start_line: 1,
                end_line: 1,
                body_start_byte: 0,
                body_end_byte: 0,
                replacement: String::new(),
                return_kind: String::new(),
            }),
        tests: {
            let union = union_selected_tests(planned);
            info!(
                "null-schema gate: {} of {} unique selected tests (cheapest)",
                union.len().min(NULL_GATE_TESTS),
                union.len()
            );
            cheapest_tests(union, NULL_GATE_TESTS)
        },
        estimated_cost_secs: 0.0,
    };
    // Schema binaries carry a prologue in every site. 3× green duration is
    // not enough; a 0.4s test timed out at 5s and failed the whole crate.
    let floor = cfg.timeout_floor.max(Duration::from_secs(30));
    let outcome = run_mutant_tests_with_env(&dummy, run_dir, baseline, floor, &[])?;
    match outcome.kind {
        VerdictKind::Caught | VerdictKind::Timeout | VerdictKind::Errored => {
            bail!(
                "null-schema gate failed: staged tests are not green with QS_MUTANT unset ({:?} {})",
                outcome.kind,
                outcome
                    .test
                    .as_deref()
                    .or(outcome.message.as_deref())
                    .unwrap_or("")
            );
        }
        _ => Ok(()),
    }
}

/// One run of every candidate test under the schema binary with no mutant
/// active, recording which sites each test enters. This replaces LLVM
/// Stage A and Stage B: a mutant's tests are exactly the green tests that
/// entered its site, and a site no green test entered is NotCovered.
fn record_sites(
    cfg: &Config,
    run_dir: &Path,
    baseline: &Path,
    planned: &[PlannedMutant],
    sites: &[Site],
) -> Result<crate::exec::test::SiteRecording> {
    let Some(first) = planned.first() else {
        return Ok(Default::default());
    };
    let union = PlannedMutant {
        mutant: first.mutant.clone(),
        tests: union_selected_tests(planned),
        estimated_cost_secs: 0.0,
    };
    let policy = TestRunPolicy {
        floor: cfg.timeout_floor.max(Duration::from_secs(30)),
        slowdown: SCHEMA_SLOWDOWN,
        confirm: false,
    };
    let recording =
        crate::exec::test::record_site_hits(&union, run_dir, baseline, &policy, cfg.test_jobs)?;
    let site_ids: BTreeSet<u32> = sites.iter().map(|s| s.site_id).collect();
    let entered: BTreeSet<u32> = recording
        .sites_by_test
        .values()
        .flat_map(|s| s.iter().copied())
        .filter(|s| site_ids.contains(s))
        .collect();
    info!(
        "site recording: {} tests in {:.1}s, {} not green; {} of {} sites entered",
        recording.sites_by_test.len() + recording.not_green.len(),
        recording.wall_secs,
        recording.not_green.len(),
        entered.len(),
        site_ids.len()
    );
    for (test, why) in recording.not_green.iter().take(8) {
        warn!("  not green under the schema binary, selects nothing: {test}: {why}");
    }
    if !recording.sites_by_test.is_empty() && entered.is_empty() {
        // Legitimate when no test calls this crate's mutated functions. A
        // recorder that cannot open its file panics, which fails the test
        // and lands it in not_green instead.
        warn!(
            "site recording: {} green tests entered none of {} sites; every mutant in this batch is NotCovered",
            recording.sites_by_test.len(),
            site_ids.len()
        );
    }
    Ok(recording)
}

fn identity_gate(
    cfg: &Config,
    run_dir: &Path,
    baseline: &Path,
    sites: &[Site],
    by_id: &HashMap<String, PlannedMutant>,
) -> Result<()> {
    let mut sample: Vec<&Site> = sites.iter().collect();
    sample.sort_by(|a, b| a.fqn.cmp(&b.fqn));
    let n = if cfg.identity_sample == 0 {
        sample.len()
    } else {
        cfg.identity_sample.min(sample.len())
    };
    sample.truncate(n);
    for site in sample {
        let mut tests: Vec<TestInfo> = Vec::new();
        for v in &site.variants {
            if let Some(p) = by_id.get(&v.mutant_id) {
                for t in &p.tests {
                    if !tests.iter().any(|x| x.id == t.id) {
                        tests.push(t.clone());
                    }
                }
            }
        }
        if tests.is_empty() {
            continue;
        }
        tests = cheapest_tests(tests, NULL_GATE_TESTS);
        let dummy = PlannedMutant {
            mutant: by_id
                .get(&site.variants[0].mutant_id)
                .map(|p| p.mutant.clone())
                .unwrap_or_else(|| Mutant {
                    id: site.fqn.clone(),
                    package: site.package.clone(),
                    path: site.path.clone(),
                    fqn: site.fqn.clone(),
                    start_byte: site.fn_start_byte,
                    end_byte: site.body_end_byte,
                    start_line: 1,
                    end_line: 1,
                    body_start_byte: site.body_start_byte,
                    body_end_byte: site.body_end_byte,
                    replacement: String::new(),
                    return_kind: String::new(),
                }),
            tests,
            estimated_cost_secs: 0.0,
        };
        let env = vec![qs_mutant_env(site.site_id, site.identity_variant)];
        let floor = cfg.timeout_floor.max(Duration::from_secs(30));
        let outcome = run_mutant_tests_with_env(&dummy, run_dir, baseline, floor, &env)?;
        match outcome.kind {
            crate::types::VerdictKind::Caught
            | crate::types::VerdictKind::Timeout
            | crate::types::VerdictKind::Errored => {
                bail!(
                    "identity-mutant gate failed for {} (QS_MUTANT={}:{}): {:?} {}",
                    site.fqn,
                    site.site_id,
                    site.identity_variant,
                    outcome.kind,
                    outcome
                        .test
                        .as_deref()
                        .or(outcome.message.as_deref())
                        .unwrap_or("")
                );
            }
            _ => {}
        }
    }
    Ok(())
}

fn stage_binaries(
    cfg: &Config,
    target: &Path,
    run_dir: &Path,
    planned: &[PlannedMutant],
    sites: &[Site],
    executables: &[crate::cargo_cmd::BuiltExe],
) -> Result<()> {
    let live: BTreeSet<&str> = sites
        .iter()
        .flat_map(|s| s.variants.iter().map(|v| v.mutant_id.as_str()))
        .collect();
    let mut tests: Vec<&TestInfo> = Vec::new();
    for p in planned {
        if !live.contains(p.mutant.id.as_str()) {
            continue;
        }
        for t in &p.tests {
            if !tests.iter().any(|x| x.id == t.id) {
                tests.push(t);
            }
        }
    }
    stage_tests(cfg, target, run_dir, &tests, executables)
}

/// Copy the test binaries `tests` need from `target` into `run_dir` and
/// write the manifest that names each test's binary and working directory.
pub(crate) fn stage_tests(
    cfg: &Config,
    target: &Path,
    run_dir: &Path,
    tests: &[&TestInfo],
    executables: &[crate::cargo_cmd::BuiltExe],
) -> Result<()> {
    if let Some(parent) = run_dir.parent() {
        fs::create_dir_all(parent)?;
        crate::scratch::check_free_space(parent, cfg.max_scratch)?;
    }
    if run_dir.exists() {
        let _ = fs::remove_dir_all(run_dir);
    }
    fs::create_dir_all(run_dir)?;
    let mut built: Vec<crate::cargo_cmd::BuiltExe> = executables.to_vec();
    if built.is_empty() {
        built.extend(
            scan_test_exes(target)
                .into_values()
                .map(|path| crate::cargo_cmd::BuiltExe {
                    path,
                    manifest_dir: None,
                }),
        );
    }
    // One staged copy per distinct binary: tests share harnesses, and a copy
    // per test staged 1,380 copies of clap's test binaries.
    let mut staged: HashMap<PathBuf, String> = HashMap::new();
    let mut manifest_tests = Vec::new();
    for t in tests.iter() {
        let src = resolve_binary(target, t, &built)?;
        let dest_name = match staged.get(&src) {
            Some(name) => name.clone(),
            None => {
                let dest_name = format!("bin{}", staged.len());
                let dest = run_dir.join(&dest_name);
                // A copy, not a link into `target/`: mbx may collect the
                // managed target, and later builds rewrite it.
                reflink_copy::reflink_or_copy(&src, &dest)
                    .with_context(|| format!("stage {} -> {}", src.display(), dest.display()))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perm = fs::metadata(&dest)?.permissions();
                    perm.set_mode(0o755);
                    fs::set_permissions(&dest, perm)?;
                }
                staged.insert(src.clone(), dest_name.clone());
                dest_name
            }
        };
        manifest_tests.push(ManifestTest {
            name: t.name.clone(),
            binary: dest_name,
            cwd_rel: t.cwd_rel.clone(),
            id: t.id.clone(),
        });
    }
    let manifest = BinariesManifest {
        mutant_id: "schema".into(),
        tests: manifest_tests,
    };
    fs::write(
        run_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

fn scan_test_exes(target: &Path) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    for profile in ["debug"] {
        let deps = target.join(profile).join("deps");
        let Ok(rd) = fs::read_dir(&deps) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.contains('.') {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let Ok(meta) = e.metadata() else { continue };
                if meta.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            out.insert(name.into_owned(), p);
        }
    }
    out
}

pub(crate) fn rebase_to_worker(path: &Path, baseline: &Path, worker: &Path) -> PathBuf {
    if let Ok(rel) = path.strip_prefix(worker) {
        return worker.join(rel);
    }
    if let Ok(rel) = path.strip_prefix(baseline) {
        return worker.join(rel);
    }
    path.to_path_buf()
}

/// The first rendered rustc errors from a `--message-format=json` build.
/// With JSON output, diagnostics are on stdout and stderr only has cargo's
/// "could not compile" summary, so a stderr tail hides the actual error.
pub(crate) fn compiler_errors(stdout: &[u8], stderr: &[u8]) -> String {
    let mut out = String::new();
    for line in stdout.split(|b| *b == b'\n') {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        let msg = &v["message"];
        if msg.get("level").and_then(|l| l.as_str()) != Some("error") {
            continue;
        }
        if let Some(rendered) = msg.get("rendered").and_then(|r| r.as_str()) {
            out.push_str(rendered);
            if out.len() > 6000 {
                break;
            }
        }
    }
    out.push_str(&tail_utf8(stderr));
    out
}

pub(crate) fn tail_utf8(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let n = s.len().saturating_sub(4000);
    s[n..].to_string()
}

fn progress_bar(n: u64, quiet: bool) -> ProgressBar {
    if quiet || n == 0 {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new(n);
    let _ = pb.set_style(
        ProgressStyle::with_template(
            "{bar:40.cyan/blue} {pos}/{len} {msg} elapsed {elapsed_precise} eta {eta}",
        )
        .unwrap_or_else(|_| ProgressStyle::default_bar()),
    );
    pb
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Mutant;

    fn mutant(id: &str) -> Mutant {
        Mutant {
            id: id.into(),
            package: "p".into(),
            path: "p/src/lib.rs".into(),
            fqn: "p::f".into(),
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

    fn testinfo(id: &str, dur: f64) -> TestInfo {
        TestInfo {
            id: id.into(),
            name: id.into(),
            package: "p".into(),
            binary_id: "p".into(),
            binary_name: "p".into(),
            binary_rel: "deps/p".into(),
            cwd_rel: "p".into(),
            duration_secs: dur,
        }
    }

    #[test]
    fn null_gate_uses_cheapest_tests_not_the_whole_union() {
        let planned = vec![
            PlannedMutant {
                mutant: mutant("a"),
                tests: vec![testinfo("t1", 20.0), testinfo("t2", 10.0)],
                estimated_cost_secs: 0.0,
            },
            PlannedMutant {
                mutant: mutant("b"),
                tests: vec![testinfo("t2", 10.0), testinfo("t3", 0.4)],
                estimated_cost_secs: 0.0,
            },
        ];
        let union = union_selected_tests(&planned);
        assert_eq!(union.len(), 3);
        let gate = cheapest_tests(union, 1);
        assert_eq!(gate.len(), 1);
        assert_eq!(gate[0].id, "t3");
        assert!((gate[0].duration_secs - 0.4).abs() < 1e-9);
    }
}
