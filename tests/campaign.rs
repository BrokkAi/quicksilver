mod support;

use cargo_quicksilver::{capture::EditedMutant, exec::edited::State, sitemap::SiteMap};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn git(root: &Path, args: &[&str]) {
    assert!(Command::new("git")
        .args(args)
        .current_dir(root)
        .status()
        .unwrap()
        .success());
}
fn qs(root: &Path, args: &[&str], code: i32) -> Output {
    let out = Command::new(env!("CARGO_BIN_EXE_cargo-quicksilver"))
        .current_dir(root)
        .args(["quicksilver", "-p", "tiny_a", "--extra-tests", "tiny_b"])
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(code),
        "{args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}
fn json(root: &Path, name: &str) -> serde_json::Value {
    serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap()
}

/// Real nextest profile inheritance and timeout override; typed coverage
/// exclusions; saved-state/report carry-forward and new regression kills.
#[test]
fn campaign_carries_mutants_and_reports_baseline_coverage_loss() {
    assert!(
        cargo_quicksilver::cargo_cmd::nextest_available(),
        "nextest required"
    );
    let repo = support::setup_tiny();
    let root = repo.path();
    fs::write(root.join(".gitignore"), "*.json\nreview/\n").unwrap();
    fs::create_dir(root.join(".config")).unwrap();
    fs::write(
        root.join(".config/nextest.toml"),
        r#"
[store]
dir = "target/project-nextest-store"
[profile.default]
slow-timeout = { period = "5s", terminate-after = 2 }
[profile.campaign]
inherits = "default"
default-filter = 'not test(profile_omitted)'
[[profile.campaign.overrides]]
filter = 'test(baseline_timeout)'
slow-timeout = { period = "50ms", terminate-after = 1, grace-period = "0s" }
[[profile.campaign.overrides]]
filter = 'test(baseline_slow)'
slow-timeout = { period = "10s", terminate-after = 1 }
"#,
    )
    .unwrap();
    fs::write(
        root.join("quicksilver.toml"),
        "[map]\nnextest-profile = 'campaign'\n",
    )
    .unwrap();
    let extra = root.join("crate_b/src/lib.rs");
    let mut code = fs::read_to_string(&extra).unwrap();
    code.push_str(
        r#"
#[cfg(test)] mod campaign_extra {
    #[test] fn profile_omitted() { panic!("project profile excludes this test"); }
    #[test] fn baseline_fail() { panic!("unrelated original failure"); }
    #[test] fn baseline_timeout() { std::thread::sleep(std::time::Duration::from_secs(3)); }
    #[test] fn baseline_slow() {
        std::thread::sleep(std::time::Duration::from_millis(150));
        assert!(tiny_a::flag());
    }
}
"#,
    );
    fs::write(&extra, code).unwrap();
    git(
        root,
        &[
            "add",
            ".gitignore",
            ".config/nextest.toml",
            "quicksilver.toml",
            "crate_b/src/lib.rs",
        ],
    );
    git(root, &["commit", "-m", "project baseline policy"]);
    let mapped = qs(root, &["map", "--json", "map-old.json"], 0);
    let warnings = String::from_utf8_lossy(&mapped.stderr);
    assert!(warnings.contains("coverage loss: 2 test(s)"), "{warnings}");
    assert!(
        warnings.contains("FAIL tiny_b::campaign_extra::baseline_fail"),
        "{warnings}"
    );
    assert!(
        warnings.contains("TIMEOUT tiny_b::campaign_extra::baseline_timeout"),
        "{warnings}"
    );
    let map: SiteMap = serde_json::from_value(json(root, "map-old.json")).unwrap();
    assert_eq!(map.baseline_failures.len(), 2);
    assert!(!map
        .tests
        .iter()
        .any(|t| t.name.ends_with("profile_omitted")));
    assert!(map.tests.iter().any(|t| t.name.ends_with("baseline_slow")));
    for f in &map.baseline_failures {
        assert!(!map.tests.iter().any(|t| t.id == f.id));
        assert!(!map.hits.contains_key(&f.id));
        assert!(!map.not_green.iter().any(|(id, _)| id == &f.id));
    }
    let lib = root.join("crate_a/src/lib.rs");
    let original = fs::read_to_string(&lib).unwrap();
    let edited = original
        .replace("a + b\n", "a + b + if a == 3 { 1 } else { 0 }\n")
        .replace(
            "pub fn flag() -> bool {\n    true",
            "pub fn flag() -> bool {\n    false",
        );
    assert_ne!(edited, original);
    fs::write(&lib, edited).unwrap();
    qs(root, &["build", "--json", "build-old.json"], 0);
    fs::write(&lib, &original).unwrap();
    let tested = qs(root, &["test", "--json", "test-old.json"], 0);
    assert!(String::from_utf8_lossy(&tested.stderr).contains("coverage loss: 2 test(s)"));
    let old = json(root, "test-old.json");
    assert_eq!(old["baseline_failures"].as_array().unwrap().len(), 2);
    let old_rows = old["mutants"].as_array().unwrap();
    assert_eq!(old_rows.len(), 2);
    assert!(old_rows.iter().any(|m| m["verdict"]["kind"] == "missed"));
    let survivors = qs(
        root,
        &[
            "survivors",
            "--json",
            "survivors.json",
            "--split",
            "1",
            "--out",
            "review",
        ],
        0,
    );
    assert!(String::from_utf8_lossy(&survivors.stderr).contains("coverage loss: 2 test(s)"));
    assert!(fs::read_to_string(root.join("review/survivors-1.txt"))
        .unwrap()
        .contains("TIMEOUT tiny_b"));
    assert_eq!(
        json(root, "survivors.json")[0]["baseline_failures"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let newer = format!("// unrelated line shift\n{original}\n#[cfg(test)] mod regression {{\n#[test] fn catches_three() {{ assert_eq!(super::add(3,2),5); }}\n}}\n");
    fs::write(&lib, &newer).unwrap();
    git(root, &["add", "crate_a/src/lib.rs"]);
    git(root, &["commit", "-m", "add missing assertion"]);
    qs(root, &["map", "--json", "map-new.json"], 0);
    qs(
        root,
        &[
            "build",
            "--from",
            "test-old.json",
            "--json",
            "build-new.json",
        ],
        0,
    );
    let built: State = serde_json::from_value(json(root, "build-new.json")).unwrap();
    assert_eq!(built.records.len(), 2);
    assert!(built.records.iter().all(|r| matches!(
        r.status,
        cargo_quicksilver::exec::edited::Status::Built { .. }
    )));
    for r in &built.records {
        assert!(old_rows
            .iter()
            .any(|m| m["stable_id"] == r.mutant.stable_id()));
    }
    assert_eq!(
        fs::read_to_string(&lib).unwrap(),
        newer,
        "checkout unchanged by carry-forward"
    );
    qs(root, &["test", "--json", "test-new.json"], 0);
    assert!(json(root, "test-new.json")["mutants"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["verdict"]["kind"] == "caught"));
    // Saved state is accepted too; importing it twice doesn't duplicate mutants.
    qs(root, &["build", "--from", "build-old.json"], 0);
    let cfg = support::cfg_for(root);
    assert_eq!(State::load(&cfg.paths()).unwrap().records.len(), 2);
    let current: SiteMap = serde_json::from_value(json(root, "map-new.json")).unwrap();
    let mut drifted = old.clone();
    drifted["mutants"][0]["edits"][0]["original"] = "{ drifted }".into();
    fs::write(
        root.join("drifted.json"),
        serde_json::to_vec(&drifted).unwrap(),
    )
    .unwrap();
    let imported =
        cargo_quicksilver::carry::capture_from(&current, root, &root.join("drifted.json")).unwrap();
    assert_eq!(imported.mutants.len(), 1);
    assert_eq!(imported.dropped.len(), 1);
    let dropped = qs(
        root,
        &["build", "--from", "drifted.json", "--json", "partial.json"],
        2,
    );
    assert!(String::from_utf8_lossy(&dropped.stderr).contains("original body changed"));
    assert_eq!(
        json(root, "partial.json")["import_rejected"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Old exports that predate stable_id still compute the same identity.
    let one: EditedMutant =
        serde_json::from_value(json(root, "build-old.json")["records"][0]["mutant"].clone())
            .unwrap();
    assert!(old_rows.iter().any(|m| m["stable_id"] == one.stable_id()));
    // A project can choose to reject failures of its extra test suites.
    let mut strict = support::cfg_for(root);
    strict.packages = vec!["tiny_a".into()];
    strict.extra_tests = vec!["tiny_b".into()];
    strict.nextest_profile = "campaign".into();
    strict.fail_on_extra_baseline = true;
    let error =
        cargo_quicksilver::baseline::prepare_baseline(&strict, &strict.paths(), false).unwrap_err();
    assert!(error.to_string().contains("red baseline"), "{error:#}");
}

/// Skips are execution policy, not merely ignored failures. Same-named
/// tests in another binary remain eligible, and empty selections are valid.
#[test]
fn baseline_skip_globs_prevent_execution_in_seed_and_extra_binaries() {
    let repo = support::setup_tiny();
    let root = repo.path();
    fs::write(root.join(".gitignore"), "*.json\nmarker-*\n").unwrap();
    let seed = root.join("crate_a/src/lib.rs");
    let extra = root.join("crate_b/src/lib.rs");
    let seed_marker = root.join("marker-seed");
    let extra_marker = root.join("marker-extra");
    let green_marker = root.join("marker-green");
    let mut seed_source = fs::read_to_string(&seed).unwrap();
    seed_source.push_str(&format!(
        r#"
#[cfg(test)] mod baseline_policy {{
    #[test] fn skip_me() {{
        std::fs::write({:?}, "executed").unwrap();
        panic!("skipped seed test must never run");
    }}
}}
"#,
        seed_marker
    ));
    fs::write(&seed, &seed_source).unwrap();
    let mut extra_source = fs::read_to_string(&extra).unwrap();
    extra_source.push_str(&format!(
        r#"
#[cfg(test)] mod baseline_policy {{
    #[test] fn skip_me() {{ std::fs::write({:?}, "executed").unwrap(); }}
    #[test] fn skipped_slow_test() {{
        std::fs::write({:?}, "executed").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(10));
        panic!("skipped extra test must never run");
    }}
}}
"#,
        green_marker, extra_marker
    ));
    fs::write(&extra, extra_source).unwrap();
    let patterns = [
        "tiny_a::baseline_policy::skip_*",
        "tiny_b::baseline_policy::skipped_slow_*",
    ];
    fs::write(
        root.join("quicksilver.toml"),
        format!("[map]\nskip-tests = {patterns:?}\n"),
    )
    .unwrap();
    git(
        root,
        &[
            "add",
            ".gitignore",
            "crate_a/src/lib.rs",
            "crate_b/src/lib.rs",
            "quicksilver.toml",
        ],
    );
    git(
        root,
        &["commit", "-m", "tests must not execute when skipped"],
    );
    qs(root, &["map", "--json", "skip-map.json"], 0);
    assert!(!seed_marker.exists());
    assert!(!extra_marker.exists());
    assert!(
        green_marker.exists(),
        "same test name in a different binary still runs"
    );
    let map: SiteMap = serde_json::from_value(json(root, "skip-map.json")).unwrap();
    assert_eq!(map.skipped_tests.len(), 2);
    assert!(map.baseline_failures.is_empty());
    let cfg = support::cfg_for(root);
    let junit = fs::read_to_string(cfg.paths().index.join("baseline-junit.xml")).unwrap();
    assert!(!junit.contains("skipped_slow_test"));
    assert!(!junit.contains("classname=\"tiny_a\" name=\"baseline_policy::skip_me\""));
    // Workspace-wide baseline uses the same policy, even without --package.
    let mut workspace = cfg.clone();
    workspace.skip_tests = patterns.iter().map(|s| s.to_string()).collect();
    let baseline =
        cargo_quicksilver::baseline::prepare_baseline(&workspace, &workspace.paths(), false)
            .unwrap();
    assert!(!baseline
        .tests
        .iter()
        .any(|t| t.id == "tiny_a::baseline_policy::skip_me"));
    assert!(!baseline
        .tests
        .iter()
        .any(|t| t.name.ends_with("skipped_slow_test")));
    assert!(!seed_marker.exists());
    assert!(!extra_marker.exists());
    // All tests skipped is an intentional empty baseline, not a red suite.
    workspace.packages = vec!["tiny_a".into()];
    workspace.skip_tests = vec!["tiny_a::*".into()];
    let empty =
        cargo_quicksilver::baseline::prepare_baseline(&workspace, &workspace.paths(), false)
            .unwrap();
    assert!(empty.tests.is_empty());
    // Policy must not suppress an unskipped failure in the mutated package.
    seed_source.push_str("\n#[test] fn unskipped_bad() { panic!(\"must block map\"); }\n");
    fs::write(&seed, seed_source).unwrap();
    git(root, &["add", "crate_a/src/lib.rs"]);
    git(
        root,
        &["commit", "-m", "unskipped failure remains blocking"],
    );
    let blocked = qs(root, &["map"], 2);
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("unskipped_bad"));
    assert!(!seed_marker.exists());
    assert!(!extra_marker.exists());
}

