use axum::{
    Json, Router,
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::web::AppState;
use crate::web::middleware::auth::AuthContext;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/sessions/{id}/git-status", get(git_status))
        .route("/sessions/{id}/git-diff-numstat", get(git_diff_numstat))
        .route("/sessions/{id}/git-diff-file", get(git_diff_file))
        .route("/sessions/{id}/file", get(get_file))
        .route("/sessions/{id}/files", get(list_files))
        .route("/sessions/{id}/directory", get(list_directory))
}

/// Helper to resolve session access and extract the session path. Returns
/// `(session_id, session_path)` on success, or an error response tuple.
async fn resolve_session_and_path(
    state: &AppState,
    auth: &AuthContext,
    raw_id: &str,
) -> Result<(String, String), (StatusCode, Json<Value>)> {
    let (session_id, session) = state
        .sync_engine
        .resolve_session_access(raw_id, &auth.namespace)
        .await
        .map_err(|reason| {
            if reason == "access-denied" {
                (
                    StatusCode::FORBIDDEN,
                    Json(json!({"error": "Session access denied"})),
                )
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "Session not found"})),
                )
            }
        })?;

    let session_path = session
        .metadata
        .as_ref()
        .map(|m| m.path.clone())
        .filter(|p| !p.is_empty())
        .ok_or_else(|| {
            (
                StatusCode::OK,
                Json(json!({"success": false, "error": "Session path not available"})),
            )
        })?;

    Ok((session_id, session_path))
}

/// Wraps an async RPC call, converting errors to a JSON error payload.
fn rpc_error(err: anyhow::Error) -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({"success": false, "error": err.to_string()})),
    )
}

// --- Handlers ---

async fn git_status(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
) -> (StatusCode, Json<Value>) {
    let (session_id, session_path) = match resolve_session_and_path(&state, &auth, &id).await {
        Ok(v) => v,
        Err(e) => return e,
    };

    match state
        .sync_engine
        .get_git_status(&session_id, Some(&session_path))
        .await
    {
        Ok(resp) => {
            let val = serde_json::to_value(&resp).unwrap_or_else(|_| json!({"success": false}));
            (StatusCode::OK, Json(val))
        }
        Err(e) => rpc_error(e),
    }
}

#[derive(Deserialize)]
struct DiffNumstatQuery {
    staged: Option<String>,
}

async fn git_diff_numstat(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(query): Query<DiffNumstatQuery>,
) -> (StatusCode, Json<Value>) {
    let (session_id, session_path) = match resolve_session_and_path(&state, &auth, &id).await {
        Ok(v) => v,
        Err(e) => return e,
    };

    let staged = parse_bool_param(query.staged.as_deref());

    match state
        .sync_engine
        .get_git_diff_numstat(&session_id, Some(&session_path), staged)
        .await
    {
        Ok(resp) => {
            let val = serde_json::to_value(&resp).unwrap_or_else(|_| json!({"success": false}));
            (StatusCode::OK, Json(val))
        }
        Err(e) => rpc_error(e),
    }
}

#[derive(Deserialize)]
struct DiffFileQuery {
    path: String,
    staged: Option<String>,
}

async fn git_diff_file(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    query: Result<Query<DiffFileQuery>, axum::extract::rejection::QueryRejection>,
) -> (StatusCode, Json<Value>) {
    let Query(query) = match query {
        Ok(q) => q,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Invalid file path"})),
            );
        }
    };

    if query.path.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid file path"})),
        );
    }

    let (session_id, session_path) = match resolve_session_and_path(&state, &auth, &id).await {
        Ok(v) => v,
        Err(e) => return e,
    };

    let staged = parse_bool_param(query.staged.as_deref());

    match state
        .sync_engine
        .get_git_diff_file(&session_id, Some(&session_path), &query.path, staged)
        .await
    {
        Ok(resp) => {
            let val = serde_json::to_value(&resp).unwrap_or_else(|_| json!({"success": false}));
            (StatusCode::OK, Json(val))
        }
        Err(e) => rpc_error(e),
    }
}

