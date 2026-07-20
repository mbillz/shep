use crate::config::Config;
use crate::github;
use crate::paths;
use crate::review;
use crate::state::{ReviewCheck, State};
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
            let alive = std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .status()
                .map(|s| s.success())
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
                    match review::await_and_notify(&pr_for_thread, &triggered, Duration::from_secs(900)) {
                        Ok(()) => {
                            if let Ok(mut state) = State::load_or_default() {
                                state.mark_reviewed(
                                    &pr_for_thread.owner,
                                    &pr_for_thread.repo,
                                    pr_for_thread.number,
                                    &sha,
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
