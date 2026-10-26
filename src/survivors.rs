//! Review captured mutants that selected tests did not catch.

use crate::cli::{CommonOpts, SurvivorsArgs};
use crate::config::ConfigExtras;
use crate::exec::edited::{State, Status};
use crate::sitemap::SiteMap;
use crate::types::VerdictKind;
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct EditView {
    pub fqn: String,
    pub path: String,
    pub line: usize,
    pub diff: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Survivor {
    pub id: String,
    pub fqns: String,
    pub path: String,
    pub line: usize,
    pub end_line: usize,
    pub intent: Option<String>,
    pub tests_run: usize,
    pub tests_selected: usize,
    pub tests_sample: Vec<String>,
    /// Selected tests that could not judge the mutant: they fail under the
    /// schema binary with no mutant active.
    pub unusable_tests: Vec<String>,
    pub at_head: Option<bool>,
    pub at_commit: String,
    pub at_status: AtStatus,
    pub edits: Vec<EditView>,
}

/// A unified diff of one body, with the file's line numbers (`line` is the
/// body's first line) and 3 lines of context. A stored body ends at its
/// `}`; in the file a newline follows, so both texts get one, and the diff
/// applies to the file.
pub fn unified_diff(old: &str, new: &str, line: usize, path: &str, prefix: &str) -> String {
    let with_newline = |s: &str| {
        if s.ends_with('\n') {
            s.to_string()
        } else {
            format!("{s}\n")
        }
    };
    let (old, new) = (with_newline(old), with_newline(new));
    let a = crate::linediff::lines(&old);
    let b = crate::linediff::lines(&new);
    let mut out = format!("--- a/{prefix}{path}\n+++ b/{prefix}{path}\n");
    let hunks = crate::linediff::diff(&a, &b);
    let mut groups: Vec<Vec<crate::linediff::Hunk>> = Vec::new();
    for h in hunks {
        if groups
            .last()
            .is_some_and(|g| h.a.start.saturating_sub(g.last().unwrap().a.end) <= 6)
        {
            groups.last_mut().unwrap().push(h);
        } else {
            groups.push(vec![h]);
        }
    }
    for group in groups {
        let first = &group[0];
        let last = group.last().unwrap();
        let start = first.a.start.saturating_sub(3);
        let end = (last.a.end + 3).min(a.len());
        let new_start = first.b.start.saturating_sub(3);
        let new_end = (last.b.end + 3).min(b.len());
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            line + start,
            end - start,
            line + new_start,
            new_end - new_start
        ));
        let mut ai = start;
        let mut bi = new_start;
        for h in group {
            while ai < h.a.start {
                push_diff_line(&mut out, ' ', a[ai]);
                ai += 1;
            }
            for s in &a[h.a.clone()] {
                push_diff_line(&mut out, '-', s);
            }
            for s in &b[h.b.clone()] {
                push_diff_line(&mut out, '+', s);
            }
            ai = h.a.end;
            bi = h.b.end;
        }
        while ai < end && bi < new_end {
            push_diff_line(&mut out, ' ', a[ai]);
            ai += 1;
            bi += 1;
        }
    }
    out
}

fn push_diff_line(out: &mut String, prefix: char, line: &str) {
    out.push(prefix);
    out.push_str(line.trim_end_matches('\n'));
    out.push('\n');
    if !line.ends_with('\n') {
        out.push_str("\\ No newline at end of file\n");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AtStatus {
    Unchanged,
    Changed,
    Gone,
    Unknown,
}

impl AtStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Changed => "changed",
            Self::Gone => "gone",
            Self::Unknown => "unknown",
        }
    }
}

type FileCache = BTreeMap<String, Option<(String, Vec<crate::locate::Located>)>>;

fn status_at(
    prefix: &str,
    root: &Path,
    commit: &str,
    cache: &mut FileCache,
    edit: &crate::capture::BodyEdit,
) -> AtStatus {
    let key = format!("{}{}", prefix, edit.path);
    let file = cache.entry(key.clone()).or_insert_with(|| {
        match crate::worktree::show(root, commit, &key) {
            Err(_) => Some((String::new(), vec![])),
            Ok(bytes) => String::from_utf8(bytes).ok().and_then(|src| {
                crate::locate::functions(&src)
                    .ok()
                    .map(|functions| (src, functions))
            }),
        }
    });
    let Some((source, functions)) = file else {
        return AtStatus::Unknown;
    };
    let matches: Vec<_> = functions.iter().filter(|f| f.matches(&edit.fqn)).collect();
    if matches.is_empty() {
        AtStatus::Gone
    } else if matches
        .iter()
        .any(|f| source[f.body.clone()] == edit.original)
    {
        AtStatus::Unchanged
    } else {
        AtStatus::Changed
    }
}

