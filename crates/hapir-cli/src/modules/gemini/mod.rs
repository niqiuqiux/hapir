use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::{debug, error, warn};

use hapir_shared::schemas::StartedBy as SharedStartedBy;

use crate::agent::local_launch_policy::{
    LocalLaunchContext, LocalLaunchExitReason, get_local_launch_exit_reason,
};
use crate::agent::loop_base::{LoopOptions, LoopResult, run_local_remote_session};
use crate::agent::runner_lifecycle::{
    RunnerLifecycle, RunnerLifecycleOptions, create_mode_change_handler, set_controlled_by_user,
};
use crate::agent::session_base::{AgentSessionBase, AgentSessionBaseOptions, SessionMode};
use crate::agent::session_factory::{SessionBootstrapOptions, bootstrap_session};
use hapir_acp::acp_sdk::backend::AcpSdkBackend;
use hapir_acp::types::{AgentBackend, AgentMessage, AgentSessionConfig, PromptContent};
use hapir_infra::config::Configuration;
use hapir_infra::utils::message_queue::MessageQueue2;
use hapir_infra::ws::session_client::WsSessionClient;

/// The mode type for Gemini sessions.
#[derive(Debug, Clone, Default)]
pub struct GeminiMode {
    pub permission_mode: Option<String>,
    pub model: Option<String>,
}

/// Compute a deterministic hash of the gemini mode for queue batching.
fn compute_mode_hash(mode: &GeminiMode) -> String {
    let mut hasher = Sha256::new();
    hasher.update(mode.permission_mode.as_deref().unwrap_or(""));
    hasher.update("|");
    hasher.update(mode.model.as_deref().unwrap_or(""));
    hex::encode(hasher.finalize())
}

/// Resolve the starting session mode from an optional string.
fn resolve_starting_mode(mode_str: Option<&str>, started_by: SharedStartedBy) -> SessionMode {
    if let Some(s) = mode_str {
        match s {
            "remote" => return SessionMode::Remote,
            "local" => return SessionMode::Local,
            _ => {}
        }
    }
    match started_by {
        SharedStartedBy::Terminal => SessionMode::Local,
        SharedStartedBy::Runner => SessionMode::Remote,
    }
}
/// Forward an `AgentMessage` from the ACP backend to the WebSocket session.
async fn forward_agent_message(ws: &WsSessionClient, msg: AgentMessage) {
    match msg {
        AgentMessage::Text { text } => {
            ws.send_message(serde_json::json!({
                "type": "assistant",
                "message": { "role": "assistant", "content": text },
            }))
            .await;
        }
        AgentMessage::TextDelta {
            message_id,
            text,
            is_final,
        } => {
            ws.send_message_delta(&message_id, &text, is_final).await;
        }
        AgentMessage::ToolCall {
            id,
            name,
            input,
            status,
        } => {
            ws.send_message(serde_json::json!({
                "type": "tool_call",
                "toolCall": { "id": id, "name": name, "input": input, "status": status },
            }))
            .await;
        }
        AgentMessage::ToolResult { id, output, status } => {
            ws.send_message(serde_json::json!({
                "type": "tool_result",
                "toolResult": { "id": id, "output": output, "status": status },
            }))
            .await;
        }
        AgentMessage::Plan { items } => {
            ws.send_message(serde_json::json!({
                "type": "plan",
                "entries": items,
            }))
            .await;
        }
        AgentMessage::Error { message } => {
            ws.send_message(serde_json::json!({
                "type": "error",
                "message": message,
            }))
            .await;
        }
        AgentMessage::TurnComplete { .. } => {}
    }
}

