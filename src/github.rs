use crate::config::Config;
use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct PrRef {
    pub owner: String,
    pub repo: String,
    pub number: u64,
}

impl PrRef {
    pub fn full_ref(&self) -> String {
        format!("{}/{}#{}", self.owner, self.repo, self.number)
    }

    pub fn url(&self) -> String {
        format!(
            "https://github.com/{}/{}/pull/{}",
            self.owner, self.repo, self.number
        )
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    items: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
struct SearchItem {
    number: u64,
    repository_url: String,
}

fn owner_repo_from_repository_url(repository_url: &str) -> Result<(String, String)> {
    // e.g. "https://api.github.com/repos/owner/repo"
    let mut parts = repository_url.rsplit('/');
    let repo = parts
        .next()
        .ok_or_else(|| anyhow!("malformed repository_url: {repository_url}"))?;
    let owner = parts
        .next()
        .ok_or_else(|| anyhow!("malformed repository_url: {repository_url}"))?;
    Ok((owner.to_string(), repo.to_string()))
}

fn run_gh(args: &[&str]) -> Result<String> {
    let output = Command::new("gh")
        .args(args)
        .output()
        .with_context(|| format!("failed to run `gh {}`", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "`gh {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// `updated:>=<cutoff>` here is a cheap pre-filter, not the real check (see
/// `review_requested_at`) - it just bounds how many PRs need the more
/// expensive per-PR timeline lookup. Safe to use for that: adding a reviewer
/// always bumps a PR's `updated_at` to at least the request time (verified
/// against real data), so this can never exclude a PR that was genuinely
/// tagged within the window, only include some extra ones that get filtered
/// out precisely afterward.
fn build_search_query(lookback_days: u32) -> String {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(lookback_days.into()))
        .format("%Y-%m-%dT%H:%M:%SZ");
    format!("q=is:pr is:open review-requested:@me updated:>={cutoff}")
}

pub fn current_user() -> Result<String> {
    let raw = run_gh(&["api", "user", "--jq", ".login"])?;
    Ok(raw.trim().to_string())
}

/// The most recent time `username` was requested as a reviewer on this PR,
/// via the issue timeline API - the precise signal for "when was I tagged,"
/// as opposed to `updated_at` which also moves on unrelated activity like
/// new commits or comments. `username` is safe to interpolate directly into
/// the jq filter: GitHub usernames are restricted to alphanumerics/hyphens,
/// so no quoting/injection concern.
fn review_requested_at(pr: &PrRef, username: &str) -> Result<Option<DateTime<Utc>>> {
    let raw = run_gh(&[
        "api",
        &format!(
            "repos/{}/{}/issues/{}/timeline",
            pr.owner, pr.repo, pr.number
        ),
        "--paginate",
        "--jq",
        &format!(
            r#".[] | select(.event == "review_requested" and .requested_reviewer.login == "{username}") | .created_at"#
        ),
    ])?;
    Ok(latest_timestamp(&raw))
}

fn latest_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    raw.lines()
        .filter_map(|line| DateTime::parse_from_rfc3339(line.trim()).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .max()
}

/// Lists open PRs where the authenticated user was requested as a reviewer
/// within `config.lookback_days` - checked precisely per PR via
/// `review_requested_at`, not just "the PR had some recent activity" -
/// filtered down to the repos in the config allowlist.
pub fn list_review_requested(config: &Config) -> Result<Vec<PrRef>> {
    let username = current_user()?;
    let cutoff = Utc::now() - chrono::Duration::days(config.lookback_days.into());

    let query = build_search_query(config.lookback_days);
    let raw = run_gh(&["api", "-X", "GET", "search/issues", "-f", &query])?;
    let parsed: SearchResponse =
        serde_json::from_str(&raw).context("parsing gh search/issues response")?;

    let mut prs = Vec::new();
    for item in parsed.items {
        let (owner, repo) = owner_repo_from_repository_url(&item.repository_url)?;
        if !config.contains_repo(&owner, &repo) {
            continue;
        }
        let pr = PrRef {
            owner,
            repo,
            number: item.number,
        };
        match review_requested_at(&pr, &username) {
            // Couldn't pin down when (e.g. a team-based review request
            // rather than a direct one) - include it rather than risk
            // silently hiding something the user was genuinely tagged on.
            Ok(None) => prs.push(pr),
            Ok(Some(requested_at)) if requested_at >= cutoff => prs.push(pr),
            Ok(Some(_)) => {}
            Err(e) => eprintln!(
                "skipping {}: could not check review-request time: {e:#}",
                pr.full_ref()
            ),
        }
    }
    Ok(prs)
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrDetails {
    pub title: String,
    #[serde(rename = "headRefOid")]
    pub head_sha: String,
}

pub fn pr_view(pr: &PrRef) -> Result<PrDetails> {
    let raw = run_gh(&["pr", "view", &pr.url(), "--json", "title,headRefOid"])?;
    serde_json::from_str(&raw).context("parsing gh pr view response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_owner_repo_from_repository_url() {
        let (owner, repo) =
            owner_repo_from_repository_url("https://api.github.com/repos/magicaltome/lightfield")
                .unwrap();
        assert_eq!(owner, "magicaltome");
        assert_eq!(repo, "lightfield");
    }

    #[test]
    fn search_query_includes_updated_cutoff() {
        let query = build_search_query(1);
        assert!(query.contains("is:pr is:open review-requested:@me"));
        assert!(query.contains("updated:>="));
    }

    #[test]
    fn latest_timestamp_picks_the_max_across_lines() {
        // `gh api --jq` outputs raw (unquoted) strings, like `jq -r` -
        // verified live against a real timeline response.
        let raw = "2026-07-15T18:57:32Z\n2026-07-10T21:04:46Z\n";
        let latest = latest_timestamp(raw).unwrap();
        assert_eq!(latest.to_rfc3339(), "2026-07-15T18:57:32+00:00");
    }

    #[test]
    fn latest_timestamp_none_when_empty() {
        assert!(latest_timestamp("").is_none());
    }

    #[test]
    fn builds_pr_url() {
        let pr = PrRef {
            owner: "acme".to_string(),
            repo: "widgets".to_string(),
            number: 42,
        };
        assert_eq!(pr.url(), "https://github.com/acme/widgets/pull/42");
        assert_eq!(pr.full_ref(), "acme/widgets#42");
    }
}
