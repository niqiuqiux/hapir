use std::collections::HashMap;
use std::sync::Arc;

use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use super::types::*;

/// Callback invoked when a session webhook arrives (keyed by PID).
pub type SessionAwaiter = Box<dyn FnOnce(TrackedSession) + Send>;

/// Shared state for the control server
#[derive(Clone)]
pub struct RunnerState {
    pub sessions: Arc<Mutex<HashMap<String, TrackedSession>>>,
    pub shutdown_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<ShutdownSource>>>>,
    pub shutdown_tx_mpsc: Arc<Mutex<Option<tokio::sync::mpsc::Sender<ShutdownSource>>>>,
    /// PID → awaiter: resolved when the spawned process calls /session-started.
    pub pid_to_awaiter: Arc<Mutex<HashMap<u32, SessionAwaiter>>>,
}

impl RunnerState {
    pub fn new(shutdown_tx: tokio::sync::oneshot::Sender<ShutdownSource>) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            shutdown_tx: Arc::new(Mutex::new(Some(shutdown_tx))),
            shutdown_tx_mpsc: Arc::new(Mutex::new(None)),
            pid_to_awaiter: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn new_mpsc(shutdown_tx: tokio::sync::mpsc::Sender<ShutdownSource>) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            shutdown_tx: Arc::new(Mutex::new(None)),
            shutdown_tx_mpsc: Arc::new(Mutex::new(Some(shutdown_tx))),
            pid_to_awaiter: Arc::new(Mutex::new(HashMap::new())),
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

/// Cleanup worktree if one was created and the child process is dead (or no PID).
/// Matches TS `maybeCleanupWorktree` behavior.
async fn maybe_cleanup_worktree(
    worktree_info: &Option<super::worktree::WorktreeInfo>,
    pid: Option<u32>,
) {
    let Some(wt) = worktree_info else { return };
    // If child is still alive, skip cleanup
    if let Some(pid) = pid
        && crate::persistence::is_process_alive(pid)
    {
        info!(pid = pid, "skipping worktree cleanup; child still running");
        return;
    }
    if let Err(e) = super::worktree::remove_worktree(&wt.base_path, &wt.worktree_path).await {
        warn!("failed to remove worktree {}: {e}", wt.worktree_path);
    }
}

async fn session_started(
    State(state): State<RunnerState>,
    Json(payload): Json<SessionStartedPayload>,
) -> (StatusCode, Json<Value>) {
    info!(session_id = %payload.session_id, "session started webhook");

    // Extract hostPid from metadata (matches TS: sessionMetadata.hostPid)
    let host_pid = payload.metadata.as_ref()
        .and_then(|m| m.get("hostPid"))
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);

    let mut sessions = state.sessions.lock().await;

    if let Some(pid) = host_pid {
        // Look up existing runner-spawned session by PID
        let mut found_key: Option<String> = None;
        for (key, session) in sessions.iter_mut() {
            if session.pid == Some(pid) && session.started_by == "runner" {
                // Update runner-spawned session with reported data
                session.session_id = payload.session_id.clone();
                session.metadata = payload.metadata.clone();
                session.webhook_received = true;
                found_key = Some(key.clone());
                break;
            }
        }

        if let Some(key) = found_key {
            // Resolve awaiter for this PID
            let awaiter = state.pid_to_awaiter.lock().await.remove(&pid);
            if let Some(callback) = awaiter
                && let Some(session) = sessions.get(&key)
            {
                callback(session.clone());
            }
        } else {
            // New session started externally (PID not tracked by runner)
            sessions.insert(
                payload.session_id.clone(),
                TrackedSession {
                    session_id: payload.session_id.clone(),
                    pid: Some(pid),
                    directory: String::new(),
                    started_by: "hapi directly - likely by user from terminal".to_string(),
                    metadata: payload.metadata,
                    directory_created: None,
                    message: None,
                    stderr_tail: None,
                    webhook_received: true,
                },
            );
        }
    } else {
        info!("session webhook missing hostPid for sessionId: {}", payload.session_id);
    }

    (StatusCode::OK, Json(json!({"status": "ok"})))
}

