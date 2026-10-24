# Improve capture notes and build feedback

This living ExecPlan follows `.agents/PLANS.md`.

## Purpose / Big Picture

Authors can attach separate notes to two patches that edit the same function, see when a source mostly changes one token, and distinguish newly compiled mutants from the generation total.

## Progress

- [x] (2026-10-24) Read issues #19, #21 and #22 and inspect capture flow.
- [x] (2026-10-24) Implemented #19; formatting, clippy and both test suites pass.
- [ ] Implement and verify #21, then commit.
- [ ] Implement and verify #22, then commit.

## Context and Orientation

`src/cli.rs` defines build arguments; `src/lib.rs::cmd_build` constructs capture sources and prints results. `src/exec/edited.rs::build` captures each patch independently, attaches notes and records unique mutants before compiling. `src/capture.rs::attach_intents` matches full function names or locations and reports ambiguous or unknown keys. `tests/edited.rs` exercises capture and build with a small fixture.

## Plan of Work

For #19 add an explicit patch specification parsed at the last equals sign. Keep global matching across sources, then apply each patch's notes only within its capture, counting overrides once. Merge notes on duplicate records without replacing an existing different note, and report the source and mutant id. Add parsing, scoped matching and integration coverage; update the README.

For #21 flatten proc_macro2 token streams recursively including group delimiters. Count equal-length sequences differing once across all edits. Print source counts at ten mutants and warn at eighty percent, using named constants and unit tests.

For #22 count captured mutants that have Built results in this generation, and separately count all Built results in this generation. Keep unviable reporting.

## Milestones

Each issue ends with a separate detached-HEAD commit. Before each commit run the four validation commands below. No branches, pushes, changes to cargo target paths, or edits to other agents' owned functions are permitted.

## Concrete Steps

From `/mnt/optane/quicksilver/qs-wt-b`, edit the named capture code and run `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test --lib`, and `cargo test --test edited`. Use plain cargo through the configured mbx shim. Save textual validation logs for handback.

## Validation and Acceptance

Parsing tests prove the final equals sign separates patch and note paths. Scoped tests and the edited fixture prove sibling mutants receive their own notes and duplicate notes merge predictably. Token tests cover operators, literals, identifiers, multiple changes, added tokens and invalid token streams. All existing library and edited integration tests must pass; the pre-existing clippy warning in src/exec/build.rs is allowed.

## Idempotence and Recovery

Tests create disposable fixture repositories. Review each diff and stage only the files changed for that issue. The parent cherry-picks the three commits.

## Interfaces and Dependencies

Use existing serde_json intent files and proc_macro2 token streams; no dependencies or persisted schemas change. Patch specifications carry an explicit optional intents path. State, reconciliation, load_for_build, test(), cmd_test, cmd_uncovered and src/config.rs remain outside this assignment.

## Surprises & Discoveries

GitHub reads require network escalation in this sandbox; all three issues were successfully read with gh.

## Decision Log

The assigned design takes precedence over issue #19's suggestion of automatic sidecar pickup: only explicit equals syntax loads patch notes. Global notes apply first, followed by scoped patch notes.

## Outcomes & Retrospective

Issue #19 now supports explicit patch notes and duplicate note merging. Library tests: 139 passed, one ignored. Edited tests: two passed. Only the known clippy warning remains.

## Artifacts and Notes

Sandboxed process tests initially aborted in wait-timeout due to denied signaling; rerunning with permitted host operations passed. Validation logs will be listed in the handback report. Plan created before implementation to track the three independent commits.

Updated after #19 validation to record note behavior and the sandbox limitation.
