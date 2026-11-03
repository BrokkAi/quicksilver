//! Which production crate root a mutation pass instruments. Test selection is
//! still package-wide, so integration drivers remain available.
use anyhow::{bail, ensure, Context, Result};
use cargo_metadata::{Metadata, Target};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Selection {
    pub lib: bool,
    pub bin: Option<String>,
}

impl Selection {
    pub fn is_default(&self) -> bool {
        !self.lib && self.bin.is_none()
    }
    pub fn validate(&self, packages: &[String]) -> Result<()> {
        ensure!(
            !(self.lib && self.bin.is_some()),
            "select only one production target: --lib or --bin NAME"
        );
        ensure!(
            self.bin.as_ref().is_none_or(|s| !s.is_empty()),
            "--bin needs a nonempty target name"
        );
        ensure!(
            self.is_default() || packages.len() == 1,
            "explicit production target selection requires exactly one --package"
        );
        Ok(())
    }
    pub fn require_match(&self, requested: &Self) -> Result<()> {
        ensure!(self == requested,
            "production target differs from the saved map: map={}, requested={}; use the map's --lib/--bin selection or run `cargo quicksilver map` again",
            serde_json::to_string(self)?, serde_json::to_string(requested)?);
        Ok(())
    }
    pub fn target<'a>(&self, meta: &'a Metadata, package: &str) -> Result<Option<&'a Target>> {
        let pkg = meta
            .workspace_packages()
            .into_iter()
            .find(|p| p.name.as_str() == package)
            .with_context(|| format!("no workspace package {package}"))?;
        let library = |t: &&Target| {
            t.kind.iter().any(|k| {
                matches!(
                    k.to_string().as_str(),
                    "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"
                )
            })
        };
        let binary = |t: &&Target| t.kind.iter().any(|k| k.to_string() == "bin");
        let found = if self.lib {
            pkg.targets.iter().find(library)
        } else if let Some(name) = &self.bin {
            pkg.targets.iter().filter(binary).find(|t| t.name == *name)
        } else {
            pkg.targets
                .iter()
                .find(library)
                .or_else(|| pkg.targets.iter().find(binary))
        };
        if found.is_none() && !self.is_default() {
            bail!(
                "package {package} has no selected production target {}; available: {}",
                self.bin
                    .as_ref()
                    .map(|b| format!("bin {b}"))
                    .unwrap_or_else(|| "lib".into()),
                pkg.targets
                    .iter()
                    .filter(|t| library(t) || binary(t))
                    .map(|t| format!("{} ({})", t.name, if binary(&t) { "bin" } else { "lib" }))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        Ok(found)
    }
    pub fn root(&self, meta: &Metadata, package: &str) -> Result<Option<PathBuf>> {
        Ok(self
            .target(meta, package)?
            .map(|t| t.src_path.clone().into_std_path_buf()))
    }
    pub fn fqn_root(&self, package: &str, target: &Target) -> String {
        if self.is_default() {
            target.name.replace('-', "_")
        } else {
            format!(
                "{package}::{}::{}",
                if self.bin.is_some() { "bin" } else { "lib" },
                target.name
            )
        }
    }
}

/// Conservatively include workspace dependencies of every built test package.
/// Metadata without dependency resolution still describes dependency names,
/// versions and path constraints; disabled optional edges may overinclude a
/// package, but a compiled shared context must never be omitted.
fn compiled_packages<'a>(
    meta: &'a Metadata,
    cfg: &crate::config::Config,
) -> Vec<&'a cargo_metadata::Package> {
    let workspace = meta.workspace_packages();
    let seeds = crate::cargo_cmd::test_packages(cfg, meta);
    if seeds.is_empty() {
        return workspace;
    }
    let mut included: std::collections::BTreeSet<String> = seeds.into_iter().collect();
    loop {
        let mut next = included.clone();
        for pkg in &workspace {
            if !included.contains(pkg.name.as_str()) {
                continue;
            }
            for dependency in &pkg.dependencies {
                for candidate in &workspace {
                    if dependency.name == candidate.name.as_str()
                        && dependency.req.matches(&candidate.version)
                    {
                        let path_matches = dependency.path.as_ref().is_none_or(|p| {
                            p.as_std_path().canonicalize().ok()
                                == candidate
                                    .manifest_path
                                    .parent()
                                    .and_then(|p| p.as_std_path().canonicalize().ok())
                        });
                        if path_matches {
                            next.insert(candidate.name.to_string());
                        }
                    }
                }
            }
        }
        if next == included {
            break;
        }
        included = next;
    }
    workspace
        .into_iter()
        .filter(|p| included.contains(p.name.as_str()))
        .collect()
}