async fn list_sessions(
    State(state): State<RunnerState>,
) -> Json<ListSessionsResponse> {
    let sessions = state.sessions.lock().await;
    Json(ListSessionsResponse {
        children: sessions
            .values()
            // Only show sessions after webhook has populated the real session ID
            // (matches TS: filter by happySessionId !== undefined)
            .filter(|s| s.webhook_received)
            .map(|s| ListSessionEntry {
                started_by: s.started_by.clone(),
                happy_session_id: s.session_id.clone(),
                pid: s.pid.unwrap_or(0),
            })
            .collect(),
    })
}

/// Stop a session by ID (or PID- prefix fallback). Shared logic used by both HTTP handler and RPC.
pub async fn do_stop_session(state: &RunnerState, session_id: &str) -> Value {
    let mut sessions = state.sessions.lock().await;

    // Find session by happySessionId or PID- prefix fallback (matches TS behavior)
    let found_key = sessions.iter().find_map(|(key, session)| {
        if session.session_id == session_id {
            return Some(key.clone());
        }
        // PID- prefix fallback: "PID-1234" matches session with pid=1234
        if let Some(pid_str) = session_id.strip_prefix("PID-")
            && let Ok(target_pid) = pid_str.parse::<u32>()
            && session.pid == Some(target_pid)
        {
            return Some(key.clone());
        }
        None
    });

    if let Some(key) = found_key
        && let Some(session) = sessions.remove(&key)
    {
        if let Some(pid) = session.pid {
            #[cfg(unix)]
            {
                // Kill the entire process group (matches TS killProcessTree behavior).
                // Sessions are spawned with setsid(), so pid == pgid.
                // Try process group first, fall back to single process.
                let pgid_result = unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
                if pgid_result != 0 {
                    // Fallback: kill single process
                    unsafe { libc::kill(pid as i32, libc::SIGTERM); }
                }
            }
            let _ = pid;
        }
        info!(session_id = %session_id, "session stopped");
        return json!({"success": true});
    }

    json!({"success": false, "error": "session not found"})
}

async fn stop_session(
    State(state): State<RunnerState>,
    Json(req): Json<StopSessionRequest>,
) -> (StatusCode, Json<Value>) {
    let result = do_stop_session(&state, &req.session_id).await;
    // TS always returns 200 regardless of success/failure
    (StatusCode::OK, Json(result))
}

