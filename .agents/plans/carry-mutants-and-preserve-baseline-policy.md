# Carry mutants across maps and preserve baseline test policy

This ExecPlan follows `.agents/PLANS.md` and is a living document. Maintain Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective as work proceeds.

## Purpose / Big Picture

A campaign must be able to map a new commit containing regression tests, rebuild old mutants without fuzzy patches, and compare results without matching human notes. Its baseline should honor the project's nextest timeout policy. Tests already known to fail without a mutant must be visibly excluded from coverage and kill selection, rather than silently lost or mistaken for a kill. This implements Quicksilver issues #36, #37 and #38 without changing Bifrost or running large benchmarks.

## Progress

- [x] (2026-11-01) Read issues and current code; verified origin/master is be52427 and working tree is clean.
- [x] (2026-11-01) Preserve nextest configuration/profile policy, including selected profile default filters; verified real nextest timeout overrides and custom store path.
- [x] (2026-11-01) Persist typed baseline exclusions, remove them before recording, and report coverage loss in map/test/survivors; strict extra-baseline mode tested.
- [x] (2026-11-01) Add function-anchored `build --from`, exact original validation, and cross-map stable IDs; both saved-state and test-report imports verified end to end.
- [x] (2026-11-01) Full test suites, Clippy and formatting pass; RUNNING.md and CI regression added.
- [ ] Push tested commits and send the exact installable head to the campaign session.

## Surprises & Discoveries

The generated nextest profile explicitly replaces repository configuration and terminates tests after 120 seconds twice. The baseline report already stores failure IDs, but the map discards that information and does not exclude those IDs from its recording list. Existing BodyEdit records already store full names and original bodies, so no fuzzy patch application is necessary for carry-forward. Existing runtime mutant IDs must remain compatible with saved state and verdict journals. Real end-to-end testing also exposed that profile filtering must be applied to `nextest list`, not only `run`, or deliberately omitted tests reappear in recording. The test catalog now passes the selected profile and respects nextest filter mismatches. JUnit uses an absolute path under the QS index so a project's custom store directory remains supported. Unignored JSON exports inside the edited checkout are intentionally rejected as added files; the tutorial now recommends external or ignored export paths.

## Decision Log

- Decision: Merge the project's complete nextest configuration, select its default profile or `[map] nextest-profile`, and override only fail-fast/JUnit reporting needed for baseline collection. Preserve timeouts, overrides and profile inheritance. Recording continues to use budgets derived from measured baseline durations.
  Rationale: A separate timeout policy would duplicate configuration. Direct schema recording already scales its budget from each baseline duration, and retries timeouts at a larger budget.
  Date/Author: 2026-11-01, Codex.
- Decision: Keep existing runtime IDs and add a separate stable_id derived from full function names and exact replacement bodies, independent of map sites, paths, line numbers and notes.
  Rationale: Existing state, activation and journals continue to work. A body change produces a distinct comparison identity; moving lines does not.
  Date/Author: 2026-11-01, Codex.
- Decision: `build --from FILE` accepts a saved edited state or `test --json` report, validates each whole mutant against the new map's full names, original paths and exact original bodies, reports invalid mutants individually, and imports accepted mutants as pending with new runtime IDs. It never copies old verdicts or uses fuzzy fallback.
  Rationale: This removes external patch rebasing and permits valid mutants to proceed while moved/deleted/drifted functions remain explicit omissions. Read all source files before changing state so importing the current state file is safe.
  Date/Author: 2026-11-01, Codex.

## Outcomes & Retrospective

Implemented all three issues. Full `mbx test --tests` passed: 187 library tests, one new campaign integration, three edited-workflow integrations, nine engine integrations and three schema integrations (203 passed, two pre-existing ignored). `mbx clippy --all-targets -- -D warnings`, `cargo fmt --check` and `git diff --check` passed. The tiny campaign proves a 50ms project timeout override is preserved, a separate slower test passes its longer override, a selected profile's default filter also controls recording, and a nonstandard nextest store directory still produces QS's isolated JUnit report. Both a failing and timed-out extra test are recorded as exclusions and never enter the map's tests/hits/not-green set. Strict mode rejects them.

The same integration captures a missed boundary mutant, saves its report, commits a new assertion with a source-line shift, remaps, imports, builds and kills the mutant while retaining stable_id. It also exercises saved-state deduplication, partial rejection with exit 2 and JSON diagnostics, survivor JSON and split review warnings. Unit tests cover missing/moved/drifted/ambiguous functions, invalid or overlapping edits, legacy serialization and detailed intent warnings. No benchmark host, engine change or large campaign was used. Push and handoff are the only remaining steps.

## Context and Orientation

