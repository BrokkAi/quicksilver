# cargo-quicksilver

cargo-quicksilver is a mutation tester for Cargo workspaces. It changes your
code on purpose, runs your tests, and reports the changes your tests did not
notice. Each change is called a *mutant*. A mutant that makes a test fail is
*caught*. A mutant that every test passes is *missed*, and points at a gap in
your tests.

It is fast for two reasons:

- **One build for many mutants.** Quicksilver puts a small switch at the
  start of every function and compiles the workspace once. Each mutant is
  turned on at run time, so a thousand mutants cost one build, not a
  thousand.
- **Only the tests that matter.** The same switch records which tests enter
  which functions. A mutant runs only the tests that reach it.

You can let quicksilver write the mutants, or write your own, by hand or with
a language model.


## Install

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
builds get sessions of their own.


## Test a change

Commit first: quicksilver tests a commit, never uncommitted edits. Then
mutate the functions your change touches:

```
cargo quicksilver run --in-diff origin/main...HEAD
```

Or every function in a package:

```
cargo quicksilver run --package my_crate
```

With `--package`, only that package's own tests are built and run. To add a
test binary of another package that exercises it, name the binary as nextest
does: `--extra-tests other_pkg::suite_name` (repeatable, or
`extra-tests = [...]` in `quicksilver.toml`). `--reverse-dependents` adds the
tests of every workspace package that depends on it. For a crate low in the
dependency graph, that can be most of the workspace's tests.

To add partner tests whenever a particular package is selected, configure:

```toml
[package.my-language]
extra-tests = ["my-language-analysis"]
```

These entries apply only with `--package my-language`, after CLI and
top-level `extra-tests`, without duplicates. Names are nextest binary ids:
`pkg` for unit tests and `pkg::target` for integration tests.

Quicksilver replaces each function's body with a simple value of its return
type, such as `0`, `true`, `None` or `Default::default()`. It prints a
summary, here for clap's `clap_lex` package:

```
outcomes: 67 total — 40 caught, 0 missed, 13 not covered, 14 unviable, 0 errored, 0 timeout (51 mutant builds, 2 cargo invocations)
sandbox: 240 test processes ran with their own HOME and TMPDIR; the largest left 1.7 KiB; all deleted
```

Each missed mutant also gets a line with its id, function and new body, such
as `MISSED <id> my_crate::my_crate.parse.is_valid { true }`: no test notices
when `is_valid` always returns `true`. `run` exits with 1 when a mutant was
missed, so it can gate CI.

To keep quicksilver away from a function, mark it with `#[mutants::skip]`.


## Write your own mutants

Quicksilver's own mutants are blunt. A model, or you, can write realistic
ones: an off-by-one, a swapped condition, a forgotten case. The loop is:

1. **Map once per commit.**

   ```
   cargo quicksilver map
   ```

   This builds the workspace with a switch in every function and records
   which functions each test enters.

   Mapping the same commit again, for example with more `--extra-tests`,
   keeps the mutants you captured. With the same test packages, their
   builds and verdicts stay too. With other test packages, kills and
   unviable mutants stay, and the next `build` compiles the rest again so
   that `test` can run the new tests against them.

2. **Edit function bodies** in your checkout, as many as you like, in any
   files. Each edited function becomes its own mutant. Only edits inside
   function bodies are accepted: not signatures, not new items, not test
   code, not new files.

