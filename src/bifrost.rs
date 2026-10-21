//! Opening Bifrost's persisted analyzer for a workspace.
//!
//! The store lives at `<primary repo root>/.bifrost/cache/`. For a QS session
//! that is the baseline copy, which carries its own `.git`, so the cache is
//! private to the session and survives between runs as long as the baseline
//! is synced in place rather than replaced.

use anyhow::{Context, Result};
use brokk_bifrost_analysis::{
    AnalyzerConfig, DispatchHierarchyExpansion, FilesystemProject, Language, WorkspaceAnalyzer,
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

/// Rust-only persisted analyzer. Test selection wants every implementation a
/// call could dispatch to, so concrete overrides are expanded: an extra
/// candidate costs test time, a missing one can turn Caught into Missed.
pub fn open_workspace(root: &Path) -> Result<OpenedWorkspace> {
    let start = Instant::now();
    let project = Arc::new(
        FilesystemProject::new(root)
            .with_context(|| format!("FilesystemProject {}", root.display()))?,
    );
    let config = AnalyzerConfig {
        dispatch_hierarchy_expansion: DispatchHierarchyExpansion::CONCRETE_OVERRIDES,
        ..AnalyzerConfig::default()
    };
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
