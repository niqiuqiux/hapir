use std::sync::Arc;
use std::time::Duration;

use tokio::select;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use hapir_shared::modes::{AgentFlavor, PermissionMode, SessionMode};
use hapir_shared::schemas::SessionStartedBy as SharedStartedBy;

use crate::agent::bootstrap::{AgentBootstrapConfig, bootstrap_agent};
use crate::agent::cleanup::cleanup_agent_session;
use crate::agent::common_rpc::{ApplyConfigFn, CommonRpc, MessagePreProcessor, OnKillFn};
use crate::agent::local_launch_policy::{
    LocalLaunchContext, LocalLaunchExitReason, get_local_launch_exit_reason,
};
use crate::agent::local_sync::LocalSyncDriver;
use crate::agent::loop_base::{LoopOptions, LoopResult, run_local_remote_session};
use crate::agent::session_base::{AgentSessionBase, AgentSessionBaseOptions};
use hapir_acp::codex_app_server::backend::CodexAppServerBackend;
use hapir_acp::types::{
    AgentBackend, AgentMessage, AgentSessionConfig, PermissionResponse, PromptContent,
};
use hapir_infra::config::CliConfiguration;
use hapir_infra::utils::message_queue::MessageQueue2;
use hapir_infra::utils::terminal::{restore_terminal_state, save_terminal_state};
use hapir_infra::ws::session_client::WsSessionClient;

use super::session_scanner::CodexSessionScanner;
use super::{CodexMode, compute_mode_hash};

