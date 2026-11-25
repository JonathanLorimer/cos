use std::collections::HashMap;

use anyhow::{Result, bail};
use serde_json::{Value, json};

const API_URL: &str = "https://api.linear.app/graphql";

// Field names deliberately match the old mock::LinearNotificationFixture
// shape — a drop-in replacement, same pattern as slack.rs.
#[derive(Debug, Clone)]
pub struct Ticket {
    pub identifier: String,
    pub title: String,
    pub state: String,
    pub kind: String,
    pub linked_github_url: Option<String>,
    pub updated_at: String,
}

const QUERY: &str = "{
    viewer {
        assignedIssues(first: 50) { nodes { identifier title state { name } updatedAt attachments(first: 10) { nodes { url sourceType } } } }
        createdIssues(first: 50) { nodes { identifier title state { name } updatedAt attachments(first: 10) { nodes { url sourceType } } } }
    }
    notifications(first: 50) {
        nodes {
            type
            ... on IssueNotification {
                issue { identifier title state { name } updatedAt attachments(first: 10) { nodes { url sourceType } } }
            }
        }
    }
}";

pub struct LinearClient {
    api_key: String,
    http: reqwest::Client,
}

impl LinearClient {
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("LINEAR_API_KEY").ok()?;
        if api_key.trim().is_empty() {
            return None;
        }
        Some(Self {
            api_key,
            http: reqwest::Client::new(),
        })
    }

    async fn query(&self, query: &str) -> Result<Value> {
        let resp = self
            .http
            .post(API_URL)
            .header("Content-Type", "application/json")
            // Linear personal API keys go in Authorization as-is — no
            // "Bearer" prefix (verified live; OAuth app tokens would use
            // Bearer, but that's a different credential type).
            .header("Authorization", &self.api_key)
            .json(&json!({ "query": query }))
            .send()
            .await?;
        let body: Value = resp.json().await?;
        if let Some(errors) = body.get("errors") {
            bail!("linear graphql error: {errors}");
        }
        Ok(body)
    }

    fn github_url(attachments: &Value) -> Option<String> {
        attachments
            .get("nodes")?
            .as_array()?
            .iter()
            .find(|a| a.get("sourceType").and_then(|v| v.as_str()) == Some("github"))
            .and_then(|a| a.get("url"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    fn ticket_from_issue(issue: &Value, kind: &str) -> Option<Ticket> {
        Some(Ticket {
            identifier: issue.get("identifier")?.as_str()?.to_string(),
            title: issue.get("title")?.as_str()?.to_string(),
            state: issue.pointer("/state/name")?.as_str()?.to_string(),
            kind: kind.to_string(),
            linked_github_url: issue.get("attachments").and_then(Self::github_url),
            updated_at: issue.get("updatedAt")?.as_str()?.to_string(),
        })
    }

    fn nodes<'a>(body: &'a Value, pointer: &str) -> Vec<&'a Value> {
        body.pointer(pointer)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    }

    // One combined query -> assigned, created, and notification-surfaced
    // issues, deduped by identifier. Precedence on conflict: assigned >
    // created > notification-derived kind, since "it's assigned to me" is
    // the strongest, most specific signal.
    pub async fn fetch_tickets(&self) -> Result<Vec<Ticket>> {
        let body = self.query(QUERY).await?;
        let mut by_id: HashMap<String, Ticket> = HashMap::new();

        for issue in Self::nodes(&body, "/data/viewer/assignedIssues/nodes") {
            if let Some(t) = Self::ticket_from_issue(issue, "assigned") {
                by_id.insert(t.identifier.clone(), t);
            }
        }
        for issue in Self::nodes(&body, "/data/viewer/createdIssues/nodes") {
            if let Some(t) = Self::ticket_from_issue(issue, "created") {
                by_id.entry(t.identifier.clone()).or_insert(t);
            }
        }
        for notif in Self::nodes(&body, "/data/notifications/nodes") {
            let Some(issue) = notif.get("issue") else {
                continue;
            };
            let notif_type = notif.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let kind = if notif_type.to_lowercase().contains("mention") {
                "mentioned"
            } else {
                "subscribed_update"
            };
            if let Some(t) = Self::ticket_from_issue(issue, kind) {
                by_id.entry(t.identifier.clone()).or_insert(t);
            }
        }

        Ok(by_id.into_values().collect())
    }
}
