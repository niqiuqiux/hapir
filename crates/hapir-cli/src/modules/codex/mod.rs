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
use hapir_acp::codex_app_server::backend::CodexAppServerBackend;
use hapir_acp::types::{AgentBackend, AgentMessage, AgentSessionConfig, PromptContent};
use hapir_infra::config::Configuration;
use hapir_infra::utils::message_queue::MessageQueue2;
use hapir_infra::ws::session_client::WsSessionClient;

#[derive(Debug, Clone, Default)]
pub struct CodexMode {
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    pub collaboration_mode: Option<String>,
}

fn compute_mode_hash(mode: &CodexMode) -> String {
    let mut hasher = Sha256::new();
    hasher.update(mode.permission_mode.as_deref().unwrap_or(""));
    hasher.update("|");
    hasher.update(mode.model.as_deref().unwrap_or(""));
    hasher.update("|");
    hasher.update(mode.collaboration_mode.as_deref().unwrap_or(""));
    hex::encode(hasher.finalize())
}

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

fn codex_message(data: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "role": "assistant",
        "content": { "type": "codex", "data": data }
    })
}

async fn forward_agent_message(ws: &WsSessionClient, msg: AgentMessage) {
    match msg {
        AgentMessage::Text { text } => {
            ws.send_message(codex_message(serde_json::json!({
                "type": "message",
                "message": text,
            })))
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
            ws.send_message(codex_message(serde_json::json!({
                "type": "tool-call",
                "callId": id,
                "name": name,
                "input": input,
                "status": status,
            })))
            .await;
        }
        AgentMessage::ToolResult { id, output, status } => {
            ws.send_message(codex_message(serde_json::json!({
                "type": "tool-call-result",
                "callId": id,
                "output": output,
                "status": status,
            })))
            .await;
        }
        AgentMessage::Plan { items } => {
            ws.send_message(codex_message(serde_json::json!({
                "type": "plan",
                "entries": items,
            })))
            .await;
        }
        AgentMessage::Error { message } => {
            ws.send_message(codex_message(serde_json::json!({
                "type": "message",
                "message": message,
            })))
            .await;
        }
        AgentMessage::TurnComplete { .. } | AgentMessage::ThinkingStatus { .. } => {}
    }
}

