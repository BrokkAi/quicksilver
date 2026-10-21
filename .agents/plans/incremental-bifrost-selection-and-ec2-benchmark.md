# Select tests from Bifrost's incremental call graph and benchmark against cargo-mutants on EC2

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds. Maintain it in accordance with `.agents/PLANS.md` at the repository root.

The prior plan `.agents/plans/prove-schema-v2-on-bifrost-core.md` proved that the schema engine (one compile per crate, mutants switched on at run time through the `QS_MUTANT` environment variable) gives trustworthy first-order verdicts. This plan builds on that engine and does not change how a mutant is compiled or judged.

## Purpose / Big Picture

Today cargo-quicksilver (QS) is only fast on a source tree that has not changed since its coverage index was built. The index has two stages. Stage A runs the whole test suite once under LLVM source-based coverage to learn which lines any test executes. Stage B runs every test of every package that depends on the mutated crate, one at a time under coverage, to learn which test executes which line. On `brokk-bifrost-core` Stage A takes hours and Stage B took about 47 minutes for 43 files. Any edit to any `.rs` or `Cargo.toml` file changes the index cache key (`src/index.rs` `source_cache_key`) and throws both stages away. Discovery also parses the whole workspace from scratch on every run (`src/discover.rs`, `WorkspaceAnalyzer::build_ephemeral`).

Bifrost (the code-analysis library QS already uses for discovery, checked out at `../bifrost` relative to the main QS checkout, i.e. `/home/jonathan/Projects/bifrost`) now has a persistent, content-addressed analysis store. A second process reuses facts for every file whose git blob did not change and re-derives only the crates an edit reached. It also exposes the whole-workspace usage graph: every caller-to-callee edge between functions, with completeness flags.

After this plan, a user can run QS with `--test-selection static`. QS then picks each mutant's tests by walking Bifrost's call graph backwards from the mutated function to the test functions that can reach it, with no instrumented build, no Stage A and no Stage B. On a later commit the Bifrost store is warm, the cargo target directory is reused, and only the changed functions' mutants are run with `--in-diff`. The plan ends with a measured comparison against cargo-mutants on an idle EC2 machine: exhaustive on `brokk-bifrost-core` and per commit over a replay of real commits, with wall time and per-mutant verdict agreement. The claim "N× faster than cargo-mutants" in this repository must cite those measurements.

## Progress

