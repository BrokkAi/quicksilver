//! Mutant schemata: insert a dispatch prologue, leave the original body intact.
//!
//! Insertion (not wrapping the body in a `match`) is load-bearing: moving a
//! body into a match arm can change drop order and NLL on the unmutated path.

use crate::discover::kind_is_schema_prefiltered;
use crate::report::JsonReport;
use crate::types::{Mutant, Outcome, VerdictKind};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

pub const QS_RT_SRC: &str = r#"
#[doc(hidden)]
#[allow(dead_code)]
pub mod __qs_rt {
    use std::sync::LazyLock;
    static ACTIVE: LazyLock<(u32, u32)> = LazyLock::new(|| {
        std::env::var("QS_MUTANT").ok().and_then(|s| {
            let (a, b) = s.split_once(':')?;
            Some((a.parse().ok()?, b.parse().ok()?))
        })
        .unwrap_or((u32::MAX, 0))
    });
    #[inline(always)]
    pub fn active(site: u32) -> Option<u32> {
        let (s, v) = *ACTIVE;
        if s == site { Some(v) } else { None }
    }
}
"#;

#[derive(Debug, Clone)]
pub struct SiteVariant {
    pub index: u32,
    pub mutant_id: String,
    pub return_kind: String,
    pub return_stmt: String,
}

#[derive(Debug, Clone)]
pub struct Site {
    pub site_id: u32,
    pub salt: u32,
    pub package: String,
    pub path: String,
    pub fqn: String,
    pub body_start_byte: usize,
    pub body_end_byte: usize,
    pub fn_start_byte: usize,
    pub identity_variant: u32,
    pub variants: Vec<SiteVariant>,
}

impl Site {
    pub fn seed(&self) -> String {
        site_seed(&self.package, &self.path, &self.fqn, self.body_start_byte)
    }
}

pub fn site_seed(package: &str, path: &str, fqn: &str, body_start: usize) -> String {
    format!("{package}\0{path}\0{fqn}\0{body_start}")
}

pub fn default_site_hash(seed: &str, salt: u32) -> u32 {
    let mut h = Sha256::new();
    h.update(seed.as_bytes());
    h.update(salt.to_le_bytes());
    let d = h.finalize();
    u32::from_le_bytes([d[0], d[1], d[2], d[3]])
}

/// Assign unique `u32` site ids. `u32::MAX` is reserved (runtime "no mutant").
/// Collisions re-salt rather than fire two sites at once.
pub fn assign_unique_u32<F>(seeds: &[String], mut hash: F) -> Result<Vec<(u32, u32)>>
where
    F: FnMut(&str, u32) -> u32,
{
    let mut used = HashSet::new();
    let mut out = Vec::with_capacity(seeds.len());
    for seed in seeds {
        let mut salt = 0u32;
        loop {
            let id = hash(seed, salt);
            if id != u32::MAX && used.insert(id) {
                out.push((id, salt));
                break;
            }
            salt = salt.checked_add(1).context("site-id salt overflow")?;
            if salt > 100_000 {
                bail!("could not resolve site-id collision for {seed}");
            }
        }
    }
    Ok(out)
}

pub fn replacement_to_return(repl: &str) -> String {
    let inner = repl.trim();
    let inner = inner
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(inner)
        .trim();
    if inner.is_empty() {
        "return".to_string()
    } else {
        format!("return {inner}")
    }
}