/// Other crate roots that compile an edited physical source need an inactive
/// dispatch stub. Their original function bodies remain unchanged.
pub fn inactive_roots(
    root: &Path,
    cfg: &crate::config::Config,
    selected: &Path,
    sources: &[PathBuf],
) -> Result<Vec<PathBuf>> {
    if cfg.mutation_target.is_default() {
        return Ok(Vec::new());
    }
    let meta = crate::cargo_cmd::load_metadata_with_features(root, &cfg.cargo_features)?;
    let packages = compiled_packages(&meta, cfg);
    let selected = selected.canonicalize()?;
    let sources: std::collections::BTreeSet<_> = sources
        .iter()
        .filter_map(|p| p.canonicalize().ok())
        .collect();
    let repo = crate::worktree::repo_of(root)?;
    let top = repo.toplevel.canonicalize()?;
    let tracked = crate::worktree::tracked_paths(&repo)?;
    let mut roots = std::collections::BTreeSet::new();
    for target in packages.iter().flat_map(|p| &p.targets) {
        let path = target.src_path.as_std_path().canonicalize()?;
        if path == selected {
            continue;
        }
        let files = crate::scan::scan_crate(&target.name.replace('-', "_"), &path)?;
        if files.iter().any(|f| sources.contains(&f.path)) {
            let relative = path
                .strip_prefix(&top)
                .context("runtime support root outside the Git repository")?;
            ensure!(
                tracked.contains(&crate::util::rel_slash(relative)),
                "runtime support root is untracked: {}",
                path.display()
            );
            roots.insert(path);
        }
    }
    Ok(roots.into_iter().collect())
}

pub fn extend_snapshot(
    cfg: &crate::config::Config,
    root: &Path,
    selected: &Path,
    sites: &[crate::schema::Site],
    originals: &mut std::collections::HashMap<String, Vec<u8>>,
) -> Result<()> {
    let sources: Vec<_> = sites.iter().map(|s| root.join(&s.path)).collect();
    let repo = crate::worktree::repo_of(root)?;
    let commit = crate::worktree::head(root)?;
    for extra in inactive_roots(root, cfg, selected, &sources)? {
        let top = extra
            .strip_prefix(&repo.toplevel)
            .context("support root outside repository")?;
        let top = crate::util::rel_slash(top);
        let path = crate::worktree::workspace_path(&repo.prefix, &top)?;
        originals.insert(path, crate::worktree::show(root, &commit, &top)?);
    }
    Ok(())
}

pub fn inject_selected_runtime(
    cfg: &crate::config::Config,
    root: &Path,
    selected: &Path,
) -> Result<()> {
    crate::schema::inject_runtime(selected)?;
    if cfg.mutation_target.is_default() {
        return Ok(());
    }
    let meta = crate::cargo_cmd::load_metadata_with_features(root, &cfg.cargo_features)?;
    let target = cfg
        .mutation_target
        .target(&meta, &cfg.packages[0])?
        .context("selected target")?;
    let owner = if cfg.mutation_target.bin.is_some() {
        format!("option_env!(\"CARGO_BIN_NAME\") == Some({:?})", target.name)
    } else {
        format!("option_env!(\"CARGO_BIN_NAME\").is_none() && option_env!(\"CARGO_CRATE_NAME\") == Some({:?})", target.name.replace('-', "_"))
    };
    let needle = "pub fn active(site: u32) -> Option<u32> {";
    let replacement = format!("{needle} if !(option_env!(\"CARGO_PKG_NAME\") == Some({:?}) && ({owner})) {{ return None; }}", cfg.packages[0]);
    let runtime = crate::schema::QS_RT_SRC.replace(needle, &replacement);
    let source = std::fs::read_to_string(selected)?;
    std::fs::write(selected, source.replace(crate::schema::QS_RT_SRC, &runtime))?;
    Ok(())
}

