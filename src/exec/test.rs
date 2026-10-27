use crate::exec::process::{run_exact_capture_output_with_env, run_exact_with_env, RunStatus};
use crate::types::{Outcome, PlannedMutant, TestObservation, TestVerdict};
use crate::util::duration_from_secs;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BinariesManifest {
    pub mutant_id: String,
    pub tests: Vec<ManifestTest>,
    /// `CARGO_PKG_*` for each package's tests, as cargo and nextest set them.
    #[serde(default)]
    pub package_env: BTreeMap<String, Vec<(String, String)>>,
    /// The toolchain's cargo, given to tests as `CARGO`.
    #[serde(default)]
    pub cargo: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManifestTest {
    pub name: String,
    pub binary: String,
    pub cwd_rel: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub package: String,
}

/// How to budget and confirm one mutant's test run.
pub struct TestRunPolicy {
    /// Never give a test less than this.
    pub floor: Duration,
    /// Multiplier on the green duration for binaries slower than the baseline
    /// ones the durations were measured on.
    pub slowdown: f64,
    /// Re-run a non-passing test, then run it with no mutant active, before
    /// attributing the result to the mutant.
    pub confirm: bool,
    /// Tests found unusable (failing with no mutant active) during this run,
    /// so later mutants skip them instead of checking them again.
    pub unusable: UnusableMemo,
}

/// Tests found unusable under the schema binary, by staged binary and name.
/// Shared by the clones of one policy, and so by one run's mutants.
#[derive(Debug, Clone, Default)]
pub struct UnusableMemo(Arc<Mutex<HashMap<(PathBuf, String), InfraSkip>>>);

impl UnusableMemo {
    fn get(&self, t: &ResolvedTest) -> Option<InfraSkip> {
        let memo = self.0.lock().unwrap_or_else(|e| e.into_inner());
        memo.get(&(t.binary.clone(), t.name.clone())).cloned()
    }

    /// The ids of the tests found unusable so far in this run.
    pub fn ids(&self) -> std::collections::HashSet<String> {
        let memo = self.0.lock().unwrap_or_else(|e| e.into_inner());
        memo.values().map(|s| s.id.clone()).collect()
    }

    fn remember(&self, t: &ResolvedTest, skip: &InfraSkip) {
        let mut memo = self.0.lock().unwrap_or_else(|e| e.into_inner());
        memo.insert((t.binary.clone(), t.name.clone()), skip.clone());
    }
}

/// One `--exact` invocation. `capture` pipes stderr; it stays off for the
/// first pass so the common (passing) case costs exactly what it used to.
#[derive(Debug, Clone)]
pub struct RunSpec {
    pub binary: PathBuf,
    pub test_name: String,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub env: Vec<(String, String)>,
    pub capture: bool,
}

/// A manifest test joined with the green duration from the plan.
#[derive(Debug, Clone)]
struct ResolvedTest {
    id: String,
    name: String,
    binary: PathBuf,
    /// The package directory under the tree root: the directory nextest
    /// would start the test in.
    cwd: PathBuf,
    duration_secs: f64,
    cpu_secs: f64,
    /// What nextest gives the test process besides the caller's environment:
    /// `CARGO`, `CARGO_MANIFEST_DIR`, `CARGO_PKG_*` and `NEXTEST*`.
    env: Vec<(String, String)>,
}

/// One id for this QS process's test runs, as nextest gives each run.
fn run_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let hex = crate::sitemap::random_id() + &crate::sitemap::random_id();
        format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    })
}

/// The environment nextest gives a test of `package` that starts in `cwd`.
fn nextest_like_env(
    manifest: &BinariesManifest,
    package: &str,
    cwd: &Path,
) -> Vec<(String, String)> {
    let mut env = manifest
        .package_env
        .get(package)
        .cloned()
        .unwrap_or_default();
    if let Some(cargo) = &manifest.cargo {
        env.push(("CARGO".into(), cargo.clone()));
        // `cargo` (and `rustc`) on PATH mean the toolchain's too, whatever
        // wrapper (such as mbx's) comes first in the caller's PATH; a test
        // that clears its environment but keeps PATH still finds them.
        if let Some(bin) = Path::new(cargo).parent() {
            if let Some(path) = path_with_first(bin, std::env::var_os("PATH").as_deref()) {
                env.push(("PATH".into(), path.to_string_lossy().into_owned()));
            }
        }
    }
    env.push((
        "CARGO_MANIFEST_DIR".into(),
        cwd.to_string_lossy().into_owned(),
    ));
    env.push(("NEXTEST".into(), "1".into()));
    env.push(("NEXTEST_EXECUTION_MODE".into(), "process-per-test".into()));
    env.push(("NEXTEST_RUN_ID".into(), run_id().into()));
    env
}

