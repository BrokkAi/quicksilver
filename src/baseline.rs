use crate::cargo_cmd::{
    apply_build_env, count_compiling_units, dump_metadata_json, nextest_available, BuildResult,
};
use crate::config::Config;
use crate::discover::{apply_mutant, restore_file};
use crate::types::{BaselineFailure, BaselineFailureKind, BaselineReport, Mutant, Paths, TestInfo};
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;
use std::process::Stdio;
use std::time::Instant;
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
    let meta = crate::cargo_cmd::load_metadata_with_features(&paths.baseline, &cfg.cargo_features)?;
    dump_metadata_json(paths, &meta)?;
    Ok(meta)
}

pub fn build_baseline(cfg: &Config, paths: &Paths) -> Result<BuildResult> {
    let target = paths.baseline.join("target");
    info!("baseline cargo build --tests");
    let meta = crate::cargo_cmd::load_metadata_with_features(&paths.baseline, &cfg.cargo_features)?;
    let packages = crate::cargo_cmd::test_packages(cfg, &meta);
    let build = crate::cargo_cmd::build_tests(
        &paths.baseline,
        &target,
        cfg,
        &packages,
        cfg.mutation_target.bin.is_some(),
    )?;
    if cfg.mutation_target.bin.is_some() {
        let outputs = crate::cargo_cmd::parse_all_executables(&build.stdout, &target);
        crate::production::require_selected_binary(cfg, &paths.baseline, &outputs)?;
    }
    Ok(build)
}

