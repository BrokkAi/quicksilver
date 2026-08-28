use crate::config::Config;
use crate::types::Paths;
use crate::util::fast_linker;
use anyhow::{bail, Context, Result};
use cargo_metadata::{DependencyKind, Metadata, MetadataCommand};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Instant;

pub const PROFILE: &str = "quicksilver";

/// `cargo --config ...` must come *before* the subcommand.
pub fn cargo_command() -> Command {
    let mut cmd = Command::new("cargo");
    cmd.arg("--config")
        .arg(r#"profile.quicksilver.inherits="dev""#);
    cmd.arg("--config").arg("profile.quicksilver.debug=0");
    cmd.arg("--config")
        .arg("profile.quicksilver.incremental=true");
    cmd
}

pub fn cargo_profile_args() -> Vec<String> {
    vec!["--profile".into(), PROFILE.into()]
}

pub fn nextest_profile_args() -> Vec<String> {
    vec![
        "--profile".into(),
        PROFILE.into(),
        "--cargo-profile".into(),
        PROFILE.into(),
    ]
}

/// Which crates' tests the green suite / unscoped cargo should run.
///
/// Bare `cargo nextest run` follows `default-members`. On bifrost that is the
/// facade crate, so a core-scoped estimate got 8525 facade JUnit cases and
/// zero timings for the 317 indexed core tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageSelect {
    Workspace,
    Packages(Vec<String>),
}

pub fn covering_seeds(cfg: &Config) -> Vec<String> {
    covering_seeds_from(&cfg.packages, cfg.index_scope.as_deref())
}

pub fn covering_seeds_from(packages: &[String], index_scope: Option<&str>) -> Vec<String> {
    let mut pkgs = packages.to_vec();
    if let Some(scope) = index_scope {
        if !pkgs.iter().any(|p| p == scope) {
            pkgs.push(scope.to_string());
        }
    }
    pkgs
}

/// Packages whose incremental `cargo build --tests -p` we time for t_build.
///
/// A scoped estimate rebuilds the mutant's crate, not the whole workspace.
/// Timing every member serializes a rustc on a shared box and then averages
/// in unrelated crates, which both wastes cores and biases t_build.
pub fn packages_for_rebuild_timing(
    packages: &[String],
    index_scope: Option<&str>,
    meta: &Metadata,
) -> Vec<String> {
    let seeds = covering_seeds_from(packages, index_scope);
    if seeds.is_empty() {
        meta.workspace_packages()
            .iter()
            .map(|p| p.name.to_string())
            .collect()
    } else {
        seeds
    }
}

pub fn package_select(cfg: &Config, meta: &Metadata) -> PackageSelect {
    package_select_from(&cfg.packages, cfg.index_scope.as_deref(), meta)
}

pub fn package_select_from(
    packages: &[String],
    index_scope: Option<&str>,
    meta: &Metadata,
) -> PackageSelect {
    let seeds = covering_seeds_from(packages, index_scope);
    if seeds.is_empty() {
        PackageSelect::Workspace
    } else {
        PackageSelect::Packages(reverse_dependents(meta, &seeds).into_iter().collect())
    }
}

/// Packages whose test binaries can cover `seeds`: each seed and every
/// workspace package that transitively depends on one (normal or dev).
pub fn reverse_dependents(meta: &Metadata, seeds: &[String]) -> BTreeSet<String> {
    let ws_names: HashSet<String> = meta
        .workspace_packages()
        .iter()
        .map(|p| p.name.to_string())
        .collect();
    let mut reverse: HashMap<String, Vec<String>> = HashMap::new();
    for pkg in meta.workspace_packages() {
        for dep in &pkg.dependencies {
            if dep.kind == DependencyKind::Build {
                continue;
            }
            if !ws_names.contains(dep.name.as_str()) {
                continue;
            }
            reverse
                .entry(dep.name.to_string())
                .or_default()
                .push(pkg.name.to_string());
        }
    }
    let mut out = BTreeSet::new();
    let mut stack: Vec<String> = seeds
        .iter()
        .filter(|s| ws_names.contains(s.as_str()))
        .cloned()
        .collect();
    while let Some(p) = stack.pop() {
        if !out.insert(p.clone()) {
            continue;
        }
        if let Some(dependents) = reverse.get(&p) {
            for d in dependents {
                if !out.contains(d) {
                    stack.push(d.clone());
                }
            }
        }
    }
    out
}