3. **Build the mutant, then reset the checkout.**

   ```
   cargo quicksilver build
   git checkout -- .
   ```

   `build` captures your edits and compiles all of them in one build, in
   quicksilver's own copy; your checkout is only read.

   ```
     e1a2853eb638848c clap_builder.builder.arg.Arg.required
   captured 1 mutant(s) from /src/clap
     COMPILED e1a2853eb638848c clap_builder.builder.arg.Arg.required
   ```

   A body that does not compile is reported as `UNVIABLE` with the compiler's
   message. Mutants of functions that return `impl Trait` are built one at a
   time, since each may return a different concrete type. An edit outside a body rejects the round with its file and line;
   `--skip-outside-bodies` drops such edits with a warning and keeps the rest.

   Repeat steps 2 and 3 for as many mutants as you like. A second mutant of
   a function you already edited needs its own round.

   Several authors can work at once, for example one agent per worktree at
   the mapped commit. Collect each one's edits as a patch (`git diff`) and
   build them all at once, with no checkout to apply them to:

   ```
   cargo quicksilver build --patch lane1.patch --patch lane2.patch
   ```

   Each patch is captured on its own. Concurrent `build` and `test` runs
   wait for each other.

   To keep what each mutant was meant to model, pass
   `--intents notes.json`: a JSON object from a function's full name (as
   quicksilver prints it) or `path:line` to a note, such as
   `{"crate_a/src/lib.rs:2": "subtracts instead of adding"}`. Survivors are
   shown with their note and a diff of the original body. One file can
   serve several builds: `build` warns only about keys that name no
   function in the map, and counts the keys meant for other builds in one
   line. Use `--patch lane.patch=notes.json` for notes that apply only to
   that patch, so different mutants of the same function can each have a
   note. Only this explicit form loads per-patch notes; no sidecar file is
   picked up automatically. Per-patch notes take precedence over global
   `--intents` notes. When a duplicate mutant carries a note, it fills a
   missing note on the kept mutant; an existing different note is kept.
   Build reports both cases with the mutant id and source.

