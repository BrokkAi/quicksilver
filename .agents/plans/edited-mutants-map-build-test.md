# Judge mutants written in a checkout: `map`, `build` and `test`

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md` at the repository root.


## Purpose / Big Picture


Today cargo-quicksilver (QS) invents its own mutants: for every function it finds, it replaces the body with a constant chosen from the return type, then runs the tests that enter that function. The generator is blunt. On clap, 20% of its mutants do not compile; on itertools, 46%. A language model (LLM) can write far more realistic mutants, but it has nothing to run them with. A model editing a checkout and running `cargo test` pays one rebuild per mutant and has to rediscover, every time, which tests matter, what a timeout means and how to keep its edits out of the user's tree.

After this change a model, or a person, can work like this:

1. `cargo quicksilver map` builds the workspace once with a runtime switch at the entry of every function and records, test by test, which functions each test enters. The result is a map from test to function, saved in the scratch session.
2. The model edits function bodies in its checkout. It may edit several bodies at once. It must not touch anything outside a function body, and it must not touch test code.
3. `cargo quicksilver build` reads the edited checkout, verifies that the edits are confined to function bodies, captures them as one mutant, and compiles every captured mutant into the single schema binary. It prints which mutants compiled and the compiler's reason for each that did not. The checkout is never compiled directly.
4. The model resets its checkout and repeats steps 2 and 3 as often as it likes.
5. `cargo quicksilver test` runs each compiled mutant against exactly the tests that entered the functions it changed, and prints `caught by <test>`, `missed`, `not covered`, `timeout` or `errored` for each.

The user's checkout is only ever read. Every build and every test run happens in a scratch copy.


## Progress


- [x] (2026-10-23 05:10Z) Read the schema engine (`src/exec/schema.rs`), the test runner (`src/exec/test.rs`), discovery (`src/discover.rs`), the baseline code (`src/baseline.rs`) and the fixtures, and designed the three commands.
- [x] (2026-10-23 05:30Z) Wrote this plan.
- [ ] Milestone 1: body splitting. `src/capture.rs` with `split_bodies` and unit tests.
- [ ] Milestone 2: function scan and the map artifact. `discover::scan_functions`, `src/sitemap.rs`, `cargo quicksilver map`.
- [ ] Milestone 3: capture and build. `capture::capture`, `src/exec/edited.rs::build`, `cargo quicksilver build`.
- [ ] Milestone 4: test. `src/exec/edited.rs::test`, `cargo quicksilver test`.
- [ ] Milestone 5: end-to-end test on the `tiny` fixture and a run on a real crate.


## Surprises & Discoveries


- Observation: a mutant that moves a parameter in its new body still compiles after the original body. The arm is `N => return { <body> }`, and `return` diverges, so the borrow checker does not see the move on the path that falls through to the original body.
  Evidence: reasoning from Rust's flow-sensitive move checking; to be confirmed by the fixture test in Milestone 5.


## Decision Log


- Decision: three commands, `map`, `build` and `test`, with artifacts between them, and no one-shot command.
  Rationale: the owner asked for exactly this split on 2026-10-23. `build` is the loop a model iterates on and must answer in seconds without running tests. `map` is paid once per baseline and reused across many `build` calls.
  Date/Author: 2026-10-23, owner and Claude.

- Decision: the model edits a checkout freely, and QS checks afterwards that the edits stayed inside function bodies, rather than giving the model a tool that can only replace bodies.
  Rationale: the owner's call. Editing files is what models do well; a body-only tool would be a second interface to learn and would still need the same check.
  Date/Author: 2026-10-23, owner.

- Decision: the body check is a byte-anchored split against the baseline, not a parse of the edited file.
  Rationale: discovery already knows every function's body byte range in the baseline. The text outside those ranges must appear verbatim and in order in the edited file; whatever lies between is the new body. Linear time, no parser, and the split produces the mutant's bodies as a side effect. Brace-escaping bodies get through this check and fail in the schema build, where rustc is the judge.
  Date/Author: 2026-10-23, Claude.

- Decision: edits to test code are rejected.
  Rationale: owner's call. Test modules and `tests/` files are not sites, so an edit there matches no body and is rejected with a message that names the location.
  Date/Author: 2026-10-23, owner.

- Decision: one mutant may change several bodies, in several files and crates, and is switched on as one activation (`QS_MUTANT=site:variant;site:variant`).
  Rationale: the owner expects several bodies at once to be the norm. The runtime already accepts a list of activations.
  Date/Author: 2026-10-23, owner.

- Decision: the map build has a site at every function with a body, including return kinds the built-in generator pre-filters (`&T`, `impl Trait`, `dyn Trait`), functions returning `!`, functions with trivial bodies and `unsafe fn`. It excludes `const fn` (a const body cannot call the runtime), functions with a skip attribute, and functions in test regions.
  Rationale: the pre-filters exist because `Default::default()` cannot type-check there, not because a prologue cannot be inserted. A model's body returns the right concrete type.
  Date/Author: 2026-10-23, Claude.

- Decision: a nested function inside another function's body is not its own site. The outer body contains it.
  Rationale: overlapping body ranges would make the anchor split ambiguous. An edit to the nested function is an edit to the outer body, which is what gets replaced.
  Date/Author: 2026-10-23, Claude.

- Decision: all builds use `cargo build --tests --workspace` in one worker tree.
  Rationale: cargo rebuilds only the crates whose files changed. Scoping the build to the mutated packages would also need a second build for test packages, as `run_crate` does today, and the workspace build is simpler to reason about.
  Date/Author: 2026-10-23, Claude.

- Decision: a mutant's new body is rendered as `return { <body> }`, keeping the braces, instead of the generator's `return <expr>`.
  Rationale: a model body has statements. `return { let x = ...; x }` is a block expression; `return let x = ...` is not Rust.
  Date/Author: 2026-10-23, Claude.

- Decision: the worker tree that `map` builds is kept, and `build` reuses it. `map` always clones a fresh one.
  Rationale: incremental rebuilds in `build` depend on the worker's target directory from the map build.
  Date/Author: 2026-10-23, Claude.


## Outcomes & Retrospective


Not yet written. To be filled at the end of Milestone 5.


## Context and Orientation


QS is a Cargo subcommand, `cargo quicksilver`, built from this repository. The binary is `cargo-quicksilver`; the library is `cargo_quicksilver`. The entry points are `src/main.rs`, `src/cli.rs` (the clap definitions) and `src/lib.rs` (`run_cli`, one `cmd_*` function per subcommand). Configuration is `src/config.rs`; `engine::config_from_cli` turns the parsed CLI into a `Config`, and `Config::paths()` gives the scratch layout as a `types::Paths`.

A scratch session is a directory under the scratch root named by a hash of the repository path. Inside it: `baseline/` is a copy of the user's checkout; `index/` holds artifacts such as `baseline.json` (test durations from one green run of the suite); `workers/wNN/` are copies of the baseline that builds happen in; `runs/` holds staged test binaries; `state/` holds results. `scratch::materialize_baseline` and `scratch::sync_baseline` copy the checkout into `baseline/`; `scratch::ensure_worker` deletes and re-clones a worker.

The schema engine is the mechanism that makes one build serve many mutants. `src/schema.rs` rewrites source: for each site (one function), it inserts a prologue after the body's opening brace:

    if let Some(v) = crate::__qs_rt::active(0x00000007) { match v { 0 => return true, 1 => return false, 2 => {}, _ => {} } }

and appends a module `__qs_rt` to the crate root. `active(site)` reads the `QS_MUTANT` environment variable once (a list like `7:1;9:0`) and answers with the active variant for that site. When `QS_RECORD_SITES=<file>` is set, `active` also appends the site id to that file the first time the process enters it. So the same call is the mutant switch and the coverage probe. Each variant arm has a byte range in the rewritten file (`AppliedVariant`), which is how a rustc error span is attributed to the variant that caused it.

`src/exec/schema.rs` drives the engine for the built-in generator: per crate, `run_crate` builds sites from mutants, applies them in a worker, builds with `--message-format=json`, and in `isolate` removes the variants rustc complains about, round by round, bisecting when a diagnostic has no usable span. Then `stage_binaries` copies the test binaries into a run directory with a `manifest.json` naming each test's binary and working directory, `null_schema_gate` checks a few tests pass with no mutant active, `identity_gate` checks a sample of sites behave as the original when their identity variant is active, and `record_sites` runs every candidate test once with `QS_RECORD_SITES` to learn which sites each test enters.

`src/exec/test.rs` runs tests: `run_mutant_tests_observed(planned, run_dir, tree_root, policy, env, complete_vector)` resolves the planned tests through the run directory's manifest, runs each as `<binary> --exact <name>` with the environment from `env` plus `CARGO_TARGET_DIR=<tree_root>/target`, and applies the confirmation policy: a failure is re-run, then run with no mutant, before it counts as Caught. `record_site_hits(planned_union, run_dir, tree_root, policy, jobs)` is the recording run and returns a `SiteRecording` with `sites_by_test`, `secs_by_test` and `not_green`.

Discovery is `src/discover.rs`. `discover_with` walks Bifrost's declarations (Bifrost is the code analysis library QS uses to list functions; it is opened by `src/bifrost.rs`), skips test regions and test-like paths, resolves the package, parses each function with `syn` (`parse_fn`) to find the body's byte range and the return kind, and emits one `types::Mutant` per replacement. `Mutant` carries `package`, `path` (repository-relative, forward slashes), `fqn`, `start_byte..end_byte` (the whole function), `body_start_byte..body_end_byte` (the braces included) and `replacement`.

Test identity: a `types::TestInfo` has `id` (`<binary id>::<name>`), `name`, `package`, `binary_id`, `cwd_rel` and `duration_secs`. `index::list_tests_plain` lists tests with `cargo nextest list`; `baseline::run_green_suite` runs the suite once with nextest and reads durations from its JUnit output.

Terms used below. A *site* is one function that carries a prologue, identified by a `u32` site id hashed from (package, path, fqn, body start). A *variant* is one arm of a site's match. An *activation* is the list of (site, variant) pairs that `QS_MUTANT` switches on. An *edited mutant* is a mutant captured from the checkout: one or more (site, new body) pairs. The *map* is the artifact `map` writes: the sites, the tests, and which sites each test entered.


## Plan of Work


### Milestone 1: body splitting (`src/capture.rs`)


Create `src/capture.rs` and add `pub mod capture;` to `src/lib.rs`.

Define the pure function

    pub struct BodyRange { pub site_id: u32, pub fqn: String, pub start: usize, pub end: usize }

    pub enum Rejection {
        /// Text outside every body differs. `line` is 1-based in the edited file;
        /// `near` names the function whose surroundings did not match, or is empty
        /// before the first function.
        OutsideBody { line: usize, near: String },
        /// A new body does not start with `{` and end with `}` with balanced braces.
        Unbalanced { fqn: String },
    }

    pub fn split_bodies(original: &str, edited: &str, bodies: &[BodyRange])
        -> Result<Vec<(u32, String)>, Rejection>

Sort `bodies` by `start`. Walk the original and produce the outside-body segments: `original[0..b0.start]`, `original[b0.end..b1.start]`, ..., `original[bn.end..]`. Match them in order against `edited`: the first segment must be a prefix of `edited`; each following segment is searched from the current position with `str::find`; the last segment must end exactly at the end of `edited`. The text between the end of segment i and the start of segment i+1 is the new body of body i. Return only the bodies that differ from the original body text.

Brace balance: count `{` and `}` in the new body while skipping string literals, character literals, line comments and block comments. A body that does not start with `{`, end with `}` or balance is `Unbalanced`. This is a safety net against a body that closes the function early; rustc is the final judge.

When a segment is not found, compute how much of it matched (the longest common prefix of the segment and the edited text at the current position, or at the best candidate position) and report the 1-based line of the edited file where the match broke, with the fqn of the function whose body precedes the segment (or the one that follows, for the first segment).

Unit tests in the same file: an unchanged file yields no bodies; a one-line function `fn f() -> u32 { 0 }` edited to `{ 1 }` yields one body; two edited bodies in one file yield two; an edited signature is `OutsideBody` with the right line and fqn; an edit after the last function is `OutsideBody`; an edit inside `#[cfg(test)] mod tests` (which has no bodies in the list) is `OutsideBody`; a body with an extra `}` is `Unbalanced`; a body that contains a string with braces is fine.


