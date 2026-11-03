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
    let paths = support::cfg_for(root).paths();
    assert!(
        fs::read_dir(paths.runs.join("artifacts"))
            .unwrap()
            .next()
            .is_none(),
        "map releases temporary snapshots after final staging"
    );
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
    assert!(
        fs::read_dir(paths.runs.join("artifacts"))
            .unwrap()
            .next()
            .is_none(),
        "build releases temporary snapshots before test execution"
    );
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

/// Repeated same-file patch sections, real v-parameter execution, and the
/// full existing-catalog recovery (including note repair and a new variant).
#[test]
fn hygienic_variants_keep_inputs_and_rebuild_all_statuses_without_remapping() {
    use cargo_quicksilver::{
        capture::{capture_patch, CaptureOptions},
        exec::edited::Status,
    };
    let repo = support::setup_tiny();
    let root = repo.path();
    fs::write(root.join(".gitignore"), "*.json\n").unwrap();
    let lib = root.join("crate_a/src/lib.rs");
    let mut original = fs::read_to_string(&lib).unwrap();
    original.push_str(
        r#"
#[derive(Clone, Copy, Debug)]
pub struct Reading { pub nodes: u32, pub edges: u32, pub digest: u32 }
pub struct Limits { pub max: u32 }
impl From<Reading> for Limits {
    fn from(v: Reading) -> Self { Self { max: v.nodes } }
}
pub fn bump(v: u32) -> u32 {
    v + 1
}
fn serialize<T: std::fmt::Debug>(value: &T) -> String { format!("{value:?}") }
pub fn result_digest(v: &Reading) -> String {
    let normalized = Reading { digest: 0, ..*v };
    serialize(&normalized)
}
pub fn malformed() -> u32 { 7 }
#[cfg(test)] mod argument_tests {
    use super::*;
    // Both the baseline and the intended mutant are allowed. A selector-
    // based result is not: this distinguishes a real gap from a false kill.
    #[test] fn struct_input_is_retained() {
        assert!([41,42].contains(&Limits::from(Reading { nodes:41, edges:42, digest:9 }).max));
    }
    #[test] fn scalar_input_is_retained() { assert!([41,42].contains(&bump(40))); }
    #[test] fn generic_serializer_receives_the_record() {
        let actual = result_digest(&Reading { nodes:42, edges:43, digest:9 });
        assert!(actual.contains("nodes: 42") && actual.contains("edges: 43") &&
            (actual.contains("digest: 0") || actual.contains("digest: 9")), "{actual}");
    }
    #[test] fn malformed_baseline() { assert!([7,8].contains(&malformed())); }
}
"#,
    );
    fs::write(&lib, &original).unwrap();
    git(root, &["add", ".gitignore", "crate_a/src/lib.rs"]);
    git(root, &["commit", "-m", "argument hygiene regression"]);
    qs(root, &["map", "--json", "hygiene-map.json"], 0);
    let map: SiteMap = serde_json::from_value(json(root, "hygiene-map.json")).unwrap();
    let paths = support::cfg_for(root).paths();
    let original_map = fs::read(SiteMap::path(&paths)).unwrap();
    let archive = tempfile::tempdir().unwrap();
    let patch = |from: &str, to: &str| {
        assert!(original.contains(from));
        fs::write(&lib, original.replacen(from, to, 1)).unwrap();
        let out = Command::new("git")
            .current_dir(root)
            .args(["diff", "--unified=1", "--", "crate_a/src/lib.rs"])
            .output()
            .unwrap();
        assert!(out.status.success());
        fs::write(&lib, &original).unwrap();
        String::from_utf8(out.stdout).unwrap()
    };
    let round_a = patch("Self { max: v.nodes }", "Self { max: v.edges }")
        + &patch("    v + 1\n", "    v + 2\n");
    let round_b = patch(
        "let normalized = Reading { digest: 0, ..*v };",
        "let normalized = v;",
    ) + &patch(
        "pub fn malformed() -> u32 { 7 }",
        "pub fn malformed() -> u32 { \"bad\" }",
    );
    let repair = patch(
        "pub fn malformed() -> u32 { 7 }",
        "pub fn malformed() -> u32 { 8 }",
    );
    let a = archive.path().join("round-a.patch");
    let b = archive.path().join("round-b.patch");
    let repaired = archive.path().join("repaired.patch");
    fs::write(&a, &round_a).unwrap();
    fs::write(&b, &round_b).unwrap();
    fs::write(&repaired, repair).unwrap();
    assert_eq!(round_a.matches("--- a/crate_a/src/lib.rs").count(), 2);
    for path in [&a, &b] {
        let captured = capture_patch(&map, root, path, CaptureOptions::default()).unwrap();
        assert!(captured.problems.is_empty(), "{:?}", captured.problems);
        assert_eq!(
            captured.mutants.len(),
            2,
            "a final patched file is captured once, not once per header"
        );
    }
    let notes_a = archive.path().join("notes-a.json");
    let notes_b = archive.path().join("notes-b.json");
    let notes_repair = archive.path().join("notes-repair.json");
    fs::write(&notes_a,serde_json::to_vec(&serde_json::json!({
        "tiny_a.Limits.from":"uses edges instead of nodes", "tiny_a.bump":"adds two instead of one"})).unwrap()).unwrap();
    fs::write(&notes_b,serde_json::to_vec(&serde_json::json!({
        "tiny_a.result_digest":"includes the stored digest", "tiny_a.malformed":"genuinely wrong return type"})).unwrap()).unwrap();
    fs::write(
        &notes_repair,
        serde_json::to_vec(&serde_json::json!({"tiny_a.malformed":"repaired candidate body"}))
            .unwrap(),
    )
    .unwrap();
    let source_a = format!("{}={}", a.display(), notes_a.display());
    let source_b = format!("{}={}", b.display(), notes_b.display());
    let source_repair = format!("{}={}", repaired.display(), notes_repair.display());
    let first_out = qs(
        root,
        &[
            "build",
            "--patch",
            &source_a,
            "--patch",
            &source_b,
            "--verbose-intents",
            "--json",
            "first-hygiene.json",
        ],
        1,
    );
    assert!(!String::from_utf8_lossy(&first_out.stderr).contains("matched 2 mutants"));
    let first: State = serde_json::from_value(json(root, "first-hygiene.json")).unwrap();
    assert_eq!(first.records.len(), 4);
    assert_eq!(
        first
            .records
            .iter()
            .filter(|r| matches!(r.status, Status::Built { .. }))
            .count(),
        3,
        "{}",
        json(root, "first-hygiene.json")
    );
    assert_eq!(
        first
            .records
            .iter()
            .filter(|r| matches!(r.status, Status::Unviable { .. }))
            .count(),
        1
    );
    assert!(first.records.iter().all(|r| r.mutant.intent.is_some()));
    qs(root, &["test", "--json", "first-hygiene-verdicts.json"], 0);
    let verdicts = json(root, "first-hygiene-verdicts.json");
    let rows = verdicts["mutants"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter().all(|m| m["verdict"]["kind"] == "missed"),
        "{verdicts:#}"
    );

    // Emulate the actual old catalog: false Unviable, old Built, old Tested,
    // genuine malformed Unviable, and lost notes. Only fixture state is seeded.
    let mut legacy = State::load(&paths).unwrap();
    for record in &mut legacy.records {
        record.mutant.intent = None;
        if record.mutant.edits[0].fqn.ends_with("Limits.from") {
            record.status = Status::Unviable {
                message: "old generated v shadow caused E0610".into(),
            };
        } else if record.mutant.edits[0].fqn.ends_with(".bump") {
            record.status = first.find(&record.mutant.id).unwrap().status.clone();
        }
    }
    legacy.save(&paths).unwrap();
    let saved_state = fs::read(State::path(&paths)).unwrap();
    let bad_patch = archive.path().join("invalid.patch");
    fs::write(&bad_patch, "not a patch").unwrap();
    qs(
        root,
        &["build", "--rebuild", "--patch", bad_patch.to_str().unwrap()],
        2,
    );
    assert_eq!(
        fs::read(State::path(&paths)).unwrap(),
        saved_state,
        "rejected inputs don't clear existing statuses"
    );

    let recovered_out = qs(
        root,
        &[
            "build",
            "--rebuild",
            "--patch",
            &source_a,
            "--patch",
            &source_b,
            "--patch",
            &source_repair,
            "--json",
            "recovered-hygiene.json",
        ],
        1,
    );
    assert!(!String::from_utf8_lossy(&recovered_out.stderr).contains("matched 2 mutants"));
    let recovered: State = serde_json::from_value(json(root, "recovered-hygiene.json")).unwrap();
    assert_eq!(recovered.records.len(), 5);
    assert!(recovered.generation > first.generation);
    for old in &first.records {
        let kept = recovered.find(&old.mutant.id).unwrap();
        assert_eq!(
            kept.mutant, old.mutant,
            "full names, bodies, IDs and per-patch notes preserved"
        );
        assert_eq!(kept.captured_at, old.captured_at);
        if old.mutant.edits[0].fqn.ends_with(".malformed") {
            assert!(matches!(kept.status, Status::Unviable { .. }));
        } else {
            assert!(matches!(kept.status, Status::Built { .. }));
        }
    }
    let repaired = recovered
        .records
        .iter()
        .find(|r| first.find(&r.mutant.id).is_none())
        .unwrap();
    assert_eq!(
        repaired.mutant.intent.as_deref(),
        Some("repaired candidate body")
    );
    assert!(matches!(repaired.status, Status::Built { .. }));
    assert_eq!(fs::read_to_string(&lib).unwrap(), original);
    assert_eq!(
        fs::read(SiteMap::path(&paths)).unwrap(),
        original_map,
        "map is reused unchanged"
    );
    qs(
        root,
        &["test", "--json", "recovered-hygiene-verdicts.json"],
        0,
    );
    let recovered_verdicts = json(root, "recovered-hygiene-verdicts.json");
    assert_eq!(recovered_verdicts["mutants"].as_array().unwrap().len(), 4);
    assert!(recovered_verdicts["mutants"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["verdict"]["kind"] == "missed"));
    for old in rows {
        assert!(recovered_verdicts["mutants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["stable_id"] == old["stable_id"]));
    }
    // The simpler protocol also resets Tested and Unviable, with no new edits.
    qs(
        root,
        &[
            "build",
            "--no-capture",
            "--rebuild",
            "--json",
            "repeated-rebuild.json",
        ],
        1,
    );
    let repeated: State = serde_json::from_value(json(root, "repeated-rebuild.json")).unwrap();
    assert_eq!(repeated.records.len(), 5);
    assert_eq!(
        repeated
            .records
            .iter()
            .filter(|r| matches!(r.status, Status::Built { .. }))
            .count(),
        4
    );
    assert_eq!(fs::read(SiteMap::path(&paths)).unwrap(), original_map);
}

/// Real Cargo activation, including an optional dependency and a test binary
/// with required-features; default mapping must not invent coverage.
#[test]
fn cargo_features_pin_optional_dependency_coverage_and_replay() {
    use cargo_quicksilver::exec::edited::Status;
    let repo = support::setup_tiny();
    let root = repo.path();
    fs::write(root.join(".gitignore"), "*.json\n").unwrap();
    let manifest = root.join("crate_a/Cargo.toml");
    let mut text = fs::read_to_string(&manifest).unwrap();
    text.push_str(
        r#"
[features]
default = ["base-on"]
base-on = []
advanced = ["dep:optional-helper"]
[dependencies]
optional-helper = { path = "../optional-helper", optional = true }
[[test]]
name = "advanced"
required-features = ["advanced"]
"#,
    );
    fs::write(&manifest, text).unwrap();
    let workspace = root.join("Cargo.toml");
    let text = fs::read_to_string(&workspace).unwrap().replace(
        "\"crate_schema\"]",
        "\"crate_schema\", \"optional-helper\"]",
    );
    fs::write(&workspace, text).unwrap();
    fs::create_dir_all(root.join("optional-helper/src")).unwrap();
    fs::write(
        root.join("optional-helper/Cargo.toml"),
        "[package]\nname='optional-helper'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    fs::write(
        root.join("optional-helper/src/lib.rs"),
        "pub fn increment(n: u32) -> u32 { n + 1 }\n",
    )
    .unwrap();
    let source = root.join("crate_a/src/lib.rs");
    let original = fs::read_to_string(&source).unwrap()
        + r#"
#[cfg(feature = "advanced")]
pub fn feature_value(n: u32) -> u32 { optional_helper::increment(n) }
#[cfg(feature = "base-on")]
pub fn default_value() -> bool { true }
#[cfg(test)] mod feature_defaults {
    #[test] fn defaults_are_consistent() {
        #[cfg(feature = "base-on")] assert!(super::default_value());
    }
}
"#;
    fs::write(&source, &original).unwrap();
    fs::create_dir(root.join("crate_a/tests")).unwrap();
    fs::write(
        root.join("crate_a/tests/advanced.rs"),
        "#[test] fn feature_dependency_works() { assert_eq!(tiny_a::feature_value(41), 42); }\n",
    )
    .unwrap();
    // The extra suite must not re-enable tiny_a defaults through its edge.
    let extra_manifest = root.join("crate_b/Cargo.toml");
    fs::write(
        &extra_manifest,
        fs::read_to_string(&extra_manifest).unwrap().replace(
            "path = \"../crate_a\"",
            "path = \"../crate_a\", default-features = false",
        ),
    )
    .unwrap();
    // Commit all fixture inputs before map; QS tests the committed tree.
    git(
        root,
        &[
            "add",
            ".gitignore",
            "Cargo.toml",
            "crate_a/Cargo.toml",
            "crate_b/Cargo.toml",
            "crate_a/src/lib.rs",
            "crate_a/tests/advanced.rs",
            "optional-helper",
        ],
    );
    git(
        root,
        &["commit", "-m", "feature-gated optional dependency fixture"],
    );
    qs(root, &["map", "--json", "map-default.json"], 0);
    let defaults: SiteMap = serde_json::from_value(json(root, "map-default.json")).unwrap();
    let site = defaults
        .sites
        .iter()
        .find(|s| s.fqn.ends_with(".feature_value"))
        .unwrap()
        .site_id;
    assert!(!defaults.hits.values().any(|hits| hits.contains(&site)));
    assert!(!defaults
        .tests
        .iter()
        .any(|t| t.binary_id == "tiny_a::advanced"));
    assert_eq!(defaults.cargo_features, Default::default());
    let baseline: serde_json::Value = serde_json::from_slice(
        &fs::read(support::cfg_for(root).paths().index.join("baseline.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        baseline["cargo_features"]["features"],
        serde_json::json!([])
    );

    // Persistent selection applies to every subsequent command without flags.
    // Leave the config untracked: it must not change the map's commit.
    fs::write(
        root.join("quicksilver.toml"),
        "[cargo]\nfeatures=['advanced']\n",
    )
    .unwrap();
    qs(root, &["map", "--json", "map-enabled.json"], 0);
    let enabled: SiteMap = serde_json::from_value(json(root, "map-enabled.json")).unwrap();
    assert!(enabled.hits.values().any(|hits| hits.contains(&site)));
    assert!(enabled
        .tests
        .iter()
        .any(|t| t.binary_id == "tiny_a::advanced"));
    assert_eq!(enabled.cargo_features.features, ["advanced"]);
    fs::write(
        &source,
        original.replace("{ optional_helper::increment(n) }", "{ n }"),
    )
    .unwrap();
    qs(root, &["build", "--json", "feature-built.json"], 0);
    git(root, &["restore", "crate_a/src/lib.rs"]);
    qs(root, &["test", "--json", "feature-tested.json"], 0);
    let tested = json(root, "feature-tested.json");
    assert_eq!(
        tested["cargo_features"]["features"],
        serde_json::json!(["advanced"])
    );
    assert_eq!(tested["mutants"][0]["verdict"]["kind"], "caught");
    assert!(tested["mutants"][0]["verdict"]["test"]
        .as_str()
        .unwrap()
        .contains("feature_dependency_works"));
    qs(
        root,
        &["build", "--rebuild", "--json", "feature-rebuilt.json"],
        0,
    );
    let rebuilt = json(root, "feature-rebuilt.json");
    assert_eq!(rebuilt["cargo_features"], tested["cargo_features"]);
    assert_eq!(rebuilt["records"][0]["status"]["status"], "built");

    let paths = support::cfg_for(root).paths();
    let before_state = fs::read(State::path(&paths)).unwrap();
    let before_map = fs::read(SiteMap::path(&paths)).unwrap();
    for args in [
        vec!["--no-default-features", "test"],
        vec!["--all-features", "build", "--rebuild"],
        vec!["--features", "", "build"],
        vec!["--features", "", "uncovered"],
    ] {
        let out = qs(root, &args, 2);
        assert!(String::from_utf8_lossy(&out.stderr).contains("Cargo features differ"));
        assert_eq!(fs::read(State::path(&paths)).unwrap(), before_state);
        assert_eq!(fs::read(SiteMap::path(&paths)).unwrap(), before_map);
    }
    // An explicit CLI selection replaces config. Package-qualified names and
    // disabling defaults must reach metadata, list, baseline and schema builds.
    qs(
        root,
        &[
            "--features",
            "tiny_a/advanced,tiny_a/advanced",
            "--no-default-features",
            "map",
            "--json",
            "map-no-default.json",
        ],
        0,
    );
    let changed: SiteMap = serde_json::from_value(json(root, "map-no-default.json")).unwrap();
    assert!(changed.cargo_features.no_default_features);
    assert_eq!(changed.cargo_features.features, ["tiny_a/advanced"]);
    let default_site = changed
        .sites
        .iter()
        .find(|s| s.fqn.ends_with(".default_value"))
        .unwrap()
        .site_id;
    assert!(!changed
        .hits
        .values()
        .any(|hits| hits.contains(&default_site)));
    qs(
        root,
        &[
            "--features",
            "tiny_a/advanced",
            "--no-default-features",
            "test",
            "--json",
            "test-remapped.json",
        ],
        0,
    );
    let state = State::load(&paths).unwrap();
    assert_eq!(state.records.len(), 1);
    assert!(matches!(state.records[0].status, Status::Pending));
    assert!(json(root, "test-remapped.json")["mutants"]
        .as_array()
        .unwrap()
        .is_empty());
    qs(
        root,
        &["--all-features", "map", "--json", "map-all.json"],
        0,
    );
    assert_eq!(
        json(root, "map-all.json")["cargo_features"]["all-features"],
        true
    );
    qs(root, &["--all-features", "build", "--no-capture"], 0);
    qs(
        root,
        &["--all-features", "test", "--json", "test-all.json"],
        0,
    );
    assert_eq!(
        json(root, "test-all.json")["mutants"][0]["verdict"]["kind"],
        "caught"
    );
}

#[test]
fn nested_workspace_maps_captures_and_replays_shared_production_modules() {
    let repo = support::setup_tiny();
    let root = repo.path();
    let workspace = root.join("scripts/tool");
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::create_dir_all(root.join("shared")).unwrap();
    fs::write(root.join(".gitignore"), "*.json\nscripts/tool/Cargo.lock\n").unwrap();
    fs::write(
        workspace.join("Cargo.toml"),
        "[workspace]\n[package]\nname='nested_tool'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    let main = r#"
#[cfg(test)]
#[path = "../../../tests/shared.rs"] mod test_alias;
#[path = "../../../tests/shared.rs"] mod shared;
#[path = "../../../shared/other.rs"] mod other;
#[cfg(test)]
#[path = "../../../tests/test_only.rs"] mod only_tests;
fn main() { let _ = shared::answer(20); let _ = other::flag(); }
#[cfg(test)] mod tests {
    #[test] fn catches_shared() { assert_eq!(super::shared::answer(41), 42); }
    #[test] fn catches_other() { assert!(super::other::flag()); }
    #[test] fn test_only_stays_excluded() { assert!(super::only_tests::hidden()); }
}
"#;
    fs::write(workspace.join("src/main.rs"), main).unwrap();
    let shared = root.join("tests/shared.rs");
    let original = "pub fn answer(n: u32) -> u32 { n + 1 }\n";
    fs::write(&shared, original).unwrap();
    fs::write(
        root.join("tests/test_only.rs"),
        "pub fn hidden() -> bool { true }\n",
    )
    .unwrap();
    fs::write(
        root.join("shared/other.rs"),
        "pub fn flag() -> bool { true }\n",
    )
    .unwrap();
    git(
        root,
        &["add", ".gitignore", "scripts/tool", "tests", "shared"],
    );
    git(
        root,
        &[
            "commit",
            "-m",
            "nested production graph shares tracked test-directory sources",
        ],
    );
    let tool = |args: &[&str], code: i32| {
        let out = Command::new(env!("CARGO_BIN_EXE_cargo-quicksilver"))
            .current_dir(root)
            .args([
                "quicksilver",
                "--manifest-path",
                "scripts/tool/Cargo.toml",
                "-p",
                "nested_tool",
            ])
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(code),
            "{args:?}\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    };
    tool(&["list", "--json", "nested-list.json"], 0);
    let listed = json(root, "nested-list.json");
    let mutants = listed.as_array().unwrap();
    assert!(mutants
        .iter()
        .any(|m| m["path"] == "../../tests/shared.rs" && m["fqn"] == "nested_tool.shared.answer"));
    assert!(mutants.iter().any(|m| m["path"] == "../../shared/other.rs"));
    assert!(!mutants
        .iter()
        .any(|m| m["fqn"].as_str().unwrap().contains("test_alias")
            || m["fqn"].as_str().unwrap().contains("hidden")));
    tool(
        &[
            "--in-diff",
            "HEAD^...HEAD",
            "list",
            "--json",
            "nested-diff.json",
        ],
        0,
    );
    assert!(json(root, "nested-diff.json")
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["path"] == "../../tests/shared.rs"));
    // Only tracked, in-repository shared sources can become mutation inputs.
    fs::write(
        root.join("shared/untracked.rs"),
        "pub fn untracked() -> bool { true }\n",
    )
    .unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(
        outside.path().join("outside.rs"),
        "pub fn outside() -> bool { true }\n",
    )
    .unwrap();
    fs::write(workspace.join("src/main.rs"), format!("{main}\n#[path=\"../../../shared/untracked.rs\"] mod untracked;\n#[path={:?}] mod outside;\n", outside.path().join("outside.rs"))).unwrap();
    let out = tool(&["list", "--json", "nested-boundaries.json"], 0);
    let diagnostic = String::from_utf8_lossy(&out.stderr);
    assert!(
        diagnostic.contains("untracked shared source"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("outside the Git repository"),
        "{diagnostic}"
    );
    assert!(!json(root, "nested-boundaries.json")
        .as_array()
        .unwrap()
        .iter()
        .any(|m| ["untracked", "outside"]
            .iter()
            .any(|n| m["fqn"].as_str().unwrap().ends_with(n))));
    fs::write(workspace.join("src/main.rs"), main).unwrap();
    fs::remove_file(root.join("shared/untracked.rs")).unwrap();
    tool(&["map", "--json", "nested-map.json"], 0);
    let map: SiteMap = serde_json::from_value(json(root, "nested-map.json")).unwrap();
    assert_eq!(map.prefix, "scripts/tool/");
    assert!(map.files.contains_key("../../tests/shared.rs"));
    assert!(map.test_lines["../../tests/shared.rs"].is_empty());
    let site = map
        .sites
        .iter()
        .find(|s| s.fqn == "nested_tool.shared.answer")
        .unwrap();
    assert!(map.hits.values().any(|hits| hits.contains(&site.site_id)));
    assert!(!map
        .sites
        .iter()
        .any(|s| s.fqn.contains("test_alias") || s.fqn.contains("hidden")));
    let paths = support::cfg_for(&workspace).paths();
    let map_bytes = fs::read(SiteMap::path(&paths)).unwrap();
    // Implicit checkout capture sees changes outside the Cargo root.
    fs::write(&shared, original.replace("n + 1", "n")).unwrap();
    tool(&["build", "--json", "nested-build.json"], 0);
    let captured: State = serde_json::from_value(json(root, "nested-build.json")).unwrap();
    assert_eq!(captured.records.len(), 1);
    let id = captured.records[0].mutant.id.clone();
    let body = captured.records[0].mutant.edits[0].clone();
    assert_eq!(body.path, "../../tests/shared.rs");
    assert_eq!(body.original, "{ n + 1 }");
    fs::write(&shared, original).unwrap();
    tool(&["test", "--json", "nested-test.json"], 0);
    assert_eq!(
        json(root, "nested-test.json")["mutants"][0]["verdict"]["kind"],
        "caught"
    );
    // A normal root-relative Git patch and line intent name the same body.
    let patch = root.join("shared.patch");
    fs::write(&patch, "--- a/tests/shared.rs\n+++ b/tests/shared.rs\n@@ -1 +1 @@\n-pub fn answer(n: u32) -> u32 { n + 1 }\n+pub fn answer(n: u32) -> u32 { n }\n").unwrap();
    fs::write(
        root.join("shared-intents.json"),
        "{\"tests/shared.rs:1\":\"off-by-one in shared production code\"}",
    )
    .unwrap();
    tool(
        &[
            "build",
            "--rebuild",
            "--patch",
            "shared.patch=shared-intents.json",
            "--json",
            "nested-rebuild.json",
        ],
        0,
    );
    let rebuilt: State = serde_json::from_value(json(root, "nested-rebuild.json")).unwrap();
    assert_eq!(rebuilt.records[0].mutant.id, id);
    assert_eq!(rebuilt.records[0].mutant.edits[0], body);
    assert_eq!(
        rebuilt.records[0].mutant.intent.as_deref(),
        Some("off-by-one in shared production code")
    );
    assert_eq!(fs::read(SiteMap::path(&paths)).unwrap(), map_bytes);
    tool(&["test", "--json", "nested-retest.json"], 0);
    assert_eq!(
        json(root, "nested-retest.json")["mutants"][0]["verdict"]["kind"],
        "caught"
    );
    assert_eq!(fs::read_to_string(&shared).unwrap(), original);
    // Explicit body-only replay across a fresh map also preserves identity.
    tool(&["map"], 0);
    // Simulate a fresh campaign rather than reusing an already caught record.
    State::default().save(&paths).unwrap();
    tool(
        &[
            "build",
            "--from",
            "nested-retest.json",
            "--json",
            "nested-replay.json",
        ],
        0,
    );
    let replayed: State = serde_json::from_value(json(root, "nested-replay.json")).unwrap();
    assert_eq!(replayed.records[0].mutant.id, id);
    assert!(matches!(
        replayed.records[0].status,
        cargo_quicksilver::exec::edited::Status::Built { .. }
    ));
    assert_eq!(replayed.records[0].mutant.edits[0], body);
    assert_eq!(
        replayed.records[0].mutant.intent,
        rebuilt.records[0].mutant.intent
    );
    tool(&["test", "--json", "nested-replayed-test.json"], 0);
    assert_eq!(
        json(root, "nested-replayed-test.json")["mutants"][0]["verdict"]["kind"],
        "caught"
    );
    let before = fs::read(State::path(&paths)).unwrap();
    fs::write(
        root.join("tests/test_only.rs"),
        "pub fn hidden() -> bool { false }\n",
    )
    .unwrap();
    tool(&["build"], 2);
    assert_eq!(fs::read(State::path(&paths)).unwrap(), before);
    fs::write(
        root.join("tests/test_only.rs"),
        "pub fn hidden() -> bool { true }\n",
    )
    .unwrap();
    fs::write(
        root.join("escape.patch"),
        "--- a/../../escape.rs\n+++ b/../../escape.rs\n@@ -1 +1 @@\n-a\n+b\n",
    )
    .unwrap();
    let out = tool(&["build", "--patch", "escape.patch"], 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("escapes the Git repository"));
    assert_eq!(fs::read(State::path(&paths)).unwrap(), before);
    assert_eq!(fs::read_to_string(&shared).unwrap(), original);
}

#[test]
fn explicit_production_binary_pass_keeps_cli_drivers_and_target_identity() {
    use cargo_quicksilver::exec::test::BinariesManifest;
    let repo = support::setup_tiny();
    let root = repo.path();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::create_dir_all(root.join("optional-helper/src")).unwrap();
    fs::write(root.join(".gitignore"), "*.json\n").unwrap();
    fs::write(
        root.join("Cargo.toml"),
        r#"
[package]
name = "mixed"
version = "0.1.0"
edition = "2021"
[workspace]
members = ["optional-helper"]
[features]
default = []
tooling = ["dep:optional_helper"]
[dependencies]
optional_helper = { path = "optional-helper", optional = true }
[lib]
path = "src/lib.rs"
[[bin]]
name = "author-cli"
path = "src/author.rs"
required-features = ["tooling"]
[[bin]]
name = "viewer"
path = "src/viewer.rs"
[[test]]
name = "cli"
required-features = ["tooling"]
"#,
    )
    .unwrap();
    fs::write(
        root.join("optional-helper/Cargo.toml"),
        "[package]\nname='optional_helper'\nversion='0.1.0'\nedition='2021'\n",
    )
    .unwrap();
    fs::write(
        root.join("optional-helper/src/lib.rs"),
        "pub fn offset() -> u32 { 0 }\n",
    )
    .unwrap();
    let library = "mod common; pub fn library_answer() -> u32 { common::answer() }\n";
    let viewer = "mod common; fn main() { println!(\"{}\", common::answer()); }\n";
    let author = "mod common; fn author_only() -> u32 { optional_helper::offset() } fn opaque() -> impl std::fmt::Debug { 42u32 } fn main() { println!(\"{}\", common::answer() + author_only()); }\n";
    let common = "pub fn answer() -> u32 { 42 }\n";
    fs::write(root.join("src/lib.rs"), library).unwrap();
    fs::write(root.join("src/viewer.rs"), viewer).unwrap();
    fs::write(root.join("src/author.rs"), author).unwrap();
    fs::write(root.join("src/common.rs"), common).unwrap();
    fs::write(root.join("tests/cli.rs"), r#"
use std::process::Command;
#[test]
fn authoring_cli() {
    let author = env!("CARGO_BIN_EXE_author-cli");
    let viewer = env!("CARGO_BIN_EXE_viewer");
    if std::env::var_os("QS_RECORD_SITES").is_some() || std::env::var_os("QS_MUTANT").is_some() {
        let root = std::env::var_os("CARGO_MANIFEST_DIR").unwrap();
        let cleaned = Command::new("mbx").arg("clean").arg(root).output().unwrap();
        assert!(cleaned.status.success(), "{}", String::from_utf8_lossy(&cleaned.stderr));
        assert!(std::path::Path::new(author).is_file(), "active mbx lease must protect the CLI child");
        assert!(std::path::Path::new(viewer).is_file());
    }
    assert_eq!(mixed::library_answer(), 42, "library context");
    let other = Command::new(viewer).output().unwrap();
    assert!(other.status.success());
    assert_eq!(String::from_utf8_lossy(&other.stdout).trim(), "42", "other binary context");
    let selected = Command::new(author).output().unwrap();
    assert!(selected.status.success());
    assert_eq!(String::from_utf8_lossy(&selected.stdout).trim(), "42", "selected binary context");
}
"#).unwrap();
    git(
        root,
        &[
            "add",
            "Cargo.toml",
            ".gitignore",
            "src",
            "tests/cli.rs",
            "optional-helper",
        ],
    );
    git(
        root,
        &[
            "commit",
            "-m",
            "mixed production library and required-feature binaries",
        ],
    );
    let tool = |args: &[&str], code: i32| {
        let out = Command::new(env!("CARGO_BIN_EXE_cargo-quicksilver"))
            .current_dir(root)
            .args([
                "quicksilver",
                "--manifest-path",
                "Cargo.toml",
                "-p",
                "mixed",
            ])
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
    };
    tool(&["list", "--json", "default-target.json"], 0);
    assert!(json(root, "default-target.json")
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["fqn"] == "mixed.library_answer"));
    let missing = tool(&["--bin", "absent", "list"], 2);
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no selected production target"));
    let disabled = tool(&["--bin", "author-cli", "map"], 2);
    assert!(
        String::from_utf8_lossy(&disabled.stderr).contains("required Cargo features"),
        "{}",
        String::from_utf8_lossy(&disabled.stderr)
    );
    fs::write(
        root.join("quicksilver.toml"),
        "[cargo]\nfeatures=['tooling']\n[mutation]\nbin='author-cli'\n",
    )
    .unwrap();
    tool(&["map", "--json", "author-map.json"], 0);
    let map: SiteMap = serde_json::from_value(json(root, "author-map.json")).unwrap();
    assert_eq!(map.mutation_target.bin.as_deref(), Some("author-cli"));
    assert_eq!(map.crate_roots["mixed"], "src/author.rs");
    assert!(map.tests.iter().any(|t| t.binary_id == "mixed::cli"));
    assert!(!map
        .sites
        .iter()
        .any(|s| s.fqn.contains("library_answer") || s.path == "src/viewer.rs"));
    let selected = map
        .sites
        .iter()
        .find(|s| s.fqn == "mixed::bin::author-cli.common.answer")
        .unwrap();
    let selected_id = selected.site_id;
    assert!(map
        .tests_entering(&[selected_id])
        .iter()
        .any(|t| t.binary_id == "mixed::cli"));
    let paths = support::cfg_for(root).paths();
    let map_bytes = fs::read(SiteMap::path(&paths)).unwrap();
    fs::write(root.join("src/common.rs"), common.replace("42", "0")).unwrap();
    tool(&["build", "--json", "author-built.json"], 0);
    fs::write(root.join("src/common.rs"), common).unwrap();
    let built: State = serde_json::from_value(json(root, "author-built.json")).unwrap();
    let before = fs::read(State::path(&paths)).unwrap();
    fs::write(
        root.join("src/author.rs"),
        author.replace("{ 42u32 }", "{ 43u32 }"),
    )
    .unwrap();
    let unsupported = tool(&["build"], 2);
    assert!(String::from_utf8_lossy(&unsupported.stderr).contains("opaque-return direct variants"));
    assert_eq!(fs::read(State::path(&paths)).unwrap(), before);
    fs::write(root.join("src/author.rs"), author).unwrap();

    for args in [
        vec!["--lib", "build", "--rebuild"],
        vec!["--bin", "viewer", "test"],
        vec!["--lib", "apply", &built.records[0].mutant.id],
    ] {
        let rejected = tool(&args, 2);
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("production target differs"));
        assert_eq!(fs::read(State::path(&paths)).unwrap(), before);
        assert_eq!(fs::read(SiteMap::path(&paths)).unwrap(), map_bytes);
    }
    let shared_apply = tool(&["apply", &built.records[0].mutant.id], 2);
    assert!(String::from_utf8_lossy(&shared_apply.stderr).contains("other compilation contexts"));
    let shared_prove = tool(
        &["prove", &built.records[0].mutant.id, "--", "/bin/true"],
        3,
    );
    assert!(String::from_utf8_lossy(&shared_prove.stderr).contains("other compilation contexts"));
    assert_eq!(
        fs::read_to_string(root.join("src/common.rs")).unwrap(),
        common
    );
    assert_eq!(fs::read(State::path(&paths)).unwrap(), before);
    // A missing compile-time child is an infrastructure error, never a kill.
    let run = paths
        .runs
        .join("edited")
        .join(format!("gen-{}", built.generation));
    let manifest: BinariesManifest =
        serde_json::from_slice(&fs::read(run.join("manifest.json")).unwrap()).unwrap();
    let child = manifest
        .children
        .iter()
        .find(|c| c.path.file_name().unwrap() == "author-cli")
        .unwrap();
    let held = child.path.with_extension("qs-test-held");
    fs::rename(&child.path, &held).unwrap();
    let rejected = tool(&["test"], 2);
    assert!(String::from_utf8_lossy(&rejected.stderr)
        .contains("child executable from this generation is missing or changed"));
    assert_eq!(fs::read(State::path(&paths)).unwrap(), before);
    fs::rename(held, &child.path).unwrap();
    tool(&["test", "--json", "author-tested.json"], 0);
    let tested = json(root, "author-tested.json");
    assert_eq!(tested["mutation_target"]["bin"], "author-cli");
    assert_eq!(tested["mutants"][0]["verdict"]["kind"], "caught");
    assert!(tested["mutants"][0]["verdict"]["test"]
        .as_str()
        .unwrap()
        .contains("authoring_cli"));
    let id = built.records[0].mutant.id.clone();
    tool(&["build", "--rebuild", "--json", "author-rebuilt.json"], 0);
    let rebuilt: State = serde_json::from_value(json(root, "author-rebuilt.json")).unwrap();
    assert_eq!(rebuilt.records[0].mutant.id, id);
    assert_eq!(
        rebuilt.records[0].mutant.edits,
        built.records[0].mutant.edits
    );
    assert_eq!(rebuilt.mutation_target, map.mutation_target);
    tool(&["test", "--json", "author-retested.json"], 0);
    assert_eq!(
        json(root, "author-retested.json")["mutants"][0]["verdict"]["kind"],
        "caught"
    );
    tool(&["map"], 0);
    State::default().save(&paths).unwrap();
    tool(
        &[
            "build",
            "--from",
            "author-tested.json",
            "--json",
            "author-replayed.json",
        ],
        0,
    );
    let replayed: State = serde_json::from_value(json(root, "author-replayed.json")).unwrap();
    assert_eq!(replayed.records[0].mutant.id, id);
    assert_eq!(
        replayed.records[0].mutant.edits,
        built.records[0].mutant.edits
    );
    tool(&["test", "--json", "author-replay-test.json"], 0);
    assert_eq!(
        json(root, "author-replay-test.json")["mutants"][0]["verdict"]["kind"],
        "caught"
    );
    tool(&["--bin", "viewer", "map", "--json", "viewer-map.json"], 0);
    let other: SiteMap = serde_json::from_value(json(root, "viewer-map.json")).unwrap();
    assert_ne!(
        other
            .sites
            .iter()
            .find(|s| s.fqn == "mixed::bin::viewer.common.answer")
            .unwrap()
            .site_id,
        selected_id
    );
    tool(
        &["--bin", "viewer", "test", "--json", "viewer-empty.json"],
        0,
    );
    assert!(State::load(&paths).unwrap().records.is_empty());
    assert!(json(root, "viewer-empty.json")["mutants"]
        .as_array()
        .unwrap()
        .is_empty());
    let before = fs::read(State::path(&paths)).unwrap();
    tool(
        &["--bin", "viewer", "build", "--from", "author-tested.json"],
        2,
    );
    assert_eq!(fs::read(State::path(&paths)).unwrap(), before);
    tool(&["--lib", "map", "--json", "library-map.json"], 0);
    let library_map: SiteMap = serde_json::from_value(json(root, "library-map.json")).unwrap();
    assert!(library_map.mutation_target.lib);
    assert_eq!(library_map.crate_roots["mixed"], "src/lib.rs");
    assert!(library_map
        .sites
        .iter()
        .any(|s| s.fqn == "mixed::lib::mixed.common.answer"));
    assert_eq!(
        fs::read_to_string(root.join("src/lib.rs")).unwrap(),
        library
    );
    assert_eq!(
        fs::read_to_string(root.join("src/viewer.rs")).unwrap(),
        viewer
    );
    assert_eq!(
        fs::read_to_string(root.join("src/author.rs")).unwrap(),
        author
    );
    tool(
        &[
            "--bin",
            "author-cli",
            "--file",
            "src/common.rs",
            "run",
            "--limit",
            "1",
            "--identity-sample",
            "1",
            "--json",
            "author-builtin.json",
        ],
        0,
    );
    assert!(json(root, "author-builtin.json")["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["kind"] == "caught"));
    assert_eq!(
        fs::read_to_string(root.join("src/common.rs")).unwrap(),
        common
    );
}
