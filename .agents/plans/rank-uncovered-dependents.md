# Rank omitted dependent packages by listed function names

This living ExecPlan follows `.agents/PLANS.md`.

## Purpose / Big Picture

Make `uncovered` actionable in large workspaces: show up to five omitted dependent packages ordered by how many listed function names their committed Rust code contains. Show counts, the number of additional matches, and the number with no matches. On failure retain the existing unranked hint.

## Progress

- [x] (2026-10-25) Read issues #26 and #27 and inspected caller scanning and cargo metadata.
- [x] (2026-10-25) Extract the shared committed Rust blob reader while retaining caller early termination.
- [x] (2026-10-25) Rank omitted packages, format the hint, and document it.
- [x] (2026-10-25) Verify temporary-repository behavior, required checks, and clap timing.
- [x] (2026-10-25) Prepare the #27 commit with implementation, tests and this completed plan.

## Surprises & Discoveries

Tests require IPC access unavailable in the sandbox. The required checks pass when run with escalated permissions. Git worktree index metadata also requires that access.

## Decision Log

Use one ls-tree invocation followed by one cat-file batch process with a separate writer thread, as required by the assigned design. Keep package discovery and ranking failure-tolerant. Metadata manifest directories are relative to the configured workspace directory; prepend the map prefix to match full repository tree paths. Count each listed site separately, even when names repeat; repeated occurrences of one name do not increase a site's count.

## Outcomes & Retrospective

The ranked hint, shared reader, documentation and regression tests are complete. The required checks pass: 149 library tests, two integration tests, formatting, and clippy with only the known src/exec/build.rs warning. On clap commit 13f2db5072d600c11d8d6298e4e9ba53ffc6c1ab, ranking 2,488 distinct function names across eight workspace packages took 245.912272 ms at nice 10, including metadata loading. The complete timing test took 0.30 seconds. Logs are under `/mnt/optane/tmp/qs-26-27-logs/27-*.log`.

## Context and Orientation

`src/lib.rs::cmd_uncovered` chooses the listed sites and prints the hint. `src/sitemap.rs::omitted_test_dependents` finds omitted workspace packages. `scan_callers` already streams committed Rust blobs and recognizes words using Unicode alphanumeric characters and underscores. `src/sitemap/caller_tests.rs` builds temporary git repositories to test that scanner. README describes the commands.

## Plan of Work

Extract tree enumeration and batch reading into a private helper taking path selection and blob visitor closures. Preserve binary detection and caller early termination. Add a ranking function using metadata directories and sets of matched function names per package, converting matches to site counts. Add a hint formatter that limits output to five positive matches, reports zero matches, and quietly uses the original list on ranking failure. Change only the assigned command and scanner areas.

## Concrete Steps

Work in `/mnt/optane/quicksilver/qs-wt-a`. Run `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test --lib`, and `cargo test --test edited` before committing. Run an ignored read-only ranking evidence test with `QS_RANK_REPO=/mnt/optane/quicksilver/local/clap nice -n 10 cargo test --lib real_repository_ranking_evidence -- --ignored --nocapture`. Save text logs under `/mnt/optane/tmp/qs-26-27-logs`.

## Validation and Acceptance

Temporary repository tests must distinguish whole words from substrings, count duplicate names correctly, ignore main and trait implementations, honor map commit and nested prefix, and retain existing caller tests. Hint tests must cover the five-item limit, zeros, and fallback. Clap ranking should finish well under a minute. Clippy must emit no new warnings.

## Idempotence and Recovery

Tests use temporary directories; the real-repository harness only reads metadata and committed sources. Repeat checks freely. Do not redirect cargo targets, change branches, push, or modify other worktrees.

## Artifacts and Notes

The #26 commit is `c34271e`. Its required checks passed with 147 library tests and two edited integration tests. Logs for both issues use the issue number as filename prefix.

## Interfaces and Dependencies

Reuse cargo_metadata through `crate::cargo_cmd::load_metadata`, standard library collections and git subprocesses. Keep `omitted_test_dependents` compatible with its existing tests. No persisted map fields or versions change.

Updated 2026-10-25: implementation and verification completed; recorded clap timing and check counts. Git subprocess stderr is suppressed in the shared reader so failed ranking can fall back quietly.