4. **Test them all.**

   ```
   cargo quicksilver test
   ```

   ```
     CAUGHT      e1a2853eb638848c clap_builder.builder.arg.Arg.required by clap::builder::default_vals::default_ifs_arg_present_order
   ```

   Each mutant runs only the tests that enter the functions it changed,
   cheapest first by the durations the map recorded, so a kill is
   usually found early; a survivor runs them all. Every five minutes,
   `test` prints how many mutants are decided and about how long the rest
   will take. Each verdict is saved as soon as it is decided. To stop a long run,
   press Ctrl-C or send SIGTERM: the mutants already started finish, and
   `--json` is written. A second signal stops at once. Either way, running
   `test` again tests only the mutants without a verdict, and its `--json`
   file holds every verdict for the map, not only that run's.

   Tests are named by their id, the test binary as nextest names it
   (`clap::builder`) followed by the test's name, as in the map's test
   list.

   A verdict that only [proptest](https://docs.rs/proptest) properties
   decided is flagged: properties run random cases, so the verdict can
   change between runs. `--proptest-cases 1024` runs such a survivor once
   more with `PROPTEST_CASES=1024` before calling it missed; the verdict
   then says "still missed with PROPTEST_CASES=1024", and `--json` records
   `proptest_rerun_cases`.


## Find functions no test reaches

After `map`, list every function no test enters, as `path:line name`:

```
cargo quicksilver uncovered
```

These are test gaps that need no mutants to find. Mutants there would only
come back not covered, so write tests first. `--package` and `--file` narrow
the list.

When the map limits tests to some packages, a hint names omitted workspace
dependents whose tests may enter these functions. It shows up to five, ranked
by how many listed function names appear as whole words in each package's
committed Rust sources, with counts. This is a name match, not proof of a call.
The hint also counts additional matches and packages that name none; if ranking
is unavailable, it lists all omitted dependents. Add their binaries with
`--extra-tests` or `[package.<name>] extra-tests` and map again.

A function marked `[no callers]` is named nowhere in the workspace except
its own definition and its own file's tests: check whether it is dead code
before writing tests for it. Test scaffolding (functions in modules such as
`test_support`, named `*_for_test`, or under a `cfg` that mentions `test`)
is left out; `--scaffolding` lists it.


## Find functions that still need mutants

After `map`, list entered functions with no captured mutant:

```
cargo quicksilver pending --min-lines 5 --package my_crate
cargo quicksilver pending --split 4 --out mutant-lanes
```

Each line gives `path:start-end name (N lines)`. `--all` includes functions that
already have mutants and shows their counts. Unviable mutants do not count;
test scaffolding and functions no green test entered are left out.
`--file` narrows the list and `--json FILE` saves the rows, including `line`
and `end_line` for each function.

`--split N --out DIR` also writes `pending-1.txt` through `pending-N.txt`,
assigning the largest functions first to the list with the fewest lines so
far. The summary reports functions with mutants and remaining functions and
lines; each split file gets its own function and line counts.


## Review the survivors

After `test`, list mutants that no selected test caught:

```
cargo quicksilver survivors --package my_crate --file 'src/**/*.rs'
cargo quicksilver survivors --split 4 --out survivor-lanes
cargo quicksilver survivors --at master
```

Each record shows the mutant id and intent, its function and unified body
diffs, how many tests ran and were selected (with a short sample), and
the function's body status at the chosen revision. `--json FILE` saves the
same records as JSON. `--at REV` compares the first edited function against
that commit (default: HEAD), reporting `unchanged`, `changed`, `gone`, or `unknown` when
the file cannot be parsed. The summary counts changed and gone survivors.
JSON includes `at_commit` and `at_status`, and retains `at_head` as a function
existence check. `--split N --out DIR` writes `survivors-1.txt` through
`survivors-N.txt`, balancing survivor counts while keeping each source file
in one list. Functions match by name and methods by type and name; body
comparisons use the exact original text saved by the map.


## Prove a kill in another checkout

Run these commands in your own workspace checkout or Git worktree of the
same repository. They read the repository's shared captured mutant state,
so the checkout may be at a different commit from the map:

```
cargo quicksilver apply <ID>
cargo quicksilver apply <ID> --revert
cargo quicksilver prove <ID> -- cargo test test_that_should_catch_it
```

`ID` can be the full mutant id or a unique prefix. `apply` locates each
function by name and matches its original body byte for byte before
replacing it. It validates every edit before writing; if a function has
changed since the map, it refuses and shows a diff. Changes outside the
matched bodies stay in place. Repeating an apply or revert reports that
there is nothing to change. QS refuses to edit its own managed worktrees.

`prove` applies the mutant, runs your command with your environment and
stdio, reverts, then runs the command again. It also reverts if the command
cannot start, and if you press Ctrl-C, after which it stops (exit 3). The final result is `caught` (exit 0) when
the command fails with the mutant and passes without it, `not caught`
(exit 1) when both runs pass, or `inconclusive` (exit 2) when the command
fails without the mutant too. Errors such as a refused apply or a command
that cannot start use exit 3. You can also apply, run your checks manually,
and revert; reverting needs no record of the earlier apply.

`prove` judges only the command's exit status. If the mutant does not
compile in this checkout (code elsewhere changed since the map), a cargo
command fails as well, and `prove` says `caught`: read the command's output,
which `prove` shows, before you count the kill.


## What the results mean

- **Caught**: a test fails with the mutant and passes without it.
- **Missed**: every test that reaches the changed code still passes. Either
  the tests do not check this behavior, or the change makes no observable
  difference. These are the ones to read. A selected test that fails even
  without the mutant (such as a wall-clock watchdog the slower instrumented
  build cannot meet) cannot judge it; if every other selected test passed,
  the mutant is still missed, and its verdict names the tests that could
  not judge it. Within one `test` run, such a test is not run again for
  later mutants, and the run ends with a list of these tests to add to
  `[map] skip-tests`.
- **Not covered**: no test reaches the changed function.
- **Unviable**: the mutant does not compile.
- **Caught by timeout**: the mutant uses at least three times the test's
  recorded CPU time plus one second, or it times out again with four times
  the wall budget while using less than 5% of a CPU (blocked, for example
  by a deadlock). These kills appear separately in the caught total.
- **Timeout**: inconclusive. The test still exceeds four times its budget
  at a normal CPU rate without using three times its usual CPU. Host load
  is the likely cause. `[test-budget]` in `quicksilver.toml` gives named
  tests a longer minimum budget, for example `"my_crate::tests::waits" = 60.0`.
- **Errored**: a test could not run properly, for example because every
  selected test fails twice even without the mutant. The final list gives each mutant id and
  reason. Signal exits name the signal (for example `9 (SIGKILL)`); a test
  stopped at its deadline says `stopped by quicksilver`.

Repeat timeout kills and inconclusive timeouts on a quieter host with
`cargo quicksilver test --again timeouts`. Use `--again errored` for errors,
or repeat `--again` to select both kinds. `--mutant PREFIX` limits these
selections to matching ids. Built, untested mutants are also tested. Current
staged binaries are reused; if their generation is stale or their files are
missing, run `cargo quicksilver build` first. Old states without saved build
metadata also need a build before repeating verdicts.


## Choose the tests the map uses

Some tests exist only under a cfg, such as heavy tests marked
`#[cfg_attr(not(scheduled_tests), ignore)]` that a release gate runs with
`RUSTFLAGS='--cfg scheduled_tests'`. Without that cfg they are ignored and
pin no mutants. `[build-env]` in `quicksilver.toml` sets variables for
every build quicksilver runs, so the same flag maps them:

```toml
[build-env]
RUSTFLAGS = "--cfg scheduled_tests"
```

As with cargo, `RUSTFLAGS` replaces the rustflags from `.cargo/config.toml`
and from your environment, so repeat any you need. The map records these
settings: a new map of the same commit with other settings compiles your
mutants again (kills stay), and `build` warns when they changed since
`map`.

A test that cannot pass under quicksilver, such as a timing guard that the
switch in every function slows down, can be left out by its id, the binary
as nextest names it followed by the test's name (globs allowed):

```toml
[map]
skip-tests = ["my_crate::suite::*::indexes_in_linear_time"]
```

`map` and `run` then never run it themselves, and count it as left out,
not as not green. The baseline `cargo nextest run` at the start still runs
it, but its failure no longer stops `map` or `run`.


## Tests run in a sandbox

A mutant can make a test write the wrong thing to the wrong place. So each
test process quicksilver starts gets its own `HOME`, `XDG_CONFIG_HOME`,
`XDG_CACHE_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` and `TMPDIR`, in a new
directory under the system temporary directory (a private `qs-sandbox-*`
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
quicksilver refuses one that is inside. Keep the path short, too: a Unix
socket's path must fit in about 100 bytes, and tools such as mbx put
sockets in `TMPDIR`.

Tests that run cargo, rustup, git or mbx still work: `CARGO_HOME`,
`RUSTUP_HOME` and `MBX_CACHE_DIR` point at your own, and your git and mbx
config files are copied in. A test that needs other files from your home
directory will fail in the sandbox and select no mutants.

`[test-env]` (below) is applied after these, so it can change them too. Do
that when the code under test writes to one of these places itself: a
mutant could otherwise write bad entries into your real cache. For
example, when testing mbx itself:

```toml
[test-env]
MBX_CACHE_DIR = "{sandbox}/mbx-cache"
```

`[test-env]` in `quicksilver.toml` sets more variables for every test
process; `{sandbox}` stands for the process's sandbox directory:

```toml
[test-env]
APP_CACHE_ROOT = "{sandbox}/cache"
```

A test that fails or times out with no mutant active selects no mutants.
`map` and `run` print why, with the last lines of its output, and the map
keeps the end of its output under `not_green_output`.

`--no-sandbox`, or `enabled = false` under `[sandbox]`, runs tests in your
own environment. The baseline `cargo nextest run`, which runs no mutants,
always runs in your environment: it runs through mbx, and a different
`HOME` can change how mbx caches builds.


## Clean up

Quicksilver builds in git worktrees under `.git/quicksilver/`, which
`git worktree list` shows. To remove them and everything quicksilver keeps
for the repository:

```
cargo quicksilver clean
```

`cargo quicksilver --help` lists the other commands.


## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for
the full text.
