use super::*;
use std::collections::HashSet;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {:?}", out);
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn site(id: u32, path: &str, name: &str, start: usize, end: usize) -> MapSite {
    MapSite {
        site_id: id,
        salt: 0,
        package: "fixture".into(),
        path: path.into(),
        fqn: format!("Type.{name}"),
        fn_start_byte: 0,
        body_start_byte: 0,
        body_end_byte: 0,
        start_line: start,
        end_line: end,
        opaque_return: false,
        trait_impl: false,
        scaffolding: None,
    }
}

fn map(repo: &Path, sites: Vec<MapSite>) -> SiteMap {
    SiteMap {
        version: MAP_VERSION,
        map_id: "fixture".into(),
        commit: git(repo, &["rev-parse", "HEAD"]),
        prefix: String::new(),
        repo_root: repo.into(),
        target: String::new(),
        files: BTreeMap::new(),
        sites,
        test_lines: Default::default(),
        dropped: Vec::new(),
        crate_roots: BTreeMap::new(),
        test_packages: Vec::new(),
        build_env: BTreeMap::new(),
        tests: Vec::new(),
        hits: BTreeMap::new(),
        not_green: Vec::new(),
        skipped_tests: Vec::new(),
        not_green_output: Default::default(),
        property_tests: Default::default(),
        worker: PathBuf::new(),
        run_dir: PathBuf::new(),
        map_secs: 0.0,
    }
}

