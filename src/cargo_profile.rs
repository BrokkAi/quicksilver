//! Explicit Cargo compilation selection, independent of nextest test policy.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CargoProfile(pub Option<String>);

#[derive(Clone, Copy)]
pub enum Frontend {
    Cargo,
    Nextest,
}

impl CargoProfile {
    pub fn validate(&self) -> Result<()> {
        if let Some(name) = &self.0 {
            ensure!(
                !name.trim().is_empty(),
                "Cargo profile name must not be empty"
            );
        }
        Ok(())
    }

    pub fn is_default(&self) -> bool {
        self.0.is_none()
    }

    /// Metadata has no compilation profile flag. Use only for build/list/run.
    pub fn apply(&self, cmd: &mut Command, frontend: Frontend) {
        if let Some(name) = &self.0 {
            cmd.arg(match frontend {
                Frontend::Cargo => "--profile",
                Frontend::Nextest => "--cargo-profile",
            })
            .arg(name);
        }
    }

    pub fn require_match(&self, requested: &Self) -> Result<()> {
        ensure!(self == requested,
            "Cargo compilation profile differs from the saved map: map={}, requested={}. Use the map's selection or run `cargo quicksilver map` with the new profile before building or testing",
            serde_json::to_string(self)?, serde_json::to_string(requested)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_flags_and_absent_command_defaults_are_distinct() {
        for (frontend, flag) in [
            (Frontend::Cargo, "--profile"),
            (Frontend::Nextest, "--cargo-profile"),
        ] {
            let mut cmd = Command::new("unused");
            CargoProfile::default().apply(&mut cmd, frontend);
            assert_eq!(cmd.get_args().count(), 0);
            CargoProfile(Some("measure".into())).apply(&mut cmd, frontend);
            assert_eq!(cmd.get_args().collect::<Vec<_>>(), [flag, "measure"]);
        }
        assert!(CargoProfile(Some(" ".into())).validate().is_err());
        assert!(CargoProfile::default()
            .require_match(&CargoProfile(Some("dev".into())))
            .is_err());
    }
}
