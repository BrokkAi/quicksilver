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