fn repository(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    for (path, contents) in files {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    git(dir.path(), &["add", "."]);
    git(
        dir.path(),
        &[
            "-c",
            "user.name=QS test",
            "-c",
            "user.email=qs@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    dir
}

fn check(map: &SiteMap, expected: &[u32]) {
    let sites: Vec<_> = map.sites.iter().collect();
    let actual = without_callers(map, &map.repo_root, &sites).unwrap();
    assert_eq!(actual, expected.iter().copied().collect::<HashSet<_>>());
    assert_eq!(
        actual,
        without_callers_old(map, &map.repo_root, &sites).unwrap()
    );
}

#[test]
fn own_body_and_own_tests_are_allowed() {
    let dir = repository(&[(
        "nested/a.rs",
        b"fn own() {\n own();\n}\nfn tests() { own(); tested(); }\nfn tested() {}\n",
    )]);
    let mut map = map(
        dir.path(),
        vec![
            site(1, "a.rs", "own", 1, 3),
            site(2, "a.rs", "tested", 5, 5),
        ],
    );
    map.prefix = "nested/".into();
    map.test_lines.insert("a.rs".into(), vec![(4, 4)]);
    check(&map, &[1, 2]);
}

#[test]
fn other_files_and_outside_prefix_are_callers() {
    let dir = repository(&[
        ("workspace/a.rs", b"fn cross() {}\nfn outside() {}\n"),
        ("workspace/deep/b.rs", b"cross();\n"),
        ("elsewhere.rs", b"outside();\n"),
    ]);
    let mut map = map(
        dir.path(),
        vec![
            site(1, "a.rs", "cross", 1, 1),
            site(2, "a.rs", "outside", 2, 2),
        ],
    );
    map.prefix = "workspace/".into();
    check(&map, &[]);
    // Running below the repository root must still inspect the full tree.
    assert!(without_callers(
        &map,
        &dir.path().join("workspace"),
        &map.sites.iter().collect::<Vec<_>>()
    )
    .unwrap()
    .is_empty());
}

#[test]
fn shared_names_are_checked_against_each_candidate() {
    let dir = repository(&[("a.rs", b"fn same() {}\nfn same() {}\n")]);
    check(
        &map(
            dir.path(),
            vec![site(1, "a.rs", "same", 1, 1), site(2, "a.rs", "same", 2, 2)],
        ),
        &[],
    );
    // One candidate's definition can enclose the other's definition.
    check(
        &map(
            dir.path(),
            vec![site(1, "a.rs", "same", 1, 2), site(2, "a.rs", "same", 2, 2)],
        ),
        &[1],
    );
}

#[test]
fn whole_unicode_words_and_lossy_text() {
    let dir = repository(&[
        ("a.rs", "fn new() {}\nfn café() {}\n".as_bytes()),
        ("b.rs", "new_thing anew new2 newé caféine\n".as_bytes()),
        ("c.rs", b"\xffnew\xff\r\n"),
    ]);
    check(
        &map(
            dir.path(),
            vec![site(1, "a.rs", "new", 1, 1), site(2, "a.rs", "café", 2, 2)],
        ),
        &[2],
    );
}

#[test]
fn binary_blobs_and_non_rust_files_are_skipped() {
    let dir = repository(&[
        ("a.rs", b"fn alone() {}\n"),
        ("binary.rs", b"alone();\n\0"),
        ("text.txt", b"alone();\n"),
    ]);
    check(&map(dir.path(), vec![site(1, "a.rs", "alone", 1, 1)]), &[1]);
}

#[test]
fn binary_detection_uses_only_first_8000_bytes() {
    let mut blob = vec![b' '; 8000];
    blob.extend_from_slice(b"\0alone();\n");
    let dir = repository(&[("a.rs", b"fn alone() {}\n"), ("b.rs", &blob)]);
    check(&map(dir.path(), vec![site(1, "a.rs", "alone", 1, 1)]), &[]);
}

#[test]
fn reads_map_commit_and_excludes_main_and_trait_impls() {
    let dir = repository(&[("a.rs", b"fn alone() {}\n")]);
    let mut trait_site = site(2, "a.rs", "trait_method", 1, 1);
    trait_site.trait_impl = true;
    let map = map(
        dir.path(),
        vec![
            site(1, "a.rs", "alone", 1, 1),
            trait_site,
            site(3, "a.rs", "main", 1, 1),
        ],
    );
    std::fs::write(dir.path().join("a.rs"), "alone();\nalone();\n").unwrap();
    check(&map, &[1]);
}

#[test]
fn stops_before_last_file_and_joins_writer_with_full_pipes() {
    let mut files = vec![("a.rs".to_owned(), b"called();\n".to_vec())];
    // Enough requests and replies to fill OS pipes if cleanup joins the
    // writer without first terminating the child.
    for i in 0..4000 {
        files.push((format!("z/{i:04}.rs"), vec![b' '; 100]));
    }
    let borrowed: Vec<_> = files
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .collect();
    let dir = repository(&borrowed);
    let map = map(dir.path(), vec![site(1, "missing.rs", "called", 1, 1)]);
    let (result, scanned) =
        scan_callers(&map, dir.path(), &map.sites.iter().collect::<Vec<_>>()).unwrap();
    assert!(result.is_empty());
    assert_eq!(scanned, 1);
    let absent = site(2, "missing.rs", "not_in_any_blob", 1, 1);
    let (result, scanned) = scan_callers(&map, dir.path(), &[&absent]).unwrap();
    assert_eq!(result, HashSet::from([2]));
    assert_eq!(scanned, files.len());
}

#[test]
fn no_candidates_or_no_rust_blobs() {
    let dir = repository(&[("a.txt", b"not Rust")]);
    let map = map(dir.path(), vec![site(1, "absent.rs", "absent", 1, 1)]);
    check(&map, &[1]);
    let mut trait_site = site(2, "a.rs", "method", 1, 1);
    trait_site.trait_impl = true;
    let main = site(3, "a.rs", "main", 1, 1);
    assert_eq!(
        scan_callers(&map, Path::new("/does-not-exist"), &[&trait_site, &main]).unwrap(),
        (HashSet::new(), 0)
    );
}

#[cfg(unix)]
#[test]
fn only_regular_blobs_including_executables_are_searched() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = repository(&[("a.rs", b"fn alone() {}\nfn called() {}\n")]);
    symlink("alone", dir.path().join("symlink.rs")).unwrap();
    std::fs::write(dir.path().join("executable.rs"), "called();\n").unwrap();
    std::fs::set_permissions(
        dir.path().join("executable.rs"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    git(dir.path(), &["add", "."]);
    let commit = git(dir.path(), &["rev-parse", "HEAD"]);
    git(
        dir.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{commit},module.rs"),
        ],
    );
    git(
        dir.path(),
        &[
            "-c",
            "user.name=QS test",
            "-c",
            "user.email=qs@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "file modes",
        ],
    );
    check(
        &map(
            dir.path(),
            vec![
                site(1, "a.rs", "alone", 1, 1),
                site(2, "a.rs", "called", 2, 2),
            ],
        ),
        &[1],
    );
}

// Reusable, read-only evidence harness. No map/build command runs in the
// subject repository. Choose the old implementation for a bounded baseline.
#[test]
#[ignore = "set QS_CALLER_REPO to a real repository for equivalence or timing"]
fn real_repository_caller_evidence() {
    let repo = PathBuf::from(std::env::var_os("QS_CALLER_REPO").expect("QS_CALLER_REPO"));
    let commit = git(&repo, &["rev-parse", "HEAD"]);
    let declarations = git(
        &repo,
        &[
            "grep",
            "-n",
            "-I",
            "-E",
            "fn [a-zA-Z_][a-zA-Z_0-9]*",
            &commit,
            "--",
            "*.rs",
        ],
    );
    let mut named = BTreeMap::new();
    for line in declarations.lines() {
        let Some(rest) = line.strip_prefix(&format!("{commit}:")) else {
            continue;
        };
        let mut parts = rest.splitn(3, ':');
        let (Some(path), Some(lineno), Some(text)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Some((_, after)) = text.split_once("fn ") else {
            continue;
        };
        let name = after
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap();
        if name.is_empty() {
            continue;
        }
        let Ok(lineno) = lineno.parse::<usize>() else {
            continue;
        };
        named
            .entry(name.to_owned())
            .or_insert((path.to_owned(), lineno));
    }
    let mut sites = Vec::new();
    let common = ["new", "fmt", "len", "from", "default", "build", "get"];
    for name in common
        .iter()
        .copied()
        .map(str::to_owned)
        .chain(
            named
                .keys()
                .filter(|n| !common.contains(&n.as_str()))
                .cloned(),
        )
        .take(1100)
    {
        let (path, line) = named.get(&name).cloned().unwrap_or(("absent.rs".into(), 1));
        sites.push(site(sites.len() as u32 + 1, &path, &name, line, line));
    }
    let map = map(&repo, sites);
    let sites: Vec<_> = map.sites.iter().collect();
    let now = std::time::Instant::now();
    let new = if std::env::var_os("QS_CALLER_OLD").is_some() {
        without_callers_old(&map, &repo, &sites).unwrap()
    } else {
        let (result, scanned) = scan_callers(&map, &repo, &sites).unwrap();
        eprintln!("blobs scanned: {scanned}");
        result
    };
    eprintln!(
        "repo={} commit={} candidates={} old={} elapsed={:?} uncalled={}",
        repo.display(),
        commit,
        sites.len(),
        std::env::var_os("QS_CALLER_OLD").is_some(),
        now.elapsed(),
        new.len()
    );
    if std::env::var_os("QS_CALLER_COMPARE").is_some() {
        let now = std::time::Instant::now();
        let old = without_callers_old(&map, &repo, &sites).unwrap();
        eprintln!("old elapsed={:?} uncalled={}", now.elapsed(), old.len());
        assert_eq!(new, old);
    }
}