/// The environment every test process gets on top of the caller's: the tree's
/// target directory, then `extra` (the mutant switch or the site recorder).
/// `path` with `bin` as its first directory, and nowhere else.
fn path_with_first(bin: &Path, path: Option<&std::ffi::OsStr>) -> Option<std::ffi::OsString> {
    let rest = path
        .map(|p| {
            std::env::split_paths(p)
                .filter(|d| d != bin)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    std::env::join_paths(std::iter::once(bin.to_path_buf()).chain(rest)).ok()
}

/// A test process's variables: what nextest would give it, then QS's own
/// (`extra`). A variable the user sets in `[test-env]` keeps the user's
/// value, because the sandbox applies `[test-env]` before these.
fn test_env(t: &ResolvedTest, extra: &[(String, String)]) -> Vec<(String, String)> {
    let settings = crate::sandbox::current();
    let user_sets = |name: &str| settings.as_ref().is_some_and(|s| s.sets(name));
    let mut env: Vec<(String, String)> = t
        .env
        .iter()
        .filter(|(k, _)| !user_sets(k))
        .cloned()
        .collect();
    env.extend(extra.iter().cloned());
    env
}

#[derive(Debug, Clone)]
pub struct ObservedRun {
    pub outcome: Outcome,
    pub observations: Vec<TestObservation>,
    /// True only when every selected test was executed to a canonical result.
    pub complete: bool,
}

/// Minimum budgets per test id or name, from `[test-budget]` in
/// quicksilver.toml: for tests whose own deadlines (a bounded wait, an
/// `elapsed < 60s` assertion) are longer than three times their duration.
static TEST_BUDGETS: LazyLock<Mutex<HashMap<String, Duration>>> = LazyLock::new(Default::default);

/// Install the per-test minimum budgets for this process.
pub fn set_test_budgets(budgets: &std::collections::BTreeMap<String, f64>) {
    let mut map = TEST_BUDGETS.lock().unwrap_or_else(|e| e.into_inner());
    map.clear();
    for (test, secs) in budgets {
        map.insert(test.clone(), duration_from_secs(*secs));
    }
}

/// `budget`, raised to the test's configured minimum.
fn with_minimum(t: &ResolvedTest, budget: Duration) -> Duration {
    let map = TEST_BUDGETS.lock().unwrap_or_else(|e| e.into_inner());
    [t.id.as_str(), t.name.as_str()]
        .iter()
        .filter_map(|k| map.get(*k))
        .fold(budget, |b, m| b.max(*m))
}

/// CPU growth this large establishes extra computation independently of host load.
const RUNAWAY_CPU_FACTOR: f64 = 3.0;
/// Allow one second of measurement noise for short tests.
const RUNAWAY_CPU_ALLOWANCE: f64 = 1.0;
/// Retry with extra wall time so a loaded host can finish ordinary computation.
const TIMEOUT_RETRY_FACTOR: u32 = 4;
/// Below five percent of a CPU, the test is waiting rather than computing.
const BLOCKED_CPU_SHARE: f64 = 0.05;

/// Cap on the stderr tail copied into an `Outcome` message. The capture
/// buffer holds 64KB; outcomes.jsonl does not need all of it.
const MESSAGE_TAIL: usize = 2000;

pub fn run_mutant_tests_with_env(
    planned: &PlannedMutant,
    run_dir: &Path,
    tree_root: &Path,
    timeout_floor: Duration,
    extra_env: &[(String, String)],
) -> Result<Outcome> {
    let policy = TestRunPolicy {
        floor: timeout_floor,
        slowdown: 1.0,
        confirm: false,
        unusable: Default::default(),
    };
    run_mutant_tests_policy(planned, run_dir, tree_root, &policy, extra_env)
}

pub fn run_mutant_tests_policy(
    planned: &PlannedMutant,
    run_dir: &Path,
    tree_root: &Path,
    policy: &TestRunPolicy,
    extra_env: &[(String, String)],
) -> Result<Outcome> {
    Ok(run_mutant_tests_observed(planned, run_dir, tree_root, policy, extra_env, false)?.outcome)
}

/// Run selected tests and retain their canonical results. `complete_vector`
/// keeps going after a kill/error/timeout; ordinary verdict execution passes
/// false and preserves fail-fast behavior.
pub fn run_mutant_tests_observed(
    planned: &PlannedMutant,
    run_dir: &Path,
    tree_root: &Path,
    policy: &TestRunPolicy,
    extra_env: &[(String, String)],
    complete_vector: bool,
) -> Result<ObservedRun> {
    run_mutant_tests_against(
        planned,
        run_dir,
        None,
        tree_root,
        policy,
        extra_env,
        complete_vector,
    )
}

/// The variable that marks a run of a directly built mutant (one whose
/// binaries contain the mutated body with no switch). Runs without it are
/// the confirmation runs with no mutant active.
pub const DIRECT_MUTANT_ENV: &str = "QS_DIRECT_MUTANT";

/// [`run_mutant_tests_observed`] for binaries that always contain the
/// mutant. `null_run_dir` holds binaries of the same tests without it: the
/// confirmation runs (a failure's null run) use
/// those. `extra_env` must contain [`DIRECT_MUTANT_ENV`].
pub fn run_mutant_tests_against(
    planned: &PlannedMutant,
    run_dir: &Path,
    null_run_dir: Option<&Path>,
    tree_root: &Path,
    policy: &TestRunPolicy,
    extra_env: &[(String, String)],
    complete_vector: bool,
) -> Result<ObservedRun> {
    let tests = resolve_tests(planned, run_dir, tree_root)?;
    let null_binary: HashMap<PathBuf, PathBuf> = match null_run_dir {
        Some(dir) => {
            let nulls = resolve_tests(planned, dir, tree_root)?;
            tests
                .iter()
                .filter_map(|t| {
                    nulls
                        .iter()
                        .find(|n| n.id == t.id && n.name == t.name)
                        .map(|n| (t.binary.clone(), n.binary.clone()))
                })
                .collect()
        }
        None => HashMap::new(),
    };
    let mut run = |spec: &RunSpec| -> Result<(RunStatus, Duration, f64, String)> {
        let mut spec = spec.clone();
        if !null_binary.is_empty() && !spec.env.iter().any(|(k, _)| k == DIRECT_MUTANT_ENV) {
            if let Some(null) = null_binary.get(&spec.binary) {
                spec.binary = null.clone();
            }
        }
        let spec = &spec;
        // libtest prints a failing test's panic and output on stdout, so a
        // confirmation run keeps stdout as well as stderr.
        if spec.capture {
            run_exact_capture_output_with_env(
                &spec.binary,
                &spec.test_name,
                &spec.cwd,
                spec.timeout,
                &spec.env,
            )
        } else {
            let (status, wall, cpu) = run_exact_with_env(
                &spec.binary,
                &spec.test_name,
                &spec.cwd,
                spec.timeout,
                &spec.env,
            )?;
            Ok((status, wall, cpu, String::new()))
        }
    };
    run_selected_tests_observed(
        planned,
        &tests,
        policy,
        extra_env,
        complete_vector,
        &mut run,
    )
}

/// Which mutation sites each staged test entered, from one run of every test
/// with no mutant active and `QS_RECORD_SITES` pointing at a per-test file.
#[derive(Debug, Default)]
pub struct SiteRecording {
    /// Test id (name when the manifest has no id) -> site ids it entered.
    pub sites_by_test: std::collections::HashMap<String, std::collections::BTreeSet<u32>>,
    /// Test id (or name) -> wall seconds of its green run under the schema
    /// binary. Mutant budgets in sites mode come from this, not from the
    /// baseline binary's timing times a guessed schema slowdown.
    pub secs_by_test: std::collections::HashMap<String, f64>,
    pub cpu_secs_by_test: std::collections::HashMap<String, f64>,
    /// Tests that did not pass with no mutant active. They select nothing:
    /// the schema binary cannot judge a mutant with a test that is not green.
    pub not_green: Vec<(String, String)>,
    /// Test id (or name) -> the end of its output, for not-green tests.
    pub not_green_output: std::collections::BTreeMap<String, String>,
    pub wall_secs: f64,
}

impl SiteRecording {
    pub fn recorded_cpu_secs(&self, test: &crate::types::TestInfo) -> Option<f64> {
        [test.id.as_str(), test.name.as_str()]
            .iter()
            .find_map(|key| self.cpu_secs_by_test.get(*key).copied())
    }

    pub fn recorded_secs(&self, test: &crate::types::TestInfo) -> Option<f64> {
        [test.id.as_str(), test.name.as_str()]
            .iter()
            .filter(|key| !key.is_empty())
            .find_map(|key| self.secs_by_test.get(*key).copied())
    }

    pub fn entered(&self, test: &crate::types::TestInfo, site: u32) -> bool {
        [test.id.as_str(), test.name.as_str()]
            .iter()
            .filter(|key| !key.is_empty())
            .any(|key| {
                self.sites_by_test
                    .get(*key)
                    .is_some_and(|sites| sites.contains(&site))
            })
    }
}

pub fn record_site_hits(
    planned_union: &PlannedMutant,
    run_dir: &Path,
    tree_root: &Path,
    policy: &TestRunPolicy,
    jobs: usize,
    progress: Option<&crate::progress::Progress>,
) -> Result<SiteRecording> {
    let tests = resolve_tests(planned_union, run_dir, tree_root)?;
    let dir = run_dir.join("site-hits");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    Ok(record_all(&tests, &dir, policy, jobs, progress))
}

/// A recording run that times out gets one more run with this many times
/// the budget. The first run of a test that runs `cargo` itself (trycmd
/// building clap's examples) compiles those programs in the worker; the
/// retry finds them built. A test still timing out is not green.
const RECORD_RETRY_FACTOR: u32 = 4;

/// `progress`, when given, advances once per test of the first pass.
fn record_all(
    tests: &[ResolvedTest],
    dir: &Path,
    policy: &TestRunPolicy,
    jobs: usize,
    progress: Option<&crate::progress::Progress>,
) -> SiteRecording {
    let started = Instant::now();
    let budget = |t: &ResolvedTest| {
        with_minimum(
            t,
            test_timeout_scaled(t.duration_secs, policy.floor, policy.slowdown),
        )
    };
    let all: Vec<(usize, Duration)> = tests
        .iter()
        .enumerate()
        .map(|(i, t)| (i, budget(t)))
        .collect();
    let mut results = record_pass(tests, &all, dir, jobs, progress);
    let retry: Vec<(usize, Duration)> = results
        .iter()
        .filter(|(_, r)| matches!(r, Err((RecordFailure::TimedOut(_), _))))
        .map(|(i, _)| (*i, budget(&tests[*i]) * RECORD_RETRY_FACTOR))
        .collect();
    if !retry.is_empty() {
        let note = format!(
            "site recording: {} tests timed out; running them once more with {}x the budget",
            retry.len(),
            RECORD_RETRY_FACTOR
        );
        match progress {
            Some(p) => p.println(&note),
            None => tracing::info!("{note}"),
        }
        for (i, again) in record_pass(tests, &retry, dir, jobs, None) {
            if let Some(slot) = results.iter_mut().find(|(j, _)| *j == i) {
                slot.1 = again;
            }
        }
    }
    let mut recording = SiteRecording {
        wall_secs: started.elapsed().as_secs_f64(),
        ..SiteRecording::default()
    };
    for (i, result) in results {
        let t = &tests[i];
        let key = if t.id.is_empty() {
            t.name.clone()
        } else {
            t.id.clone()
        };
        match result {
            Ok((sites, secs, cpu)) => {
                recording.cpu_secs_by_test.insert(key.clone(), cpu);
                recording.secs_by_test.insert(key.clone(), secs);
                recording.sites_by_test.insert(key, sites);
            }
            Err((why, output)) => {
                if !output.trim().is_empty() {
                    recording
                        .not_green_output
                        .insert(key.clone(), output_tail(&output));
                }
                recording.not_green.push((key, why.to_string()));
            }
        }
    }
    recording.not_green.sort();
    recording
}

/// Lines of a not-green test's output kept in the map, and bytes at most.
const NOT_GREEN_LINES: usize = 40;
const NOT_GREEN_BYTES: usize = 8 * 1024;

/// The end of a test's output, as kept for a test that is not green.
fn output_tail(output: &str) -> String {
    let lines: Vec<&str> = output.trim_end().lines().collect();
    let mut tail = lines[lines.len().saturating_sub(NOT_GREEN_LINES)..].join("\n");
    if tail.len() > NOT_GREEN_BYTES {
        let mut cut = tail.len() - NOT_GREEN_BYTES;
        while !tail.is_char_boundary(cut) {
            cut += 1;
        }
        tail.drain(..cut);
    }
    tail
}

/// Warning lines for the first `limit` tests that are not green: each
/// test and why, then the last lines of its output, indented.
pub fn describe_not_green(
    not_green: &[(String, String)],
    output: &std::collections::BTreeMap<String, String>,
    limit: usize,
) -> Vec<String> {
    const SHOWN_LINES: usize = 15;
    let mut out = Vec::new();
    for (test, why) in not_green.iter().take(limit) {
        let mut text = format!("{test}: {why}");
        if let Some(tail) = output.get(test) {
            let lines: Vec<&str> = tail.lines().collect();
            for line in &lines[lines.len().saturating_sub(SHOWN_LINES)..] {
                text.push_str("\n      | ");
                text.push_str(line);
            }
        }
        out.push(text);
    }
    if not_green.len() > limit {
        out.push(format!("... and {} more", not_green.len() - limit));
    }
    out
}

/// Run `(test index, timeout)` pairs on `jobs` threads.
fn record_pass(
    tests: &[ResolvedTest],
    runs: &[(usize, Duration)],
    dir: &Path,
    jobs: usize,
    progress: Option<&crate::progress::Progress>,
) -> Vec<(usize, RecordResult)> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, RecordResult)>> = Mutex::new(Vec::with_capacity(runs.len()));
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1).min(runs.len().max(1)) {
            scope.spawn(|| loop {
                let k = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some(&(i, timeout)) = runs.get(k) else {
                    break;
                };
                let result = record_one(&tests[i], i, dir, timeout);
                results.lock().unwrap().push((i, result));
                if let Some(p) = progress {
                    p.inc(1);
                }
            });
        }
    });
    results.into_inner().unwrap()
}

