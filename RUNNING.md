# Running quicksilver

This is the complete guide to cargo-quicksilver: every command, setting
and verdict. It is written so that a coding agent can read it once and then
drive quicksilver on its own; people who want the details can read it too.
For a short introduction, see the [README](README.md).


## Contents

- [Requirements](#requirements)
- [How quicksilver works](#how-quicksilver-works)
- [Built-in mutants: `run`](#built-in-mutants-run)
- [Your own mutants: `map`, `build`, `test`](#your-own-mutants-map-build-test)
- [What the results mean](#what-the-results-mean)
- [Finding work and reviewing it](#finding-work-and-reviewing-it)
- [Configuration](#configuration)
- [How tests run](#how-tests-run)
- [Clean up](#clean-up)


## Requirements

Quicksilver runs on Linux and other Unix systems; it does not build on
Windows. You need git, Rust 1.99 or newer,
[cargo-nextest](https://nexte.st), and the
[mbx](https://mr-boxington.jdx.dev/installation) build cache, which
quicksilver runs every build through.

```
cargo install cargo-nextest
cargo install cargo-quicksilver
```

Run it as `cargo quicksilver`. If your `cargo` is mbx's cargo wrapper,
quicksilver leaves the wrapper's build session at start, so that its own
builds get sessions of their own. `cargo quicksilver --help` lists the
commands, and `cargo quicksilver <command> --help` their options.


## How quicksilver works

- A **mutant** is a deliberate change to one function's body. A mutant
  that makes a test fail is **caught**; one that every test passes is
  **missed**, and points at behavior no test checks.
- Quicksilver tests a **commit**, never uncommitted edits. It builds in its
  own git worktrees under `.git/quicksilver/`; your checkout is only read.
- It puts a small **switch** at the start of every function and compiles
  the workspace once. A mutant is turned on at run time, so one build
  serves many mutants.
- The same switch records which tests enter which functions: the **map**.
  A mutant runs only the tests that entered its function, cheapest first.
- Each test runs in its own process, in a sandbox (see
  [How tests run](#how-tests-run)).

There are two ways to use it. `run` writes simple mutants itself and tests
them in one command. `map`, `build` and `test` let you, or an agent, write
realistic mutants by editing function bodies.


## Built-in mutants: `run`

Commit first. Then mutate the functions a change touches:

```
cargo quicksilver run --in-diff origin/main...HEAD
```

or every function in a package:

```
cargo quicksilver run --package my_crate
```

Quicksilver replaces each function's body with a simple value of its return
type, such as `0`, `true`, `None` or `Default::default()`, and prints a
summary, here for clap's `clap_lex` package:

```
outcomes: 67 total — 40 caught, 0 missed, 13 not covered, 14 unviable, 0 errored, 0 timeout (51 mutant builds, 2 cargo invocations)
sandbox: 240 test processes ran with their own HOME and TMPDIR; the largest left 1.7 KiB; all deleted
```

Each missed mutant also gets a line with its id, function and new body, such
as `MISSED <id> my_crate::my_crate.parse.is_valid { true }`: no test notices
when `is_valid` always returns `true`. `run` exits with 1 when a mutant was
missed, so it can gate CI. `--json FILE` saves the outcomes, and `--resume`
skips mutants a previous run already decided. `cargo quicksilver list`
shows the mutants `run` would try.

To keep quicksilver away from a function, mark it with `#[mutants::skip]`.

### Which tests count

With `--package`, only that package's own tests are built and run. To add
more:

- `--extra-tests BINARY` adds one test binary of another package, named as
  nextest names it: `pkg` for its unit tests, `pkg::target` for an
  integration test. Repeatable.
- `--reverse-dependents` adds the tests of every workspace package that
  depends on the named one. For a crate low in the dependency graph, that
  can be most of the workspace's tests.
- In `quicksilver.toml`, `extra-tests = [...]` adds binaries to every
  package run, and a table per package adds them only when that package is
  named:

  ```toml
  [package.my-language]
  extra-tests = ["my-language-analysis"]
  ```

  These come after the command line's and the top-level list, without
  duplicates.


## Your own mutants: `map`, `build`, `test`

Quicksilver's own mutants are blunt. You, or a language model, can write
realistic ones: an off-by-one, a swapped condition, a forgotten case.

1. **Map once per commit.**

   ```
   cargo quicksilver map
   ```

   This builds the workspace with a switch in every function and records
   which functions each test enters. `--package`, `--extra-tests` and
   `--reverse-dependents` choose the tests, as for `run`.

2. **Edit function bodies** in your checkout: as many as you like, in any
   files. Each edited function becomes its own mutant. Only edits inside
   function bodies count: not signatures, new items, test code or new
   files.

3. **Build, then reset the checkout.**

   ```
   cargo quicksilver build
   git checkout -- .
   ```

   `build` captures your edits and compiles all of them in one build, in
   quicksilver's own worktree:

   ```
     7f0ab5a77c57f1b0 voting.can_vote
   captured 1 mutant(s) from /src/voting
     COMPILED 7f0ab5a77c57f1b0 voting.can_vote
   build: 1 new, 1 untested in total, 0 unviable (generation 1, 1 cargo invocation(s), 29.9s)
   ```

   A body that does not compile is reported `UNVIABLE` with the compiler's
   message. Mutants of functions that return `impl Trait` are built one at
   a time, since each may return a different concrete type. An edit outside
   a body rejects the whole round with its file and line;
   `--skip-outside-bodies` drops such edits with a warning and keeps the
   rest. For each source with at least 10 mutants, `build` says how many
   change a single token, and warns when most do: realistic faults usually
   need more than flipping one operator.

   Repeat steps 2 and 3 as often as you like. A second mutant of a function
   you already edited needs its own round.

4. **Test them all.**

   ```
   cargo quicksilver test
   ```

   ```
     MISSED      7f0ab5a77c57f1b0 voting.can_vote (2 tests passed: voting::tests::children_cannot_vote, voting::tests::adults_can_vote)
         voting/src/lib.rs:1
           {
         -     age >= 18
         +     age > 18
           }
   tested 1 mutant(s) in 0.0s: 0 caught, 1 missed, 0 not covered, 0 timeout, 0 errored
   ```

   Tests are named by their id: the test binary as nextest names it,
   followed by the test's name.

### Mapping again

Mapping the same commit again, for example with more `--extra-tests`,
keeps the mutants you captured. With the same test packages and build
settings, their builds and verdicts stay too. With others, kills and
unviable mutants stay, and the next `build` compiles the rest again so that
`test` runs the new tests against them. A map of another commit drops them.

### Several authors at once

Several authors can work at once, for example one agent per worktree at the
mapped commit. Collect each one's edits as a patch (`git diff`) and build
them all together, with no checkout to apply them to:

```
cargo quicksilver build --patch lane1.patch --patch lane2.patch
```

Each patch is captured on its own. Concurrent `build` and `test` commands
wait for each other.

### Notes on what a mutant models

A note says what fault a mutant models, such as "off-by-one at the upper
bound". Survivors are shown with their note.

- `--intents notes.json`: a JSON object from a function's full name (as
  quicksilver prints it) or `path:line` to a note, for example
  `{"crate_a/src/lib.rs:2": "subtracts instead of adding"}`. It applies to
  the mutants this build captures. One file can serve several builds:
  `build` warns only about keys that name no function in the map, and
  counts the keys meant for other builds in one line.
- `--patch lane.patch=notes.json`: notes for that patch's mutants only, so
  different mutants of one function can each have their own. These win
  over `--intents`. No notes file is picked up without this form.
- When a duplicate of an already captured mutant brings a note, it fills a
  missing note but never replaces one; `build` says which happened.

### Long test runs

Each mutant runs only the tests that entered the functions it changed,
cheapest first by the durations the map recorded, so a kill is usually
found early; a survivor runs them all. Every five minutes, `test` prints
how many mutants are decided and about how long the rest will take.

Each verdict is saved as soon as it is decided. To stop, press Ctrl-C or
send SIGTERM: the mutants already started finish, and `--json` is written.
A second signal stops at once. Either way, running `test` again tests only
the mutants without a verdict, and its `--json` file holds every verdict
for the map, not only that run's.

### Property tests

A verdict that only [proptest](https://docs.rs/proptest) properties decided
is flagged: properties run random cases, so the verdict can change between
runs. `--proptest-cases 1024` runs such a survivor once more with
`PROPTEST_CASES=1024` before calling it missed; its line then says
"still missed with PROPTEST_CASES=1024", and `--json` records
`proptest_rerun_cases`.


## What the results mean

- **Caught**: a test fails with the mutant, twice, and passes without it.
- **Caught by crash**: the test crashes with the mutant (a signal or an
  exit code other than libtest's) and passes without it.
- **Caught by timeout**: with the mutant, the test uses at least three
  times its recorded CPU time plus one second, or it runs past four times
  its budget while using under 5% of a CPU (blocked, for example by a
  deadlock). The summary counts these apart from other kills.
- **Missed**: every test that reaches the changed code still passes. Either
  the tests do not check this behavior, or the change makes no observable
  difference. These are the ones to read.
- **Not covered**: no test reaches the changed function.
- **Unviable**: the mutant does not compile.
- **Timeout**: inconclusive. The test ran past four times its budget at a
  normal CPU rate without using three times its usual CPU; the host's load
  is the likely cause. `[test-budget]` gives a test a longer minimum budget.
- **Errored**: the mutant could not be judged, for example because every
  selected test also fails without it. The end of `test` lists each one
  with the reason; signals are named, such as `9 (SIGKILL)`, and a test
  stopped at its deadline says `stopped by quicksilver`.

Some details:

- A selected test that fails even without the mutant (such as a wall-clock
  watchdog the slower instrumented build cannot meet) cannot judge it. If
  every other selected test passed, the mutant is still missed, and its
  verdict names the tests that could not judge it. Within one `test` run,
  such a test is not run again for later mutants, and the run ends with a
  list of these tests, for `[map] skip-tests`.
- A kill by a test that later in the same run fails with no mutant active
  is not trusted: the test may fail at random. Such a kill becomes errored,
  shown as `UNCONFIRMED`.
- `cargo quicksilver test --again timeouts` repeats timeout kills and
  inconclusive timeouts, for example on a quieter host; `--again errored`
  repeats errors. Repeat `--again` for both, and add `--mutant PREFIX` to
  narrow them. If the mutants' binaries are gone, run `build` first.


## Finding work and reviewing it

### Functions no test reaches

After `map`, list every function no test enters, as `path:line name`:

```
cargo quicksilver uncovered
```

These are test gaps that need no mutants to find; mutants there would only
come back not covered, so write tests first. `--package` and `--file`
narrow the list.

- `[no callers]` marks a function named nowhere in the workspace except its
  own definition and its own file's tests: check whether it is dead code
  before writing tests for it.
- Test scaffolding (functions in modules such as `test_support`, named
  `*_for_test`, or under a `cfg` that mentions `test`) is left out;
  `--scaffolding` lists it.
- When the map used only some packages' tests, a hint names workspace
  packages that depend on them and whose tests were not recorded, ranked
  by how many of the listed functions their code names (a name match, not
  proof of a call). Add their tests with `--extra-tests` or
  `[package.<name>] extra-tests` and map again.
- `--not-green` instead lists every test that failed with no mutant active
  when the map was built, with why and the end of its output.

### Functions that still need mutants

```
cargo quicksilver pending --min-lines 5 --package my_crate
cargo quicksilver pending --split 4 --out mutant-lanes
```

Each line is `path:start-end name (N lines)`, for a function that tests
enter and that has no viable mutant yet. `--all` also lists functions that
have mutants, with their counts. `--split N --out DIR` writes
`pending-1.txt` to `pending-N.txt` for N authors, balanced by lines.
`--json FILE` saves the rows.

### Survivors

```
cargo quicksilver survivors
cargo quicksilver survivors --split 4 --out survivor-lanes
cargo quicksilver survivors --at master
```

For each missed mutant, sorted by file and line:

- its id, function, `path:start-end` and note;
- how many tests ran and were selected, with a sample;
- a unified diff of each edit against the mapped original;
- the function's status at a revision (`--at REV`, default `HEAD`):
  `unchanged`, `changed`, `gone`, or `unknown` when the file does not
  parse. Methods are matched with their type.

`--split N --out DIR` writes `survivors-1.txt` to `survivors-N.txt`,
keeping each source file's survivors in one list. `--json FILE` saves the
records. The diffs are for reading; they do not apply with `git apply`.

### Proving a kill in another checkout

Run these in your own checkout or worktree of the same repository, at any
commit; they read the repository's shared quicksilver state:

```
cargo quicksilver apply <ID>
cargo quicksilver apply <ID> --revert
cargo quicksilver prove <ID> -- cargo test the_new_test
```

`ID` is a mutant id or a unique prefix of one.

- `apply` finds each function by name and replaces its body only if it
  still equals the mapped original byte for byte, and only once every edit
  of the mutant matches. Otherwise it changes nothing and shows how the
  function changed. `--revert` does the reverse and needs no record of the
  earlier `apply`. Both refuse to edit quicksilver's own worktrees.
- `prove` applies the mutant, runs the command, reverts, and runs it again.
  It always reverts, also when the command cannot start or you press
  Ctrl-C (then it stops). It prints one line and exits with 0 `caught`
  (fails with the mutant, passes without), 1 `not caught`, 2
  `inconclusive` (fails without the mutant too), or 3 for an error.
- `prove` judges only exit statuses. If the mutant does not compile in this
  checkout (other code changed since the map), a cargo command fails too
  and `prove` says `caught`: read the command's output before counting it.


## Configuration

Settings live in `quicksilver.toml` (or `.quicksilver.toml`) at the
workspace root.

| Setting | What it does |
|---|---|
| `build-jobs`, `test-jobs` | Builds and test processes at once, as `--build-jobs` and `--test-jobs`. |
| `extra-tests = [...]` | Test binaries of other packages to add, as `--extra-tests`. |
| `[package.<name>] extra-tests` | The same, only when `--package <name>` is given. |
| `[build-env]` | Variables for every build and baseline test run. |
| `[map] skip-tests` | Test ids (globs allowed) that quicksilver never runs. |
| `[test-budget]` | Minimum seconds per test id, for tests with long deadlines of their own. |
| `[test-env]` | Variables for every test process; `{sandbox}` is that process's sandbox. |
| `[sandbox]` | `enabled = false` turns the sandbox off; `dir` moves it. |

### Tests that exist only under a cfg

Some tests exist only under a cfg, such as heavy tests marked
`#[cfg_attr(not(scheduled_tests), ignore)]` that a release gate runs with
`RUSTFLAGS='--cfg scheduled_tests'`. Without that cfg they are ignored and
catch nothing. `[build-env]` sets variables for every build quicksilver
runs, so the same flag maps them:

```toml
[build-env]
RUSTFLAGS = "--cfg scheduled_tests"
```

As with cargo, `RUSTFLAGS` replaces the rustflags from `.cargo/config.toml`
and from your environment, so repeat any you need. The map records these
settings; `build` warns when they changed since `map`.

### Tests to leave out

A test that cannot pass under quicksilver, such as a timing guard that the
switches slow down, can be left out by its id (globs allowed):

```toml
[map]
skip-tests = ["my_crate::suite::*::indexes_in_linear_time"]
```

`map` and `run` never run it themselves, and count it as left out, not as
failing. The baseline `cargo nextest run` at the start still runs it, but
its failure does not stop quicksilver.

### Test budgets

```toml
[test-budget]
"my_crate::tests::waits_for_the_server" = 60.0
```


## How tests run

### The sandbox

A mutant can make a test write the wrong thing to the wrong place. So each
test process quicksilver starts gets its own `HOME`, `XDG_CONFIG_HOME`,
`XDG_CACHE_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` and `TMPDIR`, in a new
directory under the system temporary directory (one private `qs-sandbox-*`
directory per run), deleted when the process exits. At the end, quicksilver
says how many processes ran and the most any one left behind.

To put sandboxes elsewhere, for example when the temporary directory is a
small filesystem, set an absolute path:

```toml
[sandbox]
dir = "/fast/disk/qs-sandboxes"
```

The directory must be outside any git repository, or tests that check
whether their temporary directory is in a repository would see one;
quicksilver refuses a directory inside one. Keep the path short: a Unix
socket's path must fit in about 100 bytes, and tools such as mbx put
sockets in `TMPDIR`.

Tests that run cargo, rustup, git or mbx still work: `CARGO_HOME`,
`RUSTUP_HOME` and `MBX_CACHE_DIR` point at your own, and your git and mbx
config files are copied in. A test that needs other files from your home
directory fails in the sandbox and selects no mutants.

`--no-sandbox`, or `enabled = false` under `[sandbox]`, runs tests in your
own environment. The baseline `cargo nextest run`, which runs no mutants,
always runs in your environment.

### The test environment

Quicksilver runs each test itself, one process per test, not through
nextest or mbx. A test gets what nextest gives it: `CARGO` (the toolchain's
own cargo, not a wrapper), `CARGO_MANIFEST_DIR`, the `CARGO_PKG_*`
variables of its package, `NEXTEST=1`, `NEXTEST_EXECUTION_MODE` and
`NEXTEST_RUN_ID`. Its `PATH` starts with the toolchain's `bin` directory,
so `cargo` and `rustc` on `PATH` are the toolchain's even when a wrapper,
such as mbx's, comes first in yours. It does not get `CARGO_TARGET_DIR`,
and it does not run inside mbx's build session.

`[test-env]` sets more variables for every test process, and wins over all
of the above; `{sandbox}` stands for the process's sandbox directory:

```toml
[test-env]
APP_CACHE_ROOT = "{sandbox}/cache"
```

Use it when the code under test writes to a place the sandbox keeps
pointing at your own, so a mutant cannot write bad entries there. For
example, when testing mbx itself:

```toml
[test-env]
MBX_CACHE_DIR = "{sandbox}/mbx-cache"
```

Your mbx config is still copied in, so with a fresh cache each test starts
empty: a config that names a downloaded linker makes every test that builds
download it again (and can hit the download site's rate limit).
`MBX_LINKER = "system"` in `[test-env]` avoids that, but it overrides mbx's
choice of linker for every target, so tests of that choice then fail with
no mutant active.

### What can make a test invisible

- The mutant's switch, and the record of which functions a test enters,
  travel in environment variables whose names start with `QS_`. A test
  helper that starts a subprocess with a cleared environment (Rust's
  `Command::env_clear`) must keep them, or the subprocess neither records
  what it enters nor sees the mutant: the tests behind such a helper never
  catch a mutant in code only the subprocess runs.
- A test can pass under `cargo nextest run` and fail here, for example one
  that depends on something only a test runner sets. A test that fails or
  times out with no mutant active selects no mutants. `map` and `run` print
  why, with the last lines of its output; `cargo quicksilver uncovered
  --not-green` lists every such test with the end of its output.


## Clean up

Quicksilver builds in git worktrees under `.git/quicksilver/`, which
`git worktree list` shows. To remove them and everything quicksilver keeps
for the repository:

```
cargo quicksilver clean
```
