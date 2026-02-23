use axum::{
    Json, Router,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use serde_json::{Value, json};

use hapir_shared::schemas::cli_api::{
    CreateMachineRequest, CreateMachineResponse, CreateSessionRequest, CreateSessionResponse,
    ListMessagesQuery, ListMessagesResponse,
};

use crate::web::AppState;
use crate::web::middleware::cli_auth::CliAuthContext;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sessions", post(create_session))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}/messages", get(list_messages))
        .route("/machines", post(create_machine))
        .route("/machines/{id}", get(get_machine))
}

// --- Handlers ---

async fn create_session(
    State(state): State<AppState>,
    Extension(auth): Extension<CliAuthContext>,
    Json(body): Json<CreateSessionRequest>,
) -> (StatusCode, Json<Value>) {
    let namespace = body.namespace.as_deref().unwrap_or(&auth.namespace);

    match state
        .sync_engine
        .get_or_create_session(
            &body.tag,
            &body.metadata,
            body.agent_state.as_ref(),
            namespace,
        )
        .await
    {
        Ok(session) => (
                StatusCode::OK,
                Json(serde_json::to_value(CreateSessionResponse { session }).unwrap()),
            ),
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
    match state
        .sync_engine
        .resolve_session_access(&id, &auth.namespace)
        .await
    {
        Ok((_session_id, session)) => (
            StatusCode::OK,
            Json(serde_json::to_value(CreateSessionResponse { session }).unwrap()),
        ),
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
    let resolved_session_id = match state
        .sync_engine
        .resolve_session_access(&id, &auth.namespace)
        .await
    {
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
    let messages =
        state
            .sync_engine
            .get_messages_after(&resolved_session_id, query.after_seq, limit);

    (
        StatusCode::OK,
        Json(serde_json::to_value(ListMessagesResponse { messages }).unwrap()),
    )
}

async fn create_machine(
    State(state): State<AppState>,
    Extension(auth): Extension<CliAuthContext>,
    Json(body): Json<CreateMachineRequest>,
) -> (StatusCode, Json<Value>) {
    let namespace = body.namespace.as_deref().unwrap_or(&auth.namespace);

    // Check if existing machine belongs to a different namespace
    if let Some(existing) = state.sync_engine.get_machine(&body.id).await
        && existing.namespace != namespace
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Machine access denied"})),
        );
    }

    match state
        .sync_engine
        .get_or_create_machine(
            &body.id,
            &body.metadata,
            body.runner_state.as_ref(),
            namespace,
        )
        .await
    {
        Ok(machine) => (
                StatusCode::OK,
                Json(serde_json::to_value(CreateMachineResponse {
                    machine: serde_json::to_value(machine).unwrap(),
                }).unwrap()),
            ),
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
    match state
        .sync_engine
        .get_machine_by_namespace(&id, &auth.namespace)
        .await
    {
        Some(machine) => (StatusCode::OK, Json(json!({ "machine": machine }))),
        None => {
            // Check if machine exists but belongs to different namespace
            if state.sync_engine.get_machine(&id).await.is_some() {
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
