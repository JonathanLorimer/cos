use std::collections::HashSet;

use anyhow::Result;
use serde_json::json;

use crate::agent::AnthropicClient;
use crate::db::{self, Db};
use crate::util::days_ago;

const STANDALONE_WINDOW_DAYS: i64 = 7;

// Bulk, infrequent, expensive-model pass: looks at every item that hasn't
// had an initiative decision yet, in a SINGLE call, so it can group related
// items consistently with each other — not just against whatever initiatives
// already existed before this batch arrived. This is the "after the fact"
// counterpart to agent_extraction's per-message triage.
//
// Not every item gets assigned — the model may legitimately decide something
// is a one-off and leave it standalone (no tool call for it). That decision
// is still recorded (initiative_reviewed_at is stamped either way) so it
// isn't re-litigated every cycle, but recently-reviewed standalone items are
// re-offered as optional context for a short window, in case a new item
// arriving later retroactively reveals they belong together.
const SYSTEM: &str = "\
You are the bulk initiative-assessment stage of a chief-of-staff tool. You are given two lists: items \
that need a decision now, and recently-reviewed items that were previously left standalone. You also \
get the list of initiatives that already exist.

For each item in the 'needs a decision' list, decide whether it belongs to an initiative. Call \
assign_initiative_item for it if so. If it's a genuine one-off — a small, isolated ask or fix with \
nothing else related to it — do NOT call the tool for it; leaving it standalone is a normal, expected \
outcome, not a failure to classify. Do not invent an initiative just to give every item a home.

The 'recently reviewed, still standalone' list is reference only — most of the time you should do \
nothing with it. Only call assign_initiative_item for one of these items if something in THIS batch's \
'needs a decision' list makes it suddenly obvious it's part of the same effort (e.g. a new PR lands that \
ties together two things that looked unrelated on their own). Don't reorganize things that are still \
fine standalone just because you can.

When you do assign an item: reuse an existing initiative name EXACTLY when it fits, otherwise invent a \
short new name (2-4 words) with a one-sentence description. Prefer consolidating closely related work \
under one initiative over creating near-duplicates.";