#[derive(Deserialize)]
struct FilePathQuery {
    path: String,
}

async fn get_file(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    query: Result<Query<FilePathQuery>, axum::extract::rejection::QueryRejection>,
) -> (StatusCode, Json<Value>) {
    let Query(query) = match query {
        Ok(q) => q,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Invalid file path"})),
            );
        }
    };

    if query.path.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Invalid file path"})),
        );
    }

    let (session_id, _session_path) = match resolve_session_and_path(&state, &auth, &id).await {
        Ok(v) => v,
        Err(e) => return e,
    };

    match state
        .sync_engine
        .read_session_file(&session_id, &query.path)
        .await
    {
        Ok(resp) => {
            let val = serde_json::to_value(&resp).unwrap_or_else(|_| json!({"success": false}));
            (StatusCode::OK, Json(val))
        }
        Err(e) => rpc_error(e),
    }
}

#[derive(Deserialize)]
struct FileSearchQuery {
    query: Option<String>,
    limit: Option<usize>,
}

async fn list_files(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(query): Query<FileSearchQuery>,
) -> (StatusCode, Json<Value>) {
    let (session_id, session_path) = match resolve_session_and_path(&state, &auth, &id).await {
        Ok(v) => v,
        Err(e) => return e,
    };

    let search_query = query.query.as_deref().map(|q| q.trim()).unwrap_or("");
    let limit = query.limit.unwrap_or(200).clamp(1, 500);

    let mut args: Vec<String> = vec!["--files".to_string()];
    if !search_query.is_empty() {
        args.push("--iglob".to_string());
        args.push(format!("*{search_query}*"));
    }

    match state
        .sync_engine
        .run_ripgrep(&session_id, &args, Some(&session_path))
        .await
    {
        Ok(resp) => {
            if !resp.success {
                return (
                    StatusCode::OK,
                    Json(json!({
                        "success": false,
                        "error": resp.error.unwrap_or_else(|| "Failed to list files".to_string())
                    })),
                );
            }

            let stdout = resp.stdout.unwrap_or_default();
            let files: Vec<Value> = stdout
                .split('\n')
                .map(|line| line.trim())
                .filter(|line| !line.is_empty())
                .take(limit)
                .map(|full_path| {
                    let parts: Vec<&str> = full_path.split('/').collect();
                    let file_name = parts.last().copied().unwrap_or(full_path);
                    let file_path = if parts.len() > 1 {
                        parts[..parts.len() - 1].join("/")
                    } else {
                        String::new()
                    };
                    json!({
                        "fileName": file_name,
                        "filePath": file_path,
                        "fullPath": full_path,
                        "fileType": "file"
                    })
                })
                .collect();

            (
                StatusCode::OK,
                Json(json!({"success": true, "files": files})),
            )
        }
        Err(e) => rpc_error(e),
    }
}

#[derive(Deserialize)]
struct DirectoryQuery {
    path: Option<String>,
}

async fn list_directory(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(id): Path<String>,
    Query(query): Query<DirectoryQuery>,
) -> (StatusCode, Json<Value>) {
    let (session_id, _session_path) = match resolve_session_and_path(&state, &auth, &id).await {
        Ok(v) => v,
        Err(e) => return e,
    };

    let path = query.path.as_deref().unwrap_or("");

    match state.sync_engine.list_directory(&session_id, path).await {
        Ok(resp) => {
            let val = serde_json::to_value(&resp).unwrap_or_else(|_| json!({"success": false}));
            (StatusCode::OK, Json(val))
        }
        Err(e) => rpc_error(e),
    }
}

/// Parse an optional boolean query parameter ("true"/"false").
fn parse_bool_param(value: Option<&str>) -> Option<bool> {
    match value {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}
