use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::{debug, error, warn};

use hapir_shared::schemas::StartedBy;

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

/// The mode type for OpenCode sessions.
#[derive(Debug, Clone, Default)]
pub struct OpencodeMode {
    pub permission_mode: Option<String>,
}

/// Compute a deterministic hash of the opencode mode for queue batching.
fn compute_mode_hash(mode: &OpencodeMode) -> String {
    let mut hasher = Sha256::new();
    hasher.update(mode.permission_mode.as_deref().unwrap_or(""));
    hex::encode(hasher.finalize())
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
                "type": "assistant",
                "message": { "role": "assistant", "content": message },
            }))
            .await;
        }
        AgentMessage::TurnComplete { .. } | AgentMessage::ThinkingStatus { .. } => {}
    }
}

/// Local launcher for OpenCode.
async fn opencode_local_launcher(session: &Arc<AgentSessionBase<OpencodeMode>>) -> LoopResult {
    let working_directory = session.path.clone();
    debug!("[opencodeLocalLauncher] Starting in {}", working_directory);

    // TODO: Implement local message sync for OpenCode
    // - Create OpencodeSessionScanner that scans `~/.local/share/opencode/storage/` JSON files
    // - Use LocalSyncDriver::start(scanner, ws_client, Duration::from_secs(2), "opencodeLocalSync")

    let mut cmd = tokio::process::Command::new("opencode");
    cmd.current_dir(&working_directory);

    let _exit_status = match cmd.status().await {
        Ok(status) => {
            debug!("[opencodeLocalLauncher] opencode exited: {:?}", status);
            Some(status)
        }
        Err(e) => {
            warn!("[opencodeLocalLauncher] Failed to spawn opencode: {}", e);
            session
                .ws_client
                .send_message(serde_json::json!({
                    "type": "error",
                    "message": format!("Failed to launch opencode CLI: {}", e),
                }))
                .await;
            None
        }
    };

    let exit_reason = get_local_launch_exit_reason(&LocalLaunchContext {
        started_by: None,
        starting_mode: Some(SessionMode::Local),
    });

    match exit_reason {
        LocalLaunchExitReason::Switch => LoopResult::Switch,
        LocalLaunchExitReason::Exit => LoopResult::Exit,
    }
}
/// Remote launcher for OpenCode using ACP protocol.
async fn opencode_remote_launcher(
    session: &Arc<AgentSessionBase<OpencodeMode>>,
    backend: &Arc<AcpSdkBackend>,
) -> LoopResult {
    let working_directory = session.path.clone();
    debug!("[opencodeRemoteLauncher] Starting in {}", working_directory);

    if let Err(e) = backend.initialize().await {
        warn!(
            "[opencodeRemoteLauncher] Failed to initialize ACP backend: {}",
            e
        );
        session
            .ws_client
            .send_message(serde_json::json!({
                "type": "error",
                "message": format!("Failed to initialize opencode ACP: {}", e),
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
            warn!(
                "[opencodeRemoteLauncher] Failed to create ACP session: {}",
                e
            );
            session
                .ws_client
                .send_message(serde_json::json!({
                    "type": "error",
                    "message": format!("Failed to create opencode ACP session: {}", e),
                }))
                .await;
            return LoopResult::Exit;
        }
    };

    loop {
        let batch = match session.queue.wait_for_messages().await {
            Some(batch) => batch,
            None => {
                debug!("[opencodeRemoteLauncher] Queue closed, exiting");
                return LoopResult::Exit;
            }
        };

        let prompt = batch.message;

        debug!(
            "[opencodeRemoteLauncher] Processing message: {}",
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
            warn!("[opencodeRemoteLauncher] Prompt error: {}", e);
            session
                .ws_client
                .send_message(serde_json::json!({
                    "type": "error",
                    "message": format!("OpenCode ACP error: {}", e),
                }))
                .await;
        }

        session.on_thinking_change(false).await;

        if session.queue.is_closed().await {
            return LoopResult::Exit;
        }
    }
}
/// Entry point for running an OpenCode agent session.
pub async fn run(working_directory: &str, runner_port: Option<u16>) -> anyhow::Result<()> {
    debug!("[runOpenCode] Starting in {}", working_directory);

    let config = Configuration::create()?;
    let bootstrap = bootstrap_session(
        SessionBootstrapOptions {
            flavor: "opencode".to_string(),
            started_by: Some(StartedBy::Terminal),
            working_directory: Some(working_directory.to_string()),
            tag: None,
            agent_state: Some(serde_json::json!({
                "controlledByUser": true,
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

    debug!("[runOpenCode] Session bootstrapped: {}", session_id);

    if let Some(port) = runner_port {
        let pid = std::process::id();
        if let Err(e) = hapir_runner::control_client::notify_session_started(
            port,
            &session_id,
            Some(serde_json::json!({ "hostPid": pid })),
        )
        .await
        {
            tracing::warn!("[runOpenCode] Failed to notify runner of session start: {e}");
        }
    }

    let lifecycle = RunnerLifecycle::new(RunnerLifecycleOptions {
        ws_client: ws_client.clone(),
        log_tag: "runOpenCode".to_string(),
        stop_keep_alive: None,
        on_before_close: None,
        on_after_close: None,
    });
    lifecycle.register_process_handlers();

    set_controlled_by_user(&ws_client, SessionMode::Local).await;
    let initial_mode = OpencodeMode::default();
    let queue = Arc::new(MessageQueue2::new(compute_mode_hash));

    let on_mode_change = create_mode_change_handler(ws_client.clone());
    let session_base = AgentSessionBase::new(AgentSessionBaseOptions {
        api: api.clone(),
        ws_client: ws_client.clone(),
        path: working_directory.to_string(),
        log_path,
        session_id: None,
        queue: queue.clone(),
        on_mode_change_cb: on_mode_change,
        mode: SessionMode::Local,
        session_label: "opencode".to_string(),
        session_id_label: "opencodeSessionId".to_string(),
        apply_session_id_to_metadata: Box::new(|mut metadata, sid| {
            metadata.opencode_session_id = Some(sid.to_string());
            metadata
        }),
        permission_mode: bootstrap.session_info.permission_mode,
        model_mode: bootstrap.session_info.model_mode,
    });

    // Create ACP backend for OpenCode
    let backend = Arc::new(AcpSdkBackend::new(
        "opencode".to_string(),
        vec![
            "acp".to_string(),
            "--cwd".to_string(),
            working_directory.to_string(),
        ],
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
                    debug!("[runOpenCode] Received {} command", trimmed);
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
                    debug!("[runOpenCode] Permission mode changed to: {}", pm);
                    m.permission_mode = Some(pm.to_string());
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
                debug!("[runOpenCode] killSession RPC received");
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
                debug!("[runOpenCode] abort RPC received");
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
        crate::terminal::setup_terminal(&ws_client, &session_id, working_directory).await;

    ws_client.connect().await;

    let sb_for_local = session_base.clone();
    let sb_for_remote = session_base.clone();
    let backend_for_remote = backend.clone();

    let loop_result = run_local_remote_session(LoopOptions {
        session: session_base.clone(),
        starting_mode: Some(SessionMode::Local),
        log_tag: "runOpenCode".to_string(),
        run_local: Box::new(move |_base| {
            let s = sb_for_local.clone();
            Box::pin(async move { opencode_local_launcher(&s).await })
        }),
        run_remote: Box::new(move |_base| {
            let s = sb_for_remote.clone();
            let b = backend_for_remote.clone();
            Box::pin(async move { opencode_remote_launcher(&s, &b).await })
        }),
        on_session_ready: None,
    })
    .await;

    debug!("[runOpenCode] Main loop exited");
    let _ = backend.disconnect().await;
    terminal_mgr.close_all().await;
    lifecycle.cleanup().await;

    if let Err(e) = loop_result {
        error!("[runOpenCode] Loop error: {}", e);
        lifecycle.mark_crash(&e.to_string()).await;
    }

    Ok(())
}
