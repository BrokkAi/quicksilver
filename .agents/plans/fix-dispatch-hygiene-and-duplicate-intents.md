# Preserve mutant meaning, duplicate intents and explicit rebuilds

This living ExecPlan follows `.agents/PLANS.md`. Keep Progress, Surprises & Discoveries, Decision Log and Outcomes & Retrospective current.

## Purpose / Big Picture

Runtime campaign issues #40 and #41 show that generated dispatch can change a mutant's meaning by shadowing a production `v`, and that repeated appearances of the same captured mutant can lose a per-patch intent before state deduplication. Correct dispatch must neither capture user identifiers nor require fresh mapping. Users must also be able to rebuild every existing captured status with corrected instrumentation while keeping source, map and mutant identities unchanged.

## Progress

- [x] (2026-11-03) Read issue receipts and current code; origin/master matches f90439c and working tree is clean.
- [x] (2026-11-03) Binding-free dispatch and AST no-binding property pass; real CLI struct/scalar/generic-serializer regression passes.
- [x] (2026-11-03) Actual frozen receipts confirm repeated file headers; touched files deduplicate before capture and intent groups count distinct identities. Duplicate-note and distinct-variant unit/CLI tests pass.
- [x] (2026-11-03) Full-catalog rebuild and combined patch/note repair tested across Built/Unviable/Tested states; RUNNING.md documents both exact recovery procedures.
- [x] (2026-11-03) All 213 tests, Clippy/format and both CI jobs pass; #40/#41 closed. Delivered executable head d2ed5d2ee4138762cece4bd02ece9d5164c4d7c4 with exact install/frozen-path recovery commands and status/identity checks to session 5b9275ef422b8eb3146a4a11fb9dd1aa.

## Surprises & Discoveries

`render_prologue_with_ranges` currently wraps mutant bodies inside `if let Some(v) ... match v`, so struct parameters fail and generic consumers may silently use the selector integer. Identity falls through outside the generated binding, so the null gate cannot find this. `capture_patch` also records every repeated file header in a vector and captures the same final patched file once per occurrence; concatenated per-function diffs can therefore create duplicate entries before state deduplication. Confirmed in both immutable E/round2 and E/round6 receipts: each has exactly two sections for crates/bifrost-runtime/src/extension/artifacts.rs, so the old code lifts both final edits twice. Normal build already recompiles Built records but omits Unviable/Tested records; replaying a saved export does not reset duplicate records' statuses.

## Decision Log

- Decision: Match directly on `crate::__qs_rt::active(site)` using fully qualified `::core::option::Option::Some(index)` patterns and wildcard/identity fallthrough. Introduce no selector bindings.
  Rationale: Renaming the generated variable cannot guarantee hygiene for arbitrary source identifiers. No source-side identifiers are introduced by the dispatch at all.
  Date/Author: 2026-11-03, Codex.
- Decision: Capture a touched file once after the full patch applies, and attach an intent to every repeated occurrence of one exact mutant identity. Distinct replacement identities remain ambiguous even when they share a function name.
  Rationale: Address both the duplicate source and note matching, while preserving deduplication and genuine ambiguity diagnostics.
  Date/Author: 2026-11-03, Codex.
- Decision: Add `build --rebuild`, suppressing implicit checkout capture but accepting explicit patch/import inputs, to reset every captured record to Pending under StateLock and save that reset before compilation. Preserve catalog contents, IDs, intents, capture timestamps, map and committed source; stale verdicts are intentionally invalidated.
  Rationale: This is an explicit supported recovery from faulty generated code, including false Unviable and already Tested records. A stopped or failed rebuild must not leave old verdicts eligible for reuse.
  Date/Author: 2026-11-03, Codex.

## Outcomes & Retrospective

Local validation complete: all 213 tests pass (194 library, four campaign, three edited, nine engine, three schema), with two existing ignored. Clippy across all targets and formatting pass. The real CLI regression compiles struct/scalar/generic v mutants, demonstrates intended-value MISSED results instead of selector-based false kills, restores lost per-patch notes, adds a repaired candidate while retaining the genuine old Unviable, and verifies full catalog/map/ID/capture-timestamp preservation after repeated rebuilds. Invalid recovery patches leave existing statuses untouched.

A counterfactual check temporarily used the old production dispatcher from f90439c for only this tiny CLI regression. It fails with E0610 and an extra false Unviable, proving the regression catches the reported bug. Fixed source was restored in a finally block before the successful full suite. No Bifrost code/state or mbx policy was modified. The complete implementation is pushed at d2ed5d2ee4138762cece4bd02ece9d5164c4d7c4. CI run 41693120912 passed both jobs. Issues #40/#41 are closed, and the exact combined note-repair/candidate/rebuild procedure was handed off. No required work remains in this batch. No engine repository, mbx settings, target layout, selection policy or cloud changes are in scope.

## Context and Orientation

`src/schema.rs::render_prologue_with_ranges` emits dispatch and byte ranges used to attribute rustc errors. `src/capture.rs::capture_patch` applies unified patches in a scratch directory and `capture_files` lifts body edits. Its `attach_intents_detailed` currently counts vector positions rather than distinct mutant identities. `src/exec/edited.rs::build` captures then compiles Pending/Built state records; `State::save` writes atomically and folds/removes the verdict journal. `src/cli.rs` defines build flags and `src/lib.rs::cmd_build` constructs CaptureSources. `tests/campaign.rs` exercises the real CLI on tiny temporary repositories. Use `.agents/` for internal artifacts; update RUNNING.md for supported user commands.

## Plan of Work

