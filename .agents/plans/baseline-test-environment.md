# Apply test-only environment to the nextest baseline

This living ExecPlan follows `.agents/PLANS.md`.

## Purpose / Big Picture

The original baseline received only build-env, while schema and mutant tests received test-env; commit e20d852 fixed that mismatch. A scale follow-up now requires reclaiming completed baseline test scratch while nextest is still running, so caches do not accumulate for an entire large suite. The campaign reproduced a real test expecting BIFROST_QS_ENV_PROBE=configured that fails in baseline despite its quicksilver.toml setting. Tests must receive the same configured test-only values without passing them to Cargo/rustc. Keep selected nextest profiles, overrides, retries, budgets, wrappers and target runners working. A sandbox placeholder must stay private to a subprocess; baseline HOME/TMP remain in the caller environment unless explicitly overridden.

## Progress

- [x] (2026-11-03) Inspect source and report: run_green_suite applies only build_env; Settings.prepare applies test_env only to QS direct test runs. Sub-agents remain disabled for this session, so parent owns implementation/checks.
- [x] (2026-11-03) Reproduce with a committed real CLI fixture: control fails red baseline with NotPresent; fixed CLI maps/builds/tests a caught mutant while build.rs and option_env! reject compiler leaks.
- [x] (2026-11-03) Implement environment-only helper, private specification and nextest overlays for listing/baseline; retain original project wrappers, all four runner policies and inherited profile rules.
- [x] (2026-11-03) Validate real nextest runner policies, special-character values, explicit env precedence, preserved HOME, inherited TIMEOUT and scratch cleanup including timeout. Plain test-env also passes with --no-sandbox.
- [x] (2026-11-03) Format and Clippy clean; 226 tests passed across all suites, two existing unit ignores. Adapt the old schema fixture to distinguish recording from baseline explicitly, then rerun its failure and finish the previously unrun suites.
- [x] (2026-11-03) Commit e20d852, obtain clean required review (tree 2199c69), push origin/master, verify exact-head CI success and send install/map clearance to Bifrost session 5b9275ef422b8eb3146a4a11fb9dd1aa.

- [x] (2026-11-03) Follow-up source diagnosis: exec replaces the helper guard; only Prepared::drop sweeps records, retaining every completed invocation until nextest exits. Peer reports ENOSPC at about 11k tests; total disk attribution remains unconfirmed.
- [x] (2026-11-03) Prove the bug with a real-nextest barrier: original e20 cleanup fails after 8 seconds with completed scratch retained; fixed code passes for normal, SIGTERM and TIMEOUT exits while observer scratch stays live, in all four wrapper/runner modes.
- [x] (2026-11-03) Add a joined owner-side Reaper, sweeping recorded dead PIDs during list/run at a 100 ms interval. No helper exec, runner, timeout or build-env changes. Retain failed-removal records for retry and cover retry/final join with a unit test.
- [x] (2026-11-03) Full local checks pass: format, Clippy and 227 tests (202 unit, 10 campaign, three edited, nine engine, three schema), with two existing unit ignores; log /tmp/qs-test-scratch-checks.log.
- [x] (2026-11-03) Commit 2e35d9c, obtain clean required review (tree 40c8d0d), push origin/master and verify exact-head CI success: https://github.com/BrokkAi/quicksilver/actions/runs/41748356081 . Send verified install handoff to Bifrost session 5b9275ef422b8eb3146a4a11fb9dd1aa; remote diagnostics retain responsibility for rerun readiness.

## Surprises & Discoveries

Nextest supports list/run wrapper scripts since 0.9.98, enabled with experimental wrapper-scripts in the generated configuration. A wrapper receives the target runner and test argv according to its target-runner policy. Only the first matching wrapper is used, so a blanket new wrapper rule would overwrite user wrappers. Existing wrapper commands must instead be composed with QS's environment helper, while a lowest-priority fallback handles tests without a project wrapper. Nextest binaries-only metadata provides paths without executing test processes.

The first full check found that the existing schema fixture treated FIXTURE_STATE and FIXTURE_NOT_GREEN as schema-only flags, relying on the very omission being fixed. It now checks QS_RECORD_SITES/QS_MUTANT for direct-run-only assertions and deliberately fails its diagnostic test only during recording. Baseline scratch freshness is still checked, without writing to real HOME.

Evidence logs: /tmp/qs-test-env-control.log (red baseline before fix); /tmp/qs-test-env-fixed.log (initial real CLI pass); /tmp/qs-test-env-policy.log (runner policies); /tmp/qs-test-env-checks.log (Clippy, units, campaign and original fixture failure); /tmp/qs-test-env-edited-retry.log (fixed failing case); /tmp/qs-test-env-remaining.log (engine/schema). The initial check passed 201 unit and 10 campaign tests; the edited suite passed its other two tests and its failing case passed on rerun; engine nine and schema three then passed. Two unit tests retain their existing ignored status.

