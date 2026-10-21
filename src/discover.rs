use crate::cargo_cmd::{load_metadata, package_for_path};
use crate::types::Mutant;
use crate::util::{hex, line_col_to_offset};
use anyhow::{Context, Result};
use brokk_bifrost_analysis::analyzer::test_paths;
use brokk_bifrost_analysis::diff_analysis::{
    analyze_diff_at_root, AnalyzeDiffParams, DiffAnalysisOptions, DiffCallEdges,
};
use brokk_bifrost_analysis::{CancellationToken, CodeUnitType, Language, WorkspaceAnalyzer};
use globset::{Glob, GlobSet, GlobSetBuilder};
use quote::ToTokens;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::Path;
use syn::spanned::Spanned;
use syn::{Block, ImplItemFn, ItemFn, ReturnType, Type};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnKind {
    Unit,
    Bool,
    Number,
    Stringy,
    Option,
    Result,
    Vec,
    /// `-> !`. No replacements; skipped at discovery.
    Never,
    /// `&T` / `&mut T` other than `&str`. Structurally unviable under schemata.
    Ref,
    /// `-> impl Trait`. Structurally unviable under schemata.
    ImplTrait,
    /// `-> dyn Trait` (and `-> &dyn Trait` is Ref wrapping dyn; this is the
    /// bare trait object). Structurally unviable under schemata.
    Dyn,
    Other,
}

impl ReturnKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unit => "unit",
            Self::Bool => "bool",
            Self::Number => "number",
            Self::Stringy => "string",
            Self::Option => "option",
            Self::Result => "result",
            Self::Vec => "vec",
            Self::Never => "never",
            Self::Ref => "ref",
            Self::ImplTrait => "impl_trait",
            Self::Dyn => "dyn",
            Self::Other => "other",
        }
    }

    /// v2 pre-filter: these kinds cannot typecheck a `return Default::default()`
    /// (or any of our operators) in the inserted prologue.
    pub fn schema_prefiltered(self) -> bool {
        matches!(self, Self::Ref | Self::ImplTrait | Self::Dyn)
    }

    pub fn replacements(self) -> &'static [&'static str] {
        match self {
            Self::Unit => &["{}"],
            Self::Bool => &["{ true }", "{ false }"],
            Self::Number => &["{ 0 }", "{ 1 }"],
            Self::Stringy => &[r#"{ String::new() }"#, r#"{ "xyzzy".into() }"#],
            Self::Option => &["{ None }", "{ Some(Default::default()) }"],
            Self::Result => &["{ Ok(Default::default()) }"],
            Self::Vec => &["{ vec![] }"],
            Self::Never => &[],
            Self::Ref | Self::ImplTrait | Self::Dyn | Self::Other => &["{ Default::default() }"],
        }
    }
}

pub fn kind_is_schema_prefiltered(label: &str) -> bool {
    matches!(label, "ref" | "impl_trait" | "dyn")
}

#[derive(Debug, Clone)]
pub struct DiscoverFilter {
    pub packages: Vec<String>,
    pub files: Vec<String>,
    pub in_diff: Option<String>,
}

pub fn discover(root: &Path, filter: &DiscoverFilter) -> Result<Vec<Mutant>> {
    let opened = crate::bifrost::open_workspace(root)?;
    discover_in(&opened.workspace, root, filter)
}

