use crate::exec::process::{
    run_exact_capture_with_env, run_exact_with_env, RunStatus, LIBTEST_FAILURE_CODE,
};
use crate::types::{Outcome, PlannedMutant, TestObservation, TestVerdict};
use crate::util::duration_from_secs;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinariesManifest {
    pub mutant_id: String,
    pub tests: Vec<ManifestTest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestTest {
    pub name: String,
    pub binary: String,
    pub cwd_rel: String,
    #[serde(default)]
    pub id: String,
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
    /// `<tree root>/target`. A test that runs `cargo` itself (trycmd
    /// building clap's examples) then builds into the tree's own target
    /// directory, even when the caller's environment names another one.
    target_dir: PathBuf,
    duration_secs: f64,
}

/// The environment every test process gets on top of the caller's: the tree's
/// target directory, then `extra` (the mutant switch or the site recorder).
fn test_env(t: &ResolvedTest, extra: &[(String, String)]) -> Vec<(String, String)> {
    let mut env = vec![(
        "CARGO_TARGET_DIR".to_string(),
        t.target_dir.to_string_lossy().into_owned(),
    )];
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

/// Timeout confirmations are serialized process-wide. One costs up to a
/// 120s calibration run; letting all test-pool threads do that at once would
/// both swamp the box and make every measured duration useless. Failure
/// confirmations re-run a test that just failed fast, so they run in
/// parallel: serializing them made every caught mutant queue behind every
/// other (itertools: 90 s of a 147 s run at 3 of 64 cores busy).
static CONFIRM_LOCK: Mutex<()> = Mutex::new(());

fn confirm_lock() -> std::sync::MutexGuard<'static, ()> {
    CONFIRM_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Null-run durations that timeout calibration measured in this process, by
/// test id. Baseline timings come from an unloaded nextest run, so under a
/// full test pool a test can overrun its budget with no mutant at all. Once
/// calibrated, a test keeps that duration for every later mutant: on bifrost
/// one test was calibrated 55 times in one run and 136 first passes timed
/// out only because of their budget.
static CALIBRATED: LazyLock<Mutex<HashMap<String, f64>>> = LazyLock::new(Default::default);

fn calibrated_secs(t: &ResolvedTest) -> Option<f64> {
    CALIBRATED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&t.id)
        .copied()
}

/// What a passing run of `t` is expected to take: its baseline duration, or
/// its calibrated one when that is longer.
fn expected_secs(t: &ResolvedTest) -> f64 {
    calibrated_secs(t).map_or(t.duration_secs, |c| c.max(t.duration_secs))
}

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
    let tests = resolve_tests(planned, run_dir, tree_root)?;
    let mut run = |spec: &RunSpec| -> Result<(RunStatus, Duration, String)> {
        if spec.capture {
            run_exact_capture_with_env(
                &spec.binary,
                &spec.test_name,
                &spec.cwd,
                spec.timeout,
                &spec.env,
            )
        } else {
            let started = Instant::now();
            let status = run_exact_with_env(
                &spec.binary,
                &spec.test_name,
                &spec.cwd,
                spec.timeout,
                &spec.env,
            )?;
            Ok((status, started.elapsed(), String::new()))
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
    /// Tests that did not pass with no mutant active. They select nothing:
    /// the schema binary cannot judge a mutant with a test that is not green.
    pub not_green: Vec<(String, String)>,
    pub wall_secs: f64,
}

impl SiteRecording {
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
    let budget =
        |t: &ResolvedTest| test_timeout_scaled(t.duration_secs, policy.floor, policy.slowdown);
    let all: Vec<(usize, Duration)> = tests
        .iter()
        .enumerate()
        .map(|(i, t)| (i, budget(t)))
        .collect();
    let mut results = record_pass(tests, &all, dir, jobs, progress);
    let retry: Vec<(usize, Duration)> = results
        .iter()
        .filter(|(_, r)| matches!(r, Err(RecordFailure::TimedOut(_))))
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
            Ok((sites, secs)) => {
                recording.secs_by_test.insert(key.clone(), secs);
                recording.sites_by_test.insert(key, sites);
            }
            Err(why) => recording.not_green.push((key, why.to_string())),
        }
    }
    recording.not_green.sort();
    recording
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
            RecordFailure::TimedOut(after) => write!(f, "timed out after {after:?}"),
            RecordFailure::Other(why) => f.write_str(why),
        }
    }
}

