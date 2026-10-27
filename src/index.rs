//! The test catalog: `cargo nextest list` for the baseline, and the green
//! suite's durations attached to it.

use crate::cargo_cmd::{
    apply_build_env, apply_package_select, load_metadata, package_dir_rel, relative_to_target,
    test_packages,
};
use crate::config::Config;
use crate::types::{Paths, TestInfo};
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::Path;

/// Every test of the baseline, from `cargo nextest list` (through mbx, which
/// builds the tests if they are not built yet).
/// `[map] skip-tests` as a glob set over test ids.
pub fn skip_globs(patterns: &[String]) -> Result<Option<globset::GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut set = globset::GlobSetBuilder::new();
    for p in patterns {
        set.add(globset::Glob::new(p).with_context(|| format!("[map] skip-tests: {p}"))?);
    }
    Ok(Some(set.build()?))
}

/// Remove the tests `[map] skip-tests` names from `tests`, and return their
/// ids, sorted.
pub fn drop_skipped(cfg: &Config, tests: &mut Vec<TestInfo>) -> Result<Vec<String>> {
    let Some(globs) = skip_globs(&cfg.skip_tests)? else {
        return Ok(Vec::new());
    };
    let mut skipped = Vec::new();
    tests.retain(|t| {
        let skip = globs.is_match(&t.id);
        if skip {
            skipped.push(t.id.clone());
        }
        !skip
    });
    skipped.sort();
    Ok(skipped)
}

