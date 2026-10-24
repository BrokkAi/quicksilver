# Configure partner tests and list remaining mutant work

This living ExecPlan follows `.agents/PLANS.md`.

## Purpose / Big Picture

Authors can configure partner test binaries for individual packages and ask which entered functions still need mutants. `uncovered` warns about omitted dependent packages without building them. `pending` can divide work into files balanced by source lines.

## Progress

- [x] (2026-10-24) Read issues #18 and #20 and inspected command, configuration, map and mutant state code.
- [x] (2026-10-24) Implemented and verified #18; ready to commit.
- [x] (2026-10-24) Implemented pending, inventory matching, balanced splitting and end-to-end checks; all required checks pass.

## Surprises & Discoveries

The first pending end-to-end test wrote reports inside its capture fixture; later captures correctly rejected those added files. Moving reporting output to a separate temporary directory fixed the test. GitHub access needs network escalation. The existing reverse_dependents helper includes transitive normal and development dependents and excludes build dependencies, matching the decided hint design.

## Decision Log

The delegated design is authoritative: package-specific extras follow CLI and global extras; the hint makes no coverage claims. Pending matches records by site id, path and full function name, ignores unviable records and does not compare map ids. Keep command logic in a new pending module to minimize shared-file edits. Log checks under /tmp/qs-18-20-logs because the assigned worktree boundary excludes the prescribed report directory in the original checkout.

## Outcomes & Retrospective

Issue #18 passes fmt, clippy (known build.rs warning only), library tests (138 passed, 1 ignored) and edited tests (2 passed). Sandbox process signal handling aborts library tests; the unsandboxed rerun passes. Issue #20 passes fmt, clippy (same known warning), library tests (139 passed, 1 ignored), and edited tests (2 passed). Both requested outcomes are complete.

## Context and Orientation

`src/config.rs` reads strict TOML configuration and merges CLI options. `src/cargo_cmd.rs` loads cargo metadata and walks workspace dependents. `src/lib.rs` dispatches commands and implements uncovered, whose map loading and filters pending will follow. `src/sitemap.rs` defines sites (mapped functions) and test hits. `src/exec/edited.rs` exposes captured mutant State; do not edit it. `src/cli.rs` holds clap arguments. `tests/edited.rs` exercises real captures on the tiny fixture.

## Plan of Work

First add a strict package table type to config, merge its selected extras, and test precedence, deduplication and no-package behavior. Add a sitemap helper that identifies omitted dependents using cargo metadata, call it from uncovered, test using tiny metadata, and document both changes. Then add PendingArgs and a small dispatch arm. A new pending module loads the map and state, filters entered non-scaffolding functions, counts records with matching edits, prints rows and summary and writes JSON or balanced text files. Test matching and line-balanced splitting, plus the captured-mutant workflow in edited.rs. Document the new command.

## Concrete Steps

From /mnt/optane/quicksilver/qs-wt-a run plain cargo (the mbx shim), without target directory overrides. Before each issue commit run `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test --lib` and `cargo test --test edited`. Save output in /tmp/qs-18-20-logs. Commit only assigned changes on detached HEAD with issue-specific messages and the requested co-author trailer. Do not push.

## Validation and Acceptance

The merge test must preserve CLI then global then selected-package ordering and reject unknown table keys. The hint test must identify tiny_b and tiny_c as omitted dependents of tiny_a, with no hint for an unrestricted map. Pending must omit captured sites, include entered uncaptured sites, reject mismatched identities and unviable records, support filters and JSON, and distribute descending-size functions to the least-loaded bucket. Required checks pass with only the known build.rs warning allowed.

## Idempotence and Recovery

Read-only reporting does not build or mutate state. Split files overwrite their own numbered paths. Tests use temporary fixture checkouts. Re-run checks after fixes and stage only explicit paths.

## Artifacts and Notes

Evidence logs: /tmp/qs-18-20-logs. Issue details obtained with gh issue view 18 and 20 in BrokkAi/quicksilver.

## Interfaces and Dependencies

Use existing cargo metadata dependency and reverse_dependents, serde JSON output and atomic writing. Keep FileConfig and PackageConfig strict with deny_unknown_fields. Pending reads State::load without modifying state or its persisted contract.

Revision (2026-10-24): Recorded completed validation and the fixture-output fix. Pending follows the decided design with no persisted-state changes.
