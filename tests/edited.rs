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
    // Tests that exist only under a cfg are mapped when [build-env] sets it.
    cfg.build_env
        .insert("RUSTFLAGS".into(), "--cfg qs_fixture_cfg".into());
    // A timing guard that fails under the switch is left out by name.
    cfg.skip_tests = vec!["tiny_schema::*::timing_guard_*".into()];
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
    assert_eq!(
        map.skipped_tests,
        vec!["tiny_schema::tests::timing_guard_red_under_quicksilver".to_string()]
    );
    assert!(!map
        .tests
        .iter()
        .any(|t| t.id.ends_with("timing_guard_red_under_quicksilver")));
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
    let gated = site(".cfg_gated_target").site_id;
    assert!(
        map.hits
            .get("tiny_schema::tests::only_with_the_cfg")
            .is_some_and(|h| h.contains(&gated)),
        "the cfg-gated test is mapped: {:?}",
        map.hits.keys()
    );
    assert_eq!(
        map.build_env.get("RUSTFLAGS").map(String::as_str),
        Some("--cfg qs_fixture_cfg")
    );
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
            patches: patches.into_iter().map(Into::into).collect(),
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
    // Pending reads the actual captured state without rebuilding anything.
    let pending_dir = tempfile::tempdir().unwrap();
    let pending_json = pending_dir.path().join("pending.json");
    let split_dir = pending_dir.path().join("pending-lanes");
    let pending_output = std::process::Command::new(env!("CARGO_BIN_EXE_cargo-quicksilver"))
        .current_dir(repo.path())
        .args([
            "pending",
            "--package",
            "tiny_a",
            "--file",
            "crate_a/src/lib.rs",
            "--json",
            pending_json.to_str().unwrap(),
            "--split",
            "2",
            "--out",
            split_dir.to_str().unwrap(),
        ])
        .output()
        .expect("run pending");
    assert!(
        pending_output.status.success(),
        "{}",
        String::from_utf8_lossy(&pending_output.stderr)
    );
    let rows: Vec<serde_json::Value> =
        serde_json::from_slice(&fs::read(&pending_json).unwrap()).unwrap();
    assert!(!rows
        .iter()
        .any(|r| r["fqn"].as_str().unwrap().ends_with(".add")));
    assert!(rows
        .iter()
        .any(|r| r["fqn"].as_str().unwrap().ends_with(".flag")));
    assert!(rows.iter().all(|r| r["package"] == "tiny_a"
        && r["path"] == "crate_a/src/lib.rs"
        && r["mutants"] == 0));
    let printed = String::from_utf8_lossy(&pending_output.stdout);
    for row in &rows {
        let start = row["line"].as_u64().unwrap();
        let end = row["end_line"].as_u64().unwrap();
        assert_eq!(end - start + 1, row["lines"].as_u64().unwrap());
        assert!(printed.contains(&format!(
            "{}:{start}-{end} {} ({} lines)",
            row["path"].as_str().unwrap(),
            row["fqn"].as_str().unwrap(),
            row["lines"]
        )));
    }
    let lanes: Vec<_> = (1..=2)
        .map(|i| fs::read_to_string(split_dir.join(format!("pending-{i}.txt"))).unwrap())
        .collect();
    assert_eq!(
        lanes.iter().map(|s| s.lines().count()).sum::<usize>(),
        rows.len()
    );
    let totals: Vec<usize> = lanes
        .iter()
        .map(|lane| {
            rows.iter()
                .filter(|r| lane.contains(r["fqn"].as_str().unwrap()))
                .map(|r| r["lines"].as_u64().unwrap() as usize)
                .sum()
        })
        .collect();
    let largest = rows
        .iter()
        .map(|r| r["lines"].as_u64().unwrap() as usize)
        .max()
        .unwrap();
    assert!(totals[0].abs_diff(totals[1]) <= largest);
    let all_output = std::process::Command::new(env!("CARGO_BIN_EXE_cargo-quicksilver"))
        .current_dir(repo.path())
        .args([
            "pending",
            "--all",
            "--package",
            "tiny_a",
            "--min-lines",
            "3",
        ])
        .output()
        .unwrap();
    assert!(all_output.status.success());
    assert!(String::from_utf8_lossy(&all_output.stdout)
        .lines()
        .any(|line| line.contains("tiny_a.add (3 lines) [1 mutants]")));
    let state = cargo_quicksilver::exec::edited::State::load(&paths).unwrap();
    let inventory = cargo_quicksilver::pending::inventory(&map, &state, 1);
    assert_eq!(
        inventory
            .iter()
            .find(|r| r.fqn.ends_with(".add"))
            .unwrap()
            .mutants,
        1
    );
    assert!(!inventory.iter().any(|r| r.fqn.ends_with(".uncovered")));
    let mut changed_state = state.clone();
    changed_state.map_id = "another map".into();
    assert_eq!(
        cargo_quicksilver::pending::inventory(&map, &changed_state, 1)
            .iter()
            .find(|r| r.fqn.ends_with(".add"))
            .unwrap()
            .mutants,
        1
    );
    for record in &mut changed_state.records {
        record.status = Status::Unviable {
            message: "does not compile".into(),
        };
    }
    assert!(
        cargo_quicksilver::pending::inventory(&map, &changed_state, 1)
            .iter()
            .all(|r| r.mutants == 0)
    );
    for field in ["site_id", "path", "fqn"] {
        let mut changed = state.clone();
        for record in &mut changed.records {
            for edit in &mut record.mutant.edits {
                match field {
                    "site_id" => edit.site_id = edit.site_id.wrapping_add(1),
                    "path" => edit.path.push_str(".other"),
                    _ => edit.fqn.push_str(".other"),
                }
            }
        }
        assert!(cargo_quicksilver::pending::inventory(&map, &changed, 1)
            .iter()
            .all(|r| r.mutants == 0));
    }
    assert!(cargo_quicksilver::pending::inventory(&map, &state, usize::MAX).is_empty());
    let mut scaffolding_map = map.clone();
    for site in &mut scaffolding_map.sites {
        site.scaffolding = Some("helper".into());
    }
    assert!(cargo_quicksilver::pending::inventory(&scaffolding_map, &state, 1).is_empty());
    reset();

    // Capturing the same edit again is a duplicate, not a second mutant.
    edit("    a + b\n", "    a - b\n");
    edit("\"hello\".into()", "\"bye\".into()");
    let report = build_full(
        Vec::new(),
        false,
        &[
            (
                "tiny_a.add",
                "a different note that must not replace the first",
            ),
            ("tiny_a.greeting", "uses the wrong greeting"),
        ],
    );
    assert_eq!(report.duplicates.len(), 2);
    assert!(report.captured.is_empty());
    let kept_add = by_fqn(
        &report
            .results
            .iter()
            .map(|(m, _)| m.clone())
            .collect::<Vec<_>>(),
        ".add",
    );
    let kept_greeting = by_fqn(
        &report
            .results
            .iter()
            .map(|(m, _)| m.clone())
            .collect::<Vec<_>>(),
        ".greeting",
    );
    assert_eq!(kept_add.intent, add_m.intent);
    assert_eq!(
        kept_greeting.intent.as_deref(),
        Some("uses the wrong greeting")
    );
    reset();

    // Two more mutants of `add` in one build: one from a patch (with no
    // checkout to apply it to), one from the checkout. A body that does not
    // type-check is unviable with rustc's reason; the rest still build.
    edit("    a + b\n", "    a * b\n");
    let patch = repo.path().join("../mul.patch");
    fs::write(&patch, git_diff()).unwrap();
    reset();
    edit("    a + b\n", "    \"x\"\n");
    let bad_patch = repo.path().join("../bad.patch");
    fs::write(&bad_patch, git_diff()).unwrap();
    reset();
    let mul_notes = repo.path().join("../mul-notes.json");
    let bad_notes = repo.path().join("../bad-notes.json");
    fs::write(
        &mul_notes,
        r#"{"tiny_a.add":"multiplies instead of adding"}"#,
    )
    .unwrap();
    fs::write(
        &bad_notes,
        r#"{"crate_a/src/lib.rs:2":"returns text instead of a number"}"#,
    )
    .unwrap();
    let sources = CaptureSources {
        patches: vec![
            format!("{}={}", patch.display(), mul_notes.display())
                .parse()
                .unwrap(),
            format!("{}={}", bad_patch.display(), bad_notes.display())
                .parse()
                .unwrap(),
        ],
        ..CaptureSources::default()
    };
    let report = build(&cfg, &paths, &sources).unwrap();
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
    assert_eq!(
        mul_m.intent.as_deref(),
        Some("multiplies instead of adding")
    );
    assert_eq!(
        bad.intent.as_deref(),
        Some("returns text instead of a number")
    );
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
    let results = test(&cfg, &paths, std::slice::from_ref(&lower.id), 0, &[]).expect("test");
    assert_eq!(results.len(), 1);
    let v = &results[0].1;
    assert_eq!(v.kind, VerdictKind::Missed, "{v:?}");
    assert!(v.note.is_some(), "{v:?}");
    let line = cargo_quicksilver::exec::edited::verdict_line(&lower, v);
    assert!(line.contains("proptest properties only"), "{line}");
    assert!(line.contains("try `test --proptest-cases"), "{line}");
    assert_eq!(v.proptest_rerun_cases, None);
    let results = test(&cfg, &paths, std::slice::from_ref(&same.id), 1000, &[]).expect("test");
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
    let results = test(
        &cfg,
        &paths,
        std::slice::from_ref(&equivalent.id),
        1000,
        &[],
    )
    .expect("test");
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

    let results = test(&cfg, &paths, &[], 0, &[]).expect("test");
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
    let state = cargo_quicksilver::exec::edited::State::load(&paths).unwrap();
    let head = cargo_quicksilver::worktree::head(repo.path()).unwrap();
    let survivors =
        cargo_quicksilver::survivors::collect(&map, &state, repo.path(), &head, &[], None);
    let survivor = survivors
        .iter()
        .find(|s| s.id == missed_m.id)
        .expect("always_ok survivor");
    assert_eq!(
        survivor.intent.as_deref(),
        Some("returns the opposite constant")
    );
    assert_eq!(survivor.at_head, Some(true));
    assert!(survivor.edits[0].diff.contains("-    true"));
    assert!(survivor.edits[0].diff.contains("+    false"));

    // Apply and prove the real captured survivor in the author's checkout.
    // Scope grep to always_ok: maybe_hang has a false body even at baseline.
    assert_eq!(missed_m.edits.len(), 1);
    assert_eq!(missed_m.edits[0].original, "{\n    true\n}");
    assert_eq!(missed_m.edits[0].body, "{\n    false\n}");
    let before_apply = fs::read(&lib).unwrap();
    let qs = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_cargo-quicksilver"))
            .current_dir(repo.path())
            .args(args)
            .output()
            .expect("run QS")
    };
    let check_output = |output: &std::process::Output| {
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let scoped_command =
        r#"! sed -n "/pub fn always_ok()/,/^}/p" crate_a/src/lib.rs | grep -q "    false""#;
    let grep_exit = || {
        std::process::Command::new("sh")
            .args(["-c", scoped_command])
            .current_dir(repo.path())
            .status()
            .unwrap()
            .code()
    };
    assert_eq!(grep_exit(), Some(0), "original must pass before prove");
    let applied = qs(&["apply", &missed_m.id[..8]]);
    check_output(&applied);
    assert!(String::from_utf8_lossy(&applied.stdout)
        .contains(&format!("applied {} to crate_a/src/lib.rs", missed_m.id)));
    assert_ne!(fs::read(&lib).unwrap(), before_apply);
    assert_eq!(grep_exit(), Some(1), "mutant must fail before prove");
    check_output(&qs(&["apply", &missed_m.id, "--revert"]));
    assert_eq!(fs::read(&lib).unwrap(), before_apply);
    let proven = qs(&["prove", &missed_m.id, "--", "sh", "-c", scoped_command]);
    check_output(&proven);
    let final_line = format!(
        "caught: {} {}: the command fails with the mutant and passes without it",
        missed_m.id,
        missed_m.fqns()
    );
    assert_eq!(
        String::from_utf8_lossy(&proven.stdout).lines().last(),
        Some(final_line.as_str())
    );
    assert_eq!(fs::read(&lib).unwrap(), before_apply);

    // A command spawn error uses exit 3 and still restores the checkout.
    let spawn_error = qs(&["prove", &missed_m.id, "--", "./missing-qs-proof-command"]);
    assert_eq!(spawn_error.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&spawn_error.stderr).contains("start command"));
    assert_eq!(fs::read(&lib).unwrap(), before_apply);

    // The command sends the first Ctrl-C to QS while the mutant is active.
    // QS must stay alive long enough to revert, then stop: an interrupted
    // command's exit status proves nothing.
    #[cfg(unix)]
    {
        let signal_command = r#"if sed -n "/pub fn always_ok()/,/^}/p" crate_a/src/lib.rs | grep -q "    false"; then kill -INT "$PPID"; kill -INT $$; fi"#;
        let interrupted = qs(&["prove", &missed_m.id, "--", "sh", "-c", signal_command]);
        assert_eq!(interrupted.status.code(), Some(3));
        assert!(
            String::from_utf8_lossy(&interrupted.stderr)
                .contains("interrupted; the mutant is reverted"),
            "{}",
            String::from_utf8_lossy(&interrupted.stderr)
        );
        assert!(String::from_utf8_lossy(&interrupted.stdout).contains("reverted"));
        assert_eq!(fs::read(&lib).unwrap(), before_apply);
    }

    let split_dir = tempfile::tempdir().unwrap();
    let buckets = cargo_quicksilver::survivors::split(&survivors, 3);
    for (lane, bucket) in buckets.iter().enumerate() {
        let content: String = bucket
            .iter()
            .map(|&i| cargo_quicksilver::survivors::text_block(&survivors[i]))
            .collect();
        fs::write(
            split_dir.path().join(format!("survivors-{}.txt", lane + 1)),
            content,
        )
        .unwrap();
    }
    let paths_for_survivors: Vec<_> = (1..=3)
        .map(|i| fs::read_to_string(split_dir.path().join(format!("survivors-{i}.txt"))).unwrap())
        .collect();
    for path in survivors
        .iter()
        .map(|s| s.path.as_str())
        .collect::<std::collections::BTreeSet<_>>()
    {
        let lanes: Vec<_> = paths_for_survivors
            .iter()
            .enumerate()
            .filter(|(_, text)| {
                survivors
                    .iter()
                    .any(|s| s.path == path && text.contains(&s.id))
            })
            .map(|(i, _)| i)
            .collect();
        assert!(
            lanes.len() <= 1,
            "survivors from {path} were split across lanes: {lanes:?}"
        );
    }
    assert!(paths_for_survivors
        .iter()
        .any(|text| text.contains(&missed_m.id)));
    assert_eq!(verdict(&uncovered_m).kind, VerdictKind::NotCovered);
    // Tests are named by id, with their binary.
    assert_eq!(
        verdict(&add_m).test.as_deref(),
        Some("tiny_a::tests::add_works")
    );
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
            .any(|t| t == "tiny_c::tests::calls_always_ok_without_asserting"),
        "{v:?}"
    );
    assert!(
        !results.iter().any(|(m, _)| m.id == bad.id),
        "unviable mutants are not run"
    );

    // Tested mutants are final: a second test run has nothing to do.
    assert!(test(&cfg, &paths, &[], 0, &[]).unwrap().is_empty());
    // Each verdict was saved; the journal is folded into the state.
    use cargo_quicksilver::exec::edited::{State, Status as S};
    let tested = || {
        State::load(&paths)
            .unwrap()
            .records
            .iter()
            .filter(|r| matches!(r.status, S::Tested { .. }))
            .count()
    };
    let tested_before = tested();
    assert!(tested_before > 0);
    assert!(!State::journal_path(&paths).exists());
    // A new map of the same commit, with the same test packages, keeps the
    // mutants and their verdicts: nothing to capture, build or test again.
    build_map(&cfg, &paths).expect("map the same commit again");
    assert!(test(&cfg, &paths, &[], 0, &[]).unwrap().is_empty());
    assert_eq!(tested(), tested_before);
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

