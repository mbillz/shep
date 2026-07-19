use crate::claude_trust;
use crate::config::Config;
use crate::github::{PrDetails, PrRef};
use crate::git;
use crate::herdr;
use anyhow::{Context, Result};
use std::time::Duration;

pub struct TriggeredReview {
    pub tab_id: String,
    pub pane_id: String,
    pub details: PrDetails,
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The bare `claude` launch command - deliberately has no prompt argument.
/// Passing the initial message as a CLI argument is not reliably picked up
/// as the first turn in interactive mode (verified empirically: the session
/// launches and just sits idle, never processing it). The prompt is instead
/// typed in and submitted as a separate step once the session is ready, the
/// same way a human would.
fn build_claude_command(config: &Config) -> String {
    // Unscoped Bash, not `Bash(git *) Bash(gh *)`: Claude Code's allowlist
    // matching requires every piece of a compound command (pipes, `;`
    // chains, `$(...)`) to match, and ordinary review exploration chains
    // git/gh through head/grep/echo constantly. A narrower list causes a
    // permission prompt that nobody is present to answer in an unattended
    // review, which then hangs. Still no Edit/Write/WebFetch - this can
    // read and run shell commands in its (disposable, per-PR) worktree, not
    // modify files or reach the network beyond git/gh.
    let allowed_tools = "Bash Read Grep Glob";
    format!(
        "claude --model {} --permission-mode acceptEdits --allowedTools {}",
        shell_quote(&config.model),
        shell_quote(allowed_tools),
    )
}

fn review_prompt(pr: &PrRef) -> String {
    format!("/principal-review {}", pr.url())
}

/// Ensures the PR's code is checked out, opens a tab for it in the shared
/// review workspace, launches Claude in that pane, and submits the
/// principal-review skill invocation as its first message. Does not wait for
/// the review itself to finish.
///
/// Takes `details` rather than fetching them internally so callers that
/// already needed them (e.g. the daemon, to check the head SHA before
/// deciding to trigger at all) don't pay for a second `gh pr view`.
pub fn trigger_review(config: &Config, pr: &PrRef, details: PrDetails) -> Result<TriggeredReview> {
    let clone_root = config.clone_root();
    let base_repo = git::ensure_base_clone(&clone_root, &pr.owner, &pr.repo)?;
    claude_trust::ensure_trusted(&base_repo)?;
    let worktree_root = clone_root.join(format!("{}-{}-worktrees", pr.owner, pr.repo));
    let worktree_path = git::ensure_pr_worktree(&base_repo, &worktree_root, pr.number)?;

    let workspace = herdr::ensure_workspace(&config.herdr_workspace_label)?;
    let pane = herdr::create_tab(&workspace.workspace_id, &worktree_path, &pr.full_ref())?;
    if let Some(starter_tab_id) = workspace.starter_tab_id {
        // Safe now: the workspace has our new tab too, so this is no longer
        // the last one (herdr refuses to close a workspace's last tab).
        if let Err(e) = herdr::close_tab(&starter_tab_id) {
            eprintln!("warning: could not close the '{}' workspace's starter tab: {e:#}", config.herdr_workspace_label);
        }
    }

    herdr::run_in_pane(&pane.pane_id, &build_claude_command(config))?;
    herdr::wait_for_text(&pane.pane_id, "accept edits on", Duration::from_secs(30))
        .context("Claude Code never became ready to accept the initial prompt")?;
    // Even after the status bar renders, there's a brief window right at the
    // shell-to-claude pty handoff where a submitted Enter can land on the
    // outgoing process instead of claude's input handler (verified
    // empirically: an Enter sent immediately after the ready-text match was
    // silently dropped; a second one moments later worked). A short settle
    // avoids racing that handoff.
    std::thread::sleep(Duration::from_millis(500));
    herdr::send_text(&pane.pane_id, &review_prompt(pr))?;
    std::thread::sleep(Duration::from_millis(300));
    herdr::send_enter(&pane.pane_id)?;

    Ok(TriggeredReview {
        tab_id: pane.tab_id,
        pane_id: pane.pane_id,
        details,
    })
}

/// Blocks until the review's initial turn finishes, then fires a system
/// notification. See `herdr::wait_until_finished` for why this polls status
/// rather than waiting on a single agent-status transition.
pub fn await_and_notify(pr: &PrRef, review: &TriggeredReview, timeout: Duration) -> Result<()> {
    herdr::wait_until_finished(&review.pane_id, timeout)?;
    herdr::notify(
        &format!("Review ready: {}", pr.full_ref()),
        &review.details.title,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_and_escapes_single_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn claude_command_has_no_prompt_argument() {
        let mut config = Config::default();
        config.model = "sonnet".to_string();
        let cmd = build_claude_command(&config);
        assert!(cmd.contains("--model 'sonnet'"));
        assert!(!cmd.contains("principal-review"));
    }

    #[test]
    fn review_prompt_includes_pr_url_and_skill_invocation() {
        let pr = PrRef {
            owner: "acme".to_string(),
            repo: "widgets".to_string(),
            number: 7,
        };
        let prompt = review_prompt(&pr);
        assert_eq!(prompt, "/principal-review https://github.com/acme/widgets/pull/7");
    }
}