/// Why a test selects nothing.
#[derive(Debug)]
enum RecordFailure {
    TimedOut(Duration),
    Other(String),
}

impl std::fmt::Display for RecordFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecordFailure::TimedOut(after) => f.write_str(&RunStatus::Timeout.describe(*after)),
            RecordFailure::Other(why) => f.write_str(why),
        }
    }
}

/// One test's recorded sites and wall seconds, or why it selects nothing
/// with the end of its output.
type RecordResult = Result<(std::collections::BTreeSet<u32>, f64, f64), (RecordFailure, String)>;

fn record_one(t: &ResolvedTest, index: usize, dir: &Path, timeout: Duration) -> RecordResult {
    let other = |why: String| (RecordFailure::Other(why), String::new());
    let path = dir.join(format!("{index:06}.txt"));
    fs::write(&path, b"").map_err(|err| other(format!("create {}: {err}", path.display())))?;
    let env = test_env(
        t,
        &[(
            "QS_RECORD_SITES".to_string(),
            path.to_string_lossy().into_owned(),
        )],
    );
    let (status, elapsed, cpu, output) =
        run_exact_capture_output_with_env(&t.binary, &t.name, &t.cwd, timeout, &env)
            .map_err(|err| other(format!("{err:#}")))?;
    let secs = elapsed.as_secs_f64();
    let failed = |why: RecordFailure| Err((why, output.clone()));
    match status {
        RunStatus::Passed => {}
        RunStatus::Failed => {
            return failed(RecordFailure::Other("failed with no mutant active".into()))
        }
        RunStatus::Error { .. } => {
            return failed(RecordFailure::Other(format!(
                "{} with no mutant active",
                status.describe(elapsed)
            )))
        }
        RunStatus::Timeout => return failed(RecordFailure::TimedOut(timeout)),
    }
    let text = fs::read_to_string(&path)
        .map_err(|err| other(format!("read {}: {err}", path.display())))?;
    Ok((
        text.lines().filter_map(|l| l.trim().parse().ok()).collect(),
        secs,
        cpu,
    ))
}

fn resolve_tests(
    planned: &PlannedMutant,
    run_dir: &Path,
    tree_root: &Path,
) -> Result<Vec<ResolvedTest>> {
    let manifest_path = run_dir.join("manifest.json");
    let text = fs::read_to_string(&manifest_path)?;
    let manifest: BinariesManifest = serde_json::from_str(&text)?;
    // Exact id first. Two test binaries in one package often share a test
    // name (`test_parse` in tests/test_version.rs and tests/test_identifier.rs);
    // a name match must never shadow a later exact id match.
    let find = |want: &crate::types::TestInfo| -> Option<&ManifestTest> {
        (!want.id.is_empty())
            .then(|| manifest.tests.iter().find(|t| t.id == want.id))
            .flatten()
            .or_else(|| {
                manifest
                    .tests
                    .iter()
                    .find(|t| t.name == want.name && t.cwd_rel == want.cwd_rel)
            })
            .or_else(|| manifest.tests.iter().find(|t| t.name == want.name))
    };
    let tests: Vec<&ManifestTest> = if planned.tests.is_empty() {
        manifest.tests.iter().collect()
    } else {
        let mut picked: Vec<&ManifestTest> = Vec::new();
        for t in planned.tests.iter().filter_map(find) {
            if !picked.iter().any(|p| std::ptr::eq(*p, t)) {
                picked.push(t);
            }
        }
        picked
    };
    let tests = if tests.is_empty() {
        manifest.tests.iter().collect::<Vec<_>>()
    } else {
        tests
    };

    Ok(tests
        .into_iter()
        .map(|t| {
            let baseline = planned
                .tests
                .iter()
                .find(|p| !p.id.is_empty() && p.id == t.id)
                .or_else(|| planned.tests.iter().find(|p| p.name == t.name));
            let cwd = if t.cwd_rel.is_empty() {
                tree_root.to_path_buf()
            } else {
                tree_root.join(&t.cwd_rel)
            };
            ResolvedTest {
                cpu_secs: baseline
                    .map(|p| p.cpu_secs.unwrap_or(p.duration_secs))
                    .unwrap_or(0.001),
                id: t.id.clone(),
                name: t.name.clone(),
                binary: run_dir.join(&t.binary),
                env: nextest_like_env(&manifest, &t.package, &cwd),
                cwd,
                duration_secs: planned
                    .tests
                    .iter()
                    .find(|p| !p.id.is_empty() && p.id == t.id)
                    .or_else(|| planned.tests.iter().find(|p| p.name == t.name))
                    .map(|p| p.duration_secs)
                    .unwrap_or(0.001),
            }
        })
        .collect())
}

