use crate::types::{Outcome, VerdictKind};

#[derive(Debug, Clone, Default)]
pub struct OutcomeCounts {
    pub caught: usize,
    pub missed: usize,
    pub not_covered: usize,
    pub unviable: usize,
    pub timeout: usize,
    pub builds: usize,
}

impl OutcomeCounts {
    pub fn from_outcomes(outcomes: &[Outcome]) -> Self {
        let mut c = Self::default();
        for o in outcomes {
            if o.build {
                c.builds += 1;
            }
            match o.kind {
                VerdictKind::Caught => c.caught += 1,
                VerdictKind::Missed => c.missed += 1,
                VerdictKind::NotCovered => c.not_covered += 1,
                VerdictKind::Unviable => c.unviable += 1,
                VerdictKind::Timeout => c.timeout += 1,
            }
        }
        c
    }

    pub fn total(&self) -> usize {
        self.caught + self.missed + self.not_covered + self.unviable + self.timeout
    }
}