The scale report describes ENOSPC during a root CLI baseline with per-test XDG_CACHE_HOME placeholders. Source conclusively shows completed-cache retention; the report does not yet attribute all remote disk usage to it. The new acceptance test observes cleanup before suite completion, unlike the original test which checked only after prepare_baseline returned. Evidence is /tmp/qs-test-scratch-control.log (old code fails the barrier) and /tmp/qs-test-scratch-fixed.log (four policies pass). /tmp/qs-test-scratch-checks.log captures full checks. Cleanup latency is the sweep interval plus any deletion backlog, not a hard storage-byte limit; no remote disk attribution or benchmark claim is inferred.

## Decision Log

- Decision: Apply test-env in a hidden QS wrapper immediately before exec of the test, project wrapper or runner, rather than putting it on the cargo-nextest process.
  Rationale: Cargo builds and metadata retain build-env; arbitrary test-only CARGO_HOME/PATH/flags do not affect QS compilation.
  Date/Author: 2026-11-03 Codex.
- Decision: Preserve baseline HOME/TMP defaults; allocate the normal private scratch structure only when a configured value uses {sandbox}. Schema/mutant full sandboxes keep their existing behavior. Exec the test chain so nextest receives its native exit/signal behavior, and clean completed scratch records after nextest returns.
  Rationale: The documented unsandboxed baseline stays compatible while explicit placeholders remain per-process and no timeout policy is reimplemented.
  Date/Author: 2026-11-03 Codex.
- Decision: Preserve original wrapper rules, compose each original command and target-runner policy, and append a fallback to the default profile. Require nextest 0.9.98+ only when test-env needs this feature.
  Rationale: Project timeouts, inherited overrides, setup scripts and wrappers must not be silently replaced.
  Date/Author: 2026-11-03 Codex.

- Decision: Supersede suite-end-only cleanup with an owner-side thread that periodically reclaims recorded dead test PIDs during list/run and joins with a final sweep on teardown. Keep helper exec unchanged.
  Rationale: Completed test cache retention then follows process completion plus sweep backlog, not total suite length, while nextest still directly owns the process it signals and observes. No extra per-test supervising process, concurrency cap, filesystem/target relocation or mbx policy change is needed. Allocate this worker only for settings containing the sandbox placeholder; retry failed removals by retaining their records.
  Date/Author: 2026-11-03 Codex.

## Outcomes & Retrospective

The original environment fix and local acceptance checks are complete. The scratch-lifetime follow-up is complete at reviewed, pushed commit 2e35d9c6f337630daffd4a6c63bf66e7168341b3. All 227 local tests plus format/Clippy pass; exact-head CI passed both jobs at https://github.com/BrokkAi/quicksilver/actions/runs/41748356081 . The peer received the verified install handoff. The original final-only cleanup proved insufficient at scale. Baseline and listing tests receive configured test-only values while Cargo/rustc do not. Existing nextest wrappers/runners and timeout policy remain effective; placeholder storage is private and removed after the nextest run. No Bifrost source/config workaround, target layout, mbx policy or engine change is needed. Commit e20d85255c7805e3f19e278d0f11ead80f9699de passed required review and is on origin/master. Exact-head CI succeeded in both jobs: https://github.com/BrokkAi/quicksilver/actions/runs/41740977340 . The peer received the verified install/map handoff. That original completion record changed no executable source. The accepted executable install head is now 2e35d9c, superseding e20d852.

## Context and Orientation

`src/baseline.rs::run_green_suite` launches mbx nextest run and parses its JUnit failures and timings. `src/cargo_cmd.rs::write_nextest_config` merges project policy, changing fail-fast and JUnit output only. `src/index.rs::list_tests_plain` builds/lists tests through mbx. `src/sandbox.rs::Settings` applies configured test values and per-process sandbox placeholders for QS direct runs. Add `src/baseline_env.rs` to generate the wrapper overlay and dispatch its hidden helper from `src/lib.rs` before leaving the inherited mbx test session. Keep original project environment and runner behavior apart from explicit test-env overrides.

## Plan of Work

First add a real temporary package with build.rs checking test-only variables are absent, compile-time option_env assertions, and tests requiring configured values while the outer process lacks them. Confirm failure on current code. Extend Settings with an environment-only preparation path that preserves baseline variables and reuses placeholder allocation/cleanup.

Write a private serialized wrapper specification alongside generated nextest config. Obtain actual test executable paths with nextest list --list-type binaries-only under build-env. Compose existing wrapper command forms (string, argv and command table), relative-to, command env and target-runner policy; use shell-words parsing consistent with nextest. Preserve profile script rules and append the fallback after project default rules. Helper exec passes original argv and status/signal behavior unchanged. Use the generated overlay for full list operations as well as baseline test execution, with cleanup on success and error.

Exercise map/build/test and no-sandbox plain values, special-character values, per-test scratch isolation, project wrappers/runners and selected timeout overrides. Update RUNNING.md with the test/build environment boundary and conditional nextest minimum.

## Milestones

Milestone 1 reproduces the mismatch at the real CLI boundary and supplies test-env through the nextest wrapper helper. It is verified by a fixture with a build.rs leak assertion, an option_env! check and a required runtime flag. The same fixture completes map/build/test and catches the intended edited function.

