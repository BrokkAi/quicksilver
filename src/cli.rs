use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "cargo-quicksilver",
    version,
    about = "Impact-selected mutation testing for Cargo workspaces"
)]
pub struct Cli {
    #[command(flatten)]
    pub common: CommonOpts,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args, Debug, Clone)]
pub struct CommonOpts {
    #[arg(long, global = true)]
    pub build_jobs: Option<usize>,

    #[arg(long, global = true)]
    pub test_jobs: Option<usize>,

    /// Parallelism assumed for the cargo-mutants comparison baseline (J_cm).
    /// Defaults to the same RAM/CPU formula as --build-jobs: cargo-mutants
    /// workers each run a full rustc and will not actually use all cores.
    #[arg(long, global = true)]
    pub cm_jobs: Option<usize>,

    /// Restrict mutants to these packages.
    #[arg(long = "package", short = 'p', global = true)]
    pub packages: Vec<String>,

    /// With `--package`, also build and run the tests of every workspace
    /// package that depends on one. By default only the named packages' own
    /// tests count as covering them.
    #[arg(long, global = true)]
    pub reverse_dependents: bool,

    /// With `--package`, also build and run this test binary of another
    /// package, named as nextest names it: `pkg` for its unit tests,
    /// `pkg::target` for an integration test. Repeatable; `extra-tests` in
    /// quicksilver.toml adds more.
    #[arg(long = "extra-tests", value_name = "BINARY", global = true)]
    pub extra_tests: Vec<String>,

    /// Run tests in your own environment. By default each test process gets
    /// a new HOME, XDG directories and TMPDIR, deleted when it exits.
    #[arg(long, global = true)]
    pub no_sandbox: bool,

    /// Restrict mutants to matching files (globs).
    #[arg(long = "file", global = true)]
    pub files: Vec<String>,

    /// Mutate only functions a `git diff` touches, e.g. `HEAD^...HEAD`.
    /// The diff's target must be what is checked out.
    #[arg(long, global = true)]
    pub in_diff: Option<String>,

    /// Machine-readable outcomes / estimate.
    #[arg(long, global = true)]
    pub json: Option<PathBuf>,

    /// Skip mutants already decided in a prior run.
    #[arg(long, global = true)]
    pub resume: bool,

    /// Cross-machine sharding, locality-preserving slices (`K/N`).
    #[arg(long, global = true)]
    pub shard: Option<String>,

    /// Workspace root (directory containing Cargo.toml).
    #[arg(long, global = true)]
    pub manifest_path: Option<PathBuf>,

    /// RAM budget for the build pool, e.g. `70%`.
    #[arg(long, global = true)]
    pub max_build_memory: Option<String>,

    /// Abort if free space where QS keeps its state falls below this many bytes.
    #[arg(long, global = true)]
    pub max_scratch: Option<u64>,

    /// How many times to run the baseline suite. >1 catches flakes.
    #[arg(long, global = true, default_value_t = 1)]
    pub baseline_runs: u32,

    /// Floor for per-mutant test timeouts, in milliseconds.
    #[arg(long, global = true, default_value_t = 5000, hide = true)]
    pub timeout_floor_ms: u64,

    /// Hold the build slot until selected tests finish, so tests never see a later mutant's source.
    #[arg(long, global = true)]
    pub strict_source_isolation: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Sample mutation outcomes and extrapolate exhaustive-run wall clock.
    /// Needs a map of HEAD from `cargo quicksilver map`.
    Estimate(EstimateArgs),
    /// Execute mutants with the schema engine.
    Run(RunArgs),
    /// Print the mutant catalog and exit.
    List(ListArgs),
    /// Check QS on this repository: run a sample of mutants with the tests
    /// QS selects and with every candidate test, and report any kill that
    /// selection lost.
    #[command(hide = true)]
    AuditSelection(AuditSelectionArgs),
    /// Build once with a switch at every function and record which
    /// functions each test enters. `build` and `test` use the result.
    Map(MapArgs),
    /// Capture the edited function bodies in the checkout, one mutant per
    /// edited function, and compile every captured mutant into the schema
    /// binary in one build.
    Build(BuildArgs),
    /// Run each compiled mutant against the tests that enter its functions.
    Test(TestArgs),
    /// List the functions no test enters, from the map of HEAD. `--package`
    /// and `--file` narrow the list.
    Uncovered(UncoveredArgs),
    /// List entered functions that still need a captured mutant.
    Pending(PendingArgs),
    /// Review mutants that no selected test caught.
    Survivors(SurvivorsArgs),
    /// Apply a captured mutant to this checkout, or undo it with --revert.
    Apply(ApplyArgs),
    /// Check that a command fails with a mutant and passes after reverting it.
    Prove(ProveArgs),
    /// Remove QS's worktrees of this repository and its state, and tell mbx
    /// their build output is no longer needed.
    Clean,
}

