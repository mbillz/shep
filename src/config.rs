use crate::paths;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl RepoRef {
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub poll_interval_secs: u64,
    pub model: String,
    pub tmux_session: String,
    pub repo_clone_root: String,
    #[serde(rename = "repos")]
    pub repos: Vec<RepoRef>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            poll_interval_secs: 60,
            model: "sonnet".to_string(),
            tmux_session: "pr-review".to_string(),
            repo_clone_root: "~/.cache/shep/repos".to_string(),
            repos: Vec::new(),
        }
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        Ok(paths::config_dir()?.join("config.toml"))
    }

    pub fn load_or_default() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config at {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing config at {}", path.display()))
    }

    pub fn write_if_missing(&self) -> Result<bool> {
        let path = Self::path()?;
        if path.exists() {
            return Ok(false);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = toml::to_string_pretty(self)?;
        std::fs::write(&path, raw)?;
        Ok(true)
    }

    pub fn clone_root(&self) -> PathBuf {
        paths::expand_tilde(&self.repo_clone_root)
    }

    pub fn contains_repo(&self, owner: &str, name: &str) -> bool {
        self.repos
            .iter()
            .any(|r| r.owner == owner && r.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_empty_allowlist() {
        let cfg = Config::default();
        assert!(cfg.repos.is_empty());
        assert_eq!(cfg.poll_interval_secs, 60);
    }

    #[test]
    fn round_trips_through_toml() {
        let mut cfg = Config::default();
        cfg.repos.push(RepoRef {
            owner: "magicaltome".to_string(),
            name: "lightfield".to_string(),
        });
        let raw = toml::to_string_pretty(&cfg).unwrap();
        let parsed: Config = toml::from_str(&raw).unwrap();
        assert_eq!(parsed.repos.len(), 1);
        assert!(parsed.contains_repo("magicaltome", "lightfield"));
    }
}
