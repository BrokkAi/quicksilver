//! The map: every function that carries a runtime switch, every test, and
//! which functions each test enters. `cargo quicksilver map` writes it;
//! `build` and `test` read it.

use crate::discover::FnDecl;
use crate::schema::{assign_unique_u32, default_site_hash, site_seed};
use crate::types::{Paths, TestInfo};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MAP_VERSION: u32 = 1;

/// (path, fqn, why) for a function without a site.
pub type Dropped = (String, String, String);

/// One function with a prologue. The site id is what `QS_MUTANT` and
/// `QS_RECORD_SITES` speak; byte offsets describe the baseline file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MapSite {
    pub site_id: u32,
    pub salt: u32,
    pub package: String,
    pub path: String,
    pub fqn: String,
    pub fn_start_byte: usize,
    pub body_start_byte: usize,
    pub body_end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteMap {
    pub version: u32,
    /// Random per `map` run. The mutant state records which map it was built
    /// against, so a new map invalidates old `Built` records.
    pub map_id: String,
    pub repo_root: PathBuf,
    /// Every tracked file of the checkout and its SHA-256 when the map was
    /// made. `build` compares the checkout against this.
    pub files: BTreeMap<String, String>,
    pub sites: Vec<MapSite>,
    /// (path, fqn, why) for functions that have no site.
    pub dropped: Vec<Dropped>,
    /// package -> crate root source file, repository-relative.
    pub crate_roots: BTreeMap<String, String>,
    /// The catalogued tests with their durations under the schema binary.
    pub tests: Vec<TestInfo>,
    /// test id -> site ids the test entered with no mutant active.
    pub hits: BTreeMap<String, Vec<u32>>,
    /// Tests that did not pass under the schema binary, with why.
    pub not_green: Vec<(String, String)>,
    /// The worker tree the map was built in; `build` rebuilds there.
    pub worker: PathBuf,
    /// Staged binaries of the map build (the recording run).
    pub run_dir: PathBuf,
    pub map_secs: f64,
}

impl SiteMap {
    pub fn path(paths: &Paths) -> PathBuf {
        paths.index.join("sitemap.json")
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        crate::util::write_atomic(&Self::path(paths), serde_json::to_vec_pretty(self)?)
    }

    pub fn load(paths: &Paths) -> Result<Option<SiteMap>> {
        let p = Self::path(paths);
        if !p.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        let map: SiteMap =
            serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))?;
        anyhow::ensure!(
            map.version == MAP_VERSION,
            "{} is map version {}, this build reads {}; run `cargo quicksilver map` again",
            p.display(),
            map.version,
            MAP_VERSION
        );
        Ok(Some(map))
    }

    pub fn site(&self, id: u32) -> Option<&MapSite> {
        self.sites.iter().find(|s| s.site_id == id)
    }

    /// Tests that entered `site`, cheapest first, with their recorded
    /// durations.
    pub fn tests_entering(&self, sites: &[u32]) -> Vec<TestInfo> {
        let mut out: Vec<TestInfo> = Vec::new();
        for t in &self.tests {
            let entered = self
                .hits
                .get(&t.id)
                .is_some_and(|hit| sites.iter().any(|s| hit.contains(s)));
            if entered && !out.iter().any(|x| x.id == t.id) {
                out.push(t.clone());
            }
        }
        out.sort_by(|a, b| {
            a.duration_secs
                .partial_cmp(&b.duration_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.id.cmp(&b.id))
        });
        out
    }
}

/// Which scanned functions get a site, and why the others do not. A nested
/// function (its range inside another kept function's body) is part of the
/// outer body and not its own site.
pub fn sites_from_functions(fns: &[FnDecl]) -> Result<(Vec<MapSite>, Vec<Dropped>)> {
    let mut kept: Vec<&FnDecl> = Vec::new();
    let mut dropped = Vec::new();
    let mut sorted: Vec<&FnDecl> = fns.iter().collect();
    sorted.sort_by(|a, b| (&a.path, a.start_byte).cmp(&(&b.path, b.start_byte)));
    for f in sorted {
        let why = if f.const_fn {
            Some("const fn: a const body cannot call the runtime switch")
        } else if f.skip {
            Some("carries a skip attribute")
        } else {
            None
        };
        if let Some(why) = why {
            dropped.push((f.path.clone(), f.fqn.clone(), why.to_string()));
            continue;
        }
        let nested = kept
            .iter()
            .rev()
            .take_while(|k| k.path == f.path)
            .any(|k| k.body_start_byte <= f.start_byte && f.end_byte <= k.body_end_byte);
        if nested {
            dropped.push((
                f.path.clone(),
                f.fqn.clone(),
                "nested in another function's body, which is the site".to_string(),
            ));
            continue;
        }
        kept.push(f);
    }
    let seeds: Vec<String> = kept
        .iter()
        .map(|f| site_seed(&f.package, &f.path, &f.fqn, f.body_start_byte))
        .collect();
    let ids = assign_unique_u32(&seeds, default_site_hash)?;
    let sites = kept
        .iter()
        .zip(ids)
        .map(|(f, (site_id, salt))| MapSite {
            site_id,
            salt,
            package: f.package.clone(),
            path: f.path.clone(),
            fqn: f.fqn.clone(),
            fn_start_byte: f.start_byte,
            body_start_byte: f.body_start_byte,
            body_end_byte: f.body_end_byte,
            start_line: f.start_line,
            end_line: f.end_line,
        })
        .collect();
    Ok((sites, dropped))
}

