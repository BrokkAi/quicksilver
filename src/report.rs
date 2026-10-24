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
    /// The target triple the verdicts were measured on: a survivor on one
    /// platform can be killed on another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub outcomes: Vec<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<EstimateReport>,
    pub counts: CountReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<crate::higher_order::TriageReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second_order: Option<crate::higher_order::SecondOrderReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coupling: Option<crate::coupling::CouplingSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CountReport {
    pub caught: usize,
    #[serde(default)]
    pub caught_by_crash: usize,
    #[serde(default)]
    pub caught_by_timeout: usize,
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
            caught_by_crash: c.caught_by_crash,
            caught_by_timeout: c.caught_by_timeout,
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
        "outcomes: {} total — {} caught ({} by crash, {} by timeout), {} missed, {} not covered, {} unviable, {} errored, {} timeout ({} mutant builds, {} cargo invocations)",
        c.total(),
        c.caught,
        c.caught_by_crash,
        c.caught_by_timeout,
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
                    "  MISSED  {}  {}::{}  {}{}",
                    o.mutant.id,
                    o.mutant.package,
                    o.mutant.fqn,
                    o.mutant.replacement,
                    if o.message.as_deref() == Some(crate::proptest::NOTE) {
                        "  (proptest properties only; may vary between runs)"
                    } else {
                        ""
                    }
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
    write_json_with_evidence(
        path,
        outcomes,
        estimate,
        cargo_invocations,
        None,
        None,
        None,
    )
}

pub fn write_json_with_evidence(
    path: &Path,
    outcomes: &[Outcome],
    estimate: Option<&EstimateReport>,
    cargo_invocations: u64,
    triage: Option<&crate::higher_order::TriageReport>,
    second_order: Option<&crate::higher_order::SecondOrderReport>,
    coupling: Option<&crate::coupling::CouplingSummary>,
) -> Result<()> {
    let counts = OutcomeCounts::from_outcomes(outcomes);
    let mut report = JsonReport {
        target: crate::target::host_triple(Path::new(".")).ok(),
        outcomes: outcomes.to_vec(),
        estimate: estimate.cloned(),
        counts: CountReport::from(&counts),
        triage: triage.cloned(),
        second_order: second_order.cloned(),
        coupling: coupling.cloned(),
    };
    report.counts.cargo_invocations = cargo_invocations;
    crate::util::write_atomic(path, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

pub fn print_triage(report: &crate::higher_order::TriageReport) {
    eprintln!("=== cargo-quicksilver survivor triage ===");
    eprintln!(
        "survivors: {}  experiments: {}",
        report.survivors,
        report.experiments.len()
    );
    for finding in &report.experiments {
        eprintln!(
            "  {} + {}  {:?}  kills survivor/control/pair={}/{}/{}",
            finding.survivor_id,
            finding.control_id,
            finding.classification,
            finding.survivor.killed_test_ids.len(),
            finding.control.killed_test_ids.len(),
            finding.pair.killed_test_ids.len()
        );
    }
}

pub fn print_second_order(report: &crate::higher_order::SecondOrderReport) {
    eprintln!("=== cargo-quicksilver second-order estimate ===");
    eprintln!(
        "pairs: {}/{} completed ({} caught); h={:.3}; implied first-order p={:.3}",
        report.completed_pairs,
        report.requested_pairs,
        report.caught_pairs,
        report.higher_order_kill_rate,
        report.implied_first_order_kill_probability
    );
    eprintln!("assumption: {}", report.assumption);
}

pub fn print_coupling(report: &crate::coupling::CouplingSummary) {
    eprintln!(
        "coupling artifact: {} row(s), {} complete, {} test column(s), {} subsumption relation(s) at {}",
        report.rows,
        report.complete_rows,
        report.test_columns,
        report.subsumptions.len(),
        report.path.display()
    );
    if report.invalidated_for_source_change {
        eprintln!("  prior coupling rows invalidated because the source cache key changed");
    }
    for relation in &report.subsumptions {
        eprintln!(
            "  {} subsumes {}",
            relation.dominator_id, relation.subsumed_id
        );
    }
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
    /// Mutants the full suite caught but the selected tests did not: Missed
    /// or NotCovered with selection. Selection lost the test that kills them.
    pub lost_kills: usize,
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
    let mut lost_kills = 0usize;
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
        // NotCovered with selection and Missed with the full suite say the
        // same thing: no test catches the mutant. The full suite only ran
        // tests that never reach it.
        if o.kind == other.kind
            || (o.kind == VerdictKind::NotCovered && other.kind == VerdictKind::Missed)
        {
            agreements += 1;
            continue;
        }
        if matches!(o.kind, VerdictKind::Missed | VerdictKind::NotCovered)
            && other.kind == VerdictKind::Caught
        {
            lost_kills += 1;
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
        lost_kills,
    }
}

pub fn print_audit(report: &AuditReport) {
    eprintln!("=== cargo-quicksilver audit-selection ===");
    eprintln!("sample: {}", report.sample_size);
    eprintln!("agreements: {}", report.agreements);
    eprintln!("disagreements: {}", report.disagreements.len());
    eprintln!(
        "kills lost by selection (full suite Caught, selected tests Missed or NotCovered): {}",
        report.lost_kills
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