pub fn run_green_suite(
    cfg: &Config,
    paths: &Paths,
) -> Result<(f64, Vec<TestInfo>, Vec<BaselineFailure>)> {
    if !nextest_available() {
        bail!("cargo nextest is required. Install it with: cargo install cargo-nextest");
    }
    let config = crate::baseline_env::prepare(cfg, paths)?;
    let meta = crate::cargo_cmd::load_metadata_with_features(&paths.baseline, &cfg.cargo_features)?;
    // Resolve QS globs against concrete IDs instead of approximating their
    // semantics in nextest's DSL. Listing builds/runs no test bodies.
    let catalog = if cfg.skip_tests.is_empty() {
        Vec::new()
    } else {
        crate::index::list_tests_plain(cfg, paths)?
    };
    let filter = baseline_filter(cfg, &meta, &catalog)?;
    let mut last_tests = Vec::new();
    let mut failures = std::collections::BTreeMap::new();
    let mut last_secs = 0.0;
    for run in 0..cfg.baseline_runs.max(1) {
        info!(
            "baseline cargo nextest run ({}/{}, project profile {})",
            run + 1,
            cfg.baseline_runs.max(1),
            cfg.nextest_profile
        );
        let mut cmd = crate::cargo_cmd::cargo_command();
        cmd.current_dir(&paths.baseline);
        apply_build_env(&mut cmd, cfg);
        cmd.args(["nextest", "run"]);
        cfg.cargo_features.apply(&mut cmd);
        cmd.args(["--profile", &cfg.nextest_profile]);
        cmd.arg("--config-file").arg(&config.path);
        crate::cargo_cmd::apply_package_select(
            &mut cmd,
            &crate::cargo_cmd::test_packages(cfg, &meta),
        );
        if let Some(filter) = &filter {
            cmd.arg("-E").arg(filter);
        }
        if !cfg.skip_tests.is_empty() {
            cmd.args(["--no-tests", "pass"]);
        }
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
        // Never mistake a previous invocation's JUnit report for this run's.
        let junit = paths.index.join("baseline-junit.xml");
        if junit.is_file() {
            fs::remove_file(&junit)?;
        }
        let start = Instant::now();
        let status = cmd.status().context("cargo nextest run")?;
        last_secs = start.elapsed().as_secs_f64();
        if junit.is_file() {
            let xml = fs::read_to_string(&junit)?;
            last_tests = parse_junit(&xml, &meta)?;
            let seeds = crate::cargo_cmd::covering_seeds(cfg);
            let failed = junit_failures(&xml);
            let skip = crate::index::skip_globs(&cfg.skip_tests)?;
            let blocking_seeds = if cfg.fail_on_extra_baseline {
                &[][..]
            } else {
                &seeds
            };
            let (seed_fails, skipped) = blocking_failures(&failed, blocking_seeds, skip.as_ref());
            if !seed_fails.is_empty() {
                bail!(
                    "red baseline: {} failing test(s) in {}: {} … Mutation results are meaningless against a failing suite. A test that cannot pass here can be left out with [map] skip-tests in quicksilver.toml.",
                    seed_fails.len(),
                    scope_text(&seeds),
                    seed_fails
                        .iter()
                        .take(5)
                        .map(|f| format!("{:?} {} (nextest profile {})", f.kind, f.id, cfg.nextest_profile))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if skipped > 0 {
                tracing::info!(
                    "{skipped} failing test(s) in the baseline run are left out by [map] skip-tests"
                );
            }
            if failed.len() > skipped {
                tracing::warn!(
                    "{} other workspace test(s) failed (outside {}); keeping timings. first: {}",
                    failed.len() - skipped,
                    scope_text(&seeds),
                    failed[0].id
                );
            }
            if !status.success() && failed.is_empty() {
                bail!("red baseline: nextest failed but JUnit identified no failing tests (profile {})", cfg.nextest_profile);
            }
            for failure in failed {
                if skip.as_ref().is_some_and(|g| g.is_match(&failure.id)) {
                    continue;
                }
                failures
                    .entry(failure.id)
                    .and_modify(|kind| {
                        if failure.kind == BaselineFailureKind::Timeout {
                            *kind = failure.kind;
                        }
                    })
                    .or_insert(failure.kind);
            }
        } else {
            bail!(
                "baseline cargo nextest run produced no JUnit report: {} (status {status})",
                junit.display()
            );
        }
    }
    let json = serde_json::to_vec_pretty(&last_tests)?;
    crate::util::write_atomic(&paths.index.join("tests.json"), json)?;
    Ok((
        last_secs,
        last_tests,
        failures
            .into_iter()
            .map(|(id, kind)| BaselineFailure { id, kind })
            .collect(),
    ))
}

/// Exact-match text in nextest's no-quotes DSL. Escape control and delimiter
/// characters; escaping as a shell or JSON string would change the matcher.
fn exact_match(value: &str) -> String {
    let mut out = String::from("=");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ')' => out.push_str("\\)"),
            ',' => out.push_str("\\,"),
            '/' => out.push_str("\\/"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_whitespace() || c == '(' => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn baseline_filter(
    cfg: &Config,
    meta: &cargo_metadata::Metadata,
    catalog: &[TestInfo],
) -> Result<Option<String>> {
    let selection = crate::cargo_cmd::nextest_filter(cfg, meta);
    let Some(skip) = crate::index::skip_globs(&cfg.skip_tests)? else {
        return Ok(selection);
    };
    let mut by_binary: std::collections::BTreeMap<&str, (usize, Vec<&str>)> = Default::default();
    for test in catalog {
        let (total, names) = by_binary.entry(&test.binary_id).or_default();
        *total += 1;
        if skip.is_match(&test.id) {
            names.push(&test.name);
        }
    }
    let exclusions: Vec<String> = by_binary
        .into_iter()
        .filter_map(|(binary, (total, names))| {
            if names.is_empty() {
                return None;
            }
            let binary = format!("binary_id({})", exact_match(binary));
            if names.len() == total {
                return Some(binary);
            }
            Some(format!(
                "({binary} & ({}))",
                names
                    .iter()
                    .map(|name| format!("test({})", exact_match(name)))
                    .collect::<Vec<_>>()
                    .join(" | ")
            ))
        })
        .collect();
    if exclusions.is_empty() {
        return Ok(selection);
    }
    let omitted = format!("not ({})", exclusions.join(" | "));
    Ok(Some(match selection {
        Some(selection) => format!("({selection}) & {omitted}"),
        None => omitted,
    }))
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
            cpu_secs: None,
        });
    }
    Ok(tests)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JunitFailure {
    pub id: String,
    pub package: String,
    pub name: String,
    pub kind: BaselineFailureKind,
}

/// The baseline failures that stop QS: those in the mutated packages
/// (`seeds`; all of them when empty) that `[map] skip-tests` does not leave
/// out. Also returns how many failures skip-tests left out.
fn blocking_failures<'a>(
    failed: &'a [JunitFailure],
    seeds: &[String],
    skip: Option<&globset::GlobSet>,
) -> (Vec<&'a JunitFailure>, usize) {
    let skipped = |f: &JunitFailure| skip.is_some_and(|g| g.is_match(&f.id));
    let blocking = failed
        .iter()
        .filter(|f| !skipped(f))
        .filter(|f| seeds.is_empty() || seeds.iter().any(|s| s == &f.package))
        .collect();
    (blocking, failed.iter().filter(|f| skipped(f)).count())
}

/// The packages a run is about, as a person reads them.
fn scope_text(seeds: &[String]) -> String {
    if seeds.is_empty() {
        "the workspace".to_string()
    } else {
        seeds.join(", ")
    }
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
                kind: failure_kind(body),
            });
        }
        rest = &rest[tag_end.min(rest.len()) + 1..];
    }
    out
}

