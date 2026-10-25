//! Read-only inventory of entered functions and their captured mutants.

use crate::cli::{CommonOpts, PendingArgs};
use crate::config::ConfigExtras;
use crate::exec::edited::{State, Status};
use crate::sitemap::SiteMap;
use anyhow::Result;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub package: String,
    pub path: String,
    pub line: usize,
    pub end_line: usize,
    pub fqn: String,
    pub lines: usize,
    pub mutants: usize,
}

impl Row {
    fn text(&self, all: bool) -> String {
        let mut text = format!(
            "{}:{}-{} {} ({} lines)",
            self.path, self.line, self.end_line, self.fqn, self.lines
        );
        if all {
            text.push_str(&format!(" [{} mutants]", self.mutants));
        }
        text
    }
}

/// Count records once per site, even when a record repeats an edit.
/// A remap's identity does not affect whether an existing mutant covers a site.
pub fn inventory(map: &SiteMap, state: &State, min_lines: usize) -> Vec<Row> {
    let entered: BTreeSet<u32> = map.hits.values().flatten().copied().collect();
    let mut counts = BTreeMap::new();
    for record in &state.records {
        if matches!(record.status, Status::Unviable { .. }) {
            continue;
        }
        let keys: BTreeSet<_> = record
            .mutant
            .edits
            .iter()
            .map(|e| (e.site_id, e.path.as_str(), e.fqn.as_str()))
            .collect();
        for key in keys {
            *counts.entry(key).or_insert(0usize) += 1;
        }
    }
    let mut rows: Vec<_> = map
        .sites
        .iter()
        .filter(|s| entered.contains(&s.site_id) && s.scaffolding.is_none())
        .filter_map(|s| {
            let lines = s.end_line.saturating_sub(s.start_line) + 1;
            (lines >= min_lines).then(|| Row {
                package: s.package.clone(),
                path: s.path.clone(),
                line: s.start_line,
                end_line: s.end_line,
                fqn: s.fqn.clone(),
                lines,
                mutants: counts
                    .get(&(s.site_id, s.path.as_str(), s.fqn.as_str()))
                    .copied()
                    .unwrap_or(0),
            })
        })
        .collect();
    rows.sort_by(|a, b| (&a.path, a.line, &a.fqn).cmp(&(&b.path, b.line, &b.fqn)));
    rows
}

/// Largest functions first, assigned to the bucket with the fewest lines.
pub fn split(rows: &[Row], n: usize) -> Vec<Vec<&Row>> {
    assert!(n > 0);
    let mut sorted: Vec<_> = rows.iter().collect();
    sorted.sort_by_key(|r| std::cmp::Reverse(r.lines));
    let mut buckets = vec![Vec::new(); n];
    let mut totals = vec![0usize; n];
    for row in sorted {
        let i = (0..n).min_by_key(|&i| totals[i]).unwrap();
        totals[i] += row.lines;
        buckets[i].push(row);
    }
    buckets
}

pub fn run(common: &CommonOpts, args: PendingArgs) -> Result<i32> {
    let (cfg, _) = crate::engine::config_from_cli(common, ConfigExtras::default())?;
    let paths = cfg.paths();
    let head = crate::worktree::head(&cfg.repo_root)?;
    let map = crate::exec::edited::map_at(&paths, &head)?;
    let state = State::load(&paths)?;
    let files = crate::discover::file_globset(&cfg.files)?;
    let rows: Vec<_> = inventory(&map, &state, args.min_lines)
        .into_iter()
        .filter(|r| cfg.packages.is_empty() || cfg.packages.contains(&r.package))
        .filter(|r| files.as_ref().is_none_or(|g| g.is_match(&r.path)))
        .collect();
    let pending = rows.iter().filter(|r| r.mutants == 0).count();
    let pending_lines: usize = rows
        .iter()
        .filter(|r| r.mutants == 0)
        .map(|r| r.lines)
        .sum();
    // Count distinct records for the summary: one multi-site mutant is still one mutant.
    let names: BTreeSet<_> = rows
        .iter()
        .map(|r| (r.path.as_str(), r.fqn.as_str()))
        .collect();
    let identities: BTreeSet<_> = map
        .sites
        .iter()
        .filter(|s| names.contains(&(s.path.as_str(), s.fqn.as_str())))
        .map(|s| (s.site_id, s.path.as_str(), s.fqn.as_str()))
        .collect();
    let mutants = state
        .records
        .iter()
        .filter(|record| {
            !matches!(record.status, Status::Unviable { .. })
                && record
                    .mutant
                    .edits
                    .iter()
                    .any(|e| identities.contains(&(e.site_id, e.path.as_str(), e.fqn.as_str())))
        })
        .count();
    eprintln!(
        "{} entered functions; {} have mutants ({} mutants); {} pending, {} lines",
        rows.len(),
        rows.len() - pending,
        mutants,
        pending,
        pending_lines
    );
    let shown: Vec<_> = rows
        .into_iter()
        .filter(|r| args.all || r.mutants == 0)
        .collect();
    for row in &shown {
        println!("{}", row.text(args.all));
    }
    if let Some(n) = args.split {
        let out = args.out.as_ref().expect("clap requires --out with --split");
        std::fs::create_dir_all(out)?;
        for (i, bucket) in split(&shown, n).iter().enumerate() {
            let path = out.join(format!("pending-{}.txt", i + 1));
            let text: String = bucket
                .iter()
                .map(|r| format!("{}\n", r.text(args.all)))
                .collect();
            crate::util::write_atomic(&path, text)?;
            let lines: usize = bucket.iter().map(|r| r.lines).sum();
            eprintln!(
                "{}: {} functions, {} lines",
                path.display(),
                bucket.len(),
                lines
            );
        }
    }
    if let Some(path) = &cfg.json {
        crate::util::write_atomic(path, serde_json::to_vec_pretty(&shown)?)?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(lines: usize) -> Row {
        Row {
            package: "p".into(),
            path: "src/lib.rs".into(),
            line: 1,
            end_line: lines,
            fqn: format!("f{lines}"),
            lines,
            mutants: 0,
        }
    }

    #[test]
    fn text_and_json_include_end_line() {
        let mut row = row(7);
        row.mutants = 2;
        assert_eq!(row.text(false), "src/lib.rs:1-7 f7 (7 lines)");
        assert_eq!(row.text(true), "src/lib.rs:1-7 f7 (7 lines) [2 mutants]");
        assert_eq!(serde_json::to_value(row).unwrap()["end_line"], 7);
    }

    #[test]
    fn split_balances_lines_largest_first() {
        let rows: Vec<_> = [2, 8, 3, 7, 6, 4].into_iter().map(row).collect();
        let buckets = split(&rows, 3);
        assert_eq!(
            buckets
                .iter()
                .map(|b| b.iter().map(|r| r.lines).sum::<usize>())
                .collect::<Vec<_>>(),
            [10, 10, 10]
        );
        assert_eq!(split(&[], 2).len(), 2);
        let buckets = split(&rows[..1], 3);
        assert_eq!(buckets.iter().map(Vec::len).collect::<Vec<_>>(), [1, 0, 0]);
    }
}