pub fn list_tests_plain(cfg: &Config, paths: &Paths) -> Result<Vec<TestInfo>> {
    let meta = load_metadata(&paths.baseline)?;
    let target_dir = paths.baseline.join("target");
    let mut cmd = crate::cargo_cmd::cargo_command();
    cmd.current_dir(&paths.baseline);
    apply_build_env(&mut cmd, cfg);
    cmd.args(["nextest", "list", "--message-format", "json"]);
    apply_package_select(&mut cmd, &test_packages(cfg, &meta));
    let out = cmd.output().context("cargo nextest list")?;
    if !out.status.success() {
        bail!(
            "cargo nextest list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut tests = parse_nextest_list(&out.stdout, &meta, &target_dir)?;
    tests.retain(|t| crate::cargo_cmd::keeps_test(cfg, &meta, t));
    for binary in &cfg.extra_tests {
        if !tests.iter().any(|t| t.binary_id == *binary) {
            bail!(
                "--extra-tests {binary}: no such test binary (nextest names binaries `package` or `package::target`)"
            );
        }
    }
    Ok(tests)
}

pub fn parse_nextest_list(
    bytes: &[u8],
    meta: &cargo_metadata::Metadata,
    target_dir: &Path,
) -> Result<Vec<TestInfo>> {
    let v: serde_json::Value = serde_json::from_slice(bytes).context("nextest list json")?;
    let suites = v
        .get("rust-suites")
        .or_else(|| v.get("rust-binaries"))
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));
    let mut tests = Vec::new();
    let Some(map) = suites.as_object() else {
        return Ok(tests);
    };
    for (_id, suite) in map {
        let package = suite
            .get("package-name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let binary_id = suite
            .get("binary-id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let binary_name = suite
            .get("binary-name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let binary_path = suite
            .get("binary-path")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let binary_rel = relative_to_target(Path::new(binary_path), target_dir);
        let cwd_rel = package_dir_rel(meta, &package);
        let cases = suite
            .get("testcases")
            .or_else(|| suite.get("test-cases"))
            .cloned()
            .unwrap_or(serde_json::Value::Object(Default::default()));
        if let Some(obj) = cases.as_object() {
            for (name, spec) in obj {
                let kind = spec.get("kind").and_then(|k| k.as_str()).unwrap_or("test");
                if kind == "bench" {
                    continue;
                }
                let ignored = spec
                    .get("ignored")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                if ignored {
                    continue;
                }
                tests.push(TestInfo {
                    id: format!("{binary_id}::{name}"),
                    name: name.clone(),
                    package: package.clone(),
                    binary_id: binary_id.clone(),
                    binary_name: binary_name.clone(),
                    binary_rel: binary_rel.clone(),
                    cwd_rel: cwd_rel.clone(),
                    duration_secs: 0.001,
                    cpu_secs: None,
                });
            }
        }
    }
    Ok(tests)
}

#[derive(Debug, Clone, Copy)]
pub struct TimingMatch {
    pub matched: usize,
    pub total: usize,
}

/// Overlay baseline timings onto listed tests by (binary_id, name).
/// Bare test name is ambiguous across binaries and must not be the key.
pub fn attach_timings(tests: &mut [TestInfo], baseline_tests: &[TestInfo]) -> TimingMatch {
    let mut by_bin_name: HashMap<(String, String), f64> = HashMap::new();
    let mut by_id: HashMap<String, f64> = HashMap::new();
    for t in baseline_tests {
        by_bin_name.insert((t.binary_id.clone(), t.name.clone()), t.duration_secs);
        by_id.insert(t.id.clone(), t.duration_secs);
    }
    let mut matched = 0;
    for t in tests.iter_mut() {
        if let Some(d) = by_bin_name
            .get(&(t.binary_id.clone(), t.name.clone()))
            .copied()
            .or_else(|| by_id.get(&t.id).copied())
        {
            t.duration_secs = d;
            matched += 1;
        }
    }
    TimingMatch {
        matched,
        total: tests.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn testinfo(id: &str, binary_id: &str, name: &str, dur: f64) -> TestInfo {
        TestInfo {
            id: id.into(),
            name: name.into(),
            package: "pkg".into(),
            binary_id: binary_id.into(),
            binary_name: "pkg".into(),
            binary_rel: "deps/pkg".into(),
            cwd_rel: "pkg".into(),
            duration_secs: dur,
            cpu_secs: None,
        }
    }

    #[test]
    fn attach_timings_matches_binary_id_and_name_not_bare_name() {
        let mut tests = vec![
            testinfo("pkg::bin_a::same", "pkg::bin_a", "same", 0.001),
            testinfo("pkg::bin_b::same", "pkg::bin_b", "same", 0.001),
        ];
        let baseline = vec![
            testinfo("pkg::bin_a::same", "pkg::bin_a", "same", 1.25),
            testinfo("pkg::bin_b::same", "pkg::bin_b", "same", 4.5),
        ];
        let m = attach_timings(&mut tests, &baseline);
        assert_eq!(m.matched, 2);
        assert_eq!(tests[0].duration_secs, 1.25);
        assert_eq!(tests[1].duration_secs, 4.5);
    }

    #[test]
    fn attach_timings_does_not_steal_by_prefix_name() {
        let mut tests = vec![
            testinfo(
                "pkg::row_identities_are_deterministic",
                "pkg",
                "row_identities_are_deterministic",
                0.001,
            ),
            testinfo(
                "pkg::row_identities_are_deterministic_and_distinct",
                "pkg",
                "row_identities_are_deterministic_and_distinct",
                0.001,
            ),
        ];
        let baseline = vec![
            testinfo(
                "pkg::row_identities_are_deterministic",
                "pkg",
                "row_identities_are_deterministic",
                0.2,
            ),
            testinfo(
                "pkg::row_identities_are_deterministic_and_distinct",
                "pkg",
                "row_identities_are_deterministic_and_distinct",
                0.8,
            ),
        ];
        attach_timings(&mut tests, &baseline);
        assert_eq!(tests[0].duration_secs, 0.2);
        assert_eq!(tests[1].duration_secs, 0.8);
    }
}

#[cfg(test)]
mod skip_tests {
    use super::*;

    #[test]
    fn skip_tests_match_whole_ids_with_globs() {
        let globs = skip_globs(&[
            "my-app::suite::*::c_wide_*_in_linear_time".to_string(),
            "p::exact::name".to_string(),
        ])
        .unwrap()
        .unwrap();
        assert!(globs.is_match(
            "my-app::suite::wide_tree_traversal::c_wide_translation_unit_indexes_in_linear_time"
        ));
        assert!(globs.is_match("p::exact::name"));
        assert!(!globs.is_match("p::exact::name_too"));
        assert!(!globs.is_match("q::p::exact::name"));
        assert!(skip_globs(&[]).unwrap().is_none());
        assert!(skip_globs(&["a[".to_string()]).is_err());
    }
}
