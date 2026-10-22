# Make sites selection see child processes, and cut QS's per-commit fixed cost

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds. Maintain it in accordance with `.agents/PLANS.md` at the repository root.

The prior plan `.agents/plans/incremental-bifrost-selection-and-ec2-benchmark.md` added `run --test-selection sites` and measured it against cargo-mutants on EC2. This plan fixes the correctness gap that measurement found and then reduces the fixed cost QS pays on every commit. It does not use Bifrost's usage graph for test selection: on 2026-10-21 the owner decided to stop that work (see the Decision Log).

## Purpose / Big Picture

cargo-quicksilver (QS) is a mutation-testing tool for Rust. A mutant is a copy of one function whose body is replaced by a fixed value, for example `0` or `Default::default()`. A mutant is Caught when at least one test fails with it active, Missed when every test still passes, and NotCovered when no test executes the function at all. cargo-mutants is the established tool QS is compared against.

QS compiles each crate once with every mutant built in. Each mutated function starts with a check, `__qs_rt::active(site)`, that reads the environment variable `QS_MUTANT` and, when it names this function's site, returns the replacement value. This one-compile design is called the schema engine, and the compiled program is the schema binary. In sites mode (`--test-selection sites`), QS runs every candidate test once under the schema binary with `QS_RECORD_SITES` set, and the runtime writes down which sites each test entered. A mutant's tests are then exactly the tests that entered its site.

On clap v4.6.7, 11 mutants that cargo-mutants catches are NotCovered (7) or Missed (4) under QS. Examples are `Command::get_matches` and `Error::exit_code`. clap's `tests/examples.rs` and `tests/ui.rs` use the `trycmd` library, which runs `cargo build --examples` while the test runs and then executes the example programs as child processes. QS runs each test with its working directory inside the baseline copy of the repository, which holds the original source. So the child `cargo build` compiles the examples from the original source. Those child programs cannot record sites and never see the mutant.

After this plan, a test that builds and runs programs from the workspace sees the same source as the schema binary, so its child processes record sites and see the active mutant. On clap, every mutant that cargo-mutants catches is Caught by QS too, or the plan explains why not.

The second half of the plan reduces what QS pays per commit before it runs any mutant. On itertools, a release QS spent 18 s per commit, against 11 to 27 s for cargo-mutants on the same commits. Most of that is fixed cost that sites mode does not need.

## Progress

