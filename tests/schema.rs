mod support;

use cargo_quicksilver::discover::{discover, DiscoverFilter};
use cargo_quicksilver::engine::{prepare, run_schema};
use cargo_quicksilver::schema::{assign_unique_u32, build_sites, gate_differential};
use cargo_quicksilver::types::VerdictKind;

fn tools_ok() -> bool {
    cargo_quicksilver::cargo_cmd::nextest_available()
        && cargo_quicksilver::util::find_llvm_tool("llvm-cov").is_ok()
        && cargo_quicksilver::util::find_llvm_tool("llvm-profdata").is_ok()
}

fn schema_cfg(
    repo: &std::path::Path,
    scratch: &std::path::Path,
) -> cargo_quicksilver::config::Config {
    let mut cfg = support::scratch_cfg(repo, scratch);
    cfg.packages = vec!["tiny_schema".into()];
    cfg.index_scope = Some("tiny_schema".into());
    cfg.identity_sample = 0;
    cfg
}

#[test]
fn prefilter_and_site_ids_on_fixture_sources() {
    let tmp = support::setup_tiny();
    let mutants = discover(
        tmp.path(),
        &DiscoverFilter {
            packages: vec!["tiny_schema".into()],
            files: Vec::new(),
            in_diff: None,
        },
    )
    .expect("discover");
    assert!(
        mutants.iter().any(|m| m.return_kind == "ref"),
        "expected &T mutants: {:?}",
        mutants.iter().map(|m| &m.return_kind).collect::<Vec<_>>()
    );
    assert!(mutants.iter().any(|m| m.return_kind == "impl_trait"));
    assert!(
        mutants.iter().any(|m| m.fqn.contains("identity")),
        "generic identity should still be catalogued"
    );
    assert!(
        mutants.iter().any(|m| m.fqn.contains("add_async")),
        "async fn should still be catalogued"
    );
    let (sites, pre) = build_sites(&mutants).unwrap();
    assert!(
        pre.iter().all(|m| m.return_kind == "ref"
            || m.return_kind == "impl_trait"
            || m.return_kind == "dyn"),
        "pre-filter should only drop structural kinds: {:?}",
        pre.iter()
            .map(|m| m.return_kind.as_str())
            .collect::<Vec<_>>()
    );
    assert!(!pre.is_empty(), "fixture must exercise the pre-filter");
    let ids: std::collections::BTreeSet<u32> = sites.iter().map(|s| s.site_id).collect();
    assert_eq!(ids.len(), sites.len(), "site ids must be unique");
    assert!(!ids.contains(&u32::MAX));
}

#[test]
fn site_id_collision_resalts() {
    let hash = |seed: &str, salt: u32| -> u32 {
        if salt == 0 {
            7
        } else {
            7 + salt + seed.len() as u32
        }
    };
    let assigned = assign_unique_u32(&["one".into(), "two".into(), "three".into()], hash).unwrap();
    let ids: std::collections::BTreeSet<u32> = assigned.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids.len(), 3);
    assert!(assigned.iter().any(|(_, salt)| *salt > 0));
}

#[test]
fn schema_gates_and_differential_self_consistent() {
    if !tools_ok() {
        eprintln!("skip: nextest or llvm-cov not available");
        return;
    }
    let repo = support::setup_tiny();
    let scratch = tempfile::tempdir().unwrap();
    let cfg = schema_cfg(repo.path(), scratch.path());
    let prepared = prepare(cfg, false, false).expect("prepare tiny_schema");

    let (v2, cargo_invocations) = run_schema(&prepared, 0, None).expect("schema run");
    assert!(!v2.is_empty(), "expected outcomes for tiny_schema");
    assert!(
        cargo_invocations >= 1,
        "schema path must cargo-build at least once"
    );
    assert!(
        cargo_invocations < v2.len() as u64,
        "schema must not cargo-per-mutant: {cargo_invocations} cargo invocations for {} mutants",
        v2.len()
    );

    let by_kind = |os: &[cargo_quicksilver::types::Outcome], k: VerdictKind| {
        os.iter().filter(|o| o.kind == k).count()
    };

    let pre = v2
        .iter()
        .filter(|o| {
            o.kind == VerdictKind::Unviable
                && o.message
                    .as_deref()
                    .is_some_and(|m| m.contains("pre-filtered"))
        })
        .count();
    assert!(pre >= 1, "pre-filter must mark ref/impl_trait unviable");

    let add0 = v2.iter().find(|o| {
        o.mutant.fqn.contains("add")
            && !o.mutant.fqn.contains("async")
            && o.mutant.replacement.contains('0')
    });
    assert!(
        add0.is_some_and(|o| o.kind == VerdictKind::Caught),
        "add -> 0 should be Caught under schemata: {:?}",
        add0.map(|o| o.kind)
    );

    let generic = v2.iter().filter(|o| o.mutant.fqn.contains("identity"));
    assert!(
        generic.clone().all(|o| o.kind == VerdictKind::Unviable),
        "generic identity should be isolated as Unviable, not take down add: {:?}",
        generic
            .map(|o| (&o.mutant.fqn, o.kind, o.message.clone()))
            .collect::<Vec<_>>()
    );

    let unique = v2.iter().filter(|o| o.mutant.fqn.contains("unique"));
    assert!(
        unique.clone().all(|o| o.kind == VerdictKind::Unviable),
        "Unique (no Default) should be Unviable: {:?}",
        unique.map(|o| o.kind).collect::<Vec<_>>()
    );

    assert!(by_kind(&v2, VerdictKind::Caught) >= 1);

    // Self-consistency replaces the old v1 agreement check: the schema engine
    // is the only executor, so the reference it has to match is another run of
    // itself. Serialize run one as the `--json` report shape and gate run two
    // against it.
    let reference = scratch.path().join("reference.json");
    cargo_quicksilver::report::write_json(&reference, &v2, None, cargo_invocations)
        .expect("write reference report");

    // Fresh state, same scratch so the coverage index and baseline are cached.
    // `resume` is false in `scratch_cfg`, so run two decides every mutant again
    // rather than reading back run one's outcomes.jsonl.
    let cfg2 = schema_cfg(repo.path(), scratch.path());
    assert!(!cfg2.resume, "run two must not resume run one's verdicts");
    let prepared2 = prepare(cfg2, false, false).expect("prepare schema again");
    let (v2b, _) = run_schema(&prepared2, 0, None).expect("second schema run");
    assert_eq!(
        v2b.len(),
        v2.len(),
        "both runs must decide the same mutant set"
    );

    let diffs = gate_differential(&reference, &v2b).expect("differential gate");
    assert!(
        diffs.is_empty(),
        "differential gate: two schema runs must agree on every mutant id: {diffs:?}"
    );
}
