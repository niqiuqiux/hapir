use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::{debug, error, warn};

use hapi_shared::schemas::StartedBy;

use crate::agent::local_launch_policy::{
    get_local_launch_exit_reason, LocalLaunchContext, LocalLaunchExitReason,
};
use crate::agent::loop_base::{LoopOptions, LoopResult, run_local_remote_session};
use crate::agent::runner_lifecycle::{
    create_mode_change_handler, set_controlled_by_user, RunnerLifecycle, RunnerLifecycleOptions,
};
use crate::agent::session_base::{AgentSessionBase, AgentSessionBaseOptions, SessionMode};
use crate::agent::session_factory::{bootstrap_session, SessionBootstrapOptions};
use crate::config::Configuration;
use crate::utils::message_queue::MessageQueue2;

/// The mode type for OpenCode sessions.
///
/// Only captures permission_mode since OpenCode has fewer knobs than Claude.
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

/// Local launcher for OpenCode.
///
/// Spawns the `opencode` CLI process in interactive mode.
async fn opencode_local_launcher(
    session: &Arc<AgentSessionBase<OpencodeMode>>,
) -> LoopResult {
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

/// Remote launcher for OpenCode.
///
/// Spawns the `opencode` CLI with `--print` for each queued message,
/// reads stdout for the response, and forwards it to the session.
async fn opencode_remote_launcher(
    session: &Arc<AgentSessionBase<OpencodeMode>>,
) -> LoopResult {
    let working_directory = session.path.clone();
    debug!("[opencodeRemoteLauncher] Starting in {}", working_directory);

    loop {
        let batch = match session.queue.wait_for_messages().await {
            Some(batch) => batch,
            None => {
                debug!("[opencodeRemoteLauncher] Queue closed, exiting");
                return LoopResult::Exit;
            }
        };

        let prompt = batch.message;
        let is_isolate = batch.isolate;

        debug!(
            "[opencodeRemoteLauncher] Processing message (isolate={}): {}",
            is_isolate,
            if prompt.len() > 100 {
                format!("{}...", &prompt[..100])
            } else {
                prompt.clone()
            }
        );

        session.on_thinking_change(true).await;

        let mut cmd = tokio::process::Command::new("opencode");
        cmd.arg("--print")
            .arg(&prompt)
            .current_dir(&working_directory)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        match cmd.output().await {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stdout.is_empty() {
                    session
                        .ws_client
                        .send_message(serde_json::json!({
                            "type": "assistant",
                            "message": {
                                "role": "assistant",
                                "content": stdout.trim(),
                            },
                        }))
                        .await;
                }

                let stderr_text = String::from_utf8_lossy(&output.stderr);
                if !stderr_text.is_empty() {
                    debug!("[opencodeRemoteLauncher] stderr: {}", stderr_text);
                }

                session
                    .ws_client
                    .send_message(serde_json::json!({
                        "type": "result",
                        "subtype": "success",
                        "isError": !output.status.success(),
                    }))
                    .await;
            }
            Err(e) => {
                warn!("[opencodeRemoteLauncher] Failed to spawn opencode: {}", e);
                session
                    .ws_client
                    .send_message(serde_json::json!({
                        "type": "error",
                        "message": format!("Failed to spawn opencode: {}", e),
                    }))
                    .await;
            }
        }

        session.on_thinking_change(false).await;

        if session.queue.is_closed().await {
            return LoopResult::Exit;
        }
    }
}

/// Entry point for running an OpenCode agent session.
///
/// Bootstraps the session, creates the message queue, and enters the
/// main local/remote loop. Unlike Claude, there is no hook server or
/// session wrapper — the AgentSessionBase is used directly.
pub async fn run(working_directory: &str) -> anyhow::Result<()> {
    debug!("[runOpenCode] Starting in {}", working_directory);

    // 1. Bootstrap session
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

    // 2. Create RunnerLifecycle
    let lifecycle = RunnerLifecycle::new(RunnerLifecycleOptions {
        ws_client: ws_client.clone(),
        log_tag: "runOpenCode".to_string(),
        stop_keep_alive: None,
        on_before_close: None,
        on_after_close: None,
    });
    lifecycle.register_process_handlers();

    // 3. Set controlledByUser
    set_controlled_by_user(&ws_client, SessionMode::Local).await;

    // 4. Create MessageQueue2<OpencodeMode>
    let initial_mode = OpencodeMode::default();
    let queue = Arc::new(MessageQueue2::new(compute_mode_hash));

    // 5. Create AgentSessionBase
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

    // 6. Register on-user-message RPC handler
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

    // 7. Register set-session-config RPC handler
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

    // 8. Enter the main local/remote loop
    let sb_for_local = session_base.clone();
    let sb_for_remote = session_base.clone();

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
            Box::pin(async move { opencode_remote_launcher(&s).await })
        }),
        on_session_ready: None,
    })
    .await;

    // 9. Cleanup
    debug!("[runOpenCode] Main loop exited");
    lifecycle.cleanup().await;

    if let Err(e) = loop_result {
        error!("[runOpenCode] Loop error: {}", e);
        lifecycle.mark_crash(&e.to_string()).await;
    }

    Ok(())
}
