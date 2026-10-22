//! Opening Bifrost's persisted analyzer for a workspace.
//!
//! The store lives at `<primary repo root>/.bifrost/cache/`. For a QS session
//! that is the baseline copy, which carries its own `.git`, so the cache is
//! private to the session and survives between runs as long as the baseline
//! is synced in place rather than replaced.

use anyhow::{Context, Result};
use brokk_bifrost_analysis::{
    AnalyzerConfig, DispatchHierarchyExpansion, FileSetProject, FilesystemProject, Language,
    WorkspaceAnalyzer,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

pub struct OpenedWorkspace {
    pub workspace: WorkspaceAnalyzer,
    pub open_secs: f64,
    pub store_path: Option<PathBuf>,
}

/// Analyzer over only `rel_paths` under `root`, sharing the persisted store's
/// per-file facts. Bifrost's whole-workspace open reconciles every crate an
/// edit touches, which on a one-crate repository recomputes name resolution
/// for the whole crate; a scoped build analyzes the listed files and nothing
/// else. `--in-diff` discovery needs only the changed files.
pub fn open_files(root: &Path, rel_paths: &[PathBuf]) -> Result<OpenedWorkspace> {
    let start = Instant::now();
    let project = Arc::new(FileSetProject::new(root, rel_paths.iter().cloned()));
    let workspace = WorkspaceAnalyzer::build_scoped_persisted(project, analyzer_config())
        .map_err(|e| anyhow::anyhow!("WorkspaceAnalyzer::build_scoped_persisted: {e}"))?;
    let open_secs = start.elapsed().as_secs_f64();
    let store_path = workspace.persisted_store_path();
    tracing::info!(
        "bifrost analyzer for {} files ready in {open_secs:.2}s",
        rel_paths.len()
    );
    Ok(OpenedWorkspace {
        workspace,
        open_secs,
        store_path,
    })
}

fn analyzer_config() -> AnalyzerConfig {
    AnalyzerConfig {
        dispatch_hierarchy_expansion: DispatchHierarchyExpansion::CONCRETE_OVERRIDES,
        ..AnalyzerConfig::default()
    }
}

/// Rust-only persisted analyzer. Test selection wants every implementation a
/// call could dispatch to, so concrete overrides are expanded: an extra
/// candidate costs test time, a missing one can turn Caught into Missed.
pub fn open_workspace(root: &Path) -> Result<OpenedWorkspace> {
    let start = Instant::now();
    let project = Arc::new(
        FilesystemProject::new(root)
            .with_context(|| format!("FilesystemProject {}", root.display()))?,
    );
    let config = analyzer_config();
    let languages = BTreeSet::from([Language::Rust]);
    let workspace = WorkspaceAnalyzer::build_persisted_for_languages(project, config, &languages)
        .map_err(|e| anyhow::anyhow!("WorkspaceAnalyzer::build_persisted: {e}"))?;
    let open_secs = start.elapsed().as_secs_f64();
    let store_path = workspace.persisted_store_path();
    tracing::info!(
        "bifrost analyzer ready in {open_secs:.2}s (store {})",
        store_path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "none".into())
    );
    Ok(OpenedWorkspace {
        workspace,
        open_secs,
        store_path,
    })
}
