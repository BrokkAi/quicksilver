use crate::cargo_cmd::{
    crate_root_source, load_metadata, write_nextest_config, BuildResult, apply_build_env,
    cargo_profile_args, count_compiling_units, dump_metadata_json, nextest_available,
};
use crate::config::Config;
use crate::discover::{apply_mutant, restore_file};
use crate::types::{BaselineReport, Mutant, PackageCost, Paths, TestInfo};
use anyhow::{Context, Result, bail};
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::{Instant, SystemTime};
use tracing::info;

pub fn materialize_and_metadata(cfg: &Config, paths: &Paths) -> Result<cargo_metadata::Metadata> {
    paths.ensure()?;
    if !paths.baseline.join("Cargo.toml").is_file() {
        info!("materializing baseline from {}", cfg.repo_root.display());
        crate::scratch::materialize_baseline(&cfg.repo_root, &paths.baseline)?;
    }
    crate::scratch::write_quicksilver_cargo_config(&paths.baseline)?;
    let meta = load_metadata(&paths.baseline)?;
    dump_metadata_json(paths, &meta)?;
    Ok(meta)
}

pub fn build_baseline(cfg: &Config, paths: &Paths) -> Result<BuildResult> {
    let target = paths.baseline.join("target");
    info!("baseline cargo build --tests (profile quicksilver)");
    crate::cargo_cmd::build_tests(&paths.baseline, &target, cfg, None, false, false)
}

pub fn run_green_suite(cfg: &Config, paths: &Paths) -> Result<(f64, Vec<TestInfo>)> {
    if !nextest_available() {
        bail!("cargo nextest is required. Install it with: cargo install cargo-nextest");
    }
    let config_path = write_nextest_config(&paths.index)?;
    let target = paths.baseline.join("target");
    let mut last_tests = Vec::new();
    let mut last_secs = 0.0;
    for run in 0..cfg.baseline_runs.max(1) {
        info!("baseline cargo nextest run ({}/{})", run + 1, cfg.baseline_runs.max(1));
        let mut cmd = crate::cargo_cmd::cargo_command();
        cmd.current_dir(&paths.baseline);
        cmd.env("CARGO_TARGET_DIR", &target);
        apply_build_env(&mut cmd, cfg, false);
        cmd.args(["nextest", "run"]);
        cmd.args(crate::cargo_cmd::nextest_profile_args());
        cmd.arg("--config-file").arg(&config_path);
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
        let start = Instant::now();
        let status = cmd.status().context("cargo nextest run")?;
        last_secs = start.elapsed().as_secs_f64();
        if !status.success() {
            bail!("red baseline: cargo nextest run failed. Mutation results are meaningless against a failing suite.");
        }
        let junit = paths
            .baseline
            .join("target")
            .join("nextest")
            .join("quicksilver")
            .join("junit.xml");
        let junit = if junit.is_file() {
            junit
        } else {
            paths.index.join("junit.xml")
        };
        if junit.is_file() {
            last_tests = parse_junit(&fs::read_to_string(&junit)?, &load_metadata(&paths.baseline)?)?;
        }
    }
    let json = serde_json::to_vec_pretty(&last_tests)?;
    crate::util::write_atomic(&paths.index.join("tests.json"), json)?;
    Ok((last_secs, last_tests))
}

pub fn parse_junit(xml: &str, meta: &cargo_metadata::Metadata) -> Result<Vec<TestInfo>> {
    let mut tests = Vec::new();
    for (name, classname, time) in junit_cases(xml) {
        let package = classname.clone();
        let cwd = crate::cargo_cmd::package_dir_rel(meta, &package);
        tests.push(TestInfo {
            id: format!("{package}::{name}"),
            name,
            package,
            binary_id: classname,
            binary_name: String::new(),
            binary_rel: String::new(),
            cwd_rel: cwd,
            duration_secs: time,
        });
    }
    Ok(tests)
}

