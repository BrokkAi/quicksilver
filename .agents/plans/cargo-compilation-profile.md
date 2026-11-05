# Select and pin the Cargo compilation profile

This living ExecPlan follows `.agents/PLANS.md` and must be kept current.

## Purpose / Big Picture

The Bifrost campaign requires a project-defined Cargo compilation profile named measure. Quicksilver currently omits a profile on every build, so those lanes cannot use automatic maps. Add optional global --cargo-profile NAME and [cargo] profile = 'NAME'. Absence preserves Cargo's normal command defaults. Keep [map] nextest-profile independent: it controls test runner policy, while Cargo profiles control compilation. Refuse build/test when requested compilation profile differs from the map, and never retain stale verdicts when remapping with another profile.

## Progress

- [x] (2026-11-05) Verify clean current source e7596a1 has no compilation profile selection. Audit internal build, nextest, state reconciliation and staging paths. Delegation tools are unavailable, so parent owns implementation and checks.
- [x] (2026-11-05) Implement CargoProfile, global CLI flag and [cargo] profile with independent feature/profile precedence; shared frontend helper covers ordinary/protected/preflight builds and all three nextest run/list paths.
- [x] (2026-11-05) Persist profile in map/baseline/state/staged/test and built-in run reports. Pin built-in resume with an outcomes.profile.json sidecar; remove old log before publishing a fresh context. Map v11, legacy v7–10 default compatibility, all-status invalidation and body-only cross-profile imports implemented.
- [x] (2026-11-05) Focused CLI regression passes baseline/schema attribution retries, separate opaque direct build, independent nextest policy, provenance, drift refusal without state/map/journal writes, same-commit remap invalidation and body-only imports. Source audit removes debug/deps guess; full mixed target/child/lease and built-in run regression now uses a custom profile.
- [x] (2026-11-05) Format and Clippy clean; all 207 unit tests pass, two existing ignored.
- [x] (2026-11-05) Full checks pass: format, Clippy and 233 tests (207 unit, 11 campaign, three edited, nine engine, three schema), two existing ignored. Mixed custom-profile library/binary/child staging and built-in run passes.
- [ ] Commit, obtain required review, push, verify exact-head CI and send peer handoff.

## Surprises & Discoveries

Cargo uses --profile NAME; nextest uses --cargo-profile NAME for compilation and --profile NAME for its own test policy. Cargo metadata has no compilation profile option and must keep receiving only feature flags. Protected Cargo builds already derive executable paths from Cargo JSON and retain the mbx session through staging. Target leases cover the entire managed target, independent of profile directory. A legacy resolve_binary fallback still guesses target/debug/deps; remove that guess instead of allowing cross-profile fallback.

The audit found a second verdict path: built-in run --resume reads outcomes.jsonl independently of captured mutant State. It now checks a profile sidecar before reading old verdicts; a fresh run removes old outcomes before atomically publishing its profile, so an interrupted reset cannot relabel older verdicts. Legacy outcomes without a sidecar mean only default compilation. Built-in JSON also records cargo_profile.

The first profile CLI test combined a malformed candidate with an opaque direct candidate and received a whole-site attribution result for the latter. The profile forwarding test now exercises malformed schema attribution and opaque direct compilation separately; no cause or profile-specific bug is claimed for that mixed case. A first raw State read after map also correctly remained on the prior map until the first state-consuming command reconciled it, matching existing feature remap behavior. The test now invokes test after remap and proves an empty verdict report plus all Pending states before rebuilding.

Evidence: /tmp/qs-cargo-profile-check.log (all-target compilation), /tmp/qs-cargo-profile-cli.log (focused CLI pass), /tmp/qs-cargo-profile-all.log (format/Clippy/all 233 tests pass). The build-script path marker proves all initial-profile fixture compilations used measure, and a deliberately failing test remains absent under the independently named nextest lane-policy profile.

## Decision Log

- Decision: Represent the selected profile as a distinct optional CargoProfile value, serialized as a string or null, apart from CargoFeatures. Use a shared frontend-aware forwarding helper for every compilation command.
  Rationale: Feature flags replace only feature configuration; --cargo-profile overrides only the configured profile. Absence must remain different from explicit dev/test because command defaults can differ. Nextest policy selection must not be confused with compilation.
  Date/Author: 2026-11-05 Codex.
- Decision: Bump map format to 11, accept versions 7–10 only with their original default profile and existing target/feature restrictions. Compare profile before build/test state changes and invalidate every retained status when map profile changes.
  Rationale: Old binaries must not silently ignore a new compilation setting; old default maps remain usable. A kill or compile failure under one compiler profile is not evidence under another.
  Date/Author: 2026-11-05 Codex.
- Decision: Keep Cargo-reported artifact paths and managed target leases; remove the debug/deps guess. Include profile in baseline reuse and preflight cache keys, and record it in staged manifest provenance.
  Rationale: Custom profiles write to their own Cargo-chosen output directory. No target redirection, environment wrapper, compiler setting or mbx policy change is needed.
  Date/Author: 2026-11-05 Codex.

## Outcomes & Retrospective

Implementation and all local acceptance checks are complete. Format, Clippy and 233 tests pass with two existing ignores; committed review, push and exact-head CI handoff remain. The independent custom-profile CLI and mixed production-binary tests verify compilation, staging/leases, nextest policy separation, provenance, status invalidation, imports and built-in run. No Bifrost source/profile changes or cloud resources are required. The separate reported identity-gate failure has no QS bug attribution and is outside this profile capability.

## Context and Orientation