#[cfg(unix)]
#[test]
fn artifact_copy_failure_keeps_captured_state_for_same_state_retry() {
    use cargo_quicksilver::{
        capture::CaptureOptions,
        exec::edited::{build, build_map, CaptureSources, Status},
    };
    use std::os::unix::fs::PermissionsExt;
    let repo = support::setup_tiny();
    let root = repo.path();
    let mut cfg = support::cfg_for(root);
    cfg.packages = vec!["tiny_a".into()];
    let paths = cfg.paths();
    build_map(&cfg, &paths).unwrap();
    let original = fs::read_to_string(root.join("crate_a/src/lib.rs")).unwrap();
    fs::write(
        root.join("crate_a/src/lib.rs"),
        original.replace(
            "pub fn flag() -> bool {\n    true",
            "pub fn flag() -> bool {\n    false",
        ),
    )
    .unwrap();
    let fake = tempfile::tempdir().unwrap();
    let cargo = fake.path().join("fake-cargo");
    let missing = fake.path().join("missing-executable");
    let event = serde_json::json!({"reason":"compiler-artifact", "profile":{"test":true},
        "executable":missing, "manifest_path":root.join("crate_a/Cargo.toml")});
    fs::write(&cargo, format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", event)).unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
    // A test double for the mbx command, not a global config/cache change.
    let mbx = fake.path().join("mbx");
    fs::write(
        &mbx,
        format!(
            "#!/bin/sh\nexport CARGO='{}'\nexec cargo-quicksilver \"$@\"\n",
            cargo.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&mbx, fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = vec![fake.path().to_path_buf()];
    path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    cfg.build_env.insert(
        "PATH".into(),
        std::env::join_paths(path)
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    let sources = CaptureSources {
        checkout: Some(root.into()),
        options: CaptureOptions::default(),
        intents: [("tiny_a.flag".into(), "missing condition".into())]
            .into_iter()
            .collect(),
        ..Default::default()
    };
    let error = build(&cfg, &paths, &sources).err().expect("copy fails");
    assert!(
        format!("{error:#}").contains("stage protected Cargo artifact"),
        "{error:#}"
    );
    let saved = State::load(&paths).unwrap();
    assert_eq!(saved.records.len(), 1);
    assert_eq!(saved.records[0].status, Status::Pending);
    assert_eq!(
        saved.records[0].mutant.intent.as_deref(),
        Some("missing condition")
    );
    let id = saved.records[0].mutant.id.clone();
    let diagnostic = fs::read_to_string(paths.index.join("last-staging-failure.json")).unwrap();
    assert!(diagnostic.contains("compiler-artifact"));
    assert!(diagnostic.contains("missing-executable"));
    assert!(
        fs::read_dir(paths.runs.join("artifacts"))
            .unwrap()
            .next()
            .is_none(),
        "failed snapshots cleaned up"
    );
    fs::write(root.join("crate_a/src/lib.rs"), &original).unwrap();
    cfg.build_env.clear();
    let retry = build(&cfg, &paths, &CaptureSources::default()).unwrap();
    assert!(retry.captured.is_empty(), "retry does not recapture");
    assert_eq!(retry.results.len(), 1);
    let retried = State::load(&paths).unwrap();
    assert_eq!(retried.records[0].mutant.id, id);
    assert!(matches!(retried.records[0].status, Status::Built { .. }));
}
