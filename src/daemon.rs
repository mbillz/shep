use crate::config::Config;
use crate::github;
use crate::paths;
use crate::review;
use crate::state::State;
use crate::tmux;
use std::time::Duration;

/// Polls allowlisted repos for review requests every `poll_interval_secs`;
/// each triggered review is watched for completion on its own thread so a
/// slow one doesn't stall the next poll.
pub fn run(config: &Config) -> anyhow::Result<()> {
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
                println!("[{now}] checked {checked} PR(s), nothing new")
            }
            Ok((checked, triggered)) => {
                println!("[{now}] checked {checked} PR(s), triggered {triggered} review(s)")
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
        let details = match github::pr_view(&pr) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("skipping {}: could not fetch PR details: {e:#}", pr.full_ref());
                continue;
            }
        };
        if !state.needs_review(&pr.owner, &pr.repo, pr.number, &details.head_sha) {
            continue;
        }

        println!("triggering review for {}", pr.full_ref());
        match review::trigger_review(config, &pr, details) {
            Ok(triggered) => {
                state.mark_reviewed(&pr.owner, &pr.repo, pr.number, &triggered.details.head_sha);
                state.save()?;
                triggered_count += 1;

                let pr_for_thread = pr.clone();
                std::thread::spawn(move || {
                    if let Err(e) =
                        review::await_and_notify(&pr_for_thread, &triggered, Duration::from_secs(900))
                    {
                        eprintln!(
                            "review notification failed for {}: {e:#}",
                            pr_for_thread.full_ref()
                        );
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
