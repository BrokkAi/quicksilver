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
    /// Independent of the quicksilver build pool; cargo-mutants runs whole
    /// scenarios and typically uses all cores.
    #[arg(long, global = true)]
    pub cm_jobs: Option<usize>,

    /// Restrict mutants to these packages.
    #[arg(long = "package", short = 'p', global = true)]
    pub packages: Vec<String>,

    /// Restrict mutants to matching files (globs).
    #[arg(long = "file", global = true)]
    pub files: Vec<String>,

    /// Mutate only symbols `analyze_diff` reports as edited or introduced.
    #[arg(long, global = true)]
    pub in_diff: Option<String>,

    /// Build the coverage index for one crate at a time.
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
    /// Sample plus extrapolate exhaustive-run wall clock (the v1 deliverable).
    Estimate(EstimateArgs),
    /// Execute mutants (scoped; exhaustive runs are v2).
    Run(RunArgs),
    /// Print the mutant catalog and exit.
    List(ListArgs),
    /// Build or refresh the coverage index.
    Index(IndexArgs),
    /// Clone-backend and build-environment diagnostics.
    Probe(ProbeArgs),
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
}

#[derive(Args, Debug, Default)]
pub struct RunArgs {
    /// Maximum mutants to execute (after NotCovered filtering). 0 means no cap.
    #[arg(long, default_value_t = 0)]
    pub limit: usize,
    #[arg(long, hide = true)]
    pub skip_baseline: bool,
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
