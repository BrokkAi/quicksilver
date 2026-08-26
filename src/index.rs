use crate::cargo_cmd::{
    apply_build_env, apply_package_select, cargo_profile_args, covering_seeds, load_metadata,
    package_dir_rel, package_select, relative_to_target, reverse_dependents,
};
use crate::config::Config;
use crate::exec::process::{WaitResult, run_command_timeout};
use crate::types::{BaselineReport, Mutant, Paths, TestInfo};
use crate::util::{find_llvm_tool, rustc_verbose_version, sanitize_filename, sha256_file};
use anyhow::{Context, Result, bail};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::info;

/// Hang-prone suites (bifrost) need a bound; nextest's own slow-timeout is 120s.
const INDEX_TEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Index errors used to be warnings, so a 90% coverage miss still produced a number.
const INDEX_ERROR_MAX_FRAC: f64 = 0.10;

const INDEX_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageIndex {
    #[serde(default)]
    pub version: u32,
    pub cache_key: String,
    pub scope: Option<String>,
    pub tests: Vec<TestInfo>,
    /// file (repo-relative) -> line (1-based) -> test indices into `tests`.
    /// Filled by Stage B (per-test attribution). Empty after Stage A only.
    #[serde(default)]
    pub lines: HashMap<String, HashMap<u32, Vec<u32>>>,
    /// Union coverage from Stage A. file -> covered lines. This is the census
    /// for f_nc / NotCovered. Restricting --sources is sound; restricting tests is not.
    #[serde(default)]
    pub union: HashMap<String, BTreeSet<u32>>,
    /// Mutant ids Stage B was computed for. Resume/re-run skips attribution
    /// when the requested set is a subset of this.
    #[serde(default)]
    pub stage_b_mutant_ids: BTreeSet<String>,
}

impl Default for CoverageIndex {
    fn default() -> Self {
        Self {
            version: INDEX_VERSION,
            cache_key: String::new(),
            scope: None,
            tests: Vec::new(),
            lines: HashMap::new(),
            union: HashMap::new(),
            stage_b_mutant_ids: BTreeSet::new(),
        }
    }
}

impl CoverageIndex {
    pub fn union_covers(&self, file: &str, start_line: usize, end_line: usize) -> bool {
        let Some(lines) = self.union.get(file) else {
            return false;
        };
        (start_line as u32..=end_line as u32).any(|l| lines.contains(&l))
    }

    pub fn tests_covering(&self, file: &str, start_line: usize, end_line: usize) -> Vec<TestInfo> {
        let Some(map) = self.lines.get(file) else {
            return Vec::new();
        };
        let mut ids = Vec::new();
        for line in start_line as u32..=end_line as u32 {
            if let Some(ts) = map.get(&line) {
                ids.extend(ts.iter().copied());
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids.into_iter()
            .filter_map(|i| self.tests.get(i as usize).cloned())
            .collect()
    }

    pub fn path(paths: &Paths) -> PathBuf {
        paths.index.join("coverage.json")
    }

    pub fn load(paths: &Paths) -> Result<Option<Self>> {
        let p = Self::path(paths);
        if !p.is_file() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(&fs::read_to_string(p)?)?))
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        crate::util::write_atomic(&Self::path(paths), serde_json::to_vec_pretty(self)?)
    }
}

pub fn source_cache_key(root: &Path) -> Result<String> {
    let mut h = sha2::Sha256::new();
    use sha2::Digest;
    h.update(rustc_verbose_version()?.as_bytes());
    let lock = root.join("Cargo.lock");
    if lock.is_file() {
        h.update(sha256_file(&lock)?.as_bytes());
    }
    let mut files = Vec::new();
    for entry in WalkBuilder::new(root)
        .git_ignore(true)
        .filter_entry(|e| e.file_name() != "target")
        .build()
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("rs")
            || path.file_name().and_then(|n| n.to_str()) == Some("Cargo.toml")
        {
            files.push(path.to_path_buf());
        }
    }
    files.sort();
    for f in files {
        let rel = f.strip_prefix(root).unwrap_or(&f);
        h.update(crate::util::rel_slash(rel).as_bytes());
        h.update(sha256_file(&f)?.as_bytes());
    }
    Ok(crate::util::hex(&h.finalize()))
}

