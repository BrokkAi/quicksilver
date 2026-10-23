use crate::cargo_cmd::{load_metadata, package_for_path};
use crate::types::Mutant;
use crate::util::{hex, line_col_to_offset};
use anyhow::{Context, Result};
use brokk_bifrost_analysis::analyzer::test_paths;
use brokk_bifrost_analysis::{CancellationToken, CodeUnitType, Language, WorkspaceAnalyzer};
use brokk_bifrost_tools::diff_analysis::{
    analyze_diff_at_root, AnalyzeDiffParams, DiffAnalysisOptions, DiffCallEdges,
};
use globset::{Glob, GlobSet, GlobSetBuilder};
use quote::ToTokens;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
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
    let diff_started = std::time::Instant::now();
    let diff = match &filter.in_diff {
        Some(spec) => Some(diff_symbol_fqns(root, spec)?),
        None => None,
    };
    discover_with(
        workspace,
        root,
        filter,
        diff,
        diff_started.elapsed().as_secs_f64(),
    )
}

/// `--in-diff` discovery that analyzes only the files the diff touches,
/// through a scoped analyzer over the shared store, instead of opening the
/// whole workspace.
pub fn discover_in_diff(root: &Path, filter: &DiscoverFilter) -> Result<Vec<Mutant>> {
    let spec = filter
        .in_diff
        .as_deref()
        .context("discover_in_diff needs --in-diff")?;
    let diff_started = std::time::Instant::now();
    let diff = match git_diff_lines(root, spec)? {
        Some(lines) => DiffSelection::Lines(lines),
        None => diff_symbol_fqns(root, spec)?,
    };
    let diff_secs = diff_started.elapsed().as_secs_f64();
    let files: Vec<std::path::PathBuf> = diff
        .paths()
        .into_iter()
        .filter(|p| p.ends_with(".rs"))
        .map(std::path::PathBuf::from)
        .collect();
    if files.is_empty() {
        tracing::info!("discovery: {diff_secs:.2}s diff analysis, no changed functions");
        return Ok(Vec::new());
    }
    let opened = crate::bifrost::open_files(root, &files)?;
    discover_with(&opened.workspace, root, filter, Some(diff), diff_secs)
}

/// One function with a body, as discovery sees it before any filter that
/// decides whether the built-in generator mutates it.
#[derive(Debug, Clone)]
pub struct FnDecl {
    pub package: String,
    /// Repository-relative path using forward slashes.
    pub path: String,
    pub fqn: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
    /// Absolute byte range of the body, braces included.
    pub body_start_byte: usize,
    pub body_end_byte: usize,
    pub return_kind: ReturnKind,
    pub unsafe_fn: bool,
    pub const_fn: bool,
    /// Carries `#[quicksilver::skip]` or `#[mutants::skip]`.
    pub skip: bool,
    /// Empty, or a single `todo!()`-style placeholder.
    pub boring_body: bool,
    pub original_body: String,
}

/// Every function with a body in the workspace, outside test regions and
/// test-like paths, in mutable (non proc-macro) packages.
pub fn scan_workspace(root: &Path, filter: &DiscoverFilter) -> Result<Vec<FnDecl>> {
    let opened = crate::bifrost::open_workspace(root)?;
    scan_functions(&opened.workspace, root, filter, None, 0.0)
}

