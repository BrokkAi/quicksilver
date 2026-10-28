# Evidence for the README real-project results

Verified 2026-10-28. These are case studies, not a controlled scaling benchmark.
Writing and reviewing mutants is excluded from the timings.

## mbx

Source: session c4726d0363d4a1daef5e51bd6bca3890, original campaign records
from c31788f646a8565e51cecb86f9049f9a, and jdx/mr-boxington PRs #854, #859–#863.

- Coherent run 1: mbx aa16be7, QS f9c5ab0. map-5.log: 1,251 s;
  build-2.log: 38.7 s; test-1.log: 347.2 s. Build + test = 385.9 s.
- 7 crates, roughly 150k Rust lines, 2,100 mapped tests, 2,657 sites,
  169 mutants. Raw 132 caught / 37 missed included three later false kills.
- Review of 40 (37 survivors + those three corrected kills):
  31 gaps, 5 races needing hooks, 4 tool limitations.
- Five product defects were found during QS investigations, rather than
  directly by survivors. Tested fixes #859–#863 were all OPEN when checked,
  not merged upstream. The older four-bug count omitted #859.
- #854 merged 23 new + 3 extended tests guarding 27 mutants. The measured
  163/169 result used all 29 proposed tests BEFORE review. The retained
  26-test suite was not remeasured; 160/169 is inferred, not observed.

## Bifrost source audit

Copied from session 5b9275ef422b8eb3146a4a11fb9dd1aa at
/home/jonathan/Projects/bifrost/.mj/qs-report/showcase-facts.md.
Relative supporting-artifact links below refer to that original directory;
the commit and PR links are durable primary sources. The numbered defect
inventories reconcile to 21 + 27 + 17 = 65, a conservative lower bound.
Findings cover C#, PHP and Ruby; the headline timing is the C# generation-A
batch, not all 1,449 mutants.

| Round | Mutants tested | Caught | Missed | Other outcomes | Gaps / suspected defects | Gap mutants newly killed | Strict confirmed bug groups (minimum) |
|---|---:|---:|---:|---:|---:|---:|---:|
| 8: C# | 664 D | 459 D | 205 D | 0 C | 128 C / 9 C | 101 D | >=21 D |
| 9: PHP | 442 D | 295 D | 147 D | 0 C | 86 C / 12 C | 84 D | >=27 D |
| 10: Ruby | 343 D | 255 D | 88 D | 0 C | 49 C / 11 C | 44 D | >=17 D |
| **Total** | **1449 D** | **1009 D** | **440 D** | **0 D** | **263 D / 32 D** | **229 D** | **>=65 D** |

C = confirmed from an existing record, including an explicit PR/commit statement. D = derived arithmetic or the deduplicated defect grouping explained below. U = unknown from the surviving evidence. The bug column is a lower bound, **not** a replacement exact count: it excludes fixes without explicit surviving before-test evidence and avoids counting the same defect in multiple serving paths. All other summary columns have complete round scope. The strict confirmed inventory is **21 C#, 27 PHP, 17 Ruby = 65 distinct groups D**; this is the count verified here, not an exhaustive replacement for the campaign-wide bug total. Another C# fix and three Ruby coverage fixes have regressions but lack an explicit surviving red-test receipt; Ruby max_files=0 likewise lacks that receipt.

Records audit performed 2026-10-28. Local origin/master was 54f6c8dba9c575c3d8e0a21d152c8b2bad006787. No map, mutant build, test, benchmark, or cloud launch was performed. Existing EC2 files were read over SSH; the busy bifrost-analysis clone was not inspected. Issue states were read from GitHub on the audit date.

## What differs from the README

- **1449 / 1009 / 440, 263 gaps, 32 suspected defects, 81 equivalent, 44 defensive and 229 gap mutants killed all reconcile.** These are mutant or review-item counts, not numbers of test functions or production bugs.
- **20 other review items = 18 observability-only + 2 covered-since.** Show these separately. They are unrelated to timeout/error mutant outcomes, which were zero.
- **31 bugs fixed (9/9, 12/12, 10/11) is not an audited distinct-bug inventory.** It appears verbatim in campaign session 5b9275ef422b8eb3146a4a11fb9dd1aa, message 5915; it describes disposition of initial suspected-defect review items. The later commits include defects found while closing gaps and reading neighboring code, plus fixes in the earlier coverage PRs. An initial suspicion is not necessarily one distinct defect, and multiple mutation/serving paths can expose the same defect. The ledger below establishes a substantially larger conservative lower bound using the requested stricter definition. Do not print 31 as an exact production-bug total.
- **The Ruby contract question did not become a fixed bug.** It was filed as #7287, remains OPEN, and still concerns lost absolute/lexical context in persisted ancestor and constant-owner facts. It is a confirmed but deferred defect, not an undecided question of whether the behavior is wrong. #7288 (load order) also remains OPEN. #7289 (shared initializer reads) was fixed separately in #7324; it is outside the six requested round PRs.
- Coverage deletion counts are **2 C# + 10 PHP + 7 Ruby = 19 named functions/methods D**, not bugs. Later survivor fixes also remove dead/redundant branches and helpers, separately noted below. Exact total removed function count across those later refactors is U.

## Recorded mutant outcomes, by generation