- [x] (2026-10-21) Research: Bifrost API surface for persistence, usage graph, diff analysis, and test detection; EC2 tooling and conventions; cargo-mutants defaults. Findings are in `Context and Orientation`.
- [x] (2026-10-21) Milestone 1 code: build against the local Bifrost checkout through the uncommitted patch; fixed the four API breaks (`build_ephemeral` → persisted store in `src/bifrost.rs`, `AnalyzeDiffParams.call_edges`, `analyze_diff_at_root` cancellation token); discovery now uses `bifrost::open_workspace`.
- [x] (2026-10-21) Fixed a pre-existing Stage A failure (also on unmodified `5d8131f`): `util::find_llvm_tool` and `rustc_verbose_version` now ask the target workspace's toolchain, not QS's; the tiny fixture pins 1.97.1. `cargo test --lib --test engine --test schema`: 84 + 4 + 3 passed.
- [x] (2026-10-21) Milestone 2 prototype: `src/reach.rs` and hidden `graph-probe`. On the tiny fixture all 12 nextest tests map to graph nodes and 24 of 25 mutants get a complete selection (the 25th, `as_mut`, falls back because the name is unresolved somewhere).
- [x] (2026-10-21) Aligned QS with Bifrost master for the shared mbx cache: toolchain 1.99.1, `[profile.dev] debug = "line-tables-only"`, `LIBSQLITE3_FLAGS` in a committed `.cargo/config.toml` (commit `4d93551`).
- [x] (2026-10-21) Measured on `semver` (debug build, owner's instruction): Bifrost store open 2.17 s cold, 0.18 s warm; `usage_graph` about 18–21 s warm for 158 nodes and 333 edges, almost all in `usage_graph::resolve_rust`, recomputed by every process.
- [x] (2026-10-21) Found and fixed unsound static selection: `unresolved_names=None` now marks every walk incomplete (Bifrost's documented contract), and trait methods Rust calls implicitly (`fmt`, `eq`, `cmp`, `drop`, `deref`, `clone`, …) mark a walk incomplete. On `semver` every mutant now falls back to all 34 tests.
- [ ] Switch to `=0.14.0` when published.
- [ ] Milestone 2 remainder: fixture cases for trait dispatch, default-method overrides, function references, macro-wrapped calls, integration tests under `tests/`; compare static against dynamic selection on the fixture.
- [ ] Milestone 3: `--test-selection static` execution mode that skips Stage A and Stage B, with conservative fallback for incomplete reachability; in-place baseline sync that keeps `target/` and the Bifrost cache across commits.
- [ ] Milestone 4: audit static selection against dynamic Stage B selection on a `brokk-bifrost-core` sample (soundness: every dynamically selected test must also be statically selected).
- [ ] Milestone 5: EC2 benchmark against cargo-mutants (exhaustive and per-commit replay), verdict agreement, teardown with proof.

## Surprises & Discoveries

- Observation: the QS scratch session used by the prior plan (`/mnt/optane/quicksilver/065e17f14dc00ece`, including `coverage.json` and the reference reports `estimate-core-v1f.json` and `estimate-core-v2c.json`) no longer exists on this host. Every index has to be rebuilt; the per-mutant reference verdicts of the prior plan are not available for comparison.
  Evidence: `ls /mnt/optane/quicksilver` → no such file or directory (2026-10-21).
- Observation: QS's baseline copy is materialized once and replaced wholesale (`src/scratch.rs` `materialize_baseline` calls `remove_dir_all`). A per-commit workflow therefore loses the cargo target directory and any Bifrost cache inside the baseline on every refresh.
  Evidence: `src/baseline.rs` `materialize_and_metadata` only materializes when `baseline/Cargo.toml` is missing; `materialize_baseline` deletes the directory first.
- Observation: cargo-mutants by default runs only the tests of the package that contains the mutant ("By default, each mutant runs only the tests from the package that's being mutated", mutants.rs/workspaces.html). `--test-workspace=true` runs every workspace test. `--in-diff FILE` keeps only mutants that overlap changed regions. QS selects tests from every package that depends on the mutated crate, which is a stronger test set; the benchmark must report which cargo-mutants mode it compares against.

- Observation: Bifrost spells Rust fqns with dots (`tiny_a.tests.add_works`), while libtest names use `::`.
  Evidence: first `graph-probe` on the fixture mapped 0 of 12 tests; splitting on both separators mapped 12 of 12.
- Observation: the usage graph's global `complete` flag is false on any real crate because standard-library names (`fmt`, `clone`, `into`, `poll`, …) are unresolved. The only per-node signals are inside free-text messages: `unresolved_names=Some({...})` in `selected_reference_graph_incomplete`, and declaration ids in `unproven_reference_edges`. QS parses both (`reach::parse_unresolved_names`, `reach::parse_unproven_ids`). Bifrost requirement: expose both as structured fields of `UsageGraphResult`.
- Observation: Stage A reported every profile as corrupt because `find_llvm_tool` ran `rustc --print sysroot` in QS's own directory (pinned 1.97.1, LLVM 22) while the target built with the rustup default (1.99.1, LLVM 23, no `llvm-tools` component), and then fell back to whatever `llvm-profdata` was on PATH.
  Evidence: `RUST_LOG=cargo_quicksilver=debug cargo-quicksilver index` on the fixture: `batch profraw merge failed (8 files)` then `stage A produced no merged profdata`; `rustc -vV` in the fixture copy reports 1.99.1 / LLVM 23.1.1.
- Observation: the Bifrost workspace (3,464 Rust files, 29 crates, about 12,900 tests) is far too large as a development or benchmark subject. The owner stopped a probe on it.

- Observation: on `semver`, the usage graph gives no sound narrowing. Before the fix, 63 of 110 mutants had zero selected tests and looked complete, including `Display::fmt`, `PartialEq::eq`, `Ord::cmp`, `Drop::drop`, `Deref::deref`, `Clone::clone`, and `Comparator::matches`, which the tests call. Causes: Bifrost reports `unresolved_names=None` (some unresolved reference has no readable name, so its contract says abstain on the whole pass), and implicit calls (operators, formatting, drop, auto-deref, `#[derive]` output) produce no reference site and so no edge.
  Evidence: `/mnt/optane/tmp/probe-semver-warm.out` (before) and `/mnt/optane/tmp/probe-semver-fixed.out` (after: `0 complete`).
- Observation: the usage graph is not persisted. Each process pays `usage_graph::resolve_rust` again: about 21 s in a debug build on `semver` with `BIFROST_TIMING=1`, against 2.3 s for the whole analyzer build.
  Evidence: aggregated `[bifrost-timing]` scopes in `/mnt/optane/tmp/semver-timing.err`.

## Decision Log

- Decision: use `dtolnay/semver` (one crate, no dependencies, about 2,200 lines of source, unit and integration tests, 712 commits) as the development and benchmark subject, cloned at `/nfs/morannon/nvme/repo-clones/dtolnay__semver`. Test repositories go under `/nfs/morannon/nvme/repo-clones/<owner>__<repo>`.
  Rationale: owner instruction after the Bifrost-workspace probe; small enough that cargo-mutants can run exhaustively in minutes and a per-commit replay is cheap.
  Date/Author: 2026-10-21, owner.

- Decision: depend on `brokk-bifrost-analysis = "=0.13.0"` in `Cargo.toml` and override it locally with an uncommitted `.cargo/config.toml` `[patch.crates-io]` entry pointing at `/home/jonathan/Projects/bifrost/crates/bifrost-analysis`, until 0.14.0 is published; then pin `=0.14.0` and drop the override.
  Rationale: the owner asked to build against `../bifrost` until 0.14.0 ships later on 2026-10-21. A path dependency written into `Cargo.toml` cannot resolve from QS's agent worktrees, which live three directories below the main checkout. The patch keeps the committed manifest resolvable from crates.io.
  Date/Author: 2026-10-21, agent.
- Decision: use `searchtools::usage_graph` with `include_tests: true, paths: None, depth: 1` once per source state, and walk it backwards in QS, instead of issuing one reverse query per mutated function.
  Rationale: the reverse closure of low-level functions in `brokk-bifrost-core` is likely to cover a large part of the workspace. One graph build plus local breadth-first search costs one Bifrost operation; per-function `scan_usages` or `CallRelationService::incoming` calls would repeat the shared part of every closure. The graph reports `complete`, `truncated_symbols` (hot symbols whose inbound edges were omitted), and `incomplete_reasons`, which are the signals the fallback needs.
  Date/Author: 2026-10-21, agent.
- Decision: keep the Bifrost cache in its default location, `<baseline>/.bifrost/cache/`, and make baseline refresh incremental instead of exporting `BIFROST_CACHE_DIR`.
  Rationale: the baseline has its own copied `.git` directory, so Bifrost treats it as a primary repository and caches beside it, isolated from the user's checkouts. Setting `BIFROST_CACHE_DIR` in the QS process would be inherited by the Bifrost test suites that QS runs as the mutation target and would redirect their caches.
  Date/Author: 2026-10-21, agent.
- Decision: static selection over-approximates on purpose. Enable Bifrost's concrete-override dispatch expansion (`AnalyzerConfig.dispatch_hierarchy_expansion = DispatchHierarchyExpansion::CONCRETE_OVERRIDES`) for the selection analyzer, and treat any mutant whose reverse walk meets an incomplete or truncated node as "fallback": select every test in the packages that depend on the mutant's package.
  Rationale: an extra selected test costs only test time and, with fail-fast, almost nothing for a caught mutant. A missing test can turn a Caught mutant into a false Missed, which is the error QS exists to avoid.
  Date/Author: 2026-10-21, agent.

## Outcomes & Retrospective

Not started.

## Context and Orientation

QS is a single Rust package (`cargo-quicksilver`, edition 2021, toolchain 1.97.1 from `rust-toolchain.toml`). The working pipeline for `estimate` and `run` is in `src/engine.rs` `prepare`: materialize a baseline copy of the target workspace under the scratch session (`src/scratch.rs`, `src/baseline.rs`), run the green suite with cargo-nextest to get test names and durations (`src/baseline.rs` `run_green_suite`), discover mutants (`src/discover.rs` `discover`), build or load the coverage index (`src/index.rs` `build_or_load`, Stage A), attribute tests to lines for the mutants in scope (`src/index.rs` `stage_b_for_mutants`, Stage B), plan (`src/plan.rs`), and execute with the schema engine (`src/exec/schema.rs` `execute`). A mutant in QS replaces a whole function body with a fixed value (`src/discover.rs` `ReturnKind::replacements`). A test is identified by `TestInfo` (`src/types.rs`): `binary_id` (nextest's id for the test binary, for example `brokk-bifrost-core` for the library unit tests or `brokk-bifrost::suite_issues` for `tests/suite_issues.rs`), `name` (the libtest path inside that binary, for example `analyzer::gitblob::tests::cache_key_stable`), `package`, and the binary path.

Bifrost is at `/home/jonathan/Projects/bifrost` (workspace version 0.13.0; edition 2024; toolchain 1.97.1). The relevant entry points, all in `crates/bifrost-analysis/src`:

`analyzer/workspace.rs` `WorkspaceAnalyzer::build_persisted(project, config)` and `build_persisted_for_languages` open the persistent store. The store file is `<primary repo root>/.bifrost/cache/bifrost_cache.v<N>.db` (`crates/bifrost-core/src/gitblob.rs` `cache_dir_path`). Facts are keyed by git blob id. A fresh process after an edit re-derives only the Rust crates reached by changed paths (`analyzer/store/rust_crates.rs`, test `a_fresh_process_after_a_body_edit_reconciles_only_the_edited_crate`). A `Cargo.toml` or `Cargo.lock` change falls back to a full pass. `BIFROST_TIMING=1` prints reconcile notes. `build_ephemeral` was renamed `build_ephemeral_footgun`.

`searchtools/scan_usages.rs` `usage_graph(analyzer, UsageGraphParams { include_tests, paths, depth })` returns `UsageGraphResult { complete, nodes, edges, truncated_symbols, incomplete_reasons }`. A node is a class or function with a stable `id`, `fqn`, `path`, and `start_line`. An edge is caller → callee with the list of reference sites. With `paths: None` and `depth: 1` it is the whole workspace graph.

`diff_analysis.rs` `analyze_diff_at_root(root, params, options, cancellation)` reports edited and introduced symbols; `AnalyzeDiffParams` gained `call_edges: DiffCallEdges` (use `DiffCallEdges::Omit`).

Test detection: Bifrost marks Rust code as test context when an attribute's last path segment is `test` (`#[test]`, `#[tokio::test]`), for `#[rstest]`, for `cfg(test)` regions, for files that are only reachable through test modules, and for paths with a `tests`/`test` segment (`crates/bifrost-rust/src/test_detection.rs`, `analyzer::test_paths::is_test_like_path`, `IAnalyzer::in_test_region`). Test context includes helpers, not only runnable tests, so QS must match reached test-context functions against the nextest test list to decide which are runnable tests.

Dispatch: calls through a bodiless trait method are always expanded to workspace implementations; expansion of concrete (default-bodied) methods to their overrides is off unless `DispatchHierarchyExpansion::CONCRETE_OVERRIDES` is set (`crates/bifrost-core/src/analyzer/config.rs`).

EC2 conventions on this account (from `/home/jonathan/Projects/bifrost5-python-hunt/ec2/` and `/home/jonathan/Projects/bifrost/.agents/records/tract-warm-latency/materialize-ec2/`): AWS profile `default`, region us-east-1, an idle on-demand `c7i.16xlarge` (64 vCPU, 128 GiB) as the reference machine, Ubuntu 24.04, 500 GB gp3 root, SSH from the operator's IP only with a fresh timestamped key, user-data `shutdown -h +720` with terminate-on-shutdown as a cost cap, resources tagged `Owner=jonathan` and `Project=<name>`, code shipped with rsync, and teardown that deletes the instance, volume, security group, key pair, and local key file with before/after describe output kept as proof. The `bifrost-ci` profile is the CI account and must not be used.

## Plan of Work

Milestone 1 moves the dependency and makes discovery persistent. In `src/discover.rs`, replace `WorkspaceAnalyzer::build_ephemeral` with `WorkspaceAnalyzer::build_persisted_for_languages` restricted to Rust, pass `call_edges: DiffCallEdges::Omit` and a default `CancellationToken` to `analyze_diff_at_root`, and time the build. Acceptance is the existing test suite passing and a measured cold/warm pair for discovery on a baseline copy of the Bifrost workspace.

Milestone 2 adds `src/reach.rs`. It builds the usage graph from the same persisted analyzer, maps each mutant to the graph node whose `path` matches and whose range contains the mutant's function (fall back to `fqn`), builds reverse adjacency, and walks backwards from each mutated function. Every reached node in test context is checked against the nextest test list; matching uses the test function's file and its module path relative to the test binary's crate root, with a suffix match on the libtest name as the fallback, and an ambiguous match selects all candidates. A walk that touches a truncated symbol, a node from an incomplete file, or a graph with `complete == false` marks that mutant as fallback. A hidden `graph-probe` subcommand prints graph size, completeness, timings, and the selection for a sample. The tiny fixture under `tests/fixtures/tiny/` gains cases that dynamic coverage selects and a naive static walk could miss: a call inside `assert_eq!`, a call through `&dyn Trait`, a function passed by reference, a default trait method overridden by an implementation, and an integration test under `tests/`.

Milestone 3 adds `--test-selection {coverage,static}` (default `coverage`) to the common options. With `static`, `prepare` skips `index::build_or_load` and `stage_b_for_mutants`, builds a `CoverageIndex`-shaped selection from `reach.rs` so that `src/plan.rs` and the executor are unchanged, files mutants that no test reaches (with a complete walk) as NotCovered, and records the selection mode and fallback counts in the JSON report. `materialize_and_metadata` gains an in-place sync: when the baseline exists, copy files whose content differs from the repository, delete files that vanished from the repository, refresh `.git`, and never touch `target/` or `.bifrost/cache/`.

Milestone 4 runs `audit-selection`-style comparison on a `brokk-bifrost-core` sample: for each covered mutant, the dynamic Stage B test set must be a subset of the static set. Report violations with the test and mutant, and the ratio of static to dynamic selection size. Violations are either fixed in QS (mapping bugs) or recorded as Bifrost requirements.

Milestone 5 runs the benchmark on EC2 and writes the results into this plan and a record under `.agents/docs/`.

## Concrete Steps

Working directory: the QS checkout (for agents, the worktree root). Always `unset RUSTUP_TOOLCHAIN`.

Local override, created once per checkout and never committed:

    mkdir -p .cargo
    cat > .cargo/config.toml <<'EOF'
    [patch.crates-io]
    brokk-bifrost-analysis = { path = "/home/jonathan/Projects/bifrost/crates/bifrost-analysis" }
    EOF

Build and test:

    cargo build
    cargo test --lib --test schema --test engine

Tests must run outside the agent sandbox (the sandbox aborts `wait-timeout` with SIGABRT).

## Validation and Acceptance

Milestone 1: `cargo test --lib --test schema --test engine` passes with the same counts as before the change (82 lib, 3 schema, 4 engine as of 2026-08-30, plus any new tests), and a second discovery on an unchanged Bifrost baseline is faster than the first.

Milestone 2: on the fixture, every test that dynamic coverage attributes to a mutant is in that mutant's static selection, and `graph-probe` on the Bifrost workspace prints the graph build time cold and warm and the completeness counts.

Milestone 3: `run --test-selection static` on the fixture produces the same verdicts as `run` with coverage selection and performs no instrumented build (no `baseline-cov` directory is created).

Milestone 4 and 5: see the milestone text; the numbers go into `Outcomes & Retrospective`.

## Idempotence and Recovery

The persisted Bifrost store can be deleted at any time (`rm -rf <baseline>/.bifrost/cache`); the next run rebuilds it cold. Deleting the QS scratch session forces a full rebuild of everything. EC2 resources must be torn down by their recorded ids; if a session dies, the 12-hour shutdown cap terminates the instance, and the security group and key pair are removed by the teardown script.

## Artifacts and Notes

None yet.

## Interfaces and Dependencies

In `src/reach.rs`, define:

    pub struct StaticSelection {
        pub tests: Vec<TestInfo>,
        pub complete: bool,
        pub reached_test_fns: usize,
        pub fallback_reason: Option<String>,
    }

    pub struct ReachIndex { /* reverse adjacency over usage-graph node ids, node metadata, completeness */ }

    impl ReachIndex {
        pub fn build(analyzer: &dyn IAnalyzer, tests: &[TestInfo], meta: &cargo_metadata::Metadata) -> anyhow::Result<Self>;
        pub fn select(&self, mutant: &Mutant) -> StaticSelection;
    }

Dependency: `brokk-bifrost-analysis` (0.13.0 source now, `=0.14.0` when published).

## Revision note

2026-10-21: Created after the owner asked to integrate incremental Bifrost into QS, verify it works, and benchmark against cargo-mutants on EC2.
