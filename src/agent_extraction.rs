use anyhow::Result;
use futures::future::join_all;
use serde_json::json;

use crate::agent::AnthropicClient;
use crate::db::{self, Db};
use crate::slack::Message as SlackMessage;
use crate::util::repo_from_url;

const MAX_TOKENS: u32 = 1024;
// Network round-trips are the bottleneck, not local work — run this many
// triage calls concurrently instead of one message at a time. At real
// volume (hundreds of messages per first backfill) sequential calls took
// ~7 minutes; this is the actual fix, a cheaper model alone doesn't address
// the round-trip count.
const CONCURRENCY: usize = 10;

// Preliminary categorization only — fast/cheap triage per message. Initiative
// assignment is deliberately NOT this stage's job: it happens "after the
// fact" in agent_bulk_classify, which sees every unassigned item at once and
// can group them consistently with each other, not just one at a time
// against whatever initiatives already existed before this message arrived.
const SYSTEM: &str = "\
You are the triage stage of a chief-of-staff tool for Jonathan. You are given one Slack message at a \
time and decide what, if anything, is worth tracking from it. Jonathan's Slack handle is 'jonathan'. \
Call zero or more of the available tools based on the message content — most messages warrant zero or \
one tool call, don't force one. Do not worry about which initiative this belongs to; that's decided in \
a separate pass later. Some messages include surrounding thread context — use it only to understand what \
the target message means (e.g. what a bare 'I'll get to it Friday' is actually about); every tool call \
should still describe the target message's own content, not the thread as a whole.

For log_github_link: relationship is 'authored' if jonathan is the message's author and the PR is his \
own work; 'reviewing' if someone is asking jonathan to look at/review/approve it, or jonathan is \
actively engaging with someone else's PR; 'mentioned' if the PR is referenced only in passing.

For flag_mention: needs_response is true only if the message is a direct question or ask addressed to \
jonathan that expects a reply — not a pure FYI/status update.

For log_commitment: only call this when jonathan is the author and is explicitly committing to do \
something by a certain time — not for messages merely asking him to do something.";

fn tools() -> serde_json::Value {
    json!([
        {
            "name": "log_github_link",
            "description": "Record a GitHub PR/issue link found in the message.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "relationship": {"type": "string", "enum": ["authored", "reviewing", "mentioned"]},
                    "context": {"type": "string", "description": "short note on why this matters"}
                },
                "required": ["url", "relationship", "context"]
            }
        },
        {
            "name": "log_commitment",
            "description": "Record a commitment jonathan made in the message.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "due_date": {"type": "string", "description": "as stated, e.g. 'Friday' or 'EOD Friday'; 'unspecified' if none given"}
                },
                "required": ["text", "due_date"]
            }
        },
        {
            "name": "flag_mention",
            "description": "Flag that this message is an ask/mention directed at jonathan.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "needs_response": {"type": "boolean"}
                },
                "required": ["needs_response"]
            }
        }
    ])
}

fn user_content_for(msg: &SlackMessage) -> String {
    let thread_note = msg
        .thread_context
        .as_ref()
        .map(|c| format!("\n\nOther messages in this thread (for context only — classify the Message above, not these):\n{c}"))
        .unwrap_or_default();
    format!(
        "From: {}\nChannel: {}\nMentions jonathan: {}\njonathan reacted: {}\nMessage: {}{thread_note}",
        msg.from_user, msg.channel, msg.mentions_me, msg.reacted_by_me, msg.text
    )
}

pub async fn run_messages(
    db: &Db,
    client: &AnthropicClient,
    messages: &[SlackMessage],
    now: &str,
) -> Result<()> {
    let tools = tools();

    // Fetch concurrently in bounded chunks (the slow, network-bound part);
    // apply DB writes afterward in original order (fast, local, and keeps
    // upsert/merge semantics deterministic regardless of response arrival
    // order). One message's API failure is logged and skipped, not fatal.
    // join_all over chunks instead of a Stream/buffer_unordered combinator —
    // the latter hits a known rustc HRTB inference failure (fails to prove
    // `Send` through tokio::spawn several frames up) when the mapped closure
    // borrows from a function parameter like `messages` here.
    let indexed: Vec<(usize, &SlackMessage)> = messages.iter().enumerate().collect();
    let mut results: Vec<(usize, Vec<(String, serde_json::Value)>)> = Vec::with_capacity(messages.len());
    for chunk in indexed.chunks(CONCURRENCY) {
        let futs = chunk.iter().map(|&(i, msg)| {
            let tools = tools.clone();
            let user_content = user_content_for(msg);
            async move {
                match client.call_tools(SYSTEM, &user_content, tools, MAX_TOKENS).await {
                    Ok(calls) => (i, calls),
                    Err(e) => {
                        tracing::warn!("agent_extraction: triage call failed for message {i}: {e:#}");
                        (i, Vec::new())
                    }
                }
            }
        });
        results.extend(join_all(futs).await);
    }

    for (i, calls) in results {
        let msg = &messages[i];
        for (name, input) in calls {
            match name.as_str() {
                "log_github_link" => {
                    let url = input.get("url").and_then(|v| v.as_str()).unwrap_or_default();
                    if url.is_empty() {
                        continue;
                    }
                    let relationship = input
                        .get("relationship")
                        .and_then(|v| v.as_str())
                        .unwrap_or("mentioned");
                    let context = input.get("context").and_then(|v| v.as_str()).unwrap_or_default();
                    let repo = repo_from_url(url);
                    db::log_github_link(
                        db,
                        url,
                        &repo,
                        relationship,
                        "open",
                        context,
                        &msg.permalink,
                        now,
                    )?;
                }
                "log_commitment" => {
                    let text = input.get("text").and_then(|v| v.as_str()).unwrap_or_default();
                    let due_date = input
                        .get("due_date")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unspecified");
                    db::log_commitment(db, text, due_date, &msg.permalink, now)?;
                }
                "flag_mention" => {
                    let needs_response = input
                        .get("needs_response")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    db::log_mention(db, &msg.permalink, &msg.from_user, &msg.text, needs_response, now)?;
                }
                _ => {}
            }
        }
    }

    Ok(())
}
