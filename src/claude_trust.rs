use crate::paths;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;

/// Marks `repo_path` trusted in ~/.claude.json so Claude Code doesn't block
/// on the first-run trust dialog. Claude Code resolves a worktree's trust
/// against its main repo's path, so this runs once per base clone, not per
/// PR. Only touches `hasTrustDialogAccepted`; writes atomically since this
/// is shared global state.
pub fn ensure_trusted(repo_path: &Path) -> Result<()> {
    ensure_trusted_at(&paths::claude_config_file()?, repo_path)
}

/// Does the actual work against an explicit config path, so tests can point
/// it at a scratch file instead of mutating the real `$HOME`/`~/.claude.json`
/// (and racing other tests that do the same).
fn ensure_trusted_at(path: &Path, repo_path: &Path) -> Result<()> {
    let key = repo_path
        .to_str()
        .context("repo path is not valid UTF-8")?
        .to_string();

    // Missing file is treated as `{}` rather than an error - it just means
    // `claude` has never been run interactively on this machine yet. Not
    // this function's job to bootstrap a fresh Claude Code install; it only
    // needs somewhere to record trust once one exists.
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => "{}".to_string(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut root: Value =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;

    let projects = root
        .as_object_mut()
        .context("~/.claude.json root is not an object")?
        .entry("projects")
        .or_insert_with(|| json!({}));
    let projects = projects
        .as_object_mut()
        .context("~/.claude.json `projects` is not an object")?;

    let entry = projects.entry(key).or_insert_with(|| json!({}));
    if entry.get("hasTrustDialogAccepted").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    entry
        .as_object_mut()
        .context("project entry in ~/.claude.json is not an object")?
        .insert("hasTrustDialogAccepted".to_string(), Value::Bool(true));

    let tmp_path = path.with_extension("json.shep-tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(&root)?)
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &path)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_claude_json_when_missing() {
        let dir = std::env::temp_dir().join(format!("shep-trust-test-missing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join(".claude.json");

        ensure_trusted_at(&config_path, Path::new("/some/repo")).unwrap();

        let raw = std::fs::read_to_string(&config_path).unwrap();
        let root: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            root["projects"]["/some/repo"]["hasTrustDialogAccepted"],
            Value::Bool(true)
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn preserves_existing_unrelated_config() {
        let dir = std::env::temp_dir().join(format!("shep-trust-test-existing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join(".claude.json");
        std::fs::write(
            &config_path,
            r#"{"someOtherSetting": true, "projects": {"/already/trusted": {"hasTrustDialogAccepted": true}}}"#,
        )
        .unwrap();

        ensure_trusted_at(&config_path, Path::new("/some/repo")).unwrap();

        let raw = std::fs::read_to_string(&config_path).unwrap();
        let root: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(root["someOtherSetting"], Value::Bool(true));
        assert_eq!(
            root["projects"]["/already/trusted"]["hasTrustDialogAccepted"],
            Value::Bool(true)
        );
        assert_eq!(
            root["projects"]["/some/repo"]["hasTrustDialogAccepted"],
            Value::Bool(true)
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn second_call_is_a_noop_once_already_trusted() {
        let dir = std::env::temp_dir().join(format!("shep-trust-test-idempotent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join(".claude.json");

        ensure_trusted_at(&config_path, Path::new("/some/repo")).unwrap();
        ensure_trusted_at(&config_path, Path::new("/some/repo")).unwrap();

        let raw = std::fs::read_to_string(&config_path).unwrap();
        let root: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            root["projects"]["/some/repo"]["hasTrustDialogAccepted"],
            Value::Bool(true)
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
