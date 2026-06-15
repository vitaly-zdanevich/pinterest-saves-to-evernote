use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    /// Pinterest pin ids that were already baseline-marked or exported to Evernote.
    #[serde(default)]
    pub processed_pin_ids: BTreeSet<String>,
    /// Set on the first successful baseline/import run.
    pub initialized_at: Option<DateTime<Utc>>,
    /// Updated after a successful sync, including runs with no new pins.
    pub last_successful_sync_at: Option<DateTime<Utc>>,
}

impl State {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let json = fs::read_to_string(path)
            .with_context(|| format!("failed to read state file {}", path.display()))?;
        serde_json::from_str(&json)
            .with_context(|| format!("failed to parse state file {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create state directory {}", parent.display())
            })?;
        }

        // Write through a temporary file in the target directory so GitHub Actions
        // cache restores never leave a partially written JSON state file.
        let mut temp = tempfile::NamedTempFile::new_in(
            path.parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new(".")),
        )
        .with_context(|| {
            format!(
                "failed to create temporary state file for {}",
                path.display()
            )
        })?;
        serde_json::to_writer_pretty(&mut temp, self).context("failed to serialize state")?;
        temp.persist(path)
            .with_context(|| format!("failed to write state file {}", path.display()))?;
        Ok(())
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized_at.is_some() || !self.processed_pin_ids.is_empty()
    }

    pub fn contains(&self, pin_id: &str) -> bool {
        self.processed_pin_ids.contains(pin_id)
    }

    pub fn mark_processed(&mut self, pin_id: impl Into<String>) {
        self.processed_pin_ids.insert(pin_id.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let mut state = State::default();
        state.mark_processed("pin-1");

        state.save(&path).expect("save state");
        let loaded = State::load(&path).expect("load state");

        assert!(loaded.contains("pin-1"));
        assert!(!loaded.contains("pin-2"));
        assert!(loaded.is_initialized());
    }
}