pub fn apply_schema(
    root: &Path,
    sites: &[crate::schema::Site],
    selected: &Path,
    cfg: &crate::config::Config,
) -> Result<Vec<crate::schema::AppliedSite>> {
    let applied = crate::schema::apply_schema_to_tree(root, sites, selected)?;
    inject_selected_runtime(cfg, root, selected)?;
    let sources: Vec<_> = sites.iter().map(|s| root.join(&s.path)).collect();
    for extra in inactive_roots(root, cfg, selected, &sources)? {
        let mut source = std::fs::read_to_string(&extra)?;
        if !source.contains("mod __qs_rt") {
            source.push_str("\n#[doc(hidden)] #[allow(dead_code)] mod __qs_rt { #[inline(always)] pub fn active(_: u32) -> Option<u32> { None } }\n");
            std::fs::write(extra, source)?;
        }
    }
    Ok(applied)
}

pub fn require_selected_binary(
    cfg: &crate::config::Config,
    root: &Path,
    artifacts: &[crate::cargo_cmd::BuiltExe],
) -> Result<()> {
    let Some(name) = &cfg.mutation_target.bin else {
        return Ok(());
    };
    let meta = crate::cargo_cmd::load_metadata_with_features(root, &cfg.cargo_features)?;
    let package = meta
        .workspace_packages()
        .into_iter()
        .find(|p| p.name.as_str() == cfg.packages[0])
        .context("selected binary package")?;
    let directory = package
        .manifest_path
        .parent()
        .context("selected package directory")?
        .as_std_path()
        .canonicalize()?;
    ensure!(artifacts.iter().any(|e| e.binary_target.as_deref() == Some(name.as_str())
        && e.manifest_dir.as_ref().and_then(|p| p.canonicalize().ok()).as_ref() == Some(&directory)),
        "selected binary {name} produced neither a unit-test harness nor a standalone executable for package {}; enable its required Cargo features with --features or provide binary unit/integration tests", cfg.packages[0]);
    Ok(())
}

/// Applying a physical edit cannot isolate one compilation context of a
/// shared file. Refuse rather than claim such a proof reproduces the schema.
pub fn require_physical_target(
    cfg: &crate::config::Config,
    mutant: &crate::capture::EditedMutant,
) -> Result<()> {
    if cfg.mutation_target.is_default() {
        return Ok(());
    }
    let meta = crate::cargo_cmd::load_metadata_with_features(&cfg.repo_root, &cfg.cargo_features)?;
    let selected = cfg
        .mutation_target
        .target(&meta, &cfg.packages[0])?
        .context("selected target")?;

    let sources: std::collections::BTreeSet<_> = mutant
        .edits
        .iter()
        .map(|e| cfg.repo_root.join(&e.path).canonicalize())
        .collect::<std::io::Result<_>>()?;
    for (package, other) in meta
        .workspace_packages()
        .into_iter()
        .flat_map(|p| p.targets.iter().map(move |t| (p, t)))
    {
        if std::ptr::eq(other, selected) {
            continue;
        }
        if crate::scan::scan_crate(&other.name.replace('-', "_"), other.src_path.as_std_path())?
            .iter()
            .any(|f| sources.contains(&f.path))
        {
            bail!("target-isolated mutant shares source with target {} in package {}; physical apply/prove would change other compilation contexts, so use build/test for this mutant", other.name, package.name);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn source_target_cli_is_explicit_scoped_and_replaces_config() {
        let file: crate::config::FileConfig = toml::from_str("[mutation]\nbin='author'\n").unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let merge = |args: &[&str], packages: Vec<String>| {
            let cli = crate::cli::Cli::try_parse_from(args).unwrap();
            crate::config::merge_config(
                root.clone(),
                file.clone(),
                &cli.common,
                crate::config::ConfigExtras {
                    packages,
                    ..Default::default()
                },
            )
        };
        assert_eq!(
            merge(&["qs", "map"], vec!["p".into()])
                .unwrap()
                .mutation_target
                .bin
                .as_deref(),
            Some("author")
        );
        let selected = merge(&["qs", "--lib", "map"], vec!["p".into()])
            .unwrap()
            .mutation_target;
        assert!(selected.lib && selected.bin.is_none());
        assert!(merge(&["qs", "map"], vec![]).is_err());
        assert!(merge(&["qs", "map"], vec!["p".into(), "q".into()]).is_err());
        assert!(
            crate::cli::Cli::try_parse_from(["qs", "--lib", "--bin", "author", "map"]).is_err()
        );
        assert!(Selection {
            lib: true,
            bin: Some("author".into())
        }
        .validate(&["p".into()])
        .is_err());
        assert!(Selection {
            lib: false,
            bin: Some(String::new())
        }
        .validate(&["p".into()])
        .is_err());
    }
}
