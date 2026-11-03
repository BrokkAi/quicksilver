# Protect build artifacts through staging and filter baseline skips

This living ExecPlan follows `.agents/PLANS.md`. Maintain Progress, Surprises & Discoveries, Decision Log and Outcomes & Retrospective throughout implementation.

## Purpose / Big Picture

A successful mutant build must leave usable staged test binaries even if mbx collects the original managed target immediately afterward. Issue #39 reports disappearance of a whole target between Cargo success and QS staging; the removal actor is unknown. QS must close its verified lifetime gap without changing mbx scheduling, target placement or collection policy. Issue #35 also requires `[map] skip-tests` to prevent baseline execution, rather than merely ignoring failures after execution.

## Progress

- [x] (2026-11-03) Read both issues and current QS/mbx sources; origin/master matches efe4b09 and the checkout is clean.
- [x] (2026-11-03) Implement protected Cargo/staging handshake, shared snapshot ownership, original artifact filenames/mtime, and durable first/final-stage failure records.
- [x] (2026-11-03) Implement exact catalog-backed baseline skip filtering, preserving package/profile filters and supporting an intentionally empty baseline.
- [x] (2026-11-03) Deterministic lifetime, copy-failure cleanup, snapshot fingerprint ordering, real baseline-policy and same-state retry regressions pass; full suites/lint/format pass.
- [ ] Commit/push, verify CI and deliver tested install head to the paused campaign.

## Surprises & Discoveries

mbx 1.28 holds a target usage lease for its wrapped command, but its low-level ViewLease is private. QS must use the supported wrapped-command lifetime, rather than reimplement private lock paths. Cargo external subcommands already receive the ordinary mbx session. A hidden QS helper can therefore run Cargo in that session, report Cargo completion, and wait for an explicit parent acknowledgment while the parent snapshots test binaries. Existing BuiltExe values can carry shared ownership of those snapshots through isolation and final staging.

## Decision Log

- Decision: Invoke a hidden Cargo subcommand through mbx, preserving that session in the helper. Run the real Cargo compiler command once; after its completion, retain the wrapping mbx process until the parent acknowledges that artifact copies are complete.
  Rationale: This uses mbx's existing command lifetime, retains normal scheduling and collection, and avoids a race acquiring protection after Cargo has returned. Parent failure closes the acknowledgment pipe so the helper releases naturally.
  Date/Author: 2026-11-03, Codex.
- Decision: Snapshot reported test harnesses into a private directory under QS state; retain its directory through shared owners on BuiltExe until final staging finishes. Persist a diagnostic JSON record on staging failure.
  Rationale: All schema isolation, direct mutant and map paths already pass BuiltExe values; ownership follows those values and later collections cannot invalidate them. Failed artifacts must not be silently accepted or recaptured.
  Date/Author: 2026-11-03, Codex.
- Decision: Resolve skip globs against the concrete test catalog, then exclude each matching binary-id/test-name pair using nextest's equality predicates and documented escapes.
  Rationale: QS glob semantics remain identical, duplicate test names in other binaries remain selected, and there is no approximate glob translation.
  Date/Author: 2026-11-03, Codex.

## Outcomes & Retrospective

Local validation complete: `nice -n 10 mbx test --tests` passed 192 library tests and 18 integrations (three campaign, three edited, nine engine, three schema), for 210 passed and two existing ignored. `mbx clippy --all-targets -- -D warnings`, `cargo fmt --check` and `git diff --check` passed. Full suites exercise real mbx map/shared/direct/built-in builds with the helper. A deterministic collector cannot take the exclusive lease after simulated Cargo exit until copying is acknowledged; removing the original target afterward leaves the snapshots usable. Fault injection proves a copy error releases the helper, removes partial snapshots, saves Cargo/path diagnostics, keeps the captured mutant Pending with the same ID/intent, and permits a no-recapture retry. Baseline marker regressions verify skipped seed and extra tests never execute, same-named tests in other binaries remain selected, workspace/all-skipped baselines work, and an unskipped seed failure still blocks mapping.

Implementation and local validation are complete. Push, CI and install-head handoff remain. No mbx/engine repository changes or cloud launches are in scope.

## Context and Orientation

