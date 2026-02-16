use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{delete, get, patch, post},
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use hapi_shared::modes::{
    is_model_mode_allowed_for_flavor, is_permission_mode_allowed_for_flavor,
    permission_modes_for_flavor, AgentFlavor, ModelMode, PermissionMode,
};
use hapi_shared::session_summary::to_session_summary;

use crate::sync::{ResumeSessionErrorCode, ResumeSessionResult};
use crate::web::middleware::auth::AuthContext;
use crate::web::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", get(get_session))
        .route("/sessions/{id}", delete(delete_session))
        .route("/sessions/{id}", patch(update_session))
        .route("/sessions/{id}/resume", post(resume_session))
        .route("/sessions/{id}/archive", post(archive_session))
        .route("/sessions/{id}/abort", post(abort_session))
        .route("/sessions/{id}/switch", post(switch_session))
        .route("/sessions/{id}/permission-mode", post(set_permission_mode))
        .route("/sessions/{id}/model", post(set_model))
}

fn pending_count(session: &hapi_shared::schemas::Session) -> usize {
    session
        .agent_state
        .as_ref()
        .and_then(|s| s.requests.as_ref())
        .map(|r| r.len())
        .unwrap_or(0)
}

fn parse_flavor(session: &hapi_shared::schemas::Session) -> Option<AgentFlavor> {
    session
        .metadata
        .as_ref()
        .and_then(|m| m.flavor.as_deref())
        .and_then(|f| serde_json::from_value(Value::String(f.to_string())).ok())
}

// ---------- handlers ----------

async fn list_sessions(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> (StatusCode, Json<Value>) {
    let engine = state.sync_engine.lock().await;
    let mut sessions = engine.get_sessions_by_namespace(&auth.namespace);

    sessions.sort_by(|a, b| {
        // Active sessions first
        if a.active != b.active {
            return if a.active {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }
        // Within active sessions, sort by pending requests count descending
        if a.active {
            let a_pending = pending_count(a);
            let b_pending = pending_count(b);
            if a_pending != b_pending {
                return b_pending.cmp(&a_pending);
            }
        }
        // Then by updatedAt descending
        b.updated_at
            .partial_cmp(&a.updated_at)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let summaries: Vec<_> = sessions.iter().map(to_session_summary).collect();
    (StatusCode::OK, Json(json!({ "sessions": summaries })))
}

async fn get_session(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let mut engine = state.sync_engine.lock().await;
    match engine.resolve_session_access(&id, &auth.namespace) {
        Ok((_session_id, session)) => (StatusCode::OK, Json(json!({ "session": session }))),
        Err("access-denied") => (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "Session access denied" })),
        ),
        Err(_) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Session not found" })),
        ),
    }
}

async fn delete_session(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let mut engine = state.sync_engine.lock().await;

    let (session_id, session) = match engine.resolve_session_access(&id, &auth.namespace) {
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

    if session.active {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "Cannot delete active session. Archive it first." })),
        );
    }

    match engine.delete_session(&session_id) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => {
            let message = e.to_string();
            if message.contains("active") {
                (StatusCode::CONFLICT, Json(json!({ "error": message })))
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": message })),
                )
            }
        }
    }
}

#[derive(Deserialize)]
struct RenameBody {
    name: String,
}

async fn update_session(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    body: Option<Json<RenameBody>>,
) -> (StatusCode, Json<Value>) {
    let Json(body) = match body {
        Some(b) => b,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid body: name is required" })),
            )
        }
    };

    if body.name.is_empty() || body.name.len() > 255 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid body: name is required" })),
        );
    }

    let mut engine = state.sync_engine.lock().await;

    let session_id = match engine.resolve_session_access(&id, &auth.namespace) {
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

    match engine.rename_session(&session_id, &body.name) {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => {
            let message = e.to_string();
            if message.contains("concurrently") || message.contains("version") {
                (StatusCode::CONFLICT, Json(json!({ "error": message })))
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": message })),
                )
            }
        }
    }
}

