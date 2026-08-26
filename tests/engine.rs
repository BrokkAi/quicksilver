mod support;

use cargo_quicksilver::discover::{discover, mutant_id, DiscoverFilter};
use cargo_quicksilver::engine::{prepare, run_prepared};
use cargo_quicksilver::types::VerdictKind;
use std::fs;

fn filter() -> DiscoverFilter {
    DiscoverFilter {
        packages: Vec::new(),
        files: Vec::new(),
        in_diff: None,
    }
}

#[test]
fn discovery_finds_production_fns_and_skips_test_region() {
    let tmp = support::setup_tiny();
    let mutants = discover(tmp.path(), &filter()).expect("discover");
    let fqns: Vec<String> = mutants.iter().map(|m| m.fqn.clone()).collect();
    assert!(
        fqns.iter().any(|f| f.contains("add")),
        "expected add, got {fqns:?}"
    );
    assert!(
        fqns.iter().any(|f| f.contains("uncovered")),
        "expected uncovered, got {fqns:?}"
    );
    assert!(
        !fqns.iter().any(|f| f.contains("test_only_helper")),
        "test-region helper must be excluded: {fqns:?}"
    );
    assert!(
        mutants.iter().any(|m| m.path.contains("crate_a") && m.fqn.contains("add")),
        "production symbols in a file that also has #[cfg(test)] must still be found"
    );
}

#[test]
fn mutant_ids_stable_across_line_shift() {
    let tmp = support::setup_tiny();
    let before = discover(tmp.path(), &filter()).unwrap();
    let id_add_0 = mutant_id(
        "tiny_a",
        before
            .iter()
            .find(|m| m.fqn.contains("add") && m.replacement.contains('0'))
            .unwrap()
            .path
            .as_str(),
        before
            .iter()
            .find(|m| m.fqn.contains("add") && m.replacement.contains('0'))
            .unwrap()
            .fqn
            .as_str(),
        "{ 0 }",
    );
    let lib = tmp.path().join("crate_a/src/lib.rs");
    let src = fs::read_to_string(&lib).unwrap();
    fs::write(&lib, format!("// shifted\n{src}")).unwrap();
    let after = discover(tmp.path(), &filter()).unwrap();
    let shifted = after
        .iter()
        .find(|m| m.fqn.contains("add") && m.replacement.contains('0'))
        .expect("add mutant after shift");
    assert_ne!(
        before.iter().find(|m| m.id == shifted.id).unwrap().start_line,
        shifted.start_line,
        "line numbers should have moved"
    );
    assert_eq!(shifted.id, id_add_0);
}

fn tools_ok() -> bool {
    cargo_quicksilver::cargo_cmd::nextest_available()
        && cargo_quicksilver::util::find_llvm_tool("llvm-cov").is_ok()
        && cargo_quicksilver::util::find_llvm_tool("llvm-profdata").is_ok()
}

#[test]
fn coverage_caught_missed_notcovered_timeout() {
    if !tools_ok() {
        eprintln!("skip: nextest or llvm-cov not available");
        return;
    }
    let repo = support::setup_tiny();
    let scratch = tempfile::tempdir().unwrap();
    let mut cfg = support::scratch_cfg(repo.path(), scratch.path());
    cfg.timeout_floor = std::time::Duration::from_millis(800);
    let prepared = prepare(cfg, false, false).expect("prepare");

    let covering = prepared
        .index
        .tests_covering("crate_a/src/lib.rs", 1, 3);
    assert!(
        covering.iter().any(|t| t.name.contains("add_works")),
        "add should be covered by add_works; covering={:?}",
        covering.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    let uncovered = prepared
        .mutants
        .iter()
        .filter(|m| m.fqn.contains("uncovered"))
        .cloned()
        .collect::<Vec<_>>();
    assert!(!uncovered.is_empty(), "expected uncovered mutants");

    let (nc_outcomes, nc_builds) =
        run_prepared(&prepared, 0, Some(&uncovered)).expect("run uncovered");
    assert_eq!(nc_builds, 0, "NotCovered must not build");
    assert!(
        nc_outcomes
            .iter()
            .all(|o| o.kind == VerdictKind::NotCovered && !o.build),
        "uncovered mutants must be NotCovered: {:?}",
        nc_outcomes.iter().map(|o| o.kind).collect::<Vec<_>>()
    );

    let add0 = prepared
        .mutants
        .iter()
        .find(|m| m.fqn.contains("add") && m.replacement.contains('0'))
        .cloned()
        .expect("add -> 0");
    let (caught, builds) = run_prepared(&prepared, 0, Some(&[add0])).expect("run add");
    assert!(builds >= 1);
    assert!(
        caught.iter().any(|o| o.kind == VerdictKind::Caught),
        "add -> 0 should be Caught: {:?}",
        caught
    );

    let always_false = prepared
        .mutants
        .iter()
        .find(|m| m.fqn.contains("always_ok") && m.replacement.contains("false"))
        .cloned()
        .expect("always_ok -> false");
    let (missed, _) = run_prepared(&prepared, 0, Some(&[always_false])).expect("run always_ok");
    assert!(
        missed.iter().any(|o| o.kind == VerdictKind::Missed),
        "always_ok -> false is a genuine gap: {:?}",
        missed
    );

    let hang = prepared
        .mutants
        .iter()
        .find(|m| m.fqn.contains("maybe_hang") && m.replacement.contains("true"))
        .cloned()
        .expect("maybe_hang -> true");
    let (to, _) = run_prepared(&prepared, 0, Some(&[hang])).expect("run hang");
    assert!(
        to.iter()
            .any(|o| o.kind == VerdictKind::Timeout || o.kind == VerdictKind::Caught),
        "maybe_hang -> true should timeout (Caught as Timeout): {:?}",
        to
    );
}

#[test]
fn scoped_index_does_not_file_other_crates_as_not_covered() {
    if !tools_ok() {
        eprintln!("skip: nextest or llvm-cov not available");
        return;
    }
    let repo = support::setup_tiny();
    let scratch = tempfile::tempdir().unwrap();
    let mut cfg = support::scratch_cfg(repo.path(), scratch.path());
    cfg.index_scope = Some("tiny_a".into());
    let prepared = prepare(cfg, false, false).expect("prepare");
    assert!(
        prepared.mutants.iter().all(|m| m.package == "tiny_a"),
        "in-scope planning must drop other crates, not keep them as mutants: {:?}",
        prepared
            .mutants
            .iter()
            .map(|m| m.package.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        prepared.plan.not_covered.iter().all(|m| m.package == "tiny_a"),
        "NotCovered must be in-scope only: {:?}",
        prepared
            .plan
            .not_covered
            .iter()
            .map(|m| m.package.as_str())
            .collect::<Vec<_>>()
    );
    let all = discover(repo.path(), &filter()).unwrap();
    assert!(
        all.iter().any(|m| m.package != "tiny_a"),
        "fixture must have out-of-scope mutants for this assertion to mean anything"
    );
    assert!(
        prepared.mutants.len() < all.len(),
        "scoped prepare should drop out-of-crate mutants"
    );
}


