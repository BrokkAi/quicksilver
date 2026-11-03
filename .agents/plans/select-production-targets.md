# Select one production library or binary per mutation pass

This living ExecPlan follows `.agents/PLANS.md`.

## Purpose / Big Picture

Issue #44 needs explicit source-target selection in mixed library/multi-binary packages. Users will choose --lib or --bin NAME with exactly one --package, optionally persist that choice in [mutation], and run map/build/test with the same selection. Integration tests remain in the Cargo/nextest suite. The selected binary can be exercised by a real CLI driver, including a required-feature binary. The legacy unspecified selection remains library, otherwise first binary.

## Progress

- [x] (2026-11-03) Read issue, discovery/runtime routing, feature identity and protected artifact boundaries. Delegation refused because this session disables sub-agents; parent retains implementation and validation.
- [x] (2026-11-03) Separately reproduced the reported #43 apply alias concern in a temporary Git repository; preparing its independent correction/review.
- [x] (2026-11-03) Selection/config, metadata discovery, target-qualified identities, persistence and stale-state refusal implemented.
- [x] (2026-11-03) Selected-runtime ownership and inactive shared-source contexts passed real lib/two-bin integration checks.
- [x] (2026-11-03) Ordinary mbx helper view lease, protected child snapshots, saved digests and pre-test refusal implemented. CLI driver calls mbx clean while recording/testing and confirms its children remain present.
- [x] (2026-11-03) Mixed lib/two-bin required-feature CLI campaign passed entered/caught, capture/rebuild/fresh replay, root/ID, mismatch and missing-child refusal checks.
- [x] (2026-11-03) Initial full suite passed 222 tests/two existing ignores. Final hyphenated-binary campaign passed schema and built-in run, bare-manifest argv, proof-context and opaque-return refusal checks. Final lint/full-head checks and review remain.
- [x] (2026-11-03) Reproduced #45 with --manifest-path Cargo.toml from its directory (ENOENT); normalized empty parent to current directory and applied that argv to the real CLI campaign.
- [ ] Finish local checks, commit, request review, push, verify exact CI head and hand off.

## Surprises & Discoveries

Tests using env!("CARGO_BIN_EXE_name") embed Cargo's absolute output path. Copying the test harness does not redirect this child. The existing protected build copies only test harnesses. The new path needs an ordinary mbx command lifetime while child processes run, plus saved executable content identity. Do not rewrite test source, inject a new rustc wrapper, patch executable strings, change target directories, or overwrite Cargo outputs to hide this boundary.

A physical source module can compile in both the selected target and an unselected target. The selected crate root needs the active runtime; other reached crate roots need an inactive runtime, and selected runtime ownership must distinguish binaries compiling the same root through Cargo's compile-time target environment. Mutant bodies still have to compile in every Cargo context that uses the edited source.

## Decision Log

- Decision: Global mutually exclusive --lib / --bin NAME, requiring exactly one --package. Configuration is [mutation] lib=true or bin="name"; explicit CLI selection replaces configuration. Persist mutation_target separately from the existing host target triple and Cargo features.
  Rationale: Separate target passes meet the request without expanding simultaneous-target mutation. The test suite remains package-selected, never narrowed with Cargo --bin.
  Date/Author: 2026-11-03 Codex.
- Decision: Explicit selection qualifies function names with package, target kind and target name; unspecified selection preserves historical IDs. Resolve selected roots from Cargo metadata and reject absent/ambiguous targets.
  Rationale: Shared source compiled in different targets must not share captured identities or reuse verdicts.
  Date/Author: 2026-11-03 Codex.
- Decision: Hold an ordinary mbx helper command as a view lease through recording and testing, and record/verify ordinary child executable paths and hashes. If a retained child was collected or replaced between commands, refuse before verdict/state mutation and request an explicit rebuild; never restore files into mbx-managed outputs.
  Rationale: Compile-time Cargo child paths cannot transparently be redirected by test environment. This keeps normal mbx layout/GC semantics and prevents stale child execution. Explain this supported rebuild boundary in RUNNING.md and the handoff.
  Date/Author: 2026-11-03 Codex.

- Decision: Refuse physical apply/prove for explicit target mutants whose source is shared by another target. Refuse edited opaque-return direct variants in explicit passes.
  Rationale: Physical edits cannot reproduce context-isolated dispatch; direct variant child outputs cannot safely represent several compiled generations at one Cargo path. These cases have explicit diagnostics rather than false proofs or stale execution. Physical prove remains available for source unique to its target.
  Date/Author: 2026-11-03 Codex.