pub fn discover_in(
    workspace: &WorkspaceAnalyzer,
    root: &Path,
    filter: &DiscoverFilter,
) -> Result<Vec<Mutant>> {
    let meta = load_metadata(root)?;
    let globset = file_globset(&filter.files)?;
    let diff_fqns = if let Some(spec) = &filter.in_diff {
        Some(diff_symbol_fqns(root, spec)?)
    } else {
        None
    };

    let analyzer = workspace.analyzer();

    let mut mutants = Vec::new();
    for unit in analyzer.all_declarations() {
        if unit.is_synthetic() {
            continue;
        }
        if unit.kind() != CodeUnitType::Function {
            continue;
        }
        let rel = crate::util::rel_slash(unit.source().rel_path());
        if !rel.ends_with(".rs") {
            continue;
        }
        let lang = unit
            .source()
            .rel_path()
            .extension()
            .and_then(|e| e.to_str())
            .map(Language::from_extension)
            .unwrap_or(Language::None);
        if analyzer.in_test_region(&unit) || test_paths::is_test_like_path(&rel, lang) {
            continue;
        }
        let Some(package) = package_for_path(&meta, &rel) else {
            continue;
        };
        if !filter.packages.is_empty() && !filter.packages.iter().any(|p| p == &package) {
            continue;
        }
        if let Some(gs) = &globset {
            if !gs.is_match(&rel) {
                continue;
            }
        }
        let fqn = unit.fq_name();
        if let Some(allowed) = &diff_fqns {
            if !allowed.contains(&fqn) {
                continue;
            }
        }
        let Some(range) = analyzer.ranges_of(&unit).into_iter().next() else {
            continue;
        };
        let source = match unit.source().read_to_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        if range.end_byte > source.len() || range.start_byte >= range.end_byte {
            continue;
        }
        let snippet = &source[range.start_byte..range.end_byte];
        let parsed = match parse_fn(snippet) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if parsed.skip {
            continue;
        }
        if parsed.unsafe_fn || parsed.const_fn {
            continue;
        }
        if parsed.boring_body {
            continue;
        }
        let body_start = range.start_byte + parsed.body_start;
        let body_end = range.start_byte + parsed.body_end;
        if body_end > source.len() || body_start >= body_end {
            continue;
        }
        let original_body = &source[body_start..body_end];
        for repl in parsed.kind.replacements() {
            if collapse_ws(repl) == collapse_ws(original_body) {
                continue;
            }
            let id = mutant_id(&package, &rel, &fqn, repl);
            mutants.push(Mutant {
                id,
                package: package.clone(),
                path: rel.clone(),
                fqn: fqn.clone(),
                start_byte: range.start_byte,
                end_byte: range.end_byte,
                start_line: range.start_line,
                end_line: range.end_line,
                body_start_byte: body_start,
                body_end_byte: body_end,
                replacement: (*repl).to_string(),
                return_kind: parsed.kind.label().to_string(),
            });
        }
    }
    mutants.sort_by(|a, b| {
        (&a.package, &a.path, a.start_byte, &a.replacement).cmp(&(
            &b.package,
            &b.path,
            b.start_byte,
            &b.replacement,
        ))
    });
    Ok(mutants)
}

pub fn mutant_id(package: &str, path: &str, fqn: &str, replacement: &str) -> String {
    let mut h = Sha256::new();
    h.update(package.as_bytes());
    h.update([0]);
    h.update(path.as_bytes());
    h.update([0]);
    h.update(fqn.as_bytes());
    h.update([0]);
    h.update(replacement.as_bytes());
    hex(&h.finalize())[..16].to_string()
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn file_globset(patterns: &[String]) -> Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        b.add(Glob::new(p).with_context(|| format!("invalid glob {p}"))?);
    }
    Ok(Some(b.build()?))
}

fn diff_symbol_fqns(root: &Path, spec: &str) -> Result<BTreeSet<String>> {
    let params = if let Some((base, target)) = spec.split_once("...") {
        AnalyzeDiffParams {
            base: Some(base.to_string()),
            target: Some(target.to_string()),
            include_tests: false,
            call_edges: DiffCallEdges::Omit,
        }
    } else {
        AnalyzeDiffParams {
            base: Some(spec.to_string()),
            target: None,
            include_tests: false,
            call_edges: DiffCallEdges::Omit,
        }
    };
    let result = analyze_diff_at_root(
        root,
        params,
        &DiffAnalysisOptions::default(),
        &CancellationToken::default(),
    )
    .map_err(|e| anyhow::anyhow!("analyze_diff: {e}"))?;
    let mut set = BTreeSet::new();
    for e in result.patch_symbols.edited {
        set.insert(e.after.fqn);
    }
    for i in result.patch_symbols.introduced {
        set.insert(i.after.fqn);
    }
    Ok(set)
}

