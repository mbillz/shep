use crate::claude_trust;
use crate::config::Config;
use crate::github::{PrDetails, PrRef};
use crate::git;
use crate::notify;
use crate::paths;
use crate::tmux;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct TriggeredReview {
    pub window_id: String,
    done_path: PathBuf,
    pub details: PrDetails,
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// `claude` launch command, deliberately with no prompt argument (see
/// trigger_review) and unscoped Bash rather than `Bash(git *) Bash(gh *)`:
/// Claude Code's allowlist requires every part of a compound/piped command
/// to match, and review exploration chains git/gh through head/grep/echo
/// constantly, so a narrower list hangs on an unanswered permission prompt.
/// Still no Edit/Write/WebFetch. `--settings` wires up the Stop hook that
/// signals completion (see write_hook_settings).
fn build_claude_command(config: &Config, settings_path: &str) -> String {
    let allowed_tools = "Bash Read Grep Glob";
    format!(
        "claude --model {} --permission-mode acceptEdits --allowedTools {} --settings {}",
        shell_quote(&config.model),
        shell_quote(allowed_tools),
        shell_quote(settings_path),
    )
}

fn review_prompt(pr: &PrRef) -> String {
    format!("/principal-review {}", pr.url())
}

/// Writes a settings file whose Stop hook touches `done_path` when Claude
/// finishes a turn - this is how completion is detected (see
/// `await_and_notify`), verified live to fire correctly in interactive mode.
fn write_hook_settings(settings_path: &Path, done_path: &Path) -> Result<()> {
    let done_str = done_path.to_str().context("done path is not valid UTF-8")?;
    let settings = serde_json::json!({
        "hooks": {
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("touch {}", shell_quote(done_str)),
                }]
            }]
        }
    });
    std::fs::write(settings_path, serde_json::to_string(&settings)?)
        .with_context(|| format!("writing {}", settings_path.display()))
}

/// Checks out the PR, opens a tmux window for it in the shared review
/// session, launches Claude, and submits the principal-review invocation as
/// its first message. Doesn't wait for the review to finish.
///
/// Takes `details` instead of fetching them, since callers like the daemon
/// already needed them to check the head SHA before deciding to trigger.
pub fn trigger_review(config: &Config, pr: &PrRef, details: PrDetails) -> Result<TriggeredReview> {
    let clone_root = config.clone_root();
    let base_repo = git::ensure_base_clone(&clone_root, &pr.owner, &pr.repo)?;
    claude_trust::ensure_trusted(&base_repo)?;
    let worktree_root = git::worktree_root(&clone_root, &pr.owner, &pr.repo);
    let worktree_path = git::ensure_pr_worktree(&base_repo, &worktree_root, pr.number)?;

    // Scratch dir lives outside the worktree so the reviewing agent's own
    // `git status`/exploration sees a pristine checkout, not shep's own
    // files. Worktrees are reused across re-reviews of the same PR, so any
    // `done` file from a previous review must be cleared before relaunching
    // - otherwise completion detection would return instantly on the stale
    // sentinel instead of waiting for the new hook to fire.
    let scratch_dir = paths::state_dir()?
        .join("reviews")
        .join(format!("{}-{}-{}", pr.owner, pr.repo, pr.number));
    std::fs::create_dir_all(&scratch_dir)?;
    let done_path = scratch_dir.join("done");
    let _ = std::fs::remove_file(&done_path);
    let settings_path = scratch_dir.join("settings.json");
    write_hook_settings(&settings_path, &done_path)?;
    let settings_str = settings_path
        .to_str()
        .context("settings path is not valid UTF-8")?;

    let window_id = tmux::create_window(&config.tmux_session, &worktree_path, &pr.full_ref())?;

    tmux::send_text(&window_id, &build_claude_command(config, settings_str))?;
    tmux::send_enter(&window_id)?;
    tmux::wait_for_text(&window_id, "accept edits on", Duration::from_secs(30))
        .context("Claude Code never became ready to accept the initial prompt")?;
    // Short settle: an Enter sent right at the shell-to-claude pty handoff
    // can still land on the outgoing process and get dropped.
    std::thread::sleep(Duration::from_millis(500));
    tmux::send_text(&window_id, &review_prompt(pr))?;
    std::thread::sleep(Duration::from_millis(300));
    tmux::send_enter(&window_id)?;

    Ok(TriggeredReview {
        window_id,
        done_path,
        details,
    })
}

fn wait_for_file(path: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if path.exists() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out after {:?} waiting for the review to finish", timeout);
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Blocks until the review's initial turn finishes (its Stop hook fires),
/// then fires a system notification.
pub fn await_and_notify(pr: &PrRef, review: &TriggeredReview, timeout: Duration) -> Result<()> {
    wait_for_file(&review.done_path, timeout)?;
    notify::notify(
        &format!("\u{1f415} Review ready: {}", pr.full_ref()),
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
    fn claude_command_has_no_prompt_argument_but_has_settings() {
        let mut config = Config::default();
        config.model = "sonnet".to_string();
        let cmd = build_claude_command(&config, "/tmp/settings.json");
        assert!(cmd.contains("--model 'sonnet'"));
        assert!(cmd.contains("--settings '/tmp/settings.json'"));
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

    #[test]
    fn hook_settings_reference_the_done_path() {
        let dir = std::env::temp_dir().join(format!("shep-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let settings_path = dir.join("settings.json");
        let done_path = dir.join("done");
        write_hook_settings(&settings_path, &done_path).unwrap();
        let raw = std::fs::read_to_string(&settings_path).unwrap();
        assert!(raw.contains("Stop"));
        assert!(raw.contains(done_path.to_str().unwrap()));
        std::fs::remove_dir_all(&dir).ok();
    }
}
