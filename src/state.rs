use crate::paths;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewedPr {
    pub last_sha: String,
    pub reviewed_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(flatten)]
    entries: HashMap<String, ReviewedPr>,
}

fn key(owner: &str, repo: &str, number: u64) -> String {
    format!("{owner}/{repo}#{number}")
}

impl State {
    pub fn path() -> Result<PathBuf> {
        Ok(paths::state_dir()?.join("state.json"))
    }

    pub fn load_or_default() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading state at {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("parsing state at {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, raw)?;
        Ok(())
    }

    /// True if unseen, or its head SHA has moved since last reviewed.
    pub fn needs_review(&self, owner: &str, repo: &str, number: u64, head_sha: &str) -> bool {
        match self.entries.get(&key(owner, repo, number)) {
            Some(entry) => entry.last_sha != head_sha,
            None => true,
        }
    }

    pub fn mark_reviewed(&mut self, owner: &str, repo: &str, number: u64, head_sha: &str) {
        self.entries.insert(
            key(owner, repo, number),
            ReviewedPr {
                last_sha: head_sha.to_string(),
                reviewed_at: chrono::Local::now().to_rfc3339(),
            },
        );
    }

    pub fn entries(&self) -> impl Iterator<Item = (&String, &ReviewedPr)> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unseen_pr_needs_review() {
        let state = State::default();
        assert!(state.needs_review("acme", "widgets", 1, "abc123"));
    }

    #[test]
    fn same_sha_does_not_need_review_again() {
        let mut state = State::default();
        state.mark_reviewed("acme", "widgets", 1, "abc123");
        assert!(!state.needs_review("acme", "widgets", 1, "abc123"));
    }

    #[test]
    fn new_sha_needs_review_again() {
        let mut state = State::default();
        state.mark_reviewed("acme", "widgets", 1, "abc123");
        assert!(state.needs_review("acme", "widgets", 1, "def456"));
    }

    #[test]
    fn round_trips_through_json() {
        let mut state = State::default();
        state.mark_reviewed("acme", "widgets", 1, "abc123");
        let raw = serde_json::to_string(&state).unwrap();
        let parsed: State = serde_json::from_str(&raw).unwrap();
        assert!(!parsed.needs_review("acme", "widgets", 1, "abc123"));
    }
}