struct ParsedFn {
    kind: ReturnKind,
    unsafe_fn: bool,
    const_fn: bool,
    skip: bool,
    boring_body: bool,
    body_start: usize,
    body_end: usize,
}

fn parse_fn(snippet: &str) -> Result<ParsedFn> {
    if let Ok(item) = syn::parse_str::<ItemFn>(snippet) {
        return from_item_fn(snippet, &item);
    }
    if let Ok(item) = syn::parse_str::<ImplItemFn>(snippet) {
        return from_impl_fn(snippet, &item);
    }
    anyhow::bail!("not a function")
}

fn from_item_fn(snippet: &str, item: &ItemFn) -> Result<ParsedFn> {
    let kind = classify_return(&item.sig.output);
    let (body_start, body_end) = block_offsets(snippet, &item.block)?;
    Ok(ParsedFn {
        kind,
        unsafe_fn: item.sig.unsafety.is_some(),
        const_fn: item.sig.constness.is_some(),
        skip: has_skip(&item.attrs),
        boring_body: boring_body(&item.block),
        body_start,
        body_end,
    })
}

fn from_impl_fn(snippet: &str, item: &ImplItemFn) -> Result<ParsedFn> {
    let kind = classify_return(&item.sig.output);
    let (body_start, body_end) = block_offsets(snippet, &item.block)?;
    Ok(ParsedFn {
        kind,
        unsafe_fn: item.sig.unsafety.is_some(),
        const_fn: item.sig.constness.is_some(),
        skip: has_skip(&item.attrs),
        boring_body: boring_body(&item.block),
        body_start,
        body_end,
    })
}

fn block_offsets(snippet: &str, block: &Block) -> Result<(usize, usize)> {
    let span = block.span();
    let start = span.start();
    let end = span.end();
    let body_start = line_col_to_offset(snippet, start.line, start.column);
    let body_end = line_col_to_offset(snippet, end.line, end.column);
    if body_start >= body_end || body_end > snippet.len() {
        anyhow::bail!("body span out of range");
    }
    Ok((body_start, body_end))
}

fn has_skip(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let segs: Vec<String> = a
            .path()
            .segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect();
        segs == ["quicksilver", "skip"] || segs == ["mutants", "skip"]
    })
}

fn boring_body(block: &Block) -> bool {
    if block.stmts.is_empty() {
        return true;
    }
    if block.stmts.len() != 1 {
        return false;
    }
    match &block.stmts[0] {
        syn::Stmt::Macro(m) => is_placeholder_macro(&m.mac.path),
        syn::Stmt::Expr(syn::Expr::Macro(m), _) => is_placeholder_macro(&m.mac.path),
        syn::Stmt::Expr(syn::Expr::Verbatim(_), _) => false,
        _ => false,
    }
}

fn is_placeholder_macro(path: &syn::Path) -> bool {
    path.segments
        .last()
        .map(|s| {
            matches!(
                s.ident.to_string().as_str(),
                "panic" | "todo" | "unimplemented" | "unreachable"
            )
        })
        .unwrap_or(false)
}

pub fn classify_return(output: &ReturnType) -> ReturnKind {
    match output {
        ReturnType::Default => ReturnKind::Unit,
        ReturnType::Type(_, ty) => classify_type(ty),
    }
}

pub fn classify_type(ty: &Type) -> ReturnKind {
    match ty {
        Type::Tuple(t) if t.elems.is_empty() => ReturnKind::Unit,
        Type::Paren(p) => classify_type(&p.elem),
        Type::Group(g) => classify_type(&g.elem),
        Type::Never(_) => ReturnKind::Never,
        Type::ImplTrait(_) => ReturnKind::ImplTrait,
        Type::TraitObject(_) => ReturnKind::Dyn,
        Type::Reference(r) => {
            let inner = r.elem.to_token_stream().to_string().replace(' ', "");
            if inner == "str" {
                ReturnKind::Stringy
            } else {
                ReturnKind::Ref
            }
        }
        Type::Path(p) => classify_path(p),
        _ => ReturnKind::Other,
    }
}

