use crate::exec::process::{
    run_exact_capture_with_env, run_exact_with_env, RunStatus, LIBTEST_FAILURE_CODE,
};
use crate::types::{Outcome, PlannedMutant};
use crate::util::duration_from_secs;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
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
    name: String,
    binary: PathBuf,
    cwd: PathBuf,
    duration_secs: f64,
}

/// Confirmations are serialized process-wide. A confirmation costs up to a
/// 120s calibration run; letting all test-pool threads do that at once would
/// both swamp the box and make every measured duration useless.
static CONFIRM_LOCK: Mutex<()> = Mutex::new(());

fn confirm_lock() -> std::sync::MutexGuard<'static, ()> {
    CONFIRM_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Cap on the stderr tail copied into an `Outcome` message. The capture
/// buffer holds 64KB; outcomes.jsonl does not need all of it.
const MESSAGE_TAIL: usize = 2000;

pub fn run_mutant_tests(
    planned: &PlannedMutant,
    run_dir: &Path,
    baseline_root: &Path,
    timeout_floor: Duration,
) -> Result<Outcome> {
    run_mutant_tests_with_env(planned, run_dir, baseline_root, timeout_floor, &[])
}

pub fn run_mutant_tests_with_env(
    planned: &PlannedMutant,
    run_dir: &Path,
    baseline_root: &Path,
    timeout_floor: Duration,
    extra_env: &[(String, String)],
) -> Result<Outcome> {
    let policy = TestRunPolicy {
        floor: timeout_floor,
        slowdown: 1.0,
        confirm: false,
    };
    run_mutant_tests_policy(planned, run_dir, baseline_root, &policy, extra_env)
}

pub fn run_mutant_tests_policy(
    planned: &PlannedMutant,
    run_dir: &Path,
    baseline_root: &Path,
    policy: &TestRunPolicy,
    extra_env: &[(String, String)],
) -> Result<Outcome> {
    let tests = resolve_tests(planned, run_dir, baseline_root)?;
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
    run_selected_tests(planned, &tests, policy, extra_env, &mut run)
}

fn resolve_tests(
    planned: &PlannedMutant,
    run_dir: &Path,
    baseline_root: &Path,
) -> Result<Vec<ResolvedTest>> {
    let manifest_path = run_dir.join("manifest.json");
    let text = fs::read_to_string(&manifest_path)?;
    let manifest: BinariesManifest = serde_json::from_str(&text)?;
    let tests: Vec<&ManifestTest> = if planned.tests.is_empty() {
        manifest.tests.iter().collect()
    } else {
        planned
            .tests
            .iter()
            .filter_map(|want| {
                manifest.tests.iter().find(|t| {
                    (!want.id.is_empty() && (t.id == want.id || t.name == want.name))
                        || (t.name == want.name && t.cwd_rel == want.cwd_rel)
                        || t.name == want.name
                })
            })
            .collect()
    };
    let tests = if tests.is_empty() {
        manifest.tests.iter().collect::<Vec<_>>()
    } else {
        tests
    };

    Ok(tests
        .into_iter()
        .map(|t| ResolvedTest {
            name: t.name.clone(),
            binary: run_dir.join(&t.binary),
            cwd: if t.cwd_rel.is_empty() {
                baseline_root.to_path_buf()
            } else {
                baseline_root.join(&t.cwd_rel)
            },
            duration_secs: planned
                .tests
                .iter()
                .find(|p| {
                    (!p.id.is_empty() && (p.id == t.id || p.name == t.name)) || p.name == t.name
                })
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
    // Running no test at all is not evidence that the mutant survived. An
    // empty selection here means staging or attribution produced nothing (the
    // manifest fallback in `resolve_tests` has already had its turn), so the
    // mutant has to be reported as an error rather than a silent Missed.
    if tests.is_empty() {
        return Ok(Outcome::errored(
            planned.mutant.clone(),
            String::new(),
            "no tests were staged for this mutant (empty per-test attribution)".into(),
        ));
    }
    let mut skipped: Vec<InfraSkip> = Vec::new();
    for t in tests {
        let budget = test_timeout_scaled(t.duration_secs, policy.floor, policy.slowdown);
        let (first, first_secs, _) = run(&spec_for(t, budget, extra_env, false))?;
        if first == RunStatus::Passed {
            continue;
        }
        if !policy.confirm {
            return Ok(match first {
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
            });
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
            Step::Next => continue,
            Step::Skip(s) => skipped.push(s),
            Step::Done(o) => return Ok(o),
        }
    }
    if skipped.is_empty() {
        return Ok(Outcome::missed(planned.mutant.clone()));
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
    Ok(Outcome::errored(
        planned.mutant.clone(),
        first.test.clone(),
        message,
    ))
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
    let _guard = confirm_lock();
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
        let message = format!(
            "test {} exited {:?} (libtest failures are 101; this is not Caught); stderr: {}",
            t.name,
            code,
            tail(&retry_err, MESSAGE_TAIL)
        );
        tracing::info!(
            "confirm {}: ERRORED under mutant, passes without: {}",
            t.name,
            message
        );
        return Ok(Step::Done(Outcome::errored(
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
    let _guard = confirm_lock();
    let calibration = policy.floor.max(Duration::from_secs(120));
    let (null, null_secs, null_err) = run(&spec_for(t, calibration, &[], true))?;
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
    let budget = duration_from_secs((null_secs.as_secs_f64() * 3.0).max(policy.floor.as_secs_f64()));
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
                null_secs.as_secs_f64()
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
                "test {} exited {:?} (libtest failures are 101; this is not Caught)",
                t.name, code
            );
            tracing::info!("confirm {}: ERRORED on the calibrated re-run", t.name);
            Ok(Step::Done(Outcome::errored(
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
        env: env.to_vec(),
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

    fn fake_tests(names: &[&str]) -> Vec<ResolvedTest> {
        names
            .iter()
            .map(|n| ResolvedTest {
                name: (*n).into(),
                binary: PathBuf::from("/bin/true"),
                cwd: PathBuf::from("/"),
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
            &[
                (RunStatus::Failed, 0.5, ""),
                (RunStatus::Passed, 0.5, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Missed);
        assert_eq!(seen.len(), 2, "no null run after a passing retry");
        assert!(!seen[1].env.is_empty(), "retry keeps the mutant env");
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
        assert!(seen[2].env.is_empty(), "null run drops the mutant env");
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
            &fake_tests(&["t1"]),
            &[
                (RunStatus::Timeout, 30.0, ""),
                (RunStatus::Passed, 12.0, ""),
                (RunStatus::Timeout, 36.0, ""),
            ],
        );
        assert_eq!(outcome.kind, VerdictKind::Timeout);
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[1].timeout, Duration::from_secs(120), "calibration");
        assert!(seen[1].env.is_empty());
        // 3 × the 12s null run beats the 30s floor.
        assert_eq!(seen[2].timeout, Duration::from_secs(36));
        assert!(!seen[2].env.is_empty());
    }

    #[test]
    fn confirm_timeout_with_null_timeout_is_infra_skip() {
        let (outcome, seen) = drive(
            true,
            &fake_tests(&["t1"]),
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
            &fake_tests(&["t1", "t2"]),
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

    #[test]
    fn confirm_error_twice_then_null_pass_reports_stderr() {
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
        assert_eq!(outcome.kind, VerdictKind::Errored);
        let msg = outcome.message.unwrap();
        assert!(msg.contains("exited Some(1)"), "{msg}");
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
                (RunStatus::Error { code: Some(1) }, 0.5, "no mutant, still boom"),
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