First change only dispatch rendering and update byte-range expectations. Add tests with struct-valued and scalar `v` arguments, plus a generic serializer whose wrong-selector use compiles. Tests must permit the intended mutant result while rejecting the selector result, so successful compilation alone is insufficient evidence.

Next deduplicate touched file paths before initializing/reading patched files. Inspect real receipts to explain duplicate entry generation. Change intent resolution to group matched occurrences by exact runtime ID and edit identity; repeated copies receive the note together, while two actual variants remain a reported ambiguity. Exercise concatenated same-file diff sections and repeated per-patch notes through capture, build, JSON and replay; verify full names, IDs and notes.

Finally add the explicit no-capture rebuild flag. Validate current HEAD matches the map. Validate explicit captures/notes first, then requeue the resulting whole catalog; rejected captures must leave existing statuses unchanged. Reset all statuses only after validations, persist Pending records before Cargo, then use the normal shared build/isolation flow. Regression setup includes Built, falsely Unviable, already Tested and genuinely unviable records: rebuild must compile the valid ones, retain a genuine compile error, discard old verdicts, preserve identity and use the unchanged map. Do not require manual state editing in the user protocol; deliberate status seeding is confined to the temporary test fixture.

## Concrete Steps

Run focused unit tests through `mbx test --lib`, then tiny CLI regressions using `nice -n 10 mbx test --test campaign`. Use escalation for tests requiring Unix sockets as in previous batches. Complete `nice -n 10 mbx test --tests`, `mbx clippy --all-targets -- -D warnings`, `cargo fmt --check` and `git diff --check`. Commit checkpoints directly on the existing branch and push to origin/master. Preserve the campaign's existing state/map; only the reporter reinstalls and runs its owner-authorized affected replay.

## Validation and Acceptance

A struct-valued parameter mutant must compile. A scalar input 40 must produce the intended 42 rather than selector-based 2. Generic serialization of an input record must serialize that record rather than the selector; use real compiled tests that allow baseline/intended outcomes but reject selector outcomes. Verify byte ranges still cover exactly the generated variant arms and runtime IDs remain unchanged.

A patch with repeated file sections must capture each changed function once. Duplicate copies of one exact mutant with a per-patch FQN intent must retain the note, while distinct bodies of one function still reject an ambiguous intent. Verify state/build/test exports and replay preserve notes and IDs.

Supported recovery after installation is `cargo quicksilver build --no-capture --rebuild --json /tmp/runtime-rebuilt.json`, followed by `cargo quicksilver test --json /tmp/runtime-retested.json`. Run the same package/extra-test/config scope as the existing map. A genuine malformed mutant can still make build exit 1; examine Unviable diagnostics before testing compiled records. The map ID/commit and captured bodies/IDs/intents must match before/after, the generation must advance, and old Tested/Unviable statuses must be reconsidered. Never clean or remap as a recovery shortcut.

## Idempotence and Recovery

`--rebuild` is deliberately repeatable and discards verdicts each time. A failed rebuild leaves the saved catalog Pending and retryable, never claiming old binaries were corrected. Ordinary build/capture/import behavior stays unchanged. Store recovery exports outside the edited checkout, preserve the original campaign export, and do not change branches or force push.

## Interfaces and Dependencies

Use existing Rust parsing, JSON and state-lock helpers; add no dependencies. BuildArgs and CaptureSources gain a rebuild boolean, default false. Preserve existing report shapes with an additive requeued count if needed. Rebuild suppresses implicit checkout capture at the CLI; explicit patch/import sources remain supported so lost notes can be restored and repaired candidates added in the same shared build. Dispatch uses no local selector identifier. Note grouping compares mutant edit identity, not note strings or vector positions.

Revision note: Initial plan for the requested runtime hygiene/intents/rebuild batch.

Scope clarification: The existing campaign already lost four notes, so recovery also supports `build --rebuild --patch PATCH=INTENTS ...` to fill missing duplicate notes and capture a separately repaired new variant before requeueing all records. The old genuinely malformed variant keeps its historical ID and is rechecked rather than deleted. The simpler no-capture rebuild remains available when no notes need repair.

Immutable receipts: round2.patch SHA256 3c76b71ba60f84394e293825e56c83ef3f3476da89657925e3b3130d09e1d457; round6.patch SHA256 c801f89eff8ebe47b28ecf2f17e47b6bdb82de8c230bbc01741c8d59240217b0. Read-only copies fetched from /tmp/qs17-runtime/intake-r1/E in session 5b9275ef422b8eb3146a4a11fb9dd1aa. No Bifrost source or artifacts were modified.

Revision note: Expanded recovery to explicit patch/intents repair before rebuild, and confirmed duplicate-header root cause against actual frozen inputs. A separate Cargo-feature-selection question will be answered after this fix handoff; it is not part of this batch.

Validation logs: `/mnt/optane/tmp/qs40-all-tests.log`, `/mnt/optane/tmp/qs40-clippy.log`, `/mnt/optane/tmp/qs40-campaign.log`, and the expected failing `/mnt/optane/tmp/qs40-old-dispatch-regression.log`. The latter is intentional evidence, not an unresolved current test failure.

Revision note: Completed operational recovery documentation and full validation, including a failing old-dispatcher control and a passing fixed-code full suite. Ready for pushed-head/CI handoff.

Completion note: https://github.com/BrokkAi/quicksilver/actions/runs/41693120912 is green for the tested executable head d2ed5d2ee4138762cece4bd02ece9d5164c4d7c4. Answered the separate feature-selection capability question: no supported consistent QS interface currently exists; the reporter will file separately. This final checkpoint changes plan metadata only.
