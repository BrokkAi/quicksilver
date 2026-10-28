# Bifrost campaign audit

Records-only audit received 2026-10-28 from session
5b9275ef422b8eb3146a4a11fb9dd1aa. Original:
`/home/jonathan/Projects/bifrost/.mj/qs-report/campaign-totals.md`.
Supporting artifact links in the preserved report refer to that original
directory; commit and PR links identify durable sources. Current README
scope and subsequent clarifications are in
[real-project-results-evidence.md](real-project-results-evidence.md).

| Round | Mutated packages | Tested / cohort* | Caught | Missed | Gaps reviewed | Proven new-test kills | Bug groups in inventory | Dead functions |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| **Completed total** | **12 D** | **M** | **>=4052 D** | **>=3626 D** | **M** | **>=1220 D** | **>=233 D** | **>=133 D** |
| 1: core | 1 C | 920 C | 434 C | 475 C | 292 C | 60 C | >=3 D | >=6 C |
| 2: store | 2 C | 799 C | 188 C | 610 C | 352 C | 112 C | >=8 D | M |
| 3: rust | 1 C | 1098 C | 823 C | 274 C | 189 C | >=174 D | >=11 D | 45 C |
| 4: cpp | 1 C | 1477 C | M | M | 283 C | >=153 D | >=40 D | >=8 C |
| 5: jvm | 1 C | 1669 C | M | 408 C | 59 C | 57 D | >=11 D | 15 C |
| 6: python | 1 C | 818 C | 551 C | 266 C | 74 C | 74 C | >=20 D | 15 C |
| 7: js-ts | 1 C | 940 D | M | 332 D | >=213 D | M | >=28 D | 5 C |
| 8: csharp | 1 C | 664 D | 459 D | 205 D | 128 C | 101 D | >=21 D | 2 C |
| 9: php | 1 C | 442 D | 295 D | 147 D | 86 C | 84 D | >=27 D | 10 C |
| 10: ruby | 1 C | 343 D | 255 D | 88 D | 49 C | 44 D | >=17 D | 7 C |
| 11: go | 1 C | 275 D | 177 D | 98 D | 48 C | 45 D | >=14 D | 9 C |
| 12: analysis | 1 C | 1593 D | 870 D | 723 D | >=260 D | 316 D | >=33 D | >=11 C |
| 13: flow (ongoing) | 1 C | excluded | excluded | excluded | excluded | excluded | excluded | excluded |

C = confirmed from a surviving record (including explicit PR/session statements), D = derived as explained, M = missing. **Every >= is a lower bound, not an exact campaign total.** The full tested total is M. The round cohort records sum to 11,038 D, but that is not a certified distinct actually-tested total: core follow-up overlap is unresolved and early records include outcomes not separated from not-covered cases. *Rows 2, 3, 4, 5 and 7 display recorded cohort sizes, not certified actually-tested cardinalities; their JSON tested fields are M and recorded_cohort preserves the number. Caught and missed totals additionally omit missing round cells and therefore do not reconcile to tested. Bug cells are the exact counts in the explicitly enumerated, conservative inventory; they do not claim exhaustive campaign discovery. Five inventory entries are fixed and validated in ready PR #7832, not yet merged at this audit cutoff.

The records support **12 distinct completed mutated packages**, not ~20: the nine language adapter crates supplied extra tests but their code was not mutated. There are **12 additional packages whose tests were selected**, or 24 distinct mutated-or-extra-test packages. Flow is a thirteenth mutation target still in progress.

## Recommended matched cohort for the README

Use **rounds 8-11: C#, PHP, Ruby and Go**, the broadest cohort here with complete final outcome counts and complete map/build/test summaries for every tested generation. All counts below cover exactly these four mutated packages. The full 12-package audit remains useful for scope and minimum findings, but its complete tested/outcome/timing totals are missing.

| Cohort | Mutated packages | Package src Rust LOC D | Tested D | Caught D | Missed D | Proven gap kills D | Confirmed bug inventory D | Map seconds D | Build seconds D | Test seconds D |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| **Recommended: rounds 8-11** | **4** | **78764** | **1724** | **1186** | **538** | **274** | **79** | **3887.8** | **2738.4** | **11008.4** |
| Fallback: rounds 8-10 | 3 | 51225 | 1449 | 1009 | 440 | 229 | 65 | 2870.2 | 1813.3 | 7951.2 |