### Milestone 2: function scan and the map (`src/discover.rs`, `src/sitemap.rs`, `qs map`)


In `src/discover.rs`, split `discover_with` into a scan and an expansion. Add

    pub struct FnDecl {
        pub package: String,
        pub path: String,
        pub fqn: String,
        pub start_byte: usize, pub end_byte: usize,
        pub start_line: usize, pub end_line: usize,
        pub body_start_byte: usize, pub body_end_byte: usize,
        pub return_kind: ReturnKind,
        pub unsafe_fn: bool, pub const_fn: bool, pub skip: bool, pub boring_body: bool,
        pub original_body: String,
    }

    pub fn scan_functions(workspace: &WorkspaceAnalyzer, root: &Path, filter: &DiscoverFilter,
                          diff: Option<DiffSelection>, diff_secs: f64) -> Result<Vec<FnDecl>>

that does everything `discover_with` does up to and including `parse_fn`, and returns every parsed function (it does not apply the `skip`, `unsafe`, `const` or `boring_body` filters; it records them). `discover_with` then becomes: scan, filter as before, expand replacements, sort, disambiguate. Behavior of `discover` must not change; `tests/engine.rs::discovery_finds_production_fns_and_skips_test_region` and `mutant_ids_stable_across_line_shift` guard that.

