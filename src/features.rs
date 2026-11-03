//! Cargo's requested feature selection, shared by every campaign command.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct CargoFeatures {
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
}

impl CargoFeatures {
    pub fn normalize(&mut self) {
        self.features = self
            .features
            .iter()
            .flat_map(|s| s.split(|c: char| c == ',' || c.is_whitespace()))
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        self.features.sort();
        self.features.dedup();
    }

    pub fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if self.all_features {
            args.push("--all-features".into());
        }
        if self.no_default_features {
            args.push("--no-default-features".into());
        }
        if !self.features.is_empty() {
            args.extend(["--features".into(), self.features.join(",")]);
        }
        args
    }

    pub fn apply(&self, cmd: &mut Command) {
        cmd.args(self.args());
    }

    pub fn require_match(&self, requested: &Self) -> Result<()> {
        ensure!(self == requested,
            "Cargo features differ from the saved map: map={}, requested={}. Use the map's selection or run `cargo quicksilver map` with the new selection before building or testing",
            serde_json::to_string(self)?, serde_json::to_string(requested)?);
        Ok(())
    }
}
