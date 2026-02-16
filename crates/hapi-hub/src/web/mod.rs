pub mod middleware;
pub mod routes;
pub mod telegram_init_data;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use tokio::sync::Mutex;
use tower_http::cors::{Any, CorsLayer};

use crate::store::Store;
use crate::sync::SyncEngine;

/// Shared application state passed to all handlers.
#[derive(Clone)]
pub struct AppState {
    pub jwt_secret: Vec<u8>,
    pub cli_api_token: String,
    pub sync_engine: Arc<Mutex<SyncEngine>>,
    pub store: Arc<Store>,
    pub vapid_public_key: Option<String>,
    pub telegram_bot_token: Option<String>,
    pub data_dir: PathBuf,
}

/// Build the axum Router with all routes and middleware.
pub fn build_router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::DELETE,
            axum::http::Method::PATCH,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
        ])
        .allow_origin(Any);

    let cli_routes = routes::cli::router().route_layer(
        axum::middleware::from_fn_with_state(state.clone(), middleware::cli_auth::cli_auth),
    );

    let api_routes = routes::api_router().route_layer(
        axum::middleware::from_fn_with_state(state.clone(), middleware::auth::jwt_auth),
    );

    Router::new()
        .route("/health", axum::routing::get(|| async { "ok" }))
        .nest("/cli", cli_routes)
        .nest("/api", api_routes)
        .layer(cors)
        .with_state(state)
}
