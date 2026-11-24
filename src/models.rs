use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct GithubLink {
    pub id: i64,
    pub url: String,
    pub repo: String,
    pub relationship: String,
    pub status: String,
    pub context: String,
    pub source_permalink: String,
    pub first_seen: String,
    pub last_synced: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Commitment {
    pub id: i64,
    pub text: String,
    pub due_date: String,
    pub status: String,
    pub source_permalink: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Mention {
    pub id: i64,
    pub source_permalink: String,
    pub from_user: String,
    pub text: String,
    pub needs_response: bool,
    pub responded: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinearTicket {
    pub identifier: String,
    pub title: String,
    pub state: String,
    pub relationship: String,
    pub linked_github_url: Option<String>,
    pub last_synced: String,
}
