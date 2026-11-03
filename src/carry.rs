//! Carry captured bodies to a new map without applying textual patches.
use crate::capture::{BodyEdit, CaptureOutcome, EditedMutant};
use crate::sitemap::SiteMap;
use anyhow::{ensure, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Deserialize)]
struct SavedMutant {
    id: String,
    #[serde(default)]
    intent: Option<String>,
    edits: Vec<SavedEdit>,
}

#[derive(Deserialize)]
struct SavedEdit {
    fqn: String,
    path: String,
    #[serde(default)]
    package: Option<String>,
    #[serde(default)]
    original: String,
    body: String,
}

/// Read a saved state or test JSON report. Runtime ids and verdicts from
/// the source are never reused. Reject whole mutants, not individual edits.
/// Rejected entries are reported as dropped, while valid entries proceed.
pub fn capture_from(map: &SiteMap, repo: &Path, source: &Path) -> Result<CaptureOutcome> {
    let json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(source).with_context(|| format!("read {}", source.display()))?,
    )
    .with_context(|| format!("parse {}", source.display()))?;
    let prior_target: crate::production::Selection = serde_json::from_value(
        json.get("mutation_target")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    )
    .context("read source production target")?;
    map.mutation_target.require_match(&prior_target)?;
    let prior_features: crate::features::CargoFeatures = serde_json::from_value(
        json.get("cargo_features")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    )
    .context("read source Cargo feature selection")?;
    if prior_features != map.cargo_features {
        eprintln!("carrying bodies to a different Cargo feature selection; previous coverage, binaries and verdicts are not imported");
    }
    let saved: Vec<SavedMutant> = if let Some(records) =
        json.get("records").and_then(|v| v.as_array())
    {
        records
            .iter()
            .map(|r| {
                serde_json::from_value(r.get("mutant").cloned().unwrap_or(serde_json::Value::Null))
            })
            .collect::<Result<_, _>>()?
    } else if let Some(mutants) = json.get("mutants").and_then(|v| v.as_array()) {
        serde_json::from_value(serde_json::Value::Array(mutants.clone()))?
    } else {
        anyhow::bail!(
            "{} must be a saved edited state (records) or test --json report (mutants)",
            source.display()
        );
    };
    let mut files = BTreeMap::new();
    let mut outcome = CaptureOutcome::default();
    for mutant in saved {
        match rebind(map, repo, &mut files, &mutant) {
            Ok(m) => outcome.mutants.push(m),
            Err(reason) => outcome
                .dropped
                .push(format!("mutant {}: {reason:#}", mutant.id)),
        }
    }
    Ok(outcome)
}

