use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME environment variable is not set")
}

fn xdg_dir(env_var: &str, fallback_under_home: &str) -> Result<PathBuf> {
    if let Some(v) = std::env::var_os(env_var) {
        if !v.is_empty() {
            return Ok(PathBuf::from(v));
        }
    }
    Ok(home_dir()?.join(fallback_under_home))
}

pub fn config_dir() -> Result<PathBuf> {
    Ok(xdg_dir("XDG_CONFIG_HOME", ".config")?.join("shep"))
}

pub fn state_dir() -> Result<PathBuf> {
    Ok(xdg_dir("XDG_STATE_HOME", ".local/state")?.join("shep"))
}

pub fn claude_skills_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".claude").join("skills"))
}

pub fn claude_config_file() -> Result<PathBuf> {
    Ok(home_dir()?.join(".claude.json"))
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_leading_tilde() {
        std::env::set_var("HOME", "/tmp/fake-home");
        let expanded = expand_tilde("~/.cache/shep/repos");
        assert_eq!(expanded, PathBuf::from("/tmp/fake-home/.cache/shep/repos"));
    }

    #[test]
    fn leaves_absolute_paths_alone() {
        let expanded = expand_tilde("/already/absolute");
        assert_eq!(expanded, PathBuf::from("/already/absolute"));
    }
}