/// Group mutants into sites. Pre-filtered kinds (`ref`, `impl_trait`, `dyn`)
/// are returned separately and never injected.
pub fn build_sites(mutants: &[Mutant]) -> Result<(Vec<Site>, Vec<Mutant>)> {
    let mut groups: BTreeMap<(String, String, String, usize), Vec<&Mutant>> = BTreeMap::new();
    let mut prefiltered = Vec::new();
    for m in mutants {
        if kind_is_schema_prefiltered(&m.return_kind) {
            prefiltered.push(m.clone());
            continue;
        }
        groups
            .entry((
                m.package.clone(),
                m.path.clone(),
                m.fqn.clone(),
                m.body_start_byte,
            ))
            .or_default()
            .push(m);
    }
    let mut stubs = Vec::new();
    for ((package, path, fqn, body_start), ms) in groups {
        let mut variants = Vec::new();
        for (i, m) in ms.iter().enumerate() {
            variants.push(SiteVariant {
                index: i as u32,
                mutant_id: m.id.clone(),
                return_kind: m.return_kind.clone(),
                return_stmt: replacement_to_return(&m.replacement),
            });
        }
        let identity = variants.len() as u32;
        let first = ms[0];
        stubs.push(Site {
            site_id: 0,
            salt: 0,
            package,
            path,
            fqn,
            body_start_byte: body_start,
            body_end_byte: first.body_end_byte,
            fn_start_byte: first.start_byte,
            identity_variant: identity,
            variants,
        });
    }
    let seeds: Vec<String> = stubs.iter().map(|s| s.seed()).collect();
    let ids = assign_unique_u32(&seeds, default_site_hash)?;
    for (site, (id, salt)) in stubs.iter_mut().zip(ids) {
        site.site_id = id;
        site.salt = salt;
    }
    Ok((stubs, prefiltered))
}

pub fn render_prologue(site: &Site) -> String {
    render_prologue_with_ranges(site).0
}

pub fn render_prologue_with_ranges(site: &Site) -> (String, Vec<(u32, String, usize, usize)>) {
    let mut s = format!(
        " if let Some(v) = crate::__qs_rt::active({:#010x}) {{ match v {{ ",
        site.site_id
    );
    let mut ranges = Vec::new();
    for v in &site.variants {
        let arm = format!("{} => {}, ", v.index, v.return_stmt);
        let start = s.len();
        s.push_str(&arm);
        ranges.push((v.index, v.mutant_id.clone(), start, s.len()));
    }
    s.push_str(&format!("{} => {{}}, ", site.identity_variant));
    s.push_str("_ => {} } } ");
    (s, ranges)
}

