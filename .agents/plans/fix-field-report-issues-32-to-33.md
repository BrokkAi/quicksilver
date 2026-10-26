# Fix the brokk-bifrost-python field-report batch (BrokkAi/quicksilver#32 and #33)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md` at the repository root.


## Purpose / Big Picture


The agent testing cargo-quicksilver (QS) on Bifrost filed two issues from the brokk-bifrost-python round, where #29 to #31 worked in real use (timeout kills fell from 22% to 4% of kills). Reviewers prove each claimed kill by hand in a newer checkout than the map's commit: 74 times in this round, 59 in the jvm round, with one misapplied mutant. And `survivors` says whether a function is "still at HEAD" of the checkout it runs in, usually the map's own worktree, so the answer was always yes. After this plan:

- `cargo quicksilver apply <id> [--revert]` puts a captured mutant into, or takes it out of, any checkout of the repository, refusing when the function's body changed since the map (#32).
- `cargo quicksilver prove <id> -- <command>` applies, runs the command expecting failure, reverts, and runs it again expecting success, then prints one line: caught, not caught or inconclusive (#32).
- `survivors --at <rev>` says whether each survivor's function is unchanged, changed or gone at that revision (#33).


## Progress


- [x] (2026-10-26) Read both issues and the capture, survivors and worktree code.
- [x] (2026-10-26) #33 and the shared locator (d5cc1ae), then #32 (f755f41), by a child agent, reviewed and cherry-picked. Its own plan: `.agents/plans/apply-prove-and-survivor-revisions.md`.
- [x] (2026-10-26) Review additions: trait default methods in the locator, and a non-ASCII column test (asked of the child, in f755f41); `prove` stops after reverting when interrupted, and the README warns that a mutant that does not compile in the checkout makes a cargo command fail too (this session).
- [x] (2026-10-26) Full test run on the combined code; pushed.


## Surprises & Discoveries


- My brief's end-to-end command for `prove` (`! grep -q "    false" crate_a/src/lib.rs`) could never report caught: `maybe_hang()` in the fixture already has a `false` body. The child stopped and asked, as instructed, and scoped the grep to `always_ok`'s lines.

- The first locator missed trait default methods, which the map makes sites for (`<module>.<Trait>.<method>`): `apply` would have refused them and `survivors --at` called them gone.

- The child's interpretation of "QS is not killed by the first Ctrl-C" was to revert and then run the clean check anyway; an interrupted command's exit status proves nothing, so `prove` now stops after reverting.


## Decision Log


- Decision: one locator (`src/locate.rs`) finds functions by full name, with methods matched with their `impl` type, as `survivors` already did; `apply`, `--revert`, `prove` and `survivors --at` all use it.
  Date/Author: 2026-10-26, Claude.

- Decision (#32): `apply` replaces a body only when it equals the mapped original byte for byte, and only after every edit of the mutant has matched (all or nothing). `--revert` is the same operation with the mutant body and the original swapped, so it needs no record of the earlier `apply` and works after a crash. Both refuse to run inside QS's own worktrees.
  Rationale: the field report's failure was a misapplied mutant; an exact match either applies the reviewed edit or refuses with a diff of what changed. Editing QS's map worktree by hand would corrupt later builds.
  Date/Author: 2026-10-26, Claude.

- Decision (#32): `prove` always reverts, even when the command cannot start or the user interrupts; it reports caught (fails with, passes without), not caught, or inconclusive (fails without), with exit codes 0, 1 and 2, and 3 for errors.
  Date/Author: 2026-10-26, Claude.

- Decision (#33): `--at` reports `unchanged` (a function with that name has the mapped original body), `changed`, `gone` or `unknown` (the file does not parse); the JSON keeps `at_head` for compatibility and adds `at_commit` and `at_status`.
  Date/Author: 2026-10-26, Claude.


## Outcomes & Retrospective


Both issues are closed. The unit tests (183), `tests/edited.rs` (3), `tests/engine.rs` and `tests/schema.rs` pass. The end-to-end test applies and reverts the fixture's missed mutant in the author's checkout and in a separate newer worktree, proves it caught, checks a command that cannot start, and an interrupt during the first run; by hand, `prove` also reported not caught and inconclusive for `true` and `false`.

Limit: `prove` judges only exit statuses, so a mutant that no longer compiles in the checkout makes a cargo command fail and reads as caught; the README says to read the command's output.


## Context and Orientation


Key code: `capture::BodyEdit` (`original` is the file text over the site's body bytes at the map's commit), `src/survivors.rs`, `crate::worktree`, `Config::worktree_root`.


## Plan of Work, Concrete Steps, Validation


The child commits in its worktree; I review, cherry-pick, run `cargo test --lib`, `cargo test --test edited --test engine --test schema` and clippy on the combined code, and push.


## Idempotence and Recovery


Plain code changes; each commit can be reverted.


## Artifacts and Notes


Commits: d5cc1ae (#33), f755f41 (#32), and this session's review fixes in the commit that records this outcome.


## Interfaces and Dependencies


No new crates.