| Round | Generation | Tested | Caught | Missed | Timeouts | Errors | Not covered | Build-unviable |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| C# | A / 1 | 521 C | 371 C | 150 C | 0 C | 0 C | 0 C | 0 C |
| C# | B / 2 | 143 C | 88 C | 55 C | 0 C | 0 C | 0 C | 0 C |
| PHP | A / 1 | 321 C | 223 C | 98 C | 0 C | 0 C | 0 C | 0 C |
| PHP | B / 2 | 121 C | 72 C | 49 C | 0 C | 0 C | 0 C | 0 C |
| Ruby | A / 1 | 260 C | 196 C | 64 C | 0 C | 0 C | 0 C | 0 C |
| Ruby | B / 2 | 83 C | 59 C | 24 C | 0 C | 0 C | 0 C | 0 C |
| **First generation total** | A | **1102 D** | **790 D** | **312 D** | **0 D** | **0 D** | **0 D** | **0 D** |
| **Later generation total** | B | **347 D** | **219 D** | **128 D** | **0 D** | **0 D** | **0 D** | **0 D** |

Sources: EC2 /home/ubuntu/qs{8,9,10}-build-{A,B}.log and qs{8,9,10}-test-{A,B}.log final lines; excerpts in [remote-log-summary.txt](remote-log-summary.txt). One C# generation-A caught verdict was a test CPU timeout, already included in 371 caught; it is **not** an additional timeout/other outcome. Its test was declarations::grammar_regression_tests::contextual_async_argument_preserves_following_members.

Important JSON scope trap: qs8-test-B.json contains 664 cumulative mutants, qs9-test-B.json 442, and qs10-test-B.json 343. Do not add A JSON length to B JSON length. Generation B counts above come from its log, or from set difference by mutant ID. Existing JSON was independently summarized by verdict.kind; see [outcome-json-summary.txt](outcome-json-summary.txt) and compact ID evidence [outcome-ids.txt](outcome-ids.txt).

Map bases: C# ae2a8d00737bf7a5995cd08d87ddf39269ff51c4, PHP 847cee536608f2b216eddead78342ff1045678ee, Ruby 826c41013fe036bbcc6a7a16aa5b7e4777a8c304 (C, JSON). These generation labels are build generations; B includes late/rebuilt first intents as well as second mutants. It is not a clean split by mutation novelty.

## Survivor classifications

| Round | Test gap | Equivalent | Observability only | Unreachable / defensive | Suspected defect | Other: covered since | Total reviewed |
|---|---:|---:|---:|---:|---:|---:|---:|
| C# | 128 C | 33 C | 12 C | 22 C | 9 C | 1 C | 205 D |
| PHP | 86 C | 30 C | 5 C | 14 C | 12 C | 0 D | 147 D |
| Ruby | 49 C | 18 C | 1 C | 8 C | 11 C | 1 C | 88 D |
| **Total** | **263 D** | **81 D** | **18 D** | **44 D** | **32 D** | **2 D** | **440 D** |

These are the original review classifications, not final bug dispositions. Complete recorded breakdowns survive in [prior-showcase.json](prior-showcase.json), rounds 8/9/10, copied from the Quicksilver session's /mnt/optane/tmp/qs-showcase.3do8/showcase.json. That record names the former /tmp/qs8/review/all.json, /tmp/qs9/review/all.json and /tmp/qs10/review/all.json inputs; those full inputs no longer survive locally. Counts are therefore C from the consolidated record, not a fresh recount of every original review JSON. Available partial originals agree: C# groups 4/5/6/7 and Ruby groups 1/2 plus group3-findings.md under .mj/agents. PHP zero other is D: its five listed classes already sum to all 147.

## Gap mutants killed by new tests

| Round | Lane results | New-test kills | Obsolete | Remaining / not proven |
|---|---|---:|---:|---:|
| C# | A 18; B 17; C 28; D 17; E 21 | 101 D | 2 D | 25 D |
| PHP | A 25; B 22; C 21; D 16 on Linux | 84 D | 0 D | 2 D |
| Ruby | A 18; B 16; C 10 | 44 D | 2 D | 3 D |
| **Total** | | **229 D** | **4 D** | **30 D** |

C# arithmetic: 101+2+25=128. PHP: 84+2=86. Ruby: 44+2+3=49. These do not count regression-test functions: one golden/table can kill many mutants and one mutant may fail several tests.

C# proof sources: each lane's kills.md, linked by the directory aliases below. PHP: #7228 and commits c6093774eb / 71c6b685e5 / 7e0d660678 / 48be758c03; D explicitly records only 16 Linux kills and one Windows-only case awaiting CI. Thus **84 is the proven count**, not 85 or 86. The other PHP survivor raises an inference cap by one and has no supported beyond-cap oracle. Ruby: RA/kills.md, RB/kills.md, RC/kills.md. RC applied ten supplied mutants in a batch with distinct attributed failing assertions; nine tests caught ten mutants, so do not claim ten individually isolated runs there. C# and PHP initial review JSON is not needed to infer kills from test names; the lane proof records explicitly state mutation failure/restoration.

## Classification of claimed fixes: genuine product defects

Count a bug only when records establish a production defect independent of a mutant, a production correction, and a regression failing on old code. C below refers to recorded evidence, not a new rerun. Rows group one root defect across call paths; they do not count every changed function, bullet, mutant or test. This is a conservative lower bound; no claim of exhaustive bug enumeration is made. Every ledger row cites its fix commit and PR. An issue number is given where a separate one is known; otherwise the PR itself is the recorded reference, and a separate issue is U.

Local evidence aliases (all beneath /home/jonathan/Projects/bifrost/.mj/agents/):

| Alias | Directory |
|---|---|
| A | 270721d65c09b22a249005fedfd3a68f |
| B | b77081aa70cfcbf2c5784b4fa28d4d1c |
| C | 0b55b07fd4a6b26d47e442fdd098598c |
| D | 6ca26c0d52491f0ef6a4c7ce12703c7a |
| E | 971ab70dcd1faf054139119ee2d2e0e7 |
| RA | b49b2b6714cdeb34d80020315ca75b59 |
| RB | c9f12d83c12c853db941857cf9afb24f |
| RC | 8a3a5208bf159ba6e8aae346996914e1 |

