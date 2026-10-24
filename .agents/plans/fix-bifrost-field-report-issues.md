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
- [x] (2026-10-23) #7 capture: lock, `--patch`, `--skip-outside-bodies`, remove `--capture-only` (1dc454d).
- [x] (2026-10-23) #2 original body and intent (c507c2f).
- [x] (2026-10-23) #4 nested items as sites (f57ec39).
- [x] (2026-10-23) #3 direct builds for `impl Trait` sites (3f8b0dd).
- [x] (2026-10-23) #6 host `cfg` and target triple (bd1330b).
- [x] (2026-10-23) #9 `uncovered` marks (8ee0dcc).
- [x] (2026-10-23) #8 `--extra-tests` (3491126).
- [x] (2026-10-23) #5 timeouts (f0a2d38).
- [x] (2026-10-23) #10 proptest (7e279b1).
- [x] (2026-10-23) #1 sandbox, in the commit that closes this plan.
- [x] (2026-10-23) README updated with each issue; the sample `outcomes:` line regenerated from a clap run.


## Surprises & Discoveries


- Even a single schema arm cannot hold an edited mutant of an `-> impl Trait` function: the original body stays after the prologue, so a different concrete type (any other closure) conflicts. Hence direct builds (#3).
  Evidence: the `as_impl` fixture mutant was unviable under the schema and compiles directly.

- Nested sites need offsets into the already rewritten text: an outer function's prologue shifts every inner body. `schema::apply_to_source` maps offsets with `shifted()`, and errors are attributed to the innermost site (#4).
  Evidence: `nested_sites_get_offsets_into_the_rewritten_text`.

- The calibration test with a 40 s null run against a 120 s budget failed under `<=`; the issue says "under a third", so the comparison is a strict `<` (#5).

- nextest 0.9.140 still marks setup scripts as experimental ("experimental features used but not enabled: setup-scripts"), so there is no stable way to give only the baseline's test processes a sandbox while `mbx nextest run` keeps the user's environment (#1).

- The first clap run with the sandbox turned `clap_complete::examples::example_tests` red: it builds the examples through the mbx cargo shim, which binds a socket under `TMPDIR`, and the sandbox path under `.git/quicksilver/main/.quicksilver/sandbox/<pid>/<n>/tmp` was longer than a Unix socket path may be.
  Evidence: `mbx[error]: failed to bind cache session listener at /mnt/optane/quicksilver/local/clap/.git/quicksilver/main/.quicksilver/sandbox/79228/81/tmp/mbx-session-OnIIon/cache-agent.sock: path must be shorter than SUN_LEN`. With the short links the test is green and the sandboxed and unsandboxed runs give the same outcomes.

- The old README sample (`--limit 120 -p clap_lex -p clap_complete`, 46 caught and 1 missed) no longer reproduces at the same clap commit: the same command now makes 41 mutant builds instead of 59, and `OsStr::contains { true }` is the 239th of 302 mutants, past the limit. `--limit` keeps the first planned mutants in batch order (`src/exec/schema.rs`), so changes to the catalog (nested functions, compiled-out functions) change which ones run. With no limit, `-p clap_lex` has no missed mutant, so the README now shows that run's real summary.

- mbx uses `HOME` for C-compiler path mappings (`crates/mbx/src/cc.rs`, `path_mappings`) and for `~` in its `keep` and `evict_first` patterns (`crates/mbx/src/config.rs`). Running mbx itself with a sandboxed `HOME` could therefore change its caching.


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

- Decision (#1): every test process QS starts itself is sandboxed (site recording in `map`, mutant runs, null runs, timeout calibration), each in its own directory. The baseline `cargo nextest run` is not, which departs from the issue's proposal to run the baseline in the same sandbox.
  Rationale: the baseline runs no mutants, so it cannot plant bad state; the verdicts compare only sandboxed runs, and a test that fails in the sandbox is not green in the recording and selects nothing. Sandboxing it would mean changing `HOME` for mbx itself (see Surprises) or depending on nextest's experimental setup scripts.
  Date/Author: 2026-10-23, Claude.

- Decision (#1): each sandbox's variables name it through a symbolic link `<system temp>/qs-sandbox-<random>/<n>`, in a directory only the user can enter; the files stay under QS's state.
  Rationale: a Unix socket's path must fit in about 108 bytes, and the state directory can be deep. The issue asked for the data to stay on the worktree's filesystem, because a `TMPDIR` under `/tmp` filled the root disk; the link keeps the data there and the paths short.
  Date/Author: 2026-10-23, Claude.

- Decision (#1): the reported size is what a process left in its sandbox when it exited, measured before deletion, not a true peak.
  Rationale: a true peak needs polling during the run; what a process leaves is what filled the disk in the field report.
  Date/Author: 2026-10-23, Claude.


## Outcomes & Retrospective


All ten issues are fixed, one commit each with "Fixes #N". The unit tests, `tests/edited.rs`, `tests/engine.rs` and `tests/schema.rs` pass. The end-to-end test in `tests/edited.rs` now covers patches, skipping edits outside bodies, intents, nested sites, `impl Trait` direct builds, `cfg` filtering, `uncovered` marks, proptest flagging with a re-run, and the sandbox (a fixture test that checks its `HOME` and `[test-env]` passes, and no sandbox file remains).

Not done: a true peak size per sandbox, and sandboxing the baseline nextest run (see Decision Log). Bifrost-core itself was not re-run; the fixes were checked on the fixture and on clap.


## Context and Orientation


See `.agents/plans/edited-mutants-map-build-test.md` for the map, build and test commands, `.agents/plans/git-worktrees-and-mbx.md` for the worktree layout, and `.agents/plans/remove-coverage-mode.md` for selection. Key files: `src/capture.rs` (splitting edited files into bodies), `src/exec/edited.rs` (map, build, test), `src/exec/test.rs` (running tests and confirming verdicts), `src/exec/process.rs` (spawning test processes), `src/scan.rs` (the module walk), `src/sitemap.rs` (the map), `src/schema.rs` (source rewriting and error attribution), `src/lib.rs` and `src/cli.rs` (commands).


## Plan of Work, Concrete Steps, Validation


Each issue lands with unit tests where the logic is pure and an extension of `tests/edited.rs` (or `tests/engine.rs`) where it needs a build. After each: `cargo test --lib`, `cargo test --test edited --test engine --test schema`, `cargo clippy --all-targets`, then commit and push. The fixture `tests/fixtures/tiny/crate_schema` gains a nested `impl` and an asserted `impl Trait` function for #3 and #4.


## Idempotence and Recovery


Plain code changes; each commit stands alone and can be reverted.


## Artifacts and Notes


Runs in `/mnt/optane/quicksilver/local/clap` at 13f2db5, inside `systemd-run --user --scope -p MemoryMax=48G`:

- `run --build-jobs 3 -p clap_lex` (the README sample), log `clap-lex-run.log`: 67 total, 40 caught, 0 missed, 13 not covered, 14 unviable; 240 sandboxed test processes, the largest left 1.7 KiB, all deleted.
- `run --build-jobs 3 --limit 120 -p clap_lex -p clap_complete`, with and without `--no-sandbox` (logs `clap-sandbox-run.log`, `clap-nosandbox-run.log`): both 31 caught, 74 not covered, 15 unviable, no test red.


## Interfaces and Dependencies


No new crates except enabling `syn`'s `visit` feature for #4.
