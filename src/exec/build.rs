use anyhow::Result;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Which file to stage for `test`. The executables this build just produced
/// come first: a worker's `target/` is cloned from the baseline, so an exact
/// `binary_rel` path can still exist there as a stale baseline binary when
/// the schema build gave the target a new fingerprint (clap: a narrower
/// package set unified features differently). Staging that file runs tests
/// against code without the schema runtime.
pub(crate) fn resolve_binary(
    target_dir: &Path,
    test: &crate::types::TestInfo,
    built: &[crate::cargo_cmd::BuiltExe],
) -> Result<PathBuf> {
    let want = WantBase::for_test(test);
    let worker = target_dir.parent().unwrap_or(target_dir);
    let same_package = |e: &crate::cargo_cmd::BuiltExe| match &e.manifest_dir {
        Some(dir) => {
            let rel = dir.strip_prefix(worker).unwrap_or(dir);
            crate::util::rel_slash(rel) == test.cwd_rel
        }
        None => true,
    };
    let exact_name = Path::new(&test.binary_rel)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    let mut fresh: Vec<&crate::cargo_cmd::BuiltExe> = built
        .iter()
        .filter(|e| {
            e.path
                .file_name()
                .is_some_and(|n| strip_fingerprint(&n.to_string_lossy()) == want.base)
        })
        .filter(|e| same_package(e))
        .collect();
    fresh.sort_by(|a, b| a.path.cmp(&b.path));
    if let Some(e) = fresh
        .iter()
        .find(|e| {
            exact_name.as_deref() == e.path.file_name().map(|n| n.to_string_lossy()).as_deref()
        })
        .or_else(|| fresh.iter().max_by_key(|e| mtime_or_epoch(&e.path)))
    {
        if fresh.len() > 1 {
            tracing::info!(
                "resolve {}: {} fresh fingerprints of {}; using {}",
                test.id,
                fresh.len(),
                want.base,
                e.path.display()
            );
        }
        return Ok(e.path.clone());
    }
    if !built.is_empty() {
        tracing::warn!(
            "resolve {}: no executable from this build matches base {} in {}; falling back to {}",
            test.id,
            want.base,
            if test.cwd_rel.is_empty() {
                "."
            } else {
                &test.cwd_rel
            },
            test.binary_rel
        );
    }
    let candidate = target_dir.join(&test.binary_rel);
    if candidate.is_file() {
        return Ok(candidate);
    }
    if let Some(key) = &exact_name {
        // cargo uses the profile dir name; tests build in `debug`.
        let p = target_dir.join("debug").join("deps").join(key);
        if p.is_file() {
            return Ok(p);
        }
    }
    anyhow::bail!(
        "could not locate test binary for {} (rel {}, base {})",
        test.id,
        test.binary_rel,
        want.base
    )
}

/// The target base name a staged binary must match, and whether it had to be
/// guessed from the package name because the index entry was incomplete.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WantBase {
    pub base: String,
    pub from_package: bool,
}

impl WantBase {
    pub(crate) fn for_test(test: &crate::types::TestInfo) -> Self {
        Self::new(&test.binary_rel, &test.binary_name, &test.package)
    }

    pub(crate) fn new(binary_rel: &str, binary_name: &str, package: &str) -> Self {
        if let Some(name) = Path::new(binary_rel).file_name() {
            let name = name.to_string_lossy();
            if !name.is_empty() {
                return Self {
                    base: strip_fingerprint(&name).to_string(),
                    from_package: false,
                };
            }
        }
        // Cargo writes artifact file names with `-` replaced by `_`, so the
        // fallbacks have to be normalized the same way to compare equal.
        if !binary_name.is_empty() {
            return Self {
                base: strip_fingerprint(binary_name).replace('-', "_"),
                from_package: false,
            };
        }
        Self {
            base: package.replace('-', "_"),
            from_package: true,
        }
    }
}

/// `brokk_bifrost_core-c4d0bb3ad97e1f96` -> `brokk_bifrost_core`. The only
/// `-` cargo leaves in an artifact file name is the fingerprint separator.
pub(crate) fn strip_fingerprint(name: &str) -> &str {
    match name.rsplit_once('-') {
        Some((base, hash))
            if hash.len() >= 8
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            base
        }
        _ => name,
    }
}

