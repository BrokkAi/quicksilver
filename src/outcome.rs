use crate::types::{Outcome, VerdictKind};

#[derive(Debug, Clone, Default)]
pub struct OutcomeCounts {
    pub caught: usize,
    pub missed: usize,
    pub not_covered: usize,
    pub unviable: usize,
    pub errored: usize,
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
                VerdictKind::Errored => c.errored += 1,
                VerdictKind::Timeout => c.timeout += 1,
            }
        }
        c
    }

    pub fn total(&self) -> usize {
        self.caught + self.missed + self.not_covered + self.unviable + self.errored + self.timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Mutant;

    fn mutant() -> Mutant {
        Mutant {
            id: "m".into(),
            package: "p".into(),
            path: "p/src/lib.rs".into(),
            fqn: "f".into(),
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            end_line: 1,
            body_start_byte: 0,
            body_end_byte: 1,
            replacement: "{ 0 }".into(),
            return_kind: "number".into(),
        }
    }

    #[test]
    fn errored_is_not_unviable() {
        let o = Outcome::errored(mutant(), "t".into(), "exit 127".into());
        let c = OutcomeCounts::from_outcomes(&[o]);
        assert_eq!(c.errored, 1);
        assert_eq!(c.unviable, 0);
        assert_eq!(c.total(), 1);
    }
}