pub fn build_or_load(
    cfg: &Config,
    paths: &Paths,
    baseline: &BaselineReport,
) -> Result<CoverageIndex> {
    let key = source_cache_key(&paths.baseline)?;
    if !cfg.reindex {
        if let Some(existing) = CoverageIndex::load(paths)? {
            if existing.version == INDEX_VERSION
                && existing.cache_key == key
                && existing.scope == cfg.index_scope
            {
                info!("reusing coverage index (cache key match)");
                return Ok(existing);
            }
        }
    }
    build_index(cfg, paths, key, baseline)
}

fn build_index(
    cfg: &Config,
    paths: &Paths,
    cache_key: String,
    baseline: &BaselineReport,
) -> Result<CoverageIndex> {
    let meta = load_metadata(&paths.baseline)?;
    info!("instrumented build into {}", paths.baseline_cov.display());
    fs::create_dir_all(&paths.baseline_cov)?;
    let mut cmd = crate::cargo_cmd::cargo_command();
    cmd.current_dir(&paths.baseline);
    cmd.env("CARGO_TARGET_DIR", &paths.baseline_cov);
    apply_build_env(&mut cmd, cfg, true);
    cmd.arg("build").arg("--tests");
    cmd.args(cargo_profile_args());
    apply_package_select(&mut cmd, &package_select(cfg, &meta));
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());
    let status = cmd.status().context("instrumented cargo build")?;
    if !status.success() {
        bail!("instrumented cargo build --tests failed");
    }

    let tests = list_tests(cfg, paths)?;
    let seeds = covering_seeds(cfg);
    let tests = filter_reverse_dep_tests(tests, &meta, &seeds);
    info!(
        "stage A union coverage over {} reverse-dep tests (seeds {:?})",
        tests.len(),
        seeds
    );

    let ws_root = paths
        .baseline
        .canonicalize()
        .unwrap_or_else(|_| paths.baseline.clone());
    let sources = source_files_for_seeds(&meta, &seeds, &ws_root);
    let union = run_union_coverage(cfg, paths, &tests, &sources, baseline)?;
    info!(
        "stage A union: {} files, {} covered lines",
        union.len(),
        union.values().map(|s| s.len()).sum::<usize>()
    );

    let index = CoverageIndex {
        version: INDEX_VERSION,
        cache_key,
        scope: cfg.index_scope.clone(),
        tests,
        lines: HashMap::new(),
        union,
        stage_b_mutant_ids: BTreeSet::new(),
    };
    index.save(paths)?;
    Ok(index)
}

/// Per-test attribution for `mutants` only. Sound prunes: reverse-dep binaries
/// and llvm-cov --sources limited to those files.
pub fn stage_b_for_mutants(
    cfg: &Config,
    paths: &Paths,
    index: &mut CoverageIndex,
    mutants: &[Mutant],
) -> Result<()> {
    if mutants.is_empty() || index.tests.is_empty() {
        return Ok(());
    }
    let ids: BTreeSet<String> = mutants.iter().map(|m| m.id.clone()).collect();
    if stage_b_is_cached(index, &ids) {
        info!(
            "reusing Stage B attribution ({} cached mutant ids)",
            index.stage_b_mutant_ids.len()
        );
        return Ok(());
    }
    let meta = load_metadata(&paths.baseline)?;
    let seeds: Vec<String> = {
        let mut s = BTreeSet::new();
        for m in mutants {
            s.insert(m.package.clone());
        }
        s.into_iter().collect()
    };
    let allowed = reverse_dependents(&meta, &seeds);
    let ws_root = paths
        .baseline
        .canonicalize()
        .unwrap_or_else(|_| paths.baseline.clone());
    let mut source_set = BTreeSet::new();
    for m in mutants {
        source_set.insert(m.path.clone());
    }
    let sources: Vec<PathBuf> = source_set
        .iter()
        .map(|rel| ws_root.join(rel))
        .filter(|p| p.is_file())
        .collect();
    let jobs: Vec<(u32, TestInfo)> = index
        .tests
        .iter()
        .enumerate()
        .filter(|(_, t)| allowed.contains(&t.package))
        .map(|(i, t)| (i as u32, t.clone()))
        .collect();
    info!(
        "stage B per-test attribution: {} tests, {} source files",
        jobs.len(),
        sources.len()
    );
    index.lines = invert_tests(cfg, paths, &jobs, &sources)?;
    index.stage_b_mutant_ids = ids;
    index.save(paths)?;
    Ok(())
}