pub fn apply_package_select(cmd: &mut Command, select: &PackageSelect) {
    match select {
        PackageSelect::Workspace => {
            cmd.arg("--workspace");
        }
        PackageSelect::Packages(pkgs) => {
            for p in pkgs {
                cmd.arg("-p").arg(p);
            }
        }
    }
}

pub fn rustflags(coverage: bool) -> String {
    let mut parts = Vec::new();
    if coverage {
        parts.push("-C instrument-coverage".to_string());
    }
    if let Some(linker) = fast_linker() {
        parts.push(format!("-C link-arg=-fuse-ld={linker}"));
    }
    parts.join(" ")
}

pub fn apply_build_env(cmd: &mut Command, cfg: &Config, coverage: bool) {
    cmd.env_remove("RUSTC_WRAPPER");
    cmd.env_remove("RUSTC_WORKSPACE_WRAPPER");
    cmd.env_remove("SCCACHE");
    // Honour the target repo's rust-toolchain.toml, not the caller's pin.
    cmd.env_remove("RUSTUP_TOOLCHAIN");
    cmd.env("CARGO_INCREMENTAL", "1");
    cmd.env("RUSTFLAGS", rustflags(coverage));
    for (k, v) in &cfg.build_env {
        cmd.env(k, v);
    }
}

pub fn cargo_in<'a>(root: &'a Path, target_dir: &'a Path, cfg: &'a Config) -> Command {
    let mut cmd = cargo_command();
    cmd.current_dir(root);
    cmd.env("CARGO_TARGET_DIR", target_dir);
    apply_build_env(&mut cmd, cfg, false);
    cmd
}

pub fn load_metadata(root: &Path) -> Result<Metadata> {
    MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .context("cargo metadata")
}

pub fn package_for_path(meta: &Metadata, rel: &str) -> Option<String> {
    let rel = rel.trim_start_matches("./");
    let ws = PathBuf::from(meta.workspace_root.as_str());
    let mut best: Option<(usize, String)> = None;
    for pkg in meta.workspace_packages() {
        let manifest = Path::new(pkg.manifest_path.as_std_path());
        let Some(pkg_dir) = manifest.parent() else {
            continue;
        };
        // A package whose directory is not under workspace_root is skippable,
        // not a reason to abort the whole lookup.
        let Ok(rel_pkg) = pkg_dir.strip_prefix(&ws) else {
            continue;
        };
        let prefix = crate::util::rel_slash(rel_pkg);
        let matches = if prefix.is_empty() {
            true
        } else {
            rel == prefix || rel.starts_with(&format!("{prefix}/"))
        };
        if matches {
            let score =
                prefix.chars().filter(|c| *c == '/').count() + usize::from(!prefix.is_empty());
            if best.as_ref().map(|(s, _)| score >= *s).unwrap_or(true) {
                best = Some((score, pkg.name.to_string()));
            }
        }
    }
    best.map(|(_, n)| n)
}

pub fn crate_root_source(meta: &Metadata, package: &str) -> Option<PathBuf> {
    let pkg = meta
        .workspace_packages()
        .into_iter()
        .find(|p| p.name.as_str() == package)?;
    let mut lib = None;
    let mut bin = None;
    for t in &pkg.targets {
        let kinds: Vec<String> = t.kind.iter().map(|k| k.to_string()).collect();
        let path = t.src_path.as_std_path().to_path_buf();
        if kinds
            .iter()
            .any(|k| k == "lib" || k == "rlib" || k == "dylib" || k == "cdylib")
        {
            lib = Some(path);
            break;
        }
        if kinds.iter().any(|k| k == "bin") && bin.is_none() {
            bin = Some(path);
        }
    }
    lib.or(bin)
}

