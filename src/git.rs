use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

fn run_git(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .with_context(|| format!("failed to run `git {}` in {}", args.join(" "), cwd.display()))?;
    if !output.status.success() {
        bail!(
            "`git {}` in {} failed: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Ensures a base clone of owner/repo exists under clone_root; PR worktrees
/// branch off of this instead of a fresh clone per review. Partial clone
/// (`--filter=blob:none`): full commit graph, but blob contents are fetched
/// lazily on checkout instead of downloading the repo's whole history.
pub fn ensure_base_clone(clone_root: &Path, owner: &str, repo: &str) -> Result<PathBuf> {
    let repo_path = clone_root.join(owner).join(repo);
    if repo_path.join(".git").exists() {
        run_git(&repo_path, &["fetch", "origin"])?;
        return Ok(repo_path);
    }
    std::fs::create_dir_all(&repo_path)?;
    let output = Command::new("gh")
        .args([
            "repo",
            "clone",
            &format!("{owner}/{repo}"),
            repo_path.to_str().context("clone path is not valid UTF-8")?,
            "--",
            "--filter=blob:none",
        ])
        .output()
        .with_context(|| format!("failed to run `gh repo clone {owner}/{repo}`"))?;
    if !output.status.success() {
        bail!(
            "`gh repo clone {owner}/{repo}` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(repo_path)
}

/// Fetches the PR's head and ensures a detached worktree at that ref exists
/// under worktree_root. Uses a non-`refs/heads/*` ref: a branch ref can't be
/// re-fetched while checked out in an existing worktree (the re-review
/// case), and a detached checkout sidesteps that.
pub fn ensure_pr_worktree(
    base_repo: &Path,
    worktree_root: &Path,
    number: u64,
) -> Result<PathBuf> {
    let ref_name = format!("refs/shepherd/pr-{number}");
    run_git(
        base_repo,
        &[
            "fetch",
            "origin",
            &format!("+refs/pull/{number}/head:{ref_name}"),
        ],
    )
    .with_context(|| format!("fetching PR #{number} head"))?;

    let worktree_path = worktree_root.join(format!("pr-{number}"));
    if worktree_path.join(".git").exists() {
        run_git(&worktree_path, &["checkout", "--detach", &ref_name])
            .with_context(|| format!("updating existing worktree for PR #{number}"))?;
    } else {
        std::fs::create_dir_all(worktree_root)?;
        run_git(
            base_repo,
            &[
                "worktree",
                "add",
                "--detach",
                worktree_path
                    .to_str()
                    .context("worktree path is not valid UTF-8")?,
                &ref_name,
            ],
        )
        .with_context(|| format!("creating worktree for PR #{number}"))?;
    }
    Ok(worktree_path)
}
