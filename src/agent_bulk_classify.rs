use anyhow::Result;
use serde_json::json;

use crate::agent::AnthropicClient;
use crate::db::{self, Db};

// Bulk, infrequent, expensive-model pass: looks at every currently
// unassigned item across all four tables in a SINGLE call, so it can group
// related items consistently with each other — not just against whatever
// initiatives already existed before this batch arrived. This is the "after
// the fact" counterpart to agent_extraction's per-message triage.
const SYSTEM: &str = "\
You are the bulk initiative-assessment stage of a chief-of-staff tool. You are given every currently \
unclassified item (GitHub PRs, commitments, Slack mentions, Linear tickets) in one batch, plus the list \
of initiatives that already exist. For each listed item, call assign_initiative_item exactly once. \
Group related items together under the same initiative — including items within THIS batch that \
clearly belong together, even if no existing initiative covers them yet. Reuse an existing initiative \
name EXACTLY when it fits; otherwise invent a short new name (2-4 words) with a one-sentence \
description. Prefer consolidating closely related work under one initiative over creating \
near-duplicates.";

struct UnassignedItem {
    item_ref: String,
    description: String,
}

fn tools() -> serde_json::Value {
    json!([{
        "name": "assign_initiative_item",
        "description": "Assign one of the listed items to an initiative, existing or new.",
        "input_schema": {
            "type": "object",
            "properties": {
                "item_ref": {"type": "string", "description": "copy exactly from the item list, e.g. 'github_link:3'"},
                "initiative_name": {"type": "string"},
                "initiative_description": {"type": "string"}
            },
            "required": ["item_ref", "initiative_name"]
        }
    }])
}

fn initiatives_context(db: &Db) -> Result<String> {
    let initiatives = db::list_initiatives(db)?;
    if initiatives.is_empty() {
        return Ok("(none yet)".to_string());
    }
    Ok(initiatives
        .iter()
        .map(|i| format!("- {}: {}", i.name, i.description))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn gather_unassigned(db: &Db) -> Result<Vec<UnassignedItem>> {
    let mut items = Vec::new();

    for l in db::list_github_links(db)? {
        if l.initiative_id.is_none() {
            items.push(UnassignedItem {
                item_ref: format!("github_link:{}", l.id),
                description: format!(
                    "GitHub PR in {}: {} (relationship: {})",
                    l.repo, l.context, l.relationship
                ),
            });
        }
    }
    for c in db::list_commitments(db)? {
        if c.initiative_id.is_none() {
            items.push(UnassignedItem {
                item_ref: format!("commitment:{}", c.id),
                description: format!("Commitment: {} (due {})", c.text, c.due_date),
            });
        }
    }
    for m in db::list_mentions(db)? {
        if m.initiative_id.is_none() {
            items.push(UnassignedItem {
                item_ref: format!("mention:{}", m.id),
                description: format!("Slack mention from {}: {}", m.from_user, m.text),
            });
        }
    }
    for t in db::list_linear_tickets(db)? {
        if t.initiative_id.is_none() {
            items.push(UnassignedItem {
                item_ref: format!("linear_ticket:{}", t.identifier),
                description: format!("Linear ticket {}: {} (state: {})", t.identifier, t.title, t.state),
            });
        }
    }

    Ok(items)
}

fn apply_assignment(db: &Db, item_ref: &str, initiative_id: i64) -> Result<()> {
    let Some((kind, id)) = item_ref.split_once(':') else {
        return Ok(());
    };
    match kind {
        "github_link" => {
            if let Ok(id) = id.parse() {
                db::assign_github_link(db, id, Some(initiative_id))?;
            }
        }
        "commitment" => {
            if let Ok(id) = id.parse() {
                db::assign_commitment(db, id, Some(initiative_id))?;
            }
        }
        "mention" => {
            if let Ok(id) = id.parse() {
                db::assign_mention(db, id, Some(initiative_id))?;
            }
        }
        "linear_ticket" => {
            db::assign_linear_ticket(db, id, Some(initiative_id))?;
        }
        _ => {}
    }
    Ok(())
}

pub async fn run(db: &Db, client: &AnthropicClient, now: &str) -> Result<()> {
    let items = gather_unassigned(db)?;
    if items.is_empty() {
        tracing::debug!("bulk classify: nothing unassigned, skipping call");
        return Ok(());
    }

    let initiatives = initiatives_context(db)?;
    let item_list = items
        .iter()
        .map(|i| format!("- {}: {}", i.item_ref, i.description))
        .collect::<Vec<_>>()
        .join("\n");
    let system = format!("{SYSTEM}\n\nExisting initiatives:\n{initiatives}");
    let user_content = format!("Unclassified items:\n{item_list}");
    let max_tokens = ((items.len() as u32) * 150 + 1024).min(8192);

    let calls = client
        .call_tools(&system, &user_content, tools(), max_tokens)
        .await?;

    let mut assigned = 0;
    for (name, input) in calls {
        if name != "assign_initiative_item" {
            continue;
        }
        let item_ref = input.get("item_ref").and_then(|v| v.as_str()).unwrap_or_default();
        if item_ref.is_empty() {
            continue;
        }
        let initiative_name = input
            .get("initiative_name")
            .and_then(|v| v.as_str())
            .unwrap_or("Unsorted");
        let initiative_description = input
            .get("initiative_description")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let initiative_id = db::get_or_create_initiative(db, initiative_name, initiative_description, now)?;
        apply_assignment(db, item_ref, initiative_id)?;
        assigned += 1;
    }

    tracing::info!(
        "bulk classify: {assigned}/{} item(s) assigned in one call",
        items.len()
    );

    Ok(())
}
