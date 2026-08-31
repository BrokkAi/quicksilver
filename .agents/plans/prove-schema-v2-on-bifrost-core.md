# Prove schema (v2) mutation testing and build higher-order evidence on bifrost-core

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

Maintain this document in accordance with `.agents/PLANS.md`. A novice with only the working tree, `.agents/PLANS.md`, and this file should be able to finish the proof or see why it is not yet finished.

This plan began retroactively: schema execution already existed in the tree (commit `9071b2e` and ancestors), and the first job was to make its verdicts trustworthy enough to delete the older one-compile-per-mutant path. That proof is complete. The active continuation adds three evidence layers in this order: survivor triage, pooled second-order estimation, and a persistent coupling matrix. Do not restore or run the deleted v1 executor.

## Purpose / Big Picture

After the completed proof, a user can run `cargo-quicksilver estimate` or `run` on a scoped crate, get one compile for that crate instead of one compile per mutant, and trust its first-order verdicts. The continuation lets that user ask what a survivor means instead of receiving only a flat Missed inventory. Opt-in triage combines a survivor with nearby caught mutants and compares complete per-test kill vectors. Opt-in second-order estimation executes reproducible mutant pairs and reports the first-order kill probability implied by the observed pair kill rate using `p = 1 - sqrt(1 - h)`. Every actually observed mutant-by-test result is merged into a versioned matrix under the session index and, when `--json` is used, summarized in the report. These behaviors must work on the tiny fixture without Bifrost and must not query Bifrost's usage graph, so issue 2771 is not on their critical path.

## Progress

