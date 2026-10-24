# Fix the brokk-bifrost-rust field-report batch (BrokkAi/quicksilver#16 to #22)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md` at the repository root.


## Purpose / Big Picture


The agent testing cargo-quicksilver (QS) on Bifrost filed seven issues from the brokk-bifrost-rust round (1,098 agent-written mutants, QS at 1a9a6f9). After this plan:

- A stopped `test` keeps every verdict it decided, and the next `test` continues (#16).
- Mapping the same commit again keeps the captured mutants (#17).
- `[package.<name>] extra-tests` adds a package's partner test binaries; `uncovered` names dependents whose tests were not recorded (#18).
- `--patch PATCH=INTENTS` binds notes to one patch (#19).
- `cargo quicksilver pending` lists entered functions without mutants and splits them for parallel authors (#20).
- `build` flags sources whose mutants mostly change one token (#21).
- The build summary separates new mutants from the generation's total (#22).


## Progress


- [x] (2026-10-24) Read the seven issues and the state, map and capture code.
- [x] (2026-10-24) #16 and #17, in this session (e9fa10e; one commit, since both change `State`).
- [x] (2026-10-24) #18 and #20, by a child agent in a detached worktree, reviewed and cherry-picked (6d4e883, fb0e1fa). Its plan: `.agents/plans/package-tests-and-pending.md`.
- [x] (2026-10-24) #19, #21 and #22, by a second child agent, reviewed and cherry-picked (48e62fa, 78245d6, 679a1af). Its plan: `.agents/plans/capture-issues-19-21-22.md`.
- [x] (2026-10-24) Follow-up: a run's sandbox directory recreated after the end is removed (5cd9056).


## Surprises & Discoveries


- Site ids are a deterministic function of the code (`schema::default_site_hash` over the site seed, with collisions resolved in site order), so a new map of the same commit and packages has the same ids, and compiled generations stay valid. Staged test binaries are copies (`reflink_or_copy`), so a re-map cannot change them.

- Each run of `tests/edited.rs` left one empty `qs-sandbox-*` directory in the temporary directory: the sandbox settings are global to the test process, and the second test in that binary started test processes after the first called `sandbox::summary`. Fixed in 5cd9056.


## Decision Log


- Decision (#16): append each verdict to `state/verdicts.jsonl` as it is decided; `State::load` applies entries of the same map and generation to `Built` records (ignoring a partial last line), and `State::save` folds them in and removes the journal.
  Rationale: rewriting the whole state per verdict would rewrite megabytes per mutant on large runs; an append-only journal survives SIGKILL.
  Date/Author: 2026-10-24, Claude.

- Decision (#16): Ctrl-C, SIGTERM and SIGHUP stop `test` after the mutants already started (the ctrlc crate's `termination` feature); a second signal exits at once. `test --json` holds every verdict of the map, not only the run's.
  Rationale: test processes run in their own process groups, so a signal to QS does not stop them; finishing them takes at most their budgets. A JSON of all verdicts means a continued run needs no merging.
  Date/Author: 2026-10-24, Claude.

- Decision (#17): the state records its map's commit and test packages. A new map of another commit drops every record (as before). A new map of the same commit keeps records whose sites it has; with the same test packages, builds and verdicts stay; with other test packages, kills and unviable mutants stay and the rest return to pending, because the old generation lacks the new test binaries. `test` applies the same rules instead of refusing to run.
  Rationale: the issue's cost was recapturing 1,098 mutants; compiling again is unavoidable when the test binaries change, because the extra test crates depend on the mutated crate.
  Date/Author: 2026-10-24, Claude.

- Decision: #18 and #20, and #19, #21 and #22, went to two child agents, each in its own detached worktree, with the designs settled in their briefs (per-package table merged after CLI and top-level lists; `pending` counts non-unviable records by site id, path and name; `PATCH=INTENTS` split at the last `=`, per-patch notes win over global ones, a duplicate's note only fills a missing one; one-token share printed from 10 mutants, warning at 80%).
  Rationale: they touched separate parts of the code from #16 and #17; separate worktrees kept builds from interfering.
  Date/Author: 2026-10-24, Claude.


## Outcomes & Retrospective


All seven issues are closed. The unit tests (146), `tests/edited.rs`, `tests/engine.rs` and `tests/schema.rs` pass on the combined code. The end-to-end test re-maps the same commit and checks that no mutant needs capturing, building or testing again.

Checked by hand on a copy of the tiny fixture: with two mutants that each make a test sleep four seconds and `--test-jobs 1`, SIGTERM after two seconds printed the stopping notice, finished the started mutant, wrote its `--json`, and saved its verdict; the next `test` ran only the other mutant, and its `--json` held both.

Not done: a state saved before e9fa10e has no commit recorded, so the first re-map after upgrading still drops it once.


## Context and Orientation


Key code: `State`, `State::reconcile`, `Reconciled`, `JournalEntry` and `test()` in `src/exec/edited.rs`; `engine::install_ctrlc`; `cmd_test` in `src/lib.rs`; `src/pending.rs`; `PatchSource` and `record_capture` in `src/exec/edited.rs`; `PackageConfig` in `src/config.rs`.


## Plan of Work, Concrete Steps, Validation


Done as described in `Progress`. Validation: `cargo test --lib`, `cargo test --test edited --test engine --test schema`, `cargo clippy --all-targets`, and the manual SIGTERM check above.


## Idempotence and Recovery


Plain code changes; each commit can be reverted on its own, except that #16 and #17 share one commit.


## Artifacts and Notes


Commits: e9fa10e (#16, #17), 6d4e883 (#18), fb0e1fa (#20), 48e62fa (#19), 78245d6 (#21), 679a1af (#22), 5cd9056 (sandbox directory cleanup).


## Interfaces and Dependencies


The ctrlc dependency gains its `termination` feature. No new crates.