/// Spawn a session process. Shared logic used by both HTTP handler and RPC.
pub async fn do_spawn_session(state: &RunnerState, req: SpawnSessionRequest) -> SpawnSessionResponse {
    let session_type = req.session_type.as_deref().unwrap_or("simple");
    let dir = std::path::Path::new(&req.directory);
    let mut directory_created = false;
    let mut spawn_directory = req.directory.clone();
    let mut worktree_info: Option<super::worktree::WorktreeInfo> = None;

    if session_type == "simple" {
        // Simple session: create directory if needed
        if !dir.exists() {
            let approved = req.approved_new_directory_creation.unwrap_or(true);
            if !approved {
                return SpawnSessionResponse {
                    r#type: "requestToApproveDirectoryCreation".to_string(),
                    session_id: None,
                    error: None,
                    directory: Some(req.directory),
                };
            }
            if let Err(e) = std::fs::create_dir_all(&req.directory) {
                let msg = match e.kind() {
                    std::io::ErrorKind::PermissionDenied => {
                        format!("Permission denied. You don't have write access to create a folder at '{}'. Try using a different path or check your permissions.", req.directory)
                    }
                    _ => {
                        let raw = e.raw_os_error();
                        match raw {
                            Some(28) => "No space left on device. Your disk is full. Please free up some space and try again.".to_string(),
                            Some(30) => format!("The file system is read-only. Cannot create directories at '{}'. Please choose a writable location.", req.directory),
                            Some(20) => format!("A file already exists at '{}' or in the parent path. Cannot create a directory here. Please choose a different location.", req.directory),
                            _ => format!("Unable to create directory at '{}'. System error: {e}. Please verify the path is valid and you have the necessary permissions.", req.directory),
                        }
                    }
                };
                return SpawnSessionResponse {
                    r#type: "error".to_string(),
                    session_id: None,
                    error: Some(msg),
                    directory: None,
                };
            }
            directory_created = true;
            info!(directory = %req.directory, "created directory for session");
        }
    } else {
        // Worktree session: base directory must exist
        if !dir.exists() {
            return SpawnSessionResponse {
                r#type: "error".to_string(),
                session_id: None,
                error: Some(format!("Worktree sessions require an existing Git repository. Directory not found: {}", req.directory)),
                directory: None,
            };
        }
    }

    // Create worktree if requested
    if session_type == "worktree" {
        match super::worktree::create_worktree(&req.directory, req.worktree_name.as_deref()).await {
            Ok(wt) => {
                spawn_directory = wt.worktree_path.clone();
                info!(worktree = %wt.worktree_path, branch = %wt.branch, "created worktree");
                worktree_info = Some(wt);
            }
            Err(e) => {
                return SpawnSessionResponse {
                    r#type: "error".to_string(),
                    session_id: None,
                    error: Some(e),
                    directory: None,
                };
            }
        }
    }

    let session_id = req
        .session_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let agent = req.agent.as_deref().unwrap_or("claude");

    let agent_cmd = match agent {
        "codex" => "codex",
        "gemini" => "gemini",
        "opencode" => "opencode",
        _ => "claude",
    };

    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "failed to determine current executable");
            return SpawnSessionResponse {
                r#type: "error".to_string(),
                session_id: None,
                error: Some(format!("failed to get current exe: {e}")),
                directory: None,
            };
        }
    };

    info!(
        session_id = %session_id,
        directory = %req.directory,
        agent = agent,
        "spawning session process"
    );

    // Build args
    let mut args = vec![agent_cmd.to_string()];
    if let Some(ref resume_id) = req.resume_session_id {
        if agent_cmd == "codex" {
            args.push("resume".to_string());
            args.push(resume_id.clone());
        } else {
            args.push("--resume".to_string());
            args.push(resume_id.clone());
        }
    }
    args.extend(["--hapir-starting-mode".to_string(), "remote".to_string(), "--started-by".to_string(), "runner".to_string()]);
    if let Some(ref model) = req.model
        && agent_cmd != "opencode"
    {
        args.push("--model".to_string());
        args.push(model.clone());
    }
    if req.yolo == Some(true) {
        args.push("--yolo".to_string());
    }

    // Build command — capture stderr for debugging, detach on Unix
    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(&args)
        .current_dir(&spawn_directory)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Detach on Unix so sessions survive runner shutdown
    #[cfg(unix)]
    {
        // Safety: pre_exec runs in the forked child before exec.
        // setsid() creates a new session so the child is fully detached.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    let mut codex_temp_dir: Option<std::path::PathBuf> = None;

    if let Some(ref token) = req.token {
        if agent_cmd == "codex" {
            // Create temp dir with auth.json for Codex
            let codex_dir = std::env::temp_dir().join(format!("hapi-codex-{}", uuid::Uuid::new_v4()));
            if std::fs::create_dir_all(&codex_dir).is_ok() {
                let auth_path = codex_dir.join("auth.json");
                if std::fs::write(&auth_path, token).is_ok() {
                    cmd.env("CODEX_HOME", &codex_dir);
                    codex_temp_dir = Some(codex_dir);
                }
            }
        } else if agent_cmd == "claude" {
            cmd.env("CLAUDE_CODE_OAUTH_TOKEN", token);
        }
    }

    // Pass worktree metadata as environment variables
    if let Some(ref wt) = worktree_info {
        cmd.env("HAPI_WORKTREE_BASE_PATH", &wt.base_path);
        cmd.env("HAPI_WORKTREE_BRANCH", &wt.branch);
        cmd.env("HAPI_WORKTREE_NAME", &wt.name);
        cmd.env("HAPI_WORKTREE_PATH", &wt.worktree_path);
        cmd.env("HAPI_WORKTREE_CREATED_AT", wt.created_at.to_string());
    }

    // Pre-register awaiter BEFORE spawning so the webhook can't arrive before we're ready.
    // We use a placeholder PID (0) and update it after spawn.
    let (awaiter_tx, awaiter_rx) = tokio::sync::oneshot::channel::<TrackedSession>();

    let result = cmd.spawn();

    match result {
        Ok(mut child) => {
            let pid = match child.id() {
                Some(p) => p,
                None => {
                    warn!("failed to spawn process - no PID returned");
                    maybe_cleanup_worktree(&worktree_info, None).await;
                    return SpawnSessionResponse {
                        r#type: "error".to_string(),
                        session_id: None,
                        error: Some("Failed to spawn process - no PID returned".to_string()),
                        directory: None,
                    };
                }
            };

            info!(pid = pid, session_id = %session_id, "spawned session process");

            // Register awaiter with real PID immediately so webhook can resolve it
            {
                let mut awaiters = state.pid_to_awaiter.lock().await;
                awaiters.insert(pid, Box::new(move |session| {
                    let _ = awaiter_tx.send(session);
                }));
            }

            let dir_msg = if directory_created {
                Some(format!("The path '{}' did not exist. We created a new folder and spawned a new session there.", req.directory))
            } else {
                None
            };

            // Insert tracked session
            {
                let mut sessions = state.sessions.lock().await;
                sessions.insert(
                    session_id.clone(),
                    TrackedSession {
                        session_id: session_id.clone(),
                        pid: Some(pid),
                        directory: req.directory,
                        started_by: "runner".to_string(),
                        metadata: None,
                        directory_created: Some(directory_created),
                        message: dir_msg,
                        stderr_tail: None,
                        webhook_received: false,
                    },
                );
            }

            // Spawn a background task to capture stderr and handle child exit
            let state_clone = state.clone();
            let sid = session_id.clone();
            let codex_temp_dir_clone = codex_temp_dir.clone();
            tokio::spawn(async move {
                // Capture stderr and forward child logs in real-time
                let stderr = child.stderr.take();
                if let Some(stderr) = stderr {
                    let state_for_stderr = state_clone.clone();
                    let sid_for_stderr = sid.clone();
                    tokio::spawn(async move {
                        use tokio::io::AsyncBufReadExt;
                        let reader = tokio::io::BufReader::new(stderr);
                        let mut lines = reader.lines();
                        let mut tail = String::new();
                        while let Ok(Some(line)) = lines.next_line().await {
                            debug!(session_id = %sid_for_stderr, "[child] {}", line);
                            tail.push_str(&line);
                            tail.push('\n');
                            // Keep only last 4000 chars
                            if tail.len() > 4000 {
                                let start = tail.len() - 4000;
                                tail = tail[start..].to_string();
                            }
                            // Update tracked session
                            let mut sessions = state_for_stderr.sessions.lock().await;
                            if let Some(s) = sessions.get_mut(&sid_for_stderr) {
                                s.stderr_tail = Some(tail.clone());
                            }
                        }
                    });
                }

                // Wait for child exit
                match child.wait().await {
                    Ok(status) => {
                        let code = status.code();
                        #[cfg(unix)]
                        let signal = std::os::unix::process::ExitStatusExt::signal(&status);
                        #[cfg(not(unix))]
                        let signal: Option<i32> = None;
                        info!(pid = pid, code = ?code, signal = ?signal, session_id = %sid, "child process exited");
                        // Match TS: log stderr if code !== 0 || signal
                        if code != Some(0) || signal.is_some() {
                            let sessions = state_clone.sessions.lock().await;
                            if let Some(s) = sessions.get(&sid)
                                && let Some(ref tail) = s.stderr_tail
                            {
                                warn!(pid = pid, session_id = %sid, "stderr tail on exit:\n{}", tail);
                            }
                        }
                    }
                    Err(e) => {
                        warn!(pid = pid, error = %e, session_id = %sid, "child process error");
                    }
                }

                // Remove from tracked sessions
                state_clone.sessions.lock().await.remove(&sid);
                // Remove awaiter if still pending
                state_clone.pid_to_awaiter.lock().await.remove(&pid);
                // Clean up codex temp directory
                if let Some(ref dir) = codex_temp_dir_clone {
                    let _ = std::fs::remove_dir_all(dir);
                }
            });

            // Wait for session webhook (up to 15 seconds)
            match tokio::time::timeout(std::time::Duration::from_secs(15), awaiter_rx).await {
                Ok(Ok(completed_session)) => {
                    info!(session_id = %session_id, "session fully spawned with webhook");
                    SpawnSessionResponse {
                        r#type: "success".to_string(),
                        session_id: Some(completed_session.session_id),
                        error: None,
                        directory: None,
                    }
                }
                _ => {
                    // Timeout or channel closed — remove awaiter
                    state.pid_to_awaiter.lock().await.remove(&pid);
                    // Collect stderr tail for error reporting
                    let stderr_tail = {
                        let sessions = state.sessions.lock().await;
                        sessions.get(&session_id).and_then(|s| s.stderr_tail.clone())
                    };
                    if let Some(ref tail) = stderr_tail {
                        warn!(pid = pid, "session webhook timeout, stderr tail:\n{}", tail);
                    }
                    warn!(pid = pid, session_id = %session_id, "session webhook timeout after 15s");
                    maybe_cleanup_worktree(&worktree_info, Some(pid)).await;
                    let error_msg = match stderr_tail {
                        Some(tail) => format!("Session webhook timeout for PID {pid}: {tail}"),
                        None => format!("Session webhook timeout for PID {pid}"),
                    };
                    SpawnSessionResponse {
                        r#type: "error".to_string(),
                        session_id: None,
                        error: Some(error_msg),
                        directory: None,
                    }
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "failed to spawn session process");
            maybe_cleanup_worktree(&worktree_info, None).await;
            SpawnSessionResponse {
                r#type: "error".to_string(),
                session_id: None,
                error: Some(format!("failed to spawn process: {e}")),
                directory: None,
            }
        }
    }
}

async fn spawn_session(
    State(state): State<RunnerState>,
    Json(req): Json<SpawnSessionRequest>,
) -> (StatusCode, Json<Value>) {
    let resp = do_spawn_session(&state, req).await;
    match resp.r#type.as_str() {
        "success" => (
            StatusCode::OK,
            Json(json!({
                "success": true,
                "sessionId": resp.session_id,
                "approvedNewDirectoryCreation": true,
            })),
        ),
        "requestToApproveDirectoryCreation" => (
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "requiresUserApproval": true,
                "actionRequired": "CREATE_DIRECTORY",
                "directory": resp.directory,
            })),
        ),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "success": false,
                "error": resp.error,
            })),
        ),
    }
}