#[derive(Args, Debug)]
pub struct EstimateArgs {
    /// Mutants to run end to end.
    #[arg(long, default_value_t = 150)]
    pub sample: usize,
    /// Reproducible sampling.
    #[arg(long, default_value_t = 1)]
    pub sample_seed: u64,
    /// Skip the green-suite / rebuild-cost measurements (tests only).
    #[arg(long, hide = true)]
    pub skip_baseline: bool,
    /// Deprecated: schemata is the only engine; this flag is accepted and ignored.
    #[arg(long, global = false, hide = true)]
    pub schema: bool,
    /// Isolation rebuilds that map rustc diagnostics back to sites.
    #[arg(long, default_value_t = 5)]
    pub schema_max_rounds: u32,
    /// Split a crate's sites when this many are exceeded. 0 = one batch per crate.
    #[arg(long, default_value_t = 0)]
    pub schema_batch_size: usize,
    /// Prior run's `--json` report to compare verdicts against (differential gate).
    #[arg(long)]
    pub diff_against: Option<PathBuf>,
    /// Identity-mutant gate: sites to activate with the original-body variant.
    /// 0 means every site in the batch.
    #[arg(long, default_value_t = 8)]
    pub identity_sample: usize,
    /// Execute this many disjoint second-order mutant pairs after the sample.
    #[arg(long, default_value_t = 0)]
    pub second_order_pairs: usize,
}

#[derive(Args, Debug, Default)]
pub struct RunArgs {
    /// Maximum mutants to execute (after NotCovered filtering). 0 means no cap.
    #[arg(long, default_value_t = 0)]
    pub limit: usize,
    #[arg(long, hide = true)]
    pub skip_baseline: bool,
    /// Deprecated: schemata is the only engine; this flag is accepted and ignored.
    #[arg(long, global = false, hide = true)]
    pub schema: bool,
    /// Isolation rebuilds that map rustc diagnostics back to sites.
    #[arg(long, default_value_t = 5)]
    pub schema_max_rounds: u32,
    /// Split a crate's sites when this many are exceeded. 0 = one batch per crate.
    #[arg(long, default_value_t = 0)]
    pub schema_batch_size: usize,
    /// Prior run's `--json` report to compare verdicts against (differential gate).
    #[arg(long)]
    pub diff_against: Option<PathBuf>,
    /// Identity-mutant gate: sites to activate with the original-body variant.
    /// 0 means every site in the batch.
    #[arg(long, default_value_t = 8)]
    pub identity_sample: usize,
    /// Run higher-order survivor triage after first-order verdicts.
    #[arg(long)]
    pub triage: bool,
    /// Nearby caught controls to combine with each survivor.
    #[arg(long, default_value_t = 1)]
    pub triage_neighbors: usize,
}

#[derive(Args, Debug)]
pub struct AuditSelectionArgs {
    /// Covered mutants to run twice (selected tests vs the whole suite).
    #[arg(long, default_value_t = 20)]
    pub sample: usize,
    #[arg(long, default_value_t = 1)]
    pub sample_seed: u64,
    #[arg(long, hide = true)]
    pub skip_baseline: bool,
}

#[derive(Args, Debug, Default)]
pub struct MapArgs {
    /// Isolation rebuilds that map rustc diagnostics back to sites.
    #[arg(long, default_value_t = 5)]
    pub schema_max_rounds: u32,
    /// Sites to check with the identity variant active after recording.
    /// 0 means every entered site.
    #[arg(long, default_value_t = 8)]
    pub identity_sample: usize,
}