pub fn collect(
    map: &SiteMap,
    state: &State,
    root: &Path,
    head: &str,
    packages: &[String],
    files: Option<&globset::GlobSet>,
) -> Vec<Survivor> {
    let mut cache = BTreeMap::new();
    let mut out = Vec::new();
    for record in &state.records {
        let Status::Tested { verdict, .. } = &record.status else {
            continue;
        };
        if verdict.kind != VerdictKind::Missed {
            continue;
        }
        let Some(first) = record.mutant.edits.first() else {
            continue;
        };
        if !packages.is_empty() && !packages.contains(&first.package) {
            continue;
        }
        if files.is_some_and(|g| !g.is_match(&first.path)) {
            continue;
        }
        let Some(site) = map.site(first.site_id) else {
            continue;
        };
        let selected = map.tests_entering(
            &record
                .mutant
                .edits
                .iter()
                .map(|e| e.site_id)
                .collect::<Vec<_>>(),
        );
        let edits: Vec<_> = record
            .mutant
            .edits
            .iter()
            .map(|edit| EditView {
                fqn: edit.fqn.clone(),
                path: edit.path.clone(),
                line: edit.line,
                diff: unified_diff(
                    &edit.original,
                    &edit.body,
                    edit.line,
                    &edit.path,
                    &map.prefix,
                ),
            })
            .collect();
        let at_status = status_at(&map.prefix, root, head, &mut cache, first);
        let at_head = match at_status {
            AtStatus::Unchanged | AtStatus::Changed => Some(true),
            AtStatus::Gone => Some(false),
            AtStatus::Unknown => None,
        };
        out.push(Survivor {
            id: record.mutant.id.clone(),
            fqns: record.mutant.fqns(),
            path: site.path.clone(),
            line: site.start_line,
            end_line: site.end_line,
            intent: record.mutant.intent.clone(),
            tests_run: verdict.tests_run.len(),
            tests_selected: selected.len(),
            tests_sample: selected.iter().take(5).map(|t| t.id.clone()).collect(),
            unusable_tests: verdict.unusable_tests.clone(),
            at_head,
            at_commit: head.to_string(),
            at_status,
            edits,
        });
    }
    out.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    out
}

pub fn text_block(s: &Survivor) -> String {
    let mut out = format!("{} {} {}:{}-{}\n", s.id, s.fqns, s.path, s.line, s.end_line);
    if let Some(intent) = &s.intent {
        out.push_str(&format!("  intent: {intent}\n"));
    }
    out.push_str(&format!(
        "  tests: {} ran, {} selected; sample: {}\n  at {}: {}\n",
        s.tests_run,
        s.tests_selected,
        s.tests_sample.join(", "),
        &s.at_commit[..12.min(s.at_commit.len())],
        s.at_status.as_str()
    ));
    if !s.unusable_tests.is_empty() {
        out.push_str(&format!(
            "  could not be judged by (they fail with no mutant active): {}\n",
            s.unusable_tests.join(", ")
        ));
    }
    for edit in &s.edits {
        out.push_str(&edit.diff);
    }
    out.push_str("\n---\n");
    out
}

pub fn split(survivors: &[Survivor], n: usize) -> Vec<Vec<usize>> {
    assert!(n > 0);
    let mut files: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, s) in survivors.iter().enumerate() {
        files.entry(&s.path).or_default().push(i);
    }
    let mut files: Vec<_> = files.into_iter().collect();
    files.sort_by_key(|(_, ids)| std::cmp::Reverse(ids.len()));
    let mut buckets = vec![Vec::new(); n];
    let mut totals = vec![0usize; n];
    for (_, ids) in files {
        let at = (0..n).min_by_key(|&i| totals[i]).unwrap();
        totals[at] += ids.len();
        buckets[at].extend(ids);
    }
    buckets
}