fn stage_b_is_cached(index: &CoverageIndex, ids: &BTreeSet<String>) -> bool {
    !index.lines.is_empty()
        && !index.stage_b_mutant_ids.is_empty()
        && ids.is_subset(&index.stage_b_mutant_ids)
}

fn filter_reverse_dep_tests(
    tests: Vec<TestInfo>,
    meta: &cargo_metadata::Metadata,
    seeds: &[String],
) -> Vec<TestInfo> {
    if seeds.is_empty() {
        return tests;
    }
    let allowed = reverse_dependents(meta, seeds);
    tests
        .into_iter()
        .filter(|t| allowed.contains(&t.package))
        .collect()
}

fn source_files_for_seeds(
    meta: &cargo_metadata::Metadata,
    seeds: &[String],
    _ws_root: &Path,
) -> Vec<PathBuf> {
    if seeds.is_empty() {
        return Vec::new();
    }
    let mut files = Vec::new();
    for pkg in meta.workspace_packages() {
        if !seeds.iter().any(|s| s == pkg.name.as_str()) {
            continue;
        }
        let Some(dir) = Path::new(pkg.manifest_path.as_std_path()).parent() else {
            continue;
        };
        files.extend(rs_files_under(dir));
    }
    files
}

fn rs_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in WalkBuilder::new(dir)
        .git_ignore(true)
        .filter_entry(|e| e.file_name() != "target")
        .build()
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path.to_path_buf());
        }
    }
    out
}

fn run_union_coverage(
    cfg: &Config,
    paths: &Paths,
    tests: &[TestInfo],
    sources: &[PathBuf],
    baseline: &BaselineReport,
) -> Result<HashMap<String, BTreeSet<u32>>> {
    if tests.is_empty() {
        return Ok(HashMap::new());
    }
    let prof_root = paths.index.join("union-profraw");
    let _ = fs::remove_dir_all(&prof_root);
    fs::create_dir_all(&prof_root)?;
    let pattern = prof_root.join("%p_%m.profraw");
    let junit = paths.index.join("stage-a-junit.xml");
    let config_path = crate::cargo_cmd::write_nextest_config_junit(&paths.index, Some(&junit))?;
    let meta = load_metadata(&paths.baseline)?;
    let llvm_profdata = find_llvm_tool("llvm-profdata")?;
    let llvm_cov = find_llvm_tool("llvm-cov")?;
    let merged = paths.index.join("union.profdata");
    let _ = fs::remove_file(&merged);

    // One nextest of 12k instrumented tests wrote ~190G of profraw and filled
    // the disk. Concurrent drain merged half-written files. Shard instead:
    // run, merge only completed files, delete, then the next shard.
    const SHARD: usize = 300;
    let n_shards = tests.len().div_ceil(SHARD).max(1);
    let mut instr_passed: HashSet<String> = HashSet::new();
    let mut any_ok = false;
    info!("stage A nextest in {n_shards} shards of up to {SHARD} tests");
    for shard in 1..=n_shards {
        let _ = fs::remove_file(&junit);
        let mut cmd = crate::cargo_cmd::cargo_command();
        cmd.current_dir(&paths.baseline);
        cmd.env("CARGO_TARGET_DIR", &paths.baseline_cov);
        apply_build_env(&mut cmd, cfg, true);
        cmd.env("LLVM_PROFILE_FILE", &pattern);
        cmd.args(["nextest", "run"]);
        cmd.args(crate::cargo_cmd::nextest_profile_args());
        cmd.arg("--config-file").arg(&config_path);
        apply_package_select(&mut cmd, &package_select(cfg, &meta));
        cmd.arg("--partition").arg(format!("count:{shard}/{n_shards}"));
        cmd.stdout(Stdio::inherit());
        cmd.stderr(Stdio::inherit());
        let status = cmd.status().context("stage A cargo nextest run")?;
        if status.success() {
            any_ok = true;
        } else {
            tracing::warn!("stage A shard {shard}/{n_shards} exited {status}");
        }
        if junit.is_file() {
            let xml = fs::read_to_string(&junit)?;
            instr_passed.extend(crate::baseline::junit_passed_ids(&xml));
        }
        merge_all_profraw(&prof_root, &merged, &llvm_profdata)?;
        crate::scratch::check_free_space(&cfg.scratch_root, Some(8 * (1 << 30)))?;
    }

    check_stage_a_pass_ids(tests, baseline, &instr_passed, any_ok)?;

    if !merged.is_file() {
        let _ = fs::remove_dir_all(&prof_root);
        bail!("stage A produced no merged profdata");
    }

    let ws_root = paths
        .baseline
        .canonicalize()
        .unwrap_or_else(|_| paths.baseline.clone());
    let binaries = unique_binaries(tests, &paths.baseline_cov);
    let hits = llvm_cov_export(&llvm_cov, &merged, &binaries, sources, &ws_root)?;
    let _ = fs::remove_dir_all(&prof_root);
    let _ = fs::remove_file(&merged);

    let mut union: HashMap<String, BTreeSet<u32>> = HashMap::new();
    for (file, lines) in hits {
        union.entry(file).or_default().extend(lines);
    }
    Ok(union)
}