/// A test that does not pass even with no mutant active. The mutant tells us
/// nothing about it, so it is skipped rather than charged to the mutant.
#[derive(Debug, Clone)]
pub struct InfraSkip {
    test: String,
    id: String,
    reason: String,
    output: String,
}

enum Step {
    /// Move on to the next selected test.
    Next,
    /// This test is unusable under this binary; remember it and move on.
    Skip(InfraSkip),
    /// Verdict for the whole mutant.
    Done(Outcome),
}

fn run_selected_tests_observed<R>(
    planned: &PlannedMutant,
    tests: &[ResolvedTest],
    policy: &TestRunPolicy,
    extra_env: &[(String, String)],
    complete_vector: bool,
    run: &mut R,
) -> Result<ObservedRun>
where
    R: FnMut(&RunSpec) -> Result<(RunStatus, Duration, f64, String)>,
{
    // Running no test at all is not evidence that the mutant survived. An
    // empty selection here means staging or attribution produced nothing (the
    // manifest fallback in `resolve_tests` has already had its turn), so the
    // mutant has to be reported as an error rather than a silent Missed.
    if tests.is_empty() {
        return Ok(ObservedRun {
            outcome: Outcome::errored(
                planned.mutant.clone(),
                String::new(),
                "no tests were staged for this mutant (empty per-test attribution)".into(),
            ),
            observations: Vec::new(),
            complete: false,
        });
    }
    let mut skipped: Vec<InfraSkip> = Vec::new();
    let mut observations = Vec::new();
    let mut decisive: Option<Outcome> = None;
    for t in tests {
        // Unusable for an earlier mutant of this run: it fails with no mutant
        // active, so it cannot judge this one either.
        if let Some(skip) = policy.unusable.get(t) {
            observations.push(observation(t, TestVerdict::Infrastructure, Duration::ZERO));
            skipped.push(skip);
            continue;
        }
        let budget = with_minimum(
            t,
            test_timeout_scaled(t.duration_secs, policy.floor, policy.slowdown),
        );
        let (first, first_secs, first_cpu, _) = run(&spec_for(t, budget, extra_env, false))?;
        if first == RunStatus::Passed {
            observations.push(observation(t, TestVerdict::Passed, first_secs));
            continue;
        }
        if !policy.confirm {
            let outcome = match first {
                RunStatus::Passed => unreachable!(),
                RunStatus::Failed => Outcome::caught(planned.mutant.clone(), t.name.clone()),
                RunStatus::Error { .. } => Outcome::errored(
                    planned.mutant.clone(),
                    t.name.clone(),
                    format!(
                        "test {} {} (libtest failures are 101; this is not Caught)",
                        t.name,
                        first.describe(first_secs)
                    ),
                ),
                RunStatus::Timeout => Outcome::timeout(planned.mutant.clone(), t.name.clone()),
            };
            observations.push(observation(t, verdict_for_outcome(&outcome), first_secs));
            if !complete_vector {
                return Ok(ObservedRun {
                    outcome,
                    observations,
                    complete: false,
                });
            }
            decisive.get_or_insert(outcome);
            continue;
        }
        tracing::info!(
            "confirm {}: first pass {:?} in {:.2}s (budget {:.1}s, green {:.3}s)",
            t.name,
            first,
            first_secs.as_secs_f64(),
            budget.as_secs_f64(),
            t.duration_secs
        );
        let step = if first == RunStatus::Timeout {
            confirm_timeout(planned, t, budget, first_cpu, extra_env, run)?
        } else {
            confirm_failure(planned, t, first, budget, extra_env, run)?
        };
        match step {
            Step::Next => observations.push(observation(t, TestVerdict::Passed, first_secs)),
            Step::Skip(s) => {
                policy.unusable.remember(t, &s);
                observations.push(observation(t, TestVerdict::Infrastructure, first_secs));
                skipped.push(s);
            }
            Step::Done(o) => {
                observations.push(observation(t, verdict_for_outcome(&o), first_secs));
                if !complete_vector {
                    return Ok(ObservedRun {
                        outcome: o,
                        observations,
                        complete: false,
                    });
                }
                decisive.get_or_insert(o);
            }
        }
    }
    if let Some(outcome) = decisive {
        return Ok(ObservedRun {
            outcome,
            observations,
            complete: true,
        });
    }
    if skipped.is_empty() {
        return Ok(ObservedRun {
            outcome: Outcome::missed(planned.mutant.clone()),
            observations,
            complete: true,
        });
    }
    let first = &skipped[0];
    // Every test that could judge the mutant passed: it survived them, and
    // the rest could not judge it. Without any such test there is no
    // evidence either way.
    if observations
        .iter()
        .any(|o| o.verdict == TestVerdict::Passed)
    {
        let mut outcome = Outcome::missed(planned.mutant.clone());
        outcome.message = Some(format!(
            "{} selected test(s) could not judge it: they fail under the schema binary with no mutant active; first: {} {}",
            skipped.len(),
            first.test,
            first.reason
        ));
        return Ok(ObservedRun {
            outcome,
            observations,
            complete: true,
        });
    }
    let message = format!(
        "{} selected test(s) unusable under schema binary (fail with no mutant active); first: {} {}; output: {}",
        skipped.len(),
        first.test,
        first.reason,
        first.output
    );
    tracing::info!("confirm: {}", message);
    Ok(ObservedRun {
        outcome: Outcome::errored(planned.mutant.clone(), first.test.clone(), message),
        observations,
        complete: true,
    })
}

#[cfg(test)]
fn run_selected_tests<R>(
    planned: &PlannedMutant,
    tests: &[ResolvedTest],
    policy: &TestRunPolicy,
    extra_env: &[(String, String)],
    run: &mut R,
) -> Result<Outcome>
where
    R: FnMut(&RunSpec) -> Result<(RunStatus, Duration, f64, String)>,
{
    Ok(run_selected_tests_observed(planned, tests, policy, extra_env, false, run)?.outcome)
}

fn observation(t: &ResolvedTest, verdict: TestVerdict, wall: Duration) -> TestObservation {
    TestObservation {
        test_id: t.id.clone(),
        test_name: t.name.clone(),
        verdict,
        wall_secs: wall.as_secs_f64(),
    }
}

pub(crate) fn verdict_for_outcome(outcome: &Outcome) -> TestVerdict {
    match outcome.kind {
        crate::types::VerdictKind::Caught => TestVerdict::Killed,
        crate::types::VerdictKind::Timeout => TestVerdict::Timeout,
        crate::types::VerdictKind::Errored => TestVerdict::Errored,
        crate::types::VerdictKind::Missed => TestVerdict::Passed,
        crate::types::VerdictKind::NotCovered | crate::types::VerdictKind::Unviable => {
            TestVerdict::Infrastructure
        }
    }
}

