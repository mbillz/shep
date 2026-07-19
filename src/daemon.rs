use crate::config::Config;
use crate::github;
use crate::review;
use crate::state::State;
use std::time::Duration;

/// Foreground poll loop: checks GitHub for review requests on the allowlisted
/// repos every `poll_interval_secs`, triggers a review for anything new or
/// updated since we last saw it, and watches each one for completion on its
/// own background thread so a slow review doesn't stall the next poll.
pub fn run(config: &Config) -> anyhow::Result<()> {
    if config.repos.is_empty() {
        eprintln!(
            "warning: no repos configured in {} - the daemon has nothing to watch",
            Config::path()?.display()
        );
    }

    loop {
        if let Err(e) = poll_once(config) {
            eprintln!("poll failed: {e:#}");
        }
        std::thread::sleep(Duration::from_secs(config.poll_interval_secs));
    }
}

fn poll_once(config: &Config) -> anyhow::Result<()> {
    let mut state = State::load_or_default()?;
    let prs = github::list_review_requested(config)?;

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

    Ok(())
}