Add `pub fn scan_workspace(root: &Path, filter: &DiscoverFilter) -> Result<Vec<FnDecl>>` that opens Bifrost (`crate::bifrost::open_workspace`) and scans with no diff.

Create `src/sitemap.rs` (add `pub mod sitemap;` to `src/lib.rs`):

    #[derive(Serialize, Deserialize)]
    pub struct MapSite {
        pub site_id: u32, pub salt: u32,
        pub package: String, pub path: String, pub fqn: String,
        pub fn_start_byte: usize, pub body_start_byte: usize, pub body_end_byte: usize,
        pub start_line: usize, pub end_line: usize,
    }

    #[derive(Serialize, Deserialize)]
    pub struct SiteMap {
        pub version: u32,                        // 1
        pub repo_root: PathBuf,
        pub files: BTreeMap<String, String>,     // tracked file -> sha256, at map time
        pub sites: Vec<MapSite>,
        pub dropped: Vec<(String, String)>,      // (fqn, why) for functions without a site
        pub crate_roots: BTreeMap<String, String>, // package -> crate root source, relative
        pub tests: Vec<TestInfo>,                // durations under the schema binary
        pub hits: BTreeMap<String, Vec<u32>>,    // test id -> sites entered
        pub not_green: Vec<(String, String)>,
        pub worker: PathBuf,
        pub run_dir: PathBuf,
    }

