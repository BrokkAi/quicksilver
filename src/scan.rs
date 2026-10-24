//! List the functions of a crate by walking its module tree with `syn`, the
//! way rustc finds them: from the crate root through `mod x;`, `mod x { }`
//! and `#[path]`.
//!
//! Names follow the form QS has always hashed into mutant ids: crate name,
//! module path, the self type for methods (or the trait for default
//! methods), then the function name, joined with `.`.

use crate::target::Target;
use anyhow::{Context, Result};
use quote::ToTokens;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ScannedFn {
    pub fqn: String,
    /// The item from its first attribute or visibility to the closing brace.
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
    /// The body, braces included.
    pub body_start_byte: usize,
    pub body_end_byte: usize,
    pub sig: syn::Signature,
    pub attrs: Vec<syn::Attribute>,
    pub block: syn::Block,
    /// Under `#[cfg(test)]`, or a `#[test]` function.
    pub in_test: bool,
    /// The `cfg` (on the function or an enclosing item) that is false on
    /// the target, such as `cfg(windows)` on Linux.
    pub cfg_off: Option<String>,
    /// A method of an `impl Trait for Type` block: called through the
    /// trait, so its name need not appear at any call site.
    pub trait_impl: bool,
    /// A `cfg` (on the function or an enclosing item) that mentions `test`
    /// without requiring it, such as `cfg(any(test, feature = "testing"))`:
    /// test scaffolding compiled into some non-test builds.
    pub cfg_maybe_test: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub source: String,
    pub functions: Vec<ScannedFn>,
    /// First and last lines of test code in this file: `#[cfg(test)]`
    /// modules and impls, test functions, or the whole file.
    pub test_lines: Vec<(usize, usize)>,
}

/// Every function with a body reachable from `root_file`, with no target
/// known (nothing is marked compiled out).
pub fn scan_crate(crate_name: &str, root_file: &Path) -> Result<Vec<ScannedFile>> {
    scan_crate_for(crate_name, root_file, &Target::default())
}

/// [`scan_crate`], marking functions whose `cfg` is false on `target`.
pub fn scan_crate_for(
    crate_name: &str,
    root_file: &Path,
    target: &Target,
) -> Result<Vec<ScannedFile>> {
    let mut walker = Walker {
        files: Vec::new(),
        visited: HashSet::new(),
        target,
    };
    let root_file = root_file
        .canonicalize()
        .with_context(|| format!("crate root {}", root_file.display()))?;
    walker.scan_file(&root_file, &[crate_name.to_string()], &Ctx::default(), true)?;
    Ok(walker.files)
}

struct Walker<'t> {
    files: Vec<ScannedFile>,
    visited: HashSet<PathBuf>,
    target: &'t Target,
}

/// What an item inherits from the items around it.
#[derive(Debug, Clone, Default)]
struct Ctx {
    in_test: bool,
    cfg_off: Option<String>,
    maybe_test: Option<String>,
}

impl Ctx {
    /// The context inside an item with `attrs`.
    fn inside(&self, attrs: &[syn::Attribute], target: &Target) -> Ctx {
        Ctx {
            in_test: self.in_test || is_cfg_test(attrs),
            cfg_off: self.cfg_off.clone().or_else(|| target.compiled_out(attrs)),
            maybe_test: self
                .maybe_test
                .clone()
                .or_else(|| cfg_mentioning_test(attrs)),
        }
    }

    /// The context of a function with `attrs`: `#[test]` makes it test code.
    fn function(&self, attrs: &[syn::Attribute], target: &Target) -> Ctx {
        let mut ctx = self.inside(attrs, target);
        ctx.in_test |= is_test_fn(attrs);
        ctx
    }
}

/// Where the walk of one file is: its directories, line index and target.
struct Env<'a> {
    dirs: &'a Dirs,
    index: &'a LineIndex,
    target: &'a Target,
}