`src/cargo_cmd.rs` invokes mbx and parses compiler artifacts. `src/exec/edited.rs` and `src/exec/schema.rs` perform isolation builds and then `stage_tests`; `src/exec/build.rs` resolves artifact paths. `src/cli.rs` and `src/lib.rs` dispatch Cargo subcommands and normally restore the enclosing mbx environment. The hidden helper must bypass that restore so compilation stays in its existing session. `src/baseline.rs` runs nextest with the selected project profile. `src/index.rs` lists tests and applies skip globs. `tests/campaign.rs` is the real CLI regression suite; Cargo and mbx are already installed for it in CI.

## Plan of Work

First add a small artifact-lifetime module containing the hidden helper, acknowledgment protocol, owned snapshot directory and parent command runner. A private PATH entry points Cargo at this exact QS executable. The parent consumes a completion event, copies all reported harnesses while mbx still waits, then acknowledges and reaps the command. CapturedBuild retains existing stdout/stderr for diagnostics and compilation attribution. BuiltExe gains shared ownership of the copied file and its original filename, so existing binary resolution remains correct. Error paths must close/reap the child, remove partial snapshots, and save Cargo output and path context under the index.

Second add baseline skip selection from a catalog using the same selected profile/package set. Combine equality exclusions with the existing package/extra-test filter using explicit parentheses. Escape DSL control characters; do not use shell quoting as DSL escaping. Preserve post-run failure filtering as a compatibility safeguard. Update RUNNING.md to describe actual skipping.

## Concrete Steps

Work in the existing checkout and branch. Run focused unit regressions with `mbx test --lib`, the campaign CLI test with `nice -n 10 mbx test --test campaign`, then `nice -n 10 mbx test --tests`, `mbx clippy --all-targets -- -D warnings`, `cargo fmt --check` and `git diff --check`. Use permission escalation for tests requiring Unix sockets, as in the previous batch. Commit checkpoint progress before final validation and push to origin/master.

## Validation and Acceptance

A deterministic fake collector takes an exclusive lease only after the wrapped command finishes. Simulate Cargo exit, verify the lease is still unavailable during snapshot copying, finish the acknowledgment, then allow collection and verify the copied binary remains usable. Also verify copy-error cleanup and early helper exit release the command without publishing Built records. Existing full integration suites must exercise map, shared schema, built-in schema and direct impl-Trait builds through the real helper. For #35, use a skipped seed test that writes a marker and fails, plus a skipped extra-suite test and a similarly named non-skipped test; the marker must never be created and baseline JUnit/coverage must omit only the configured IDs. Verify project-profile and extra-suite selection still work. CI must pass before the final campaign handoff.

## Idempotence and Recovery

Use unique snapshot directories and atomic diagnostic writes. Shared ownership removes a snapshot only after its last reference disappears, including errors. Closing the acknowledgment pipe releases the helper if the parent fails; kill/reap the helper on transport errors. No global environment, Cargo target layout or GC setting changes are permitted. Retry operates on the same captured state. Do not force push or change branches; merge concurrent master edits if necessary.

## Artifacts and Notes

Historical evidence remains in the reporter's `/tmp/qs16-semantic/` archives and issue #39. It does not prove a particular eviction actor. Record focused validation output and the final pushed hash here as work completes.

## Interfaces and Dependencies

Use existing libc, serde_json, reflink-copy and standard library process/filesystem primitives. Add no dependency on private mbx lock paths. BuiltExe must continue matching package manifest paths and original artifact filenames after copying; the snapshot copy must retain the artifact filename. The helper is hidden from ordinary CLI help and never restores its enclosing mbx session. The parent supplies only Cargo build arguments and a completion/acknowledgment transport; it applies the existing build environment to mbx normally.

Revision note: Initial implementation plan for the requested #39/#35 between-crate batch.

Checkpoint note: First copy happens before the acknowledgment releases mbx. Real compiler PATH omits the temporary plugin discovery directory, avoiding changed build-script PATH fingerprints. Final staging refuses to scan unprotected targets when Cargo reported no harnesses. Fault injection preserves the captured mutant ID/intent as Pending and succeeds on retry without recapture.

Validation artifacts: `/mnt/optane/tmp/qs39-all-tests.log`, `/mnt/optane/tmp/qs39-clippy.log`, `/mnt/optane/tmp/qs39-lifetime.log`, and `/mnt/optane/tmp/qs39-campaign.log`. Intentional FAIL/TIMEOUT fixture output is part of the successful policy tests. Initial sandboxed library execution hit the existing Unix socket restriction; unrestricted test runs passed.

Revision note: Recorded complete local validation, documented protected copying and diagnostics, and confirmed no source/cache/layout/GC changes in other repositories. Ready for pushed-head/CI handoff.
