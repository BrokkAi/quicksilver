//! Schema pool + test pool. Test binaries do not change between mutants.

use crate::cargo_cmd::{
    apply_build_env, apply_package_select, build_tests_captured, cargo_command, crate_root_source,
    load_metadata, nextest_profile_args, write_nextest_config, PackageSelect,
};
use crate::config::Config;
use crate::exec::build::resolve_binary;
use crate::exec::process::RunStatus;
use crate::exec::test::{run_mutant_tests_with_env, BinariesManifest, ManifestTest};
use crate::exec::{append_outcome, load_resume, ExecResult};
use crate::plan::Plan;
use crate::schema::{
    apply_schema_to_tree, attribute_spans, build_sites, chunk_sites, disable_attributed,
    disable_sites, inject_runtime, lookup_variant, parse_compiler_error_spans, qs_mutant_env,
    restore_files, snapshot_sources, Site,
};
use crate::scratch::{ensure_worker, CloneBackend};
use crate::types::{Mutant, Outcome, Paths, PlannedMutant, TestInfo};
use anyhow::{bail, Context, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

struct SchemaReady {
    planned: PlannedMutant,
    run_dir: PathBuf,
    env: Vec<(String, String)>,
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

    let build_counter = Arc::new(AtomicU64::new(0));
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
            let backend = Arc::clone(&backend);
            let cfg = Arc::clone(&cfg);
            let paths = Arc::clone(&paths);
            let cancel = Arc::clone(&cancel);
            let build_counter = Arc::clone(&build_counter);
            scope.spawn(move || {
                while let Ok((package, planned)) = job_rx.recv() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let planned_for_err = planned.clone();
                    match run_crate(
                        &cfg,
                        &paths,
                        backend.as_ref(),
                        slot,
                        &package,
                        planned,
                        &build_counter,
                        &cancel,
                    ) {
                        Ok(CrateDone {
                            unviable,
                            ready,
                            keep,
                        }) => {
                            for o in unviable {
                                if out_tx.send(o).is_err() {
                                    return;
                                }
                            }
                            for r in ready {
                                if ready_tx.send(r).is_err() {
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
            let paths = Arc::clone(&paths);
            let cfg = Arc::clone(&cfg);
            let cancel = Arc::clone(&cancel);
            scope.spawn(move || {
                while let Ok(ready) = ready_rx.recv() {
                    if cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let outcome = run_mutant_tests_with_env(
                        &ready.planned,
                        &ready.run_dir,
                        &paths.baseline,
                        cfg.timeout_floor,
                        &ready.env,
                    );
                    match outcome {
                        Ok(o) => {
                            let _ = out_tx.send(o);
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
    let _ = fs::remove_dir_all(paths.runs.join("schema"));

    Ok(ExecResult {
        builds: build_counter.load(Ordering::SeqCst),
        outcomes,
    })
}

struct CrateDone {
    unviable: Vec<Outcome>,
    ready: Vec<SchemaReady>,
    keep: Vec<PathBuf>,
}

#[allow(clippy::too_many_arguments)]
fn run_crate(
    cfg: &Config,
    paths: &Paths,
    backend: &dyn CloneBackend,
    slot: usize,
    package: &str,
    planned: Vec<PlannedMutant>,
    build_counter: &AtomicU64,
    cancel: &AtomicBool,
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
            ready: Vec::new(),
            keep: Vec::new(),
        });
    }

    if cancel.load(Ordering::Relaxed) {
        return Ok(CrateDone {
            unviable,
            ready: Vec::new(),
            keep: Vec::new(),
        });
    }

    let worker = ensure_worker(backend, paths, slot)?;
    let target = worker.join("target");
    let meta = load_metadata(&worker)?;
    let crate_root_abs =
        crate_root_source(&meta, package).with_context(|| format!("crate root for {package}"))?;
    let crate_root = rebase_to_worker(&crate_root_abs, &paths.baseline, &worker);
    let crate_root_rel = crate::util::rel_slash(
        crate_root
            .strip_prefix(&worker)
            .unwrap_or(crate_root.as_path()),
    );

    let mut packages = vec![package.to_string()];
    for p in &planned {
        for t in &p.tests {
            if !packages.iter().any(|x| x == &t.package) {
                packages.push(t.package.clone());
            }
        }
    }

    let batches = chunk_sites(sites, cfg.schema_batch_size);
    let mut ready = Vec::new();
    let mut keep = Vec::new();

    for (bi, batch) in batches.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let snap = snapshot_sources(&worker, &batch, &crate_root_rel)?;
        let isolated = isolate(
            cfg,
            &worker,
            &target,
            &packages,
            &snap,
            &crate_root,
            batch,
            build_counter,
        )?;
        for (id, msg) in isolated.unviable {
            if let Some(p) = by_id.get(&id) {
                unviable.push(Outcome::unviable(p.mutant.clone(), msg));
            }
        }
        if isolated.sites.is_empty() {
            restore_files(&worker, &snap)?;
            continue;
        }
        null_schema_gate(cfg, &worker, &target, &packages)?;
        let run_dir = paths.runs.join("schema").join(format!("{package}-{bi}"));
        stage_binaries(
            cfg,
            backend,
            &target,
            &run_dir,
            &planned,
            &isolated.sites,
            &isolated.executables,
        )?;
        identity_gate(cfg, &run_dir, &paths.baseline, &isolated.sites, &by_id)?;
        for p in &planned {
            if let Some((site, variant)) = lookup_variant(&isolated.sites, &p.mutant.id) {
                ready.push(SchemaReady {
                    planned: p.clone(),
                    run_dir: run_dir.clone(),
                    env: vec![qs_mutant_env(site.site_id, variant)],
                });
            }
        }
        keep.push(run_dir);
        restore_files(&worker, &snap)?;
    }

    Ok(CrateDone {
        unviable,
        ready,
        keep,
    })
}

struct Isolated {
    sites: Vec<Site>,
    unviable: Vec<(String, String)>,
    executables: Vec<PathBuf>,
}

#[allow(clippy::too_many_arguments)]
fn isolate(
    cfg: &Config,
    worker: &Path,
    target: &Path,
    packages: &[String],
    originals: &HashMap<String, Vec<u8>>,
    crate_root: &Path,
    mut sites: Vec<Site>,
    build_counter: &AtomicU64,
) -> Result<Isolated> {
    let mut unviable = Vec::new();
    let max_rounds = cfg.schema_max_rounds.max(1);
    for round in 1..=max_rounds {
        restore_files(worker, originals)?;
        if sites.is_empty() {
            inject_runtime(crate_root)?;
            build_counter.fetch_add(1, Ordering::SeqCst);
            let built = build_tests_captured(worker, target, cfg, packages, false)?;
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
        build_counter.fetch_add(1, Ordering::SeqCst);
        let built = build_tests_captured(worker, target, cfg, packages, false)?;
        if built.success {
            return Ok(Isolated {
                sites,
                unviable,
                executables: built.executables,
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
                packages,
                originals,
                crate_root,
                &sites,
                build_counter,
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
        build_counter.fetch_add(1, Ordering::SeqCst);
        let built = build_tests_captured(worker, target, cfg, packages, false)?;
        if !built.success {
            warn!("schema still failing after {max_rounds} diagnostic rounds; bisecting");
            let offenders = bisect_offenders(
                cfg,
                worker,
                target,
                packages,
                originals,
                crate_root,
                &sites,
                build_counter,
            )?;
            let disabled = disable_sites(&mut sites, &offenders);
            for (id, why) in disabled {
                unviable.push((id, format!("bisection (after max rounds): {why}")));
            }
            restore_files(worker, originals)?;
            if !sites.is_empty() {
                let _ = apply_schema_to_tree(worker, &sites, crate_root)?;
                build_counter.fetch_add(1, Ordering::SeqCst);
                let last = build_tests_captured(worker, target, cfg, packages, false)?;
                if !last.success {
                    bail!(
                        "schema build still failing after bisection:\n{}",
                        tail_utf8(&last.stderr)
                    );
                }
                return Ok(Isolated {
                    sites,
                    unviable,
                    executables: last.executables,
                });
            }
        } else {
            return Ok(Isolated {
                sites,
                unviable,
                executables: built.executables,
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
    packages: &[String],
    originals: &HashMap<String, Vec<u8>>,
    crate_root: &Path,
    sites: &[Site],
    build_counter: &AtomicU64,
) -> Result<BTreeSet<u32>> {
    restore_files(worker, originals)?;
    inject_runtime(crate_root)?;
    build_counter.fetch_add(1, Ordering::SeqCst);
    let rt_only = build_tests_captured(worker, target, cfg, packages, false)?;
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
        build_counter.fetch_add(1, Ordering::SeqCst);
        let built = build_tests_captured(worker, target, cfg, packages, false)?;
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
        build_counter.fetch_add(1, Ordering::SeqCst);
        let left_built = build_tests_captured(worker, target, cfg, packages, false)?;
        if !left_built.success {
            remaining = left;
        } else {
            remaining = remaining[mid..].to_vec();
        }
    }
    Ok(offenders)
}

fn null_schema_gate(cfg: &Config, worker: &Path, target: &Path, packages: &[String]) -> Result<()> {
    let config_path = write_nextest_config(&worker.join("qs-nextest"))?;
    let mut cmd = cargo_command();
    cmd.current_dir(worker);
    cmd.env("CARGO_TARGET_DIR", target);
    apply_build_env(&mut cmd, cfg, false);
    cmd.env_remove("QS_MUTANT");
    cmd.args(["nextest", "run"]);
    cmd.args(nextest_profile_args());
    cmd.arg("--config-file").arg(&config_path);
    apply_package_select(&mut cmd, &PackageSelect::Packages(packages.to_vec()));
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    let st = cmd.status().context("null-schema nextest")?;
    if !st.success() {
        bail!("null-schema gate failed: suite is not green with QS_MUTANT unset");
    }
    Ok(())
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
        let outcome =
            run_mutant_tests_with_env(&dummy, run_dir, baseline, cfg.timeout_floor, &env)?;
        match outcome.kind {
            crate::types::VerdictKind::Caught
            | crate::types::VerdictKind::Timeout
            | crate::types::VerdictKind::Errored => {
                bail!(
                    "identity-mutant gate failed for {} (QS_MUTANT={}:{}): {:?}",
                    site.fqn,
                    site.site_id,
                    site.identity_variant,
                    outcome.kind
                );
            }
            _ => {}
        }
    }
    Ok(())
}

fn stage_binaries(
    cfg: &Config,
    backend: &dyn CloneBackend,
    target: &Path,
    run_dir: &Path,
    planned: &[PlannedMutant],
    sites: &[Site],
    executables: &[PathBuf],
) -> Result<()> {
    if let Some(parent) = run_dir.parent() {
        fs::create_dir_all(parent)?;
        crate::scratch::check_free_space(parent, cfg.max_scratch)?;
    }
    if run_dir.exists() {
        let _ = fs::remove_dir_all(run_dir);
    }
    fs::create_dir_all(run_dir)?;
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
    let mut name_to_exe: HashMap<String, PathBuf> = HashMap::new();
    for exe in executables {
        if let Some(name) = exe.file_name() {
            name_to_exe.insert(name.to_string_lossy().into_owned(), exe.clone());
        }
    }
    if name_to_exe.is_empty() {
        name_to_exe.extend(scan_test_exes(target));
    }
    let mut manifest_tests = Vec::new();
    for (i, t) in tests.iter().enumerate() {
        let dest_name = format!("bin{i}");
        let src = resolve_binary(target, t, &name_to_exe)?;
        if let Some(name) = src.file_name() {
            name_to_exe.insert(name.to_string_lossy().into_owned(), src.clone());
        }
        let dest = run_dir.join(&dest_name);
        backend.clone_file(&src, &dest).or_else(|_| {
            fs::copy(&src, &dest)
                .map(|_| ())
                .context("copy test binary")
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perm = fs::metadata(&dest)?.permissions();
            perm.set_mode(0o755);
            fs::set_permissions(&dest, perm)?;
        }
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
    for profile in ["quicksilver", "debug", "dev"] {
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

fn rebase_to_worker(path: &Path, baseline: &Path, worker: &Path) -> PathBuf {
    if let Ok(rel) = path.strip_prefix(worker) {
        return worker.join(rel);
    }
    if let Ok(rel) = path.strip_prefix(baseline) {
        return worker.join(rel);
    }
    path.to_path_buf()
}

fn tail_utf8(bytes: &[u8]) -> String {
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