pub(crate) fn codex_message(data: serde_json::Value) -> serde_json::Value {
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

pub struct CodexStartOptions {
    pub working_directory: String,
    pub runner_port: Option<u16>,
    pub started_by: SharedStartedBy,
    pub starting_mode: Option<SessionMode>,
    pub model: Option<String>,
    pub yolo: bool,
    pub resume: Option<String>,
}

pub async fn run_codex(
    options: CodexStartOptions,
    config: &CliConfiguration,
) -> anyhow::Result<()> {
    let working_directory = options.working_directory;

    save_terminal_state();
    let started_by = options.started_by;
    let starting_mode = options.starting_mode.unwrap_or(match started_by {
        SharedStartedBy::Terminal => SessionMode::Local,
        SharedStartedBy::Runner => SessionMode::Remote,
    });

    debug!(
        "[runCodex] Starting in {} (startedBy={:?}, mode={:?})",
        working_directory, started_by, starting_mode
    );

    let boot = bootstrap_agent(
        AgentBootstrapConfig {
            flavor: AgentFlavor::Codex,
            working_directory: working_directory.clone(),
            started_by,
            starting_mode,
            runner_port: options.runner_port,
            log_tag: "runCodex",
        },
        config,
    )
    .await?;

    let ws_client = boot.ws_client.clone();

    let initial_mode = CodexMode {
        model: options.model,
        ..Default::default()
    };
    let queue = Arc::new(MessageQueue2::new(compute_mode_hash));
    let current_mode = Arc::new(Mutex::new(initial_mode));

    let on_mode_change = boot.lifecycle.create_mode_change_handler();
    let session_base = AgentSessionBase::new(AgentSessionBaseOptions {
        ws_client: ws_client.clone(),
        path: working_directory.clone(),
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
        permission_mode: boot.permission_mode,
        model_mode: boot.model_mode,
    });

    let mut codex_args = vec!["app-server".to_string()];
    if options.yolo {
        codex_args.push("--full-auto".to_string());
    }
    let backend = Arc::new(CodexAppServerBackend::new(
        "codex".to_string(),
        codex_args,
        None,
    ));

    let pending_attachments: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let attachments_for_rpc = pending_attachments.clone();

    // Extract attachment paths from params and accumulate them for the next prompt
    let pre_process: Arc<MessagePreProcessor> = Arc::new(Box::new(move |params| {
        let text = params
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if let Some(attachments) = params.get("attachments").and_then(|v| v.as_array()) {
            let paths: Vec<String> = attachments
                .iter()
                .filter_map(|a| a.get("path").and_then(|p| p.as_str()))
                .map(|s| s.to_string())
                .collect();
            if !paths.is_empty() {
                debug!("[runCodex] Received {} attachment(s)", paths.len());
                let att = attachments_for_rpc.clone();
                tokio::spawn(async move {
                    att.lock().await.extend(paths);
                });
            }
        }

        text
    }));

    let rpc = CommonRpc::new(&ws_client, queue.clone(), "runCodex");
    rpc.on_user_message(
        current_mode.clone(),
        Some(session_base.switch_notify.clone()),
        Some(Arc::new(std::sync::Mutex::new(starting_mode))),
        Some(pre_process),
    )
    .await;

    let apply_config: Arc<ApplyConfigFn<CodexMode>> =
        Arc::new(Box::new(|m, params| {
            if let Some(pm) = params.get("permissionMode") {
                if let Ok(mode) = serde_json::from_value::<PermissionMode>(pm.clone()) {
                    debug!("[runCodex] Permission mode changed to: {:?}", mode);
                    m.permission_mode = Some(mode);
                }
            }
            if let Some(cm) = params.get("collaborationMode").and_then(|v| v.as_str()) {
                debug!("[runCodex] Collaboration mode changed to: {}", cm);
                m.collaboration_mode = Some(cm.to_string());
            }
        }));
    rpc.set_session_config(current_mode.clone(), apply_config)
        .await;

    rpc.switch(session_base.switch_notify.clone()).await;

    let backend_for_kill = backend.clone();
    let on_kill: OnKillFn = Arc::new(move || {
        let b = backend_for_kill.clone();
        Box::pin(async move {
            let _ = b.disconnect().await;
        })
    });
    rpc.kill_session(Some(on_kill)).await;

    rpc.acp_abort(
        backend.clone() as Arc<dyn AgentBackend>,
        session_base.clone(),
    )
    .await;

    // Publish permission requests to agent state so the web UI can render approval buttons
    let ws_for_perm = ws_client.clone();
    backend.on_permission_request(Box::new(move |req: hapir_acp::types::PermissionRequest| {
        let ws = ws_for_perm.clone();
        let tool_name = req.kind.clone().unwrap_or_default();
        let arguments = req.raw_input.clone().unwrap_or(serde_json::json!({}));
        let key = req.id.clone();
        let requested_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as f64;

        tokio::spawn(async move {
            let _ = ws
                .update_agent_state(move |mut state| {
                    if let Some(obj) = state.as_object_mut() {
                        let requests = obj
                            .entry("requests")
                            .or_insert_with(|| serde_json::json!({}));
                        if let Some(req_map) = requests.as_object_mut() {
                            req_map.insert(
                                key,
                                serde_json::json!({
                                    "tool": tool_name,
                                    "arguments": arguments,
                                    "createdAt": requested_at,
                                }),
                            );
                        }
                    }
                    state
                })
                .await;
        });
    }));

    let backend_for_perm = backend.clone();
    let sb_for_perm = session_base.clone();
    ws_client
        .register_rpc("permission", move |params| {
            let b = backend_for_perm.clone();
            let sb = sb_for_perm.clone();
            Box::pin(async move {
                debug!("[runCodex] permission RPC received: {:?}", params);

                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let approved = params
                    .get("approved")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let option_id = if approved { "accept" } else { "deny" };
                let perm_req = hapir_acp::types::PermissionRequest {
                    id: id.clone(),
                    session_id: String::new(),
                    tool_call_id: id.clone(),
                    title: None,
                    kind: None,
                    raw_input: None,
                    raw_output: None,
                    options: vec![],
                };
                let perm_resp = PermissionResponse::Selected {
                    option_id: option_id.to_string(),
                };

                if let Some(sid) = sb.session_id.lock().await.clone() {
                    let _ = b.respond_to_permission(&sid, &perm_req, perm_resp).await;
                } else {
                    let _ = b
                        .respond_to_permission(
                            "",
                            &perm_req,
                            PermissionResponse::Selected {
                                option_id: option_id.to_string(),
                            },
                        )
                        .await;
                }

                let id_for_state = id.clone();
                let status = if approved { "approved" } else { "denied" };
                let completed_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as f64;

                let _ = sb
                    .ws_client
                    .update_agent_state(move |mut state| {
                        if let Some(requests) =
                            state.get_mut("requests").and_then(|v| v.as_object_mut())
                        {
                            if let Some(request) = requests.remove(&id_for_state) {
                                if state.get("completedRequests").is_none() {
                                    state["completedRequests"] = serde_json::json!({});
                                }
                                if let Some(completed_map) = state
                                    .get_mut("completedRequests")
                                    .and_then(|v| v.as_object_mut())
                                {
                                    let mut entry: serde_json::Map<String, serde_json::Value> =
                                        request.as_object().cloned().unwrap_or_default();
                                    entry.insert("status".to_string(), serde_json::json!(status));
                                    entry.insert(
                                        "completedAt".to_string(),
                                        serde_json::json!(completed_at),
                                    );
                                    completed_map.insert(id_for_state, serde_json::json!(entry));
                                }
                            }
                        }
                        state
                    })
                    .await;

                serde_json::json!({"ok": true})
            })
        })
        .await;

    let terminal_mgr =
        crate::terminal::setup_terminal(&ws_client, &boot.session_id, &working_directory).await;

    let _ = ws_client.connect(Duration::from_secs(10)).await;

    let sb_for_local = session_base.clone();
    let sb_for_remote = session_base.clone();
    let backend_for_remote = backend.clone();
    let resume_thread_id: Option<String> = options.resume;
    let attachments_for_remote = pending_attachments.clone();

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
            let resume = resume_thread_id.clone();
            let att = attachments_for_remote.clone();
            Box::pin(async move { codex_remote_launcher(&sb, &b, resume.as_deref(), att).await })
        }),
        on_session_ready: None,
        terminal_reclaim: started_by == SharedStartedBy::Terminal,
    })
    .await;

    let _ = backend.disconnect().await;
    cleanup_agent_session(loop_result, terminal_mgr, boot.lifecycle, true, "runCodex").await;

    Ok(())
}

