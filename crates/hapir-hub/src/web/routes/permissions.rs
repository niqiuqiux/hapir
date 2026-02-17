use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::post,
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use hapir_shared::modes::{is_permission_mode_allowed_for_flavor, AgentFlavor, PermissionMode};
use hapir_shared::schemas::{AnswersFormat, PermissionDecision};

use crate::web::middleware::auth::AuthContext;
use crate::web::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/sessions/{id}/permissions/{request_id}/approve",
            post(approve_permission),
        )
        .route(
            "/sessions/{id}/permissions/{request_id}/deny",
            post(deny_permission),
        )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApproveBody {
    mode: Option<PermissionMode>,
    allow_tools: Option<Vec<String>>,
    decision: Option<PermissionDecision>,
    answers: Option<AnswersFormat>,
}

#[derive(Deserialize)]
struct DenyBody {
    decision: Option<PermissionDecision>,
}

async fn approve_permission(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((id, request_id)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> (StatusCode, Json<Value>) {
    // Parse body, treating absent body as empty object
    let raw = body.map(|Json(v)| v).unwrap_or(json!({}));
    let parsed: ApproveBody = match serde_json::from_value(raw) {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid body" })),
            )
        }
    };

    let mut engine = state.sync_engine.lock().await;

    // Verify session access and require active
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

    // Check that the permission request exists in agent_state
    let request_exists = session
        .agent_state
        .as_ref()
        .and_then(|s| s.requests.as_ref())
        .is_some_and(|r| r.contains_key(&request_id));

    if !request_exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Request not found" })),
        );
    }

    // Validate permission mode against session flavor
    if let Some(mode) = parsed.mode {
        let flavor: Option<AgentFlavor> = session
            .metadata
            .as_ref()
            .and_then(|m| m.flavor.as_deref())
            .and_then(|f| serde_json::from_value(Value::String(f.to_string())).ok());

        if !is_permission_mode_allowed_for_flavor(mode, flavor) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid permission mode for session flavor" })),
            );
        }
    }

    let decision_str = parsed.decision.map(|d| match d {
        PermissionDecision::Approved => "approved",
        PermissionDecision::ApprovedForSession => "approved_for_session",
        PermissionDecision::Denied => "denied",
        PermissionDecision::Abort => "abort",
    });

    match engine
        .approve_permission(
            &session_id,
            &request_id,
            parsed.mode,
            parsed.allow_tools,
            decision_str,
            parsed.answers,
        )
        .await
    {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ),
    }
}

async fn deny_permission(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((id, request_id)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> (StatusCode, Json<Value>) {
    // Parse body, treating absent body as empty object
    let raw = body.map(|Json(v)| v).unwrap_or(json!({}));
    let parsed: DenyBody = match serde_json::from_value(raw) {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid body" })),
            )
        }
    };

    let mut engine = state.sync_engine.lock().await;

    // Verify session access and require active
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

    // Check that the permission request exists in agent_state
    let request_exists = session
        .agent_state
        .as_ref()
        .and_then(|s| s.requests.as_ref())
        .is_some_and(|r| r.contains_key(&request_id));

    if !request_exists {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Request not found" })),
        );
    }

    let decision_str = parsed.decision.map(|d| match d {
        PermissionDecision::Approved => "approved",
        PermissionDecision::ApprovedForSession => "approved_for_session",
        PermissionDecision::Denied => "denied",
        PermissionDecision::Abort => "abort",
    });

    match engine
        .deny_permission(&session_id, &request_id, decision_str)
        .await
    {
        Ok(()) => (StatusCode::OK, Json(json!({ "ok": true }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        ),
    }
}