- [x] (2026-10-22 03:30Z) Listed the clap disagreements from the saved EC2 data (`/mnt/optane/quicksilver/ec2/20261021T145959Z/final/bench/clap2/qs-sites.json` against `clap3/cm-fn-ws/mutants.out/outcomes.json`). Besides the 11 selection misses there are 4 NotCovered where cargo-mutants timed out, 2 Errored (stack overflows), and 2 Unviable (`Parser::is_new_arg`).
- [x] (2026-10-22 03:35Z) Ruled out doctests: cargo-mutants ran `cargo nextest run --workspace`, which runs no doctests (its `debug.log`).
- [x] (2026-10-22 03:45Z) Reproduced locally on clap v4.6.7 (`13f2db50`) restricted to `clap_builder/src/error/mod.rs` and `clap_builder/src/builder/command.rs`: `get_matches`, `styles`, `next_line_help` and both `exit_code` mutants NotCovered, `is_next_line_help_set -> false` Missed, as on EC2.
- [x] (2026-10-22 04:05Z) Milestone 1: each batch's mutants now run in the worker tree while it still holds the schema source; every test process gets `CARGO_TARGET_DIR=<tree>/target`. New fixture `tests/fixtures/child` and test `a_child_process_built_from_the_workspace_sees_the_mutant` (tests/engine.rs): both `greeting` mutants NotCovered before the change, Caught after. On the two clap files, 7 verdicts changed, all to Caught by `example_tests`: the 6 misses from the EC2 list in those files plus `Error::print` (was NotCovered). `cargo test --lib --test engine --test schema` passes.
- [x] (2026-10-22 04:10Z) The 4 NotCovered mutants that cargo-mutants reported as timeouts (`RawOccurrenceValues::next_back` x2, `Man::get_filename` x2): applied each by hand to a clean clap copy and ran `cargo nextest run --workspace`; all 1,334 tests pass within seconds. The cargo-mutants timeouts came from load on the VM, so QS's NotCovered stands.
- [x] (2026-10-22 03:52Z) Milestone 2: the 2 Errored (`RangedI64ValueParser::from`, `RangedU64ValueParser::from`, stack overflows) are now Caught by the crash policy of commit `bd6c821`, which the EC2 binary predated. The 2 Unviable `Parser::is_new_arg` mutants came from a prologue inserted before an inner attribute; fixed in `54aebd4`, both now Caught. The 5 value-parser misses (`RangedI64ValueParser::range`, `format_bounds` x2, `MapValueParser::possible_values`, `u16::value_parser`) are Caught by `example_tests` after Milestone 1.
- [x] (2026-10-22 04:17Z) Found and fixed a worker-isolation bug while checking a full clap run (commit `5e09c50`, see Surprises): copied `target` symlinks made the baseline and all workers share one target directory.
- [x] (2026-10-22 04:17Z) Milestone 1 and 2 accepted on the full clap workspace (local, debug QS, scratch `/mnt/optane/quicksilver/local/clap-scratch2`): against cargo-mutants' EC2 verdicts, 792 mutants are caught by both (777 on EC2) and none is caught by cargo-mutants and not by QS (11 on EC2). QS totals: 944 caught (18 by crash), 50 missed, 523 not covered, 379 unviable, 0 errored (17 on EC2), 2 timeout.
- [x] (2026-10-22 04:00Z) Milestone 3, step 1 (commit `85ef3f9`): preflight measured once per scratch session, keyed on manifests, build scripts, Cargo.lock and the cargo config; sites mode reuses earlier test durations and skips the baseline build and suite.
- [x] (2026-10-22 04:14Z) Milestone 3, step 2 (commit `abd1c63`): `--in-diff` discovery skips declarations outside the changed files before any per-function query (8.2 s to 1.4 s on itertools, debug).
- [x] (2026-10-22 04:20Z) Milestone 3, step 3 (commit `3f34cb9`): clone backends keep modification times, so a fresh worker's target directory stays fresh (36 of 61 itertools units rebuilt without this).
- [x] (2026-10-22 04:25Z) Milestone 3, step 4: in `run` with sites or static selection, `cargo nextest list` runs on a thread while Bifrost opens and discovery runs. On itertools (debug), listing used to end 2.0 s after discovery and now ends with it; per-commit totals 7.8 to 8.9 s on a host with load average 14 to 21 from other work, so differences under about 2 s are noise.
- [x] (2026-10-22 04:30Z) Recording runs retry a timed-out test once with 4x the budget. Since tests start in the worker, the first run of a test that runs `cargo` itself (clap's `example_tests`) compiles programs there, and with a symlinked target the worker's `target` now starts empty; a timeout used to drop the test from every selection without notice. Test: `a_recording_timeout_gets_one_longer_retry`.
- [x] (2026-10-22 05:45Z) Milestone 3, step 5: under `--in-diff` (coverage and sites selection), discovery no longer opens the whole workspace. It takes the changed files and lines from `git diff -U0`, builds `WorkspaceAnalyzer::build_scoped_persisted` over a `FileSetProject` of only the changed `.rs` files (the owner confirmed this is Bifrost's intended way), and selects functions whose line range overlaps a changed line. Bifrost's diff analysis remains the fallback when the diff's target is not what is checked out. Static selection still opens everything. On 10 real itertools commits the mutants are identical to full discovery (`scoped_diff_discovery_on_a_real_repo`, ignored test run by hand). On six commits whose content the store had never seen, a per-commit run took 5.8 to 13.6 s (debug), against 24 to 51 s with Bifrost's diff analysis and 25 to 45 s with the full open.
- [ ] Re-measure per-commit cost on EC2 (no mbx there) and compare with cargo-mutants; requires the owner's go-ahead for EC2 spending.

## Surprises & Discoveries

- Observation: QS starts tests in the baseline copy, not in the tree the schema binary was built from.
  Evidence: `src/exec/test.rs` `resolve_tests` sets `cwd` to `baseline_root.join(cwd_rel)`, and every caller in `src/exec/schema.rs` passes `paths.baseline`.
- Observation: by the time any mutant of a crate runs, the worker tree no longer holds the schema source.
  Evidence: `src/exec/schema.rs` `run_crate` calls `restore_files(worker, &snap)` at the end of every batch and returns all of the crate's mutants only afterwards; the build thread in `execute` then sends them to the test threads.
- Observation: snapbox (used by trycmd) runs `cargo build --examples` with no working directory and no target directory of its own, so cargo builds the workspace that contains the test's working directory, into that workspace's default target directory.
  Evidence: `snapbox-1.2.2/src/cmd.rs` `compile_examples` calls `escargot::CargoBuild::new().current_target().current_release().examples().args(args).exec()`.

- Observation: a function body that starts with an inner attribute (clap's `Parser::is_new_arg` begins with `#![allow(clippy::needless_bool)]`) cannot take a prologue right after its `{`, so the schema build marked both of its mutants Unviable. cargo-mutants replaces the whole body and is not affected.
  Evidence: QS's EC2 report gives `rustc diagnostic (round 1): site 0xd468e591 (clap_builder.parser.parser.Parser.is_new_arg)`; `src/schema.rs` `apply_to_source` inserted at `body_start_byte + 1`.
- Observation: cargo-mutants' clap timeouts are not evidence that a test reaches the function. It timed out on `generate_to::generate`, which no test calls, and on 4 mutants that pass every test in seconds on an idle host. The EC2 run had a load average near 90 on 64 cores, and each timeout ended just after the 42 s limit.
  Evidence: phase durations in `clap3/cm-fn-ws/mutants.out/outcomes.json`; `/mnt/optane/tmp/cm-manual.log`.
- Observation: holding the worker until a batch's mutants finish costs little on clap. On the two-file subset, recording went from 5.2 s to 8.1 s and the mutant phase from about 5 s to about 9 s, because `example_tests` (which runs `cargo` and every example) is now selected for more mutants. The host was shared with other work, so these are rough.

- Observation: a full local clap run (all 5 crates, 15 build workers) recorded "0 of 911 sites entered" for clap_builder, so 714 mutants that cargo-mutants catches became NotCovered. The other 4 crates recorded normally, as did every single-crate run. Afterwards the baseline's `target/quicksilver/deps` held clap_lex and clap_builder libraries built from schema source, and their `.d` files named worker `w02`'s target directory. Cause, confirmed: on this host mbx had made the baseline's `target` a symlink to its managed directory `/mnt/optane/mbx-targets/v1/<digest>`, and `ensure_worker` copied that symlink into every worker, so all six trees built into one directory and concurrent schema builds overwrote each other. `readlink -f <tree>/target` gave the same directory for the baseline and every worker. An earlier hypothesis, that mbx's rustc cache served one copy's output to another, was wrong: reading the mbx source (`/home/jonathan/Projects/mr-boxington`) shows its key hashes every source file's contents and replaces absolute roots with placeholders. Fixed in `5e09c50` by giving each worker an empty private `target` when the copy brings a symlink. The EC2 runs had no mbx and were not affected.
  Evidence: `/mnt/optane/quicksilver/local/clap-full.log` (broken) and `clap-full2.log` (fixed: 676 of 911 sites entered).
- Observation: the copy backends stamped every cloned file with the copy time, so cargo saw a fresh worker as dirty. Copying an itertools baseline with `cp -r` and building with no source change rebuilt 36 of 61 units, build scripts of registry crates included; with `cp -a`, 0 units, 0.1 s.
  Evidence: `/mnt/optane/quicksilver/local/exp/{fresh,kept}.log`.
- Observation: per-commit cost on itertools, local, debug QS, after steps 1 to 3 (commit `86df43484e`, Bifrost store already holding the commit): 8.0 s in total, made of 0.4 s baseline sync, 0.7 s Bifrost open, 1.8 s discovery, 2.0 s test listing (which now builds the tests), 1.4 s worker copy, 0.4 s schema build, 1.3 s gates and recording. Before step 1 the same commit took 35.7 s. For a commit the Bifrost store has not seen, opening it took 10 to 25 s in a debug build (1.3 to 1.5 s in release on EC2), because Bifrost reconciles a whole crate when one file changes. That is now the largest per-commit cost in a debug build, and it is on the Bifrost side.
  Evidence: `/mnt/optane/quicksilver/local/replay-{cuts1,disc2,mtime1}/`. This host's mbx cache had seen these commits in earlier replays, so compile times are lower than on a host without it.
- Observation: discovery from the diff result alone (skipping the workspace open under `--in-diff`) is not safe with today's Bifrost API. Its per-commit symbol snapshot is a map keyed by fully qualified name (`symbol_snapshot_map` in `bifrost-tools/src/diff_analysis.rs`), so two functions with the same name, such as `fmt` in `impl Display` and `impl Debug`, collapse into one entry, and a diff that edits one can report the other's lines. A Bifrost call that returns the declarations of a few files, with byte ranges, from the shared cache would remove the workspace open from per-commit runs.

- Observation: for a commit the store has not seen, the whole-workspace open spent 11.9 s of SQL rebuilding crate-wide resolution tables (`cr_authority_blockers` alone 4.3 s) and rewrote 57,747 rows of the crate's resolution summary. Bifrost's design makes the crate the unit of name resolution, and its per-unit summary work for Rust (its plan's Milestone 5) is open. A scoped build over one changed file ran 3.4 s of SQL, most of it a 1.1 s statistics refresh (`ANALYZE`, run whenever the stored file count changes) and per-file fact inserts; the rest of its time is parsing and fact extraction of the changed file, which grows with file size (9.4 s for itertools' 4,800-line `src/lib.rs` in a debug build).
  Evidence: `/mnt/optane/quicksilver/local/probe-sql.json` (full open), `probe-sql2.json` (scoped).
- Observation: Bifrost's diff analysis cost 3 to 35 s per store-new commit in a debug build, because it builds analyzers for both commits. Selecting changed functions by `git diff -U0` line overlap costs 0.02 s and gave identical mutants on the 10 real itertools replay commits.

## Decision Log

- Decision: stop using Bifrost's usage graph for test selection; keep it behind the `static-selection` cargo feature.
  Rationale: on itertools the graph gives no narrowing (`withheld_references_complete=false`, caused by 70 references inside `macro_rules!` bodies that Bifrost cannot name), its candidate edges are too broad, and 189 of 445 tests have no graph node. Even where it is sound (semver) it cost more per run than the ~1 s recording run it replaces, because the graph is rebuilt every process. The analysis is on BrokkAi/bifrost-dev#6229. Sites mode is the measured selector.
  Date/Author: 2026-10-21, owner.

- Decision: run each batch's mutants in the worker that built them and keep the worker's schema source in place until all of them have a verdict, instead of copying the schema source tree per batch.
  Rationale: the scratch disk here has no block cloning, so a per-batch copy of the tree and its `target/` would be a real multi-gigabyte copy. Waiting costs only overlap between one worker's next build and its current tests; other build threads still build in parallel, and one crate is one batch by default.
  Date/Author: 2026-10-22, agent.
- Decision: give every test process `CARGO_TARGET_DIR=<tree>/target`.
  Rationale: a child `cargo build` must use the target directory the schema build used, not one named in the caller's environment or in the subject's `.cargo/config.toml`. For a tree with neither, this is the directory cargo would pick anyway.
  Date/Author: 2026-10-22, agent.
- Decision: triage and second-order runs (`--triage`, `--second-order-pairs`) still start tests in the baseline after all crates finish, because the workers have moved on by then. Their child processes see the original source. These modes are off by default; the limitation is recorded here and not fixed.
  Date/Author: 2026-10-22, agent.

- Decision: select `--in-diff` functions by line overlap with `git diff -U0`, as cargo-mutants does, instead of Bifrost's edited and introduced symbols.
  Rationale: it removes a second Bifrost analysis per commit. Two cases now select more than before: a function whose only change is a comment or whitespace, and a function moved to another file. Both are cheap to test and match cargo-mutants. The line numbers must describe the analyzed files, so when an explicit diff target differs from what is checked out (`git diff --quiet <target> -- <files>` fails), QS falls back to Bifrost's diff analysis.
  Date/Author: 2026-10-22, agent.

## Outcomes & Retrospective

2026-10-22. Sites selection now agrees with cargo-mutants on clap: with the final build (commit `56a55ef`, debug, local), 792 mutants are caught by both, none is caught by cargo-mutants and missed or left uncovered by QS (11 on EC2), and the remaining differences are 397 NotCovered against Missed (both say no test detects the mutant) and 7 cargo-mutants timeouts that came from load on the VM. A warm whole-workspace clap run took 132 s on this shared host.

The clap misses had one cause: tests that build and run workspace programs as child processes (trycmd) saw the original source. The fix runs each batch's tests in the worker that holds its schema source. Checking it on the full workspace exposed two older bugs that EC2 could not show: worker copies shared one target directory through a copied symlink, and copies reset modification times so cargo rebuilt most of the workspace in every fresh worker.

Per-commit cost on itertools (debug QS) went from 35.7 s to about 8 s for a commit the Bifrost store already holds. What remains, largest first: opening Bifrost on a commit it has not seen (10 to 25 s in a debug build, 1.3 to 1.5 s in release), which Bifrost would have to make incremental per file or replace with a per-file declaration query; discovery (about 1.8 s); worker copy (1.4 s here, real bytes on ZFS without block cloning; 0.07 s on EC2); gates and recording (about 1.2 s); and the schema build itself.

Not done: a fresh EC2 measurement against cargo-mutants with these changes. The owner stopped EC2 spending on 2026-10-21, so it waits for a go-ahead. Lesson: run the whole workspace, not only the files that showed the bug, before accepting a fix; the single-crate checks passed while the multi-crate run was broken.

## Context and Orientation

All paths are relative to the repository root. QS is a single Rust package (`cargo-quicksilver`). The pipeline for `cargo quicksilver run` is `src/engine.rs` `prepare` followed by `src/exec/schema.rs` `execute`.

`prepare` copies the target repository into a scratch directory called the baseline (`src/baseline.rs`, `src/scratch.rs`), builds its tests and runs the whole suite once with cargo-nextest to learn test names and durations, discovers mutants with Bifrost (`src/discover.rs`), lists tests, and plans which tests are candidates for each mutant (`src/plan.rs`). In sites mode the candidates are all tests in packages that depend on the mutated package (`PackageSelector`).

`execute` works per crate. Each build thread owns a worker, which is a full copy of the baseline at `<scratch>/workers/<slot>` (`src/scratch.rs` `ensure_worker`). For each crate, `run_crate` writes the schema source into the worker (`apply_schema_to_tree`), builds the tests with `cargo build --tests --profile quicksilver` into `<worker>/target` (`src/cargo_cmd.rs` `build_tests_captured`), copies the test binaries into a run directory (`stage_binaries`), runs a null gate (a few tests with no mutant active must pass) and an identity gate, records site hits (`record_sites`, sites mode only), narrows each mutant's tests to those that entered its site, restores the original source, and returns the mutants. Separate test threads run each mutant's tests from the run directory with `QS_MUTANT` set (`src/exec/test.rs` `run_mutant_tests_observed`).

`src/exec/test.rs` `resolve_tests` maps planned tests to staged binaries and chooses each test's working directory: the directory nextest would use, which is the package directory, taken relative to a root that callers pass in. Today every caller passes the baseline.

The test subject for this plan is clap v4.6.7, commit `13f2db50`, cloned at `/nfs/morannon/nvme/repo-clones/clap-rs__clap` and copied for local runs to `/mnt/optane/quicksilver/local/clap`. Local runs use a debug QS build and the copy clone backend:

    cd <QS repository root>
    cargo build
    W=/mnt/optane/quicksilver/local
    XDG_RUNTIME_DIR=/run/user/$(id -u) systemd-run --user --scope -q -p MemoryMax=48G -- \
      $PWD/target/debug/cargo-quicksilver --manifest-path $W/clap/Cargo.toml \
      --scratch-root $W/clap-scratch --clone-backend copy --allow-copy \
      --test-selection sites --file clap_builder/src/error/mod.rs \
      --file clap_builder/src/builder/command.rs --json $W/clap.json run

The memory limit matters: on EC2 a runaway clap mutant made the kernel's out-of-memory killer end unrelated processes.

`scripts/bench/compare_verdicts.py QS_REPORT.json CARGO_MUTANTS_OUTCOMES.json --list` matches QS and cargo-mutants mutants by file, function end line, and replacement, and lists verdict disagreements.

## Plan of Work

Milestone 1 makes tests run inside the worker that built their schema binary, while that worker still holds the schema source. In `src/exec/schema.rs`, `run_crate` will hand each batch's mutants to the test threads itself and wait until all of them have a verdict before it restores the source and moves on. The gates and the recording run will use the worker as the root for working directories instead of the baseline, and so will the mutant runs. Test processes will get `CARGO_TARGET_DIR=<worker>/target`, so that a child cargo build uses the worker's target directory even when the caller's environment names another one. The cost is that a worker cannot build its next crate while the current crate's mutants run; other build threads still build in parallel.

Milestone 2 deals with the other clap disagreements one at a time, with a local reproduction for each.

Milestone 3 reduces per-commit fixed cost in sites mode. Candidates, in the order of expected saving on itertools (release, 18 s per commit): skip the baseline build and suite in sites mode, because the recording run already times every candidate test under the schema binary; discover only the changed functions under `--in-diff`; and list tests from the schema-built binaries instead of a separate `nextest list`. Each is measured before and after.

## Concrete Steps

To be filled in as milestones proceed.

## Validation and Acceptance

Milestone 1 is accepted when, on clap v4.6.7 restricted to `clap_builder/src/error/mod.rs` and `clap_builder/src/builder/command.rs`, the mutants of `Error::exit_code` and `Command::get_matches` are Caught, where before the change they are NotCovered, and `cargo test` passes in the QS repository, including a new test that fails before the change. A full clap run then shows no mutant that cargo-mutants catches and QS files as NotCovered or Missed, or each remaining one is explained here.

## Idempotence and Recovery

Every QS run writes only under its `--scratch-root`. Deleting that directory resets all state. The clap copy under `/mnt/optane/quicksilver/local/clap` can be recreated with `git clone` from the NFS clone and `git checkout 13f2db50`.

## Artifacts and Notes

None yet.

## Interfaces and Dependencies

No new dependencies.