async fn codex_local_launcher(session: &Arc<AgentSessionBase<CodexMode>>) -> LoopResult {
    let working_directory = session.path.clone();
    debug!("[codexLocalLauncher] Starting in {}", working_directory);

    let session_ref = session.clone();
    let scanner = CodexSessionScanner::new(
        &working_directory,
        session.session_id.lock().await.clone(),
        Some(Box::new(move |sid: &str| {
            let s = session_ref.clone();
            let sid = sid.to_string();
            tokio::spawn(async move {
                s.on_session_found(&sid).await;
            });
        })),
    );
    let scanner = Arc::new(Mutex::new(scanner));

    let mut sync_driver = LocalSyncDriver::start(
        scanner.clone(),
        session.ws_client.clone(),
        Duration::from_secs(2),
        "codexLocalSync",
    );

    let mut cmd = tokio::process::Command::new("codex");
    if let Some(ref sid) = *session.session_id.lock().await {
        cmd.arg("resume").arg(sid);
    }
    cmd.current_dir(&working_directory);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            warn!("[codexLocalLauncher] Failed to spawn codex: {}", e);
            session
                .ws_client
                .send_message(serde_json::json!({
                    "type": "error",
                    "message": format!("Failed to launch codex CLI: {}", e),
                }))
                .await;
            sync_driver.stop();
            return LoopResult::Exit;
        }
    };

    let switched = select! {
        status = child.wait() => {
            match status {
                Ok(s) => debug!("[codexLocalLauncher] Codex process exited: {:?}", s),
                Err(e) => warn!("[codexLocalLauncher] Error waiting for codex: {}", e),
            }
            false
        }
        _ = session.switch_notify.notified() => {
            info!("[codexLocalLauncher] Switch requested, killing codex process");
            kill_child_gracefully(&mut child).await;
            true
        }
    };

    sync_driver.stop();
    LocalSyncDriver::final_flush(&scanner, &session.ws_client, "codexLocalSync").await;

    restore_terminal_state();

    if switched {
        return LoopResult::Switch;
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
    resume_thread_id: Option<&str>,
    pending_attachments: Arc<Mutex<Vec<String>>>,
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

    let resume_id = match resume_thread_id {
        Some(id) => Some(id.to_string()),
        None => session
            .ws_client
            .metadata()
            .await
            .and_then(|m| m.codex_session_id),
    };

    let thread_id = match resume_id {
        Some(ref prev_id) => {
            debug!(
                "[codexRemoteLauncher] Attempting to resume thread: {}",
                prev_id
            );
            match backend.resume_session(prev_id).await {
                Ok(tid) => {
                    debug!("[codexRemoteLauncher] Thread resumed: {}", tid);
                    session.on_session_found(&tid).await;
                    tid
                }
                Err(e) => {
                    warn!(
                        "[codexRemoteLauncher] Failed to resume thread {}: {}, starting new",
                        prev_id, e
                    );
                    match backend
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
                        Err(e2) => {
                            warn!("[codexRemoteLauncher] Failed to start thread: {}", e2);
                            session
                                .ws_client
                                .send_message(serde_json::json!({
                                    "type": "error",
                                    "message": format!("Failed to start codex thread: {}", e2),
                                }))
                                .await;
                            return LoopResult::Exit;
                        }
                    }
                }
            }
        }
        None => {
            match backend
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
            }
        }
    };

    loop {
        let batch = select! {
            batch = session.queue.wait_for_messages() => {
                match batch {
                    Some(b) => b,
                    None => {
                        debug!("[codexRemoteLauncher] Queue closed, exiting");
                        return LoopResult::Exit;
                    }
                }
            }
            _ = session.switch_notify.notified() => {
                info!("[codexRemoteLauncher] Switch requested, returning to local");
                return LoopResult::Switch;
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

        let mut content = vec![PromptContent::Text { text: prompt }];
        let image_paths: Vec<String> = pending_attachments.lock().await.drain(..).collect();
        for path in image_paths {
            content.push(PromptContent::LocalImage { path });
        }
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

        let _ = consumer.await;
        session.on_thinking_change(false).await;

        if session.queue.is_closed().await {
            return LoopResult::Exit;
        }
    }
}

async fn kill_child_gracefully(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let _ = hapir_infra::utils::process::kill_process_tree(pid, false).await;
    } else {
        let _ = child.kill().await;
    }
    let _ = child.wait().await;
}
