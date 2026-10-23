# Build in git worktrees through mbx instead of scratch copies

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md` at the repository root.


## Purpose / Big Picture


cargo-quicksilver (QS) builds and runs tests in copies of the user's workspace, never in the checkout itself. Today those copies are directories under a "scratch root" that QS manages on its own: a baseline copy, worker copies made with reflinks or plain copies, and each copy's `target/`. Nothing outside QS knows about them, the default scratch path only exists on the author's machine, and QS forces build settings (`CARGO_INCREMENTAL=1`, its own `RUSTFLAGS`, a custom profile) that make the mbx build cache skip every QS compilation.

mbx is the Rust build cache the owner uses (source at `~/Projects/mr-boxington`, command `mbx`). It places each checkout's `target/` in its own managed directory, shares compiled work between checkouts by content, and collects a checkout's target once the checkout is gone or when told with `mbx clean <path>`.

After this change QS requires mbx and git, and asks the user to commit first. Each QS tree is a git worktree, visible in `git worktree list`, checked out at the commit under test. QS runs cargo through `mbx` with ordinary settings, so its builds share mbx's cache with each other and with the user's own builds. The clone layer (reflink, copy and ZFS backends, the probe, the baseline sync) is deleted. `cargo quicksilver clean` removes QS's worktrees and tells mbx their targets are gone.

To see it work: in a committed repository, `cargo quicksilver map` prints the worktree it built in, `git worktree list` shows it, and `cargo quicksilver clean` removes it.


## Progress


- [x] (2026-10-23 17:30Z) Read mbx's managed-target, incremental and cache-result docs and the prediction code; measured fresh worktree builds on clap (34.8 s with no lookups, then 7.9 s with 162 hits); confirmed `git worktree remove` without `--force` removes a worktree whose only extra files are ignored.
- [ ] Milestone 1: `src/worktree.rs` (git and mbx plumbing) with tests on temporary repositories.
- [ ] Milestone 2: QS on worktrees: `Paths`, baseline, workers, cargo invocation, the edited-mutant commands, `clean`; the clone layer, `probe`, and the scratch flags removed.
- [ ] Milestone 3: tests and README updated; the edited-mutant loop and `run` measured on clap.


## Surprises & Discoveries


- Observation: QS's builds opted out of parts of mbx and keyed their compilations differently from the user's. QS sets `CARGO_TARGET_DIR`, and mbx leaves an explicitly named target directory alone, so QS trees never got a managed target. QS also overwrote `RUSTFLAGS` with `-C link-arg=-fuse-ld=mold` (mold is installed here) and used its own `quicksilver` profile, so its compiler invocations differ from a plain `cargo test` build. My first reading, that QS's `CARGO_INCREMENTAL=1` made mbx bypass its cache, is likely wrong: the mbx session (a56c857e, on jdx main 6d5945d) reports that an mbx session forces `CARGO_INCREMENTAL=0` over an inherited 1 (`crates/mbx/src/session.rs:630-635`), and the 43 QS sessions still recorded show no incremental bypasses.
  Evidence: `src/cargo_cmd.rs::apply_build_env` and `rustflags` in QS before this plan; mbx's `docs/managed-targets.md` ("mbx does not override a target directory you set"); the mbx session's reply of 2026-10-23.

- Observation: "seeding" names two mbx features. Target-unit seeding copies registry and git dependency units into a new checkout's managed target and needs Cargo 1.100 (`target_seed.rs::keeps_units_in_directories`); it also needs a managed target, which QS's `CARGO_TARGET_DIR` prevented. Incremental seeding copies another checkout's private rustc incremental state for an edited workspace crate and needs rustc 1.98 (`rustc.rs::seed_mode_for`, mbx commit fdde96b, #766). On 1.99.1 a new worktree gets action-cache hits and incremental seeding, not unit copying.
  Evidence: the mbx session's reply; the local mbx checkout at 6ff583a predates fdde96b.

- Observation: automatic collection is not reliable on this host: a collector started by a sandboxed build is killed when the sandbox exits (a fix is on jbellis/mr-boxington `fix/gc-stamp-after-sweep`, not upstream). QS must call `mbx clean` itself. `mbx clean --under <absolute root>` cleans every recorded workspace below a path, including ones already deleted.
  Evidence: the mbx session's reply.

- Observation: the first build of a fresh clap worktree looked nothing up ("168 unconsulted"), the second took 7.9 s with 162 hits. mbx groups its predictions of what each compilation reads by the `Cargo.lock` digest, not by path, and keys them by the exact compiler invocation. QS's earlier builds of the same commit used other flags and were bypassed, so there was nothing to predict from.
  Evidence: `mbx explain --last` in the first worktree; `crates/mbx-cache-cargo/src/lib.rs::build_identity` (identity = lockfile digest, OS, architecture). Which difference (the mold flag, the custom profile, or the target directory) mattered is not isolated.

- Observation: mbx rejects `--config` before the subcommand (`mbx --config ... metadata` fails), but passes cargo subcommands such as `nextest` through.
  Evidence: `error: unexpected argument '--config' found` from `mbx --config 'profile.x.inherits="dev"' metadata`; `mbx nextest --version` prints `cargo-nextest 0.9.140`.


## Decision Log


- Decision: require mbx and git; no code path without mbx.
  Rationale: the owner's call (2026-10-23): without mbx, workers need the whole clone layer; with it, a worker is a fresh worktree mbx fills from its cache. Two code paths would each need debugging.
  Date/Author: 2026-10-23, owner.

- Decision: require the code under test to be committed. `map`, `run`, `estimate`, `index` and `audit-selection` refuse a checkout with modified tracked files. `list` scans the checkout as it is.
  Rationale: the owner's call. A worktree checks out a commit, so uncommitted edits would not be tested.
  Date/Author: 2026-10-23, owner.

- Decision: QS's worktrees live under `<git common dir>/quicksilver/`: `main` for the baseline and first worker, `w01`, `w02`, ... for extra `run` workers, which are removed at the end of each run.
  Rationale: inside the repository's own `.git`, so QS writes nothing beside the user's checkout and the trees vanish with the repository; registered with git, so `git worktree list` shows them. mbx users can add `.git/quicksilver` to `target.evict_first`.
  Date/Author: 2026-10-23, Claude.

- Decision: QS's state (map, mutant records, timings, staged test binaries, the coverage target) lives in `<main>/.quicksilver/`, which holds a `.gitignore` containing `*`.
  Rationale: never under `target/`, which mbx owns and may collect. A self-ignoring directory keeps `git status` clean in the worktree without editing the repository's exclude file, and `git worktree remove` treats ignored files as clean.
  Date/Author: 2026-10-23, Claude.

- Decision: before removing a worktree, run `mbx clean <path>`.
  Rationale: the owner noted that telling mbx is the best practice, and the mbx session confirmed the order (stop builds, `mbx clean <path>`, then `git worktree remove --force`). It removes the managed target, the `target` link and learned incremental state but keeps cache claims, so later builds still restore. `cargo quicksilver clean` also runs `mbx clean --under <worktree root>` to catch trees deleted by hand.
  Date/Author: 2026-10-23, owner and Claude.

- Decision: build with ordinary settings: the dev and test profiles, the user's `RUSTFLAGS`, no `CARGO_INCREMENTAL`, no `CARGO_TARGET_DIR`. Cargo runs as `mbx <subcommand>`. Only the LLVM coverage build keeps its own flags and target directory.
  Rationale: identical invocations share mbx cache entries and predictions with the user's own `cargo test` builds and between QS's worktrees, and an unset `CARGO_TARGET_DIR` gives each tree a managed target (needed for target-unit seeding from Cargo 1.100). mbx gives crates that keep changing (the ones carrying schema prologues) private incremental state on its own, seeded from another checkout on rustc 1.98 and later.
  Date/Author: 2026-10-23, Claude.

- Decision: test processes still get `CARGO_TARGET_DIR=<tree>/target`.
  Rationale: tests that run cargo themselves (trycmd building clap's examples) must build in the tree that holds the mutant even when the caller's environment names another target directory. `<tree>/target` is mbx's link, so this is the tree's own managed target.
  Date/Author: 2026-10-23, Claude.

- Decision: original source text for restoring files and for capturing edited bodies comes from git (`git cat-file` at the commit), not from a baseline copy.
  Rationale: the worktree itself holds schema source during builds; git has the exact committed bytes.
  Date/Author: 2026-10-23, Claude.


## Outcomes & Retrospective


(to be written)


## Context and Orientation


Today `types::Paths::for_session(scratch_root, repo_root)` names a session directory `<scratch_root>/<hash of repo path>` with `baseline/` (a copy of the checkout, made by `scratch::materialize_baseline` and kept current by `scratch::sync_baseline`), `baseline-cov/` (the LLVM coverage target directory), `index/` (artifacts such as `baseline.json` and `sitemap.json`), `workers/wNN/` (copies of the baseline made by `scratch::ensure_worker` through a `scratch::CloneBackend`), `runs/` (staged test binaries) and `state/` (outcomes and the edited-mutant records). `scratch::probe_and_select` chooses the clone backend.

Builds go through `cargo_cmd::cargo_command()`, which adds `--config profile.quicksilver.*` flags before the subcommand, and `cargo_cmd::apply_build_env`, which sets `CARGO_INCREMENTAL=1` and `RUSTFLAGS`. `cargo_cmd::build_tests_captured` and `build_tests` add `--profile quicksilver` and set `CARGO_TARGET_DIR`. `index::list_tests_plain` runs `cargo nextest list --cargo-profile quicksilver`; `baseline::run_green_suite` runs `cargo nextest run` with `nextest_profile_args()`. `exec::schema::scan_test_exes` looks for test binaries under `target/{quicksilver,debug,dev}/deps`.

`engine::prepare` builds the baseline, lists tests, discovers mutants and plans; `exec::schema::execute` runs crates in workers; `exec::edited` implements `map`, `build` and `test`, and `capture::capture` reads the baseline copy to split edited files.


## Plan of Work


Milestone 1. Create `src/worktree.rs`:

    pub struct Repo { pub toplevel: PathBuf, pub common_dir: PathBuf, pub prefix: String }
    pub fn repo_of(dir: &Path) -> Result<Repo>                // git rev-parse --show-toplevel, --git-common-dir, --show-prefix
    pub fn head(dir: &Path) -> Result<String>                  // git rev-parse HEAD
    pub fn require_committed(dir: &Path) -> Result<String>     // refuses modified tracked files; returns HEAD
    pub fn root(repo: &Repo) -> PathBuf                        // <common_dir>/quicksilver
    pub fn ensure(repo: &Repo, name: &str, commit: &str) -> Result<PathBuf>   // add or check out --detach --force
    pub fn remove(repo: &Repo, path: &Path)                    // mbx clean, then git worktree remove --force; warnings only
    pub fn list_ours(repo: &Repo) -> Result<Vec<PathBuf>>      // git worktree list --porcelain under root()
    pub fn state_dir(tree: &Path) -> Result<PathBuf>           // <tree>/.quicksilver with its `*` .gitignore
    pub fn show(repo_dir: &Path, commit: &str, path: &str) -> Result<Vec<u8>>   // git cat-file blob
    pub fn require_mbx() -> Result<()>                         // `mbx --version`

Tests: on a temporary repository, `ensure` creates a worktree at the commit, a second `ensure` at a later commit updates it and keeps an ignored file, `state_dir` leaves `git status` clean, `remove` deletes it and `list_ours` no longer shows it, `require_committed` refuses a modified tracked file.

Milestone 2. `Paths` gains `Paths::for_tree(main: &Path, prefix: &str)`: `baseline = main/<prefix>`, `session = main/.quicksilver`, `index`, `state`, `runs` and `cov_target` under it, `workers = <common_dir>/quicksilver`. `Config` drops `scratch_root`, `clone_backend`, `allow_copy`, `zfs_remote`, `zfs_dataset`; `Config::paths()` resolves the repository and the main worktree. `baseline::materialize_and_metadata` becomes `prepare_tree`: `require_mbx`, `require_committed`, `ensure(main, HEAD)`. `scratch.rs` shrinks to `check_free_space` and the worker functions: `ensure_worker(paths, slot)` returns the main tree for slot 0 and `ensure(w{slot:02}, commit)` for others; `remove_extra_workers` runs after `exec::schema::execute`. `cargo_cmd`: `cargo_command()` is `mbx`, no profile flags, `apply_build_env` sets only `[build-env]` and removes `CARGO_TARGET_DIR`; the coverage build sets its flags explicitly. Staging copies binaries with `reflink_copy::reflink_or_copy`. `exec::edited` and `capture` read originals with `worktree::show`; the map records `commit` and `prefix`. The CLI loses `probe`, `--scratch-root`, `--clone-backend`, `--allow-copy`, `--zfs-*` and gains `clean`.

Milestone 3. `tests/support::scratch_cfg` drops the removed fields; `tests/clone_probe.rs` goes; the engine, schema and edited tests pass; README rewritten for mbx, commit first, worktrees and `clean`; clap measured: `map`, one `build`, `test`, and a `run --limit` with several build jobs.


## Concrete Steps


From the repository root:

    cargo test --lib worktree::
    cargo test --test edited --test engine --test schema
    cd /mnt/optane/quicksilver/local/clap && <qs> quicksilver map && git worktree list && <qs> quicksilver clean


## Validation and Acceptance


`cargo test` passes. On clap, `map` prints the worktree path under `.git/quicksilver/main`; `git worktree list` shows it; `mbx explain --last` in it shows cache hits rather than incremental bypasses; `cargo quicksilver clean` removes it and `git worktree list` no longer shows it.


## Idempotence and Recovery


`ensure` checks out with `--force`, so a tree left holding schema source by a crash is restored on the next command. `clean` can always be run; it removes whatever of QS's worktrees exist. A worktree deleted by hand is cleared with `git worktree prune`.


## Artifacts and Notes


(to be filled)


## Interfaces and Dependencies


No new crates. `reflink-copy` stays, for `reflink_or_copy` when staging test binaries. Requires `git` and `mbx` on `PATH`, and cargo-nextest.
