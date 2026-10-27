# Remove research features before the first release

This living ExecPlan follows `.agents/PLANS.md`.

## Purpose / Big Picture

QS's released help should show its supported workflows without research commands or developer tuning options. Mutation runs, edited-mutant workflows, and the hidden selection audit must retain their behavior. Work takes place only in `/mnt/optane/quicksilver/qs-wt-a` on its detached HEAD.

## Progress

- [x] (2026-10-27) Inspect command, configuration, executor, report, and baseline dependencies.
- [x] (2026-10-27) Remove estimate and higher-order features and hide developer options; all requested checks passed before commit.
- [ ] Remove Bifrost-specific environment handling and examples; verify and commit.

## Surprises & Discoveries

Coupling rows also carry selected test IDs used to annotate survivors tested only by proptest properties. Preserve those IDs separately. Preflight is used by run and must stay. Package rebuild timings are consumed by planning; preserve the cached data fields and consumer, removing only estimate-only measurements. Excluded src/exec/build.rs contains Bifrost test examples that must remain for the parent.

## Decision Log

- Decision: Preserve the proptest annotation with an internal map of mutant IDs to selected test IDs.
  Rationale: Removing research artifacts must preserve first-order behavior.
  Date/Author: 2026-10-27 / Codex.
- Decision: Use exactly two detached-HEAD commits, with no push or branch changes.
  Rationale: The parent will cherry-pick them; concurrent edits exclude Cargo.toml, .github/, scripts/, src/exec/build.rs, and README Install.
  Date/Author: 2026-10-27 / delegated instructions.

## Context and Orientation

`src/cli.rs` defines clap commands. `src/config.rs` merges CLI and file configuration. `src/lib.rs` dispatches commands. `src/engine.rs` prepares and reports runs; `src/exec/schema.rs` performs mutation testing. `src/report.rs` writes JSON. `src/baseline.rs` measures baseline tests and build behavior. Tests in `tests/engine.rs` and `tests/schema.rs` exercise those interfaces.

## Plan of Work

First remove `src/estimate.rs`, `src/higher_order.rs`, and `src/coupling.rs`, their commands, configuration fields, reports, and executor paths. Move deterministic audit sampling to `src/plan.rs`. Remove estimate-only baseline measurements after checking callers; preserve shared planning and preflight behavior. Hide the specified developer options in all clap argument definitions. Update first-order integration callers and README outside Install. The second milestone removes the Bifrost environment special case, warning example, comments, and test names.

## Concrete Steps

From the worktree root, use plain cargo through mbx without changing target directories. Before each commit run `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test --lib`, and separately `nice -n 10 cargo test --test edited`, `nice -n 10 cargo test --test engine`, and `nice -n 10 cargo test --test schema`. Save each command's output in `/mnt/optane/tmp/qs-publish-logs`. Run `cargo run -- quicksilver --help` and `cargo run -- quicksilver run --help` and inspect visible commands and options. Verify hidden flags still parse. Finish with `grep -rni bifrost src tests README.md`.

## Validation and Acceptance

All requested suites pass; clippy may show only the known warning in excluded `src/exec/build.rs`. Help excludes estimate, audit-selection, and hidden options while supported commands and visible common options remain. Hidden options parse. The final Bifrost grep has no unexplained matches. Commit messages explain removals as research and benchmarking features outside the released tool and include the requested coauthor trailer.

## Idempotence and Recovery

Only stage changed files explicitly. Do not touch excluded files or other worktrees. Rerun failed checks after repairs and keep failure logs. If tests are blocked by sandbox signals or sockets, rerun with normal permissions.

## Artifacts and Notes

Logs live in `/mnt/optane/tmp/qs-publish-logs`. Handback will list both commits and line removals.

## Interfaces and Dependencies

Keep the `run`, `list`, `map`, `build`, `test`, `uncovered`, `pending`, `survivors`, `apply`, `prove`, and `clean` commands. Keep audit-selection hidden. Simplify prepare and JSON output interfaces to remove research-only parameters and update all callers. No dependency or package manifest edits are authorized.

## Outcomes & Retrospective

First milestone complete. Unit tests: 175 passed, 2 ignored. Edited and schema suites: 3 passed each. Engine suite passed. Clippy reports only the known excluded build.rs warning. The sandbox blocked a socket unit test; rerunning with normal permissions passed. Help and hidden parsing are verified.

Plan created after inspecting dependencies to preserve first-order behavior during removal.

Milestone one updated after compilation repairs and passing verification.
