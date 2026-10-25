# Fix the brokk-bifrost-cpp field-report batch (BrokkAi/quicksilver#23 to #28)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md` at the repository root.


## Purpose / Big Picture


The agent testing cargo-quicksilver (QS) on Bifrost filed six issues from the brokk-bifrost-cpp round (1,477 agent-written mutants, QS at 679a1af). After this plan:

- Tests that exist only under a cfg (Bifrost's scheduled-only tests) can be mapped, and a re-map with other build settings recompiles mutants while keeping kills (#28).
- Named tests, such as timing guards that fail under the switch, can be left out of the map (#23).
- A long `test` reports counts and the time left every five minutes (#24).
- The README states that each mutant's tests run cheapest first (#25).
- `pending` prints each function's end line (#26).
- The `uncovered` dependents hint is ranked (#27).


## Progress


- [x] (2026-10-25) Read the six issues and the build, listing, selection and progress code.
- [x] (2026-10-25) #23, #24, #25 and #28 in this session (cf741a2, one commit: they share the map and state changes).
- [x] (2026-10-25) #26 and #27 by a child agent in a detached worktree, reviewed and cherry-picked (abb7040, d522893). Its plan: `.agents/plans/rank-uncovered-dependents.md`.
- [x] (2026-10-25) Review fix for #27: each file counts for the deepest package that holds it (25053ba).


## Surprises & Discoveries


- #28 needed no new build option: QS sets no `RUSTFLAGS`, and every cargo call it makes (`baseline.rs`, `index.rs`, `cargo_cmd.rs` build functions) goes through `apply_build_env`, so `[build-env] RUSTFLAGS = "--cfg ..."` already reaches the baseline, the listing and the instrumented builds. What was missing was recording it in the map.

- #25 was already true: `SiteMap::tests_entering` sorts a mutant's tests by recorded duration, and `resolve_tests` and `run_selected_tests_observed` keep that order.

- The first #27 ranking counted a file for every omitted package whose directory held it, so a package at the workspace root (Bifrost's root suites, likely) counted the mapped package's own sources and always came first.


## Decision Log


- Decision (#28): document `[build-env] RUSTFLAGS` instead of adding `[build] rustflags`; record `[build-env]` in the map (`SiteMap.build_env`) and the state, and treat a change like a change of test packages in `State::reconcile`; warn in `build` when `[build-env]` differs from the map's. No `run-ignored` option.
  Rationale: a second way to set flags would duplicate `[build-env]`. Passing `--config build.rustflags` instead would interact badly with `target.<triple>.rustflags` and would not match Bifrost's own release gate, which uses `RUSTFLAGS`. With the cfg set, `cfg_attr(not(scheduled_tests), ignore)` tests are not ignored, so `run-ignored` would be speculative.
  Date/Author: 2026-10-25, Claude.

- Decision (#23): `[map] skip-tests` takes globs over test ids; `index::drop_skipped` removes them right after listing, in `build_map_from` and `engine::prepare`, so neither the gate nor the recording runs them; the map keeps their ids in `skipped_tests`. The baseline nextest run still runs them.
  Date/Author: 2026-10-25, Claude.

- Decision (#24): a status line every five minutes through `Progress::println`, with an estimate of elapsed × (cost left ÷ cost decided), where a mutant's cost is its selected tests' recorded durations plus 0.02 s each.
  Rationale: per-mutant counts mislead when a few survivors with long selections remain; recorded durations are what a survivor runs.
  Date/Author: 2026-10-25, Claude.


## Outcomes & Retrospective


All six issues are closed. The unit tests (152), `tests/edited.rs`, `tests/engine.rs` and `tests/schema.rs` pass. The end-to-end test maps a fixture test that exists only under `--cfg qs_fixture_cfg` (set through `[build-env]`), and skips a fixture test that fails only under QS by a glob. Unit tests cover the status line and estimate, the skip globs, the re-map rule for build settings and the ranking with a root package.


## Context and Orientation


Key code: `SiteMap.build_env` and `skipped_tests`, `State::reconcile` and `Tally` in `src/exec/edited.rs`; `index::skip_globs` and `drop_skipped`; `MapFile` in `src/config.rs`; `rank_test_dependents` in `src/sitemap.rs`.


## Plan of Work, Concrete Steps, Validation


Done as described in `Progress`. Validation: `cargo test --lib`, `cargo test --test edited --test engine --test schema`, `cargo clippy --all-targets`.


## Idempotence and Recovery


Plain code changes; each commit can be reverted, except that #23, #24, #25 and #28 share one.


## Artifacts and Notes


Commits: cf741a2 (#23, #24, #25, #28), abb7040 (#26), d522893 and 25053ba (#27).


## Interfaces and Dependencies


No new crates.
