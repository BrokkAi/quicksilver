mod support;

use cargo_quicksilver::capture::EditedMutant;
use cargo_quicksilver::exec::edited::{build, build_map, test, Status};
use cargo_quicksilver::types::VerdictKind;
use std::fs;
use std::path::Path;
use std::sync::Arc;

/// The whole loop on the `tiny` fixture: map once, then capture, build and
/// test mutants written as edits to the checkout.
#[test]
fn edited_mutants_are_captured_built_and_judged() {
    if !cargo_quicksilver::cargo_cmd::nextest_available() {
        eprintln!("skip: nextest not available");
        return;
    }
    let repo = support::setup_tiny();
    let scratch = tempfile::tempdir().unwrap();
    let cfg = support::scratch_cfg(repo.path(), scratch.path());
    let paths = cfg.paths();
    paths.ensure().unwrap();
    let (backend, _) = cargo_quicksilver::scratch::probe_and_select(&cfg).unwrap();

    let map = build_map(&cfg, &paths, Arc::clone(&backend)).expect("map");
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
    let run_build = |capture: bool| {
        build(
            &cfg,
            &paths,
            Arc::clone(&backend),
            capture.then_some(repo.path()),
            false,
        )
        .expect("build")
    };

    // One mutant, two bodies: caught by add_works or greeting_works.
    edit("    a + b\n", "    a - b\n");
    edit("\"hello\".into()", "\"bye\".into()");
    let report = run_build(true);
    assert!(report.rejected.is_empty(), "{:?}", report.rejected);
    let two = report.captured.clone().expect("captured");
    assert_eq!(two.edits.len(), 2, "{two:?}");
    assert!(two.fqns().contains("add") && two.fqns().contains("greeting"));
    assert!(
        matches!(status_of(&report.results, &two), Status::Built { .. }),
        "{:?}",
        status_of(&report.results, &two)
    );
    reset();

    // Capturing the same edit again is a duplicate, not a second mutant.
    edit("    a + b\n", "    a - b\n");
    edit("\"hello\".into()", "\"bye\".into()");
    let report = run_build(true);
    assert!(report.duplicate);
    reset();

    // A body that does not type-check is unviable with rustc's reason.
    edit("    a + b\n", "    \"x\"\n");
    let report = run_build(true);
    let bad = report.captured.clone().expect("captured");
    match status_of(&report.results, &bad) {
        Status::Unviable { message } => assert!(message.contains("mismatched types"), "{message}"),
        other => panic!("expected unviable: {other:?}"),
    }
    // The earlier mutant is rebuilt into the new generation and stays viable.
    assert!(matches!(
        status_of(&report.results, &two),
        Status::Built { .. }
    ));
    reset();

    edit("    7\n", "    8\n");
    let report = run_build(true);
    let uncovered_m = report.captured.clone().expect("captured");
    reset();
    edit(
        "pub fn always_ok() -> bool {\n    true\n",
        "pub fn always_ok() -> bool {\n    false\n",
    );
    let report = run_build(true);
    let missed_m = report.captured.clone().expect("captured");
    assert!(matches!(
        status_of(&report.results, &missed_m),
        Status::Built { .. }
    ));
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
    assert!(report.captured.is_none());
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
    let v = verdict(&two);
    assert_eq!(v.kind, VerdictKind::Caught, "{v:?}");
    assert!(
        v.test
            .as_deref()
            .is_some_and(|t| t.ends_with("add_works") || t.ends_with("greeting_works")),
        "{v:?}"
    );
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
