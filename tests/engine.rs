mod support;

use cargo_quicksilver::discover::{discover, mutant_id, DiscoverFilter};
use cargo_quicksilver::engine::{prepare, run_schema, run_schema_detailed};
use cargo_quicksilver::higher_order::TriageClassification;
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
        mutants
            .iter()
            .any(|m| m.path.contains("crate_a") && m.fqn.contains("add")),
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
        before
            .iter()
            .find(|m| m.id == shifted.id)
            .unwrap()
            .start_line,
        shifted.start_line,
        "line numbers should have moved"
    );
    assert_eq!(shifted.id, id_add_0);
}

fn tools_ok() -> bool {
    cargo_quicksilver::cargo_cmd::nextest_available()
        && cargo_quicksilver::util::find_llvm_tool("llvm-cov", &support::fixture_src()).is_ok()
        && cargo_quicksilver::util::find_llvm_tool("llvm-profdata", &support::fixture_src()).is_ok()
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
    let mut prepared = prepare(cfg, false, false).expect("prepare");

    let covering = prepared.index.tests_covering("crate_a/src/lib.rs", 1, 3);
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
        run_schema(&prepared, 0, Some(&uncovered)).expect("run uncovered");
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
    let (caught, builds) = run_schema(&prepared, 0, Some(&[add0])).expect("run add");
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
    let (missed, _) =
        run_schema(&prepared, 0, Some(std::slice::from_ref(&always_false))).expect("run always_ok");
    assert!(
        missed.iter().any(|o| o.kind == VerdictKind::Missed),
        "always_ok -> false is a genuine gap: {:?}",
        missed
    );

    prepared.cfg.triage = true;
    prepared.cfg.triage_neighbors = 1;
    prepared.cfg.second_order_pairs = 1;
    let triaged = run_schema_detailed(
        &prepared,
        0,
        Some(&[add0_for_triage(&prepared), always_false.clone()]),
    )
    .expect("triage survivor against add control");
    let triage = triaged.triage.as_ref().expect("triage report");
    assert_eq!(triage.survivors, 1);
    assert_eq!(triage.experiments.len(), 1);
    assert_eq!(
        triage.experiments[0].classification,
        TriageClassification::LikelyEquivalent
    );
    let second_order = triaged.second_order.as_ref().expect("second-order report");
    assert_eq!(second_order.requested_pairs, 1);
    assert_eq!(second_order.completed_pairs, 1);
    assert_eq!(second_order.caught_pairs, 1);
    assert_eq!(second_order.implied_first_order_kill_probability, 1.0);
    let coupling = triaged.coupling.as_ref().expect("coupling summary");
    assert!(
        coupling.path.is_file(),
        "artifact path: {}",
        coupling.path.display()
    );
    assert!(coupling.rows >= 3, "single and pair rows: {coupling:?}");
    assert!(
        coupling.complete_rows >= 3,
        "triage completes all three vectors"
    );
    assert!(
        coupling.test_columns >= 2,
        "union tests must become columns"
    );
    let evidence_json = scratch.path().join("evidence.json");
    cargo_quicksilver::report::write_json_with_evidence(
        &evidence_json,
        &triaged.outcomes,
        None,
        triaged.cargo_invocations,
        triaged.triage.as_ref(),
        triaged.second_order.as_ref(),
        triaged.coupling.as_ref(),
    )
    .expect("write evidence JSON");
    let evidence: serde_json::Value =
        serde_json::from_slice(&fs::read(evidence_json).unwrap()).unwrap();
    assert!(evidence["triage"].is_object());
    assert!(evidence["second_order"].is_object());
    assert!(evidence["coupling"].is_object());

    let hang = prepared
        .mutants
        .iter()
        .find(|m| m.fqn.contains("maybe_hang") && m.replacement.contains("true"))
        .cloned()
        .expect("maybe_hang -> true");
    let (to, _) = run_schema(&prepared, 0, Some(&[hang])).expect("run hang");
    assert!(
        to.iter()
            .any(|o| o.kind == VerdictKind::Timeout || o.kind == VerdictKind::Caught),
        "maybe_hang -> true should timeout (Caught as Timeout): {:?}",
        to
    );
}

