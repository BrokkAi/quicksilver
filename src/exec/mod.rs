use crate::types::Outcome;
use anyhow::Result;
use std::fs::{self, OpenOptions};
use std::io::Write;

pub mod build;
pub mod edited;
pub mod process;
pub mod schema;
pub mod test;

pub struct ExecResult {
    pub outcomes: Vec<Outcome>,
    /// Real `cargo` invocations, not per-mutant Outcome.build flags.
    pub cargo_invocations: u64,
    /// Selected tests for each executed mutant, used for proptest survivor notes.
    pub(crate) selected_test_ids: std::collections::HashMap<String, Vec<String>>,
}

pub(crate) fn append_outcome(path: &std::path::Path, outcome: &Outcome) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    serde_json::to_writer(&mut f, outcome)?;
    f.write_all(b"\n")?;
    Ok(())
}

pub(crate) fn load_resume(path: &std::path::Path) -> Result<Vec<Outcome>> {
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}

/// Bind built-in run's append-only verdicts to a compilation profile before
/// reading them. Start a new log before publishing a changed profile, so a
/// stopped run cannot label older verdicts as belonging to the new build.
pub(crate) fn prepare_outcomes(
    path: &std::path::Path,
    profile: &crate::cargo_profile::CargoProfile,
    resume: bool,
) -> Result<Vec<Outcome>> {
    let provenance = path.with_extension("profile.json");
    let outcomes = if resume && path.is_file() {
        let prior: crate::cargo_profile::CargoProfile = if provenance.is_file() {
            serde_json::from_slice(&fs::read(&provenance)?)?
        } else {
            Default::default()
        };
        anyhow::ensure!(prior == *profile,
            "Cargo compilation profile differs from the built-in run's saved outcomes; start a new run without --resume");
        load_resume(path)?
    } else {
        if path.is_file() {
            fs::remove_file(path)?;
        }
        Vec::new()
    };
    crate::util::write_atomic(&provenance, serde_json::to_vec(profile)?)?;
    Ok(outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn run_resume_refuses_profile_drift_without_relabeling_old_verdicts() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("outcomes.jsonl");
        fs::write(&log, "old verdict data").unwrap();
        let profile = crate::cargo_profile::CargoProfile(Some("measure".into()));
        let error = prepare_outcomes(&log, &profile, true).err().unwrap();
        assert!(error
            .to_string()
            .contains("Cargo compilation profile differs"));
        assert_eq!(fs::read_to_string(&log).unwrap(), "old verdict data");
        assert!(!log.with_extension("profile.json").exists());
        assert!(prepare_outcomes(&log, &profile, false).unwrap().is_empty());
        assert!(!log.exists());
        fs::write(&log, "").unwrap();
        assert!(prepare_outcomes(&log, &Default::default(), true).is_err());
        assert!(prepare_outcomes(&log, &profile, true).unwrap().is_empty());
        fs::remove_file(log.with_extension("profile.json")).unwrap();
        assert!(prepare_outcomes(&log, &Default::default(), true)
            .unwrap()
            .is_empty());
    }
}