/// First pass was Failed or Error. Re-run once (flakes are common under a
/// schema binary), then run with no mutant active to see whether the test is
/// broken on its own.
fn confirm_failure<R>(
    planned: &PlannedMutant,
    t: &ResolvedTest,
    first: RunStatus,
    budget: Duration,
    extra_env: &[(String, String)],
    run: &mut R,
) -> Result<Step>
where
    R: FnMut(&RunSpec) -> Result<(RunStatus, Duration, f64, String)>,
{
    let (retry, retry_secs, _retry_cpu, retry_err) = run(&spec_for(t, budget, extra_env, true))?;
    tracing::info!(
        "confirm {}: retry with mutant {:?} in {:.2}s; output: {}",
        t.name,
        retry,
        retry_secs.as_secs_f64(),
        tail(&retry_err, MESSAGE_TAIL)
    );
    if retry == RunStatus::Passed {
        tracing::info!(
            "confirm {}: flake (first {:?}, retry passed with mutant active); next test",
            t.name,
            first
        );
        return Ok(Step::Next);
    }
    let (mut null, mut null_secs, _, mut null_err) = run(&spec_for(t, budget, &[], true))?;
    tracing::info!(
        "confirm {}: null run {:?} in {:.2}s; output: {}",
        t.name,
        null,
        null_secs.as_secs_f64(),
        tail(&null_err, MESSAGE_TAIL)
    );
    // Every selected test was green when recorded. A single null failure
    // under load is not enough to discard that evidence.
    if null != RunStatus::Passed {
        let (status, wall, _, stderr) = run(&spec_for(t, budget, &[], true))?;
        null = status;
        null_secs = wall;
        null_err = stderr;
        tracing::info!(
            "confirm {}: second null run {}",
            t.name,
            null.describe(null_secs)
        );
    }
    if null == RunStatus::Passed {
        if first == RunStatus::Failed || retry == RunStatus::Failed {
            tracing::info!(
                "confirm {}: CAUGHT (fails with mutant, passes without)",
                t.name
            );
            return Ok(Step::Done(Outcome::caught(
                planned.mutant.clone(),
                t.name.clone(),
            )));
        }
        let crash = if matches!(first, RunStatus::Error { .. }) {
            first
        } else {
            retry
        };
        let exit = crash.describe(retry_secs);
        // A deterministic crash (stack overflow, abort, OOM kill, a nonzero
        // non-101 exit) under the mutant, on the same schema binary that
        // passes without it, is a test detecting the mutant.
        let message = format!(
            "{} ({}) and passes without it; test {}; output: {}",
            crate::types::CRASH_PREFIX,
            exit,
            t.name,
            tail(&retry_err, MESSAGE_TAIL)
        );
        tracing::info!(
            "confirm {}: CAUGHT by crash ({}), passes without",
            t.name,
            exit
        );
        return Ok(Step::Done(Outcome::caught_by_crash(
            planned.mutant.clone(),
            t.name.clone(),
            message,
        )));
    }
    tracing::info!(
        "confirm {}: infra-skip ({:?} with no mutant active); next test",
        t.name,
        null
    );
    Ok(Step::Skip(InfraSkip {
        test: t.name.clone(),
        id: t.id.clone(),
        reason: null.describe(null_secs),
        output: tail(&null_err, MESSAGE_TAIL),
    }))
}

/// CPU, rather than a cached null wall duration, distinguishes extra work
/// from host load. A quiet second timeout also detects blocked tests.
fn confirm_timeout<R>(
    planned: &PlannedMutant,
    t: &ResolvedTest,
    budget: Duration,
    first_cpu: f64,
    extra_env: &[(String, String)],
    run: &mut R,
) -> Result<Step>
where
    R: FnMut(&RunSpec) -> Result<(RunStatus, Duration, f64, String)>,
{
    let threshold = RUNAWAY_CPU_FACTOR * t.cpu_secs + RUNAWAY_CPU_ALLOWANCE;
    let caught = |message| {
        Step::Done(Outcome::caught_by_timeout(
            planned.mutant.clone(),
            t.name.clone(),
            message,
        ))
    };
    let runaway = |cpu: f64| {
        caught(format!(
            "{}: test {} used {:.2} s of CPU where the test normally uses {:.2} s",
            crate::types::TIMEOUT_PREFIX,
            t.name,
            cpu,
            t.cpu_secs
        ))
    };
    if first_cpu >= threshold {
        return Ok(runaway(first_cpu));
    }
    let retry_budget = budget.saturating_mul(TIMEOUT_RETRY_FACTOR);
    let (again, wall, cpu, _) = run(&spec_for(t, retry_budget, extra_env, true))?;
    match again {
        RunStatus::Passed => Ok(Step::Next),
        RunStatus::Failed | RunStatus::Error { .. } =>
            confirm_failure(planned, t, again, retry_budget, extra_env, run),
        RunStatus::Timeout if cpu >= threshold => Ok(runaway(cpu)),
        RunStatus::Timeout if cpu / wall.as_secs_f64() < BLOCKED_CPU_SHARE => {
            Ok(caught(format!(
                "{}: test {} was blocked: used {:.2} s of CPU in {:.2} s, so it was waiting (for example a deadlock), not computing",
                crate::types::TIMEOUT_PREFIX, t.name, cpu, wall.as_secs_f64())))
        }
        RunStatus::Timeout => {
            let mut outcome = Outcome::timeout(planned.mutant.clone(), t.name.clone());
            outcome.message = Some(format!(
                "test {} ran past {} × its budget ({:.1} s, using {:.2} s of CPU where it normally uses {:.2} s) at a normal CPU rate without using {} × its usual CPU; the host's load is the likely cause; recheck with `test --again timeouts` on a quieter host",
                t.name,
                TIMEOUT_RETRY_FACTOR,
                wall.as_secs_f64(),
                cpu,
                t.cpu_secs,
                RUNAWAY_CPU_FACTOR
            ));
            Ok(Step::Done(outcome))
        }
    }
}

fn spec_for(
    t: &ResolvedTest,
    timeout: Duration,
    env: &[(String, String)],
    capture: bool,
) -> RunSpec {
    RunSpec {
        binary: t.binary.clone(),
        test_name: t.name.clone(),
        cwd: t.cwd.clone(),
        timeout,
        env: test_env(t, env),
        capture,
    }
}

fn tail(s: &str, max: usize) -> String {
    let s = s.trim_end();
    if s.len() <= max {
        return s.to_string();
    }
    let mut start = s.len() - max;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_string()
}

pub fn cleanup_run(run_dir: &Path) {
    let _ = fs::remove_dir_all(run_dir);
}

pub fn run_dir_for(runs_root: &Path, mutant_id: &str) -> PathBuf {
    runs_root.join(mutant_id)
}

/// Per-test budget: 3× that test's green duration, never the sum of the
/// whole selection. Missing timings (index default 0.001s) used to collapse
/// this to the 5s floor, so 17s integration tests always timed out.
pub fn test_timeout(duration_secs: f64, floor: Duration) -> Duration {
    test_timeout_scaled(duration_secs, floor, 1.0)
}

