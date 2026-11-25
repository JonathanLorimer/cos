use std::time::Duration;

use anyhow::Result;

use crate::agent::{self, AnthropicClient};
use crate::db::{self, Db};
use crate::util;
use crate::{agent_bulk_classify, agent_extraction, classify, extraction, github, linear, slack};

// Stage 1: fast, cheap, frequent. Pulls only what's new since each source's
// last_synced_at cursor and runs preliminary categorization (Sonnet, or the
// heuristic fallback) on it. Does NOT touch initiative assignment — see
// run_bulk_classification.
//
// Slack, GitHub, and Linear are all live now (slack.rs / github.rs /
// linear.rs). Any one being unconfigured, or erroring mid-fetch, degrades to
// "no new data from that source this cycle" rather than failing the whole
// cycle — an unapproved GitHub token or an absent Slack token are expected,
// ongoing states here, not crashes.
pub async fn run_extraction(db: &Db) -> Result<()> {
    let now = util::now();

    let since_slack = db::get_cursor(db, "slack")?;
    let slack_messages = match slack::SlackClient::from_env() {
        Some(client) => match client.fetch_new_messages().await {
            Ok(msgs) => msgs,
            Err(e) => {
                tracing::warn!("slack: fetch failed, skipping this cycle: {e:#}");
                Vec::new()
            }
        },
        None => {
            tracing::debug!("slack: no SLACK_USER_TOKEN configured, skipping");
            Vec::new()
        }
    };
    let new_messages: Vec<_> = slack_messages.into_iter().filter(|m| m.ts > since_slack).collect();

    let since_linear = db::get_cursor(db, "linear")?;
    let linear_tickets = match linear::LinearClient::from_env() {
        Some(client) => match client.fetch_tickets().await {
            Ok(tickets) => tickets,
            Err(e) => {
                tracing::warn!("linear: fetch failed, skipping this cycle: {e:#}");
                Vec::new()
            }
        },
        None => {
            tracing::debug!("linear: no LINEAR_API_KEY configured, skipping");
            Vec::new()
        }
    };
    let new_tickets: Vec<_> = linear_tickets
        .into_iter()
        .filter(|n| n.updated_at > since_linear)
        .collect();

    match AnthropicClient::from_env(agent::SONNET, "low") {
        Some(client) => {
            agent_extraction::run_messages(db, &client, &new_messages, &now).await?;
        }
        None => {
            extraction::run_messages(db, &new_messages, &now)?;
        }
    }
    extraction::insert_linear_tickets(db, &new_tickets, &now)?;

    // Status sync isn't an event feed — it's "what's the current state of
    // links we already track," so it always runs in full, cursor or not.
    sync_github_statuses(db, &now).await;

    db::set_cursor(db, "slack", &now)?;
    db::set_cursor(db, "linear", &now)?;

    if new_messages.is_empty() && new_tickets.is_empty() {
        tracing::debug!("extraction: nothing new since last cycle");
    } else {
        tracing::info!(
            "extraction: {} new slack message(s), {} linear update(s)",
            new_messages.len(),
            new_tickets.len()
        );
    }

    Ok(())
}

async fn sync_github_statuses(db: &Db, now: &str) {
    let Some(client) = github::GithubClient::from_env() else {
        tracing::debug!("github: no GITHUB_TOKEN configured, skipping status sync");
        return;
    };

    let links = match db::list_github_links(db) {
        Ok(links) => links,
        Err(e) => {
            tracing::warn!("github: couldn't list tracked links: {e:#}");
            return;
        }
    };

    for link in links {
        // `resolved` is cos's own sticky terminal marker, not a GitHub
        // state — no need to keep spending API calls checking it.
        if link.status == "resolved" {
            continue;
        }
        let Some((owner, repo, number)) = github::GithubClient::parse_pr_url(&link.url) else {
            continue;
        };
        match client.pr_status(&owner, &repo, number).await {
            Ok(Some(status)) => {
                if let Err(e) = db::sync_github_status(db, &link.url, &status, now) {
                    tracing::warn!("github: failed writing status for {}: {e:#}", link.url);
                }
            }
            Ok(None) => {} // not accessible yet (pending approval, 404, etc.) — leave as-is
            Err(e) => tracing::warn!("github: status check failed for {}: {e:#}", link.url),
        }
    }
}

// Stage 2: slower, more expensive, infrequent ("after the fact"). Looks at
// everything still unassigned across all four tables in one shot (Opus, or
// the heuristic fallback) so related items get grouped consistently with
// each other, not just against initiatives that existed before this batch.
pub async fn run_bulk_classification(db: &Db) -> Result<()> {
    let now = util::now();
    match AnthropicClient::from_env(agent::OPUS, "high") {
        Some(client) => agent_bulk_classify::run(db, &client, &now).await?,
        None => classify::run(db, &now)?,
    }
    Ok(())
}

// One scheduled cycle: always extract, then only pay for the expensive bulk
// pass once pending items reach a critical mass — avoids running Opus on a
// timer when there's nothing worth batching yet.
pub async fn run_cycle(db: &Db, bulk_threshold: usize) -> Result<()> {
    run_extraction(db).await?;

    let pending = db::count_pending(db)?;
    if pending >= bulk_threshold {
        tracing::info!("{pending} item(s) pending (>= threshold {bulk_threshold}), running bulk classification");
        run_bulk_classification(db).await?;
    } else {
        tracing::debug!("{pending} item(s) pending (< threshold {bulk_threshold}), skipping bulk classification");
    }

    Ok(())
}

pub fn spawn_cycle_loop(db: Db, interval: Duration, bulk_threshold: usize) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            if let Err(e) = run_cycle(&db, bulk_threshold).await {
                tracing::error!("poll cycle failed: {e:#}");
            }
        }
    });
}
