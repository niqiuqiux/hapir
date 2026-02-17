use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::web::middleware::cli_auth::CliAuthContext;
use crate::web::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sessions", post(create_session))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/messages", get(list_messages))
        .route("/machines", post(create_machine))
        .route("/machines/{id}", get(get_machine))
}

// --- Request types ---

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionBody {
    tag: String,
    metadata: Value,
    agent_state: Option<Value>,
    namespace: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListMessagesQuery {
    after_seq: i64,
    limit: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateMachineBody {
    id: String,
    metadata: Value,
    runner_state: Option<Value>,
    namespace: Option<String>,
}

// --- Handlers ---

async fn create_session(
    State(state): State<AppState>,
    Extension(auth): Extension<CliAuthContext>,
    Json(body): Json<CreateSessionBody>,
) -> (StatusCode, Json<Value>) {
    let namespace = body.namespace.as_deref().unwrap_or(&auth.namespace);
    let mut engine = state.sync_engine.lock().await;

    match engine.get_or_create_session(
        &body.tag,
        &body.metadata,
        body.agent_state.as_ref(),
        namespace,
    ) {
        Ok(session) => (StatusCode::OK, Json(json!({ "session": session }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

async fn get_session(
    State(state): State<AppState>,
    Extension(auth): Extension<CliAuthContext>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let mut engine = state.sync_engine.lock().await;

    match engine.resolve_session_access(&id, &auth.namespace) {
        Ok((_session_id, session)) => (StatusCode::OK, Json(json!({ "session": session }))),
        Err("access-denied") => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Session access denied"})),
        ),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "Session not found"})),
        ),
    }
}

async fn list_messages(
    State(state): State<AppState>,
    Extension(auth): Extension<CliAuthContext>,
    Path(id): Path<String>,
    Query(query): Query<ListMessagesQuery>,
) -> (StatusCode, Json<Value>) {
    let mut engine = state.sync_engine.lock().await;

    let resolved_session_id = match engine.resolve_session_access(&id, &auth.namespace) {
        Ok((session_id, _session)) => session_id,
        Err("access-denied") => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "Session access denied"})),
            );
        }
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Session not found"})),
            );
        }
    };

    let limit = query.limit.unwrap_or(200).clamp(1, 200);
    let messages = engine.get_messages_after(&resolved_session_id, query.after_seq, limit);

    (StatusCode::OK, Json(json!({ "messages": messages })))
}

async fn create_machine(
    State(state): State<AppState>,
    Extension(auth): Extension<CliAuthContext>,
    Json(body): Json<CreateMachineBody>,
) -> (StatusCode, Json<Value>) {
    let namespace = body.namespace.as_deref().unwrap_or(&auth.namespace);
    let mut engine = state.sync_engine.lock().await;

    // Check if existing machine belongs to a different namespace
    if let Some(existing) = engine.get_machine(&body.id) {
        if existing.namespace != namespace {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "Machine access denied"})),
            );
        }
    }

    match engine.get_or_create_machine(
        &body.id,
        &body.metadata,
        body.runner_state.as_ref(),
        namespace,
    ) {
        Ok(machine) => (StatusCode::OK, Json(json!({ "machine": machine }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}

async fn get_machine(
    State(state): State<AppState>,
    Extension(auth): Extension<CliAuthContext>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let engine = state.sync_engine.lock().await;

    match engine.get_machine_by_namespace(&id, &auth.namespace) {
        Some(machine) => (StatusCode::OK, Json(json!({ "machine": machine }))),
        None => {
            // Check if machine exists but belongs to different namespace
            if engine.get_machine(&id).is_some() {
                (
                    StatusCode::FORBIDDEN,
                    Json(json!({"error": "Machine access denied"})),
                )
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "Machine not found"})),
                )
            }
        }
    }
}