## Outcomes & Retrospective

The required-feature mixed-target regression works end to end, including inactive unselected shared contexts and ordinary mbx lease protection. Initial full suite passed 222 tests with two existing ignores; final checks/review/CI remain. The accepted #43 standalone one-bin workflow can proceed at e798cc4 independently. No engine files, Cargo defaults, build profiles, physical targets or mbx scheduling will change.

## Context and Orientation

`src/cargo_cmd.rs::mutable_scope` and `crate_root_source` choose the current legacy root. `src/discover.rs` walks it and assigns FQNs. `src/schema.rs` inserts crate::__qs_rt dispatch; `src/exec/edited.rs` groups sites by package and persists map/state. `src/exec/schema.rs` handles built-in mutants. `src/features.rs`, `src/config.rs`, `src/sitemap.rs` and `src/types.rs` provide patterns for serializable campaign identity. `src/artifacts.rs` keeps an mbx build session alive until harness copies finish. `src/exec/test.rs::BinariesManifest` records staged tests but currently no ordinary child executable identity.

## Plan of Work

Add a production-selection module with validation, exact metadata resolution, qualified names and a require-match guard. Thread selection through configuration, discovery and both engines' crate-root lookups. Save selection in map/state/baseline/test reports, invalidate every prior status on a new target pass, reject incompatible commands/imports, and use a new map version while accepting legacy versions only with default source selection.

For explicit selections, snapshot auxiliary crate roots that compile edited source, install inactive runtime support there, and put the active owned runtime only in the selected root. Preserve original user source and original nonselected bodies. Cover a shared file in distinct binary compilation contexts.

Add an mbx helper lease using the ordinary hidden-subcommand lifecycle, acquire it for map/build/test before child-dependent work, and save ordinary child path/hash identity with staged manifests. Verify before using a generation; a changed or missing child causes an actionable rebuild error, never a kill or survivor. Apply equivalent safeguards to direct-variant manifests; if a current child does not represent that direct variant, refuse rather than silently test the shared generation.

Create a real temporary mixed library/two-binary package with a release-tooling feature. Integration tests spawn the selected binary via env!("CARGO_BIN_EXE_name"). Verify discovery, entered sites, caught exact fault, unchanged lib/other-bin user source, target-qualified identities, config/CLI behavior, selected root, rebuild/replay and mismatch rejection without changing state. Prove ordinary mbx lease protection through a deterministic helper boundary test. Exercise collected/replaced child refusal.

## Concrete Steps

From this repository root run targeted regressions with `nice -n 10 mbx test --test campaign`, then `nice -n 10 mbx test`, `mbx fmt --check` and `nice -n 10 mbx clippy --all-targets -- -D warnings`. No target overrides. Commit only owned files on the current branch. Call mcp__mj_agents.request_review after committing and before every push; await every required reviewer, address findings and retry incomplete reviews. Push origin HEAD:master only after the gate is clean, then verify CI's exact SHA and both job conclusions.

## Validation and Acceptance

A selected required-feature binary function has recorded hits from an actual CLI integration test and its faulty body is Caught. Library/other binary functions are absent from that pass's mutation inventory. Switching target selection refuses stale build/rebuild/test and resets old results after an explicit remap. Function/body/target identity survives same-target capture, rebuild and exact --from replay. A stale child is rejected before any new verdict. Defaults and existing map v7-v9 remain supported under their original source selection.

## Idempotence and Recovery

Keep temporary subjects in disposable Git repositories. Save catalogs before changing maps, restore only caller-owned fixture edits, and preserve the exact old tested handoff until this change passes checks/review/CI. A missing retained child requires explicit build --rebuild; no implicit map or Cargo output restoration. Never launch cloud hosts for this work.

## Interfaces and Dependencies

Use existing clap/serde/cargo_metadata and syn scanning. Keep host-target probing in src/target.rs separate from production source-target selection. Normal mbx commands supply all view protection; add no mbx dependency, GC policy or custom build layout.

Plan created 2026-11-03 after investigation and the separate confirmed apply alias report.

Updated 2026-11-03 after real CLI/lease/source-context evidence and the confirmed #45 argv normalization control. The separate alias fix 1381ccd passed clean review and CI run 41703957226 and is already on origin/master.

Updated 2026-11-03 with final hyphenated CARGO_BIN_EXE coverage, real mbx-clean-under-lease evidence, child/target refusal checks and the small #45 manifest normalization regression. Explicit binary presence is checked by target name and owning package manifest, so another package cannot satisfy a missing required-feature binary.
