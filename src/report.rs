use crate::estimate::EstimateReport;
use crate::outcome::OutcomeCounts;
use crate::types::{Outcome, VerdictKind};
use anyhow::Result;
use serde::Serialize;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct JsonReport {
    pub outcomes: Vec<Outcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimate: Option<EstimateReport>,
    pub counts: CountReport,
}

#[derive(Debug, Clone, Serialize)]
pub struct CountReport {
    pub caught: usize,
    pub missed: usize,
    pub not_covered: usize,
    pub unviable: usize,
    pub errored: usize,
    pub timeout: usize,
    pub builds: usize,
    pub total: usize,
}

impl From<&OutcomeCounts> for CountReport {
    fn from(c: &OutcomeCounts) -> Self {
        Self {
            caught: c.caught,
            missed: c.missed,
            not_covered: c.not_covered,
            unviable: c.unviable,
            errored: c.errored,
            timeout: c.timeout,
            builds: c.builds,
            total: c.total(),
        }
    }
}

pub fn print_outcomes(outcomes: &[Outcome]) {
    let c = OutcomeCounts::from_outcomes(outcomes);
    eprintln!(
        "outcomes: {} total — {} caught, {} missed, {} not covered, {} unviable, {} errored, {} timeout ({} builds)",
        c.total(),
        c.caught,
        c.missed,
        c.not_covered,
        c.unviable,
        c.errored,
        c.timeout,
        c.builds
    );
    for o in outcomes {
        match o.kind {
            VerdictKind::Missed => {
                eprintln!(
                    "  MISSED  {}  {}::{}  {}",
                    o.mutant.id, o.mutant.package, o.mutant.fqn, o.mutant.replacement
                );
            }
            VerdictKind::Timeout => {
                eprintln!(
                    "  TIMEOUT {}  {}  ({})",
                    o.mutant.id,
                    o.mutant.fqn,
                    o.test.as_deref().unwrap_or("?")
                );
            }
            VerdictKind::Errored => {
                eprintln!(
                    "  ERRORED {}  {}  ({})  {}",
                    o.mutant.id,
                    o.mutant.fqn,
                    o.test.as_deref().unwrap_or("?"),
                    o.message.as_deref().unwrap_or("")
                );
            }
            _ => {}
        }
    }
}

pub fn write_json(path: &Path, outcomes: &[Outcome], estimate: Option<&EstimateReport>) -> Result<()> {
    let counts = OutcomeCounts::from_outcomes(outcomes);
    let report = JsonReport {
        outcomes: outcomes.to_vec(),
        estimate: estimate.cloned(),
        counts: CountReport::from(&counts),
    };
    crate::util::write_atomic(path, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

pub fn print_list(mutants: &[crate::types::Mutant], mut out: impl Write) -> Result<()> {
    writeln!(out, "{} mutants", mutants.len())?;
    for m in mutants {
        writeln!(
            out,
            "{}\t{}\t{}\t{}\t{}",
            m.id, m.package, m.path, m.fqn, m.replacement
        )?;
    }
    Ok(())
}