fn add0_for_triage(
    prepared: &cargo_quicksilver::engine::Prepared,
) -> cargo_quicksilver::types::Mutant {
    prepared
        .mutants
        .iter()
        .find(|m| m.fqn.contains("add") && m.replacement.contains('0'))
        .cloned()
        .expect("add -> 0")
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
        prepared
            .plan
            .not_covered
            .iter()
            .all(|m| m.package == "tiny_a"),
        "NotCovered must be in-scope only: {:?}",
        prepared
            .plan
            .not_covered
            .iter()
            .map(|m| m.package.as_str())
            .collect::<Vec<_>>()
    );
    // always_ok and maybe_hang live in tiny_a but are reached only from tiny_c's
    // tests. Scoping the index must not turn cross-crate coverage into NotCovered.
    let covered_fqns: Vec<String> = prepared
        .plan
        .batches
        .iter()
        .flat_map(|b| b.mutants.iter())
        .map(|m| m.mutant.fqn.clone())
        .collect();
    assert!(
        covered_fqns.iter().any(|f| f.contains("always_ok")),
        "tiny_c covers tiny_a::always_ok; covered={covered_fqns:?}"
    );
    assert!(
        covered_fqns.iter().any(|f| f.contains("maybe_hang")),
        "tiny_c covers tiny_a::maybe_hang; covered={covered_fqns:?}"
    );
    assert_eq!(
        prepared.plan.not_covered.len(),
        2,
        "only uncovered() is NotCovered, f_nc=2/9; not_covered={:?}",
        prepared
            .plan
            .not_covered
            .iter()
            .map(|m| m.fqn.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        prepared
            .plan
            .not_covered
            .iter()
            .all(|m| m.fqn.contains("uncovered")),
        "NotCovered should be uncovered() only"
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

/// `--test-selection sites` replaces LLVM Stage A and B with one recording
/// run under the schema binary. On the fixture it must reach the same verdict
/// as coverage selection for every mutant, without an instrumented build.
#[test]
fn site_selection_matches_coverage_selection() {
    if !tools_ok() {
        eprintln!("skip: nextest or llvm-cov not available");
        return;
    }
    let repo = support::setup_tiny();
    let run = |selection: cargo_quicksilver::config::TestSelection| {
        let scratch = tempfile::tempdir().unwrap();
        let mut cfg = support::scratch_cfg(repo.path(), scratch.path());
        cfg.timeout_floor = std::time::Duration::from_millis(800);
        cfg.identity_sample = 0;
        cfg.test_selection = selection;
        let paths = cfg.paths();
        let prepared = prepare(cfg, false, false).expect("prepare");
        let (outcomes, _) = run_schema(&prepared, 0, None).expect("schema run");
        let verdicts: std::collections::BTreeMap<String, (String, VerdictKind)> = outcomes
            .iter()
            .map(|o| {
                (
                    o.mutant.id.clone(),
                    (
                        format!("{} -> {}", o.mutant.fqn, o.mutant.replacement),
                        o.kind,
                    ),
                )
            })
            .collect();
        (verdicts, paths.baseline_cov.exists(), scratch)
    };
    let (coverage, _, _keep_a) = run(cargo_quicksilver::config::TestSelection::Coverage);
    let (sites, instrumented, _keep_b) = run(cargo_quicksilver::config::TestSelection::Sites);
    assert!(!instrumented, "sites selection must not build baseline-cov");
    assert_eq!(coverage.len(), sites.len(), "same mutants in both runs");
    let diffs: Vec<_> = coverage
        .iter()
        .filter(|(id, v)| sites.get(*id) != Some(v))
        .map(|(id, v)| (v.0.clone(), v.1, sites.get(id).map(|s| s.1)))
        .collect();
    assert!(
        diffs.is_empty(),
        "coverage vs sites verdicts differ: {diffs:#?}"
    );
    let kinds: Vec<VerdictKind> = sites.values().map(|v| v.1).collect();
    for k in [
        VerdictKind::Caught,
        VerdictKind::Missed,
        VerdictKind::NotCovered,
    ] {
        assert!(
            kinds.contains(&k),
            "fixture should exercise {k:?}: {kinds:?}"
        );
    }
}
