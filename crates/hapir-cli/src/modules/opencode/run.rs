use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use hapir_shared::modes::{AgentFlavor, PermissionMode, SessionMode};
use hapir_shared::schemas::SessionStartedBy;

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

use super::{OpencodeMode, compute_mode_hash};

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

async fn opencode_local_launcher(session: &Arc<AgentSessionBase<OpencodeMode>>) -> LoopResult {
    let working_directory = session.path.clone();
    debug!("[opencodeLocalLauncher] Starting in {}", working_directory);

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

pub struct OpencodeStartOptions {
    pub working_directory: String,
    pub runner_port: Option<u16>,
    pub started_by: SessionStartedBy,
    pub starting_mode: Option<SessionMode>,
    pub resume: Option<String>,
}

pub async fn run_opencode(
    options: OpencodeStartOptions,
    config: &CliConfiguration,
) -> anyhow::Result<()> {
    let started_by = options.started_by;
    let starting_mode = options.starting_mode.unwrap_or(match started_by {
        SessionStartedBy::Terminal => SessionMode::Local,
        SessionStartedBy::Runner => SessionMode::Remote,
    });

    debug!(
        "[runOpenCode] Starting in {} (startedBy={:?}, mode={:?})",
        options.working_directory, started_by, starting_mode
    );

    let boot = bootstrap_agent(
        AgentBootstrapConfig {
            flavor: AgentFlavor::Opencode,
            working_directory: options.working_directory.clone(),
            started_by,
            starting_mode,
            runner_port: options.runner_port,
            log_tag: "runOpenCode",
        },
        config,
    )
    .await?;

    let ws_client = boot.ws_client.clone();
    let working_directory = options.working_directory;

    let queue = Arc::new(MessageQueue2::new(compute_mode_hash));
    let current_mode = Arc::new(Mutex::new(OpencodeMode::default()));

    let on_mode_change = boot.lifecycle.create_mode_change_handler();
    let session_base = AgentSessionBase::new(AgentSessionBaseOptions {
        ws_client: ws_client.clone(),
        path: working_directory.clone(),
        session_id: None,
        queue: queue.clone(),
        on_mode_change_cb: on_mode_change,
        mode: starting_mode,
        session_label: "opencode".to_string(),
        session_id_label: "opencodeSessionId".to_string(),
        apply_session_id_to_metadata: Box::new(|mut metadata, sid| {
            metadata.opencode_session_id = Some(sid.to_string());
            metadata
        }),
        permission_mode: boot.permission_mode,
        model_mode: boot.model_mode,
    });

    let backend = Arc::new(AcpSdkBackend::new(
        "opencode".to_string(),
        vec![
            "acp".to_string(),
            "--cwd".to_string(),
            working_directory.to_string(),
        ],
        None,
    ));

    let rpc = CommonRpc::new(&ws_client, queue.clone(), "runOpenCode");
    rpc.on_user_message(current_mode.clone(), None, None, None)
        .await;

    let apply_config: Arc<ApplyConfigFn<OpencodeMode>> =
        Arc::new(Box::new(|m, params| {
            if let Some(pm) = params.get("permissionMode") {
                if let Ok(mode) = serde_json::from_value::<PermissionMode>(pm.clone()) {
                    debug!("[runOpenCode] Permission mode changed to: {:?}", mode);
                    m.permission_mode = Some(mode);
                }
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
        terminal_reclaim: started_by == SessionStartedBy::Terminal,
    })
    .await;

    let _ = backend.disconnect().await;
    cleanup_agent_session(
        loop_result,
        terminal_mgr,
        boot.lifecycle,
        false,
        "runOpenCode",
    )
    .await;

    Ok(())
}