/// One test's recorded sites and wall seconds, or why it selects nothing.
type RecordResult = Result<(std::collections::BTreeSet<u32>, f64), RecordFailure>;

fn record_one(t: &ResolvedTest, index: usize, dir: &Path, timeout: Duration) -> RecordResult {
    let other = RecordFailure::Other;
    let path = dir.join(format!("{index:06}.txt"));
    fs::write(&path, b"").map_err(|err| other(format!("create {}: {err}", path.display())))?;
    let env = test_env(
        t,
        &[(
            "QS_RECORD_SITES".to_string(),
            path.to_string_lossy().into_owned(),
        )],
    );
    let started = Instant::now();
    let status = run_exact_with_env(&t.binary, &t.name, &t.cwd, timeout, &env)
        .map_err(|err| other(format!("{err:#}")))?;
    let secs = started.elapsed().as_secs_f64();
    match status {
        RunStatus::Passed => {}
        RunStatus::Failed => return Err(other("failed with no mutant active".into())),
        RunStatus::Error { code } => {
            return Err(other(format!("exited {code:?} with no mutant active")))
        }
        RunStatus::Timeout => return Err(RecordFailure::TimedOut(timeout)),
    }
    let text = fs::read_to_string(&path)
        .map_err(|err| other(format!("read {}: {err}", path.display())))?;
    Ok((
        text.lines().filter_map(|l| l.trim().parse().ok()).collect(),
        secs,
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
        .map(|t| ResolvedTest {
            id: t.id.clone(),
            name: t.name.clone(),
            binary: run_dir.join(&t.binary),
            cwd: if t.cwd_rel.is_empty() {
                tree_root.to_path_buf()
            } else {
                tree_root.join(&t.cwd_rel)
            },
            target_dir: tree_root.join("target"),
            duration_secs: planned
                .tests
                .iter()
                .find(|p| !p.id.is_empty() && p.id == t.id)
                .or_else(|| planned.tests.iter().find(|p| p.name == t.name))
                .map(|p| p.duration_secs)
                .unwrap_or(0.001),
        })
        .collect())
}

/// A test that does not pass even with no mutant active. The mutant tells us
/// nothing about it, so it is skipped rather than charged to the mutant.
#[derive(Debug, Clone)]
struct InfraSkip {
    test: String,
    code: Option<i32>,
    stderr: String,
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
    R: FnMut(&RunSpec) -> Result<(RunStatus, Duration, String)>,
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
        let budget = test_timeout_scaled(expected_secs(t), policy.floor, policy.slowdown);
        let (first, first_secs, _) = run(&spec_for(t, budget, extra_env, false))?;
        if first == RunStatus::Passed {
            observations.push(observation(t, TestVerdict::Passed, first_secs));
            continue;
        }
        if !policy.confirm {
            let outcome = match first {
                RunStatus::Passed => unreachable!(),
                RunStatus::Failed => Outcome::caught(planned.mutant.clone(), t.name.clone()),
                RunStatus::Error { code } => Outcome::errored(
                    planned.mutant.clone(),
                    t.name.clone(),
                    format!(
                        "test {} exited {:?} (libtest failures are 101; this is not Caught)",
                        t.name, code
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
            confirm_timeout(planned, t, policy, extra_env, run)?
        } else {
            confirm_failure(planned, t, first, budget, extra_env, run)?
        };
        match step {
            Step::Next => observations.push(observation(t, TestVerdict::Passed, first_secs)),
            Step::Skip(s) => {
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
    let message = format!(
        "{} selected test(s) unusable under schema binary (fail with no mutant active); first: {} exited {:?}; stderr: {}",
        skipped.len(),
        first.test,
        first.code,
        first.stderr
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
    R: FnMut(&RunSpec) -> Result<(RunStatus, Duration, String)>,
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

fn verdict_for_outcome(outcome: &Outcome) -> TestVerdict {
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
    R: FnMut(&RunSpec) -> Result<(RunStatus, Duration, String)>,
{
    let (retry, retry_secs, retry_err) = run(&spec_for(t, budget, extra_env, true))?;
    tracing::info!(
        "confirm {}: retry with mutant {:?} in {:.2}s; stderr: {}",
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
    let (null, null_secs, null_err) = run(&spec_for(t, budget, &[], true))?;
    tracing::info!(
        "confirm {}: null run {:?} in {:.2}s; stderr: {}",
        t.name,
        null,
        null_secs.as_secs_f64(),
        tail(&null_err, MESSAGE_TAIL)
    );
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
        let code = match first {
            RunStatus::Error { code } => code,
            _ => status_code(retry),
        };
        // A deterministic crash (stack overflow, abort, OOM kill, a nonzero
        // non-101 exit) under the mutant, on the same schema binary that
        // passes without it, is a test detecting the mutant.
        let message = format!(
            "{} ({}) and passes without it; test {}; stderr: {}",
            crate::types::CRASH_PREFIX,
            crate::types::describe_exit(code),
            t.name,
            tail(&retry_err, MESSAGE_TAIL)
        );
        tracing::info!(
            "confirm {}: CAUGHT by crash ({}), passes without",
            t.name,
            crate::types::describe_exit(code)
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
        code: status_code(null),
        stderr: tail(&null_err, MESSAGE_TAIL),
    }))
}

/// First pass timed out. The green duration was measured on a normal binary,
/// so the budget may simply have been too small: measure the test with no
/// mutant active on this binary, then re-run the mutant against 3× that.
fn confirm_timeout<R>(
    planned: &PlannedMutant,
    t: &ResolvedTest,
    policy: &TestRunPolicy,
    extra_env: &[(String, String)],
    run: &mut R,
) -> Result<Step>
where
    R: FnMut(&RunSpec) -> Result<(RunStatus, Duration, String)>,
{
    // Only the calibration can take minutes; the re-run that follows is
    // budgeted from it, so it runs without the lock. Holding the lock across
    // both made N hanging mutants cost N full re-runs in series.
    let null_secs = match calibrated_secs(t) {
        Some(secs) => {
            tracing::info!(
                "confirm {}: null run calibrated earlier in {secs:.2}s",
                t.name
            );
            secs
        }
        None => {
            let calibration = policy.floor.max(Duration::from_secs(120));
            let (null, null_secs, null_err) = {
                let _guard = confirm_lock();
                run(&spec_for(t, calibration, &[], true))?
            };
            tracing::info!(
                "confirm {}: null calibration {:?} in {:.2}s (budget {:.1}s)",
                t.name,
                null,
                null_secs.as_secs_f64(),
                calibration.as_secs_f64()
            );
            if null != RunStatus::Passed {
                tracing::info!(
                    "confirm {}: infra-skip ({:?} with no mutant active at {:.1}s budget); next test",
                    t.name,
                    null,
                    calibration.as_secs_f64()
                );
                return Ok(Step::Skip(InfraSkip {
                    test: t.name.clone(),
                    code: status_code(null),
                    stderr: tail(&null_err, MESSAGE_TAIL),
                }));
            }
            let secs = null_secs.as_secs_f64();
            CALIBRATED
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(t.id.clone(), secs);
            secs
        }
    };
    let budget = duration_from_secs((null_secs * 3.0).max(policy.floor.as_secs_f64()));
    let (again, again_secs, _) = run(&spec_for(t, budget, extra_env, false))?;
    tracing::info!(
        "confirm {}: re-run with mutant {:?} in {:.2}s (calibrated budget {:.1}s)",
        t.name,
        again,
        again_secs.as_secs_f64(),
        budget.as_secs_f64()
    );
    match again {
        RunStatus::Timeout => {
            tracing::info!(
                "confirm {}: TIMEOUT attributed to mutant (null passed in {:.2}s)",
                t.name,
                null_secs
            );
            Ok(Step::Done(Outcome::timeout(
                planned.mutant.clone(),
                t.name.clone(),
            )))
        }
        RunStatus::Failed => {
            tracing::info!("confirm {}: CAUGHT on the calibrated re-run", t.name);
            Ok(Step::Done(Outcome::caught(
                planned.mutant.clone(),
                t.name.clone(),
            )))
        }
        RunStatus::Passed => {
            tracing::info!(
                "confirm {}: passed on the calibrated re-run (first timeout was budget, not mutant); next test",
                t.name
            );
            Ok(Step::Next)
        }
        RunStatus::Error { code } => {
            let message = format!(
                "{} ({}) on the calibrated re-run; it passed without the mutant in {:.2}s; test {}",
                crate::types::CRASH_PREFIX,
                crate::types::describe_exit(code),
                null_secs,
                t.name
            );
            tracing::info!(
                "confirm {}: CAUGHT by crash on the calibrated re-run",
                t.name
            );
            Ok(Step::Done(Outcome::caught_by_crash(
                planned.mutant.clone(),
                t.name.clone(),
                message,
            )))
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

fn status_code(status: RunStatus) -> Option<i32> {
    match status {
        RunStatus::Passed => Some(0),
        RunStatus::Failed => Some(LIBTEST_FAILURE_CODE),
        RunStatus::Error { code } => code,
        RunStatus::Timeout => None,
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
                },
                ManifestTest {
                    name: "test_parse".into(),
                    binary: "test_version-bbbb".into(),
                    cwd_rel: "".into(),
                    id: "semver::test_version::test_parse".into(),
                },
            ],
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
                target_dir: dir.path().join("target"),
                duration_secs: 0.01,
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
    fn every_run_builds_into_the_tree_target_dir() {
        let t = &fake_tests(&["t1"])[0];
        let spec = spec_for(t, Duration::from_secs(1), &[], false);
        assert_eq!(
            spec.env,
            vec![("CARGO_TARGET_DIR".to_string(), "/target".to_string())]
        );
    }

    fn fake_tests(names: &[&str]) -> Vec<ResolvedTest> {
        names
            .iter()
            .map(|n| ResolvedTest {
                id: format!("id-{n}"),
                name: (*n).into(),
                binary: PathBuf::from("/bin/true"),
                cwd: PathBuf::from("/"),
                target_dir: PathBuf::from("/target"),
                duration_secs: 0.41,
            })
            .collect()
    }

    /// Drive the machine with a scripted sequence of run results and return
    /// the verdict plus every RunSpec the machine asked for.
    fn drive(
        confirm: bool,
        tests: &[ResolvedTest],
        script: &[(RunStatus, f64, &str)],
    ) -> (Outcome, Vec<RunSpec>) {
        let planned = PlannedMutant {
            mutant: fake_mutant(),
            tests: Vec::new(),
            estimated_cost_secs: 0.0,
        };
        let policy = TestRunPolicy {
            floor: Duration::from_secs(30),
            slowdown: 3.0,
            confirm,
        };
        let env = vec![("QS_MUTANT".to_string(), "site:variant".to_string())];
        let mut seen: Vec<RunSpec> = Vec::new();
        let mut next = 0usize;
        let outcome = {
            let mut run = |spec: &RunSpec| -> Result<(RunStatus, Duration, String)> {
                seen.push(spec.clone());
                let (status, secs, err) = script
                    .get(next)
                    .unwrap_or_else(|| panic!("unscripted run #{next}: {}", spec.test_name));
                next += 1;
                Ok((*status, Duration::from_secs_f64(*secs), (*err).to_string()))
            };
            run_selected_tests(&planned, tests, &policy, &env, &mut run).unwrap()
        };
        (outcome, seen)
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
    fn confirm_fail_twice_then_null_fail_is_infra_skip_then_errored() {
        let (outcome, seen) = drive(
            true,
            &fake_tests(&["t1"]),
            &[
                (RunStatus::Failed, 0.5, ""),
                (RunStatus::Failed, 0.5, ""),
                (RunStatus::Failed, 0.5, "panicked at src/lib.rs:12"),
            ],
        );
        assert_eq!(seen.len(), 3);
        assert_eq!(outcome.kind, VerdictKind::Errored);
        let msg = outcome.message.unwrap();
        assert!(msg.contains("1 selected test(s) unusable under schema binary"));
        assert!(msg.contains("exited Some(101)"), "{msg}");
        assert!(msg.contains("panicked at src/lib.rs:12"), "{msg}");
    }

    #[test]
    fn confirm_timeout_with_fast_null_and_second_timeout_is_a_timeout() {
        let (outcome, seen) = drive(
            true,
            &fake_tests(&["timeout-rerun"]),
            &[
                (RunStatus::Timeout, 30.0, ""),
                (RunStatus::Passed, 12.0, ""),
                (RunStatus::Timeout, 36.0, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Timeout);
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[1].timeout, Duration::from_secs(120), "calibration");
        assert!(!has_mutant(&seen[1]));
        // 3 × the 12s null run beats the 30s floor.
        assert_eq!(seen[2].timeout, Duration::from_secs(36));
        assert!(has_mutant(&seen[2]));
    }

    #[test]
    fn confirm_timeout_then_crash_on_the_rerun_is_caught_by_crash() {
        // A runaway mutant: the first pass times out, the test passes with no
        // mutant, and the calibrated re-run is killed (an OOM kill shows up
        // as a signal, code None).
        let (outcome, seen) = drive(
            true,
            &fake_tests(&["timeout-crash"]),
            &[
                (RunStatus::Timeout, 30.0, ""),
                (RunStatus::Passed, 0.5, ""),
                (RunStatus::Error { code: None }, 4.0, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Caught);
        let msg = outcome.message.unwrap();
        assert!(msg.starts_with(crate::types::CRASH_PREFIX), "{msg}");
        assert!(msg.contains("killed by a signal"), "{msg}");
        assert_eq!(seen.len(), 3);
    }

    #[test]
    fn confirm_timeout_with_null_timeout_is_infra_skip() {
        let (outcome, seen) = drive(
            true,
            &fake_tests(&["timeout-null"]),
            &[
                (RunStatus::Timeout, 30.0, ""),
                (RunStatus::Timeout, 120.0, ""),
            ],
        );
        assert_eq!(seen.len(), 2, "no mutant re-run once the null run hangs");
        assert_eq!(outcome.kind, VerdictKind::Errored);
        let msg = outcome.message.unwrap();
        assert!(msg.contains("unusable under schema binary"), "{msg}");
        assert!(msg.contains("exited None"), "{msg}");
    }

    #[test]
    fn confirm_timeout_calibrated_pass_moves_on_to_the_next_test() {
        let (outcome, seen) = drive(
            true,
            &fake_tests(&["timeout-pass", "t2"]),
            &[
                (RunStatus::Timeout, 30.0, ""),
                (RunStatus::Passed, 12.0, ""),
                (RunStatus::Passed, 13.0, ""),
                (RunStatus::Passed, 0.5, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Missed);
        assert_eq!(seen.len(), 4);
        assert_eq!(seen[3].test_name, "t2");
    }

    /// A test whose baseline budget is too tight under load is calibrated
    /// once; later mutants get a first-pass budget from the calibrated
    /// duration and skip the calibration run.
    #[test]
    fn a_calibrated_test_keeps_its_duration_for_later_mutants() {
        let tests = fake_tests(&["calibrated-once"]);
        let (outcome, seen) = drive(
            true,
            &tests,
            &[
                (RunStatus::Timeout, 30.0, ""),
                (RunStatus::Passed, 40.0, ""),
                (RunStatus::Passed, 41.0, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Missed);
        assert_eq!(seen[1].timeout, Duration::from_secs(120), "calibration");

        // 3 x 40s x slowdown 3 = 360s, instead of the 30s floor.
        let (outcome, seen) = drive(true, &tests, &[(RunStatus::Passed, 41.0, "")]);
        assert_eq!(outcome.kind, VerdictKind::Missed);
        assert_eq!(seen[0].timeout, Duration::from_secs(360));

        // A real hang still times out, and is confirmed without recalibrating:
        // the re-run is budgeted from the stored 40s.
        let (outcome, seen) = drive(
            true,
            &tests,
            &[
                (RunStatus::Timeout, 360.0, ""),
                (RunStatus::Timeout, 120.0, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Timeout);
        assert_eq!(seen.len(), 2, "no second calibration");
        assert_eq!(seen[1].timeout, Duration::from_secs(120));
        assert!(has_mutant(&seen[1]));
    }

    #[test]
    fn confirm_crash_twice_then_null_pass_is_caught_by_crash() {
        let (outcome, _) = drive(
            true,
            &fake_tests(&["t1"]),
            &[
                (RunStatus::Error { code: Some(1) }, 0.5, ""),
                (
                    RunStatus::Error { code: Some(1) },
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
            msg.contains("stderr: error while loading shared libraries"),
            "{msg}"
        );
    }

    #[test]
    fn confirm_error_twice_then_null_error_is_infra_skip() {
        let (outcome, seen) = drive(
            true,
            &fake_tests(&["t1"]),
            &[
                (RunStatus::Error { code: Some(1) }, 0.5, ""),
                (RunStatus::Error { code: Some(1) }, 0.5, "boom"),
                (
                    RunStatus::Error { code: Some(1) },
                    0.5,
                    "no mutant, still boom",
                ),
            ],
        );
        assert_eq!(seen.len(), 3);
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