fn failure_kind(body: &str) -> BaselineFailureKind {
    for marker in ["<failure", "<error"] {
        if let Some(start) = body.find(marker) {
            let tag = &body[start..body[start..].find('>').map_or(body.len(), |n| start + n)];
            if attr(tag, "type").is_some_and(|t| t.to_ascii_lowercase().contains("timeout")) {
                return BaselineFailureKind::Timeout;
            }
        }
    }
    BaselineFailureKind::Fail
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

/// Mutate one throwaway function, rebuild, count how many units recompiled.
pub fn preflight_recompile_count(cfg: &Config, paths: &Paths, mutant: &Mutant) -> Result<u64> {
    let original = apply_mutant(&paths.baseline, mutant)?;
    let mut cmd = crate::cargo_cmd::cargo_command();
    cmd.current_dir(&paths.baseline);
    apply_build_env(&mut cmd, cfg);
    // Workspace `cargo build --tests`, not `-p mutant.package`. A source-hashing
    // build script can relink a *different* crate; scoping
    // to the mutant package misses the cascade this preflight exists to catch.
    cmd.args(["build", "--tests", "-v"]);
    cfg.cargo_features.apply(&mut cmd);
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
    preflight_key_with_features(root, &Default::default())
}

pub fn preflight_key_with_features(
    root: &Path,
    features: &crate::features::CargoFeatures,
) -> Result<String> {
    use sha2::Digest;
    let meta = crate::cargo_cmd::load_metadata_with_features(root, features)?;
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
    h.update(serde_json::to_vec(features)?);
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
        if !earlier.tests.is_empty()
            && earlier.cargo_features == cfg.cargo_features
            && earlier.mutation_target == cfg.mutation_target
        {
            let _meta = materialize_and_metadata(cfg, paths)?;
            info!(
                "sites selection: reusing {} test durations from an earlier run; no baseline build or suite run",
                earlier.tests.len()
            );
            earlier.failed_test_ids.clear();
            earlier.failures.clear();
            earlier.preflight_recompiled = None;
            return Ok(earlier);
        }
    }
    prepare_baseline(cfg, paths, false)
}

