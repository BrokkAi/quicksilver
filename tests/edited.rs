mod support;

use cargo_quicksilver::capture::CaptureOptions;
use cargo_quicksilver::capture::EditedMutant;
use cargo_quicksilver::exec::edited::{build, build_map, test, CaptureSources, Status};
use cargo_quicksilver::types::VerdictKind;
use std::fs;
use std::path::Path;

fn walk_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            out.extend(walk_files(&entry.path()));
        } else {
            out.push(entry.path());
        }
    }
    out
}

/// The whole loop on the `tiny` fixture: map once, then capture, build and
/// test mutants written as edits to the checkout.
#[test]
fn edited_mutants_are_captured_built_and_judged() {
    if !cargo_quicksilver::cargo_cmd::nextest_available() {
        eprintln!("skip: nextest not available");
        return;
    }
    let repo = support::setup_tiny();
    let mut cfg = support::cfg_for(repo.path());
    cfg.sandbox = true;
    cfg.test_env
        .insert("FIXTURE_STATE".into(), "{sandbox}/state".into());
    cfg.test_env.insert("FIXTURE_NOT_GREEN".into(), "1".into());
    let paths = cfg.paths();
    cargo_quicksilver::sandbox::configure(&cfg, &paths).expect("sandbox");

    let map = build_map(&cfg, &paths).expect("map");
    // The test that checks its HOME passed in its sandbox, and no sandbox
    // is left behind.
    assert!(
        map.hits
            .keys()
            .any(|id| id.ends_with("state_goes_under_a_fresh_home")),
        "{:?}",
        map.hits.keys()
    );
    let settings = cargo_quicksilver::sandbox::current().expect("configured");
    let root = settings
        .root()
        .expect("test processes used a sandbox")
        .to_path_buf();
    assert_eq!(
        cargo_quicksilver::sandbox::enclosing_repository(&root),
        None,
        "sandboxes are outside any repository: {}",
        root.display()
    );
    let left: Vec<_> = walk_files(&root);
    assert!(left.is_empty(), "{left:?}");
    let summary = settings.summary().expect("test processes ran in sandboxes");
    assert!(summary.contains("all deleted"), "{summary}");
    // A test that is not green keeps the end of its output, stdout first.
    let (red, why) = map
        .not_green
        .iter()
        .find(|(id, _)| id.ends_with("red_only_when_asked"))
        .unwrap_or_else(|| panic!("{:?}", map.not_green));
    assert!(why.contains("failed with no mutant active"), "{why}");
    let output = &map.not_green_output[red];
    assert!(output.contains("output before the failure"), "{output}");
    assert!(
        output.contains("asked to fail by FIXTURE_NOT_GREEN"),
        "{output}"
    );
    assert_eq!(map.not_green.len(), 1, "{:?}", map.not_green);
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
    // A function compiled only for another target has no site and is not
    // listed as uncovered; the map records its target.
    assert!(!map.target.is_empty());
    let (here, elsewhere) = if cfg!(windows) {
        ("windows_only", "not_windows")
    } else {
        ("not_windows", "windows_only")
    };
    site(here);
    assert!(!map.sites.iter().any(|s| s.fqn.ends_with(elsewhere)));
    assert!(
        map.dropped
            .iter()
            .any(|(_, fqn, why)| fqn.ends_with(elsewhere) && why.contains("compiled out")),
        "{:?}",
        map.dropped
    );
    assert!(!map.unentered().iter().any(|s| s.fqn.ends_with(elsewhere)));
    // Uncovered functions are marked: no callers at all, or scaffolding.
    let unentered: Vec<&cargo_quicksilver::sitemap::MapSite> = map.unentered();
    let no_callers =
        cargo_quicksilver::sitemap::without_callers(&map, repo.path(), &unentered).unwrap();
    let marked = |name: &str| {
        let s = site(name);
        assert!(
            unentered.iter().any(|u| u.site_id == s.site_id),
            "{name} is uncovered"
        );
        no_callers.contains(&s.site_id)
    };
    assert!(marked(".calls_no_one_tests"));
    assert!(
        !marked(".called_in_production"),
        "called by calls_no_one_tests"
    );
    assert!(marked("tiny_a.uncovered"));
    assert_eq!(
        site(".sample_for_test").scaffolding.as_deref(),
        Some("named `*_for_test`")
    );
    assert_eq!(site(".called_in_production").scaffolding, None);

    // A method of an `impl` nested in a function body is a site of its own.
    let nested = site("parse_pair.Pair.parse");
    let outer = site(".parse_pair");
    assert!(
        outer.body_start_byte < nested.fn_start_byte && nested.body_end_byte < outer.body_end_byte
    );

    let lib = repo.path().join("crate_a/src/lib.rs");
    let original = fs::read_to_string(&lib).unwrap();
    let edit = |from: &str, to: &str| {
        let text = fs::read_to_string(&lib).unwrap();
        assert!(text.contains(from), "{from} in crate_a/src/lib.rs");
        fs::write(&lib, text.replace(from, to)).unwrap();
    };
    let reset = || fs::write(&lib, &original).unwrap();
    let build_full = |patches: Vec<std::path::PathBuf>, skip: bool, intents: &[(&str, &str)]| {
        let sources = CaptureSources {
            checkout: Some(repo.path().to_path_buf()),
            patches,
            options: CaptureOptions {
                skip_outside_bodies: skip,
            },
            intents: intents
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        };
        build(&cfg, &paths, &sources).expect("build")
    };
    let build_with = |patches: Vec<std::path::PathBuf>, skip: bool| build_full(patches, skip, &[]);
    let run_build = |_capture: bool| build_with(Vec::new(), false);
    let git_diff = || {
        let out = std::process::Command::new("git")
            .args(["diff"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(out.status.success());
        out.stdout
    };
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
    let report = build_full(
        Vec::new(),
        false,
        &[
            ("tiny_a.always_ok", "returns the opposite constant"),
            ("crate_a/src/lib.rs:2", "subtracts instead of adding"),
        ],
    );
    assert!(report.rejected.is_empty(), "{:?}", report.rejected);
    assert_eq!(report.captured.len(), 4, "{:?}", report.captured);
    assert!(report.captured.iter().all(|m| m.edits.len() == 1));
    assert_eq!(report.builds, 1, "one build for all four mutants");
    let add_m = by_fqn(&report.captured, ".add");
    let greeting_m = by_fqn(&report.captured, ".greeting");
    let uncovered_m = by_fqn(&report.captured, ".uncovered");
    let missed_m = by_fqn(&report.captured, ".always_ok");
    assert_eq!(
        missed_m.intent.as_deref(),
        Some("returns the opposite constant")
    );
    assert_eq!(add_m.intent.as_deref(), Some("subtracts instead of adding"));
    assert_eq!(greeting_m.intent, None);
    assert_eq!(missed_m.edits[0].original, "{\n    true\n}");
    assert_eq!(missed_m.edits[0].line, 9);
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

    // Two more mutants of `add` in one build: one from a patch (with no
    // checkout to apply it to), one from the checkout. A body that does not
    // type-check is unviable with rustc's reason; the rest still build.
    edit("    a + b\n", "    a * b\n");
    let patch = repo.path().join("../mul.patch");
    fs::write(&patch, git_diff()).unwrap();
    reset();
    edit("    a + b\n", "    \"x\"\n");
    let report = build_with(vec![patch.clone()], false);
    assert_eq!(report.captured.len(), 2, "{:?}", report.captured);
    let mul_m = report
        .captured
        .iter()
        .find(|m| m.edits[0].body.contains("a * b"))
        .expect("the patch's mutant")
        .clone();
    let bad = report
        .captured
        .iter()
        .find(|m| m.edits[0].body.contains("\"x\""))
        .expect("the checkout's mutant")
        .clone();
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

    // An edit inside the nested method is that method's mutant.
    let schema_lib = repo.path().join("crate_schema/src/lib.rs");
    let schema_original = fs::read_to_string(&schema_lib).unwrap();
    fs::write(
        &schema_lib,
        schema_original.replace(
            "Pair(it.next().unwrap(), it.next().unwrap())",
            "Pair(it.next().unwrap(), 0)",
        ),
    )
    .unwrap();
    let report = run_build(true);
    assert_eq!(report.captured.len(), 1, "{:?}", report.captured);
    let nested_m = report.captured[0].clone();
    assert!(
        nested_m.fqns().ends_with("parse_pair.Pair.parse"),
        "{nested_m:?}"
    );
    assert!(matches!(
        status_of(&report.results, &nested_m),
        Status::Built { .. }
    ));
    fs::write(&schema_lib, &schema_original).unwrap();

    // Two mutants of an `impl Trait` function with different concrete
    // types cannot share a schema `match`; each gets a build of its own.
    fs::write(
        &schema_lib,
        schema_original.replace("Display {\n    42\n}", "Display {\n    \"forty-two\"\n}"),
    )
    .unwrap();
    let str_patch = repo.path().join("../impl_str.patch");
    fs::write(&str_patch, git_diff()).unwrap();
    fs::write(
        &schema_lib,
        schema_original.replace("Display {\n    42\n}", "Display {\n    43\n}"),
    )
    .unwrap();
    let report = build_with(vec![str_patch], false);
    assert_eq!(report.captured.len(), 2, "{:?}", report.captured);
    for m in &report.captured {
        assert!(m.fqns().ends_with(".as_impl"), "{m:?}");
        match status_of(&report.results, m) {
            Status::Built { direct, .. } => assert!(direct, "{m:?} is built on its own"),
            other => panic!("{}: expected built, got {other:?}", m.edits[0].body),
        }
    }
    let impl_ms = report.captured.clone();
    fs::write(&schema_lib, &schema_original).unwrap();

    // A survivor that only proptest properties judged is flagged, and more
    // cases can catch it.
    assert!(
        map.property_tests
            .iter()
            .any(|id| id.ends_with("shout_property")),
        "{:?}",
        map.property_tests
    );
    fs::write(
        &schema_lib,
        schema_original.replace("    s.to_uppercase()\n", "    s.to_lowercase()\n"),
    )
    .unwrap();
    let lower_patch = repo.path().join("../shout_lower.patch");
    fs::write(&lower_patch, git_diff()).unwrap();
    fs::write(
        &schema_lib,
        schema_original.replace("    s.to_uppercase()\n", "    s.to_uppercase().clone()\n"),
    )
    .unwrap();
    let same_patch = repo.path().join("../shout_same.patch");
    fs::write(&same_patch, git_diff()).unwrap();
    fs::write(
        &schema_lib,
        schema_original.replace("    s.to_uppercase()\n", "    s.to_string()\n"),
    )
    .unwrap();
    let report = build_with(vec![lower_patch, same_patch], false);
    assert_eq!(report.captured.len(), 3, "{:?}", report.captured);
    fs::write(&schema_lib, &schema_original).unwrap();
    let lower = report
        .captured
        .iter()
        .find(|m| m.edits[0].body.contains("to_lowercase"))
        .unwrap()
        .clone();
    let same = report
        .captured
        .iter()
        .find(|m| m.edits[0].body.contains("to_string"))
        .unwrap()
        .clone();
    let equivalent = report
        .captured
        .iter()
        .find(|m| m.edits[0].body.contains(".clone()"))
        .unwrap()
        .clone();
    let results = test(&cfg, &paths, std::slice::from_ref(&lower.id), 0).expect("test");
    assert_eq!(results.len(), 1);
    let v = &results[0].1;
    assert_eq!(v.kind, VerdictKind::Missed, "{v:?}");
    assert!(v.note.is_some(), "{v:?}");
    let line = cargo_quicksilver::exec::edited::verdict_line(&lower, v);
    assert!(line.contains("proptest properties only"), "{line}");
    assert!(line.contains("try `test --proptest-cases"), "{line}");
    assert_eq!(v.proptest_rerun_cases, None);
    let results = test(&cfg, &paths, std::slice::from_ref(&same.id), 1000).expect("test");
    let v = &results[0].1;
    assert_eq!(v.kind, VerdictKind::Caught, "{v:?}");
    assert!(
        v.message
            .as_deref()
            .is_some_and(|m| m.contains("PROPTEST_CASES=1000")),
        "{v:?}"
    );
    assert!(v.note.is_some(), "a property decided it: {v:?}");
    assert_eq!(v.proptest_rerun_cases, Some(1000));
    // A survivor of the re-run says so instead of suggesting it again.
    let results = test(&cfg, &paths, std::slice::from_ref(&equivalent.id), 1000).expect("test");
    let v = &results[0].1;
    assert_eq!(v.kind, VerdictKind::Missed, "{v:?}");
    assert_eq!(v.proptest_rerun_cases, Some(1000));
    let line = cargo_quicksilver::exec::edited::verdict_line(&equivalent, v);
    assert!(
        line.contains("still missed with PROPTEST_CASES=1000"),
        "{line}"
    );
    assert!(!line.contains("try `test"), "{line}");
    assert!(serde_json::to_string(v)
        .unwrap()
        .contains("\"proptest_rerun_cases\":1000"));

    // An edit outside any body rejects the round, unless edits outside
    // bodies are skipped: then the body edit is kept and the comment dropped.
    edit(
        "pub fn flag() -> bool {\n    true\n",
        "pub fn flag() -> bool {\n    false\n",
    );
    edit("pub fn flag()", "// a note an agent added\npub fn flag()");
    let report = run_build(true);
    assert!(!report.rejected.is_empty());
    assert!(report.captured.is_empty());
    let report = build_with(Vec::new(), true);
    assert!(report.rejected.is_empty(), "{:?}", report.rejected);
    assert_eq!(report.dropped.len(), 1, "{:?}", report.dropped);
    assert_eq!(report.captured.len(), 1, "{:?}", report.captured);
    let flag_m = report.captured[0].clone();
    assert!(flag_m.fqns().ends_with(".flag"), "{flag_m:?}");
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

    let results = test(&cfg, &paths, &[], 0).expect("test");
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
        (&flag_m, "flag_is_true"),
        (&nested_m, "parse_pair_works"),
        (&impl_ms[0], "impl_and_dyn_work"),
        (&impl_ms[1], "impl_and_dyn_work"),
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
    let shown = cargo_quicksilver::exec::edited::verdict_line(&missed_m, v);
    assert!(
        shown.contains("intent: returns the opposite constant")
            && shown.contains("crate_a/src/lib.rs:9")
            && shown.contains("-     true")
            && shown.contains("+     false"),
        "{shown}"
    );
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
    assert!(test(&cfg, &paths, &[], 0).unwrap().is_empty());
    // The checkout was only read.
    assert_eq!(fs::read_to_string(&lib).unwrap(), original);
    assert!(!Path::new(&new_file).exists());
    // Every sandbox from build and test is gone too, and the summary
    // removes the run's directory and its record.
    let summary = cargo_quicksilver::sandbox::summary().expect("sandboxes were used");
    assert!(summary.contains("all deleted"), "{summary}");
    assert!(!root.exists(), "{}", root.display());
    assert!(!paths
        .session
        .join("sandbox")
        .join(std::process::id().to_string())
        .exists());
}

fn status_of(results: &[(EditedMutant, Status)], m: &EditedMutant) -> Status {
    results
        .iter()
        .find(|(r, _)| r.id == m.id)
        .map(|(_, s)| s.clone())
        .unwrap_or_else(|| panic!("{} in results", m.id))
}

/// bifrost-store's test binary installs a counting `#[global_allocator]`. Its
/// methods get prologues, and the runtime allocates while it initializes, so
/// every test in such a binary deadlocked at startup on the runtime's own
/// `LazyLock` and the map timed it out.
#[test]
fn a_crate_with_its_own_global_allocator_records_its_tests() {
    if !cargo_quicksilver::cargo_cmd::nextest_available() {
        eprintln!("skip: nextest not available");
        return;
    }
    let repo = support::setup_fixture("allocator");
    let cfg = support::cfg_for(repo.path());
    let paths = cfg.paths();
    let map = build_map(&cfg, &paths).expect("map");
    assert!(map.not_green.is_empty(), "{:?}", map.not_green);
    assert!(
        map.sites
            .iter()
            .any(|s| s.fqn.ends_with("CountingAllocator.alloc")),
        "the allocator's methods stay sites: {:?}",
        map.sites.iter().map(|s| &s.fqn).collect::<Vec<_>>()
    );
    let repeat = map
        .sites
        .iter()
        .find(|s| s.fqn.ends_with(".repeat"))
        .expect("repeat is a site")
        .site_id;
    assert!(
        map.hits.values().any(|hits| hits.contains(&repeat)),
        "the test that calls repeat was recorded: {:?}",
        map.hits
    );
}