/// Where `mod name;` and `#[path]` resolve from while walking one file.
struct Dirs {
    /// Directory of the file itself: `#[path]` on a non-inline `mod`.
    file_dir: PathBuf,
    /// Directory child modules live in: the file's directory for a mod-rs
    /// file (`lib.rs`, `main.rs`, `mod.rs`, the crate root), else
    /// `<dir>/<stem>`; inline modules append their names.
    child_dir: PathBuf,
    inline_depth: usize,
}

impl Walker<'_> {
    fn scan_file(
        &mut self,
        path: &Path,
        module_path: &[String],
        ctx: &Ctx,
        is_root: bool,
    ) -> Result<()> {
        if !self.visited.insert(path.to_path_buf()) {
            return Ok(());
        }
        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!("skipping {}: {err}", path.display());
                return Ok(());
            }
        };
        let file = match syn::parse_file(&source) {
            Ok(f) => f,
            Err(err) => {
                tracing::warn!("skipping {}: does not parse: {err}", path.display());
                return Ok(());
            }
        };
        let file_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let mod_rs = is_root || stem == "mod";
        let dirs = Dirs {
            child_dir: if mod_rs {
                file_dir.clone()
            } else {
                file_dir.join(stem)
            },
            file_dir,
            inline_depth: 0,
        };
        let index = LineIndex::new(&source);
        let mut sink = Sink::default();
        // Inner attributes (`#![cfg(windows)]`) apply to the whole file.
        let ctx = ctx.inside(&file.attrs, self.target);
        if ctx.in_test {
            sink.test_lines.push((1, usize::MAX));
        }
        let env = Env {
            dirs: &dirs,
            index: &index,
            target: self.target,
        };
        walk_items(&file.items, module_path, &ctx, &env, &mut sink);
        self.files.push(ScannedFile {
            path: path.to_path_buf(),
            source,
            functions: sink.functions,
            test_lines: sink.test_lines,
        });
        for (child, module_path, ctx) in sink.pending {
            self.scan_file(&child, &module_path, &ctx, false)?;
        }
        Ok(())
    }
}

/// Items declared anywhere inside a function body (in its statements,
/// nested blocks or closures), without descending into those items: their
/// own bodies are walked when they are scanned.
fn nested_items(block: &syn::Block) -> Vec<syn::Item> {
    struct Collect(Vec<syn::Item>);
    impl<'ast> syn::visit::Visit<'ast> for Collect {
        fn visit_item(&mut self, item: &'ast syn::Item) {
            self.0.push(item.clone());
        }
    }
    let mut c = Collect(Vec::new());
    syn::visit::Visit::visit_block(&mut c, block);
    c.0
}

/// A module file still to scan: its path, module path and context.
type Pending = (PathBuf, Vec<String>, Ctx);

/// What the walk of one file produces.
#[derive(Default)]
struct Sink {
    functions: Vec<ScannedFn>,
    pending: Vec<Pending>,
    test_lines: Vec<(usize, usize)>,
}

impl Sink {
    /// Record `item` as test code when it is where test code starts.
    fn note_test(&mut self, outer: &Ctx, inner: &Ctx, item: &impl syn::spanned::Spanned) {
        if inner.in_test && !outer.in_test {
            let span = item.span();
            self.test_lines.push((span.start().line, span.end().line));
        }
    }
}

/// One function to scan: its name, attributes, visibility, signature and
/// body.
struct FnParts<'a> {
    fqn: String,
    attrs: &'a [syn::Attribute],
    vis: Option<&'a syn::Visibility>,
    sig: &'a syn::Signature,
    block: &'a syn::Block,
    trait_impl: bool,
}

/// Scan one function and the items nested in its body, which are named
/// under it (`outer.Inner.method`, `outer.helper`) and are sites of their
/// own: a serde `Visitor` impl inside `deserialize` holds the real logic.
fn push_fn(f: FnParts<'_>, outer: &Ctx, env: &Env<'_>, sink: &mut Sink) {
    let ctx = outer.function(f.attrs, env.target);
    let mut scanned = scanned(
        f.fqn.clone(),
        f.attrs,
        f.vis,
        f.sig,
        f.block,
        &ctx,
        env.index,
    );
    scanned.trait_impl = f.trait_impl;
    if ctx.in_test && !outer.in_test {
        sink.test_lines.push((scanned.start_line, scanned.end_line));
    }
    sink.functions.push(scanned);
    let nested = nested_items(f.block);
    if !nested.is_empty() {
        walk_items(&nested, &[f.fqn], &ctx, env, sink);
    }
}