with `save(&self, paths)` and `load(paths)` at `paths.index.join("sitemap.json")`, and

    pub fn sites_from_functions(fns: &[FnDecl]) -> (Vec<MapSite>, Vec<(String, String)>)

which keeps functions that are not `const_fn` and not `skip`, drops a function whose range lies inside another kept function's body range (nested), and assigns site ids with `schema::assign_unique_u32` over `schema::site_seed(package, path, fqn, body_start)` with `schema::default_site_hash`.

`pub fn tracked_file_hashes(root: &Path) -> Result<BTreeMap<String, String>>` runs `git -C root ls-files -z --cached --others --exclude-standard`, and hashes each listed file that exists with SHA-256 (`util::sha256_file`). Files under `target/` are skipped.

In `src/exec/schema.rs`, make these usable from the new executor: `pub(crate)` on `null_schema_gate`, `identity_gate`, `cheapest_tests`, `union_selected_tests`, `compiler_errors`, `tail_utf8`, `SCHEMA_SLOWDOWN`, and `NULL_GATE_TESTS`. Split `stage_binaries` into `stage_binaries` (unchanged signature) and `pub(crate) fn stage_tests(cfg, backend, target, run_dir, tests: &[&TestInfo], executables)` which does the staging for an explicit test list.

Create `src/exec/edited.rs` (add `pub mod edited;` to `src/exec/mod.rs`). Its first function is

    pub fn build_map(cfg: &Config, paths: &Paths, backend: Arc<dyn CloneBackend>) -> Result<SiteMap>

