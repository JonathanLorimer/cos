use std::sync::{Arc, Mutex};

use anyhow::Result;
use rusqlite::{Connection, params};

use crate::models::{Commitment, GithubLink, Initiative, LinearTicket, Mention};

pub type Db = Arc<Mutex<Connection>>;

pub fn open(path: &str) -> Result<Db> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS initiatives (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            description TEXT NOT NULL DEFAULT '',
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS github_links (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            url TEXT NOT NULL UNIQUE,
            repo TEXT NOT NULL,
            relationship TEXT NOT NULL,
            status TEXT NOT NULL,
            context TEXT NOT NULL,
            source_permalink TEXT NOT NULL,
            first_seen TEXT NOT NULL,
            last_synced TEXT NOT NULL,
            initiative_id INTEGER REFERENCES initiatives(id)
        );
        CREATE TABLE IF NOT EXISTS commitments (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            text TEXT NOT NULL,
            due_date TEXT NOT NULL,
            status TEXT NOT NULL,
            source_permalink TEXT NOT NULL,
            created_at TEXT NOT NULL,
            initiative_id INTEGER REFERENCES initiatives(id)
        );
        CREATE TABLE IF NOT EXISTS mentions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            source_permalink TEXT NOT NULL UNIQUE,
            from_user TEXT NOT NULL,
            text TEXT NOT NULL,
            needs_response INTEGER NOT NULL,
            responded INTEGER NOT NULL,
            created_at TEXT NOT NULL,
            initiative_id INTEGER REFERENCES initiatives(id)
        );
        CREATE TABLE IF NOT EXISTS sync_cursors (
            source TEXT PRIMARY KEY,
            last_synced_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS linear_tickets (
            identifier TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            state TEXT NOT NULL,
            relationship TEXT NOT NULL,
            linked_github_url TEXT,
            last_synced TEXT NOT NULL,
            initiative_id INTEGER REFERENCES initiatives(id)
        );
        ",
    )?;
    Ok(Arc::new(Mutex::new(conn)))
}

// Epoch default means a never-polled source is treated as "everything is
// new" — a fresh DB naturally does a full backfill on its first poll.
const EPOCH: &str = "0000-01-01T00:00:00Z";

pub fn get_cursor(db: &Db, source: &str) -> Result<String> {
    let conn = db.lock().unwrap();
    Ok(conn
        .query_row(
            "SELECT last_synced_at FROM sync_cursors WHERE source = ?1",
            params![source],
            |r| r.get(0),
        )
        .unwrap_or_else(|_| EPOCH.to_string()))
}

pub fn set_cursor(db: &Db, source: &str, at: &str) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO sync_cursors (source, last_synced_at) VALUES (?1, ?2)
         ON CONFLICT(source) DO UPDATE SET last_synced_at = excluded.last_synced_at",
        params![source, at],
    )?;
    Ok(())
}

// --- tool-call layer: these are the write primitives an extraction pass
// (rule-based here, Claude tool-use in the live system) invokes per event ---

// authored > reviewing > mentioned: a later, weaker mention of a PR already
// tracked as jonathan's own shouldn't demote it. Merge only ever upgrades.
fn relationship_rank(r: &str) -> u8 {
    match r {
        "authored" => 3,
        "reviewing" => 2,
        _ => 1,
    }
}

fn merge_relationship(old: &str, new: &str) -> String {
    if relationship_rank(new) > relationship_rank(old) {
        new.to_string()
    } else {
        old.to_string()
    }
}

const MAX_CONTEXT_ENTRIES: usize = 3;

// Accumulates distinct context notes instead of replacing the one we had —
// each re-mention adds information rather than erasing the last mention's.
fn merge_context(old: &str, new: &str) -> String {
    if old.is_empty() || old == new {
        return new.to_string();
    }
    let mut entries: Vec<&str> = old.split(" | ").collect();
    if entries.last() == Some(&new) {
        return old.to_string();
    }
    entries.push(new);
    let start = entries.len().saturating_sub(MAX_CONTEXT_ENTRIES);
    entries[start..].join(" | ")
}