Recommended bug inventory is **79 confirmed groups**, each with a production fix and recorded failing-before regression (C#21+PHP27+Ruby17+Go14). This is an explicit conservative inventory, not an exact count of every later follow-up. The fallback inventory is 65 groups. Exact number of test functions added remains M for both; 274 and 229 count proven gap mutants killed, not test functions.

For rounds 8-11, the **summed recorded process wall time is 17634.6 seconds D** across 22 completed operations. For the 1,449-mutant fallback, **12634.7 seconds D** across 15 completed operations. Both include all A/B generations; Go also includes C. Map times include compilation, and builds are incremental after maps. Neither sum is the campaign's calendar duration or CPU time. Sources are the corresponding qs8/9/10/11-map.log, build-A/B[/C].log and test-A/B[/C].log summaries in campaign-remote-counts.json.

The four-package review has 311 test gaps, 36 suspected defects, plus exact C#/PHP/Ruby subclasses and 27 Go equivalent-or-unreachable items whose split is M. Do not claim a complete separate equivalent/defensive breakdown for the four-package cohort. The three-package fallback has complete class totals: 263 gaps, 81 equivalent, 44 defensive, 18 observability, 32 suspected, two covered-since. Separate timeout/error/not-covered outcomes are zero for both cohorts; the one C# CPU-timeout catch is already included in caught.

### Matched cohort timing per round

| Round | Mutated src Rust LOC D | Completed generations | Map seconds D | Build seconds D | Test seconds D |
|---|---:|---|---:|---:|---:|
| 8: csharp | 25840 | A, B | 948.9 | 802.8 | 4977 |
| 9: php | 15133 | A, B | 964.2 | 527.9 | 1709.6 |
| 10: ruby | 10252 | A, B | 957.1 | 482.6 | 1264.6 |
| 11: go | 27539 | A, B, C | 1017.6 | 925.1 | 3057.2 |

The map is run once per row; every tested mutant generation in that row contributes one build and test process. No cumulative JSON lengths or retry processes are added.

### LOC at one stated commit

Counted tracked Rust source at **54f6c8dba9c575c3d8e0a21d152c8b2bad006787** (clean HEAD = origin/master). Method is equivalent to `git ls-files -z 'crates/<crate>/src/**.rs' | xargs -0 cat | wc -l`: count newline bytes once across the selected tracked files, never sum xargs wc grand totals. Includes inline tests. This snapshot is later than the historical mutation bases, so the sizes describe these package scopes at one current commit, not their exact historic base sizes.

| Scope | Rust LOC D |
|---|---:|
| 12 completed mutation targets, src only | 1150970 |
| Their 12 extra-test-only package owners, src only | 965253 |
| Recommended four mutated packages, src only | 78764 |
| Recommended cohort's five extra-test-only owners, src only | 211182 |
| C#/PHP/Ruby fallback mutated packages, src only | 51225 |
| Fallback's four extra-test-only owners, src only | 137178 |
| Three fallback adapter crates alone, tests-only extra owners | 78895 |
| Whole workspace: all tracked *.rs, including tests and fixtures | 3396512 |
| All crates/src plus root src, excludes out-of-src tests/fixtures | 2605235 |

| Mutated package | Tracked src Rust LOC D |
|---|---:|
| brokk-bifrost-core | 81588 |
| brokk-bifrost-store | 436164 |
| brokk-bifrost-rust | 86749 |
| brokk-bifrost-cpp | 88899 |
| brokk-bifrost-jvm | 95241 |
| brokk-bifrost-python | 33163 |
| brokk-bifrost-js-ts | 38565 |
| brokk-bifrost-csharp | 25840 |
| brokk-bifrost-php | 15133 |
| brokk-bifrost-ruby | 10252 |
| brokk-bifrost-go | 27539 |
| brokk-bifrost-analysis | 211837 |

| Extra-test-only package owner | Tracked src Rust LOC D |
|---|---:|
| brokk-bifrost | 58283 |
| brokk-bifrost-rust-analysis | 73641 |
| brokk-bifrost-cpp-analysis | 49327 |
| brokk-bifrost-jvm-analysis | 113874 |
| brokk-bifrost-python-analysis | 64814 |
| brokk-bifrost-js-ts-analysis | 34848 |
| brokk-bifrost-csharp-analysis | 37306 |
| brokk-bifrost-php-analysis | 20674 |
| brokk-bifrost-ruby-analysis | 20915 |
| brokk-bifrost-go-analysis | 74004 |
| brokk-bifrost-tools | 213585 |
| brokk-bifrost-rql | 203982 |

No -analysis crate is included in mutated LOC for the fallback: the mutation maps and captured generations target the base crates only. All nine -analysis crates are listed separately as extra-test owners in the broader campaign. The root brokk-bifrost package supplies the six root suites; its src LOC does not include those out-of-src test suites. The whole-workspace 3,396,512 lines confirms the approximate 3.3M figure and must not be substituted for a mutated-package cohort size. Machine-readable details: [campaign-loc.json](campaign-loc.json).

## Scope, definitions and defects

Records-only audit on 2026-10-28. No map, build, test, benchmark or cloud launch was performed. Existing EC2 files were read from outside the busy clone; only configuration text was read inside it. Original early /tmp logs are not present on this host after the reboot; their retained summaries and session statements are cited, rather than declared recovered logs. The 712k LOC value in the old showcase is not used: requester corrected workspace size to 3.3M lines, which was not remeasured here.

**Bugs confirmed: 233 distinct groups in this audited inventory (228 merged, five fixed in ready #7832).** The count requires a production correction plus a regression recorded as failing on old unmutated code. Equivalent mutants, test gaps, dead code, test-only defects, behavior-preserving rewrites, new-behavior rework and mere suspicions do not qualify. This is a conservative inventory, not an exhaustive claim about every follow-up PR opened during the campaign. Each group has its commit, PR and proof source in [campaign-defect-ledger.json](campaign-defect-ledger.json); the 65 C#/PHP/Ruby entries reuse [bug-ledger.json](bug-ledger.json) and [showcase-facts.md](showcase-facts.md). New ledger entries identify integration commits where an exact lane hash was not needed to identify the landed fix.

Excluded follow-up scope includes Rust wrapper-design/macro campaigns and their later fixes, #6953 real-corpus crash fixes, #7324 shared initializer classification, #7831 overlay fixes and later performance regression repairs. These are not asserted to be nonbugs: a full deduplicated red-test inventory of all follow-ups is missing. No total in this document incorporates Quicksilver or mbx tool defects.

## Per-round provenance and caveats

### Round 1: core

Sources: [#6656](https://github.com/BrokkAi/bifrost-dev/pull/6656) ([saved body](pr-6656.json)), [#6673](https://github.com/BrokkAi/bifrost-dev/pull/6673) ([saved body](pr-6673.json)), [#6674](https://github.com/BrokkAi/bifrost-dev/pull/6674) ([saved body](pr-6674.json)).

Initial run only. Later 849-mutant recheck overlaps this cohort. Coverage suite grew 460 to 532 (net +72), then to 543 (+11), not an exact count of tests added.

Outcome basis: tested - prior-showcase.json round 1; session message 258; caught - same initial run; missed - same initial run. Other outcomes: {"timeout":11,"error":0,"not_covered":0} C (prior-showcase.json; timeout classification predates later fixes).

Tests added: M. Exact additions, extensions and deletions not inventoried.

### Round 2: store

Sources: [#6709](https://github.com/BrokkAi/bifrost-dev/pull/6709) ([saved body](pr-6709.json)), [#6719](https://github.com/BrokkAi/bifrost-dev/pull/6719) ([saved body](pr-6719.json)), [#6727](https://github.com/BrokkAi/bifrost-dev/pull/6727) ([saved body](pr-6727.json)), [#6731](https://github.com/BrokkAi/bifrost-dev/pull/6731) ([saved body](pr-6731.json)), [#6734](https://github.com/BrokkAi/bifrost-dev/pull/6734) ([saved body](pr-6734.json)), [#6736](https://github.com/BrokkAi/bifrost-dev/pull/6736) ([saved body](pr-6736.json)), [#6737](https://github.com/BrokkAi/bifrost-dev/pull/6737) ([saved body](pr-6737.json)).

799 covers store only. Core follow-up tested 916 on an overlapping original cohort and added 30 nested-visitor captures; cross-run ID union is missing, so 916 and 30 are not added to the total. Store after coverage: 269 caught and 529 survivors; these are repeat outcomes, not additional mutants. #6719 also records 30 hand-applied IR kills whose overlap with the 112 is unknown. Store tests 868 to 896 (+28 net), then 919 (+23 net).

Outcome basis: tested - PR #6734 and session 710; baseline cohort; caught - session 576, initial baseline; missed - session 576, initial baseline. Other outcomes: M (799 - 188 - 610 = 1; type missing).

Tests added: M. Exact additions, extensions and deletions not inventoried.

### Round 3: rust

Sources: [#6812](https://github.com/BrokkAi/bifrost-dev/pull/6812) ([saved body](pr-6812.json)), [#6813](https://github.com/BrokkAi/bifrost-dev/pull/6813) ([saved body](pr-6813.json)), [#6816](https://github.com/BrokkAi/bifrost-dev/pull/6816) ([saved body](pr-6816.json)), [#6822](https://github.com/BrokkAi/bifrost-dev/pull/6822) ([saved body](pr-6822.json)), [#6842](https://github.com/BrokkAi/bifrost-dev/pull/6842) ([saved body](pr-6842.json)), [#6845](https://github.com/BrokkAi/bifrost-dev/pull/6845) ([saved body](pr-6845.json)), [#6853](https://github.com/BrokkAi/bifrost-dev/pull/6853) ([saved body](pr-6853.json)), [#6863](https://github.com/BrokkAi/bifrost-dev/pull/6863) ([saved body](pr-6863.json)).

275 reviewed includes an outcome not reconciled to the 274 missed. #6863 claims 187 proven closures but its theme table sums to 174; retain both and count only the supported 174 in the lower-bound total.

Outcome basis: tested - campaign-source-memory.md; session 820 confirms all built, later completed survivor review; caught - campaign-source-memory.md expanded map; missed - campaign-source-memory.md expanded map. Other outcomes: M (One of 1098 is not described by 823 + 274).

Tests added: M. Exact additions, extensions and deletions not inventoried.

### Round 4: cpp

Sources: [#6848](https://github.com/BrokkAi/bifrost-dev/pull/6848) ([saved body](pr-6848.json)), [#6849](https://github.com/BrokkAi/bifrost-dev/pull/6849) ([saved body](pr-6849.json)), [#6850](https://github.com/BrokkAi/bifrost-dev/pull/6850) ([saved body](pr-6850.json)), [#6891](https://github.com/BrokkAi/bifrost-dev/pull/6891) ([saved body](pr-6891.json)), [#6900](https://github.com/BrokkAi/bifrost-dev/pull/6900) ([saved body](pr-6900.json)), [#6936](https://github.com/BrokkAi/bifrost-dev/pull/6936) ([saved body](pr-6936.json)).

1442 first-batch mutants plus 36 proposed supplement edits is not 1478 distinct mutants: use the completed 1477 catalog record. No raw ID union survives locally. #6900 says about 170 kills; the seven exact lane counts sum to 153. 99 observations are not survivor classifications.

Outcome basis: tested - campaign-source-memory.md completed round catalog; prior-showcase.json agrees; caught - Final raw verdict summary not recovered; missed - 502 reviewed survivors recorded, exact missed/other split missing. Other outcomes: M (Final timeout/error/not-covered outcomes missing).

Tests added: M. Exact additions, extensions and deletions not inventoried.

### Round 5: jvm

Sources: [#6890](https://github.com/BrokkAi/bifrost-dev/pull/6890) ([saved body](pr-6890.json)), [#6939](https://github.com/BrokkAi/bifrost-dev/pull/6939) ([saved body](pr-6939.json)), [#6946](https://github.com/BrokkAi/bifrost-dev/pull/6946) ([saved body](pr-6946.json)).

About 22% of caught verdicts were wall-clock timeout kills on a loaded host (session 2137, Quicksilver #29); count and corrected catch rate missing. The 59 reviewed gaps are not 59 proven kills.

Outcome basis: tested - PR #6939 and session 1819; caught - 1669 - 408 = 1261 non-survivors includes possible other outcomes; do not call all caught; missed - PR #6939 survivor review. Other outcomes: M (An earlier 1294-mutant progress report had four other outcomes; final disposition missing).

Tests added: M. Exact additions, extensions and deletions not inventoried.

### Round 6: python

Sources: [#6955](https://github.com/BrokkAi/bifrost-dev/pull/6955) ([saved body](pr-6955.json)), [#6960](https://github.com/BrokkAi/bifrost-dev/pull/6960) ([saved body](pr-6960.json)), [#6999](https://github.com/BrokkAi/bifrost-dev/pull/6999) ([saved body](pr-6999.json)).

CPU-based timeout confirmation installed before this round at Quicksilver ab87eee (#29-#31). 23/551 timeout catches must not be described as 23 inflated false kills.

Outcome basis: tested - #6999 and session 2495; caught - same run; 23 included timeout catches; missed - same run. Other outcomes: {"error":1,"timeout":0,"not_covered":0} C (session 2495 accounts for all 818; the 23 timeout catches are in caught, not other).

Tests added: M. Exact additions, extensions and deletions not inventoried.

### Round 7: js-ts

Sources: [#7023](https://github.com/BrokkAi/bifrost-dev/pull/7023) ([saved body](pr-7023.json)), [#7218](https://github.com/BrokkAi/bifrost-dev/pull/7218) ([saved body](pr-7218.json)).

940 is a reconstruction from two distinct build generations, not an A+B cumulative JSON sum. Nine retests add zero mutants. #7218 says about 205 proven catches; an exact ID count is missing. Round ran locally on precision-3260, not EC2: session 3432 corrects earlier showcase host text.

Outcome basis: tested - 753 generation A plus 187 generation B from session 3289; completed B and survivor review at 3369-3387; caught - Full final verdict totals after error rejudging missing; A alone caught 463; missed - 323 initially reviewed survivors (3387) plus nine formerly errored mutants rejudged missed (3684). Other outcomes: M (A alone: 26 errors and two not covered (3312). Nine later errored mutants became missed; full final outcome inventory missing).

Tests added: M. Exact additions, extensions and deletions not inventoried.

### Round 8: csharp

Sources: [#7157](https://github.com/BrokkAi/bifrost-dev/pull/7157) ([saved body](pr-7157.json)), [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) ([saved body](pr-7320.json)).

Reuses the existing audit without reruns; exact bug ledger and exclusions remain in showcase-facts.md.

Outcome basis: tested - showcase-facts.md generation log sums and cumulative JSON IDs; caught - same audit; missed - same audit. Other outcomes: {"timeout":0,"error":0,"not_covered":0} C (showcase-facts.md and outcome-json-summary.txt).

Tests added: M. Exact additions, extensions and deletions not inventoried.

### Round 9: php

Sources: [#7188](https://github.com/BrokkAi/bifrost-dev/pull/7188) ([saved body](pr-7188.json)), [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) ([saved body](pr-7228.json)).

Reuses the existing audit without reruns; exact bug ledger and exclusions remain in showcase-facts.md.

Outcome basis: tested - showcase-facts.md generation log sums and cumulative JSON IDs; caught - same audit; missed - same audit. Other outcomes: {"timeout":0,"error":0,"not_covered":0} C (showcase-facts.md and outcome-json-summary.txt).

Tests added: M. Exact additions, extensions and deletions not inventoried.

### Round 10: ruby

Sources: [#7216](https://github.com/BrokkAi/bifrost-dev/pull/7216) ([saved body](pr-7216.json)), [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) ([saved body](pr-7290.json)).

Reuses the existing audit without reruns; exact bug ledger and exclusions remain in showcase-facts.md.

Outcome basis: tested - showcase-facts.md generation log sums and cumulative JSON IDs; caught - same audit; missed - same audit. Other outcomes: {"timeout":0,"error":0,"not_covered":0} C (showcase-facts.md and outcome-json-summary.txt).

Tests added: M. Exact additions, extensions and deletions not inventoried.

### Round 11: go

Sources: [#7248](https://github.com/BrokkAi/bifrost-dev/pull/7248) ([saved body](pr-7248.json)), [#7317](https://github.com/BrokkAi/bifrost-dev/pull/7317) ([saved body](pr-7317.json)).



Outcome basis: tested - A 70 + B 151 + C 54; cumulative C JSON has 275 unique IDs; caught - 45+95+37; cumulative JSON; missed - 25+56+17; cumulative JSON. Other outcomes: {"timeout":0,"error":0,"not_covered":0} C (all three completed logs and cumulative JSON).

Tests added: M. Exact additions, extensions and deletions not inventoried.

### Round 12: analysis

Sources: [#7367](https://github.com/BrokkAi/bifrost-dev/pull/7367) ([saved body](pr-7367.json)), [#7482](https://github.com/BrokkAi/bifrost-dev/pull/7482) ([saved body](pr-7482.json)), [#7574](https://github.com/BrokkAi/bifrost-dev/pull/7574) ([saved body](pr-7574.json)), [#7646](https://github.com/BrokkAi/bifrost-dev/pull/7646) ([saved body](pr-7646.json)), [#7784](https://github.com/BrokkAi/bifrost-dev/pull/7784) ([saved body](pr-7784.json)), [#7832](https://github.com/BrokkAi/bifrost-dev/pull/7832) ([saved body](pr-7832.json)).

T4: corrected session 6513 says 459 compilable A mutants (462 authored, three unviable), versus older build log 460. A review batches were 158+31=189 distinct survivors; A caught = 459-189=270. B =198, caught101/missed97 (6753). Thus T4 =657/371/286. Final shard ID union no longer available to verify the 459/460 discrepancy independently. 66 accidental duplicate A verdicts on the first B host are excluded (6436). #7832 includes a re-review of 51 already counted A survivors; those are not new mutants or new reviews.

Outcome basis: tested - 471 T1 + 214 T2 + 251 T3 + 657 T4; generation/ID notes below; caught - 266 T1 + 78 T2 + 155 T3 + 371 T4; T4 derived from completed review cardinality; missed - 205 + 136 + 96 + 286; T4 A 189 reviewed + B 97. Other outcomes: M (T1-T3 logs zero others; final T4 shard timeout/error counts missing; three build-unviable mutants excluded from tested).

Tests added: M. Exact additions, extensions and deletions not inventoried.

### Round 12 tranche arithmetic

| Tranche | Tested | Caught | Missed | Proven new-test kills | Notes |
|---|---:|---:|---:|---:|---|
| 1 | 471 C | 266 C | 205 C | 100 C | A357 + B114; #7482 |
| 2 | 214 C | 78 C | 136 C | 65 D | failed A build staged no runnable tests; successful B tested A134+B80 together; 97 caught minus 32 existing tests |
| 3 | 251 D | 155 D | 96 D | 42 D | A182+B69; final T3B JSON is cumulative T2+T3 (465), not B69 |
| 4 | 657 D | 371 D | 286 D | 109 D | corrected A459+B198, see discrepancy note above; A57+B52 new-test kills |
| Total | 1593 D | 870 D | 723 D | 316 D | final shard other outcomes not independently recovered |

T1 final JSON IDs and T2/T3 final JSON IDs have zero overlap (derived read-only set comparison). T2/T3 cumulative final has 465 = 214+182+69, caught233 =78+110+45, missed232 =136+72+24. T4 original process stopped after 206 decided, then shards covered the rest. Its partial A1 JSON has 291 = 206+85; do not add that to 459. B shard lists are three disjoint sets of 66 IDs (198 total). Final A/B shard logs on the three terminated clones were not found on the surviving host; classification-based cardinalities replace missing full verdict files only where explicitly stated.

## Survivor classifications

| Round | Test gap | Equivalent | Defensive/unreachable | Observability | Suspected defect | Other |
|---|---:|---:|---:|---:|---:|---|
| 1 | 292 C | 36 C | 58 C | 79 C | M | {"downstream_only":11,"timeout_noise":10} C |
| 2 | 352 C | M | M | M | 5 C | M |
| 3 | 189 C | M | M | M | M | M |
| 4 | 283 C | M | M | M | M | M |
| 5 | 59 C | M | M | M | M | {"resolution_deferred":314,"unpartitioned_remainder":35} C |
| 6 | 74 C | M | M | M | M | {"resolution_deferred":130} C |
| 7 | >=213 D | M | M | M | 17 D | M |
| 8 | 128 C | 33 C | 22 C | 12 C | 9 C | 1 C |
| 9 | 86 C | 30 C | 14 C | 5 C | 12 C | 0 C |
| 10 | 49 C | 18 C | 8 C | 1 C | 11 C | 1 C |
| 11 | 48 C | M | M | M | 4 C | {"resolution_deferred":19,"unpartitioned_equivalent_or_unreachable":27} C |
| 12 | >=260 D | M | M | M | M | M |

These are review-item classifications, not bug counts. Core reviewed 486 including 11 timeouts. Store reviewed 529 after its coverage PR, not the initial 610. Rust reviewed 275, C++ 502, JVM 408, Python 266, JS/TS 323 plus nine late survivors, C#205/PHP147/Ruby88, Go98. Analysis T1 reviewed205, T2 136, T3 96 and T4 286; the last B review reclassified 51 A survivors, so 148 =97 B+51 A is not 148 new survivors. The T4 B-plus-re-reviewed-A ledger has 55 gaps, 53 observability, 19 covered-since, 12 equivalent, five suspected and four defensive, confirmed by unique IDs in .mj/qs12t4b/group{1..5}.out.json; it is not added again to the initial A ledger.

## Package lists supporting the counts

Completed mutated packages (12):

- `brokk-bifrost-core`
- `brokk-bifrost-store`
- `brokk-bifrost-rust`
- `brokk-bifrost-cpp`
- `brokk-bifrost-jvm`
- `brokk-bifrost-python`
- `brokk-bifrost-js-ts`
- `brokk-bifrost-csharp`
- `brokk-bifrost-php`
- `brokk-bifrost-ruby`
- `brokk-bifrost-go`
- `brokk-bifrost-analysis`

Extra-test-only package owners (12):

- `brokk-bifrost`
- `brokk-bifrost-rust-analysis`
- `brokk-bifrost-cpp-analysis`
- `brokk-bifrost-jvm-analysis`
- `brokk-bifrost-python-analysis`
- `brokk-bifrost-js-ts-analysis`
- `brokk-bifrost-csharp-analysis`
- `brokk-bifrost-php-analysis`
- `brokk-bifrost-ruby-analysis`
- `brokk-bifrost-go-analysis`
- `brokk-bifrost-tools`
- `brokk-bifrost-rql`

| Round | Mutated package(s) | Extra test targets (suite targets share their root package) |
|---|---|---|
| 1 | brokk-bifrost-core | none (core own tests only) |
| 2 | brokk-bifrost-store, brokk-bifrost-core | brokk-bifrost::suite_persistence |
| 3 | brokk-bifrost-rust | brokk-bifrost-rust-analysis |
| 4 | brokk-bifrost-cpp | brokk-bifrost-cpp-analysis, brokk-bifrost::suite_issues, brokk-bifrost::suite_usages, brokk-bifrost::suite_symbols, brokk-bifrost::suite_analyzers, brokk-bifrost::suite_cross_language |
| 5 | brokk-bifrost-jvm | brokk-bifrost-jvm-analysis, brokk-bifrost::suite_issues, brokk-bifrost::suite_usages, brokk-bifrost::suite_symbols, brokk-bifrost::suite_analyzers, brokk-bifrost::suite_cross_language |
| 6 | brokk-bifrost-python | brokk-bifrost-python-analysis, brokk-bifrost::suite_issues, brokk-bifrost::suite_usages, brokk-bifrost::suite_symbols, brokk-bifrost::suite_analyzers, brokk-bifrost::suite_cross_language, brokk-bifrost::suite_semantic |
| 7 | brokk-bifrost-js-ts | brokk-bifrost-js-ts-analysis, brokk-bifrost::suite_issues, brokk-bifrost::suite_usages, brokk-bifrost::suite_symbols, brokk-bifrost::suite_analyzers, brokk-bifrost::suite_cross_language, brokk-bifrost::suite_semantic |
| 8 | brokk-bifrost-csharp | brokk-bifrost-csharp-analysis, brokk-bifrost::suite_issues, brokk-bifrost::suite_usages, brokk-bifrost::suite_symbols, brokk-bifrost::suite_analyzers, brokk-bifrost::suite_cross_language, brokk-bifrost::suite_semantic |
| 9 | brokk-bifrost-php | brokk-bifrost-php-analysis, brokk-bifrost::suite_issues, brokk-bifrost::suite_usages, brokk-bifrost::suite_symbols, brokk-bifrost::suite_analyzers, brokk-bifrost::suite_cross_language, brokk-bifrost::suite_semantic |
| 10 | brokk-bifrost-ruby | brokk-bifrost-ruby-analysis, brokk-bifrost::suite_issues, brokk-bifrost::suite_usages, brokk-bifrost::suite_symbols, brokk-bifrost::suite_analyzers, brokk-bifrost::suite_cross_language, brokk-bifrost::suite_semantic |
| 11 | brokk-bifrost-go | brokk-bifrost-go-analysis, brokk-bifrost::suite_issues, brokk-bifrost::suite_usages, brokk-bifrost::suite_symbols, brokk-bifrost::suite_analyzers, brokk-bifrost::suite_cross_language, brokk-bifrost::suite_semantic |
| 12 | brokk-bifrost-analysis | brokk-bifrost-tools, brokk-bifrost-rust-analysis, brokk-bifrost-cpp-analysis, brokk-bifrost-jvm-analysis, brokk-bifrost-python-analysis, brokk-bifrost-js-ts-analysis, brokk-bifrost-csharp-analysis, brokk-bifrost-php-analysis, brokk-bifrost-ruby-analysis, brokk-bifrost-go-analysis, brokk-bifrost::suite_issues, brokk-bifrost::suite_usages, brokk-bifrost::suite_symbols, brokk-bifrost::suite_analyzers, brokk-bifrost::suite_cross_language, brokk-bifrost::suite_semantic, brokk-bifrost::suite_smells, brokk-bifrost-rql |

Round 12 tranche 1 did not include smells/RQL extras until the tranche 2 remap. Round 13 flow extra owners are policy, rql, runtime, semantic-packs, mcp and the root package; their run is excluded from completed-package counts. Extra-test package counts describe Quicksilver map/test target sets, not every package used later in fix validation. Test selection does not prove every selected test passed: map summaries retain non-green baseline tests.

## Recorded wall time, with exact process coverage

| Recorded completed successful processes | Map wall seconds | Build wall seconds | Test wall seconds | Sequential sum |
|---|---:|---:|---:|---:|
| Selected coverage listed below | >=15866.7 D | >=4171.4 D | >=66840.7 D | 86878.8 s D |

This is a **partial sequential sum of recorded process wall times**, not CPU time, not calendar elapsed time, and not a cold-build benchmark. It excludes authoring, reviewing, fixing and validation. Operations on different hosts can overlap. Map times already include their internal compilation; embedded `compiled ... in ...` lines are not added. Later builds were incremental after maps. Repeated map processes count as real time spent, but duplicate `qs8-map-tail.log` is the same map and is excluded.

| Operation/run | Wall seconds | Coverage/source |
|---|---:|---|
| qs10-build-A.log (build) | 318.6 C | /home/ubuntu/qs10-build-A.log; build: 260 new, 260 untested in total, 0 unviable (generation 1, 3 cargo invocation(s), 318.6s) |
| qs10-build-B.log (build) | 164 C | /home/ubuntu/qs10-build-B.log; build: 83 new, 83 untested in total, 0 unviable (generation 2, 1 cargo invocation(s), 164.0s) |
| qs10-map.log (map) | 957.1 C | /home/ubuntu/qs10-map.log; map: 346 sites (0 functions without a site), 8122 tests (8115 green), 308 sites entered by at least one test, 957.1s |
| qs10-test-A.log (test) | 837.8 C | /home/ubuntu/qs10-test-A.log; tested 260 mutant(s) in 837.8s: 196 caught, 64 missed, 0 not covered, 0 timeout, 0 errored |
| qs10-test-B.log (test) | 426.8 C | /home/ubuntu/qs10-test-B.log; tested 83 mutant(s) in 426.8s: 59 caught, 24 missed, 0 not covered, 0 timeout, 0 errored |
| qs11-build-A.log (build) | 425.3 C | /home/ubuntu/qs11-build-A.log; build: 70 new, 70 untested in total, 0 unviable (generation 1, 3 cargo invocation(s), 425.3s) |
| qs11-build-B.log (build) | 339.4 C | /home/ubuntu/qs11-build-B.log; build: 151 new, 151 untested in total, 0 unviable (generation 2, 3 cargo invocation(s), 339.4s) |
| qs11-build-C.log (build) | 160.4 C | /home/ubuntu/qs11-build-C.log; build: 54 new, 54 untested in total, 0 unviable (generation 3, 1 cargo invocation(s), 160.4s) |
| qs11-map.log (map) | 1017.6 C | /home/ubuntu/qs11-map.log; map: 706 sites (1 functions without a site), 8446 tests (8445 green), 660 sites entered by at least one test, 1017.6s |
| qs11-test-A.log (test) | 801.4 C | /home/ubuntu/qs11-test-A.log; tested 70 mutant(s) in 801.4s: 45 caught, 25 missed, 0 not covered, 0 timeout, 0 errored |
| qs11-test-B.log (test) | 1425.5 C | /home/ubuntu/qs11-test-B.log; tested 151 mutant(s) in 1425.5s: 95 caught, 56 missed, 0 not covered, 0 timeout, 0 errored |
| qs11-test-C.log (test) | 830.3 C | /home/ubuntu/qs11-test-C.log; tested 54 mutant(s) in 830.3s: 37 caught, 17 missed, 0 not covered, 0 timeout, 0 errored |
| qs12-build-A.log (build) | 544.5 C | /home/ubuntu/qs12-build-A.log; build: 357 new, 357 untested in total, 0 unviable (generation 1, 5 cargo invocation(s), 544.5s) |
| qs12-build-B.log (build) | 134.5 C | /home/ubuntu/qs12-build-B.log; build: 114 new, 114 untested in total, 0 unviable (generation 2, 1 cargo invocation(s), 134.5s) |
| qs12-build-T2B.log (build) | 233.4 C | /home/ubuntu/qs12-build-T2B.log; build: 80 new, 214 untested in total, 0 unviable (generation 1, 1 cargo invocation(s), 233.4s) |
| qs12-build-T3A.log (build) | 193.2 C | /home/ubuntu/qs12-build-T3A.log; build: 182 new, 182 untested in total, 0 unviable (generation 2, 1 cargo invocation(s), 193.2s) |
| qs12-build-T3B.log (build) | 165.4 C | /home/ubuntu/qs12-build-T3B.log; build: 69 new, 69 untested in total, 0 unviable (generation 3, 1 cargo invocation(s), 165.4s) |
| qs12-build-T4A.log (build) | 162 C | /home/ubuntu/qs12-build-T4A.log; build: 0 new, 460 untested in total, 0 unviable (generation 2, 1 cargo invocation(s), 162.0s) |
| qs12-build-T4A.try1.log (build, failed attempt; excluded from sum) | 515.7 C | /home/ubuntu/qs12-build-T4A.try1.log; build: 460 new, 460 untested in total, 3 unviable (generation 1, 5 cargo invocation(s), 515.7s) |
| qs12-map.log (map) | 1206.4 C | /home/ubuntu/qs12-map.log; map: 4955 sites (176 functions without a site), 12355 tests (12352 green), 4214 sites entered by at least one test, 1206.4s |
| qs12-test-A.log (test) | 11650.2 C | /home/ubuntu/qs12-test-A.log; tested 357 mutant(s) in 11650.2s: 210 caught, 147 missed, 0 not covered, 0 timeout, 0 errored |
| qs12-test-B.log (test) | 4767 C | /home/ubuntu/qs12-test-B.log; tested 114 mutant(s) in 4767.0s: 56 caught, 58 missed, 0 not covered, 0 timeout, 0 errored |
| qs12-test-T2B.log (test) | 2413 C | /home/ubuntu/qs12-test-T2B.log; tested 214 mutant(s) in 2413.0s: 78 caught, 136 missed, 0 not covered, 0 timeout, 0 errored |
| qs12-test-T3A.log (test) | 11830.6 C | /home/ubuntu/qs12-test-T3A.log; tested 182 mutant(s) in 11830.6s: 110 caught, 72 missed, 0 not covered, 0 timeout, 0 errored |
| qs12-test-T3B.log (test) | 5731.8 C | /home/ubuntu/qs12-test-T3B.log; tested 69 mutant(s) in 5731.8s: 45 caught, 24 missed, 0 not covered, 0 timeout, 0 errored |
| qs12-test-T4A1.log (test) | 18411.8 C | /home/ubuntu/qs12-test-T4A1.log; tested 85 mutant(s) in 18411.8s: 33 caught, 52 missed, 0 not covered, 0 timeout, 0 errored |
| qs12t2-map.log (map) | 1182.6 C | /home/ubuntu/qs12t2-map.log; map: 4963 sites (176 functions without a site), 13176 tests (13169 green), 4339 sites entered by at least one test, 1182.6s |
| qs12t4-map-nogo.log (map) | 1184.3 C | /home/ubuntu/qs12t4-map-nogo.log; map: 5018 sites (180 functions without a site), 13303 tests (13283 green), 4415 sites entered by at least one test, 1184.3s |
| qs12t4-map.log (map) | 1195.6 C | /home/ubuntu/qs12t4-map.log; map: 5018 sites (180 functions without a site), 13303 tests (13300 green), 4417 sites entered by at least one test, 1195.6s |
| qs8-build-A.log (build) | 642.5 C | /home/ubuntu/qs8-build-A.log; build: 521 new, 521 untested in total, 0 unviable (generation 1, 5 cargo invocation(s), 642.5s) |
| qs8-build-B.log (build) | 160.3 C | /home/ubuntu/qs8-build-B.log; build: 143 new, 143 untested in total, 0 unviable (generation 2, 1 cargo invocation(s), 160.3s) |
| qs8-map.log (map) | 948.9 C | /home/ubuntu/qs8-map.log; map: 796 sites (0 functions without a site), 8177 tests (8174 green), 657 sites entered by at least one test, 948.9s |
| qs8-test-A.log (test) | 3547.7 C | /home/ubuntu/qs8-test-A.log; tested 521 mutant(s) in 3547.7s: 371 caught (1 by timeout, 0 by crash), 150 missed, 0 not covered, 0 timeout, 0 errored |
| qs8-test-B.log (test) | 1429.3 C | /home/ubuntu/qs8-test-B.log; tested 143 mutant(s) in 1429.3s: 88 caught, 55 missed, 0 not covered, 0 timeout, 0 errored |
| qs9-build-A.log (build) | 363.6 C | /home/ubuntu/qs9-build-A.log; build: 321 new, 321 untested in total, 0 unviable (generation 1, 3 cargo invocation(s), 363.6s) |
| qs9-build-B.log (build) | 164.3 C | /home/ubuntu/qs9-build-B.log; build: 121 new, 121 untested in total, 0 unviable (generation 2, 1 cargo invocation(s), 164.3s) |
| qs9-map.log (map) | 964.2 C | /home/ubuntu/qs9-map.log; map: 452 sites (0 functions without a site), 8109 tests (8102 green), 414 sites entered by at least one test, 964.2s |
| qs9-test-A.log (test) | 1041.5 C | /home/ubuntu/qs9-test-A.log; tested 321 mutant(s) in 1041.5s: 223 caught, 98 missed, 0 not covered, 0 timeout, 0 errored |
| qs9-test-B.log (test) | 668.1 C | /home/ubuntu/qs9-test-B.log; tested 121 mutant(s) in 668.1s: 72 caught, 49 missed, 0 not covered, 0 timeout, 0 errored |
| round-2-recorded-map (map) | 41.5 C | prior-showcase.json: /tmp/qs2/core-map.log; Only this recorded completed process; original log no longer located |
| round-3-recorded-map (map) | 281.5 C | prior-showcase.json: /tmp/qs3/rust-map2.log; Only this recorded completed process; original log no longer located |
| round-4-recorded-map (map) | 1754.5 C | prior-showcase.json: /tmp/qs4/cpp-map2.log; Only this recorded completed process; original log no longer located |
| round-5-recorded-map (map) | 1074.5 C | prior-showcase.json: /tmp/qs5/jvm-map.log; Only this recorded completed process; original log no longer located |
| round-6-recorded-map (map) | 4058 C | prior-showcase.json: /tmp/qs6/python-map.log; Only this recorded completed process; original log no longer located |
| round-1-recorded-test (test) | 1027.9 C | prior-showcase.json: /tmp/qs-core/test.log; Only this recorded completed process; original log no longer located |

The completed map with no Go toolchain (`qs12t4-map-nogo.log`) is included as a distinct, superseded map process. Failed completed build attempt `qs12-build-T4A.try1.log` has 515.7 s and three unviable mutants; it is listed separately and excluded from the successful-process sum. `qs12-build-T2A.log` ends in a staging error and has no final build-operation wall summary, so its 106 s inner compiler line is not substituted. `qs12-test-T2A.log` tested zero mutants and has no operation timing. Interrupted `qs12-test-T4A.log`, other-host A2/A3/B build/test shards and the mistaken B host duplicate run have missing process totals. Their absence makes the wall-time sum a lower bound.

Core recheck 395 s, Rust final 138-survivor replay 4650 s and core build about 28 s survive as rounded session reports; they are not mixed into the precise-summary sum. The core 1027.9 s test is retained from the prior audit record. Round 7 map/build/test time, most rounds 1-7 build/test times, and the full store map time remain missing.

Calendar span: first round 2026-10-23 through last completed round 2026-10-28, five date-difference days across six calendar dates D. Exact first-to-last process timestamps are M. Flow began on 2026-10-28; its completed map took 1255.7 s C but remains in the ongoing row, outside the completed-campaign sum. Its 662 authored mutants are not tested mutants.

## Timeout inflation and missing cells

- Round 1: 11 separate timeout outcomes, of which review called ten timeout noise (session 307). Recalibration problems generated 136 budget timeouts during investigation; that is not 136 distinct additional mutants. Raw 434/475/11 is retained; do not convert all timeouts to catches or claim an inflation-corrected rate.
- Rounds 2-4 predate the CPU-based confirmation change. Exact false-timeout counts and corrected catches/misses are M. Keep their tested packages/cohorts in scope. Re-running survivors with downstream tests changes the cohort verdicts, not the number of distinct mutants.
- Round 5: about 22% of catches were wall-clock timeout catches under host load (Quicksilver #29); precise count and corrected catches/misses are M. Therefore 1669-408 must not become an unqualified kill count.
- Round 6: 23 timeout catches are included in 551; the CPU-based repeated confirmation fix was installed first. This is recorded timeout killing, not established false-positive inflation. One other outcome was an error.
- Round 7: A had zero timeout catches; its major outcome problem was unusable selected tests, particularly jsts_usage_graph_deadlock. Rejudging nine errored mutants produced nine misses; retries add no mutants. Full final outcomes are M.
- Round 8: one of 371 A catches was a CPU timeout; it is included in 459 and is not an additional timeout outcome. Rounds 9-11 and recorded analysis T1-T3 summaries report zero timeout/error outcomes. Full T4 shard counts are M.

No exact whole-campaign caught/missed/error/timeout total can be certified from the surviving evidence. The headline tested lower bound excludes unknown new core-follow-up IDs; Rust has one unclassified outcome; C++ final outcome split is missing; JVM corrected kills and others are missing; JS/TS final outcomes are missing; analysis T4 has a 459/460 intermediate discrepancy and missing full shard logs. A full set of survivor class ledgers, exact added-test functions, exact proven-kill ID union for the conflicting Rust/C++ headlines, and an exhaustive follow-up defect ledger are also missing. Empty cells are M, never zero.

## Supporting evidence

- [Original three-round audit](showcase-facts.md), [its ledger](bug-ledger.json), [prior aggregate record](prior-showcase.json).
- [Read-only EC2 summary/ID extraction](campaign-remote-counts.json); source files remain under /home/ubuntu on 3.144.176.231. [Operation excerpts](campaign-remote-summaries.txt), [configuration excerpts](campaign-remote-details.txt), [current analysis/flow config](campaign-analysis-current-config.toml).
- [Historical campaign memory, copied as evidence](campaign-source-memory.md), [session searches](campaign-history.json), [additional bounded pages](campaign-history-detail.json), [later searches](campaign-more-caught.json), [survivor searches](campaign-more-survivors.json). All refer to session 5b9275ef422b8eb3146a4a11fb9dd1aa; message indices are cited above. Historical text is evidence, not current guidance.
- [C++ fix commit messages](campaign-cpp-commit-messages.txt), [Go proof/defect messages](campaign-go-fix-messages.txt), [T4B failing-before statements](campaign-t4b-fix-messages.txt), [#7832 status/head](campaign-7832-status.json).
- Local existing .mj/qs12t4b/NOTES.md, group{1..5}.out.json; /tmp/qs12/t4/genB/shard{1..3}.txt; .mj/qs13/NOTES.md. These are records read at the audit cutoff, not newly generated runs.
