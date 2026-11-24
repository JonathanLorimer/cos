use axum::Json;
use serde::Serialize;

// Fixtures standing in for what the real pollers would fetch from
// Slack search.messages/reactions.list, Linear viewer.notifications, and a
// GitHub status-sync pass. Shaped like the subset of each API's response we
// actually need, so swapping the mock for a live call only touches
// `extraction.rs`, not the schema or the UI.

#[derive(Debug, Clone, Serialize)]
pub struct SlackMessageFixture {
    pub permalink: String,
    pub channel: String,
    pub from_user: String,
    pub text: String,
    pub mentions_me: bool,
    pub reacted_by_me: bool,
    pub ts: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinearNotificationFixture {
    pub identifier: String,
    pub title: String,
    pub state: String,
    pub kind: String,
    pub linked_github_url: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GithubStatusFixture {
    pub url: String,
    pub repo: String,
    pub status: String,
}

pub fn slack_messages() -> Vec<SlackMessageFixture> {
    vec![
        SlackMessageFixture {
            permalink: "https://mercury.slack.com/archives/C01/p1001".into(),
            channel: "#payments-eng".into(),
            from_user: "jonathan".into(),
            text: "Opened https://github.com/mercury-eng/ledger/pull/482 for the dedupe fix, I'll get it reviewed by EOD Friday".into(),
            mentions_me: false,
            reacted_by_me: false,
            ts: "2026-11-20T15:04:00Z".into(),
        },
        SlackMessageFixture {
            permalink: "https://mercury.slack.com/archives/C02/p1002".into(),
            channel: "#platform".into(),
            from_user: "priya".into(),
            text: "@jonathan can you take a look at https://github.com/mercury-eng/infra/pull/119 before we cut the release?".into(),
            mentions_me: true,
            reacted_by_me: false,
            ts: "2026-11-21T09:12:00Z".into(),
        },
        SlackMessageFixture {
            permalink: "https://mercury.slack.com/archives/C02/p1003".into(),
            channel: "#platform".into(),
            from_user: "jonathan".into(),
            text: "👀 reacted to thread".into(),
            mentions_me: false,
            reacted_by_me: true,
            ts: "2026-11-21T09:20:00Z".into(),
        },
        SlackMessageFixture {
            permalink: "https://mercury.slack.com/archives/C03/p1004".into(),
            channel: "#incident-response".into(),
            from_user: "dmitri".into(),
            text: "jonathan, when you get a sec can you review the rollback plan? no rush".into(),
            mentions_me: true,
            reacted_by_me: false,
            ts: "2026-11-19T18:45:00Z".into(),
        },
        SlackMessageFixture {
            permalink: "https://mercury.slack.com/archives/C01/p1005".into(),
            channel: "#payments-eng".into(),
            from_user: "jonathan".into(),
            text: "I will write up the postmortem for the webhook outage by Monday".into(),
            mentions_me: false,
            reacted_by_me: false,
            ts: "2026-11-22T11:00:00Z".into(),
        },
        SlackMessageFixture {
            permalink: "https://mercury.slack.com/archives/C04/p1006".into(),
            channel: "#api-gateway".into(),
            from_user: "jonathan".into(),
            text: "Shipped https://github.com/mercury-eng/gateway/pull/301, merged and deployed".into(),
            mentions_me: false,
            reacted_by_me: false,
            ts: "2026-11-18T14:30:00Z".into(),
        },
        SlackMessageFixture {
            permalink: "https://mercury.slack.com/archives/C05/p1007".into(),
            channel: "#design-review".into(),
            from_user: "alex".into(),
            text: "cc @jonathan, thoughts on the new onboarding flow doc?".into(),
            mentions_me: true,
            reacted_by_me: false,
            ts: "2026-11-23T10:05:00Z".into(),
        },
    ]
}

pub fn linear_notifications() -> Vec<LinearNotificationFixture> {
    vec![
        LinearNotificationFixture {
            identifier: "ENG-482".into(),
            title: "Ledger entries double-counted on retry".into(),
            state: "In Review".into(),
            kind: "assigned".into(),
            linked_github_url: Some("https://github.com/mercury-eng/ledger/pull/482".into()),
            updated_at: "2026-11-20T15:10:00Z".into(),
        },
        LinearNotificationFixture {
            identifier: "ENG-119".into(),
            title: "Infra: bump terraform provider before release cut".into(),
            state: "In Progress".into(),
            kind: "mentioned".into(),
            linked_github_url: Some("https://github.com/mercury-eng/infra/pull/119".into()),
            updated_at: "2026-11-21T09:15:00Z".into(),
        },
        LinearNotificationFixture {
            identifier: "ENG-510".into(),
            title: "Write webhook outage postmortem".into(),
            state: "Todo".into(),
            kind: "created".into(),
            linked_github_url: None,
            updated_at: "2026-11-22T11:05:00Z".into(),
        },
        LinearNotificationFixture {
            identifier: "ENG-301".into(),
            title: "Gateway: raise rate limit for internal callers".into(),
            state: "Done".into(),
            kind: "assigned".into(),
            linked_github_url: Some("https://github.com/mercury-eng/gateway/pull/301".into()),
            updated_at: "2026-11-18T14:35:00Z".into(),
        },
        LinearNotificationFixture {
            identifier: "ENG-528".into(),
            title: "Onboarding flow doc feedback requested".into(),
            state: "Todo".into(),
            kind: "subscribed_update".into(),
            linked_github_url: None,
            updated_at: "2026-11-23T10:06:00Z".into(),
        },
    ]
}

pub fn github_statuses() -> Vec<GithubStatusFixture> {
    vec![
        GithubStatusFixture {
            url: "https://github.com/mercury-eng/ledger/pull/482".into(),
            repo: "mercury-eng/ledger".into(),
            status: "changes_requested".into(),
        },
        GithubStatusFixture {
            url: "https://github.com/mercury-eng/infra/pull/119".into(),
            repo: "mercury-eng/infra".into(),
            status: "open".into(),
        },
        GithubStatusFixture {
            url: "https://github.com/mercury-eng/gateway/pull/301".into(),
            repo: "mercury-eng/gateway".into(),
            status: "merged".into(),
        },
    ]
}

pub async fn mock_slack() -> Json<Vec<SlackMessageFixture>> {
    Json(slack_messages())
}

pub async fn mock_linear() -> Json<Vec<LinearNotificationFixture>> {
    Json(linear_notifications())
}

pub async fn mock_github() -> Json<Vec<GithubStatusFixture>> {
    Json(github_statuses())
}