fn walk_items(
    items: &[syn::Item],
    module_path: &[String],
    ctx: &Ctx,
    env: &Env<'_>,
    sink: &mut Sink,
) {
    for item in items {
        match item {
            syn::Item::Fn(f) => push_fn(
                FnParts {
                    fqn: join(module_path, &f.sig.ident.to_string()),
                    attrs: &f.attrs,
                    vis: Some(&f.vis),
                    sig: &f.sig,
                    block: &f.block,
                    trait_impl: false,
                },
                ctx,
                env,
                sink,
            ),
            syn::Item::Impl(i) => {
                let outer = ctx;
                let ctx = ctx.inside(&i.attrs, env.target);
                sink.note_test(outer, &ctx, i);
                let owner = join(module_path, &self_type_name(&i.self_ty));
                for member in &i.items {
                    if let syn::ImplItem::Fn(m) = member {
                        push_fn(
                            FnParts {
                                fqn: format!("{owner}.{}", m.sig.ident),
                                attrs: &m.attrs,
                                vis: Some(&m.vis),
                                sig: &m.sig,
                                block: &m.block,
                                trait_impl: i.trait_.is_some(),
                            },
                            &ctx,
                            env,
                            sink,
                        );
                    }
                }
            }
            syn::Item::Trait(t) => {
                let outer = ctx;
                let ctx = ctx.inside(&t.attrs, env.target);
                sink.note_test(outer, &ctx, t);
                let owner = join(module_path, &t.ident.to_string());
                for member in &t.items {
                    if let syn::TraitItem::Fn(m) = member {
                        let Some(block) = &m.default else { continue };
                        push_fn(
                            FnParts {
                                fqn: format!("{owner}.{}", m.sig.ident),
                                attrs: &m.attrs,
                                vis: None,
                                sig: &m.sig,
                                block,
                                trait_impl: false,
                            },
                            &ctx,
                            env,
                            sink,
                        );
                    }
                }
            }
            syn::Item::Mod(m) => {
                let outer = ctx;
                let ctx = ctx.inside(&m.attrs, env.target);
                sink.note_test(outer, &ctx, m);
                let dirs = env.dirs;
                let name = m.ident.to_string();
                let child_path: Vec<String> = module_path
                    .iter()
                    .cloned()
                    .chain(std::iter::once(name.clone()))
                    .collect();
                match &m.content {
                    Some((_, items)) => {
                        let inner = Dirs {
                            file_dir: dirs.file_dir.clone(),
                            child_dir: dirs.child_dir.join(&name),
                            inline_depth: dirs.inline_depth + 1,
                        };
                        let inner_env = Env {
                            dirs: &inner,
                            index: env.index,
                            target: env.target,
                        };
                        walk_items(items, &child_path, &ctx, &inner_env, sink);
                    }
                    None => {
                        if let Some(file) = resolve_mod_file(&name, &m.attrs, dirs) {
                            sink.pending.push((file, child_path, ctx.clone()));
                        } else {
                            tracing::debug!(
                                "mod {}: no file found under {}",
                                child_path.join("."),
                                dirs.child_dir.display()
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn resolve_mod_file(name: &str, attrs: &[syn::Attribute], dirs: &Dirs) -> Option<PathBuf> {
    if let Some(p) = path_attr(attrs) {
        let base = if dirs.inline_depth == 0 {
            &dirs.file_dir
        } else {
            &dirs.child_dir
        };
        // Canonical, like the crate scope it is compared with: a `..` path
        // into a directory shared by several crates (bifrost's
        // `#[path = "../../../test-support/git.rs"]`) otherwise still
        // `starts_with` the crate's source directory, so its functions
        // became sites in every crate that includes it, and `git cat-file`
        // rejected the unnormalized path.
        return base.join(p).canonicalize().ok().filter(|p| p.is_file());
    }
    let flat = dirs.child_dir.join(format!("{name}.rs"));
    if flat.is_file() {
        return Some(flat);
    }
    let nested = dirs.child_dir.join(name).join("mod.rs");
    nested.is_file().then_some(nested)
}

fn path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|a| {
        if !a.path().is_ident("path") {
            return None;
        }
        match &a.meta {
            syn::Meta::NameValue(nv) => match &nv.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) => Some(s.value()),
                _ => None,
            },
            _ => None,
        }
    })
}

/// A `cfg` whose predicate implies `test`, such as `#[cfg(test)]` or
/// `#[cfg(all(feature = "x", test))]`: compiled only for tests.
pub fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("cfg")
            && a.parse_args::<syn::Meta>()
                .is_ok_and(|m| cfg_implies_test(&m))
    })
}

/// `test` holds whenever the predicate does: `test` itself, an `all` with
/// such a member, or an `any` whose every member is one.
fn cfg_implies_test(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(p) => p.is_ident("test"),
        syn::Meta::List(list) => {
            let Ok(args) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            ) else {
                return false;
            };
            if list.path.is_ident("all") {
                args.iter().any(cfg_implies_test)
            } else if list.path.is_ident("any") {
                !args.is_empty() && args.iter().all(cfg_implies_test)
            } else {
                false
            }
        }
        syn::Meta::NameValue(_) => false,
    }
}