async fn resume_session(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let mut engine = state.sync_engine.lock().await;

    // Verify the session exists and belongs to the namespace first
    if let Err(reason) = engine.resolve_session_access(&id, &auth.namespace) {
        return match reason {
            "access-denied" => (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "Session access denied" })),
            ),
            _ => (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Session not found" })),
            ),
        };
    }

    let result = engine.resume_session(&id, &auth.namespace).await;
    match result {
        ResumeSessionResult::Success { session_id } => (
            StatusCode::OK,
            Json(json!({ "type": "success", "sessionId": session_id })),
        ),
        ResumeSessionResult::Error { message, code } => {
            let status = match code {
                ResumeSessionErrorCode::NoMachineOnline => StatusCode::SERVICE_UNAVAILABLE,
                ResumeSessionErrorCode::AccessDenied => StatusCode::FORBIDDEN,
                ResumeSessionErrorCode::SessionNotFound => StatusCode::NOT_FOUND,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            let code_str = match code {
                ResumeSessionErrorCode::SessionNotFound => "session_not_found",
                ResumeSessionErrorCode::AccessDenied => "access_denied",
                ResumeSessionErrorCode::NoMachineOnline => "no_machine_online",
                ResumeSessionErrorCode::ResumeUnavailable => "resume_unavailable",
                ResumeSessionErrorCode::ResumeFailed => "resume_failed",
            };
            (
                status,
                Json(json!({ "error": message, "code": code_str })),
            )
        }
    }
}

async fn archive_session(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let mut engine = state.sync_engine.lock().await;

    let (session_id, session) = match engine.resolve_session_access(&id, &auth.namespace) {
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

    match engine.archive_session(&session_id).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ),
    }
}

async fn abort_session(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let mut engine = state.sync_engine.lock().await;

    let (session_id, session) = match engine.resolve_session_access(&id, &auth.namespace) {
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

    match engine.abort_session(&session_id).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ),
    }
}

#[derive(Deserialize)]
struct SwitchBody {
    to: Option<String>,
}

async fn switch_session(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    body: Option<Json<SwitchBody>>,
) -> (StatusCode, Json<Value>) {
    let mut engine = state.sync_engine.lock().await;

    let (session_id, session) = match engine.resolve_session_access(&id, &auth.namespace) {
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

    let to = body
        .and_then(|Json(b)| b.to)
        .unwrap_or_else(|| "remote".to_string());

    match engine.switch_session(&session_id, &to).await {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ),
    }
}

#[derive(Deserialize)]
struct PermissionModeBody {
    mode: PermissionMode,
}

async fn set_permission_mode(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    body: Option<Json<PermissionModeBody>>,
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

    let mut engine = state.sync_engine.lock().await;

    let (session_id, session) = match engine.resolve_session_access(&id, &auth.namespace) {
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

    let flavor = parse_flavor(&session);

    let allowed_modes = permission_modes_for_flavor(flavor);
    if allowed_modes.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Permission mode not supported for session flavor" })),
        );
    }

    if !is_permission_mode_allowed_for_flavor(body.mode, flavor) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Invalid permission mode for session flavor" })),
        );
    }

    match engine
        .apply_session_config(&session_id, Some(body.mode), None)
        .await
    {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => (StatusCode::CONFLICT, Json(json!({ "error": e.to_string() }))),
    }
}

#[derive(Deserialize)]
struct ModelModeBody {
    model: ModelMode,
}

async fn set_model(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    body: Option<Json<ModelModeBody>>,
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

    let mut engine = state.sync_engine.lock().await;

    let (session_id, session) = match engine.resolve_session_access(&id, &auth.namespace) {
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

    let flavor = parse_flavor(&session);

    if !is_model_mode_allowed_for_flavor(body.model, flavor) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Model mode is only supported for Claude sessions" })),
        );
    }

    match engine
        .apply_session_config(&session_id, None, Some(body.model))
        .await
    {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => (StatusCode::CONFLICT, Json(json!({ "error": e.to_string() }))),
    }
}
