# Remove LLVM coverage selection, `build --restore`, and refocus the README

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md` at the repository root.


## Purpose / Big Picture


cargo-quicksilver (QS) chooses each mutant's tests in one of two ways, set with `--test-selection`. `coverage` (the default) builds the workspace a second time with LLVM instrumentation and runs the suite under it; `sites` records which tests enter which functions through the same run-time switch that turns mutants on. For QS's mutants, which replace whole function bodies, the two select the same tests, and `sites` needs no second build and no `llvm-tools`. The coverage code is about 1,600 lines in `src/index.rs` and broke under mbx this week.

After this change there is one way: sites. `--test-selection`, `--index-scope`, `--reindex`, `--index-jobs` and the `index` command are gone. The two features that relied on coverage keep working on sites: `estimate` takes the share of mutants no test reaches from a map (the result of `cargo quicksilver map`, which records which functions every test enters), building one when there is none for the current commit; `audit-selection` compares selected tests with the full suite under sites selection and counts a NotCovered or Missed verdict that the full suite catches as a lost kill. It becomes a hidden command for checking QS on a new repository.

`cargo quicksilver build --restore` also goes. It was the only place QS wrote into the user's checkout, and `git checkout -- .` does the same.

The README is rewritten around the happy path: install, commit, `run` (or `run --in-diff`), the map, edit, build, reset, test loop for one's own mutants, what results mean, and `clean`.


## Progress


- [x] (2026-10-23 20:00Z) Surveyed coverage-mode dependencies: `config::TestSelection`, `index::CoverageIndex` (also the container of the test list and the coupling cache key), `engine::prepare`, `engine::estimate_from`, `exec::schema::run_crate` and `mutant_pool_policy`, `lib.rs` (`cmd_index`, `cmd_estimate`, `cmd_audit_selection`), `plan.rs`, `baseline.rs` and `cargo_cmd.rs` (`index_scope` in seeds), the engine and schema tests.
- [x] (2026-10-23 21:00Z) Code: mode and flags removed; `index.rs` reduced from about 1,650 to about 200 lines; `Plan.full_suite`; `sitemap::MapSelector`; `estimate` on the map (`exec::edited::map_for_head`); `audit-selection` on sites, hidden; `--restore` removed; `util::find_llvm_tool`, the rustc sysroot helpers, the `ignore` dependency and the `llvm-tools` component removed.
- [x] (2026-10-23 21:10Z) Tests: 96 unit, 12 integration, all passing. `site_selection_matches_coverage_selection` became `selection_loses_no_kill_against_the_full_suite`; the index-scope test went with the flag; a unit test covers `MapSelector`.
- [x] (2026-10-23 21:15Z) README rewritten around install, `run`, the map/edit/build/reset/test loop, results, and `clean`.


## Surprises & Discoveries


- Observation: a full-suite audit reports "selected NotCovered, full Missed" for every function no test reaches, because the full suite runs tests that never enter it. That is agreement, not a disagreement: no test catches the mutant either way. `report::audit_compare` now counts it as agreement.
  Evidence: `audit-selection --sample 8` on the fixture listed `tiny_a.uncovered`, `tiny_b.uses_flag` and `tiny_c.wrapper` as disagreements before the change.


## Decision Log


- Decision: `audit-selection` stays, hidden from `--help`, and counts a selected NotCovered or Missed against a full-suite Caught as a lost kill.
  Rationale: the owner agreed it is a check that QS works as intended on a new repository. Under sites selection the failure to catch is a test that never recorded entering a function it does reach, which shows up as NotCovered, not only as Missed. The 11 clap mutants cargo-mutants caught and QS did not were of that kind.
  Date/Author: 2026-10-23, owner and Claude.

- Decision: `estimate` uses a map for the not-covered share and for the mean selected-test time, through a `MapSelector` that plans every discovered mutant against the map's recorded tests.
  Rationale: the owner pointed out that after `map` sites mode knows, for every function, whether any test enters it. Matching map sites to mutants by the site seed (package, path, name, body start) avoids depending on site id assignment order.
  Date/Author: 2026-10-23, Claude.

- Decision: `estimate` does not build a map itself. It refuses to start without a map of HEAD and tells the user to run `cargo quicksilver map`.
  Rationale: the owner asked for this not to be implicit (2026-10-23). A map build is a full schema build and recording run; a command should not start one as a side effect.
  Date/Author: 2026-10-23, owner.

- Decision: the coupling evidence's cache key becomes the commit hash of the main worktree.
  Rationale: it was the coverage index's source hash; the commit identifies the source exactly now that QS tests commits.
  Date/Author: 2026-10-23, Claude.


## Outcomes & Retrospective


Done. QS has one selection mode, needs no `llvm-tools`, and lost about 1,500 lines. `estimate` on the fixture built a map the first time (`map: ... 14 of 17 sites entered`), reused it the second time (`using the map built at 3757734cc353`), and reported `f_nc = 0.240` as a census of all 25 mutants rather than a sample proportion. `audit-selection --sample 8` found no lost kills. `--help` lists no `index` command and no `--test-selection`; `build --help` has no `--restore`.


## Context and Orientation


`engine::prepare` builds the baseline, lists tests (`index::list_tests_plain`), discovers mutants and plans them with a `plan::TestSelector`. Under sites selection the selector is `plan::PackageSelector`, which gives every mutant all tests of packages that depend on its package; `exec::schema::run_crate` then records which of those enter each site and narrows each mutant to them. A plan made with `plan::plan_full_suite` must skip that narrowing, so `Plan` gains `full_suite: bool`.


## Plan of Work


Remove `TestSelection` and the flags from `src/config.rs` and `src/cli.rs`, the `Index` command, and `BuildArgs::restore`. Reduce `src/index.rs` to `list_tests_plain`, `parse_nextest_list`, `TimingMatch` and `attach_timings` (on a test slice). `engine::Prepared` holds `tests`, `source_key` and a `PackageSelector`. Add `sitemap::MapSelector` implementing `plan::TestSelector`. Split `exec::edited::build_map` into `build_map` and `build_map_from(cfg, paths, baseline, tests)`, and add `map_for_head` that reuses a map built at HEAD or builds one and then checks the main worktree out clean again. `cmd_estimate` plans the census with the map selector. `cmd_audit_selection` runs the sample selected and with `plan_full_suite`. Remove `util::find_llvm_tool` and the `llvm-tools` component.


## Concrete Steps


    cargo test --lib
    cargo test --test edited --test engine --test schema


## Validation and Acceptance


All tests pass; `cargo quicksilver --help` lists no `index` command and no `--test-selection`; `cargo tree` and the toolchain file need no `llvm-tools`; a new engine test runs every fixture mutant with selection and with the full suite and finds no lost kill.


## Idempotence and Recovery


Plain edits; `git revert` restores the old mode.


## Artifacts and Notes


    $ cargo quicksilver estimate --sample 6        (fixture copy, first run)
    map: 17 sites, 0 functions without a site
    map: recorded 12 tests in 0.0s, 0 not green; 14 of 17 sites entered
      f_nc (not covered): 0.240  [census of workspace]
      - f_nc = 0.240 is a census of the 25 mutants against the map: a mutant is not covered when no test entered its function in the map's recording run.

    $ cargo quicksilver estimate --sample 6        (second run)
    using the map built at 3757734cc353

    $ cargo quicksilver audit-selection --sample 8
    kills lost by selection (full suite Caught, selected tests Missed or NotCovered): 0


## Interfaces and Dependencies


    // src/sitemap.rs
    pub struct MapSelector<'a> { .. }
    impl<'a> MapSelector<'a> { pub fn new(map: &'a SiteMap) -> Self }
    impl plan::TestSelector for MapSelector<'_> { .. }

    // src/exec/edited.rs
    pub fn build_map_from(cfg: &Config, paths: &Paths, tests: Vec<TestInfo>) -> Result<SiteMap>;
    pub fn map_at(paths: &Paths, commit: &str) -> Result<SiteMap>;   // loads only; errors with "run `cargo quicksilver map` first"

Revision note (2026-10-23, later): `estimate` no longer builds a map when none exists; `exec::edited::map_for_head` became `map_at`, which only loads a map of the given commit or fails with the command to run.
