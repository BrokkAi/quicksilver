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

In both projects, agents wrote realistic mutants and reviewed the survivors.
Most reviewed mutants exposed behavior the tests did not check.

![Small project on the left, large project on the right: mbx had 31 test gaps among 40 reviewed mutants; Bifrost had 263 test gaps and 32 suspected bugs among 440 reviewed mutants.](https://raw.githubusercontent.com/BrokkAi/quicksilver/master/assets/results.svg)

| Follow-up in mbx | Follow-up in Bifrost |
| --- | --- |
| **26 tests retained**, bringing the suite to **163 of 169 mutants caught** | **229 test gaps covered** by proven tests; **31 bugs fixed** |

Bars show the share of reviewed mutants. The mbx review includes 37
survivors and 3 corrected false kills. Bifrost's review covers the C#, PHP
and Ruby analyzers. “Other findings” groups the remaining review categories,
broken down below.

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
  its mutant. Review kept 26 of them. With those tests, 163 of the 169
  mutants are caught.

Times are from a 96-CPU machine shared with other work.

### A large project: Bifrost

[Bifrost](https://github.com/BrokkAi/bifrost) is a multi-language static
analyzer: about 30 crates, 3.3 million lines of Rust and more than 12,000
tests.

- Agents worked through the workspace one crate at a time, in 12 rounds,
  and wrote about 7,000 mutants.
- In the round for the C# analyzer, on a 64-CPU machine, `map` took 16
  minutes. After `map`, `build` incrementally rebuilt the changed crates
  for 521 mutants in 11 minutes, and `test` ran them in an hour.
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
check. *Other*: the mutant changes only log or diagnostic output, or a test
added later catches it.

The agents added tests for 229 of the 263 gaps, each one checked to fail
with its mutant and pass without it. Follow-up fixed 31 of the 32 suspected
bugs in Bifrost; the last one needs a design decision first.

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