- [x] (2026-08-27) Implement schema transform, runtime module `__qs_rt`, isolation rebuilds, null-schema gate, identity-mutant gate, and `run --schema` in `src/schema.rs` and `src/exec/schema.rs` (landed in `8021310` and follow-ups).
- [x] (2026-08-27) Cut schema compile and null-gate cost to the mutated crate (`171fdaa`).
- [x] (2026-08-27) Finish a v1 estimate sample of 150 mutants on `brokk-bifrost-core` (N = 1720 in scope). Report: `/mnt/optane/quicksilver/bringup/estimate-core.json`. Counts: 51 caught, 44 missed, 7 not covered, 37 unviable, 3 errored, 8 timeout.
- [x] (2026-08-27) Diagnose v1 timeouts and errors: Stage B reload wiped test durations to 0.001 seconds so a 17-second green test died at a 5-second floor; SIGKILL from host pressure was labeled Errored. Fixed in `src/exec/test.rs`, `src/exec/process.rs`, `src/engine.rs`, `src/lib.rs`.
- [x] (2026-08-27) Wire `estimate --schema` and `--diff-against` so the same sample can run v2 and print disagreements (`src/cli.rs`, `src/lib.rs` `cmd_estimate`).
- [x] (2026-08-27) First v2 attempt: schema compiled; null-schema gate ran the union of selected tests (about 10,684 tests) serially; a 0.41-second test timed out at 5 seconds; all 143 covered mutants marked unviable.
- [x] (2026-08-27) Null-schema gate now smokes the cheapest 8 tests with a 30-second floor (`src/exec/schema.rs` `NULL_GATE_TESTS`, `cheapest_tests`, `null_schema_gate`).
- [x] (2026-08-27) Second v2 attempt: null-schema passed; identity-mutant gate ran thousands of tests for `hash_domain_bytes` and timed out; crate unviable again. Identity gate now uses the same cheapest-8 and 30-second floor.
- [x] (2026-08-27 20:29 CDT – 21:12 CDT, about 44 minutes) Third v2 attempt executed mutants. Report: `/mnt/optane/quicksilver/bringup/estimate-core-v2.json`. Counts: 58 caught, 8 missed, 7 not covered, 41 unviable, 13 errored, 23 timeout. Differential gate: 91 agree, 59 disagree. Exit is treated as failure (`Ok(2)` from `cmd_estimate` when diffs are non-empty).
- [x] Drive timeouts and infrastructure errors down so they are not the main disagreement with v1. Completed 2026-08-28: v2 timeouts 23 → 0 (pool floor parity + confirmation), error cluster eliminated (binary resolution fix); see the dated entries below.
- [x] (2026-08-27) Diagnose the 23 v2 timeouts without a rebuild: every timed-out test has a green duration of 0.04–0.97 seconds, so every one received exactly the 5-second floor budget. The gates were already raised to a 30-second floor for schema slowness (`src/exec/schema.rs` comment near `null_schema_gate`), but the mutant pool still passed plain `cfg.timeout_floor` (default 5000 ms, `src/cli.rs`). The first v2 attempt also timed out a 0.41-second test at 5 seconds with `QS_MUTANT` unset, so the slowdown is schema-binary overhead, not mutant-caused hangs.
- [x] (2026-08-27) Diagnose the disagreement structure: `run_mutant_tests_with_env` returns on the first Timeout or Error, so one poison test decides every mutant it covers (5 timeout rows share `persisted_build_reports_cache_open_failure`; all 12 error rows share the Scala fixture test). Stderr is discarded (`Stdio::null`), so the exit-1 cause is not recoverable from logs. The fixture test is a plain libtest `#[test]` (assertions would exit 101, not 1) and `__qs_rt` has no exit path, so the exit-1 source is still unknown.
- [x] (2026-08-27) Re-ran the v1 sanity sample with the fixed binary as `estimate-core-v1b.json`; the later healed-index `estimate-core-v1f.json` superseded it.
- [x] (2026-08-27) Implement schema-pool budget parity and a confirmation machine: pool floor `max(cfg.timeout_floor, 30 s)` and slowdown factor 3.0 on the 3× green budget; on first-pass Failed/Timeout/Error, retry once, then run the same test with no mutant active to attribute the failure to the mutant or to the schema binary; skip infrastructure-broken tests and continue instead of poisoning the mutant; capture stderr on confirmation runs. v1 and gate behavior unchanged. Commit `ea6e467`.
- [x] (2026-08-27) v1 baseline refreshed with the fixed binary: `estimate-core-v1b.json`, 54 caught / 44 missed / 7 not covered / 37 unviable / 3 errored / 5 timeout. Only 9 rows moved vs the stale baseline, all timeout-related. v1b's own noise: 5 floor timeouts on 0.46–0.68 s tests and 2 signal-death errors on `persisted_build_reports_cache_open_failure`.
- [x] (2026-08-28) Fourth v2 run (confirmation machinery, diff against v1b): `estimate-core-v2b.json`, 79 caught / 10 missed / 7 not covered / 41 unviable / 13 errored / **0 timeout**. Disagreements 59 → 43. All 23 old timeouts gone.
- [x] (2026-08-28) Root-caused the whole exit-1 error cluster from captured stderr (`unknown argument: --exact`): `resolve_binary` falls through to a substring `contains()` scan (the instrumented Stage A hashes in `binary_rel` never exist in plain worker builds) and picked the `bifrost_mcp_property_fuzzer` bin target — a non-harness binary — for every test whose needle it contains ("bifrost", "mcp_property_fuzzer"). `parse_executables` also offers non-test bin builds as candidates.
- [x] (2026-08-28) Root-caused the missed→caught class (27 rows): all 27 v1-Missed mutants have **empty Stage B per-test selections**; v1 built them, staged zero tests, ran nothing, and reported Missed vacuously (verified: hand-applied `gitblob.cache_repository_key → {String::new()}` in a v1 worker, the selected lib test exits 101; instrumented single-mutant re-runs show `resolve_binary` is never called; `analyzer.gitblob.rs` union coverage has 20 lines in the mutant span, Stage B attribution has none). v2's empty-selection fallback runs the whole staged batch manifest and found the real kills.
- [x] (2026-08-28) Root-caused thin Stage B attribution: `invert_tests` wipes and reuses a shared `index/profraw` directory; the cached attribution (hash `d801aecc`) was computed during the four-shard era when concurrent processes wiped each other's profraws mid-flight. 10 of 47 bifrost-core files in `lines` had <25% of their union lines attributed (gitblob.rs: 5 of ~100+). Healed by surgery: dropped all 47 bifrost-core files from `lines`, cleared `stage_b_mutant_ids`, backup at `index/coverage.json.bak-thin-stageb`; next run recomputes single-process.
- [x] (2026-08-28) Fix `resolve_binary` (exact hash-stripped base-name matching, no substring scan; `parse_executables` keeps only `profile.test` artifacts; empty staged test list becomes Errored, not vacuous Missed). Commit `f3af8bc`.
- [x] (2026-08-28) v1f baseline on the healed index (Stage B recompute: 12,868 tests × 43 files, ~47 minutes single-process; gitblob.rs attribution went from 5 lines to 884): `estimate-core-v1f.json` — 82 caught, 10 missed, 7 not covered, 37 unviable, 3 errored, 11 timeout. The 28 formerly vacuous Missed are now Caught under v1 itself, independently confirming v2b's catches. Zero executed mutants have empty selections. v1f's residual noise is fully classified: all 11 timeouts are 0.41–2.04 s-green tests at v1's 5-second floor (v1 has no confirmation pass); errors are 2 signal-deaths on `persisted_build_reports_cache_open_failure` and the stack-overflow abort in `edge_weight_only_merge_sums_distinct_file_line_sites` (a real bifrost test bug, aborts in every run).
- [x] (2026-08-28) v2c: `estimate --schema` on the healed index, `--diff-against` v1f (`estimate-core-v2c.json`) — 83 caught, 16 missed, 7 not covered, 41 unviable, 3 errored, 0 timeout, in about 29 minutes wall. 15 disagreements, all in the two allow-listed classes (see Decision Log): 4 `ref`-prefilter rows and 11 v1 floor-timeout rows. The 3 errored mutants agree on both engines.
- [x] (2026-08-28) Drive timeouts and infrastructure errors down: v2 timeouts went 23 → 0 across two runs; the error cluster is gone; the remaining 3 errors are real test-level problems shared with v1 (two `persisted_build_reports_cache_open_failure` signal flakes, one stack-overflow abort in `edge_weight_only_merge_sums_distinct_file_line_sites`).
- [x] (2026-08-28) Differential allow-list documented (Decision Log). Criterion One of Validation (literal empty diff, exit 0) is NOT met, so per that section the v1 execute path stays in the tree for now.
- [x] Get a differential pass, or a documented allow-list of expected disagreements, on the same `--sample 150 --sample-seed 1` set. Completed 2026-08-28: 15 disagreements, all allow-listed in the Decision Log (4 `ref`-prefilter, 11 v1 floor-timeouts). The 22 v1-Missed→v2-Caught shift was real — v1's Missed verdicts were vacuous (empty selections) and v1 itself confirms the catches on the healed index.
- [x] (2026-08-28) Delete the v1 execute path (`src/exec/mod.rs` `execute`, `engine::run_prepared`) and make schemata the only engine. Authorized by the user on 2026-08-28 after reviewing the v2c differential: the documented allow-list is accepted as the trust bar in place of Validation criterion One. `--schema` remains as a hidden accepted no-op flag.
- [x] (2026-08-30) Milestone 1, survivor triage. Added cached multi-site activation, complete confirmed observation vectors, deterministic nearby caught controls, five evidence classifications plus inconclusive, `run --triage`/`--triage-neighbors`, human and JSON reporting, and a tiny-workspace end-to-end proof.
- [x] (2026-08-30) Milestone 2, pooled second-order estimation. Added seeded disjoint pair sampling within a staged schema binary, complete confirmed pair vectors, `estimate --second-order-pairs N`, human/JSON reporting, boundary-tested inversion `1 - sqrt(1 - h)`, and an explicit independence-model caveat. The tiny end-to-end case completed and caught one requested pair.
- [x] (2026-08-30) Milestone 3, persistent coupling artifacts. Ordinary first-order prefixes and complete experimental vectors merge into versioned `index/coupling-v1.json`; source-key mismatches invalidate stale evidence; stable activation/test ids merge reruns; cross-process file locking prevents shard lost updates; only complete comparable equal-column vectors produce subsumption relations; summaries appear in human and JSON output.
- [x] (2026-08-30) Verification completed. `cargo test --offline --lib --test schema --test engine` passed 82 library, 4 engine, and 3 schema tests outside the sandbox; the focused evidence/JSON fixture passed in 65.54 seconds; clippy completed with the same 12 pre-existing style warnings and no warnings introduced by this work.

## Surprises & Discoveries