pub fn run_captured(cmd: &mut Command) -> Result<Output> {
    let output = cmd.output().with_context(|| format!("spawn {:?}", cmd))?;
    Ok(output)
}

pub fn run_status(cmd: &mut Command) -> Result<std::process::ExitStatus> {
    let status = cmd.status().with_context(|| format!("spawn {:?}", cmd))?;
    Ok(status)
}

pub fn build_tests(
    root: &Path,
    target_dir: &Path,
    cfg: &Config,
    packages: &[String],
    coverage: bool,
    json: bool,
) -> Result<BuildResult> {
    let mut cmd = cargo_command();
    cmd.current_dir(root);
    cmd.env("CARGO_TARGET_DIR", target_dir);
    apply_build_env(&mut cmd, cfg, coverage);
    cmd.arg("build").arg("--tests");
    cmd.args(cargo_profile_args());
    // One cargo invocation with repeated -p. Separate `cargo build -p X` per
    // package pays startup + lock + fingerprint 2–4× on the compile-bound path.
    // Empty means the whole workspace — cargo-mutants pays for everything, and
    // default-members can be just a facade crate.
    if packages.is_empty() {
        cmd.arg("--workspace");
    } else {
        for p in packages {
            cmd.arg("-p").arg(p);
        }
    }
    if json {
        cmd.arg("--message-format=json");
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::inherit());
    } else {
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
    }
    let start = Instant::now();
    let output = cmd.output().context("cargo build --tests")?;
    let secs = start.elapsed().as_secs_f64();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("cargo build --tests failed:\n{stderr}");
    }
    let executables = if json {
        parse_executables(&output.stdout, target_dir)
    } else {
        Vec::new()
    };
    Ok(BuildResult {
        secs,
        executables,
        stdout: output.stdout,
    })
}

pub struct BuildResult {
    pub secs: f64,
    pub executables: Vec<PathBuf>,
    pub stdout: Vec<u8>,
}

/// Like `build_tests` with `--message-format=json`, but does not bail on
/// rustc failure — schema isolation needs the diagnostic spans.
pub struct CapturedBuild {
    pub success: bool,
    pub secs: f64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub executables: Vec<PathBuf>,
}

pub fn build_tests_captured(
    root: &Path,
    target_dir: &Path,
    cfg: &Config,
    packages: &[String],
    coverage: bool,
) -> Result<CapturedBuild> {
    let mut cmd = cargo_command();
    cmd.current_dir(root);
    cmd.env("CARGO_TARGET_DIR", target_dir);
    apply_build_env(&mut cmd, cfg, coverage);
    cmd.arg("build").arg("--tests");
    cmd.args(cargo_profile_args());
    if packages.is_empty() {
        cmd.arg("--workspace");
    } else {
        for p in packages {
            cmd.arg("-p").arg(p);
        }
    }
    cmd.arg("--message-format=json");
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let start = Instant::now();
    let output = cmd
        .output()
        .context("cargo build --tests --message-format=json")?;
    let secs = start.elapsed().as_secs_f64();
    let executables = parse_executables(&output.stdout, target_dir);
    Ok(CapturedBuild {
        success: output.status.success(),
        secs,
        stdout: output.stdout,
        stderr: output.stderr,
        executables,
    })
}

pub fn parse_executables(stdout: &[u8], target_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in stdout.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-artifact") {
            continue;
        }
        if let Some(exe) = v.get("executable").and_then(|e| e.as_str()) {
            let p = PathBuf::from(exe);
            out.push(p);
        }
    }
    let _ = target_dir;
    out
}

pub fn count_compiling_units(stderr: &str) -> u64 {
    stderr.lines().filter(|l| l.contains("Compiling ")).count() as u64
}

pub fn nextest_available() -> bool {
    crate::util::which("cargo-nextest").is_some()
}

pub fn write_nextest_config(index: &Path) -> Result<PathBuf> {
    write_nextest_config_junit(index, None)
}

