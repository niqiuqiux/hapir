use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tracing::{debug, error, warn};

use hapir_shared::schemas::StartedBy as SharedStartedBy;

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

/// The mode type for Codex sessions.
#[derive(Debug, Clone, Default)]
pub struct CodexMode {
    pub permission_mode: Option<String>,
    pub model: Option<String>,
    pub collaboration_mode: Option<String>,
}

/// Compute a deterministic hash of the codex mode for queue batching.
fn compute_mode_hash(mode: &CodexMode) -> String {
    let mut hasher = Sha256::new();
    hasher.update(mode.permission_mode.as_deref().unwrap_or(""));
    hasher.update("|");
    hasher.update(mode.model.as_deref().unwrap_or(""));
    hasher.update("|");
    hasher.update(mode.collaboration_mode.as_deref().unwrap_or(""));
    hex::encode(hasher.finalize())
}

/// Entry point for running a Codex agent session.
pub async fn run(working_directory: &str, runner_port: Option<u16>) -> anyhow::Result<()> {
    let working_directory = working_directory.to_string();
    let starting_mode = SessionMode::Local;

    debug!(
        "[runCodex] Starting in {} (mode={:?})",
        working_directory, starting_mode
    );

    // Bootstrap session
    let config = Configuration::create()?;
    let bootstrap = bootstrap_session(
        SessionBootstrapOptions {
            flavor: "codex".to_string(),
            started_by: Some(SharedStartedBy::Terminal),
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

    debug!("[runCodex] Session bootstrapped: {}", session_id);

    // Notify runner that this session has started
    if let Some(port) = runner_port {
        let pid = std::process::id();
        if let Err(e) = crate::runner::control_client::notify_session_started(
            port,
            &session_id,
            Some(serde_json::json!({ "hostPid": pid })),
        )
        .await
        {
            tracing::warn!("[runCodex] Failed to notify runner of session start: {e}");
        }
    }

    // Create RunnerLifecycle and register process handlers
    let lifecycle = RunnerLifecycle::new(RunnerLifecycleOptions {
        ws_client: ws_client.clone(),
        log_tag: "runCodex".to_string(),
        stop_keep_alive: None,
        on_before_close: None,
        on_after_close: None,
    });
    lifecycle.register_process_handlers();

    // Set controlledByUser on session
    set_controlled_by_user(&ws_client, starting_mode).await;

    // Create MessageQueue2<CodexMode> with mode hash
    let initial_mode = CodexMode::default();
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
        session_label: "codex".to_string(),
        session_id_label: "codexSessionId".to_string(),
        apply_session_id_to_metadata: Box::new(|mut metadata, sid| {
            metadata.codex_session_id = Some(sid.to_string());
            metadata
        }),
        permission_mode: bootstrap.session_info.permission_mode,
        model_mode: bootstrap.session_info.model_mode,
    });

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
                    debug!("[runCodex] Received {} command, isolate-and-clear", trimmed);
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

    // Enter the main local/remote loop
    let wd_local = working_directory.clone();
    let wd_remote = working_directory.clone();
    let queue_remote = queue.clone();
    let ws_remote = ws_client.clone();

    let loop_result = run_local_remote_session(LoopOptions {
        session: session_base.clone(),
        starting_mode: Some(starting_mode),
        log_tag: "runCodex".to_string(),
        run_local: Box::new(move |_base| {
            let wd = wd_local.clone();
            Box::pin(async move { codex_local_launcher(&wd).await })
        }),
        run_remote: Box::new(move |_base| {
            let wd = wd_remote.clone();
            let q = queue_remote.clone();
            let ws = ws_remote.clone();
            Box::pin(async move { codex_remote_launcher(&wd, &q, &ws).await })
        }),
        on_session_ready: None,
    })
    .await;

    // Cleanup
    debug!("[runCodex] Main loop exited");
    lifecycle.cleanup().await;

    if let Err(e) = loop_result {
        error!("[runCodex] Loop error: {}", e);
        lifecycle.mark_crash(&e.to_string()).await;
    }

    Ok(())
}

/// Local launcher: spawns the `codex` CLI process interactively.
async fn codex_local_launcher(working_directory: &str) -> LoopResult {
    debug!("[codexLocalLauncher] Starting in {}", working_directory);

    let mut cmd = tokio::process::Command::new("codex");
    cmd.current_dir(working_directory);

    match cmd.status().await {
        Ok(status) => {
            debug!("[codexLocalLauncher] Codex process exited: {:?}", status);
        }
        Err(e) => {
            warn!("[codexLocalLauncher] Failed to spawn codex: {}", e);
        }
    }

    let exit_reason = get_local_launch_exit_reason(&LocalLaunchContext {
        started_by: Some(SharedStartedBy::Terminal),
        starting_mode: Some(SessionMode::Local),
    });

    match exit_reason {
        LocalLaunchExitReason::Switch => LoopResult::Switch,
        LocalLaunchExitReason::Exit => LoopResult::Exit,
    }
}

/// Remote launcher: loops on the message queue, spawning `codex --print`
/// for each message and forwarding stdout lines as assistant messages.
async fn codex_remote_launcher(
    working_directory: &str,
    queue: &MessageQueue2<CodexMode>,
    ws_client: &crate::ws::session_client::WsSessionClient,
) -> LoopResult {
    debug!("[codexRemoteLauncher] Starting in {}", working_directory);

    loop {
        let batch = match queue.wait_for_messages().await {
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
                format!("{}...", &prompt[..100])
            } else {
                prompt.clone()
            }
        );

        // Spawn `codex --print` with the message piped to stdin
        let mut cmd = tokio::process::Command::new("codex");
        cmd.arg("--print")
            .current_dir(working_directory)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                warn!("[codexRemoteLauncher] Failed to spawn codex: {}", e);
                ws_client
                    .send_message(serde_json::json!({
                        "type": "error",
                        "message": format!("Failed to spawn codex: {}", e),
                    }))
                    .await;
                continue;
            }
        };

        // Write the prompt to stdin
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            if let Err(e) = stdin.write_all(prompt.as_bytes()).await {
                warn!("[codexRemoteLauncher] Failed to write to stdin: {}", e);
            }
            drop(stdin);
        }

        // Read stdout lines and forward as assistant messages
        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.is_empty() {
                    continue;
                }
                ws_client
                    .send_message(serde_json::json!({
                        "type": "assistant",
                        "message": {
                            "role": "assistant",
                            "content": line,
                        },
                    }))
                    .await;
            }
        }

        // Wait for the process to finish
        match child.wait().await {
            Ok(status) => {
                debug!("[codexRemoteLauncher] Codex process exited: {:?}", status);
            }
            Err(e) => {
                warn!("[codexRemoteLauncher] Error waiting for codex: {}", e);
            }
        }

        // Check if queue is closed
        if queue.is_closed().await {
            return LoopResult::Exit;
        }
    }
}
