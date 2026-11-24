use std::collections::HashMap;

use anyhow::Result;

use crate::db::{self, Db};
use crate::util::{find_github_url, repo_display_name, repo_from_url};

// Heuristic stand-in for agent-based initiative classification: groups
// everything touching the same GitHub repo into one initiative. Real
// classification needs an LLM call (ANTHROPIC_API_KEY, unset today — see
// ARCHITECTURE.md) to group by intent rather than repo; this groups by repo
// in the meantime and only touches unassigned items, so re-running it after
// wiring in the real classifier won't clobber manual assignments.
pub fn run(db: &Db, now: &str) -> Result<()> {
    let mut repo_to_initiative: HashMap<String, i64> = HashMap::new();

    for link in db::list_github_links(db)? {
        if link.initiative_id.is_some() {
            continue;
        }
        let initiative_id = match repo_to_initiative.get(&link.repo) {
            Some(id) => *id,
            None => {
                let name = repo_display_name(&link.repo);
                let id = db::get_or_create_initiative(db, &name, "", now)?;
                repo_to_initiative.insert(link.repo.clone(), id);
                id
            }
        };
        db::assign_github_link(db, link.id, Some(initiative_id))?;
    }

    for ticket in db::list_linear_tickets(db)? {
        if ticket.initiative_id.is_some() {
            continue;
        }
        let Some(url) = &ticket.linked_github_url else {
            continue;
        };
        if let Some(&id) = repo_to_initiative.get(&repo_from_url(url)) {
            db::assign_linear_ticket(db, &ticket.identifier, Some(id))?;
        }
    }

    for mention in db::list_mentions(db)? {
        if mention.initiative_id.is_some() {
            continue;
        }
        let Some(url) = find_github_url(&mention.text) else {
            continue;
        };
        if let Some(&id) = repo_to_initiative.get(&repo_from_url(&url)) {
            db::assign_mention(db, mention.id, Some(id))?;
        }
    }

    for commitment in db::list_commitments(db)? {
        if commitment.initiative_id.is_some() {
            continue;
        }
        let Some(url) = find_github_url(&commitment.text) else {
            continue;
        };
        if let Some(&id) = repo_to_initiative.get(&repo_from_url(&url)) {
            db::assign_commitment(db, commitment.id, Some(id))?;
        }
    }

    Ok(())
}
