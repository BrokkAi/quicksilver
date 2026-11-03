# Apply test-only environment to the nextest baseline

This living ExecPlan follows `.agents/PLANS.md`.

## Purpose / Big Picture

A baseline currently gets only build-env, while schema and mutant tests get test-env. The campaign reproduced a real test expecting BIFROST_QS_ENV_PROBE=configured that fails in baseline despite its quicksilver.toml setting. Tests must receive the same configured test-only values without passing them to Cargo/rustc. Keep selected nextest profiles, overrides, retries, budgets, wrappers and target runners working. A sandbox placeholder must stay private to a subprocess; baseline HOME/TMP remain in the caller environment unless explicitly overridden.

## Progress

- [x] (2026-11-03) Inspect source and report: run_green_suite applies only build_env; Settings.prepare applies test_env only to QS direct test runs. Sub-agents remain disabled for this session, so parent owns implementation/checks.
- [x] (2026-11-03) Reproduce with a committed real CLI fixture: control fails red baseline with NotPresent; fixed CLI maps/builds/tests a caught mutant while build.rs and option_env! reject compiler leaks.
- [x] (2026-11-03) Implement environment-only helper, private specification and nextest overlays for listing/baseline; retain original project wrappers, all four runner policies and inherited profile rules.
- [x] (2026-11-03) Validate real nextest runner policies, special-character values, explicit env precedence, preserved HOME, inherited TIMEOUT and scratch cleanup including timeout. Plain test-env also passes with --no-sandbox.
- [x] (2026-11-03) Format and Clippy clean; 226 tests passed across all suites, two existing unit ignores. Adapt the old schema fixture to distinguish recording from baseline explicitly, then rerun its failure and finish the previously unrun suites.
- [ ] Commit, obtain clean review, push, verify CI and send exact install handoff.

## Surprises & Discoveries

Nextest supports list/run wrapper scripts since 0.9.98, enabled with experimental wrapper-scripts in the generated configuration. A wrapper receives the target runner and test argv according to its target-runner policy. Only the first matching wrapper is used, so a blanket new wrapper rule would overwrite user wrappers. Existing wrapper commands must instead be composed with QS's environment helper, while a lowest-priority fallback handles tests without a project wrapper. Nextest binaries-only metadata provides paths without executing test processes.

The first full check found that the existing schema fixture treated FIXTURE_STATE and FIXTURE_NOT_GREEN as schema-only flags, relying on the very omission being fixed. It now checks QS_RECORD_SITES/QS_MUTANT for direct-run-only assertions and deliberately fails its diagnostic test only during recording. Baseline scratch freshness is still checked, without writing to real HOME.

Evidence logs: /tmp/qs-test-env-control.log (red baseline before fix); /tmp/qs-test-env-fixed.log (initial real CLI pass); /tmp/qs-test-env-policy.log (runner policies); /tmp/qs-test-env-checks.log (Clippy, units, campaign and original fixture failure); /tmp/qs-test-env-edited-retry.log (fixed failing case); /tmp/qs-test-env-remaining.log (engine/schema). The initial check passed 201 unit and 10 campaign tests; the edited suite passed its other two tests and its failing case passed on rerun; engine nine and schema three then passed. Two unit tests retain their existing ignored status.

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

## Outcomes & Retrospective

The implementation and local acceptance checks are complete. Baseline and listing tests receive configured test-only values while Cargo/rustc do not. Existing nextest wrappers/runners and timeout policy remain effective; placeholder storage is private and removed after the nextest run. No Bifrost source/config workaround, target layout, mbx policy or engine change is needed. Committed review, push, CI and install handoff remain.

## Context and Orientation

`src/baseline.rs::run_green_suite` launches mbx nextest run and parses its JUnit failures and timings. `src/cargo_cmd.rs::write_nextest_config` merges project policy, changing fail-fast and JUnit output only. `src/index.rs::list_tests_plain` builds/lists tests through mbx. `src/sandbox.rs::Settings` applies configured test values and per-process sandbox placeholders for QS direct runs. Add `src/baseline_env.rs` to generate the wrapper overlay and dispatch its hidden helper from `src/lib.rs` before leaving the inherited mbx test session. Keep original project environment and runner behavior apart from explicit test-env overrides.

## Plan of Work

First add a real temporary package with build.rs checking test-only variables are absent, compile-time option_env assertions, and tests requiring configured values while the outer process lacks them. Confirm failure on current code. Extend Settings with an environment-only preparation path that preserves baseline variables and reuses placeholder allocation/cleanup.

Write a private serialized wrapper specification alongside generated nextest config. Obtain actual test executable paths with nextest list --list-type binaries-only under build-env. Compose existing wrapper command forms (string, argv and command table), relative-to, command env and target-runner policy; use shell-words parsing consistent with nextest. Preserve profile script rules and append the fallback after project default rules. Helper exec passes original argv and status/signal behavior unchanged. Use the generated overlay for full list operations as well as baseline test execution, with cleanup on success and error.

Exercise map/build/test and no-sandbox plain values, special-character values, per-test scratch isolation, project wrappers/runners and selected timeout overrides. Update RUNNING.md with the test/build environment boundary and conditional nextest minimum.

## Milestones

Milestone 1 reproduces the mismatch at the real CLI boundary and supplies test-env through the nextest wrapper helper. It is verified by a fixture with a build.rs leak assertion, an option_env! check and a required runtime flag. The same fixture completes map/build/test and catches the intended edited function.

Milestone 2 preserves project policy and operational behavior. A real nextest campaign exercises all four runner policies, an inherited timeout override, private scratch cleanup and literal values. Existing schema diagnostics keep their recording-specific behavior. Complete local checks, committed review, exact-head CI and the verified peer handoff close this milestone.

## Concrete Steps

From repository root run targeted CLI tests through nice -n 10 mbx test --test campaign, then the full suite and Clippy, plus mbx fmt --check. Do not redirect targets or alter mbx scheduling/GC. Commit only owned files on the current branch. Request review of committed work before every push and address findings. Verify exact-head CI before handing off to session 5b9275ef422b8eb3146a4a11fb9dd1aa.

## Validation and Acceptance

The CLI map succeeds when the outer environment lacks the probe and test-env supplies it. Build scripts and compile-time option_env must show the value absent, while baseline, schema recording and mutant tests see it. An exact caught mutant proves the full path. Existing nextest profile timeout/failure kinds, skipped tests, custom wrappers and runners must remain in effect. Placeholder directories are distinct per subprocess and are cleaned even after a timed-out baseline test.

## Idempotence and Recovery

Use disposable committed repositories. Generated wrapper data lives under QS state and can be regenerated. Reserved generated script names must not overwrite user definitions. No test source rewriting, no Rust wrapper replacement, and no target layout changes. Failed baseline runs keep existing reports/diagnostics and clean owned scratch records.

## Interfaces and Dependencies

Use existing serde/toml/Command/Unix exec and sandbox helpers. Add shell-words for nextest-compatible command parsing instead of a shell or custom parser. The hidden helper accepts a specification file, wrapper key and trailing argv; it emits no protocol text into test output. Generated config enables nextest wrapper-scripts without removing existing experimental flags or profile rules.

Plan created 2026-11-03 after reading the campaign report and nextest's primary wrapper/environment documentation.
