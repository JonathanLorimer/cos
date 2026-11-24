use anyhow::Result;
use serde_json::json;

use crate::agent::AnthropicClient;
use crate::db::{self, Db};
use crate::mock::SlackMessageFixture;
use crate::util::repo_from_url;

const MAX_TOKENS: u32 = 1024;

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
a separate pass later.

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

pub async fn run_messages(
    db: &Db,
    client: &AnthropicClient,
    messages: &[SlackMessageFixture],
    now: &str,
) -> Result<()> {
    let tools = tools();

    for msg in messages {
        let user_content = format!(
            "From: {}\nChannel: {}\nMentions jonathan: {}\njonathan reacted: {}\nMessage: {}",
            msg.from_user, msg.channel, msg.mentions_me, msg.reacted_by_me, msg.text
        );

        let calls = client
            .call_tools(SYSTEM, &user_content, tools.clone(), MAX_TOKENS)
            .await?;
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