src/config.rs loads quicksilver.toml and merges global CLI flags from src/cli.rs. Its existing cargo table currently deserializes only CargoFeatures; introduce a CargoFile configuration carrying feature fields plus profile without changing feature serialization. src/cargo_profile.rs will hold optional profile selection, validation, forwarding and drift errors. src/cargo_cmd.rs::build_tests and src/artifacts.rs::build are the internal compilation boundaries; schema retries, bisection and physical/direct variants call the protected builder. src/baseline.rs also launches nextest and a preflight build; src/index.rs and src/baseline_env.rs launch full and binaries-only nextest listing.

src/sitemap.rs persists the map and format compatibility. src/exec/edited.rs persists captured mutant records and reconciles them against a new map. The first state-consuming command after a profile remap must reset Built, Tested and Unviable records to Pending even on the same commit or matching map id, before using any old binaries/verdicts. src/types.rs::BaselineReport, src/exec/test.rs::BinariesManifest and the test report in src/lib.rs provide provenance. src/exec/mod.rs::prepare_outcomes guards the separate built-in run resume log and src/report.rs records its compilation profile. src/carry.rs imports bodies and notes only, never old verdicts or binaries. Tests in tests/campaign.rs exercise real committed fixture repositories through the CLI.

## Plan of Work

Add the optional selection and file/flag precedence first, plus a shared helper that emits --profile for Cargo and --cargo-profile for nextest. Apply it to the ordinary baseline build, nextest baseline/list/binaries-only list, preflight and protected artifact builder. Metadata stays profile-free. Inspect all call sites to establish schema retries and bisection inherit the same helper.

Persist the selection in map, baseline, captured state, test report and staged manifests. Check requested profile against the map before build/test state mutations; report drift with a map-again instruction. Include it in state reconciliation and baseline/preflight reuse. Ensure old JSON defaults to no explicit selection and old maps cannot masquerade as profile-bearing maps. Cross-profile --from imports may carry exact validated bodies but must import no status/coverage/binaries.

Use a tiny fixture with custom Cargo profiles and an independently named nextest profile. A build script and test should prove the custom profile was compiled, and a nextest filter must continue excluding a deliberately failing test. Complete map, shared schema generation, a deliberate unviable mutant that forces attribution/rebuild, a direct opaque-return variant where practical, and normal test. Assert custom-profile output provenance, including any ordinary binary child paths. Verify flags override only their own config settings, changed profile refuses build/rebuild/test without altering saved map/state, remapping clears every old status, and old default JSON remains usable. Document usage and restrictions in RUNNING.md.

## Milestones

Milestone 1 yields a usable option with complete forwarding. Verify config parsing/precedence and generated Cargo versus nextest argv, then use a real CLI map to prove compilation uses the custom profile while nextest still uses project test policy.

Milestone 2 pins provenance and prevents stale reuse. Regression tests verify drift refusal before state writes, profile remaps resetting all statuses, default legacy compatibility, body-only imports and protected custom-profile staging. Source audit must find no production target/debug assumptions.

Milestone 3 completes acceptance and install. Run the focused tests first, then format, Clippy and all suites through mbx at nice 10. Commit only owned files on the current branch, request required review before pushing origin/master, verify exact-source-head CI and send the tested install handoff to session 5b9275ef422b8eb3146a4a11fb9dd1aa.

## Concrete Steps

Work from /home/jonathan/Projects/quicksilver/.mj/worktrees/0efccbcef11b71ab37df2f0e62e9cd9e. Use nice -n 10 mbx test --test campaign with the new profile test filter, then mbx fmt --check, nice -n 10 mbx clippy --all-targets -- -D warnings and nice -n 10 mbx test --all-targets. Preserve normal targets and mbx configuration. Store concise evidence logs under /tmp/qs-cargo-profile-* and record outcomes here.

## Validation and Acceptance

The CLI accepts --cargo-profile before or after the command; [cargo] profile works without a flag. Explicit profile overrides configured profile while explicit feature flags preserve configured profile and vice versa. A real map/build/test under a custom profile succeeds and records its string name; no-setting JSON records null or defaults to absent semantics. The chosen nextest profile still applies its filters/timeouts. Profile drift refuses before saved state/map changes. A remap under another profile retains validated bodies but resets every old build/verdict/compile-failure status. Old map versions with default compilation selection stay readable, while a profile-bearing legacy map is rejected. Cross-profile imports prove no verdict reuse. Staging uses fresh Cargo-reported artifacts, including custom-profile ordinary binaries, under the existing lease.

## Idempotence and Recovery

Use only disposable committed fixture repositories. Changing the configured profile requires an explicit map, then build again. A mismatched profile must not rewrite or delete existing map/state/journal. No Cargo target redirects, user-project profile definitions, environment hacks, mbx policies, cloud hosts or Bifrost engine files are changed. Custom profiles are defined only in disposable test fixtures. The independent identity-gate report remains the campaign agent's investigation.

## Interfaces and Dependencies

Use existing serde, clap and Command; no new dependency is needed. CargoProfile is a serde-transparent optional string with validate, apply(frontend) and require_match methods. CargoFile holds the existing feature configuration fields and optional profile. Add defaulted cargo_profile fields to persisted structures. Keep existing public feature-key functions as default-profile delegates when extending preflight keys.

Plan created 2026-11-05 for the campaign's explicit compilation-profile requirement after source and official Cargo/nextest documentation verification.

Implementation update 2026-11-05: focused CLI passes; full checks running. Added built-in resume provenance after audit found it bypassed captured-state checks. A mixed malformed/direct candidate test was separated to keep profile forwarding verification independent of attribution behavior; no unrelated engine fix was made.

Local acceptance completed 2026-11-05: 233 tests passed with two existing ignores, format/Clippy clean. The mixed custom-profile production target regression includes child-path assertions, lease/staging protection, missing-child refusal and built-in run JSON/resume provenance. Source is ready for committed review, push and exact-head CI.
