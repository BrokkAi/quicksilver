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
  at run time, so a thousand mutants cost one build, not a thousand. On a
  real workspace of 7 crates and 2,100 tests, 169 hand-written mutants
  compiled in one cargo invocation, in 38 seconds.
- **Only the tests that matter.** The same switch records which tests enter
  which functions. A mutant runs only the tests that reach it, cheapest
  first, so a kill is usually found early.

**Realistic mutants find real gaps.** Built-in mutants are blunt: replace a
body with `0`, `true` or `None`. Real bugs are an off-by-one, a swapped
condition, a forgotten case. With quicksilver you, or a coding agent, write
those as ordinary edits to function bodies, and quicksilver turns each
edited function into a mutant. In that same workspace, of the first 20
surviving mutants reviewed, 13 pointed at behavior its tests did not check.

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
  Quicksilver runs the mapped and mutant tests directly, in its own
  sandboxes, to record coverage and activate each mutant.
- **[mbx](https://mr-boxington.jdx.dev)** manages build state for
  quicksilver's isolated git worktrees. It gives each worktree its own
  [target directory](https://mr-boxington.jdx.dev/managed-targets), reuses
  matching build outputs, and can seed
  [private incremental compiler state](https://mr-boxington.jdx.dev/incremental)
  from another checkout when quicksilver edits a crate. Before removing a
  worktree, quicksilver calls `mbx clean` to release its target and
  incremental state. Requiring mbx lets quicksilver delegate build reuse
  and cleanup to one backend; it replaces quicksilver's former build-output
  cloning layer. Quicksilver invokes mbx directly, so no global Cargo
  wrapper setup is needed.

Both must be on `PATH`. They are executable dependencies:
[`cargo install`](https://doc.rust-lang.org/cargo/commands/cargo-install.html)
does not install them automatically with quicksilver, but you can install
all three in one command below. Existing installations can be reused.


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