pub fn log_github_link(
    db: &Db,
    url: &str,
    repo: &str,
    relationship: &str,
    status: &str,
    context: &str,
    source_permalink: &str,
    now: &str,
) -> Result<i64> {
    let conn = db.lock().unwrap();

    let existing: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT id, relationship, context FROM github_links WHERE url = ?1",
            params![url],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();

    match existing {
        // Deliberately does not touch `status` here: that's owned by
        // sync_github_status (the live state check) and the manual Resolve
        // action, not by extraction re-mentioning a PR it already knows
        // about. Also leaves `first_seen`/`source_permalink` pinned to
        // whichever message first reported this PR.
        Some((id, old_relationship, old_context)) => {
            let merged_relationship = merge_relationship(&old_relationship, relationship);
            let merged_context = merge_context(&old_context, context);
            conn.execute(
                "UPDATE github_links SET relationship = ?1, context = ?2, last_synced = ?3 WHERE id = ?4",
                params![merged_relationship, merged_context, now, id],
            )?;
            Ok(id)
        }
        None => {
            conn.execute(
                "INSERT INTO github_links (url, repo, relationship, status, context, source_permalink, first_seen, last_synced)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                params![url, repo, relationship, status, context, source_permalink, now],
            )?;
            Ok(conn.last_insert_rowid())
        }
    }
}

pub fn log_commitment(
    db: &Db,
    text: &str,
    due_date: &str,
    source_permalink: &str,
    now: &str,
) -> Result<i64> {
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO commitments (text, due_date, status, source_permalink, created_at)
         VALUES (?1, ?2, 'open', ?3, ?4)",
        params![text, due_date, source_permalink, now],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn log_mention(
    db: &Db,
    source_permalink: &str,
    from_user: &str,
    text: &str,
    needs_response: bool,
    now: &str,
) -> Result<i64> {
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO mentions (source_permalink, from_user, text, needs_response, responded, created_at)
         VALUES (?1, ?2, ?3, ?4, 0, ?5)
         ON CONFLICT(source_permalink) DO UPDATE SET
            text = excluded.text,
            needs_response = excluded.needs_response",
        params![source_permalink, from_user, text, needs_response as i64, now],
    )?;
    conn.query_row(
        "SELECT id FROM mentions WHERE source_permalink = ?1",
        params![source_permalink],
        |r| r.get(0),
    )
    .map_err(Into::into)
}

pub fn log_linear_ticket(
    db: &Db,
    identifier: &str,
    title: &str,
    state: &str,
    relationship: &str,
    linked_github_url: Option<&str>,
    now: &str,
) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO linear_tickets (identifier, title, state, relationship, linked_github_url, last_synced)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(identifier) DO UPDATE SET
            title = excluded.title,
            state = excluded.state,
            relationship = excluded.relationship,
            linked_github_url = excluded.linked_github_url,
            last_synced = excluded.last_synced",
        params![identifier, title, state, relationship, linked_github_url, now],
    )?;
    Ok(())
}

// --- reads ---

