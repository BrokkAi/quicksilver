use crate::config::Config;
use crate::types::Paths;
use anyhow::{bail, Context, Result};
use cargo_metadata::{DependencyKind, Metadata, MetadataCommand};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Instant;

/// The nextest profile QS writes into its own nextest config file. Builds
/// use Cargo's ordinary dev and test profiles, so they share mbx cache
/// entries with the user's own builds.
pub const NEXTEST_PROFILE: &str = "quicksilver";

/// Cargo, run through mbx: `mbx <subcommand> ...`. mbx rejects options
/// before the subcommand, so callers add everything after it.
pub fn cargo_command() -> Command {
    Command::new("mbx")
}

pub fn nextest_profile_args() -> Vec<String> {
    vec!["--profile".into(), NEXTEST_PROFILE.into()]
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

/// The packages whose code is mutated: `--package`, or none for all.
pub fn covering_seeds(cfg: &Config) -> Vec<String> {
    cfg.packages.clone()
}

/// Packages whose incremental `cargo build --tests -p` we time for t_build.
///
/// A scoped estimate rebuilds the mutant's crate, not the whole workspace.
/// Timing every member serializes a rustc on a shared box and then averages
/// in unrelated crates, which both wastes cores and biases t_build.
pub fn packages_for_rebuild_timing(packages: &[String], meta: &Metadata) -> Vec<String> {
    let seeds = packages.to_vec();
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
    package_select_from(&cfg.packages, meta)
}

pub fn package_select_from(packages: &[String], meta: &Metadata) -> PackageSelect {
    let seeds = packages.to_vec();
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
            let root = cmd.get_current_dir().map(Path::to_path_buf);
            for p in package_specs(root.as_deref(), pkgs) {
                cmd.arg("-p").arg(p);
            }
        }
    }
}

/// `-p` values that name workspace members unambiguously. A bare name is
/// ambiguous when a dependency has the same name (itertools' dev-dependencies
/// pull in a registry itertools), and cargo then refuses the whole command.
/// The package id from `cargo metadata` is unique by construction.
pub fn package_specs(root: Option<&Path>, packages: &[String]) -> Vec<String> {
    let meta = root.and_then(|r| load_metadata(r).ok());
    packages
        .iter()
        .map(|p| {
            meta.as_ref()
                .and_then(|m| {
                    m.workspace_packages()
                        .into_iter()
                        .find(|w| w.name.as_str() == p)
                        .map(|w| w.id.repr.clone())
                })
                .unwrap_or_else(|| p.clone())
        })
        .collect()
}

