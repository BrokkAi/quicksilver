use crate::config::CloneBackendKind;
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
    /// Scratch filesystem that supports CoW clones.
    #[arg(long, global = true)]
    pub scratch_root: Option<PathBuf>,

    /// Clone mechanism. `auto` probes and refuses to silently copy.
    #[arg(long, global = true, value_enum)]
    pub clone_backend: Option<CloneBackendKind>,

    #[arg(long, global = true)]
    pub build_jobs: Option<usize>,

    #[arg(long, global = true)]
    pub test_jobs: Option<usize>,

    #[arg(long, global = true)]
    pub index_jobs: Option<usize>,

    /// Parallelism assumed for the cargo-mutants comparison baseline (J_cm).
    /// Defaults to the same RAM/CPU formula as --build-jobs: cargo-mutants
    /// workers each run a full rustc and will not actually use all cores.
    #[arg(long, global = true)]
    pub cm_jobs: Option<usize>,

    /// Restrict mutants to these packages.
    #[arg(long = "package", short = 'p', global = true)]
    pub packages: Vec<String>,

    /// Restrict mutants to matching files (globs).
    #[arg(long = "file", global = true)]
    pub files: Vec<String>,

    /// Mutate only functions a `git diff` touches, e.g. `HEAD^...HEAD`.
    /// The diff's target must be what is checked out.
    #[arg(long, global = true)]
    pub in_diff: Option<String>,

    /// How each mutant's tests are chosen: `coverage` (LLVM Stage A and B)
    /// or `sites` (one schema-binary run that records mutation-site hits).
    #[arg(long, global = true, value_enum, default_value_t = crate::config::TestSelection::Coverage)]
    pub test_selection: crate::config::TestSelection,

    /// Limit union coverage reporting to this crate's sources (sound).
    /// Tests from reverse-dependent crates are still run.
    #[arg(long, global = true)]
    pub index_scope: Option<String>,

    /// Force coverage index rebuild.
    #[arg(long, global = true)]
    pub reindex: bool,

    /// Machine-readable outcomes / estimate.
    #[arg(long, global = true)]
    pub json: Option<PathBuf>,

    /// Skip mutants already decided in a prior run.
    #[arg(long, global = true)]
    pub resume: bool,

    /// Permit the non-CoW fallback (loudly).
    #[arg(long, global = true)]
    pub allow_copy: bool,

    /// Cross-machine sharding, locality-preserving slices (`K/N`).
    #[arg(long, global = true)]
    pub shard: Option<String>,

    /// Workspace root (directory containing Cargo.toml).
    #[arg(long, global = true)]
    pub manifest_path: Option<PathBuf>,

    /// RAM budget for the build pool, e.g. `70%`.
    #[arg(long, global = true)]
    pub max_build_memory: Option<String>,

    /// Abort if free space on the scratch filesystem falls below this many bytes.
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

    #[arg(long, global = true)]
    pub zfs_remote: Option<String>,

    #[arg(long, global = true)]
    pub zfs_dataset: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Sample mutation outcomes and extrapolate exhaustive-run wall clock.
    Estimate(EstimateArgs),
    /// Execute mutants with the schema engine.
    Run(RunArgs),
    /// Print the mutant catalog and exit.
    List(ListArgs),
    /// Build or refresh the coverage index.
    Index(IndexArgs),
    /// Clone-backend and build-environment diagnostics.
    Probe(ProbeArgs),
    /// Compare impact-selected verdicts to full-suite verdicts.
    AuditSelection(AuditSelectionArgs),
    /// Build once with a switch at every function and record which
    /// functions each test enters. `build` and `test` use the result.
    Map(MapArgs),
    /// Capture the edited function bodies in the checkout as one mutant and
    /// compile every captured mutant into the schema binary.
    Build(BuildArgs),
    /// Run each compiled mutant against the tests that enter its functions.
    Test(TestArgs),
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
    /// After capturing, reset every captured file to the baseline's version
    /// so the next edit starts clean.
    #[arg(long)]
    pub restore: bool,
    /// Isolation rebuilds that map rustc diagnostics back to variants.
    #[arg(long, default_value_t = 5)]
    pub schema_max_rounds: u32,
}

#[derive(Args, Debug, Default)]
pub struct TestArgs {
    /// Only mutants whose id starts with this (repeatable).
    #[arg(long = "mutant")]
    pub mutants: Vec<String>,
}

#[derive(Args, Debug, Default)]
pub struct ListArgs {
    /// Include mutants that would be skipped (unsafe/const/empty).
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug, Default)]
pub struct IndexArgs {}

#[derive(Args, Debug, Default)]
pub struct ProbeArgs {
    /// Also materialize a tiny clone of the current repo and time an incremental rebuild.
    #[arg(long)]
    pub with_rebuild: bool,
}

pub fn parse_cli() -> Cli {
    let mut args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("quicksilver") {
        args.remove(1);
    }
    Cli::parse_from(args)
}
