use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use hapir_shared::modes::{AgentFlavor, PermissionMode, SessionMode};
use hapir_shared::schemas::SessionStartedBy as SharedStartedBy;

use crate::agent::bootstrap::{AgentBootstrapConfig, bootstrap_agent};
use crate::agent::cleanup::cleanup_agent_session;
use crate::agent::common_rpc::{ApplyConfigFn, CommonRpc, OnKillFn};
use crate::agent::local_launch_policy::{
    LocalLaunchContext, LocalLaunchExitReason, get_local_launch_exit_reason,
};
use crate::agent::loop_base::{LoopOptions, LoopResult, run_local_remote_session};
use crate::agent::session_base::{AgentSessionBase, AgentSessionBaseOptions};
use hapir_acp::acp_sdk::backend::AcpSdkBackend;
use hapir_acp::types::{AgentBackend, AgentMessage, AgentSessionConfig, PromptContent};
use hapir_infra::config::CliConfiguration;
use hapir_infra::utils::message_queue::MessageQueue2;
use hapir_infra::ws::session_client::WsSessionClient;

use super::{GeminiMode, compute_mode_hash};

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

pub struct GeminiStartOptions {
    pub working_directory: String,
    pub runner_port: Option<u16>,
    pub started_by: SharedStartedBy,
    pub starting_mode: Option<SessionMode>,
}

pub async fn run_gemini(
    options: GeminiStartOptions,
    config: &CliConfiguration,
) -> anyhow::Result<()> {
    let started_by = options.started_by;
    let starting_mode = options.starting_mode.unwrap_or(match started_by {
        SharedStartedBy::Terminal => SessionMode::Local,
        SharedStartedBy::Runner => SessionMode::Remote,
    });

    debug!(
        "[runGemini] Starting in {} (startedBy={:?}, mode={:?})",
        options.working_directory, started_by, starting_mode
    );

    let boot = bootstrap_agent(
        AgentBootstrapConfig {
            flavor: AgentFlavor::Gemini,
            working_directory: options.working_directory.clone(),
            started_by,
            starting_mode,
            runner_port: options.runner_port,
            log_tag: "runGemini",
        },
        config,
    )
    .await?;

    let ws_client = boot.ws_client.clone();
    let working_directory = options.working_directory;

    let queue = Arc::new(MessageQueue2::new(compute_mode_hash));
    let current_mode = Arc::new(Mutex::new(GeminiMode::default()));

    let on_mode_change = boot.lifecycle.create_mode_change_handler();
    let session_base = AgentSessionBase::new(AgentSessionBaseOptions {
        ws_client: ws_client.clone(),
        path: working_directory.clone(),
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
        permission_mode: boot.permission_mode,
        model_mode: boot.model_mode,
    });

    let backend = Arc::new(AcpSdkBackend::new(
        "gemini".to_string(),
        vec!["--experimental-acp".to_string()],
        None,
    ));

    let rpc = CommonRpc::new(&ws_client, queue.clone(), "runGemini");
    rpc.on_user_message(current_mode.clone(), None, None, None)
        .await;

    let apply_config: Arc<ApplyConfigFn<GeminiMode>> =
        Arc::new(Box::new(|m, params| {
            if let Some(pm) = params.get("permissionMode") {
                if let Ok(mode) = serde_json::from_value::<PermissionMode>(pm.clone()) {
                    debug!("[runGemini] Permission mode changed to: {:?}", mode);
                    m.permission_mode = Some(mode);
                }
            }
            if let Some(model) = params.get("model").and_then(|v| v.as_str()) {
                debug!("[runGemini] Model changed to: {}", model);
                m.model = Some(model.to_string());
            }
        }));
    rpc.set_session_config(current_mode.clone(), apply_config)
        .await;

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

    let terminal_mgr =
        crate::terminal::setup_terminal(&ws_client, &boot.session_id, &working_directory).await;

    let _ = ws_client.connect(Duration::from_secs(10)).await;

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
        terminal_reclaim: started_by == SharedStartedBy::Terminal,
    })
    .await;

    let _ = backend.disconnect().await;
    cleanup_agent_session(
        loop_result,
        terminal_mgr,
        boot.lifecycle,
        false,
        "runGemini",
    )
    .await;

    Ok(())
}

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

async fn gemini_remote_launcher(
    session: &Arc<AgentSessionBase<GeminiMode>>,
    backend: &Arc<AcpSdkBackend>,
) -> LoopResult {
    let working_directory = session.path.clone();
    debug!("[geminiRemoteLauncher] Starting in {}", working_directory);

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