/// The environment of every QS build. Builds keep the caller's flags and let
/// mbx manage the tree's `target/`: identical invocations share mbx's cache
/// with the user's own builds, and mbx gives the crates QS edits private
/// incremental state.
pub fn apply_build_env(cmd: &mut Command, cfg: &Config) {
    cmd.env_remove("RUSTC_WRAPPER");
    cmd.env_remove("RUSTC_WORKSPACE_WRAPPER");
    cmd.env_remove("SCCACHE");
    // Honour the target repo's rust-toolchain.toml, not the caller's pin.
    cmd.env_remove("RUSTUP_TOOLCHAIN");
    cmd.env_remove("CARGO_TARGET_DIR");
    for (k, v) in &cfg.build_env {
        cmd.env(k, v);
    }
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

/// Which files of a package the schema engine can mutate: those in the module
/// tree of the target it instruments (the library, else the first binary).
/// Other targets (examples, benches, tests, build scripts, extra binaries)
/// are separate crates where `crate::__qs_rt` does not exist.
#[derive(Debug, Clone)]
pub struct MutableScope {
    pub root_dir: PathBuf,
    pub root_file: PathBuf,
    /// Entry files and directories of the package's other targets.
    pub other_targets: Vec<PathBuf>,
    /// Proc-macro code runs inside rustc while dependents compile, so a
    /// mutant switched on at test run time never takes effect.
    pub proc_macro: bool,
}

impl MutableScope {
    pub fn contains(&self, abs: &Path) -> bool {
        if abs == self.root_file {
            return true;
        }
        abs.starts_with(&self.root_dir)
            && !self
                .other_targets
                .iter()
                .any(|t| abs == t || (t.is_dir() && abs.starts_with(t)))
    }
}

pub fn mutable_scope(meta: &Metadata, package: &str) -> Option<MutableScope> {
    let pkg = meta
        .workspace_packages()
        .into_iter()
        .find(|p| p.name.as_str() == package)?;
    let root_file = crate_root_source(meta, package)?;
    let root_dir = root_file.parent()?.to_path_buf();
    let proc_macro = pkg
        .targets
        .iter()
        .any(|t| t.kind.iter().any(|k| k.to_string() == "proc-macro"));
    let mut other_targets = Vec::new();
    for t in &pkg.targets {
        let src = t.src_path.as_std_path().to_path_buf();
        if src == root_file {
            continue;
        }
        // `src/bin/tool/main.rs` owns its whole directory; `src/main.rs`
        // next to `src/lib.rs` owns only itself.
        match src.parent() {
            Some(dir) if dir != root_dir && dir.starts_with(&root_dir) => {
                other_targets.push(dir.to_path_buf())
            }
            _ => other_targets.push(src),
        }
    }
    Some(MutableScope {
        root_dir,
        root_file,
        other_targets,
        proc_macro,
    })
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
        if kinds.iter().any(|k| {
            k == "lib" || k == "rlib" || k == "dylib" || k == "cdylib" || k == "proc-macro"
        }) {
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
    json: bool,
) -> Result<BuildResult> {
    let mut cmd = cargo_command();
    cmd.current_dir(root);
    apply_build_env(&mut cmd, cfg);
    cmd.arg("build").arg("--tests");
    // One cargo invocation with repeated -p. Separate `cargo build -p X` per
    // package pays startup + lock + fingerprint 2–4× on the compile-bound path.
    // Empty means the whole workspace — cargo-mutants pays for everything, and
    // default-members can be just a facade crate.
    if packages.is_empty() {
        cmd.arg("--workspace");
    } else {
        for p in package_specs(Some(root), packages) {
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
    pub executables: Vec<BuiltExe>,
    pub stdout: Vec<u8>,
}

/// Like `build_tests` with `--message-format=json`, but does not bail on
/// rustc failure — schema isolation needs the diagnostic spans.
pub struct CapturedBuild {
    pub success: bool,
    pub secs: f64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub executables: Vec<BuiltExe>,
}

/// `cargo build --tests --message-format=json`, capturing the JSON. With
/// `progress`, cargo's stderr is read as it arrives: each `Compiling` line
/// advances the progress and names the crate, and mbx's summary lines are
/// shown, so a long build is not silent.
pub fn build_tests_captured(
    root: &Path,
    target_dir: &Path,
    cfg: &Config,
    packages: &[String],
    progress: Option<&crate::progress::Progress>,
) -> Result<CapturedBuild> {
    let mut cmd = cargo_command();
    cmd.current_dir(root);
    apply_build_env(&mut cmd, cfg);
    cmd.arg("build").arg("--tests");
    if packages.is_empty() {
        cmd.arg("--workspace");
    } else {
        for p in package_specs(Some(root), packages) {
            cmd.arg("-p").arg(p);
        }
    }
    cmd.arg("--message-format=json");
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let start = Instant::now();
    let (status, stdout, stderr) = match progress {
        None => {
            let output = cmd
                .output()
                .context("cargo build --tests --message-format=json")?;
            (output.status, output.stdout, output.stderr)
        }
        Some(progress) => {
            use std::io::{BufRead, Read};
            let mut child = cmd
                .spawn()
                .context("cargo build --tests --message-format=json")?;
            let err = child.stderr.take().context("cargo stderr")?;
            let mut out = child.stdout.take().context("cargo stdout")?;
            let stderr = std::thread::scope(|scope| {
                let reader = scope.spawn(move || {
                    let mut kept = Vec::new();
                    for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
                        let trimmed = line.trim_start();
                        if let Some(rest) = trimmed.strip_prefix("Compiling ") {
                            progress.set_message(rest.split_whitespace().next().unwrap_or(rest));
                            progress.inc(1);
                        } else if trimmed.starts_with("mbx[") {
                            progress.println(trimmed);
                        }
                        kept.extend_from_slice(line.as_bytes());
                        kept.push(b'\n');
                    }
                    kept
                });
                let mut stdout = Vec::new();
                let _ = out.read_to_end(&mut stdout);
                (stdout, reader.join().unwrap_or_default())
            });
            let status = child.wait().context("wait for cargo build")?;
            (status, stderr.0, stderr.1)
        }
    };
    let secs = start.elapsed().as_secs_f64();
    let executables = parse_executables(&stdout, target_dir);
    Ok(CapturedBuild {
        success: status.success(),
        secs,
        stdout,
        stderr,
        executables,
    })
}

/// Test harnesses built by `cargo build --tests`, in build order.
///
/// Only artifacts with `profile.test == true` are kept. `--tests` also
/// produces plain `bin` artifacts (a CLI, a fuzzer); every caller here stages
/// binaries to run with `--exact <test name>`, which a plain bin either
/// rejects with a nonzero exit or, worse, ignores.
/// A test harness cargo just built, with the directory of the manifest that
/// owns it. Two packages can both have a test target named `builder`;
/// the manifest directory tells them apart.
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltExe {
    pub path: PathBuf,
    pub manifest_dir: Option<PathBuf>,
}

pub fn parse_executables(stdout: &[u8], target_dir: &Path) -> Vec<BuiltExe> {
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
        let is_test = v
            .get("profile")
            .and_then(|p| p.get("test"))
            .and_then(|t| t.as_bool())
            .unwrap_or(false);
        if !is_test {
            continue;
        }
        if let Some(exe) = v.get("executable").and_then(|e| e.as_str()) {
            let manifest_dir = v
                .get("manifest_path")
                .and_then(|m| m.as_str())
                .and_then(|m| Path::new(m).parent().map(Path::to_path_buf));
            out.push(BuiltExe {
                path: PathBuf::from(exe),
                manifest_dir,
            });
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
    fn package_specs_name_members_by_package_id() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny");
        let specs = package_specs(Some(&root), &["tiny_a".into(), "not_a_member".into()]);
        assert!(
            specs[0].starts_with("path+file://") && specs[0].ends_with("#tiny_a@0.1.0"),
            "{specs:?}"
        );
        assert_eq!(specs[1], "not_a_member");
        assert_eq!(
            package_specs(None, &["tiny_a".into()]),
            vec!["tiny_a".to_string()]
        );
    }

    #[test]
    fn parse_executables_keeps_only_test_harnesses() {
        let stdout = concat!(
            r#"{"reason":"compiler-artifact","target":{"name":"brokk-bifrost-core"},"profile":{"test":true},"executable":"/t/quicksilver/deps/brokk_bifrost_core-c4d0bb3ad97e1f96"}"#,
            "\n",
            r#"{"reason":"compiler-artifact","target":{"name":"bifrost_mcp_property_fuzzer"},"profile":{"test":false},"executable":"/t/quicksilver/deps/bifrost_mcp_property_fuzzer-abcdef1234567890"}"#,
            "\n",
            // No profile at all: not provably a harness, so it is dropped.
            r#"{"reason":"compiler-artifact","target":{"name":"odd"},"executable":"/t/quicksilver/deps/odd-1111111111111111"}"#,
            "\n",
            r#"{"reason":"build-finished","success":true}"#,
            "\n",
        );
        let exes: Vec<PathBuf> = parse_executables(stdout.as_bytes(), Path::new("/t"))
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(
            exes,
            vec![PathBuf::from(
                "/t/quicksilver/deps/brokk_bifrost_core-c4d0bb3ad97e1f96"
            )],
            "plain bin artifacts must not be staged as test binaries"
        );
    }

    #[test]
    fn package_select_unscoped_is_workspace() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny");
        let meta = load_metadata(&root).expect("tiny metadata");
        assert_eq!(package_select_from(&[], &meta), PackageSelect::Workspace);
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
        let scoped = packages_for_rebuild_timing(&["tiny_a".into()], &meta);
        assert_eq!(scoped, vec!["tiny_a".to_string()]);
        let unscoped = packages_for_rebuild_timing(&[], &meta);
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
