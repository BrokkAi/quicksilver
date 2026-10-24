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

pub const MAP_VERSION: u32 = 6;

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
    /// The function returns `impl Trait`: its edited mutants are built one
    /// per build instead of as schema arms.
    #[serde(default)]
    pub opaque_return: bool,
    /// A method of an `impl Trait for Type`, called through the trait.
    #[serde(default)]
    pub trait_impl: bool,
    /// Why the function looks like test scaffolding, if it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scaffolding: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteMap {
    pub version: u32,
    /// Random per `map` run. The mutant state records which map it was built
    /// against, so a new map invalidates old `Built` records.
    pub map_id: String,
    /// The commit the map was built from. QS's worktree is checked out there,
    /// and original file text comes from it.
    pub commit: String,
    /// The workspace's path below the repository top level, empty or ending
    /// in `/`; git paths are `<prefix><path>`.
    pub prefix: String,
    pub repo_root: PathBuf,
    /// The target triple the map was built and recorded for.
    #[serde(default)]
    pub target: String,
    /// Every tracked file of the checkout and its SHA-256 when the map was
    /// made. `build` compares the checkout against this.
    pub files: BTreeMap<String, String>,
    pub sites: Vec<MapSite>,
    /// Lines of test code per file, so `uncovered` can tell a function used
    /// only by its own file's tests from one used elsewhere.
    #[serde(default)]
    pub test_lines: crate::discover::TestLines,
    /// (path, fqn, why) for functions that have no site.
    pub dropped: Vec<Dropped>,
    /// package -> crate root source file, repository-relative.
    pub crate_roots: BTreeMap<String, String>,
    /// The packages whose tests were built and recorded; empty means the
    /// workspace. `build` compiles the same set.
    pub test_packages: Vec<String>,
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

    /// Functions with a site that no green test entered, by path and line.
    /// Functions without a site (`const fn`, skipped, nested) are not here:
    /// the map cannot say whether tests reach them.
    pub fn unentered(&self) -> Vec<&MapSite> {
        let entered: std::collections::HashSet<u32> =
            self.hits.values().flatten().copied().collect();
        let mut out: Vec<&MapSite> = self
            .sites
            .iter()
            .filter(|s| !entered.contains(&s.site_id))
            .collect();
        out.sort_by(|a, b| (&a.path, a.start_line).cmp(&(&b.path, b.start_line)));
        out
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

/// The sites among `sites` whose name appears nowhere in the repository's
/// Rust sources at the map's commit except in their own definition and in
/// their own file's test code: candidates for deletion rather than tests.
/// One `git grep -w -F` finds every occurrence. Methods of trait impls
/// (called through the trait) and `main` are never candidates. A name
/// shared with another function counts as used, so the hint errs toward
/// "has callers".
pub fn without_callers(
    map: &SiteMap,
    repo_dir: &Path,
    sites: &[&MapSite],
) -> Result<std::collections::HashSet<u32>> {
    use std::collections::{HashMap, HashSet};
    let name_of = |s: &MapSite| s.fqn.rsplit('.').next().unwrap_or("").to_string();
    let candidates: Vec<&MapSite> = sites
        .iter()
        .copied()
        .filter(|s| !s.trait_impl && name_of(s) != "main")
        .collect();
    let names: HashSet<String> = candidates.iter().map(|s| name_of(s)).collect();
    if names.is_empty() {
        return Ok(HashSet::new());
    }
    let patterns =
        std::env::temp_dir().join(format!("qs-names-{}-{}", std::process::id(), random_id()));
    std::fs::write(
        &patterns,
        names.iter().cloned().collect::<Vec<_>>().join("\n") + "\n",
    )?;
    let out = std::process::Command::new("git")
        .args(["grep", "-n", "-w", "-F", "-I", "--no-color", "-f"])
        .arg(&patterns)
        .arg(&map.commit)
        .args(["--", "*.rs"])
        .current_dir(repo_dir)
        .output();
    let _ = std::fs::remove_file(&patterns);
    let out = out.context("git grep")?;
    // 1 means no match at all.
    if !out.status.success() && out.status.code() != Some(1) {
        anyhow::bail!("git grep: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let mut seen: HashMap<&str, Vec<(String, usize)>> = HashMap::new();
    let prefix = format!("{}:", map.commit);
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some(rest) = line.strip_prefix(&prefix) else {
            continue;
        };
        let mut parts = rest.splitn(3, ':');
        let (Some(path), Some(lineno), Some(text)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Ok(lineno) = lineno.parse::<usize>() else {
            continue;
        };
        let path = path.strip_prefix(map.prefix.as_str()).unwrap_or(path);
        for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
            if let Some(name) = names.get(word) {
                seen.entry(name.as_str())
                    .or_default()
                    .push((path.to_string(), lineno));
            }
        }
    }
    let in_tests = |path: &str, line: usize| {
        map.test_lines
            .get(path)
            .is_some_and(|ranges| ranges.iter().any(|(a, b)| *a <= line && line <= *b))
    };
    Ok(candidates
        .into_iter()
        .filter(|s| {
            let name = name_of(s);
            seen.get(name.as_str()).is_none_or(|uses| {
                uses.iter().all(|(path, line)| {
                    *path == s.path
                        && ((s.start_line <= *line && *line <= s.end_line) || in_tests(path, *line))
                })
            })
        })
        .map(|s| s.site_id)
        .collect())
}

/// Plans built-in mutants against a map: a mutant's tests are the ones that
/// entered its function when the map was recorded. Mutants are matched to
/// sites by the site seed (package, path, name, body start), not by site id,
/// which depends on the order ids were assigned in.
pub struct MapSelector<'a> {
    map: &'a SiteMap,
    by_seed: std::collections::HashMap<String, u32>,
}

impl<'a> MapSelector<'a> {
    pub fn new(map: &'a SiteMap) -> Self {
        let by_seed = map
            .sites
            .iter()
            .map(|s| {
                (
                    site_seed(&s.package, &s.path, &s.fqn, s.body_start_byte),
                    s.site_id,
                )
            })
            .collect();
        Self { map, by_seed }
    }

    fn site_of(&self, m: &crate::types::Mutant) -> Option<u32> {
        self.by_seed
            .get(&site_seed(&m.package, &m.path, &m.fqn, m.body_start_byte))
            .copied()
    }
}

impl crate::plan::TestSelector for MapSelector<'_> {
    /// A mutant whose function has no site in the map (the map is from
    /// another commit, or the function was dropped) counts as covered: that
    /// is the cautious answer for a census of what needs no run.
    fn covers(&self, m: &crate::types::Mutant) -> bool {
        match self.site_of(m) {
            Some(site) => self.map.hits.values().any(|h| h.contains(&site)),
            None => true,
        }
    }
    fn tests_for(&self, m: &crate::types::Mutant) -> Vec<TestInfo> {
        match self.site_of(m) {
            Some(site) => self.map.tests_entering(&[site]),
            None => self.map.tests.clone(),
        }
    }
    fn catalog(&self) -> &[TestInfo] {
        &self.map.tests
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
        let why = if let Some(cfg) = &f.cfg_off {
            Some(format!(
                "compiled out on this target ({cfg}); no test here can reach it"
            ))
        } else if f.const_fn {
            Some("const fn: a const body cannot call the runtime switch".to_string())
        } else if f.skip {
            Some("carries a skip attribute".to_string())
        } else {
            None
        };
        if let Some(why) = why {
            dropped.push((f.path.clone(), f.fqn.clone(), why));
            continue;
        }
        // A function nested in another's body is a site of its own; an edit
        // inside it is that function's, an edit elsewhere in the outer body
        // is the outer function's (`capture::split_nested`).
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
            opaque_return: f.opaque_return,
            trait_impl: f.trait_impl,
            scaffolding: f.scaffolding.clone(),
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
            opaque_return: false,
            cfg_off: None,
            trait_impl: false,
            scaffolding: None,
            original_body: "{}".into(),
        }
    }

    #[test]
    fn const_and_skipped_functions_get_no_site_but_nested_ones_do() {
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
        assert_eq!(kept, vec!["outer", "inner", "next"]);
        let why: Vec<(&str, &str)> = dropped
            .iter()
            .map(|(_, fqn, why)| (fqn.as_str(), why.split(':').next().unwrap()))
            .collect();
        assert_eq!(
            why,
            vec![("k", "const fn"), ("s", "carries a skip attribute"),]
        );
        let ids: std::collections::BTreeSet<u32> = sites.iter().map(|s| s.site_id).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn the_map_selector_matches_mutants_to_sites_by_seed() {
        use crate::plan::TestSelector;
        let site = |id: u32, fqn: &str, body: usize| MapSite {
            site_id: id,
            salt: 0,
            package: "p".into(),
            path: "src/lib.rs".into(),
            fqn: fqn.into(),
            fn_start_byte: body - 5,
            body_start_byte: body,
            body_end_byte: body + 5,
            start_line: 1,
            end_line: 1,
            opaque_return: false,
            trait_impl: false,
            scaffolding: None,
        };
        let t = |id: &str| TestInfo {
            id: id.into(),
            name: id.into(),
            package: "p".into(),
            binary_id: "p".into(),
            binary_name: String::new(),
            binary_rel: String::new(),
            cwd_rel: String::new(),
            duration_secs: 0.1,
        };
        let map = SiteMap {
            version: MAP_VERSION,
            map_id: "x".into(),
            commit: String::new(),
            prefix: String::new(),
            repo_root: PathBuf::new(),
            target: String::new(),
            files: BTreeMap::new(),
            test_lines: Default::default(),
            sites: vec![site(7, "p.entered", 10), site(8, "p.untouched", 40)],
            dropped: Vec::new(),
            crate_roots: BTreeMap::new(),
            test_packages: Vec::new(),
            tests: vec![t("p::t1")],
            hits: [("p::t1".to_string(), vec![7])].into_iter().collect(),
            not_green: Vec::new(),
            worker: PathBuf::new(),
            run_dir: PathBuf::new(),
            map_secs: 0.0,
        };
        let mutant = |fqn: &str, body: usize| crate::types::Mutant {
            id: fqn.into(),
            package: "p".into(),
            path: "src/lib.rs".into(),
            fqn: fqn.into(),
            start_byte: body - 5,
            end_byte: body + 5,
            start_line: 1,
            end_line: 1,
            body_start_byte: body,
            body_end_byte: body + 5,
            replacement: "{ 0 }".into(),
            return_kind: "number".into(),
        };
        let unentered: Vec<&str> = map.unentered().iter().map(|s| s.fqn.as_str()).collect();
        assert_eq!(unentered, vec!["p.untouched"]);
        let sel = MapSelector::new(&map);
        assert!(sel.covers(&mutant("p.entered", 10)));
        assert_eq!(sel.tests_for(&mutant("p.entered", 10)).len(), 1);
        assert!(!sel.covers(&mutant("p.untouched", 40)));
        // A function the map does not know (moved since) counts as covered.
        assert!(sel.covers(&mutant("p.entered", 11)));
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
            commit: String::new(),
            prefix: String::new(),
            repo_root: PathBuf::new(),
            target: String::new(),
            files: BTreeMap::new(),
            test_lines: Default::default(),
            sites: Vec::new(),
            dropped: Vec::new(),
            crate_roots: BTreeMap::new(),
            test_packages: Vec::new(),
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
