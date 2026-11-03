# Follow tracked production modules beyond nested Cargo roots

This living ExecPlan follows `.agents/PLANS.md`.

## Purpose / Big Picture

Issue #43 reports a standalone nested Cargo workspace whose binary includes three tracked files under the Git repository's tests directory through explicit path modules. They compile as production code, but discovery drops them because of their directory and package boundaries. Users must be able to map these functions, capture exact shared-file edits, build/rebuild them and catch faults without copying engine files or changing Cargo/mbx settings.

## Progress

- [x] (2026-11-03) Inspect issue and source/module, path, capture and replay boundaries. Complete #42 verified handoff first (fbf805f, CI 41697489540).
- [x] (2026-11-03) Implement production graph discovery, tracked/shared boundaries, nested path conversion and map format v9 (reads v8 and default-only v7).
- [x] (2026-11-03) Real nested binary regression passed map hits, caught shared-body fault, implicit capture, repository-root patch/intents, rebuild and fresh-state replay. Also passed diff selection, test-only edit refusal, untracked/out-of-Git exclusion and traversal refusal.
- [x] (2026-11-03) Final role checks: 197 library tests passed/two existing ignores; all six campaign tests and Clippy/fmt passed.
- [ ] Finish full-suite run and CI verification.
- [ ] Run local checks, commit/push, verify CI and hand off exact executable head.

## Surprises & Discoveries

The syn walker already resolves explicit path modules beyond a crate directory. `src/discover.rs` discards them afterward via workspace strip_prefix, test-like-path and package scope checks. Capture also assumes every patch path starts with the workspace prefix. Original source reads concatenate prefix and workspace paths without reducing parent components. A role-blind visited-file set can let an earlier cfg(test) alias suppress the same file's production alias. The walker now distinguishes these roles; discovery prefers the production instance for line classification so a test alias does not label every shared source line test-only. The real fixture declares the test alias first to exercise this boundary.

## Decision Log

- Decision: Keep public source paths relative to the Cargo workspace, using parent components for shared files, and convert explicitly to normalized repository paths at Git/patch boundaries.
  Rationale: Existing local paths/IDs stay unchanged, while every physical source remains anchored inside the same Git repository. Repository-relative patch headers remain normal Git diffs.
  Date/Author: 2026-11-03 Codex.
- Decision: Only source files reached from the selected production root count; shared files outside the workspace must be tracked in that Git repository. Module context determines test-only exclusion; directory names do not.
  Rationale: A reused file under tests can be production code, while arbitrary external dependencies and untracked external files must not become mutation inputs.
  Date/Author: 2026-11-03 Codex.

- Decision: Write map version 9 for parent-relative shared-source semantics; accept feature-aware v8 and default-only v7 maps.
  Rationale: Older binaries cannot interpret all shared-source capture paths safely and must refuse the new format. Previously mapped runtime and feature campaigns remain readable by the new tool.
  Date/Author: 2026-11-03 Codex.

## Outcomes & Retrospective

The reported standalone nested graph now works end to end in a real CLI fixture. Shared source under tests is production when imported that way, while pure cfg(test) source stays excluded. Same-map rebuild and fresh-state body replay preserve ID, body and notes; user source remains unchanged. Final full-suite and CI verification remain. No engine manifests/files, Cargo defaults, profiles, targets, mbx layout or scheduling will change.

## Context and Orientation

`src/scan.rs` follows Rust modules and marks functions in cfg(test) contexts. `src/discover.rs` turns its results into function declarations with workspace-relative paths. `src/worktree.rs` reads original Git blobs; it needs a checked normalization helper for repository paths and an inverse conversion for patch paths. `src/sitemap.rs` saves source hashes and coverage. `src/capture.rs` compares checkout/patch contents against map source, while `src/carry.rs`, `src/apply.rs` and `src/survivors.rs` replay/export exact bodies. `src/exec/edited.rs` instruments sources in a full Git worktree whose workspace lies below a prefix. Parent-relative source paths must always remain within that worktree.

## Plan of Work

Add checked repository-path normalization and workspace-relative conversion to worktree helpers. Reject absolute/escaping patch paths before materializing scratch files. Make discovery retain reached shared source files under the Git top level, requiring tracked membership outside the workspace, and remove location-based production rejection. Distinguish test and production visits to a shared file. Preserve all source-role line data and add shared source hashes to the map. Capture checkout changes to those additional mapped files and convert patch paths regardless of workspace prefix. Normalize original Git reads, intent aliases, survivor diff headers and diff-based discovery. Validate saved source paths against the repository boundary before mutation.

Add a real nested-workspace CLI fixture in tests/campaign.rs: local binary modules, tracked shared production code under tests, a test-only path module, a file visited through both test and production aliases, and an untracked shared source that must be omitted. List/map must show production functions with shared paths and hits; a body edit must be captured, built and caught. Exercise explicit patch and repository-relative line intents, full rebuild and body carry-forward, preserving IDs/original bodies. Check traversal/symlink safety with focused unit or CLI regressions where meaningful.

## Concrete Steps

Work from the repository root. Run `nice -n 10 mbx test --test campaign` and `nice -n 10 mbx test`, then `mbx fmt --check` and `nice -n 10 mbx clippy --all-targets -- -D warnings`. Use ordinary mbx sessions and local sockets; do not redirect targets. Commit owned files on the current branch, push `git push origin HEAD:master`, verify CI for that exact head, then send an install handoff to session 5b9275ef422b8eb3146a4a11fb9dd1aa.

## Validation and Acceptance

An actual nested Cargo binary must list and map a function under ../../tests/shared.rs, record a selected test entering it, and catch an exact faulty body with no engine changes. Test-only functions must be absent. Patch capture and carry-forward must retain function/body identity across builds and rebuilds. No path may escape the repository or scratch staging directory. Existing feature-selection and runtime-hygiene campaign tests must remain green.

## Idempotence and Recovery

Temporary repositories hold regression inputs. Map/build operate on committed source in owned Git worktrees. Patches apply only in isolated scratch trees; bodies are validated before saving. Existing local source notation stays stable. Save exported mutant catalogs before mapping another commit and use build --from for exact-body carry-forward. A failed capture must leave state and user source unchanged.

## Interfaces and Dependencies

Use existing syn, cargo_metadata and Git commands. Add worktree helpers returning Result<String> for normalized repository and workspace paths; never normalize an escaping path by discarding leading parent components. No dependencies or new build backend are needed.

Plan created 2026-11-03 after the #42 handoff and read-only triage of #43.

Updated 2026-11-03 after real CLI and role-boundary evidence, path/symlink validation, and initial full-suite progress.
