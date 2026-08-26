use crate::config::Config;
use crate::types::Paths;
use crate::util::fast_linker;
use anyhow::{Context, Result, bail};
use cargo_metadata::{Metadata, MetadataCommand};
use std::collections::BTreeMap;
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
    cmd.arg("--config").arg("profile.quicksilver.incremental=true");
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
        let pkg_dir = manifest.parent()?;
        let rel_pkg = pkg_dir.strip_prefix(&ws).ok()?;
        let prefix = crate::util::rel_slash(rel_pkg);
        let matches = if prefix.is_empty() {
            true
        } else {
            rel == prefix || rel.starts_with(&format!("{prefix}/"))
        };
        if matches {
            let score = prefix.chars().filter(|c| *c == '/').count() + usize::from(!prefix.is_empty());
            if best.as_ref().map(|(s, _)| score >= *s).unwrap_or(true) {
                best = Some((score, pkg.name.to_string()));
            }
        }
    }
    best.map(|(_, n)| n)
}

pub fn crate_root_source(meta: &Metadata, package: &str) -> Option<PathBuf> {
    let pkg = meta.workspace_packages().into_iter().find(|p| p.name.as_str() == package)?;
    let mut lib = None;
    let mut bin = None;
    for t in &pkg.targets {
        let kinds: Vec<String> = t.kind.iter().map(|k| k.to_string()).collect();
        let path = t.src_path.as_std_path().to_path_buf();
        if kinds.iter().any(|k| k == "lib" || k == "rlib" || k == "dylib" || k == "cdylib") {
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
    package: Option<&str>,
    coverage: bool,
    json: bool,
) -> Result<BuildResult> {
    let mut cmd = cargo_command();
    cmd.current_dir(root);
    cmd.env("CARGO_TARGET_DIR", target_dir);
    apply_build_env(&mut cmd, cfg, coverage);
    cmd.arg("build").arg("--tests");
    cmd.args(cargo_profile_args());
    if let Some(p) = package {
        cmd.arg("-p").arg(p);
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
    Ok(BuildResult { secs, executables, stdout: output.stdout })
}

pub struct BuildResult {
    pub secs: f64,
    pub executables: Vec<PathBuf>,
    pub stdout: Vec<u8>,
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
    stderr
        .lines()
        .filter(|l| l.contains("Compiling "))
        .count() as u64
}

pub fn nextest_available() -> bool {
    crate::util::which("cargo-nextest").is_some()
}

pub fn write_nextest_config(index: &Path) -> Result<PathBuf> {
    let path = index.join("nextest.toml");
    let body = r#"
[profile.quicksilver]
slow-timeout = { period = "120s", terminate-after = 2 }

[profile.quicksilver.junit]
path = "junit.xml"
"#;
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
