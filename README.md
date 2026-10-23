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

You need git, [cargo-nextest](https://nexte.st), and the
[mbx](https://mr-boxington.jdx.dev/installation) build cache, which
quicksilver builds through.

```
cargo install cargo-nextest
cargo install --path .
```


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

Quicksilver replaces each function's body with a simple value of its return
type, such as `0`, `true`, `None` or `Default::default()`. It prints a
summary and a line for each missed mutant:

```
outcomes: 120 total — 46 caught (0 by crash), 1 missed, 56 not covered, 16 unviable, 0 errored, 1 timeout (59 mutant builds, 5 cargo invocations)
  MISSED  a21a581a0a795431  clap_lex::clap_lex.ext.OsStr.contains  { true }
```

That line says no test notices when `OsStr::contains` always returns `true`.
`run` exits with 1 when a mutant was missed, so it can gate CI.

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
   captured 1 mutant(s) from /src/clap
     e1a2853eb638848c clap_builder.builder.arg.Arg.required
     COMPILED e1a2853eb638848c clap_builder.builder.arg.Arg.required
   ```

   A body that does not compile is reported as `UNVIABLE` with the compiler's
   message. An edit outside a body is rejected with its file and line.

   Repeat steps 2 and 3 for as many mutants as you like. A second mutant of
   a function you already edited needs its own round.

4. **Test them all.**

   ```
   cargo quicksilver test
   ```

   ```
     CAUGHT      e1a2853eb638848c clap_builder.builder.arg.Arg.required by default_vals::default_ifs_arg_present_order
   ```

   Each mutant runs only the tests that enter the functions it changed.


## Find functions no test reaches

After `map`, list every function no test enters, as `path:line name`:

```
cargo quicksilver uncovered
```

These are test gaps that need no mutants to find. Mutants there would only
come back not covered, so write tests first. `--package` and `--file` narrow
the list.


## What the results mean

- **Caught**: a test fails with the mutant and passes without it.
- **Missed**: every test that reaches the changed code still passes. Either
  the tests do not check this behavior, or the change makes no observable
  difference. These are the ones to read.
- **Not covered**: no test reaches the changed function.
- **Unviable**: the mutant does not compile.
- **Timeout**: a test hangs with the mutant and finishes without it.
- **Errored**: a test could not run properly, for example because it fails
  even without the mutant.


## Clean up

Quicksilver builds in git worktrees under `.git/quicksilver/`, which
`git worktree list` shows. To remove them and everything quicksilver keeps
for the repository:

```
cargo quicksilver clean
```

`cargo quicksilver --help` lists the other commands, such as `estimate`.


## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for
the full text.