fn mtime_or_epoch(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_suffix_is_stripped_only_when_it_is_a_hash() {
        assert_eq!(
            strip_fingerprint("brokk_bifrost_core-c4d0bb3ad97e1f96"),
            "brokk_bifrost_core"
        );
        assert_eq!(strip_fingerprint("bifrost"), "bifrost");
        // Too short to be a fingerprint.
        assert_eq!(strip_fingerprint("mcp-fuzz01"), "mcp-fuzz01");
        // Not hex.
        assert_eq!(
            strip_fingerprint("integration-tests_zzzzzzzz"),
            "integration-tests_zzzzzzzz"
        );
        assert_eq!(
            strip_fingerprint("proptest-not_hex_here"),
            "proptest-not_hex_here"
        );
    }

    #[test]
    fn want_base_prefers_binary_rel_then_binary_name_then_package() {
        let from_rel = WantBase::new(
            "quicksilver/deps/brokk_bifrost_core-c4d0bb3ad97e1f96",
            "ignored",
            "ignored",
        );
        assert_eq!(from_rel.base, "brokk_bifrost_core");
        assert!(!from_rel.from_package);

        let from_name = WantBase::new("", "brokk-bifrost-core", "other-pkg");
        assert_eq!(from_name.base, "brokk_bifrost_core");
        assert!(!from_name.from_package);

        let from_pkg = WantBase::new("", "", "brokk-bifrost-core");
        assert_eq!(from_pkg.base, "brokk_bifrost_core");
        assert!(from_pkg.from_package, "package fallback is flagged");
    }

    use crate::cargo_cmd::BuiltExe;
    use crate::types::TestInfo;

    struct Worker {
        _tmp: tempfile::TempDir,
        root: std::path::PathBuf,
    }

    impl Worker {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().to_path_buf();
            fs::create_dir_all(root.join("target/quicksilver/deps")).unwrap();
            Self { _tmp: tmp, root }
        }
        fn target(&self) -> std::path::PathBuf {
            self.root.join("target")
        }
        /// Write a deps binary and report it as built by the package in `pkg_dir`.
        fn exe(&self, name: &str, pkg_dir: &str) -> BuiltExe {
            let path = self.target().join("quicksilver/deps").join(name);
            fs::write(&path, name).unwrap();
            BuiltExe {
                path,
                manifest_dir: Some(self.root.join(pkg_dir)),
            }
        }
    }

    fn test(binary_rel: &str, cwd_rel: &str) -> TestInfo {
        TestInfo {
            id: format!("{cwd_rel}::t"),
            name: "t".into(),
            package: cwd_rel.into(),
            binary_id: cwd_rel.into(),
            binary_name: String::new(),
            binary_rel: binary_rel.into(),
            cwd_rel: cwd_rel.into(),
            duration_secs: 0.1,
            cpu_secs: None,
        }
    }

    #[test]
    fn fresh_fingerprint_beats_a_stale_exact_path() {
        // clap: the baseline's `builder-225d...` is still in the cloned
        // target/, but the schema build produced `builder-9999...`.
        let w = Worker::new();
        let stale = w.exe("builder-225d7afc8b310acb", "clap");
        let fresh = w.exe("builder-9999999999999999", "clap");
        let got = resolve_binary(
            &w.target(),
            &test("quicksilver/deps/builder-225d7afc8b310acb", "clap"),
            std::slice::from_ref(&fresh),
        )
        .unwrap();
        assert_eq!(got, fresh.path);
        assert_ne!(got, stale.path);
    }

    #[test]
    fn same_target_name_in_two_packages_resolves_by_manifest_dir() {
        let w = Worker::new();
        let a = w.exe("testsuite-1111111111111111", "crate_a");
        let b = w.exe("testsuite-2222222222222222", "crate_b");
        let built = vec![a.clone(), b.clone()];
        let rel = "quicksilver/deps/testsuite-0000000000000000";
        assert_eq!(
            resolve_binary(&w.target(), &test(rel, "crate_a"), &built).unwrap(),
            a.path
        );
        assert_eq!(
            resolve_binary(&w.target(), &test(rel, "crate_b"), &built).unwrap(),
            b.path
        );
    }

    #[test]
    fn base_match_is_exact_and_never_a_substring() {
        // The old substring scan matched the fuzzer for needle "bifrost".
        let w = Worker::new();
        let fuzzer = w.exe("bifrost_mcp_property_fuzzer-abcdef1234567890", "");
        let harness = w.exe("bifrost-1111111111111111", "");
        let built = vec![fuzzer, harness.clone()];
        let rel = "quicksilver/deps/bifrost-0000000000000000";
        assert_eq!(
            resolve_binary(&w.target(), &test(rel, ""), &built).unwrap(),
            harness.path
        );
        let rel = "quicksilver/deps/mcp_property_fuzzer-0000000000000000";
        assert!(resolve_binary(&w.target(), &test(rel, ""), &built).is_err());
    }

    #[test]
    fn exact_fresh_name_wins_over_other_fresh_fingerprints() {
        let w = Worker::new();
        let one = w.exe("core-1111111111111111", "");
        let two = w.exe("core-2222222222222222", "");
        let built = vec![one.clone(), two];
        let rel = "quicksilver/deps/core-1111111111111111";
        assert_eq!(
            resolve_binary(&w.target(), &test(rel, ""), &built).unwrap(),
            one.path
        );
    }
}
