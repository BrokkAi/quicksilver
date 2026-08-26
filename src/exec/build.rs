use crate::cargo_cmd::{build_tests, parse_executables, relative_to_target};
use crate::config::Config;
use crate::discover::{apply_mutant, restore_file};
use crate::exec::test::{BinariesManifest, ManifestTest};
use crate::scratch::CloneBackend;
use crate::types::{LocalityBatch, Outcome, PlannedMutant};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub struct BuiltMutant {
    pub planned: PlannedMutant,
    pub run_dir: PathBuf,
}

pub fn build_one(
    cfg: &Config,
    backend: &dyn CloneBackend,
    worker_root: &Path,
    target_dir: &Path,
    runs_root: &Path,
    planned: &PlannedMutant,
    build_counter: &AtomicU64,
) -> Result<Result<BuiltMutant, Outcome>> {
    crate::scratch::check_free_space(runs_root, cfg.max_scratch)?;
    let original = apply_mutant(worker_root, &planned.mutant)?;
    build_counter.fetch_add(1, Ordering::SeqCst);
    let mut packages: Vec<String> = Vec::new();
    packages.push(planned.mutant.package.clone());
    for t in &planned.tests {
        if !packages.iter().any(|p| p == &t.package) {
            packages.push(t.package.clone());
        }
    }
    let built = build_tests(worker_root, target_dir, cfg, &packages, false, true);
    let restore = restore_file(worker_root, &planned.mutant, &original);
    let result = match built {
        Ok(result) => result,
        Err(err) => {
            let _ = restore_file(worker_root, &planned.mutant, &original);
            return Ok(Err(Outcome::unviable(
                planned.mutant.clone(),
                err.to_string(),
            )));
        }
    };
    let stdout = result.stdout;
    let executables = result.executables;
    if let Err(err) = restore {
        anyhow::bail!("restore failed after build: {err:#}");
    }
    let run_dir = runs_root.join(&planned.mutant.id);
            if run_dir.exists() {
                let _ = fs::remove_dir_all(&run_dir);
            }
            fs::create_dir_all(&run_dir)?;
            let mut name_to_exe: HashMap<String, PathBuf> = HashMap::new();
            for exe in parse_executables(&stdout, target_dir)
                .into_iter()
                .chain(executables)
            {
                if let Some(name) = exe.file_name() {
                    name_to_exe.insert(name.to_string_lossy().into_owned(), exe);
                }
            }
            let mut tests = Vec::new();
            for (i, t) in planned.tests.iter().enumerate() {
                let dest_name = format!("bin{i}");
                let src = match resolve_binary(target_dir, t, &name_to_exe) {
                    Ok(p) => p,
                    Err(err) => {
                        return Ok(Err(Outcome::unviable(
                            planned.mutant.clone(),
                            err.to_string(),
                        )));
                    }
                };
                let dest = run_dir.join(&dest_name);
                backend
                    .clone_file(&src, &dest)
                    .or_else(|_| {
                        fs::copy(&src, &dest).map(|_| ()).context("copy test binary")
                    })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perm = fs::metadata(&dest)?.permissions();
                    perm.set_mode(0o755);
                    fs::set_permissions(&dest, perm)?;
                }
                tests.push(ManifestTest {
                    name: t.name.clone(),
                    binary: dest_name,
                    cwd_rel: t.cwd_rel.clone(),
                });
            }
            let manifest = BinariesManifest {
                mutant_id: planned.mutant.id.clone(),
                tests,
            };
            fs::write(
                run_dir.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest)?,
            )?;
    Ok(Ok(BuiltMutant {
        planned: planned.clone(),
        run_dir,
    }))
}

fn resolve_binary(
    target_dir: &Path,
    test: &crate::types::TestInfo,
    built: &HashMap<String, PathBuf>,
) -> Result<PathBuf> {
    let candidate = target_dir.join(&test.binary_rel);
    if candidate.is_file() {
        return Ok(candidate);
    }
    if let Some(name) = Path::new(&test.binary_rel).file_name() {
        let key = name.to_string_lossy().into_owned();
        if let Some(p) = built.get(&key) {
            return Ok(p.clone());
        }
        let deps = target_dir.join("quicksilver").join("deps").join(&key);
        if deps.is_file() {
            return Ok(deps);
        }
        // cargo uses the profile dir name
        for dir in ["quicksilver", "debug", "dev"] {
            let p = target_dir.join(dir).join("deps").join(&key);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    if let Some((_, p)) = built.iter().find(|(n, _)| {
        n.contains(&test.binary_name) || test.binary_name.is_empty() && n.contains(&test.package.replace('-', "_"))
    }) {
        return Ok(p.clone());
    }
    anyhow::bail!(
        "could not locate test binary for {} (rel {})",
        test.id,
        test.binary_rel
    )
}

pub fn walk_batch(
    cfg: &Config,
    backend: &dyn CloneBackend,
    worker_root: &Path,
    runs_root: &Path,
    batch: &LocalityBatch,
    build_counter: &AtomicU64,
    mut on_built: impl FnMut(Result<BuiltMutant, Outcome>) -> Result<()>,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    let target_dir = worker_root.join("target");
    for planned in &batch.mutants {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let result = match build_one(
            cfg,
            backend,
            worker_root,
            &target_dir,
            runs_root,
            planned,
            build_counter,
        ) {
            Ok(r) => r,
            Err(err) => Err(Outcome::unviable(planned.mutant.clone(), err.to_string())),
        };
        on_built(result)?;
    }
    Ok(())
}

pub fn _relative_helper(exe: &Path, target: &Path) -> String {
    relative_to_target(exe, target)
}