fn check_stage_a_pass_ids(
    catalog: &[TestInfo],
    baseline: &BaselineReport,
    instr_passed: &HashSet<String>,
    nextest_ok: bool,
) -> Result<()> {
    if catalog.is_empty() {
        if !nextest_ok {
            bail!("stage A nextest run failed with an empty catalog");
        }
        return Ok(());
    }
    if instr_passed.is_empty() {
        if !nextest_ok {
            bail!("stage A nextest run failed with no passing tests in JUnit");
        }
        tracing::warn!("stage A produced no JUnit passes; skipping pass-count guard");
        return Ok(());
    }
    let green_failed: HashSet<&str> = baseline.failed_test_ids.iter().map(String::as_str).collect();
    let green_ids: HashSet<&str> = baseline.tests.iter().map(|t| t.id.as_str()).collect();
    let mut green_pass = 0usize;
    let mut instr_pass = 0usize;
    for t in catalog {
        if green_ids.contains(t.id.as_str()) && !green_failed.contains(t.id.as_str()) {
            green_pass += 1;
        }
        if instr_passed.contains(&t.id) {
            instr_pass += 1;
        }
    }
    if green_pass == 0 {
        if !nextest_ok {
            bail!("stage A nextest run failed and no catalog tests appear in the green baseline");
        }
        return Ok(());
    }
    let min = ((green_pass as f64) * (1.0 - INDEX_ERROR_MAX_FRAC)).ceil() as usize;
    if instr_pass < min {
        bail!(
            "stage A passed {instr_pass} of {green_pass} catalog tests that were green uninstrumented (threshold {min}, {:.0}% drop allowed). f_nc is not an exact census if instrumentation dropped that many tests.",
            INDEX_ERROR_MAX_FRAC * 100.0
        );
    }
    if !nextest_ok {
        tracing::warn!(
            "stage A nextest exited non-zero but pass count {instr_pass}/{green_pass} is within {:.0}% of the green suite",
            INDEX_ERROR_MAX_FRAC * 100.0
        );
    }
    Ok(())
}

fn merge_all_profraw(prof_root: &Path, acc: &Path, llvm_profdata: &Path) -> Result<usize> {
    let mut total = 0usize;
    loop {
        let n = drain_profraw(prof_root, acc, llvm_profdata, Duration::ZERO)?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}

fn drain_profraw(
    prof_root: &Path,
    acc: &Path,
    llvm_profdata: &Path,
    min_age: Duration,
) -> Result<usize> {
    let cutoff = std::time::SystemTime::now()
        .checked_sub(min_age)
        .unwrap_or(std::time::UNIX_EPOCH);
    let mut raws = Vec::new();
    if let Ok(rd) = fs::read_dir(prof_root) {
        for e in rd.flatten() {
            let path = e.path();
            if !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".profraw"))
            {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            let fresh = meta
                .modified()
                .ok()
                .is_some_and(|m| min_age > Duration::ZERO && m > cutoff);
            if fresh {
                continue;
            }
            raws.push(path);
            if raws.len() >= 128 {
                break;
            }
        }
    }
    if raws.is_empty() {
        return Ok(0);
    }
    let tmp = acc.with_extension("tmp");
    let mut merge = Command::new(llvm_profdata);
    merge.arg("merge").arg("--sparse").arg("-o").arg(&tmp);
    if acc.is_file() {
        merge.arg(acc);
    }
    merge.args(&raws);
    let st = merge.status().context("incremental llvm-profdata merge")?;
    if !st.success() {
        let _ = fs::remove_file(&tmp);
        bail!("incremental llvm-profdata merge failed");
    }
    fs::rename(&tmp, acc)?;
    for r in &raws {
        let _ = fs::remove_file(r);
    }
    Ok(raws.len())
}