- Observation: The sandbox denies the signal-handler pipe operation used by `wait-timeout` while Stage B indexes fixture tests, aborting the process with `bad error on write fd: Operation not permitted`. The identical focused integration test passes outside the sandbox in 65.52 seconds.
  Evidence: sandbox run exited by SIGABRT in `wait_timeout::imp::sigchld_handler`; approved rerun reported `1 passed; 0 failed` including the new triage assertion.

- Observation: Durable coupling writes need process-level serialization, not only atomic rename. Multiple shards can load the same old artifact and atomically overwrite one another with individually valid but incomplete snapshots.
  Evidence: `src/coupling.rs::merge_and_save` now holds `flock(LOCK_EX)` across load, merge, and atomic save; this follows the earlier Stage B lost-update failure documented in this plan.

- Observation: Index test rows all stored `duration_secs = 0.001` on disk even when baseline JUnit had real times (for example `a_jar_indexed_member_assertion_concludes_only_when_the_jar_was_read` is 16.993 seconds green). `attach_timings` matched 12,868 of 12,868 pairs in memory, then `stage_b_for_mutants` reloaded `coverage.json` and threw the durations away. Evidence: Python compare of `/mnt/optane/quicksilver/065e17f14dc00ece/index/coverage.json` versus `baseline.json`; timeout list in `estimate-core.json` all hit that jar test or other 5–17 second tests.

- Observation: A 5-second floor times out any reverse-dependent integration test whose duration was missing. After attach, using 3 times the sum of every selected test would wait hundreds of seconds per test. Per-test budget `max(floor, 3 × this test)` is the right shape. Evidence: `src/exec/test.rs` `test_timeout` unit test `timeout_uses_this_test_not_the_selection_sum`.

- Observation: SIGKILL (`exited None`) during the four-way sample swamp was classified Errored. That is host pressure or our process-group kill, not a libtest assertion (libtest failures are exit 101). Evidence: three v1 errors on `analyze_diff_builds_one_scope_index_per_distinct_source` with message `exited None`; `classify_exit` now maps `libc::SIGKILL` to Timeout.

- Observation: Null-schema gate on a core-scoped sample is not a smoke test if it unions every selected test. That union was 10,684 tests, about 10,676 seconds of green wall time if run serially. Evidence: coverage invert over the 143 covered sample mutants; first v2 log line `Timeout value_flow_provider::provider_call_bindings_match_oracle_and_repeat_is_a_cache_hit` then `143 unviable`.

- Observation: Identity-mutant gate for `hash_domain_bytes` selected 9,480 covering tests. Same failure mode. Evidence: line map walk on `crates/bifrost-core/src/analyzer/canonical_hash.rs`; second v2 log `identity-mutant gate failed for brokk_bifrost_core.analyzer.canonical_hash.hash_domain_bytes`.

- Observation: Schema binaries are slower than per-mutant v1 binaries. Even after gates passed, 15 mutants that v1 Caught became v2 Timeout. A 0.41-second green test exceeded 5 seconds in the schema binary on the first attempt. Evidence: differential pairs `('caught', 'timeout'): 15` in the third v2 JSON; first-attempt timeout on `provider_call_bindings_match_oracle_and_repeat_is_a_cache_hit`.

- Observation: Twelve v2 errors share one test, `i1_silent_on_scala_auxiliary_constructor_fixture`, exit code 1 (not 101). That is a process abort or clap/tool failure, not an assertion catch. Evidence: tail of `/mnt/optane/quicksilver/bringup/v2.log` on the third run.

- Observation: Four v2 unviables are intentional: return kind `ref` is pre-filtered in `kind_is_schema_prefiltered`. The rest of the 41 unviables are rustc isolation diagnostics (`rustc diagnostic (round 1): site 0x…`). Evidence: message histogram on `estimate-core-v2.json`.

- Observation: Shared-machine contention made “clean” serial timings a fiction and also broke cache keys. `maybe_preflight` edits `baseline/` in place while another process hashes that tree, so `--skip-baseline` shards rebuilt Stage A. Evidence: shards 2 and 3 logged `instrumented build into …/baseline-cov` while shards 1 and 4 logged `reusing coverage index`.

- Observation: All 23 v2 timeouts are floor timeouts. Joining the timed-out test names against `baseline.json` durations: minimum 0.04 s, maximum 0.97 s green, so every budget was the 5-second floor, never the 3× term. No long test appears. Evidence: Python join of `estimate-core-v2.json` timeout rows with `baseline.json` `tests[].duration_secs`.

- Observation: The gate fix was not applied to the pool. `null_schema_gate` and `identity_gate` use `cfg.timeout_floor.max(30 s)` with a comment that 3× green is not enough for schema binaries; the pool call site passes plain `cfg.timeout_floor` (5 s default). Evidence: `src/exec/schema.rs` pool spawn loop versus gate functions.

- Observation: One poison test decides many mutants because `run_mutant_tests_with_env` returns on first Timeout or Error, and its stderr is discarded. Five timeout rows share `persisted_build_reports_cache_open_failure`; all 12 error rows share `i1_silent_on_scala_auxiliary_constructor_fixture`. That fixture test is a plain libtest `#[test]` (assertion failures exit 101, not 1) and `QS_RT_SRC` has no exit path, so exit 1 must come from the process environment; needs captured stderr to identify. Evidence: `src/exec/test.rs` match arms; `tests/mcp_property_fuzzer.rs` in bifrost; `src/schema.rs` lines 15–33.

- Observation: The exit-1 tests all ran a binary that is not a libtest harness. Captured stderr is `unknown argument: --exact`, exit 1 in 0.01 s, with and without a mutant active. Probing every executable in a schema worker found the printer: `bifrost_mcp_property_fuzzer`, a plain bin target. Its name contains both "bifrost" (needle for `bin/bifrost` unit tests like `policy_explain_exit_status_tests::*`) and "mcp_property_fuzzer" (needle for the fuzzer integration tests), so `resolve_binary`'s substring scan routed both families to it. The scan only runs because `binary_rel` hashes (e.g. `bifrost-5bf56d3c544872ed`) come from the instrumented Stage A build and never exist in plain worker builds. Evidence: `/mnt/optane/quicksilver/bringup/v2b.log` confirm lines; probe of `workers/v2b-w00/target/quicksilver/deps`.

