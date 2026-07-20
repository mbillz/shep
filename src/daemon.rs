use crate::config::Config;
use crate::git;
use crate::github;
use crate::github::PrRef;
use crate::paths;
use crate::review;
use crate::state::{self, ReviewCheck, State};
use crate::tmux;
use anyhow::{bail, Context};
use std::time::Duration;

/// Refuses to start if another daemon is already running: two instances
/// polling the same state file can race (both see "needs review" for the
/// same PR before either saves), opening a duplicate window for it.
fn acquire_lock() -> anyhow::Result<()> {
    let lock_path = paths::state_dir()?.join("daemon.pid");
    if let Ok(existing) = std::fs::read_to_string(&lock_path) {
        if let Ok(pid) = existing.trim().parse::<u32>() {
            // .output() (not .status()) so "kill: N: No such process" for a
            // stale/dead pid is captured and discarded instead of leaking to
            // the terminal - the exit status alone is all that's needed.
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if alive {
                bail!(
                    "another shep daemon is already running (pid {pid}) - stop it first, or delete {} if that's stale",
                    lock_path.display()
                );
            }
        }
    }
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&lock_path, std::process::id().to_string())
        .with_context(|| format!("writing {}", lock_path.display()))
}

/// Polls allowlisted repos for review requests every `poll_interval_secs`;
/// each triggered review is watched for completion on its own thread so a
/// slow one doesn't stall the next poll.
pub fn run(config: &Config) -> anyhow::Result<()> {
    acquire_lock()?;
    if config.repos.is_empty() {
        eprintln!(
            "warning: no repos configured in {} - the daemon has nothing to watch",
            Config::path()?.display()
        );
    }
    tmux::ensure_session(&config.tmux_session, &paths::home_dir()?)?;

    loop {
        let now = chrono::Local::now().format("%H:%M:%S");
        match poll_once(config) {
            Ok((checked, 0)) => {
                println!("[{now}] \u{1f415} checked {checked} PR(s), nothing new")
            }
            Ok((checked, triggered)) => {
                println!("[{now}] \u{1f415} checked {checked} PR(s), triggered {triggered} review(s)")
            }
            Err(e) => eprintln!("[{now}] poll failed: {e:#}"),
        }
        if let Err(e) = cleanup_stale(config) {
            eprintln!("[{now}] cleanup failed: {e:#}");
        }
        std::thread::sleep(Duration::from_secs(config.poll_interval_secs));
    }
}

/// Returns (PRs checked, reviews newly triggered).
fn poll_once(config: &Config) -> anyhow::Result<(usize, usize)> {
    let mut state = State::load_or_default()?;
    let prs = github::list_review_requested(config)?;
    let checked = prs.len();
    let mut triggered_count = 0;

    for pr in prs {
        if triggered_count >= config.max_triggers_per_poll {
            println!(
                "\u{1f415} hit max_triggers_per_poll ({}) - the rest will pick up on the next poll",
                config.max_triggers_per_poll
            );
            break;
        }

        let details = match github::pr_view(&pr) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("skipping {}: could not fetch PR details: {e:#}", pr.full_ref());
                continue;
            }
        };
        match state.check(&pr.owner, &pr.repo, pr.number, &details.head_sha) {
            ReviewCheck::AlreadyReviewed => continue,
            // Only genuinely skip if the window's still there; a killed
            // window (e.g. closed by hand) means it never finished, so
            // retrigger the same as a fresh review.
            ReviewCheck::InProgress {
                window_id: Some(w),
            } if tmux::window_exists(&w) => continue,
            ReviewCheck::InProgress { .. } | ReviewCheck::NeedsReview => {}
        }

        println!("\u{1f415} triggering review for {}", pr.full_ref());
        match review::trigger_review(config, &pr, details) {
            Ok(triggered) => {
                state.mark_reviewing(
                    &pr.owner,
                    &pr.repo,
                    pr.number,
                    &triggered.details.head_sha,
                    &triggered.window_id,
                );
                state.save()?;
                triggered_count += 1;

                let pr_for_thread = pr.clone();
                std::thread::spawn(move || {
                    let sha = triggered.details.head_sha.clone();
                    let window_id = triggered.window_id.clone();
                    match review::await_and_notify(&pr_for_thread, &triggered, Duration::from_secs(900)) {
                        Ok(()) => {
                            if let Ok(mut state) = State::load_or_default() {
                                state.mark_reviewed(
                                    &pr_for_thread.owner,
                                    &pr_for_thread.repo,
                                    pr_for_thread.number,
                                    &sha,
                                    &window_id,
                                );
                                if let Err(e) = state.save() {
                                    eprintln!(
                                        "could not persist completion for {}: {e:#}",
                                        pr_for_thread.full_ref()
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "review notification failed for {}: {e:#}",
                                pr_for_thread.full_ref()
                            );
                        }
                    }
                });
            }
            Err(e) => {
                eprintln!("failed to trigger review for {}: {e:#}", pr.full_ref());
            }
        }
    }

    Ok((checked, triggered_count))
}