PHP's original lane before-test logs were not found in the surviving local child-report tree. Its entries below are confirmed from explicit before-failure statements in the merged PR/commit records, not independently inspected raw PHP failure logs. Git commit messages for the lane fixes are saved in [fix-commit-messages.txt](fix-commit-messages.txt); PR bodies in pr-N.json. Before-test stdout/diagnostics alone is weaker than an explicitly recorded failing regression; the exclusions below apply that distinction.

### Round 8: C#

**>=21 distinct confirmed groups D.**

| # | Production defect (C) | Fix commit | PR | Before-fix regression evidence |
|---|---|---|---|---|
| 1 | Static-using bare calls and method groups were missing from inverted usage edges. | [f81e14a2424d](https://github.com/BrokkAi/bifrost-dev/commit/f81e14a2424d45bf2d8733cb703fa9ec18a334c3) | [#7157](https://github.com/BrokkAi/bifrost-dev/pull/7157) | PR #7157: csharp-using-namespace golden exposed the defect; the static-extension control also failed without its exclusion. |
| 2 | Completed lambdas, sibling blocks and earlier methods leaked receiver bindings into later scopes. | [9cf46687c09a](https://github.com/BrokkAi/bifrost-dev/commit/9cf46687c09a172897b937437b9ce7dcd8993582) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | A/defects.md and A/before.log: completed_initializer_and_sibling_scopes_preserve_outer_bindings. Integration 0baec8fd95 enables the prior-method/field diagnostic regression; grouped here, not counted again. |
| 3 | Initializer inference entered arguments and chose an inner new instead of the factory result. | [9cf46687c09a](https://github.com/BrokkAi/bifrost-dev/commit/9cf46687c09a172897b937437b9ce7dcd8993582) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | A/before.log: factory_arguments_do_not_determine_initializer_type; D/observation-g1-before.log independently pins the inverted-scan path, fixed by 1993e7a051. One defect group, two implementations. |
| 4 | Explicitly typed lambdas supplied no target type to target-typed initializer members. | [1b42d83d8bc4](https://github.com/BrokkAi/bifrost-dev/commit/1b42d83d8bc4766bcab0b113f8eb292dca78eacc) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | B/grammar-before.log and B/lambda-expression-before.log; enclosing_function_return_type and expression_target_type regressions. |
| 5 | Inverse generic-return lookup converted an ambiguous import into a proven unrelated global type. | [1b42d83d8bc4](https://github.com/BrokkAi/bifrost-dev/commit/1b42d83d8bc4766bcab0b113f8eb292dca78eacc) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | B/ambiguity-before.txt; csharp-ambiguous-generic-return golden; commit records failing tests and Roslyn CS0104. |
| 6 | Imported extension methods competed with extensions declared in the enclosing namespace. | [1b42d83d8bc4](https://github.com/BrokkAi/bifrost-dev/commit/1b42d83d8bc4766bcab0b113f8eb292dca78eacc) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | B/extension-scope-before.txt; usage-graph golden, Roslyn namespace-H control. |
| 7 | Generic member identifiers had no member-position role. | [26fbd9cf5624](https://github.com/BrokkAi/bifrost-dev/commit/26fbd9cf5624dd038337fc32bb0c200e3f94b751) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | C/defects-before.log: generic_member_grammar_preserves_identifier_roles. |
| 8 | A deconstruction tuple pattern was published as its initializer. | [26fbd9cf5624](https://github.com/BrokkAi/bifrost-dev/commit/26fbd9cf5624dd038337fc32bb0c200e3f94b751) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | C/tuple-before.log: declarator_grammar_distinguishes_patterns_from_initializers. |
| 9 | Custom event add/remove accessors lost their virtual-dispatch owner. | [26fbd9cf5624](https://github.com/BrokkAi/bifrost-dev/commit/26fbd9cf5624dd038337fc32bb0c200e3f94b751) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | C/defects-before.log: accessor_grammar_retains_owner_dispatch. |
| 10 | Comment extras counted as generic type arguments and changed arity. | [26fbd9cf5624](https://github.com/BrokkAi/bifrost-dev/commit/26fbd9cf5624dd038337fc32bb0c200e3f94b751) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | C/defects-before.log: generic_argument_grammar_excludes_named_extras. |
| 11 | Nested declaration spellings beyond ten components were omitted. | [1993e7a05116](https://github.com/BrokkAi/bifrost-dev/commit/1993e7a05116dd894fec0e8fc3ab80f22ce469d6) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | D/defects-before.log: nested_declaration_spellings_remain_addressable_at_every_depth. |
| 12 | Qualified array type references were counted twice and incorrectly exceeded the graph site cap. | [1993e7a05116](https://github.com/BrokkAi/bifrost-dev/commit/1993e7a05116dd894fec0e8fc3ab80f22ce469d6) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | D/array-and-scope-before.log: csharp_array_type_sites_count_written_references_once_at_the_graph_cap. |
| 13 | Mutually recursive receiver/expression/call-return walks overflowed the Rust stack. | [1993e7a05116](https://github.com/BrokkAi/bifrost-dev/commit/1993e7a05116dd894fec0e8fc3ab80f22ce469d6) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | D/observation-g5-before.log: csharp_initializer_typing_uses_the_outer_expression_without_rust_recursion; original abort at 3000 wrappers. |
| 14 | A receiver missing a member was rebound to an unrelated outer type with the same short name. | [1993e7a05116](https://github.com/BrokkAi/bifrost-dev/commit/1993e7a05116dd894fec0e8fc3ab80f22ce469d6) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | D/new-goldens-and-owner-before.log: csharp_receiver_type_binding_does_not_fall_through_when_member_is_missing; Roslyn CS0117. |
| 15 | Parameter label spans selected matching text in types/defaults instead of the parameter token. | [f2fd2f0cff07](https://github.com/BrokkAi/bifrost-dev/commit/f2fd2f0cff0713beea8e620cb7c83195c130fdab) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | E/before.log: parameter_labels_follow_name_tokens. |
| 16 | Declaration headers were truncated at braces inside attribute/default literals. | [f2fd2f0cff07](https://github.com/BrokkAi/bifrost-dev/commit/f2fd2f0cff0713beea8e620cb7c83195c130fdab) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | E/before.log: signatures_preserve_literal_values_and_structural_bodies. |
| 17 | Signature whitespace normalization changed literal values. | [f2fd2f0cff07](https://github.com/BrokkAi/bifrost-dev/commit/f2fd2f0cff0713beea8e620cb7c83195c130fdab) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | E/findings.md and E/before.log: signatures_preserve_literal_values_and_structural_bodies; separate value-corruption defect from header boundaries. |
| 18 | Ref-return referents were checked twice or omitted from diagnostics. | [f2fd2f0cff07](https://github.com/BrokkAi/bifrost-dev/commit/f2fd2f0cff0713beea8e620cb7c83195c130fdab) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | E/before.log: grammar_type_positions_cover_wrappers_arguments_and_bases_once. |
| 19 | Using-directive diagnostics bypassed the diagnostic cap. | [f2fd2f0cff07](https://github.com/BrokkAi/bifrost-dev/commit/f2fd2f0cff0713beea8e620cb7c83195c130fdab) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | E/diagnostic-baseline.log: public diagnostic budget regression published 201 outcomes. |
| 20 | Qualified external type diagnostics used an unrelated file-wide workspace-bound flag. | [f2fd2f0cff07](https://github.com/BrokkAi/bifrost-dev/commit/f2fd2f0cff0713beea8e620cb7c83195c130fdab) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | E/diagnostic-baseline.log and E/findings.md: qualified external/workspace proof regressions. |
| 21 | A namespace was chosen before a local value receiver with the same name. | [f2fd2f0cff07](https://github.com/BrokkAi/bifrost-dev/commit/f2fd2f0cff0713beea8e620cb7c83195c130fdab) | [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320) | E/diagnostic-baseline.log and E/findings.md: Widget N / @N value-receiver regression. |

### Round 9: PHP

**>=27 distinct confirmed groups D.**

| # | Production defect (C) | Fix commit | PR | Before-fix regression evidence |
|---|---|---|---|---|
| 1 | Mixed-case constructors lost constructor identity, usages and dependency projection. | [fa28f5b921be](https://github.com/BrokkAi/bifrost-dev/commit/fa28f5b921bea05314eae5e1e7c48655d649b795) | [#7188](https://github.com/BrokkAi/bifrost-dev/pull/7188) | PR #7188 explicitly records old (false,0,0,0) versus expected (true,1,1,2); projection regression failed Method versus Constructor, fixed in e0d49753c7. One name-case defect group. |
| 2 | Raw type arms admitted relative keywords and primitive int as nominal class names. | [c6093774ebfa](https://github.com/BrokkAi/bifrost-dev/commit/c6093774ebfa08fb8ae227b001c90bc8f81061e1) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 says every defect has a regression that failed before; c6093774eb records confirmed fixes. Regression at introduction: qs9_declared_types_match_grammar_boundary (qs9 prefixes renamed in a0c6b81178). |
| 3 | Global and namespace constant identities placed or duplicated the synthetic module scope incorrectly. | [c6093774ebfa](https://github.com/BrokkAi/bifrost-dev/commit/c6093774ebfa08fb8ae227b001c90bc8f81061e1) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 says every defect has a regression that failed before; c6093774eb records confirmed fixes. Regression at introduction: qs9_global_constant_identity (qs9 prefixes renamed in a0c6b81178). |
| 4 | PHPDoc unions discarded ambiguous/unsupported arms and fabricated uniqueness. | [c6093774ebfa](https://github.com/BrokkAi/bifrost-dev/commit/c6093774ebfa08fb8ae227b001c90bc8f81061e1) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 says every defect has a regression that failed before; c6093774eb records confirmed fixes. Regression at introduction: qs9_phpdoc_union_projection_never_discards_unknown_arms (qs9 prefixes renamed in a0c6b81178). |
| 5 | Property and constant attributes were omitted from signatures. | [c6093774ebfa](https://github.com/BrokkAi/bifrost-dev/commit/c6093774ebfa08fb8ae227b001c90bc8f81061e1) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 says every defect has a regression that failed before; c6093774eb records confirmed fixes. Regression at introduction: qs9_attributes_are_declaration_children (qs9 prefixes renamed in a0c6b81178). |
| 6 | Class and function aliases were matched case-sensitively. | [c6093774ebfa](https://github.com/BrokkAi/bifrost-dev/commit/c6093774ebfa08fb8ae227b001c90bc8f81061e1) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 says every defect has a regression that failed before; c6093774eb records confirmed fixes. Regression at introduction: qs9_callable_namespace_and_alias_rules (qs9 prefixes renamed in a0c6b81178). |
| 7 | Qualified callable/constant names did not expand namespace/type aliases. | [c6093774ebfa](https://github.com/BrokkAi/bifrost-dev/commit/c6093774ebfa08fb8ae227b001c90bc8f81061e1) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 says every defect has a regression that failed before; c6093774eb records confirmed fixes. Regression at introduction: qs9_callable_namespace_and_alias_rules (qs9 prefixes renamed in a0c6b81178). |
| 8 | Parameter highlights could land in an earlier string default. | [c6093774ebfa](https://github.com/BrokkAi/bifrost-dev/commit/c6093774ebfa08fb8ae227b001c90bc8f81061e1) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 says every defect has a regression that failed before; c6093774eb records confirmed fixes. Regression at introduction: qs9_parameter_ranges_follow_name_nodes (qs9 prefixes renamed in a0c6b81178). |
| 9 | Missing field modifiers and storage-insensitive write inference mixed static and instance fields. | [c6093774ebfa](https://github.com/BrokkAi/bifrost-dev/commit/c6093774ebfa08fb8ae227b001c90bc8f81061e1) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | Producer in c6093774eb plus consumer in 71c6b685e5; static_field_inference_respects_declaration_storage enabled in a0c6b81178; PR #7228 records the before-fix failure. Counted once across both sides. |
| 10 | Nested-class writes were credited to outer classes rather than their nearest lexical owner. | [71c6b685e543](https://github.com/BrokkAi/bifrost-dev/commit/71c6b685e543be80c577a2c053c7de807dd07183) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 and commit record a failing-before regression: nested_class_writes_have_one_lexical_owner. |
| 11 | An empty-array assignment failed to clear historical element inference. | [71c6b685e543](https://github.com/BrokkAi/bifrost-dev/commit/71c6b685e543be80c577a2c053c7de807dd07183) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 and commit record a failing-before regression: indexed_collection_reset_replaces_historical_values. |
| 12 | A foreach element type overrode a later receiver reassignment. | [71c6b685e543](https://github.com/BrokkAi/bifrost-dev/commit/71c6b685e543be80c577a2c053c7de807dd07183) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 and commit record a failing-before regression: typed_foreach_reassignment_uses_the_current_value. |
| 13 | Qualified instanceof operands lost their type-reference edge. | [71c6b685e543](https://github.com/BrokkAi/bifrost-dev/commit/71c6b685e543be80c577a2c053c7de807dd07183) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 and commit record a failing-before regression: qualified_instanceof_publishes_the_type_edge. |
| 14 | Method names, overrides and relative self/parent/static names were compared case-sensitively. | [71c6b685e543](https://github.com/BrokkAi/bifrost-dev/commit/71c6b685e543be80c577a2c053c7de807dd07183) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 and commit record a failing-before regression: mixed_case_method_calls_reach_targeted_and_inverse_scans / relative_static_scopes_and_method_names_ignore_ascii_case. |
| 15 | Three recursive AST walks overflowed the stack on deep syntax. | [71c6b685e543](https://github.com/BrokkAi/bifrost-dev/commit/71c6b685e543be80c577a2c053c7de807dd07183) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 and commit record a failing-before regression: deep_syntax_uses_bounded_rust_stack_through_both_usage_scans; commit records original aborts at 2048 levels. |
| 16 | Expression-valued static scopes fabricated absent nominal classes or vanished. | [7e0d66067895](https://github.com/BrokkAi/bifrost-dev/commit/7e0d6606789554f431fcec26097459055e53558b) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 and commit record a failing-before regression: expression_static_scopes_always_report_dynamic_evidence. |
| 17 | Mixed-case PHPUnit TestCase parents were not detected. | [7e0d66067895](https://github.com/BrokkAi/bifrost-dev/commit/7e0d6606789554f431fcec26097459055e53558b) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 and commit record a failing-before regression: test_detection_uses_declarations_and_attached_comments. |
| 18 | Foreach comment extras were taken as binder/header syntax. | [7e0d66067895](https://github.com/BrokkAi/bifrost-dev/commit/7e0d6606789554f431fcec26097459055e53558b) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 and commit record a failing-before regression: comments_do_not_change_header_roles_or_constructor_callees. |
| 19 | Raw-source docblock discovery matched text that was not an attached parsed docblock. | [7e0d66067895](https://github.com/BrokkAi/bifrost-dev/commit/7e0d6606789554f431fcec26097459055e53558b) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 and commit record a failing-before regression: test_detection_uses_declarations_and_attached_comments. |
| 20 | Constants in bracketed namespaces and parameter/property defaults had no outcomes. | [7e0d66067895](https://github.com/BrokkAi/bifrost-dev/commit/7e0d6606789554f431fcec26097459055e53558b) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 and commit record a failing-before regression: constant_outcomes_do_not_depend_on_namespace_braces_or_default_position. |
| 21 | Runtime builtins took precedence over actual workspace function/constant bindings. | [7e0d66067895](https://github.com/BrokkAi/bifrost-dev/commit/7e0d6606789554f431fcec26097459055e53558b) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 and commit record a failing-before regression: runtime_builtin_outcomes_follow_namespace_precedence / builtin_aliases_and_case_use_resolved_candidates. |
| 22 | A comment after new was selected as the constructor callee. | [7e0d66067895](https://github.com/BrokkAi/bifrost-dev/commit/7e0d6606789554f431fcec26097459055e53558b) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | PR #7228 and commit record a failing-before regression: comments_do_not_change_header_roles_or_constructor_callees. |
| 23 | Include concatenation discarded separators inside path pieces. | [48be758c0363](https://github.com/BrokkAi/bifrost-dev/commit/48be758c0363f62c35890a0ae80875193144312b) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | 48be758c03 explicitly records static_include_facts_follow_php_literal_rules failing before each fix. |
| 24 | Parentheses prevented otherwise-static include folding. | [48be758c0363](https://github.com/BrokkAi/bifrost-dev/commit/48be758c0363f62c35890a0ae80875193144312b) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | 48be758c03 explicitly records static_include_facts_follow_php_literal_rules failing before each fix. |
| 25 | An octal dirname level was interpreted as decimal. | [48be758c0363](https://github.com/BrokkAi/bifrost-dev/commit/48be758c0363f62c35890a0ae80875193144312b) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | 48be758c03 explicitly records static_include_facts_follow_php_literal_rules failing before each fix. |
| 26 | A plain double-quoted static include was treated as dynamic. | [48be758c0363](https://github.com/BrokkAi/bifrost-dev/commit/48be758c0363f62c35890a0ae80875193144312b) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | 48be758c03 explicitly records static_include_facts_follow_php_literal_rules failing before each fix. |
| 27 | Member-call collection inference recursed without consuming its depth budget. | [49b00bbf3a6a](https://github.com/BrokkAi/bifrost-dev/commit/49b00bbf3a6ade8e0746375cb23b1820f5df9108) | [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | 49b00bbf3a records deep_member_collection_chain_is_bounded aborting on 2048 links before its fix. The MemberCall arm was unchanged from map base 847cee5366 until this correction. |

### Round 10: Ruby

**>=17 distinct confirmed groups D.**

| # | Production defect (C) | Fix commit | PR | Before-fix regression evidence |
|---|---|---|---|---|
| 1 | Nested factory-cache hits panicked by taking a mutable RefCell borrow during an immutable borrow. | [7961e9a1049d](https://github.com/BrokkAi/bifrost-dev/commit/7961e9a1049dd561a79b6853b390d5e447a6a0b7) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RA/before.log: ruby_factory_nested_cache_hits_and_trailing_comments. |
| 2 | A trailing comment was mistaken for a factory tail expression. | [7961e9a1049d](https://github.com/BrokkAi/bifrost-dev/commit/7961e9a1049dd561a79b6853b390d5e447a6a0b7) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RA/before-tail-superclass.log: ruby_factory_trailing_comment_preserves_return_value. |
| 3 | Local inference leaked across class, module and method boundaries. | [7961e9a1049d](https://github.com/BrokkAi/bifrost-dev/commit/7961e9a1049dd561a79b6853b390d5e447a6a0b7) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RA/before.log: ruby_locals_do_not_cross_class_module_or_method_boundaries. |
| 4 | A superclass reference was resolved again inside the class it introduces. | [7961e9a1049d](https://github.com/BrokkAi/bifrost-dev/commit/7961e9a1049dd561a79b6853b390d5e447a6a0b7) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RA/before-tail-superclass.log: reports_superclass_reference_before_entering_declared_class_scope; b94e39db56 fixes the corresponding shared resolver path, RB/findings.md. Counted once. |
| 5 | B.make.new inferred the inner factory result as the outer receiver type. | [7961e9a1049d](https://github.com/BrokkAi/bifrost-dev/commit/7961e9a1049dd561a79b6853b390d5e447a6a0b7) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RA/before.log: ruby_factory_outer_new_does_not_infer_inner_result. |
| 6 | Cross-file reopenings of the same class/module were treated as different owners and lost relation rows. | [b94e39db5669](https://github.com/BrokkAi/bifrost-dev/commit/b94e39db5669760cec546a8ebb7cdc0c1d767924) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RB/lane-b-resolver-before.log: reopening_relation_reader_and_dispatch; RB/defect-proof.json / reopening-relation-aggregation-before.log also prove the isolated correction. |
| 7 | Root classes won over lexically nearer constants. | [b94e39db5669](https://github.com/BrokkAi/bifrost-dev/commit/b94e39db5669760cec546a8ebb7cdc0c1d767924) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RB/lane-b-resolver-before.log: lexical_constant_and_qualified_prefix_resolution. |
| 8 | An unresolved qualified prefix fell back to any same-leaf class or ancestor. | [b94e39db5669](https://github.com/BrokkAi/bifrost-dev/commit/b94e39db5669760cec546a8ebb7cdc0c1d767924) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RB/qualified-forward-prefix-before.log and qualified-hierarchy-prefix-before.log; RB/defect-proof.json records both before=100/restored=0. |
| 9 | Send/public_send names were read as raw escape spellings rather than decoded literal values. | [b94e39db5669](https://github.com/BrokkAi/bifrost-dev/commit/b94e39db5669760cec546a8ebb7cdc0c1d767924) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RB/lane-b-resolver-before.log and RB/findings.md: dynamic-dispatch grammar/value table pi\x6eg versus ping. |
| 10 | A user-declared loop method was treated as the unconditional Kernel loop. | [b94e39db5669](https://github.com/BrokkAi/bifrost-dev/commit/b94e39db5669760cec546a8ebb7cdc0c1d767924) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RB/lane-b-resolver-before.log: loop_dispatch_grammar_and_shadowing; shadow-scope rework in d192ae10dc / 40c18e8ef4. |
| 11 | Absolute and multi-segment constant assignments were published under the wrong owner. | [e1f683ff1c4c](https://github.com/BrokkAi/bifrost-dev/commit/e1f683ff1c4cae82039a4e379516fc618096f591) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RC/before.log: ruby_declaration_ownership; imported cross-file ownership remains #7287, not counted as fixed. |
| 12 | An absolute class declaration inside a module was incorrectly nested under the module. | [e1f683ff1c4c](https://github.com/BrokkAi/bifrost-dev/commit/e1f683ff1c4cae82039a4e379516fc618096f591) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RC/before.log: ruby_declaration_ownership (N$Top versus Top). |
| 13 | Methods/mixins inside ordinary executing blocks were skipped. | [e1f683ff1c4c](https://github.com/BrokkAi/bifrost-dev/commit/e1f683ff1c4cae82039a4e379516fc618096f591) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RC/before.log: ruby_executing_blocks_and_dynamic_names. |
| 14 | Interpolated attr/alias names became fabricated static names; unknown module_function arguments acted as the bare form. | [e1f683ff1c4c](https://github.com/BrokkAi/bifrost-dev/commit/e1f683ff1c4cae82039a4e379516fc618096f591) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RC/before.log: ruby_executing_blocks_and_dynamic_names; dynamic-name handling grouped as one defect, not a separate module_function count. |
| 15 | String autoload constant names and path arguments were selected in the wrong positions. | [e1f683ff1c4c](https://github.com/BrokkAi/bifrost-dev/commit/e1f683ff1c4cae82039a4e379516fc618096f591) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RC/before.log: ruby_load_argument_grammar. |
| 16 | The Gemfile detector missed multiline gem calls and could match heredoc text. | [e1f683ff1c4c](https://github.com/BrokkAi/bifrost-dev/commit/e1f683ff1c4cae82039a4e379516fc618096f591) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RC/before.log: ruby_gemfile_call_grammar. |
| 17 | String interpolations were skipped by semantic diagnostics. | [e1f683ff1c4c](https://github.com/BrokkAi/bifrost-dev/commit/e1f683ff1c4cae82039a4e379516fc618096f591) | [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | RC/interpolation-before.log: ruby_diagnostic_executable_expressions. |

### Genuine production corrections with incomplete before-test receipts (not counted)

- C# generic type-parameter receiver rebound to unrelated Library.T: [0baec8fd95a0](https://github.com/BrokkAi/bifrost-dev/commit/0baec8fd95a0130507c96ef0e124dcb4f813bcfe), [#7320](https://github.com/BrokkAi/bifrost-dev/pull/7320). B/typeparameter-diagnostics-before.txt and Roslyn CS1061 show the old defect; the final regression is csharp_type_parameter_receiver_does_not_resolve_to_unrelated_type. **U** whether that exact new regression was run and failed before the fix: #7320 explicitly says there was no captured failing-test run for this case. Do not count it under the requested strict rule. This makes at least 22 reported C# fix groups D, 21 meeting the strict recorded-proof threshold.
- Ruby coverage PR [#7216](https://github.com/BrokkAi/bifrost-dev/pull/7216), [bee34bacbf58](https://github.com/BrokkAi/bifrost-dev/commit/bee34bacbf58c920a1a6092c7d009326235b9165), reports **3 C** defects found by its new tests: single-quoted escapes were not decoded; CRLF continuations became unknown; for-loop bare iterable calls were omitted. Regression names: ruby_literal_grammar_and_values, ruby_bare_call_grammar_and_lexical_scopes. The commit/PR records the fixes and independent Ruby/Ripper oracles, but an explicit before-fix failure receipt was not located (**U**). They are reported production fixes, excluded from the strict minimum rather than guessed into it.
- Ruby max_files=0: [b94e39db5669](https://github.com/BrokkAi/bifrost-dev/commit/b94e39db5669760cec546a8ebb7cdc0c1d767924), [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290). The cap correction is recorded, but a regression demonstrated failing against the old unmutated code is **U**. A mutant kill by itself does not prove that.
- Scope of the six round PRs excludes the shared field-initializer correction [4ac5a7d9d804](https://github.com/BrokkAi/bifrost-dev/commit/4ac5a7d9d80462ae2a1959b81b3289247ae43a73), [#7324](https://github.com/BrokkAi/bifrost-dev/pull/7324), fixes #7289. #7289 is now CLOSED C. This was independently confirmed, fixed and regression-tested, but belongs in an explicitly broader campaign scope if included in a headline. The later Ruby duplicate-test deletion [4a3ba9005ab3](https://github.com/BrokkAi/bifrost-dev/commit/4a3ba9005ab3c3349937b41849270f5bf3bc253e) is test cleanup, not another bug.

### Rework of new behavior, not additional pre-campaign defects

Keep these outside a claim about bugs present in the campaign's map bases:

| Round | Rework groups | Commit / PR | Recorded failing regression |
|---|---:|---|---|
| PHP | 4 C | [49b00bbf3a6a](https://github.com/BrokkAi/bifrost-dev/commit/49b00bbf3a6ade8e0746375cb23b1820f5df9108) / [#7228](https://github.com/BrokkAi/bifrost-dev/pull/7228) | NULL union spelling, qualified keyword segments, include comment extras, named-argument labels; commit explicitly says four defects were in new behavior. The fifth correction is the pre-existing member-call stack defect counted above. |
| Ruby | 3 D | [d192ae10dc43](https://github.com/BrokkAi/bifrost-dev/commit/d192ae10dc4346f1799d8fa827ec8edb7c610261) and [40c18e8ef414](https://github.com/BrokkAi/bifrost-dev/commit/40c18e8ef4143d427c1662187b2ea25d85986579) / [#7290](https://github.com/BrokkAi/bifrost-dev/pull/7290) | Loop shadow scope (both reviews grouped), computed-send unproven accounting/nominations (both reviews grouped), Concern block declaration ownership. The commit explicitly records tests failing before each fix. |

These are real review corrections with red regressions, but counting them alongside baseline bugs without explaining their origin would inflate a showcase of pre-existing defects. Exact all-history production-bug total remains **U** because grouping and missing before-test receipts matter.

### Ruby's pending contract case

Initial suspected survivor cac10db3e59ebdbc is identified in RB/findings.md as CONFIRMED-NEEDS-DECISION: the producer discards RubyNamePath.absolute and lexical nesting, so Base and ::Base emit the same ancestor target. superclass_facts_preserve_absolute_lookup remains an intentionally ignored failing regression; RB/ancestry-design-gap.log records actual Base versus expected Outer$Base. The parent filed [#7287](https://github.com/BrokkAi/bifrost-dev/issues/7287), also covering imported constant-assignment owners; issue state is OPEN C on 2026-10-28. Its comment adds that dropping unresolved ancestor names also needs an incompleteness signal. #7290 explicitly leaves it open. No later fix is recorded in the issue or current origin/master history for this case. This is the disposition of the former pending decision, not a 11th fixed Ruby bug.

Load-order issue [#7288](https://github.com/BrokkAi/bifrost-dev/issues/7288) is separately OPEN C. Do not conflate the two. Shared field reads [#7289](https://github.com/BrokkAi/bifrost-dev/issues/7289) became #7324 and are CLOSED C. Saved current issue records: issue-7287.json, issue-7288.json, issue-7289.json.

### Contract/design cases: not bug fixes in these PRs

| Language | Case | Recorded disposition |
|---|---|---|
| C# | #7156 alias-qualified prefix; #7165 namespace-scoped usings; #7167 static-imported extension on a literal | #7157 files these rather than shipping a text-based or unverified repair. Current resolution beyond the round PR is U here. |
| C# | #7319 base lookup needs using-directive scopes; #7313 conditional typing / incomplete reason; #7330 value-first receiver versus namespace path | #7320 explicitly leaves these open; do not count as fixes. |
| Shared | #7245 test discovery; #7220 PHP shared terminal lookup; #7209 per-site uncertainty accounting | Routed to shared owners, not fixed by the language-local PRs. Current later disposition was not audited (U). |
| Ruby | #7287 absolute/lexical ancestor facts | Confirmed wrong, filed and deferred to a producer contract change; OPEN C, not fixed. |
| Ruby | #7288 require load order | Deferred to execution-order modeling; OPEN C, not fixed. |
| Ruby/shared | #7289 field-initializer read classification | Fixed separately in #7324 (4ac5a7d9d8), CLOSED C; outside these six PRs. |
| Heap residency | #7225 C# and #7210 Ruby | Explicit exclusions left unchanged by these rounds; not fixes. |


### Deletions and test-only work

| Category | C# | PHP | Ruby | Total |
|---|---|---|---|---|
| Named dead functions/methods removed in coverage PR | 2 C | 10 C | 7 C | 19 D |
| Gap mutants killed through new tests | 101 D | 84 D | 44 D | 229 D |
| Actual count of new test functions | U | U | U | U |
| Additional dead/redundant functions and branches removed in survivor refactors | U exact count | U exact count | U exact count | U |

Coverage deletion commits: f81e14a242 (#7157), fa28f5b921 (#7188), bee34bacbf (#7216). C#: arity_free_visible_type_candidates and usage_arity_free_visible_type_candidates. PHP: use_declarations, sites, SemanticDiagnostic conversion, five UnindexedPhpExternalSurface methods, declared_type_arm_fq_names, php_file_context_from_source. Ruby: collect_ruby_identifiers; three UnacquiredRubyGems methods; mixin_owners_of; ruby_has_unresolved_load_directive; sites. Source DTOs/stub types/fields removed alongside them are not additional function counts.

Later examples are C# redundant recovery tests/global-using work/dead this/base arms; PHP unused declaration-name predicate, equivalent PHPDoc arm, unreachable test-method arm and raw dynamic-name helper; Ruby synthetic macro-selection route. They are not counted as bugs even if their bodies had defects. Pure test work includes grammar boundaries, golden controls, changing tests that returned too early, naming/registration fixes (a0c6b81178), enabling an integration regression and deleting the duplicate Ruby regression (4a3ba9005a). Counts of those edits by test function are U; 229 counts proven mutant-gap closures only.

## Representative C# timing only

| Operation | Wall time from log | Rounded README claim |
|---|---:|---|
| Round 8 C# analyzer map | **948.9 seconds C** (15m48.9s D) | Map covers 796 sites C, 657 entered C, 8177 mapped tests C / 8174 green C |
| Generation A build: 521 mutants C | **642.5 seconds C** (10m42.5s D) | About 11 minutes: confirmed |
| Generation A test: 521 mutants C | **3547.7 seconds C** (59m07.7s D) | About 1 hour: confirmed |

Host: **c7i.16xlarge C**, ubuntu@3.144.176.231. Current read-only IMDSv2 instance-type lookup agrees with campaign session message 3432. This is Quicksilver's operation-summary wall time, not CPU time or end-to-end authoring/review time.

**The build was incremental after the map C.** qs8-build-A.log records mbx cache reports with 38 incremental compilations and a final build summary of 5 Cargo invocations C. The map log separately records initial dev/test compilation. Do not describe this as a cold build.

Original sources: /home/ubuntu/qs8-map.log (final map summary), /home/ubuntu/qs8-build-A.log (final build summary and mbx reports), /home/ubuntu/qs8-test-A.log (final tested summary). Captured excerpts: [remote-log-summary.txt](remote-log-summary.txt), [build-cache-evidence.txt](build-cache-evidence.txt). Other-round timing and later Go/analysis rounds are omitted per the narrowed request.

## Headline recommendation

Use: “Across the C#, PHP and Ruby rounds, 1,449 mutants were tested: existing tests caught 1,009 and missed 440. Review classified 263 test gaps, 81 equivalent mutations, 44 unreachable/defensive cases, 32 suspected defects, 18 observability-only cases and 2 already covered by newer tests. New tests were demonstrated to kill 229 gap mutants.” All are confirmed from the cited records, with totals derived by addition.

For bugs, use a conservative explicit inventory: **at least 65 distinct pre-existing production defect groups with recorded before-failing regressions (21 C#, 27 PHP, 17 Ruby)**. This includes coverage-PR fixes when a before-failure is recorded, groups duplicate serving-path corrections, excludes dead code/test-only work, and excludes new-behavior rework. **The exact overall production-bug total is unverified**, so neither 31 nor 65 should be advertised as that exact total. The Ruby pending case is #7287, still OPEN, confirmed but deferred pending context-preserving producer design.