/// The first `cfg` among `attrs` that mentions `test` without implying
/// it, as written.
fn cfg_mentioning_test(attrs: &[syn::Attribute]) -> Option<String> {
    fn mentions(tokens: proc_macro2::TokenStream) -> bool {
        tokens.into_iter().any(|t| match t {
            proc_macro2::TokenTree::Ident(i) => i == "test",
            proc_macro2::TokenTree::Group(g) => mentions(g.stream()),
            _ => false,
        })
    }
    attrs.iter().find_map(|a| {
        if !a.path().is_ident("cfg") {
            return None;
        }
        let meta = a.parse_args::<syn::Meta>().ok()?;
        (!cfg_implies_test(&meta) && mentions(meta.to_token_stream()))
            .then(|| crate::target::cfg_text(&meta))
    })
}

/// `#[test]`, `#[tokio::test]`, `#[bench]` and the like.
pub fn is_test_fn(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .is_some_and(|s| s.ident == "test" || s.ident == "bench")
    })
}

fn join(module_path: &[String], name: &str) -> String {
    let mut s = module_path.join(".");
    if !s.is_empty() {
        s.push('.');
    }
    s.push_str(name);
    s
}

/// The name a method's owner contributes: the last path segment of the self
/// type, through references and parentheses; anything else verbatim with
/// whitespace removed.
pub fn self_type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default(),
        syn::Type::Reference(r) => self_type_name(&r.elem),
        syn::Type::Paren(p) => self_type_name(&p.elem),
        syn::Type::Group(g) => self_type_name(&g.elem),
        other => other
            .to_token_stream()
            .to_string()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect(),
    }
}

fn scanned(
    fqn: String,
    attrs: &[syn::Attribute],
    vis: Option<&syn::Visibility>,
    sig: &syn::Signature,
    block: &syn::Block,
    ctx: &Ctx,
    index: &LineIndex,
) -> ScannedFn {
    let first = attrs
        .first()
        .map(|a| a.pound_token.span.start())
        .or_else(|| match vis {
            Some(syn::Visibility::Public(p)) => Some(p.span.start()),
            Some(syn::Visibility::Restricted(r)) => Some(r.pub_token.span.start()),
            _ => None,
        })
        .unwrap_or_else(|| sig_start(sig));
    let brace = block.brace_token.span.join();
    let (body_start, body_end) = (brace.start(), brace.end());
    ScannedFn {
        fqn,
        start_byte: index.offset(first.line, first.column),
        end_byte: index.offset(body_end.line, body_end.column),
        start_line: first.line,
        end_line: body_end.line,
        body_start_byte: index.offset(body_start.line, body_start.column),
        body_end_byte: index.offset(body_end.line, body_end.column),
        sig: sig.clone(),
        attrs: attrs.to_vec(),
        block: block.clone(),
        in_test: ctx.in_test,
        cfg_off: ctx.cfg_off.clone(),
        trait_impl: false,
        cfg_maybe_test: ctx.maybe_test.clone(),
    }
}