/// SHA-256 of every tracked and untracked-but-not-ignored file under `root`
/// (`git ls-files --cached --others --exclude-standard`), keyed by
/// repository-relative path with forward slashes. `target/` is skipped.
pub fn tracked_file_hashes(root: &Path) -> Result<BTreeMap<String, String>> {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "core.quotePath=false",
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .current_dir(root)
        .output()
        .context("git ls-files")?;
    anyhow::ensure!(
        out.status.success(),
        "git ls-files in {} failed: {}",
        root.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let mut files = BTreeMap::new();
    for rel in out.stdout.split(|b| *b == 0) {
        if rel.is_empty() {
            continue;
        }
        let rel = String::from_utf8_lossy(rel).into_owned();
        if rel == "target" || rel.starts_with("target/") {
            continue;
        }
        let path = root.join(&rel);
        if !path.is_file() {
            continue;
        }
        files.insert(rel, crate::util::sha256_file(&path)?);
    }
    Ok(files)
}

pub fn random_id() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut bytes);
    crate::util::hex(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::ReturnKind;

    fn decl(path: &str, fqn: &str, start: usize, end: usize, body: usize) -> FnDecl {
        FnDecl {
            package: "p".into(),
            path: path.into(),
            fqn: fqn.into(),
            start_byte: start,
            end_byte: end,
            start_line: 1,
            end_line: 1,
            body_start_byte: body,
            body_end_byte: end,
            return_kind: ReturnKind::Unit,
            unsafe_fn: false,
            const_fn: false,
            skip: false,
            boring_body: false,
            original_body: "{}".into(),
        }
    }

    #[test]
    fn nested_const_and_skipped_functions_get_no_site() {
        let mut inner = decl("a.rs", "outer::inner", 20, 40, 30);
        inner.fqn = "inner".into();
        let mut k = decl("a.rs", "k", 100, 120, 110);
        k.const_fn = true;
        let mut s = decl("b.rs", "s", 0, 10, 5);
        s.skip = true;
        let fns = vec![
            decl("a.rs", "outer", 0, 60, 10),
            inner,
            k,
            decl("a.rs", "next", 70, 90, 80),
            s,
        ];
        let (sites, dropped) = sites_from_functions(&fns).unwrap();
        let kept: Vec<&str> = sites.iter().map(|s| s.fqn.as_str()).collect();
        assert_eq!(kept, vec!["outer", "next"]);
        let why: Vec<(&str, &str)> = dropped
            .iter()
            .map(|(_, fqn, why)| (fqn.as_str(), why.split(':').next().unwrap()))
            .collect();
        assert_eq!(
            why,
            vec![
                (
                    "inner",
                    "nested in another function's body, which is the site"
                ),
                ("k", "const fn"),
                ("s", "carries a skip attribute"),
            ]
        );
        assert_ne!(sites[0].site_id, sites[1].site_id);
    }

    #[test]
    fn tests_entering_are_cheapest_first_and_unique() {
        let t = |id: &str, secs: f64| TestInfo {
            id: id.into(),
            name: id.into(),
            package: "p".into(),
            binary_id: "p".into(),
            binary_name: String::new(),
            binary_rel: String::new(),
            cwd_rel: String::new(),
            duration_secs: secs,
        };
        let map = SiteMap {
            version: MAP_VERSION,
            map_id: "x".into(),
            repo_root: PathBuf::new(),
            files: BTreeMap::new(),
            sites: Vec::new(),
            dropped: Vec::new(),
            crate_roots: BTreeMap::new(),
            tests: vec![t("slow", 2.0), t("fast", 0.1), t("other", 0.5)],
            hits: [
                ("slow".to_string(), vec![1, 2]),
                ("fast".to_string(), vec![2]),
                ("other".to_string(), vec![3]),
            ]
            .into_iter()
            .collect(),
            not_green: Vec::new(),
            worker: PathBuf::new(),
            run_dir: PathBuf::new(),
            map_secs: 0.0,
        };
        let ids: Vec<String> = map
            .tests_entering(&[1, 2])
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, vec!["fast", "slow"]);
        assert!(map.tests_entering(&[9]).is_empty());
    }
}