pub fn prepare_baseline(cfg: &Config, paths: &Paths, skip: bool) -> Result<BaselineReport> {
    let _meta = materialize_and_metadata(cfg, paths)?;
    if skip {
        if let Some(existing) = load_baseline_report(paths)?.filter(|r| {
            r.cargo_features == cfg.cargo_features && r.mutation_target == cfg.mutation_target
        }) {
            return Ok(existing);
        }
        return Ok(BaselineReport {
            mutation_target: cfg.mutation_target.clone(),
            cargo_features: cfg.cargo_features.clone(),
            suite_secs: 0.0,
            tests: Vec::new(),
            package_costs: Vec::new(),
            preflight_recompiled: None,
            failed_test_ids: Vec::new(),
            failures: Vec::new(),
        });
    }
    build_baseline(cfg, paths)?;
    let (suite_secs, tests, failures) = run_green_suite(cfg, paths)?;
    let report = BaselineReport {
        mutation_target: cfg.mutation_target.clone(),
        cargo_features: cfg.cargo_features.clone(),
        suite_secs,
        tests,
        package_costs: Vec::new(),
        preflight_recompiled: None,
        failed_test_ids: failures.iter().map(|f| f.id.clone()).collect(),
        failures,
    };
    write_baseline_report(paths, &report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junit_preserves_failure_and_timeout_kinds_without_reading_output_text() {
        let xml = r#"<testsuite>
<testcase name="assertion" classname="pkg::extra"><failure type="test failure">timeout in assertion text</failure></testcase>
<testcase name="slow" classname="pkg::extra"><failure type="test timeout"/></testcase>
<testcase name="killed" classname="pkg::extra"><error type="test timeout"/></testcase>
</testsuite>"#;
        let failures = junit_failures(xml);
        assert_eq!(
            failures.iter().map(|f| f.kind).collect::<Vec<_>>(),
            vec![
                BaselineFailureKind::Fail,
                BaselineFailureKind::Timeout,
                BaselineFailureKind::Timeout
            ]
        );
        let typed: Vec<_> = failures
            .iter()
            .map(|f| BaselineFailure {
                id: f.id.clone(),
                kind: f.kind,
            })
            .collect();
        let json = serde_json::to_value(&typed).unwrap();
        assert_eq!(json[0]["kind"], "FAIL");
        assert_eq!(json[1]["kind"], "TIMEOUT");
        let warning = crate::types::baseline_failure_summary(&typed);
        assert!(warning.contains("FAIL pkg::extra::assertion"));
        assert!(warning.contains("TIMEOUT pkg::extra::slow"));
    }

    #[test]
    fn equality_matchers_escape_nextest_delimiters_without_shell_quotes() {
        assert_eq!(exact_match("a),x\\z\n"), "=a\\)\\,x\\\\z\\n");
        assert_eq!(exact_match("(space here)"), "=\\u{28}space\\u{20}here\\)");
        assert_eq!(exact_match("module::case[0]"), "=module::case[0]");
    }

    #[test]
    fn junit_classname_is_binary_id_not_package() {
        let xml = r#"
<testsuite>
  <testcase name="identities_are_stable" classname="my-core" time="0.20"/>
  <testcase name="identities_are_distinct" classname="my-core" time="0.80"/>
  <testcase name="it_works" classname="tiny_a::integration" time="1.5"/>
</testsuite>
"#;
        let meta = emptyish_metadata();
        let tests = parse_junit(xml, &meta).unwrap();
        assert_eq!(tests[0].binary_id, "my-core");
        assert_eq!(tests[0].package, "my-core");
        assert_eq!(tests[0].id, "my-core::identities_are_stable");
        assert_eq!(tests[2].binary_id, "tiny_a::integration");
        assert_eq!(tests[2].package, "tiny_a");
        assert_eq!(tests[2].id, "tiny_a::integration::it_works");
        assert_eq!(tests[2].duration_secs, 1.5);
    }

    #[test]
    fn skipped_tests_do_not_make_the_baseline_red() {
        let fail = |id: &str, package: &str| JunitFailure {
            id: id.into(),
            package: package.into(),
            name: id.rsplit("::").next().unwrap().into(),
            kind: BaselineFailureKind::Fail,
        };
        let failed = vec![
            fail("my-core::flaky_ctime_order", "my-core"),
            fail("my-app::suite::large_input_completes_in_time", "my-app"),
            fail("other::broken", "other"),
        ];
        let skip = crate::index::skip_globs(&["my-core::flaky_*".to_string()])
            .unwrap()
            .unwrap();
        // The whole workspace: the skipped test no longer counts.
        let (blocking, skipped) = blocking_failures(&failed, &[], Some(&skip));
        assert_eq!(skipped, 1);
        assert_eq!(blocking.len(), 2);
        // Only my-core's own tests count, and its failure is skipped.
        let (blocking, _) = blocking_failures(&failed, &["my-core".to_string()], Some(&skip));
        assert!(blocking.is_empty());
        // Without skip-tests it would stop the map.
        let (blocking, skipped) = blocking_failures(&failed, &["my-core".to_string()], None);
        assert_eq!((blocking.len(), skipped), (1, 0));
        assert_eq!(scope_text(&[]), "the workspace");
        assert_eq!(scope_text(&["a".into(), "b".into()]), "a, b");
    }

    #[test]
    fn junit_failures_reads_nested_failure_and_package() {
        let xml = r#"
<testsuite>
  <testcase name="ok" classname="my-core" time="0.1"/>
  <testcase name="large_input_completes_in_time" classname="my-app::suite" time="9.3">
    <failure message="took 8s"/>
  </testcase>
</testsuite>
"#;
        let failed = junit_failures(xml);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].package, "my-app");
        assert!(failed[0].name.contains("large_input"));
        assert!(failed[0].id.contains("suite"));
        let passed = junit_passed_ids(xml);
        assert!(passed.iter().any(|id| id.ends_with("::ok")));
        assert!(!passed.iter().any(|id| id.contains("large_input")));
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
