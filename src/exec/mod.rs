use crate::config::Config;
use crate::exec::build::{walk_batch, BuiltMutant};
use crate::exec::test::{cleanup_run, run_mutant_tests};
use crate::plan::Plan;
use crate::scratch::{ensure_worker, CloneBackend};
use crate::types::{Outcome, Paths};
use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tracing::info;

pub mod build;
pub mod process;
pub mod schema;
pub mod test;

pub struct ExecResult {
    pub outcomes: Vec<Outcome>,
    pub builds: u64,
}

pub fn execute(
    cfg: &Config,
    paths: &Paths,
    backend: Arc<dyn CloneBackend>,
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
    for o in &decided {
        outcomes.push(o.clone());
    }

    let mut not_covered = Vec::new();
    for m in plan.not_covered {
        if decided_ids.contains(&m.id) {
            continue;
        }
        let o = Outcome::not_covered(m);
        append_outcome(&paths.state.join("outcomes.jsonl"), &o)?;
        not_covered.push(o);
    }
    outcomes.extend(not_covered);

    let mut batches = plan.batches;
    if limit > 0 {
        let mut remaining = limit;
        for b in &mut batches {
            if remaining == 0 {
                b.mutants.clear();
                continue;
            }
            if b.mutants.len() > remaining {
                b.mutants.truncate(remaining);
            }
            remaining = remaining.saturating_sub(b.mutants.len());
        }
        batches.retain(|b| !b.mutants.is_empty());
    }
    for b in &mut batches {
        b.mutants.retain(|m| !decided_ids.contains(&m.mutant.id));
    }
    batches.retain(|b| !b.mutants.is_empty());

    let n_mutants: usize = batches.iter().map(|b| b.mutants.len()).sum();
    info!(
        "executing {n_mutants} mutants across {} batches ({} build / {} test jobs)",
        batches.len(),
        cfg.build_jobs,
        cfg.test_jobs
    );

    let build_counter = Arc::new(AtomicU64::new(0));
    let test_bound = (4 * cfg.test_jobs).max(4);
    let (batch_tx, batch_rx) = bounded::<crate::types::LocalityBatch>(batches.len().max(1));
    let (built_tx, built_rx) = bounded::<BuiltMutant>(test_bound);
    let (out_tx, out_rx) = bounded::<Outcome>(test_bound.max(32));

    for b in batches {
        batch_tx.send(b).ok();
    }
    drop(batch_tx);

    let cfg = Arc::new(cfg.clone());
    let paths = Arc::new(paths.clone());

    std::thread::scope(|scope| {
        // Build workers.
        for slot in 0..cfg.build_jobs {
            let batch_rx = batch_rx.clone();
            let built_tx = built_tx.clone();
            let out_tx = out_tx.clone();
            let backend = Arc::clone(&backend);
            let cfg = Arc::clone(&cfg);
            let paths = Arc::clone(&paths);
            let cancel = Arc::clone(&cancel);
            let build_counter = Arc::clone(&build_counter);
            scope.spawn(move || {
                let worker = match ensure_worker(backend.as_ref(), &paths, slot) {
                    Ok(p) => p,
                    Err(err) => {
                        tracing::error!("worker {slot} clone failed: {err:#}");
                        return;
                    }
                };
                while let Ok(batch) = batch_rx.recv() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let _ = walk_batch(
                        &cfg,
                        backend.as_ref(),
                        &worker,
                        &paths.runs,
                        &batch,
                        &build_counter,
                        |item| match item {
                            Ok(built) => {
                                if cfg.strict_source_isolation {
                                    let timeout = cfg.timeout_floor;
                                    let outcome = run_mutant_tests(
                                        &built.planned,
                                        &built.run_dir,
                                        &paths.baseline,
                                        timeout,
                                    );
                                    cleanup_run(&built.run_dir);
                                    match outcome {
                                        Ok(o) => {
                                            let _ = out_tx.send(o);
                                        }
                                        Err(err) => {
                                            let _ = out_tx.send(Outcome::unviable(
                                                built.planned.mutant.clone(),
                                                err.to_string(),
                                            ));
                                        }
                                    }
                                } else if built_tx.send(built).is_err() {
                                    return Err(anyhow::anyhow!("test channel closed"));
                                }
                                Ok(())
                            }
                            Err(outcome) => {
                                let _ = out_tx.send(outcome);
                                Ok(())
                            }
                        },
                        &cancel,
                    );
                }
            });
        }

        // Test workers.
        if !cfg.strict_source_isolation {
            for _ in 0..cfg.test_jobs {
                let built_rx: Receiver<BuiltMutant> = built_rx.clone();
                let out_tx: Sender<Outcome> = out_tx.clone();
                let paths = Arc::clone(&paths);
                let cfg = Arc::clone(&cfg);
                let cancel = Arc::clone(&cancel);
                scope.spawn(move || {
                    while let Ok(built) = built_rx.recv() {
                        if cancel.load(Ordering::Relaxed) {
                            cleanup_run(&built.run_dir);
                            break;
                        }
                        let outcome = run_mutant_tests(
                            &built.planned,
                            &built.run_dir,
                            &paths.baseline,
                            cfg.timeout_floor,
                        );
                        cleanup_run(&built.run_dir);
                        match outcome {
                            Ok(o) => {
                                let _ = out_tx.send(o);
                            }
                            Err(err) => {
                                let _ = out_tx.send(Outcome::unviable(
                                    built.planned.mutant.clone(),
                                    err.to_string(),
                                ));
                            }
                        }
                    }
                });
            }
        }

        drop(built_tx);
        drop(out_tx);
        drop(built_rx);

        let collected = Arc::new(Mutex::new(Vec::<Outcome>::new()));
        let jsonl = paths.state.join("outcomes.jsonl");
        while let Ok(o) = out_rx.recv() {
            let _ = append_outcome(&jsonl, &o);
            collected.lock().unwrap().push(o);
        }
        outcomes.extend(collected.lock().unwrap().clone());
    });

    Ok(ExecResult {
        builds: build_counter.load(Ordering::SeqCst),
        outcomes,
    })
}

pub(crate) fn append_outcome(path: &std::path::Path, outcome: &Outcome) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut f, outcome)?;
    f.write_all(b"\n")?;
    Ok(())
}

pub(crate) fn load_resume(path: &std::path::Path) -> Result<Vec<Outcome>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}
