mod agent;
mod agent_bulk_classify;
mod agent_extraction;
mod classify;
mod db;
mod extraction;
mod github;
mod linear;
mod models;
mod poller;
mod routes;
mod slack;
mod util;

use std::sync::Arc;

use axum::Router;
use minijinja::Environment;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

pub struct AppState {
    pub db: db::Db,
    pub env: Environment<'static>,
}

pub type SharedState = Arc<AppState>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::var("RUST_LOG").is_err() {
        // SAFETY: single-threaded at this point, before any env reads race it.
        unsafe { std::env::set_var("RUST_LOG", "cos=info,tower_http=info") };
    }
    tracing_subscriber::fmt::init();

    let db_path = std::env::var("COS_DB_PATH").unwrap_or_else(|_| "cos.sqlite3".into());
    let db = db::open(&db_path)?;

    tracing::info!(
        agent = agent::AnthropicClient::is_configured(),
        "running startup extraction + bulk classification"
    );
    // Logged, not `?`: a live-service hiccup (GitHub token pending approval,
    // Slack down, a transient network error) shouldn't prevent the server
    // from coming up and serving whatever data already exists.
    if let Err(e) = poller::run_extraction(&db).await {
        tracing::error!("startup extraction failed: {e:#}");
    }
    if let Err(e) = poller::run_bulk_classification(&db).await {
        tracing::error!("startup bulk classification failed: {e:#}");
    }

    let poll_interval_secs: u64 = std::env::var("COS_POLL_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let bulk_threshold: usize = std::env::var("COS_BULK_THRESHOLD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20);
    poller::spawn_cycle_loop(
        db.clone(),
        std::time::Duration::from_secs(poll_interval_secs),
        bulk_threshold,
    );
    tracing::info!(
        "polling every {poll_interval_secs}s, bulk-classifying once pending items reach {bulk_threshold}"
    );

    let templates_dir = std::env::var("COS_TEMPLATES_DIR").unwrap_or_else(|_| "templates".into());
    let static_dir = std::env::var("COS_STATIC_DIR").unwrap_or_else(|_| "static".into());

    let mut env = Environment::new();
    env.set_loader(minijinja::path_loader(&templates_dir));
    env.add_filter("short_date", |s: String| -> String { s.chars().take(10).collect() });
    env.add_filter("is_link", |s: String| -> bool { s.starts_with("https://") });

    let state: SharedState = Arc::new(AppState { db, env });

    let app = Router::new()
        .merge(routes::router())
        .nest_service("/static", ServeDir::new(static_dir))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr = std::env::var("COS_ADDR").unwrap_or_else(|_| "127.0.0.1:4700".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
