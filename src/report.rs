use crate::estimate::EstimateReport;
use crate::outcome::OutcomeCounts;
use crate::types::{Outcome, VerdictKind};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonReport {
    pub outcomes: Vec<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<EstimateReport>,
    pub counts: CountReport,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CountReport {
    pub caught: usize,
    pub missed: usize,
    pub not_covered: usize,
    pub unviable: usize,
    pub errored: usize,
    pub timeout: usize,
    pub builds: usize,
    #[serde(default)]
    pub cargo_invocations: u64,
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
            cargo_invocations: 0,
            total: c.total(),
        }
    }
}

pub fn print_outcomes(outcomes: &[Outcome], cargo_invocations: u64) {
    let c = OutcomeCounts::from_outcomes(outcomes);
    eprintln!(
        "outcomes: {} total — {} caught, {} missed, {} not covered, {} unviable, {} errored, {} timeout ({} mutant builds, {} cargo invocations)",
        c.total(),
        c.caught,
        c.missed,
        c.not_covered,
        c.unviable,
        c.errored,
        c.timeout,
        c.builds,
        cargo_invocations
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

pub fn write_json(
    path: &Path,
    outcomes: &[Outcome],
    estimate: Option<&EstimateReport>,
    cargo_invocations: u64,
) -> Result<()> {
    let counts = OutcomeCounts::from_outcomes(outcomes);
    let mut report = JsonReport {
        outcomes: outcomes.to_vec(),
        estimate: estimate.cloned(),
        counts: CountReport::from(&counts),
    };
    report.counts.cargo_invocations = cargo_invocations;
    crate::util::write_atomic(path, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

pub fn print_exhaustive(outcomes: &[Outcome]) {
    let c = OutcomeCounts::from_outcomes(outcomes);
    let denom = c.caught + c.missed;
    let score = if denom == 0 {
        1.0
    } else {
        c.caught as f64 / denom as f64
    };
    eprintln!(
        "mutation score: {:.1}%  ({}/{} caught; excluding not-covered/unviable/errored/timeout)",
        score * 100.0,
        c.caught,
        denom
    );

    let mut by_crate: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for o in outcomes {
        let e = by_crate.entry(o.mutant.package.clone()).or_default();
        match o.kind {
            VerdictKind::Caught => e.0 += 1,
            VerdictKind::Missed => e.1 += 1,
            _ => {}
        }
    }
    if !by_crate.is_empty() {
        eprintln!("per crate:");
        for (pkg, (caught, missed)) in &by_crate {
            let d = caught + missed;
            let s = if d == 0 {
                1.0
            } else {
                *caught as f64 / d as f64
            };
            eprintln!("  {pkg}: {:.1}%  ({caught}/{d})", s * 100.0);
        }
    }

    let mut missed_by_file: BTreeMap<String, Vec<&Outcome>> = BTreeMap::new();
    for o in outcomes {
        if o.kind == VerdictKind::Missed {
            missed_by_file
                .entry(o.mutant.path.clone())
                .or_default()
                .push(o);
        }
    }
    if missed_by_file.is_empty() {
        eprintln!("missed inventory: (none)");
        return;
    }
    eprintln!("missed inventory:");
    for (file, rows) in &missed_by_file {
        eprintln!("  {file}");
        for o in rows {
            eprintln!(
                "    {}  {}  {}",
                o.mutant.id, o.mutant.fqn, o.mutant.replacement
            );
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditReport {
    pub sample_size: usize,
    pub agreements: usize,
    pub disagreements: Vec<AuditDisagreement>,
    pub selected_missed_full_caught: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditDisagreement {
    pub mutant_id: String,
    pub fqn: String,
    pub package: String,
    pub selected: VerdictKind,
    pub full: VerdictKind,
}

pub fn audit_compare(selected: &[Outcome], full: &[Outcome]) -> AuditReport {
    let full_by: BTreeMap<&str, &Outcome> =
        full.iter().map(|o| (o.mutant.id.as_str(), o)).collect();
    let mut disagreements = Vec::new();
    let mut agreements = 0usize;
    let mut selected_missed_full_caught = 0usize;
    for o in selected {
        let Some(other) = full_by.get(o.mutant.id.as_str()) else {
            disagreements.push(AuditDisagreement {
                mutant_id: o.mutant.id.clone(),
                fqn: o.mutant.fqn.clone(),
                package: o.mutant.package.clone(),
                selected: o.kind,
                full: o.kind,
            });
            continue;
        };
        if o.kind == other.kind {
            agreements += 1;
            continue;
        }
        if o.kind == VerdictKind::Missed && other.kind == VerdictKind::Caught {
            selected_missed_full_caught += 1;
        }
        disagreements.push(AuditDisagreement {
            mutant_id: o.mutant.id.clone(),
            fqn: o.mutant.fqn.clone(),
            package: o.mutant.package.clone(),
            selected: o.kind,
            full: other.kind,
        });
    }
    AuditReport {
        sample_size: selected.len(),
        agreements,
        disagreements,
        selected_missed_full_caught,
    }
}

pub fn print_audit(report: &AuditReport) {
    eprintln!("=== cargo-quicksilver audit-selection ===");
    eprintln!("sample: {}", report.sample_size);
    eprintln!("agreements: {}", report.agreements);
    eprintln!("disagreements: {}", report.disagreements.len());
    eprintln!(
        "selected Missed / full Caught (under-selection): {}",
        report.selected_missed_full_caught
    );
    for d in &report.disagreements {
        eprintln!(
            "  {}  {}  selected={:?} full={:?}",
            d.mutant_id, d.fqn, d.selected, d.full
        );
    }
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
