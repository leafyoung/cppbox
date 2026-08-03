pub mod admin;
pub mod db;
pub mod debug;
pub mod lsp;
pub mod remote;
pub mod routes;
pub mod sandbox;
pub mod storage;

use std::path::PathBuf;

use axum::Router;
use sqlx::SqlitePool;
use tower_http::{cors::CorsLayer, services::ServeDir};

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub root: PathBuf,
}

/// Resolve the repo/app root: `CPPBOX_ROOT` env, else the current working dir.
/// During dev (`cargo run`), CWD is the repo root, so `data/`, `projects/`, and
/// `frontend/` resolve next to the Python backend — enabling a non-destructive
/// A/B test against the same data.
pub fn root_dir() -> PathBuf {
    std::env::var("CPPBOX_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Build the full Axum app: API routes + a catch-all serving `frontend_dir`.
pub fn build_app(state: AppState, frontend_dir: PathBuf) -> Router {
    Router::new()
        .merge(routes::routes())
        .merge(admin::routes())
        .with_state(state)
        .layer(CorsLayer::permissive())
        .fallback_service(ServeDir::new(frontend_dir))
}
