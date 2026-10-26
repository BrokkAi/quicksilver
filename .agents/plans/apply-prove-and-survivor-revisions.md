# Compare survivors and prove captured mutations in a checkout

This living ExecPlan follows `.agents/PLANS.md`.

## Purpose / Big Picture

Users can review captured survivors against a chosen Git commit, then apply and revert a captured mutant in their current workspace and prove that a command catches it. Function bodies are located by name and exact text, so line movement does not cause edits to target the wrong code.

## Progress

- [x] (2026-10-25) Read issues #32 and #33 and inspect CLI, state, survivor and signal handling code.
- [x] (2026-10-25) Implement shared function locator and revision status; complete checks for #33.
- [ ] Implement apply/revert/prove, documentation and tests; validate and commit #32.

## Surprises & Discoveries

The existing survivor command requires the saved map's commit to equal checkout HEAD. Revision review must load the saved map directly instead. GitHub access required running the requested gh commands outside the network sandbox.

## Decision Log

The parent's design is authoritative: use syn spans for exact brace-inclusive ranges, preserve suffix matching of free names and Type.method names, compare original body bytes, and use shared repository state. Do not create branches or push. Apply validates every edit before writing; prove restores through a guard even when commands cannot start or receive a signal.

## Outcomes & Retrospective

The locator and survivor revision milestone is complete. Formatting passed; clippy reports only the known src/exec/build.rs:76 warning. Library tests passed (170 passed, two ignored) with normal permissions; the initial sandbox run failed only the socket-binding test. Integration suites passed: edited 2, engine 9, schema 3. Logs use the 33- prefix. Apply/prove implementation is pending.

## Context and Orientation

`src/survivors.rs` loads `SiteMap` and `exec::edited::State`, lists Missed records and formats text/JSON. `src/cli.rs` defines clap arguments and `src/lib.rs` dispatches commands. `src/capture.rs` stores BodyEdit original/body text, workspace-relative path, first line and full name. `src/worktree.rs` reads repository commits. `src/engine.rs::install_ctrlc` prevents termination on the first signal. Tests in `tests/edited.rs` build a tiny workspace and capture an always_ok mutant.

## Plan of Work

Create `src/locate.rs` with Located name, first line and body byte range, using syn's recursive visitor. Reuse its matching rule for survivors, cache parsed files once, add --at resolution and unchanged/changed/gone/unknown statuses plus compatible at_head JSON. Commit this first. Next create apply logic that plans replacements for every file before writing, reverses replacement roles for --revert, refuses QS-managed worktrees, and resolves unique mutant ID prefixes. Prove applies, invokes inherited command argv in cfg.repo_root, restores using a guard, invokes again and reports caught/not caught/inconclusive with codes 0/1/2; errors use 3. Extend README after survivor review and extend edited integration coverage without leaving edits behind.

## Concrete Steps

Work in `/mnt/optane/quicksilver/qs-wt-a`. Before each of the two commits run `cargo fmt --check`, `cargo clippy --all-targets`, `cargo test --lib`, and `nice -n 10 cargo test --test edited --test engine --test schema`. Save text logs in `/mnt/optane/tmp/qs-32-33-logs`. Use plain cargo with the existing mbx cache configuration. If socket or signal tests are blocked, rerun outside the sandbox.

## Validation and Acceptance

Locator tests cover free, method, nested and duplicate names and exact braces. A temporary git repository changes, removes and preserves three functions at a second commit and verifies statuses against the first map. Apply tests cover byte-exact restoration, idempotence, body-change diffs, multi-edit refusal without partial writes and managed-worktree refusal. Prove shell commands exercise all three outcomes. The edited integration test applies the captured always_ok mutant, restores original bytes and proves a grep command catches it. All mandatory checks pass, allowing only the known build.rs warning.

## Idempotence and Recovery

Apply/revert recognize already-applied/restored bodies. Validation refusal writes no files. Prove restores edited bodies on command errors and first Ctrl-C. Keep any uncommitted unrelated changes untouched. Make exactly two commits on detached HEAD; the parent cherry-picks them.

## Artifacts and Notes

Logs: `/mnt/optane/tmp/qs-32-33-logs`.

## Interfaces and Dependencies

`locate::functions_named(source: &str, fqn: &str) -> anyhow::Result<Vec<Located>>` uses existing syn/proc-macro2 span-locations and util::line_col_to_offset. No dependencies are needed. Survivor JSON adds at_commit and at_status while retaining at_head. Commands use shared State::load and Config repo_root/worktree_root.

Initial plan records the supplied design and repository observations.

Revision: recorded the first milestone and successful checks, including the sandbox-only socket failure.
