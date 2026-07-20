use crate::config::Config;
use anyhow::{anyhow, bail, Context, Result};
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

/// `updated:>=<cutoff>` restricts the search to PRs with recent activity
/// (new commits, comments, or the review request itself) rather than every
/// open PR the user has ever been asked to review.
fn build_search_query(lookback_days: u32) -> String {
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(lookback_days.into()))
        .format("%Y-%m-%dT%H:%M:%SZ");
    format!("q=is:pr is:open review-requested:@me updated:>={cutoff}")
}

/// Lists open PRs where the authenticated user is a requested reviewer,
/// updated within `config.lookback_days`, filtered down to the repos in the
/// config allowlist.
pub fn list_review_requested(config: &Config) -> Result<Vec<PrRef>> {
    let query = build_search_query(config.lookback_days);
    let raw = run_gh(&["api", "-X", "GET", "search/issues", "-f", &query])?;
    let parsed: SearchResponse =
        serde_json::from_str(&raw).context("parsing gh search/issues response")?;

    let mut prs = Vec::new();
    for item in parsed.items {
        let (owner, repo) = owner_repo_from_repository_url(&item.repository_url)?;
        if config.contains_repo(&owner, &repo) {
            prs.push(PrRef {
                owner,
                repo,
                number: item.number,
            });
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
