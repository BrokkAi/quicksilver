mod support;

use cargo_quicksilver::capture::EditedMutant;
use cargo_quicksilver::exec::edited::{build, build_map, capture_edits, test, Status};
use cargo_quicksilver::types::VerdictKind;
use std::fs;
use std::path::Path;

/// The whole loop on the `tiny` fixture: map once, then capture, build and
/// test mutants written as edits to the checkout.
#[test]
fn edited_mutants_are_captured_built_and_judged() {
    if !cargo_quicksilver::cargo_cmd::nextest_available() {
        eprintln!("skip: nextest not available");
        return;
    }
    let repo = support::setup_tiny();
    let cfg = support::cfg_for(repo.path());
    let paths = cfg.paths();

    let map = build_map(&cfg, &paths).expect("map");
    let site = |fqn: &str| {
        map.sites
            .iter()
            .find(|s| s.fqn.ends_with(fqn))
            .unwrap_or_else(|| {
                panic!(
                    "site {fqn} in {:?}",
                    map.sites.iter().map(|s| &s.fqn).collect::<Vec<_>>()
                )
            })
    };
    let add = site("add").site_id;
    assert!(
        map.hits
            .get("tiny_a::tests::add_works")
            .is_some_and(|h| h.contains(&add)),
        "add_works enters add: {:?}",
        map.hits
    );
    let uncovered = site("uncovered").site_id;
    assert!(
        map.hits.values().all(|h| !h.contains(&uncovered)),
        "no test enters uncovered"
    );
    assert!(
        !map.sites
            .iter()
            .any(|s| s.fqn.ends_with("test_only_helper")),
        "test-module functions are not sites"
    );
    let unentered: Vec<&str> = map.unentered().iter().map(|s| s.fqn.as_str()).collect();
    assert!(
        unentered.iter().any(|f| f.ends_with(".uncovered")),
        "{unentered:?}"
    );
    assert!(
        !unentered.iter().any(|f| f.ends_with(".add")),
        "{unentered:?}"
    );
    // The generator pre-filters these; the map still has them.
    for fqn in ["as_ref", "as_impl", "as_dyn", "identity"] {
        site(fqn);
    }

    let lib = repo.path().join("crate_a/src/lib.rs");
    let original = fs::read_to_string(&lib).unwrap();
    let edit = |from: &str, to: &str| {
        let text = fs::read_to_string(&lib).unwrap();
        assert!(text.contains(from), "{from} in crate_a/src/lib.rs");
        fs::write(&lib, text.replace(from, to)).unwrap();
    };
    let reset = || fs::write(&lib, &original).unwrap();
    let run_build =
        |capture: bool| build(&cfg, &paths, capture.then_some(repo.path())).expect("build");
    let by_fqn = |captured: &[EditedMutant], name: &str| {
        captured
            .iter()
            .find(|m| m.fqns().ends_with(name))
            .unwrap_or_else(|| panic!("{name} in {captured:?}"))
            .clone()
    };

    // Four functions edited at once: four mutants, one cargo invocation.
    edit("    a + b\n", "    a - b\n");
    edit("\"hello\".into()", "\"bye\".into()");
    edit("    7\n", "    8\n");
    edit(
        "pub fn always_ok() -> bool {\n    true\n",
        "pub fn always_ok() -> bool {\n    false\n",
    );
    let report = run_build(true);
    assert!(report.rejected.is_empty(), "{:?}", report.rejected);
    assert_eq!(report.captured.len(), 4, "{:?}", report.captured);
    assert!(report.captured.iter().all(|m| m.edits.len() == 1));
    assert_eq!(report.builds, 1, "one build for all four mutants");
    let add_m = by_fqn(&report.captured, ".add");
    let greeting_m = by_fqn(&report.captured, ".greeting");
    let uncovered_m = by_fqn(&report.captured, ".uncovered");
    let missed_m = by_fqn(&report.captured, ".always_ok");
    for m in &report.captured {
        assert!(
            matches!(status_of(&report.results, m), Status::Built { .. }),
            "{}: {:?}",
            m.fqns(),
            status_of(&report.results, m)
        );
    }
    reset();

    // Capturing the same edit again is a duplicate, not a second mutant.
    edit("    a + b\n", "    a - b\n");
    let report = run_build(true);
    assert_eq!(report.duplicates.len(), 1);
    assert!(report.captured.is_empty());
    reset();

    // A second mutant of `add` needs its own round. Capturing it alone
    // compiles nothing; the next build compiles it with the rest. A body
    // that does not type-check is unviable with rustc's reason; the rest
    // still build.
    edit("    a + b\n", "    a * b\n");
    let report = capture_edits(&paths, repo.path()).expect("capture");
    assert_eq!(report.captured.len(), 1);
    assert!(report.results.is_empty(), "{:?}", report.results);
    assert_eq!(report.builds, 0);
    let mul_m = report.captured[0].clone();
    reset();
    edit("    a + b\n", "    \"x\"\n");
    let report = run_build(true);
    let bad = report.captured[0].clone();
    match status_of(&report.results, &bad) {
        Status::Unviable { message } => assert!(message.contains("mismatched types"), "{message}"),
        other => panic!("expected unviable: {other:?}"),
    }
    for m in [&mul_m, &add_m, &greeting_m, &uncovered_m, &missed_m] {
        assert!(
            matches!(status_of(&report.results, m), Status::Built { .. }),
            "{} stays built: {:?}",
            m.fqns(),
            status_of(&report.results, m)
        );
    }
    reset();

    // Signature edits and test edits are rejected, naming the place.
    edit(
        "pub fn add(a: i32, b: i32) -> i32",
        "pub fn add(a: i64, b: i64) -> i64",
    );
    let report = run_build(true);
    assert_eq!(report.rejected.len(), 1, "{:?}", report.rejected);
    assert!(
        report.rejected[0].contains("add`"),
        "{}",
        report.rejected[0]
    );
    assert!(report.captured.is_empty());
    reset();
    edit("assert_eq!(add(2, 3), 5)", "assert_eq!(add(2, 3), 6)");
    let report = run_build(true);
    assert_eq!(report.rejected.len(), 1, "{:?}", report.rejected);
    assert!(
        report.rejected[0].contains("after `"),
        "{}",
        report.rejected[0]
    );
    reset();
    let new_file = repo.path().join("crate_a/src/extra.rs");
    fs::write(&new_file, "pub fn extra() {}\n").unwrap();
    let report = run_build(true);
    assert!(
        report
            .rejected
            .iter()
            .any(|p| p.contains("extra.rs") && p.contains("added")),
        "{:?}",
        report.rejected
    );
    fs::remove_file(&new_file).unwrap();

    let results = test(&cfg, &paths, &[]).expect("test");
    let verdict = |m: &EditedMutant| {
        &results
            .iter()
            .find(|(r, _)| r.id == m.id)
            .unwrap_or_else(|| panic!("verdict for {}", m.id))
            .1
    };
    for (m, test_name) in [
        (&add_m, "add_works"),
        (&greeting_m, "greeting_works"),
        (&mul_m, "add_works"),
    ] {
        let v = verdict(m);
        assert_eq!(v.kind, VerdictKind::Caught, "{}: {v:?}", m.fqns());
        assert!(
            v.test.as_deref().is_some_and(|t| t.ends_with(test_name)),
            "{v:?}"
        );
    }
    assert_eq!(verdict(&uncovered_m).kind, VerdictKind::NotCovered);
    let v = verdict(&missed_m);
    assert_eq!(v.kind, VerdictKind::Missed, "{v:?}");
    assert!(
        v.tests_run
            .iter()
            .any(|t| t.ends_with("calls_always_ok_without_asserting")),
        "{v:?}"
    );
    assert!(
        !results.iter().any(|(m, _)| m.id == bad.id),
        "unviable mutants are not run"
    );

    // Tested mutants are final: a second test run has nothing to do.
    assert!(test(&cfg, &paths, &[]).unwrap().is_empty());
    // The checkout was only read.
    assert_eq!(fs::read_to_string(&lib).unwrap(), original);
    assert!(!Path::new(&new_file).exists());
}

fn status_of(results: &[(EditedMutant, Status)], m: &EditedMutant) -> Status {
    results
        .iter()
        .find(|(r, _)| r.id == m.id)
        .map(|(_, s)| s.clone())
        .unwrap_or_else(|| panic!("{} in results", m.id))
}
