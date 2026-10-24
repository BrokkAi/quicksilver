//! Finding proptest properties among the tests, so that verdicts they
//! decide alone can be flagged: a property runs random cases, so whether it
//! catches a mutant can change from one run to the next.

use crate::types::TestInfo;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::Path;

/// The ids of the tests in `tests` that are proptest properties: functions
/// declared inside a `proptest!` invocation, functions with a `#[proptest]`
/// attribute, and `#[test]` functions in a module named `*_properties`.
/// Matched by package and function name, so a plain test that shares a
/// name with a property in its package is counted too.
pub fn property_tests(
    root: &Path,
    meta: &cargo_metadata::Metadata,
    tests: &[TestInfo],
) -> Result<BTreeSet<String>> {
    let out = std::process::Command::new("git")
        .args(["ls-files", "-z", "--", "*.rs"])
        .current_dir(root)
        .output()
        .context("git ls-files")?;
    let mut names: BTreeSet<(String, String)> = BTreeSet::new();
    for rel in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let rel = String::from_utf8_lossy(rel).into_owned();
        let Ok(text) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        if !text.contains("proptest") && !text.contains("_properties") {
            continue;
        }
        let Some(package) = crate::cargo_cmd::package_for_path(meta, &rel) else {
            continue;
        };
        for name in property_names(&text) {
            names.insert((package.clone(), name));
        }
    }
    Ok(tests
        .iter()
        .filter(|t| {
            let last = t.name.rsplit("::").next().unwrap_or(&t.name);
            names.contains(&(t.package.clone(), last.to_string()))
        })
        .map(|t| t.id.clone())
        .collect())
}

/// Names of the property functions in one source file.
pub fn property_names(source: &str) -> BTreeSet<String> {
    struct Find {
        names: BTreeSet<String>,
        in_properties: usize,
    }
    fn fns_in(tokens: proc_macro2::TokenStream, out: &mut BTreeSet<String>) {
        let mut after_fn = false;
        for t in tokens {
            match t {
                proc_macro2::TokenTree::Ident(i) if after_fn => {
                    out.insert(i.to_string());
                    after_fn = false;
                }
                proc_macro2::TokenTree::Ident(i) => after_fn = i == "fn",
                proc_macro2::TokenTree::Group(g) => {
                    after_fn = false;
                    fns_in(g.stream(), out);
                }
                _ => after_fn = false,
            }
        }
    }
    impl<'ast> syn::visit::Visit<'ast> for Find {
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            if m.path
                .segments
                .last()
                .is_some_and(|s| s.ident == "proptest")
            {
                fns_in(m.tokens.clone(), &mut self.names);
            }
        }
        fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
            let attr = |name: &str| {
                f.attrs
                    .iter()
                    .any(|a| a.path().segments.last().is_some_and(|s| s.ident == name))
            };
            if attr("proptest") || (self.in_properties > 0 && attr("test")) {
                self.names.insert(f.sig.ident.to_string());
            }
            syn::visit::visit_item_fn(self, f);
        }
        fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
            let props = m.ident.to_string().ends_with("_properties");
            self.in_properties += usize::from(props);
            syn::visit::visit_item_mod(self, m);
            self.in_properties -= usize::from(props);
        }
    }
    let Ok(file) = syn::parse_file(source) else {
        return BTreeSet::new();
    };
    let mut find = Find {
        names: BTreeSet::new(),
        in_properties: 0,
    };
    syn::visit::Visit::visit_file(&mut find, &file);
    find.names
}

/// The flag for a verdict that only properties decided.
pub const NOTE: &str =
    "decided only by proptest properties, which run random cases: the verdict can change between runs";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_are_found_in_macros_attributes_and_modules() {
        let src = r#"
            proptest! {
                #![proptest_config(ProptestConfig::with_cases(64))]
                #[test]
                fn roundtrip(x in 0..10u8) { prop_assert!(x < 10); }
                #[test]
                fn other(s in ".*") {}
            }
            #[test_strategy::proptest]
            fn with_attribute(#[strategy(0..3u8)] x: u8) {}
            mod span_properties {
                #[test]
                fn lookup_is_stable() {}
                fn helper() {}
            }
            #[test]
            fn plain() {}
        "#;
        let names: Vec<String> = property_names(src).into_iter().collect();
        assert_eq!(
            names,
            vec!["lookup_is_stable", "other", "roundtrip", "with_attribute"]
        );
    }
}
