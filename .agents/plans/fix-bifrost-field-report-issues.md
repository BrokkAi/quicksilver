# Fix the ten issues from the bifrost-core field run (BrokkAi/quicksilver#1 to #10)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md` at the repository root.


## Purpose / Big Picture


An agent ran cargo-quicksilver (QS) on bifrost-core on 2026-10-23: a map, about 930 hand-written mutants in parallel lanes, `build`, `test`, and a review of the survivors. It filed ten issues against BrokkAi/quicksilver. Together they say: mutants can damage the user's machine, the edited-mutant workflow loses context and breaks under parallel authoring, some functions cannot be mutated, and several verdicts and lists mix signal with noise.

After this plan:

- Tests run in a per-process sandbox: fresh `HOME`, `XDG_*` and `TMPDIR` under QS's state directory, deleted after each process (#1).
- Every edited mutant keeps its original body and an optional author's note, shown in `test` output and JSON (#2).
- Edited mutants of `-> impl Trait` functions are compiled one per build instead of coming back unviable (#3).
- Functions in `impl` blocks and `fn` items nested inside a function body are their own sites (#4).
- A timeout the mutant caused counts as caught; one that load could explain stays a timeout (#5).
- Functions compiled out on this target are left out of the mutant lists and `uncovered`; the map and verdict files record the target (#6).
- Captures are locked, can come from patch files, and can drop edits outside bodies instead of rejecting the round; `build --capture-only` is removed (#7).
- `--extra-tests` adds chosen downstream test binaries to a package run (#8).
- `uncovered` marks functions with no callers and hides test scaffolding (#9).
- Verdicts decided only by proptest properties are flagged, and can be re-run with more cases (#10).


## Progress


- [x] (2026-10-23 23:10Z) Read all ten issues and the code they touch, including the other session's seven commits (50cf721 to 5b05298).
- [ ] #7 capture: lock, `--patch`, `--skip-outside-bodies`, remove `--capture-only`.
- [ ] #2 original body and intent.
- [ ] #4 nested items as sites.
- [ ] #3 direct builds for `impl Trait` sites.
- [ ] #6 host `cfg` and target triple.
- [ ] #9 `uncovered` marks.
- [ ] #8 `--extra-tests`.
- [ ] #5 timeouts.
- [ ] #10 proptest.
- [ ] #1 sandbox.
- [ ] README and outcomes.


## Surprises & Discoveries


(none yet)


## Decision Log


- Decision: one commit per issue, each with "Fixes #N" so the issue closes when it reaches master, in the order above.
  Rationale: #2 builds on #7's capture refactor, #3 on #4's offset handling. The owner asked to fix all of them.
  Date/Author: 2026-10-23, Claude.

- Decision (#7): drop edits outside bodies hunk by hunk: diff the original and edited file with `git diff --no-index -U0`, keep each hunk whose application alone still splits cleanly into bodies, revert the others with a warning naming the lines.
  Rationale: this reuses the body split's exact semantics, so a hunk is kept exactly when `build` would have accepted it alone; line-based heuristics cannot handle one-line functions.
  Date/Author: 2026-10-23, Claude.

- Decision (#2): intents come from a JSON file passed to `build --intents FILE`, mapping a full function name (as QS prints it) or `path:line` (any line of the function) to text, applied only to mutants captured by that `build`.
  Rationale: the issue's failure mode was joins on short names attaching notes to siblings; exact keys scoped to one capture avoid both.
  Date/Author: 2026-10-23, Claude.

- Decision (#3): every edited mutant of a function whose return type contains `impl` is built directly: its body replaces the original in its own build, with all other sites as in the main generation, and its tests run against those binaries while null runs use the main generation's.
  Rationale: the schema keeps the original body after the prologue, so for an opaque return type even one mutant conflicts when its concrete type differs (a different closure is a different type). A direct build avoids any arm.
  Date/Author: 2026-10-23, Claude.

- Decision (#5): a timeout confirmed against a null run that passed in at most a third of its calibration budget becomes Caught with a "timed out under the mutant" message, counted like crashes; a timeout whose null run was itself slow stays Timeout. `[test-budget]` in quicksilver.toml sets a minimum budget per test.
  Date/Author: 2026-10-23, Claude.

- Decision (#9): scaffolding is hidden from `uncovered` by default with a count and `--scaffolding` to show it; "no callers" is a marker, computed with one `git grep -w -F` at the map's commit, ignoring trait-impl methods.
  Rationale: the owner chose to omit untracked functions by default; scaffolding is the same kind of noise.
  Date/Author: 2026-10-23, Claude.

- Decision (#10): flagging is always on; re-running with more proptest cases is opt-in (`--proptest-cases N`).
  Date/Author: 2026-10-23, Claude.

- Decision (#1): the sandbox copies small tool configs (git, mbx) into it and points `CARGO_HOME`, `RUSTUP_HOME` and `MBX_CACHE_DIR` at the real ones, so tests that run cargo or git behave as before; `--no-sandbox` and `[sandbox] enabled = false` turn it off.
  Rationale: a fresh `HOME` alone would make child cargo builds lose the registry and mbx create a new cache inside the sandbox.
  Date/Author: 2026-10-23, Claude.


## Outcomes & Retrospective


(to be written)


## Context and Orientation


See `.agents/plans/edited-mutants-map-build-test.md` for the map, build and test commands, `.agents/plans/git-worktrees-and-mbx.md` for the worktree layout, and `.agents/plans/remove-coverage-mode.md` for selection. Key files: `src/capture.rs` (splitting edited files into bodies), `src/exec/edited.rs` (map, build, test), `src/exec/test.rs` (running tests and confirming verdicts), `src/exec/process.rs` (spawning test processes), `src/scan.rs` (the module walk), `src/sitemap.rs` (the map), `src/schema.rs` (source rewriting and error attribution), `src/lib.rs` and `src/cli.rs` (commands).


## Plan of Work, Concrete Steps, Validation


Each issue lands with unit tests where the logic is pure and an extension of `tests/edited.rs` (or `tests/engine.rs`) where it needs a build. After each: `cargo test --lib`, `cargo test --test edited --test engine --test schema`, `cargo clippy --all-targets`, then commit and push. The fixture `tests/fixtures/tiny/crate_schema` gains a nested `impl` and an asserted `impl Trait` function for #3 and #4.


## Idempotence and Recovery


Plain code changes; each commit stands alone and can be reverted.


## Artifacts and Notes


(to be filled)


## Interfaces and Dependencies


No new crates except enabling `syn`'s `visit` feature for #4.
