use std::collections::HashMap;

use anyhow::{Result, bail};
use serde_json::Value;

const API_BASE: &str = "https://slack.com/api";

// Fields deliberately match the old mock::SlackMessageFixture shape — a
// drop-in replacement, so extraction.rs / agent_extraction.rs needed zero
// logic changes, only a different data source.
#[derive(Debug, Clone)]
pub struct Message {
    pub permalink: String,
    pub channel: String,
    pub from_user: String,
    pub text: String,
    pub mentions_me: bool,
    pub reacted_by_me: bool,
    pub ts: String,
}

// "jonathan" is a sentinel, not a literal Slack handle — extraction.rs's `ME`
// const and agent_extraction's system prompt both compare against this exact
// string, so normalizing the authenticated user's real ID to this value here
// keeps that logic working unchanged regardless of what the real Slack
// display name/handle actually is.
const ME: &str = "jonathan";

pub struct SlackClient {
    token: String,
    http: reqwest::Client,
}

impl SlackClient {
    pub fn from_env() -> Option<Self> {
        let token = std::env::var("SLACK_USER_TOKEN").ok()?;
        if token.trim().is_empty() {
            return None;
        }
        Some(Self {
            token,
            http: reqwest::Client::new(),
        })
    }

    async fn call(&self, method: &str, params: &[(&str, &str)]) -> Result<Value> {
        let resp = self
            .http
            .get(format!("{API_BASE}/{method}"))
            .header("Authorization", format!("Bearer {}", self.token))
            .query(params)
            .send()
            .await?;
        let body: Value = resp.json().await?;
        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let err = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown_error");
            bail!("{method} -> {err}");
        }
        Ok(body)
    }

    // (user_id, workspace base url e.g. "https://myteam.slack.com") — needed
    // to build from:/mention search queries and to construct permalinks for
    // reactions.list items, which (unlike search.messages) don't include one.
    async fn whoami(&self) -> Result<(String, String)> {
        let body = self.call("auth.test", &[]).await?;
        let user_id = body
            .get("user_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let team_url = body
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim_end_matches('/')
            .to_string();
        Ok((user_id, team_url))
    }

    fn to_rfc3339(slack_ts: &str) -> String {
        let secs: i64 = slack_ts.split('.').next().unwrap_or("0").parse().unwrap_or(0);
        chrono::DateTime::from_timestamp(secs, 0)
            .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_else(|| slack_ts.to_string())
    }

    fn permalink(team_url: &str, channel: &str, slack_ts: &str) -> String {
        format!("{team_url}/archives/{channel}/p{}", slack_ts.replace('.', ""))
    }

    fn normalize_user(raw: &str, my_user_id: &str) -> String {
        if raw == my_user_id { ME.to_string() } else { raw.to_string() }
    }

    // NOTE: unverified against a live token (none exists in this
    // environment yet — see ARCHITECTURE.md). search.messages and
    // reactions.list are called per their documented shapes; the one
    // judgment call with no official "find things mentioning me" modifier is
    // searching literal `<@USER_ID>` as query text. Worth a real smoke test
    // the moment a working SLACK_USER_TOKEN exists.
    async fn search(&self, query: &str, my_user_id: &str, mentions_me: bool) -> Result<Vec<Message>> {
        let body = self
            .call(
                "search.messages",
                &[("query", query), ("sort", "timestamp"), ("sort_dir", "desc"), ("count", "100")],
            )
            .await?;
        let matches = body
            .pointer("/messages/matches")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        Ok(matches
            .iter()
            .map(|m| {
                let ts_raw = m.get("ts").and_then(|v| v.as_str()).unwrap_or_default();
                let raw_user = m
                    .get("user")
                    .and_then(|v| v.as_str())
                    .or_else(|| m.get("username").and_then(|v| v.as_str()))
                    .unwrap_or("unknown");
                Message {
                    permalink: m.get("permalink").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                    channel: m
                        .pointer("/channel/name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    from_user: Self::normalize_user(raw_user, my_user_id),
                    text: m.get("text").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                    mentions_me,
                    reacted_by_me: false,
                    ts: Self::to_rfc3339(ts_raw),
                }
            })
            .collect())
    }

    async fn reactions(&self, my_user_id: &str, team_url: &str) -> Result<Vec<Message>> {
        let body = self.call("reactions.list", &[("count", "100")]).await?;
        let items = body.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();

        Ok(items
            .iter()
            .filter(|item| item.get("type").and_then(|v| v.as_str()) == Some("message"))
            .map(|item| {
                let channel = item.get("channel").and_then(|v| v.as_str()).unwrap_or_default();
                let msg = item.get("message").cloned().unwrap_or_default();
                let ts_raw = msg.get("ts").and_then(|v| v.as_str()).unwrap_or_default();
                let raw_user = msg.get("user").and_then(|v| v.as_str()).unwrap_or("unknown");
                Message {
                    permalink: Self::permalink(team_url, channel, ts_raw),
                    channel: channel.to_string(),
                    from_user: Self::normalize_user(raw_user, my_user_id),
                    text: msg.get("text").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                    mentions_me: false,
                    reacted_by_me: true,
                    ts: Self::to_rfc3339(ts_raw),
                }
            })
            .collect())
    }

    // Partial failures (e.g. a missing scope on one endpoint) don't abort
    // the whole cycle — whatever succeeds is still usable. Only whoami()
    // failing is fatal, since nothing else can be built without it.
    pub async fn fetch_new_messages(&self) -> Result<Vec<Message>> {
        let (user_id, team_url) = self.whoami().await?;
        let mut by_permalink: HashMap<String, Message> = HashMap::new();

        match self.search(&format!("from:<@{user_id}>"), &user_id, false).await {
            Ok(msgs) => {
                for m in msgs {
                    by_permalink.entry(m.permalink.clone()).or_insert(m);
                }
            }
            Err(e) => tracing::warn!("slack: from-me search failed: {e:#}"),
        }

        match self.search(&format!("<@{user_id}>"), &user_id, true).await {
            Ok(msgs) => {
                for m in msgs {
                    by_permalink.entry(m.permalink.clone()).or_insert(m);
                }
            }
            Err(e) => tracing::warn!("slack: mentions search failed: {e:#}"),
        }

        match self.reactions(&user_id, &team_url).await {
            Ok(msgs) => {
                for m in msgs {
                    by_permalink.entry(m.permalink.clone()).or_insert(m);
                }
            }
            Err(e) => tracing::warn!("slack: reactions.list failed: {e:#}"),
        }

        Ok(by_permalink.into_values().collect())
    }
}