pub fn list_github_links(db: &Db) -> Result<Vec<GithubLink>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, url, repo, relationship, status, context, source_permalink, first_seen, last_synced, initiative_id
         FROM github_links ORDER BY last_synced DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(GithubLink {
            id: r.get(0)?,
            url: r.get(1)?,
            repo: r.get(2)?,
            relationship: r.get(3)?,
            status: r.get(4)?,
            context: r.get(5)?,
            source_permalink: r.get(6)?,
            first_seen: r.get(7)?,
            last_synced: r.get(8)?,
            initiative_id: r.get(9)?,
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn get_github_link(db: &Db, id: i64) -> Result<Option<GithubLink>> {
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT id, url, repo, relationship, status, context, source_permalink, first_seen, last_synced, initiative_id
         FROM github_links WHERE id = ?1",
        params![id],
        |r| {
            Ok(GithubLink {
                id: r.get(0)?,
                url: r.get(1)?,
                repo: r.get(2)?,
                relationship: r.get(3)?,
                status: r.get(4)?,
                context: r.get(5)?,
                source_permalink: r.get(6)?,
                first_seen: r.get(7)?,
                last_synced: r.get(8)?,
                initiative_id: r.get(9)?,
            })
        },
    )
    .ok()
    .map(Ok)
    .transpose()
}

pub fn list_commitments(db: &Db) -> Result<Vec<Commitment>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, text, due_date, status, source_permalink, created_at, initiative_id
         FROM commitments ORDER BY due_date ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Commitment {
            id: r.get(0)?,
            text: r.get(1)?,
            due_date: r.get(2)?,
            status: r.get(3)?,
            source_permalink: r.get(4)?,
            created_at: r.get(5)?,
            initiative_id: r.get(6)?,
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn get_commitment(db: &Db, id: i64) -> Result<Option<Commitment>> {
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT id, text, due_date, status, source_permalink, created_at, initiative_id
         FROM commitments WHERE id = ?1",
        params![id],
        |r| {
            Ok(Commitment {
                id: r.get(0)?,
                text: r.get(1)?,
                due_date: r.get(2)?,
                status: r.get(3)?,
                source_permalink: r.get(4)?,
                created_at: r.get(5)?,
                initiative_id: r.get(6)?,
            })
        },
    )
    .ok()
    .map(Ok)
    .transpose()
}

pub fn list_mentions(db: &Db) -> Result<Vec<Mention>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT id, source_permalink, from_user, text, needs_response, responded, created_at, initiative_id
         FROM mentions ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Mention {
            id: r.get(0)?,
            source_permalink: r.get(1)?,
            from_user: r.get(2)?,
            text: r.get(3)?,
            needs_response: r.get::<_, i64>(4)? != 0,
            responded: r.get::<_, i64>(5)? != 0,
            created_at: r.get(6)?,
            initiative_id: r.get(7)?,
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn get_mention(db: &Db, id: i64) -> Result<Option<Mention>> {
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT id, source_permalink, from_user, text, needs_response, responded, created_at, initiative_id
         FROM mentions WHERE id = ?1",
        params![id],
        |r| {
            Ok(Mention {
                id: r.get(0)?,
                source_permalink: r.get(1)?,
                from_user: r.get(2)?,
                text: r.get(3)?,
                needs_response: r.get::<_, i64>(4)? != 0,
                responded: r.get::<_, i64>(5)? != 0,
                created_at: r.get(6)?,
                initiative_id: r.get(7)?,
            })
        },
    )
    .ok()
    .map(Ok)
    .transpose()
}

pub fn list_linear_tickets(db: &Db) -> Result<Vec<LinearTicket>> {
    let conn = db.lock().unwrap();
    let mut stmt = conn.prepare(
        "SELECT identifier, title, state, relationship, linked_github_url, last_synced, initiative_id
         FROM linear_tickets ORDER BY last_synced DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(LinearTicket {
            identifier: r.get(0)?,
            title: r.get(1)?,
            state: r.get(2)?,
            relationship: r.get(3)?,
            linked_github_url: r.get(4)?,
            last_synced: r.get(5)?,
            initiative_id: r.get(6)?,
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn get_linear_ticket(db: &Db, identifier: &str) -> Result<Option<LinearTicket>> {
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT identifier, title, state, relationship, linked_github_url, last_synced, initiative_id
         FROM linear_tickets WHERE identifier = ?1",
        params![identifier],
        |r| {
            Ok(LinearTicket {
                identifier: r.get(0)?,
                title: r.get(1)?,
                state: r.get(2)?,
                relationship: r.get(3)?,
                linked_github_url: r.get(4)?,
                last_synced: r.get(5)?,
                initiative_id: r.get(6)?,
            })
        },
    )
    .ok()
    .map(Ok)
    .transpose()
}

// --- initiatives ---

pub fn list_initiatives(db: &Db) -> Result<Vec<Initiative>> {
    let conn = db.lock().unwrap();
    let mut stmt =
        conn.prepare("SELECT id, name, description, created_at FROM initiatives ORDER BY name ASC")?;
    let rows = stmt.query_map([], |r| {
        Ok(Initiative {
            id: r.get(0)?,
            name: r.get(1)?,
            description: r.get(2)?,
            created_at: r.get(3)?,
        })
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

pub fn get_initiative(db: &Db, id: i64) -> Result<Option<Initiative>> {
    let conn = db.lock().unwrap();
    conn.query_row(
        "SELECT id, name, description, created_at FROM initiatives WHERE id = ?1",
        params![id],
        |r| {
            Ok(Initiative {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                created_at: r.get(3)?,
            })
        },
    )
    .ok()
    .map(Ok)
    .transpose()
}

pub fn create_initiative(db: &Db, name: &str, description: &str, now: &str) -> Result<i64> {
    let conn = db.lock().unwrap();
    conn.execute(
        "INSERT INTO initiatives (name, description, created_at) VALUES (?1, ?2, ?3)",
        params![name, description, now],
    )?;
    Ok(conn.last_insert_rowid())
}

// Idempotent: returns the existing initiative's id if `name` is already
// taken (its description is left untouched). Used by both the heuristic and
// agent-based classifiers so re-running either is a no-op per item.
pub fn get_or_create_initiative(db: &Db, name: &str, description: &str, now: &str) -> Result<i64> {
    {
        let conn = db.lock().unwrap();
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM initiatives WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .ok();
        if let Some(id) = existing {
            return Ok(id);
        }
    }
    create_initiative(db, name, description, now)
}

pub fn delete_initiative(db: &Db, id: i64) -> Result<()> {
    let conn = db.lock().unwrap();
    for table in ["github_links", "commitments", "mentions", "linear_tickets"] {
        conn.execute(
            &format!("UPDATE {table} SET initiative_id = NULL WHERE initiative_id = ?1"),
            params![id],
        )?;
    }
    conn.execute("DELETE FROM initiatives WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn assign_github_link(db: &Db, id: i64, initiative_id: Option<i64>) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE github_links SET initiative_id = ?1 WHERE id = ?2",
        params![initiative_id, id],
    )?;
    Ok(())
}

pub fn assign_commitment(db: &Db, id: i64, initiative_id: Option<i64>) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE commitments SET initiative_id = ?1 WHERE id = ?2",
        params![initiative_id, id],
    )?;
    Ok(())
}

pub fn assign_mention(db: &Db, id: i64, initiative_id: Option<i64>) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE mentions SET initiative_id = ?1 WHERE id = ?2",
        params![initiative_id, id],
    )?;
    Ok(())
}

pub fn assign_linear_ticket(db: &Db, identifier: &str, initiative_id: Option<i64>) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE linear_tickets SET initiative_id = ?1 WHERE identifier = ?2",
        params![initiative_id, identifier],
    )?;
    Ok(())
}

// --- updates / deletes (the U and D of CRUD) ---

pub fn sync_github_status(db: &Db, url: &str, status: &str, now: &str) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE github_links SET status = ?1, last_synced = ?2 WHERE url = ?3",
        params![status, now, url],
    )?;
    Ok(())
}

pub fn resolve_github_link(db: &Db, id: i64) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE github_links SET status = 'resolved' WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

pub fn delete_github_link(db: &Db, id: i64) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute("DELETE FROM github_links WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn resolve_commitment(db: &Db, id: i64) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE commitments SET status = 'done' WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

pub fn delete_commitment(db: &Db, id: i64) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute("DELETE FROM commitments WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn resolve_mention(db: &Db, id: i64) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE mentions SET responded = 1 WHERE id = ?1",
        params![id],
    )?;
    Ok(())
}

pub fn delete_mention(db: &Db, id: i64) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute("DELETE FROM mentions WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn resolve_linear_ticket(db: &Db, identifier: &str) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "UPDATE linear_tickets SET state = 'Done' WHERE identifier = ?1",
        params![identifier],
    )?;
    Ok(())
}

pub fn delete_linear_ticket(db: &Db, identifier: &str) -> Result<()> {
    let conn = db.lock().unwrap();
    conn.execute(
        "DELETE FROM linear_tickets WHERE identifier = ?1",
        params![identifier],
    )?;
    Ok(())
}