/// This fixture's test binary installs a counting `#[global_allocator]`. Its
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

/// A checkout at another commit reads the same captured state and matches
/// function bodies after surrounding text has moved.
#[test]
fn apply_uses_shared_state_in_another_checkout_at_a_newer_commit() {
    let repo = support::setup_tiny();
    let cfg = support::cfg_for(repo.path());
    let state = cargo_quicksilver::exec::edited::State {
        commit: cargo_quicksilver::worktree::head(repo.path()).unwrap(),
        records: vec![cargo_quicksilver::exec::edited::Record {
            mutant: EditedMutant {
                id: "0123456789abcdef".into(),
                edits: vec![cargo_quicksilver::capture::BodyEdit {
                    site_id: 1,
                    package: "tiny_a".into(),
                    path: "crate_a/src/lib.rs".into(),
                    fqn: "tiny_a.always_ok".into(),
                    line: 9,
                    original: "{\n    true\n}".into(),
                    body: "{\n    false\n}".into(),
                }],
                intent: None,
            },
            status: Status::Pending,
            captured_at: String::new(),
        }],
        ..Default::default()
    };
    state.save(&cfg.paths()).unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let git = |root: &Path, args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "--detach",
            checkout.path().to_str().unwrap(),
            "HEAD",
        ],
    );
    let lib = checkout.path().join("crate_a/src/lib.rs");
    let original = fs::read_to_string(&lib).unwrap();
    let newer = format!("// moved after the map\n{original}");
    fs::write(&lib, &newer).unwrap();
    git(checkout.path(), &["add", "crate_a/src/lib.rs"]);
    git(
        checkout.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "move functions",
        ],
    );
    assert_ne!(
        cargo_quicksilver::worktree::head(checkout.path()).unwrap(),
        state.commit
    );
    for args in [
        vec!["apply", "01234567"],
        vec!["apply", "01234567", "--revert"],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_cargo-quicksilver"))
            .current_dir(checkout.path())
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = if args.len() == 2 {
            assert!(String::from_utf8_lossy(&output.stdout).contains("crate_a/src/lib.rs:10"));
            newer.replacen(
                "pub fn always_ok() -> bool {\n    true\n}",
                "pub fn always_ok() -> bool {\n    false\n}",
                1,
            )
        } else {
            newer.clone()
        };
        assert_eq!(fs::read_to_string(&lib).unwrap(), expected);
        assert_eq!(
            fs::read_to_string(repo.path().join("crate_a/src/lib.rs")).unwrap(),
            original
        );
    }
    git(
        repo.path(),
        &["worktree", "remove", checkout.path().to_str().unwrap()],
    );
}
