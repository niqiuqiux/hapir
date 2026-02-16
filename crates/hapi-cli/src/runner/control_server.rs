use std::collections::HashMap;
use std::sync::Arc;

use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::{info, warn};

use super::types::*;

/// Shared state for the control server
#[derive(Clone)]
pub struct RunnerState {
    pub sessions: Arc<Mutex<HashMap<String, TrackedSession>>>,
    pub shutdown_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<ShutdownSource>>>>,
}

impl RunnerState {
    pub fn new(shutdown_tx: tokio::sync::oneshot::Sender<ShutdownSource>) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            shutdown_tx: Arc::new(Mutex::new(Some(shutdown_tx))),
        }
    }
}

/// Build the control server router
pub fn router(state: RunnerState) -> Router {
    Router::new()
        .route("/session-started", post(session_started))
        .route("/list", post(list_sessions))
        .route("/stop-session", post(stop_session))
        .route("/spawn-session", post(spawn_session))
        .route("/stop", post(stop_runner))
        .with_state(state)
}

/// Start the control server on a random port, returns the port
pub async fn start(state: RunnerState) -> anyhow::Result<u16> {
    let app = router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    info!(port = port, "runner control server started");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            warn!(error = %e, "control server error");
        }
    });

    Ok(port)
}

async fn session_started(
    State(state): State<RunnerState>,
    Json(payload): Json<SessionStartedPayload>,
) -> (StatusCode, Json<Value>) {
    info!(session_id = %payload.session_id, "session started webhook");
    let mut sessions = state.sessions.lock().await;
    sessions.insert(
        payload.session_id.clone(),
        TrackedSession {
            session_id: payload.session_id,
            pid: None,
            directory: String::new(),
            started_by: "external".to_string(),
            metadata: payload.metadata,
        },
    );
    (StatusCode::OK, Json(json!({"status": "ok"})))
}

async fn list_sessions(
    State(state): State<RunnerState>,
) -> Json<ListSessionsResponse> {
    let sessions = state.sessions.lock().await;
    Json(ListSessionsResponse {
        sessions: sessions.values().cloned().collect(),
    })
}

async fn stop_session(
    State(state): State<RunnerState>,
    Json(req): Json<StopSessionRequest>,
) -> (StatusCode, Json<Value>) {
    let mut sessions = state.sessions.lock().await;
    if let Some(session) = sessions.remove(&req.session_id) {
        // Kill process if we have a PID
        if let Some(pid) = session.pid {
            #[cfg(unix)]
            {
                unsafe { libc::kill(pid as i32, libc::SIGTERM); }
            }
            let _ = pid;
        }
        info!(session_id = %req.session_id, "session stopped");
        (StatusCode::OK, Json(json!({"success": true})))
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"success": false, "error": "session not found"})),
        )
    }
}

async fn spawn_session(
    State(state): State<RunnerState>,
    Json(req): Json<SpawnSessionRequest>,
) -> (StatusCode, Json<SpawnSessionResponse>) {
    // Check if directory exists
    let dir = std::path::Path::new(&req.directory);
    if !dir.exists() {
        return (
            StatusCode::OK,
            Json(SpawnSessionResponse {
                success: false,
                session_id: None,
                error: None,
                requires_user_approval: Some(true),
                action_required: Some("CREATE_DIRECTORY".to_string()),
                directory: Some(req.directory),
            }),
        );
    }

    let session_id = req
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let flavor = req.flavor.as_deref().unwrap_or("claude");

    // Map flavor to the CLI subcommand name
    let agent_cmd = match flavor {
        "codex" => "codex",
        "gemini" => "gemini",
        "opencode" => "opencode",
        _ => "claude",
    };

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "failed to determine current executable");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpawnSessionResponse {
                    success: false,
                    session_id: None,
                    error: Some(format!("failed to get current exe: {e}")),
                    requires_user_approval: None,
                    action_required: None,
                    directory: None,
                }),
            );
        }
    };

    info!(
        session_id = %session_id,
        directory = %req.directory,
        flavor = flavor,
        "spawning session process"
    );

    let result = tokio::process::Command::new(&exe)
        .args([
            agent_cmd,
            "--hapi-starting-mode",
            "remote",
            "--started-by",
            "runner",
        ])
        .current_dir(&req.directory)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    match result {
        Ok(child) => {
            let pid = child.id();
            let mut sessions = state.sessions.lock().await;
            sessions.insert(
                session_id.clone(),
                TrackedSession {
                    session_id: session_id.clone(),
                    pid,
                    directory: req.directory,
                    started_by: "runner".to_string(),
                    metadata: None,
                },
            );
            (
                StatusCode::OK,
                Json(SpawnSessionResponse {
                    success: true,
                    session_id: Some(session_id),
                    error: None,
                    requires_user_approval: None,
                    action_required: None,
                    directory: None,
                }),
            )
        }
        Err(e) => {
            warn!(error = %e, "failed to spawn session process");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(SpawnSessionResponse {
                    success: false,
                    session_id: None,
                    error: Some(format!("failed to spawn process: {e}")),
                    requires_user_approval: None,
                    action_required: None,
                    directory: None,
                }),
            )
        }
    }
}

async fn stop_runner(State(state): State<RunnerState>) -> (StatusCode, Json<Value>) {
    info!("runner stop requested");
    if let Some(tx) = state.shutdown_tx.lock().await.take() {
        let _ = tx.send(ShutdownSource::HapiCli);
    }
    (StatusCode::OK, Json(json!({"status": "ok"})))
}
