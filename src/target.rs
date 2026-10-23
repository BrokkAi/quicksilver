//! The compilation target, for leaving out code compiled only for other
//! targets (`#[cfg(windows)]` on Linux) and for recording where verdicts
//! came from.

use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Keys and names whose value the target alone decides. Everything else
/// (features, `debug_assertions`, `test`, custom cfgs) is unknown to QS, so
/// a predicate on it never marks code compiled out.
const TARGET_NAMES: &[&str] = &["unix", "windows"];
const TARGET_KEYS: &[&str] = &[
    "target_os",
    "target_family",
    "target_arch",
    "target_env",
    "target_vendor",
    "target_endian",
    "target_pointer_width",
    "target_abi",
];

#[derive(Debug, Clone, Default)]
pub struct Target {
    /// The host triple rustc reports, empty when unknown.
    pub triple: String,
    names: BTreeSet<String>,
    pairs: BTreeSet<(String, String)>,
    /// False when the probe failed: then nothing evaluates to false.
    known: bool,
}

fn rustc(dir: &Path) -> Command {
    let mut cmd = Command::new("rustc");
    cmd.current_dir(dir);
    // The workspace's rust-toolchain.toml decides, as for builds.
    for var in [
        "RUSTUP_TOOLCHAIN",
        "RUSTUP_TOOLCHAIN_SOURCE",
        "CARGO",
        "RUSTC_WRAPPER",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

impl Target {
    /// The host target of the toolchain that builds `dir`.
    pub fn probe(dir: &Path) -> Result<Target> {
        let out = rustc(dir)
            .args(["--print", "cfg"])
            .output()
            .context("rustc --print cfg")?;
        if !out.status.success() {
            bail!(
                "rustc --print cfg: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        let mut target = Target::parse_cfg(&String::from_utf8_lossy(&out.stdout));
        target.triple = host_triple(dir).unwrap_or_default();
        Ok(target)
    }

    /// The probe, or an unknown target (nothing compiled out) with a warning.
    pub fn probe_or_unknown(dir: &Path) -> Target {
        Target::probe(dir).unwrap_or_else(|err| {
            tracing::warn!("cannot tell the compilation target ({err:#}); no function is left out as compiled for another target");
            Target::default()
        })
    }

    fn parse_cfg(text: &str) -> Target {
        let mut t = Target {
            known: true,
            ..Target::default()
        };
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            match line.split_once('=') {
                Some((k, v)) => {
                    t.pairs
                        .insert((k.to_string(), v.trim_matches('"').to_string()));
                }
                None => {
                    t.names.insert(line.to_string());
                }
            }
        }
        t
    }

    /// The predicate's value on this target, or `None` when it depends on
    /// anything but the target.
    pub fn eval(&self, meta: &syn::Meta) -> Option<bool> {
        if !self.known {
            return None;
        }
        match meta {
            syn::Meta::Path(p) => {
                let name = p.get_ident()?.to_string();
                TARGET_NAMES
                    .contains(&name.as_str())
                    .then(|| self.names.contains(&name))
            }
            syn::Meta::NameValue(nv) => {
                let key = nv.path.get_ident()?.to_string();
                if !TARGET_KEYS.contains(&key.as_str()) {
                    return None;
                }
                let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(v),
                    ..
                }) = &nv.value
                else {
                    return None;
                };
                Some(self.pairs.contains(&(key, v.value())))
            }
            syn::Meta::List(list) => {
                let args = list
                    .parse_args_with(
                        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                    )
                    .ok()?;
                let values: Vec<Option<bool>> = args.iter().map(|m| self.eval(m)).collect();
                if list.path.is_ident("not") {
                    match values.as_slice() {
                        [Some(v)] => Some(!v),
                        _ => None,
                    }
                } else if list.path.is_ident("all") {
                    if values.contains(&Some(false)) {
                        Some(false)
                    } else if values.iter().all(|v| *v == Some(true)) {
                        Some(true)
                    } else {
                        None
                    }
                } else if list.path.is_ident("any") {
                    if values.contains(&Some(true)) {
                        Some(true)
                    } else if values.iter().all(|v| *v == Some(false)) {
                        Some(false)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
        }
    }

    /// The first `#[cfg(...)]` among `attrs` that is false on this target,
    /// as written, such as `cfg(windows)`.
    pub fn compiled_out(&self, attrs: &[syn::Attribute]) -> Option<String> {
        attrs.iter().find_map(|a| {
            if !a.path().is_ident("cfg") {
                return None;
            }
            let meta = a.parse_args::<syn::Meta>().ok()?;
            (self.eval(&meta) == Some(false)).then(|| {
                use quote::ToTokens;
                format!("cfg({})", meta.to_token_stream())
            })
        })
    }
}

/// `rustc -vV`'s host triple for the toolchain that builds `dir`.
pub fn host_triple(dir: &Path) -> Result<String> {
    let out = rustc(dir).arg("-vV").output().context("rustc -vV")?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_string))
        .context("rustc -vV printed no host line")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux() -> Target {
        Target::parse_cfg(
            "debug_assertions\npanic=\"unwind\"\ntarget_arch=\"x86_64\"\ntarget_env=\"gnu\"\n\
             target_family=\"unix\"\ntarget_os=\"linux\"\ntarget_pointer_width=\"64\"\nunix\n",
        )
    }

    fn eval(t: &Target, cfg: &str) -> Option<bool> {
        t.eval(&syn::parse_str::<syn::Meta>(cfg).unwrap())
    }

    #[test]
    fn target_predicates_evaluate_and_others_stay_unknown() {
        let t = linux();
        assert_eq!(eval(&t, "unix"), Some(true));
        assert_eq!(eval(&t, "windows"), Some(false));
        assert_eq!(eval(&t, "target_os = \"windows\""), Some(false));
        assert_eq!(eval(&t, "target_os = \"linux\""), Some(true));
        assert_eq!(eval(&t, "not(windows)"), Some(true));
        assert_eq!(eval(&t, "all(unix, target_os = \"macos\")"), Some(false));
        assert_eq!(eval(&t, "any(windows, target_os = \"macos\")"), Some(false));
        assert_eq!(eval(&t, "any(windows, unix)"), Some(true));
        // Features and other flags are not the target's to decide.
        assert_eq!(eval(&t, "feature = \"x\""), None);
        assert_eq!(eval(&t, "debug_assertions"), None);
        assert_eq!(eval(&t, "test"), None);
        assert_eq!(eval(&t, "all(windows, feature = \"x\")"), Some(false));
        assert_eq!(eval(&t, "any(windows, feature = \"x\")"), None);
        assert_eq!(eval(&t, "not(feature = \"x\")"), None);
        // An unknown target decides nothing.
        assert_eq!(eval(&Target::default(), "windows"), None);
    }

    #[test]
    fn compiled_out_names_the_false_predicate() {
        let t = linux();
        let attrs: syn::ItemFn = syn::parse_str("#[inline] #[cfg(windows)] fn f() {}").unwrap();
        assert_eq!(
            t.compiled_out(&attrs.attrs).as_deref(),
            Some("cfg(windows)")
        );
        let attrs: syn::ItemFn = syn::parse_str("#[cfg(unix)] fn f() {}").unwrap();
        assert_eq!(t.compiled_out(&attrs.attrs), None);
    }

    #[test]
    fn the_host_is_probed_from_rustc() {
        let dir = std::env::temp_dir();
        let t = Target::probe(&dir).expect("rustc is available");
        assert!(!t.triple.is_empty());
        assert_eq!(
            eval(&t, "any(unix, windows)"),
            Some(cfg!(any(unix, windows)))
        );
    }
}