which does, in order:

1. `baseline::prepare_baseline(cfg, paths, false, false)`: copies the checkout to `baseline/`, builds it, runs the green suite once for durations. Then `index::list_tests_plain` for the catalog and `index::attach_timings` to give each catalogued test its duration (build a `CoverageIndex` with just `tests`, as `engine::prepare` does under sites selection).
2. `tracked_file_hashes(&cfg.repo_root)`.
3. `discover::scan_workspace(&paths.baseline, &engine::discover_filter(cfg))`, then `sites_from_functions`.
4. `scratch::ensure_worker(backend, paths, 0)`: a fresh clone of the baseline at `workers/w00`.
5. For each package with sites, find the crate root with `cargo_cmd::crate_root_source` on the baseline metadata and rebase it into the worker (see `rebase_to_worker` in `src/exec/schema.rs`; make it `pub(crate)`).
6. Build the identity-only `schema::Site` list (empty `variants`, `identity_variant: 0`), apply per crate with `schema::apply_schema_to_tree(worker, &sites_of_crate, &crate_root)`, and run `isolate_all` (below). Sites that rustc rejects move to `dropped` with the diagnostic.
7. `stage_tests` for the whole catalog into `paths.runs.join("map")`; `null_schema_gate` with the catalog; `identity_gate` on the sample configured by `cfg.identity_sample`.
8. `exec::test::record_site_hits` over the whole catalog with the recording policy from `record_sites` (floor 30 s, slowdown 3, no confirm) and `cfg.test_jobs` threads. Keep `sites_by_test`, `secs_by_test` (written back into `tests[].duration_secs`) and `not_green`.
9. Save the `SiteMap`. The worker keeps the identity-only schema source and its target directory.

`isolate_all(cfg, worker, target, originals, sites_by_crate_root, cargo_invocations) -> Result<IsolatedAll { sites, unviable: Vec<(String, String)>, executables }>` is `isolate` from `src/exec/schema.rs` generalised to several crates: restore originals, apply each crate's sites to its crate root, `cargo_cmd::build_tests_captured(worker, target, cfg, &[], false)` (workspace), attribute error spans with `schema::attribute_spans` over the union of applied sites, disable what is attributed with `schema::disable_attributed`, repeat up to `cfg.schema_max_rounds` rounds, and bisect over the whole site list when a round attributes nothing. `originals` come from the baseline copy, not from the worker: `read_originals(paths, &files) -> HashMap<String, Vec<u8>>` reads `baseline/<rel>` for every site file and every crate root. A site whose identity-only prologue fails is reported by site (fqn), a variant by mutant id.

CLI: add `Map(MapArgs)` to `cli::Command` with no arguments beyond the common ones, and `cmd_map` in `src/lib.rs`:

    let (cfg, _) = engine::config_from_cli(common, ConfigExtras { identity_sample: 8, schema_max_rounds: 5, ..Default::default() })?;
    let paths = cfg.paths(); paths.ensure()?;
    let (backend, probe) = scratch::probe_and_select(&cfg)?;
    let map = exec::edited::build_map(&cfg, &paths, backend)?;
    print: sites, dropped, tests catalogued, tests green, sites entered by at least one test, wall time.

Also print the three lines a model needs next: the map path, the worker path, and "edit function bodies in <repo root>, then run cargo quicksilver build".


### Milestone 3: capture and build (`src/capture.rs`, `src/exec/edited.rs`, `qs build`)


In `src/capture.rs` add

    #[derive(Serialize, Deserialize, Clone)]
    pub struct BodyEdit { pub site_id: u32, pub package: String, pub path: String, pub fqn: String, pub body: String }

    #[derive(Serialize, Deserialize, Clone)]
    pub struct EditedMutant { pub id: String, pub edits: Vec<BodyEdit> }

    pub enum Capture {
        Mutant(EditedMutant),
        Nothing,                       // no tracked file differs from the map
        Rejected(Vec<String>),         // one message per problem, file named
    }

    pub fn capture(map: &SiteMap, baseline: &Path, checkout: &Path) -> Result<Capture>

