//! `map`, `build` and `test`: judge mutants written as edits to a checkout.
//!
//! `build_map` builds the workspace once with an identity-only prologue at
//! every function and records which functions each test enters. `build`
//! lifts the edited bodies out of the checkout, adds them as arms to the
//! sites they touch, and rebuilds the same worker incrementally. `test` runs
//! each compiled mutant against the tests that entered its sites.

use crate::capture::{capture, Capture, EditedMutant};
use crate::cargo_cmd::{build_tests_captured, crate_root_source, load_metadata, BuiltExe};
use crate::config::Config;
use crate::exec::schema::{
    cheapest_tests, compiler_errors, null_schema_gate, stage_tests, tail_utf8, NULL_GATE_TESTS,
    SCHEMA_SLOWDOWN,
};
use crate::exec::test::{
    record_site_hits, run_mutant_tests_observed, run_mutant_tests_with_env, TestRunPolicy,
};
use crate::schema::{
    apply_schema_to_tree, attribute_spans, disable_attributed, disable_sites, inject_runtime,
    parse_compiler_error_spans, qs_mutant_env, qs_mutants_env, restore_files, Site, SiteVariant,
};
use crate::sitemap::{sites_from_functions, tracked_file_hashes, MapSite, SiteMap};
use crate::types::{Mutant, Paths, PlannedMutant, TestInfo, VerdictKind};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{info, warn};