#[derive(Debug, Clone)]
pub struct AppliedVariant {
    pub site_id: u32,
    pub variant: u32,
    pub mutant_id: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct AppliedSite {
    pub site_id: u32,
    pub path: String,
    pub prologue_start: usize,
    pub prologue_end: usize,
    pub fn_start: usize,
    pub fn_end: usize,
    pub variants: Vec<AppliedVariant>,
}

/// Insert prologues after each function's opening `{`, highest offset first
/// so original insert positions stay valid. Applied ranges are computed on
/// the *final* file: earlier inserts shift later sites forward.
pub fn apply_to_source(src: &str, sites: &[&Site]) -> Result<(String, Vec<AppliedSite>)> {
    let mut ordered: Vec<&Site> = sites.to_vec();
    ordered.sort_by_key(|s| std::cmp::Reverse(s.body_start_byte));
    let mut prepared: Vec<(&Site, String, Vec<(u32, String, usize, usize)>)> = Vec::new();
    for site in &ordered {
        if site.body_start_byte >= src.len() {
            bail!("site {} body_start out of range", site.fqn);
        }
        if src.as_bytes()[site.body_start_byte] != b'{' {
            bail!(
                "site {} expected '{{' at body_start {}, found {:?}",
                site.fqn,
                site.body_start_byte,
                src.as_bytes()
                    .get(site.body_start_byte)
                    .copied()
                    .map(|c| c as char)
            );
        }
        let (prologue, rel) = render_prologue_with_ranges(site);
        prepared.push((*site, prologue, rel));
    }
    let mut text = src.to_string();
    for (site, prologue, _) in &prepared {
        text.insert_str(site.body_start_byte + 1, prologue);
    }
    prepared.sort_by_key(|(s, _, _)| s.body_start_byte);
    let mut applied = Vec::new();
    let mut shift = 0usize;
    for (site, prologue, rel) in &prepared {
        let insert_at = site.body_start_byte + 1 + shift;
        applied.push(AppliedSite {
            site_id: site.site_id,
            path: site.path.clone(),
            prologue_start: insert_at,
            prologue_end: insert_at + prologue.len(),
            fn_start: site.fn_start_byte + shift,
            fn_end: site.body_end_byte + shift + prologue.len(),
            variants: rel
                .iter()
                .map(|(index, mutant_id, start, end)| AppliedVariant {
                    site_id: site.site_id,
                    variant: *index,
                    mutant_id: mutant_id.clone(),
                    start: insert_at + start,
                    end: insert_at + end,
                })
                .collect(),
        });
        shift += prologue.len();
    }
    Ok((text, applied))
}

pub fn apply_schema_to_tree(
    root: &Path,
    sites: &[Site],
    crate_root: &Path,
) -> Result<Vec<AppliedSite>> {
    let mut by_file: BTreeMap<String, Vec<&Site>> = BTreeMap::new();
    for s in sites {
        by_file.entry(s.path.clone()).or_default().push(s);
    }
    let mut all_applied = Vec::new();
    for (rel, file_sites) in by_file {
        let path = root.join(&rel);
        let src =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let (out, applied) = apply_to_source(&src, &file_sites)?;
        std::fs::write(&path, out.as_bytes())?;
        all_applied.extend(applied);
    }
    inject_runtime(crate_root)?;
    Ok(all_applied)
}

pub fn inject_runtime(crate_root: &Path) -> Result<()> {
    let mut src = std::fs::read_to_string(crate_root)
        .with_context(|| format!("read crate root {}", crate_root.display()))?;
    if src.contains("pub mod __qs_rt") {
        return Ok(());
    }
    if !src.ends_with('\n') {
        src.push('\n');
    }
    src.push_str(QS_RT_SRC);
    std::fs::write(crate_root, src.as_bytes())?;
    Ok(())
}

pub fn snapshot_sources(
    root: &Path,
    sites: &[Site],
    crate_root_rel: &str,
) -> Result<HashMap<String, Vec<u8>>> {
    let mut files: BTreeSet<String> = sites.iter().map(|s| s.path.clone()).collect();
    files.insert(crate_root_rel.to_string());
    let mut out = HashMap::new();
    for rel in files {
        let p = root.join(&rel);
        out.insert(
            rel.clone(),
            std::fs::read(&p).with_context(|| format!("read {}", p.display()))?,
        );
    }
    Ok(out)
}

pub fn restore_files(root: &Path, originals: &HashMap<String, Vec<u8>>) -> Result<()> {
    for (rel, bytes) in originals {
        std::fs::write(root.join(rel), bytes)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct CompilerSpan {
    pub file: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub is_primary: bool,
    pub message: String,
}

pub fn parse_compiler_error_spans(stdout: &[u8]) -> Vec<CompilerSpan> {
    let mut out = Vec::new();
    for line in stdout.split(|b| *b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("reason").and_then(|r| r.as_str()) != Some("compiler-message") {
            continue;
        }
        if let Some(msg) = v.get("message") {
            collect_spans(msg, &mut out);
        }
    }
    out
}

fn collect_spans(msg: &serde_json::Value, out: &mut Vec<CompilerSpan>) {
    let level = msg.get("level").and_then(|l| l.as_str()).unwrap_or("");
    let message = msg
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    if level == "error" {
        if let Some(spans) = msg.get("spans").and_then(|s| s.as_array()) {
            for sp in spans {
                let file = sp
                    .get("file_name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let byte_start =
                    sp.get("byte_start").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                let byte_end = sp.get("byte_end").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
                let is_primary = sp
                    .get("is_primary")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                out.push(CompilerSpan {
                    file,
                    byte_start,
                    byte_end,
                    is_primary,
                    message: message.clone(),
                });
            }
        }
    }
    if let Some(children) = msg.get("children").and_then(|c| c.as_array()) {
        for c in children {
            collect_spans(c, out);
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Attributed {
    pub variant_ids: BTreeSet<String>,
    pub site_ids: BTreeSet<u32>,
}

impl Attributed {
    pub fn is_empty(&self) -> bool {
        self.variant_ids.is_empty() && self.site_ids.is_empty()
    }
}

pub fn attribute_spans(
    applied: &[AppliedSite],
    spans: &[CompilerSpan],
    worker: &Path,
) -> Attributed {
    let mut attr = Attributed::default();
    let primary: Vec<&CompilerSpan> = spans.iter().filter(|s| s.is_primary).collect();
    let use_spans: Vec<&CompilerSpan> = if primary.is_empty() {
        spans.iter().collect()
    } else {
        primary
    };
    for sp in use_spans {
        if let Some(hit) = hit_for_span(applied, sp, worker) {
            match hit {
                SpanHit::Variant(id) => {
                    attr.variant_ids.insert(id);
                }
                SpanHit::Site(id) => {
                    attr.site_ids.insert(id);
                }
            }
        }
    }
    attr
}

enum SpanHit {
    Variant(String),
    Site(u32),
}

fn hit_for_span(applied: &[AppliedSite], sp: &CompilerSpan, worker: &Path) -> Option<SpanHit> {
    for a in applied {
        if !span_matches_path(&sp.file, &a.path, worker) {
            continue;
        }
        let mut best: Option<&AppliedVariant> = None;
        for v in &a.variants {
            if overlaps(sp.byte_start, sp.byte_end, v.start, v.end) {
                let tighter = best
                    .map(|b| (v.end - v.start) < (b.end - b.start))
                    .unwrap_or(true);
                if tighter {
                    best = Some(v);
                }
            }
        }
        if let Some(v) = best {
            return Some(SpanHit::Variant(v.mutant_id.clone()));
        }
        if overlaps(sp.byte_start, sp.byte_end, a.prologue_start, a.prologue_end)
            || overlaps(sp.byte_start, sp.byte_end, a.fn_start, a.fn_end)
        {
            return Some(SpanHit::Site(a.site_id));
        }
    }
    None
}

fn span_matches_path(span_file: &str, rel: &str, worker: &Path) -> bool {
    let span = Path::new(span_file);
    if span == Path::new(rel) {
        return true;
    }
    if worker.join(rel) == span {
        return true;
    }
    let slash = crate::util::rel_slash(span);
    if slash == rel || slash.ends_with(rel) {
        return true;
    }
    if let Ok(stripped) = span.strip_prefix(worker) {
        return crate::util::rel_slash(stripped) == rel;
    }
    false
}

fn overlaps(a0: usize, a1: usize, b0: usize, b1: usize) -> bool {
    a0 < b1 && b0 < a1
}

/// Drop attributed variants / sites. Returns `(mutant_id, reason)` pairs.
pub fn disable_attributed(sites: &mut Vec<Site>, attr: &Attributed) -> Vec<(String, String)> {
    let mut out = Vec::new();
    sites.retain_mut(|site| {
        if attr.site_ids.contains(&site.site_id) {
            for v in &site.variants {
                out.push((
                    v.mutant_id.clone(),
                    format!("site {:#010x} ({})", site.site_id, site.fqn),
                ));
            }
            return false;
        }
        site.variants.retain(|v| {
            if attr.variant_ids.contains(&v.mutant_id) {
                out.push((
                    v.mutant_id.clone(),
                    format!("site {:#010x} variant {}", site.site_id, v.index),
                ));
                false
            } else {
                true
            }
        });
        !site.variants.is_empty()
    });
    out
}

pub fn disable_sites(sites: &mut Vec<Site>, ids: &BTreeSet<u32>) -> Vec<(String, String)> {
    let attr = Attributed {
        variant_ids: BTreeSet::new(),
        site_ids: ids.clone(),
    };
    disable_attributed(sites, &attr)
}

pub fn chunk_sites(sites: Vec<Site>, batch_size: usize) -> Vec<Vec<Site>> {
    if batch_size == 0 || sites.len() <= batch_size {
        return if sites.is_empty() {
            Vec::new()
        } else {
            vec![sites]
        };
    }
    sites.chunks(batch_size).map(|c| c.to_vec()).collect()
}

pub fn qs_mutant_env(site_id: u32, variant: u32) -> (String, String) {
    ("QS_MUTANT".into(), format!("{site_id}:{variant}"))
}

pub fn lookup_variant<'a>(sites: &'a [Site], mutant_id: &str) -> Option<(&'a Site, u32)> {
    for s in sites {
        for v in &s.variants {
            if v.mutant_id == mutant_id {
                return Some((s, v.index));
            }
        }
    }
    None
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VerdictDiff {
    pub mutant_id: String,
    pub fqn: String,
    pub v1: VerdictKind,
    pub v2: Option<VerdictKind>,
}

pub fn diff_verdicts(v1: &[Outcome], v2: &[Outcome]) -> Vec<VerdictDiff> {
    let v2_by: HashMap<&str, &Outcome> = v2.iter().map(|o| (o.mutant.id.as_str(), o)).collect();
    let mut diffs = Vec::new();
    for o in v1 {
        match v2_by.get(o.mutant.id.as_str()) {
            Some(other) if other.kind == o.kind => {}
            Some(other) => diffs.push(VerdictDiff {
                mutant_id: o.mutant.id.clone(),
                fqn: o.mutant.fqn.clone(),
                v1: o.kind,
                v2: Some(other.kind),
            }),
            None => diffs.push(VerdictDiff {
                mutant_id: o.mutant.id.clone(),
                fqn: o.mutant.fqn.clone(),
                v1: o.kind,
                v2: None,
            }),
        }
    }
    diffs
}

pub fn gate_differential(path: &Path, v2: &[Outcome]) -> Result<Vec<VerdictDiff>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read v1 json {}", path.display()))?;
    let report: JsonReport = serde_json::from_str(&text).context("parse v1 json")?;
    Ok(diff_verdicts(&report.outcomes, v2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn number_mutant(fqn: &str, body_start: usize, body_end: usize, repl: &str) -> Mutant {
        Mutant {
            id: format!("{fqn}:{repl}"),
            package: "p".into(),
            path: "src/lib.rs".into(),
            fqn: fqn.into(),
            start_byte: body_start.saturating_sub(20),
            end_byte: body_end,
            start_line: 1,
            end_line: 3,
            body_start_byte: body_start,
            body_end_byte: body_end,
            replacement: repl.into(),
            return_kind: "number".into(),
        }
    }

    #[test]
    fn replacement_strips_braces() {
        assert_eq!(replacement_to_return("{ 0 }"), "return 0");
        assert_eq!(replacement_to_return("{}"), "return");
        assert_eq!(
            replacement_to_return("{ Default::default() }"),
            "return Default::default()"
        );
    }

    #[test]
    fn insert_prologue_preserves_original_body() {
        let src = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let brace = src.find('{').unwrap();
        let end = src.rfind('}').unwrap() + 1;
        let m = number_mutant("p::add", brace, end, "{ 0 }");
        let (sites, pre) = build_sites(&[m]).unwrap();
        assert!(pre.is_empty());
        let refs: Vec<&Site> = sites.iter().collect();
        let (out, applied) = apply_to_source(src, &refs).unwrap();
        assert!(out.contains("a + b"), "original body lost: {out}");
        assert!(out.contains("__qs_rt::active"));
        assert!(out.contains("return 0"));
        assert_eq!(applied.len(), 1);
        assert!(out[applied[0].prologue_start..applied[0].prologue_end].contains("active"));
        let body = &out[brace..];
        assert!(body.contains("a + b"));
    }

    #[test]
    fn descending_inserts_compose_across_sites() {
        let src = "fn a() -> i32 { 1 }\nfn b() -> i32 { 2 }\n";
        let a_brace = src.find('{').unwrap();
        let a_end = src.find('}').unwrap() + 1;
        let b_brace = src.rfind('{').unwrap();
        let b_end = src.rfind('}').unwrap() + 1;
        let ma = number_mutant("p::a", a_brace, a_end, "{ 0 }");
        let mb = number_mutant("p::b", b_brace, b_end, "{ 0 }");
        let (sites, _) = build_sites(&[ma, mb]).unwrap();
        let refs: Vec<&Site> = sites.iter().collect();
        let (out, applied) = apply_to_source(src, &refs).unwrap();
        assert_eq!(applied.len(), 2);
        assert!(out.contains("{ 1 }") || out.contains("1 }"), "{out}");
        assert!(out.contains("{ 2 }") || out.contains("2 }"), "{out}");
        assert_eq!(out.matches("__qs_rt::active").count(), 2);
        for a in &applied {
            let slice = &out[a.prologue_start..a.prologue_end];
            assert!(
                slice.contains("active"),
                "applied range must land on the final prologue, got {slice:?} in {out}"
            );
            assert!(
                slice.contains(&format!("{:#010x}", a.site_id)),
                "applied range for site {:#010x} points at a different prologue: {slice}",
                a.site_id
            );
        }
    }

    #[test]
    fn prefilter_drops_ref_impl_dyn() {
        let mut m = number_mutant("p::as_ref", 0, 10, "{ Default::default() }");
        m.return_kind = "ref".into();
        let mut n = number_mutant("p::add", 20, 30, "{ 0 }");
        n.body_start_byte = 20;
        let (sites, pre) = build_sites(&[m, n]).unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0].fqn, "p::as_ref");
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].fqn, "p::add");
    }

    #[test]
    fn site_id_collision_is_detected_at_generation() {
        let hash = |seed: &str, salt: u32| -> u32 {
            if salt == 0 {
                1
            } else {
                seed.len() as u32 + salt * 1000
            }
        };
        let ids = assign_unique_u32(&["aa".into(), "bb".into()], hash).unwrap();
        assert_ne!(ids[0].0, ids[1].0);
        assert!(
            ids.iter().any(|(_, salt)| *salt > 0),
            "expected a re-salt, got {ids:?}"
        );
        assert!(ids.iter().all(|(id, _)| *id != u32::MAX));
    }

    #[test]
    fn attribute_maps_span_inside_variant_arm() {
        let src = "fn a() -> i32 { 1 }";
        let brace = src.find('{').unwrap();
        let end = src.len();
        let m = number_mutant("p::a", brace, end, "{ 0 }");
        let (sites, _) = build_sites(&[m]).unwrap();
        let refs: Vec<&Site> = sites.iter().collect();
        let (_out, applied) = apply_to_source(src, &refs).unwrap();
        let v = &applied[0].variants[0];
        let spans = [CompilerSpan {
            file: "src/lib.rs".into(),
            byte_start: v.start + 1,
            byte_end: v.end.saturating_sub(1).max(v.start + 1),
            is_primary: true,
            message: "E0277".into(),
        }];
        let attr = attribute_spans(&applied, &spans, Path::new("/worker"));
        assert!(attr.variant_ids.contains(&v.mutant_id), "{attr:?}");
        assert!(attr.site_ids.is_empty());
    }

    #[test]
    fn diff_verdicts_flags_mismatch_and_missing() {
        let mut a = number_mutant("p::a", 0, 1, "{ 0 }");
        a.id = "id-a".into();
        let mut b = number_mutant("p::b", 0, 1, "{ 0 }");
        b.id = "id-b".into();
        let v1 = vec![
            Outcome::caught(a.clone(), "t".into()),
            Outcome::missed(b.clone()),
        ];
        let v2 = vec![Outcome::caught(a, "t".into())];
        let diffs = diff_verdicts(&v1, &v2);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].mutant_id, "id-b");
        assert_eq!(diffs[0].v2, None);
    }
}
