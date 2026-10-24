# Stream committed Rust sources for the caller check

This living ExecPlan follows `.agents/PLANS.md`.

## Purpose / Big Picture

Make `uncovered` finish on large repositories without collecting matching source lines from git grep. Preserve its candidate selection and definition/test-line rules, and inspect the map commit even when the working tree differs.

## Progress

- [x] Read issue #11 and the current implementation at a705e6d.
- [x] Implement one tree listing and one concurrent batch reader/writer, with cleanup on early termination and errors.
- [x] Add ten committed temporary-repository semantic tests and an ignored real-repository evidence harness; clap equivalence passes.
- [x] Formatting and clippy pass with only the known build.rs warning; 132 library tests pass.
- [x] Both edited integration tests pass on the final code (25.50 seconds).
- [x] (2026-10-24 04:38 UTC) Old Rust run timed out after 300 seconds total; it spent about 228 seconds in the caller check after preparing candidates. Text evidence is in the assigned report directory.
- [x] Implementation and all validation are ready for the single requested commit.

## Surprises & Discoveries

The initial GitHub request required network escalation; the second request read the issue successfully. The repository already has tempfile as a development dependency. The restricted sandbox denies wait-timeout signal-handler pipe writes, causing unrelated tests to abort; unrestricted reruns pass. A new clippy byte-character-slice suggestion in the scanner was fixed.

Rust evidence uses commit a1e52fc1cf67929a7c01ed9c037520e276ec98fd. Clap uses commit 13f2db5072d600c11d8d6298e4e9ba53ffc6c1ab. Neither checkout was changed. All large evidence runs use nice level 10. No NFS recovery or mount changes were made.

## Decision Log

Use the supplied design: `git ls-tree -r -z --full-tree` supplies regular Rust blobs, and one `git cat-file --batch` process supplies committed bytes. A separate writer thread prevents pipe deadlock. Retain the former implementation only in tests for equivalence evidence. Stop tracking each candidate on its first disallowed occurrence, and terminate the child when all names resolve. Do not change public interfaces or persisted map data.

## Outcomes & Retrospective

The streaming implementation preserves the public interface and all focused semantic checks. Clap returned exactly the same 501 site IDs with both implementations for 1100 candidates: about 0.7 seconds new versus 47 seconds old. The new Rust scan read 37241 blobs in about 66 seconds at nice level 10; selecting synthetic candidates took about 45 additional seconds outside that timer. The old Rust run timed out after five minutes total (about 72 seconds preparing candidates, then about 228 seconds in the old caller check). The final edited suite passes both tests in 25.50 seconds.

## Context and Orientation

`src/sitemap.rs::without_callers` currently captures all git grep matches and stores every occurrence before classifying sites. `SiteMap` holds the commit, workspace prefix, sites and test-line ranges. `tests/edited.rs` exercises uncovered output. Candidate words are maximal Unicode alphanumeric or underscore runs in lossy UTF-8. Only non-trait-implementation sites whose final dotted name is not main are candidates. An occurrence is allowed only in the candidate's own file and either its definition or that file's test ranges. Shared names must be checked against every candidate independently.

## Plan of Work

Replace the production function in `src/sitemap.rs` with a small wrapper and internal scan helper that also reports files scanned for early-stop tests. Parse NUL-terminated tree entries, retaining modes 100644 and 100755 with paths ending in .rs. Read each batch response by its declared byte length, skipping blobs containing NUL in their first 8000 bytes. Scan each source line once against pending names. Strip the workspace prefix before comparing paths. Ensure normal completion and all error/early-stop paths close pipes, stop and reap the child, and join the writer.

Add temporary git repository tests with hand-built maps, including definition uses, tests, cross-file uses, outside-prefix uses, shared names, whole words, binary sources and early termination. Add inexpensive ignored evidence tests to compare against the old implementation on clap and measure approximately 1100 names on rust without building or changing either repository.

## Concrete Steps

From `/mnt/optane/quicksilver/qs-callers-wt`, run plain cargo (the mbx shim) for all builds. Use `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test --lib`, and `cargo test --test edited`. Run the ignored evidence tests with repository and implementation environment variables. Bound the old large-repository run with `timeout 300`. Store text evidence under the assigned report directory, never build output there.

## Validation and Acceptance

All temporary-repository tests must pass, and clap must yield identical site-id sets with the old and new functions. Report approximate wall time for the new rust scan and whether the old scan finishes within five minutes. Accept only unchanged uncovered behavior and no new clippy warnings in edited files; the known src/exec/build.rs warning is baseline.

## Idempotence and Recovery

Temporary fixtures own and remove their repositories. Evidence runs read real repositories only. Child processes are reaped on failure. No checkout, branch, target-directory or mount changes are needed. Commit only this task's files on the detached HEAD and do not push.

## Artifacts and Notes

The parent receives the final commit hash, changed files, timings, equivalence result and verification results. Long logs stay in the assigned report directory.

## Interfaces and Dependencies

Keep `pub fn without_callers(map: &SiteMap, repo_dir: &Path, sites: &[&MapSite]) -> Result<HashSet<u32>>`. Use std process, I/O, collections and threading, plus existing anyhow and tempfile. Do not add flags or dependencies.

Revision note: implementation and semantic fixtures are complete. Evidence and all required checks are complete; the old Rust scan timed out. The only warning is the known build.rs warning, and there are no unresolved implementation decisions.