/// Entry point for running a Gemini agent session.
pub async fn run(working_directory: &str, runner_port: Option<u16>) -> anyhow::Result<()> {
    let working_directory = working_directory.to_string();
    let started_by = SharedStartedBy::Terminal;
    let starting_mode = resolve_starting_mode(None, started_by);
    debug!(
        "[runGemini] Starting in {} (startedBy={:?}, mode={:?})",
        working_directory, started_by, starting_mode
    );

    let config = Configuration::create()?;
    let bootstrap = bootstrap_session(
        SessionBootstrapOptions {
            flavor: "gemini".to_string(),
            started_by: Some(started_by),
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
    let session_id = bootstrap.session_info.id.clone();
    let log_path = config
        .logs_dir
        .join(format!("{}.log", &session_id))
        .to_string_lossy()
        .to_string();

    debug!("[runGemini] Session bootstrapped: {}", session_id);

    if let Some(port) = runner_port {
        let pid = std::process::id();
        if let Err(e) = hapir_runner::control_client::notify_session_started(
            port,
            &session_id,
            Some(serde_json::json!({ "hostPid": pid })),
        )
        .await
        {
            tracing::warn!("[runGemini] Failed to notify runner of session start: {e}");
        }
    }

    let lifecycle = RunnerLifecycle::new(RunnerLifecycleOptions {
        ws_client: ws_client.clone(),
        log_tag: "runGemini".to_string(),
        stop_keep_alive: None,
        on_before_close: None,
        on_after_close: None,
    });
    lifecycle.register_process_handlers();

    set_controlled_by_user(&ws_client, starting_mode).await;

    let initial_mode = GeminiMode::default();
    let queue = Arc::new(MessageQueue2::new(compute_mode_hash));

    let on_mode_change = create_mode_change_handler(ws_client.clone());
    let session_base = AgentSessionBase::new(AgentSessionBaseOptions {
        api: bootstrap.api.clone(),
        ws_client: ws_client.clone(),
        path: working_directory.clone(),
        log_path,
        session_id: None,
        queue: queue.clone(),
        on_mode_change_cb: on_mode_change,
        mode: starting_mode,
        session_label: "gemini".to_string(),
        session_id_label: "geminiSessionId".to_string(),
        apply_session_id_to_metadata: Box::new(|mut metadata, sid| {
            metadata.gemini_session_id = Some(sid.to_string());
            metadata
        }),
        permission_mode: bootstrap.session_info.permission_mode,
        model_mode: bootstrap.session_info.model_mode,
    });
    // Create ACP backend for Gemini
    let backend = Arc::new(AcpSdkBackend::new(
        "gemini".to_string(),
        vec!["--experimental-acp".to_string()],
        None,
    ));

    // Register on-user-message RPC handler
    let queue_for_rpc = queue.clone();
    let current_mode = Arc::new(Mutex::new(initial_mode));
    let mode_for_rpc = current_mode.clone();
    ws_client
        .register_rpc("on-user-message", move |params| {
            let q = queue_for_rpc.clone();
            let mode = mode_for_rpc.clone();
            Box::pin(async move {
                let message = params
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if message.is_empty() {
                    return serde_json::json!({"ok": false, "reason": "empty message"});
                }

                let current = mode.lock().await.clone();

                let trimmed = message.trim();
                if trimmed == "/compact" || trimmed == "/clear" {
                    debug!("[runGemini] Received {} command", trimmed);
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
    ws_client
        .register_rpc("set-session-config", move |params| {
            let mode = mode_for_config.clone();
            Box::pin(async move {
                let mut m = mode.lock().await;
                if let Some(pm) = params.get("permissionMode").and_then(|v| v.as_str()) {
                    debug!("[runGemini] Permission mode changed to: {}", pm);
                    m.permission_mode = Some(pm.to_string());
                }
                if let Some(model) = params.get("model").and_then(|v| v.as_str()) {
                    debug!("[runGemini] Model changed to: {}", model);
                    m.model = Some(model.to_string());
                }
                serde_json::json!({"ok": true})
            })
        })
        .await;

    // Register killSession RPC handler
    let queue_for_kill = queue.clone();
    let backend_for_kill = backend.clone();
    ws_client
        .register_rpc("killSession", move |_params| {
            let q = queue_for_kill.clone();
            let b = backend_for_kill.clone();
            Box::pin(async move {
                debug!("[runGemini] killSession RPC received");
                q.close().await;
                let _ = b.disconnect().await;
                serde_json::json!({"ok": true})
            })
        })
        .await;

    // Register abort RPC handler
    let backend_for_abort = backend.clone();
    let sb_for_abort = session_base.clone();
    ws_client
        .register_rpc("abort", move |_params| {
            let b = backend_for_abort.clone();
            let sb = sb_for_abort.clone();
            Box::pin(async move {
                debug!("[runGemini] abort RPC received");
                if let Some(sid) = sb.session_id.lock().await.clone() {
                    let _ = b.cancel_prompt(&sid).await;
                }
                sb.on_thinking_change(false).await;
                serde_json::json!({"ok": true})
            })
        })
        .await;
    // Set up terminal manager
    let terminal_mgr =
        crate::terminal::setup_terminal(&ws_client, &session_id, &working_directory).await;

    ws_client.connect().await;

    let sb_for_local = session_base.clone();
    let sb_for_remote = session_base.clone();
    let backend_for_remote = backend.clone();

    let loop_result = run_local_remote_session(LoopOptions {
        session: session_base.clone(),
        starting_mode: Some(starting_mode),
        log_tag: "runGemini".to_string(),
        run_local: Box::new(move |_base| {
            let sb = sb_for_local.clone();
            Box::pin(async move { gemini_local_launcher(&sb).await })
        }),
        run_remote: Box::new(move |_base| {
            let sb = sb_for_remote.clone();
            let b = backend_for_remote.clone();
            Box::pin(async move { gemini_remote_launcher(&sb, &b).await })
        }),
        on_session_ready: None,
    })
    .await;

    debug!("[runGemini] Main loop exited");
    let _ = backend.disconnect().await;
    terminal_mgr.close_all().await;
    lifecycle.cleanup().await;

    if let Err(e) = loop_result {
        error!("[runGemini] Loop error: {}", e);
        lifecycle.mark_crash(&e.to_string()).await;
    }

    Ok(())
}

/// Local launcher for Gemini.
async fn gemini_local_launcher(session: &Arc<AgentSessionBase<GeminiMode>>) -> LoopResult {
    let working_directory = session.path.clone();
    debug!("[geminiLocalLauncher] Starting in {}", working_directory);

    let mut cmd = tokio::process::Command::new("gemini");
    cmd.current_dir(&working_directory);

    match cmd.status().await {
        Ok(status) => {
            debug!("[geminiLocalLauncher] Gemini process exited: {:?}", status);
        }
        Err(e) => {
            warn!("[geminiLocalLauncher] Failed to spawn gemini: {}", e);
            session
                .ws_client
                .send_message(serde_json::json!({
                    "type": "error",
                    "message": format!("Failed to launch gemini CLI: {}", e),
                }))
                .await;
        }
    }

    let exit_reason = get_local_launch_exit_reason(&LocalLaunchContext {
        started_by: Some(SharedStartedBy::Terminal),
        starting_mode: Some(*session.mode.lock().await),
    });

    debug!("[geminiLocalLauncher] Exit reason: {:?}", exit_reason);

    match exit_reason {
        LocalLaunchExitReason::Switch => LoopResult::Switch,
        LocalLaunchExitReason::Exit => LoopResult::Exit,
    }
}
/// Remote launcher for Gemini using ACP protocol.
async fn gemini_remote_launcher(
    session: &Arc<AgentSessionBase<GeminiMode>>,
    backend: &Arc<AcpSdkBackend>,
) -> LoopResult {
    let working_directory = session.path.clone();
    debug!("[geminiRemoteLauncher] Starting in {}", working_directory);

    // Initialize ACP backend and create a session
    if let Err(e) = backend.initialize().await {
        warn!(
            "[geminiRemoteLauncher] Failed to initialize ACP backend: {}",
            e
        );
        session
            .ws_client
            .send_message(serde_json::json!({
                "type": "error",
                "message": format!("Failed to initialize gemini ACP: {}", e),
            }))
            .await;
        return LoopResult::Exit;
    }

    let acp_session_id = match backend
        .new_session(AgentSessionConfig {
            cwd: working_directory.clone(),
            mcp_servers: vec![],
        })
        .await
    {
        Ok(sid) => {
            session.on_session_found(&sid).await;
            sid
        }
        Err(e) => {
            warn!("[geminiRemoteLauncher] Failed to create ACP session: {}", e);
            session
                .ws_client
                .send_message(serde_json::json!({
                    "type": "error",
                    "message": format!("Failed to create gemini ACP session: {}", e),
                }))
                .await;
            return LoopResult::Exit;
        }
    };

    loop {
        let batch = match session.queue.wait_for_messages().await {
            Some(batch) => batch,
            None => {
                debug!("[geminiRemoteLauncher] Queue closed, exiting");
                return LoopResult::Exit;
            }
        };

        let prompt = batch.message;

        debug!(
            "[geminiRemoteLauncher] Processing message: {}",
            if prompt.len() > 100 {
                format!("{}...", &prompt[..prompt.floor_char_boundary(100)])
            } else {
                prompt.clone()
            }
        );

        session.on_thinking_change(true).await;

        let ws_for_update = session.ws_client.clone();
        let on_update: Box<dyn Fn(AgentMessage) + Send + Sync> = Box::new(move |msg| {
            let ws = ws_for_update.clone();
            tokio::spawn(async move {
                forward_agent_message(&ws, msg).await;
            });
        });

        let content = vec![PromptContent::Text { text: prompt }];
        if let Err(e) = backend.prompt(&acp_session_id, content, on_update).await {
            warn!("[geminiRemoteLauncher] Prompt error: {}", e);
            session
                .ws_client
                .send_message(serde_json::json!({
                    "type": "error",
                    "message": format!("Gemini ACP error: {}", e),
                }))
                .await;
        }

        session.on_thinking_change(false).await;

        if session.queue.is_closed().await {
            return LoopResult::Exit;
        }
    }
}
