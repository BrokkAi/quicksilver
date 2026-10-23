use crate::types::Paths;
use crate::util::{default_build_jobs, nproc, parse_memory_spec};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_SCRATCH_ROOT: &str = "/nfs/morannon/nvme/quicksilver";

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CloneBackendKind {
    Auto,
    Reflink,
    ZfsClone,
    Copy,
}

impl CloneBackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Reflink => "reflink",
            Self::ZfsClone => "zfs-clone",
            Self::Copy => "copy",
        }
    }
}

/// Where each mutant's tests come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum TestSelection {
    /// LLVM coverage: Stage A (union) and Stage B (per test).
    #[default]
    Coverage,
    /// One run of every candidate test under the schema binary, recording
    /// which mutation sites it enters. No instrumented build, no LLVM.
    Sites,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    #[serde(rename = "scratch-root")]
    pub scratch_root: Option<PathBuf>,
    #[serde(rename = "clone-backend")]
    pub clone_backend: Option<String>,
    #[serde(rename = "build-jobs")]
    pub build_jobs: Option<usize>,
    #[serde(rename = "test-jobs")]
    pub test_jobs: Option<usize>,
    #[serde(rename = "index-jobs")]
    pub index_jobs: Option<usize>,
    #[serde(rename = "cm-jobs")]
    pub cm_jobs: Option<usize>,
    #[serde(rename = "allow-copy")]
    pub allow_copy: Option<bool>,
    #[serde(rename = "zfs-remote")]
    pub zfs_remote: Option<String>,
    #[serde(rename = "zfs-dataset")]
    pub zfs_dataset: Option<String>,
    #[serde(rename = "build-env", default)]
    pub build_env: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub repo_root: PathBuf,
    pub test_selection: TestSelection,
    pub scratch_root: PathBuf,
    pub clone_backend: CloneBackendKind,
    pub build_jobs: usize,
    pub test_jobs: usize,
    pub index_jobs: usize,
    /// Comparison baseline width for cargo-mutants (J_cm). Not the QS build pool.
    pub cm_jobs: usize,
    pub allow_copy: bool,
    pub max_build_memory_frac: f64,
    pub max_scratch: Option<u64>,
    pub packages: Vec<String>,
    pub files: Vec<String>,
    pub in_diff: Option<String>,
    pub index_scope: Option<String>,
    pub reindex: bool,
    pub json: Option<PathBuf>,
    pub resume: bool,
    pub shard: Option<(u32, u32)>,
    pub sample: usize,
    pub sample_seed: u64,
    pub baseline_runs: u32,
    pub timeout_floor: Duration,
    pub strict_source_isolation: bool,
    pub build_env: BTreeMap<String, String>,
    pub zfs_remote: Option<String>,
    pub zfs_dataset: Option<String>,
    pub quiet: bool,
    pub schema_max_rounds: u32,
    /// 0 means one batch per crate (no split).
    pub schema_batch_size: usize,
    pub diff_against: Option<PathBuf>,
    /// Identity-mutant gate sample per schema batch. 0 means every site.
    pub identity_sample: usize,
    pub triage: bool,
    pub triage_neighbors: usize,
    pub second_order_pairs: usize,
}

impl Config {
    pub fn paths(&self) -> Paths {
        Paths::for_session(&self.scratch_root, &self.repo_root)
    }

    pub fn load_file(repo_root: &Path) -> Result<FileConfig> {
        for name in ["quicksilver.toml", ".quicksilver.toml"] {
            let p = repo_root.join(name);
            if p.is_file() {
                let text =
                    std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
                return toml::from_str(&text).with_context(|| format!("parse {}", p.display()));
            }
        }
        Ok(FileConfig::default())
    }
}

pub fn parse_shard(s: &str) -> Result<(u32, u32)> {
    let (k, n) = s.split_once('/').context("--shard expects K/N")?;
    let k: u32 = k.parse().context("shard K")?;
    let n: u32 = n.parse().context("shard N")?;
    anyhow::ensure!(n > 0 && k >= 1 && k <= n, "--shard K/N needs 1 <= K <= N");
    Ok((k, n))
}

