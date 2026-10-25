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
use std::io::Write as _;
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
    Unviable { message: String },
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
        /// Absent in old states, whose binaries cannot safely be reused.
        #[serde(default)]
        built: Option<TestedBuild>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestedBuild {
    pub generation: u32,
    pub activation: Vec<(u32, u32)>,
    pub direct: bool,
}

impl Status {
    fn tested(&self, verdict: Verdict) -> Self {
        let built = match self {
            Status::Built {
                generation,
                activation,
                direct,
            } => Some(TestedBuild {
                generation: *generation,
                activation: activation.clone(),
                direct: *direct,
            }),
            _ => None,
        };
        Status::Tested { verdict, built }
    }
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
    /// That map's commit and test packages. A new map of the same commit
    /// keeps the records; see [`State::reconcile`].
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub test_packages: Vec<String>,
    /// That map's `[build-env]`: other build settings (a `--cfg` in
    /// `RUSTFLAGS`) also make the compiled generation stale.
    #[serde(default)]
    pub build_env: BTreeMap<String, String>,
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

    /// Verdicts `test` decided since the state was last saved, one JSON
    /// object per line, so that a stopped `test` keeps them.
    pub fn journal_path(paths: &Paths) -> PathBuf {
        paths.state.join("verdicts.jsonl")
    }

    /// The saved state, with the verdicts of the journal applied.
    pub fn load(paths: &Paths) -> Result<State> {
        let p = Self::path(paths);
        let mut state: State = if p.is_file() {
            let text =
                std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
            serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))?
        } else {
            State::default()
        };
        if let Ok(text) = std::fs::read_to_string(Self::journal_path(paths)) {
            // A run stopped while writing leaves a partial last line.
            for entry in text
                .lines()
                .filter_map(|l| serde_json::from_str::<JournalEntry>(l).ok())
            {
                state.apply(entry);
            }
        }
        Ok(state)
    }

    fn apply(&mut self, entry: JournalEntry) {
        if entry.map_id != self.map_id {
            return;
        }
        if let Some(r) = self.records.iter_mut().find(|r| r.mutant.id == entry.id) {
            if matches!(r.status, Status::Built { generation, .. } if generation == entry.generation)
            {
                r.status = r.status.tested(entry.verdict);
            }
        }
    }

    /// Write the state; the journal's verdicts are in it now.
    pub fn save(&self, paths: &Paths) -> Result<()> {
        crate::util::write_atomic(&Self::path(paths), serde_json::to_vec_pretty(self)?)?;
        match std::fs::remove_file(Self::journal_path(paths)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).context("remove the verdict journal")
            }
            _ => Ok(()),
        }
    }

    pub fn find(&self, id: &str) -> Option<&Record> {
        self.records.iter().find(|r| r.mutant.id == id)
    }

    /// Bind the records to `map`. A map of another commit drops them all:
    /// their sites and binaries no longer exist. A new map of the same
    /// commit has the same sites, so the records stay (any whose sites are
    /// gone, for example after mapping other packages, are dropped). With
    /// the same test packages, the compiled generation and the verdicts
    /// stay valid too. With other test packages, the generation lacks the
    /// new test binaries: unviable mutants and kills stay, and every other
    /// mutant goes back to pending, to be compiled by the next `build` and
    /// tested against the new map's tests.
    fn reconcile(&mut self, map: &SiteMap) -> Reconciled {
        if self.map_id == map.map_id {
            return Reconciled::default();
        }
        if self.commit.is_empty() || self.commit != map.commit {
            let dropped = self.records.len();
            *self = State {
                map_id: map.map_id.clone(),
                commit: map.commit.clone(),
                test_packages: map.test_packages.clone(),
                build_env: map.build_env.clone(),
                generation: 0,
                records: Vec::new(),
            };
            return Reconciled {
                dropped,
                ..Reconciled::default()
            };
        }
        let before = self.records.len();
        self.records.retain(|r| {
            r.mutant.edits.iter().all(|e| {
                map.site(e.site_id)
                    .is_some_and(|s| s.path == e.path && s.fqn == e.fqn)
            })
        });
        let mut done = Reconciled {
            dropped: before - self.records.len(),
            kept: self.records.len(),
            ..Reconciled::default()
        };
        if self.test_packages != map.test_packages || self.build_env != map.build_env {
            for r in &mut self.records {
                let stays = match &r.status {
                    Status::Pending | Status::Unviable { .. } => true,
                    Status::Tested { verdict, .. } => verdict.kind == VerdictKind::Caught,
                    Status::Built { .. } => false,
                };
                if !stays {
                    r.status = Status::Pending;
                    done.recompile += 1;
                }
            }
        }
        self.map_id = map.map_id.clone();
        self.test_packages = map.test_packages.clone();
        self.build_env = map.build_env.clone();
        done
    }
}

/// What binding the state to a new map did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Reconciled {
    /// Records dropped: captured against another commit, or naming sites
    /// the new map does not have.
    pub dropped: usize,
    /// Records kept from an earlier map of the same commit.
    pub kept: usize,
    /// Kept records that need compiling again, because the new map has
    /// other test packages.
    pub recompile: usize,
}

