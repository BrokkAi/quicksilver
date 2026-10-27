mod support;

use cargo_quicksilver::discover::{discover, mutant_id, DiscoverFilter};
use cargo_quicksilver::engine::{prepare, run_schema};
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
}

#[test]
fn caught_missed_notcovered_timeout() {
    if !tools_ok() {
        eprintln!("skip: nextest not available");
        return;
    }
    let repo = support::setup_tiny();
    let mut cfg = support::cfg_for(repo.path());
    cfg.timeout_floor = std::time::Duration::from_millis(800);
    let prepared = prepare(cfg, false).expect("prepare");

    let uncovered = prepared
        .mutants
        .iter()
        .filter(|m| m.fqn.contains("uncovered"))
        .cloned()
        .collect::<Vec<_>>();
    assert!(!uncovered.is_empty(), "expected uncovered mutants");

    let (nc_outcomes, _) = run_schema(&prepared, 0, Some(&uncovered)).expect("run uncovered");
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

    let hang = prepared
        .mutants
        .iter()
        .find(|m| m.fqn.contains("maybe_hang") && m.replacement.contains("true"))
        .cloned()
        .expect("maybe_hang -> true");
    let (to, _) = run_schema(&prepared, 0, Some(&[hang])).expect("run hang");
    // The test passes in milliseconds without the mutant and hangs with it:
    // a timeout the mutant caused counts as caught.
    assert!(
        to.iter().any(|o| o.kind == VerdictKind::Caught
            && o.message
                .as_deref()
                .is_some_and(|m| m.starts_with(cargo_quicksilver::types::TIMEOUT_PREFIX))),
        "maybe_hang -> true should be caught by timeout: {:?}",
        to
    );
}

/// Selecting tests by recorded site hits must not lose a kill: every
/// fixture mutant gets the same verdict with the selected tests as with every
/// candidate test, and no test reaches the functions selection calls
/// uncovered. This is `audit-selection` over the whole fixture.
#[test]
fn selection_loses_no_kill_against_the_full_suite() {
    if !tools_ok() {
        eprintln!("skip: nextest not available");
        return;
    }
    let repo = support::setup_tiny();
    let mut cfg = support::cfg_for(repo.path());
    cfg.timeout_floor = std::time::Duration::from_millis(800);
    cfg.identity_sample = 0;
    let prepared = prepare(cfg, false).expect("prepare");
    let (selected, _) = run_schema(&prepared, 0, None).expect("selected run");
    let full_plan = cargo_quicksilver::plan::plan_full_suite(
        &prepared.mutants,
        prepared.selector(),
        &prepared.baseline,
        None,
        prepared.cfg.build_jobs,
    );
    assert!(full_plan.full_suite);
    let full = cargo_quicksilver::exec::schema::execute(
        &prepared.cfg,
        &prepared.paths,
        full_plan,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        0,
    )
    .expect("full-suite run");
    let report = cargo_quicksilver::report::audit_compare(&selected, &full.outcomes);
    assert_eq!(
        report.lost_kills, 0,
        "selection lost kills: {:#?}",
        report.disagreements
    );
    let kinds: Vec<VerdictKind> = selected.iter().map(|o| o.kind).collect();
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

#[test]
fn a_child_process_built_from_the_workspace_sees_the_mutant() {
    if !tools_ok() {
        eprintln!("skip: nextest not available");
        return;
    }
    let repo = support::setup_fixture("child");
    let mut cfg = support::cfg_for(repo.path());
    // The first run in a tree compiles the example; later runs only start it.
    cfg.timeout_floor = std::time::Duration::from_secs(60);
    let prepared = prepare(cfg, false).expect("prepare");
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
    let cfg = support::cfg_for(root);
    let paths = cfg.paths();
    cargo_quicksilver::baseline::checkout_main(&cfg, &paths).unwrap();

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
        eprintln!("skip: nextest not available");
        return;
    }
    let repo = support::setup_tiny();
    let cfg = || {
        let cfg = support::cfg_for(repo.path());
        cfg
    };
    let first = prepare(cfg(), false).expect("first prepare");
    let report = first.paths.index.join("baseline.json");
    let saved = fs::metadata(&report).unwrap().modified().unwrap();
    let first_tests = first.baseline.tests.len();
    assert!(first_tests > 0, "the first run times the suite");
    drop(first);

    let second = prepare(cfg(), false).expect("second prepare");
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

/// `--package tiny_a` runs only tiny_a's tests, so `always_ok`, which only
/// tiny_c's tests call, is not covered. `--extra-tests tiny_c` adds that
/// one binary (and none of tiny_b's tests) and the mutant is reached.
#[test]
fn extra_tests_add_one_downstream_binary_to_a_package_run() {
    if !tools_ok() {
        eprintln!("skip: nextest not available");
        return;
    }
    let repo = support::setup_tiny();
    let verdict = |extra: &[&str]| {
        let mut cfg = support::cfg_for(repo.path());
        cfg.packages = vec!["tiny_a".into()];
        cfg.extra_tests = extra.iter().map(|s| s.to_string()).collect();
        cfg.timeout_floor = std::time::Duration::from_millis(800);
        let prepared = prepare(cfg, false).expect("prepare");
        let packages: std::collections::BTreeSet<String> =
            prepared.tests.iter().map(|t| t.package.clone()).collect();
        let always_false = prepared
            .mutants
            .iter()
            .find(|m| m.fqn.contains("always_ok") && m.replacement.contains("false"))
            .cloned()
            .expect("always_ok -> false");
        let (outcomes, _) =
            run_schema(&prepared, 0, Some(std::slice::from_ref(&always_false))).expect("run");
        (packages, outcomes[0].kind)
    };
    let (packages, kind) = verdict(&[]);
    assert_eq!(packages.into_iter().collect::<Vec<_>>(), vec!["tiny_a"]);
    assert_eq!(kind, VerdictKind::NotCovered);
    let (packages, kind) = verdict(&["tiny_c"]);
    assert_eq!(
        packages.into_iter().collect::<Vec<_>>(),
        vec!["tiny_a", "tiny_c"],
        "tiny_b's tests stay out"
    );
    assert_eq!(kind, VerdictKind::Missed, "tiny_c's test reaches always_ok");
}