/// Where a captured mutant is in its life.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Status {
    /// Captured, not yet compiled.
    Pending,
    /// rustc rejected one of its bodies.
    Unviable {
        message: String,
    },
    /// In the staged binaries of `generation`, switched on by `activation`.
    Built {
        generation: u32,
        activation: Vec<(u32, u32)>,
    },
    Tested {
        verdict: Verdict,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Verdict {
    pub kind: VerdictKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Tests that ran, in order.
    pub tests_run: Vec<String>,
    pub secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub mutant: EditedMutant,
    pub status: Status,
    pub captured_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    /// The map these records were built against.
    pub map_id: String,
    /// Bumped by every `build` that compiles something.
    pub generation: u32,
    pub records: Vec<Record>,
}

impl State {
    pub fn path(paths: &Paths) -> PathBuf {
        paths.state.join("edited.json")
    }

    pub fn load(paths: &Paths) -> Result<State> {
        let p = Self::path(paths);
        if !p.is_file() {
            return Ok(State::default());
        }
        let text = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        crate::util::write_atomic(&Self::path(paths), serde_json::to_vec_pretty(self)?)
    }

    pub fn find(&self, id: &str) -> Option<&Record> {
        self.records.iter().find(|r| r.mutant.id == id)
    }

    /// Drop every record when the map changed under them: their activations
    /// name sites and binaries that no longer exist.
    fn reconcile(&mut self, map: &SiteMap) -> usize {
        if self.map_id == map.map_id {
            return 0;
        }
        let n = self.records.len();
        *self = State {
            map_id: map.map_id.clone(),
            generation: 0,
            records: Vec::new(),
        };
        n
    }
}

/// Generation run directory: `runs/edited/gen-N`.
fn generation_dir(paths: &Paths, generation: u32) -> PathBuf {
    paths.runs.join("edited").join(format!("gen-{generation}"))
}

fn now_rfc3339() -> String {
    humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string()
}

/// The committed text of every file a site or crate root lives in, keyed by
/// workspace-relative path. The worktree itself may hold schema source, so
/// the originals come from git.
fn read_originals(
    tree: &Path,
    commit: &str,
    prefix: &str,
    map_sites: &[MapSite],
    crate_roots: &BTreeMap<String, String>,
) -> Result<HashMap<String, Vec<u8>>> {
    let mut files: BTreeSet<&str> = map_sites.iter().map(|s| s.path.as_str()).collect();
    files.extend(crate_roots.values().map(String::as_str));
    let mut out = HashMap::new();
    for rel in files {
        let bytes = crate::worktree::show(tree, commit, &format!("{prefix}{rel}"))?;
        out.insert(rel.to_string(), bytes);
    }
    Ok(out)
}

/// A map site as an identity-only schema site: a prologue with no arms but
/// the identity one, which is enough to record entry.
fn identity_site(s: &MapSite) -> Site {
    Site {
        site_id: s.site_id,
        salt: s.salt,
        package: s.package.clone(),
        path: s.path.clone(),
        fqn: s.fqn.clone(),
        body_start_byte: s.body_start_byte,
        body_end_byte: s.body_end_byte,
        fn_start_byte: s.fn_start_byte,
        identity_variant: 0,
        variants: Vec::new(),
    }
}

struct IsolatedAll {
    sites: Vec<Site>,
    /// (mutant id, why) for variants rustc rejected.
    unviable: Vec<(String, String)>,
    /// Site ids that were dropped whole.
    dropped_sites: BTreeSet<u32>,
    executables: Vec<BuiltExe>,
    builds: u64,
}

/// Apply every crate's sites to its crate root in the worker.
fn apply_all(
    worker: &Path,
    sites: &[Site],
    crate_roots: &BTreeMap<String, String>,
) -> Result<Vec<crate::schema::AppliedSite>> {
    let mut by_pkg: BTreeMap<&str, Vec<Site>> = BTreeMap::new();
    for s in sites {
        by_pkg
            .entry(s.package.as_str())
            .or_default()
            .push(s.clone());
    }
    let mut applied = Vec::new();
    for (pkg, sites) in by_pkg {
        let rel = crate_roots
            .get(pkg)
            .with_context(|| format!("no crate root recorded for package {pkg}"))?;
        applied.extend(apply_schema_to_tree(worker, &sites, &worker.join(rel))?);
    }
    Ok(applied)
}

/// Build the tests of `test_packages` (empty: the workspace) in `worker`.
fn build_schema(
    cfg: &Config,
    worker: &Path,
    test_packages: &[String],
    builds: &AtomicU64,
) -> Result<crate::cargo_cmd::CapturedBuild> {
    builds.fetch_add(1, Ordering::SeqCst);
    let target = worker.join("target");
    let progress = crate::progress::Progress::new("compiling", None, cfg.quiet);
    let built = build_tests_captured(worker, &target, cfg, test_packages, Some(&progress))?;
    progress.finish(&format!(
        "compiled {} crate(s) in {:.1}s{}",
        progress.done(),
        progress.elapsed_secs(),
        if built.success {
            ""
        } else {
            "; the build failed"
        }
    ));
    Ok(built)
}

/// `isolate` from the per-crate executor, over every crate at once: build
/// the tests of `test_packages`, attribute rustc errors to variants or
/// sites, drop them, repeat. When nothing can be attributed, bisect the
/// sites.
fn isolate_all(
    cfg: &Config,
    worker: &Path,
    test_packages: &[String],
    originals: &HashMap<String, Vec<u8>>,
    crate_roots: &BTreeMap<String, String>,
    mut sites: Vec<Site>,
) -> Result<IsolatedAll> {
    let builds = AtomicU64::new(0);
    let mut unviable = Vec::new();
    let mut dropped_sites = BTreeSet::new();
    let before: BTreeSet<u32> = sites.iter().map(|s| s.site_id).collect();
    let max_rounds = cfg.schema_max_rounds.max(1);
    let mut round = 0u32;
    loop {
        round += 1;
        restore_files(worker, originals)?;
        if sites.is_empty() {
            for rel in crate_roots.values() {
                inject_runtime(&worker.join(rel))?;
            }
        }
        let applied = apply_all(worker, &sites, crate_roots)?;
        let built = build_schema(cfg, worker, test_packages, &builds)?;
        if built.success {
            return Ok(IsolatedAll {
                sites,
                unviable,
                dropped_sites,
                executables: built.executables,
                builds: builds.load(Ordering::SeqCst),
            });
        }
        if sites.is_empty() {
            bail!(
                "the workspace does not compile with only the schema runtime injected:\n{}",
                compiler_errors(&built.stdout, &built.stderr)
            );
        }
        let spans = parse_compiler_error_spans(&built.stdout);
        let attr = attribute_spans(&applied, &spans, worker);
        let rendered = compiler_errors(&built.stdout, &built.stderr);
        if !attr.is_empty() && round <= max_rounds {
            let disabled = disable_attributed(&mut sites, &attr);
            for (id, why) in disabled {
                unviable.push((
                    id,
                    format!(
                        "rustc (round {round}): {why}\n{}",
                        first_error_for(&rendered, 1200)
                    ),
                ));
            }
            let after: BTreeSet<u32> = sites.iter().map(|s| s.site_id).collect();
            dropped_sites.extend(before.difference(&after).copied());
            continue;
        }
        warn!("schema isolation round {round}: no attributable rustc span; bisecting");
        let offenders = bisect_all(
            cfg,
            worker,
            test_packages,
            originals,
            crate_roots,
            &sites,
            &builds,
        )?;
        if offenders.is_empty() {
            bail!(
                "schema build failed with no attributable site and bisection found none:\n{}",
                tail_utf8(&built.stderr)
            );
        }
        for (id, why) in disable_sites(&mut sites, &offenders) {
            unviable.push((id, format!("bisection (round {round}): {why}")));
        }
        dropped_sites.extend(offenders);
    }
}

fn first_error_for(rendered: &str, max: usize) -> String {
    let s = rendered.trim();
    let end = s
        .char_indices()
        .map(|(i, _)| i)
        .find(|i| *i >= max)
        .unwrap_or(s.len());
    s[..end].to_string()
}

fn bisect_all(
    cfg: &Config,
    worker: &Path,
    test_packages: &[String],
    originals: &HashMap<String, Vec<u8>>,
    crate_roots: &BTreeMap<String, String>,
    sites: &[Site],
    builds: &AtomicU64,
) -> Result<BTreeSet<u32>> {
    let mut remaining = sites.to_vec();
    let mut offenders = BTreeSet::new();
    while !remaining.is_empty() {
        restore_files(worker, originals)?;
        apply_all(worker, &remaining, crate_roots)?;
        if build_schema(cfg, worker, test_packages, builds)?.success {
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
        apply_all(worker, &left, crate_roots)?;
        if !build_schema(cfg, worker, test_packages, builds)?.success {
            remaining = left;
        } else {
            remaining = remaining[mid..].to_vec();
        }
    }
    Ok(offenders)
}

fn recording_policy(cfg: &Config) -> TestRunPolicy {
    TestRunPolicy {
        floor: cfg.timeout_floor.max(Duration::from_secs(30)),
        slowdown: SCHEMA_SLOWDOWN,
        confirm: false,
    }
}

/// A stand-in `types::Mutant` for the test runner, which reports verdicts
/// against one. `fqn` lists every edited function.
fn representative(map: &SiteMap, m: &EditedMutant) -> Mutant {
    let first = m.edits.first();
    let site = first.and_then(|e| map.site(e.site_id));
    Mutant {
        id: m.id.clone(),
        package: first.map(|e| e.package.clone()).unwrap_or_default(),
        path: first.map(|e| e.path.clone()).unwrap_or_default(),
        fqn: m.fqns(),
        start_byte: site.map(|s| s.fn_start_byte).unwrap_or(0),
        end_byte: site.map(|s| s.body_end_byte).unwrap_or(0),
        start_line: site.map(|s| s.start_line).unwrap_or(1),
        end_line: site.map(|s| s.end_line).unwrap_or(1),
        body_start_byte: site.map(|s| s.body_start_byte).unwrap_or(0),
        body_end_byte: site.map(|s| s.body_end_byte).unwrap_or(0),
        replacement: String::new(),
        return_kind: "edited".into(),
    }
}

fn all_tests_planned(map_tests: &[TestInfo]) -> PlannedMutant {
    PlannedMutant {
        mutant: Mutant {
            id: "map".into(),
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
        },
        tests: map_tests.to_vec(),
        estimated_cost_secs: 0.0,
    }
}

/// Build the map: baseline, sites, one workspace build with identity-only
/// prologues, staged binaries, gates, and the recording run.
pub fn build_map(cfg: &Config, paths: &Paths) -> Result<SiteMap> {
    let baseline = crate::baseline::prepare_baseline(cfg, paths, false, false)?;
    let mut tests = crate::index::list_tests_plain(cfg, paths)?;
    let timing = crate::index::attach_timings(&mut tests, &baseline.tests);
    info!(
        "map: {} tests catalogued, {} with baseline durations",
        tests.len(),
        timing.matched
    );
    build_map_from(cfg, paths, tests)
}

/// The saved map, when it was built at `commit`. Commands that need a map
/// never build one themselves; the user runs `cargo quicksilver map`.
pub fn map_at(paths: &Paths, commit: &str) -> Result<SiteMap> {
    let short = &commit[..12.min(commit.len())];
    match SiteMap::load(paths)? {
        Some(map) if map.commit == commit => Ok(map),
        Some(map) => bail!(
            "the map was built at {}, not at HEAD ({short}); run `cargo quicksilver map` first",
            &map.commit[..12.min(map.commit.len())]
        ),
        None => bail!("no map for this repository; run `cargo quicksilver map` first"),
    }
}

/// [`build_map`] for a baseline that is already checked out and built, with
/// its tests listed and timed.
pub fn build_map_from(cfg: &Config, paths: &Paths, tests: Vec<TestInfo>) -> Result<SiteMap> {
    let started = Instant::now();
    let listed = tests;
    let files = tracked_file_hashes(&cfg.repo_root)?;

    let fns =
        crate::discover::scan_workspace(&paths.baseline, &crate::engine::discover_filter(cfg))?;
    let (map_sites, mut dropped) = sites_from_functions(&fns)?;
    info!(
        "map: {} sites, {} functions without a site",
        map_sites.len(),
        dropped.len()
    );

    let meta = load_metadata(&paths.baseline)?;
    let mut crate_roots = BTreeMap::new();
    for pkg in map_sites
        .iter()
        .map(|s| s.package.as_str())
        .collect::<BTreeSet<_>>()
    {
        let abs = crate_root_source(&meta, pkg).with_context(|| format!("crate root for {pkg}"))?;
        let rel = abs
            .strip_prefix(&paths.baseline)
            .map(crate::util::rel_slash)
            .with_context(|| format!("crate root {} is outside the baseline", abs.display()))?;
        crate_roots.insert(pkg.to_string(), rel);
    }

    let worker = crate::scratch::ensure_worker(paths, 0)?;
    let commit = crate::worktree::head(&worker)?;
    let originals = read_originals(&worker, &commit, &cfg.prefix, &map_sites, &crate_roots)?;
    let sites: Vec<Site> = map_sites.iter().map(identity_site).collect();
    let test_packages = crate::cargo_cmd::test_packages(cfg, &meta);
    let isolated = isolate_all(
        cfg,
        &worker,
        &test_packages,
        &originals,
        &crate_roots,
        sites,
    )?;
    let kept: BTreeSet<u32> = isolated.sites.iter().map(|s| s.site_id).collect();
    let mut map_sites: Vec<MapSite> = map_sites;
    for s in &map_sites {
        if !kept.contains(&s.site_id) {
            dropped.push((
                s.path.clone(),
                s.fqn.clone(),
                "its prologue did not compile; see the map build log".into(),
            ));
        }
    }
    map_sites.retain(|s| kept.contains(&s.site_id));
    info!(
        "map: schema built in {} cargo invocation(s); {} sites kept",
        isolated.builds,
        map_sites.len()
    );

    let run_dir = paths.runs.join("map");
    let test_refs: Vec<&TestInfo> = listed.iter().collect();
    stage_tests(
        cfg,
        &worker.join("target"),
        &run_dir,
        &test_refs,
        &isolated.executables,
    )?;
    let planned = all_tests_planned(&listed);
    null_schema_gate(cfg, &run_dir, &worker, std::slice::from_ref(&planned))?;

    let progress =
        crate::progress::Progress::new("recording", Some(listed.len() as u64), cfg.quiet);
    let recording = record_site_hits(
        &planned,
        &run_dir,
        &worker,
        &recording_policy(cfg),
        cfg.test_jobs,
        Some(&progress),
    )?;
    progress.finish(&format!(
        "recorded {} tests in {:.1}s",
        listed.len(),
        progress.elapsed_secs()
    ));
    let site_ids: BTreeSet<u32> = map_sites.iter().map(|s| s.site_id).collect();
    let mut hits: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (test, sites) in &recording.sites_by_test {
        let entered: Vec<u32> = sites
            .iter()
            .copied()
            .filter(|s| site_ids.contains(s))
            .collect();
        hits.insert(test.clone(), entered);
    }
    let mut tests = listed.clone();
    for t in &mut tests {
        if let Some(secs) = recording.recorded_secs(t) {
            t.duration_secs = secs;
        }
    }
    let entered: BTreeSet<u32> = hits.values().flatten().copied().collect();
    info!(
        "map: recorded {} tests in {:.1}s, {} not green; {} of {} sites entered",
        recording.sites_by_test.len(),
        recording.wall_secs,
        recording.not_green.len(),
        entered.len(),
        site_ids.len()
    );
    for (test, why) in recording.not_green.iter().take(8) {
        warn!("  not green under the schema binary: {test}: {why}");
    }

    let map = SiteMap {
        version: crate::sitemap::MAP_VERSION,
        map_id: crate::sitemap::random_id(),
        commit: commit.clone(),
        prefix: cfg.prefix.clone(),
        repo_root: cfg.repo_root.clone(),
        files,
        sites: map_sites,
        dropped,
        crate_roots,
        test_packages,
        tests,
        hits,
        not_green: recording.not_green.clone(),
        worker: worker.clone(),
        run_dir: run_dir.clone(),
        map_secs: started.elapsed().as_secs_f64(),
    };
    identity_gate_map(cfg, &map)?;
    map.save(paths)?;
    Ok(map)
}

/// With the identity variant active, a sample of sites must behave as the
/// original: the cheapest tests that enter each still pass.
fn identity_gate_map(cfg: &Config, map: &SiteMap) -> Result<()> {
    let mut sample: Vec<&MapSite> = map
        .sites
        .iter()
        .filter(|s| map.hits.values().any(|h| h.contains(&s.site_id)))
        .collect();
    sample.sort_by(|a, b| a.fqn.cmp(&b.fqn));
    let n = if cfg.identity_sample == 0 {
        sample.len()
    } else {
        cfg.identity_sample.min(sample.len())
    };
    for site in sample.into_iter().take(n) {
        let tests = cheapest_tests(map.tests_entering(&[site.site_id]), NULL_GATE_TESTS);
        let planned = PlannedMutant {
            mutant: Mutant {
                id: site.fqn.clone(),
                package: site.package.clone(),
                path: site.path.clone(),
                fqn: site.fqn.clone(),
                start_byte: site.fn_start_byte,
                end_byte: site.body_end_byte,
                start_line: site.start_line,
                end_line: site.end_line,
                body_start_byte: site.body_start_byte,
                body_end_byte: site.body_end_byte,
                replacement: String::new(),
                return_kind: String::new(),
            },
            tests,
            estimated_cost_secs: 0.0,
        };
        let env = vec![qs_mutant_env(site.site_id, 0)];
        let floor = cfg.timeout_floor.max(Duration::from_secs(30));
        let outcome = run_mutant_tests_with_env(&planned, &map.run_dir, &map.worker, floor, &env)?;
        if matches!(
            outcome.kind,
            VerdictKind::Caught | VerdictKind::Timeout | VerdictKind::Errored
        ) {
            bail!(
                "identity gate failed for {} (QS_MUTANT={}:0): {:?} {}",
                site.fqn,
                site.site_id,
                outcome.kind,
                outcome
                    .test
                    .as_deref()
                    .or(outcome.message.as_deref())
                    .unwrap_or("")
            );
        }
    }
    Ok(())
}

pub struct BuildReport {
    /// Mutants this call captured (new ones only).
    pub captured: Vec<EditedMutant>,
    /// Mutants this call found already captured.
    pub duplicates: Vec<EditedMutant>,
    pub rejected: Vec<String>,
    /// Every mutant in this build's set with its status afterwards.
    pub results: Vec<(EditedMutant, Status)>,
    pub generation: u32,
    pub builds: u64,
    pub secs: f64,
    pub discarded_records: usize,
}

/// The map, the mutant state reconciled with it, and an empty report.
fn load_for_build(paths: &Paths) -> Result<(SiteMap, State, BuildReport)> {
    let map = SiteMap::load(paths)?
        .context("no map for this repository; run `cargo quicksilver map` first")?;
    let mut state = State::load(paths)?;
    let discarded_records = state.reconcile(&map);
    if discarded_records > 0 {
        eprintln!("discarded {discarded_records} mutant(s) captured against an earlier map");
    }
    let report = BuildReport {
        captured: Vec::new(),
        duplicates: Vec::new(),
        rejected: Vec::new(),
        results: Vec::new(),
        generation: state.generation,
        builds: 0,
        secs: 0.0,
        discarded_records,
    };
    Ok((map, state, report))
}

/// Record the checkout's edits as pending mutants, one per edited function,
/// without compiling. Several checkouts or patches can be captured in turn
/// and then compiled in one `build`.
pub fn capture_edits(paths: &Paths, checkout: &Path) -> Result<BuildReport> {
    let started = Instant::now();
    let (map, mut state, mut report) = load_for_build(paths)?;
    capture_into(&map, &mut state, checkout, &mut report)?;
    if report.rejected.is_empty() {
        state.save(paths)?;
    }
    report.secs = started.elapsed().as_secs_f64();
    Ok(report)
}

fn capture_into(
    map: &SiteMap,
    state: &mut State,
    checkout: &Path,
    report: &mut BuildReport,
) -> Result<()> {
    match capture(map, checkout)? {
        Capture::Rejected(problems) => {
            eprintln!("the edits in {} were not captured:", checkout.display());
            for p in &problems {
                eprintln!("  {p}");
            }
            eprintln!("a mutant may only change function bodies outside test code; reset the checkout and edit again");
            report.rejected = problems;
        }
        Capture::Nothing => eprintln!("no edits in {}", checkout.display()),
        Capture::Mutants(mutants) => {
            for m in mutants {
                if state.find(&m.id).is_some() {
                    report.duplicates.push(m);
                } else {
                    state.records.push(Record {
                        mutant: m.clone(),
                        status: Status::Pending,
                        captured_at: now_rfc3339(),
                    });
                    report.captured.push(m);
                }
            }
            eprintln!(
                "captured {} mutant(s) from {}{}",
                report.captured.len(),
                checkout.display(),
                if report.duplicates.is_empty() {
                    String::new()
                } else {
                    format!(" ({} already captured)", report.duplicates.len())
                }
            );
            for m in &report.captured {
                eprintln!("  {} {}", m.id, m.fqns());
            }
        }
    }
    Ok(())
}

/// Capture the checkout's edits (unless `capture_from` is `None`) as one
/// mutant per edited function, then compile every pending and untested
/// mutant into a new generation of binaries in one build. The checkout is
/// only read; resetting it for the next edit is the caller's job
/// (`git checkout -- .`).
pub fn build(cfg: &Config, paths: &Paths, capture_from: Option<&Path>) -> Result<BuildReport> {
    let started = Instant::now();
    let (map, mut state, mut report) = load_for_build(paths)?;
    if let Some(checkout) = capture_from {
        capture_into(&map, &mut state, checkout, &mut report)?;
        if !report.rejected.is_empty() {
            report.secs = started.elapsed().as_secs_f64();
            return Ok(report);
        }
        state.save(paths)?;
    }

    let set: Vec<EditedMutant> = state
        .records
        .iter()
        .filter(|r| matches!(r.status, Status::Pending | Status::Built { .. }))
        .map(|r| r.mutant.clone())
        .collect();
    if set.is_empty() {
        report.secs = started.elapsed().as_secs_f64();
        return Ok(report);
    }
    eprintln!(
        "building {} mutant(s) not yet tested into the schema binary",
        set.len()
    );

    if !map.worker.join("Cargo.toml").is_file() {
        bail!(
            "the map's worker tree {} is gone; run `cargo quicksilver map` again",
            map.worker.display()
        );
    }

    // Every map site keeps its identity-only prologue, so the files differ
    // from the map build only at the edited functions and cargo rebuilds
    // little. Edited sites get one arm per mutant.
    let mut sites: BTreeMap<u32, Site> = map
        .sites
        .iter()
        .map(|s| (s.site_id, identity_site(s)))
        .collect();
    let mut activation: BTreeMap<String, Vec<(u32, u32)>> = BTreeMap::new();
    for m in &set {
        for e in &m.edits {
            let site = sites
                .get_mut(&e.site_id)
                .with_context(|| format!("mutant {} edits unknown site {:#x}", m.id, e.site_id))?;
            let index = site.variants.len() as u32;
            site.variants.push(SiteVariant {
                index,
                mutant_id: m.id.clone(),
                return_kind: "edited".into(),
                return_stmt: format!("return {}", e.body),
            });
            site.identity_variant = site.variants.len() as u32;
            activation
                .entry(m.id.clone())
                .or_default()
                .push((e.site_id, index));
        }
    }
    let touched: BTreeSet<u32> = set
        .iter()
        .flat_map(|m| m.edits.iter().map(|e| e.site_id))
        .collect();

    let originals = read_originals(
        &map.worker,
        &map.commit,
        &map.prefix,
        &map.sites,
        &map.crate_roots,
    )?;
    let all_sites: Vec<Site> = sites.into_values().collect();
    let isolated = isolate_all(
        cfg,
        &map.worker,
        &map.test_packages,
        &originals,
        &map.crate_roots,
        all_sites,
    )?;
    report.builds = isolated.builds;
    let mut unviable: BTreeMap<String, String> = BTreeMap::new();
    for (id, why) in &isolated.unviable {
        unviable.entry(id.clone()).or_insert_with(|| why.clone());
    }
    for m in &set {
        if unviable.contains_key(&m.id) {
            continue;
        }
        if let Some(e) = m
            .edits
            .iter()
            .find(|e| isolated.dropped_sites.contains(&e.site_id))
        {
            unviable.insert(
                m.id.clone(),
                format!(
                    "site {} was dropped whole: its prologue no longer compiles",
                    e.fqn
                ),
            );
        }
    }

    state.generation += 1;
    let generation = state.generation;
    let run_dir = generation_dir(paths, generation);
    let touched_ids: Vec<u32> = touched.iter().copied().collect();
    let union = map.tests_entering(&touched_ids);
    let union_refs: Vec<&TestInfo> = union.iter().collect();
    stage_tests(
        cfg,
        &map.worker.join("target"),
        &run_dir,
        &union_refs,
        &isolated.executables,
    )?;
    if !union.is_empty() {
        null_schema_gate(cfg, &run_dir, &map.worker, &[all_tests_planned(&union)])?;
    }
    if generation > 1 {
        let _ = std::fs::remove_dir_all(generation_dir(paths, generation - 1));
    }

    for r in &mut state.records {
        if !set.iter().any(|m| m.id == r.mutant.id) {
            continue;
        }
        r.status = match unviable.get(&r.mutant.id) {
            Some(why) => Status::Unviable {
                message: why.clone(),
            },
            None => Status::Built {
                generation,
                activation: activation.remove(&r.mutant.id).unwrap_or_default(),
            },
        };
        report.results.push((r.mutant.clone(), r.status.clone()));
    }
    state.save(paths)?;
    report.generation = generation;
    report.secs = started.elapsed().as_secs_f64();
    Ok(report)
}

fn verdict_from(
    outcome: &crate::types::Outcome,
    run: &crate::exec::test::ObservedRun,
    secs: f64,
) -> Verdict {
    Verdict {
        kind: outcome.kind,
        test: outcome.test.clone(),
        message: outcome.message.clone(),
        tests_run: run
            .observations
            .iter()
            .map(|o| o.test_name.clone())
            .collect(),
        secs,
    }
}

/// One line per verdict, as `test` prints them while it runs.
pub fn verdict_line(m: &EditedMutant, v: &Verdict) -> String {
    match v.kind {
        VerdictKind::Caught => format!(
            "  CAUGHT      {} {} by {}{}",
            m.id,
            m.fqns(),
            v.test.as_deref().unwrap_or("?"),
            if v.message.is_some() { " (crash)" } else { "" }
        ),
        VerdictKind::Missed => format!(
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
        VerdictKind::NotCovered => format!(
            "  NOT_COVERED {} {} (no test enters these functions)",
            m.id,
            m.fqns()
        ),
        VerdictKind::Timeout => format!(
            "  TIMEOUT     {} {} in {}",
            m.id,
            m.fqns(),
            v.test.as_deref().unwrap_or("?")
        ),
        VerdictKind::Errored | VerdictKind::Unviable => format!(
            "  ERRORED     {} {} {}",
            m.id,
            m.fqns(),
            v.message.as_deref().unwrap_or("")
        ),
    }
}

/// Run every mutant built into the current generation (or those whose id
/// starts with one of `only`) against the tests that entered its sites.
pub fn test(cfg: &Config, paths: &Paths, only: &[String]) -> Result<Vec<(EditedMutant, Verdict)>> {
    let map = SiteMap::load(paths)?
        .context("no map in this scratch session; run `cargo quicksilver map` first")?;
    let mut state = State::load(paths)?;
    if state.map_id != map.map_id {
        bail!(
            "the mutant state was built against another map; run `cargo quicksilver build` first"
        );
    }
    let generation = state.generation;
    let run_dir = generation_dir(paths, generation);
    let set: Vec<(EditedMutant, Vec<(u32, u32)>)> = state
        .records
        .iter()
        .filter(|r| only.is_empty() || only.iter().any(|p| r.mutant.id.starts_with(p.as_str())))
        .filter_map(|r| match &r.status {
            Status::Built {
                generation: g,
                activation,
            } if *g == generation => Some((r.mutant.clone(), activation.clone())),
            _ => None,
        })
        .collect();
    if set.is_empty() {
        return Ok(Vec::new());
    }
    if !run_dir.join("manifest.json").is_file() {
        bail!(
            "staged binaries for generation {generation} are missing at {}; run `cargo quicksilver build` again",
            run_dir.display()
        );
    }
    let policy = TestRunPolicy {
        floor: cfg.timeout_floor,
        slowdown: 1.0,
        confirm: true,
    };
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(EditedMutant, Verdict)>> = Mutex::new(Vec::new());
    let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let progress = crate::progress::Progress::new("testing", Some(set.len() as u64), cfg.quiet);
    std::thread::scope(|scope| {
        for _ in 0..cfg.test_jobs.max(1).min(set.len()) {
            scope.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::SeqCst);
                let Some((m, activation)) = set.get(k) else {
                    break;
                };
                let sites: Vec<u32> = m.edits.iter().map(|e| e.site_id).collect();
                let tests = map.tests_entering(&sites);
                let started = Instant::now();
                let verdict = if tests.is_empty() {
                    Verdict {
                        kind: VerdictKind::NotCovered,
                        test: None,
                        message: None,
                        tests_run: Vec::new(),
                        secs: 0.0,
                    }
                } else {
                    let planned = PlannedMutant {
                        mutant: representative(&map, m),
                        estimated_cost_secs: tests.iter().map(|t| t.duration_secs).sum(),
                        tests,
                    };
                    let env = vec![qs_mutants_env(activation)];
                    match run_mutant_tests_observed(
                        &planned,
                        &run_dir,
                        &map.worker,
                        &policy,
                        &env,
                        false,
                    ) {
                        Ok(run) => {
                            verdict_from(&run.outcome, &run, started.elapsed().as_secs_f64())
                        }
                        Err(err) => {
                            errors.lock().unwrap().push(format!("{}: {err:#}", m.id));
                            Verdict {
                                kind: VerdictKind::Errored,
                                test: None,
                                message: Some(format!("{err:#}")),
                                tests_run: Vec::new(),
                                secs: started.elapsed().as_secs_f64(),
                            }
                        }
                    }
                };
                progress.println(&verdict_line(m, &verdict));
                progress.inc(1);
                results.lock().unwrap().push((m.clone(), verdict));
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    let count = |k: VerdictKind| results.iter().filter(|(_, v)| v.kind == k).count();
    progress.finish(&format!(
        "tested {} mutant(s) in {:.1}s: {} caught, {} missed, {} not covered, {} timeout, {} errored",
        results.len(),
        progress.elapsed_secs(),
        count(VerdictKind::Caught),
        count(VerdictKind::Missed),
        count(VerdictKind::NotCovered),
        count(VerdictKind::Timeout),
        count(VerdictKind::Errored)
    ));
    results.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    for (m, v) in &results {
        if let Some(r) = state.records.iter_mut().find(|r| r.mutant.id == m.id) {
            r.status = Status::Tested { verdict: v.clone() };
        }
    }
    state.save(paths)?;
    for e in errors.into_inner().unwrap() {
        warn!("test run error: {e}");
    }
    Ok(results)
}
