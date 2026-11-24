use anyhow::Result;

use crate::db::{self, Db};
use crate::mock;

// Rule-based stand-in for the Claude tool-use extraction pass described in
// ARCHITECTURE.md. Same db:: write primitives either way — swapping this for
// a live Claude call only touches this file.

fn find_github_url(text: &str) -> Option<String> {
    let idx = text.find("https://github.com/")?;
    let rest = &text[idx..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some(rest[..end].trim_end_matches(['.', ',']).to_string())
}

fn repo_from_url(url: &str) -> String {
    url.trim_start_matches("https://github.com/")
        .splitn(3, '/')
        .take(2)
        .collect::<Vec<_>>()
        .join("/")
}

fn due_date_from_text(text: &str) -> String {
    match text.find("by ") {
        Some(idx) => {
            let rest = &text[idx + 3..];
            let end = rest.find('.').unwrap_or(rest.len());
            rest[..end].trim().to_string()
        }
        None => "unspecified".to_string(),
    }
}

const ME: &str = "jonathan";

pub fn run(db: &Db, now: &str) -> Result<()> {
    for msg in mock::slack_messages() {
        if let Some(url) = find_github_url(&msg.text) {
            let repo = repo_from_url(&url);
            let relationship = if msg.from_user == ME {
                "authored"
            } else if msg.mentions_me || msg.reacted_by_me {
                "reviewing"
            } else {
                "mentioned"
            };
            db::log_github_link(
                db,
                &url,
                &repo,
                relationship,
                "open",
                &msg.text,
                &msg.permalink,
                now,
            )?;
        }

        let text_lower = msg.text.to_lowercase();
        if msg.from_user == ME && (text_lower.contains("i'll") || text_lower.contains("i will")) {
            db::log_commitment(db, &msg.text, &due_date_from_text(&msg.text), &msg.permalink, now)?;
        }

        if msg.mentions_me && msg.from_user != ME {
            db::log_mention(db, &msg.permalink, &msg.from_user, &msg.text, true, now)?;
        }
    }

    for n in mock::linear_notifications() {
        db::log_linear_ticket(
            db,
            &n.identifier,
            &n.title,
            &n.state,
            &n.kind,
            n.linked_github_url.as_deref(),
            now,
        )?;
    }

    for s in mock::github_statuses() {
        db::sync_github_status(db, &s.url, &s.status, now)?;
    }

    Ok(())
}
