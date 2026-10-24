//! `map`, `build` and `test`: judge mutants written as edits to a checkout.
//!
//! `build_map` builds the workspace once with an identity-only prologue at
//! every function and records which functions each test enters. `build`
//! lifts the edited bodies out of the checkout, adds them as arms to the
//! sites they touch, and rebuilds the same worker incrementally. `test` runs
//! each compiled mutant against the tests that entered its sites.

use crate::capture::{
    capture_checkout, capture_patch, CaptureOptions, CaptureOutcome, EditedMutant,
};
use crate::cargo_cmd::{build_tests_captured, crate_root_source, load_metadata, BuiltExe};
use crate::config::Config;
use crate::exec::schema::{
    cheapest_tests, compiler_errors, null_schema_gate, stage_tests, tail_utf8, NULL_GATE_TESTS,
    SCHEMA_SLOWDOWN,
};
use crate::exec::test::{
    record_site_hits, run_mutant_tests_against, run_mutant_tests_observed,
    run_mutant_tests_with_env, TestRunPolicy, DIRECT_MUTANT_ENV,
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
    /// A `direct` mutant (of an `impl Trait` function) has binaries of its
    /// own that always contain it, in `gen-N/direct-<id>`.
    Built {
        generation: u32,
        activation: Vec<(u32, u32)>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        direct: bool,
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
    /// Set when only proptest properties decided the verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The `PROPTEST_CASES` of the re-run that decided a survivor judged
    /// only by properties, when `test --proptest-cases` ran one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proptest_rerun_cases: Option<u32>,
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

/// An exclusive lock on the mutant state, held by `build` and `test` for
/// their whole run, so that captures from parallel authors queue instead of
/// overwriting each other's records.
pub struct StateLock {
    _file: std::fs::File,
}

impl StateLock {
    pub fn acquire(paths: &Paths) -> Result<StateLock> {
        use std::os::unix::io::AsRawFd;
        std::fs::create_dir_all(&paths.state)?;
        let path = paths.state.join("edited.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let fd = file.as_raw_fd();
        // SAFETY: flock on a descriptor this function owns.
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            eprintln!("waiting for another `build` or `test` of this repository to finish");
            if unsafe { libc::flock(fd, libc::LOCK_EX) } != 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("lock {}", path.display()));
            }
        }
        Ok(StateLock { _file: file })
    }
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

/// Binaries of one directly built mutant: `runs/edited/gen-N/direct-<id>`.
fn direct_dir(paths: &Paths, generation: u32, id: &str) -> PathBuf {
    generation_dir(paths, generation).join(format!("direct-{id}"))
}

/// Build one mutant of an `impl Trait` function directly: its body replaces
/// the original in the source, and every other site is as in the main
/// generation (`main_sites`, which compiled), so a failure is the mutant's.
/// A schema arm cannot hold such a mutant: the original body stays after the
/// prologue, and the opaque type must be the same for every return, which a
/// different closure or iterator adapter already breaks. Returns the test
/// executables, or rustc's errors when the mutant does not compile.
fn build_direct(
    cfg: &Config,
    map: &SiteMap,
    originals: &HashMap<String, Vec<u8>>,
    main_sites: &[Site],
    edit: &crate::capture::BodyEdit,
) -> Result<std::result::Result<Vec<BuiltExe>, String>> {
    let site = map
        .site(edit.site_id)
        .with_context(|| format!("unknown site {:#x}", edit.site_id))?;
    let (start, end) = (site.body_start_byte, site.body_end_byte);
    let original = originals
        .get(&edit.path)
        .with_context(|| format!("no original text for {}", edit.path))?;
    let text = std::str::from_utf8(original).context("source is not UTF-8")?;
    let mut files = originals.clone();
    files.insert(
        edit.path.clone(),
        format!("{}{}{}", &text[..start], edit.body, &text[end..]).into_bytes(),
    );
    let delta = edit.body.len() as isize - (end - start) as isize;
    let shift = |off: usize| {
        if off >= end {
            (off as isize + delta) as usize
        } else {
            off
        }
    };
    // The replaced function and anything nested in it lose their prologues;
    // sites after it in the same file move by the change in length.
    let sites: Vec<Site> = main_sites
        .iter()
        .filter(|s| !(s.path == edit.path && s.body_start_byte >= start && s.body_end_byte <= end))
        .map(|s| {
            let mut s = s.clone();
            if s.path == edit.path {
                s.fn_start_byte = shift(s.fn_start_byte);
                s.body_start_byte = shift(s.body_start_byte);
                s.body_end_byte = shift(s.body_end_byte);
            }
            s
        })
        .collect();
    restore_files(&map.worker, &files)?;
    apply_all(&map.worker, &sites, &map.crate_roots)?;
    let built = build_schema(cfg, &map.worker, &map.test_packages, &AtomicU64::new(0))?;
    if built.success {
        Ok(Ok(built.executables))
    } else {
        Ok(Err(format!(
            "rustc (built on its own, as an `impl Trait` function's mutant):\n{}",
            first_error_for(&compiler_errors(&built.stdout, &built.stderr), 1500)
        )))
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
        if !attr.is_empty() && round <= max_rounds {
            let disabled = disable_attributed(&mut sites, &attr);
            for (id, why) in disabled {
                unviable.push((id, format!("rustc (round {round}): {why}")));
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

    let (fns, test_lines) = crate::discover::scan_workspace_with_tests(
        &paths.baseline,
        &crate::engine::discover_filter(cfg),
    )?;
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
    let property_tests =
        crate::proptest::property_tests(&worker, &meta, &listed).unwrap_or_else(|err| {
            warn!("cannot look for proptest properties: {err:#}");
            Default::default()
        });
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
        target: crate::target::host_triple(&worker).unwrap_or_default(),
        files,
        test_lines,
        sites: map_sites,
        dropped,
        crate_roots,
        test_packages,
        tests,
        hits,
        not_green: recording.not_green.clone(),
        property_tests,
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
    /// Edits dropped under `--skip-outside-bodies`.
    pub dropped: Vec<String>,
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
        dropped: Vec::new(),
        results: Vec::new(),
        generation: state.generation,
        builds: 0,
        secs: 0.0,
        discarded_records,
    };
    Ok((map, state, report))
}

/// Where `build` takes edits from: the checkout, patch files, or neither
/// (compile what is pending).
#[derive(Debug, Clone, Default)]
pub struct CaptureSources {
    pub checkout: Option<PathBuf>,
    pub patches: Vec<PathBuf>,
    pub options: CaptureOptions,
    /// Author's notes by full function name or `path:line`, applied to the
    /// mutants this build captures (see `capture::attach_intents`).
    pub intents: BTreeMap<String, String>,
}

/// Read an intents file: a JSON object from function name or `path:line`
/// to the fault the mutant models.
pub fn read_intents(path: &Path) -> Result<BTreeMap<String, String>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| {
        format!(
            "{} must be a JSON object mapping a function name or `path:line` to text",
            path.display()
        )
    })
}

/// Print a capture's dropped edits and problems. Returns false when the
/// capture was rejected.
fn check_capture(source: &str, outcome: &CaptureOutcome, report: &mut BuildReport) -> bool {
    for d in &outcome.dropped {
        eprintln!("  dropped: {d}");
    }
    report.dropped.extend(outcome.dropped.iter().cloned());
    if !outcome.problems.is_empty() {
        eprintln!("the edits in {source} were not captured:");
        for p in &outcome.problems {
            eprintln!("  {p}");
        }
        eprintln!("a mutant may only change function bodies outside test code; fix the edits (or pass --skip-outside-bodies) and build again");
        report.rejected.extend(outcome.problems.iter().cloned());
        return false;
    }
    true
}

/// Record one capture's mutants as pending, and print what happened.
fn record_capture(
    state: &mut State,
    source: &str,
    outcome: CaptureOutcome,
    report: &mut BuildReport,
) {
    let (mut new, mut dup) = (0usize, 0usize);
    for m in outcome.mutants {
        if state.find(&m.id).is_some() {
            dup += 1;
            report.duplicates.push(m);
        } else {
            new += 1;
            eprintln!("  {} {}", m.id, m.fqns());
            state.records.push(Record {
                mutant: m.clone(),
                status: Status::Pending,
                captured_at: now_rfc3339(),
            });
            report.captured.push(m);
        }
    }
    if new + dup == 0 {
        eprintln!("no edits in {source}");
    } else {
        eprintln!(
            "captured {new} mutant(s) from {source}{}",
            if dup == 0 {
                String::new()
            } else {
                format!(" ({dup} already captured)")
            }
        );
    }
}

/// Capture edits from `sources` as one mutant per edited function, then
/// compile every pending and untested mutant into a new generation of
/// binaries in one build. A rejected capture records nothing and compiles
/// nothing. The checkout is only read; resetting it for the next edit is the
/// caller's job (`git checkout -- .`).
pub fn build(cfg: &Config, paths: &Paths, sources: &CaptureSources) -> Result<BuildReport> {
    let started = Instant::now();
    let _lock = StateLock::acquire(paths)?;
    let (map, mut state, mut report) = load_for_build(paths)?;
    let mut captures: Vec<(String, CaptureOutcome)> = Vec::new();
    if let Some(checkout) = &sources.checkout {
        let outcome = capture_checkout(&map, checkout, sources.options)?;
        captures.push((checkout.display().to_string(), outcome));
    }
    for patch in &sources.patches {
        let outcome = capture_patch(&map, &cfg.repo_root, patch, sources.options)?;
        captures.push((patch.display().to_string(), outcome));
    }
    let mut ok = true;
    for (source, outcome) in &captures {
        ok &= check_capture(source, outcome, &mut report);
    }
    if !ok {
        report.secs = started.elapsed().as_secs_f64();
        return Ok(report);
    }
    if !sources.intents.is_empty() {
        // Intents are matched across everything this build captured, so a
        // key naming a function that two sources edited is ambiguous.
        let mut all: Vec<EditedMutant> = captures
            .iter()
            .flat_map(|(_, o)| o.mutants.iter().cloned())
            .collect();
        let spans: Vec<(usize, usize)> = all
            .iter()
            .map(|m| {
                m.edits
                    .first()
                    .and_then(|e| map.site(e.site_id))
                    .map_or((0, 0), |s| (s.start_line, s.end_line))
            })
            .collect();
        let functions: Vec<(&str, &str, (usize, usize))> = map
            .sites
            .iter()
            .map(|s| (s.fqn.as_str(), s.path.as_str(), (s.start_line, s.end_line)))
            .collect();
        for w in crate::capture::attach_intents(
            &mut all,
            &spans,
            &sources.intents,
            &map.prefix,
            &functions,
        ) {
            eprintln!("  {w}");
        }
        let mut it = all.into_iter();
        for (_, outcome) in &mut captures {
            for m in &mut outcome.mutants {
                *m = it.next().expect("same count");
            }
        }
    }
    let captured_any = !captures.is_empty();
    for (source, outcome) in captures {
        record_capture(&mut state, &source, outcome, &mut report);
    }
    if captured_any {
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
    let opaque = |m: &EditedMutant| {
        m.edits
            .iter()
            .any(|e| map.site(e.site_id).is_some_and(|s| s.opaque_return))
    };
    let direct_set: Vec<EditedMutant> = set.iter().filter(|m| opaque(m)).cloned().collect();
    for m in set.iter().filter(|m| !opaque(m)) {
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

    // Mutants of `impl Trait` functions: one build each, then the main
    // generation's source again, so that tests which run cargo themselves
    // build what the main binaries were built from.
    let mut direct: BTreeSet<String> = BTreeSet::new();
    if !direct_set.is_empty() {
        eprintln!(
            "building {} mutant(s) of `impl Trait` functions one at a time",
            direct_set.len()
        );
        for m in &direct_set {
            let Some(e) = m.edits.first() else { continue };
            let tests = map.tests_entering(&[e.site_id]);
            if tests.is_empty() {
                // No test enters it: NotCovered at test time, no build needed.
                direct.insert(m.id.clone());
                continue;
            }
            report.builds += 1;
            match build_direct(cfg, &map, &originals, &isolated.sites, e)? {
                Ok(executables) => {
                    let refs: Vec<&TestInfo> = tests.iter().collect();
                    stage_tests(
                        cfg,
                        &map.worker.join("target"),
                        &direct_dir(paths, generation, &m.id),
                        &refs,
                        &executables,
                    )?;
                    direct.insert(m.id.clone());
                }
                Err(why) => {
                    unviable.insert(m.id.clone(), why);
                }
            }
        }
        restore_files(&map.worker, &originals)?;
        apply_all(&map.worker, &isolated.sites, &map.crate_roots)?;
        report.builds += 1;
        let back = build_schema(cfg, &map.worker, &map.test_packages, &AtomicU64::new(0))?;
        if !back.success {
            bail!(
                "the main generation no longer builds after the `impl Trait` builds:\n{}",
                compiler_errors(&back.stdout, &back.stderr)
            );
        }
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
                direct: direct.contains(&r.mutant.id),
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
        note: None,
        proptest_rerun_cases: None,
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
            match v.message.as_deref() {
                Some(m) if m.starts_with(crate::types::TIMEOUT_PREFIX) => " (timeout)",
                Some(m) if m.starts_with(crate::types::CRASH_PREFIX) => " (crash)",
                _ => "",
            }
        ),
        VerdictKind::Missed => {
            let mut line = format!(
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
            );
            if v.note.is_some() {
                line.push_str(&match v.proptest_rerun_cases {
                    Some(n) => format!(
                        "\n      proptest properties only: still missed with PROPTEST_CASES={n}"
                    ),
                    None => "\n      proptest properties only: the verdict can change between runs (try `test --proptest-cases 1024`)".to_string(),
                });
            }
            if let Some(intent) = &m.intent {
                line.push_str(&format!("\n      intent: {intent}"));
            }
            for e in &m.edits {
                line.push_str(&format!("\n      {}:{}\n", e.path, e.line));
                line.push_str(
                    crate::linediff::render(&e.original, &e.body, "      ").trim_end_matches('\n'),
                );
            }
            line
        }
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

/// A mutant ready to test: its activation, and whether it was built on its
/// own.
type BuiltMutant = (EditedMutant, Vec<(u32, u32)>, bool);

/// Run every mutant built into the current generation (or those whose id
/// starts with one of `only`) against the tests that entered its sites.
/// With `proptest_cases` above zero, a survivor judged only by proptest
/// properties is run once more with `PROPTEST_CASES` set to it.
pub fn test(
    cfg: &Config,
    paths: &Paths,
    only: &[String],
    proptest_cases: u32,
) -> Result<Vec<(EditedMutant, Verdict)>> {
    let _lock = StateLock::acquire(paths)?;
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
    let set: Vec<BuiltMutant> = state
        .records
        .iter()
        .filter(|r| only.is_empty() || only.iter().any(|p| r.mutant.id.starts_with(p.as_str())))
        .filter_map(|r| match &r.status {
            Status::Built {
                generation: g,
                activation,
                direct,
            } if *g == generation => Some((r.mutant.clone(), activation.clone(), *direct)),
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
                let Some((m, activation, direct)) = set.get(k) else {
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
                        note: None,
                        proptest_rerun_cases: None,
                    }
                } else {
                    let planned = PlannedMutant {
                        mutant: representative(&map, m),
                        estimated_cost_secs: tests.iter().map(|t| t.duration_secs).sum(),
                        tests,
                    };
                    let run_with = |extra: &[(String, String)]| {
                        if *direct {
                            let mut env = vec![(DIRECT_MUTANT_ENV.to_string(), m.id.clone())];
                            env.extend(extra.iter().cloned());
                            run_mutant_tests_against(
                                &planned,
                                &direct_dir(paths, generation, &m.id),
                                Some(&run_dir),
                                &map.worker,
                                &policy,
                                &env,
                                false,
                            )
                        } else {
                            let mut env = vec![qs_mutants_env(activation)];
                            env.extend(extra.iter().cloned());
                            run_mutant_tests_observed(
                                &planned,
                                &run_dir,
                                &map.worker,
                                &policy,
                                &env,
                                false,
                            )
                        }
                    };
                    let mut verdict = match run_with(&[]) {
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
                                note: None,
                                proptest_rerun_cases: None,
                            }
                        }
                    };
                    // Properties run random cases: a survivor they alone
                    // judged may fall to more cases, and either verdict can
                    // change between runs.
                    let property_only = planned
                        .tests
                        .iter()
                        .all(|t| map.property_tests.contains(&t.id));
                    if verdict.kind == VerdictKind::Missed && property_only && proptest_cases > 0 {
                        info!(
                            "{}: only proptest properties judged it; running them again with PROPTEST_CASES={proptest_cases}",
                            m.id
                        );
                        let cases = proptest_cases.to_string();
                        match run_with(&[("PROPTEST_CASES".to_string(), cases)]) {
                            Ok(run) if run.outcome.kind == VerdictKind::Caught => {
                                verdict = verdict_from(
                                    &run.outcome,
                                    &run,
                                    started.elapsed().as_secs_f64(),
                                );
                                verdict.message = Some(format!(
                                    "caught only with PROPTEST_CASES={proptest_cases}; the default number of cases missed it"
                                ));
                                verdict.proptest_rerun_cases = Some(proptest_cases);
                            }
                            Ok(run) if run.outcome.kind == VerdictKind::Missed => {
                                verdict.proptest_rerun_cases = Some(proptest_cases);
                            }
                            Ok(run) => warn!(
                                "{}: the PROPTEST_CASES={proptest_cases} run ended {:?}; the verdict stays the one with default cases",
                                m.id, run.outcome.kind
                            ),
                            Err(err) => warn!(
                                "{}: the PROPTEST_CASES={proptest_cases} run failed: {err:#}",
                                m.id
                            ),
                        }
                    }
                    let decided_by_property = match verdict.kind {
                        VerdictKind::Missed => property_only,
                        VerdictKind::Caught => verdict.test.as_deref().is_some_and(|name| {
                            planned
                                .tests
                                .iter()
                                .any(|t| t.name == name && map.property_tests.contains(&t.id))
                        }),
                        _ => false,
                    };
                    if decided_by_property {
                        verdict.note = Some(crate::proptest::NOTE.to_string());
                    }
                    verdict
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