Milestone 2 preserves project policy and operational behavior. A real nextest campaign exercises all four runner policies, an inherited timeout override, private scratch cleanup and literal values. Existing schema diagnostics keep their recording-specific behavior. Complete local checks, committed review, exact-head CI and the verified peer handoff close this milestone.

Milestone 3 corrects completed-cache lifetime. Extend the real nextest regression with an observer test that stays alive until normal-exit, SIGTERM and TIMEOUT scratch is gone. Use tiny cache files and a bounded wait, not a large disk benchmark. Run it first against the original source and retain its failure evidence. Add an interruptible owner-side cleanup worker with a short polling interval, retaining records when deletion fails and joining it before Prepared teardown. Keep active test scratch untouched. Repeat all runner modes and existing build-env separation checks, then finish the normal reviewed push/CI/handoff.

## Concrete Steps

From repository root run targeted CLI tests through nice -n 10 mbx test --test campaign, then the full suite and Clippy, plus mbx fmt --check. Do not redirect targets or alter mbx scheduling/GC. Commit only owned files on the current branch. Request review of committed work before every push and address findings. Verify exact-head CI before handing off to session 5b9275ef422b8eb3146a4a11fb9dd1aa.

## Validation and Acceptance

The CLI map succeeds when the outer environment lacks the probe and test-env supplies it. Build scripts and compile-time option_env must show the value absent, while baseline, schema recording and mutant tests see it. An exact caught mutant proves the full path. Existing nextest profile timeout/failure kinds, skipped tests, custom wrappers and runners must remain in effect. Placeholder directories are distinct per subprocess. Completed normal, signaled and timed-out invocation directories must disappear while the observer test and nextest remain active, and the observer directory must stay available until that test ends. The cleanup worker must also join and perform a final sweep on owner teardown.

## Idempotence and Recovery

Use disposable committed repositories. Generated wrapper data lives under QS state and can be regenerated. Reserved generated script names must not overwrite user definitions. No test source rewriting, no Rust wrapper replacement, and no target layout changes. Failed baseline runs keep existing reports/diagnostics and clean owned scratch records.

## Interfaces and Dependencies

Use existing serde/toml/Command/Unix exec and sandbox helpers. Add shell-words for nextest-compatible command parsing instead of a shell or custom parser. The hidden helper accepts a specification file, wrapper key and trailing argv; it emits no protocol text into test output. Generated config enables nextest wrapper-scripts without removing existing experimental flags or profile rules.

Plan created 2026-11-03 after reading the campaign report and nextest's primary wrapper/environment documentation.

Plan reopened 2026-11-03 for the reported large-suite scratch lifetime issue: early cleanup now has a barrier-based acceptance test, preserving exec signal and timeout ownership. Sub-agent startup was attempted again and refused by session configuration, so parent owns this slice.

Follow-up implementation checkpoint 2026-11-03: the barrier fails before and passes after in all four runner policies; full checks pass. Cleanup now runs during nextest rather than only after it, and failed removals retain records for retry. Remote ENOSPC attribution remains qualified.

Follow-up completion 2026-11-03: source commit 2e35d9c passed required review and exact-head CI, and the verified install handoff was sent. Remote rerun readiness remains subject to the peer storage diagnostics; no remote retry, disk/layout, mbx policy or concurrency changes were made here. This completion note changes no executable source.

## Registry trust boundary and nonblocking resilience follow-up

Source review after the lifetime fix confirms a qualified cleanup contract: for a readable valid record of an owned scratch root whose test PID is gone, a failed remove_tree leaves the record available for retry. The registry is owner-controlled QS state, not an authenticated ownership registry or a defense against same-user state tampering. An absolute path with a qs-sandbox-* basename is a guardrail, not evidence that QS owns the named directory. Acceptance covers valid owned records and the tested normal, SIGTERM and TIMEOUT invocation paths.

The existing remove_stale implementation drops unreadable or malformed dead-PID record files, so transient read errors and damaged records do not have the removal-retry guarantee. Settings::make_root publishes with fs::write, which can leave a partial record if interrupted; it warns on a reported write failure. Publication occurs before the test exec, when the root is still new and empty. No claim is made that this publication race explains large completed-cache retention or total remote ENOSPC. Reaper::drop logs a worker panic but has no independent fallback final sweep; no ordinary panic was demonstrated.

Atomic record publication, distinct read-error handling, stronger owned-root validation and a panic fallback final sweep are nonblocking resilience follow-up ideas, not implemented behavior or new acceptance guarantees. This preserves the scope and verified executable head 2e35d9c. The peer source-review report is /tmp/qs21-root/tool-2e35-source-review.{json,md} on session 5b9275ef422b8eb3146a4a11fb9dd1aa; its counterexamples are source-only, not observations of unowned deletion or damaged registry state on the benchmark host.

Qualification recorded 2026-11-03 after checking make_root, remove_stale and Reaper::drop against the peer report. No executable change or new benchmark was needed; the accepted fix and remote disk-attribution limits remain unchanged.
