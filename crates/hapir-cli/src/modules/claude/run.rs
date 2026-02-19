use std::collections::HashMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use hapir_shared::schemas::StartedBy as SharedStartedBy;

use crate::agent::loop_base::{run_local_remote_session, LoopOptions};
use crate::agent::runner_lifecycle::{
    create_mode_change_handler, set_controlled_by_user, RunnerLifecycle, RunnerLifecycleOptions,
};
use crate::agent::session_base::{AgentSessionBase, AgentSessionBaseOptions, SessionMode};
use crate::agent::session_factory::{bootstrap_session, SessionBootstrapOptions};
use crate::config::Configuration;
use crate::handlers::uploads;
use crate::modules::claude::hook_server::start_hook_server;
use crate::modules::claude::session::{ClaudeSession, StartedBy};
use crate::utils::message_queue::MessageQueue2;

use super::local_launcher::claude_local_launcher;
use super::remote_launcher::claude_remote_launcher;

/// Options for starting a Claude session.
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub starting_mode: Option<String>,
    pub should_start_runner: Option<bool>,
    pub claude_env_vars: Option<HashMap<String, String>>,
    pub claude_args: Option<Vec<String>>,
    pub started_by: Option<String>,
    pub runner_port: Option<u16>,
}

/// The enhanced mode type for Claude sessions.
///
/// Captures all configuration that affects how the Claude process is spawned.
/// When any of these fields change, the mode hash changes, causing the message
/// queue to treat subsequent messages as a new batch.
#[derive(Debug, Clone, Default)]
pub struct EnhancedMode {
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    pub fallback_model: Option<String>,
    pub custom_system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
}

/// Compute a deterministic hash of the enhanced mode for queue batching.
fn compute_mode_hash(mode: &EnhancedMode) -> String {
    let mut hasher = Sha256::new();
    hasher.update(mode.permission_mode.as_deref().unwrap_or(""));
    hasher.update("|");
    hasher.update(mode.model.as_deref().unwrap_or(""));
    hasher.update("|");
    hasher.update(mode.fallback_model.as_deref().unwrap_or(""));
    hasher.update("|");
    hasher.update(mode.custom_system_prompt.as_deref().unwrap_or(""));
    hasher.update("|");
    hasher.update(mode.append_system_prompt.as_deref().unwrap_or(""));
    hasher.update("|");
    hasher.update(mode.allowed_tools.join(","));
    hasher.update("|");
    hasher.update(mode.disallowed_tools.join(","));
    hex::encode(hasher.finalize())
}

/// Map a string started_by value to the session-local StartedBy enum.
fn resolve_started_by(value: Option<&str>) -> StartedBy {
    match value {
        Some("runner") => StartedBy::Runner,
        _ => StartedBy::Terminal,
    }
}

/// Map a string started_by value to the shared StartedBy enum.
fn resolve_shared_started_by(value: Option<&str>) -> SharedStartedBy {
    match value {
        Some("runner") => SharedStartedBy::Runner,
        _ => SharedStartedBy::Terminal,
    }
}

/// Determine the starting session mode from options.
fn resolve_starting_mode(options: &StartOptions, started_by: StartedBy) -> SessionMode {
    if let Some(ref mode_str) = options.starting_mode {
        match mode_str.as_str() {
            "remote" => return SessionMode::Remote,
            "local" => return SessionMode::Local,
            _ => {}
        }
    }
    // Default: local for terminal, remote for runner
    match started_by {
        StartedBy::Terminal => SessionMode::Local,
        StartedBy::Runner => SessionMode::Remote,
    }
}