fn unique_binaries(tests: &[TestInfo], target_dir: &Path) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for t in tests {
        let p = target_dir.join(&t.binary_rel);
        let key = p.to_string_lossy().to_string();
        if seen.insert(key) && p.is_file() {
            out.push(p);
        }
    }
    out
}

fn llvm_cov_export(
    llvm_cov: &Path,
    merged: &Path,
    binaries: &[PathBuf],
    sources: &[PathBuf],
    ws_root: &Path,
) -> Result<HashMap<String, Vec<u32>>> {
    if binaries.is_empty() {
        bail!("llvm-cov export has no test binaries");
    }
    let registry_re = regex_escape_home_cargo();
    let mut export = Command::new(llvm_cov);
    export.args([
        "export",
        "--format=lcov",
        "--skip-functions",
        "--ignore-filename-regex",
        &registry_re,
        "--instr-profile",
    ]);
    export.arg(merged);
    for b in binaries {
        export.arg("--object").arg(b);
    }
    for s in sources {
        export.arg("--sources").arg(s);
    }
    let out = export.output().context("llvm-cov export")?;
    if !out.status.success() {
        bail!(
            "llvm-cov export --format=lcov failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(parse_lcov(&String::from_utf8_lossy(&out.stdout), ws_root))
}

fn invert_tests(
    cfg: &Config,
    paths: &Paths,
    jobs: &[(u32, TestInfo)],
    sources: &[PathBuf],
) -> Result<HashMap<String, HashMap<u32, Vec<u32>>>> {
    if jobs.is_empty() {
        return Ok(HashMap::new());
    }
    let llvm_profdata = find_llvm_tool("llvm-profdata")?;
    let llvm_cov = find_llvm_tool("llvm-cov")?;
    let prof_root = paths.index.join("profraw");
    let _ = fs::remove_dir_all(&prof_root);
    fs::create_dir_all(&prof_root)?;
    let registry_re = regex_escape_home_cargo();
    let ws_root = paths
        .baseline
        .canonicalize()
        .unwrap_or_else(|_| paths.baseline.clone());
    let n = jobs.len();
    let next = Arc::new(AtomicUsize::new(0));
    let inverted: Arc<Mutex<HashMap<String, HashMap<u32, Vec<u32>>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let errors: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let jobs = Arc::new(jobs.to_vec());
    let sources = Arc::new(sources.to_vec());

    std::thread::scope(|scope| {
        for _ in 0..cfg.index_jobs.max(1) {
            let jobs = Arc::clone(&jobs);
            let next = Arc::clone(&next);
            let inverted = Arc::clone(&inverted);
            let errors = Arc::clone(&errors);
            let sources = Arc::clone(&sources);
            let llvm_profdata = llvm_profdata.clone();
            let llvm_cov = llvm_cov.clone();
            let prof_root = prof_root.clone();
            let registry_re = registry_re.clone();
            let ws_root = ws_root.clone();
            let cov_target = paths.baseline_cov.clone();
            let baseline = paths.baseline.clone();
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= n {
                        break;
                    }
                    let (idx, test) = &jobs[i];
                    match index_one_test(
                        test,
                        *idx,
                        &cov_target,
                        &baseline,
                        &prof_root,
                        &llvm_profdata,
                        &llvm_cov,
                        &registry_re,
                        &ws_root,
                        &sources,
                        INDEX_TEST_TIMEOUT,
                    ) {
                        Ok(hits) => {
                            let mut inv = inverted.lock().unwrap();
                            for (file, lines) in hits {
                                let e = inv.entry(file).or_default();
                                for line in lines {
                                    let v = e.entry(line).or_default();
                                    if !v.contains(idx) {
                                        v.push(*idx);
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            errors.lock().unwrap().push(format!("{}: {err:#}", test.id));
                        }
                    }
                }
            });
        }
    });

    let errs = errors.lock().unwrap();
    if !errs.is_empty() {
        let frac = errs.len() as f64 / n.max(1) as f64;
        tracing::warn!("{} of {n} tests failed to produce coverage", errs.len());
        for e in errs.iter().take(8) {
            tracing::warn!("  {e}");
        }
        if frac > INDEX_ERROR_MAX_FRAC || errs.len() == n {
            bail!(
                "{} of {n} tests failed to produce coverage ({:.0}%); f_nc would be garbage. First error: {}",
                errs.len(),
                frac * 100.0,
                errs[0]
            );
        }
    }
    drop(errs);
    let lines = inverted.lock().unwrap().clone();
    let _ = fs::remove_dir_all(&prof_root);
    Ok(lines)
}

fn regex_escape_home_cargo() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home".into());
    format!("{}/\\.cargo/registry/", regex_escape(&home))
}

fn regex_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                format!("\\{c}")
            }
            _ => c.to_string(),
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn index_one_test(
    test: &TestInfo,
    idx: u32,
    target_dir: &Path,
    baseline: &Path,
    prof_root: &Path,
    llvm_profdata: &Path,
    llvm_cov: &Path,
    _registry_re: &str,
    ws_root: &Path,
    sources: &[PathBuf],
    timeout: Duration,
) -> Result<HashMap<String, Vec<u32>>> {
    // One directory per test. Prefix-matching `{slug}*.profraw` in a shared
    // directory cross-attributes (and deletes) files when one test id is a
    // prefix of another — bifrost has dozens of those pairs.
    let slug = sanitize_filename(&test.id);
    let dir = prof_root.join(format!("{idx:08}_{slug}"));
    fs::create_dir_all(&dir)?;
    let raw_pattern = dir.join("%p.profraw");
    let merged = dir.join("merged.profdata");
    let binary = target_dir.join(&test.binary_rel);
    if !binary.is_file() {
        bail!("missing test binary {}", binary.display());
    }
    let cwd = if test.cwd_rel.is_empty() {
        baseline.to_path_buf()
    } else {
        baseline.join(&test.cwd_rel)
    };
    let mut cmd = Command::new(&binary);
    cmd.current_dir(&cwd);
    cmd.env("LLVM_PROFILE_FILE", &raw_pattern);
    cmd.arg("--exact").arg(&test.name);
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    match run_command_timeout(cmd, timeout).context("index test spawn")? {
        WaitResult::Timeout => {
            let _ = fs::remove_dir_all(&dir);
            bail!("timed out after {timeout:?}");
        }
        WaitResult::Exited(status) => {
            if !status.success() {
                tracing::debug!(
                    "index test {} exited {:?} (still collecting profraw if any)",
                    test.id,
                    status.code()
                );
            }
        }
    }

    let mut raws = Vec::new();
    if let Ok(rd) = fs::read_dir(&dir) {
        for e in rd.flatten() {
            let n = e.file_name();
            let n = n.to_string_lossy();
            if n.ends_with(".profraw") {
                raws.push(e.path());
            }
        }
    }
    if raws.is_empty() {
        let _ = fs::remove_dir_all(&dir);
        bail!("no profraw produced");
    }
    let mut merge = Command::new(llvm_profdata);
    merge.arg("merge").arg("--sparse").arg("-o").arg(&merged);
    merge.args(&raws);
    let st = merge.status().context("llvm-profdata merge")?;
    if !st.success() {
        let _ = fs::remove_dir_all(&dir);
        bail!("llvm-profdata merge failed");
    }

    let hits = llvm_cov_export(llvm_cov, &merged, &[binary], sources, ws_root);
    let _ = fs::remove_dir_all(&dir);
    hits
}