fn rebind(
    map: &SiteMap,
    repo: &Path,
    files: &mut BTreeMap<String, String>,
    saved: &SavedMutant,
) -> Result<EditedMutant> {
    ensure!(!saved.edits.is_empty(), "has no edits");
    let mut edits = Vec::new();
    for old in &saved.edits {
        ensure!(
            !old.original.is_empty(),
            "{}: saved original body is missing; cannot validate",
            old.fqn
        );
        let candidates: Vec<_> = map.sites.iter().filter(|s| s.fqn == old.fqn).collect();
        ensure!(
            !candidates.is_empty(),
            "{}: function is missing from the new map (deleted, renamed or not mapped)",
            old.fqn
        );
        ensure!(
            candidates.len() == 1,
            "{}: function name is ambiguous in the new map",
            old.fqn
        );
        let site = candidates[0];
        ensure!(
            site.path == old.path,
            "{}: function moved from {} to {}; refusing carry-forward",
            old.fqn,
            old.path,
            site.path
        );
        ensure!(
            old.package.as_ref().is_none_or(|p| p == &site.package),
            "{}: package changed",
            old.fqn
        );
        if !files.contains_key(&site.path) {
            let bytes =
                crate::worktree::show(repo, &map.commit, &format!("{}{}", map.prefix, site.path))?;
            files.insert(
                site.path.clone(),
                String::from_utf8(bytes).context("original source is not UTF-8")?,
            );
        }
        let text = &files[&site.path];
        let original = text
            .get(site.body_start_byte..site.body_end_byte)
            .with_context(|| format!("{}: map body range is invalid", old.fqn))?;
        ensure!(
            original == old.original,
            "{}: original body changed; refusing carry-forward",
            old.fqn
        );
        ensure!(
            crate::capture::braces_balanced(&old.body),
            "{}: replacement is not a balanced body",
            old.fqn
        );
        ensure!(
            old.body != original,
            "{}: replacement is identical to the original",
            old.fqn
        );
        ensure!(
            !edits.iter().any(|e: &BodyEdit| e.site_id == site.site_id),
            "{}: duplicate edits of one function",
            old.fqn
        );
        edits.push(BodyEdit {
            site_id: site.site_id,
            package: site.package.clone(),
            path: site.path.clone(),
            fqn: site.fqn.clone(),
            line: site.start_line,
            original: original.into(),
            body: old.body.clone(),
        });
    }
    let mut ranges: Vec<_> = edits.iter().filter_map(|e| map.site(e.site_id)).collect();
    ranges.sort_by_key(|s| (&s.path, s.body_start_byte));
    ensure!(
        ranges
            .windows(2)
            .all(|w| w[0].path != w[1].path || w[0].body_end_byte <= w[1].body_start_byte),
        "has overlapping body edits"
    );
    Ok(EditedMutant {
        id: crate::capture::mutant_id(&edits),
        edits,
        intent: saved.intent.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sitemap::MapSite;
    const TEXT: &str = "// moved lines\nfn f() -> bool { true }\nfn g() -> bool { true }\n";

    fn fixture() -> (SiteMap, SavedMutant, BTreeMap<String, String>) {
        let start = TEXT.find("{ true }").unwrap();
        let site = MapSite {
            site_id: 42,
            salt: 0,
            package: "p".into(),
            path: "src/lib.rs".into(),
            fqn: "p.f".into(),
            fn_start_byte: 15,
            body_start_byte: start,
            body_end_byte: start + 8,
            start_line: 2,
            end_line: 2,
            opaque_return: false,
            trait_impl: false,
            scaffolding: None,
        };
        let map = SiteMap {
            mutation_target: Default::default(),
            cargo_features: Default::default(),
            version: crate::sitemap::MAP_VERSION,
            map_id: "new".into(),
            commit: "new-commit".into(),
            prefix: "".into(),
            repo_root: "".into(),
            target: "".into(),
            files: BTreeMap::new(),
            sites: vec![site],
            test_lines: Default::default(),
            dropped: vec![],
            crate_roots: BTreeMap::new(),
            test_packages: vec![],
            build_env: BTreeMap::new(),
            tests: vec![],
            hits: BTreeMap::new(),
            cpu_secs_by_test: BTreeMap::new(),
            not_green: vec![],
            baseline_failures: vec![],
            skipped_tests: vec![],
            not_green_output: BTreeMap::new(),
            property_tests: Default::default(),
            worker: "".into(),
            run_dir: "".into(),
            map_secs: 0.0,
        };
        let saved = SavedMutant {
            id: "old-id".into(),
            intent: Some("lost condition".into()),
            edits: vec![SavedEdit {
                fqn: "p.f".into(),
                path: "src/lib.rs".into(),
                package: Some("p".into()),
                original: "{ true }".into(),
                body: "{ false }".into(),
            }],
        };
        (
            map,
            saved,
            [("src/lib.rs".into(), TEXT.into())].into_iter().collect(),
        )
    }

    #[test]
    fn carries_by_full_name_and_exact_original_not_identical_neighbor_text() {
        let (map, saved, mut files) = fixture();
        let m = rebind(&map, Path::new("unused"), &mut files, &saved).unwrap();
        assert_eq!(m.edits[0].site_id, 42);
        assert_eq!(m.edits[0].line, 2);
        assert_eq!(m.edits[0].fqn, "p.f");
        assert_eq!(m.intent, saved.intent);
        let mut old = m.clone();
        old.id = "old-map-id".into();
        old.edits[0].site_id = 9;
        old.edits[0].line = 100;
        old.intent = Some("different note".into());
        assert_ne!(crate::capture::mutant_id(&old.edits), m.id);
        assert_eq!(old.stable_id(), m.stable_id());
        let exported = serde_json::to_value(&m).unwrap();
        assert_eq!(exported["stable_id"], m.stable_id());
        let mut legacy = exported.clone();
        legacy.as_object_mut().unwrap().remove("stable_id");
        assert_eq!(serde_json::from_value::<EditedMutant>(legacy).unwrap(), m);
        old.edits[0].body = "{ true }".into();
        assert_ne!(old.stable_id(), m.stable_id());
        old.edits[0].body = m.edits[0].body.clone();
        old.edits[0].fqn = "p.g".into();
        assert_ne!(old.stable_id(), m.stable_id());
    }

    #[test]
    fn rejects_drift_missing_moved_ambiguous_or_invalid_edits_atomically() {
        let (mut map, mut saved, mut files) = fixture();
        let check = |map: &SiteMap,
                     saved: &SavedMutant,
                     files: &mut BTreeMap<String, String>,
                     why: &str| {
            let error = rebind(map, Path::new("unused"), files, saved)
                .unwrap_err()
                .to_string();
            assert!(error.contains(why), "{error}");
        };
        saved.edits[0].original = "{ true  }".into();
        check(&map, &saved, &mut files, "original body changed");
        saved.edits[0].original.clear();
        check(&map, &saved, &mut files, "original body is missing");
        saved.edits[0].original = "{ true }".into();
        saved.edits[0].fqn = "p.deleted".into();
        check(&map, &saved, &mut files, "missing from the new map");
        saved.edits[0].fqn = "p.f".into();
        saved.edits[0].path = "old.rs".into();
        check(&map, &saved, &mut files, "moved");
        saved.edits[0].path = "src/lib.rs".into();
        map.sites.push(map.sites[0].clone());
        check(&map, &saved, &mut files, "ambiguous");
        map.sites.pop();
        saved.edits[0].body = "{ broken".into();
        check(&map, &saved, &mut files, "balanced body");
        saved.edits[0].body = "{ false }".into();
        saved.edits.push(SavedEdit {
            fqn: "p.deleted".into(),
            path: "src/lib.rs".into(),
            package: None,
            original: "{ true }".into(),
            body: "{ false }".into(),
        });
        check(&map, &saved, &mut files, "missing from the new map");
    }
}