/// Entry point for running a Claude agent session.
///
/// Bootstraps the session, starts the hook server, creates the message
/// queue, and enters the main local/remote loop.
pub async fn run_claude(options: StartOptions) -> anyhow::Result<()> {
    let working_directory = std::env::current_dir()?.to_string_lossy().to_string();

    let started_by = resolve_started_by(options.started_by.as_deref());
    let shared_started_by = resolve_shared_started_by(options.started_by.as_deref());
    let starting_mode = resolve_starting_mode(&options, started_by);

    debug!(
        "[runClaude] Starting in {} (startedBy={:?}, mode={:?})",
        working_directory, started_by, starting_mode
    );

    // Bootstrap session
    let config = Configuration::create()?;
    let bootstrap = bootstrap_session(
        SessionBootstrapOptions {
            flavor: "claude".to_string(),
            started_by: Some(shared_started_by),
            working_directory: Some(working_directory.clone()),
            tag: None,
            agent_state: Some(serde_json::json!({
                "controlledByUser": starting_mode == SessionMode::Local,
            })),
        },
        &config,
    )
    .await?;

    let ws_client = bootstrap.ws_client.clone();
    let api = bootstrap.api.clone();
    let session_id = bootstrap.session_info.id.clone();
    let log_path = config
        .logs_dir
        .join(format!("{}.log", &session_id))
        .to_string_lossy()
        .to_string();

    debug!("[runClaude] Session bootstrapped: {}", session_id);

    // Notify runner that this session has started (resolves the spawn awaiter)
    if let Some(port) = options.runner_port {
        let pid = std::process::id();
        if let Err(e) = crate::runner::control_client::notify_session_started(
            port,
            &session_id,
            Some(serde_json::json!({ "hostPid": pid })),
        )
        .await
        {
            warn!("[runClaude] Failed to notify runner of session start: {e}");
        }
    }

    // Start hook server for receiving Claude session notifications
    let ws_for_hook = ws_client.clone();
    let hook_server = start_hook_server(
        Arc::new(move |sid, _data| {
            let client = ws_for_hook.clone();
            let claude_sid = sid;
            tokio::spawn(async move {
                debug!(
                    "[runClaude] Hook server received Claude session ID: {}",
                    claude_sid
                );
                let csid = claude_sid.clone();
                let _ = client
                    .update_metadata(move |mut metadata| {
                        metadata.claude_session_id = Some(csid);
                        metadata
                    })
                    .await;
            });
        }),
        None,
    )
    .await?;

    let hook_port = hook_server.port;
    let hook_token = hook_server.token.clone();
    debug!("[runClaude] Hook server started on port {}", hook_port);

    // Write hook settings file so `claude --settings <path>` can find it.
    // The SessionStart hook calls back to our hook server with the session ID.
    let hook_settings_path = format!(
        "{}/.hapir/claude-hook-settings-{}.json",
        dirs_next::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        session_id
    );
    {
        let exe_path = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| "hapi".to_string());
        let settings_json = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {
                        "hooks": [
                            {
                                "type": "command",
                                "command": format!(
                                    "{} hook-forwarder --port {} --token {}",
                                    exe_path, hook_port, hook_token
                                )
                            }
                        ]
                    }
                ]
            }
        });
        if let Some(parent) = std::path::Path::new(&hook_settings_path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        match std::fs::write(&hook_settings_path, settings_json.to_string()) {
            Ok(_) => debug!("[runClaude] Wrote hook settings to {}", hook_settings_path),
            Err(e) => warn!("[runClaude] Failed to write hook settings: {}", e),
        }
    }

    // Create RunnerLifecycle and register process handlers
    let ws_for_lifecycle = ws_client.clone();
    let lifecycle = RunnerLifecycle::new(RunnerLifecycleOptions {
        ws_client: ws_for_lifecycle,
        log_tag: "runClaude".to_string(),
        stop_keep_alive: None, // Will be set after session base is created
        on_before_close: None,
        on_after_close: None,
    });
    lifecycle.register_process_handlers();

    // Set controlledByUser on session
    set_controlled_by_user(&ws_client, starting_mode).await;

    // Create MessageQueue2<EnhancedMode> with mode hash
    let initial_mode = EnhancedMode {
        permission_mode: options.permission_mode.clone(),
        model: options.model.clone(),
        ..Default::default()
    };

    let queue = Arc::new(MessageQueue2::new(compute_mode_hash));

    // Create AgentSessionBase
    let on_mode_change = create_mode_change_handler(ws_client.clone());
    let session_base = AgentSessionBase::new(AgentSessionBaseOptions {
        api: api.clone(),
        ws_client: ws_client.clone(),
        path: working_directory.clone(),
        log_path,
        session_id: None,
        queue: queue.clone(),
        on_mode_change_cb: on_mode_change,
        mode: starting_mode,
        session_label: "claude".to_string(),
        session_id_label: "claudeSessionId".to_string(),
        apply_session_id_to_metadata: Box::new(|mut metadata, sid| {
            metadata.claude_session_id = Some(sid.to_string());
            metadata
        }),
        permission_mode: bootstrap.session_info.permission_mode,
        model_mode: bootstrap.session_info.model_mode,
    });

    // Create ClaudeSession wrapping the base
    let claude_session = Arc::new(ClaudeSession {
        base: session_base.clone(),
        claude_env_vars: options.claude_env_vars.clone(),
        claude_args: Mutex::new(options.claude_args.clone()),
        mcp_servers: HashMap::new(),
        allowed_tools: None,
        hook_settings_path,
        started_by,
        starting_mode,
        local_launch_failure: Mutex::new(None),
        pending_permissions: Arc::new(Mutex::new(HashMap::new())),
    });

    // Register common RPC handlers (bash, files, directories, git, ripgrep)
    {
        let rpc_mgr = crate::rpc::RpcHandlerManager::new("_tmp_");
        crate::handlers::register_all_handlers(&rpc_mgr, &working_directory).await;
        for (method, handler) in rpc_mgr.drain_handlers().await {
            ws_client
                .register_rpc(&method, move |params| handler(params))
                .await;
        }
    }

    // Register onUserMessage RPC handler
    let queue_for_rpc = queue.clone();
    let current_mode = Arc::new(Mutex::new(initial_mode));
    let mode_for_rpc = current_mode.clone();
    ws_client
        .register_rpc("on-user-message", move |params| {
            let q = queue_for_rpc.clone();
            let mode = mode_for_rpc.clone();
            Box::pin(async move {
                info!("[runClaude] on-user-message RPC received: {:?}", params);
                let text = params
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Format attachments as @path references prepended to the message
                let attachment_refs: Vec<String> = params
                    .get("attachments")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|a| a.get("path").and_then(|p| p.as_str()))
                            .map(|p| format!("@{p}"))
                            .collect()
                    })
                    .unwrap_or_default();

                let message = if attachment_refs.is_empty() {
                    text.clone()
                } else {
                    let refs = attachment_refs.join(" ");
                    if text.is_empty() {
                        refs
                    } else {
                        format!("{refs}\n\n{text}")
                    }
                };

                if message.is_empty() {
                    return serde_json::json!({"ok": false, "reason": "empty message"});
                }

                let current = mode.lock().await.clone();

                // Handle /compact and /clear as isolate-and-clear
                let trimmed = message.trim();
                if trimmed == "/compact" || trimmed == "/clear" {
                    debug!(
                        "[runClaude] Received {} command, isolate-and-clear",
                        trimmed
                    );
                    q.push_isolate_and_clear(message, current).await;
                } else {
                    q.push(message, current).await;
                }

                serde_json::json!({"ok": true})
            })
        })
        .await;

    // Register set-session-config RPC handler
    let mode_for_config = current_mode.clone();
    let queue_for_config = queue.clone();
    ws_client
        .register_rpc("set-session-config", move |params| {
            let mode = mode_for_config.clone();
            let _q = queue_for_config.clone();
            Box::pin(async move {
                let mut m = mode.lock().await;
                if let Some(pm) = params.get("permissionMode").and_then(|v| v.as_str()) {
                    debug!("[runClaude] Permission mode changed to: {}", pm);
                    m.permission_mode = Some(pm.to_string());
                }
                if let Some(mm) = params.get("modelMode").and_then(|v| v.as_str()) {
                    debug!("[runClaude] Model mode changed to: {}", mm);
                    m.model = Some(mm.to_string());
                }
                serde_json::json!({
                    "applied": {
                        "permissionMode": m.permission_mode,
                        "modelMode": m.model,
                    }
                })
            })
        })
        .await;

    // Register permission RPC handler
    let cs_for_permission = claude_session.clone();
    ws_client
        .register_rpc("permission", move |params| {
            let cs = cs_for_permission.clone();
            Box::pin(async move {
                debug!("[runClaude] permission RPC received: {:?}", params);

                let id = params.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let approved = params
                    .get("approved")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let updated_input = params.get("updatedInput").cloned();

                // Find and respond to pending permission
                if let Some(tx) = cs.pending_permissions.lock().await.remove(id) {
                    let _ = tx.send((approved, updated_input));

                    // Update agent state to move request to completedRequests
                    let id_clone = id.to_string();
                    let status = if approved { "approved" } else { "denied" };
                    let completed_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as f64;

                    let _ = cs
                        .base
                        .ws_client
                        .update_agent_state(move |mut state| {
                            // Remove from requests
                            if let Some(requests) =
                                state.get_mut("requests").and_then(|v| v.as_object_mut())
                            {
                                if let Some(request) = requests.remove(&id_clone) {
                                    // Add to completedRequests
                                    let completed_requests = state
                                        .get_mut("completedRequests")
                                        .and_then(|v| v.as_object_mut())
                                        .map(|obj| obj.clone())
                                        .unwrap_or_default();

                                    let mut updated_completed = completed_requests.clone();
                                    let mut completed_request =
                                        request.as_object().cloned().unwrap_or_default();
                                    completed_request
                                        .insert("status".to_string(), serde_json::json!(status));
                                    completed_request.insert(
                                        "completedAt".to_string(),
                                        serde_json::json!(completed_at),
                                    );

                                    updated_completed.insert(
                                        id_clone.clone(),
                                        serde_json::json!(completed_request),
                                    );
                                    state["completedRequests"] =
                                        serde_json::json!(updated_completed);
                                }
                            }
                            state
                        })
                        .await;
                }

                serde_json::json!({"ok": true})
            })
        })
        .await;

    // Register killSession RPC handler (hub calls this to terminate the session)
    let queue_for_kill = queue.clone();
    ws_client
        .register_rpc("killSession", move |_params| {
            let q = queue_for_kill.clone();
            Box::pin(async move {
                debug!("[runClaude] killSession RPC received, closing queue");
                q.close().await;
                serde_json::json!({"ok": true})
            })
        })
        .await;

    // All RPC handlers registered — now connect the WebSocket.
    // This ensures the hub receives all rpc-register events before session-alive.
    ws_client.connect().await;

    // Create LoopOptions and enter the main loop
    let cs_for_local = claude_session.clone();
    let cs_for_remote = claude_session.clone();

    let loop_result = run_local_remote_session(LoopOptions {
        session: session_base.clone(),
        starting_mode: Some(starting_mode),
        log_tag: "runClaude".to_string(),
        run_local: Box::new(move |_base| {
            let cs = cs_for_local.clone();
            Box::pin(async move { claude_local_launcher(&cs).await })
        }),
        run_remote: Box::new(move |_base| {
            let cs = cs_for_remote.clone();
            Box::pin(async move { claude_remote_launcher(&cs).await })
        }),
        on_session_ready: None,
    })
    .await;

    // Handle exit
    debug!("[runClaude] Main loop exited");

    // Check for local launch failure
    let failure = claude_session.local_launch_failure.lock().await.take();
    if let Some(failure) = failure {
        warn!(
            "[runClaude] Local launch failed: {} ({})",
            failure.message, failure.exit_reason
        );
        ws_client
            .send_message(serde_json::json!({
                "type": "error",
                "message": failure.message,
                "exitReason": failure.exit_reason,
            }))
            .await;
    }

    // Stop hook server and clean up settings file
    hook_server.stop();
    if let Err(e) = std::fs::remove_file(&claude_session.hook_settings_path) {
        debug!("[runClaude] Failed to remove hook settings file: {}", e);
    }

    // Clean up upload directory
    uploads::cleanup_upload_dir(&session_id).await;

    // Cleanup lifecycle
    lifecycle.cleanup().await;

    if let Err(e) = loop_result {
        error!("[runClaude] Loop error: {}", e);
        lifecycle.mark_crash(&e.to_string()).await;
    }

    Ok(())
}
