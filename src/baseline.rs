use crate::cargo_cmd::{
    apply_build_env, count_compiling_units, crate_root_source, dump_metadata_json, load_metadata,
    nextest_available, write_nextest_config, BuildResult,
};
use crate::config::Config;
use crate::discover::{apply_mutant, restore_file};
use crate::types::{BaselineReport, Mutant, PackageCost, Paths, TestInfo};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::{Instant, SystemTime};
use tracing::info;

/// Check out the checkout's HEAD in QS's main worktree (creating it the
/// first time) and create the state directories. The checkout must have no
/// uncommitted changes to tracked files: QS tests the commit.
pub fn checkout_main(cfg: &Config, paths: &Paths) -> Result<String> {
    crate::worktree::require_mbx()?;
    let commit = crate::worktree::require_committed(&cfg.repo_root)?;
    let repo = crate::worktree::repo_of(&cfg.repo_root)?;
    let main = crate::worktree::ensure(&repo, crate::worktree::MAIN, &commit)?;
    info!(
        "worktree {} at {}",
        main.display(),
        &commit[..12.min(commit.len())]
    );
    paths.ensure()?;
    Ok(commit)
}

pub fn materialize_and_metadata(cfg: &Config, paths: &Paths) -> Result<cargo_metadata::Metadata> {
    checkout_main(cfg, paths)?;
    let meta = load_metadata(&paths.baseline)?;
    dump_metadata_json(paths, &meta)?;
    Ok(meta)
}

pub fn build_baseline(cfg: &Config, paths: &Paths) -> Result<BuildResult> {
    let target = paths.baseline.join("target");
    info!("baseline cargo build --tests (profile quicksilver)");
    let meta = load_metadata(&paths.baseline)?;
    let packages = crate::cargo_cmd::test_packages(cfg, &meta);
    crate::cargo_cmd::build_tests(&paths.baseline, &target, cfg, &packages, false)
}