Steps: `tracked_file_hashes(checkout)`, compare with `map.files`. Added files, removed files and changed files that have no site are each a rejection message ("`tests/cli.rs` changed; it has no mutable functions (test code is never mutated)"; "`src/new.rs` was added; mutants may only change function bodies"). For each changed file with sites, read the baseline's copy and the checkout's copy and call `split_bodies` with that file's sites; a `Rejection` becomes a message with the file, the line and the function. A file that differs in hash but splits to no bodies (whitespace or comment edits inside bodies count as body edits, so this means it changed only outside bodies in a way the split accepted, which cannot happen; treat as nothing). The id is the first 16 hex digits of SHA-256 over the sorted `(site_id, body)` pairs, so the same edit captured twice has the same id.

The state file. In `src/exec/edited.rs`:

    #[derive(Serialize, Deserialize)]
    pub enum Status {
        Pending,
        Unviable { message: String },
        Built { generation: u32, activation: Vec<(u32, u32)> },
        Tested { verdict: Verdict },
    }
    #[derive(Serialize, Deserialize)]
    pub struct Verdict { pub kind: VerdictKind, pub test: Option<String>, pub message: Option<String>, pub tests_run: Vec<String>, pub secs: f64 }
    #[derive(Serialize, Deserialize)]
    pub struct Record { pub mutant: EditedMutant, pub status: Status, pub captured_at: String }
    #[derive(Serialize, Deserialize, Default)]
    pub struct State { pub generation: u32, pub records: Vec<Record> }

saved at `paths.state.join("edited.json")` with `util::write_atomic`.

    pub fn build(cfg: &Config, paths: &Paths, backend: Arc<dyn CloneBackend>, capture_from: Option<&Path>) -> Result<BuildReport>

