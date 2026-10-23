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

/// A test that builds and runs a workspace program in a child process, the
/// way trycmd runs clap's examples, must see the schema source and the
/// active mutant. When tests started in the baseline copy, the child
/// compiled the original source, recorded no site, and the mutant was
/// NotCovered although the test checks its output.
#[test]
fn a_child_process_built_from_the_workspace_sees_the_mutant() {
    if !tools_ok() {
        eprintln!("skip: nextest or llvm-cov not available");
        return;
    }
    let repo = support::setup_fixture("child");
    let scratch = tempfile::tempdir().unwrap();
    let mut cfg = support::scratch_cfg(repo.path(), scratch.path());
    // The first run in a tree compiles the example; later runs only start it.
    cfg.timeout_floor = std::time::Duration::from_secs(60);
    cfg.test_selection = cargo_quicksilver::config::TestSelection::Sites;
    let prepared = prepare(cfg, false, false).expect("prepare");
    let (outcomes, _) = run_schema(&prepared, 0, None).expect("schema run");
    let greeting: Vec<_> = outcomes
        .iter()
        .filter(|o| o.mutant.fqn.ends_with("greeting"))
        .map(|o| (o.mutant.replacement.trim().to_string(), o.kind))
        .collect();
    assert!(!greeting.is_empty(), "no greeting mutants: {outcomes:#?}");
    assert!(
        greeting.iter().all(|(_, k)| *k == VerdictKind::Caught),
        "every greeting mutant changes the example's output: {greeting:?}"
    );
}

/// Preflight rebuilds the whole workspace to look for build scripts that hash
/// sources. Its answer depends on manifests and build scripts, not on
/// ordinary source, so a per-commit run reuses the count until one of those
/// changes.
#[test]
fn preflight_count_is_reused_until_a_build_script_or_manifest_changes() {
    use cargo_quicksilver::baseline::{cached_preflight, preflight_key, save_preflight};
    let repo = support::setup_tiny();
    let root = repo.path();
    let scratch = tempfile::tempdir().unwrap();
    let paths = support::scratch_cfg(root, scratch.path()).paths();
    paths.ensure().unwrap();

    let before = preflight_key(root).unwrap();
    assert_eq!(before, preflight_key(root).unwrap(), "key must be stable");
    save_preflight(&paths, &before, 3).unwrap();
    assert_eq!(cached_preflight(&paths, &before), Some(3));

    let lib = root.join("crate_a/src/lib.rs");
    let mut text = fs::read_to_string(&lib).unwrap();
    text.push_str("\npub fn added_by_the_test() -> u8 { 1 }\n");
    fs::write(&lib, text).unwrap();
    assert_eq!(
        preflight_key(root).unwrap(),
        before,
        "ordinary source edits keep the key"
    );

    fs::write(root.join("crate_a/build.rs"), "fn main() {}\n").unwrap();
    let after = preflight_key(root).unwrap();
    assert_ne!(after, before, "a new build script must measure again");
    assert_eq!(cached_preflight(&paths, &after), None);
}

/// In sites mode a second run in the same scratch session reuses the test
/// durations saved by the first and does not build or run the baseline suite
/// again; the recording run decides which tests are green for this commit.
#[test]
fn sites_mode_reuses_baseline_timings_within_a_session() {
    if !tools_ok() {
        eprintln!("skip: nextest or llvm-cov not available");
        return;
    }
    let repo = support::setup_tiny();
    let scratch = tempfile::tempdir().unwrap();
    let cfg = || {
        let mut cfg = support::scratch_cfg(repo.path(), scratch.path());
        cfg.test_selection = cargo_quicksilver::config::TestSelection::Sites;
        cfg
    };
    let first = prepare(cfg(), false, false).expect("first prepare");
    let report = first.paths.index.join("baseline.json");
    let saved = fs::metadata(&report).unwrap().modified().unwrap();
    let first_tests = first.baseline.tests.len();
    assert!(first_tests > 0, "the first run times the suite");
    drop(first);

    let second = prepare(cfg(), false, false).expect("second prepare");
    assert_eq!(
        fs::metadata(&report).unwrap().modified().unwrap(),
        saved,
        "the second run must not run the baseline suite again"
    );
    assert_eq!(second.baseline.tests.len(), first_tests);
    assert!(second.baseline.failed_test_ids.is_empty());
    assert!(
        !second.plan.batches.is_empty(),
        "tests are still listed and planned"
    );
}

/// `--in-diff` discovery returns exactly the mutants of the functions a
/// commit edits or introduces, identical (ids, byte ranges, replacements) to
/// what full discovery gives for those functions.
#[test]
fn in_diff_discovery_matches_full_discovery_for_changed_functions() {
    let repo = support::setup_tiny();
    let root = repo.path();
    let lib = root.join("crate_a/src/lib.rs");
    let text = fs::read_to_string(&lib).unwrap();
    let text = text.replace(
        "pub fn flag() -> bool {\n    true\n}",
        "pub fn flag() -> bool {\n    !false\n}",
    );
    let text = text.replacen(
        "#[cfg(test)]",
        "pub fn introduced() -> u32 {\n    41 + 1\n}\n\n#[cfg(test)]",
        1,
    );
    fs::write(&lib, text).unwrap();
    for args in [
        vec!["add", "-A"],
        vec!["-c", "commit.gpgsign=false", "commit", "-q", "-m", "edit"],
    ] {
        assert!(std::process::Command::new("git")
            .args(&args)
            .current_dir(root)
            .status()
            .unwrap()
            .success());
    }
    let diff = discover(
        root,
        &DiscoverFilter {
            in_diff: Some("HEAD^...HEAD".into()),
            ..filter()
        },
    )
    .expect("diff discovery");
    let fqns: std::collections::BTreeSet<&str> = diff.iter().map(|m| m.fqn.as_str()).collect();
    assert!(
        fqns.iter()
            .all(|f| f.ends_with(".flag") || f.ends_with(".introduced")),
        "only the changed functions: {fqns:?}"
    );
    assert!(fqns.iter().any(|f| f.ends_with(".flag")), "{fqns:?}");
    assert!(fqns.iter().any(|f| f.ends_with(".introduced")), "{fqns:?}");
    let key = |m: &cargo_quicksilver::types::Mutant| {
        (
            m.id.clone(),
            m.path.clone(),
            m.start_byte,
            m.end_byte,
            m.body_start_byte,
            m.body_end_byte,
            m.replacement.clone(),
        )
    };
    let mut full: Vec<_> = discover(root, &filter())
        .expect("full discovery")
        .iter()
        .filter(|m| fqns.contains(m.fqn.as_str()))
        .map(key)
        .collect();
    let mut got: Vec<_> = diff.iter().map(key).collect();
    full.sort();
    got.sort();
    assert_eq!(got, full);

    // discover_in_diff is the same selection by name.
    let scoped = cargo_quicksilver::discover::discover_in_diff(
        root,
        &DiscoverFilter {
            in_diff: Some("HEAD^...HEAD".into()),
            ..filter()
        },
    )
    .expect("scoped diff discovery");
    let mut scoped: Vec<_> = scoped.iter().map(key).collect();
    scoped.sort();
    assert_eq!(scoped, full, "discover_in_diff must match full discovery");
}
