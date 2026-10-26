# Count survivors that only unusable tests could not judge (BrokkAi/quicksilver#34)

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md` at the repository root.


## Purpose / Big Picture


In the brokk-bifrost-js-ts round, a deadlock watchdog test (it fails if a scan takes over 60 s) failed under the slower schema binary with no mutant active. Any mutant that selected it ended `errored`, even when every other selected test passed: 23 of 26 errored mutants in one generation, 32 of 35 in the other. These were survivors that never reached `survivors` or `pending`. And each such mutant ran the slow test about four more times. After this change, such a mutant is `missed`, its verdict names the tests that could not judge it, a test found unusable is not run again in the same `test` run, and the run ends with a list of these tests to add to `[map] skip-tests`.


## Progress


- [x] (2026-10-26) Read the issue and `run_selected_tests_observed` in `src/exec/test.rs`.
- [x] (2026-10-26) Implemented in this session: the missed verdict, the per-run memo, `Verdict.unusable_tests`, the end-of-run list, `survivors` and the `run` mode MISSED line; tests; README.


## Surprises & Discoveries


- A memo in a process-wide static would have leaked between the unit tests, whose fake runners all use `/bin/true` and names such as `t`; the memo lives in `TestRunPolicy` instead, so it lasts exactly one run (one `test` command, or one batch in `run` mode).


## Decision Log


- Decision: when at least one selected test ran and passed and the rest were unusable (failing twice with no mutant active, as #30 decides), the outcome is `Missed` with a message naming the unusable tests; when no selected test could judge the mutant, it stays `Errored`. The edited verdict also lists them in `unusable_tests`.
  Rationale: the mutant survived every test able to judge it, which is what a survivor is; a new verdict kind would touch every match on `VerdictKind` for a distinction the list already carries.
  Date/Author: 2026-10-26, Claude.

- Decision: the memo is keyed by staged binary and test name and shared by the clones of one `TestRunPolicy`.
  Date/Author: 2026-10-26, Claude.


## Outcomes & Retrospective


Unit tests (185) cover the missed verdict after the full confirmation sequence, the memo (a later mutant does not run the watchdog; another run starts empty), the all-unusable case staying errored, and `unusable_tests` on the edited verdict and its MISSED line. `tests/edited.rs` (3), `tests/engine.rs` (9) and `tests/schema.rs` (3) pass, each run on its own.


## Context and Orientation


`run_selected_tests_observed`, `UnusableMemo` and `InfraSkip` in `src/exec/test.rs`; `Verdict`, `verdict_from`, `verdict_line` and the end of `test()` in `src/exec/edited.rs`; `src/survivors.rs`; `print_exhaustive` in `src/report.rs`.


## Plan of Work, Concrete Steps, Validation


Done as above.


## Idempotence and Recovery


One commit; it can be reverted.


## Artifacts and Notes


None.


## Interfaces and Dependencies


No new crates.