pub async fn run(
    working_directory: &str,
    runner_port: Option<u16>,
    started_by: Option<&str>,
    hapir_starting_mode: Option<&str>,
    model: Option<&str>,
    yolo: bool,
) -> anyhow::Result<()> {
    let working_directory = working_directory.to_string();
    let started_by = match started_by {
        Some("runner") => SharedStartedBy::Runner,
        _ => SharedStartedBy::Terminal,
    };
    let starting_mode = resolve_starting_mode(hapir_starting_mode, started_by);

    debug!(
        "[runCodex] Starting in {} (startedBy={:?}, mode={:?})",
        working_directory, started_by, starting_mode
    );

    let config = Configuration::create()?;
    let bootstrap = bootstrap_session(
        SessionBootstrapOptions {
            flavor: "codex".to_string(),
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

    debug!("[runCodex] Session bootstrapped: {}", session_id);

    if let Some(port) = runner_port {
        let pid = std::process::id();
        if let Err(e) = hapir_runner::control_client::notify_session_started(
            port,
            &session_id,
            Some(serde_json::json!({ "hostPid": pid })),
        )
        .await
        {
            tracing::warn!("[runCodex] Failed to notify runner of session start: {e}");
        }
    }

    let lifecycle = RunnerLifecycle::new(RunnerLifecycleOptions {
        ws_client: ws_client.clone(),
        log_tag: "runCodex".to_string(),
        stop_keep_alive: None,
        on_before_close: None,
        on_after_close: None,
    });
    lifecycle.register_process_handlers();

    set_controlled_by_user(&ws_client, starting_mode).await;

    let initial_mode = CodexMode {
        model: model.map(|s| s.to_string()),
        ..Default::default()
    };
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
        session_label: "codex".to_string(),
        session_id_label: "codexSessionId".to_string(),
        apply_session_id_to_metadata: Box::new(|mut metadata, sid| {
            metadata.codex_session_id = Some(sid.to_string());
            metadata
        }),
        permission_mode: bootstrap.session_info.permission_mode,
        model_mode: bootstrap.session_info.model_mode,
    });

    // Build codex app-server args
    let mut codex_args = vec!["app-server".to_string()];
    if yolo {
        codex_args.push("--full-auto".to_string());
    }
    let backend = Arc::new(CodexAppServerBackend::new(
        "codex".to_string(),
        codex_args,
        None,
    ));

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
                    debug!("[runCodex] Received {} command", trimmed);
                    q.push_isolate_and_clear(message, current).await;
                } else {
                    q.push(message, current).await;
                }

                serde_json::json!({"ok": true})
            })
        })
        .await;

    let mode_for_config = current_mode.clone();
    ws_client
        .register_rpc("set-session-config", move |params| {
            let mode = mode_for_config.clone();
            Box::pin(async move {
                let mut m = mode.lock().await;
                if let Some(pm) = params.get("permissionMode").and_then(|v| v.as_str()) {
                    debug!("[runCodex] Permission mode changed to: {}", pm);
                    m.permission_mode = Some(pm.to_string());
                }
                if let Some(cm) = params.get("collaborationMode").and_then(|v| v.as_str()) {
                    debug!("[runCodex] Collaboration mode changed to: {}", cm);
                    m.collaboration_mode = Some(cm.to_string());
                }
                serde_json::json!({"ok": true})
            })
        })
        .await;

    let queue_for_kill = queue.clone();
    let backend_for_kill = backend.clone();
    ws_client
        .register_rpc("killSession", move |_params| {
            let q = queue_for_kill.clone();
            let b = backend_for_kill.clone();
            Box::pin(async move {
                debug!("[runCodex] killSession RPC received");
                q.close().await;
                let _ = b.disconnect().await;
                serde_json::json!({"ok": true})
            })
        })
        .await;

    let backend_for_abort = backend.clone();
    let sb_for_abort = session_base.clone();
    ws_client
        .register_rpc("abort", move |_params| {
            let b = backend_for_abort.clone();
            let sb = sb_for_abort.clone();
            Box::pin(async move {
                debug!("[runCodex] abort RPC received");
                if let Some(sid) = sb.session_id.lock().await.clone() {
                    let _ = b.cancel_prompt(&sid).await;
                }
                sb.on_thinking_change(false).await;
                serde_json::json!({"ok": true})
            })
        })
        .await;

    let terminal_mgr =
        crate::terminal::setup_terminal(&ws_client, &session_id, &working_directory).await;

    ws_client.connect().await;

    let sb_for_local = session_base.clone();
    let sb_for_remote = session_base.clone();
    let backend_for_remote = backend.clone();

    let loop_result = run_local_remote_session(LoopOptions {
        session: session_base.clone(),
        starting_mode: Some(starting_mode),
        log_tag: "runCodex".to_string(),
        run_local: Box::new(move |_base| {
            let sb = sb_for_local.clone();
            Box::pin(async move { codex_local_launcher(&sb).await })
        }),
        run_remote: Box::new(move |_base| {
            let sb = sb_for_remote.clone();
            let b = backend_for_remote.clone();
            Box::pin(async move { codex_remote_launcher(&sb, &b).await })
        }),
        on_session_ready: None,
    })
    .await;

    debug!("[runCodex] Main loop exited");
    let _ = backend.disconnect().await;
    terminal_mgr.close_all().await;
    lifecycle.cleanup().await;

    if let Err(e) = loop_result {
        error!("[runCodex] Loop error: {}", e);
        lifecycle.mark_crash(&e.to_string()).await;
    }

    Ok(())
}