struct Item {
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
                "item_ref": {"type": "string", "description": "copy exactly from one of the item lists, e.g. 'github_link:3'"},
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

// Items with initiative_reviewed_at IS NULL — never evaluated, must get a
// decision (assign or deliberately leave standalone) this cycle.
fn gather_pending(db: &Db) -> Result<Vec<Item>> {
    let mut items = Vec::new();

    for l in db::list_github_links(db)? {
        if l.initiative_reviewed_at.is_none() {
            items.push(Item {
                item_ref: format!("github_link:{}", l.id),
                description: format!(
                    "GitHub PR in {}: {} (relationship: {})",
                    l.repo, l.context, l.relationship
                ),
            });
        }
    }
    for c in db::list_commitments(db)? {
        if c.initiative_reviewed_at.is_none() {
            items.push(Item {
                item_ref: format!("commitment:{}", c.id),
                description: format!("Commitment: {} (due {})", c.text, c.due_date),
            });
        }
    }
    for m in db::list_mentions(db)? {
        if m.initiative_reviewed_at.is_none() {
            items.push(Item {
                item_ref: format!("mention:{}", m.id),
                description: format!("Slack mention from {}: {}", m.from_user, m.text),
            });
        }
    }
    for t in db::list_linear_tickets(db)? {
        if t.initiative_reviewed_at.is_none() {
            items.push(Item {
                item_ref: format!("linear_ticket:{}", t.identifier),
                description: format!("Linear ticket {}: {} (state: {})", t.identifier, t.title, t.state),
            });
        }
    }

    Ok(items)
}

// Items already reviewed, still without an initiative, seen within the last
// STANDALONE_WINDOW_DAYS — offered as optional context, never mandatory.
fn gather_recent_standalone(db: &Db, since: &str) -> Result<Vec<Item>> {
    let mut items = Vec::new();

    for l in db::list_github_links(db)? {
        if l.initiative_id.is_none() && l.initiative_reviewed_at.is_some() && l.first_seen.as_str() >= since {
            items.push(Item {
                item_ref: format!("github_link:{}", l.id),
                description: format!(
                    "GitHub PR in {}: {} (relationship: {}, seen {})",
                    l.repo, l.context, l.relationship, l.first_seen
                ),
            });
        }
    }
    for c in db::list_commitments(db)? {
        if c.initiative_id.is_none() && c.initiative_reviewed_at.is_some() && c.created_at.as_str() >= since {
            items.push(Item {
                item_ref: format!("commitment:{}", c.id),
                description: format!("Commitment: {} (due {}, seen {})", c.text, c.due_date, c.created_at),
            });
        }
    }
    for m in db::list_mentions(db)? {
        if m.initiative_id.is_none() && m.initiative_reviewed_at.is_some() && m.created_at.as_str() >= since {
            items.push(Item {
                item_ref: format!("mention:{}", m.id),
                description: format!(
                    "Slack mention from {}: {} (seen {})",
                    m.from_user, m.text, m.created_at
                ),
            });
        }
    }
    for t in db::list_linear_tickets(db)? {
        if t.initiative_id.is_none() && t.initiative_reviewed_at.is_some() && t.last_synced.as_str() >= since {
            items.push(Item {
                item_ref: format!("linear_ticket:{}", t.identifier),
                description: format!(
                    "Linear ticket {}: {} (state: {}, seen {})",
                    t.identifier, t.title, t.state, t.last_synced
                ),
            });
        }
    }

    Ok(items)
}

fn apply_assignment(db: &Db, item_ref: &str, initiative_id: i64, now: &str) -> Result<()> {
    let Some((kind, id)) = item_ref.split_once(':') else {
        return Ok(());
    };
    match kind {
        "github_link" => {
            if let Ok(id) = id.parse() {
                db::assign_github_link(db, id, Some(initiative_id), now)?;
            }
        }
        "commitment" => {
            if let Ok(id) = id.parse() {
                db::assign_commitment(db, id, Some(initiative_id), now)?;
            }
        }
        "mention" => {
            if let Ok(id) = id.parse() {
                db::assign_mention(db, id, Some(initiative_id), now)?;
            }
        }
        "linear_ticket" => {
            db::assign_linear_ticket(db, id, Some(initiative_id), now)?;
        }
        _ => {}
    }
    Ok(())
}

fn mark_standalone(db: &Db, item_ref: &str, now: &str) -> Result<()> {
    let Some((kind, id)) = item_ref.split_once(':') else {
        return Ok(());
    };
    match kind {
        "github_link" => {
            if let Ok(id) = id.parse() {
                db::assign_github_link(db, id, None, now)?;
            }
        }
        "commitment" => {
            if let Ok(id) = id.parse() {
                db::assign_commitment(db, id, None, now)?;
            }
        }
        "mention" => {
            if let Ok(id) = id.parse() {
                db::assign_mention(db, id, None, now)?;
            }
        }
        "linear_ticket" => {
            db::assign_linear_ticket(db, id, None, now)?;
        }
        _ => {}
    }
    Ok(())
}

pub async fn run(db: &Db, client: &AnthropicClient, now: &str) -> Result<()> {
    let pending = gather_pending(db)?;
    if pending.is_empty() {
        tracing::debug!("bulk classify: nothing pending, skipping call");
        return Ok(());
    }

    let since = days_ago(STANDALONE_WINDOW_DAYS);
    let standalone = gather_recent_standalone(db, &since)?;

    let initiatives = initiatives_context(db)?;
    let pending_list = pending
        .iter()
        .map(|i| format!("- {}: {}", i.item_ref, i.description))
        .collect::<Vec<_>>()
        .join("\n");
    let standalone_list = if standalone.is_empty() {
        "(none)".to_string()
    } else {
        standalone
            .iter()
            .map(|i| format!("- {}: {}", i.item_ref, i.description))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let system = format!("{SYSTEM}\n\nExisting initiatives:\n{initiatives}");
    let user_content = format!(
        "Needs a decision:\n{pending_list}\n\nRecently reviewed, still standalone (reference only):\n{standalone_list}"
    );
    let max_tokens = (((pending.len() + standalone.len()) as u32) * 150 + 1024).min(8192);

    let calls = client
        .call_tools(&system, &user_content, tools(), max_tokens)
        .await?;

    let mut touched: HashSet<String> = HashSet::new();
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
        apply_assignment(db, item_ref, initiative_id, now)?;
        touched.insert(item_ref.to_string());
        assigned += 1;
    }

    // Every pending item gets a verdict this cycle, even if that verdict is
    // "left standalone" — that's what moves it out of gather_pending and
    // into (at most) the recent-standalone reconsideration window next time.
    let mut left_standalone = 0;
    for item in &pending {
        if !touched.contains(&item.item_ref) {
            mark_standalone(db, &item.item_ref, now)?;
            left_standalone += 1;
        }
    }

    let reconsidered = standalone
        .iter()
        .filter(|i| touched.contains(&i.item_ref))
        .count();

    tracing::info!(
        "bulk classify: {assigned} assigned ({reconsidered} retroactively, from the standalone pool), {left_standalone} left standalone"
    );

    Ok(())
}
