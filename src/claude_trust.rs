use crate::paths;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;

/// Marks `repo_path` as a trusted project in ~/.claude.json so Claude Code
/// launched there (interactively, in a herdr pane) doesn't block on the
/// first-run workspace-trust dialog. Claude Code resolves trust for a git
/// worktree against its main repo's path, so this only ever needs to run
/// once per base clone, not per PR worktree.
///
/// Only the `hasTrustDialogAccepted` field is touched; any existing entry
/// (and the rest of the file) is left as-is. Writes atomically (temp file +
/// rename) since this is shared global Claude Code state.
pub fn ensure_trusted(repo_path: &Path) -> Result<()> {
    let path = paths::claude_config_file()?;
    let key = repo_path
        .to_str()
        .context("repo path is not valid UTF-8")?
        .to_string();

    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
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

    let tmp_path = path.with_extension("json.shepherd-tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(&root)?)
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, &path)
        .with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}
