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
    pub triage: Option<crate::higher_order::TriageReport>,
    pub second_order: Option<crate::higher_order::SecondOrderReport>,
    pub(crate) coupling_rows: Vec<crate::coupling::CouplingRow>,
    pub coupling: Option<crate::coupling::CouplingSummary>,
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