- Observation: The wrong-binary failure mode is worse when the wrong pick is a libtest harness: `--exact <name>` matches zero tests and exits 0, a silent vacuous pass. Four fingerprints of `brokk_bifrost_core-<hash>` coexist in one worker (feature unification varies with the built package set), so hash-based paths can never be trusted across builds.

- Observation: v1's 44 Missed decompose as 9 real (agreeing with v2, selections present) + 35 vacuous (empty Stage B selections; nothing ran). Verified three ways: hand-applied the gitblob mutant in a v1 worker and the selected lib test exits 101; single-mutant instrumented re-runs (state-v1c/d/e) reproduce Missed with `resolve_binary` never called (so `planned.tests` is empty at build time); Python join of both reports against `coverage.json` shows the EMPTY/has-tests split matches the missed→caught / missed→missed split exactly.

- Observation: Stage B attribution loss is shard-era damage. `invert_tests` does `remove_dir_all(index/profraw)` on entry; four concurrent shard processes wiped each other's per-test profraws, so surviving attribution is a near-random subset (gitblob.rs kept only 5 lines attributed to 7 js-ts `tsconfig` tests; union coverage for the same file has the full span). 10 of 47 bifrost-core files in `lines` held <25% of their union lines. The union (Stage A) was unaffected, which is why NotCovered stayed correct at 7 while selections were wrong.

## Decision Log

- Decision: Do not run exhaustive v1. Keep v1 only as a 150-mutant sanity sample for `--diff-against`.
  Rationale: The user stated v1 is a sanity check and can be deleted as soon as v2 is proven. Exhaustive v1 on 1,720 mutants is compile-bound hours and is not the product.
  Date/Author: 2026-08-27 / Jonathan via bringup session.

- Decision: Insertion prologue after `{`, original body left in place, identity arm is empty (`N => {}`) so execution falls through.
  Rationale: Wrapping the body in a match changes drop order and non-lexical lifetimes on the unmutated path. Documented at the top of `src/schema.rs`.
  Date/Author: schema design (`8021310`).

- Decision: Only libtest exit 101 is Caught. Other nonzero exits are Errored. SIGKILL is Timeout.
  Rationale: Dynamic linker failures and host kills must not inflate the kill rate. Assertion failures (101) are the mutation-testing signal.
  Date/Author: process classifier; SIGKILL exception 2026-08-27.

- Decision: Null-schema and identity-mutant gates smoke the cheapest 8 tests with a 30-second floor, not the full selected union.
  Rationale: Full union is a 10k-test serial suite and failed the crate before any mutant ran. A smoke gate still catches a schema binary that cannot run short tests with `QS_MUTANT` unset or set to the identity variant.
  Date/Author: 2026-08-27 after first two v2 attempts.

- Decision: `estimate --schema` reuses the same sample seed as v1 and writes a separate state directory (`QS_STATE_SUBDIR=state-v2`). Do not `--resume` against v1 `outcomes.jsonl`.
  Rationale: Resume would skip every mutant v1 already decided and never execute schema.
  Date/Author: 2026-08-27.

- Decision: Parallel Stage A uses `cargo nextest run --binaries-metadata` so partitions do not share cargo’s target lock. Probe directories and llvm-profdata temps are unique per process.
  Rationale: Serial partitions left a 120-core box idle; four sample processes racing `.qs-probe` and `foo.tmp` produced EEXIST and ENOENT.
  Date/Author: 2026-08-27, commit `9071b2e`.

- Decision: Skip `maybe_preflight` when `--skip-baseline` is set.
  Rationale: Preflight applies a mutant to the shared baseline clone. Parallel skip-baseline processes hashing that clone rebuilt Stage A.
  Date/Author: 2026-08-27.

- Decision: Refresh the v1 sanity baseline by re-running the same 150-mutant sample (seed 1) with the fixed binary, into `estimate-core-v1b.json`. The differential proof compares v2 against this file, not the original `estimate-core.json`.
  Rationale: The original baseline predates the duration-attach and SIGKILL fixes; its 8 timeouts and 3 errors are infrastructure artifacts, and 5+ of the 59 disagreements trace to them. This is a sample refresh, not the forbidden exhaustive v1 run.
  Date/Author: 2026-08-27 / Fable session.

- Decision: Schema pool budgets get gate parity plus a slowdown factor: floor `max(cfg.timeout_floor, 30 s)`, budget `3 × green × 3.0`. Non-green first passes get a confirmation pass: retry once with the mutant, then run the same test with no mutant active. Null-run failure means the schema binary (not the mutant) is at fault: skip that test, continue with the rest, and record stderr. Null-run success attributes the signal to the mutant (Caught for 101, Errored for other exits, Timeout after a recalibrated re-run). Confirmation runs are serialized process-wide and capture stderr tails.
  Rationale: All 23 sample timeouts were 5-second floor timeouts on sub-second tests; the gates already needed 30 s. First-failure-wins let one broken test poison 12 mutants with no diagnostics. Null-run attribution also adjudicates the 22 missed→caught rows: a catch that reproduces with no mutant active is a schema artifact, not a kill. v1 and gate code paths keep their old behavior so the baseline stays comparable.
  Date/Author: 2026-08-27 / Fable session.

- Decision: `resolve_binary` matches by exact hash-stripped base name against test-profile executables only; the substring `contains()` scan is deleted. `parse_executables` drops artifacts whose cargo message says `profile.test: false`. Multiple fingerprints of one target resolve to the newest. An executed mutant whose staged test list is empty returns Errored ("no tests were staged"), never a vacuous Missed.
  Rationale: Substring matching over a candidate set that includes plain bin builds sent `--exact` to the bifrost CLI and the fuzzer bin (exit 1, 10 poisoned mutants), and its libtest-harness variant is a silent zero-test pass. Empty selections produced 35 of v1's 44 Missed with no test executed.
  Date/Author: 2026-08-28 / Fable session.

- Decision: Invalidate and recompute the bifrost-core Stage B attribution instead of trusting the shard-era cache. Surgery: remove all `crates/bifrost-core/*` entries from `coverage.json` `lines`, clear `stage_b_mutant_ids`/`stage_b_ids_hash`; backup at `index/coverage.json.bak-thin-stageb`. The next estimate run recomputes attribution single-process.
  Rationale: The cached attribution was provably incomplete (empty selections for 35 sampled covered mutants) because parallel shards wiped each other's `index/profraw` directories. Both engines consume these selections, so the proof needs a sound index more than it needs cache reuse. Union coverage is untouched; NotCovered stays comparable.
  Date/Author: 2026-08-28 / Fable session.