fn sig_start(sig: &syn::Signature) -> proc_macro2::LineColumn {
    if let Some(t) = &sig.constness {
        return t.span.start();
    }
    if let Some(t) = &sig.asyncness {
        return t.span.start();
    }
    if let Some(t) = &sig.unsafety {
        return t.span.start();
    }
    if let Some(abi) = &sig.abi {
        return abi.extern_token.span.start();
    }
    sig.fn_token.span.start()
}

/// Byte offsets of line starts, for turning proc-macro2's 1-based line and
/// 0-based character column into a byte offset.
pub struct LineIndex {
    starts: Vec<usize>,
    source_len: usize,
    lines: Vec<String>,
}

impl LineIndex {
    pub fn new(source: &str) -> Self {
        let mut starts = vec![0];
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self {
            starts,
            source_len: source.len(),
            lines: source.split('\n').map(str::to_string).collect(),
        }
    }

    pub fn offset(&self, line: usize, column: usize) -> usize {
        let Some(start) = line.checked_sub(1).and_then(|l| self.starts.get(l)) else {
            return self.source_len;
        };
        let text = self.lines.get(line - 1).map(String::as_str).unwrap_or("");
        let within: usize = text.chars().take(column).map(char::len_utf8).sum();
        (start + within).min(self.source_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn fqns(files: &[ScannedFile]) -> Vec<(String, bool)> {
        let mut out: Vec<(String, bool)> = files
            .iter()
            .flat_map(|f| f.functions.iter().map(|g| (g.fqn.clone(), g.in_test)))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn walks_mod_files_inline_modules_and_path_attributes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/lib.rs",
            "mod a;\nmod b;\n#[path = \"odd_name.rs\"]\nmod c;\nmod missing;\n\
             pub fn top() -> u32 { 1 }\n\
             pub mod inline { pub fn inner() {} mod deeper; }\n",
        );
        write(root, "src/a.rs", "pub fn in_a() {}\nmod a_child;\n");
        write(root, "src/a/a_child.rs", "pub fn in_a_child() {}\n");
        write(root, "src/b/mod.rs", "pub fn in_b() {}\nmod b_child;\n");
        write(root, "src/b/b_child.rs", "pub fn in_b_child() {}\n");
        write(root, "src/odd_name.rs", "pub fn in_c() {}\n");
        write(root, "src/inline/deeper.rs", "pub fn in_deeper() {}\n");
        let files = scan_crate("my_crate", &root.join("src/lib.rs")).unwrap();
        let names: Vec<String> = fqns(&files).into_iter().map(|(f, _)| f).collect();
        assert_eq!(
            names,
            vec![
                "my_crate.a.a_child.in_a_child",
                "my_crate.a.in_a",
                "my_crate.b.b_child.in_b_child",
                "my_crate.b.in_b",
                "my_crate.c.in_c",
                "my_crate.inline.deeper.in_deeper",
                "my_crate.inline.inner",
                "my_crate.top",
            ]
        );
    }

    #[test]
    fn methods_name_the_type_and_default_methods_name_the_trait() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/lib.rs",
            "pub struct Thing;\n\
             impl Thing { pub fn new() -> Self { Thing } }\n\
             impl std::fmt::Display for Thing { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) } }\n\
             impl<'a> From<&'a Thing> for String { fn from(_: &'a Thing) -> String { String::new() } }\n\
             pub trait Greet { fn hi(&self) -> u32 { 1 } fn must(&self); }\n\
             impl Greet for (u8, u8) { fn must(&self) {} }\n",
        );
        let files = scan_crate("k", &root.join("src/lib.rs")).unwrap();
        let names: Vec<String> = fqns(&files).into_iter().map(|(f, _)| f).collect();
        assert_eq!(
            names,
            vec![
                "k.(u8,u8).must",
                "k.Greet.hi",
                "k.String.from",
                "k.Thing.fmt",
                "k.Thing.new",
            ]
        );
    }

    #[test]
    fn test_code_is_marked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/lib.rs",
            "pub fn prod() {}\n\
             #[test] fn t1() {}\n\
             #[tokio::test] async fn t2() {}\n\
             #[cfg(test)] fn helper() {}\n\
             #[cfg(all(test, feature = \"x\"))] fn helper2() {}\n\
             #[cfg(any(test, feature = \"x\"))] fn not_only_test() {}\n\
             #[cfg(any(feature = \"x\", test))] fn not_only_test2() {}\n\
             #[cfg(any(test, all(test, unix)))] fn helper3() {}\n\
             #[cfg(not(test))] fn not_test() {}\n\
             #[cfg(test)] mod tests { pub fn h() {} mod more; }\n\
             #[cfg(test)] impl Foo { fn x(&self) {} }\n",
        );
        write(root, "src/tests/more.rs", "pub fn deep() {}\n");
        let files = scan_crate("k", &root.join("src/lib.rs")).unwrap();
        assert_eq!(
            fqns(&files),
            vec![
                ("k.Foo.x".into(), true),
                ("k.helper".into(), true),
                ("k.helper2".into(), true),
                ("k.helper3".into(), true),
                ("k.not_only_test".into(), false),
                ("k.not_only_test2".into(), false),
                ("k.not_test".into(), false),
                ("k.prod".into(), false),
                ("k.t1".into(), true),
                ("k.t2".into(), true),
                ("k.tests.h".into(), true),
                ("k.tests.more.deep".into(), true),
            ]
        );
    }

    #[test]
    fn ranges_cover_attributes_through_the_closing_brace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let src = "// head\n#[inline]\npub fn f(x: &str) -> bool {\n    x == \"café\"\n}\nconst fn g() {}\n";
        write(root, "src/lib.rs", src);
        let files = scan_crate("k", &root.join("src/lib.rs")).unwrap();
        let f = &files[0].functions[0];
        assert_eq!(
            &src[f.start_byte..f.end_byte],
            "#[inline]\npub fn f(x: &str) -> bool {\n    x == \"café\"\n}"
        );
        assert_eq!(
            &src[f.body_start_byte..f.body_end_byte],
            "{\n    x == \"café\"\n}"
        );
        assert_eq!((f.start_line, f.end_line), (2, 5));
        let g = &files[0].functions[1];
        assert_eq!(&src[g.start_byte..g.end_byte], "const fn g() {}");
        assert!(g.sig.constness.is_some());
    }

    #[test]
    fn items_nested_in_function_bodies_are_scanned_under_their_function() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/lib.rs",
            "pub struct X;\n\
             impl X {\n\
                 pub fn deserialize() -> u32 {\n\
                     struct V;\n\
                     impl V { fn visit_seq(&self) -> u32 { fn inner() -> u32 { 1 } inner() } }\n\
                     let f = || { fn in_closure() {} in_closure() };\n\
                     f();\n\
                     V.visit_seq()\n\
                 }\n\
             }\n\
             #[cfg(test)] fn t() { fn h() {} }\n",
        );
        let files = scan_crate("k", &root.join("src/lib.rs")).unwrap();
        assert_eq!(
            fqns(&files),
            vec![
                ("k.X.deserialize".into(), false),
                ("k.X.deserialize.V.visit_seq".into(), false),
                ("k.X.deserialize.V.visit_seq.inner".into(), false),
                ("k.X.deserialize.in_closure".into(), false),
                ("k.t".into(), true),
                ("k.t.h".into(), true),
            ]
        );
        let f = &files[0].functions;
        let outer = f.iter().find(|g| g.fqn == "k.X.deserialize").unwrap();
        let visit = f.iter().find(|g| g.fqn.ends_with("visit_seq")).unwrap();
        assert!(outer.body_start_byte < visit.start_byte && visit.end_byte < outer.body_end_byte);
    }

    #[test]
    fn functions_compiled_for_another_target_are_marked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let other = if cfg!(windows) { "unix" } else { "windows" };
        write(
            root,
            "src/lib.rs",
            &format!(
                "pub fn everywhere() {{}}\n\
                 #[cfg({other})] pub fn elsewhere() {{}}\n\
                 #[cfg(feature = \"x\")] pub fn featured() {{}}\n\
                 #[cfg({other})] mod platform;\n\
                 #[cfg({other})] impl S {{ fn m(&self) {{}} }}\n"
            ),
        );
        write(root, "src/platform.rs", "pub fn in_platform_file() {}\n");
        let target = Target::probe(root).expect("rustc");
        let files = scan_crate_for("k", &root.join("src/lib.rs"), &target).unwrap();
        let mut off: Vec<(String, Option<String>)> = files
            .iter()
            .flat_map(|f| {
                f.functions
                    .iter()
                    .map(|g| (g.fqn.clone(), g.cfg_off.clone()))
            })
            .collect();
        off.sort();
        let gone = Some(format!("cfg({other})"));
        assert_eq!(
            off,
            vec![
                ("k.S.m".into(), gone.clone()),
                ("k.elsewhere".into(), gone.clone()),
                ("k.everywhere".into(), None),
                ("k.featured".into(), None),
                ("k.platform.in_platform_file".into(), gone),
            ]
        );
    }

    #[test]
    fn trait_methods_test_lines_and_test_ish_cfgs_are_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let src = "pub struct S;\n\
                   impl std::fmt::Display for S {\n\
                       fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) }\n\
                   }\n\
                   impl S { pub fn own(&self) {} }\n\
                   #[cfg(any(test, feature = \"testing\"))]\n\
                   pub fn helper() {}\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                       fn t() {}\n\
                   }\n";
        write(root, "src/lib.rs", src);
        let files = scan_crate("k", &root.join("src/lib.rs")).unwrap();
        let f = |name: &str| {
            files[0]
                .functions
                .iter()
                .find(|g| g.fqn.ends_with(name))
                .unwrap()
                .clone()
        };
        assert!(f(".fmt").trait_impl);
        assert!(!f(".own").trait_impl);
        assert_eq!(
            f(".helper").cfg_maybe_test.as_deref(),
            Some("cfg(any(test, feature = \"testing\"))")
        );
        assert_eq!(f(".own").cfg_maybe_test, None);
        assert_eq!(files[0].test_lines, vec![(8, 11)]);
    }

    #[test]
    fn a_file_included_twice_is_scanned_once() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "src/lib.rs",
            "#[path = \"shared.rs\"] mod a;\n#[path = \"shared.rs\"] mod b;\n",
        );
        write(root, "src/shared.rs", "pub fn s() {}\n");
        let files = scan_crate("k", &root.join("src/lib.rs")).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(fqns(&files), vec![("k.a.s".into(), false)]);
    }

    /// bifrost's crates include `../../../test-support/git.rs`; the scope
    /// check and `git cat-file` both need the path without `..`.
    #[test]
    fn a_path_attribute_leaving_the_crate_resolves_to_a_normalized_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            root,
            "crates/k/src/lib.rs",
            "#[path = \"../../../shared/git.rs\"] mod git;\n",
        );
        write(root, "shared/git.rs", "pub fn g() {}\n");
        let files = scan_crate("k", &root.join("crates/k/src/lib.rs")).unwrap();
        let shared = root.canonicalize().unwrap().join("shared/git.rs");
        assert!(
            files.iter().any(|f| f.path == shared),
            "{:?}",
            files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
    }
}