`src/baseline.rs` builds/runs the original suite and writes index/baseline.json. `src/cargo_cmd.rs` writes the nextest configuration. `src/config.rs` reads quicksilver.toml into Config. `src/types.rs` defines BaselineReport. `src/exec/edited.rs` maps, captures/builds mutants and saves State, whose records contain a mutant and its compilation/verdict status. `src/sitemap.rs` defines SiteMap, written to index/sitemap.json. `src/capture.rs` defines BodyEdit and EditedMutant. `src/lib.rs` handles build/test JSON, while `src/survivors.rs` exports survivor reports. `src/cli.rs` defines flags. Tests use tiny temporary Git repositories under tests/fixtures and run Cargo through mbx. Do not change target directories or build caching.

## Plan of Work

First add project profile selection to MapFile/Config and create a merged nextest config from the baseline worktree's .config/nextest.toml. Keep the selected profile's timeout settings and per-test overrides unchanged; force fail-fast false and an isolated JUnit report path. Test configuration contents and execute a tiny fixture with profile overrides to prove the fixed QS timeout is absent.

Next add a serializable BaselineFailure with id and FAIL/TIMEOUT kind. Keep old failed_test_ids for compatibility and add defaulted typed fields to baseline and map data. JUnit failure/error types distinguish timeouts. Aggregate failures across repeated baseline passes, omit configured skips, enforce optional `[map] fail-on-extra-baseline`, and remove known failures before schema recording. Summaries name every excluded test and explain the coverage loss. Include typed exclusions in map JSON, test JSON and survivor output without changing the survivor JSON list shape unnecessarily.

Finally add a carry-forward module that reads either state records or test-report mutant rows. Resolve every edit against the current map by full name, require unchanged package/path and exact original bytes from the new map commit, validate replacement syntax/braces, and reject an entire mutant if any edit is missing, ambiguous, moved or drifted. Successful imports use the current sites and current capture ID hash. Add stable_id to test JSON, state exports and survivors without changing runtime IDs. Import occurs before State reconciliation can discard old records. Add an opt-in detailed intent warning flag so existing aggregate warnings remain concise, while a patch audit can name each valid-but-uncaptured key.

## Concrete Steps

Work in the existing checkout and commit directly on its branch. Use `mbx test --lib` for focused unit tests, `mbx test --test edited` for real map/build/test fixtures, `mbx clippy --all-targets -- -D warnings`, and `cargo fmt --check`. Commit tracked implementation changes before fixture commands that require a committed repository. Large benchmarks and cloud launches are outside this plan.

## Validation and Acceptance

Prove nextest config/profile preservation with a tiny local project whose default and named profile have different timeout overrides. Verify no hardcoded terminate-after=2 is inserted. Use a failing extra test fixture and a timeout JUnit sample to prove FAIL/TIMEOUT classification, persistence and exclusion from hits/selection; ensure strict extra-baseline mode rejects it. Exercise CLI JSON and survivor text for the coverage-loss summary. Capture a mutant, save its state/report, advance the fixture commit with unrelated functions or tests, re-map, carry it forward, build and judge it, and check that stable_id matches the old report while current runtime sites are used. Test original body drift, moved/deleted/ambiguous functions, malformed bodies and legacy imports missing originals. Old state without added fields must still load. Full focused suites and clippy must pass before push and handoff.

## Idempotence and Recovery

All imports are reads until validation finishes; new captures are deduplicated by runtime ID. Importing a saved file repeatedly must not duplicate records or resurrect stale verdicts. Keep the user's checkout unchanged. Write state atomically under StateLock. If compilation fails, ordinary unviable-mutant handling applies. Do not force push or change branches; merge concurrent origin/master updates if needed.

## Artifacts and Notes

Prior verification: `mbx test --lib baseline::tests::skipped_tests_do_not_make_the_baseline_red -- --exact` passes on be52427. #35's baseline rejection behavior is already fixed; physically skipping execution in that baseline remains separate from the issues in this batch.

## Interfaces and Dependencies

Use existing serde/serde_json, toml, sha2, syn and map/worktree helpers; do not add dependencies. New serde fields must default for compatibility. Add a dedicated carry-forward module to keep source import/validation separate from the schema compiler. Config grows nextest_profile and fail_on_extra_baseline, and baseline/map reports gain typed failure records. CLI build gains repeatable --from and optional --verbose-intents; --from suppresses implicit checkout capture and cannot silently combine with an explicit no-capture request.

Revision note: Initial plan created for the explicitly requested implementation/tested-head handoff.

Checkpoint note: All 186 active library tests pass (two ignored). The initial sandboxed run hit the existing socket-binding sandbox restriction; the unrestricted run passed. Integration and lint validation remain.

Validation artifacts: `/mnt/optane/tmp/qs-batch36-38-tests.log` and `/mnt/optane/tmp/qs-batch36-38-clippy.log`. The baseline FAIL/TIMEOUT output inside the successful campaign test is intentional test input. The first isolated campaign attempt placed a JSON export in the edited checkout and was correctly rejected; the fixture now ignores its output files.

Revision note: Completed implementation and tests, added selected-profile catalog filtering, documented import exit behavior and recorded complete validation before push.