pub fn write_nextest_config_junit(index: &Path, junit: Option<&Path>) -> Result<PathBuf> {
    let path = index.join("nextest.toml");
    let junit_path = match junit {
        Some(p) => p.display().to_string(),
        None => "junit.xml".into(),
    };
    let body = format!(
        "\n[profile.quicksilver]\nslow-timeout = {{ period = \"120s\", terminate-after = 2 }}\nfail-fast = false\n\n[profile.quicksilver.junit]\npath = \"{junit_path}\"\n"
    );
    crate::util::write_atomic(&path, body.trim_start())?;
    Ok(path)
}

pub fn relative_to_target(exe: &Path, target_dir: &Path) -> String {
    if let Ok(rel) = exe.strip_prefix(target_dir) {
        return crate::util::rel_slash(rel);
    }
    exe.file_name()
        .map(|n| format!("deps/{}", n.to_string_lossy()))
        .unwrap_or_else(|| crate::util::rel_slash(exe))
}

pub fn package_dir_rel(meta: &Metadata, package: &str) -> String {
    let ws = PathBuf::from(meta.workspace_root.as_str());
    meta.workspace_packages()
        .into_iter()
        .find(|p| p.name.as_str() == package)
        .and_then(|p| {
            Path::new(p.manifest_path.as_std_path())
                .parent()
                .and_then(|d| d.strip_prefix(&ws).ok())
                .map(crate::util::rel_slash)
        })
        .unwrap_or_default()
}

pub fn dump_metadata_json(paths: &Paths, meta: &Metadata) -> Result<()> {
    let raw = serde_json::to_vec_pretty(meta)?;
    crate::util::write_atomic(&paths.index.join("cargo-metadata.json"), raw)?;
    Ok(())
}

pub fn env_for_tests() -> BTreeMap<String, String> {
    BTreeMap::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_select_unscoped_is_workspace() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny");
        let meta = load_metadata(&root).expect("tiny metadata");
        assert_eq!(
            package_select_from(&[], None, &meta),
            PackageSelect::Workspace
        );
    }

    #[test]
    fn reverse_dependents_include_downstream_crates() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny");
        let meta = load_metadata(&root).expect("tiny metadata");
        let d = reverse_dependents(&meta, &["tiny_a".into()]);
        assert!(d.contains("tiny_a"));
        assert!(d.contains("tiny_b"), "tiny_b links tiny_a: {d:?}");
        assert!(d.contains("tiny_c"), "tiny_c links tiny_a: {d:?}");
        let php_like = reverse_dependents(&meta, &["tiny_b".into()]);
        assert!(php_like.contains("tiny_b"));
        assert!(
            !php_like.contains("tiny_a"),
            "tiny_a does not depend on tiny_b: {php_like:?}"
        );
        assert!(!php_like.contains("tiny_c"));
    }

    #[test]
    fn rebuild_timing_uses_seeds_not_the_whole_workspace() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny");
        let meta = load_metadata(&root).expect("tiny metadata");
        let scoped = packages_for_rebuild_timing(&["tiny_a".into()], None, &meta);
        assert_eq!(scoped, vec!["tiny_a".to_string()]);
        let unscoped = packages_for_rebuild_timing(&[], None, &meta);
        assert!(unscoped.len() >= 3, "{unscoped:?}");
        assert!(unscoped.iter().any(|p| p == "tiny_a"));
        assert!(unscoped.iter().any(|p| p == "tiny_b"));
    }

    #[test]
    fn package_for_path_matches_workspace_members() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny");
        let meta = load_metadata(&root).expect("tiny metadata");
        assert_eq!(
            package_for_path(&meta, "crate_a/src/lib.rs").as_deref(),
            Some("tiny_a")
        );
        assert_eq!(
            package_for_path(&meta, "crate_b/src/lib.rs").as_deref(),
            Some("tiny_b")
        );
        assert_eq!(package_for_path(&meta, "no/such.rs"), None);
    }
}
