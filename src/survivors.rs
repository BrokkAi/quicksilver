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
    pub at_head: Option<bool>,
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

/// Whether the function still exists at `head`: a function with the same
/// name in the same file, and for a method, in an `impl` of the same type.
/// `None` when the file at `head` does not parse.
fn still_at_head(
    map: &SiteMap,
    root: &Path,
    head: &str,
    file_cache: &mut BTreeMap<String, Option<BTreeSet<String>>>,
    fqn: &str,
    path: &str,
) -> Option<bool> {
    if head == map.commit {
        return Some(true);
    }
    let key = format!("{}{}", map.prefix, path);
    if !file_cache.contains_key(&key) {
        let parsed = match crate::worktree::show(root, head, &key) {
            Err(_) => Some(BTreeSet::new()),
            Ok(bytes) => String::from_utf8(bytes)
                .ok()
                .and_then(|src| syn::parse_file(&src).ok())
                .map(|file| defined_names(&file)),
        };
        file_cache.insert(key.clone(), parsed);
    }
    file_cache.get(&key).and_then(|n| n.as_ref()).map(|names| {
        names
            .iter()
            .any(|n| fqn == n || fqn.ends_with(&format!(".{n}")))
    })
}

/// The functions a file defines, as `name` for free functions and
/// `Type.name` for methods in an `impl` of `Type`, the way the map's full
/// names end.
fn defined_names(file: &syn::File) -> BTreeSet<String> {
    use syn::visit::Visit;
    struct Names {
        names: BTreeSet<String>,
        self_type: Vec<Option<String>>,
    }
    impl<'ast> Visit<'ast> for Names {
        fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
            self.names.insert(i.sig.ident.to_string());
            syn::visit::visit_item_fn(self, i);
        }
        fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
            let ty = match &*i.self_ty {
                syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
                _ => None,
            };
            self.self_type.push(ty);
            syn::visit::visit_item_impl(self, i);
            self.self_type.pop();
        }
        fn visit_impl_item_fn(&mut self, i: &'ast syn::ImplItemFn) {
            let name = i.sig.ident.to_string();
            match self.self_type.last() {
                Some(Some(ty)) => self.names.insert(format!("{ty}.{name}")),
                _ => self.names.insert(name),
            };
            syn::visit::visit_impl_item_fn(self, i);
        }
    }
    let mut names = Names {
        names: BTreeSet::new(),
        self_type: Vec::new(),
    };
    names.visit_file(file);
    names.names
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
        let at_head = still_at_head(map, root, head, &mut cache, &first.fqn, &first.path);
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
            at_head,
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
        "  tests: {} ran, {} selected; sample: {}\n  still at HEAD: {}\n",
        s.tests_run,
        s.tests_selected,
        s.tests_sample.join(", "),
        match s.at_head {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        }
    ));
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
    let head = crate::worktree::head(&cfg.repo_root)?;
    let paths = cfg.paths();
    let map = crate::exec::edited::map_at(&paths, &head)?;
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
    let no_head = records.iter().filter(|s| s.at_head == Some(false)).count();
    let source_files: BTreeSet<_> = records.iter().map(|s| s.path.as_str()).collect();
    eprintln!(
        "{} survivors in {} files; {} no longer at HEAD",
        records.len(),
        source_files.len(),
        no_head
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
    fn unified_diff_formats_single_and_edge_changes() {
        assert_eq!(
            unified_diff("{\n old\n}", "{\n new\n}", 1, "x.rs", ""),
            "--- a/x.rs\n+++ b/x.rs\n@@ -1,3 +1,3 @@\n {\n- old\n+ new\n }\n"
        );
    }

    #[test]
    fn methods_at_head_are_matched_with_their_type() {
        let file: syn::File = syn::parse_str(
            "fn free() {}\nimpl Scope { fn new() {} fn get(&self) {} }\nimpl<T> Holder<T> { fn new() {} }",
        )
        .unwrap();
        let names = defined_names(&file);
        let at = |fqn: &str| {
            names
                .iter()
                .any(|n| fqn == n || fqn.ends_with(&format!(".{n}")))
        };
        assert!(at("k.mod.free"));
        assert!(at("k.Scope.new"));
        assert!(at("k.Holder.new"));
        // A `new` of another type that a later commit removed is gone.
        assert!(!at("k.Registry.new"));
        assert!(!at("k.Scope.renamed"));
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
            at_head: None,
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
