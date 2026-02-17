pub mod middleware;
pub mod routes;
pub mod static_files;
pub mod telegram_init_data;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;

use hapir_shared::version::PROTOCOL_VERSION;

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
    pub cors_origins: Vec<String>,
}

/// Build the axum Router with all routes and middleware.
pub fn build_router(state: AppState) -> Router {
    use tower_http::cors::AllowOrigin;

    let cors_origins = &state.cors_origins;
    let allow_origin = if cors_origins.iter().any(|o| o == "*") {
        AllowOrigin::any()
    } else {
        let origins: Vec<axum::http::HeaderValue> = cors_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        AllowOrigin::list(origins)
    };

    let cors = CorsLayer::new()
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
        ])
        .allow_origin(allow_origin);

    let cli_routes = routes::cli::router()
        .route_layer(
            axum::middleware::from_fn_with_state(state.clone(), middleware::cli_auth::cli_auth),
        )
        .layer(cors.clone());

    // Mount API routes under /api without nest, so middleware sees full paths.
    let api_routes = routes::api_router().layer(cors);

    Router::new()
        .route("/health", axum::routing::get(|| async {
            axum::Json(serde_json::json!({ "status": "ok", "protocolVersion": PROTOCOL_VERSION }))
        }))
        .nest("/cli", cli_routes)
        .nest("/api", api_routes)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth::jwt_auth,
        ))
        .fallback(static_files::static_handler)
        .with_state(state)
}
