use std::collections::HashSet;

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::web::middleware::auth::AuthContext;
use crate::web::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/machines", get(list_machines))
        .route("/machines/{id}/spawn", post(spawn_machine))
        .route("/machines/{id}/paths/exists", post(paths_exists))
}

async fn list_machines(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> (StatusCode, Json<Value>) {
    let engine = state.sync_engine.lock().await;
    let machines = engine.get_online_machines_by_namespace(&auth.namespace);
    (StatusCode::OK, Json(json!({ "machines": machines })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpawnBody {
    directory: String,
    agent: Option<String>,
    model: Option<String>,
    yolo: Option<bool>,
    session_type: Option<String>,
    worktree_name: Option<String>,
}

async fn spawn_machine(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(machine_id): Path<String>,
    Json(body): Json<SpawnBody>,
) -> (StatusCode, Json<Value>) {
    if body.directory.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid body"})),
        );
    }

    let engine = state.sync_engine.lock().await;

    // Verify machine exists and belongs to this namespace
    let machine = match engine.get_machine(&machine_id) {
        Some(m) => m.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Machine not found"})),
            );
        }
    };
    if machine.namespace != auth.namespace {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Machine access denied"})),
        );
    }
    if !machine.active {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "Machine is offline"})),
        );
    }

    let agent = body.agent.as_deref().unwrap_or("claude");
    if !matches!(agent, "claude" | "codex" | "gemini" | "opencode") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid agent"})),
        );
    }
    if let Some(ref st) = body.session_type {
        if !matches!(st.as_str(), "simple" | "worktree") {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Invalid sessionType"})),
            );
        }
    }

    let result = engine
        .spawn_session(
            &machine_id,
            &body.directory,
            agent,
            body.model.as_deref(),
            body.yolo,
            body.session_type.as_deref(),
            body.worktree_name.as_deref(),
            None,
        )
        .await;

    match result {
        Ok(spawn_result) => {
            let val = serde_json::to_value(&spawn_result).unwrap_or_else(|_| json!({"type": "error"}));
            (StatusCode::OK, Json(val))
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e})),
        ),
    }
}

#[derive(Deserialize)]
struct PathsExistsBody {
    paths: Vec<String>,
}

async fn paths_exists(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(machine_id): Path<String>,
    body: Result<Json<PathsExistsBody>, axum::extract::rejection::JsonRejection>,
) -> (StatusCode, Json<Value>) {
    let body = match body {
        Ok(Json(b)) => b,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Invalid body"})),
            );
        }
    };

    if body.paths.len() > 1000 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid body"})),
        );
    }

    let engine = state.sync_engine.lock().await;

    // Verify machine exists and belongs to this namespace
    let machine = match engine.get_machine(&machine_id) {
        Some(m) => m.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "Machine not found"})),
            );
        }
    };
    if machine.namespace != auth.namespace {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Machine access denied"})),
        );
    }
    if !machine.active {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": "Machine is offline"})),
        );
    }

    // Deduplicate and filter empty paths
    let unique_paths: Vec<String> = {
        let mut seen = HashSet::new();
        body.paths
            .into_iter()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty() && seen.insert(p.clone()))
            .collect()
    };

    if unique_paths.is_empty() {
        return (StatusCode::OK, Json(json!({ "exists": {} })));
    }

    match engine.check_paths_exist(&machine_id, &unique_paths).await {
        Ok(exists) => (StatusCode::OK, Json(json!({ "exists": exists }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        ),
    }
}