#[derive(Args, Debug, Default)]
pub struct BuildArgs {
    /// Compile what is pending without reading the checkout.
    #[arg(long)]
    pub no_capture: bool,
    /// Capture the edits of this patch (against the map's commit, paths as
    /// `git diff` writes them) instead of the checkout. Repeatable; each
    /// patch is captured on its own. Use PATCH=INTENTS to attach notes only
    /// to that patch (split at the last equals sign).
    #[arg(long = "patch", value_name = "PATCH[=INTENTS]")]
    pub patches: Vec<crate::exec::edited::PatchSource>,
    /// Drop edits outside function bodies with a warning instead of
    /// rejecting the whole capture.
    #[arg(long)]
    pub skip_outside_bodies: bool,
    /// JSON object mapping a function's full name (as QS prints it) or
    /// `path:line` to what the mutant models, such as "off-by-one at the
    /// upper bound". Applied to the mutants this build captures; shown with
    /// survivors and kept in `test --json`.
    #[arg(long, value_name = "FILE")]
    pub intents: Option<std::path::PathBuf>,
    /// Isolation rebuilds that map rustc diagnostics back to variants.
    #[arg(long, default_value_t = 5)]
    pub schema_max_rounds: u32,
}

#[derive(Args, Debug, Default)]
pub struct UncoveredArgs {
    /// Also list test scaffolding: functions in test-helper modules, named
    /// `*_for_test`, or under a `cfg` that mentions `test`.
    #[arg(long)]
    pub scaffolding: bool,
}

#[derive(Args, Debug)]
pub struct PendingArgs {
    /// Minimum function length in source lines, including the first and last.
    #[arg(long, default_value_t = 1)]
    pub min_lines: usize,
    /// Include functions that already have mutants, with their mutant counts.
    #[arg(long)]
    pub all: bool,
    /// Write this many lists balanced by source lines.
    #[arg(long, requires = "out", value_parser = parse_positive_split)]
    pub split: Option<usize>,
    /// Directory for pending-1.txt through pending-N.txt.
    #[arg(long, requires = "split", value_name = "DIR")]
    pub out: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ApplyArgs {
    /// Mutant id or unique id prefix from the shared captured state.
    pub id: String,
    /// Restore the captured original body instead of applying the mutant.
    #[arg(long)]
    pub revert: bool,
}

#[derive(Args, Debug)]
pub struct ProveArgs {
    /// Mutant id or unique id prefix from the shared captured state.
    pub id: String,
    /// Command and arguments, after --, run in the caller's checkout.
    #[arg(last = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

#[derive(Args, Debug)]
pub struct SurvivorsArgs {
    /// Compare survivors against this Git revision (default: HEAD).
    #[arg(long, value_name = "REV")]
    pub at: Option<String>,
    /// Write this many lists, keeping each source file together.
    #[arg(long, requires = "out", value_parser = parse_positive_split)]
    pub split: Option<usize>,
    /// Directory for survivors-1.txt through survivors-N.txt.
    #[arg(long, requires = "split", value_name = "DIR")]
    pub out: Option<PathBuf>,
}

fn parse_positive_split(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| "--split must be a positive integer".into())
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgainKind {
    Timeouts,
    Errored,
}

#[derive(Args, Debug, Default)]
pub struct TestArgs {
    /// Repeat timeout or errored verdicts (repeatable); combines with --mutant.
    #[arg(long, value_enum)]
    pub again: Vec<AgainKind>,
    /// Only mutants whose id starts with this (repeatable).
    #[arg(long = "mutant")]
    pub mutants: Vec<String>,
    /// Run a survivor that only proptest properties judged once more with
    /// `PROPTEST_CASES` set to this, before calling it missed (0: never).
    #[arg(long, default_value_t = 0)]
    pub proptest_cases: u32,
}

#[derive(Args, Debug, Default)]
pub struct ListArgs {
    /// Include mutants that would be skipped (unsafe/const/empty).
    #[arg(long)]
    pub all: bool,
}

pub fn parse_cli() -> Cli {
    let mut args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("quicksilver") {
        args.remove(1);
    }
    Cli::parse_from(args)
}
