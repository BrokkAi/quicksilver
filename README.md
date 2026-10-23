# cargo-quicksilver

cargo-quicksilver is a mutation tester for Cargo workspaces. It changes your
code on purpose, runs your tests, and reports the changes your tests did not
notice. Each change is called a *mutant*. A mutant that makes a test fail is
*caught*. A mutant that every test passes is *missed*, and points at a gap in
the tests.

Two things make it fast:

- **One build for many mutants.** Quicksilver inserts a small switch at the
  start of every function it mutates and compiles the workspace once. Each
  mutant is then turned on at run time with an environment variable, so a
  thousand mutants cost one build, not a thousand.
- **Only the tests that matter.** The same switch records which tests enter
  which functions. A mutant runs only the tests that reach it.

There are two ways to use it:

- **Built-in mutants.** Quicksilver writes the mutants itself, by replacing
  each function body with a simple value of its return type, such as `0`,
  `true`, `None` or `Default::default()`.
- **Your own mutants.** You, or a language model, edit function bodies in your
  checkout. Quicksilver checks the edit, compiles it into the same switched
  build, and runs the tests that reach the edited functions.

Your checkout is only ever read. Every build and test run happens in a git
worktree of your repository, built through the mbx build cache.


## Install

You need a Rust toolchain, git, [cargo-nextest](https://nexte.st), which
quicksilver uses to list and time tests, and
[mbx](https://mr-boxington.jdx.dev/installation), the build cache quicksilver
builds through.

```
cargo install cargo-nextest
cargo install --path .
```

Install mbx by following its installation guide. Quicksilver runs cargo as
`mbx build ...` itself, so `mbx setup` is not required, but it lets your own
builds share their cache with quicksilver's.

The default test selection, `coverage`, also needs the `llvm-tools`
component. The recommended selection, `sites`, does not.

```
rustup component add llvm-tools
```


## Where quicksilver builds

Quicksilver tests a commit, so commit your changes first. It refuses to start
when tracked files have uncommitted changes. Untracked files are ignored.

It builds in git worktrees of your repository, under `.git/quicksilver/`:

- `main` is checked out at the commit under test. It holds the baseline
  build and quicksilver's state, in `main/.quicksilver/`.
- `w01`, `w02` and so on are extra workers that `run` creates when it builds
  several crates in parallel. It removes them when the run ends.

`git worktree list` shows them. mbx gives each worktree its own build
directory and fills it from its cache, so a new worktree does not compile
from scratch.

To remove everything quicksilver keeps for a repository:

```
cargo quicksilver clean
```

This tells mbx to drop the worktrees' build directories, then removes the
worktrees and quicksilver's state. If you use mbx's budget settings, you can
let it collect these build directories first with
`mbx settings set target.evict_first .git/quicksilver`.


## Built-in mutants

Run every mutant in a package, with site-based test selection:

```
cargo quicksilver run --test-selection sites --package my_crate
```

Quicksilver builds the baseline, runs the suite once to time it, builds the
switched workspace, records which tests enter which functions, and then runs
each mutant. It ends with a summary and a line for every missed mutant:

```
outcomes: 40 total — 31 caught (0 by crash), 2 missed, 5 not covered, 2 unviable, 0 errored, 0 timeout (...)
  MISSED  <id>  my_crate::my_crate.parser.is_flag  { false }
```

Only mutate the functions a change touches, for example in CI:

```
cargo quicksilver run --test-selection sites --in-diff origin/main...HEAD
```

The second commit of `--in-diff` must be the one checked out.

Other commands:

- `cargo quicksilver list` prints every built-in mutant without running
  anything.
- `cargo quicksilver estimate --sample 150` runs a random sample and
  estimates how long a full run would take.
- `--json <file>` on `run`, `estimate` and `list` writes machine-readable
  results.
- `--resume` skips mutants already decided by an earlier run in the same
  repository.

`run` exits with 0 when no mutant was missed, and 1 when at least one was.

To keep quicksilver away from a function, mark it with
`#[mutants::skip]` or `#[quicksilver::skip]`. Quicksilver also never mutates
test code, `const fn`, `unsafe fn`, functions whose body is empty or only
`todo!()`, or proc-macro crates.


## Your own mutants

This workflow is three commands. Run `map` once, then repeat edit, `build`,
reset as often as you like, then run `test`.

### 1. Map the workspace

```
cargo quicksilver map
```

This builds the workspace once with a switch in every function and records
which functions each test enters. Run it again whenever you commit or pull:
the map belongs to one commit, and `build` refuses edits made on top of
another.

Quicksilver compares your edits with the files git tracks, and reads the
original text from the commit.

### 2. Edit function bodies

Change the code inside one or more function bodies. A mutant can change
several bodies at once, in several files and crates. They are switched on
together.

These edits are rejected:

- changes outside a function body, such as a signature, a `use` line, a
  struct, a constant or a comment between functions;
- changes to test code, including `#[cfg(test)]` modules and the `tests/`
  directory;
- new or deleted files.

### 3. Build

```
cargo quicksilver build --restore
```

`build` captures your edits as one mutant and compiles every captured mutant
that has not been tested yet. `--restore` then puts the captured files back
the way they were, ready for the next edit. Without it, reset them yourself,
for example with `git checkout -- .`.

```
captured mutant 669557f59202647f from /src/clap: edits clap_builder.builder.value_parser.FalseyValueParser.parse_ref + clap_builder.util.str_to_bool.str_to_bool
  COMPILED 669557f59202647f clap_builder.builder.value_parser.FalseyValueParser.parse_ref + clap_builder.util.str_to_bool.str_to_bool
build: 1 mutant(s) compiled, 0 unviable (generation 1, 1 cargo invocation(s), 9.6s)
```

A body that does not compile is reported as `UNVIABLE` with the compiler's
message. It does not stop the other mutants from building.

A rejected edit names the file, the line and the functions around it, and
nothing is captured:

```
the edits in /src/tiny were not captured:
  `crate_a/src/lib.rs`: edit outside a function body before `tiny_a.add` (line 1 of the edited file)
a mutant may only change function bodies outside test code; reset the checkout and edit again
```

`build` exits with 0 when everything compiled, 1 when a mutant was
unviable, and 2 when the edit was rejected. `--no-capture` builds what is
pending without reading the checkout.

### 4. Test

```
cargo quicksilver test
```

```
  MISSED      669557f59202647f clap_builder.builder.value_parser.FalseyValueParser.parse_ref + clap_builder.util.str_to_bool.str_to_bool (1 tests passed: flags::non_bool_type_flag)
  CAUGHT      e1a2853eb638848c clap_builder.builder.arg.Arg.required by app_settings::allow_missing_positional_no_default
```

A mutant whose functions no test enters is reported as `NOT_COVERED` without
running anything.

Each mutant is tested once. `--mutant <id prefix>` limits the run to
particular mutants, and `--json <file>` writes the results.


## What the results mean

- **Caught**: a test failed with the mutant switched on and passed without
  it. A test that crashes with the mutant on, twice, and passes without it
  also counts as caught.
- **Missed**: every test that reaches the mutated code still passed. Either
  the tests do not check this behavior, or the mutant does not change
  anything observable. Missed mutants are the ones to look at.
- **Not covered**: no test reaches the mutated function at all.
- **Unviable**: the mutant does not compile, so it was not run.
- **Timeout**: a test ran far longer than usual with the mutant on, and
  finished in normal time without it.
- **Errored**: a test could not be run properly, for example because it also
  fails without the mutant.


## Configuration

`quicksilver.toml` (or `.quicksilver.toml`) at the workspace root can set
`build-jobs`, `test-jobs`, and a `[build-env]` table of environment variables
for every build. Command-line flags override the file.

Builds use your workspace's ordinary dev and test profiles and your
`RUSTFLAGS`, so they share mbx cache entries with your own builds.

If your tests start `cargo` themselves (for example with trycmd or
snapbox), they run inside quicksilver's worktree and build there, so they
see the mutant too.


## Limitations

- Only function bodies are mutated. Mutations inside a body, such as
  swapping an operator, are only possible through your own mutants.
- Doc tests are not run.
- `const fn` bodies cannot hold the run-time switch, so they cannot be
  mutated.
- Tests that are flaky with no mutant switched on are reported as errored
  rather than guessed at.


## Research notes

The [`papers/`](papers/) directory contains research notes related to
mutation testing, causal program dependence, and higher-order mutants.


## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for
the full text.