fn classify_path(p: &syn::TypePath) -> ReturnKind {
    let last = match p.path.segments.last() {
        Some(s) => s,
        None => return ReturnKind::Other,
    };
    match last.ident.to_string().as_str() {
        "bool" => ReturnKind::Bool,
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" | "f32" | "f64" => ReturnKind::Number,
        "String" => ReturnKind::Stringy,
        "Option" => ReturnKind::Option,
        "Result" => ReturnKind::Result,
        "Vec" => ReturnKind::Vec,
        _ => ReturnKind::Other,
    }
}

/// Parse a standalone function snippet. Used by unit tests without standing up bifrost.
pub fn mutants_from_snippet(
    package: &str,
    path: &str,
    fqn: &str,
    snippet: &str,
    start_byte: usize,
    start_line: usize,
) -> Result<Vec<Mutant>> {
    let parsed = parse_fn(snippet)?;
    if parsed.skip || parsed.unsafe_fn || parsed.const_fn || parsed.boring_body {
        return Ok(Vec::new());
    }
    let original_body = &snippet[parsed.body_start..parsed.body_end];
    let mut out = Vec::new();
    for repl in parsed.kind.replacements() {
        if collapse_ws(repl) == collapse_ws(original_body) {
            continue;
        }
        out.push(Mutant {
            id: mutant_id(package, path, fqn, repl),
            package: package.into(),
            path: path.into(),
            fqn: fqn.into(),
            start_byte,
            end_byte: start_byte + snippet.len(),
            start_line,
            end_line: start_line,
            body_start_byte: start_byte + parsed.body_start,
            body_end_byte: start_byte + parsed.body_end,
            replacement: (*repl).to_string(),
            return_kind: parsed.kind.label().to_string(),
        });
    }
    Ok(out)
}

pub fn apply_mutant(root: &Path, mutant: &Mutant) -> Result<Vec<u8>> {
    let path = root.join(&mutant.path);
    let original = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let mut text = String::from_utf8(original.clone()).context("source is not utf-8")?;
    if mutant.body_end_byte > text.len() || mutant.body_start_byte > mutant.body_end_byte {
        anyhow::bail!("mutant body range out of date for {}", mutant.path);
    }
    text.replace_range(
        mutant.body_start_byte..mutant.body_end_byte,
        &mutant.replacement,
    );
    std::fs::write(&path, text.as_bytes())?;
    Ok(original)
}

