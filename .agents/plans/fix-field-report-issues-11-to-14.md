# Fix the second batch of field-report issues (BrokkAi/quicksilver#11 to #14)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md` at the repository root.


## Purpose / Big Picture


The agent testing cargo-quicksilver (QS) on bifrost crates filed four more issues on 2026-10-23, after the first ten were fixed (see `.agents/plans/fix-bifrost-field-report-issues.md`):

- #11: `uncovered` does not finish on a 3-million-line workspace, because the `[no callers]` check makes `git grep` print every line that contains a common name such as `new` or `fmt`.
- #12: after `test --proptest-cases N`, a survivor still says "try `test --proptest-cases 1024`", and nothing records that the re-run happened.
- #13: one intents file shared by several builds makes each build print a warning for every intent that belongs to another build.
- #14: the test sandbox lives under `.git/quicksilver/`, so git sees its temporary directories as inside a repository, and tests that expect otherwise fail. A test that is not green keeps none of its output.

After this plan, `uncovered` takes seconds on a large workspace; re-run survivors say how many cases they survived, in the output and the JSON; a shared intents file warns only about keys that name no function; sandboxes live outside any repository; and the map keeps the output tail of each test that was not green.


## Progress


- [x] (2026-10-23) Read the four issues and the code they touch; fast-forwarded to b71b180 (the other session's allocator fix).
- [x] (2026-10-23) #12 proptest re-run wording and JSON.
- [x] (2026-10-23) #13 intent warnings.
- [x] (2026-10-24) #11 caller check in-process (child agent; cherry-picked after review).
- [x] (2026-10-23) #14 sandbox location and not-green output.


## Surprises & Discoveries


- libtest prints a failing test's panic message and captured output on stdout, not stderr, so keeping a not-green test's output means capturing stdout too (#14). Seen while debugging #1: with stdout discarded, the mbx error behind `clap_complete::examples::example_tests` was invisible.

- A symbolic link does not help with #14: git finds the repository from the physical working directory, so a short link into `.git/quicksilver/...` still puts the test's temporary directory inside the repository. The real directory has to be outside.


## Decision Log


- Decision (#11): count identifier uses in-process: list `.rs` blobs with `git ls-tree` at the map commit, stream them through one `git cat-file --batch`, and scan each file once for words in the candidate set, dropping a name once every function with it has a use outside its own body and its file's tests; stop reading when no candidate is left. The semantics stay those of the `git grep -w` version.
  Rationale: the issue's measurements show the cost is the output of `git grep`, not the search; reading blobs ourselves never produces that output, and the early stop skips most of the work.
  Date/Author: 2026-10-23, Claude.

- Decision (#13): an intent that matches no mutant of the build is reported only when its key names no function in the map (a typo or a stale name); the others are counted in one line.
  Date/Author: 2026-10-23, Claude.

- Decision (#14): sandboxes move to a private, randomly named directory under the system temporary directory (QS's own `TMPDIR`), or under `[sandbox] dir` when set; QS refuses a sandbox directory that git sees as inside a repository. The short links from #1 are removed.
  Rationale: anything under `.git` is inside a repository for git and libgit2. The disk concern from #1 came from sandboxes that were never deleted; each is now deleted when its process exits, so the temporary directory holds at most one sandbox per running test. `[sandbox] dir` covers a small temporary filesystem.
  Date/Author: 2026-10-23, Claude.

- Decision (#14): a file `<QS state>/sandbox/<pid>` records each run's sandbox directory; a later run deletes the directories of runs whose process is gone, and only directories named `qs-sandbox-*`.
  Rationale: with sandboxes outside QS's state, `cargo quicksilver clean` no longer removes leftovers of a killed run; the name check keeps a corrupt record from deleting anything else.
  Date/Author: 2026-10-23, Claude.

- Decision (#14): the map keeps the last 40 lines (at most 8 KiB) of each not-green test's output, stdout then stderr, in `not_green_output`; warnings show the last 15.
  Date/Author: 2026-10-23, Claude.

- Decision (#11): a child agent implemented the decided design in a detached worktree (`/mnt/optane/quicksilver/qs-callers-wt`), and its commit was cherry-picked after review. Its own plan is `.agents/plans/stream-caller-check.md`.
  Rationale: the change was independent of #13 and #14, and its benchmark output on a large repository did not need to be in this session's context.
  Date/Author: 2026-10-23, Claude.


## Outcomes & Retrospective


(to be written)


## Context and Orientation


Key files: `src/sitemap.rs` (`without_callers`), `src/lib.rs` (`cmd_uncovered`), `src/exec/edited.rs` (`test`, `verdict_line`, `build`), `src/capture.rs` (`attach_intents`), `src/sandbox.rs`, `src/exec/test.rs` (`record_one`, `SiteRecording`), `src/exec/process.rs`, `src/exec/schema.rs` (where not-green tests are reported).


## Plan of Work, Concrete Steps, Validation


One commit per issue with "Fixes #N", in the order of `Progress`. Each adds unit tests where the logic is pure and extends `tests/edited.rs` where it needs a build. After each: `cargo test --lib`, the integration tests, `cargo clippy --all-targets`, commit, push. For #11, compare the old and new caller checks on `rust-lang/rust` (a local clone; reading only) with common names.


## Idempotence and Recovery


Plain code changes; each commit stands alone and can be reverted.


## Artifacts and Notes


#11, from the child's evidence (all at `nice -n 10` on the shared host; rough times):
- clap 13f2db5, 1,100 candidates: the old and new checks give the same 501 site ids; new about 0.7 s, old about 47 s.
- rust-lang/rust a1e52fc, 37,241 `.rs` blobs, 1,100 candidates including `new`, `fmt`, `len`, `from`, `default`, `build`, `get`: new about 66 s with 447 functions without callers (rare names force a full scan); the old check did not finish in 300 s.

#14, on clap (`run --build-jobs 3 --limit 30 -p clap_complete`, sandbox on, log `/mnt/optane/quicksilver/local/clap-sandbox-14.log`): 0 tests not green, 82 sandboxed processes, all deleted. `example_tests` builds through the mbx shim, which binds a socket in `TMPDIR`, so the new location leaves room for it.


## Interfaces and Dependencies


No new crates.