impl Reconciled {
    /// Say what happened, if anything did.
    fn report(&self) {
        if self.dropped > 0 {
            eprintln!(
                "discarded {} mutant(s) captured against an earlier map",
                self.dropped
            );
        }
        if self.kept > 0 && self.recompile > 0 {
            eprintln!(
                "kept {} mutant(s) captured against an earlier map of this commit; {} of them will be compiled again for the new map's test packages or build settings and tested against its tests (kills and unviable mutants stay)",
                self.kept, self.recompile
            );
        } else if self.kept > 0 {
            eprintln!(
                "kept {} mutant(s), their builds and verdicts from an earlier map of this commit with the same test packages and build settings",
                self.kept
            );
        }
    }
}

/// One line of the verdict journal.
#[derive(Debug, Serialize, Deserialize)]
struct JournalEntry {
    map_id: String,
    generation: u32,
    id: String,
    verdict: Verdict,
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
    let mut listed = tests;
    let skipped_tests = crate::index::drop_skipped(cfg, &mut listed)?;
    if !skipped_tests.is_empty() {
        info!(
            "map: {} test(s) left out by [map] skip-tests",
            skipped_tests.len()
        );
    }
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
    for line in
        crate::exec::test::describe_not_green(&recording.not_green, &recording.not_green_output, 8)
    {
        warn!("  not green under the schema binary: {line}");
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
        build_env: cfg.build_env.clone(),
        tests,
        hits,
        cpu_secs_by_test: recording.cpu_secs_by_test.clone().into_iter().collect(),
        not_green: recording.not_green.clone(),
        skipped_tests,
        not_green_output: recording.not_green_output.clone(),
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
    let reconciled = state.reconcile(&map);
    reconciled.report();
    let discarded_records = reconciled.dropped;
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

/// An explicit patch and optional file of notes for only that patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchSource {
    pub patch: PathBuf,
    pub intents: Option<PathBuf>,
}

impl From<PathBuf> for PatchSource {
    fn from(patch: PathBuf) -> Self {
        Self {
            patch,
            intents: None,
        }
    }
}

impl std::str::FromStr for PatchSource {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match value.rsplit_once('=') {
            Some((patch, intents)) => Self {
                patch: patch.into(),
                intents: Some(intents.into()),
            },
            None => PathBuf::from(value).into(),
        })
    }
}

