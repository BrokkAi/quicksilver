# cargo-quicksilver

**Agentic mutation testing for Rust.**

Quicksilver changes your code on purpose, runs your tests, and reports the
changes your tests did not notice. Each change is a *mutant*. A mutant that
makes a test fail is *caught*. A mutant that every test passes is *missed*:
it marks behavior your tests run but never check.


## Why quicksilver

**Coverage tells you which code your tests run, not what they check.** A
test can call a function and assert nothing about its result. Mutation
testing finds those gaps: if you can break a function and every test still
passes, no test checks it.

**Mutation testing has been too slow to use every day.** The usual approach
rebuilds your project once for each mutant. Quicksilver does not:

- **One build for many mutants.** It puts a small switch at the start of
  every function and compiles the workspace once. Each mutant is turned on
  at run time, so a thousand mutants cost one build, not a thousand. On
  mbx, a workspace of 7 crates and 2,100 tests, 169 hand-written mutants
  compiled in one incremental rebuild after `map`, in 39 seconds.
- **Only the tests that matter.** The same switch records which tests enter
  which functions. A mutant runs only the tests that reach it, cheapest
  first, so a kill is usually found early.

**Realistic mutants find real gaps.** Built-in mutants are blunt: replace a
body with `0`, `true` or `None`. Real bugs are an off-by-one, a swapped
condition, a forgotten case. With quicksilver you, or a coding agent, write
those as ordinary edits to function bodies, and quicksilver turns each
edited function into a mutant. In the two projects below, most surviving
mutants pointed at behavior no test checked, and some at real bugs.

**Built for agents.** Agents can map a workspace, split the work
(`pending --split`), write mutants in parallel and submit them as patches
with a note on what each one models, test them in one resumable run, and
review the survivors (`survivors --split`). `prove` checks that a new test
really catches a mutant. Output is plain text that agents read well, with
JSON on request.

**Verdicts you can trust.** A kill counts only if the test fails twice with
the mutant and passes without it. Timeouts are judged by CPU time, so a
slow, busy machine is not mistaken for a mutant that makes code hang, and a
kill by a test that later fails on its own is flagged. Every test process
gets its own home and temporary directories, so a mutant cannot write into
your real ones.


## Dependencies

Quicksilver builds on two existing Rust tools:

