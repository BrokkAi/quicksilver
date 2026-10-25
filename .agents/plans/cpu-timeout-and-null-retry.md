# Judge timeouts by CPU and retry transient null failures

This living ExecPlan follows `.agents/PLANS.md`. Work happens in `/mnt/optane/quicksilver/qs-wt-a` on its detached HEAD. The parent will cherry-pick; do not push or change branches.

## Purpose / Big Picture

Tests on a loaded host can use the same CPU while taking much longer to finish. Record CPU during mapping and compare mutant CPU with that baseline, so load alone does not count as a kill. Users can repeat timeout and errored verdicts without rebuilding current binaries. Retry an unsuccessful null run once and describe signals accurately.

## Progress

- [x] (2026-10-25) Read issues #29 and #30 and inspect execution, map, and state code.
- [x] (2026-10-25) Implement #29 CPU accounting, timeout decisions, summaries, repeat selection, and documentation.
- [x] (2026-10-25) Validate #29: formatting clean; clippy only known build.rs warning; library 157 passed, 2 ignored; required integration suites passed. Checkpoint commit follows.
- [ ] Implement #30 null retry, signal reporting, and errored list.
- [ ] Validate and commit #30.

## Surprises & Discoveries

The initial GitHub read needed network escalation and then succeeded. TestInfo carries wall durations across both execution modes; an optional CPU value there can carry the map's separate CPU index into resolved tests without global mutable timing state.

## Decision Log

Use the parent's supplied design: normal runs get user and system CPU from wait4; deadline runs on Linux snapshot all group members' own and waited-child CPU before killing. CPU above three times baseline plus one second establishes runaway computation. Otherwise retry with four times the original budget. A second timeout below five percent CPU establishes blocking; other second timeouts are inconclusive. Keep configured budget minimums.

## Outcomes & Retrospective

Issue #29 is implemented and validated. Linux deadline accounting uses the larger of the group snapshot and reaped CPU, so a process exiting between the deadline and the snapshot does not lose already-accounted CPU. This preserves the snapshot for live descendants. The first sandboxed library run hit the existing socket-bind restriction; rerunning with host capabilities passed. Evidence goes in `/mnt/optane/tmp/qs-29-30-logs`.

## Context and Orientation

`src/exec/process.rs` launches and stops process groups. `src/exec/test.rs` resolves staged tests, records site hits, and confirms verdicts. `src/sitemap.rs` persists map data. `src/exec/schema.rs` supplies run mode tests. `src/exec/edited.rs` persists captured mutants and their staged generation. `src/cli.rs` and `src/lib.rs` expose test selection. Another worker owns survivors; avoid that feature and keep shared-file edits narrow.

## Plan of Work

First replace exact-run waits with one exclusive wait4 reaper and add measured CPU to runner results. Persist CPU keyed by test id in SiteMap, with a default for old maps and wall-duration fallback. Pass CPU through TestInfo to ResolvedTest. Remove calibration and its lock. Add deterministic fake-runner tests for every timeout branch. Save generation, activation, and direct-build metadata with tested verdicts, including journal replay, and implement repeatable `test --again timeouts|errored` intersected with mutant prefixes. Current generation binaries allow retesting; missing or stale binaries require build. Share summary formatting with run mode.

Then retry a nonpassing null run once in confirm_failure. Carry signal numbers in errors and name them in all messages; only QS deadline kills count as Timeout. Print errored ids and reasons at the end. Update README result descriptions and repeat examples.

## Concrete Steps

From the working directory run plain cargo through its existing mbx shim. Before each commit run `cargo fmt --check`, `nice -n 10 cargo clippy --all-targets`, `nice -n 10 cargo test --lib`, and `nice -n 10 cargo test --test edited --test engine --test schema`. Capture output in the log directory. Commit only changed files with a multiline rationale, the relevant Fixes line, and the requested coauthor trailer.

## Validation and Acceptance

Fake-runner unit tests prove first and second CPU runaways, blocked and inconclusive retries, and finishing retries that pass or fail. Process tests prove a busy descendant contributes deadline CPU and distinguish outside signals from QS stops. State tests prove retry restoration, stale binaries, old metadata, and prefix intersection. Existing edited, engine (including the infinite-loop mutant), and schema suites must pass, with no new clippy warnings beyond the known build.rs warning. Do not assert wall-clock timing on this host.

## Idempotence and Recovery

Checks can be repeated. Do not redirect cargo targets or alter shared worktrees. Keep changes on detached HEAD; the parent integrates the commits. Old maps and states deserialize through serde defaults; tests without recorded CPU use recorded wall duration.

## Artifacts and Notes

Issue text was read with `gh issue view 29 -R BrokkAi/quicksilver` and the corresponding command for 30. No build outputs belong in agent reports.

## Interfaces and Dependencies

Use existing libc for wait4, sysconf clock ticks, and process group signals. Runner closures return `(RunStatus, Duration, f64, String)`, where f64 is CPU seconds. Add optional recorded CPU to TestInfo and a defaulted CPU index to SiteMap. Tested records retain optional built metadata so old records can safely go Pending when repeated.

Plan updated after the #29 implementation to record passing checks and the deadline/exit race handling.
