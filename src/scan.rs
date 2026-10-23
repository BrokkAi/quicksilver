//! List the functions of a crate by walking its module tree with `syn`, the
//! way rustc finds them: from the crate root through `mod x;`, `mod x { }`
//! and `#[path]`.
//!
//! Names follow the form QS has always hashed into mutant ids: crate name,
//! module path, the self type for methods (or the trait for default
//! methods), then the function name, joined with `.`.

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
}

#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub source: String,
    pub functions: Vec<ScannedFn>,
}

/// Every function with a body reachable from `root_file`.
pub fn scan_crate(crate_name: &str, root_file: &Path) -> Result<Vec<ScannedFile>> {
    let mut walker = Walker {
        files: Vec::new(),
        visited: HashSet::new(),
    };
    let root_file = root_file
        .canonicalize()
        .with_context(|| format!("crate root {}", root_file.display()))?;
    walker.scan_file(&root_file, &[crate_name.to_string()], false, true)?;
    Ok(walker.files)
}

struct Walker {
    files: Vec<ScannedFile>,
    visited: HashSet<PathBuf>,
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

impl Walker {
    fn scan_file(
        &mut self,
        path: &Path,
        module_path: &[String],
        in_test: bool,
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
        let mut functions = Vec::new();
        let mut pending: Vec<(PathBuf, Vec<String>, bool)> = Vec::new();
        walk_items(
            &file.items,
            module_path,
            in_test,
            &dirs,
            &index,
            &mut functions,
            &mut pending,
        );
        self.files.push(ScannedFile {
            path: path.to_path_buf(),
            source,
            functions,
        });
        for (child, module_path, in_test) in pending {
            self.scan_file(&child, &module_path, in_test, false)?;
        }
        Ok(())
    }
}

fn walk_items(
    items: &[syn::Item],
    module_path: &[String],
    in_test: bool,
    dirs: &Dirs,
    index: &LineIndex,
    out: &mut Vec<ScannedFn>,
    pending: &mut Vec<(PathBuf, Vec<String>, bool)>,
) {
    for item in items {
        match item {
            syn::Item::Fn(f) => {
                let test = in_test || is_cfg_test(&f.attrs) || is_test_fn(&f.attrs);
                out.push(scanned(
                    join(module_path, &f.sig.ident.to_string()),
                    &f.attrs,
                    Some(&f.vis),
                    &f.sig,
                    &f.block,
                    test,
                    index,
                ));
            }
            syn::Item::Impl(i) => {
                let test = in_test || is_cfg_test(&i.attrs);
                let owner = join(module_path, &self_type_name(&i.self_ty));
                for member in &i.items {
                    if let syn::ImplItem::Fn(m) = member {
                        let test = test || is_cfg_test(&m.attrs) || is_test_fn(&m.attrs);
                        out.push(scanned(
                            format!("{owner}.{}", m.sig.ident),
                            &m.attrs,
                            Some(&m.vis),
                            &m.sig,
                            &m.block,
                            test,
                            index,
                        ));
                    }
                }
            }
            syn::Item::Trait(t) => {
                let test = in_test || is_cfg_test(&t.attrs);
                let owner = join(module_path, &t.ident.to_string());
                for member in &t.items {
                    if let syn::TraitItem::Fn(m) = member {
                        let Some(block) = &m.default else { continue };
                        let test = test || is_cfg_test(&m.attrs);
                        out.push(scanned(
                            format!("{owner}.{}", m.sig.ident),
                            &m.attrs,
                            None,
                            &m.sig,
                            block,
                            test,
                            index,
                        ));
                    }
                }
            }
            syn::Item::Mod(m) => {
                let test = in_test || is_cfg_test(&m.attrs);
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
                        walk_items(items, &child_path, test, &inner, index, out, pending);
                    }
                    None => {
                        if let Some(file) = resolve_mod_file(&name, &m.attrs, dirs) {
                            pending.push((file, child_path, test));
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
        let p = base.join(p);
        return p.is_file().then_some(p);
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

/// `#[cfg(test)]` or `#[cfg(all(test, ...))]`: compiled only for tests.
pub fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        if !a.path().is_ident("cfg") {
            return false;
        }
        let syn::Meta::List(list) = &a.meta else {
            return false;
        };
        let tokens = list.tokens.to_string();
        let tokens = tokens.replace(' ', "");
        tokens == "test" || tokens.starts_with("all(test,") || tokens.ends_with(",test)")
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
    in_test: bool,
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
        in_test,
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
                ("k.not_only_test".into(), false),
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
}