pub fn run(common: &CommonOpts, args: SurvivorsArgs) -> Result<i32> {
    let (cfg, _) = crate::engine::config_from_cli(common, ConfigExtras::default())?;
    let head =
        crate::worktree::resolve_commit(&cfg.repo_root, args.at.as_deref().unwrap_or("HEAD"))?;
    let paths = cfg.paths();
    let map = SiteMap::load(&paths)?.ok_or_else(|| {
        anyhow::anyhow!("no map for this repository; run `cargo quicksilver map` first")
    })?;
    let state = State::load(&paths)?;
    let files = crate::discover::file_globset(&cfg.files)?;
    let records = collect(
        &map,
        &state,
        &cfg.repo_root,
        &head,
        &cfg.packages,
        files.as_ref(),
    );
    let gone = records
        .iter()
        .filter(|s| s.at_status == AtStatus::Gone)
        .count();
    let changed = records
        .iter()
        .filter(|s| s.at_status == AtStatus::Changed)
        .count();
    let source_files: BTreeSet<_> = records.iter().map(|s| s.path.as_str()).collect();
    eprintln!(
        "{} survivors in {} files; {} gone, {} changed at {}",
        records.len(),
        source_files.len(),
        gone,
        changed,
        &head[..12.min(head.len())]
    );
    for s in &records {
        print!("{}", text_block(s));
    }
    if let Some(n) = args.split {
        let dir = args.out.as_ref().unwrap();
        std::fs::create_dir_all(dir)?;
        for (i, bucket) in split(&records, n).iter().enumerate() {
            let path = dir.join(format!("survivors-{}.txt", i + 1));
            let content: String = bucket.iter().map(|&i| text_block(&records[i])).collect();
            crate::util::write_atomic(&path, content)?;
            let files: BTreeSet<_> = bucket.iter().map(|&i| records[i].path.as_str()).collect();
            eprintln!(
                "{}: {} survivors in {} source files",
                path.display(),
                bucket.len(),
                files.len()
            );
        }
    }
    if let Some(path) = &cfg.json {
        crate::util::write_atomic(path, serde_json::to_vec_pretty(&records)?)?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revision_compares_bodies_and_caches_each_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        git(&["init", "-q"]);
        let source = "fn changed() { 1; } fn gone() { 1; } fn same() { 1; }";
        std::fs::write(root.join("lib.rs"), source).unwrap();
        git(&["add", "lib.rs"]);
        git(&[
            "-c",
            "user.name=QS",
            "-c",
            "user.email=qs@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "map commit",
        ]);
        let first = crate::worktree::head(root).unwrap();
        std::fs::write(root.join("lib.rs"), "fn changed() { 2; } fn same() { 1; }").unwrap();
        git(&["add", "lib.rs"]);
        git(&[
            "-c",
            "user.name=QS",
            "-c",
            "user.email=qs@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "later commit",
        ]);
        git(&["tag", "review"]);
        let at = crate::worktree::resolve_commit(root, "review").unwrap();
        assert_ne!(at, first);
        let mut cache = FileCache::new();
        for (name, expected) in [
            ("changed", AtStatus::Changed),
            ("gone", AtStatus::Gone),
            ("same", AtStatus::Unchanged),
        ] {
            let edit = crate::capture::BodyEdit {
                site_id: 1,
                package: "p".into(),
                path: "lib.rs".into(),
                fqn: format!("p.{name}"),
                line: 1,
                original: "{ 1; }".into(),
                body: "{}".into(),
            };
            assert_eq!(status_at("", root, &at, &mut cache, &edit), expected);
            assert_eq!(
                status_at("", root, &first, &mut FileCache::new(), &edit),
                AtStatus::Unchanged
            );
        }
        assert_eq!(cache.len(), 1);
        assert!(crate::worktree::resolve_commit(root, "missing-revision")
            .unwrap_err()
            .to_string()
            .contains("unknown commit revision"));
    }

    #[test]
    fn unified_diff_formats_single_and_edge_changes() {
        assert_eq!(
            unified_diff("{\n old\n}", "{\n new\n}", 1, "x.rs", ""),
            "--- a/x.rs\n+++ b/x.rs\n@@ -1,3 +1,3 @@\n {\n- old\n+ new\n }\n"
        );
    }

    #[test]
    fn unified_diff_keeps_context_at_body_edges() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\n";
        let new = "A\nb\nc\nd\ne\nf\ng\nh\nI\n";
        let diff = unified_diff(old, new, 20, "x.rs", "");
        assert_eq!(diff.matches("@@ ").count(), 2, "{diff}");
        assert!(diff.contains("@@ -20,4 +20,4 @@"), "{diff}");
        assert!(diff.contains("@@ -25,4 +25,4 @@"), "{diff}");
    }
    #[test]
    fn unified_diff_combines_changes_within_three_context_lines() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nB\nc\nd\ne\nf\nG\nh\n";
        let diff = unified_diff(old, new, 4, "x.rs", "");
        assert_eq!(diff.matches("@@ ").count(), 1, "{diff}");
        assert!(diff.contains("@@ -4,8 +4,8 @@"), "{diff}");
    }
    #[test]
    fn split_keeps_source_files_whole() {
        let make = |id: &str, path: &str| Survivor {
            id: id.into(),
            fqns: String::new(),
            path: path.into(),
            line: 1,
            end_line: 1,
            intent: None,
            tests_run: 0,
            tests_selected: 0,
            tests_sample: vec![],
            unusable_tests: vec![],
            at_head: None,
            at_commit: "abc".into(),
            at_status: AtStatus::Unknown,
            edits: vec![],
        };
        let v = vec![
            make("1", "a"),
            make("2", "a"),
            make("3", "b"),
            make("4", "c"),
        ];
        let buckets = split(&v, 2);
        let mut file_lane = BTreeMap::new();
        for (lane, bucket) in buckets.iter().enumerate() {
            for &i in bucket {
                assert!(file_lane
                    .insert(v[i].path.as_str(), lane)
                    .is_none_or(|seen| seen == lane));
            }
        }
    }
}
