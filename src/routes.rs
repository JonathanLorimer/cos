use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::routing::{delete, get, patch};
use axum::{Form, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::util::now;
use crate::{SharedState, db, poller};

fn render<S: Serialize>(state: &SharedState, name: &str, ctx: S) -> Html<String> {
    let tmpl = state.env.get_template(name).expect("template exists");
    let value = minijinja::Value::from(minijinja::value::Serde(ctx));
    Html(tmpl.render(value).expect("template renders"))
}

#[derive(Serialize)]
struct AttentionItem {
    text: String,
    kind: String,
    permalink: Option<String>,
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
        .route(
            "/initiatives",
            get(initiatives_page).post(create_initiative),
        )
        .route("/initiatives/classify", axum::routing::post(classify_now))
        .route("/initiatives/{id}", get(initiative_detail).delete(delete_initiative))
        .route("/sources", get(sources_page))
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
                permalink: Some(l.source_permalink.clone()),
            });
        }
    }
    for m in &mentions {
        if !m.responded {
            attention.push(AttentionItem {
                text: format!("{}: {}", m.from_user, m.text),
                kind: "slack mention".into(),
                permalink: Some(m.source_permalink.clone()),
            });
        }
    }
    for c in &commitments {
        if c.status == "open" {
            attention.push(AttentionItem {
                text: format!("{} (due {})", c.text, c.due_date),
                kind: "commitment".into(),
                permalink: Some(c.source_permalink.clone()),
            });
        }
    }
    for t in &tickets {
        if t.state != "Done" && (t.relationship == "assigned" || t.relationship == "mentioned") {
            attention.push(AttentionItem {
                text: format!("{} — {}", t.identifier, t.title),
                kind: "linear".into(),
                permalink: None,
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
    let all = db::list_linear_tickets(&state.db).unwrap();
    // Reviewed-and-standalone tickets have no related Slack/PR activity to
    // cross-reference — that's just Linear's own task tracking, which Linear
    // already shows you. Collapse those out of the default view rather than
    // duplicating Linear's UI; pending (not-yet-reviewed) tickets still show
    // normally since "no initiative yet" isn't the same as "judged unrelated."
    let (standalone, items): (Vec<_>, Vec<_>) = all
        .into_iter()
        .partition(|t| t.initiative_id.is_none() && t.initiative_reviewed_at.is_some());
    render(
        &state,
        "linear_tickets.html",
        json!({ "active": "linear-tickets", "items": items, "standalone": standalone }),
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

// --- initiatives ---

#[derive(Serialize)]
struct InitiativeSummary {
    id: i64,
    name: String,
    description: String,
    total_items: usize,
    open_items: usize,
}

fn initiative_summaries(state: &SharedState) -> (Vec<InitiativeSummary>, usize) {
    let initiatives = db::list_initiatives(&state.db).unwrap();
    let links = db::list_github_links(&state.db).unwrap();
    let commitments = db::list_commitments(&state.db).unwrap();
    let mentions = db::list_mentions(&state.db).unwrap();
    let tickets = db::list_linear_tickets(&state.db).unwrap();

    let summaries = initiatives
        .into_iter()
        .map(|i| {
            let id = Some(i.id);
            let l: Vec<_> = links.iter().filter(|x| x.initiative_id == id).collect();
            let c: Vec<_> = commitments.iter().filter(|x| x.initiative_id == id).collect();
            let m: Vec<_> = mentions.iter().filter(|x| x.initiative_id == id).collect();
            let t: Vec<_> = tickets.iter().filter(|x| x.initiative_id == id).collect();
            let open_items = l.iter().filter(|x| x.status != "resolved" && x.status != "merged").count()
                + c.iter().filter(|x| x.status == "open").count()
                + m.iter().filter(|x| !x.responded).count()
                + t.iter().filter(|x| x.state != "Done").count();
            InitiativeSummary {
                id: i.id,
                name: i.name,
                description: i.description,
                total_items: l.len() + c.len() + m.len() + t.len(),
                open_items,
            }
        })
        .collect();

    let unclassified = links.iter().filter(|x| x.initiative_id.is_none()).count()
        + commitments.iter().filter(|x| x.initiative_id.is_none()).count()
        + mentions.iter().filter(|x| x.initiative_id.is_none()).count()
        + tickets.iter().filter(|x| x.initiative_id.is_none()).count();

    (summaries, unclassified)
}

async fn initiatives_page(State(state): State<SharedState>) -> Html<String> {
    let (initiatives, unclassified) = initiative_summaries(&state);
    render(
        &state,
        "initiatives.html",
        json!({ "active": "initiatives", "initiatives": initiatives, "unclassified": unclassified }),
    )
}

#[derive(Deserialize)]
struct NewInitiative {
    name: String,
    description: String,
}

async fn create_initiative(
    State(state): State<SharedState>,
    Form(form): Form<NewInitiative>,
) -> Html<String> {
    db::create_initiative(&state.db, &form.name, &form.description, &now()).unwrap();
    let (initiatives, unclassified) = initiative_summaries(&state);
    render(
        &state,
        "partials/initiative_cards.html",
        json!({ "initiatives": initiatives, "unclassified": unclassified }),
    )
}

async fn classify_now(State(state): State<SharedState>) -> Html<String> {
    poller::run_bulk_classification(&state.db).await.unwrap();
    let (initiatives, unclassified) = initiative_summaries(&state);
    render(
        &state,
        "partials/initiative_cards.html",
        json!({ "initiatives": initiatives, "unclassified": unclassified }),
    )
}

async fn delete_initiative(State(state): State<SharedState>, Path(id): Path<i64>) -> impl IntoResponse {
    db::delete_initiative(&state.db, id).unwrap();
    StatusCode::OK
}

async fn initiative_detail(State(state): State<SharedState>, Path(id): Path<i64>) -> Html<String> {
    let initiative = db::get_initiative(&state.db, id).unwrap();
    let init_id = Some(id);
    let github_links: Vec<_> = db::list_github_links(&state.db)
        .unwrap()
        .into_iter()
        .filter(|x| x.initiative_id == init_id)
        .collect();
    let commitments: Vec<_> = db::list_commitments(&state.db)
        .unwrap()
        .into_iter()
        .filter(|x| x.initiative_id == init_id)
        .collect();
    let mentions: Vec<_> = db::list_mentions(&state.db)
        .unwrap()
        .into_iter()
        .filter(|x| x.initiative_id == init_id)
        .collect();
    let linear_tickets: Vec<_> = db::list_linear_tickets(&state.db)
        .unwrap()
        .into_iter()
        .filter(|x| x.initiative_id == init_id)
        .collect();

    render(
        &state,
        "initiative_detail.html",
        json!({
            "active": "initiatives",
            "initiative": initiative,
            "github_links": github_links,
            "commitments": commitments,
            "mentions": mentions,
            "linear_tickets": linear_tickets,
        }),
    )
}

// --- sources ---

async fn sources_page(State(state): State<SharedState>) -> Html<String> {
    render(
        &state,
        "sources.html",
        json!({
            "active": "sources",
            "slack_configured": crate::slack::SlackClient::from_env().is_some(),
            "github_configured": crate::github::GithubClient::from_env().is_some(),
            "linear_configured": crate::linear::LinearClient::from_env().is_some(),
        }),
    )
}
