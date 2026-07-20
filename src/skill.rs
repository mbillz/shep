use crate::paths;
use anyhow::{Context, Result};

const SKILL_MD: &str = include_str!("../skills/principal-review/SKILL.md");

/// Installs (or overwrites) the principal-review skill into ~/.claude/skills,
/// so `claude` sessions shep launches can invoke it as /principal-review.
pub fn install() -> Result<()> {
    let dir = paths::claude_skills_dir()?.join("principal-review");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating skill directory at {}", dir.display()))?;
    let path = dir.join("SKILL.md");
    std::fs::write(&path, SKILL_MD)
        .with_context(|| format!("writing skill file to {}", path.display()))?;
    Ok(())
}