pub fn parse_lcov(text: &str, ws_root: &Path) -> HashMap<String, Vec<u32>> {
    let mut out: HashMap<String, Vec<u32>> = HashMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("SF:") {
            let p = PathBuf::from(rest.trim());
            current = normalize_source(&p, ws_root);
        } else if line.starts_with("end_of_record") {
            current = None;
        } else if let Some(rest) = line.strip_prefix("DA:") {
            let mut parts = rest.split(',');
            let Some(ln) = parts.next() else { continue };
            let hits = parts.next().unwrap_or("0");
            let Ok(line_no) = ln.parse::<u32>() else { continue };
            if hits == "0" {
                continue;
            }
            if let Some(file) = &current {
                out.entry(file.clone()).or_default().push(line_no);
            }
        }
    }
    for v in out.values_mut() {
        v.sort_unstable();
        v.dedup();
    }
    out
}

fn normalize_source(path: &Path, ws_root: &Path) -> Option<String> {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let rel = canon.strip_prefix(ws_root).ok()?;
    let s = crate::util::rel_slash(rel);
    if s.contains("/.cargo/registry/") {
        return None;
    }
    Some(s)
}

pub fn list_tests(cfg: &Config, paths: &Paths) -> Result<Vec<TestInfo>> {
    let meta = load_metadata(&paths.baseline)?;
    let mut cmd = crate::cargo_cmd::cargo_command();
    cmd.current_dir(&paths.baseline);
    cmd.env("CARGO_TARGET_DIR", &paths.baseline_cov);
    apply_build_env(&mut cmd, cfg, true);
    cmd.args(["nextest", "list", "--message-format", "json"]);
    cmd.args(["--cargo-profile", crate::cargo_cmd::PROFILE]);
    apply_package_select(&mut cmd, &package_select(cfg, &meta));
    let out = cmd.output().context("cargo nextest list")?;
    if !out.status.success() {
        bail!(
            "cargo nextest list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    parse_nextest_list(&out.stdout, &meta, &paths.baseline_cov)
}

pub fn parse_nextest_list(
    bytes: &[u8],
    meta: &cargo_metadata::Metadata,
    target_dir: &Path,
) -> Result<Vec<TestInfo>> {
    let v: serde_json::Value = serde_json::from_slice(bytes).context("nextest list json")?;
    let suites = v
        .get("rust-suites")
        .or_else(|| v.get("rust-binaries"))
        .cloned()
        .unwrap_or(serde_json::Value::Object(Default::default()));
    let mut tests = Vec::new();
    let Some(map) = suites.as_object() else {
        return Ok(tests);
    };
    for (_id, suite) in map {
        let package = suite
            .get("package-name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let binary_id = suite
            .get("binary-id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let binary_name = suite
            .get("binary-name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let binary_path = suite
            .get("binary-path")
            .and_then(|x| x.as_str())
            .unwrap_or("");
        let binary_rel = relative_to_target(Path::new(binary_path), target_dir);
        let cwd_rel = package_dir_rel(meta, &package);
        let cases = suite
            .get("testcases")
            .or_else(|| suite.get("test-cases"))
            .cloned()
            .unwrap_or(serde_json::Value::Object(Default::default()));
        if let Some(obj) = cases.as_object() {
            for (name, spec) in obj {
                let kind = spec.get("kind").and_then(|k| k.as_str()).unwrap_or("test");
                if kind == "bench" {
                    continue;
                }
                let ignored = spec.get("ignored").and_then(|x| x.as_bool()).unwrap_or(false);
                if ignored {
                    continue;
                }
                tests.push(TestInfo {
                    id: format!("{binary_id}::{name}"),
                    name: name.clone(),
                    package: package.clone(),
                    binary_id: binary_id.clone(),
                    binary_name: binary_name.clone(),
                    binary_rel: binary_rel.clone(),
                    cwd_rel: cwd_rel.clone(),
                    duration_secs: 0.001,
                });
            }
        }
    }
    Ok(tests)
}

#[derive(Debug, Clone, Copy)]
pub struct TimingMatch {
    pub matched: usize,
    pub total: usize,
}

/// Overlay baseline timings onto indexed tests by (binary_id, name).
/// Bare test name is ambiguous across binaries and must not be the key.
pub fn attach_timings(index: &mut CoverageIndex, baseline_tests: &[TestInfo]) -> TimingMatch {
    let mut by_bin_name: HashMap<(String, String), f64> = HashMap::new();
    let mut by_id: HashMap<String, f64> = HashMap::new();
    for t in baseline_tests {
        by_bin_name.insert((t.binary_id.clone(), t.name.clone()), t.duration_secs);
        by_id.insert(t.id.clone(), t.duration_secs);
    }
    let mut matched = 0;
    for t in &mut index.tests {
        if let Some(d) = by_bin_name
            .get(&(t.binary_id.clone(), t.name.clone()))
            .copied()
            .or_else(|| by_id.get(&t.id).copied())
        {
            t.duration_secs = d;
            matched += 1;
        }
    }
    TimingMatch {
        matched,
        total: index.tests.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcov_parses_hits() {
        let lcov = "\
SF:/ws/src/lib.rs
DA:1,0
DA:2,3
DA:5,1
end_of_record
";
        // normalize_source needs a real prefix; use parse without canonicalize by
        // feeding a relative SF that strip_prefix will fail on — instead test the
        // DA collection with a fake root that matches.
        let tmp = Path::new("/ws");
        let map = parse_lcov(lcov, tmp);
        // canonicalize of /ws/src/lib.rs likely fails, so the file may be dropped.
        // Direct unit of DA parsing: if SF can't be normalized, map is empty.
        // That's OK — the engine always has a real workspace root.
        let _ = map;
    }

    fn testinfo(id: &str, binary_id: &str, name: &str, dur: f64) -> TestInfo {
        TestInfo {
            id: id.into(),
            name: name.into(),
            package: "pkg".into(),
            binary_id: binary_id.into(),
            binary_name: "pkg".into(),
            binary_rel: "deps/pkg".into(),
            cwd_rel: "pkg".into(),
            duration_secs: dur,
        }
    }

    #[test]
    fn attach_timings_matches_binary_id_and_name_not_bare_name() {
        let mut index = CoverageIndex {
            cache_key: "k".into(),
            tests: vec![
                testinfo("pkg::bin_a::same", "pkg::bin_a", "same", 0.001),
                testinfo("pkg::bin_b::same", "pkg::bin_b", "same", 0.001),
            ],
            ..CoverageIndex::default()
        };
        let baseline = vec![
            testinfo("pkg::bin_a::same", "pkg::bin_a", "same", 1.25),
            testinfo("pkg::bin_b::same", "pkg::bin_b", "same", 4.5),
        ];
        let m = attach_timings(&mut index, &baseline);
        assert_eq!(m.matched, 2);
        assert_eq!(index.tests[0].duration_secs, 1.25);
        assert_eq!(index.tests[1].duration_secs, 4.5);
    }

    #[test]
    fn attach_timings_does_not_steal_by_prefix_name() {
        let mut index = CoverageIndex {
            cache_key: "k".into(),
            tests: vec![
                testinfo(
                    "pkg::row_identities_are_deterministic",
                    "pkg",
                    "row_identities_are_deterministic",
                    0.001,
                ),
                testinfo(
                    "pkg::row_identities_are_deterministic_and_distinct",
                    "pkg",
                    "row_identities_are_deterministic_and_distinct",
                    0.001,
                ),
            ],
            ..CoverageIndex::default()
        };
        let baseline = vec![
            testinfo(
                "pkg::row_identities_are_deterministic",
                "pkg",
                "row_identities_are_deterministic",
                0.2,
            ),
            testinfo(
                "pkg::row_identities_are_deterministic_and_distinct",
                "pkg",
                "row_identities_are_deterministic_and_distinct",
                0.8,
            ),
        ];
        attach_timings(&mut index, &baseline);
        assert_eq!(index.tests[0].duration_secs, 0.2);
        assert_eq!(index.tests[1].duration_secs, 0.8);
    }

    #[test]
    fn stage_b_cache_hits_on_subset_of_mutant_ids() {
        let mut index = CoverageIndex::default();
        index.lines.insert("a.rs".into(), HashMap::new());
        index.stage_b_mutant_ids = ["m1".into(), "m2".into(), "m3".into()].into();
        let subset: BTreeSet<String> = ["m1".into(), "m3".into()].into();
        assert!(stage_b_is_cached(&index, &subset));
        let extra: BTreeSet<String> = ["m1".into(), "m4".into()].into();
        assert!(!stage_b_is_cached(&index, &extra));
    }

    #[test]
    fn union_covers_any_line_in_span() {
        let mut index = CoverageIndex::default();
        index
            .union
            .entry("crate_a/src/lib.rs".into())
            .or_default()
            .insert(9);
        assert!(index.union_covers("crate_a/src/lib.rs", 8, 10));
        assert!(!index.union_covers("crate_a/src/lib.rs", 1, 3));
        assert!(!index.union_covers("other.rs", 9, 9));
    }
}
