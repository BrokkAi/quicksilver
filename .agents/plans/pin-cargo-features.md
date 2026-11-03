# Pin Cargo features through mutation campaigns

This living ExecPlan follows `.agents/PLANS.md`.

## Purpose / Big Picture

Issue #42 needs mutation campaigns for code behind Cargo features, including optional dependencies and required-feature integration binaries. Users select ordinary Cargo features once in configuration or repeat CLI flags. Metadata, baseline, test catalog and every schema build must use that selection. Saved maps, mutant states and test reports record it; build/rebuild/test reject mismatched coverage. Existing maps without a selection mean Cargo defaults.

## Progress

- [x] (2026-11-03) Inspect issue and Cargo/nextest/metadata/build/state boundaries.
- [x] (2026-11-03) Implement shared selection, forwarding, persistence and mismatch validation.
- [ ] Exercise optional dependency and feature-gated function/test through real CLI (confirmed coverage, kill, rebuild and mismatch rejection; remaining final no-default/all-features pass).
- [ ] Run checks, commit, push, verify CI and hand off exact head.

## Surprises & Discoveries

The first real fixture run confirmed a caught feature-gated mutant and mismatch rejection, but exposed an incorrect test expectation: command errors exit 2. A later check showed the extra test package legitimately re-enabled defaults through its dependency edge; the fixture now uses default-features=false on that edge.

Discovery walks source modules with syn and intentionally leaves feature predicates unknown. A function behind a disabled feature can have a source site but no recorded hits. This remains an honest coverage limitation, not evidence that the code ran. Cargo metadata uses --no-deps, so passing feature flags does not require loading the external dependency graph.

## Decision Log

- Decision: Add `[cargo] features`, `all-features`, and `no-default-features` plus corresponding global CLI flags. Any explicit feature CLI option replaces the entire configured selection; otherwise configuration applies. Normalize comma/whitespace-separated feature names by sorting and deduplicating.
  Rationale: Users can pin a selection for an entire campaign while the CLI can explicitly choose a different selection without silently merging hidden settings.
  Date/Author: 2026-11-03 Codex.
- Decision: Reject mismatched build/rebuild/test before state changes. A new map with different features resets all retained statuses to Pending, including kills and unviable results. Explicit carry-forward imports only bodies and notes and recompiles against the new map.
  Rationale: Tests, optional dependencies and type checking change with features; no old verdict is trustworthy under another selection.
  Date/Author: 2026-11-03 Codex.

## Outcomes & Retrospective

Implementation and validation pending. Runtime instrumentation fixes remain unchanged; this work does not change mbx, targets, profiles or engine sources.

## Context and Orientation

`src/cli.rs` and `src/config.rs` merge user settings. `src/cargo_cmd.rs` constructs Cargo metadata and build commands; `src/index.rs` and `src/baseline.rs` construct nextest commands. `src/artifacts.rs` protects build artifacts while staging by running Cargo inside an mbx session; feature flags belong after its helper separator, on Cargo's own arguments. `src/discover.rs` scans workspace functions using metadata. `src/sitemap.rs` saves coverage and `src/exec/edited.rs::State` saves captured mutant statuses. `src/lib.rs` writes JSON reports. `tests/campaign.rs` exercises real CLI workflows with temporary committed Rust workspaces.

## Plan of Work

Introduce a serializable feature-selection value with canonical names and command-argument methods. Add it to configuration and discovery filters and pass it to metadata calls where configuration is available. Forward it to baseline builds, nextest list/run, protected builds and preflight builds. Save it in SiteMap, State, BaselineReport and test JSON. Default absent fields for compatibility. Check map/config selection before build/rebuild/test and map-backed queries. Reconcile feature changes by resetting every retained result. Reuse baseline timing reports only with matching selections.

Add a local optional dependency and required-feature integration binary to a temporary fixture. Demonstrate default mapping records no feature-gated hits; requested features activate the dependency and tests; a captured fault is caught; rebuild retains selection; mismatched commands fail without changing state; remapping invalidates old statuses. Include all-features/no-default-features and config/CLI selection behavior. Document the public procedure in RUNNING.md.

## Concrete Steps

From the repository root run `nice -n 10 mbx test --test campaign`, then `nice -n 10 mbx test`, `mbx clippy --all-targets -- -D warnings` and `mbx fmt --check`. Tests need ordinary local Unix sockets used by mbx. Do not redirect target directories. Commit only owned files on the current branch and push with `git push origin HEAD:master`. Watch the resulting GitHub workflow, then send the exact executable head and CI link to session 5b9275ef422b8eb3146a4a11fb9dd1aa.

## Validation and Acceptance

Real CLI feature-gated mutants must have recorded coverage and a Caught verdict. A wrong selection must produce an actionable error mentioning Cargo features and remapping, preserve map/state, and launch no build. No explicit selection must preserve existing defaults. New-map feature changes must invalidate Built, Tested and Unviable statuses. Existing campaign tests must pass unchanged.

## Idempotence and Recovery

All fixtures live in temporary repositories. Configuration flags do not edit engine manifests. Existing runtime map remains valid with its default selection. Users changing features create a new map; save exported catalogs before remapping across commits and use build --from for validated body carry-forward. Repeating --rebuild recompiles the catalog with its map's selection.

## Interfaces and Dependencies

Use existing clap, serde and cargo_metadata dependencies. The shared selection exposes normalized Cargo arguments and equality validation. No dependency installations, target changes, new runner or cloud hosts are needed.

Plan created 2026-11-03 after tracing command and persistence boundaries.

Updated 2026-11-03 after implementation and initial real CLI evidence; full tests and lint are running.