/// Kill all tracked session processes and return the session IDs that had received webhooks
/// (i.e. are known to the hub). Used during runner graceful shutdown.
pub async fn stop_all_sessions(state: &RunnerState) -> Vec<String> {
    let mut sessions = state.sessions.lock().await;
    let mut ended_session_ids = Vec::new();

    for (_, session) in sessions.drain() {
        if let Some(pid) = session.pid {
            #[cfg(unix)]
            {
                let pgid_result = unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
                if pgid_result != 0 {
                    unsafe { libc::kill(pid as i32, libc::SIGTERM); }
                }
            }
            let _ = pid;
        }
        if session.webhook_received {
            ended_session_ids.push(session.session_id);
        }
    }

    ended_session_ids
}

/// Request runner shutdown. Shared logic used by both HTTP handler and RPC.
pub async fn do_stop_runner(state: &RunnerState, source: ShutdownSource) -> Value {
    info!("runner stop requested (source: {:?})", source);
    // Try mpsc first, then oneshot
    if let Some(tx) = state.shutdown_tx_mpsc.lock().await.as_ref() {
        let _ = tx.send(source).await;
    } else if let Some(tx) = state.shutdown_tx.lock().await.take() {
        let _ = tx.send(source);
    }
    json!({"status": "ok"})
}

async fn stop_runner(State(state): State<RunnerState>) -> (StatusCode, Json<Value>) {
    // Delay shutdown so the HTTP response gets sent first (matches TS 50ms delay)
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        do_stop_runner(&state, ShutdownSource::HapiCli).await;
    });
    (StatusCode::OK, Json(json!({"status": "stopping"})))
}
