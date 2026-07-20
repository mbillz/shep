use crate::paths;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// Old state files predate this field; a missing status defaults to Reviewed
// (not Reviewing) - that just means "trust the old record," not "treat
// every pre-existing entry as abandoned and immediately retrigger a pile
// of reviews."
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Reviewing,
    #[default]
    Reviewed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewedPr {
    pub last_sha: String,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub window_id: Option<String>,
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

/// What to do about a PR, based on stored state alone. `InProgress` needs a
/// follow-up check (is the window actually still alive?) that only the
/// caller can do, since that's I/O this module deliberately doesn't touch.
#[derive(Debug, PartialEq)]
pub enum ReviewCheck {
    NeedsReview,
    AlreadyReviewed,
    InProgress { window_id: Option<String> },
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

    /// Writes atomically (temp file + rename): the daemon's main loop and
    /// each review's background completion-watcher thread both save this
    /// file independently, so a plain in-place write risks a torn/corrupt
    /// file if two writes land at the same moment.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
    }

    pub fn check(&self, owner: &str, repo: &str, number: u64, head_sha: &str) -> ReviewCheck {
        match self.entries.get(&key(owner, repo, number)) {
            None => ReviewCheck::NeedsReview,
            Some(entry) if entry.last_sha != head_sha => ReviewCheck::NeedsReview,
            Some(entry) if entry.status == Status::Reviewed => ReviewCheck::AlreadyReviewed,
            Some(entry) => ReviewCheck::InProgress {
                window_id: entry.window_id.clone(),
            },
        }
    }

    pub fn mark_reviewing(&mut self, owner: &str, repo: &str, number: u64, head_sha: &str, window_id: &str) {
        self.entries.insert(
            key(owner, repo, number),
            ReviewedPr {
                last_sha: head_sha.to_string(),
                status: Status::Reviewing,
                window_id: Some(window_id.to_string()),
                reviewed_at: chrono::Local::now().to_rfc3339(),
            },
        );
    }

    pub fn mark_reviewed(&mut self, owner: &str, repo: &str, number: u64, head_sha: &str) {
        self.entries.insert(
            key(owner, repo, number),
            ReviewedPr {
                last_sha: head_sha.to_string(),
                status: Status::Reviewed,
                window_id: None,
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
        assert_eq!(
            state.check("acme", "widgets", 1, "abc123"),
            ReviewCheck::NeedsReview
        );
    }

    #[test]
    fn same_sha_already_reviewed() {
        let mut state = State::default();
        state.mark_reviewed("acme", "widgets", 1, "abc123");
        assert_eq!(
            state.check("acme", "widgets", 1, "abc123"),
            ReviewCheck::AlreadyReviewed
        );
    }

    #[test]
    fn new_sha_needs_review_again_even_if_previously_reviewed() {
        let mut state = State::default();
        state.mark_reviewed("acme", "widgets", 1, "abc123");
        assert_eq!(
            state.check("acme", "widgets", 1, "def456"),
            ReviewCheck::NeedsReview
        );
    }

    #[test]
    fn same_sha_still_reviewing_reports_in_progress() {
        let mut state = State::default();
        state.mark_reviewing("acme", "widgets", 1, "abc123", "@5");
        assert_eq!(
            state.check("acme", "widgets", 1, "abc123"),
            ReviewCheck::InProgress {
                window_id: Some("@5".to_string())
            }
        );
    }

    #[test]
    fn new_sha_needs_review_even_if_currently_reviewing() {
        let mut state = State::default();
        state.mark_reviewing("acme", "widgets", 1, "abc123", "@5");
        assert_eq!(
            state.check("acme", "widgets", 1, "def456"),
            ReviewCheck::NeedsReview
        );
    }

    #[test]
    fn round_trips_through_json() {
        let mut state = State::default();
        state.mark_reviewed("acme", "widgets", 1, "abc123");
        let raw = serde_json::to_string(&state).unwrap();
        let parsed: State = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            parsed.check("acme", "widgets", 1, "abc123"),
            ReviewCheck::AlreadyReviewed
        );
    }

    #[test]
    fn missing_status_field_defaults_to_reviewed() {
        // Simulates an old state file written before `status` existed.
        let raw = r#"{"acme/widgets#1":{"last_sha":"abc123","reviewed_at":"2026-01-01T00:00:00Z"}}"#;
        let state: State = serde_json::from_str(raw).unwrap();
        assert_eq!(
            state.check("acme", "widgets", 1, "abc123"),
            ReviewCheck::AlreadyReviewed
        );
    }
}
