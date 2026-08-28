use anyhow::Result;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub(crate) fn resolve_binary(
    target_dir: &Path,
    test: &crate::types::TestInfo,
    built: &HashMap<String, PathBuf>,
) -> Result<PathBuf> {
    let candidate = target_dir.join(&test.binary_rel);
    if candidate.is_file() {
        tracing::info!("resolve {}: exact rel {}", test.id, candidate.display());
        return Ok(candidate);
    }
    if let Some(name) = Path::new(&test.binary_rel).file_name() {
        let key = name.to_string_lossy().into_owned();
        if let Some(p) = built.get(&key) {
            tracing::info!("resolve {}: name key {}", test.id, p.display());
            return Ok(p.clone());
        }
        // cargo uses the profile dir name
        for dir in ["quicksilver", "debug", "dev"] {
            let p = target_dir.join(dir).join("deps").join(&key);
            if p.is_file() {
                tracing::info!("resolve {}: {dir} deps probe {}", test.id, p.display());
                return Ok(p);
            }
        }
    }
    // The index records `binary_rel` from the instrumented Stage-A build, so
    // its fingerprint never exists in a plain worker build: the strategies
    // above miss on every real run. Match on the target base name instead.
    // Substring matching used to live here and it staged the wrong target
    // (a plain bin whose name contains the wanted one), which then either
    // exits nonzero on `--exact` or passes vacuously.
    let want = WantBase::for_test(test);
    if want.from_package {
        tracing::warn!(
            "resolve {}: no binary_rel or binary_name in the index; falling back to package base {}",
            test.id,
            want.base
        );
    }
    let mut entries: Vec<(String, SystemTime)> = built
        .iter()
        .map(|(name, path)| (name.clone(), mtime_or_epoch(path)))
        .collect();
    // `built` is a HashMap; sort so equal mtimes resolve the same way twice.
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    match pick_by_base(&want.base, &entries) {
        Some(m) => {
            let path = built
                .get(&entries[m.index].0)
                .expect("entry key came from built")
                .clone();
            if m.total > 1 {
                tracing::info!(
                    "resolve {}: {} fingerprints matched base {}; using newest {}",
                    test.id,
                    m.total,
                    want.base,
                    path.display()
                );
            } else {
                tracing::info!(
                    "resolve {}: base {} -> {}",
                    test.id,
                    want.base,
                    path.display()
                );
            }
            Ok(path)
        }
        None => anyhow::bail!(
            "could not locate test binary for {} (rel {}, base {})",
            test.id,
            test.binary_rel,
            want.base
        ),
    }
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

/// Which entry to stage, and how many were in the running.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BaseMatch {
    pub index: usize,
    pub total: usize,
}

/// Entries whose hash-stripped file name equals `want_base` exactly. Several
/// can match: they are feature fingerprints of one target, and the freshest
/// belongs to the build that just finished. Ties keep the first entry, so a
/// sorted input resolves deterministically.
pub(crate) fn pick_by_base(want_base: &str, entries: &[(String, SystemTime)]) -> Option<BaseMatch> {
    let mut best: Option<usize> = None;
    let mut total = 0usize;
    for (i, (name, mtime)) in entries.iter().enumerate() {
        if strip_fingerprint(name) != want_base {
            continue;
        }
        total += 1;
        if best.is_none_or(|b| *mtime > entries[b].1) {
            best = Some(i);
        }
    }
    best.map(|index| BaseMatch { index, total })
}

fn mtime_or_epoch(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn entries(names: &[(&str, u64)]) -> Vec<(String, SystemTime)> {
        names
            .iter()
            .map(|(n, t)| ((*n).to_string(), at(*t)))
            .collect()
    }

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

    #[test]
    fn base_match_is_exact_and_never_a_substring() {
        let built = entries(&[
            ("bifrost_mcp_property_fuzzer-abcdef1234567890", 100),
            ("bifrost-1111111111111111", 100),
        ]);
        // The old substring scan matched the fuzzer for needle "bifrost".
        let m = pick_by_base("bifrost", &built).expect("bifrost harness");
        assert_eq!(built[m.index].0, "bifrost-1111111111111111");
        assert_eq!(m.total, 1);
        // And for needle "mcp_property_fuzzer" too; now neither matches.
        assert_eq!(pick_by_base("mcp_property_fuzzer", &built), None);
    }

    #[test]
    fn multiple_fingerprints_of_one_target_resolve_to_the_newest() {
        let built = entries(&[
            ("brokk_bifrost_core-1111111111111111", 100),
            ("brokk_bifrost_core-2222222222222222", 500),
            ("brokk_bifrost_core-3333333333333333", 300),
            ("unrelated-4444444444444444", 900),
        ]);
        let m = pick_by_base("brokk_bifrost_core", &built).expect("some fingerprint");
        assert_eq!(m.total, 3, "all three fingerprints are candidates");
        assert_eq!(built[m.index].0, "brokk_bifrost_core-2222222222222222");
    }

    #[test]
    fn equal_mtimes_keep_the_first_entry() {
        let built = entries(&[
            ("core-1111111111111111", 100),
            ("core-2222222222222222", 100),
        ]);
        let m = pick_by_base("core", &built).expect("tie");
        assert_eq!(built[m.index].0, "core-1111111111111111");
    }

    #[test]
    fn no_candidate_is_none_so_the_caller_bails() {
        let built = entries(&[("some_other_crate-1111111111111111", 100)]);
        assert_eq!(pick_by_base("brokk_bifrost_core", &built), None);
        assert_eq!(pick_by_base("", &built), None);
    }
}