pub fn merge_config(
    repo_root: PathBuf,
    file: FileConfig,
    cli: &crate::cli::CommonOpts,
    extras: ConfigExtras,
) -> Result<Config> {
    let scratch_root = cli
        .scratch_root
        .clone()
        .or(file.scratch_root)
        .or_else(|| std::env::var_os("QS_SCRATCH_ROOT").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SCRATCH_ROOT));

    let clone_backend = if let Some(b) = &cli.clone_backend {
        *b
    } else if let Some(s) = &file.clone_backend {
        match s.as_str() {
            "auto" => CloneBackendKind::Auto,
            "reflink" => CloneBackendKind::Reflink,
            "zfs-clone" => CloneBackendKind::ZfsClone,
            "copy" => CloneBackendKind::Copy,
            other => anyhow::bail!("unknown clone-backend {other}"),
        }
    } else {
        CloneBackendKind::Auto
    };

    let max_build_memory_frac = cli
        .max_build_memory
        .as_deref()
        .map(parse_memory_spec)
        .transpose()?
        .unwrap_or(0.70);

    let build_jobs = cli
        .build_jobs
        .or(file.build_jobs)
        .unwrap_or_else(|| default_build_jobs(max_build_memory_frac));
    let test_jobs = cli.test_jobs.or(file.test_jobs).unwrap_or_else(nproc);
    let index_jobs = cli.index_jobs.or(file.index_jobs).unwrap_or_else(nproc);
    // cargo-mutants workers are RAM-bound the same way the QS build pool is.
    // Defaulting to nproc (120 here) would overstate the competitor.
    let cm_jobs = cli
        .cm_jobs
        .or(file.cm_jobs)
        .unwrap_or_else(|| default_build_jobs(max_build_memory_frac));

    let mut build_env = file.build_env;
    if let Ok(v) = std::env::var("BIFROST_BUILD_IDENTITY_OVERRIDE") {
        build_env
            .entry("BIFROST_BUILD_IDENTITY_OVERRIDE".into())
            .or_insert(v);
    }

    Ok(Config {
        repo_root,
        test_selection: cli.test_selection,
        scratch_root,
        clone_backend,
        build_jobs: build_jobs.max(1),
        test_jobs: test_jobs.max(1),
        index_jobs: index_jobs.max(1),
        cm_jobs: cm_jobs.max(1),
        allow_copy: cli.allow_copy || file.allow_copy.unwrap_or(false),
        max_build_memory_frac,
        max_scratch: cli.max_scratch,
        packages: extras.packages,
        files: extras.files,
        in_diff: extras.in_diff,
        index_scope: extras.index_scope,
        reindex: extras.reindex,
        json: extras.json,
        resume: extras.resume,
        shard: extras.shard,
        sample: extras.sample,
        sample_seed: extras.sample_seed,
        baseline_runs: extras.baseline_runs,
        timeout_floor: Duration::from_millis(extras.timeout_floor_ms),
        strict_source_isolation: extras.strict_source_isolation,
        build_env,
        zfs_remote: cli
            .zfs_remote
            .clone()
            .or(file.zfs_remote)
            .or_else(|| std::env::var("QS_ZFS_REMOTE").ok()),
        zfs_dataset: cli
            .zfs_dataset
            .clone()
            .or(file.zfs_dataset)
            .or_else(|| std::env::var("QS_ZFS_DATASET").ok()),
        quiet: extras.quiet,
        schema_max_rounds: if extras.schema_max_rounds == 0 {
            5
        } else {
            extras.schema_max_rounds
        },
        schema_batch_size: extras.schema_batch_size,
        diff_against: extras.diff_against,
        identity_sample: extras.identity_sample,
        triage: extras.triage,
        triage_neighbors: extras.triage_neighbors.max(1),
        second_order_pairs: extras.second_order_pairs,
    })
}

#[derive(Debug, Clone, Default)]
pub struct ConfigExtras {
    pub packages: Vec<String>,
    pub files: Vec<String>,
    pub in_diff: Option<String>,
    pub index_scope: Option<String>,
    pub reindex: bool,
    pub json: Option<PathBuf>,
    pub resume: bool,
    pub shard: Option<(u32, u32)>,
    pub sample: usize,
    pub sample_seed: u64,
    pub baseline_runs: u32,
    pub timeout_floor_ms: u64,
    pub strict_source_isolation: bool,
    pub quiet: bool,
    pub schema_max_rounds: u32,
    pub schema_batch_size: usize,
    pub diff_against: Option<PathBuf>,
    pub identity_sample: usize,
    pub triage: bool,
    pub triage_neighbors: usize,
    pub second_order_pairs: usize,
}