/// Where `build` takes edits from: the checkout, patch files, or neither
/// (compile what is pending).
#[derive(Debug, Clone, Default)]
pub struct CaptureSources {
    pub checkout: Option<PathBuf>,
    pub patches: Vec<PatchSource>,
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

const SINGLE_TOKEN_MIN_MUTANTS: usize = 10;
const SINGLE_TOKEN_WARNING_PERCENT: usize = 80;

/// Compare token text, keeping group boundaries but ignoring whitespace.
fn changed_token_count(original: &str, body: &str) -> Option<usize> {
    fn flatten(stream: proc_macro2::TokenStream, out: &mut Vec<String>) {
        use proc_macro2::{Delimiter, TokenTree};
        for token in stream {
            match token {
                TokenTree::Group(group) => {
                    let delimiters = match group.delimiter() {
                        Delimiter::Parenthesis => Some(("(", ")")),
                        Delimiter::Brace => Some(("{", "}")),
                        Delimiter::Bracket => Some(("[", "]")),
                        Delimiter::None => None,
                    };
                    if let Some((open, _)) = delimiters {
                        out.push(open.into());
                    }
                    flatten(group.stream(), out);
                    if let Some((_, close)) = delimiters {
                        out.push(close.into());
                    }
                }
                leaf => out.push(leaf.to_string()),
            }
        }
    }
    let mut before = Vec::new();
    let mut after = Vec::new();
    flatten(original.parse().ok()?, &mut before);
    flatten(body.parse().ok()?, &mut after);
    (before.len() == after.len()).then(|| before.iter().zip(&after).filter(|(a, b)| a != b).count())
}

fn changes_single_token(mutant: &EditedMutant) -> bool {
    mutant.edits.iter().try_fold(0usize, |total, edit| {
        Some(total + changed_token_count(&edit.original, &edit.body)?)
    }) == Some(1)
}

fn report_single_token_changes(source: &str, outcome: &CaptureOutcome) {
    let total = outcome.mutants.len();
    if total < SINGLE_TOKEN_MIN_MUTANTS {
        return;
    }
    let single = outcome
        .mutants
        .iter()
        .filter(|m| changes_single_token(m))
        .count();
    eprintln!("{source}: {single} of {total} mutants change a single token");
    if single * 100 >= total * SINGLE_TOKEN_WARNING_PERCENT {
        eprintln!("warning: most of {source}'s mutants change a single token, such as one operator; check that each function was read and given a realistic fault");
    }
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
        if let Some(kept) = state.records.iter_mut().find(|r| r.mutant.id == m.id) {
            if let Some(note) = &m.intent {
                match &kept.mutant.intent {
                    None => {
                        kept.mutant.intent = Some(note.clone());
                        // Keep the report in step with a duplicate from a later source.
                        if let Some(captured) = report.captured.iter_mut().find(|c| c.id == m.id) {
                            captured.intent = Some(note.clone());
                        }
                        eprintln!(
                            "  {} from {source}: added duplicate's note to kept mutant",
                            m.id
                        );
                    }
                    Some(existing) if existing != note => {
                        eprintln!("  {} from {source}: kept existing note instead of duplicate's different note", m.id);
                    }
                    Some(_) => {}
                }
            }
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
    if cfg.build_env != map.build_env {
        warn!(
            "[build-env] in quicksilver.toml differs from the one `map` used, so these binaries may not behave as the map recorded; run `cargo quicksilver map` again"
        );
    }
    let mut captures: Vec<(String, CaptureOutcome)> = Vec::new();
    if let Some(checkout) = &sources.checkout {
        let outcome = capture_checkout(&map, checkout, sources.options)?;
        captures.push((checkout.display().to_string(), outcome));
    }
    for patch in &sources.patches {
        let outcome = capture_patch(&map, &cfg.repo_root, &patch.patch, sources.options)?;
        captures.push((patch.patch.display().to_string(), outcome));
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
    let mut overrides = 0usize;
    let offset = usize::from(sources.checkout.is_some());
    let functions: Vec<_> = map
        .sites
        .iter()
        .map(|s| (s.fqn.as_str(), s.path.as_str(), (s.start_line, s.end_line)))
        .collect();
    for (patch, (source, outcome)) in sources.patches.iter().zip(captures.iter_mut().skip(offset)) {
        let Some(path) = &patch.intents else { continue };
        let intents = read_intents(path)?;
        let mut scoped = outcome.mutants.clone();
        for m in &mut scoped {
            m.intent = None;
        }
        let spans: Vec<_> = scoped
            .iter()
            .map(|m| {
                m.edits
                    .first()
                    .and_then(|e| map.site(e.site_id))
                    .map_or((0, 0), |s| (s.start_line, s.end_line))
            })
            .collect();
        for warning in
            crate::capture::attach_intents(&mut scoped, &spans, &intents, &map.prefix, &functions)
        {
            eprintln!("  {source}: {warning}");
        }
        for (kept, local) in outcome.mutants.iter_mut().zip(scoped) {
            if local.intent.is_some() {
                overrides += usize::from(kept.intent.is_some());
                kept.intent = local.intent;
            }
        }
    }
    if overrides > 0 {
        eprintln!("{overrides} per-patch intent(s) took precedence over global intents");
    }
    let captured_any = !captures.is_empty();
    for (source, outcome) in captures {
        report_single_token_changes(&source, &outcome);
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

/// Tests are named by their id, `<binary id>::<name>` as in the map's test
/// list: two binaries of one run (with `--extra-tests`) can share a name.
fn verdict_from(
    outcome: &crate::types::Outcome,
    run: &crate::exec::test::ObservedRun,
    secs: f64,
) -> Verdict {
    let id = |o: &crate::types::TestObservation| {
        if o.test_id.is_empty() {
            o.test_name.clone()
        } else {
            o.test_id.clone()
        }
    };
    // The outcome names the deciding test only by name: find its
    // observation, the one whose verdict is the outcome's.
    let test = outcome.test.as_deref().map(|name| {
        let wanted = crate::exec::test::verdict_for_outcome(outcome);
        let named = |o: &&crate::types::TestObservation| o.test_name == name;
        run.observations
            .iter()
            .filter(named)
            .find(|o| o.verdict == wanted)
            .or_else(|| run.observations.iter().find(named))
            .map_or_else(|| name.to_string(), id)
    });
    Verdict {
        kind: outcome.kind,
        test,
        message: outcome.message.clone(),
        tests_run: run.observations.iter().map(id).collect(),
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

/// How often a long `test` reports how far it is.
const STATUS_EVERY: Duration = Duration::from_secs(300);
/// Seconds added per selected test for starting its process.
const PER_TEST_OVERHEAD_SECS: f64 = 0.02;

/// What a mutant is expected to cost: its selected tests' recorded
/// durations, which a survivor runs in full.
fn mutant_cost(tests: &[TestInfo]) -> f64 {
    tests
        .iter()
        .map(|t| t.duration_secs + PER_TEST_OVERHEAD_SECS)
        .sum()
}

/// Verdict counts while `test` runs, for its status lines.
struct Tally {
    total: usize,
    total_cost: f64,
    decided: usize,
    decided_cost: f64,
    caught: usize,
    missed: usize,
    other: usize,
    last_status_secs: f64,
}

impl Tally {
    fn new(total: usize, total_cost: f64) -> Tally {
        Tally {
            total,
            total_cost,
            decided: 0,
            decided_cost: 0.0,
            caught: 0,
            missed: 0,
            other: 0,
            last_status_secs: 0.0,
        }
    }

    /// Count one verdict, `elapsed` seconds into the run; a status line
    /// when one is due.
    fn decided(&mut self, kind: VerdictKind, cost: f64, elapsed: f64) -> Option<String> {
        self.decided += 1;
        self.decided_cost += cost;
        match kind {
            VerdictKind::Caught => self.caught += 1,
            VerdictKind::Missed => self.missed += 1,
            _ => self.other += 1,
        }
        if elapsed - self.last_status_secs < STATUS_EVERY.as_secs_f64()
            || self.decided == self.total
        {
            return None;
        }
        self.last_status_secs = elapsed;
        Some(self.status(elapsed))
    }

    fn counts(&self) -> String {
        format!(
            "{} caught, {} missed, {} other",
            self.caught, self.missed, self.other
        )
    }

    fn status(&self, elapsed: f64) -> String {
        let mut line = format!(
            "test: {} of {} decided ({})",
            self.decided,
            self.total,
            self.counts()
        );
        if let Some(left) = time_left(elapsed, self.decided_cost, self.total_cost) {
            line.push_str(&format!(
                ", about {} left",
                crate::util::format_duration(left)
            ));
        }
        line
    }
}

/// Seconds left, if the work still to do goes at the rate the decided work
/// went. Work is measured by [`mutant_cost`], so a few slow survivors left
/// at the end count for what they are.
fn time_left(elapsed: f64, decided_cost: f64, total_cost: f64) -> Option<f64> {
    (decided_cost > 0.0 && elapsed > 0.0)
        .then(|| elapsed * (total_cost - decided_cost).max(0.0) / decided_cost)
}

/// Restore selected verdicts only when their actual staged executables remain.
fn prepare_again(
    state: &mut State,
    paths: &Paths,
    only: &[String],
    again: &[crate::cli::AgainKind],
) -> usize {
    use crate::cli::AgainKind;
    let binaries_exist = |dir: &Path| {
        std::fs::read_to_string(dir.join("manifest.json"))
            .ok()
            .and_then(|text| {
                serde_json::from_str::<crate::exec::test::BinariesManifest>(&text).ok()
            })
            .is_some_and(|manifest| {
                !manifest.tests.is_empty()
                    && manifest.tests.iter().all(|t| dir.join(&t.binary).is_file())
            })
    };
    let mut changed = 0;
    for r in &mut state.records {
        if !only.is_empty() && !only.iter().any(|p| r.mutant.id.starts_with(p)) {
            continue;
        }
        let Status::Tested { verdict, built } = &r.status else {
            continue;
        };
        let selected = again.iter().any(|kind| match kind {
            AgainKind::Timeouts => {
                verdict.kind == VerdictKind::Timeout
                    || (verdict.kind == VerdictKind::Caught
                        && verdict
                            .message
                            .as_deref()
                            .is_some_and(|m| m.starts_with(crate::types::TIMEOUT_PREFIX)))
            }
            AgainKind::Errored => verdict.kind == VerdictKind::Errored,
        });
        if !selected {
            continue;
        }
        r.status = match built {
            Some(b)
                if b.generation == state.generation
                    && binaries_exist(&generation_dir(paths, b.generation))
                    && (!b.direct
                        || binaries_exist(&direct_dir(paths, b.generation, &r.mutant.id))) =>
            {
                Status::Built {
                    generation: b.generation,
                    activation: b.activation.clone(),
                    direct: b.direct,
                }
            }
            _ => Status::Pending,
        };
        changed += 1;
    }
    changed
}

/// Run every mutant built into the current generation (or those whose id
/// starts with one of `only`) against the tests that entered its sites.
/// With `proptest_cases` above zero, a survivor judged only by proptest
/// properties is run once more with `PROPTEST_CASES` set to it.
pub fn test(
    cfg: &Config,
    paths: &Paths,
    only: &[String],
    proptest_cases: u32,
    again: &[crate::cli::AgainKind],
) -> Result<Vec<(EditedMutant, Verdict)>> {
    let _lock = StateLock::acquire(paths)?;
    let map = SiteMap::load(paths)?
        .context("no map in this scratch session; run `cargo quicksilver map` first")?;
    let mut state = State::load(paths)?;
    let reconciled = state.reconcile(&map);
    if reconciled != Reconciled::default() {
        reconciled.report();
        state.save(paths)?;
    }
    if prepare_again(&mut state, paths, only, again) > 0 {
        state.save(paths)?;
    }
    let pending = state
        .records
        .iter()
        .filter(|r| matches!(r.status, Status::Pending))
        .count();
    if pending > 0 {
        eprintln!("{pending} captured mutant(s) are not compiled yet; run `cargo quicksilver build` to test them");
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
    // Each verdict is appended as soon as it is decided, so a run that is
    // stopped keeps what it decided: the next `test` skips those mutants.
    let journal = Mutex::new(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(State::journal_path(paths))
            .context("open the verdict journal")?,
    );
    let stop = crate::engine::install_ctrlc();
    let stopping = std::sync::Once::new();
    let costs: Vec<f64> = set
        .iter()
        .map(|(m, _, _)| {
            let sites: Vec<u32> = m.edits.iter().map(|e| e.site_id).collect();
            mutant_cost(&map.tests_entering(&sites))
        })
        .collect();
    let tally = Mutex::new(Tally::new(set.len(), costs.iter().sum()));
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(EditedMutant, Verdict)>> = Mutex::new(Vec::new());
    let errors: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let progress = crate::progress::Progress::new("testing", Some(set.len() as u64), cfg.quiet);
    std::thread::scope(|scope| {
        for _ in 0..cfg.test_jobs.max(1).min(set.len()) {
            scope.spawn(|| loop {
                if stop.load(Ordering::SeqCst) {
                    stopping.call_once(|| {
                        progress.println(
                            "stopping: the mutants already started finish; their verdicts and the earlier ones are kept (interrupt again to quit at once)",
                        )
                    });
                    break;
                }
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
                        VerdictKind::Caught => verdict
                            .test
                            .as_deref()
                            .is_some_and(|id| map.property_tests.contains(id)),
                        _ => false,
                    };
                    if decided_by_property {
                        verdict.note = Some(crate::proptest::NOTE.to_string());
                    }
                    verdict
                };
                progress.println(&verdict_line(m, &verdict));
                progress.inc(1);
                if let Some(line) =
                    tally
                        .lock()
                        .unwrap()
                        .decided(verdict.kind, costs[k], progress.elapsed_secs())
                {
                    progress.println(&line);
                }
                progress.set_message(&tally.lock().unwrap().counts());
                let entry = JournalEntry {
                    map_id: map.map_id.clone(),
                    generation,
                    id: m.id.clone(),
                    verdict: verdict.clone(),
                };
                if let Err(err) = serde_json::to_string(&entry)
                    .map_err(std::io::Error::from)
                    .and_then(|line| writeln!(journal.lock().unwrap(), "{line}"))
                {
                    errors
                        .lock()
                        .unwrap()
                        .push(format!("{}: cannot save its verdict: {err}", m.id));
                }
                results.lock().unwrap().push((m.clone(), verdict));
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    if stop.load(Ordering::SeqCst) {
        eprintln!(
            "stopped after {} of {} mutant(s); run `cargo quicksilver test` again to test the rest",
            results.len(),
            set.len()
        );
    }
    let count = |k: VerdictKind| results.iter().filter(|(_, v)| v.kind == k).count();
    progress.finish(&format!(
        "tested {} mutant(s) in {:.1}s: {} caught{}, {} missed, {} not covered, {} timeout, {} errored",
        results.len(),
        progress.elapsed_secs(),
        count(VerdictKind::Caught),
        crate::report::caught_details(
            results.iter().filter(|(_, v)| v.kind == VerdictKind::Caught && v.message.as_deref().is_some_and(|m| m.starts_with(crate::types::TIMEOUT_PREFIX))).count(),
            results.iter().filter(|(_, v)| v.kind == VerdictKind::Caught && v.message.as_deref().is_some_and(|m| m.starts_with(crate::types::CRASH_PREFIX))).count()),
        count(VerdictKind::Missed),
        count(VerdictKind::NotCovered),
        count(VerdictKind::Timeout),
        count(VerdictKind::Errored)
    ));
    results.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    let errored: Vec<_> = results
        .iter()
        .filter(|(_, v)| v.kind == VerdictKind::Errored)
        .collect();
    if !errored.is_empty() {
        eprintln!("errored mutants:");
        for (m, v) in errored {
            eprintln!(
                "  {}: {}",
                m.id,
                v.message.as_deref().unwrap_or("test run error")
            );
        }
        eprintln!("retry these mutants with `cargo quicksilver test --again errored`");
    }
    for (m, v) in &results {
        if let Some(r) = state.records.iter_mut().find(|r| r.mutant.id == m.id) {
            r.status = r.status.tested(v.clone());
        }
    }
    state.save(paths)?;
    for e in errors.into_inner().unwrap() {
        warn!("test run error: {e}");
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    #[test]
    fn token_comparison_counts_text_changes_and_rejects_invalid_tokens() {
        for (before, after, expected) in [
            ("{ a + b }", "{ a - b }", Some(1)),
            ("{ 42 }", "{ 43 }", Some(1)),
            ("{ first }", "{ second }", Some(1)),
            ("{ a + b }", "{ a - c }", Some(2)),
            ("{ a }", "{ a + b }", None),
            ("{ a }", "{ ", None),
            ("{ ", "{ a }", None),
            ("{ (a) }", "{ [a] }", Some(2)),
            ("{ a + b }", "{a /* same tokens */ + b}", Some(0)),
        ] {
            assert_eq!(
                changed_token_count(before, after),
                expected,
                "{before} -> {after}"
            );
        }
    }

    #[test]
    fn single_token_count_sums_all_edits() {
        let edit = |original: &str, body: &str| crate::capture::BodyEdit {
            site_id: 0,
            package: "p".into(),
            path: "src/lib.rs".into(),
            fqn: "p.f".into(),
            line: 1,
            original: original.into(),
            body: body.into(),
        };
        let mut mutant = EditedMutant {
            id: "m".into(),
            edits: vec![edit("{ 1 }", "{ 2 }")],
            intent: None,
        };
        assert!(changes_single_token(&mutant));
        mutant.edits.push(edit("{ a }", "{ a }"));
        assert!(changes_single_token(&mutant));
        mutant.edits.push(edit("{ b }", "{ c }"));
        assert!(!changes_single_token(&mutant));
        mutant.edits[2] = edit("{ b }", "{ ");
        assert!(!changes_single_token(&mutant));
    }

    #[test]
    fn duplicate_notes_fill_missing_notes_and_keep_the_first_note() {
        let mut state = State::default();
        let mut report = BuildReport {
            captured: vec![],
            duplicates: vec![],
            rejected: vec![],
            dropped: vec![],
            results: vec![],
            generation: 0,
            builds: 0,
            secs: 0.0,
            discarded_records: 0,
        };
        for note in [None, Some("first note"), Some("different note")] {
            record_capture(
                &mut state,
                "lane.patch",
                CaptureOutcome {
                    mutants: vec![EditedMutant {
                        id: "same".into(),
                        edits: vec![],
                        intent: note.map(str::to_owned),
                    }],
                    ..CaptureOutcome::default()
                },
                &mut report,
            );
        }
        assert_eq!(state.records.len(), 1);
        assert_eq!(
            state.records[0].mutant.intent.as_deref(),
            Some("first note")
        );
        assert_eq!(report.captured[0].intent.as_deref(), Some("first note"));
        assert_eq!(report.duplicates.len(), 2);
    }

    #[test]
    fn patch_source_splits_at_last_equals_only() {
        let plain: super::PatchSource = "lane.patch".parse().unwrap();
        assert_eq!(plain.patch, std::path::PathBuf::from("lane.patch"));
        assert_eq!(plain.intents, None);
        let scoped: super::PatchSource = "lane=2.patch=notes.json".parse().unwrap();
        assert_eq!(scoped.patch, std::path::PathBuf::from("lane=2.patch"));
        assert_eq!(scoped.intents, Some("notes.json".into()));
    }

    use super::*;
    use crate::types::{Mutant, Outcome, TestObservation, TestVerdict};

    fn observed(id: &str, name: &str, verdict: TestVerdict) -> TestObservation {
        TestObservation {
            test_id: id.into(),
            test_name: name.into(),
            verdict,
            wall_secs: 0.1,
        }
    }

    fn site(id: u32, path: &str, fqn: &str) -> crate::sitemap::MapSite {
        crate::sitemap::MapSite {
            site_id: id,
            salt: 0,
            package: "p".into(),
            path: path.into(),
            fqn: fqn.into(),
            fn_start_byte: 0,
            body_start_byte: 0,
            body_end_byte: 1,
            start_line: 1,
            end_line: 3,
            opaque_return: false,
            trait_impl: false,
            scaffolding: None,
        }
    }

    fn map_of(map_id: &str, commit: &str, test_packages: &[&str]) -> SiteMap {
        SiteMap {
            version: crate::sitemap::MAP_VERSION,
            map_id: map_id.into(),
            commit: commit.into(),
            prefix: String::new(),
            repo_root: PathBuf::new(),
            target: String::new(),
            files: BTreeMap::new(),
            sites: vec![site(1, "src/a.rs", "p.a"), site(2, "src/b.rs", "p.b")],
            test_lines: Default::default(),
            dropped: Vec::new(),
            crate_roots: BTreeMap::new(),
            test_packages: test_packages.iter().map(|p| p.to_string()).collect(),
            build_env: BTreeMap::new(),
            tests: Vec::new(),
            hits: BTreeMap::new(),
            cpu_secs_by_test: Default::default(),
            not_green: Vec::new(),
            skipped_tests: Vec::new(),
            not_green_output: Default::default(),
            property_tests: Default::default(),
            worker: PathBuf::new(),
            run_dir: PathBuf::new(),
            map_secs: 0.0,
        }
    }

    fn record(id: &str, site_id: u32, fqn: &str, status: Status) -> Record {
        Record {
            mutant: EditedMutant {
                id: id.into(),
                edits: vec![crate::capture::BodyEdit {
                    site_id,
                    package: "p".into(),
                    path: if site_id == 1 { "src/a.rs" } else { "src/b.rs" }.into(),
                    fqn: fqn.into(),
                    line: 1,
                    original: "{ 1 }".into(),
                    body: "{ 2 }".into(),
                }],
                intent: None,
            },
            status,
            captured_at: String::new(),
        }
    }

    fn verdict(kind: VerdictKind) -> Verdict {
        Verdict {
            kind,
            test: None,
            message: None,
            tests_run: Vec::new(),
            secs: 0.0,
            note: None,
            proptest_rerun_cases: None,
        }
    }

    fn built() -> Status {
        Status::Built {
            generation: 3,
            activation: vec![(1, 1)],
            direct: false,
        }
    }

    fn state_for(map: &SiteMap) -> State {
        State {
            map_id: map.map_id.clone(),
            commit: map.commit.clone(),
            test_packages: map.test_packages.clone(),
            build_env: map.build_env.clone(),
            generation: 3,
            records: vec![
                record("built", 1, "p.a", built()),
                record(
                    "caught",
                    1,
                    "p.a",
                    Status::Tested {
                        verdict: verdict(VerdictKind::Caught),
                        built: None,
                    },
                ),
                record(
                    "missed",
                    2,
                    "p.b",
                    Status::Tested {
                        verdict: verdict(VerdictKind::Missed),
                        built: None,
                    },
                ),
                record(
                    "unviable",
                    2,
                    "p.b",
                    Status::Unviable {
                        message: "e".into(),
                    },
                ),
                record("pending", 2, "p.b", Status::Pending),
                // Names a function the new map does not have at that site.
                record("gone", 2, "p.renamed", Status::Pending),
            ],
        }
    }

    fn statuses(state: &State) -> Vec<(String, &'static str)> {
        state
            .records
            .iter()
            .map(|r| {
                let s = match &r.status {
                    Status::Pending => "pending",
                    Status::Unviable { .. } => "unviable",
                    Status::Built { .. } => "built",
                    Status::Tested { verdict, .. } if verdict.kind == VerdictKind::Caught => {
                        "caught"
                    }
                    Status::Tested { .. } => "tested",
                };
                (r.mutant.id.clone(), s)
            })
            .collect()
    }

    #[test]
    fn a_new_map_of_the_same_commit_keeps_the_records() {
        let old = map_of("m1", "c1", &["p"]);
        // Same commit and test packages: builds and verdicts stay.
        let mut state = state_for(&old);
        let done = state.reconcile(&map_of("m2", "c1", &["p"]));
        assert_eq!(
            done,
            Reconciled {
                dropped: 1,
                kept: 5,
                recompile: 0
            }
        );
        assert_eq!(state.map_id, "m2");
        assert_eq!(state.generation, 3);
        let s = statuses(&state);
        assert!(s.contains(&("built".into(), "built")));
        assert!(s.contains(&("missed".into(), "tested")));
        // More test packages: the generation lacks their binaries. Kills and
        // unviable mutants stay; the rest are compiled and tested again.
        let mut state = state_for(&old);
        let done = state.reconcile(&map_of("m2", "c1", &["p", "q"]));
        assert_eq!(done.recompile, 2);
        assert_eq!(state.test_packages, vec!["p".to_string(), "q".to_string()]);
        assert_eq!(
            statuses(&state),
            vec![
                ("built".to_string(), "pending"),
                ("caught".to_string(), "caught"),
                ("missed".to_string(), "pending"),
                ("unviable".to_string(), "unviable"),
                ("pending".to_string(), "pending"),
            ]
        );
        // Other build settings, such as a `--cfg` in RUSTFLAGS: the same.
        let mut state = state_for(&old);
        let mut cfg_map = map_of("m2", "c1", &["p"]);
        cfg_map
            .build_env
            .insert("RUSTFLAGS".into(), "--cfg scheduled_tests".into());
        assert_eq!(state.reconcile(&cfg_map).recompile, 2);
        assert_eq!(state.build_env, cfg_map.build_env);
        // Another commit: nothing stays.
        let mut state = state_for(&old);
        let done = state.reconcile(&map_of("m3", "c2", &["p"]));
        assert_eq!(done.dropped, 6);
        assert!(state.records.is_empty());
        assert_eq!((state.commit.as_str(), state.generation), ("c2", 0));
        // A state saved before the commit was recorded is dropped as before.
        let mut state = state_for(&old);
        state.commit.clear();
        assert_eq!(state.reconcile(&map_of("m2", "c1", &["p"])).dropped, 6);
        // The same map changes nothing.
        let mut state = state_for(&old);
        assert_eq!(state.reconcile(&old), Reconciled::default());
    }

    #[test]
    fn again_restores_current_builds_and_requires_build_for_stale_or_missing_ones() {
        use crate::cli::AgainKind;
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::for_tree(tmp.path(), "", tmp.path());
        let dir = generation_dir(&paths, 3);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("test-bin"), b"staged binary").unwrap();
        let manifest = crate::exec::test::BinariesManifest {
            mutant_id: "schema".into(),
            tests: vec![crate::exec::test::ManifestTest {
                name: "t".into(),
                binary: "test-bin".into(),
                cwd_rel: String::new(),
                id: "t".into(),
            }],
        };
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let mut timeout = verdict(VerdictKind::Caught);
        timeout.message = Some(format!("{}: runaway", crate::types::TIMEOUT_PREFIX));
        let mut state = State {
            generation: 3,
            records: vec![
                record("retry-timeout", 1, "p.a", built().tested(timeout)),
                record(
                    "retry-error",
                    1,
                    "p.a",
                    built().tested(verdict(VerdictKind::Errored)),
                ),
                record(
                    "retry-inconclusive",
                    1,
                    "p.a",
                    built().tested(verdict(VerdictKind::Timeout)),
                ),
                record(
                    "retry-missed",
                    1,
                    "p.a",
                    built().tested(verdict(VerdictKind::Missed)),
                ),
                record(
                    "other-timeout",
                    1,
                    "p.a",
                    built().tested(verdict(VerdictKind::Timeout)),
                ),
                record("untested", 1, "p.a", built()),
            ],
            ..State::default()
        };
        let original = state.clone();
        assert_eq!(
            prepare_again(
                &mut state,
                &paths,
                &["retry-".into()],
                &[AgainKind::Timeouts, AgainKind::Errored]
            ),
            3
        );
        for id in [
            "retry-timeout",
            "retry-error",
            "retry-inconclusive",
            "untested",
        ] {
            assert_eq!(state.find(id).unwrap().status, built());
        }
        assert!(matches!(
            state.find("retry-missed").unwrap().status,
            Status::Tested { .. }
        ));
        assert!(matches!(
            state.find("other-timeout").unwrap().status,
            Status::Tested { .. }
        ));
        // The direct flag and activation survive persistence and restoration.
        state = original.clone();
        let direct = Status::Built {
            generation: 3,
            activation: vec![(1, 7)],
            direct: true,
        };
        state.records[0].status = direct.tested(verdict(VerdictKind::Timeout));
        assert_eq!(
            prepare_again(
                &mut state,
                &paths,
                &["retry-timeout".into()],
                &[AgainKind::Timeouts]
            ),
            1
        );
        assert_eq!(state.records[0].status, Status::Pending);
        let direct_dir = direct_dir(&paths, 3, "retry-timeout");
        std::fs::create_dir_all(&direct_dir).unwrap();
        std::fs::copy(dir.join("manifest.json"), direct_dir.join("manifest.json")).unwrap();
        std::fs::write(direct_dir.join("test-bin"), b"direct binary").unwrap();
        state.records[0].status = direct.tested(verdict(VerdictKind::Timeout));
        prepare_again(
            &mut state,
            &paths,
            &["retry-timeout".into()],
            &[AgainKind::Timeouts],
        );
        assert_eq!(state.records[0].status, direct);
        // Earlier generations and old records with no build metadata are pending.
        state = original.clone();
        state.generation = 4;
        prepare_again(&mut state, &paths, &[], &[AgainKind::Timeouts]);
        assert_eq!(state.records[0].status, Status::Pending);
        let old: Status = serde_json::from_value(
            serde_json::json!({"status":"tested", "verdict":verdict(VerdictKind::Errored)}),
        )
        .unwrap();
        state.records[1].status = old;
        prepare_again(&mut state, &paths, &[], &[AgainKind::Errored]);
        assert_eq!(state.records[1].status, Status::Pending);
        state = original;
        std::fs::remove_file(dir.join("test-bin")).unwrap();
        prepare_again(&mut state, &paths, &[], &[AgainKind::Timeouts]);
        assert_eq!(state.records[0].status, Status::Pending);
    }

    #[test]
    fn verdicts_in_the_journal_survive_a_stopped_run() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::for_tree(tmp.path(), "", tmp.path());
        std::fs::create_dir_all(&paths.state).unwrap();
        let map = map_of("m1", "c1", &["p"]);
        let mut state = state_for(&map);
        state.records.push(record("built2", 1, "p.a", built()));
        state.save(&paths).unwrap();
        let line = |map_id: &str, generation: u32, id: &str| {
            serde_json::to_string(&JournalEntry {
                map_id: map_id.into(),
                generation,
                id: id.into(),
                verdict: verdict(VerdictKind::Missed),
            })
            .unwrap()
        };
        let journal = [
            line("m1", 3, "built"),
            // Another map's or generation's verdicts do not apply.
            line("other", 3, "built2"),
            line("m1", 2, "built2"),
            // A run stopped mid-write leaves a partial line.
            line("m1", 3, "built2")[..20].to_string(),
        ]
        .join("\n");
        std::fs::write(State::journal_path(&paths), journal).unwrap();
        let loaded = State::load(&paths).unwrap();
        assert!(
            matches!(&loaded.find("built").unwrap().status, Status::Tested { built: Some(b), .. } if b.generation == 3 && b.activation == vec![(1, 1)] && !b.direct)
        );
        let s = statuses(&loaded);
        assert!(s.contains(&("built".into(), "tested")), "{s:?}");
        assert!(s.contains(&("built2".into(), "built")), "{s:?}");
        loaded.save(&paths).unwrap();
        assert!(!State::journal_path(&paths).exists());
        assert!(statuses(&State::load(&paths).unwrap()).contains(&("built".into(), "tested")));
    }

    #[test]
    fn status_lines_count_verdicts_and_estimate_the_time_left() {
        // Ten mutants: nine cheap, one survivor-sized at the end.
        let mut tally = Tally::new(10, 9.0 + 91.0);
        assert_eq!(
            tally.decided(VerdictKind::Caught, 1.0, 10.0),
            None,
            "not due yet"
        );
        for _ in 0..4 {
            tally.decided(VerdictKind::Missed, 1.0, 20.0);
        }
        // Five minutes in: half the mutants, but 5 of 100 cost units.
        let line = tally
            .decided(VerdictKind::Timeout, 1.0, 300.0)
            .expect("due after five minutes");
        assert!(
            line.starts_with("test: 6 of 10 decided (1 caught, 4 missed, 1 other)"),
            "{line}"
        );
        // 300 s for 6 units leaves 94 units: 4700 s.
        assert!(line.ends_with("about 1.3 h left"), "{line}");
        assert_eq!(tally.decided(VerdictKind::Caught, 1.0, 400.0), None);
        assert_eq!(time_left(10.0, 0.0, 5.0), None);
        assert_eq!(time_left(10.0, 5.0, 5.0), Some(0.0));
    }

    #[test]
    fn verdicts_name_tests_by_id_when_two_binaries_share_a_name() {
        let mutant = Mutant {
            id: "m".into(),
            package: "p".into(),
            path: "src/lib.rs".into(),
            fqn: "p.f".into(),
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            end_line: 1,
            body_start_byte: 0,
            body_end_byte: 1,
            replacement: "{}".into(),
            return_kind: "unit".into(),
        };
        let run = crate::exec::test::ObservedRun {
            outcome: Outcome::caught(mutant, "store::roundtrip".into()),
            observations: vec![
                observed(
                    "p::store::roundtrip",
                    "store::roundtrip",
                    TestVerdict::Passed,
                ),
                observed(
                    "q::suite_persistence::store::roundtrip",
                    "store::roundtrip",
                    TestVerdict::Killed,
                ),
            ],
            complete: false,
        };
        let v = verdict_from(&run.outcome, &run, 1.0);
        assert_eq!(
            v.test.as_deref(),
            Some("q::suite_persistence::store::roundtrip")
        );
        assert_eq!(
            v.tests_run,
            vec![
                "p::store::roundtrip".to_string(),
                "q::suite_persistence::store::roundtrip".to_string()
            ]
        );
    }
}