fn junit_cases(xml: &str) -> Vec<(String, String, f64)> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(idx) = rest.find("<testcase") {
        rest = &rest[idx..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        let name = attr(tag, "name").unwrap_or_default();
        let classname = attr(tag, "classname").unwrap_or_default();
        let time: f64 = attr(tag, "time")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        if !name.is_empty() {
            out.push((name, classname, time));
        }
        rest = &rest[end.min(rest.len())..];
    }
    out
}

fn attr(tag: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let i = tag.find(&pat)?;
    let rest = &tag[i + pat.len()..];
    let j = rest.find('"')?;
    Some(rest[..j].to_string())
}

pub fn measure_package_costs(cfg: &Config, paths: &Paths) -> Result<Vec<PackageCost>> {
    let meta = load_metadata(&paths.baseline)?;
    let target = paths.baseline.join("target");
    let mut costs = Vec::new();
    for pkg in meta.workspace_packages() {
        let name = pkg.name.to_string();
        let Some(src) = crate_root_source(&meta, &name) else {
            continue;
        };
        if !src.is_file() {
            continue;
        }
        touch(&src)?;
        info!("timing incremental rebuild of {name}");
        let start = Instant::now();
        match crate::cargo_cmd::build_tests(&paths.baseline, &target, cfg, Some(&name), false, false) {
            Ok(_) => {
                costs.push(PackageCost {
                    package: name,
                    rebuild_secs: start.elapsed().as_secs_f64(),
                });
            }
            Err(err) => {
                tracing::warn!("rebuild of {name} failed: {err}");
            }
        }
    }
    Ok(costs)
}

fn touch(path: &Path) -> Result<()> {
    let f = fs::OpenOptions::new().write(true).open(path)?;
    f.set_modified(SystemTime::now())?;
    Ok(())
}

/// Mutate one throwaway function, rebuild, count how many units recompiled.
pub fn preflight_recompile_count(
    cfg: &Config,
    paths: &Paths,
    mutant: &Mutant,
) -> Result<u64> {
    let target = paths.baseline.join("target");
    let original = apply_mutant(&paths.baseline, mutant)?;
    let mut cmd = crate::cargo_cmd::cargo_command();
    cmd.current_dir(&paths.baseline);
    cmd.env("CARGO_TARGET_DIR", &target);
    apply_build_env(&mut cmd, cfg, false);
    cmd.args(["build", "--tests", "-p", &mutant.package, "-v"]);
    cmd.args(cargo_profile_args());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let output = cmd.output().context("preflight rebuild")?;
    let _ = restore_file(&paths.baseline, mutant, &original);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(count_compiling_units(&stderr))
}

pub fn write_baseline_report(paths: &Paths, report: &BaselineReport) -> Result<()> {
    crate::util::write_atomic(
        &paths.index.join("baseline.json"),
        serde_json::to_vec_pretty(report)?,
    )
}

pub fn load_baseline_report(paths: &Paths) -> Result<Option<BaselineReport>> {
    let p = paths.index.join("baseline.json");
    if !p.is_file() {
        return Ok(None);
    }
    let text = fs::read_to_string(p)?;
    Ok(Some(serde_json::from_str(&text)?))
}

pub fn prepare_baseline(cfg: &Config, paths: &Paths, skip: bool) -> Result<BaselineReport> {
    let _meta = materialize_and_metadata(cfg, paths)?;
    if skip {
        if let Some(existing) = load_baseline_report(paths)? {
            return Ok(existing);
        }
        return Ok(BaselineReport {
            suite_secs: 0.0,
            tests: Vec::new(),
            package_costs: Vec::new(),
            preflight_recompiled: None,
        });
    }
    build_baseline(cfg, paths)?;
    let (suite_secs, tests) = run_green_suite(cfg, paths)?;
    let package_costs = measure_package_costs(cfg, paths)?;
    let report = BaselineReport {
        suite_secs,
        tests,
        package_costs,
        preflight_recompiled: None,
    };
    write_baseline_report(paths, &report)?;
    Ok(report)
}
