use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::agent::local_launch_policy::{
    get_local_launch_exit_reason, LocalLaunchContext, LocalLaunchExitReason,
};
use crate::agent::loop_base::LoopResult;
use crate::modules::claude::session::{ClaudeSession, StartedBy};
use crate::modules::claude::session_scanner::SessionScanner;

use super::run::EnhancedMode;

/// Local launcher for Claude.
///
/// Spawns the `claude` CLI process in local/interactive mode,
/// sets up a session scanner to watch for session IDs, and manages
/// the local launch lifecycle.
pub async fn claude_local_launcher(
    session: &Arc<ClaudeSession<EnhancedMode>>,
) -> LoopResult {
    let working_directory = session.base.path.clone();
    debug!("[claudeLocalLauncher] Starting in {}", working_directory);

    // 1. Create session scanner to watch for Claude session IDs
    let scanner = Arc::new(Mutex::new(SessionScanner::new(
        None,
        &working_directory,
        Box::new(move |_message| {
            // Process raw JSONL messages from session files
            // In the full implementation, these would be converted and forwarded
        }),
    )));

    // 2. Register session-found callback on the base session
    let scanner_for_cb = scanner.clone();
    session
        .base
        .add_session_found_callback(Box::new(move |sid| {
            let scanner = scanner_for_cb.clone();
            let sid = sid.to_string();
            tokio::spawn(async move {
                scanner.lock().await.on_new_session(&sid);
            });
        }))
        .await;

    // 3. Build CLI arguments for local (interactive) mode
    let mut args: Vec<String> = Vec::new();

    // Add hook settings if available
    if !session.hook_settings_path.is_empty() {
        args.push("--settings".to_string());
        args.push(session.hook_settings_path.clone());
    }

    // Add any passthrough claude args
    if let Some(ref extra_args) = *session.claude_args.lock().await {
        args.extend(extra_args.iter().cloned());
    }

    // 4. Spawn the claude CLI process in interactive mode
    debug!(
        "[claudeLocalLauncher] Spawning claude process with args: {:?}",
        args
    );

    let mut cmd = tokio::process::Command::new("claude");
    cmd.args(&args).current_dir(&working_directory);

    // Pass through claude env vars
    if let Some(ref env_vars) = session.claude_env_vars {
        for (key, value) in env_vars {
            cmd.env(key, value);
        }
    }

    let _exit_status = match cmd.status().await {
        Ok(status) => {
            debug!("[claudeLocalLauncher] Claude process exited: {:?}", status);
            // On success, consume one-time flags
            session.consume_one_time_flags().await;
            Some(status)
        }
        Err(e) => {
            warn!("[claudeLocalLauncher] Failed to spawn claude: {}", e);
            // Record the failure
            session.record_local_launch_failure(
                format!("Failed to launch claude: {}", e),
                "spawn_error".to_string(),
            );
            // Send failure message to session
            session
                .base
                .ws_client
                .send_message(serde_json::json!({
                    "type": "error",
                    "message": format!("Failed to launch claude CLI: {}", e),
                }))
                .await;
            None
        }
    };

    // 5. Cleanup scanner
    scanner.lock().await.cleanup().await;

    // 6. Determine exit reason
    let shared_started_by = match session.started_by {
        StartedBy::Runner => hapir_shared::schemas::StartedBy::Runner,
        StartedBy::Terminal => hapir_shared::schemas::StartedBy::Terminal,
    };

    let exit_reason = get_local_launch_exit_reason(&LocalLaunchContext {
        started_by: Some(shared_started_by),
        starting_mode: Some(session.starting_mode),
    });

    debug!(
        "[claudeLocalLauncher] Exit reason: {:?}",
        exit_reason
    );

    match exit_reason {
        LocalLaunchExitReason::Switch => LoopResult::Switch,
        LocalLaunchExitReason::Exit => LoopResult::Exit,
    }
}