1. Load the map (error: "no map; run cargo quicksilver map first") and the state.
2. If `capture_from` is given (the checkout), run `capture`. `Rejected` returns a `BuildReport` with the messages and exit code 2 without building. `Nothing` prints "no edits in <checkout>" and continues to build anything pending. A mutant whose id already exists in the state is reported as a duplicate and not added.
3. The build set is every record with status `Pending` or `Built` (untested mutants from earlier generations must be in the new binaries too, since the new build replaces the staged binaries). If the set is empty, print so and return.
4. Group edits by site: for each site touched, one `schema::Site` from the `MapSite` with one `SiteVariant` per mutant touching it (`return_stmt: format!("return {}", body)`, `mutant_id` the edited mutant's id, `index` in order), `identity_variant = variants.len()`. Untouched sites stay identity-only, so their ids and probes keep working.
5. Reuse the worker from the map (`map.worker`; error if it is gone: "worker missing; run map again"). `read_originals` for every site file and every crate root from the baseline; `isolate_all`. The unviable list maps mutant id to the rustc reason; records get `Unviable`. A bisection that disables a whole site makes every mutant touching it unviable with "site <fqn>: <why>".
6. `generation += 1`; `stage_tests` for the union of tests that entered any touched site (from `map.hits`) into `paths.runs.join("edited").join(format!("gen-{generation}"))`; `null_schema_gate` on those tests; remove the previous generation's run directory.
7. Records in the build set that survived get `Built { generation, activation }` where activation is the list of (site id, variant index) of that mutant.
8. Save the state and return the report: for each mutant in this call's set, its id, the fqns it edits, and `compiled` or the reason.

CLI: `Build(BuildArgs)` with `--no-capture` (build what is pending without reading the checkout) and `--restore` (after a successful capture, copy the baseline's version of every captured file back over the checkout so the next edit starts clean). `cmd_build` prints one line per mutant and exits 0 when everything in the set compiled, 1 when some mutant was unviable, 2 when the capture was rejected.


### Milestone 4: test (`src/exec/edited.rs`, `qs test`)


    pub fn test(cfg: &Config, paths: &Paths, only: &[String]) -> Result<Vec<(EditedMutant, Verdict)>>

1. Load map and state. The run set is every `Built` record whose generation is the current one (`only` narrows it by id prefix).
2. For each mutant: tests = union over its edits' sites of the tests whose `map.hits` contain that site, with `duration_secs` from the map, sorted cheapest first. Empty means `NotCovered` without running anything.
3. Run with `cfg.test_jobs` threads over mutants. Each mutant runs `exec::test::run_mutant_tests_observed(&planned, &run_dir, &map.worker, &policy, &[qs_mutants_env(&activation)], false)` where `planned.mutant` is a representative `types::Mutant` (id = mutant id, `fqn` = the edited fqns joined with `+`, `replacement` empty) and `policy` is the sites policy (`floor: cfg.timeout_floor, slowdown: 1.0, confirm: true`). The resulting `Outcome` becomes a `Verdict` with `tests_run` from the observations.
4. Records get `Tested { verdict }`; save.

CLI: `Test(TestArgs)` with `--mutant <id>` (repeatable). `cmd_test` prints one line per mutant: `CAUGHT <id> <fqns> by <test>`, `MISSED <id> <fqns> (<n> tests passed: <first few>)`, `NOT_COVERED <id> <fqns>`, `TIMEOUT`, `ERRORED`. With `--json <path>`, write the records of this run. Exit code 0.


### Milestone 5: end-to-end test and a real run


In `tests/edited.rs` (new), on the `tiny` fixture (`tests/fixtures/tiny`, copied by `support::setup_tiny` into a fresh git repository), using `support::scratch_cfg`:

1. `exec::edited::build_map`: assert `tiny_a::add` is a site, `add_works` entered it, `uncovered` is a site no test entered, the test-only helper `test_only_helper` is not a site.
2. Edit `crate_a/src/lib.rs` in the checkout: `add` body to `{ a - b }` and `greeting` body to `{ "bye".into() }`. `build` with capture: one mutant with two edits, compiled. Reset the file (write the fixture's original text back).
3. Edit `add` to `{ "x" }`: `build` reports unviable with a rustc type error. Reset.
4. Edit `uncovered` to `{ 8 }`: compiled. Reset. Edit `always_ok` to `{ false }`: compiled. Reset.
5. Edit the signature of `add` to `pub fn add(a: i64, b: i64) -> i64`: `Rejected` naming `add` and the line. Edit a test (`assert_eq!(add(2, 3), 6)`): `Rejected` naming the file and the line. Reset both.
6. `test`: the two-body mutant is Caught (by `add_works` or `greeting_works`), the `uncovered` mutant is NotCovered, the `always_ok` mutant is Missed with `calls_always_ok_without_asserting` among the tests run.

Then a real run on the local clap copy (`/mnt/optane/quicksilver/local/clap`, `--clone-backend copy --allow-copy`, inside the `systemd-run` scope from the memory notes): `map`, then one hand-written two-body mutant, `build`, `test`. Record the timings of each command in `Artifacts and Notes`.


## Concrete Steps


All commands run from the repository root `/home/jonathan/Projects/quicksilver/.mj/worktrees/0efccbcef11b71ab37df2f0e62e9cd9e` unless stated.

Unit tests for a module:

    cargo test --lib capture::

The fixture test:

    cargo test --test edited

Everything:

    cargo test

Checkpoint commits after each milestone, with multi-line messages explaining the why; push to `origin/master` is the owner's rule for this repository (`git push origin HEAD:master`).

A real run on clap (the scratch root and copy flags are from the memory notes for this host):

    cd /mnt/optane/quicksilver/local/clap
    QS=/home/jonathan/Projects/quicksilver/.mj/worktrees/0efccbcef11b71ab37df2f0e62e9cd9e/target/debug/cargo-quicksilver
    XDG_RUNTIME_DIR=/run/user/1000 systemd-run --user --scope -q -p MemoryMax=48G -p OOMPolicy=continue \
      $QS quicksilver map --scratch-root /mnt/optane/quicksilver/clap-edited --clone-backend copy --allow-copy
    # edit a body or two in clap_builder/src/...
    $QS quicksilver build --scratch-root /mnt/optane/quicksilver/clap-edited --clone-backend copy --allow-copy
    $QS quicksilver test  --scratch-root /mnt/optane/quicksilver/clap-edited --clone-backend copy --allow-copy


## Validation and Acceptance


After Milestone 1, `cargo test --lib capture::` passes the splitting tests listed above.

After Milestone 2, on the `tiny` fixture, `cargo quicksilver map --scratch-root <tmp> --clone-backend copy --allow-copy` prints a line like

    map: 15 sites (0 dropped), 12 tests (12 green), 9 sites entered by at least one test

and `<scratch>/<hash>/index/sitemap.json` exists with `hits["tiny_a::add_works"]` containing the site id of `tiny_a::add`.

After Milestone 3, editing `crate_a/src/lib.rs` so that `add` returns `a - b` and running `cargo quicksilver build` prints

    captured 1 mutant from <checkout>: 3f2a... edits tiny_a::add
    build: 1 mutant compiled, 0 unviable (generation 1)

and editing the signature instead prints a rejection naming `add` and the line, with exit code 2.

After Milestone 4, `cargo quicksilver test` prints

    CAUGHT 3f2a... tiny_a::add by add_works

After Milestone 5, `cargo test --test edited` passes, and the clap run completes with the timings recorded below.


## Idempotence and Recovery


`map` replaces the baseline, the worker and the map, and clears nothing else; the state file from earlier mutants is kept but its `Built` records refer to binaries that are gone, so `build` treats a state whose map changed as stale: the map stores a random `map_id`, the state stores the `map_id` it was built against, and a mismatch resets the state after printing how many records were discarded.

`build` can be re-run at any time. With no edits and nothing pending it does nothing. A capture that is rejected changes no state.

`test` can be re-run; already tested mutants are not run again unless named with `--mutant`.

If the worker is deleted, `build` fails with a message to run `map` again. If the checkout's `HEAD` moved since `map`, `build` will see many changed files and reject them, with the hint to run `map` again.


## Artifacts and Notes


To be filled: the fixture transcript and the clap timings.


## Interfaces and Dependencies


No new crates. `sha2`, `serde`, `serde_json`, `syn`, `anyhow` and `crossbeam-channel` are already dependencies.

In `src/capture.rs`:

    pub struct BodyRange { pub site_id: u32, pub fqn: String, pub start: usize, pub end: usize }
    pub enum Rejection { OutsideBody { line: usize, near: String }, Unbalanced { fqn: String } }
    pub fn split_bodies(original: &str, edited: &str, bodies: &[BodyRange]) -> Result<Vec<(u32, String)>, Rejection>;
    pub struct BodyEdit { pub site_id: u32, pub package: String, pub path: String, pub fqn: String, pub body: String }
    pub struct EditedMutant { pub id: String, pub edits: Vec<BodyEdit> }
    pub enum Capture { Mutant(EditedMutant), Nothing, Rejected(Vec<String>) }
    pub fn capture(map: &SiteMap, baseline: &Path, checkout: &Path) -> anyhow::Result<Capture>;

In `src/sitemap.rs`:

    pub struct MapSite { ... }  pub struct SiteMap { ... }
    impl SiteMap { pub fn save(&self, paths: &Paths) -> Result<()>; pub fn load(paths: &Paths) -> Result<Option<SiteMap>>; pub fn path(paths: &Paths) -> PathBuf; }
    pub fn sites_from_functions(fns: &[FnDecl]) -> (Vec<MapSite>, Vec<(String, String)>);
    pub fn tracked_file_hashes(root: &Path) -> Result<BTreeMap<String, String>>;

In `src/discover.rs`:

    pub struct FnDecl { ... }
    pub fn scan_workspace(root: &Path, filter: &DiscoverFilter) -> Result<Vec<FnDecl>>;

In `src/exec/edited.rs`:

    pub fn build_map(cfg: &Config, paths: &Paths, backend: Arc<dyn CloneBackend>) -> Result<SiteMap>;
    pub fn build(cfg: &Config, paths: &Paths, capture_from: Option<&Path>, restore: bool) -> Result<BuildReport>;
    pub fn test(cfg: &Config, paths: &Paths, only: &[String]) -> Result<Vec<(EditedMutant, Verdict)>>;
    pub struct State { ... }  impl State { pub fn load(paths) ; pub fn save(&self, paths) }

In `src/cli.rs`: `Command::Map(MapArgs)`, `Command::Build(BuildArgs { no_capture: bool, restore: bool })`, `Command::Test(TestArgs { mutant: Vec<String> })`.