- **[cargo-nextest](https://nexte.st)** discovers the workspace's test
  binaries and individual tests, and runs the unmodified suite to check
  that it passes and record per-test timings. Its machine-readable catalog
  supplies test names and executable paths. Quicksilver's own map records
  which functions each test enters and selects the tests for each mutant.
- **[mbx](https://mr-boxington.jdx.dev)** manages build state for
  quicksilver's isolated git worktrees. It gives each worktree its own
  [target directory](https://mr-boxington.jdx.dev/managed-targets), reuses
  matching build outputs, and can seed
  [private incremental compiler state](https://mr-boxington.jdx.dev/incremental)
  from another checkout when quicksilver edits a crate. Before removing a
  worktree, quicksilver calls `mbx clean` to release its target and
  incremental state.

Both must be on `PATH`. They are executable dependencies:
[`cargo install`](https://doc.rust-lang.org/cargo/commands/cargo-install.html)
does not install them automatically with quicksilver, but you can install
all three in one command below. Existing installations can be reused.


## Results on real projects

Agents used quicksilver to find missing tests and bugs in two Rust projects:

| | Small project: [mbx](https://github.com/jdx/mr-boxington) | Large project: [Bifrost](https://github.com/BrokkAi/bifrost) |
| --- | --- | --- |
| Codebase | **150k Rust lines**, 7 crates | **3.3M Rust lines**, about 30 crates |
| Scope tested | Whole workspace | C#, PHP and Ruby analyzers |
| Map time | **21 min** | **16 min** for the C# analyzer |
| Mutants checked | **169** | **1,449** |
| Build + test after map | **6 min 26 sec** for 169 mutants | **70 min** for a 521-mutant C# batch |
| Test gaps found | **31** | **263** |
| Bugs found | **5 confirmed**, fixes submitted | **65+ fixed**, each with a regression test |
| Follow-up tests | **26 added or strengthened and merged** | **229 gaps covered** by proven tests |

Mapping prepares the test-to-function map; subsequent mutant batches reuse
the build work. Times exclude writing mutants and reviewing findings. mbx
ran on a shared 96-CPU host; Bifrost ran on a 64-vCPU EC2 host, one analyzer
at a time. The Bifrost checking time is for the C# batch shown, while its
finding counts combine all three analyzers.

<details>
<summary>Run details and survivor breakdowns</summary>

### A small project: mbx

[mbx](https://github.com/jdx/mr-boxington) is a build cache for Rust: 7
crates, about 150,000 lines of Rust and 2,100 tests.

- Five agents each took one area of the code. Between them they wrote 169
  mutants, most of which bring back a bug that mbx fixed in the past.
- `map` put a switch in 2,657 functions and found that tests enter 2,439
  of them, in 21 minutes. After `map`, `build` recompiled the changed crates
  once for all 169 mutants, in one Cargo invocation, in 39 seconds. `test`
  took 6 minutes: 132 caught, 37 missed.
- Review covered the 37 survivors and 3 more mutants that an unrelated,
  randomly failing test had counted as caught. Of these 40, 31 were
  behavior no test checked. 5 were races that need a hook in mbx before a
  test can catch them, and 4 were caught by a test that quicksilver could
  not use at the time. None was an equivalent mutant.
- The agents wrote 29 tests, and `prove` confirmed that each one catches
  its mutant. A run with all 29 caught 163 of 169 mutants. Review retained
  23 new tests and 3 extended tests, guarding 27 mutants; these were merged
  in [#854](https://github.com/jdx/mr-boxington/pull/854). The reduced suite
  was not remeasured.
- Investigations during the campaign also uncovered five mbx defects:
  a flight-wait lock after a mismatched result, a same-tick input change
  escaping verification, restored outputs triggering unnecessary rebuilds
  on ZFS, a swallowed Cargo metadata error, and a lost build session under
  the Cargo shim. All five have tested fixes submitted upstream:
  [#859](https://github.com/jdx/mr-boxington/pull/859),
  [#860](https://github.com/jdx/mr-boxington/pull/860),
  [#861](https://github.com/jdx/mr-boxington/pull/861),
  [#862](https://github.com/jdx/mr-boxington/pull/862), and
  [#863](https://github.com/jdx/mr-boxington/pull/863).

Times are from a 96-CPU machine shared with other work.

### A large project: Bifrost

[Bifrost](https://github.com/BrokkAi/bifrost) is a multi-language static
analyzer: about 30 crates, 3.3 million lines of Rust and more than 12,000
tests.

- Agents worked through the workspace one crate at a time, in 12 rounds,
  and wrote about 7,000 mutants.
- In the round for the C# analyzer, on a 64-CPU machine, `map` took 16
  minutes (948.9 seconds). After `map`, `build` incrementally rebuilt the
  changed crates for 521 mutants in 642.5 seconds, and `test` ran them in
  3,547.7 seconds: about 70 minutes for build and test together.
- In three rounds, every survivor was reviewed and followed up:

| Crate | Mutants | Caught | Missed | Test gap | Suspected bug | Equivalent | Unreachable | Other |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| C# analyzer | 664 | 459 | 205 | 128 | 9 | 33 | 22 | 13 |
| PHP analyzer | 442 | 295 | 147 | 86 | 12 | 30 | 14 | 5 |
| Ruby analyzer | 343 | 255 | 88 | 49 | 11 | 18 | 8 | 2 |
| **Total** | **1,449** | **1,009** | **440** | **263** | **32** | **81** | **44** | **20** |

*Test gap*: the mutant changes behavior that no test checks. *Suspected
bug*: reviewing the survivor flagged a possible defect in the original code.
*Equivalent*: the mutant cannot change what the program does.
*Unreachable*: the mutant is in code that cannot run, or in a defensive
check. *Other*: 18 mutants change only log or diagnostic output; 2 are
caught by tests added later.

The agents added tests for 229 of the 263 gaps, each one checked to fail
with its mutant and pass without it. The 32 suspected-defect labels above
are initial review categories, not a count of distinct production bugs.
Follow-up tests and investigation found and fixed **at least 65 distinct
production bugs**: 21 in C#, 27 in PHP and 17 in Ruby. Each has a regression
test recorded failing before the fix. The fixes include a C# stack overflow
on deeply nested expressions, PHP interpreting an octal directory-depth
argument as decimal, and a Ruby factory-cache panic. They landed in the
C# [coverage](https://github.com/BrokkAi/bifrost-dev/pull/7157) and
[follow-up](https://github.com/BrokkAi/bifrost-dev/pull/7320) PRs, the PHP
[coverage](https://github.com/BrokkAi/bifrost-dev/pull/7188) and
[follow-up](https://github.com/BrokkAi/bifrost-dev/pull/7228) PRs, and the Ruby
[follow-up](https://github.com/BrokkAi/bifrost-dev/pull/7290) PR.

The campaign also removed 19 dead functions. A further confirmed Ruby bug,
[lost absolute-versus-lexical name context](https://github.com/BrokkAi/bifrost-dev/issues/7287),
remains open pending a producer contract change; it is excluded from the
fixed-bug count.

</details>


## Quick tutorial

You need Linux or another Unix system, Rust 1.99 or newer and git. Install
quicksilver and its [dependencies](#dependencies) together:

```
cargo install --locked cargo-quicksilver cargo-nextest mbx
```

Take a crate, `voting`, with one function and two tests in `src/lib.rs`:

```rust
/// Whether someone of this age may vote.
pub fn can_vote(age: u32) -> bool {
    age >= 18
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adults_can_vote() {
        assert!(can_vote(30));
    }

    #[test]
    fn children_cannot_vote() {
        assert!(!can_vote(10));
    }
}
```

Quicksilver tests a commit, so commit first.

**1. Try the built-in mutants.**

```
cargo quicksilver run
```

```
outcomes: 2 total — 2 caught, 0 missed, 0 not covered, 0 unviable, 0 errored, 0 timeout (2 mutant builds, 1 cargo invocations)
```

Both caught: a `can_vote` that always returns `true` fails
`children_cannot_vote`, and one that always returns `false` fails
`adults_can_vote`. The tests look complete. They are not.

**2. Write a realistic mutant.** First record which tests reach which
functions:

```
cargo quicksilver map
```

Now make the classic boundary mistake in your checkout: change
`age >= 18` to `age > 18`. Capture it, then put your code back:

```
cargo quicksilver build
git checkout -- .
```

```
  COMPILED d37f3e473d1f9fd2 voting.can_vote
build: 1 new, 1 untested in total, 0 unviable (generation 1, 1 cargo invocation(s), 2.7s)
```

**3. Test it.**

```
cargo quicksilver test
```

```
  MISSED      d37f3e473d1f9fd2 voting.can_vote (2 tests passed: voting::tests::adults_can_vote, voting::tests::children_cannot_vote)
      src/lib.rs:1
        {
      -     age >= 18
      +     age > 18
        }
```

Missed: nothing tests an 18-year-old.

**4. Close the gap.** Add the test:

```rust
    #[test]
    fn eighteen_year_olds_can_vote() {
        assert!(can_vote(18));
    }
```

and check that it catches the mutant (`d37f` is the start of its id).
`prove` puts the mutant into your code, runs the command, takes the mutant
out again, and runs the command once more:

```
cargo quicksilver prove d37f -- cargo test
```

```
caught: d37f3e473d1f9fd2 voting.can_vote: the command fails with the mutant and passes without it
```

On a real project, `cargo quicksilver uncovered` lists the functions no
test reaches at all, `pending` lists the ones that have no mutant yet, and
`survivors` collects every missed mutant for review. `cargo quicksilver
clean` removes everything quicksilver keeps for a repository.


## Using quicksilver with a coding agent

Point your agent at [RUNNING.md](https://github.com/BrokkAi/quicksilver/blob/master/RUNNING.md),
the complete guide to every command, setting and verdict, written so that
an agent can read it once and drive quicksilver from there. For example:

> Read https://github.com/BrokkAi/quicksilver/blob/master/RUNNING.md. Map
> this workspace with cargo quicksilver, write realistic mutants for the
> functions `cargo quicksilver pending --package my_crate` lists, and test
> them. For each survivor, add a test that catches it and check it with
> `cargo quicksilver prove`, or explain why the mutant cannot change
> observable behavior.


## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for
the full text.