pub fn run_green_suite(cfg: &Config, paths: &Paths) -> Result<(f64, Vec<TestInfo>, Vec<String>)> {
    if !nextest_available() {
        bail!("cargo nextest is required. Install it with: cargo install cargo-nextest");
    }
    let config_path = write_nextest_config(&paths.index)?;
    let meta = load_metadata(&paths.baseline)?;
    let mut last_tests = Vec::new();
    let mut last_failed_ids = Vec::new();
    let mut last_secs = 0.0;
    for run in 0..cfg.baseline_runs.max(1) {
        info!(
            "baseline cargo nextest run ({}/{})",
            run + 1,
            cfg.baseline_runs.max(1)
        );
        let mut cmd = crate::cargo_cmd::cargo_command();
        cmd.current_dir(&paths.baseline);
        apply_build_env(&mut cmd, cfg);
        cmd.args(["nextest", "run"]);
        cmd.args(crate::cargo_cmd::nextest_profile_args());
        cmd.arg("--config-file").arg(&config_path);
        crate::cargo_cmd::apply_package_select(
            &mut cmd,
            &crate::cargo_cmd::test_packages(cfg, &meta),
        );
        if let Some(filter) = crate::cargo_cmd::nextest_filter(cfg, &meta) {
            cmd.arg("-E").arg(filter);
        }
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
        let start = Instant::now();
        let status = cmd.status().context("cargo nextest run")?;
        last_secs = start.elapsed().as_secs_f64();
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
            let xml = fs::read_to_string(&junit)?;
            last_tests = parse_junit(&xml, &meta)?;
            let seeds = crate::cargo_cmd::covering_seeds(cfg);
            let failed = junit_failures(&xml);
            let seed_fails: Vec<_> = failed
                .iter()
                .filter(|f| seeds.is_empty() || seeds.iter().any(|s| s == &f.package))
                .cloned()
                .collect();
            if !seed_fails.is_empty() {
                bail!(
                    "red baseline: {} failing tests in {:?}: {} … Mutation results are meaningless against a failing suite.",
                    seed_fails.len(),
                    seeds,
                    seed_fails
                        .iter()
                        .take(5)
                        .map(|f| f.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !failed.is_empty() {
                tracing::warn!(
                    "{} workspace tests failed (not in {:?}); keeping timings. first: {}",
                    failed.len(),
                    seeds,
                    failed[0].name
                );
            }
            last_failed_ids = failed.into_iter().map(|f| f.id).collect();
        } else if !status.success() {
            bail!("red baseline: cargo nextest run failed and produced no junit.xml");
        }
    }
    let json = serde_json::to_vec_pretty(&last_tests)?;
    crate::util::write_atomic(&paths.index.join("tests.json"), json)?;
    Ok((last_secs, last_tests, last_failed_ids))
}

pub fn parse_junit(xml: &str, meta: &cargo_metadata::Metadata) -> Result<Vec<TestInfo>> {
    let mut tests = Vec::new();
    for (name, classname, time) in junit_cases(xml) {
        // nextest's JUnit classname is the binary id, not the package name.
        // Integration tests look like `pkg::bin`; lib tests are often just `pkg`.
        let binary_id = classname;
        let package = binary_id
            .split("::")
            .next()
            .unwrap_or(binary_id.as_str())
            .to_string();
        let cwd = crate::cargo_cmd::package_dir_rel(meta, &package);
        tests.push(TestInfo {
            id: format!("{binary_id}::{name}"),
            name,
            package,
            binary_id,
            binary_name: String::new(),
            binary_rel: String::new(),
            cwd_rel: cwd,
            duration_secs: time,
        });
    }
    Ok(tests)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JunitFailure {
    pub id: String,
    pub package: String,
    pub name: String,
}

/// Testcases that contain a failure or error.
pub fn junit_failures(xml: &str) -> Vec<JunitFailure> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(idx) = rest.find("<testcase") {
        rest = &rest[idx..];
        let tag_end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..tag_end];
        let self_close = tag.trim_end().ends_with('/');
        let name = attr(tag, "name").unwrap_or_default();
        let classname = attr(tag, "classname").unwrap_or_default();
        let package = classname
            .split("::")
            .next()
            .unwrap_or(classname.as_str())
            .to_string();
        let body = if self_close {
            ""
        } else {
            let close = rest.find("</testcase>").unwrap_or(rest.len());
            &rest[..close]
        };
        if !name.is_empty() && (body.contains("<failure") || body.contains("<error")) {
            out.push(JunitFailure {
                id: format!("{classname}::{name}"),
                package,
                name,
            });
        }
        rest = &rest[tag_end.min(rest.len()) + 1..];
    }
    out
}

pub fn junit_passed_ids(xml: &str) -> std::collections::HashSet<String> {
    let failed: std::collections::HashSet<String> =
        junit_failures(xml).into_iter().map(|f| f.id).collect();
    let mut pass = std::collections::HashSet::new();
    for (name, classname, _) in junit_cases(xml) {
        let id = format!("{classname}::{name}");
        if !failed.contains(&id) {
            pass.insert(id);
        }
    }
    pass
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
    let names = crate::cargo_cmd::packages_for_rebuild_timing(&cfg.packages, &meta);
    let mut costs = Vec::new();
    for name in names {
        let Some(src) = crate_root_source(&meta, &name) else {
            continue;
        };
        if !src.is_file() {
            continue;
        }
        touch(&src)?;
        info!("timing incremental rebuild of {name}");
        let start = Instant::now();
        match crate::cargo_cmd::build_tests(
            &paths.baseline,
            &target,
            cfg,
            std::slice::from_ref(&name),
            false,
        ) {
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
pub fn preflight_recompile_count(cfg: &Config, paths: &Paths, mutant: &Mutant) -> Result<u64> {
    let original = apply_mutant(&paths.baseline, mutant)?;
    let mut cmd = crate::cargo_cmd::cargo_command();
    cmd.current_dir(&paths.baseline);
    apply_build_env(&mut cmd, cfg);
    // Workspace `cargo build --tests`, not `-p mutant.package`. A source-hashing
    // build script (BIFROST_BUILD_IDENTITY) relinks a *different* crate; scoping
    // to the mutant package misses the cascade this preflight exists to catch.
    cmd.args(["build", "--tests", "-v"]);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let output = cmd.output().context("preflight rebuild")?;
    let _ = restore_file(&paths.baseline, mutant, &original);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(count_compiling_units(&stderr))
}

/// What decides the preflight count: the lock file, every workspace
/// manifest, every build script and the workspace cargo config. A build
/// script that hashes sources is what preflight looks for, so an edit to any
/// of these measures again; an edit to ordinary source does not.
pub fn preflight_key(root: &Path) -> Result<String> {
    use sha2::Digest;
    let meta = crate::cargo_cmd::load_metadata(root)?;
    let mut files: Vec<std::path::PathBuf> = vec![
        root.join("Cargo.lock"),
        root.join(".cargo").join("config.toml"),
    ];
    for pkg in meta.workspace_packages() {
        files.push(pkg.manifest_path.clone().into_std_path_buf());
        for t in &pkg.targets {
            if t.kind.iter().any(|k| k.to_string() == "custom-build") {
                files.push(t.src_path.clone().into_std_path_buf());
            }
        }
    }
    files.sort();
    files.dedup();
    let mut h = sha2::Sha256::new();
    for f in files {
        let rel = f.strip_prefix(root).unwrap_or(&f);
        h.update(crate::util::rel_slash(rel).as_bytes());
        match fs::read(&f) {
            Ok(bytes) => h.update(sha2::Sha256::digest(&bytes)),
            Err(_) => h.update(b"absent"),
        }
    }
    Ok(crate::util::hex(&h.finalize()))
}

/// A preflight count measured earlier in this scratch session for the same
/// [`preflight_key`].
pub fn cached_preflight(paths: &Paths, key: &str) -> Option<u64> {
    let text = fs::read_to_string(paths.index.join("preflight.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    (v.get("key")?.as_str()? == key)
        .then(|| v.get("recompiled")?.as_u64())
        .flatten()
}

pub fn save_preflight(paths: &Paths, key: &str, recompiled: u64) -> Result<()> {
    crate::util::write_atomic(
        &paths.index.join("preflight.json"),
        serde_json::to_vec_pretty(&serde_json::json!({ "key": key, "recompiled": recompiled }))?,
    )
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

/// Sites selection runs every candidate test under the schema binary with no
/// mutant active, so it learns which tests are green for this commit there.
/// From the baseline it needs only test durations, which size the recording
/// timeouts and change little between commits. When an earlier run in this
/// scratch session saved them, sync the baseline and reuse them: no baseline
/// build and no suite run (`cargo nextest list` builds the tests anyway).
/// The earlier failures are dropped, because they describe another commit.
pub fn prepare_baseline_reusing_timings(cfg: &Config, paths: &Paths) -> Result<BaselineReport> {
    if let Some(mut earlier) = load_baseline_report(paths)? {
        if !earlier.tests.is_empty() {
            let _meta = materialize_and_metadata(cfg, paths)?;
            info!(
                "sites selection: reusing {} test durations from an earlier run; no baseline build or suite run",
                earlier.tests.len()
            );
            earlier.failed_test_ids.clear();
            earlier.preflight_recompiled = None;
            return Ok(earlier);
        }
    }
    prepare_baseline(cfg, paths, false, false)
}

/// `measure_costs` times one incremental rebuild per package. Only `estimate`
/// uses those costs; `run` skips them.
pub fn prepare_baseline(
    cfg: &Config,
    paths: &Paths,
    skip: bool,
    measure_costs: bool,
) -> Result<BaselineReport> {
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
            failed_test_ids: Vec::new(),
        });
    }
    build_baseline(cfg, paths)?;
    let (suite_secs, tests, failed_test_ids) = run_green_suite(cfg, paths)?;
    let package_costs = if measure_costs {
        measure_package_costs(cfg, paths)?
    } else {
        Vec::new()
    };
    let report = BaselineReport {
        suite_secs,
        tests,
        package_costs,
        preflight_recompiled: None,
        failed_test_ids,
    };
    write_baseline_report(paths, &report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junit_classname_is_binary_id_not_package() {
        let xml = r#"
<testsuite>
  <testcase name="row_identities_are_deterministic" classname="brokk-bifrost-core" time="0.20"/>
  <testcase name="row_identities_are_deterministic_and_distinct" classname="brokk-bifrost-core" time="0.80"/>
  <testcase name="it_works" classname="tiny_a::integration" time="1.5"/>
</testsuite>
"#;
        let meta = emptyish_metadata();
        let tests = parse_junit(xml, &meta).unwrap();
        assert_eq!(tests[0].binary_id, "brokk-bifrost-core");
        assert_eq!(tests[0].package, "brokk-bifrost-core");
        assert_eq!(
            tests[0].id,
            "brokk-bifrost-core::row_identities_are_deterministic"
        );
        assert_eq!(tests[2].binary_id, "tiny_a::integration");
        assert_eq!(tests[2].package, "tiny_a");
        assert_eq!(tests[2].id, "tiny_a::integration::it_works");
        assert_eq!(tests[2].duration_secs, 1.5);
    }

    #[test]
    fn junit_failures_reads_nested_failure_and_package() {
        let xml = r#"
<testsuite>
  <testcase name="ok" classname="brokk-bifrost-core" time="0.1"/>
  <testcase name="large_rust_file_definition_and_hover_stay_interactive" classname="brokk-bifrost::suite_issues" time="9.3">
    <failure message="took 8s"/>
  </testcase>
</testsuite>
"#;
        let failed = junit_failures(xml);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].package, "brokk-bifrost");
        assert!(failed[0].name.contains("large_rust_file"));
        assert!(failed[0].id.contains("suite_issues"));
        let passed = junit_passed_ids(xml);
        assert!(passed.iter().any(|id| id.ends_with("::ok")));
        assert!(!passed.iter().any(|id| id.contains("large_rust_file")));
    }

    fn emptyish_metadata() -> cargo_metadata::Metadata {
        // parse_junit only uses metadata for package_dir_rel; missing packages
        // yield empty cwd_rel, which is fine for this unit test.
        let raw = r#"{
            "packages": [],
            "workspace_members": [],
            "workspace_default_members": [],
            "resolve": null,
            "target_directory": "/tmp/target",
            "build_directory": "/tmp/target",
            "version": 1,
            "workspace_root": "/tmp",
            "metadata": null
        }"#;
        serde_json::from_str(raw).expect("metadata stub")
    }
}