async fn codex_local_launcher(session: &Arc<AgentSessionBase<CodexMode>>) -> LoopResult {
    let working_directory = session.path.clone();
    debug!("[codexLocalLauncher] Starting in {}", working_directory);

    let mut cmd = tokio::process::Command::new("codex");
    cmd.current_dir(&working_directory);

    match cmd.status().await {
        Ok(status) => {
            debug!("[codexLocalLauncher] Codex process exited: {:?}", status);
        }
        Err(e) => {
            warn!("[codexLocalLauncher] Failed to spawn codex: {}", e);
            session
                .ws_client
                .send_message(serde_json::json!({
                    "type": "error",
                    "message": format!("Failed to launch codex CLI: {}", e),
                }))
                .await;
        }
    }

    let exit_reason = get_local_launch_exit_reason(&LocalLaunchContext {
        started_by: Some(SharedStartedBy::Terminal),
        starting_mode: Some(*session.mode.lock().await),
    });

    debug!("[codexLocalLauncher] Exit reason: {:?}", exit_reason);

    match exit_reason {
        LocalLaunchExitReason::Switch => LoopResult::Switch,
        LocalLaunchExitReason::Exit => LoopResult::Exit,
    }
}

async fn codex_remote_launcher(
    session: &Arc<AgentSessionBase<CodexMode>>,
    backend: &Arc<CodexAppServerBackend>,
) -> LoopResult {
    let working_directory = session.path.clone();
    debug!("[codexRemoteLauncher] Starting in {}", working_directory);

    if let Err(e) = backend.initialize().await {
        warn!(
            "[codexRemoteLauncher] Failed to initialize Codex App Server: {}",
            e
        );
        session
            .ws_client
            .send_message(serde_json::json!({
                "type": "error",
                "message": format!("Failed to initialize codex app-server: {}", e),
            }))
            .await;
        return LoopResult::Exit;
    }

    let thread_id = match backend
        .new_session(AgentSessionConfig {
            cwd: working_directory.clone(),
            mcp_servers: vec![],
        })
        .await
    {
        Ok(tid) => {
            session.on_session_found(&tid).await;
            tid
        }
        Err(e) => {
            warn!("[codexRemoteLauncher] Failed to start thread: {}", e);
            session
                .ws_client
                .send_message(serde_json::json!({
                    "type": "error",
                    "message": format!("Failed to start codex thread: {}", e),
                }))
                .await;
            return LoopResult::Exit;
        }
    };

    loop {
        let batch = match session.queue.wait_for_messages().await {
            Some(batch) => batch,
            None => {
                debug!("[codexRemoteLauncher] Queue closed, exiting");
                return LoopResult::Exit;
            }
        };

        let prompt = batch.message;

        debug!(
            "[codexRemoteLauncher] Processing message: {}",
            if prompt.len() > 100 {
                format!("{}...", &prompt[..prompt.floor_char_boundary(100)])
            } else {
                prompt.clone()
            }
        );

        session.on_thinking_change(true).await;

        let (msg_tx, mut msg_rx) = tokio::sync::mpsc::unbounded_channel::<AgentMessage>();
        let ws_for_consumer = session.ws_client.clone();
        let ts_for_consumer = session.thinking_status.clone();
        let consumer = tokio::spawn(async move {
            while let Some(msg) = msg_rx.recv().await {
                if let AgentMessage::ThinkingStatus { ref status } = msg {
                    *ts_for_consumer.lock().await = status.clone();
                    continue;
                }
                forward_agent_message(&ws_for_consumer, msg).await;
            }
        });

        let on_update: Box<dyn Fn(AgentMessage) + Send + Sync> = Box::new(move |msg| {
            let _ = msg_tx.send(msg);
        });

        let content = vec![PromptContent::Text { text: prompt }];
        if let Err(e) = backend.prompt(&thread_id, content, on_update).await {
            warn!("[codexRemoteLauncher] Prompt error: {}", e);
            session
                .ws_client
                .send_message(serde_json::json!({
                    "type": "error",
                    "message": format!("Codex error: {}", e),
                }))
                .await;
        }

        // prompt() 返回后 on_update 已被 drop，msg_tx 随之关闭，
        // consumer 会处理完剩余消息后退出
        let _ = consumer.await;

        session.on_thinking_change(false).await;

        if session.queue.is_closed().await {
            return LoopResult::Exit;
        }
    }
}
