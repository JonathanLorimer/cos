use std::time::Duration;

use anyhow::Result;

use crate::agent::{self, AnthropicClient};
use crate::db::{self, Db};
use crate::util;
use crate::{agent_bulk_classify, agent_extraction, classify, extraction, mock};

// Stage 1: fast, cheap, frequent. Pulls only what's new since each source's
// last_synced_at cursor and runs preliminary categorization (Sonnet, or the
// heuristic fallback) on it. Does NOT touch initiative assignment — see
// run_bulk_classification.
pub async fn run_extraction(db: &Db) -> Result<()> {
    let now = util::now();

    let since_slack = db::get_cursor(db, "slack")?;
    let new_messages: Vec<_> = mock::slack_messages()
        .into_iter()
        .filter(|m| m.ts > since_slack)
        .collect();

    let since_linear = db::get_cursor(db, "linear")?;
    let new_tickets: Vec<_> = mock::linear_notifications()
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
    for s in mock::github_statuses() {
        db::sync_github_status(db, &s.url, &s.status, &now)?;
    }

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