fn discover_with(
    workspace: &WorkspaceAnalyzer,
    root: &Path,
    filter: &DiscoverFilter,
    diff_fqns: Option<DiffSelection>,
    diff_secs: f64,
) -> Result<Vec<Mutant>> {
    let fns = scan_functions(workspace, root, filter, diff_fqns, diff_secs)?;
    let mut mutants = Vec::new();
    for f in &fns {
        if f.skip || f.unsafe_fn || f.const_fn || f.boring_body {
            continue;
        }
        for repl in f.return_kind.replacements() {
            if collapse_ws(repl) == collapse_ws(&f.original_body) {
                continue;
            }
            let id = mutant_id(&f.package, &f.path, &f.fqn, repl);
            mutants.push(Mutant {
                id,
                package: f.package.clone(),
                path: f.path.clone(),
                fqn: f.fqn.clone(),
                start_byte: f.start_byte,
                end_byte: f.end_byte,
                start_line: f.start_line,
                end_line: f.end_line,
                body_start_byte: f.body_start_byte,
                body_end_byte: f.body_end_byte,
                replacement: (*repl).to_string(),
                return_kind: f.return_kind.label().to_string(),
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
    disambiguate_ids(&mut mutants);
    tracing::info!("discovery: {} mutants", mutants.len());
    Ok(mutants)
}

fn scan_functions(
    workspace: &WorkspaceAnalyzer,
    root: &Path,
    filter: &DiscoverFilter,
    diff_fqns: Option<DiffSelection>,
    diff_secs: f64,
) -> Result<Vec<FnDecl>> {
    let meta = load_metadata(root)?;
    let globset = file_globset(&filter.files)?;
    let scan_started = std::time::Instant::now();

    let analyzer = workspace.analyzer();
    let root_abs = std::path::PathBuf::from(meta.workspace_root.as_str());
    let mut scopes: std::collections::HashMap<String, Option<crate::cargo_cmd::MutableScope>> =
        std::collections::HashMap::new();
    let mut skipped_proc_macro: BTreeSet<String> = BTreeSet::new();

    let mut fns = Vec::new();
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
        // Under --in-diff only the changed files matter. Checking that first
        // keeps the per-declaration queries below (test region, package,
        // module scope) off every other function in the workspace: on
        // itertools in a debug build they took 8 s per commit.
        if let Some(diff) = &diff_fqns {
            if !diff.touches_file(&rel) {
                continue;
            }
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
        let scope = scopes
            .entry(package.clone())
            .or_insert_with(|| crate::cargo_cmd::mutable_scope(&meta, &package));
        let Some(scope) = scope.as_ref() else {
            continue;
        };
        if scope.proc_macro {
            skipped_proc_macro.insert(package.clone());
            continue;
        }
        if !scope.contains(&root_abs.join(&rel)) {
            continue;
        }
        if !filter.packages.is_empty() && !filter.packages.iter().any(|p| p == &package) {
            continue;
        }
        if let Some(gs) = &globset {
            if !gs.is_match(&rel) {
                continue;
            }
        }
        let fqn = unit.fq_name();
        let Some(range) = analyzer.ranges_of(&unit).into_iter().next() else {
            continue;
        };
        if let Some(diff) = &diff_fqns {
            if !diff.selects(&rel, &fqn, range.start_line, range.end_line) {
                continue;
            }
        }
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
        let body_start = range.start_byte + parsed.body_start;
        let body_end = range.start_byte + parsed.body_end;
        if body_end > source.len() || body_start >= body_end {
            continue;
        }
        fns.push(FnDecl {
            package: package.clone(),
            path: rel.clone(),
            fqn: fqn.clone(),
            start_byte: range.start_byte,
            end_byte: range.end_byte,
            start_line: range.start_line,
            end_line: range.end_line,
            body_start_byte: body_start,
            body_end_byte: body_end,
            return_kind: parsed.kind,
            unsafe_fn: parsed.unsafe_fn,
            const_fn: parsed.const_fn,
            skip: parsed.skip,
            boring_body: parsed.boring_body,
            original_body: source[body_start..body_end].to_string(),
        });
    }
    fns.sort_by(|a, b| {
        (&a.package, &a.path, a.start_byte).cmp(&(&b.package, &b.path, b.start_byte))
    });
    tracing::info!(
        "discovery: {:.2}s diff analysis, {:.2}s declaration scan, {} functions",
        diff_secs,
        scan_started.elapsed().as_secs_f64(),
        fns.len()
    );
    if !skipped_proc_macro.is_empty() {
        eprintln!(
            "skipping proc-macro crates (their code runs inside rustc at build time, so run-time mutant switching cannot test it): {}",
            skipped_proc_macro.into_iter().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(fns)
}

/// Two functions in one file can share an fqn (`impl Display for Version`
/// and `impl Debug for Version` both give `semver.Version.fmt`). Their
/// mutants then hash to one id, and the executor, which finds a mutant's
/// schema site by id, gives one mutant the other's verdict. The first in
/// source order keeps the plain id, so existing state stays valid; later
/// ones hash in their ordinal, which is stable when lines shift.
pub fn disambiguate_ids(mutants: &mut [Mutant]) {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for m in mutants.iter_mut() {
        let n = seen.entry(m.id.clone()).or_insert(0);
        if *n > 0 {
            m.id = mutant_id(
                &m.package,
                &m.path,
                &m.fqn,
                &format!("{}\0#{}", m.replacement, n),
            );
        }
        *n += 1;
    }
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

/// Which functions a diff selects.
enum DiffSelection {
    /// Bifrost's diff analysis: the edited and introduced symbols, by name,
    /// and the files they are in.
    Symbols {
        fqns: BTreeSet<String>,
        paths: BTreeSet<String>,
    },
    /// `git diff -U0`: per changed file, the lines of the checked-out version
    /// that the diff touches. A function is selected when its line range
    /// overlaps one of them, as cargo-mutants does for `--in-diff`.
    Lines(BTreeMap<String, Vec<Hunk>>),
}

/// One hunk of a `-U0` diff on the new side, in 1-based lines.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hunk {
    /// Lines `first..=last` were added or changed.
    Lines(usize, usize),
    /// Lines were only removed, between line `after` and the next one.
    RemovedAfter(usize),
}

impl Hunk {
    fn overlaps(self, start_line: usize, end_line: usize) -> bool {
        match self {
            Hunk::Lines(first, last) => first <= end_line && start_line <= last,
            Hunk::RemovedAfter(after) => start_line <= after && after < end_line,
        }
    }
}

impl DiffSelection {
    fn paths(&self) -> BTreeSet<String> {
        match self {
            DiffSelection::Symbols { paths, .. } => paths.clone(),
            DiffSelection::Lines(files) => files.keys().cloned().collect(),
        }
    }

    fn touches_file(&self, rel: &str) -> bool {
        let rel = rel.trim_start_matches("./");
        match self {
            DiffSelection::Symbols { paths, .. } => paths.contains(rel),
            DiffSelection::Lines(files) => files.contains_key(rel),
        }
    }

    fn selects(&self, rel: &str, fqn: &str, start_line: usize, end_line: usize) -> bool {
        match self {
            DiffSelection::Symbols { fqns, .. } => fqns.contains(fqn),
            DiffSelection::Lines(files) => files
                .get(rel.trim_start_matches("./"))
                .is_some_and(|hunks| hunks.iter().any(|h| h.overlaps(start_line, end_line))),
        }
    }
}

/// The lines a diff touches in the checked-out files, or `None` when the
/// diff's target is not what is checked out (its line numbers would then
/// describe other text) or git cannot produce the diff.
fn git_diff_lines(root: &Path, spec: &str) -> Result<Option<BTreeMap<String, Vec<Hunk>>>> {
    let target = spec.split_once("...").map(|(_, target)| target);
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "core.quotePath=false",
            "diff",
            "-U0",
            "--no-color",
            "--no-ext-diff",
            "--no-renames",
            "--relative",
            spec,
        ])
        .current_dir(root)
        .output()
        .context("git diff")?;
    if !out.status.success() {
        tracing::warn!(
            "git diff {spec} failed ({}); using Bifrost's diff analysis",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return Ok(None);
    }
    let files = parse_unified_zero(&String::from_utf8_lossy(&out.stdout));
    if let Some(target) = target {
        if !files.is_empty() {
            let same = std::process::Command::new("git")
                .args(["diff", "--quiet", target, "--"])
                .args(files.keys())
                .current_dir(root)
                .status()
                .context("git diff --quiet")?;
            if !same.success() {
                tracing::info!(
                    "--in-diff target {target} is not what is checked out; using Bifrost's diff analysis"
                );
                return Ok(None);
            }
        }
    }
    Ok(Some(files))
}

/// Parse `git diff -U0` output into new-side hunks per file. Deleted files
/// are left out: nothing in them can be mutated.
fn parse_unified_zero(diff: &str) -> BTreeMap<String, Vec<Hunk>> {
    let mut files: BTreeMap<String, Vec<Hunk>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            let path = path.trim_end_matches('\t');
            current = path.strip_prefix("b/").map(str::to_string);
            continue;
        }
        let Some(header) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some(file) = &current else { continue };
        let Some(new_side) = header.split_whitespace().find(|w| w.starts_with('+')) else {
            continue;
        };
        let mut parts = new_side[1..].splitn(2, ',');
        let Some(start) = parts.next().and_then(|n| n.parse::<usize>().ok()) else {
            continue;
        };
        let count = parts
            .next()
            .map_or(Some(1), |n| n.parse::<usize>().ok())
            .unwrap_or(1);
        let hunk = if count == 0 {
            Hunk::RemovedAfter(start)
        } else {
            Hunk::Lines(start, start + count - 1)
        };
        files.entry(file.clone()).or_default().push(hunk);
    }
    files
}

fn diff_symbol_fqns(root: &Path, spec: &str) -> Result<DiffSelection> {
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
    let mut fqns = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let after = result
        .patch_symbols
        .edited
        .into_iter()
        .map(|e| e.after)
        .chain(result.patch_symbols.introduced.into_iter().map(|i| i.after));
    for symbol in after {
        paths.insert(symbol.path.trim_start_matches("./").replace('\\', "/"));
        fqns.insert(symbol.fqn);
    }
    Ok(DiffSelection::Symbols { fqns, paths })
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
    #[test]
    fn unified_zero_diff_gives_new_side_hunks_per_file() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\n\
--- a/src/a.rs\n\
+++ b/src/a.rs\n\
@@ -3 +3 @@ fn a() {\n\
-    1\n\
+    2\n\
@@ -10,2 +11,0 @@\n\
-gone\n\
-gone\n\
@@ -20,0 +20,3 @@\n\
+new\n\
diff --git a/src/b.rs b/src/b.rs\n\
new file mode 100644\n\
--- /dev/null\n\
+++ b/src/b.rs\n\
@@ -0,0 +1,4 @@\n\
+fn b() {}\n\
diff --git a/src/c.rs b/src/c.rs\n\
deleted file mode 100644\n\
--- a/src/c.rs\n\
+++ /dev/null\n\
@@ -1,2 +0,0 @@\n\
-fn c() {}\n";
        let files = parse_unified_zero(diff);
        assert_eq!(
            files.get("src/a.rs").unwrap(),
            &vec![
                Hunk::Lines(3, 3),
                Hunk::RemovedAfter(11),
                Hunk::Lines(20, 22)
            ]
        );
        assert_eq!(files.get("src/b.rs").unwrap(), &vec![Hunk::Lines(1, 4)]);
        assert!(
            !files.contains_key("src/c.rs"),
            "deleted files have nothing to mutate"
        );
    }

    #[test]
    fn a_function_is_selected_when_a_hunk_touches_its_lines() {
        // A function on lines 10..=20.
        assert!(Hunk::Lines(10, 10).overlaps(10, 20));
        assert!(Hunk::Lines(20, 25).overlaps(10, 20));
        assert!(Hunk::Lines(5, 30).overlaps(10, 20));
        assert!(!Hunk::Lines(21, 22).overlaps(10, 20));
        assert!(!Hunk::Lines(1, 9).overlaps(10, 20));
        // Lines removed from inside the body.
        assert!(Hunk::RemovedAfter(10).overlaps(10, 20));
        assert!(Hunk::RemovedAfter(19).overlaps(10, 20));
        // Lines removed just before or just after it.
        assert!(!Hunk::RemovedAfter(9).overlaps(10, 20));
        assert!(!Hunk::RemovedAfter(20).overlaps(10, 20));
    }

    #[test]
    fn same_fqn_twice_in_a_file_gets_distinct_ids() {
        let m = |start: usize| crate::types::Mutant {
            id: mutant_id(
                "semver",
                "src/display.rs",
                "semver.Version.fmt",
                "{ Ok(Default::default()) }",
            ),
            package: "semver".into(),
            path: "src/display.rs".into(),
            fqn: "semver.Version.fmt".into(),
            start_byte: start,
            end_byte: start + 10,
            start_line: 1,
            end_line: 2,
            body_start_byte: start,
            body_end_byte: start + 10,
            replacement: "{ Ok(Default::default()) }".into(),
            return_kind: "result".into(),
        };
        let mut ms = vec![m(100), m(900)];
        let first = ms[0].id.clone();
        disambiguate_ids(&mut ms);
        assert_eq!(ms[0].id, first, "first keeps its id");
        assert_ne!(ms[0].id, ms[1].id);
    }

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
