use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{delete, get, patch};
use axum::{Form, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{SharedState, db, mock};

fn render<S: Serialize>(state: &SharedState, name: &str, ctx: S) -> Html<String> {
    let tmpl = state.env.get_template(name).expect("template exists");
    let value = minijinja::Value::from(minijinja::value::Serde(ctx));
    Html(tmpl.render(value).expect("template renders"))
}

fn now() -> String {
    chrono::Local::now().to_rfc3339()
}

#[derive(Serialize)]
struct AttentionItem {
    text: String,
    kind: String,
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/", get(dashboard))
        .route(
            "/github-links",
            get(github_links_page).post(create_github_link),
        )
        .route("/github-links/{id}/resolve", patch(resolve_github_link))
        .route("/github-links/{id}", delete(delete_github_link))
        .route(
            "/commitments",
            get(commitments_page).post(create_commitment),
        )
        .route("/commitments/{id}/resolve", patch(resolve_commitment))
        .route("/commitments/{id}", delete(delete_commitment))
        .route("/mentions", get(mentions_page).post(create_mention))
        .route("/mentions/{id}/resolve", patch(resolve_mention))
        .route("/mentions/{id}", delete(delete_mention))
        .route(
            "/linear-tickets",
            get(linear_tickets_page).post(create_linear_ticket),
        )
        .route(
            "/linear-tickets/{identifier}/resolve",
            patch(resolve_linear_ticket),
        )
        .route("/linear-tickets/{identifier}", delete(delete_linear_ticket))
        .route("/sources", get(sources_page))
        .route("/mock/slack", get(mock::mock_slack))
        .route("/mock/linear", get(mock::mock_linear))
        .route("/mock/github", get(mock::mock_github))
}

async fn dashboard(State(state): State<SharedState>) -> Html<String> {
    let links = db::list_github_links(&state.db).unwrap();
    let commitments = db::list_commitments(&state.db).unwrap();
    let mentions = db::list_mentions(&state.db).unwrap();
    let tickets = db::list_linear_tickets(&state.db).unwrap();

    let open_links = links
        .iter()
        .filter(|l| l.status != "resolved" && l.status != "merged")
        .count();
    let open_commitments = commitments.iter().filter(|c| c.status == "open").count();
    let unanswered_mentions = mentions.iter().filter(|m| !m.responded).count();
    let open_tickets = tickets.iter().filter(|t| t.state != "Done").count();

    let mut attention = Vec::new();
    for l in &links {
        if l.status == "changes_requested" || (l.relationship == "reviewing" && l.status == "open")
        {
            attention.push(AttentionItem {
                text: format!("Review {} ({})", l.repo, l.status),
                kind: "github".into(),
            });
        }
    }
    for m in &mentions {
        if !m.responded {
            attention.push(AttentionItem {
                text: format!("{}: {}", m.from_user, m.text),
                kind: "slack mention".into(),
            });
        }
    }
    for c in &commitments {
        if c.status == "open" {
            attention.push(AttentionItem {
                text: format!("{} (due {})", c.text, c.due_date),
                kind: "commitment".into(),
            });
        }
    }
    for t in &tickets {
        if t.state != "Done" && (t.relationship == "assigned" || t.relationship == "mentioned") {
            attention.push(AttentionItem {
                text: format!("{} — {}", t.identifier, t.title),
                kind: "linear".into(),
            });
        }
    }

    render(
        &state,
        "dashboard.html",
        json!({
            "active": "dashboard",
            "open_links": open_links,
            "open_commitments": open_commitments,
            "unanswered_mentions": unanswered_mentions,
            "open_tickets": open_tickets,
            "attention": attention,
        }),
    )
}

// --- github links ---

#[derive(Deserialize)]
struct NewGithubLink {
    url: String,
    repo: String,
    relationship: String,
    context: String,
}

async fn github_links_page(State(state): State<SharedState>) -> Html<String> {
    let items = db::list_github_links(&state.db).unwrap();
    render(
        &state,
        "github_links.html",
        json!({ "active": "github-links", "items": items }),
    )
}

async fn create_github_link(
    State(state): State<SharedState>,
    Form(form): Form<NewGithubLink>,
) -> Html<String> {
    let id = db::log_github_link(
        &state.db,
        &form.url,
        &form.repo,
        &form.relationship,
        "open",
        &form.context,
        "manual",
        &now(),
    )
    .unwrap();
    let item = db::get_github_link(&state.db, id).unwrap().unwrap();
    render(&state, "partials/github_link_row.html", json!({ "item": item }))
}

async fn resolve_github_link(
    State(state): State<SharedState>,
    Path(id): Path<i64>,
) -> Html<String> {
    db::resolve_github_link(&state.db, id).unwrap();
    let item = db::get_github_link(&state.db, id).unwrap().unwrap();
    render(&state, "partials/github_link_row.html", json!({ "item": item }))
}

async fn delete_github_link(
    State(state): State<SharedState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    db::delete_github_link(&state.db, id).unwrap();
    StatusCode::OK
}

// --- commitments ---

#[derive(Deserialize)]
struct NewCommitment {
    text: String,
    due_date: String,
}

async fn commitments_page(State(state): State<SharedState>) -> Html<String> {
    let items = db::list_commitments(&state.db).unwrap();
    render(
        &state,
        "commitments.html",
        json!({ "active": "commitments", "items": items }),
    )
}

async fn create_commitment(
    State(state): State<SharedState>,
    Form(form): Form<NewCommitment>,
) -> Html<String> {
    let id = db::log_commitment(&state.db, &form.text, &form.due_date, "manual", &now()).unwrap();
    let item = db::get_commitment(&state.db, id).unwrap().unwrap();
    render(&state, "partials/commitment_row.html", json!({ "item": item }))
}

async fn resolve_commitment(State(state): State<SharedState>, Path(id): Path<i64>) -> Html<String> {
    db::resolve_commitment(&state.db, id).unwrap();
    let item = db::get_commitment(&state.db, id).unwrap().unwrap();
    render(&state, "partials/commitment_row.html", json!({ "item": item }))
}

async fn delete_commitment(
    State(state): State<SharedState>,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    db::delete_commitment(&state.db, id).unwrap();
    StatusCode::OK
}

// --- mentions ---

#[derive(Deserialize)]
struct NewMention {
    from_user: String,
    text: String,
    needs_response: Option<String>,
}

async fn mentions_page(State(state): State<SharedState>) -> Html<String> {
    let items = db::list_mentions(&state.db).unwrap();
    render(
        &state,
        "mentions.html",
        json!({ "active": "mentions", "items": items }),
    )
}

async fn create_mention(
    State(state): State<SharedState>,
    Form(form): Form<NewMention>,
) -> Html<String> {
    let permalink = format!("manual:{}", uuid_like());
    let id = db::log_mention(
        &state.db,
        &permalink,
        &form.from_user,
        &form.text,
        form.needs_response.is_some(),
        &now(),
    )
    .unwrap();
    let item = db::get_mention(&state.db, id).unwrap().unwrap();
    render(&state, "partials/mention_row.html", json!({ "item": item }))
}

async fn resolve_mention(State(state): State<SharedState>, Path(id): Path<i64>) -> Html<String> {
    db::resolve_mention(&state.db, id).unwrap();
    let item = db::get_mention(&state.db, id).unwrap().unwrap();
    render(&state, "partials/mention_row.html", json!({ "item": item }))
}

async fn delete_mention(State(state): State<SharedState>, Path(id): Path<i64>) -> impl IntoResponse {
    db::delete_mention(&state.db, id).unwrap();
    StatusCode::OK
}

fn uuid_like() -> String {
    format!("{:x}", std::time::SystemTime::UNIX_EPOCH.elapsed().unwrap().as_nanos())
}

// --- linear tickets ---

#[derive(Deserialize)]
struct NewLinearTicket {
    identifier: String,
    title: String,
    state: String,
    relationship: String,
    linked_github_url: String,
}

async fn linear_tickets_page(State(state): State<SharedState>) -> Html<String> {
    let items = db::list_linear_tickets(&state.db).unwrap();
    render(
        &state,
        "linear_tickets.html",
        json!({ "active": "linear-tickets", "items": items }),
    )
}

async fn create_linear_ticket(
    State(state): State<SharedState>,
    Form(form): Form<NewLinearTicket>,
) -> Html<String> {
    let linked = if form.linked_github_url.trim().is_empty() {
        None
    } else {
        Some(form.linked_github_url.as_str())
    };
    db::log_linear_ticket(
        &state.db,
        &form.identifier,
        &form.title,
        &form.state,
        &form.relationship,
        linked,
        &now(),
    )
    .unwrap();
    let item = db::get_linear_ticket(&state.db, &form.identifier)
        .unwrap()
        .unwrap();
    render(&state, "partials/linear_ticket_row.html", json!({ "item": item }))
}

async fn resolve_linear_ticket(
    State(state): State<SharedState>,
    Path(identifier): Path<String>,
) -> Html<String> {
    db::resolve_linear_ticket(&state.db, &identifier).unwrap();
    let item = db::get_linear_ticket(&state.db, &identifier)
        .unwrap()
        .unwrap();
    render(&state, "partials/linear_ticket_row.html", json!({ "item": item }))
}

async fn delete_linear_ticket(
    State(state): State<SharedState>,
    Path(identifier): Path<String>,
) -> impl IntoResponse {
    db::delete_linear_ticket(&state.db, &identifier).unwrap();
    StatusCode::OK
}

// --- mock sources ---

async fn sources_page(State(state): State<SharedState>) -> Html<String> {
    render(&state, "sources.html", json!({ "active": "sources" }))
}