/// Closes the window and removes the worktree for any tracked PR (reviewing
/// or reviewed - a stuck "reviewing" entry with a dead window is just as
/// worth cleaning up) that's either (a) approved - checked every poll, no
/// grace period, since an approval is a clear enough "done here" signal on
/// its own - or (b) both past `cleanup_after_days` and confirmed
/// merged/closed on GitHub. Still-open, unapproved PRs are left alone no
/// matter how old.
fn cleanup_stale(config: &Config) -> anyhow::Result<()> {
    if config.cleanup_after_days == 0 {
        return Ok(());
    }
    let mut state = State::load_or_default()?;
    let cutoff = chrono::Local::now() - chrono::Duration::days(config.cleanup_after_days.into());

    let mut candidates = Vec::new();
    for (key, entry) in state.entries() {
        let Some((owner, repo, number)) = state::parse_key(key) else {
            continue;
        };
        let past_grace_period = chrono::DateTime::parse_from_rfc3339(&entry.reviewed_at)
            .map(|t| t.with_timezone(&chrono::Local) <= cutoff)
            .unwrap_or(false);
        candidates.push((
            key.clone(),
            PrRef { owner, repo, number },
            entry.window_id.clone(),
            past_grace_period,
        ));
    }

    let mut cleaned = 0;
    for (key, pr, window_id, past_grace_period) in candidates {
        let should_clean = match github::already_approved(&pr) {
            Ok(true) => true,
            Ok(false) if !past_grace_period => false,
            Ok(false) => match github::pr_is_open(&pr) {
                Ok(open) => !open,
                Err(e) => {
                    eprintln!("could not check GitHub state for {}: {e:#}", pr.full_ref());
                    false
                }
            },
            Err(e) => {
                eprintln!("could not check approval state for {}: {e:#}", pr.full_ref());
                false
            }
        };
        if !should_clean {
            continue;
        }

        if let Some(w) = &window_id {
            if let Err(e) = tmux::kill_window(w) {
                eprintln!("could not close window for {}: {e:#}", pr.full_ref());
            }
        }
        let clone_root = config.clone_root();
        let base_repo = git::base_repo_path(&clone_root, &pr.owner, &pr.repo);
        let worktree_path = git::worktree_root(&clone_root, &pr.owner, &pr.repo).join(format!("pr-{}", pr.number));
        if worktree_path.exists() {
            if let Err(e) = git::remove_worktree(&base_repo, &worktree_path) {
                eprintln!("could not remove worktree for {}: {e:#}", pr.full_ref());
            }
        }
        state.remove(&key);
        cleaned += 1;
    }

    if cleaned > 0 {
        state.save()?;
        println!("\u{1f415} cleaned up {cleaned} approved/merged/closed PR(s)");
    }
    Ok(())
}
