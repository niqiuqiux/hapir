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

/// Entry point for running a Gemini agent session.
///
/// Bootstraps the session, creates the message queue, and enters the
/// main local/remote loop. Simpler than Claude: no hook server, no
/// wrapper session type -- uses AgentSessionBase directly.
pub async fn run(working_directory: &str, runner_port: Option<u16>) -> anyhow::Result<()> {
    let working_directory = working_directory.to_string();
    let started_by = SharedStartedBy::Terminal;
    let starting_mode = resolve_starting_mode(None, started_by);

    debug!(
        "[runGemini] Starting in {} (startedBy={:?}, mode={:?})",
        working_directory, started_by, starting_mode
    );

    // Bootstrap session
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
            tracing::warn!("[runGemini] Failed to notify runner of session start: {e}");
        }
    }

    // Create RunnerLifecycle and register signal handlers
    let lifecycle = RunnerLifecycle::new(RunnerLifecycleOptions {
        ws_client: ws_client.clone(),
        log_tag: "runGemini".to_string(),
        stop_keep_alive: None,
        on_before_close: None,
        on_after_close: None,
    });
    lifecycle.register_process_handlers();

    // Set controlledByUser on session
    set_controlled_by_user(&ws_client, starting_mode).await;

    // Create MessageQueue2<GeminiMode> with mode hash
    let initial_mode = GeminiMode::default();
    let queue = Arc::new(MessageQueue2::new(compute_mode_hash));

    // Create AgentSessionBase
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

    // Enter the main local/remote loop
    let sb_for_local = session_base.clone();
    let sb_for_remote = session_base.clone();

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
            Box::pin(async move { gemini_remote_launcher(&sb).await })
        }),
        on_session_ready: None,
    })
    .await;

    // Cleanup
    debug!("[runGemini] Main loop exited");
    lifecycle.cleanup().await;

    if let Err(e) = loop_result {
        error!("[runGemini] Loop error: {}", e);
        lifecycle.mark_crash(&e.to_string()).await;
    }

    Ok(())
}

/// Local launcher for Gemini.
///
/// Spawns the `gemini` CLI process in interactive mode, waits for it
/// to exit, then determines whether to switch to remote or exit.
async fn gemini_local_launcher(
    session: &Arc<AgentSessionBase<GeminiMode>>,
) -> LoopResult {
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

/// Remote launcher for Gemini.
///
/// Waits for messages from the queue, spawns `gemini --print` for each
/// message, reads stdout line-by-line and forwards output to the session.
async fn gemini_remote_launcher(
    session: &Arc<AgentSessionBase<GeminiMode>>,
) -> LoopResult {
    let working_directory = session.path.clone();
    debug!("[geminiRemoteLauncher] Starting in {}", working_directory);

    loop {
        // Wait for a message from the queue
        let batch = match session.queue.wait_for_messages().await {
            Some(batch) => batch,
            None => {
                debug!("[geminiRemoteLauncher] Queue closed, exiting");
                return LoopResult::Exit;
            }
        };

        let prompt = batch.message;
        let _mode = batch.mode;

        debug!(
            "[geminiRemoteLauncher] Processing message: {}",
            if prompt.len() > 100 {
                format!("{}...", &prompt[..100])
            } else {
                prompt.clone()
            }
        );

        // Spawn `gemini --print` with the prompt on stdin
        let mut cmd = tokio::process::Command::new("gemini");
        cmd.arg("--print")
            .current_dir(&working_directory)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                warn!("[geminiRemoteLauncher] Failed to spawn gemini: {}", e);
                session
                    .ws_client
                    .send_message(serde_json::json!({
                        "type": "error",
                        "message": format!("Failed to spawn gemini: {}", e),
                    }))
                    .await;
                continue;
            }
        };

        // Write the prompt to stdin and close it
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(prompt.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }

        session.on_thinking_change(true).await;

        // Read stdout line-by-line and forward to session
        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            let mut output = String::new();

            while let Ok(Some(line)) = lines.next_line().await {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str(&line);
            }

            if !output.is_empty() {
                session
                    .ws_client
                    .send_message(serde_json::json!({
                        "type": "assistant",
                        "message": {
                            "role": "assistant",
                            "content": output,
                        },
                    }))
                    .await;
            }
        }

        // Wait for the process to finish
        match child.wait().await {
            Ok(status) => {
                debug!(
                    "[geminiRemoteLauncher] Gemini process exited: {:?}",
                    status
                );
            }
            Err(e) => {
                warn!("[geminiRemoteLauncher] Error waiting for gemini: {}", e);
            }
        }

        session.on_thinking_change(false).await;

        // Check if queue is closed
        if session.queue.is_closed().await {
            return LoopResult::Exit;
        }
    }
}
