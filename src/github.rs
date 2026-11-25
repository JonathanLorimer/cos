use anyhow::Result;
use serde_json::Value;

const API_BASE: &str = "https://api.github.com";

pub struct GithubClient {
    token: String,
    http: reqwest::Client,
}

impl GithubClient {
    pub fn from_env() -> Option<Self> {
        let token = std::env::var("GITHUB_TOKEN").ok()?;
        if token.trim().is_empty() {
            return None;
        }
        Some(Self {
            token,
            http: reqwest::Client::new(),
        })
    }

    // https://github.com/{owner}/{repo}/pull/{number} -> (owner, repo, number)
    pub fn parse_pr_url(url: &str) -> Option<(String, String, u64)> {
        let rest = url.strip_prefix("https://github.com/")?;
        let mut parts = rest.trim_end_matches('/').splitn(4, '/');
        let owner = parts.next()?;
        let repo = parts.next()?;
        if parts.next()? != "pull" {
            return None;
        }
        let number = parts.next()?.parse().ok()?;
        Some((owner.to_string(), repo.to_string(), number))
    }

    // None means "couldn't determine status this cycle" (404/403/network
    // error) — the caller should leave the existing status alone rather than
    // treat that as a real state. A fine-grained PAT pending org approval
    // will 403/404 on repo-scoped endpoints even though the token itself is
    // valid (confirmed separately against /user) — this is the expected,
    // not-yet-working case today, not a bug to crash on.
    pub async fn pr_status(&self, owner: &str, repo: &str, number: u64) -> Result<Option<String>> {
        let url = format!("{API_BASE}/repos/{owner}/{repo}/pulls/{number}");
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "cos-chief-of-staff")
            .send()
            .await?;

        if !resp.status().is_success() {
            tracing::debug!(
                "github: {owner}/{repo}#{number} -> HTTP {} (not yet accessible?)",
                resp.status()
            );
            return Ok(None);
        }

        let body: Value = resp.json().await?;
        let merged = body.get("merged").and_then(|v| v.as_bool()).unwrap_or(false);
        let state = body.get("state").and_then(|v| v.as_str()).unwrap_or("open");

        let status = if merged {
            "merged"
        } else if state == "closed" {
            "closed"
        } else {
            "open"
        };
        Ok(Some(status.to_string()))
    }
}