pub fn restore_file(root: &Path, mutant: &Mutant, original: &[u8]) -> Result<()> {
    std::fs::write(root.join(&mutant.path), original)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_replacements() {
        let m = mutants_from_snippet(
            "pkg",
            "src/lib.rs",
            "pkg::flag",
            "pub fn flag() -> bool { true }",
            0,
            1,
        )
        .unwrap();
        let bodies: Vec<_> = m.iter().map(|x| x.replacement.as_str()).collect();
        assert!(bodies.contains(&"{ false }"));
        assert!(!bodies.contains(&"{ true }"));
    }

    #[test]
    fn skips_empty_and_todo() {
        assert!(mutants_from_snippet("p", "a.rs", "p::e", "fn e() {}", 0, 1)
            .unwrap()
            .is_empty());
        assert!(
            mutants_from_snippet("p", "a.rs", "p::t", "fn t() { todo!() }", 0, 1)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn skips_unsafe_and_const() {
        assert!(
            mutants_from_snippet("p", "a.rs", "p::u", "unsafe fn u() -> i32 { 1 }", 0, 1)
                .unwrap()
                .is_empty()
        );
        assert!(
            mutants_from_snippet("p", "a.rs", "p::c", "const fn c() -> i32 { 1 }", 0, 1)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn skips_quicksilver_attr() {
        let s = "#[quicksilver::skip]\nfn x() -> i32 { 1 }";
        assert!(mutants_from_snippet("p", "a.rs", "p::x", s, 0, 1)
            .unwrap()
            .is_empty());
        let s = "#[mutants::skip]\nfn x() -> i32 { 1 }";
        assert!(mutants_from_snippet("p", "a.rs", "p::x", s, 0, 1)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn ids_ignore_line_numbers() {
        let a = mutant_id("p", "src/lib.rs", "p::add", "{ 0 }");
        let b = mutant_id("p", "src/lib.rs", "p::add", "{ 0 }");
        assert_eq!(a, b);
        let c = mutant_id("p", "src/lib.rs", "p::add", "{ 1 }");
        assert_ne!(a, c);
    }

    #[test]
    fn option_and_result() {
        let m = mutants_from_snippet(
            "p",
            "a.rs",
            "p::o",
            "fn o() -> Option<i32> { Some(1) }",
            0,
            1,
        )
        .unwrap();
        assert!(m.iter().any(|x| x.replacement.contains("None")));
        let m = mutants_from_snippet(
            "p",
            "a.rs",
            "p::r",
            "fn r() -> Result<i32, ()> { Ok(1) }",
            0,
            1,
        )
        .unwrap();
        assert!(m
            .iter()
            .any(|x| x.replacement.contains("Ok(Default::default())")));
    }

    #[test]
    fn classifies_ref_impl_trait_dyn_never() {
        let r = mutants_from_snippet(
            "p",
            "a.rs",
            "p::as_ref",
            "fn as_ref(x: &i32) -> &i32 { x }",
            0,
            1,
        )
        .unwrap();
        assert_eq!(r[0].return_kind, "ref");
        let m = mutants_from_snippet(
            "p",
            "a.rs",
            "p::as_mut",
            "fn as_mut(x: &mut i32) -> &mut i32 { x }",
            0,
            1,
        )
        .unwrap();
        assert_eq!(m[0].return_kind, "ref");
        let i = mutants_from_snippet(
            "p",
            "a.rs",
            "p::as_impl",
            "fn as_impl() -> impl std::fmt::Display { 1 }",
            0,
            1,
        )
        .unwrap();
        assert_eq!(i[0].return_kind, "impl_trait");
        let d = mutants_from_snippet(
            "p",
            "a.rs",
            "p::as_dyn",
            "fn as_dyn() -> &'static dyn std::fmt::Debug { &() }",
            0,
            1,
        )
        .unwrap();
        assert_eq!(d[0].return_kind, "ref");
        let bare = mutants_from_snippet(
            "p",
            "a.rs",
            "p::box_dyn",
            "fn box_dyn() -> Box<dyn std::fmt::Debug> { Box::new(()) }",
            0,
            1,
        )
        .unwrap();
        // Box<dyn ...> is a path ending in Box, not a bare trait object.
        assert_eq!(bare[0].return_kind, "other");
        let n = mutants_from_snippet(
            "p",
            "a.rs",
            "p::diverge",
            "fn diverge() -> ! { loop {} }",
            0,
            1,
        )
        .unwrap();
        assert!(n.is_empty(), "-> ! is skipped");
        assert!(kind_is_schema_prefiltered("ref"));
        assert!(kind_is_schema_prefiltered("impl_trait"));
        assert!(kind_is_schema_prefiltered("dyn"));
        assert!(!kind_is_schema_prefiltered("number"));
    }

    #[test]
    fn async_fn_is_not_skipped() {
        let m = mutants_from_snippet(
            "p",
            "a.rs",
            "p::add_async",
            "async fn add_async(a: i32, b: i32) -> i32 { a + b }",
            0,
            1,
        )
        .unwrap();
        assert!(m.iter().any(|x| x.replacement.contains('0')));
    }

    #[test]
    fn body_span_keeps_closing_brace_after_multibyte() {
        let snippet = "fn f() -> bool { let x = \"café\"; x.is_empty() }";
        let m = mutants_from_snippet("p", "a.rs", "p::f", snippet, 0, 1).unwrap();
        assert!(!m.is_empty());
        let body = &snippet[m[0].body_start_byte..m[0].body_end_byte];
        assert!(body.starts_with('{'), "body={body:?}");
        assert!(body.ends_with('}'), "body={body:?}");
    }
}
