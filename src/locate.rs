//! Locate exact function body bytes using the same names as captured edits.
use anyhow::Result;
use std::ops::Range;
use syn::{spanned::Spanned, visit::Visit};

#[derive(Debug, Clone)]
pub struct Located {
    pub name: String,
    pub line: usize,
    pub body: Range<usize>,
}

impl Located {
    pub(crate) fn matches(&self, fqn: &str) -> bool {
        fqn == self.name || fqn.ends_with(&format!(".{}", self.name))
    }
}

pub fn functions_named(source: &str, fqn: &str) -> Result<Vec<Located>> {
    Ok(functions(source)?
        .into_iter()
        .filter(|f| f.matches(fqn))
        .collect())
}

pub(crate) fn functions(source: &str) -> Result<Vec<Located>> {
    struct Locator<'a> {
        source: &'a str,
        found: Vec<Located>,
        types: Vec<Option<String>>,
    }
    impl Locator<'_> {
        fn add(&mut self, name: String, line: usize, block: &syn::Block) {
            let span = block.span();
            let offset = |p: proc_macro2::LineColumn| {
                crate::util::line_col_to_offset(self.source, p.line, p.column)
            };
            self.found.push(Located {
                name,
                line,
                body: offset(span.start())..offset(span.end()),
            });
        }
    }
    impl<'ast> Visit<'ast> for Locator<'_> {
        fn visit_item_fn(&mut self, i: &'ast syn::ItemFn) {
            self.add(i.sig.ident.to_string(), i.span().start().line, &i.block);
            syn::visit::visit_item_fn(self, i);
        }
        fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
            let ty = match &*i.self_ty {
                syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()),
                _ => None,
            };
            self.types.push(ty);
            syn::visit::visit_item_impl(self, i);
            self.types.pop();
        }
        fn visit_item_trait(&mut self, i: &'ast syn::ItemTrait) {
            self.types.push(Some(i.ident.to_string()));
            syn::visit::visit_item_trait(self, i);
            self.types.pop();
        }
        fn visit_trait_item_fn(&mut self, i: &'ast syn::TraitItemFn) {
            if let Some(block) = &i.default {
                let name = match self.types.last() {
                    Some(Some(ty)) => format!("{ty}.{}", i.sig.ident),
                    _ => i.sig.ident.to_string(),
                };
                self.add(name, i.span().start().line, block);
            }
            syn::visit::visit_trait_item_fn(self, i);
        }
        fn visit_impl_item_fn(&mut self, i: &'ast syn::ImplItemFn) {
            let name = match self.types.last() {
                Some(Some(ty)) => format!("{ty}.{}", i.sig.ident),
                _ => i.sig.ident.to_string(),
            };
            self.add(name, i.span().start().line, &i.block);
            syn::visit::visit_impl_item_fn(self, i);
        }
    }
    let file = syn::parse_file(source)?;
    let mut locator = Locator {
        source,
        found: vec![],
        types: vec![],
    };
    locator.visit_file(&file);
    Ok(locator.found)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trait_default_method_has_an_exact_body_and_required_method_has_none() {
        let src = "trait Answer { fn answer(&self) -> bool { true } fn required(&self); }";
        let functions = functions_named(src, "pkg.Answer.answer").unwrap();
        assert_eq!(functions.len(), 1);
        assert_eq!(functions[0].name, "Answer.answer");
        assert_eq!(&src[functions[0].body.clone()], "{ true }");
        assert!(functions_named(src, "pkg.Answer.required")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn body_offsets_count_utf8_bytes_with_unicode_on_the_same_line() {
        let src = r#"/* é */ fn f() -> &'static str { "é" }"#;
        let function = functions_named(src, "pkg.f").unwrap().remove(0);
        assert_eq!(&src[function.body], r#"{ "é" }"#);
    }

    #[test]
    fn free_function_and_exact_body() {
        let src = "// é\nfn free() -> bool { true }\n";
        let f = functions_named(src, "pkg.free").unwrap().remove(0);
        assert_eq!(f.name, "free");
        assert_eq!(f.line, 2);
        assert_eq!(&src[f.body], "{ true }");
    }
    #[test]
    fn methods_match_their_type() {
        let src = "impl A { fn get(&self) {} } impl B { fn get(&self) { todo!() } }";
        assert_eq!(functions_named(src, "pkg.A.get").unwrap().len(), 1);
        assert_eq!(functions_named(src, "pkg.B.get").unwrap()[0].name, "B.get");
        assert!(functions_named(src, "pkg.C.get").unwrap().is_empty());
    }
    #[test]
    fn nested_function() {
        assert_eq!(
            functions_named("fn outer() { fn inner() {} }", "pkg.inner")
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn duplicate_impls_return_all_matches() {
        assert_eq!(
            functions_named(
                "impl A { fn get() {} } impl A { fn get() { } }",
                "pkg.A.get"
            )
            .unwrap()
            .len(),
            2
        );
    }
    #[test]
    fn invalid_source_is_an_error() {
        assert!(functions_named("fn broken(", "broken").is_err());
    }
}