- Decision: Remove the v1 execute path. The user accepted the documented allow-list as the trust bar (in place of Validation criterion One's literal empty diff) and authorized the deletion. `run_prepared`, `exec::execute`, `build_one`/`walk_batch`, and the plain `run_mutant_tests` wrapper are deleted; `estimate` and `run` always use the schema engine; `--schema` stays as a hidden accepted no-op so existing scripts keep working; `resolve_binary` and the confirmation machinery survive as schema infrastructure. The fixture differential test now checks v2 self-consistency instead of v1 agreement.
  Rationale: v1 existed only as the sanity baseline for this proof. After the index heal it agreed with v2 everywhere except its own floor-timeout noise and the documented prefilter class, and on those v2's verdicts are the evidenced ones.
  Date/Author: 2026-08-28 / user decision, Fable session.

- Decision: Allow-list for the v2c-vs-v1f differential (`--sample 150 --sample-seed 1`, 15 rows). Class 1, 4 rows, `caught→unviable`: `Language.extensions`, `FqName.segments`, `Role.single_target_roles`, `WorkspaceOverlayContent.*` — return kind `ref` is pre-filtered as structurally unviable under schemata; v1 caught 3 of them, so this is a real, bounded measurement loss (about 2.7% of the sample), not noise. Class 2, 11 rows, `timeout→caught` (5) and `timeout→missed` (6): every v1f timeout is a 0.41–2.04 s-green test cut off at v1's 5-second floor on a loaded box; v2's confirmation pass re-ran each with null calibration and produced a real verdict, so the v2 side is the trustworthy one in all 11. No other classes remain; errored (3) and not-covered (7) agree exactly.
  Rationale: The plan accepts "a documented allow-list of expected disagreements" in place of an empty diff. Both classes are mechanically identifiable from the reports alone and neither indicts a schema verdict.
  Date/Author: 2026-08-28 / Fable session.

- Decision: Implement the requested sequence as user-visible milestones while allowing the smallest dependency-first internal edits. Multi-site `QS_MUTANT` parsing and observation records land during triage because survivor/control combinations cannot run without them; the general pair sampler and estimator remain Milestone 2, and durable cross-run merging remains Milestone 3.
  Rationale: The three ideas have a dependency cycle if interpreted as file order: triage requires higher-order activation and kill vectors, while subsumption requires matrix rows. Preserving the requested order at the observable-feature level avoids a fake triage implementation while still delivering triage first.
  Date/Author: 2026-08-30 / Codex.

- Decision: Higher-order modes are opt-in and supplemental. They never rewrite a first-order `Outcome`, never turn a non-101 exit into Caught, never interpret an empty selection as survival, and use the existing confirmation/null-calibration policy.
  Rationale: The schema proof established strict verdict semantics. Exploratory pairing must not weaken or silently replace them.
  Date/Author: 2026-08-30 / Codex.

- Decision: Neighbor selection is deterministic and Bifrost-independent: prefer the nearest byte span in the same file, then the nearest stable mutant id in the same package, while requiring a caught first-order control and a distinct site. Pair tests are the deduplicated union of both selections.
  Rationale: Variants at the same function site cannot both be active. Syntactic locality is already available from discovery and avoids issue 2771. The caught control makes comparison interpretable and the union avoids under-selecting either constituent.
  Date/Author: 2026-08-30 / Codex.

- Decision: A kill vector is complete only when every selected test has a confirmed canonical result. Subsumption is computed only between complete rows over the same test-column universe; prefixes collected during ordinary fail-fast execution remain useful for scheduling priors but cannot prove subsumption or equivalence.
  Rationale: Treating an unexecuted suffix as passing would manufacture dominance relationships. This is the matrix analogue of refusing vacuous Missed verdicts.
  Date/Author: 2026-08-30 / Codex.

- Decision: Persist coupling evidence automatically for every schema run in `index/coupling-v1.json`; no extra recording flag is required. Invalidate the artifact conservatively when the coverage source cache key changes, and serialize cross-process merge/save with a file lock.
  Rationale: Automatic sparse capture has negligible execution cost and makes ordinary runs useful as priors. Global source-key invalidation is less incremental than per-file fingerprints but is sound with the identities available today; stable row and column ids still merge repeated runs within one exact source state.
  Date/Author: 2026-08-30 / Codex.

## Outcomes & Retrospective

v2 verdicts on the 150-mutant seed-1 sample are now trustworthy, and on this sample v2 is strictly more reliable than v1: zero timeouts (v1: 11 floor timeouts), every non-green verdict confirmed by a retry plus a no-mutant run, errored and not-covered in exact agreement, and 29 minutes wall against v1's roughly two hours. The final differential (`estimate-core-v2c.json` vs `estimate-core-v1f.json`) has 15 disagreements, all in the two allow-listed classes in the Decision Log; none indicts a schema verdict.

The proof detour mattered more than the proof: the original disagreement count (59) was dominated by three engine bugs that also silently corrupted v1 results. (1) Schema pool budgets used the 5-second floor the gates had already outgrown. (2) Test-to-binary resolution fell through to substring matching over candidates that included plain bin targets, sending `--exact` to non-harness binaries (exit-1 error cluster) — and, in the libtest-harness variant of the same bug, silently running zero tests. (3) Stage B per-test attribution was shard-era-corrupted (concurrent processes wiped each other's profraw dirs), so 35 of the 143 covered sample mutants had empty selections: v1 built them, ran nothing, and reported Missed. A hand-applied mutant proved those Missed verdicts false. bifrost-core's real sample kill picture is 82–83 caught / 10–16 missed, not the 51 caught / 44 missed the first sample reported.

What someone can do today: from the repo root after `unset RUSTUP_TOOLCHAIN`, run the v2c command in Concrete Steps. It reuses the healed index (Stage B cache now covers the sample), compiles the schema batch once, runs gates, executes 143 mutants through the confirmation pool, and diffs against `estimate-core-v1f.json`, exiting 2 with exactly the 15 allow-listed rows.

The earlier “before deleting v1” question is resolved: the user accepted the documented allow-list and v1 was removed in `1886365`. Independent follow-ups remain worthwhile: fix the `edge_weight_only_merge_sums_distinct_file_line_sites` stack overflow in bifrost, and consider a Stage B attribution sanity check that fails loudly when attributed lines are implausibly sparse.

Lessons: gates that iterate every covering test do not scale to reverse-dependent suites. Duration metadata must survive every reload of `coverage.json`. A 5-second floor is wrong for this crate under load, for v1 too. Shared scratch plus in-place baseline mutation is not safe for parallel processes, and neither is a shared profraw directory — the shard-era races quietly destroyed per-test attribution while leaving union coverage intact, which made the damage invisible until a differential run contradicted itself. Substring binary matching is never acceptable: its bad path is not an error but a silent vacuous pass. Never report Missed for a mutant that ran zero tests. Confirmation-with-null-attribution turned every anomaly in this bring-up into a self-diagnosing log line; it is the single change that made the rest findable.

The 2026-08-30 continuation delivered the requested ideas in order. `run --triage` now produces bounded, auditable survivor classifications from three complete vectors on a common test universe. `estimate --second-order-pairs N` produces reproducible pair observations and an explicitly assumption-bound inversion without rewriting first-order evidence. Every schema run contributes observations to a durable matrix, while prefixes remain visibly incomplete and subsumption requires complete comparable vectors. The tiny fixture proves multi-site activation, one likely-equivalent triage case, one caught second-order pair, artifact persistence, and all three JSON sections. No Bifrost graph query was added, so issue 2771 does not block any of these paths.

## Context and Orientation

This repository is `cargo-quicksilver`, a command-line tool that does mutation testing for Rust Cargo workspaces. Mutation testing means: change a function body on purpose, rebuild, run tests that should exercise that body, and see if a test fails. A failing test is Caught. A passing test is Missed. If no test ever executed that source span, the mutant is Not Covered. If the change does not compile, it is Unviable.

v1 (the older execute path) cloned a worker tree per build slot, applied one mutant, ran `cargo build --tests`, then ran selected tests against that binary. The entry was `src/exec/mod.rs` `execute`, called from `src/engine.rs` `run_prepared`. Removed 2026-08-28 after the proof below; schemata is the only engine, and `--schema` is a hidden accepted no-op.

v2, also called schemata in comments, inserts a short dispatch prologue after each selected function’s opening `{`. The original body stays. A small module `__qs_rt` (source string `QS_RT_SRC` in `src/schema.rs`) reads the environment variable `QS_MUTANT` once. Format is `site_id:variant` as two decimal or hex-capable integers joined by `:`. When unset, `active` returns `None` and every function runs its original body. When set, only that site takes the matching match-arm (a `return …` of the mutant replacement) or the identity arm (empty, fall through). Tests therefore share one compiled binary. The execute entry is `src/exec/schema.rs` `execute`, called from `src/engine.rs` `run_schema`.

Stage A is a union coverage census: run reverse-dependent tests once with LLVM instrumentation, merge profiles, ask which source lines were hit. That fills `CoverageIndex.union` in `src/index.rs`. Stage B is per-test attribution for files that still need it, filling `CoverageIndex.lines`. Estimate pays Stage B only for the sample (`src/lib.rs` `cmd_estimate`).

bifrost is the target workspace at `/home/jonathan/Projects/bifrost`. The scoped crate is `brokk-bifrost-core`. Scratch that supports copy-on-write file clones (Linux `FICLONE` / XFS reflink) lives at `/mnt/optane/quicksilver` on the machine that ran the bring-up. Session directory: `/mnt/optane/quicksilver/065e17f14dc00ece`. Identity pin for bifrost builds: environment `BIFROST_BUILD_IDENTITY_OVERRIDE=0cf3cc6478edbf56546f69b35c8bbd2e69da3c6f`.

Verdict JSON is an object with `outcomes` (array of `src/types.rs` `Outcome`) and usually `estimate` plus `counts`. Differential comparison is `src/schema.rs` `gate_differential`, which reads that JSON as `JsonReport` and diffs by mutant id.

Key files a novice will touch next: `src/exec/test.rs` (timeouts), `src/exec/process.rs` (exit classification), `src/exec/schema.rs` (gates and schema pool), `src/schema.rs` (prologue and runtime), `src/lib.rs` `cmd_estimate`, `src/index.rs` `attach_timings` and `stage_b_for_mutants`.

Tiny fixture for local tests: `tests/fixtures/tiny/`, including `crate_schema`. Integration tests: `tests/schema.rs`, `tests/engine.rs`. Run with `unset RUSTUP_TOOLCHAIN` then `cargo test --offline --test schema --test engine` from the repo root.

## Plan of Work

The transform and first-order runner already exist, and the v2 proof described below is historical and complete. The active continuation is implemented in three user-visible milestones. Keep the historical steps after this continuation because they explain the verdict invariants the new experiment runner must preserve.

Milestone 1 adds survivor triage. Extend `src/schema.rs` so `QS_MUTANT` accepts one or more `site:variant` entries separated by semicolons and caches the parsed list once; retain the existing single-entry spelling exactly. Add a structured observation API in `src/exec/test.rs` that can either fail fast for ordinary verdicts or continue through every selected test for an experiment, applying the same retry and null-run attribution rules. Add `src/higher_order.rs` for deterministic neighbor choice, pair identity, kill-vector comparison, and triage classifications. Thread `run --triage` through `src/cli.rs`, `src/config.rs`, `src/lib.rs`, and `src/exec/schema.rs`. A survivor/control experiment must activate two distinct sites, run the deduplicated union of selected tests, and leave the survivor's first-order Outcome unchanged. The human report and JSON must name both mutant ids, the control's first-order verdict, the pair's test vector, and whether the survivor preserved, suppressed, or added kills relative to the caught control. If a complete comparison cannot be obtained, report it as inconclusive rather than likely equivalent or a test gap.

Milestone 2 adds pooled second-order estimation. In `src/higher_order.rs`, choose deterministic disjoint pairs from eligible schema mutants using the existing sample seed; exclude two variants of the same site and mutants that are NotCovered, Unviable, Errored, or Timeout. `estimate --second-order-pairs N` executes at most N pairs through the binaries already built for the first-order sample. Report eligible pairs requested and completed, caught pair count, observed higher-order kill rate `h`, and implied first-order kill probability `1 - sqrt(1 - h)`. Label this an independence-model estimate, not a replacement verdict. Split pair kills by running constituent vectors when attribution is needed; never claim which constituent caused a pair kill from the pair result alone.

Milestone 3 persists coupling evidence. Add `src/coupling.rs` with a versioned, serde-backed artifact at `Paths.index/coupling-v1.json`. Each row identifies an activation (one mutant or a stable pair id), records constituent mutant ids, the exact test ids selected, canonical per-test observations, and whether the row is complete. Store the coverage index `cache_key` and discard old rows on a mismatch. Within a matching source state, merge by stable mutant and test ids so reruns add columns without erasing prior evidence. Compute kill-vector subsumption only for complete first-order rows with the same selected-test universe; emit dominator/subsumed relationships and never use prefix rows as negative evidence. Persist atomically after execution and include a compact artifact summary in `--json` output.

Tests proceed from pure to end-to-end. Unit tests cover multi-site parsing/rendering, pair determinism, the inversion formula at boundaries and a known interior point, complete-versus-prefix vector semantics, merge/invalidation, and subsumption direction. Extend the tiny schema fixture so one missed mutant combined with a nearby caught control produces a known triage comparison and second-order pair. Integration acceptance must demonstrate the CLI options, JSON fields, and a reusable `coupling-v1.json` without running Bifrost or depending on its graph.

First, inspect the 23 v2 timeouts in `/mnt/optane/quicksilver/bringup/estimate-core-v2.json`. For each, look up the failing test name and that test’s `duration_secs` on the in-memory index after `attach_timings` (or in `baseline.json`). If 3× duration is still under the wall time the schema binary needs, raise the schema-path multiplier or floor in `src/exec/schema.rs` only (do not silently wait 120 seconds on every v1 test). If the test hangs, that is a prologue or environment bug: reproduce with one `QS_MUTANT` on the staged binary under `065e17f14dc00ece/runs-v2/`.

Second, inspect the 13 v2 errors. The cluster on `i1_silent_on_scala_auxiliary_constructor_fixture` exiting 1 should either be treated as a documented infrastructure skip or the schema binary should be given the same working directory and environment v1 used. Do not count exit 1 as Caught.

Third, keep rustc isolation unviables as Unviable. They are compile-time, not gate failures. Prefiltered `ref` stays Unviable.

Fourth, re-run the same sample with a fresh `QS_STATE_SUBDIR` (do not resume the 143-unviable files from earlier failed gates). Compare with `--diff-against` the v1 JSON. Accept the run only if disagreements are empty or each remaining pair is written into this plan’s Decision Log with a one-sentence reason.

Fifth, only after that pass, remove v1 execute and make `--schema` the default for `estimate` and `run`. That deletion is out of scope until the proof is green.

Do not start four sample shards in parallel on this host. One process, `--build-jobs 4` or less. Schema is one crate compile plus a test pool.

## Concrete Steps

Working directory for all cargo-quicksilver commands: `/home/jonathan/Projects/quicksilver`. Always `unset RUSTUP_TOOLCHAIN`. Binary: `target/debug/cargo-quicksilver` after `cargo build --offline`.

Reuse the existing scratch session so Stage A and Stage B are not rebuilt. Confirm these files exist:

    /mnt/optane/quicksilver/065e17f14dc00ece/index/coverage.json
    /mnt/optane/quicksilver/065e17f14dc00ece/index/baseline.json
    /mnt/optane/quicksilver/bringup/estimate-core.json

Expected: `coverage.json` version 3, about 12,868 tests, scope `brokk-bifrost-core`. `estimate-core.json` has 150 outcomes.

To re-run the accepted v2 differential (state dir names are historical; any fresh pair works):

    mkdir -p /mnt/optane/quicksilver/065e17f14dc00ece/state-v2c /mnt/optane/quicksilver/065e17f14dc00ece/runs-v2c
    rm -f /mnt/optane/quicksilver/065e17f14dc00ece/state-v2c/outcomes.jsonl
    export BIFROST_BUILD_IDENTITY_OVERRIDE=0cf3cc6478edbf56546f69b35c8bbd2e69da3c6f
    export QS_STATE_SUBDIR=state-v2c
    export QS_RUNS_SUBDIR=runs-v2c
    export QS_WORKER_PREFIX=v2b-
    unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER SCCACHE RUSTUP_TOOLCHAIN
    ./target/debug/cargo-quicksilver \
      --manifest-path /home/jonathan/Projects/bifrost/Cargo.toml \
      --scratch-root /mnt/optane/quicksilver \
      --clone-backend reflink \
      --package brokk-bifrost-core \
      --index-scope brokk-bifrost-core \
      --build-jobs 4 \
      --test-jobs 16 \
      --json /mnt/optane/quicksilver/bringup/estimate-core-v2c.json \
      estimate --sample 150 --sample-seed 1 --skip-baseline --schema \
      --diff-against /mnt/optane/quicksilver/bringup/estimate-core-v1f.json

You should see, in order: reflink probe ok; `reusing coverage index (cache key match)` and `reusing Stage B attribution`; `schema exec: 143 mutants across 1 crates`; the null-schema and identity gates on the cheapest 8 tests; `confirm <test>: …` lines whenever a first pass is not green. On 2026-08-28 this took about 29 minutes and exited 2 with `differential gate failed: 15 disagreements` — exactly the allow-listed rows in the Decision Log. The v1 baseline for `--diff-against` is `estimate-core-v1f.json` (healed index; earlier baselines `estimate-core.json` and `estimate-core-v1b.json` predate the attribution heal and must not be used).

If Stage A starts (`instrumented build into …/baseline-cov`), stop. Another process mutated baseline or `coverage.json` does not match `source_cache_key`. Do not rebuild Stage A unless `--reindex` was passed.

Local tests after timeout or gate edits:

    unset RUSTUP_TOOLCHAIN
    cargo test --offline --lib timeout_uses
    cargo test --offline --lib sigkill_is_timeout
    cargo test --offline --lib null_gate_uses_cheapest
    cargo test --offline --test schema --test engine

Expect those names to pass. `tests/clone_probe.rs` may fail if `/mnt/containers` is dead; that is unrelated.

## Validation and Acceptance

v2 is proven for this plan when all of the following are true on the same sample seed.

One: `estimate --schema --sample 150 --sample-seed 1 --skip-baseline --diff-against /mnt/optane/quicksilver/bringup/estimate-core.json` prints `differential gate: 150 v1 verdicts matched` (or the same count as outcomes written) and exits 0.

Two: v2 counts for timeout plus errored are not the majority of disagreements. A leftover timeout must be a test that also timed out under v1 or a test whose green duration times 3 still cannot finish in the schema binary after a documented multiplier change.

Three: Not-covered stays 7. That is Stage A census, shared by both paths. If it moves, the index was rebuilt or the sample seed changed.

Four: `cargo test --offline --test schema` still passes, including `schema_gates_and_differential_against_v1` on the tiny fixture.

Until (1) holds, keep v1 execute. Do not delete `run_prepared`.

Superseded 2026-08-28: the user reviewed the v2c-vs-v1f differential (15 rows, all allow-listed in the Decision Log) and accepted the documented allow-list as the trust bar in place of criterion One. v1 execute is removed; `schema_gates_and_differential_self_consistent` (v2 self-diff must be empty) replaces `schema_gates_and_differential_against_v1` as the regression gate.

The third run’s actual transcript, for comparison (failure, not acceptance):

    differential gate failed: 59 disagreements
    outcomes: 150 total — 58 caught, 8 missed, 7 not covered, 41 unviable, 13 errored, 23 timeout (139 mutant builds, 3 cargo invocations)

## Idempotence and Recovery

`--skip-baseline` plus existing `baseline.json` and `coverage.json` does not rebuild the green suite or Stage A if the coverage cache key matches. Deleting `state-v2/outcomes.jsonl` is the way to force v2 mutants to run again. Deleting `coverage.json` or passing `--reindex` rebuilds Stage A (hours on this crate); do not do that to retry v2.

If a v2 process dies, `--resume` with the same `QS_STATE_SUBDIR=state-v2` continues from `outcomes.jsonl`. Do not resume a file that is 143 identical unviable rows from a failed crate-level gate; delete it and start clean.

Scratch clones are copy-on-write on `/mnt/optane` (XFS reflink). If probe fails with EEXIST, you are on a binary older than unique `.qs-probe-<pid>-<n>` directories; rebuild `cargo-quicksilver` from current `master`.

Host memory: one schema process with `--build-jobs 4`. Four parallel estimate shards previously drove load above 1,000 and crashed the machine.

## Artifacts and Notes

v1 sample (sanity), 2026-08-27:

    Sampled scope: brokk-bifrost-core
    N (mutants): 1720
    f_nc: 0.063
    Caught 51, Missed 44, NotCovered 7, Unviable 37, Errored 3, Timeout 8
    Projected exhaustive: cargo-mutants 4.4 h, v1 1.6 h, v2 formula 1.5 min (one schema batch; not measured)

v2 third run (gates passed, differential failed), 2026-08-27 21:12 CDT:

    Caught 58, Missed 8, NotCovered 7, Unviable 41, Errored 13, Timeout 23
    agree 91, disagree 59
    missed→caught 22, caught→timeout 15, missed→errored 12, timeout→caught 3, caught→unviable 3, others small

Log: `/mnt/optane/quicksilver/bringup/v2.log`. Session: `/mnt/optane/quicksilver/065e17f14dc00ece`. Git: `9071b2e` on `master` at the time this plan was written.

## Interfaces and Dependencies

Schema runtime, already in tree, `src/schema.rs`:

    pub const QS_RT_SRC: &str; // module crate::__qs_rt with active(site: u32) -> Option<u32>
    pub fn render_prologue(site: &Site) -> String;
    pub fn apply_to_source(src: &str, sites: &[&Site]) -> Result<(String, Vec<AppliedSite>)>;
    pub fn qs_mutant_env(site_id: u32, variant: u32) -> (String, String); // ("QS_MUTANT", "{site}:{variant}")
    pub fn qs_mutants_env(activations: &[(u32, u32)]) -> (String, String); // semicolon-separated, sorted sites
    pub fn gate_differential(path: &Path, v2: &[Outcome]) -> Result<Vec<VerdictDiff>>;

Timeout helper, `src/exec/test.rs`:

    pub fn test_timeout(duration_secs: f64, floor: Duration) -> Duration;

Gate sampling, `src/exec/schema.rs`:

    const NULL_GATE_TESTS: usize = 8;
    fn cheapest_tests(tests: Vec<TestInfo>, n: usize) -> Vec<TestInfo>;
    fn null_schema_gate(...) -> Result<()>;
    fn identity_gate(...) -> Result<()>;

Estimate CLI already accepts `--schema`, `--diff-against`, `--sample`, `--sample-seed`, `--skip-baseline`. Keep those names.

Higher-order evidence in `src/higher_order.rs` exposes serializable `TriageReport` and `SecondOrderReport`. `run --triage --triage-neighbors N` controls survivor experiments. `estimate --second-order-pairs N` controls disjoint pair sampling. `src/exec/test.rs::run_mutant_tests_observed` is the shared execution primitive; `complete_vector=false` retains ordinary fail-fast behavior and `true` executes every selected test.

Persistent evidence in `src/coupling.rs` uses `CouplingRow`, `CouplingArtifact`, and `CouplingSummary`. `coupling::merge_and_save(paths, source_cache_key, rows)` locks, merges, and atomically writes `index/coupling-v1.json`. `report::write_json` retains its original compatibility signature; `report::write_json_with_evidence` adds the optional triage, second-order, and coupling sections used by the CLI.

External tools: rustc 1.97.1, cargo, cargo-nextest, llvm-profdata and llvm-cov from the rustup toolchain (needed only if Stage A is rebuilt). Clone backend: reflink on XFS.

## Revision note

2026-08-27: Created this ExecPlan after the bifrost-core v1 sample and three v2 attempts. Written retroactive and status-heavy because the schema engine already landed; the open work is proving verdicts, not scaffolding. Source of truth for numbers is the JSON files named above, not chat memory.

2026-08-28: Proof completed and closed out. Recorded the diagnosis chain (pool budgets, binary resolution, Stage B corruption), the healed-index baselines (v1f/v2c), the 15-row allow-list, and the user-authorized removal of the v1 execute path. Schemata is the only engine as of this date.

2026-08-30: Extended and completed the plan for survivor triage, pooled second-order estimation, and persistent coupling evidence. Recorded conservative completeness/subsumption rules, source invalidation, cross-process locking, CLI/JSON behavior, and final verification.