/// [`test_timeout`] with a slowdown factor for binaries that are slower than
/// the ones the green durations were measured on.
pub fn test_timeout_scaled(duration_secs: f64, floor: Duration, slowdown: f64) -> Duration {
    const SENTINEL: f64 = 0.0015;
    // nextest profile.quicksilver slow-timeout period.
    const MISSING_SECS: f64 = 120.0;
    let secs = if duration_secs <= SENTINEL {
        MISSING_SECS.max(floor.as_secs_f64())
    } else {
        (duration_secs * 3.0 * slowdown).max(floor.as_secs_f64())
    };
    duration_from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Mutant, VerdictKind};

    #[test]
    fn timeout_uses_this_test_not_the_selection_sum() {
        let floor = Duration::from_secs(5);
        assert_eq!(test_timeout(17.0, floor), Duration::from_secs_f64(51.0));
        assert_eq!(test_timeout(0.2, floor), floor);
        assert_eq!(test_timeout(0.001, floor), Duration::from_secs(120));
    }

    #[test]
    fn confirm_timeout_scaled_budgets_for_slow_binaries() {
        let floor = Duration::from_secs(30);
        // 0.41s green × 3 × 3 = 3.7s: the floor still wins.
        assert_eq!(test_timeout_scaled(0.41, floor, 3.0), floor);
        assert_eq!(
            test_timeout_scaled(20.0, floor, 3.0),
            Duration::from_secs(180)
        );
        // Missing timing keeps the nextest slow-timeout period.
        assert_eq!(
            test_timeout_scaled(0.001, floor, 3.0),
            Duration::from_secs(120)
        );
        // slowdown 1.0 is the old behavior, byte for byte.
        assert_eq!(
            test_timeout_scaled(17.0, Duration::from_secs(5), 1.0),
            test_timeout(17.0, Duration::from_secs(5))
        );
    }

    /// Two test binaries of one package each have `test_parse`. Each planned
    /// test must resolve to its own binary, not to the first name match.
    #[test]
    fn same_name_in_two_binaries_resolves_by_id() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = BinariesManifest {
            mutant_id: "schema".into(),
            tests: vec![
                ManifestTest {
                    name: "test_parse".into(),
                    binary: "test_identifier-aaaa".into(),
                    cwd_rel: "".into(),
                    id: "semver::test_identifier::test_parse".into(),
                    ..Default::default()
                },
                ManifestTest {
                    name: "test_parse".into(),
                    binary: "test_version-bbbb".into(),
                    cwd_rel: "".into(),
                    id: "semver::test_version::test_parse".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        fs::write(
            dir.path().join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let want = |id: &str, secs: f64| crate::types::TestInfo {
            id: id.into(),
            name: "test_parse".into(),
            package: "semver".into(),
            binary_id: id.rsplit_once("::").unwrap().0.into(),
            binary_name: String::new(),
            binary_rel: String::new(),
            cwd_rel: "".into(),
            duration_secs: secs,
            cpu_secs: None,
        };
        let planned = PlannedMutant {
            mutant: fake_mutant(),
            tests: vec![
                want("semver::test_version::test_parse", 0.5),
                want("semver::test_identifier::test_parse", 0.25),
            ],
            estimated_cost_secs: 0.0,
        };
        let resolved = resolve_tests(&planned, dir.path(), dir.path()).unwrap();
        assert_eq!(
            resolved[0].cpu_secs, 0.5,
            "old plans use recorded wall time as CPU"
        );
        let got: Vec<(String, String, f64)> = resolved
            .iter()
            .map(|t| {
                (
                    t.id.clone(),
                    t.binary.file_name().unwrap().to_string_lossy().into_owned(),
                    t.duration_secs,
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "semver::test_version::test_parse".into(),
                    "test_version-bbbb".into(),
                    0.5
                ),
                (
                    "semver::test_identifier::test_parse".into(),
                    "test_identifier-aaaa".into(),
                    0.25
                ),
            ]
        );
    }

    fn fake_mutant() -> Mutant {
        Mutant {
            id: "m1".into(),
            package: "p".into(),
            path: "src/lib.rs".into(),
            fqn: "p::f".into(),
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            end_line: 1,
            body_start_byte: 0,
            body_end_byte: 1,
            replacement: "{}".into(),
            return_kind: "unit".into(),
        }
    }

    /// A test whose first run builds programs (trycmd compiling clap's
    /// examples in a fresh worker) can outlast its budget once. Dropping it
    /// as not green would silently remove it from every selection.
    #[test]
    fn a_recording_timeout_gets_one_longer_retry() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let path = dir.path().join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            ResolvedTest {
                id: format!("bin::{name}"),
                name: name.into(),
                binary: path,
                cwd: dir.path().to_path_buf(),
                duration_secs: 0.01,
                env: Vec::new(),
                cpu_secs: 0.01,
            }
        };
        let tests = vec![
            script(
                "slow_once",
                "if [ -e \"$0.ran\" ]; then exit 0; fi\ntouch \"$0.ran\"\nsleep 5",
            ),
            script("always_hangs", "sleep 30"),
        ];
        let hits = dir.path().join("hits");
        fs::create_dir_all(&hits).unwrap();
        let policy = TestRunPolicy {
            floor: Duration::from_millis(500),
            slowdown: 1.0,
            confirm: false,
            unusable: Default::default(),
        };
        let recording = record_all(&tests, &hits, &policy, 2, None);
        assert!(
            recording.sites_by_test.contains_key("bin::slow_once"),
            "{recording:?}"
        );
        assert_eq!(recording.not_green.len(), 1, "{recording:?}");
        assert_eq!(recording.not_green[0].0, "bin::always_hangs");
        assert!(recording.not_green[0].1.contains("timed out"));
    }

    fn has_mutant(spec: &RunSpec) -> bool {
        spec.env.iter().any(|(k, _)| k == "QS_MUTANT")
    }

    #[test]
    fn tests_get_no_cargo_target_dir_from_qs() {
        let t = &fake_tests(&["t1"])[0];
        let spec = spec_for(t, Duration::from_secs(1), &[], false);
        assert!(!spec.env.iter().any(|(k, _)| k == "CARGO_TARGET_DIR"));
    }

    #[test]
    fn the_toolchain_bin_comes_first_on_a_test_path() {
        let bin = Path::new("/toolchain/bin");
        let path = std::ffi::OsString::from("/wrapper:/usr/bin:/toolchain/bin");
        let first = path_with_first(bin, Some(&path)).unwrap();
        assert_eq!(first, "/toolchain/bin:/wrapper:/usr/bin");
        assert_eq!(path_with_first(bin, None).unwrap(), "/toolchain/bin");
    }

    fn fake_tests(names: &[&str]) -> Vec<ResolvedTest> {
        names
            .iter()
            .map(|n| ResolvedTest {
                id: format!("id-{n}"),
                name: (*n).into(),
                binary: PathBuf::from("/bin/true"),
                cwd: PathBuf::from("/"),
                duration_secs: 0.41,
                env: Vec::new(),
                cpu_secs: 0.41,
            })
            .collect()
    }

    fn drive(
        confirm: bool,
        tests: &[ResolvedTest],
        script: &[(RunStatus, f64, &str)],
    ) -> (Outcome, Vec<RunSpec>) {
        drive_cpu(
            confirm,
            tests,
            &script
                .iter()
                .map(|(s, w, e)| (*s, *w, 0.0, *e))
                .collect::<Vec<_>>(),
        )
    }

    /// Drive the machine with a scripted sequence of run results and return
    /// the verdict plus every RunSpec the machine asked for.
    fn drive_cpu(
        confirm: bool,
        tests: &[ResolvedTest],
        script: &[(RunStatus, f64, f64, &str)],
    ) -> (Outcome, Vec<RunSpec>) {
        let policy = TestRunPolicy {
            floor: Duration::from_secs(30),
            slowdown: 3.0,
            confirm,
            unusable: Default::default(),
        };
        drive_with(&policy, tests, script)
    }

    /// [`drive_cpu`] with a given policy, so that several mutants can share
    /// one run's memo of unusable tests.
    fn drive_with(
        policy: &TestRunPolicy,
        tests: &[ResolvedTest],
        script: &[(RunStatus, f64, f64, &str)],
    ) -> (Outcome, Vec<RunSpec>) {
        let planned = PlannedMutant {
            mutant: fake_mutant(),
            tests: Vec::new(),
            estimated_cost_secs: 0.0,
        };
        let env = vec![("QS_MUTANT".to_string(), "site:variant".to_string())];
        let mut seen: Vec<RunSpec> = Vec::new();
        let mut next = 0usize;
        let outcome = {
            let mut run = |spec: &RunSpec| -> Result<(RunStatus, Duration, f64, String)> {
                seen.push(spec.clone());
                let (status, secs, cpu, err) = script
                    .get(next)
                    .unwrap_or_else(|| panic!("unscripted run #{next}: {}", spec.test_name));
                next += 1;
                Ok((
                    *status,
                    Duration::from_secs_f64(*secs),
                    *cpu,
                    (*err).to_string(),
                ))
            };
            run_selected_tests(&planned, tests, policy, &env, &mut run).unwrap()
        };
        (outcome, seen)
    }

    /// #34: a deadlock watchdog that cannot meet its deadline under the
    /// schema binary fails with no mutant active for every mutant.
    #[test]
    fn a_mutant_surviving_every_usable_test_is_missed_and_unusable_tests_are_remembered() {
        let policy = TestRunPolicy {
            floor: Duration::from_secs(30),
            slowdown: 3.0,
            confirm: true,
            unusable: Default::default(),
        };
        let tests = fake_tests(&["ok", "watchdog"]);
        let (outcome, seen) = drive_with(
            &policy,
            &tests,
            &[
                (RunStatus::Passed, 0.5, 0.5, ""),
                (RunStatus::Failed, 61.0, 60.0, ""),
                (RunStatus::Failed, 61.0, 60.0, "too slow"),
                (RunStatus::Failed, 61.0, 60.0, "too slow"),
                (RunStatus::Failed, 61.0, 60.0, "too slow"),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Missed, "{outcome:?}");
        let message = outcome.message.unwrap();
        assert!(
            message.contains("could not judge it") && message.contains("watchdog"),
            "{message}"
        );
        assert_eq!(seen.len(), 5);
        // A later mutant of the same run does not run the watchdog again.
        let (outcome, seen) = drive_with(&policy, &tests, &[(RunStatus::Passed, 0.5, 0.5, "")]);
        assert_eq!(outcome.kind, VerdictKind::Missed);
        assert!(outcome.message.unwrap().contains("watchdog"));
        assert_eq!(seen.len(), 1, "only `ok` runs");
        // With no test able to judge it, there is no evidence either way.
        let (outcome, seen) = drive_with(&policy, &fake_tests(&["watchdog"]), &[]);
        assert_eq!(outcome.kind, VerdictKind::Errored);
        assert!(seen.is_empty());
        // Another run starts with an empty memo.
        let (_, seen) = drive_cpu(
            true,
            &fake_tests(&["watchdog"]),
            &[(RunStatus::Passed, 0.5, 0.5, "")],
        );
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn confirm_retry_pass_is_a_flake_and_falls_through_to_missed() {
        let (outcome, seen) = drive(
            true,
            &fake_tests(&["t1"]),
            &[(RunStatus::Failed, 0.5, ""), (RunStatus::Passed, 0.5, "")],
        );
        assert_eq!(outcome.kind, VerdictKind::Missed);
        assert_eq!(seen.len(), 2, "no null run after a passing retry");
        assert!(has_mutant(&seen[1]), "retry keeps the mutant env");
        assert!(seen[1].capture, "retry captures stderr");
    }

    #[test]
    fn confirm_fail_twice_then_null_pass_is_caught() {
        let (outcome, seen) = drive(
            true,
            &fake_tests(&["t1"]),
            &[
                (RunStatus::Failed, 0.5, ""),
                (RunStatus::Failed, 0.5, "assertion failed"),
                (RunStatus::Passed, 0.5, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Caught);
        assert_eq!(outcome.test.as_deref(), Some("t1"));
        assert_eq!(seen.len(), 3);
        assert!(!has_mutant(&seen[2]), "null run drops the mutant env");
    }

    #[test]
    fn a_failed_null_run_gets_one_retry_before_caught_or_crash() {
        for mutant in [
            RunStatus::Failed,
            RunStatus::Error {
                code: None,
                signal: Some(libc::SIGABRT),
            },
        ] {
            for null in [
                RunStatus::Failed,
                RunStatus::Timeout,
                RunStatus::Error {
                    code: None,
                    signal: Some(libc::SIGKILL),
                },
            ] {
                let (outcome, seen) = drive(
                    true,
                    &fake_tests(&["t"]),
                    &[
                        (mutant, 0.5, ""),
                        (mutant, 0.5, "assertion or crash"),
                        (null, 30.0, "transient null failure"),
                        (RunStatus::Passed, 0.5, ""),
                    ],
                );
                assert_eq!(outcome.kind, VerdictKind::Caught);
                assert_eq!(seen.len(), 4);
                assert!(!has_mutant(&seen[2]) && !has_mutant(&seen[3]));
                assert!(seen[2].capture && seen[3].capture);
                assert_eq!(seen[2].timeout, seen[3].timeout);
                if matches!(mutant, RunStatus::Error { .. }) {
                    let msg = outcome.message.unwrap();
                    assert!(msg.starts_with(crate::types::CRASH_PREFIX), "{msg}");
                    assert!(msg.contains("killed by signal 6 (SIGABRT)"), "{msg}");
                }
            }
        }
    }

    #[test]
    fn unusable_null_runs_name_signals_and_quicksilver_deadlines() {
        for (null, expected) in [
            (
                RunStatus::Error {
                    code: None,
                    signal: Some(libc::SIGKILL),
                },
                "killed by signal 9 (SIGKILL)",
            ),
            (
                RunStatus::Error {
                    code: None,
                    signal: Some(libc::SIGABRT),
                },
                "killed by signal 6 (SIGABRT)",
            ),
            (
                RunStatus::Timeout,
                "timed out after 30.00 s (stopped by quicksilver)",
            ),
        ] {
            let (outcome, seen) = drive(
                true,
                &fake_tests(&["t"]),
                &[
                    (RunStatus::Failed, 0.5, ""),
                    (RunStatus::Failed, 0.5, "assertion"),
                    (null, 30.0, "first null"),
                    (null, 30.0, "second null"),
                ],
            );
            assert_eq!(outcome.kind, VerdictKind::Errored);
            let msg = outcome.message.unwrap();
            assert!(msg.contains(expected), "{msg}");
            assert!(msg.contains("second null"), "{msg}");
            assert_eq!(seen.len(), 4, "only one null retry");
        }
    }

    #[test]
    fn confirm_fail_twice_then_null_fail_is_infra_skip_then_errored() {
        let (outcome, seen) = drive(
            true,
            &fake_tests(&["t1"]),
            &[
                (RunStatus::Failed, 0.5, ""),
                (RunStatus::Failed, 0.5, ""),
                (RunStatus::Failed, 0.5, "panicked at src/lib.rs:12"),
                (RunStatus::Failed, 0.5, "panicked at src/lib.rs:12"),
            ],
        );
        assert_eq!(seen.len(), 4);
        assert_eq!(outcome.kind, VerdictKind::Errored);
        let msg = outcome.message.unwrap();
        assert!(msg.contains("1 selected test(s) unusable under schema binary"));
        assert!(msg.contains("exit 101"), "{msg}");
        assert!(msg.contains("panicked at src/lib.rs:12"), "{msg}");
    }

    #[test]
    fn timeout_runaway_on_first_pass_is_caught_without_a_null_run() {
        let (outcome, seen) = drive_cpu(
            true,
            &fake_tests(&["t"]),
            &[(RunStatus::Timeout, 30.0, 2.23, "")],
        );
        assert_eq!(outcome.kind, VerdictKind::Caught);
        assert!(outcome.message.unwrap().contains("normally uses 0.41 s"));
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn timeout_second_pass_runaway_is_caught() {
        let (outcome, seen) = drive_cpu(
            true,
            &fake_tests(&["t"]),
            &[
                (RunStatus::Timeout, 30.0, 1.0, ""),
                (RunStatus::Timeout, 120.0, 3.0, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Caught);
        assert!(outcome
            .message
            .unwrap()
            .starts_with(crate::types::TIMEOUT_PREFIX));
        assert_eq!(seen[1].timeout, Duration::from_secs(120));
        assert!(has_mutant(&seen[1]));
    }

    #[test]
    fn timeout_with_little_cpu_on_retry_is_blocked() {
        let (outcome, seen) = drive_cpu(
            true,
            &fake_tests(&["t"]),
            &[
                (RunStatus::Timeout, 30.0, 1.0, ""),
                (RunStatus::Timeout, 120.0, 2.0, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Caught);
        assert!(outcome.message.unwrap().contains("blocked"));
        assert_eq!(seen.len(), 2);
    }

    #[test]
    fn timeout_with_normal_cpu_rate_under_load_is_inconclusive() {
        let mut tests = fake_tests(&["t"]);
        tests[0].cpu_secs = 20.0;
        let (outcome, _) = drive_cpu(
            true,
            &tests,
            &[
                (RunStatus::Timeout, 30.0, 5.0, ""),
                (RunStatus::Timeout, 120.0, 20.0, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Timeout);
        assert!(outcome.message.unwrap().contains("test --again timeouts"));
    }

    #[test]
    fn timeout_retry_pass_moves_to_next_test() {
        let (outcome, seen) = drive_cpu(
            true,
            &fake_tests(&["t", "next"]),
            &[
                (RunStatus::Timeout, 30.0, 1.0, ""),
                (RunStatus::Passed, 40.0, 1.0, ""),
                (RunStatus::Passed, 1.0, 0.1, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Missed);
        assert_eq!(seen[2].test_name, "next");
    }

    #[test]
    fn timeout_retry_failure_goes_through_failure_confirmation() {
        let (outcome, seen) = drive_cpu(
            true,
            &fake_tests(&["t"]),
            &[
                (RunStatus::Timeout, 30.0, 1.0, ""),
                (RunStatus::Failed, 40.0, 1.0, ""),
                (RunStatus::Failed, 40.0, 1.0, "assertion"),
                (RunStatus::Passed, 40.0, 1.0, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Caught);
        assert_eq!(seen.len(), 4);
        assert!(!has_mutant(&seen[3]));
    }

    #[test]
    fn timeout_retry_crash_goes_through_failure_confirmation() {
        let (outcome, seen) = drive_cpu(
            true,
            &fake_tests(&["t"]),
            &[
                (RunStatus::Timeout, 30.0, 1.0, ""),
                (
                    RunStatus::Error {
                        code: None,
                        signal: Some(libc::SIGKILL),
                    },
                    40.0,
                    1.0,
                    "",
                ),
                (
                    RunStatus::Error {
                        code: None,
                        signal: Some(libc::SIGKILL),
                    },
                    40.0,
                    1.0,
                    "",
                ),
                (RunStatus::Passed, 40.0, 1.0, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Caught);
        assert!(outcome
            .message
            .unwrap()
            .starts_with(crate::types::CRASH_PREFIX));
        assert_eq!(seen.len(), 4);
    }

    #[test]
    fn a_configured_budget_raises_first_and_retry_budgets() {
        set_test_budgets(&[("budgeted".to_string(), 90.0)].into_iter().collect());
        let (outcome, seen) = drive_cpu(
            true,
            &fake_tests(&["budgeted"]),
            &[
                (RunStatus::Timeout, 90.0, 1.0, ""),
                (RunStatus::Passed, 100.0, 1.0, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Missed);
        assert_eq!(seen[0].timeout, Duration::from_secs(90));
        assert_eq!(seen[1].timeout, Duration::from_secs(360));
    }

    #[test]
    fn later_mutants_keep_the_recorded_budget_without_calibration() {
        let tests = fake_tests(&["stable-budget"]);
        let _ = drive_cpu(
            true,
            &tests,
            &[
                (RunStatus::Timeout, 30.0, 1.0, ""),
                (RunStatus::Passed, 80.0, 1.0, ""),
            ],
        );
        let (_, seen) = drive(true, &tests, &[(RunStatus::Passed, 41.0, "")]);
        assert_eq!(seen[0].timeout, Duration::from_secs(30));
    }

    #[test]
    fn confirm_crash_twice_then_null_pass_is_caught_by_crash() {
        let (outcome, _) = drive(
            true,
            &fake_tests(&["t1"]),
            &[
                (
                    RunStatus::Error {
                        code: Some(1),
                        signal: None,
                    },
                    0.5,
                    "",
                ),
                (
                    RunStatus::Error {
                        code: Some(1),
                        signal: None,
                    },
                    0.5,
                    "error while loading shared libraries",
                ),
                (RunStatus::Passed, 0.5, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Caught);
        let msg = outcome.message.unwrap();
        assert!(msg.starts_with(crate::types::CRASH_PREFIX), "{msg}");
        assert!(msg.contains("(exit 1)"), "{msg}");
        assert!(
            msg.contains("output: error while loading shared libraries"),
            "{msg}"
        );
    }

    #[test]
    fn confirm_error_twice_then_null_error_is_infra_skip() {
        let (outcome, seen) = drive(
            true,
            &fake_tests(&["t1"]),
            &[
                (
                    RunStatus::Error {
                        code: Some(1),
                        signal: None,
                    },
                    0.5,
                    "",
                ),
                (
                    RunStatus::Error {
                        code: Some(1),
                        signal: None,
                    },
                    0.5,
                    "boom",
                ),
                (
                    RunStatus::Error {
                        code: Some(1),
                        signal: None,
                    },
                    0.5,
                    "no mutant, still boom",
                ),
                (
                    RunStatus::Error {
                        code: Some(1),
                        signal: None,
                    },
                    0.5,
                    "no mutant, still boom",
                ),
            ],
        );
        assert_eq!(seen.len(), 4);
        assert_eq!(outcome.kind, VerdictKind::Errored);
        let msg = outcome.message.unwrap();
        assert!(msg.contains("unusable under schema binary"), "{msg}");
        assert!(msg.contains("no mutant, still boom"), "{msg}");
    }

    #[test]
    fn confirm_all_pass_is_missed_with_one_run_per_test() {
        let (outcome, seen) = drive(
            true,
            &fake_tests(&["t1", "t2"]),
            &[(RunStatus::Passed, 0.4, ""), (RunStatus::Passed, 0.4, "")],
        );
        assert_eq!(outcome.kind, VerdictKind::Missed);
        assert_eq!(seen.len(), 2);
        assert!(!seen[0].capture, "the first pass never pipes stderr");
        // 0.41s green × 3 × 3 is under the 30s floor.
        assert_eq!(seen[0].timeout, Duration::from_secs(30));
    }

    #[test]
    fn an_empty_staged_list_is_errored_not_missed() {
        for confirm in [true, false] {
            let (outcome, seen) = drive(confirm, &[], &[]);
            assert!(seen.is_empty(), "nothing to run (confirm={confirm})");
            assert_eq!(
                outcome.kind,
                VerdictKind::Errored,
                "an unrun mutant is not a survivor (confirm={confirm})"
            );
            assert_eq!(
                outcome.message.as_deref(),
                Some("no tests were staged for this mutant (empty per-test attribution)"),
                "confirm={confirm}"
            );
        }
    }

    #[test]
    fn confirm_disabled_keeps_the_old_fail_fast_behavior() {
        let (outcome, seen) = drive(false, &fake_tests(&["t1"]), &[(RunStatus::Failed, 0.5, "")]);
        assert_eq!(outcome.kind, VerdictKind::Caught);
        assert_eq!(seen.len(), 1, "no retry and no null run without confirm");
    }
}
