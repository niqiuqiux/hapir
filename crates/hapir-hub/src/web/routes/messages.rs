use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use hapir_shared::schemas::AttachmentMetadata;

use crate::web::middleware::auth::AuthContext;
use crate::web::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sessions/{id}/messages", get(list_messages))
        .route("/sessions/{id}/messages", post(create_message))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessagesQuery {
    limit: Option<i64>,
    before_seq: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendMessageBody {
    text: String,
    local_id: Option<String>,
    attachments: Option<Vec<AttachmentMetadata>>,
}

async fn list_messages(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(query): Query<MessagesQuery>,
) -> (StatusCode, Json<Value>) {
    // Verify session access
    let session_id = match state.sync_engine.resolve_session_access(&id, &auth.namespace).await {
        Ok((sid, _session)) => sid,
        Err("access-denied") => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "Session access denied" })),
            )
        }
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Session not found" })),
            )
        }
    };

    // Clamp limit to 1..=200, default 50
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let before_seq = query.before_seq.filter(|&s| s >= 1);

    let result = state.sync_engine.get_messages_page(&session_id, limit, before_seq);

    (
        StatusCode::OK,
        Json(serde_json::to_value(result).unwrap_or(json!({}))),
    )
}

async fn create_message(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    body: Option<Json<SendMessageBody>>,
) -> (StatusCode, Json<Value>) {
    let Json(body) = match body {
        Some(b) => b,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid body" })),
            )
        }
    };

    // Require text or attachments
    let has_text = !body.text.is_empty();
    let has_attachments = body
        .attachments
        .as_ref()
        .is_some_and(|a| !a.is_empty());

    if !has_text && !has_attachments {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Message requires text or attachments" })),
        );
    }

    // Verify session access and require active
    let (session_id, session) = match state.sync_engine.resolve_session_access(&id, &auth.namespace).await {
        Ok(pair) => pair,
        Err("access-denied") => {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "Session access denied" })),
            )
        }
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Session not found" })),
            )
        }
    };

    if !session.active {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "Session is inactive" })),
        );
    }

    let attachments_slice = body.attachments.as_deref();

    match state.sync_engine.send_message(
        &session_id,
        &auth.namespace,
        &body.text,
        body.local_id.as_deref(),
        attachments_slice,
        Some("webapp"),
    ).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ),
    }
}
