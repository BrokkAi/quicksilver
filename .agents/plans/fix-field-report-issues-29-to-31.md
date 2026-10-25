# Fix the brokk-bifrost-jvm field-report batch (BrokkAi/quicksilver#29 to #31)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md` at the repository root.


## Purpose / Big Picture


The agent testing cargo-quicksilver (QS) on Bifrost filed three issues from the brokk-bifrost-jvm round (a 24-CPU host at load 25 to 60; 276 of 1,257 kills were timeouts). After this plan:

- A timeout counts as a kill only when CPU time shows the mutant caused it: the mutant burned far more CPU than the test normally uses, or it stopped using CPU (blocked). A test merely slowed by load is an inconclusive timeout (#29).
- The `test` summary separates kills by timeout and by crash; `test --again timeouts|errored` re-tests only those mutants (#29, #30).
- A test that fails with no mutant active is run once more before it is declared unusable, and messages name the signal that stopped a test (#30).
- `cargo quicksilver survivors` exports each missed mutant with its intent, a unified diff and its tests, split by file for parallel reviewers (#31).


## Progress


- [x] (2026-10-25) Read the three issues and the timeout, failure-confirmation and process code.
- [x] (2026-10-25) #31, by a child agent, reviewed and cherry-picked (43f7b67), with two review fixes (b2a62b6): diffs no longer end with a false "No newline" marker, and "still at HEAD" matches methods with their type.
- [x] (2026-10-25) #29 and #30, by a second child agent, reviewed and cherry-picked (a62bbc2, 3823f74). Its own plan: `.agents/plans/cpu-timeout-and-null-retry.md`.
- [x] (2026-10-25) Review fixes for #29: a failed `/proc` read at a deadline falls back to the reaped CPU instead of failing the run; the inconclusive-timeout message gives the measured wall and CPU times.
- [x] (2026-10-25) Full test run on the combined code; pushed.


## Surprises & Discoveries


- The false timeout kills come from `confirm_timeout`'s calibration: a null run (no mutant) is timed once per test and cached for the whole run, and every later timeout of that test is judged against that one wall time. On a host whose load varies eightfold, a test that once ran fast makes every later load-induced timeout look like a kill.


## Decision Log


- Decision (#29): judge timeouts by CPU time, not wall time. Each test run reports CPU (rusage from `wait4` at a normal exit; at a deadline kill, the sum over `/proc` of the test's process group, so running subprocesses count). The map records each test's CPU (C0). A first-pass timeout with CPU ≥ 3·C0 + 1 s is caught at once (load cannot inflate CPU that much). Otherwise the mutant runs once more at 4× the budget: finishing decides normally; CPU ≥ 3·C0 + 1 s is caught; a CPU rate under 5% of one CPU is caught as blocked (a deadlock); anything else is an inconclusive timeout. The per-run null calibration is removed.
  Rationale: an infinite loop burns CPU; a deadlock uses none; a test starved by load still gets a large share of a CPU (at load 60 on 24 CPUs, about 40%). Only the last case is truly ambiguous, and it is reported as such instead of as a kill.
  Date/Author: 2026-10-25, Claude.

- Decision (#29, #30): `Status::Tested` records the generation and activation a mutant was built with, so `test --again timeouts|errored` can re-test those mutants at once when their generation is current, and otherwise sends them back to `build`.
  Date/Author: 2026-10-25, Claude.

- Decision (#30): only a kill QS made at its deadline is a `Timeout`; any other signal (an outside SIGKILL included) is an error naming the signal.
  Date/Author: 2026-10-25, Claude.

- Decision (#31): `survivors` follows `pending` (map at HEAD, state, `--package`, `--file`, `--json`, `--split N --out DIR`), keeps each file's survivors in one list when splitting, prints unified diffs with file line numbers and repository-relative paths, and says whether each function still exists at HEAD.
  Date/Author: 2026-10-25, Claude.


## Outcomes & Retrospective


All three issues are closed. The unit tests (165), `tests/edited.rs`, `tests/engine.rs` and `tests/schema.rs` pass; the engine test's infinite-loop mutant is caught as a CPU runaway. Unit tests drive every branch of the new timeout rule with a fake runner, check that the CPU of a busy child in the test's process group counts at a deadline kill, that an outside SIGKILL or SIGABRT is an error naming the signal, the null-run retry, and the `--again` state transitions.

Not measured: how many of the jvm round's 276 timeout kills the new rule would keep. That needs the next round on a loaded host; the bifrost session can recheck that round's timeouts with `test --again timeouts`.

The `survivors` diffs show each body with its file line numbers, but they cannot be applied with `git apply`: a body's first line is only the tail of the file's signature line.


## Context and Orientation


Key code: `src/exec/process.rs` (running one test), `confirm_timeout` and `confirm_failure` in `src/exec/test.rs`, `test()` and `Status` in `src/exec/edited.rs`, `src/pending.rs` (the model for `survivors`).


## Plan of Work, Concrete Steps, Validation


Each child commits in its worktree; I review, cherry-pick, run `cargo test --lib`, `cargo test --test edited --test engine --test schema` and clippy on the combined code, and push.


## Idempotence and Recovery


Plain code changes; each commit can be reverted.


## Artifacts and Notes


Commits: 43f7b67 and b2a62b6 (#31), a62bbc2 (#29), 3823f74 (#30), plus the review fixes for #29 in the commit that records this plan's outcome.


## Interfaces and Dependencies


No new crates expected (`libc` is already a dependency).
